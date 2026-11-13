//! Private-server (LandSandBoat) launches.
//!
//! A private-server account always logs in through `xiloader.exe`: started directly, or by
//! Windower or Ashita, which then inject into that xiloader as they would into PlayOnline.
//! The server's launcher manifest, when it publishes one (`server_manifest`), says which
//! xiloader to run and adds the server's key fingerprint and login ports.
//!
//! This runs inside the account's session worker (`session_worker::launch_private`), so the
//! game keeps its row when Crystal's window closes and opens again, like a retail session.
//! Nothing here touches PlayOnline: retail sign-in, the Viewer and the official updater are
//! never involved.

use crate::{
    backend::{self, with_log, Account, Config, LaunchResult},
    launch_log::{self, LoaderEnd},
    launch_timing::{Milestone, Timeline},
    pol_less::{tools, Plan, Tool},
    server_manifest::{self, LoaderOptions},
};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    os::windows::io::{AsRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// How long Windower or Ashita may take to start xiloader.
const FRONTEND_LIMIT: Duration = Duration::from_secs(60);

/// xiloader's arguments, in the order it documents them.
fn xiloader_arguments(account: &Account, otp: &str, hairpin: bool, options: &LoaderOptions) -> Vec<String> {
    let mut args = vec![
        "--server".to_owned(), account.server.trim().to_owned(),
        "--user".to_owned(), account.pol_id.clone(),
        "--password".to_owned(), account.pol_password.clone(),
    ];
    if !otp.is_empty() {
        args.push("--otp".to_owned());
        args.push(otp.to_owned());
    }
    if hairpin {
        args.push("--hairpin".to_owned());
    }
    args.extend(options.arguments());
    args
}

/// One argument quoted the way the Microsoft C runtime (and xiloader) splits a command line.
fn quote_argument(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
        return argument.to_owned();
    }
    let mut quoted = String::from('"');
    let mut backslashes = 0usize;
    for c in argument.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    // Backslashes before the closing quote are doubled so they don't escape it.
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// `arguments` as one command-line string (without the program).
fn command_line<S: AsRef<str>>(arguments: &[S]) -> Zeroizing<String> {
    Zeroizing::new(arguments.iter().map(|argument| quote_argument(argument.as_ref())).collect::<Vec<_>>().join(" "))
}

/// The arguments a `Command` was given, as one string (for an elevated start of it).
fn command_arguments(command: &Command) -> Zeroizing<String> {
    let arguments: Vec<String> = command.get_args().map(|argument| argument.to_string_lossy().into_owned()).collect();
    command_line(&arguments)
}

/// Start a private-server client and return once xiloader is running. `via` is Windower or
/// Ashita, or `None` to run xiloader directly. `hairpin` adds `--hairpin`.
pub(crate) fn launch(
    config: &Config,
    account: Account,
    otp: Zeroizing<String>,
    via: Option<Plan>,
    hairpin: bool,
    mut timing: Timeline,
    mut progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    if account.mode != backend::AccountMode::Private {
        return Err("The xiloader route is only for private-server accounts.".into());
    }
    if via.as_ref().is_some_and(|plan| plan.tool == Tool::Vanilla) {
        return Err("Vanilla is not a private-server launch option. Use xiloader, Windower or Ashita.".into());
    }
    if account.server.trim().is_empty() {
        return Err("Enter the private server's host name in the account's settings.".into());
    }
    let game = config.game_directory();
    if !game.is_dir() {
        return Err(format!("The selected FINAL FANTASY XI folder does not exist: {}", game.display()));
    }

    // The server's own xiloader, fingerprint and ports when it publishes a launcher manifest;
    // otherwise xiloader.exe from the game folder.
    let prepared = server_manifest::prepare(config, &account.server, &game, &mut progress)?;
    for note in &prepared.notes {
        progress(note);
    }
    let loader = prepared.loader.clone();
    let options = prepared.options.clone();
    let launcher = via.as_ref().map_or("xiloader", |plan| plan.tool.name());

    let logs = config.logs_directory(&account);
    fs::create_dir_all(&logs).map_err(|e| format!("Could not create the private-server log folder: {e}"))?;
    let log = logs.join(format!("private-{}.log", backend::unique_id()?));
    let mut file = File::create(&log).map_err(|e| format!("Could not create the private-server launch log: {e}"))?;
    let mut lines = vec![format!("[launcher] Starting the private-server client for {} through {launcher}.", account.server.trim())];
    lines.extend(prepared.details.iter().chain(&prepared.notes).map(|line| format!("[manifest] {line}")));
    lines.push(format!(
        "[launcher] xiloader: {}{}",
        loader.display(),
        if options.fingerprint.is_some() { " (server key pinned)" } else { " (no server key pinned: xiloader asks to trust the server)" }
    ));
    lines.push(format!("[launcher] Hairpin: {}", if hairpin { "on" } else { "off" }));
    if let Some(plan) = &via {
        lines.push(format!("[launcher] {}", frontend_choice(plan)));
    }
    for line in &lines {
        writeln!(file, "{line}").map_err(|e| format!("Could not write the private-server launch log: {e}"))?;
    }
    file.flush().map_err(|e| format!("Could not write the private-server launch log: {e}"))?;
    drop(file);

    let arguments = Zeroizing::new(xiloader_arguments(&account, otp.as_str(), hairpin, &options));
    drop(otp);

    progress(&format!("Starting the private-server client through {launcher}…"));
    let started = Instant::now();
    let (pid, monitor) = match &via {
        Some(plan) => {
            let mut frontend = private_frontend(plan, &loader, &command_line(arguments.as_slice()))
                .map_err(|error| with_log(error, &log))?;
            let root = spawn_frontend(&mut frontend, &log, &mut progress).map_err(|error| with_log(error, &log))?;
            timing.mark(Milestone::ToolStarted);
            let root_pid = root.pid().ok_or_else(|| with_log(format!("Could not tell which process {} is.", frontend.name), &log))?;
            progress(&format!("Waiting for {} to start xiloader…", frontend.name));
            let name = loader.file_name().map_or_else(|| "xiloader.exe".to_owned(), |name| name.to_string_lossy().into_owned());
            let pid = match wait_for_descendant(root_pid, &name, FRONTEND_LIMIT) {
                Ok(pid) => pid,
                Err(error) => {
                    let mut root = root;
                    root.stop();
                    return Err(with_log(format!("{} {error}", frontend.name), &log));
                }
            };
            // Windower and Ashita exit or stay on their own; the game is xiloader's. Its handle
            // gives its exit code; without one (refused), it is watched by PID and name.
            drop(root);
            drop(frontend);
            use crate::win::process::{PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE};
            let monitor = crate::win::process::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)
                .map_or(Monitor::Pid(pid, name), Monitor::Handle);
            (pid, monitor)
        }
        None => {
            // xiloader asks in its console whether to trust a server it has not seen; with the
            // key pinned there is nothing to ask, so its window stays hidden.
            let show = if options.fingerprint.is_some() {
                windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE
            } else {
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL
            };
            progress("Requesting administrator rights for xiloader…");
            let handle = run_elevated(&loader, &command_line(arguments.as_slice()), &game, show, "xiloader")
                .map_err(|error| with_log(error, &log))?;
            let monitor = Monitor::Handle(handle);
            let pid = monitor.pid().ok_or_else(|| with_log("Could not tell which process xiloader is.", &log))?;
            (pid, monitor)
        }
    };
    drop(arguments);
    // The account copy holds the password: dropping it now clears it (`Account`'s Drop).
    let history = config.history_path();
    let label = account.label.clone();
    drop(account);

    let alive = Arc::new(AtomicBool::new(true));
    let game_running = Arc::new(AtomicBool::new(true));
    let outcome = Arc::new(Mutex::new(None));
    {
        let (alive, game_running, outcome, log) = (alive.clone(), game_running.clone(), outcome.clone(), log.clone());
        let mut monitor = monitor;
        std::thread::Builder::new()
            .name("private-xiloader-monitor".into())
            .spawn(move || {
                let end = monitor.wait();
                let report = launch_log::classify_loader_end(&end, started.elapsed());
                launch_log::append_history(&history, &label, &report.summary, Some(&log));
                if let Ok(mut slot) = outcome.lock() {
                    *slot = Some(report);
                }
                game_running.store(false, Ordering::Release);
                alive.store(false, Ordering::Release);
            })
            .map_err(|e| format!("Could not start the private-server monitor: {e}"))?;
    }

    Ok(LaunchResult {
        pid,
        detail: format!("Private-server client started through {launcher}."),
        alive,
        game_running,
        log,
        outcome,
        timing: timing.row_summary(),
    })
}

