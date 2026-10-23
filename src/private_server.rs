//! LandSandBoat/private-server launch path.
//!
//! Private accounts always authenticate through `xiloader.exe`. They can run xiloader
//! directly, or ask Ashita/Windower to launch and inject into that same xiloader process.
//! Retail authentication and the retail updater remain separate.

use crate::{
    backend::{self, Account, Config, LaunchResult},
    launch_log::{self, LoaderEnd},
    launch_timing::{Milestone, Timeline},
    pol_less::{self, Plan, Tool},
};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

fn xiloader_command(loader: &Path, game: &Path, account: &Account, otp: &str) -> Command {
    xiloader_command_with_hairpin(loader, game, account, otp, true)
}

fn xiloader_command_with_hairpin(
    loader: &Path,
    game: &Path,
    account: &Account,
    otp: &str,
    hairpin: bool,
) -> Command {
    let mut command = Command::new(loader);

    command
        .current_dir(game)
        .arg("--server")
        .arg(account.server.trim())
        .arg("--user")
        .arg(&account.pol_id)
        .arg("--password")
        .arg(&account.pol_password);

    if !otp.is_empty() {
        command.arg("--otp").arg(otp);
    }

    if hairpin {
        command.arg("--hairpin");
    }

    command
}

#[cfg(windows)]
fn xiloader_argument_string(
    loader: &Path,
    game: &Path,
    account: &Account,
    otp: &str,
    hairpin: bool,
) -> String {
    let command = xiloader_command_with_hairpin(loader, game, account, otp, hairpin);
    pol_less::command_arguments(&command)
}

pub(crate) fn launch(
    config: &Config,
    account: Account,
    otp: zeroize::Zeroizing<String>,
    timing: Timeline,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    launch_with_plan(config, account, otp, None, true, timing, progress)
}

pub(crate) fn launch_with_hairpin(
    config: &Config,
    account: Account,
    otp: zeroize::Zeroizing<String>,
    hairpin: bool,
    timing: Timeline,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    launch_with_plan(config, account, otp, None, hairpin, timing, progress)
}

