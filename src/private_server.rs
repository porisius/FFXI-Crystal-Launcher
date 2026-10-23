//! LandSandBoat/private-server launch path.
//!
//! Milestone 1 deliberately does only one thing: start a private account with
//! `xiloader.exe` from that account's selected FFXI folder. Retail launching,
//! Square Enix version checks and the retail updater remain untouched.

use crate::{
    backend::{self, Account, Config, LaunchResult},
    launch_log::{self, LoaderEnd},
    launch_timing::{Milestone, Timeline},
};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Instant,
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

pub(crate) fn launch(
    config: &Config,
    account: Account,
    otp: zeroize::Zeroizing<String>,
    timing: Timeline,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    launch_with_hairpin(config, account, otp, true, timing, progress)
}

pub(crate) fn launch_with_hairpin(
    config: &Config,
    account: Account,
    otp: zeroize::Zeroizing<String>,
    hairpin: bool,
    mut timing: Timeline,
    mut progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    if account.mode != backend::AccountMode::Private {
        return Err("The xiloader route is only valid for private-server accounts.".into());
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

    let logs = config.logs_directory(&account);
    fs::create_dir_all(&logs)
        .map_err(|e| format!("Could not create the private-server log folder: {e}"))?;
    let log = logs.join(format!("private-{}.log", backend::unique_id()?));
    let mut stdout = File::create(&log)
        .map_err(|e| format!("Could not create the private-server launch log: {e}"))?;
    writeln!(
        stdout,
        "[launcher] Starting private-server client through xiloader."
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
    let (child_tx, child_rx) = mpsc::sync_channel::<(crate::pol_less::Injector, Instant)>(1);

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
            let end = if child.wait().is_ok() {
                match child.exited() {
                    Some(Some(code)) => LoaderEnd::Exit(code),
                    _ => LoaderEnd::Closed,
                }
            } else {
                LoaderEnd::Closed
            };
            let report = launch_log::classify_loader_end(&end, started.elapsed());
            launch_log::append_history(&history, &label, &report.summary, Some(&log_monitor));
            if let Ok(mut slot) = outcome_monitor.lock() {
                *slot = Some(report);
            }
            game_monitor.store(false, Ordering::Release);
            alive_monitor.store(false, Ordering::Release);
        })
        .map_err(|e| format!("Could not start the private-server monitor: {e}"))?;

    progress("Starting private-server client through xiloader…");
    let mut command =
        xiloader_command_with_hairpin(&loader, &game, &account, otp.as_str(), hairpin);
    command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let started = Instant::now();

    #[cfg(windows)]
    let child = {
        progress("Requesting administrator rights for xiloader…");

        crate::pol_less::run_elevated(
            &command,
            &loader,
            &game,
            "The Windows administrator prompt for xiloader was declined.",
        )
        .map(crate::pol_less::Injector::Elevated)?
    };

    #[cfg(not(windows))]
    let child = command
        .spawn()
        .map(crate::pol_less::Injector::Child)
        .map_err(|e| format!("Could not start xiloader.exe: {e}"))?;

    let pid = child
        .pid()
        .ok_or_else(|| "Could not determine the xiloader.exe process ID.".to_owned())?;
    alive.store(true, Ordering::Release);
    game_running.store(true, Ordering::Release);
    if let Err(error) = child_tx.send((child, started)) {
        let (mut child, _) = error.0;
        child.stop();
        alive.store(false, Ordering::Release);
        game_running.store(false, Ordering::Release);
        return Err("Could not attach the private-server process monitor.".into());
    }

    // Command has copied its arguments and the child has been created. Drop the
    // account copy now so its Drop implementation zeroizes the saved secrets.
    drop(account);
    timing.mark(Milestone::ClientSpawned);
    let summary = timing.row_summary();

    Ok(LaunchResult {
        pid,
        detail: "Private-server client started through xiloader.".into(),
        alive,
        game_running,
        log,
        outcome,
        timing: summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xiloader_arguments_are_explicit_and_hairpin_enabled() {
        let account = Account {
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
        };

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
        let account = Account {
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
        };

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
        let account = Account {
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
        };

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
}