/// "Windower profile: LandSandBoat", for the launch log.
fn frontend_choice(plan: &Plan) -> String {
    match plan.tool {
        Tool::Windower => {
            let profile = plan.settings.windower_profile.trim();
            if profile.is_empty() {
                "Windower profile: Windower's unnamed default profile (none chosen for this account or in Launch tools)".to_owned()
            } else {
                format!("Windower profile: {profile}")
            }
        }
        Tool::Ashita => {
            let base = plan.settings.boot_ini.trim();
            format!("Ashita base boot config: {}", if base.is_empty() { tools::DEFAULT_BOOT_INI } else { base })
        }
        Tool::Vanilla => "Vanilla".to_owned(),
    }
}

/// ShellExecuteEx "runas": the UAC prompt (none when Crystal already runs as administrator).
fn run_elevated(exe: &Path, arguments: &str, directory: &Path, show: i32, name: &str) -> Result<OwnedHandle, String> {
    use crate::win::{shell_execute, ShellError};
    use windows_sys::Win32::Foundation::ERROR_CANCELLED;
    shell_execute("runas", exe, Some(arguments), Some(directory), show, false).map_err(|error| match error {
        ShellError::Failed(ERROR_CANCELLED) => {
            format!("The Windows administrator prompt for {name} was declined. {name} needs administrator rights: accept the prompt, or run Crystal as administrator.")
        }
        ShellError::Failed(code) => format!("Windows could not start {} as administrator (error {code}).", exe.display()),
        ShellError::NoProcess => format!("Windows started {} but returned no process to watch.", exe.display()),
    })
}