pub(crate) fn launch_with_plan(
    config: &Config,
    account: Account,
    otp: zeroize::Zeroizing<String>,
    via: Option<Plan>,
    hairpin: bool,
    mut timing: Timeline,
    mut progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    if account.mode != backend::AccountMode::Private {
        return Err("The xiloader route is only valid for private-server accounts.".into());
    }
    if via.as_ref().is_some_and(|plan| plan.tool == Tool::Vanilla) {
        return Err("Vanilla is not a private-server launch option. Use xiloader, Ashita, or Windower.".into());
    }

    let game = config.game_directory();
    if !game.is_dir() {
        return Err(format!(
            "The selected FINAL FANTASY XI folder does not exist: {}",
            game.display()
        ));
    }
    let loader = game.join("xiloader.exe");
    if !loader.is_file() {
        return Err(format!(
            "xiloader.exe was not found in the selected FINAL FANTASY XI folder: {}",
            game.display()
        ));
    }

    let launcher = via
        .as_ref()
        .map_or("xiloader", |plan| plan.tool.name());

    let logs = config.logs_directory(&account);
    fs::create_dir_all(&logs)
        .map_err(|e| format!("Could not create the private-server log folder: {e}"))?;
    let log = logs.join(format!("private-{}.log", backend::unique_id()?));
    let mut stdout = File::create(&log)
        .map_err(|e| format!("Could not create the private-server launch log: {e}"))?;
    writeln!(
        stdout,
        "[launcher] Starting private-server client through {launcher}."
    )
    .map_err(|e| format!("Could not write the private-server launch log: {e}"))?;
    stdout
        .flush()
        .map_err(|e| format!("Could not write the private-server launch log: {e}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("Could not prepare the private-server launch log: {e}"))?;

    let alive = Arc::new(AtomicBool::new(false));
    let game_running = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(Mutex::new(None));
    let (child_tx, child_rx) = mpsc::sync_channel::<(PrivateMonitor, Instant)>(1);

    let alive_monitor = alive.clone();
    let game_monitor = game_running.clone();
    let outcome_monitor = outcome.clone();
    let history = config.history_path();
    let label = account.label.clone();
    let log_monitor = log.clone();
    let _monitor = std::thread::Builder::new()
        .name("private-xiloader-monitor".into())
        .spawn(move || {
            let Ok((mut child, started)) = child_rx.recv() else {
                return;
            };
            let end = child.wait();
            let report = launch_log::classify_loader_end(&end, started.elapsed());
            launch_log::append_history(&history, &label, &report.summary, Some(&log_monitor));
            if let Ok(mut slot) = outcome_monitor.lock() {
                *slot = Some(report);
            }
            game_monitor.store(false, Ordering::Release);
            alive_monitor.store(false, Ordering::Release);
        })
        .map_err(|e| format!("Could not start the private-server monitor: {e}"))?;

    progress(&format!(
        "Starting private-server client through {launcher}…"
    ));
    let started = Instant::now();

    #[cfg(windows)]
    let (pid, monitor) = if let Some(plan) = via.as_ref() {
        let arguments =
            xiloader_argument_string(&loader, &game, &account, otp.as_str(), hairpin);
        let mut frontend = private_frontend(plan, &loader, &arguments)?;
        frontend
            .command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let injector = spawn_frontend(&mut frontend, &mut progress)?;
        let root_pid = injector
            .pid()
            .ok_or_else(|| format!("Could not determine the {} process ID.", frontend.name))?;
        progress(&format!(
            "Waiting for xiloader to start through {}…",
            frontend.name
        ));
        let pid = wait_for_xiloader(root_pid)?;
        drop(injector);
        drop(frontend);
        (pid, PrivateMonitor::Pid(pid))
    } else {
        let mut command =
            xiloader_command_with_hairpin(&loader, &game, &account, otp.as_str(), hairpin);
        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);

        progress("Requesting administrator rights for xiloader…");
        let child = pol_less::run_elevated(
            &command,
            &loader,
            &game,
            "The Windows administrator prompt for xiloader was declined.",
        )
        .map(pol_less::Injector::Elevated)?;
        let pid = child
            .pid()
            .ok_or_else(|| "Could not determine the xiloader.exe process ID.".to_owned())?;
        (pid, PrivateMonitor::Injector(child))
    };

    #[cfg(not(windows))]
    let (pid, monitor) = {
        if via.is_some() {
            return Err(
                "Private-server launching through Ashita or Windower is currently supported on Windows only."
                    .into(),
            );
        }
        let mut command =
            xiloader_command_with_hairpin(&loader, &game, &account, otp.as_str(), hairpin);
        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let child = command
            .spawn()
            .map(pol_less::Injector::Child)
            .map_err(|e| format!("Could not start xiloader.exe: {e}"))?;
        let pid = child
            .pid()
            .ok_or_else(|| "Could not determine the xiloader.exe process ID.".to_owned())?;
        (pid, PrivateMonitor::Injector(child))
    };

    alive.store(true, Ordering::Release);
    game_running.store(true, Ordering::Release);
    if let Err(error) = child_tx.send((monitor, started)) {
        let (mut child, _) = error.0;
        child.stop();
        alive.store(false, Ordering::Release);
        game_running.store(false, Ordering::Release);
        return Err("Could not attach the private-server process monitor.".into());
    }

    // Command construction has copied its arguments and the target process now owns them.
    // Drop the account copy so its Drop implementation zeroizes the saved secrets.
    drop(account);
    timing.mark(Milestone::ClientSpawned);
    let summary = timing.row_summary();

    Ok(LaunchResult {
        pid,
        detail: format!("Private-server client started through {launcher}."),
        alive,
        game_running,
        log,
        outcome,
        timing: summary,
    })
}

enum PrivateMonitor {
    Injector(pol_less::Injector),
    #[cfg(windows)]
    Pid(u32),
}

impl PrivateMonitor {
    fn wait(&mut self) -> LoaderEnd {
        match self {
            PrivateMonitor::Injector(child) => {
                if child.wait().is_ok() {
                    match child.exited() {
                        Some(Some(code)) => LoaderEnd::Exit(code),
                        _ => LoaderEnd::Closed,
                    }
                } else {
                    LoaderEnd::Closed
                }
            }
            #[cfg(windows)]
            PrivateMonitor::Pid(pid) => {
                while process_named(*pid, "xiloader.exe").unwrap_or(true) {
                    std::thread::sleep(Duration::from_millis(250));
                }
                LoaderEnd::Closed
            }
        }
    }

    fn stop(&mut self) {
        match self {
            PrivateMonitor::Injector(child) => child.stop(),
            #[cfg(windows)]
            PrivateMonitor::Pid(_) => {}
        }
    }
}

#[cfg(windows)]
struct FrontendLaunch {
    command: Command,
    executable: PathBuf,
    directory: PathBuf,
    generated_ini: Option<PathBuf>,
    name: &'static str,
}

#[cfg(windows)]
impl Drop for FrontendLaunch {
    fn drop(&mut self) {
        if let Some(path) = self.generated_ini.take() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(windows)]
fn private_frontend(
    plan: &Plan,
    loader: &Path,
    arguments: &str,
) -> Result<FrontendLaunch, String> {
    let configured = match plan.tool {
        Tool::Ashita => &plan.settings.ashita,
        Tool::Windower => &plan.settings.windower,
        Tool::Vanilla => {
            return Err(
                "Vanilla is not a private-server launch option. Use xiloader, Ashita, or Windower."
                    .into(),
            )
        }
    };
    let directory = crate::pol_less::tools::detect(plan.tool, configured).ok_or_else(|| {
        if configured.trim().is_empty() {
            format!(
                "{} was not found in the usual folders. Set its folder in Settings → System → Launch tools.",
                plan.tool.name()
            )
        } else {
            format!(
                "{} was not found: {} has no {}.",
                plan.tool.name(),
                configured.trim(),
                plan.tool.exe()
            )
        }
    })?;

    match plan.tool {
        Tool::Windower => {
            let command = crate::pol_less::tools::windower_command_with_args(
                &directory,
                &plan.settings.windower_profile,
                loader,
                arguments,
            )?;
            Ok(FrontendLaunch {
                executable: directory.join(plan.tool.exe()),
                directory,
                command,
                generated_ini: None,
                name: plan.tool.name(),
            })
        }
        Tool::Ashita => {
            let explicit = !plan.settings.boot_ini.trim().is_empty();
            let base_name = if explicit {
                plan.settings.boot_ini.trim()
            } else {
                crate::pol_less::tools::DEFAULT_BOOT_INI
            };
            if !crate::pol_less::tools::valid_boot_ini_name(base_name) {
                return Err(
                    "The base boot config must be an .ini file name in Ashita's config\\boot folder."
                        .into(),
                );
            }
            let base_path = directory.join("config").join("boot").join(base_name);
            let base = match fs::read(&base_path) {
                Ok(bytes) => Some(bytes),
                Err(error)
                    if !explicit && error.kind() == std::io::ErrorKind::NotFound =>
                {
                    None
                }
                Err(_) => {
                    return Err(format!(
                        "The base boot config {} was not found in {}.",
                        base_name,
                        base_path.parent().unwrap_or(&base_path).display()
                    ))
                }
            };
            let ini =
                crate::pol_less::tools::ashita_private_boot_ini(base.as_deref(), loader, arguments)?;
            let boot = directory.join("config").join("boot");
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
            let command = crate::pol_less::tools::ashita_command(&directory, &ini_name);
            Ok(FrontendLaunch {
                executable: directory.join(plan.tool.exe()),
                directory,
                command,
                generated_ini: Some(path),
                name: plan.tool.name(),
            })
        }
        Tool::Vanilla => unreachable!(),
    }
}

#[cfg(windows)]
fn spawn_frontend(
    frontend: &mut FrontendLaunch,
    progress: &mut dyn FnMut(&str),
) -> Result<pol_less::Injector, String> {
    match frontend.command.spawn() {
        Ok(child) => Ok(pol_less::Injector::Child(child)),
        Err(error) if error.raw_os_error() == Some(740) => {
            progress(&format!(
                "{} needs administrator rights; asking Windows…",
                frontend.name
            ));
            pol_less::run_elevated(
                &frontend.command,
                &frontend.executable,
                &frontend.directory,
                &format!(
                    "The Windows administrator prompt for {} was declined.",
                    frontend.name
                ),
            )
            .map(pol_less::Injector::Elevated)
        }
        Err(error) => Err(format!(
            "Could not start {}: {error}",
            frontend.executable.display()
        )),
    }
}

#[cfg(windows)]
#[derive(Clone, Debug)]
struct ProcessEntry {
    pid: u32,
    parent: u32,
    exe: String,
}

#[cfg(windows)]
fn process_snapshot() -> Result<Vec<ProcessEntry>, String> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err("Could not enumerate processes while waiting for xiloader.".into());
        }

        let mut entries = Vec::new();
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(entry.szExeFile.len());
                entries.push(ProcessEntry {
                    pid: entry.th32ProcessID,
                    parent: entry.th32ParentProcessID,
                    exe: String::from_utf16_lossy(&entry.szExeFile[..end]),
                });

                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        CloseHandle(snapshot);
        Ok(entries)
    }
}