/// How xiloader ended, from its exit code (xiloader's `ExitCode`), in words for the row.
fn xiloader_end(code: i32) -> LoaderEnd {
    let message = match code {
        0 => return LoaderEnd::Exit(0),
        1 => "xiloader did not accept its arguments",
        2 => "xiloader could not start its network hooks",
        3 => "xiloader could not reach the server's login",
        4 => "the server's login stopped answering",
        5 => "PlayOnline is not installed for the chosen language",
        6 => "this PlayOnline version is not one xiloader supports",
        7 => "xiloader's profile server failed",
        8 => "the game failed to start",
        9 => "xiloader refused the server's key (it does not match the one on record)",
        _ => return LoaderEnd::Exit(code),
    };
    LoaderEnd::Error(format!("{message} (exit {code})"))
}

/// What a private launch watches until xiloader exits.
enum Monitor {
    Child(Child),
    /// xiloader's (or a front end's) process handle: started through the UAC prompt, or opened
    /// once Windower or Ashita had started it.
    Handle(OwnedHandle),
    /// xiloader started by Windower or Ashita: its PID and file name (a PID alone could be
    /// reused by another program once xiloader exits).
    Pid(u32, String),
}

impl Monitor {
    fn pid(&self) -> Option<u32> {
        match self {
            Monitor::Child(child) => Some(child.id()),
            Monitor::Handle(handle) => {
                let pid = unsafe { windows_sys::Win32::System::Threading::GetProcessId(handle.as_raw_handle() as _) };
                (pid != 0).then_some(pid)
            }
            Monitor::Pid(pid, _) => Some(*pid),
        }
    }

    fn wait(&mut self) -> LoaderEnd {
        match self {
            Monitor::Child(child) => match child.wait() {
                Ok(status) => status.code().map_or(LoaderEnd::Closed, LoaderEnd::Exit),
                Err(_) => LoaderEnd::Closed,
            },
            Monitor::Handle(handle) => {
                use windows_sys::Win32::{
                    Foundation::WAIT_OBJECT_0,
                    System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE},
                };
                unsafe {
                    if WaitForSingleObject(handle.as_raw_handle() as _, INFINITE) != WAIT_OBJECT_0 {
                        return LoaderEnd::Closed;
                    }
                    let mut code = 0u32;
                    if GetExitCodeProcess(handle.as_raw_handle() as _, &mut code) != 0 {
                        xiloader_end(code as i32)
                    } else {
                        LoaderEnd::Closed
                    }
                }
            }
            Monitor::Pid(pid, name) => {
                while process_named(*pid, name).unwrap_or(true) {
                    std::thread::sleep(Duration::from_millis(250));
                }
                LoaderEnd::Closed
            }
        }
    }

    /// Stop a front end that Crystal started itself and that never started xiloader.
    fn stop(&mut self) {
        if let Monitor::Child(child) = self {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

/// Windower or Ashita, set up to start `loader` with `arguments`.
struct FrontendLaunch {
    command: Command,
    executable: PathBuf,
    directory: PathBuf,
    /// Ashita's generated boot ini, removed once Ashita has read it.
    generated_ini: Option<PathBuf>,
    name: &'static str,
}

impl Drop for FrontendLaunch {
    fn drop(&mut self) {
        if let Some(path) = self.generated_ini.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn private_frontend(plan: &Plan, loader: &Path, arguments: &str) -> Result<FrontendLaunch, String> {
    let configured = match plan.tool {
        Tool::Ashita => &plan.settings.ashita,
        Tool::Windower => &plan.settings.windower,
        Tool::Vanilla => return Err("Vanilla is not a private-server launch option. Use xiloader, Windower or Ashita.".into()),
    };
    let directory = tools::detect(plan.tool, configured).ok_or_else(|| {
        if configured.trim().is_empty() {
            format!("{} was not found in the usual folders. Set its folder in Settings → System → Launch tools.", plan.tool.name())
        } else {
            format!("{} was not found: {} has no {}.", plan.tool.name(), configured.trim(), plan.tool.exe())
        }
    })?;
    match plan.tool {
        Tool::Windower => {
            let command = tools::windower_command_with_args(&directory, &plan.settings.windower_profile, loader, arguments)?;
            Ok(FrontendLaunch { executable: directory.join(plan.tool.exe()), directory, command, generated_ini: None, name: plan.tool.name() })
        }
        Tool::Ashita => {
            let explicit = !plan.settings.boot_ini.trim().is_empty();
            let base_name = if explicit { plan.settings.boot_ini.trim() } else { tools::DEFAULT_BOOT_INI };
            if !tools::valid_boot_ini_name(base_name) {
                return Err("The base boot config must be an .ini file name in Ashita's config\\boot folder.".into());
            }
            let boot = directory.join("config").join("boot");
            let base_path = boot.join(base_name);
            let base = match fs::read(&base_path) {
                Ok(bytes) => Some(bytes),
                Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return Err(format!("The base boot config {base_name} was not found in {}.", boot.display())),
            };
            let ini = tools::ashita_private_boot_ini(base.as_deref(), loader, arguments)?;
            let ini_name = format!("crystal-private-{}.ini", backend::unique_id()?);
            let path = boot.join(&ini_name);
            let temporary = boot.join(format!("{ini_name}.tmp"));
            fs::create_dir_all(&boot)
                .and_then(|_| fs::write(&temporary, &ini))
                .and_then(|_| fs::rename(&temporary, &path))
                .map_err(|_| {
                    let _ = fs::remove_file(&temporary);
                    format!("Could not write Ashita's private-server boot config {}.", path.display())
                })?;
            let command = tools::ashita_command(&directory, &ini_name);
            Ok(FrontendLaunch { executable: directory.join(plan.tool.exe()), directory, command, generated_ini: Some(path), name: plan.tool.name() })
        }
        Tool::Vanilla => unreachable!(),
    }
}

/// Start the front end; one that needs administrator rights (error 740) goes through the UAC
/// prompt. Its output goes to the launch log.
fn spawn_frontend(frontend: &mut FrontendLaunch, log: &Path, progress: &mut dyn FnMut(&str)) -> Result<Monitor, String> {
    if let Ok(file) = fs::OpenOptions::new().append(true).open(log) {
        if let Ok(copy) = file.try_clone() {
            frontend.command.stdout(Stdio::from(file)).stderr(Stdio::from(copy));
        }
    }
    match frontend.command.spawn() {
        Ok(child) => Ok(Monitor::Child(child)),
        Err(error) if error.raw_os_error() == Some(740) => {
            progress(&format!("{} needs administrator rights; asking Windows…", frontend.name));
            run_elevated(
                &frontend.executable,
                &command_arguments(&frontend.command),
                &frontend.directory,
                windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
                frontend.name,
            )
            .map(Monitor::Handle)
        }
        Err(error) => Err(format!("Could not start {}: {error}", frontend.executable.display())),
    }
}

#[derive(Clone, Debug)]
struct ProcessEntry {
    pid: u32,
    parent: u32,
    exe: String,
}

fn process_snapshot() -> Result<Vec<ProcessEntry>, String> {
    crate::win::process::processes()
        .map(|list| list.into_iter().map(|entry| ProcessEntry { pid: entry.pid, parent: entry.parent, exe: entry.name }).collect())
        .ok_or_else(|| "Could not list the running programs.".to_owned())
}

/// The first process named `name` among `known`'s descendants (`known` grows with them, so a
/// helper chain such as Windower's updater is followed).
fn named_descendant(entries: &[ProcessEntry], known: &mut HashSet<u32>, name: &str) -> Option<u32> {
    loop {
        let mut changed = false;
        for entry in entries {
            if known.contains(&entry.parent) && known.insert(entry.pid) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    entries.iter().find(|entry| known.contains(&entry.pid) && entry.exe.eq_ignore_ascii_case(name)).map(|entry| entry.pid)
}

fn wait_for_descendant(root_pid: u32, name: &str, limit: Duration) -> Result<u32, String> {
    let deadline = Instant::now() + limit;
    let mut known = HashSet::from([root_pid]);
    loop {
        if let Some(pid) = named_descendant(&process_snapshot()?, &mut known, name) {
            return Ok(pid);
        }
        if Instant::now() >= deadline {
            return Err(format!("did not start {name} within {} seconds.", limit.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn process_named(pid: u32, name: &str) -> Result<bool, String> {
    Ok(process_snapshot()?.iter().any(|entry| entry.pid == pid && entry.exe.eq_ignore_ascii_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_account() -> Account {
        Account {
            label: "Fixture".into(),
            mode: backend::AccountMode::Private,
            server: " play.example.test ".into(),
            pol_id: "fixture-user".into(),
            pol_password: "fixture secret".into(),
            se_id: String::new(),
            se_password: String::new(),
            region: 0,
            launch_with: None,
            ashita_boot_ini: None,
            windower_profile: None,
            pol_handle_names: String::new(),
            totp_key: String::new(),
        }
    }

    #[test]
    fn xiloader_arguments_are_explicit_with_hairpin_and_otp() {
        let args = xiloader_arguments(&private_account(), "123456", true, &LoaderOptions::default());
        assert_eq!(args, [
            "--server", "play.example.test", "--user", "fixture-user", "--password", "fixture secret",
            "--otp", "123456", "--hairpin",
        ]);
        let args = xiloader_arguments(&private_account(), "", false, &LoaderOptions::default());
        assert!(!args.iter().any(|arg| arg == "--hairpin" || arg == "--otp"));
    }

    #[test]
    fn manifest_fingerprint_and_ports_follow_the_login_arguments() {
        let options = LoaderOptions {
            fingerprint: Some("SHA256:eQmR2ar3Lq/CHlKETW+sZ8JqMDS48h/M/RIH8UmR0E4".into()),
            ports: Some(server_manifest::Ports { data: 54230, view: 54001, auth: 54231, profile: 51220 }),
        };
        let args = xiloader_arguments(&private_account(), "", true, &options);
        assert_eq!(&args[6..], [
            "--hairpin",
            "--dataport", "54230", "--viewport", "54001", "--authport", "54231", "--profileport", "51220",
            "--fingerprint", "SHA256:eQmR2ar3Lq/CHlKETW+sZ8JqMDS48h/M/RIH8UmR0E4",
        ]);
    }

    #[test]
    fn arguments_are_quoted_like_the_c_runtime_splits_them() {
        assert_eq!(quote_argument("plain"), "plain");
        assert_eq!(quote_argument(""), r#""""#);
        assert_eq!(quote_argument("two words"), r#""two words""#);
        assert_eq!(quote_argument(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote_argument(r"C:\Program Files\"), r#""C:\Program Files\\""#);
        assert_eq!(quote_argument(r"a\\b c"), r#""a\\b c""#);
        assert_eq!(quote_argument(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(command_line(&["--password", "p w", "--hairpin"]).as_str(), r#"--password "p w" --hairpin"#);
    }

    #[test]
    fn the_launch_log_names_the_windower_profile_or_says_it_is_the_unnamed_default() {
        let mut plan = Plan {
            tool: Tool::Windower,
            settings: Default::default(),
            social_only: false,
            auto_away_minutes: crate::pol_less::default_auto_away(),
            auto_login: Default::default(),
            viewer_gaps: false,
            keep_session: false,
            trigger: Default::default(),
        };
        assert!(frontend_choice(&plan).contains("unnamed default"));
        plan.settings.windower_profile = "LandSandBoat".into();
        assert_eq!(frontend_choice(&plan), "Windower profile: LandSandBoat");
        plan.tool = Tool::Ashita;
        assert_eq!(frontend_choice(&plan), "Ashita base boot config: default.ini");
    }

    #[test]
    fn ashita_gets_the_loader_and_command_and_keeps_the_rest_of_the_base() {
        let base = b"[ashita.boot]\r\nfile = old.exe\r\ncommand = --old\r\nscript = mine.txt\r\n\r\n[ashita.language]\r\nplayonline = 1\r\n";
        let ini = String::from_utf8(tools::ashita_private_boot_ini(Some(base), Path::new(r"C:\Crystal\xiloader.exe"), "--server h --hairpin").unwrap()).unwrap();
        assert!(ini.contains(r"file = C:\Crystal\xiloader.exe"));
        assert!(ini.contains("command = --server h --hairpin"));
        assert!(ini.contains("script = mine.txt"), "the user's script stays");
        assert!(ini.contains("playonline = 1"), "the language stays as the user set it");
        assert!(!ini.contains("old.exe") && !ini.contains("--old"));
        assert!(tools::ashita_private_boot_ini(None, Path::new(r"C:\Crystal\xiloader.exe"), "--a\r\nb").is_err());
        assert!(tools::ashita_private_boot_ini(None, Path::new("C:\\Cr\u{e9}stal\\xiloader.exe"), "--a").is_err());
    }

    #[test]
    fn xiloader_exit_codes_become_reasons() {
        assert_eq!(xiloader_end(0), LoaderEnd::Exit(0));
        assert_eq!(xiloader_end(42), LoaderEnd::Exit(42));
        assert!(matches!(xiloader_end(3), LoaderEnd::Error(m) if m.contains("reach") && m.ends_with("(exit 3)")));
        assert!(matches!(xiloader_end(9), LoaderEnd::Error(m) if m.contains("key")));
        let report = launch_log::classify_loader_end(&xiloader_end(0), Duration::from_secs(3600));
        assert!(!report.is_problem(), "a normal quit is not a problem");
    }

    #[test]
    fn descendant_tracking_follows_helper_chains_and_matches_the_loader_name() {
        let entries = vec![
            ProcessEntry { pid: 20, parent: 10, exe: "Updater.exe".into() },
            ProcessEntry { pid: 30, parent: 20, exe: "XILOADER.EXE".into() },
            ProcessEntry { pid: 40, parent: 99, exe: "xiloader.exe".into() },
        ];
        let mut known = HashSet::from([10]);
        assert_eq!(named_descendant(&entries, &mut known, "xiloader.exe"), Some(30));
        assert!(known.contains(&20) && known.contains(&30) && !known.contains(&40));
        let mut known = HashSet::from([50]);
        assert_eq!(named_descendant(&entries, &mut known, "xiloader.exe"), None, "another program's xiloader is not ours");
    }
}