#[cfg(windows)]
fn named_descendant(
    entries: &[ProcessEntry],
    known: &mut HashSet<u32>,
    name: &str,
) -> Option<u32> {
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

    entries
        .iter()
        .find(|entry| known.contains(&entry.pid) && entry.exe.eq_ignore_ascii_case(name))
        .map(|entry| entry.pid)
}

#[cfg(windows)]
fn wait_for_xiloader(root_pid: u32) -> Result<u32, String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut known = HashSet::from([root_pid]);

    loop {
        let entries = process_snapshot()?;
        if let Some(pid) = named_descendant(&entries, &mut known, "xiloader.exe") {
            return Ok(pid);
        }
        if Instant::now() >= deadline {
            return Err(
                "The selected launch tool did not start xiloader.exe within 60 seconds.".into(),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(windows)]
fn process_named(pid: u32, name: &str) -> Result<bool, String> {
    Ok(process_snapshot()?
        .iter()
        .any(|entry| entry.pid == pid && entry.exe.eq_ignore_ascii_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_account() -> Account {
        Account {
            label: "Fixture".into(),
            mode: backend::AccountMode::Private,
            server: "play.example.test".into(),
            pol_id: "fixture-user".into(),
            pol_password: "fixture-secret".into(),
            se_id: String::new(),
            se_password: String::new(),
            region: 0,
            launch_with: None,
            ashita_boot_ini: None,
            windower_profile: None,
        }
    }

    #[test]
    fn xiloader_arguments_are_explicit_and_hairpin_enabled() {
        let account = private_account();
        let command = xiloader_command(
            Path::new(r"C:\FFXI\xiloader.exe"),
            Path::new(r"C:\FFXI"),
            &account,
            "123456",
        );

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        let expected: Vec<String> = [
            "--server",
            "play.example.test",
            "--user",
            "fixture-user",
            "--password",
            "fixture-secret",
            "--otp",
            "123456",
            "--hairpin",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        assert_eq!(args, expected);
    }

    #[test]
    fn xiloader_omits_hairpin_when_disabled() {
        let account = private_account();
        let command = xiloader_command_with_hairpin(
            Path::new(r"C:\FFXI\xiloader.exe"),
            Path::new(r"C:\FFXI"),
            &account,
            "123456",
            false,
        );

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(!args.iter().any(|arg| arg == "--hairpin"));
        assert!(args.iter().any(|arg| arg == "--otp"));
        assert!(args.iter().any(|arg| arg == "123456"));
    }

    #[test]
    fn xiloader_omits_otp_when_none_was_entered() {
        let account = private_account();
        let command = xiloader_command(
            Path::new(r"C:\FFXI\xiloader.exe"),
            Path::new(r"C:\FFXI"),
            &account,
            "",
        );

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert!(!args.iter().any(|arg| arg == "--otp"));
        assert!(!args.iter().any(|arg| arg == "123456"));
    }

    #[cfg(windows)]
    #[test]
    fn descendant_tracking_follows_frontend_helper_chains() {
        let entries = vec![
            ProcessEntry {
                pid: 20,
                parent: 10,
                exe: "Updater.exe".into(),
            },
            ProcessEntry {
                pid: 30,
                parent: 20,
                exe: "xiloader.exe".into(),
            },
        ];
        let mut known = HashSet::from([10]);
        assert_eq!(
            named_descendant(&entries, &mut known, "xiloader.exe"),
            Some(30)
        );
        assert!(known.contains(&20) && known.contains(&30));
    }
}
