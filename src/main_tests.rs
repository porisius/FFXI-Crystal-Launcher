use super::*;
pub(super) fn app() -> App {
    let (sender, receiver) = mpsc::channel();
    let mut app = App::base(Config::default(), Settings::default(), tick_gate::EventSender::new(sender), receiver, dgvoodoo::State::new(false));
    app.load_preview();
    app
}
/// Audit 2026-11-09: launcher close reads the sign-in's flag (IdleSessionClose::with_sign_in_done);
/// it is set before the Complete goes out, never by progress.
#[test]
fn a_sign_in_marks_itself_done_before_its_completion_is_sent() {
    let (sender, events) = mpsc::channel();
    let done = Arc::new(AtomicBool::new(false));
    let tx = LaunchTx { done: Some(done.clone()), ..LaunchTx::new(tick_gate::EventSender::new(sender), "a".into(), 1) };
    tx.progress("signing in");
    assert!(!done.load(Ordering::Acquire), "progress is no completion");
    tx.complete(Err("failed before its worker started".into()));
    assert!(done.load(Ordering::Acquire));
    assert_eq!(events.try_iter().count(), 2);
}
#[test]
fn the_row_shut_down_ends_only_its_own_game() {
    let mut app=app();
    for (i,row) in app.accounts.iter_mut().enumerate() {
        row.pid=Some(100+i as u32); row.busy=false; row.selected=true;
        row.game_running=Some(Arc::new(AtomicBool::new(true)));
        row.alive=Some(Arc::new(AtomicBool::new(true)));
    }
    assert_eq!(app.update(Message::ShutdownAccount(app.accounts[1].key.clone())).units(),1);
    assert!(app.accounts[1].shutting_down && !app.accounts[0].shutting_down);
}
#[test]
fn game_shutdown_selection_is_scoped_and_stays_signed_out_with_the_setting_off() {
    let mut app=app();
    for (i,row) in app.accounts.iter_mut().enumerate() {
        row.pid=Some(100+i as u32); row.busy=false; row.selected=i==0;
        row.game_running=Some(Arc::new(AtomicBool::new(true)));
        row.alive=Some(Arc::new(AtomicBool::new(true)));
    }
    assert_eq!(app.shutdown_count(true),1);
    assert_eq!(app.update(Message::ShutdownSelected).units(),1);
    assert!(app.accounts[0].shutting_down && !app.accounts[0].can_play());
    assert!(app.accounts[0].resign_declined && app.accounts[0].signed_out);
    let _=app.update(Message::Tick);
    assert!(app.accounts[0].resign_declined && app.accounts[0].signed_out,"the first game-observation tick must preserve explicit shutdown");
    app.accounts[0].pid=None;
    assert!(app.has_sessions() && app.sessions_block_update() && !app.can_restart_launcher(),"POL cleanup remains pending after the game window closes");
    app.accounts[0].pid=Some(100);
    assert!(!app.accounts[1].shutting_down);
    assert_eq!(app.update(Message::ShutdownSelected).units(),0,"a repeated click must not toggle the game countdown");
    assert_eq!(app.update(Message::ShutdownAll).units(),2);
    let key=app.accounts[0].key.clone(); let generation=app.accounts[0].launch_generation;
    let _=app.update(Message::GameShutdownFinished(key,generation,Err("Game refused shutdown".into())));
    assert!(app.accounts[0].pid.is_some() && app.accounts[0].problem && !app.accounts[0].shutting_down);
    assert!(app.accounts[1].shutting_down,"one failure cannot change another client");
    let key=app.accounts[1].key.clone(); let generation=app.accounts[1].launch_generation;
    let _=app.update(Message::GameShutdownFinished(key.clone(),generation,Ok(())));
    let _=app.update(Message::Tick);
    assert!(app.accounts[1].pid.is_none() && !app.accounts[1].problem && !app.accounts[1].shutting_down);
    app.accounts[1].launch_generation+=1; app.accounts[1].pid=Some(999);
    let _=app.update(Message::GameShutdownFinished(key,generation,Ok(())));
    assert_eq!(app.accounts[1].pid,Some(999),"a stale result cannot clear a fresh launch");
}

#[test]
fn close_games_instantly_is_off_by_default_persists_and_picks_the_instant_shutdown() {
    use session_worker::game_shutdown::Mode;
    let root = std::env::temp_dir().join(format!("crystal-instant-close-{}", backend::unique_id().unwrap()));
    let mut saved = app();
    assert!(!saved.settings.close_games_instantly);
    assert!(serde_json::to_value(Settings::default()).unwrap().get("close_games_instantly").is_none(), "older files stay unchanged");
    saved.config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    saved.preview = false;
    let _ = saved.update(Message::CloseGamesInstantly(true));
    assert!(load_settings(&saved.config).unwrap().close_games_instantly, "the switch is saved");
    let _ = saved.update(Message::CloseGamesInstantly(false));
    assert!(!load_settings(&saved.config).unwrap().close_games_instantly);
    drop(saved);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(Mode::from_setting(false), Mode::Graceful);
    assert_eq!(Mode::from_setting(true), Mode::Instant);

    // Shut down (one game, then all) takes the instant path while the switch is on.
    let mut app = app();
    app.settings.close_games_instantly = true;
    for (i, row) in app.accounts.iter_mut().enumerate() {
        row.pid = Some(100 + i as u32); row.busy = false; row.selected = i == 0;
        row.game_running = Some(Arc::new(AtomicBool::new(true)));
        row.alive = Some(Arc::new(AtomicBool::new(true)));
    }
    assert_eq!(app.update(Message::ShutdownSelected).units(), 1);
    assert!(app.accounts[0].shutting_down && app.accounts[0].shutdown_mode == Mode::Instant);
    assert_eq!(app.accounts[0].status_text(), t("Closing FFXI…"));
    app.settings.close_games_instantly = false;
    let others = app.shutdown_count(false);
    assert_eq!(others, app.accounts.iter().filter(|row| row.is_playing()).count() - 1, "the running instant close is not repeated");
    assert_eq!(app.update(Message::ShutdownAll).units(), others);
    assert_eq!(app.accounts[1].shutdown_mode, Mode::Graceful);
    assert_eq!(app.accounts[1].status_text(), t("Shutting down FFXI…"));
    assert_eq!(app.accounts[0].shutdown_mode, Mode::Instant);

    // The ended host reports a normal close: no problem, no crash recovery, no network wait.
    let key = app.accounts[0].key.clone(); let generation = app.accounts[0].launch_generation;
    let _ = app.update(Message::GameShutdownFinished(key, generation, Ok(())));
    let report = launch_log::classify_loader_end(&launch_log::LoaderEnd::Terminated, Duration::from_secs(1));
    assert!(!report.is_problem() && !report.during_startup);
    *app.accounts[0].outcome.get_or_insert_with(Default::default).lock().unwrap() = Some(report.clone());
    app.accounts[0].alive.as_ref().unwrap().store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    let row = &app.accounts[0];
    assert!(row.pid.is_none() && !row.problem && !row.shutting_down);
    assert_eq!(row.status_text(), t("Game closed normally"));
    assert!(row.network_wait.is_none());
    assert_eq!(features::recovery::classify(Some(&report), false, false), features::recovery::EndKind::UserClosed);
}

#[test]
fn reshade_setup_blocks_launch_updates_and_launcher_restart() {
    let mut app = app();
    app.preview = false;
    app.reshade.busy = true;
    assert!(app.launch_blocked());
    assert!(!app.can_restart_launcher());
    assert_eq!(app.main_action(), MainAction::Working);
    let _ = app.run_main_action();
    assert!(app.page == Page::Settings && app.settings_tab == settings_page::Tab::Graphics);
    assert_eq!(app.update(Message::CloseRequested).units(), 0);
    assert_eq!(app.update(Message::ConfirmClose).units(), 0);
    assert_eq!(app.start_maintenance(MaintenanceKind::Update).units(), 0);
    let _ = dgvoodoo::update(&mut app, dgvoodoo::Msg::Toggle(false));
    assert!(app.settings.dgvoodoo);
}

/// Proposal 09 phase 1: an interrupted update no longer refuses Play; it is finished
/// first and the launch goes on, or the rows say why it could not be.
#[test]
fn an_interrupted_update_is_finished_before_the_launch_goes_on() {
    let mut app = app();
    app.preview = false;
    let key = app.accounts[0].key.clone();
    let root = std::env::temp_dir().join(format!("crystal-interrupted-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(root.join(".xi-updater")).unwrap();
    let journal = root.join(".xi-updater").join("transaction.json");
    std::fs::write(&journal, b"{}").unwrap();
    app.settings.install_path = root.to_string_lossy().into_owned();
    app.settings.install_selected = true;
    app.viewer = None;
    // The window's report is no update: Play stays the main button and finishes it.
    app.patch = Some(patch::local_report(&root));
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Interrupted);
    assert_eq!(app.main_action(), MainAction::Play);
    assert_eq!(app.interrupted_updates(std::slice::from_ref(&key), false), vec![root.clone()]);
    // A sign-in only (startup, Connect) never touches a game folder.
    assert!(app.interrupted_updates(std::slice::from_ref(&key), true).is_empty());
    let clicked = Instant::now();
    let held = HeldLaunch { keys: vec![key.clone()], social_only: false, clicked, trigger: None };
    let recovered = |result: Result<(), String>, game: Option<patch::PatchReport>| WorkerEvent::InterruptedUpdate(Box::new(Recovered {
        held: held.clone(), folders: vec![root.clone()], parks_closed: false, result, game, viewer: None }));
    app.recovering = Some(false);
    assert!(app.launch_blocked(), "nothing starts while files are being restored");
    assert!(!app.files_changing(), "a game folder alone leaves other accounts' parked hosts alone");
    app.recovering = Some(true);
    assert!(app.files_changing(), "PlayOnline's folder closes them");
    // No parked host to blame: the refusal is final.
    app.worker_event(recovered(Err("fixture refusal".into()), None));
    assert!(app.recovering.is_none() && app.accounts[0].problem);
    assert!(app.accounts[0].status.contains("fixture refusal"), "{}", app.accounts[0].status);
    // Finished, but another journal appeared meanwhile: reported, never a loop.
    app.launches = Some(Vec::new());
    app.worker_event(recovered(Ok(()), None));
    assert!(app.launches.as_ref().unwrap().is_empty());
    std::fs::remove_file(&journal).unwrap();
    // Finished: the launch goes on, and the "interrupted" report is read again.
    app.worker_event(recovered(Ok(()), Some(patch::local_report(&root))));
    assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![key.clone()], false, clicked, None)]);
    assert_ne!(app.patch.as_ref().unwrap().state, patch::PatchState::Interrupted);
    assert!(app.interrupted_updates(&[key], false).is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

fn refused_report() -> patch::PatchReport {
    patch::PatchReport { state: patch::PatchState::Required, local_version: "30260904_1".into(),
        remote_version: "30261006_0".into(), detail: String::new(), files_verified: None }
}

/// A game folder at `version` (no record: a fresh install), isolated app state under it,
/// and the Settings folder pointing there.
fn play_update_app(label: &str, version: Option<&str>) -> (App, PathBuf) {
    let root = std::env::temp_dir().join(format!("crystal-play-update-{label}-{}", backend::unique_id().unwrap()));
    let game = root.join("FINAL FANTASY XI");
    std::fs::create_dir_all(&game).unwrap();
    for name in ["FFXi.dll", "FFXiMain.dll"] { std::fs::write(game.join(name), b"fixture").unwrap(); }
    if let Some(version) = version { std::fs::write(game.join("patch.ver"), patch::encode_applied_record(version, 1).unwrap()).unwrap(); }
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    app.viewer = None;
    app.config = Config { experiment: root.join("data"), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    app.settings.install_path = game.to_string_lossy().into_owned();
    app.settings.install_selected = true;
    (app, root)
}

/// A row signed in at the PlayOnline main menu (its host `pid`), its Play in flight.
fn signed_in(app: &mut App, index: usize, pid: u32, clicked: Instant) -> String {
    let row = &mut app.accounts[index];
    row.pid = Some(pid);
    row.alive = Some(Arc::new(AtomicBool::new(true)));
    row.game_running = Some(Arc::new(AtomicBool::new(false)));
    row.busy = true;
    row.play_timing = Some(launch_timing::Timeline::begin_at(clicked));
    row.key.clone()
}

fn refused(app: &mut App, key: &str) {
    let message = patch::gate(patch::LaunchCheck::Update(refused_report())).unwrap_err();
    app.worker_event(WorkerEvent::StartGame(key.to_owned(), Some(refused_report()), Err(message)));
}

fn generation(app: &App) -> u64 { app.play_updates.jobs[0].generation }

/// A row signed in and idle at the PlayOnline main menu (its host `pid`).
fn idle_signed_in(app: &mut App, index: usize, pid: u32) -> String {
    let key = signed_in(app, index, pid, Instant::now());
    let row = &mut app.accounts[index];
    (row.busy, row.play_timing) = (false, None);
    key
}

/// A play flag read made now, during maintenance, with `$patch_enable` as given.
fn closed_flag(app: &mut App, patch: Option<bool>) {
    let _ = app.update(Message::PlayFlagLoaded(play_flag::Reading { at: chrono::Utc::now(),
        result: Ok(play_flag::Flag::Down(Some("The game is unavailable for play during maintenance.".into()))), patch }));
}

fn maintenance_signins(app: &App) -> Vec<Vec<String>> {
    app.launches.as_ref().unwrap().iter()
        .filter(|(_, social, _, trigger)| *social && *trigger == Some(launch_timing::LaunchTrigger::MaintenanceSignIn))
        .map(|(keys, ..)| keys.clone()).collect()
}

/// Owner, 1.1.0: "I want it to update as soon as update is available". In maintenance, at
/// `$patch_enable`'s 0 → 1: the accounts Crystal may sign in by itself and a waiting Play
/// without a code sign in (once per trigger; an OTP account never), one W20 runs for the
/// folder from a session at the main menu, "update required" runs the in-session update
/// with no Play waiting on it, and the outcome is installed, game not started.
#[test]
fn maintenance_checks_the_version_and_installs_the_update_without_starting_the_game() {
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("watch", Some("30260904_1"));
    let game = root.join("FINAL FANTASY XI");
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    assert_eq!(app.main_action(), MainAction::Maintenance);
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    app.settings.auto_sign_in = true;
    app.settings.account_no_otp.extend([keys[0].clone(), keys[2].clone()]);
    app.settings.account_otp.insert(keys[1].clone());
    // Plays that wait: an OTP account with its code, and an account that needs none.
    app.accounts[1].otp = "123456".into();
    app.start_launch(vec![keys[1].clone(), keys[2].clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&keys[1]) && app.is_waiting(&keys[2]));
    // patch_enable 0: nothing signs in and nothing is checked.
    closed_flag(&mut app, Some(false));
    assert!(app.launches.as_ref().unwrap().is_empty() && app.patch_watch.checks.is_empty());
    // The flip: one maintenance sign-in, never the OTP account; not again in this cycle.
    closed_flag(&mut app, Some(true));
    assert_eq!(maintenance_signins(&app), vec![vec![keys[0].clone(), keys[2].clone()]]);
    closed_flag(&mut app, Some(true));
    let _ = app.update(Message::Tick);
    assert_eq!(maintenance_signins(&app).len(), 1, "once per trigger");
    // Signed in at the main menu: one W20 for the folder.
    idle_signed_in(&mut app, 0, 41);
    let _ = app.update(Message::Tick);
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 1, "one check per folder per trigger");
    assert!(play_update::same_folder(&app.patch_watch.checks[0].0, &game) && app.patch_watch.checks[0].1 == keys[0]);
    assert_eq!(play_update::watch_line(&app, &keys[0]).unwrap().headline, "Checking for the FFXI update");
    // Update required: the update a Play's W20 would start, with no Play waiting on it.
    app.worker_event(WorkerEvent::PatchCheck(app.patch_watch.checks[0].0.clone(), keys[0].clone(), Ok((refused_report(), false, None))));
    assert_eq!(app.play_updates.spawned.len(), 1);
    assert!(app.play_updates.jobs[0].probe && app.play_updates.jobs[0].waiters.is_empty() && !app.accounts[0].busy);
    assert_eq!(play_update::watch_line(&app, &keys[0]).unwrap().headline, "Downloading the FFXI update");
    let row = account_row::row_view(&app, &app.accounts[0]);
    assert!(row.detail.starts_with("Downloading the FFXI update") && matches!(row.action, Some((_, Message::CancelPlayUpdate(_), _))));
    let job = generation(&app);
    app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Finished(Ok(play_update::Done { summary: "Updated.".into(), report: None }))));
    assert!(app.play_updates.jobs.is_empty() && app.w20_skipped.is_empty());
    assert_eq!(app.launches.as_ref().unwrap().iter().filter(|(_, social, ..)| !social).count(), 0, "installed, game not started");
    assert_eq!(play_update::watch_line(&app, &keys[0]).unwrap().headline, "Update installed · waiting for servers");
    assert_eq!(app.status, "Update installed · waiting for servers");
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 1, "nothing more once installed");
    // The worlds open: the waiting Plays go through the normal start path (W20 → GameStart).
    let _ = app.update(Message::PlayFlagLoaded(play_flag::Reading { at: chrono::Utc::now(), result: Ok(play_flag::Flag::Up), patch: Some(true) }));
    assert!(!app.patch_watch.active() && play_update::watch_line(&app, &keys[0]).is_none());
    let plays: Vec<_> = app.launches.as_ref().unwrap().iter().filter(|(_, social, ..)| !social).collect();
    assert_eq!(plays.len(), 1, "the waiting Plays start together through the normal start path");
    assert_eq!(plays[0].0, vec![keys[1].clone(), keys[2].clone()]);
    let history = std::fs::read_to_string(app.config.history_path()).unwrap();
    for line in ["play flag: $patch_enable unknown → 0", "play flag: $patch_enable 0 → 1", "maintenance sign-in: 2 accounts",
        "maintenance update check (W20): update required, installed 30260904_1, official 30261006_0",
        "maintenance update: started, official 30261006_0", "maintenance update: installed 30261006_0: Updated."] {
        assert!(history.contains(line), "{line}\n{history}");
    }
    for row in &app.accounts {
        for secret in [&row.account.label, &row.account.pol_id, &row.account.se_id].into_iter().filter(|text| text.len() > 2) {
            assert!(!history.contains(secret.as_str()), "no account names in the history");
        }
    }
    let _ = std::fs::remove_dir_all(root);
}

/// The schedule in the window: nothing outside maintenance, at once at first sight with a
/// session at the main menu, then 5 min ± 30 s after "current", none while
/// `$patch_enable` is 0; a session maintenance ended signs in again once per trigger.
#[test]
fn maintenance_version_checks_follow_the_schedule_and_never_run_outside_maintenance() {
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("watch-schedule", Some("30261006_0"));
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    app.settings.auto_sign_in = true;
    app.settings.account_no_otp.extend([keys[0].clone(), keys[1].clone()]);
    let key = idle_signed_in(&mut app, 0, 51);
    // Outside maintenance: no flag read is wanted, and a 1 changes nothing.
    status_read(&mut app, services::Availability::Operational, None);
    assert!(!app.flag_wanted());
    let _ = app.update(Message::PlayFlagLoaded(play_flag::Reading { at: chrono::Utc::now(), result: Ok(play_flag::Flag::Up), patch: Some(true) }));
    let _ = app.update(Message::Tick);
    assert!(app.patch_watch.checks.is_empty() && !app.patch_watch.active() && maintenance_signins(&app).is_empty());
    // Maintenance first seen with patch_enable 1 and a session at the main menu: at once.
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    app.play_flag = play_flag::Known::default();
    assert!(app.flag_wanted());
    closed_flag(&mut app, Some(true));
    assert_eq!(app.patch_watch.checks.len(), 1);
    assert_eq!(maintenance_signins(&app), vec![vec![keys[1].clone()]], "the account not signed in signs in");
    let folder = app.patch_watch.checks[0].0.clone();
    let current = patch::PatchReport { state: patch::PatchState::Current, local_version: "30261006_0".into(),
        remote_version: "30261006_0".into(), detail: String::new(), files_verified: None };
    app.worker_event(WorkerEvent::PatchCheck(folder.clone(), key.clone(), Ok((current.clone(), true, Some("connect 180 ms".into())))));
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 1, "not before the next 5 minutes");
    let next = app.patch_watch.next_due().unwrap();
    let now = Instant::now();
    assert!(next > now + patch_watch::RECHECK - patch_watch::JITTER - Duration::from_secs(5) && next <= now + patch_watch::RECHECK + patch_watch::JITTER);
    assert!(app.next_timed_work().is_some_and(|at| at <= next), "a quiet window wakes for it");
    app.patch_watch.checked(&folder, false, now - patch_watch::RECHECK * 2, Duration::ZERO);
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 2, "due again");
    assert!(app.play_updates.spawned.is_empty(), "current never updates");
    // A check that sent nothing (the session left its main menu) waits for the schedule too.
    app.worker_event(WorkerEvent::PatchCheck(folder.clone(), key.clone(), Err("PlayOnline did not return to its main menu in time, so FFXI was not started.".into())));
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 2, "no tight loop");
    // patch_enable 0: no checks, however long.
    closed_flag(&mut app, Some(false));
    app.patch_watch.checked(&folder, false, now - patch_watch::RECHECK * 2, Duration::ZERO);
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 2);
    // Back to 1 (a flip): checked at once, and the account whose session maintenance ended
    // (it never signed in here) signs in once more.
    closed_flag(&mut app, Some(true));
    assert_eq!(app.patch_watch.checks.len(), 3);
    assert_eq!(maintenance_signins(&app).len(), 2);
    closed_flag(&mut app, Some(true));
    assert_eq!(maintenance_signins(&app).len(), 2, "once per trigger");
    let _ = std::fs::remove_dir_all(root);
}

/// Review findings: an answer that comes once `$patch_enable` is no longer 1 starts no
/// update; "update required" with no update possible stops the folder until the next flip;
/// a waiting Play whose account's OTP use is unknown is not signed in for the check.
#[test]
fn a_late_or_unusable_update_required_starts_nothing_and_stops_further_checks() {
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("watch-late", Some("30260904_1"));
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    app.start_launch(vec![keys[2].clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&keys[2]));
    idle_signed_in(&mut app, 0, 61);
    closed_flag(&mut app, Some(true));
    assert!(maintenance_signins(&app).is_empty(), "OTP use unknown: not signed in");
    assert_eq!(app.patch_watch.checks.len(), 1);
    let folder = app.patch_watch.checks[0].0.clone();
    // $patch_enable 0 before the answer: no update.
    closed_flag(&mut app, Some(false));
    app.worker_event(WorkerEvent::PatchCheck(folder.clone(), keys[0].clone(), Ok((refused_report(), false, None))));
    assert!(app.play_updates.spawned.is_empty() && app.play_updates.jobs.is_empty());
    // The flip checks at once; "required" with no update possible (the host is gone).
    closed_flag(&mut app, Some(true));
    assert_eq!(app.patch_watch.checks.len(), 2);
    app.accounts[0].pid = None;
    app.worker_event(WorkerEvent::PatchCheck(folder.clone(), keys[0].clone(), Ok((refused_report(), false, None))));
    assert!(app.play_updates.spawned.is_empty());
    let later = Instant::now() + patch_watch::RECHECK * 10;
    assert!(!app.patch_watch.due(&folder, later), "no more W20s with no download");
    closed_flag(&mut app, Some(false));
    closed_flag(&mut app, Some(true));
    assert!(app.patch_watch.due(&folder, Instant::now()), "until the next flip");
    let history = std::fs::read_to_string(app.config.history_path()).unwrap();
    assert!(history.contains("maintenance update: not started") && !history.contains("maintenance update: started"), "{history}");
    let _ = std::fs::remove_dir_all(root);
}

/// Review finding: one W20 at a time on a session. Two sessions of one folder give one
/// check; a Play on the session with a check out (a wait maintenance's end releases, or a
/// click) waits for its answer, then starts once.
#[test]
fn a_play_waits_for_the_check_out_on_its_session() {
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("watch-overlap", Some("30261006_0"));
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    closed_flag(&mut app, Some(false));
    idle_signed_in(&mut app, 0, 71);
    idle_signed_in(&mut app, 1, 72);
    app.start_launch(vec![keys[0].clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&keys[0]));
    closed_flag(&mut app, Some(true));
    let _ = app.update(Message::Tick);
    assert_eq!(app.patch_watch.checks.len(), 1, "one check per folder");
    let (folder, key) = app.patch_watch.checks[0].clone();
    assert!(app.patch_watch.in_flight(&key));
    let plays = |app: &App| app.launches.as_ref().unwrap().iter().filter(|(keys, social, ..)| !social && keys.contains(&key)).count();
    // The worlds open while it is out: the wait is not released on that session yet.
    let _ = app.update(Message::PlayFlagLoaded(play_flag::Reading { at: chrono::Utc::now(), result: Ok(play_flag::Flag::Up), patch: Some(true) }));
    assert!(!app.patch_watch.active() && app.patch_watch.in_flight(&key));
    let _ = app.update(Message::Tick);
    assert_eq!(plays(&app), 0);
    assert!(key == keys[0] && app.is_waiting(&key));
    // A click there waits too.
    let _ = app.update(Message::CancelWait(vec![keys[0].clone()]));
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    let _ = app.update(Message::Tick);
    assert_eq!(plays(&app), 0, "no second W20 on the session");
    assert!(app.accounts.iter().any(|row| row.key == key && row.queued_play.is_some()));
    // The answer: then it starts, once.
    let current = patch::PatchReport { state: patch::PatchState::Current, local_version: "30261006_0".into(),
        remote_version: "30261006_0".into(), detail: String::new(), files_verified: None };
    app.worker_event(WorkerEvent::PatchCheck(folder, key.clone(), Ok((current, true, None))));
    let _ = app.update(Message::Tick);
    assert_eq!(plays(&app), 1);
    let _ = std::fs::remove_dir_all(root);
}

/// A Friends request past its time limit is reported as stalled, but a later read that finds
/// it done (the loader's mail deadlines are longer) counts it: a send relists Sent.
#[test]
fn a_friends_result_after_its_time_limit_still_counts() {
    let mut app = app();
    app.friends.pending = Some(("req-1".into(), Instant::now() - FRIENDS_REQUEST_LIMIT - Duration::from_secs(1)));
    app.friends.mail.listed[2] = Some(Vec::new());
    app.expire_friends_request();
    assert!(app.friends.pending.is_none() && app.friends.mail.stalled && !app.friends.error.is_empty());
    assert_eq!(app.friends_expired.as_deref(), Some("req-1"));
    let mut snapshot = friends::Snapshot::default();
    snapshot.social.id = Some("req-1".into());
    snapshot.social.action = Some(crate::social::Action::Send);
    app.friends_read(true, Some(snapshot));
    assert!(app.friends_expired.is_none() && !app.friends.mail.stalled && app.friends.error.is_empty());
    assert!(app.friends.mail.listed[2].is_none(), "Sent is listed again");
}

/// An expired send's draft no longer shows as sending (Send stays usable) once another
/// request replaces the expired one, which can then no longer be matched.
#[test]
fn a_new_friends_request_drops_an_expired_sends_draft() {
    let mut app = app();
    app.friends.pending = Some(("req-1".into(), Instant::now() - FRIENDS_REQUEST_LIMIT - Duration::from_secs(1)));
    app.friends.outgoing = Some(friends::Outgoing { uid: 7, subject: String::new(), message: "hi".into(), sent: false });
    app.expire_friends_request();
    assert!(app.friends.outgoing.is_some(), "kept while the late result may still match");
    app.friends_request_started("req-2".into(), None);
    assert!(app.friends_expired.is_none() && app.friends.outgoing.is_none());
    assert_eq!(app.friends.pending.as_ref().map(|(id, _)| id.as_str()), Some("req-2"));
    // A draft already sent is not touched.
    app.friends_expired = Some("req-2".into());
    app.friends.outgoing = Some(friends::Outgoing { uid: 7, subject: String::new(), message: "hi".into(), sent: true });
    app.friends_request_started("req-3".into(), None);
    assert!(app.friends.outgoing.as_ref().is_some_and(|o| o.sent));
}

/// Remove confirmed while a sync request is in flight: nothing goes out, the bar stays
/// open with its reason, and conversation sync waits (so the reason is not cleared).
#[test]
fn remove_confirmed_during_a_sync_request_waits_with_the_bar_open() {
    let mut app = app();
    app.preview = false;
    app.friends.pending = Some(("sync-1".into(), Instant::now()));
    let friend = friends::Friend { name: "Ayame".into(), alternate: false, slot: 1, uid: Some(9), online: None, location: None, character: None, portrait: None };
    let _ = app.update(Message::FriendsRemove(friend));
    let _ = app.update(Message::FriendsRemoveConfirm);
    assert!(app.friends.remove.is_some());
    assert_eq!(app.friends.pending.as_ref().map(|(id, _)| id.as_str()), Some("sync-1"));
    assert_eq!(app.friends.error, t("Wait for the current PlayOnline request to finish."));
    app.friends.pending = None;
    assert!(app.friends.next_sync().is_none(), "sync waits for the open Remove");
    let _ = app.drive_sync();
    assert_eq!(app.friends.error, t("Wait for the current PlayOnline request to finish."));
}

/// A row's note follows the language and is removed by what it is: a language switch while
/// it signs back in after its game no longer leaves the old language's note behind.
#[test]
fn a_row_note_follows_the_language_and_goes_after_a_switch() {
    let mut app = app();
    app.accounts[0].status = t("Ready to launch").into();
    app.accounts[0].set_note(Some(RowNote::ResignAfterGame));
    let _ = app.update(Message::SetLanguage(i18n::Lang::Ja));
    let shown = app.accounts[0].status_text().into_owned();
    assert_eq!(shown, format!("{} · {}", t("Ready to launch"), t("Signing back in after the game closed.")));
    app.accounts[0].drop_resign();
    assert_eq!(app.accounts[0].status_text(), t("Ready to launch"));
    // A status that replaces the one the note was added to ends the note too.
    app.accounts[0].set_note(Some(RowNote::NetworkGone));
    app.accounts[0].status = t("Client closed").into();
    assert_eq!(app.accounts[0].status_text(), t("Client closed"));
    i18n::set(i18n::Lang::En);
}

/// A Play with nothing launchable before the first status read holds nothing and leaves no
/// "checking" line behind that nothing would clear.
#[test]
fn an_empty_play_before_the_first_status_read_is_not_held() {
    let mut app = app();
    app.preview = false;
    app.status_read_done = false;
    app.status = "Ready.".into();
    app.start_launch(Vec::new(), false, Instant::now(), None);
    assert!(app.held_plays.is_empty());
    assert_eq!(app.status, "Ready.");
}

/// The W20 skip after an in-session update is one-shot: a start that waits (here for a
/// maintenance that began meanwhile) runs its own W20 when it is released later.
#[test]
fn a_play_update_skip_does_not_outlive_a_start_that_waits() {
    let (mut app, root) = play_update_app("skip-wait", Some("30260904_1"));
    let clicked = Instant::now();
    let key = signed_in(&mut app, 0, 101, clicked);
    refused(&mut app, &key);
    let job = generation(&app);
    status_read(&mut app, services::Availability::Maintenance, None);
    app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Finished(Ok(play_update::Done { summary: "Updated.".into(), report: None }))));
    assert!(app.is_waiting(&key), "the release waits for FFXI");
    assert!(app.w20_skipped.is_empty(), "no skip survives the wait");
    status_read(&mut app, services::Availability::Operational, None);
    app.play_flag.note(play_flag::Reading { at: chrono::Utc::now(), result: Ok(play_flag::Flag::Up), patch: None });
    app.release_waiting_plays(true);
    assert_eq!(app.launches.as_ref().unwrap().last().map(|launch| launch.0.clone()), Some(vec![key.clone()]));
    assert!(app.w20_skipped.is_empty(), "the released Play runs its own W20");
    std::fs::remove_dir_all(root).unwrap();
}

/// Proposal 09 phase 2: the W20 of a signed-in Play says "update". The account stays signed
/// in while its folder updates, using that W20's verdict (no second version request), another
/// account of the folder that the W20 refused joins the same job, and both start through the
/// signed-in path with no version check once it is installed.
#[test]
fn a_refused_play_updates_in_its_session_then_starts_without_a_second_check() {
    let (mut app, root) = play_update_app("session", Some("30260904_1"));
    let game = root.join("FINAL FANTASY XI");
    let clicked = Instant::now() - Duration::from_secs(3);
    let first = signed_in(&mut app, 0, 101, clicked);
    let second = signed_in(&mut app, 1, 102, clicked);
    refused(&mut app, &first);
    // One job, for the account's own folder, on the W20's official version.
    assert_eq!(app.play_updates.spawned.len(), 1);
    let (folder, official, closed) = &app.play_updates.spawned[0];
    assert!(play_update::same_folder(folder, &game) && official == "30261006_0" && closed.is_none());
    assert_eq!(app.main_action(), MainAction::Play, "no Update button: Play is the update");
    assert!(app.refused_plays.is_empty(), "nothing waits for a Downloads update");
    let row = &app.accounts[0];
    assert!(row.busy && !row.problem && row.pid == Some(101), "still signed in: {}", row.status);
    let line = crate::play_update::line(&app, &first).unwrap();
    assert!(line.headline.starts_with(t("Updating FINAL FANTASY XI")), "{}", line.headline);
    assert_eq!(line.cancel, Some(generation(&app)));
    // The second account's W20 asked for the same update: it waits for this one.
    refused(&mut app, &second);
    assert_eq!(app.play_updates.spawned.len(), 1, "one job per folder");
    assert_eq!(app.play_updates.jobs[0].waiters.iter().map(|w| w.key.as_str()).collect::<Vec<_>>(), [first.as_str(), second.as_str()]);
    // A third account's Play now waits at its main menu, before its own version check.
    let third = app.accounts[2].key.clone();
    {
        let row = &mut app.accounts[2];
        (row.pid, row.alive, row.game_running) = (Some(103), Some(Arc::new(AtomicBool::new(true))), Some(Arc::new(AtomicBool::new(false))));
    }
    app.launches = None;
    let pressed = Instant::now();
    app.start_launch_now(vec![third.clone()], false, pressed, None, false);
    app.launches = Some(Vec::new());
    assert_eq!(app.accounts[2].queued_play, Some(pressed));
    assert!(matches!(app.play_update_of(&third), Some((_, false))));
    assert!(crate::play_update::line(&app, &third).unwrap().headline.starts_with("Waiting for the update"));
    app.start_queued_plays();
    assert!(app.launches.as_ref().unwrap().is_empty() && app.accounts[2].queued_play.is_some(), "held while the folder updates");
    // Downloaded; a game on another account still runs from the folder: it waits for it.
    let job = generation(&app);
    app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Waiting(Some("Alice".into()))));
    let line = crate::play_update::line(&app, &first).unwrap();
    assert!(line.headline.contains("Alice") && line.label == t("Update ready"), "{}", line.headline);
    app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Installing));
    // Installed: both start, signed in, without another W20.
    app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Finished(Ok(play_update::Done {
        summary: "3 files updated. Applied version 30261006_0.".into(), report: None }))));
    assert!(app.play_updates.jobs.is_empty());
    let reissued = Some(launch_timing::LaunchTrigger::ReissuedPlay);
    assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![first.clone()], false, clicked, reissued), (vec![second.clone()], false, clicked, reissued)]);
    assert_eq!(app.w20_skipped, [first.clone(), second.clone()], "both start without a second W20");
    assert!(app.accounts.iter().take(2).all(|row| row.pid.is_some()), "nobody signed out");
    // The account that waited before its W20 is released now, with its own check.
    app.start_queued_plays();
    assert_eq!(app.launches.as_ref().unwrap().last().unwrap().0, vec![third.clone()]);
    assert!(!app.w20_skipped.contains(&third));
    // The signed-in start with the skip (its thread finds no live session here).
    app.launches = None;
    app.accounts[0].busy = false;
    app.settings.loader_path = root.join("no-loader.exe").to_string_lossy().into_owned();
    app.start_launch_now(vec![first.clone()], false, clicked, reissued, true);
    // Wait for that start thread to finish (it writes into `root`) before removing it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match app.receiver.recv_timeout(left) {
            Ok(WorkerEvent::Launch(key, _, event)) if key == first && matches!(*event, WorkerEvent::StartGame(..)) => break,
            Ok(_) => {}
            Err(error) => panic!("the signed-in start did not finish: {error}"),
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// Cancel and a network drop pause the update: the rows say Play continues it (its verified
/// downloads stay in the folder's cache), nothing starts, and a Play that waited stops waiting.
#[test]
fn a_cancelled_or_failed_play_update_is_paused_and_play_continues_it() {
    for cancel in [true, false] {
        let (mut app, root) = play_update_app("pause", None);
        let clicked = Instant::now();
        let key = signed_in(&mut app, 0, 101, clicked);
        refused(&mut app, &key);
        assert_eq!(app.play_updates.spawned.len(), 1, "a fresh install updates in its session too");
        let job = generation(&app);
        if cancel {
            let _ = app.update(Message::CancelPlayUpdate(job));
            assert!(app.play_updates.jobs[0].op.cancelled());
            assert_eq!(crate::play_update::line(&app, &key).unwrap().cancel, None);
        }
        let error = if cancel { t("Cancelled. Verified downloads are kept for resuming.").to_owned() } else { "Patch server timed out".to_owned() };
        app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Finished(Err(error))));
        let row = &app.accounts[0];
        assert!(!row.busy && row.status == t("Update paused. Press Play to continue.") && row.problem != cancel);
        assert!(row.pid.is_some() && row.can_play(), "still signed in; Play continues it");
        assert!(app.launches.as_ref().unwrap().is_empty() && app.play_updates.jobs.is_empty());
        assert!(cancel || app.status.contains("Patch server timed out"), "{}", app.status);
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A signed-in host that holds a game file (its install could not proceed) was signed out:
/// that one account signs in again after the install (one new sign-in, still one click). An
/// account with a one-time code asks for a new one instead.
#[test]
fn an_account_whose_host_held_a_file_signs_in_again_after_the_install() {
    for otp in [false, true] {
        let (mut app, root) = play_update_app("fallback", Some("30260904_1"));
        let clicked = Instant::now();
        let key = signed_in(&mut app, 0, 101, clicked);
        if otp { app.settings.account_otp.insert(key.clone()); }
        refused(&mut app, &key);
        let job = generation(&app);
        app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::SignedOut(key.clone())));
        assert!(app.play_updates.jobs[0].waiters[0].fallback);
        // Its host ended with the sign-out.
        app.accounts[0].alive.as_ref().unwrap().store(false, Ordering::Release);
        let _ = app.update(Message::Tick);
        assert!(app.accounts[0].pid.is_none() && crate::play_update::line(&app, &key).is_some(), "the row still shows the update");
        app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Finished(Ok(play_update::Done { summary: "Updated.".into(), report: None }))));
        assert!(!app.w20_skipped.contains(&key), "a new sign-in checks its version");
        if otp {
            assert!(app.launches.as_ref().unwrap().is_empty());
            assert!(app.accounts[0].problem && app.accounts[0].status == t("FINAL FANTASY XI is updated. Enter a new one-time code, then press Play."), "{}", app.accounts[0].status);
        } else {
            assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![key.clone()], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay))]);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// A batch reports one account's update refusal to every account. One whose own folder is
/// another, already at that version, starts on its own; one without a session, or a report
/// without a usable official version, keeps the refusal (nothing to update in-session).
#[test]
fn only_folders_the_refusal_applies_to_update_and_a_current_batch_account_starts() {
    let (mut app, root) = play_update_app("batch", Some("30260904_1"));
    let current = root.join("current");
    std::fs::create_dir_all(&current).unwrap();
    for name in ["FFXi.dll", "FFXiMain.dll"] { std::fs::write(current.join(name), b"fixture").unwrap(); }
    std::fs::write(current.join("patch.ver"), patch::encode_applied_record("30261006_0", 1).unwrap()).unwrap();
    app.settings.account_installs.insert(account_key(&app.accounts[1].account), current.to_string_lossy().into_owned());
    let clicked = Instant::now();
    let (first, second) = (signed_in(&mut app, 0, 101, clicked), signed_in(&mut app, 1, 102, clicked));
    let message = patch::gate(patch::LaunchCheck::Update(refused_report())).unwrap_err();
    for key in [&first, &second] {
        app.accounts.iter_mut().find(|row| row.key == **key).unwrap().batch_requested = true;
        app.worker_event(WorkerEvent::Complete((*key).clone(), Err(message.clone())));
    }
    assert_eq!(app.play_updates.spawned.len(), 1);
    assert!(app.play_updates.jobs[0].has(&first) && !app.play_updates.jobs[0].has(&second));
    assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![second.clone()], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay))]);
    // No official version in the report (an unreadable record sends no W20): no job.
    let (mut app, other) = play_update_app("no-version", Some("30260904_1"));
    let key = signed_in(&mut app, 0, 101, clicked);
    let mut report = refused_report();
    report.remote_version = t("Unavailable").into();
    app.worker_event(WorkerEvent::StartGame(key.clone(), Some(report), Err("Update required".into())));
    assert!(app.play_updates.spawned.is_empty() && app.accounts[0].problem && !app.accounts[0].busy);
    std::fs::remove_dir_all(other).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// The sign-in error of a W2U that says PlayOnline is out of date, as a session worker
/// reports it (the owner's launch history, 2026-11-09 14:20).
fn viewer_refusal() -> String {
    format!("Game loader error during startup: {} (Ashita v4 via crystal-loader).",
        viewer::ConnectVerdict::Outdated { local: "20110829_E".into(), remote: "20261022_0".into() }.message())
}

/// `index`'s sign-in in flight (started by `trigger`, a Play queued on it at `queued`) is
/// stopped by its W2U.
fn refuse_for_viewer(app: &mut App, index: usize, trigger: launch_timing::LaunchTrigger, queued: Option<Instant>) {
    let row = &mut app.accounts[index];
    (row.busy, row.preparing_session, row.presign_login) = (true, trigger.is_presign(), trigger.is_presign());
    (row.launch_trigger, row.queued_play) = (Some(trigger), queued);
    let key = row.key.clone();
    app.worker_event(WorkerEvent::Complete(key, Err(viewer_refusal())));
}

/// Play is the PlayOnline update too. A Play its W2U stopped updates PlayOnline by itself
/// (PlayOnline only, on that W2U's answer: no version request of its own), its row shows
/// the update, another account's Play meanwhile waits for the same update, and its success
/// starts both again. A game running elsewhere says why it can't run now, and an update
/// that installed the version the W2U still asks for is never started twice.
#[test]
fn an_outdated_playonline_sign_in_updates_playonline_from_that_play_then_plays() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer", None);
    app.patch = None;
    let key = app.accounts[0].key.clone();
    // Another account in game: PlayOnline can't update now; the row says so, nothing waits.
    app.accounts[1].pid = Some(55);
    app.accounts[1].game_running = Some(Arc::new(AtomicBool::new(true)));
    let clicked = Instant::now();
    refuse_for_viewer(&mut app, 0, LaunchTrigger::Play, Some(clicked));
    assert!(app.maintenance.is_none() && app.refused_plays.is_empty());
    assert!(app.accounts[0].problem && app.accounts[0].status.contains("Close FINAL FANTASY XI"), "{}", app.accounts[0].status);
    let shown = app.viewer.as_ref().unwrap();
    assert!(shown.requires_update());
    assert_eq!((shown.version.local_version.as_str(), shown.version.remote_version.as_str()), ("20110829_E", "20261022_0"));
    assert_eq!(app.main_action(), MainAction::Play);
    // Nothing else signed in: the same refusal updates PlayOnline by itself, PlayOnline only.
    (app.accounts[1].pid, app.accounts[1].game_running) = (None, None);
    refuse_for_viewer(&mut app, 0, LaunchTrigger::Play, Some(clicked));
    assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
    assert_eq!(app.update_scope, (true, false));
    assert_eq!(app.maintenance_label(), t("Updating PlayOnline"));
    assert_eq!(app.play_updates.viewer_started, [Some("20261022_0".to_owned())], "on the W2U's answer");
    assert_eq!(app.play_updates.tasks.len(), 1, "the Tick returns its task");
    assert!(app.launch_blocked(), "nothing signs in while PlayOnline's files change");
    // The row shows the update and its progress, not the refusal.
    assert_eq!(account_row::row_state(&app, &app.accounts[0]), account_row::RowState::ViewerUpdating);
    let row = account_row::row_view(&app, &app.accounts[0]);
    assert!(row.label.starts_with("Updating PlayOnline") && matches!(row.action, Some((_, Message::CancelMaintenance, false))), "{}", row.label);
    assert!(!app.accounts[0].problem);
    // Another account's Play meanwhile waits for this update instead of the same refusal.
    let other = app.accounts[1].key.clone();
    let pressed = Instant::now();
    app.start_launch(vec![other.clone()], false, pressed, None);
    assert!(app.launches.as_ref().unwrap().is_empty() && app.viewer_update_of(&other) == Some(true));
    assert!(!app.can_restart_launcher());
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    let reissued = Some(LaunchTrigger::ReissuedPlay);
    assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![key.clone()], false, clicked, reissued), (vec![other.clone()], false, pressed, reissued)]);
    assert_eq!(app.play_updates.viewer_started.len(), 1, "one update for both");
    assert!(app.reissue_after_update.is_empty() && app.viewer_update_of(&key).is_none());
    // That update installed the version this W2U still asks for: no second update, no loop.
    refuse_for_viewer(&mut app, 0, LaunchTrigger::ReissuedPlay, None);
    assert!(app.maintenance.is_none() && app.refused_plays.is_empty() && app.play_updates.viewer_started.len() == 1);
    assert!(app.accounts[0].problem && app.accounts[0].status.contains("PlayOnline update required"), "{}", app.accounts[0].status);
    std::fs::remove_dir_all(root).unwrap();
}

/// Live 2026-11-09 14:20: a multi-account Play whose first sign-ins the W2U stopped while
/// the other accounts were still signing in. Each refusal found those busy, said "Close
/// FINAL FANTASY XI on every account", and PlayOnline was never updated. Now the refused
/// accounts wait for the others' own results, PlayOnline updates once, and the accounts of
/// that Play start together again.
#[test]
fn the_accounts_of_one_play_share_one_playonline_update() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer-batch", None);
    app.patch = None;
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    // What start_together leaves on the rows.
    let clicked = Instant::now() - Duration::from_secs(2);
    for row in &mut app.accounts {
        (row.busy, row.batch_pending, row.batch_requested) = (true, true, true);
        row.launch_trigger = Some(LaunchTrigger::BatchPlay);
        row.play_timing = Some(launch_timing::Timeline::begin_at(clicked));
    }
    app.worker_event(WorkerEvent::Complete(keys[0].clone(), Err(viewer_refusal())));
    // The other two are still signing in: the update waits for their own verdicts.
    assert!(app.maintenance.is_none() && app.play_updates.viewer_started.is_empty());
    assert_eq!(app.viewer_update_of(&keys[0]), Some(false));
    let row = account_row::row_view(&app, &app.accounts[0]);
    assert!(row.label == "Waiting to update PlayOnline…" && !app.accounts[0].problem, "{}", row.label);
    let _ = app.update(Message::Tick);
    assert!(app.maintenance.is_none(), "a Tick starts nothing while they sign in");
    app.worker_event(WorkerEvent::Complete(keys[1].clone(), Err(viewer_refusal())));
    assert!(app.maintenance.is_none() && app.refused_plays.len() == 2);
    // The third never reached its W2U (its loader did not connect): its own failure stays
    // on its row and ends the wait.
    app.worker_event(WorkerEvent::Complete(keys[2].clone(), Err("crystal-loader did not connect within 60 s.".into())));
    assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
    assert_eq!(app.play_updates.viewer_started, [Some("20261022_0".to_owned())]);
    assert!(app.accounts[2].problem && app.viewer_update_of(&keys[2]).is_none());
    assert!(app.accounts[..2].iter().all(|row| account_row::row_state(&app, row) == account_row::RowState::ViewerUpdating));
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    assert_eq!(app.launches.as_ref().unwrap().as_slice(),
        &[(vec![keys[0].clone(), keys[1].clone()], false, clicked, Some(LaunchTrigger::ReissuedPlay))], "together again, on the Play's click");
    assert_eq!(app.play_updates.viewer_started.len(), 1, "one update for the install they share");
    let history = std::fs::read_to_string(app.config.history_path()).unwrap();
    for line in ["PlayOnline update: started for 2 sign-ins, official 20261022_0", "PlayOnline update: installed; starting 2 sign-ins again"] {
        assert!(history.contains(line), "{line}\n{history}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// The sign-ins Crystal starts by itself (at its start: the owner's five rows at 14:20:01)
/// are stopped by the same W2U: PlayOnline updates from them too, and they sign in again.
/// A Play pressed on one of them meanwhile is that row's start instead; an explicit sign-in
/// (Connect) signs in again as one. An account that takes a typed code asks for a new one.
#[test]
fn crystals_own_sign_ins_and_connect_update_playonline_too() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer-presign", None);
    app.patch = None;
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    app.settings.auto_sign_in = true;
    app.settings.account_no_otp.extend(keys.iter().cloned());
    for index in 0..3 {
        let row = &mut app.accounts[index];
        (row.busy, row.preparing_session, row.presign_login, row.launch_trigger) = (true, true, true, Some(LaunchTrigger::StartupSignIn));
    }
    for key in &keys { app.worker_event(WorkerEvent::Complete(key.clone(), Err(viewer_refusal()))); }
    assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
    assert_eq!(app.play_updates.viewer_started.len(), 1, "one update for the three");
    assert!(app.settings.account_otp.is_empty(), "a version stop teaches nothing about one-time codes");
    let pressed = Instant::now();
    app.start_launch(vec![keys[0].clone()], false, pressed, None);
    assert!(app.launches.as_ref().unwrap().is_empty());
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    let started: Vec<_> = app.launches.as_ref().unwrap().iter().map(|(keys, social, _, trigger)| (keys.clone(), *social, *trigger)).collect();
    assert_eq!(started, [(vec![keys[0].clone()], false, Some(LaunchTrigger::ReissuedPlay)),
        (vec![keys[1].clone(), keys[2].clone()], true, Some(LaunchTrigger::StartupSignIn))]);
    assert_eq!(app.launches.as_ref().unwrap()[0].2, pressed);
    // Connect, on an account whose sign-ins take a typed code: a new code is needed.
    app.launches = Some(Vec::new());
    app.viewer_installed = None;
    app.settings.account_otp.insert(keys[1].clone());
    refuse_for_viewer(&mut app, 0, LaunchTrigger::Connect, None);
    assert!(app.maintenance == Some(MaintenanceKind::Update));
    app.start_launch(vec![keys[1].clone()], true, Instant::now(), None);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    let started: Vec<_> = app.launches.as_ref().unwrap().iter().map(|(keys, social, _, trigger)| (keys.clone(), *social, *trigger)).collect();
    assert_eq!(started, [(vec![keys[0].clone()], true, Some(LaunchTrigger::Connect))]);
    assert!(app.accounts[1].problem && app.accounts[1].status == "PlayOnline is updated. Enter a new one-time code, then press Play.", "{}", app.accounts[1].status);
    std::fs::remove_dir_all(root).unwrap();
}

/// The update as soon as it is out (patch_watch): its maintenance sign-ins are stopped by
/// the W2U when PlayOnline itself was updated. PlayOnline updates, the watch signs the
/// accounts in again, and a Play that waits for FFXI keeps waiting.
#[test]
fn a_maintenance_sign_in_stopped_for_playonline_updates_it_then_signs_in_again() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer-watch", Some("30260904_1"));
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    app.settings.auto_sign_in = true;
    app.settings.account_no_otp.extend([keys[0].clone(), keys[2].clone()]);
    app.start_launch(vec![keys[2].clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&keys[2]));
    closed_flag(&mut app, Some(true));
    assert_eq!(maintenance_signins(&app), vec![vec![keys[0].clone(), keys[2].clone()]]);
    // Both sign in at once; each is stopped at its own W2U.
    for index in [0, 2] { (app.accounts[index].busy, app.accounts[index].preparing_session) = (true, true); }
    for index in [0, 2] { refuse_for_viewer(&mut app, index, LaunchTrigger::MaintenanceSignIn, None); }
    assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
    assert_eq!(app.play_updates.viewer_started.len(), 1);
    assert!(app.is_waiting(&keys[2]), "its Play still waits for FFXI");
    let waiting = account_row::row_view(&app, &app.accounts[2]);
    assert!(waiting.detail.starts_with("Updating PlayOnline"), "{}", waiting.detail);
    let _ = app.update(Message::Tick);
    assert_eq!(maintenance_signins(&app).len(), 1, "nothing signs in while PlayOnline updates");
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    let _ = app.update(Message::Tick);
    assert_eq!(maintenance_signins(&app), vec![vec![keys[0].clone(), keys[2].clone()]; 2], "the watch signs them in again");
    assert!(app.is_waiting(&keys[2]));
    std::fs::remove_dir_all(root).unwrap();
}

/// PlayOnline's update that could not run says why on the rows that asked for it, in a
/// short sentence when its files are in use; a cancelled one says Play continues. Nothing
/// starts by itself afterwards.
#[test]
fn a_playonline_update_that_cannot_run_says_why_on_the_row() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer-failed", None);
    app.patch = None;
    // No game folder selected: PlayOnline's own update needs none.
    app.settings.install_selected = false;
    for (error, cancel, shown) in [
        (updater::viewer_in_use().to_owned(), false, "PlayOnline is in use. Close FINAL FANTASY XI and PlayOnline Viewer, then press Play."),
        ("Patch server timed out".to_owned(), false, "PlayOnline could not be updated: Patch server timed out"),
        ("Cancelled.".to_owned(), true, "Update paused. Press Play to continue."),
    ] {
        refuse_for_viewer(&mut app, 0, LaunchTrigger::Play, None);
        assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
        if cancel { let _ = app.update(Message::CancelMaintenance); }
        let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Err(error), None));
        let row = &app.accounts[0];
        assert!(row.status == shown && row.problem != cancel && !row.busy, "{}", row.status);
        assert!(app.launches.as_ref().unwrap().is_empty() && app.refused_plays.is_empty() && app.viewer_update_of(&row.key).is_none());
    }
    assert_eq!(app.play_updates.viewer_started.len(), 3, "each Play's own W2U asks again");
    std::fs::remove_dir_all(root).unwrap();
}

/// `index`'s sign-in (started by `trigger`) ends with `error`.
fn sign_in_fails(app: &mut App, index: usize, trigger: launch_timing::LaunchTrigger, error: &str) {
    let row = &mut app.accounts[index];
    (row.busy, row.preparing_session, row.presign_login) = (true, trigger.is_presign(), trigger.is_presign());
    row.launch_trigger = Some(trigger);
    let key = row.key.clone();
    app.worker_event(WorkerEvent::Complete(key, Err(error.to_owned())));
}

/// Live 2026-11-12 17:56 (Crystal 1.2.6): a reinstalled PlayOnline, older than a version a real
/// check told, or never updated. The pre-launch gate refuses it from local evidence, before any
/// host starts, and that Play runs PlayOnline's update exactly as a W2U's "outdated" does,
/// except that the update asks the server for its version itself (no version to trust). A
/// sign-in Crystal starts again after that update, still refused, starts no second update.
#[test]
fn an_outdated_install_refused_before_the_host_updates_playonline_from_that_play() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer-stale", None);
    app.patch = None;
    let key = app.accounts[0].key.clone();
    let refusal = viewer::stale_message("Not initialized");
    sign_in_fails(&mut app, 0, LaunchTrigger::Play, &refusal);
    assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
    assert_eq!(app.update_scope, (true, false), "PlayOnline only");
    assert_eq!(app.play_updates.viewer_started, [None], "no W2U answer to reuse: the update asks the server once");
    assert!(app.viewer.as_ref().is_some_and(|status| status.stale && status.requires_update()));
    assert_eq!(account_row::row_state(&app, &app.accounts[0]), account_row::RowState::ViewerUpdating);
    assert!(!app.accounts[0].problem);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
    assert_eq!(app.launches.as_ref().unwrap()[0].0, [key.clone()]);
    // Its start again is still refused: no loop.
    sign_in_fails(&mut app, 0, LaunchTrigger::ReissuedPlay, &refusal);
    assert!(app.maintenance.is_none() && app.refused_plays.is_empty() && app.play_updates.viewer_started.len() == 1);
    assert!(app.accounts[0].problem && app.accounts[0].status == refusal, "{}", app.accounts[0].status);
    // The user's next Play tries the update once more.
    sign_in_fails(&mut app, 0, LaunchTrigger::Play, &refusal);
    assert!(app.maintenance == Some(MaintenanceKind::Update) && app.play_updates.viewer_started.len() == 2);
    std::fs::remove_dir_all(root).unwrap();
}

/// The host's core check is the safety net: an old core it refuses (the owner's "service 463
/// (PlayOnline sign-in) is missing from the function table") reads as PlayOnline's update, which
/// runs from the sign-in that hit it (Crystal's own startup sign-in too). The row never shows
/// the service number. A core still refused right after a successful update keeps the host's
/// own reason and starts nothing more.
#[test]
fn a_core_check_that_finds_an_old_core_updates_playonline() {
    use launch_timing::LaunchTrigger;
    i18n::set(i18n::Lang::En);
    let (mut app, root) = play_update_app("viewer-core", None);
    app.patch = None;
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    app.settings.auto_sign_in = true;
    app.settings.account_no_otp.extend(keys.iter().cloned());
    let core = "Game loader error during startup: PlayOnline core check failed: service 463 (PlayOnline sign-in) is missing from the function table. (Windower 4 via crystal-loader).";
    sign_in_fails(&mut app, 0, LaunchTrigger::StartupSignIn, core);
    assert!(app.maintenance == Some(MaintenanceKind::Update), "{}", app.status);
    assert_eq!(app.play_updates.viewer_started, [None]);
    assert!(app.viewer.as_ref().is_some_and(|status| status.stale && status.requires_update()));
    assert!(!app.status.contains("service 463"), "{}", app.status);
    assert!(app.accounts.iter().all(|row| !row.status.contains("service 463")));
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), None));
    let started: Vec<_> = app.launches.as_ref().unwrap().iter().map(|(keys, social, _, trigger)| (keys.clone(), *social, *trigger)).collect();
    assert_eq!(started, [(vec![keys[0].clone()], true, Some(LaunchTrigger::StartupSignIn))], "signs in again as it was");
    // The same core refused again after that update: the host's reason, no second update.
    sign_in_fails(&mut app, 0, LaunchTrigger::StartupSignIn, core);
    assert!(app.maintenance.is_none() && app.refused_plays.is_empty() && app.play_updates.viewer_started.len() == 1);
    assert!(app.accounts[0].problem && app.accounts[0].status.contains("service 463"), "{}", app.accounts[0].status);
    // Another core failure (a patched core) is not an update.
    let patched = "Game loader error during startup: PlayOnline core check failed: service 463 (PlayOnline sign-in) does not point into the core's code. (Windower 4 via crystal-loader).";
    let (mut other, other_root) = play_update_app("viewer-core-patched", None);
    sign_in_fails(&mut other, 0, LaunchTrigger::Play, patched);
    assert!(other.maintenance.is_none() && other.refused_plays.is_empty() && other.accounts[0].status == patched);
    std::fs::remove_dir_all(other_root).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// No host parks against a PlayOnline known to need its update (its core may be too old to
/// park, and the update closes every park), nor while sign-ins wait for that update.
#[test]
fn no_park_starts_while_playonline_needs_its_update() {
    let mut app = app();
    app.viewer = Some(viewer_report(patch::PatchState::Current));
    assert_eq!(app.park_keys().len(), 3);
    app.viewer = Some(viewer::stale(app.viewer.take()));
    assert!(app.park_keys().is_empty());
    app.viewer = Some(viewer_report(patch::PatchState::Required));
    assert!(app.park_keys().is_empty());
    app.viewer = None;
    app.refused_plays.push(RefusedSignIn { key: app.accounts[0].key.clone(), clicked: Instant::now(), sign_in: None });
    assert!(app.park_keys().is_empty(), "sign-ins wait for that update");
}


#[test]
fn reshade_toggle_preserves_a_running_sessions_renderer() {
    let mut app = app();
    app.settings.reshade.enabled = true;
    app.reshade.installed = true;
    app.preview = false;
    app.accounts[0].pid = Some(12345);
    let _ = reshade::update(&mut app, reshade::Msg::Toggle(false));
    assert!(app.settings.reshade.enabled);
    let _ = dgvoodoo::update(&mut app, dgvoodoo::Msg::Toggle(false));
    assert!(app.settings.dgvoodoo && app.settings.reshade.enabled);
}

#[test]
fn reshade_ignores_install_checks_from_before_a_newer_check() {
    let mut app = app();
    app.preview = false;
    app.reshade.installed = true;
    let _ = reshade::update(&mut app, reshade::Msg::Refresh);
    let _ = reshade::update(&mut app, reshade::Msg::Checked(0, false, "stale.ini".into()));
    assert!(app.reshade.installed && app.reshade.preset.is_empty());
    let _ = reshade::update(&mut app, reshade::Msg::Checked(1, true, "current.ini".into()));
    assert_eq!(app.reshade.preset, "current.ini");
    let _ = reshade::update(&mut app, reshade::Msg::ChoosePreset);
    assert!(app.reshade.busy);
    let _ = reshade::update(&mut app, reshade::Msg::Picked(None));
    let _ = reshade::update(&mut app, reshade::Msg::Checked(1, false, "stale.ini".into()));
    assert!(app.reshade.installed && app.reshade.preset == "current.ini");
}

#[test]
fn ffxiah_lookup_tracks_world_changes_and_blocks_background_controller_actions() {
    let mut app = app();
    app.ah_query = "copper".into();
    let _ = app.update(Message::SearchAh);
    assert!(app.ah_lookup.open && app.controller_modal());
    let previous = app.ah_lookup.generation;
    let _ = app.update(Message::AhServerChanged("Bismarck"));
    assert_eq!(app.ah_lookup.world, "Bismarck");
    assert!(app.ah_lookup.generation > previous);
    let states: Vec<_> = app.accounts.iter().map(|a| a.busy).collect();
    let page = app.page;
    let _ = app.update(Message::ControllerPlay);
    let _ = app.update(Message::ControllerPage(1));
    assert_eq!(app.accounts.iter().map(|a| a.busy).collect::<Vec<_>>(), states);
    assert!(app.page == page);
    let _ = app.update(Message::ControllerBack);
    assert!(!app.ah_lookup.open && !app.controller_modal());
}

#[test]
fn controller_changes_target_running_retail_games_without_selection_or_passwords() {
    let mut app = app();
    for (index, row) in app.accounts.iter_mut().enumerate() {
        row.account.mode = if index == 2 { AccountMode::Private } else { AccountMode::Retail };
        row.pid = Some(100 + index as u32);
        row.alive = Some(Arc::new(AtomicBool::new(true)));
        row.game_running = Some(Arc::new(AtomicBool::new(index != 1)));
        row.selected = false;
    }
    let key = app.accounts[0].key.clone();
    app.accounts[0].account.pol_password = "kept locally".into();
    app.accounts[0].account.se_password = "kept locally too".into();
    app.settings.account_installs.insert(key, r"C:\test\FFXI".into());
    let targets = app.controller_settings_targets();
    assert_eq!(targets.len(), 1, "idle POL sessions and private clients are excluded");
    assert_eq!(account_key(&targets[0].1), account_key(&app.accounts[0].account));
    assert!(targets[0].1.pol_password.is_empty() && targets[0].1.se_password.is_empty());
    assert_eq!(targets[0].0.game_install, Some(PathBuf::from(r"C:\test\FFXI")));
    assert_eq!(app.accounts[0].account.pol_password, "kept locally");
    assert_eq!(app.accounts[0].account.se_password, "kept locally too");
    app.accounts[0].alive.as_ref().unwrap().store(false, Ordering::Release);
    assert!(app.controller_settings_targets().is_empty(), "a client already closed receives no changes");
}

#[test]
fn privacy_mode_is_a_saved_setting_that_changes_no_data() {
    let old: Settings = serde_json::from_str(r#"{"install_path":"C:\\Games\\FFXI"}"#).unwrap();
    assert!(!old.privacy);
    assert!(!serde_json::to_string(&old).unwrap().contains("privacy"));
    let mut app = app();
    let accounts: Vec<_> = app.accounts.iter().map(|a| (a.account.label.clone(), a.account.pol_id.clone())).collect();
    let _ = app.update(Message::SetPrivacy(true));
    assert!(app.settings.privacy && privacy::on());
    assert!(serde_json::to_string(&app.settings).unwrap().contains(r#""privacy":true"#));
    assert_eq!(app.accounts.iter().map(|a| (a.account.label.clone(), a.account.pol_id.clone())).collect::<Vec<_>>(), accounts);
    let _ = app.update(Message::SetPrivacy(false));
    assert!(!privacy::on());
}

#[test]
fn deleting_an_account_requires_confirmation_and_cancel_keeps_it() {
    let mut app = app();
    let key = app.accounts[0].key.clone();
    app.preview = false;
    assert_eq!(app.update(Message::DeleteAccount(key.clone())).units(), 0);
    assert!(!app.vault_saving);
    let _ = app.update(Message::RequestDeleteAccount(key.clone()));
    assert_eq!(app.pending_delete.as_deref(), Some(key.as_str()));
    let _ = app.update(Message::CancelDeleteAccount);
    assert!(app.pending_delete.is_none());
    assert_eq!(app.accounts.len(), 3);
    let _ = app.update(Message::RequestDeleteAccount(key.clone()));
    assert_eq!(app.update(Message::DeleteAccount(key)).units(), 1);
    assert!(app.vault_saving);
    assert!(app.pending_delete.is_none());
}

#[test]
fn switching_language_swaps_the_news_and_drops_the_other_languages_late_results() {
    let root = std::env::var_os("CRYSTAL_DATA_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join(format!("crystal-news-language-{}", backend::unique_id().unwrap()));
    let mut app = app();
    app.config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    app.preview = false;
    let en_cache = news_cache_path(&app.config, i18n::Lang::En);
    let ja_cache = news_cache_path(&app.config, i18n::Lang::Ja);
    assert_eq!(en_cache.file_name().unwrap(), "news-cache.json", "the English file keeps its name");
    assert_eq!(ja_cache.file_name().unwrap(), "news-cache.ja.json");
    std::fs::create_dir_all(en_cache.parent().unwrap()).unwrap();
    let saved = |title: &str, site: &str, lang: &str| format!(r#"{{"items":[{{"title":"{title}","url":"https://www.playonline.com/{site}/polnews/news1.shtml","date":"","summary":"","image_url":""}}],"saved_at":"2026-10-06 00:00:00 UTC","language":"{lang}"}}"#);
    std::fs::write(&en_cache, saved("English headline", "ff11us", "en")).unwrap();
    let headline = |app: &App| app.services.as_ref().map(|s| s.news[0].title.clone());
    let late = |title: &str| {
        let item = services::NewsItem { title: title.into(), url: "https://www.playonline.com/news/late.html".into(), ..Default::default() };
        services::ServiceSnapshot { news: vec![item], notices: Vec::new(), news_note: String::new(), availability: services::Availability::Unknown, availability_note: String::new(), windows: Vec::new(), windows_complete: false, ended_early: Vec::new(), checked_at: String::new() }
    };

    i18n::set(i18n::Lang::En);
    app.services = services::cached(&en_cache, i18n::Lang::En);
    assert_eq!(headline(&app).as_deref(), Some("English headline"));
    app.news_loading = true; // an English refresh is running
    app.news_lang = i18n::Lang::En;
    let first_en = app.news_generation;

    // No Japanese headlines saved yet: the English ones go, none take their place,
    // and a Japanese read starts although an English one is still running.
    assert_eq!(app.update(Message::SetLanguage(i18n::Lang::Ja)).units(), 1);
    assert_eq!(i18n::current(), i18n::Lang::Ja);
    assert!(app.services.is_none());
    assert!(app.news_loading && app.news_lang == i18n::Lang::Ja);
    let first_ja = app.news_generation;

    // The English read finishing now changes nothing.
    app.worker_event(WorkerEvent::Services(first_en, i18n::Lang::En, services::Update::Headlines { items: late("Late English").news, note: String::new() }));
    let _ = app.update(Message::ServicesLoaded(first_en, i18n::Lang::En, Ok(late("Late English"))));
    assert!(app.services.is_none(), "English results are not shown in Japanese");
    assert!(app.news_loading, "the Japanese read still owns the loading state");
    // A second request while the Japanese read runs does not start another.
    assert_eq!(app.start_news().units(), 0);

    // The Japanese read's own results show and end the loading state.
    app.worker_event(WorkerEvent::Services(first_ja, i18n::Lang::Ja, services::Update::Headlines { items: late("日本語の見出し").news, note: String::new() }));
    assert_eq!(headline(&app).as_deref(), Some("日本語の見出し"));
    let _ = app.update(Message::ServicesLoaded(first_ja, i18n::Lang::Ja, Ok(late("日本語の見出し"))));
    assert!(!app.news_loading);

    // Back to English: its saved headlines return (the cache file the Japanese
    // read would write is a different one), and a fresh read starts.
    std::fs::write(&ja_cache, saved("日本語の見出し", "ff11", "ja")).unwrap();
    assert_eq!(app.update(Message::SetLanguage(i18n::Lang::En)).units(), 1);
    assert_eq!(headline(&app).as_deref(), Some("English headline"));
    // The first English request still cannot publish after switching back.
    app.worker_event(WorkerEvent::Services(first_en, i18n::Lang::En, services::Update::Headlines { items: late("Stale English").news, note: String::new() }));
    let _ = app.update(Message::ServicesLoaded(first_en, i18n::Lang::En, Ok(late("Stale English"))));
    let _ = app.update(Message::ServiceStatusLoaded(first_en, i18n::Lang::En, Ok(late("Stale status"))));
    assert_eq!(headline(&app).as_deref(), Some("English headline"));
    assert!(app.news_loading, "the current English request still owns loading");
    assert_eq!(app.update(Message::SetLanguage(i18n::Lang::Ja)).units(), 1);
    assert_eq!(headline(&app).as_deref(), Some("日本語の見出し"));
    let _ = app.update(Message::ServicesLoaded(first_ja, i18n::Lang::Ja, Err("stale failure".into())));
    assert!(app.news_loading && app.news_error.is_empty());
    // Choosing the language already showing is not a change and fetches nothing.
    let _ = app.update(Message::ServicesLoaded(app.news_generation, i18n::Lang::Ja, Err("offline".into())));
    assert_eq!(app.update(Message::SetLanguage(i18n::Lang::Ja)).units(), 0);
    assert_eq!(headline(&app).as_deref(), Some("日本語の見出し"));
    i18n::set(i18n::Lang::En);
}

#[test]
fn language_setting_is_optional_and_applies_at_once() {
    // A settings file from before the language setting still loads, follows
    // Windows, and is written back without a language until one is chosen.
    let old: Settings = serde_json::from_str(r#"{"install_path":"C:\\Games\\FFXI","light_theme":true}"#).unwrap();
    assert_eq!(old.language, None);
    assert!(!serde_json::to_string(&old).unwrap().contains("language"));
    let mut app = app();
    i18n::set(i18n::Lang::En);
    let _ = app.update(Message::SetLanguage(i18n::Lang::Ja));
    assert_eq!(app.settings.language, Some(i18n::Lang::Ja));
    assert_eq!(i18n::current(), i18n::Lang::Ja);
    assert_eq!(i18n::t("Settings"), "設定");
    let saved = serde_json::to_string(&app.settings).unwrap();
    assert!(saved.contains(r#""language":"ja""#), "{saved}");
    let back: Settings = serde_json::from_str(&saved).unwrap();
    assert_eq!(back.language, Some(i18n::Lang::Ja));
    let _ = app.update(Message::SetLanguage(i18n::Lang::En));
    assert_eq!(i18n::t("Settings"), "Settings");
}

#[test]
fn window_and_session_worker_take_their_language_from_the_same_saved_settings() {
    let root = std::env::temp_dir().join(format!("crystal-language-{}", backend::unique_id().unwrap()));
    let config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    // No settings file yet: the Windows display language.
    i18n::set(i18n::Lang::En);
    load_settings_and_language(&config).unwrap();
    assert_eq!(i18n::current(), i18n::system_default());
    // A saved choice wins, whatever Windows says.
    let settings = Settings { language: Some(i18n::Lang::Ja), ..Settings::default() };
    save_settings(&config, &settings).unwrap();
    i18n::set(i18n::Lang::En);
    assert!(load_settings_and_language(&config).is_ok());
    assert_eq!(i18n::current(), i18n::Lang::Ja);
    // A damaged file reports its error in the Windows language and falls back to it.
    std::fs::write(settings_path(&config), b"{\"install_path\":\"unfinished").unwrap();
    i18n::set(i18n::Lang::Ja);
    assert!(load_settings_and_language(&config).is_err());
    assert_eq!(i18n::current(), i18n::system_default());
    i18n::set(i18n::Lang::En);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn launcher_names_join_as_translatable_pairs() {
    i18n::set(i18n::Lang::En);
    assert_eq!(join_and(&[]), "");
    assert_eq!(join_and(&["Ashita v4"]), "Ashita v4");
    assert_eq!(join_and(&["Ashita v4", "Windower 4"]), "Ashita v4 and Windower 4");
    assert_eq!(join_and(&["the native client", "Ashita v4", "Windower 4"]), "the native client and Ashita v4 and Windower 4");
}

#[test]
fn the_last_result_carries_its_tone_apart_from_its_words() {
    let mut app = app();
    app.maintenance = Some(MaintenanceKind::Check);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Check, Err("boom".into()), None));
    assert_eq!(app.maintenance_tone, ResultTone::Stopped);
    assert!(app.maintenance_result.starts_with("Operation stopped"));
    app.maintenance = Some(MaintenanceKind::Check);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Check, Ok("Checked.".into()), None));
    assert_eq!(app.maintenance_tone, ResultTone::Done);
}

#[test]
fn unchecked_selections_read_as_unverified_not_done() {
    let mut app = app();
    app.preview = false;
    app.maintenance_tone = ResultTone::Done;
    let _ = app.update(Message::InstallPathChanged(r"C:\Elsewhere".into()));
    assert_eq!(app.maintenance_tone, ResultTone::Unverified);
    assert_eq!(app.maintenance_result, t("Installation path changed. Save settings to select this folder."));
}

#[test]
fn choosing_an_install_folder_rejects_the_previous_folders_pending_report() {
    let root = std::env::var_os("CRYSTAL_DATA_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join(format!("crystal-picker-{}", backend::unique_id().unwrap()));
    let install = root.join("game");
    std::fs::create_dir_all(&install).unwrap();
    let mut app = app();
    app.config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    app.preview = false;
    app.patch_loading = true;
    let old_generation = app.patch_generation;
    let old_report = app.patch.clone().unwrap();
    let _ = app.update(Message::Picked(PickTarget::GameFolder, Some(install.clone())));
    assert!(app.settings.install_selected);
    assert_eq!(app.settings.install_path, install.to_string_lossy());
    assert!(!app.patch_loading && app.patch.is_none());
    assert_ne!(app.patch_generation, old_generation);
    let _ = app.update(Message::PatchLoaded(old_generation, old_report));
    assert!(app.patch.is_none());
    assert_eq!(load_settings(&app.config).unwrap().install_path, install.to_string_lossy());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn automatic_installation_detection_saves_the_folder_and_rejects_old_version_results() {
    let root = std::env::temp_dir().join(format!("crystal-detect-{}", backend::unique_id().unwrap()));
    let install = root.join("FINAL FANTASY XI");
    std::fs::create_dir_all(&install).unwrap();
    let mut app = app();
    app.config.experiment = root.clone();
    app.preview = false;
    app.installation_loading = true;
    app.installation_generation = 7;
    app.patch_loading = true;
    let previous = app.patch.clone().unwrap();
    let previous_generation = app.patch_generation;
    assert_eq!(app.update(Message::InstallationFound(7, true, Some(install.clone()))).units(), 1);
    assert!(app.settings.install_selected && !app.installation_loading);
    assert_eq!(load_settings(&app.config).unwrap().install_path, install.to_string_lossy());
    assert!(app.patch.is_none() && app.patch_loading);
    let _ = app.update(Message::PatchLoaded(previous_generation, previous));
    assert!(app.patch.is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn installation_detection_cannot_replace_later_edits_or_run_in_a_preview() {
    let mut app = app();
    assert_eq!(app.update(Message::FindInstallation).units(), 0);
    assert!(!app.installation_loading);
    app.preview = false;
    let original = app.settings.install_path.clone();
    assert_eq!(app.find_installation(true).units(), 1);
    let generation = app.installation_generation;
    let _ = app.update(Message::InstallPathChanged(r"C:\Another install".into()));
    let _ = app.update(Message::InstallPathChanged(original.clone()));
    let _ = app.update(Message::InstallationFound(generation, true, Some(PathBuf::from(r"C:\Late install"))));
    assert_eq!(app.settings.install_path, original);
    assert!(!app.installation_loading && !app.settings.install_selected);
    app.maintenance = Some(MaintenanceKind::Update);
    assert_eq!(app.update(Message::FindInstallation).units(), 0);
}

#[test]
fn installation_completion_starts_pending_signin_without_waiting_for_a_ui_tick() {
    let (mut app, key) = resign_app("startup-install-ready");
    app.startup_signin_pending = true;
    app.installation_loading = true;
    app.installation_generation = 2;
    let install = app.config.experiment.join("game");
    std::fs::create_dir_all(&install).unwrap();
    app.settings.install_selected = true;
    app.settings.install_path = install.to_string_lossy().into_owned();
    assert!(app.take_startup_signins().is_empty());
    let _ = app.update(Message::InstallationFound(2, true, Some(install.clone())));
    assert!(!app.startup_signin_pending);
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![key]);
    let _ = app.update(Message::InstallationFound(2, true, Some(install)));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1, "one automatic sign-in only");
    std::fs::remove_dir_all(&app.config.experiment).unwrap();
}

#[test]
fn automatic_detection_keeps_a_damaged_settings_file_unchanged() {
    let root = std::env::temp_dir().join(format!("crystal-detect-damaged-{}", backend::unique_id().unwrap()));
    let install = root.join("game");
    std::fs::create_dir_all(&install).unwrap();
    let mut app = app();
    app.config.experiment = root.clone();
    let path = settings_path(&app.config);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let damaged = b"{\"install_path\":\"unfinished";
    std::fs::write(&path, damaged).unwrap();
    app.preview = false;
    app.startup_warning = Some("Settings could not be read".into());
    app.settings_damaged = true;
    // The vault loads first and shows (takes) the warning: the file stays protected.
    app.launcher_start_logged = true;
    let _ = app.update(Message::AccountsLoaded(Ok(Vec::new())));
    assert!(app.startup_warning.is_none() && app.settings_damaged);
    app.installation_loading = true;
    app.installation_generation = 5;
    assert_eq!(app.update(Message::InstallationFound(5, true, Some(install.clone()))).units(), 1);
    assert!(app.settings.install_selected);
    assert_eq!(app.settings.install_path, install.to_string_lossy());
    assert_eq!(std::fs::read(path).unwrap(), damaged);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn successful_official_setup_rescans_installation_and_failed_setup_does_not() {
    let mut app = app();
    app.preview = false;
    app.maintenance = Some(MaintenanceKind::Setup);
    // The installation, the Viewer, and the installed PlayOnline editions.
    assert_eq!(app.update(Message::MaintenanceFinished(MaintenanceKind::Setup, Ok("Setup closed".into()), None)).units(), 3);
    assert!(app.installation_loading && app.viewer_loading);
    app.installation_loading = false;
    app.maintenance = Some(MaintenanceKind::Setup);
    assert_eq!(app.update(Message::MaintenanceFinished(MaintenanceKind::Setup, Err("Cancelled".into()), None)).units(), 0);
    assert!(!app.installation_loading);
}

#[test]
fn fixed_sentences_follow_a_language_change() {
    // A table entry (Settings ↔ 設定) read back from Japanese into English.
    i18n::set(i18n::Lang::En);
    assert_eq!(retranslated("設定", i18n::Lang::Ja, &["Settings"]), Some("Settings"));
    assert_eq!(retranslated("Settings", i18n::Lang::Ja, &["Settings"]), None);
    assert_eq!(retranslated("Ready (3 accounts)", i18n::Lang::Ja, &["Settings"]), None);
    // The window's own lines, both ways.
    let mut app = app();
    app.maintenance_result = t(RESULT_TEXTS[0]).into();
    app.accounts[0].status = t("Client closed").into();
    let _ = app.update(Message::SetLanguage(i18n::Lang::Ja));
    assert_eq!(app.maintenance_result, i18n::t_in(i18n::Lang::Ja, RESULT_TEXTS[0]));
    assert_eq!(app.accounts[0].status, i18n::t_in(i18n::Lang::Ja, "Client closed"));
    let _ = app.update(Message::SetLanguage(i18n::Lang::En));
    assert_eq!(app.maintenance_result, RESULT_TEXTS[0]);
    assert_eq!(app.accounts[0].status, "Client closed");
}

#[test]
fn an_exit_report_on_a_row_is_worded_when_shown() {
    i18n::set(i18n::Lang::En);
    let mut app = app();
    let report = launch_log::classify_loader_end(&launch_log::LoaderEnd::Closed, Duration::from_secs(1));
    app.accounts[0].set_exit(report.clone());
    assert!(app.accounts[0].problem);
    assert_eq!(app.accounts[0].status, report.summary, "stored in English");
    assert_eq!(app.accounts[0].status_text(), report.message());
    // A later status replaces it.
    app.accounts[0].status = "Starting sign-in…".into();
    assert_eq!(app.accounts[0].status_text(), "Starting sign-in…");
}

#[test]
fn window_input_is_forwarded_at_most_every_few_seconds() {
    let start = Instant::now();
    let mut last = None;
    assert!(activity_due(&mut last, start));
    assert!(!activity_due(&mut last, start + Duration::from_secs(1)));
    assert!(!activity_due(&mut last, start + ACTIVITY_INTERVAL - Duration::from_millis(1)));
    assert!(activity_due(&mut last, start + ACTIVITY_INTERVAL));
    assert!(!activity_due(&mut last, start + ACTIVITY_INTERVAL + Duration::from_secs(4)));
}

#[test]
fn status_changes_include_groups_unless_unchecked_and_refresh_is_gone() {
    let mut app = app();
    assert!(!app.friends.skip_groups, "Also set for my groups starts checked");
    let _ = app.update(Message::FriendsStatusGroups(false));
    assert!(app.friends.skip_groups);
    let _ = app.update(Message::FriendsStatusGroups(true));
    assert!(!app.friends.skip_groups);
    // Preview: settings are not changed or saved by the Auto-Away picker.
    let _ = app.update(Message::FriendsAutoAway(30));
    assert!(app.settings.account_auto_away.is_empty());
}

#[test]
fn switching_friends_accounts_rejects_an_earlier_read_after_switching_back() {
    let mut app = app();
    let first = app.accounts[0].key.clone();
    let second = app.accounts[1].key.clone();
    let _ = app.update(Message::FriendsAccount(first.clone()));
    let old_generation = app.friends_generation;
    let _ = app.update(Message::FriendsAccount(second));
    let _ = app.update(Message::FriendsAccount(first.clone()));
    app.friends.loading = true;
    app.friends.connected = true;
    app.friends.pending = Some(("current request".into(), Instant::now()));
    app.friends.snapshot = Some(friends::Snapshot { fetched_at: 42, ..Default::default() });

    app.worker_event(WorkerEvent::Friends(old_generation, first.clone(), false, None));
    assert!(app.friends.loading && app.friends.connected && app.friends.pending.is_some());
    assert_eq!(app.friends.snapshot.as_ref().unwrap().fetched_at, 42);
    app.worker_event(WorkerEvent::Friends(app.friends_generation, first, true,
        Some(friends::Snapshot { fetched_at: 43, ..Default::default() })));
    assert!(!app.friends.loading);
    assert_eq!(app.friends.snapshot.as_ref().unwrap().fetched_at, 43);
}

#[test]
fn removing_the_friends_account_clears_its_session_and_pending_results() {
    let mut app = app();
    app.load_friends_preview(false, false, true, false);
    let removed = app.friends.account.clone().unwrap();
    let old_generation = app.friends_generation;
    app.friends.loading = true;
    app.friends.pending = Some(("old request".into(), Instant::now()));
    let remaining = app.accounts.iter().filter(|row| row.key != removed).map(|row| row.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(remaining)));
    assert!(app.friends.account.is_none() && app.friends.snapshot.is_none());
    assert!(!app.friends.loading && !app.friends.connected);
    assert!(app.friends.pending.is_none() && app.friends.composer.is_none());
    app.worker_event(WorkerEvent::Friends(old_generation, removed, true, Some(friends::Snapshot::default())));
    assert!(app.friends.snapshot.is_none());
    let _ = app.update(Message::FriendsAccount(app.accounts[0].key.clone()));
    assert_eq!(app.friends.account.as_ref(), Some(&app.accounts[0].key));
}

#[test]
fn back_closes_the_settings_dialog_and_friends_panes_before_leaving_the_page() {
    let mut app = app();
    app.show_settings = true;
    let _ = app.update(Message::ControllerBack);
    assert!(!app.show_settings);
    app.load_friends_preview(false, false, true, false);
    app.friends.remove = Some(app.friends.snapshot.as_ref().unwrap().entries[0].clone());
    let _ = app.update(Message::ControllerBack);
    assert!(app.friends.remove.is_none() && app.friends.composer.is_some());
    let _ = app.update(Message::ControllerBack);
    assert!(app.page == Page::Friends && app.friends.pane.is_none() && app.friends.composer.is_none());
    for pane in [friends::Pane::Mailbox, friends::Pane::Blocked] {
        app.friends.select(Some(pane));
        let _ = app.update(Message::ControllerBack);
        assert!(app.page == Page::Friends && app.friends.pane.is_none());
    }
    let _ = app.update(Message::ControllerBack);
    assert!(app.page == Page::Home);
}

#[test]
fn refreshing_during_news_load_does_not_start_another_feed_stream() {
    let mut app = app();
    app.preview = false;
    assert_eq!(app.refresh().units(), 2);
    assert!(app.news_loading);
    assert_eq!(app.refresh().units(), 1);
    assert_eq!(app.patch_generation, 2);
}

#[test]
fn main_button_tracks_update_downtime_and_running_operations() {
    let mut app = app();
    assert_eq!(app.main_action(), MainAction::Play);
    app.services.as_mut().unwrap().availability = services::Availability::Maintenance;
    assert_eq!(app.main_action(), MainAction::Maintenance);
    let _ = app.update(Message::MainAction);
    assert!(app.show_details);
    assert!(app.accounts.iter().all(|row| !row.busy && row.pid.is_none()));
    // A published patch installs from the Play that asks for it, not from a main-button Update.
    app.patch.as_mut().unwrap().state = patch::PatchState::Required;
    assert_eq!(app.main_action(), MainAction::Maintenance);
    app.services.as_mut().unwrap().availability = services::Availability::Operational;
    assert_eq!(app.main_action(), MainAction::Play);
    app.maintenance = Some(MaintenanceKind::Update);
    assert_eq!(app.main_action(), MainAction::Working);
    app.maintenance = None;
    app.patch.as_mut().unwrap().state = patch::PatchState::Current;
    app.services.as_mut().unwrap().availability = services::Availability::Outage;
    assert_eq!(app.main_action(), MainAction::Offline);
    for availability in [services::Availability::Operational, services::Availability::PartialOutage, services::Availability::Unknown] {
        app.services.as_mut().unwrap().availability = availability;
        assert_eq!(app.main_action(), MainAction::Play);
    }
}

#[test]
fn main_update_starts_the_combined_updater_and_keeps_one_operation() {
    for viewer_needs_update in [false, true] {
        let mut app = app();
        app.preview = false;
        app.accounts.clear();
        app.settings.install_path = r"C:\fixture\FINAL FANTASY XI".into();
        app.settings.install_selected = true;
        app.show_settings = true;
        app.show_details = true;
        app.patch.as_mut().unwrap().state = if viewer_needs_update {
            patch::PatchState::Current
        } else {
            patch::PatchState::Required
        };
        if viewer_needs_update {
            app.viewer = Some(viewer_report(patch::PatchState::Required));
        }

        // Creating the task must schedule the shared updater without polling
        // it here: this test must not download or alter an installation.
        assert_eq!(app.start_maintenance(MaintenanceKind::Update).units(), 1);
        assert!(app.maintenance == Some(MaintenanceKind::Update));
        assert!(app.active_operation.is_some());
        assert_eq!(app.main_action(), MainAction::Working);
        assert!(app.launch_blocked());
        let generation = app.viewer_generation;

        app.page = Page::Home;
        assert_eq!(app.update(Message::MainAction).units(), 0);
        assert!(app.page == Page::Maintenance);
        assert!(!app.show_settings && !app.show_details);
        assert_eq!(app.viewer_generation, generation);
        assert!(app.maintenance == Some(MaintenanceKind::Update));
    }
}

#[test]
fn main_update_keeps_folder_and_session_protections() {
    let mut app = app();
    app.preview = false;
    app.patch.as_mut().unwrap().state = patch::PatchState::Required;

    assert_eq!(app.start_maintenance(MaintenanceKind::Update).units(), 0);
    assert!(app.maintenance.is_none() && app.active_operation.is_none());
    assert_eq!(app.status, t("Select the target folder explicitly before starting this operation."));

    app.settings.install_selected = true;
    app.vault_ready = false;
    assert_eq!(app.start_maintenance(MaintenanceKind::Update).units(), 0);
    assert!(app.maintenance.is_none() && app.active_operation.is_none());
    assert_eq!(app.status, t("Wait for saved accounts and running sessions to be checked before changing game files."));

    app.vault_ready = true;
    app.accounts[0].pid = Some(42);
    assert_eq!(app.start_maintenance(MaintenanceKind::Update).units(), 0);
    assert!(app.maintenance.is_none() && app.active_operation.is_none());
    assert_eq!(app.accounts[0].pid, Some(42));
    assert_eq!(app.status, t("Close all games and sign out PlayOnline sessions before updating, repairing, or running official setup. Existing sessions were kept."));
}

#[test]
fn local_refresh_preserves_a_known_update_until_the_installed_version_changes() {
    let mut app = app();
    app.patch.as_mut().unwrap().state = patch::PatchState::Required;
    let mut local = app.patch.as_ref().unwrap().clone();
    local.state = patch::PatchState::Unknown;
    local.remote_version = t("Not checked").into();
    let _ = app.update(Message::PatchLoaded(app.patch_generation, local.clone()));
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Required);
    local.local_version = "30260904_2".into();
    let _ = app.update(Message::PatchLoaded(app.patch_generation, local));
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Unknown);
}

#[test]
fn successful_file_check_survives_version_refresh_but_not_a_changed_record() {
    let mut app = app();
    let mut checked = app.patch.as_ref().unwrap().clone();
    checked.state = patch::PatchState::Current;
    checked.files_verified = Some(true);
    app.maintenance = Some(MaintenanceKind::Check);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Check, Ok("Files verified".into()), Some(checked.clone())));
    let mut local = checked;
    local.state = patch::PatchState::Unknown;
    local.files_verified = None;
    local.remote_version = t("Not checked").into();
    let _ = app.update(Message::PatchLoaded(app.patch_generation, local.clone()));
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Current);
    assert_eq!(app.patch.as_ref().unwrap().files_verified, Some(true));
    assert_eq!(app.maintenance_tone, ResultTone::Done);
    local.local_version = "30260904_2".into();
    let _ = app.update(Message::PatchLoaded(app.patch_generation, local));
    assert_eq!(app.patch.as_ref().unwrap().files_verified, None);
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Unknown);
}

/// The Downloads page after Check files: no version check ran, so the latest version is
/// the remembered one with its age, until nothing is known ("Press Play to check").
#[test]
fn check_files_shows_the_remembered_official_version_instead_of_not_checked() {
    let root = std::env::temp_dir().join(format!("crystal-latest-{}-{}", std::process::id(), official_versions::now()));
    let _ = std::fs::remove_dir_all(&root);
    let mut app = app();
    app.preview = false;
    app.config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    app.settings.install_path = r"C:\Games\FFXI".into();
    let mut local = app.patch.as_ref().unwrap().clone();
    local.state = patch::PatchState::Unknown;
    local.local_version = "30260904_1".into();
    local.remote_version = t("Not checked").into();
    app.patch = Some(local.clone());
    // Nothing learned yet: asks for a Play, never "Not checked".
    assert_eq!(crate::official_versions::latest_game_text(&app), t("Press Play to check"));
    assert_eq!(crate::official_versions::latest_playonline_text(&app), t("Press Play to check"));
    // A Play's W20 (any process) wrote the answer: the next outcome reaching the window reads it.
    let path = official_versions::path_for(&app.config);
    let now = official_versions::now();
    official_versions::learn_game_at(&path, Path::new(r"C:\Games\FFXI"), "30260905_0", now - 3 * 3600);
    official_versions::learn_playonline_at(&path, "20110829_E", now - 90);
    let _ = app.update(Message::PatchLoaded(app.patch_generation, local));
    assert_eq!(crate::official_versions::latest_game_text(&app), "30260905_0 (checked 3 h ago)");
    assert_eq!(crate::official_versions::latest_playonline_text(&app), "20110829_E (checked 1 min ago)");
    // Installed 30260904_1 is older than the remembered 30260905_0.
    assert_eq!(official_versions::compare("30260904_1", app.learned_game()), Some(std::cmp::Ordering::Less));
    // An Update installs it: the updater's own answer is remembered and installed matches it.
    let mut updated = app.patch.as_ref().unwrap().clone();
    updated.state = patch::PatchState::Current;
    updated.local_version = "30260906_0".into();
    updated.remote_version = "30260906_0".into();
    app.maintenance = Some(MaintenanceKind::Update);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update, Ok("Updated".into()), Some(updated)));
    assert_eq!(app.learned_game().map(|l| l.version.as_str()), Some("30260906_0"));
    assert_eq!(official_versions::compare("30260906_0", app.learned_game()), Some(std::cmp::Ordering::Equal));
    // A Check afterwards (offline: placeholder remote) leaves the memory alone.
    let mut checked = app.patch.as_ref().unwrap().clone();
    checked.state = patch::PatchState::Unknown;
    checked.remote_version = t("Not checked").into();
    app.maintenance = Some(MaintenanceKind::Check);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Check, Ok("Checked".into()), Some(checked)));
    assert_eq!(app.learned_game().map(|l| l.version.as_str()), Some("30260906_0"));
    assert!(crate::official_versions::latest_game_text(&app).starts_with("30260906_0 (checked "), "{}", crate::official_versions::latest_game_text(&app));
    // Another game folder has its own answer.
    app.patch_folder = Some(PathBuf::from(r"D:\Other"));
    assert_eq!(crate::official_versions::latest_game_text(&app), t("Press Play to check"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn cancelled_import_picker_cannot_overwrite_a_reopened_import() {
    use account_import_ui::Msg;
    let mut app = app();
    let _ = app.update(Message::AccountImport(Msg::Open));
    let _ = app.update(Message::AccountImport(Msg::Sources(1, Vec::new())));
    let _ = app.update(Message::AccountImport(Msg::Browse(false)));
    assert!(app.account_import.busy);
    let _ = app.update(Message::AccountImport(Msg::Close));
    let _ = app.update(Message::AccountImport(Msg::Open));
    let _ = app.update(Message::AccountImport(Msg::Sources(4, Vec::new())));
    let _ = app.update(Message::AccountImport(Msg::Path(r"D:\Current\creds.dat".into())));
    let _ = app.update(Message::AccountImport(Msg::Picked(2, false, Some(PathBuf::from(r"D:\Old\creds.dat")))));
    assert_eq!(app.account_import.path, r"D:\Current\creds.dat");
    assert!(!app.account_import.busy);
}

#[test]
fn back_cancels_visible_close_confirmation_before_a_pending_import() {
    let mut app = app();
    app.confirm_close = true;
    app.account_import.open = true;
    let _ = app.update(Message::ControllerBack);
    assert!(!app.confirm_close);
    assert!(app.account_import.open);
    let _ = app.update(Message::ControllerBack);
    assert!(!app.account_import.open);
}

#[test]
fn unreadable_or_interrupted_update_replaces_a_previous_ready_verdict() {
    let mut app = app();
    app.patch.as_mut().unwrap().files_verified = Some(true);
    let mut invalid = app.patch.as_ref().unwrap().clone();
    invalid.state = patch::PatchState::Required;
    invalid.remote_version = t("Not checked").into();
    invalid.files_verified = None;
    invalid.detail = "Interrupted update".into();
    let _ = app.update(Message::PatchLoaded(app.patch_generation, invalid));
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Required);
    assert_eq!(app.patch.as_ref().unwrap().files_verified, None);
}

#[test]
fn a_fresh_install_stays_on_update_and_preserves_the_server_version_on_refresh() {
    let mut app = app();
    let fresh = patch::PatchReport { state: patch::PatchState::Required,
        local_version: t("Not initialized").into(), remote_version: "30260904_1".into(),
        detail: "Initial update required".into(), files_verified: None };
    app.maintenance = Some(MaintenanceKind::Update);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Update,
        Ok("0 known files updated; files preserved".into()), Some(fresh.clone())));
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Required);
    assert_eq!(app.maintenance_tone, ResultTone::Unverified);
    let mut local = fresh;
    local.remote_version = t("Not checked").into();
    let _ = app.update(Message::PatchLoaded(app.patch_generation, local));
    assert_eq!(app.patch.as_ref().unwrap().remote_version, "30260904_1");
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Required);
}

/// Every world down: a Play waits for them ("Wait and play"), and nothing signs in meanwhile.
#[test]
fn confirmed_downtime_sets_new_game_starts_waiting_without_touching_sessions() {
    let mut app = app();
    app.preview = false;
    let snapshot = app.services.as_mut().unwrap();
    snapshot.availability = services::Availability::Outage;
    snapshot.checked_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let keys = app.launch_keys(true);
    assert!(!keys.is_empty());
    app.start_launch(keys.clone(), false, Instant::now(), None);
    assert!(keys.iter().all(|key| app.is_waiting(key)));
    assert!(app.accounts.iter().all(|row| !row.busy && row.pid.is_none() && row.queued_play.is_none()));
    assert_eq!(app.status, "FFXI's worlds are down. Starting as soon as they're back…");
    assert!(app.receiver.try_recv().is_err(), "no sign-in worker");
}

#[test]
fn status_polling_is_background_only_and_preserves_news_and_patch_verdict() {
    let mut app = app();
    app.preview = false;
    let original_news = app.services.as_ref().unwrap().news[0].title.clone();
    let generation = app.patch_generation;
    app.refreshed = Instant::now() - Duration::from_secs(301);
    assert_eq!(app.poll_service_status().units(), 1);
    assert!(app.news_loading);
    assert_eq!(app.poll_service_status().units(), 0);
    assert_eq!(app.patch_generation, generation, "Only public HTTP status is polled");
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Current);
    let mut snapshot = app.services.as_ref().unwrap().clone();
    snapshot.news.clear();
    snapshot.availability = services::Availability::Outage;
    snapshot.checked_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let _ = app.update(Message::ServiceStatusLoaded(app.news_generation, i18n::current(), Ok(snapshot)));
    assert!(!app.news_loading);
    assert_eq!(app.services.as_ref().unwrap().news[0].title, original_news);
    assert_eq!(app.main_action(), MainAction::Offline);
    app.refreshed = Instant::now() - Duration::from_secs(61);
    assert_eq!(app.poll_service_status().units(), 1, "Downtime is followed more closely");
    let _ = app.update(Message::ServiceStatusLoaded(app.news_generation, i18n::current(), Err("status unavailable".into())));
    assert_eq!(app.main_action(), MainAction::Play);
    assert_eq!(app.services.as_ref().unwrap().news[0].title, original_news);
}

#[test]
fn damaged_settings_are_preserved_before_saving_and_warn_at_startup() {
    let root = std::env::temp_dir().join(format!("crystal-settings-{}", backend::unique_id().unwrap()));
    let config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    let path = settings_path(&config);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let damaged = b"{\"install_path\":\"unfinished";
    std::fs::write(&path, damaged).unwrap();
    assert!(load_settings(&config).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
    let settings = Settings { install_path: root.join("client").to_string_lossy().into_owned(), ..Settings::default() };
    save_settings(&config, &settings).unwrap();
    assert_eq!(load_settings(&config).unwrap().install_path, settings.install_path);
    let backup = std::fs::read_dir(path.parent().unwrap()).unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("settings.invalid-")).unwrap();
    assert_eq!(std::fs::read(backup).unwrap(), damaged);
    let oversized = vec![b'x'; 1024 * 1024 + 1];
    std::fs::write(&path, &oversized).unwrap();
    assert!(save_settings(&config, &settings).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), oversized);
    std::fs::remove_dir_all(root).unwrap();

    let mut app = app();
    app.startup_warning = Some("Damaged settings were kept unchanged.".into());
    let _ = app.update(Message::AccountsLoaded(Ok(vec![])));
    assert!(app.status.contains("Damaged settings"));
    assert!(app.startup_warning.is_none());
}

#[test]
fn verbatim_prefixes_are_cleaned_from_saved_settings_and_picked_folders() {
    let mut settings = Settings {
        install_path: r"\\?\C:\Games\SquareEnix\FINAL FANTASY XI".into(),
        installer_path: r"\\?\C:\Downloads\installer".into(),
        ashita_path: r"\\?\D:\Ashita".into(),
        windower_path: r"\\?\UNC\srv\share\Windower".into(),
        loader_path: r"\\?\C:\crystal\pol.exe".into(),
        ..Settings::default()
    };
    settings.plain_paths();
    assert_eq!(settings.install_path, r"C:\Games\SquareEnix\FINAL FANTASY XI");
    assert_eq!(settings.installer_path, r"C:\Downloads\installer");
    assert_eq!(settings.ashita_path, r"D:\Ashita");
    assert_eq!(settings.windower_path, r"\\srv\share\Windower");
    assert_eq!(settings.loader_path, r"C:\crystal\pol.exe");
    // Plain and empty values are left alone.
    settings.ashita_path.clear();
    settings.plain_paths();
    assert!(settings.ashita_path.is_empty());

    let dir = std::env::temp_dir();
    let chosen = selected_folder(&dir.to_string_lossy(), true).unwrap();
    assert!(!chosen.to_string_lossy().starts_with(r"\\?\"), "{}", chosen.display());
    let missing = dir.join("xi-launcher-not-yet-there").join("deeper");
    let chosen = selected_folder(&missing.to_string_lossy(), false).unwrap();
    assert!(!chosen.to_string_lossy().starts_with(r"\\?\"), "{}", chosen.display());
    assert!(chosen.ends_with("deeper"));
}

#[test]
fn dialogs_open_at_the_field_or_its_nearest_existing_parent() {
    let dir = std::env::temp_dir();
    assert_eq!(dialog_start("", false), None);
    assert_eq!(dialog_start("relative\\dir", false), None);
    let nested = dir.join("xi-launcher-missing-a").join("b");
    let start = dialog_start(&nested.to_string_lossy(), false).unwrap();
    assert!(dir.starts_with(&start) || start.starts_with(&dir) || start == dir);
    // A file field opens in the file's folder.
    let exe = dir.join("pol.exe");
    assert_eq!(dialog_start(&exe.to_string_lossy(), true), dialog_start(&dir.to_string_lossy(), false));
}

#[test]
fn separate_completions_preserve_other_running_account() {
    let mut app = app();
    let first = app.accounts[0].key.clone();
    let second = app.accounts[1].key.clone();
    app.accounts[0].busy = true;
    app.accounts[1].busy = true;
    app.worker_event(WorkerEvent::Complete(
        second.clone(),
        Ok(LaunchResult {
            pid: 77,
            detail: "started".into(),
            alive: Arc::new(AtomicBool::new(true)),
            game_running: Arc::new(AtomicBool::new(true)),
            log: PathBuf::new(),
            outcome: Arc::new(std::sync::Mutex::new(None)),
            timing: "Signed in 2.8s · game started 9.4s".into(),
        }),
    ));
    assert!(app.accounts[0].busy);
    assert_eq!(app.accounts[1].pid, Some(77));
    assert_eq!(app.accounts[1].timing, "Signed in 2.8s · game started 9.4s");
    assert!(app.accounts[0].timing.is_empty());
    app.worker_event(WorkerEvent::Complete(first, Err("Rejected".into())));
    assert_eq!(app.accounts[1].pid, Some(77));
    assert_eq!(app.launch_keys(false).len(), 2);
}
#[test]
fn ending_ffxi_keeps_the_pol_session_and_makes_play_available() {
    let mut app = app();
    let key = app.accounts[0].key.clone();
    let game = Arc::new(AtomicBool::new(true));
    app.worker_event(WorkerEvent::Complete(key, Ok(LaunchResult {
        pid: 123, detail: "fixture".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: game.clone(), log: PathBuf::new(), outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })));
    assert!(app.accounts[0].is_playing());
    assert!(!app.accounts[0].can_play());
    game.store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert_eq!(app.accounts[0].pid, Some(123));
    assert!(app.has_sessions());
    assert!(app.accounts[0].can_play());
    assert!(app.accounts[0].status.contains("Signed in to PlayOnline"));
}
#[test]
fn file_changes_block_launches_but_scans_allow_them() {
    let mut app = app();
    app.maintenance = Some(MaintenanceKind::Check);
    assert_eq!(app.launchable_count(), 3);
    for kind in [
        MaintenanceKind::Update,
        MaintenanceKind::Repair,
        MaintenanceKind::Setup,
    ] {
        app.maintenance = Some(kind);
        assert_eq!(app.launchable_count(), 0);
        assert_eq!(app.selected_count(), 0);
    }
}
#[test]
fn maintenance_requires_explicit_path_and_keeps_sessions() {
    let mut app = app();
    app.preview = false;
    let _ = app.start_maintenance(MaintenanceKind::Update);
    assert!(app.maintenance.is_none());
    assert!(app.status.contains("Select the target folder"));
    app.settings.install_selected = true;
    app.accounts[0].pid = Some(42);
    let _ = app.start_maintenance(MaintenanceKind::Repair);
    assert!(app.maintenance.is_none());
    assert_eq!(app.accounts[0].pid, Some(42));
    assert!(app.status.contains("Close all games"));
}
#[test]
fn installer_buttons_select_their_folder_on_click_and_report_invalid_paths() {
    let root = std::env::temp_dir().join(format!("crystal-installer-click-{}", backend::unique_id().unwrap()));
    for kind in [MaintenanceKind::Download, MaintenanceKind::Extract, MaintenanceKind::Setup] {
        let mut app = app();
        app.preview = false;
        app.config.experiment = root.clone();
        app.settings.install_path = root.join("game").to_string_lossy().into_owned();
        app.settings.installer_path = root.join("installer").to_string_lossy().into_owned();
        assert!(!app.settings.installer_selected);
        // Task creation must not run the download, extractor or installer.
        assert_eq!(app.start_maintenance(kind).units(), 1);
        assert!(app.maintenance == Some(kind));
        assert!(app.settings.installer_selected);
    }
    let mut app = app();
    app.preview = false;
    app.settings.installer_path = "relative-installer".into();
    assert_eq!(app.start_maintenance(MaintenanceKind::Download).units(), 0);
    assert!(app.maintenance.is_none());
    assert!(app.status.contains("absolute folder path"));
    assert_eq!(app.installer_error.as_deref(), Some(app.status.as_str()));
    let _ = app.update(Message::InstallerPathChanged(root.join("installer").to_string_lossy().into_owned()));
    assert!(app.installer_error.is_none());
    std::fs::remove_dir_all(root).unwrap();
}

/// Downloads → Install's region: US with nothing installed, an installed client's region until one
/// is chosen, then the saved choice. The default folder follows it; a picked folder stays.
#[test]
fn the_installer_region_defaults_to_the_installed_client_and_the_choice_is_saved() {
    use retail_region::RetailRegion::{Eu, Jp, Us};
    let root = std::env::temp_dir().join(format!("crystal-installer-region-{}", backend::unique_id().unwrap()));
    let mut app = app();
    app.preview = false;
    app.config.experiment = root.clone();
    app.viewer = None;
    app.settings.installer_path = settings::installer_folder(Us);
    assert_eq!(app.installer_region(), Us);
    let _ = app.update(Message::PolEditions(vec![Eu]));
    assert_eq!((app.installer_region(), app.settings.installer_path.clone()), (Eu, settings::installer_folder(Eu)));
    assert!(app.settings.installer_path.ends_with("official-installer-EU"));
    let _ = app.update(Message::InstallerRegion(2));
    assert_eq!((app.installer_region(), app.settings.installer_path.clone()), (Jp, settings::installer_folder(Jp)));
    let saved = load_settings(&app.config).unwrap();
    assert_eq!((saved.installer_region, saved.installer_path), (Some(2), settings::installer_folder(Jp)));
    // A saved choice outranks the installed client; a folder the user picked is kept.
    let _ = app.update(Message::PolEditions(vec![Us]));
    assert_eq!(app.installer_region(), Jp);
    app.settings.installer_path = root.join("mine").to_string_lossy().into_owned();
    let _ = app.update(Message::InstallerRegion(0));
    assert_eq!((app.installer_region(), app.settings.installer_path.clone()), (Us, root.join("mine").to_string_lossy().into_owned()));
    // Not while an installer step runs, nor an unknown region.
    app.maintenance = Some(MaintenanceKind::Download);
    let _ = app.update(Message::InstallerRegion(1));
    app.maintenance = None;
    let _ = app.update(Message::InstallerRegion(7));
    assert_eq!(app.settings.installer_region, Some(0));
    let _ = std::fs::remove_dir_all(root);
}

fn viewer_report(state: patch::PatchState) -> viewer::Status {
    viewer::Status { folder: Some(PathBuf::from(r"C:\fixture\PlayOnlineViewer")),
        version: patch::PatchReport { state, local_version: "20070101_E".into(), remote_version: "20110829_E".into(),
            detail: "Viewer fixture".into(), files_verified: None }, core_error: None, stale: false }
}

#[test]
fn viewer_verdict_routes_to_downloads_without_replacing_the_game_verdict() {
    let mut app = app();
    app.viewer_generation = 2;
    let game = patch::PatchReport { state: patch::PatchState::Current, local_version: "30260904_1".into(),
        remote_version: "30260904_1".into(), detail: "Game fixture".into(), files_verified: None };
    app.patch = Some(game);
    let _ = app.update(Message::ViewerLoaded(1, viewer_report(patch::PatchState::Required)));
    assert!(app.viewer.is_none()); assert_eq!(app.main_action(), MainAction::Play);
    let _ = app.update(Message::ViewerLoaded(2, viewer_report(patch::PatchState::Required)));
    // Play updates PlayOnline when its sign-in finds it out of date: no Update button.
    assert!(app.viewer.as_ref().is_some_and(viewer::Status::requires_update));
    assert_eq!(app.main_action(), MainAction::Play);
    assert_eq!(app.patch.as_ref().unwrap().local_version, "30260904_1");
    app.viewer_generation = 3;
    let _ = app.update(Message::ViewerLoaded(3, viewer_report(patch::PatchState::Current)));
    app.worker_event(WorkerEvent::Viewer(2, viewer_report(patch::PatchState::Required)));
    assert_eq!(app.main_action(), MainAction::Play);
    assert_eq!(app.patch.as_ref().unwrap().state, patch::PatchState::Current);
}

#[test]
fn unified_update_refuses_idle_social_owners_and_unfinished_session_discovery() {
    let mut app = app(); app.preview = false;
    app.settings.install_selected = true;
    app.accounts[0].pid = Some(42);
    app.accounts[0].game_running = Some(Arc::new(AtomicBool::new(false)));
    let _ = app.start_maintenance(MaintenanceKind::Update);
    assert!(app.maintenance.is_none()); assert_eq!(app.accounts[0].pid, Some(42));
    assert!(app.status.contains("sign out PlayOnline sessions"));
    app.accounts[0].pid = None; app.vault_ready = false;
    let _ = app.start_maintenance(MaintenanceKind::Update);
    assert!(app.maintenance.is_none()); assert!(app.status.contains("Wait for saved accounts"));
}

#[test]
fn installer_steps_start_when_the_download_folder_contains_the_game_folder() {
    let root = std::env::temp_dir().join(format!("crystal-installer-parent-{}", backend::unique_id().unwrap()));
    let game = root.join("SquareEnix/FINAL FANTASY XI");
    std::fs::create_dir_all(&game).unwrap();
    for kind in [MaintenanceKind::Download, MaintenanceKind::Extract, MaintenanceKind::Setup] {
        let mut app = app();
        app.preview = false;
        app.config.experiment = root.clone();
        app.settings.install_path = game.to_string_lossy().into_owned();
        app.settings.installer_path = game.parent().unwrap().to_string_lossy().into_owned();
        app.settings.install_selected = true;
        // This is the user's actual parent/child layout. A click must schedule
        // the operation; checking a path must not execute external setup itself.
        assert_eq!(app.start_maintenance(kind).units(), 1);
        assert!(app.maintenance == Some(kind));
        assert!(app.settings.installer_selected && app.installer_error.is_none());
    }
    let mut app = app();
    app.preview = false;
    app.config.experiment = root.clone();
    app.settings.install_path = game.to_string_lossy().into_owned();
    app.settings.installer_path = game.parent().unwrap().to_string_lossy().into_owned();
    app.select_path(false);
    assert!(app.settings.install_selected, "selecting the game folder must also accept its installer parent");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn installer_failure_is_visible_until_the_next_folder_or_attempt() {
    let mut app = app();
    app.maintenance = Some(MaintenanceKind::Download);
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Download, Err("Metadata request failed".into()), None));
    assert_eq!(app.installer_error.as_deref(), Some("Metadata request failed"));
    let _ = app.update(Message::InstallerPathChanged(r"C:\Installer".into()));
    assert!(app.installer_error.is_none());
    app.maintenance = Some(MaintenanceKind::Download);
    app.installer_error = Some("Previous failure".into());
    let _ = app.update(Message::MaintenanceFinished(MaintenanceKind::Download, Ok("Downloaded".into()), None));
    assert!(app.installer_error.is_none());
}

/// A private-server Play starts on its own, ahead of everything that only concerns
/// PlayOnline (the first status read, FFXI's maintenance, the coordinated batch); a sign-in
/// only leaves it alone.
#[test]
fn a_private_server_play_starts_by_itself_and_a_sign_in_skips_it() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    app.accounts[0].account.mode = AccountMode::Private;
    let private = app.accounts[0].key.clone();
    assert!(app.launch_keys(false).contains(&private), "Play all includes it");
    assert!(!app.wait_keys().contains(&private), "it never waits for FFXI's maintenance");
    app.play(vec![private.clone()]);
    let launches = app.launches.replace(Vec::new()).unwrap();
    assert_eq!(launches.iter().map(|launch| launch.0.clone()).collect::<Vec<_>>(), vec![vec![private.clone()]]);
    assert!(!launches[0].1, "a game start, not a sign-in");
    app.connect(vec![private.clone()]);
    assert!(app.launches.as_ref().unwrap().is_empty());
    assert_eq!(app.accounts.len(), 3);
}

/// A private server that hasn't approved the new client holds FFXI's update in the folder its
/// accounts play from; its approval (the server's manifest) lifts the hold.
#[test]
fn a_private_servers_update_hold_stops_the_game_update_in_its_folder() {
    let mut app = app();
    app.preview = false;
    let root = std::env::temp_dir().join(format!("crystal-update-hold-{}", backend::unique_id().unwrap()));
    app.config.experiment = root.clone();
    app.settings.install_selected = true;
    app.settings.install_path = root.join("FFXI").to_string_lossy().into_owned();
    app.accounts[2].account.mode = AccountMode::Private;
    app.accounts[2].account.server = "ffxi.example.test".into();
    let folder = std::path::PathBuf::from(&app.settings.install_path);
    assert!(app.update_hold(&folder).is_none(), "no manifest read yet: no hold");
    let dir = root.join("iced-launcher").join("private-servers").join("ffxi.example.test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("manifest.json"), br#"{"schemaVersion":1,"client":{"requiredVersion":"30261006_0","updateApproved":false}}"#).unwrap();
    let _ = app.start_maintenance(MaintenanceKind::Update);
    assert!(app.maintenance.is_none());
    assert!(app.status.contains("ffxi.example.test") && app.status.contains("on hold"), "{}", app.status);
    assert!(app.update_hold(&root.join("Other FFXI")).is_none(), "another folder is not held");
    std::fs::write(dir.join("manifest.json"), br#"{"schemaVersion":1,"client":{"requiredVersion":"30261006_0","updateApproved":true}}"#).unwrap();
    assert!(app.update_hold(&folder).is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn legacy_native_default_loads_as_an_available_retail_launcher() {
    let root = std::env::temp_dir().join(format!("crystal-retail-default-{}", backend::unique_id().unwrap()));
    let config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    let settings = Settings { launch_with: crate::backend::LaunchWith::Native, ..Settings::default() };
    save_settings(&config, &settings).unwrap();
    assert_eq!(load_settings(&config).unwrap().launch_with, crate::backend::LaunchWith::Windower);
    let account = blank_account();
    assert_eq!(load_settings(&config).unwrap().launch_plan(&account).unwrap().tool, pol_less::Tool::Windower);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn operation_close_request_keeps_launcher_open() {
    let mut app = app();
    app.maintenance = Some(MaintenanceKind::Update);
    let _ = app.update(Message::CloseRequested);
    assert!(app.page == Page::Maintenance);
    assert!(!app.confirm_close);
    assert!(app.status.contains("before closing"));
}

#[test]
fn launcher_update_handshake_cannot_start_new_work_or_change_settings() {
    let mut app = app();
    app.self_update = self_update::State::Applying;
    let _ = app.update(Message::Navigate(Page::Friends));
    let _ = app.update(Message::NewAccount);
    let _ = app.update(Message::MainAction);
    let _ = app.update(Message::SelfUpdate(self_update::Msg::AutoCheck(false)));
    assert!(app.page == Page::Home);
    assert!(app.editor.is_none());
    assert!(app.maintenance.is_none());
    assert!(app.settings.launcher_auto_updates);
    assert!(app.launch_blocked());
    let _ = app.update(Message::CloseRequested);
    assert!(app.status.contains("update helper"));
}

/// The automatic restart for a downloaded update, as the app gathers its conditions.
#[test]
fn a_ready_update_restarts_by_itself_only_when_no_game_runs_and_nobody_uses_crystal() {
    use self_update::auto::{COUNTDOWN, IDLE_AFTER};
    let mut app = app();
    for row in &mut app.accounts {
        row.busy = false;
        row.batch_pending = false;
        row.queued_play = None;
        row.pid = None;
        row.game_running = None;
    }
    let start = Instant::now();
    let is_apply = |effect: &Effect<Message>| matches!(effect, Effect::Message(Message::SelfUpdate(self_update::Msg::Apply)));
    // Nothing downloaded: nothing happens.
    assert!(matches!(app.auto_restart_step(start), Effect::None));
    app.self_update = self_update::State::Ready(self_update::tests::fixture_stage("9.0.0"));
    // A game runs: it waits, with no timer.
    app.accounts[0].pid = Some(1234);
    app.accounts[0].game_running = Some(Arc::new(AtomicBool::new(true)));
    assert!(matches!(app.auto_restart_step(start), Effect::None));
    assert!(app.auto_restart.remaining(start).is_none());
    assert_eq!(app.auto_restart.next_due(app.auto_conditions(start), start), None);
    // The game ends (the session stays signed in at the main menu): scheduled.
    app.accounts[0].game_running.as_ref().unwrap().store(false, Ordering::Release);
    assert!(matches!(app.auto_restart_step(start), Effect::None));
    assert_eq!(app.auto_restart.remaining(start), Some(COUNTDOWN.as_secs()));
    assert!(is_apply(&app.auto_restart_step(start + COUNTDOWN)), "the countdown ran out");
    // Someone is using the window: it waits until they stop for a minute.
    let mut app = app_with_ready();
    app.window = Some(WindowId::unique());
    app.auto_restart.focused = true;
    app.auto_restart.last_input = Some(start);
    let _ = app.auto_restart_step(start);
    assert!(app.auto_restart.remaining(start).is_none());
    let idle = start + IDLE_AFTER;
    let _ = app.auto_restart_step(idle);
    assert_eq!(app.auto_restart.remaining(idle), Some(COUNTDOWN.as_secs()));
    // "Not now" postpones it.
    let _ = app.update(Message::SelfUpdate(self_update::Msg::Postpone));
    assert!(app.auto_restart.remaining(idle).is_none());
    assert!(matches!(app.auto_restart_step(idle + COUNTDOWN), Effect::None));
    assert!(app.auto_restart.postponed(Instant::now()));
    // The setting off: never by itself, saved off; the pill still works (its own test).
    let mut app = app_with_ready();
    let _ = app.update(Message::SelfUpdate(self_update::Msg::AutoRestart(false)));
    assert!(!app.settings.launcher_auto_restart);
    assert!(matches!(app.auto_restart_step(start), Effect::None));
    assert!(matches!(app.auto_restart_step(start + COUNTDOWN), Effect::None));
    assert!(app.auto_restart.remaining(start).is_none());
    assert!(app.can_restart_launcher());
    // An open dialog or a sign-in in flight waits too.
    let mut app = app_with_ready();
    app.show_settings = true;
    let _ = app.auto_restart_step(start);
    assert!(app.auto_restart.remaining(start).is_none());
    app.show_settings = false;
    app.accounts[0].busy = true;
    let _ = app.auto_restart_step(start);
    assert!(app.auto_restart.remaining(start).is_none());
    // Older settings files without the switch turn it on.
    let settings: Settings = serde_json::from_str(r#"{"install_path":""}"#).unwrap();
    assert!(settings.launcher_auto_restart);
}

/// 1.2.2 field report: FFXI returned to the Viewer, a batch Play relaunched it in the same
/// signed-in sessions, and Crystal restarted for its update while those games were starting
/// (every game closed). The batch's Ready clears busy and batch_pending, so `can_restart_launcher`
/// alone said yes; a download landing then applied at once.
#[test]
fn an_update_never_restarts_while_a_game_relaunches_in_a_signed_in_session() {
    use self_update::auto::COUNTDOWN;
    let mut app = app_with_ready();
    let downloaded = self_update::Msg::Downloaded(Ok(self_update::tests::fixture_stage("9.0.0")));
    // Signed in at the Viewer after FFXI returned: nothing is lost by restarting.
    let game = Arc::new(AtomicBool::new(false));
    app.accounts[0].pid = Some(1234);
    app.accounts[0].alive = Some(Arc::new(AtomicBool::new(true)));
    app.accounts[0].game_running = Some(game.clone());
    assert!(!app.game_active() && app.self_update_can_restart(&downloaded));
    // Batch Play into that session: preparing.
    app.accounts[0].batch_pending = true;
    app.accounts[0].busy = true;
    assert!(app.game_active() && !app.self_update_can_restart(&downloaded));
    // The batch reported Ready (busy and batch_pending clear) and the game is starting.
    app.accounts[0].batch_pending = false;
    app.accounts[0].busy = false;
    game.store(true, Ordering::Release);
    assert!(app.can_restart_launcher(), "Crystal itself has no work in flight");
    assert!(app.game_active());
    assert!(!app.self_update_can_restart(&downloaded), "a finished download waits for the games");
    assert!(app.self_update_can_restart(&self_update::Msg::Apply), "the pill's own click is unchanged");
    // Ready meanwhile: the automatic restart never schedules while it runs.
    let start = Instant::now();
    assert!(matches!(app.auto_restart_step(start), Effect::None));
    assert!(matches!(app.auto_restart_step(start + COUNTDOWN), Effect::None));
    assert!(app.auto_restart.remaining(start + COUNTDOWN).is_none());
}

fn app_with_ready() -> App {
    let mut app = app();
    for row in &mut app.accounts {
        row.busy = false;
        row.batch_pending = false;
        row.queued_play = None;
        row.pid = None;
        row.game_running = None;
    }
    app.self_update = self_update::State::Ready(self_update::tests::fixture_stage("9.0.0"));
    app
}

#[test]
fn launcher_restart_waits_for_saves_and_requests_but_allows_running_games() {
    let mut app = app();
    for account in &mut app.accounts {
        account.busy = false;
        account.batch_pending = false;
        account.pid = Some(1234);
    }
    assert!(app.can_restart_launcher());
    app.vault_saving = true;
    assert!(!app.can_restart_launcher());
    app.vault_saving = false;
    app.friends.pending = Some(("pending-request".into(), Instant::now()));
    assert!(!app.can_restart_launcher());
    app.friends.pending = None;
    app.accounts[0].batch_pending = true;
    assert!(!app.can_restart_launcher());
    app.accounts[0].batch_pending = false;
    assert!(app.can_restart_launcher());
}
#[test]
fn game_changes_wait_for_accounts_to_be_loaded_after_reopening() {
    let mut app = app();
    app.preview = false;
    app.accounts.clear();
    app.settings.install_selected = true;
    app.vault_ready = false;
    let _ = app.start_maintenance(MaintenanceKind::Update);
    assert!(app.maintenance.is_none());
    assert!(app.status.contains("running sessions"));
}

#[test]
fn dependency_setup_keeps_the_window_open_and_account_saves_keep_the_editor() {
    let mut app = app();
    let component = prereqs::Component::DirectPlay;
    let _ = prereqs::update(&mut app.prereqs, prereqs::Msg::Ask(component));
    let _ = prereqs::update(&mut app.prereqs, prereqs::Msg::Install(component));
    assert!(app.prereqs.busy());
    let _ = app.update(Message::CloseRequested);
    assert!(app.page == Page::Settings);
    assert!(app.status.contains("Windows component"));
    let original = app.accounts[0].key.clone();
    let other = app.accounts[1].key.clone();
    let _ = app.update(Message::EditAccount(original.clone()));
    app.vault_saving = true;
    let _ = app.update(Message::EditAccount(other));
    assert_eq!(app.editor.as_ref().unwrap().key.as_ref(), Some(&original));
}
#[test]
fn canonical_login_key_prevents_case_alias_duplicate() {
    let mut a = blank_account();
    a.pol_id = "abc12345".into();
    let mut b = a.clone();
    b.label = "another name".into();
    b.pol_id = "ABC12345".into();
    assert_eq!(account_key(&a), account_key(&b));
    a.mode = AccountMode::Private;
    b.mode = AccountMode::Private;
    a.server = "Example.COM".into();
    b.server = "example.com".into();
    b.pol_id = a.pol_id.clone();
    assert_eq!(account_key(&a), account_key(&b));
}

#[test]
fn coordinated_selection_includes_warm_and_signing_in_accounts_once() {
    let mut app = app();
    let warm = app.accounts[0].key.clone();
    let signing_in = app.accounts[1].key.clone();
    let running = app.accounts[2].key.clone();
    app.accounts[0].pid = Some(77);
    app.accounts[0].game_running = Some(Arc::new(AtomicBool::new(false)));
    app.accounts[1].busy = true;
    app.accounts[1].preparing_session = true;
    app.accounts[2].pid = Some(88);
    app.accounts[2].game_running = Some(Arc::new(AtomicBool::new(true)));
    assert_eq!(app.coordinated_keys(&[warm.clone(), signing_in.clone(), warm.clone(), running]), vec![warm.clone(), signing_in]);
    app.accounts[1].batch_pending = true;
    assert_eq!(app.coordinated_keys(&app.launch_keys(false)), vec![warm]);
}

#[test]
fn coordinated_play_before_worker_publication_keeps_both_original_signins() {
    let mut app = app();
    app.preview = false;
    app.config.experiment = std::env::temp_dir().join(format!("crystal-unpublished-batch-{}", std::process::id()));
    assert!(!app.config.experiment.exists());
    let keys: Vec<_> = app.accounts[..2].iter_mut().map(|row| {
        row.busy = true;
        row.preparing_session = true;
        row.key.clone()
    }).collect();
    let clicked = Instant::now() - Duration::from_secs(2);
    app.start_launch(keys, false, clicked, None);
    app.start_queued_plays();
    for row in &app.accounts[..2] {
        assert_eq!(row.queued_play, Some(clicked));
        assert!(row.busy && row.preparing_session && !row.batch_pending);
        assert!(row.pid.is_none() && !row.problem);
    }
    assert!(app.receiver.try_recv().is_err(), "keep both startup workers; never start a second sign-in");
}

#[test]
fn coordinated_play_ignores_late_startup_results_and_keeps_other_game() {
    let mut app = app();
    let key = app.accounts[0].key.clone();
    app.accounts[0].launch_generation = 2;
    app.accounts[0].batch_pending = true;
    app.accounts[0].batch_requested = true;
    app.accounts[0].busy = true;
    app.accounts[0].status = "Waiting for the other account".into();
    app.accounts[1].pid = Some(88);
    app.accounts[1].game_running = Some(Arc::new(AtomicBool::new(true)));
    app.worker_event(WorkerEvent::Progress(key.clone(), "Old startup progress".into()).for_launch(&key, 1));
    app.worker_event(WorkerEvent::Complete(key.clone(), Err("Old startup result".into())).for_launch(&key, 1));
    let row = &app.accounts[0];
    assert!(row.batch_pending && row.busy && row.game_start_pending());
    assert!(!row.can_play());
    assert_eq!(row.status, "Waiting for the other account");
    app.worker_event(WorkerEvent::Complete(key.clone(), Ok(LaunchResult {
        pid: 77, detail: "started together".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: Arc::new(AtomicBool::new(true)), log: PathBuf::new(),
        outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })).for_launch(&key, 2));
    assert!(!app.accounts[0].batch_pending && !app.accounts[0].busy);
    assert_eq!(app.accounts[0].pid, Some(77));
    assert_eq!(app.accounts[1].pid, Some(88));
    assert!(app.accounts[1].is_playing());
}

#[test]
fn failed_coordinated_play_keeps_prepared_session_and_error() {
    let mut app = app();
    let key = app.accounts[0].key.clone();
    app.accounts[0].batch_pending = true;
    app.accounts[0].batch_requested = true;
    app.accounts[0].busy = true;
    app.worker_event(WorkerEvent::Complete(key.clone(), Ok(LaunchResult {
        pid: 77, detail: "signed in".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: Arc::new(AtomicBool::new(false)), log: PathBuf::from("batch.log"),
        outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })));
    app.worker_event(WorkerEvent::Complete(key, Err("Another account needs an update".into())));
    let row = &app.accounts[0];
    assert!(!row.batch_pending && !row.busy && row.can_play());
    assert_eq!(row.pid, Some(77));
    assert_eq!(row.log, Some(PathBuf::from("batch.log")));
    assert!(row.problem);
    assert_eq!(row.status, "Another account needs an update");
    assert!(app.has_sessions(), "a prepared signed-in session still blocks file changes");
}

#[test]
fn coordinated_play_stays_pending_when_an_old_host_exits() {
    let mut app = app();
    app.accounts[0].batch_pending = true;
    app.accounts[0].batch_requested = true;
    app.accounts[0].busy = true;
    app.accounts[0].pid = Some(77);
    app.accounts[0].alive = Some(Arc::new(AtomicBool::new(false)));
    let _ = app.update(Message::Tick);
    let row = &app.accounts[0];
    assert!(row.batch_pending && row.busy && row.game_start_pending());
    assert!(!row.can_play());
    assert!(app.has_sessions());
}

#[test]
fn native_preparation_does_not_finish_batch_timing_before_release() {
    let mut app = app();
    let row = &mut app.accounts[0];
    row.pid = Some(77);
    row.alive = Some(Arc::new(AtomicBool::new(true)));
    row.game_running = Some(Arc::new(AtomicBool::new(true)));
    row.batch_pending = true;
    row.batch_requested = true;
    row.busy = true;
    row.play_timing = Some(launch_timing::Timeline::begin_at(Instant::now()));
    row.status = "Waiting for the other account".into();
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].play_timing.is_some());
    assert!(app.accounts[0].busy);
    assert_eq!(app.accounts[0].status, "Waiting for the other account");
    app.accounts[0].batch_requested = false;
    app.accounts[0].batch_pending = false;
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].play_timing.is_none());
    assert!(!app.accounts[0].busy);
    assert!(!app.accounts[0].timing.is_empty());
}

#[test]
fn reconnected_batch_stops_blocking_play_after_ownership_ends() {
    let mut app = app();
    app.accounts[0].pid = Some(77);
    app.accounts[0].alive = Some(Arc::new(AtomicBool::new(true)));
    app.accounts[0].game_running = Some(Arc::new(AtomicBool::new(false)));
    app.accounts[0].batch_pending = true;
    app.accounts[0].batch_requested = false;
    assert!(!app.accounts[0].can_play());
    let _ = app.update(Message::Tick);
    assert!(!app.accounts[0].batch_pending);
    assert!(app.accounts[0].can_play());
    assert_eq!(app.accounts[0].pid, Some(77));
}

#[test]
fn background_signin_accepts_one_play_and_keeps_its_click_time() {
    let mut app = app();
    app.preview = false;
    let row = &mut app.accounts[0];
    row.busy = true;
    row.preparing_session = true;
    let key = row.key.clone();
    assert!(row.can_play(), "startup sign-in must not disable Play");
    assert!(!row.game_start_pending(), "background preparation is not a requested game launch");
    assert!(app.launch_keys(true).contains(&key));
    let clicked = Instant::now() - Duration::from_secs(2);
    app.start_launch(vec![key.clone()], false, clicked, None);
    let row = &app.accounts[0];
    assert_eq!(row.queued_play, Some(clicked));
    assert!(row.game_start_pending());
    assert!(!row.can_play(), "a second click must not queue another launch");
    assert!(row.pid.is_none());
    assert!(!app.launch_keys(true).contains(&key));
    app.play(vec![key.clone()]);
    assert_eq!(app.accounts[0].queued_play, Some(clicked));
    app.start_queued_plays();
    assert_eq!(app.accounts[0].queued_play, Some(clicked), "an unpublished or legacy worker must keep the original queue");
    assert!(!app.accounts[0].batch_pending);
    app.worker_event(WorkerEvent::Progress(key, "Signing in to PlayOnline…".into()));
    assert!(app.accounts[0].status_text().contains("FFXI will start when sign-in finishes."));
    assert!(app.receiver.try_recv().is_err(), "queued Play reuses the existing worker");
}

/// pol-spec 07d §5.4: a refused sign-in (password, OTP, any verdict on the account) drops
/// the queued Play as before; nothing signs in a second time.
#[test]
fn failed_background_signin_clears_queued_play_without_retrying() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    for failure in ["Sign-in failed",
        "Game loader error during startup: Native PlayOnline member login failed (state -1036, POL result -1036). (Windower via crystal-loader).",
        &format!("{} installed 30260904_1, official server 30260905_0. Open Downloads.", patch::UPDATE_REQUIRED)] {
        let row = &mut app.accounts[0];
        row.busy = true;
        row.preparing_session = true;
        row.queued_play = Some(Instant::now());
        row.launch_trigger = Some(launch_timing::LaunchTrigger::StartupSignIn);
        let key = row.key.clone();
        app.worker_event(WorkerEvent::Complete(key, Err(failure.to_owned())));
        let row = &app.accounts[0];
        assert!(!row.busy && !row.preparing_session);
        assert!(row.queued_play.is_none() && row.pid.is_none() && row.network_wait.is_none());
        assert!(row.problem && row.can_play());
        assert_eq!(row.status, failure);
    }
    assert!(app.launches.as_ref().unwrap().is_empty(), "never a second login");
    assert!(app.receiver.try_recv().is_err());
}

/// 07d §5.4: a pre-sign-in whose host went away re-issues its queued Play at once, cold,
/// with the original click; one that failed on the network waits for the probe first.
/// Neither happens twice.
#[test]
fn failed_background_signin_reissues_its_queued_play_once_for_network_or_transient_failures() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    let key = app.accounts[0].key.clone();
    let clicked = Instant::now() - Duration::from_secs(3);
    let queue = |app: &mut App| {
        let row = &mut app.accounts[0];
        row.busy = true;
        row.preparing_session = true;
        row.queued_play = Some(clicked);
        row.launch_trigger = Some(launch_timing::LaunchTrigger::StartupSignIn);
    };
    queue(&mut app);
    app.worker_event(WorkerEvent::Complete(key.clone(), Err("The background session worker ended before starting the game.".into())));
    assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![key.clone()], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay))]);
    app.launches = Some(Vec::new());
    queue(&mut app);
    app.worker_event(WorkerEvent::Complete(key.clone(), Err("Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).".into())));
    let row = &app.accounts[0];
    assert_eq!(row.network_wait, Some(NetworkWait::Reissue(clicked)));
    assert!(row.status_text().ends_with("Crystal will try again when the network is back."), "{}", row.status_text());
    assert!(app.launches.as_ref().unwrap().is_empty(), "not before the network is back");
    let generation = row.launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert_eq!(app.launches.as_ref().unwrap().as_slice(), &[(vec![key.clone()], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay))]);
    assert!(app.accounts[0].network_wait.is_none());
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1, "at most once");
    // The re-issued Play is an ordinary Play: its own failure queues nothing more.
    app.accounts[0].busy = true;
    app.accounts[0].launch_trigger = Some(launch_timing::LaunchTrigger::ReissuedPlay);
    app.worker_event(WorkerEvent::Complete(key.clone(), Err("The background session worker ended.".into())));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
    assert!(app.accounts[0].network_wait.is_none());
}

/// Finding: a pre-sign-in that took the row's one-time code never re-issues its queued
/// Play (or retries a startup sign-in) by itself: the code is spent, and a second login
/// without one is refused at Square Enix. The row asks for a new code instead; a code typed
/// since makes the re-issue safe again.
#[test]
fn a_failed_signin_that_spent_its_one_time_code_is_never_reissued() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    let key = app.accounts[0].key.clone();
    let clicked = Instant::now();
    let network = "Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).";
    for (failure, trigger) in [(network, launch_timing::LaunchTrigger::Connect), ("The background session worker ended before starting the game.", launch_timing::LaunchTrigger::Connect),
        (network, launch_timing::LaunchTrigger::StartupSignIn)] {
        let row = &mut app.accounts[0];
        row.busy = true;
        row.preparing_session = true;
        row.queued_play = (trigger != launch_timing::LaunchTrigger::StartupSignIn).then_some(clicked);
        row.launch_trigger = Some(trigger);
        row.otp_spent = true;
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(failure.to_owned())));
        let row = &app.accounts[0];
        assert!(row.network_wait.is_none() && row.queued_play.is_none(), "{failure}");
        assert!(row.status_text().ends_with("That one-time code was used. Enter a new one, then press Play."), "{}", row.status_text());
    }
    assert!(app.launches.as_ref().unwrap().is_empty(), "no automatic login without a code");
    // A new code typed since: the re-issue may use it.
    let row = &mut app.accounts[0];
    (row.busy, row.preparing_session, row.queued_play, row.otp) = (true, true, Some(clicked), "123456".into());
    app.worker_event(WorkerEvent::Complete(key.clone(), Err("The background session worker ended before starting the game.".into())));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
}

/// Finding: a host lost after the pre-sign-in reached authentication had its login taken;
/// its queued Play is not re-issued as a second login by itself.
#[test]
fn a_transient_failure_after_authentication_is_not_reissued() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    let root = std::env::temp_dir().join(format!("crystal-reissue-auth-{}", crate::backend::unique_id().unwrap()));
    app.config = crate::backend::Config { experiment: root.clone(), repository: root.clone(), game_install: None, dat_overlays: Vec::new() };
    let key = app.accounts[0].key.clone();
    let row = &mut app.accounts[0];
    (row.busy, row.preparing_session, row.queued_play) = (true, true, Some(Instant::now()));
    let config = app.settings.config_for(&app.config, &app.accounts[0].account);
    session_worker::save_authenticated_record_for_test(&config, &app.accounts[0].account);
    app.worker_event(WorkerEvent::Complete(key, Err("Game exited during startup (Windower via crystal-loader).".into())));
    assert!(app.launches.as_ref().unwrap().is_empty(), "the login was already taken");
    assert!(app.accounts[0].queued_play.is_none() && app.accounts[0].network_wait.is_none());
    let _ = std::fs::remove_dir_all(root);
}

/// Finding: once the probe gives up, the row stops promising a retry.
#[test]
fn a_probe_that_gives_up_takes_back_the_retry_promise() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    let key = app.accounts[0].key.clone();
    let network = "Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).";
    let row = &mut app.accounts[0];
    (row.busy, row.preparing_session, row.launch_trigger) = (true, true, Some(launch_timing::LaunchTrigger::StartupSignIn));
    app.worker_event(WorkerEvent::Complete(key.clone(), Err(network.into())));
    assert!(app.accounts[0].status_text().ends_with("Crystal will try again when the network is back."));
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), false).for_launch(&key, generation));
    let status = app.accounts[0].status_text();
    assert!(!status.contains("Crystal will try again") && status.starts_with(network)
        && status.ends_with("The network did not come back. Sign in again once it is."), "{status}");
}

/// 07d §5.5 step 3: a startup sign-in that failed on the network starts once more, as a
/// startup retry, when the probe sees the network; a non-network failure never does, and
/// neither does a probe that gave up or a row the user started meanwhile.
#[test]
fn a_startup_signin_that_failed_on_the_network_retries_once_when_it_returns() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    let key = app.accounts[0].key.clone();
    app.settings.account_no_otp.insert(key.clone());
    let fail = |app: &mut App, trigger, error: &str| {
        app.accounts[0].busy = true;
        app.accounts[0].preparing_session = true;
        app.accounts[0].launch_trigger = Some(trigger);
        app.worker_event(WorkerEvent::Complete(app.accounts[0].key.clone(), Err(error.to_owned())));
    };
    let network = "Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).";
    fail(&mut app, launch_timing::LaunchTrigger::StartupSignIn, "Sign-in failed");
    assert!(app.accounts[0].network_wait.is_none());
    fail(&mut app, launch_timing::LaunchTrigger::Connect, network);
    assert!(app.accounts[0].network_wait.is_none(), "only the startup sign-in retries by itself");
    fail(&mut app, launch_timing::LaunchTrigger::StartupSignIn, network);
    assert_eq!(app.accounts[0].network_wait, Some(NetworkWait::StartupRetry));
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), false).for_launch(&key, generation));
    assert!(app.launches.as_ref().unwrap().is_empty() && app.accounts[0].network_wait.is_none(), "the probe gave up");
    fail(&mut app, launch_timing::LaunchTrigger::StartupSignIn, network);
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    let launches = app.launches.as_ref().unwrap();
    assert_eq!(launches.len(), 1);
    assert_eq!((&launches[0].0, launches[0].1, launches[0].3), (&vec![key.clone()], true, Some(launch_timing::LaunchTrigger::StartupRetry)));
    // The retry failing on the network again is not retried.
    fail(&mut app, launch_timing::LaunchTrigger::StartupRetry, network);
    assert!(app.accounts[0].network_wait.is_none());
    // A row the user started meanwhile keeps its own launch.
    fail(&mut app, launch_timing::LaunchTrigger::StartupSignIn, network);
    app.accounts[0].busy = true;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
}

/// 07d §1.5 "Who": retail accounts with a tool plan and no session of any kind; pre-signed
/// accounts not while they are to sign in (here: when Crystal opens), OTP accounts and
/// unknown ones too.
#[test]
fn park_eligibility_excludes_startup_and_busy_accounts_but_not_otp_ones() {
    let mut app = app();
    app.settings.account_no_otp.insert(app.accounts[1].key.clone());
    app.startup_signin_pending = true;
    app.accounts[2].otp = "123456".into();
    assert_eq!(app.park_keys().len(), 2);
    app.accounts[0].busy = true;
    let keys: Vec<String> = app.park_keys().into_iter().map(|(row, ..)| row).collect();
    assert_eq!(keys, vec![app.accounts[2].key.clone()]);
    let (_, key, plan) = app.park_keys().remove(0);
    assert_eq!((key.tool, key.profile.as_str(), plan.tool), ("Windower 4", "", pol_less::Tool::Windower));
    app.accounts[2].pid = Some(77);
    assert!(app.park_keys().is_empty(), "a running session is never parked beside");
    app.accounts[2].pid = None;
    app.accounts[2].account.mode = AccountMode::Private;
    assert!(app.park_keys().is_empty());
}

/// 07d §1.5 "Kill": parked hosts close while files change (polcore.dll is loaded in a parked
/// pol.exe), before the profile editor and on exit; none park before the startup sign-ins.
#[test]
fn parks_close_while_files_change_and_on_exit() {
    let mut app = app();
    app.preview = false;
    app.config.experiment = std::env::temp_dir().join(format!("crystal-park-close-{}", backend::unique_id().unwrap()));
    let park = |app: &mut App| {
        let (account, row) = (app.accounts[0].account.clone(), app.accounts[0].key.clone());
        let plan = app.settings.launch_plan(&account).unwrap();
        // A stand-in worker that exits once its stdin closes.
        let worker = session_worker::spawn_test_park(std::process::Command::new("sort"), &app.config, &account, &plan).unwrap();
        app.parks.insert(pol_less::park::ParkKey::for_plan(&account, &plan), row, Default::default(), worker);
        assert!(!app.parks.is_empty());
    };
    park(&mut app);
    // A read-only installation scan keeps the parked host (a Play right after it stays warm).
    app.installation_loading = true;
    app.tend_parks(Instant::now());
    assert!(!app.parks.is_empty(), "an installation scan changes no files");
    app.installation_loading = false;
    // Files really changing close it.
    app.reshade.busy = true;
    app.park_tended = None;
    app.tend_parks(Instant::now());
    assert!(app.parks.is_empty(), "closed while files change");
    app.reshade.busy = false;
    park(&mut app);
    let _ = app.update(Message::EditWindowerProfiles);
    assert!(app.parks.is_empty(), "the profile editor needs the profile guard");
    app.windower_editor_open = false;
    park(&mut app);
    let _ = app.update(Message::CloseRequested);
    assert!(app.parks.is_empty(), "closed on exit");
    // Nothing parks before the startup sign-ins were issued, nor in previews.
    app.startup_issued = None;
    app.tend_parks(Instant::now() + Duration::from_secs(60));
    assert!(app.parks.is_empty());
}

#[test]
fn profile_editor_holds_new_launches_and_preserves_pending_startup_signins() {
    let mut app = app();
    app.windower_editor_open = true;
    app.startup_signin_pending = true;
    app.launches = Some(Vec::new());
    assert!(app.launch_blocked());
    assert!(app.take_startup_signins().is_empty());
    assert!(app.startup_signin_pending);
    app.start_launch_now(vec![app.accounts[0].key.clone()], false, Instant::now(), None, false);
    assert!(app.launches.as_ref().unwrap().is_empty());
    assert!(app.status.contains("profile editor"));
    app.windower_editor_open = false;
    assert!(!app.launch_blocked());
}

/// 07d §5.5 steps 1-2 (08 §1 row 9): a startup key whose row is still busy with the
/// previous session stays pending with a status, starts once the row is free, and is
/// skipped with a status after STARTUP_WAIT_LIMIT. An idle signed-in session needs nothing.
#[test]
fn startup_signin_waits_for_a_closing_session_then_starts_or_is_skipped() {
    let mut app = app();
    app.preview = false;
    app.config.experiment = std::env::temp_dir().join(format!("crystal-startup-wait-{}", backend::unique_id().unwrap()));
    app.startup_signin_pending = true;
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    app.settings.account_no_otp = keys.iter().cloned().collect();
    app.accounts[0].busy = true; // the previous game's host is still closing
    app.accounts[1].pid = Some(77);
    app.accounts[1].game_running = Some(Arc::new(AtomicBool::new(false))); // signed in and idle
    app.accounts[2].account.mode = AccountMode::Private;
    assert!(app.take_startup_signins().is_empty());
    assert!(app.startup_issued.is_some());
    assert_eq!(app.startup_waiting.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(), vec![keys[0].clone()]);
    assert_eq!(app.accounts[0].status, "Startup sign-in waits for the previous session to end.");
    let now = Instant::now();
    assert!(app.take_waiting_startups(now).is_empty(), "still busy");
    app.accounts[0].busy = false;
    assert_eq!(app.take_waiting_startups(now), vec![keys[0].clone()]);
    assert!(app.startup_waiting.is_empty());
    // Skipped after the limit.
    app.accounts[0].busy = true;
    app.startup_waiting = vec![(keys[0].clone(), now)];
    assert!(app.take_waiting_startups(now + STARTUP_WAIT_LIMIT - Duration::from_secs(1)).is_empty());
    assert_eq!(app.startup_waiting.len(), 1);
    assert!(app.take_waiting_startups(now + STARTUP_WAIT_LIMIT).is_empty());
    assert!(app.startup_waiting.is_empty());
    assert_eq!(app.accounts[0].status, "Startup sign-in skipped: a session is still running.");
}

/// Owner decision 2026-10-12: a test app whose first account signs in when Crystal opens
/// (2026-10-13: it is known to sign in without a one-time code; the switch is on by default).
fn resign_app(name: &str) -> (App, String) {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    app.config.experiment = std::env::temp_dir().join(format!("crystal-{name}-{}", backend::unique_id().unwrap()));
    let key = app.accounts[0].key.clone();
    assert!(app.settings.auto_sign_in);
    app.settings.sign_in_after_game = true;
    app.settings.account_no_otp.insert(key.clone());
    (app, key)
}

/// Row `index` runs its game (seen on a Tick), then the session ends with `report`.
fn game_ends(app: &mut App, index: usize, report: launch_log::ExitReport) {
    let alive = Arc::new(AtomicBool::new(true));
    let row = &mut app.accounts[index];
    row.pid = Some(77);
    row.alive = Some(alive.clone());
    row.game_running = Some(Arc::new(AtomicBool::new(true)));
    row.outcome = Some(Arc::new(std::sync::Mutex::new(Some(report))));
    let _ = app.update(Message::Tick);
    assert!(app.accounts[index].game_seen);
    alive.store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(app.accounts[index].pid.is_none());
}

fn signed_out() -> launch_log::ExitReport { launch_log::classify_loader_end(&launch_log::LoaderEnd::SignedOut, Duration::from_secs(900)) }

#[test]
fn shutdown_buttons_sign_back_in_once_in_either_completion_order() {
    for selected in [true, false] {
        for completion_first in [true, false] {
            let (mut app, _) = resign_app("shutdown-resign-order");
            let mut ended = Vec::new();
            for (index, row) in app.accounts.iter_mut().enumerate() {
                row.pid = Some(77 + index as u32);
                row.busy = false;
                row.selected = index == 0;
                row.alive = Some(Arc::new(AtomicBool::new(true)));
                row.game_running = Some(Arc::new(AtomicBool::new(true)));
                let outcome = Arc::new(std::sync::Mutex::new(None));
                row.outcome = Some(outcome.clone());
                app.settings.account_no_otp.insert(row.key.clone());
                if !selected || row.selected {
                    ended.push((row.key.clone(), row.launch_generation, row.alive.clone().unwrap(), outcome));
                }
            }
            let _ = app.update(Message::Tick);
            let message = if selected { Message::ShutdownSelected } else { Message::ShutdownAll };
            assert_eq!(app.update(message).units(), ended.len());
            for (key, generation, _, _) in &ended {
                let row = app.accounts.iter().find(|row| row.key == *key).unwrap();
                assert!(row.shutting_down && row.game_seen);
                assert!(!row.resign_declined && !row.signed_out, "shutdown must respect automatic sign-in");
                if completion_first {
                    let _ = app.update(Message::GameShutdownFinished(key.clone(), *generation, Ok(())));
                    let row = app.accounts.iter().find(|row| row.key == *key).unwrap();
                    assert!(row.game_seen && row.pid.is_some() && row.outcome.is_some(), "the exit observer still owns the report");
                }
            }
            // The monitor publishes the exit report before releasing `alive`.
            for (_, _, alive, outcome) in &ended {
                *outcome.lock().unwrap() = Some(signed_out());
                alive.store(false, Ordering::Release);
            }
            let _ = app.update(Message::Tick);
            for (key, generation, _, _) in &ended {
                let row = app.accounts.iter().find(|row| row.key == *key).unwrap();
                let pending = row.resign.expect("one pending sign-in after game shutdown");
                assert!(!pending.network, "preserve the clean exit report");
                if !completion_first {
                    let _ = app.update(Message::GameShutdownFinished(key.clone(), *generation, Ok(())));
                }
                let row = app.accounts.iter().find(|row| row.key == *key).unwrap();
                assert_eq!(row.resign.unwrap().ended, pending.ended, "completion must not restart the delay");
                assert!(row.status_text().ends_with(t("Signing back in after the game closed.")));
                assert!(!row.shutting_down && row.pid.is_none());
            }
            let _ = app.update(Message::Tick);
            let keys = app.take_resigns(Instant::now() + RESIGN_DELAY);
            assert_eq!(keys, ended.iter().map(|(key, ..)| key.clone()).collect::<Vec<_>>());
            app.start_resigns(keys, launch_timing::LaunchTrigger::SignInAfterGame);
            assert_eq!(app.launches.as_ref().unwrap().len(), 1);
            assert!(app.take_resigns(Instant::now() + RESIGN_DELAY * 2).is_empty());
            assert!(app.accounts.iter().filter(|row| selected && !row.selected).all(|row| row.is_playing() && row.resign.is_none()));
        }
    }
}

#[test]
fn explicit_signout_cancels_relogin_even_with_a_shutdown_result_still_pending() {
    for signout_before_end in [true, false] {
        let (mut app, key) = resign_app("shutdown-resign-cancel");
        let alive = Arc::new(AtomicBool::new(true));
        let outcome = Arc::new(std::sync::Mutex::new(None));
        let row = &mut app.accounts[0];
        row.pid = Some(77);
        row.alive = Some(alive.clone());
        row.game_running = Some(Arc::new(AtomicBool::new(true)));
        row.outcome = Some(outcome.clone());
        row.selected = true;
        let generation = row.launch_generation;
        let _ = app.update(Message::Tick);
        assert_eq!(app.update(Message::ShutdownSelected).units(), 1);
        if signout_before_end {
            let _ = app.update(Message::AccountSignOut(key.clone()));
        }
        *outcome.lock().unwrap() = Some(signed_out());
        alive.store(false, Ordering::Release);
        let _ = app.update(Message::Tick);
        if !signout_before_end {
            assert!(app.accounts[0].resign.is_some());
            let _ = app.update(Message::AccountSignOut(key.clone()));
        }
        let _ = app.update(Message::GameShutdownFinished(key, generation, Ok(())));
        let _ = app.update(Message::Tick);
        assert!(app.accounts[0].signed_out && app.accounts[0].resign.is_none());
        assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty());
        assert!(app.launches.as_ref().unwrap().is_empty());
    }
}

#[test]
fn closing_a_game_stays_signed_out_by_default_even_with_startup_signin_enabled() {
    let (mut app, key) = resign_app("close-game-stays-out");
    app.settings.sign_in_after_game = false;
    let mut older = serde_json::to_value(Settings::default()).unwrap();
    older.as_object_mut().unwrap().remove("sign_in_after_game");
    assert!(!serde_json::from_value::<Settings>(older).unwrap().sign_in_after_game);
    app.startup_waiting.push((key.clone(), Instant::now()));
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].signed_out && app.accounts[0].resign.is_none());
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty());
    assert!(app.take_waiting_startups(Instant::now()).is_empty());
    assert!(!app.park_keys().iter().any(|(row, ..)| row == &key), "no replacement parked pol.exe");
    assert!(app.launches.as_ref().unwrap().is_empty());
    app.startup_signin_pending = true;
    assert!(app.take_startup_signins().is_empty(), "an explicit Play or next launcher window owns the next sign-in");
}

#[test]
fn account_signout_is_available_without_opening_friends_and_blocks_recreation() {
    let mut app = app();
    app.page = Page::Home;
    let key = app.accounts[0].key.clone();
    app.settings.account_no_otp.insert(key.clone());
    app.accounts[0].pid = Some(77);
    app.accounts[0].game_running = Some(Arc::new(AtomicBool::new(false)));
    assert!(app.accounts[0].can_sign_out());
    assert_eq!(account_row::row_state(&app, &app.accounts[0]), account_row::RowState::SignedIn);
    let _ = app.update(Message::EditAccount(key.clone()));
    let _ = app.update(Message::CloseEditor);
    assert_eq!(app.update(Message::AccountSignOut(key.clone())).units(), 1);
    assert!(app.accounts[0].busy && app.accounts[0].signed_out && !app.accounts[0].can_play());
    // Finished can be published while Windower still has its profile save.
    app.accounts[0].alive = Some(Arc::new(AtomicBool::new(false)));
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].signing_out && !app.accounts[0].busy && !app.accounts[0].can_play());
    assert!(!app.can_restart_launcher() && app.sessions_block_update());
    let generation = app.accounts[0].launch_generation;
    let _ = app.update(Message::AccountSignedOut(key.clone(), generation, Ok(())));
    assert!(app.accounts[0].pid.is_none() && app.accounts[0].can_play());
    assert_eq!(app.accounts[0].status, "Signed out of PlayOnline");
    assert!(!app.park_keys().iter().any(|(row, ..)| row == &key));
    assert!(app.take_waiting_startups(Instant::now()).is_empty());
}

#[test]
fn launcher_close_drops_automatic_signins_and_preserves_running_game_state() {
    let (mut app, key) = resign_app("close-window");
    app.startup_waiting.push((key.clone(), Instant::now()));
    let alive = Arc::new(AtomicBool::new(true));
    let game = Arc::new(AtomicBool::new(true));
    app.accounts[0].pid = Some(77);
    app.accounts[0].alive = Some(alive.clone());
    app.accounts[0].game_running = Some(game.clone());
    let _ = app.update(Message::CloseRequested);
    assert!(app.closing && app.launch_blocked());
    assert!(app.startup_waiting.is_empty() && !app.startup_signin_pending);
    assert!(app.accounts[0].signed_out && app.accounts[0].resign.is_none());
    assert!(alive.load(Ordering::Acquire) && game.load(Ordering::Acquire));
    assert!(app.accounts[0].is_playing());
    let _ = app.update(Message::Tick);
    let _ = app.update(Message::Launch(key));
    assert!(app.launches.as_ref().unwrap().is_empty(), "shutdown cannot start more work");
    let size = Size::new(1280.0, 720.0);
    let _ = app.update(Message::WindowResized(size));
    assert_eq!(app.viewport, size, "window controls stay responsive during sign-out");
    app.title_held = true;
    let _ = app.update(Message::TitleMoved);
    assert!(!app.title_held);
    let _ = app.update(Message::WindowMaximized(true));
    assert!(app.maximized);
}

/// After a normal quit (or FFXI's usual -14) the account signs back in, as a pre-sign-in
/// named "sign-in after game", only once its row has been free for RESIGN_DELAY (≥ 3 s).
/// The row says so, and a Play during that sign-in queues behind it (07d §5.4).
#[test]
fn a_startup_account_signs_back_in_after_its_game_closes_after_the_delay() {
    assert!(RESIGN_DELAY >= Duration::from_secs(3), "human-plausible: no instant sign-in after a sign-out");
    for end in [launch_log::LoaderEnd::SignedOut, launch_log::LoaderEnd::Exit(-14)] {
        let (mut app, key) = resign_app("resign");
        game_ends(&mut app, 0, launch_log::classify_loader_end(&end, Duration::from_secs(900)));
        let resign = app.accounts[0].resign.expect("pending");
        assert!(!resign.network && resign.free_since.is_some(), "{end:?}");
        assert!(app.accounts[0].status_text().ends_with("Signing back in after the game closed."), "{}", app.accounts[0].status_text());
        assert!(app.launches.as_ref().unwrap().is_empty(), "not at once");
        let free = resign.free_since.unwrap();
        assert!(app.take_resigns(free + RESIGN_DELAY - Duration::from_millis(1)).is_empty());
        let keys = app.take_resigns(free + RESIGN_DELAY);
        assert_eq!(keys, vec![key.clone()]);
        assert!(app.accounts[0].resign.is_none());
        app.start_resigns(keys, launch_timing::LaunchTrigger::SignInAfterGame);
        let launches = app.launches.as_ref().unwrap();
        assert_eq!((&launches[0].0, launches[0].1, launches[0].3), (&vec![key.clone()], true, Some(launch_timing::LaunchTrigger::SignInAfterGame)));
        assert!(app.take_resigns(free + RESIGN_DELAY * 2).is_empty(), "once");
    }
    // The sign-in in flight: its progress says so, and a Play queues on it.
    let (mut app, key) = resign_app("resign-play");
    let row = &mut app.accounts[0];
    (row.busy, row.preparing_session, row.launch_trigger) = (true, true, Some(launch_timing::LaunchTrigger::SignInAfterGame));
    app.worker_event(WorkerEvent::Progress(key.clone(), "Signing in to PlayOnline…".into()));
    assert_eq!(app.accounts[0].status_text(), "Signing in to PlayOnline… · Signing back in after the game closed.");
    app.launches = None;
    let clicked = Instant::now();
    app.start_launch(vec![key.clone()], false, clicked, None);
    assert_eq!(app.accounts[0].queued_play, Some(clicked), "a queued Play, not a second sign-in");
    app.worker_event(WorkerEvent::Progress(key, "Signing in to PlayOnline…".into()));
    assert!(app.accounts[0].status_text().ends_with("FFXI will start when sign-in finishes."));
    assert_eq!(launch_timing::LaunchTrigger::SignInAfterGame.line(), "launch trigger: sign-in after game");
    // Two accounts start together, as the startup sign-ins do (their tool turns stagger them).
    let (mut app, first) = resign_app("resign-two");
    let second = app.accounts[1].key.clone();
    app.settings.account_no_otp.insert(second.clone());
    game_ends(&mut app, 0, signed_out());
    game_ends(&mut app, 1, signed_out());
    let keys = app.take_resigns(Instant::now() + RESIGN_DELAY);
    app.start_resigns(keys, launch_timing::LaunchTrigger::SignInAfterGame);
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![first, second]);
}

/// No sign-in after a game for an OTP account (its code is spent), with the setting off or
/// turned off meanwhile, after an explicit sign-out or an account edit, after a worker that
/// vanished (its host may live on), or once the user started something during the delay.
#[test]
fn no_sign_in_after_game_for_otp_setting_off_sign_out_edit_or_a_users_launch() {
    let (mut app, key) = resign_app("resign-none");
    let pending = |app: &App| app.accounts[0].resign.is_some();
    app.accounts[0].otp_spent = true;
    game_ends(&mut app, 0, signed_out());
    assert!(!pending(&app));
    app.accounts[0].otp_spent = false;
    app.accounts[0].otp = "123456".into();
    game_ends(&mut app, 0, signed_out());
    assert!(!pending(&app), "a typed code is for the user's Play");
    app.accounts[0].otp.clear();
    game_ends(&mut app, 0, launch_log::ExitReport::worker_ended());
    assert!(!pending(&app));
    let _ = app.update(Message::AutoSignIn(false));
    game_ends(&mut app, 0, signed_out());
    assert!(!pending(&app), "setting off");
    let _ = app.update(Message::AutoSignIn(true));
    // Turned off during the delay.
    game_ends(&mut app, 0, signed_out());
    assert!(pending(&app));
    let _ = app.update(Message::AutoSignIn(false));
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty());
    assert!(!pending(&app) && !app.accounts[0].status.contains("Signing back in"), "{}", app.accounts[0].status);
    let _ = app.update(Message::AutoSignIn(true));
    // The account learned to need a code during the delay.
    game_ends(&mut app, 0, signed_out());
    assert!(pending(&app));
    app.settings.note_sign_in(&key, true);
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty() && !pending(&app));
    app.settings.note_sign_in(&key, false);
    // The user's own Play during the delay wins.
    game_ends(&mut app, 0, signed_out());
    app.accounts[0].launch_generation += 1;
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty() && !pending(&app));
    // An explicit sign-out while the session runs.
    app.friends.account = Some(key.clone());
    let alive = Arc::new(AtomicBool::new(true));
    let row = &mut app.accounts[0];
    (row.pid, row.alive, row.game_running) = (Some(77), Some(alive.clone()), Some(Arc::new(AtomicBool::new(true))));
    let _ = app.update(Message::Tick);
    let _ = app.update(Message::FriendsSignOut);
    alive.store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(!pending(&app) && !app.accounts[0].resign_declined);
    // An edit while pending.
    game_ends(&mut app, 0, signed_out());
    assert!(pending(&app));
    app.accounts[0].decline_resign();
    assert!(!pending(&app) && !app.accounts[0].resign_declined, "no session to decline");
    // An edit saved while the game runs.
    let alive = Arc::new(AtomicBool::new(true));
    let row = &mut app.accounts[0];
    (row.pid, row.alive, row.game_running) = (Some(77), Some(alive.clone()), Some(Arc::new(AtomicBool::new(true))));
    let _ = app.update(Message::Tick);
    let _ = app.update(Message::EditAccount(key.clone()));
    let saved = app.accounts.iter().map(|r| r.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(saved)));
    assert!(app.accounts[0].resign_declined && app.settings.presigns(&key), "an edit of the same login keeps what was learned");
    alive.store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(!pending(&app));
    assert!(app.launches.as_ref().unwrap().is_empty());
    // A session that never ran its game (an idle sign-out) is no game end.
    let row = &mut app.accounts[0];
    let alive = Arc::new(AtomicBool::new(true));
    (row.pid, row.alive, row.game_running) = (Some(77), Some(alive.clone()), Some(Arc::new(AtomicBool::new(false))));
    let _ = app.update(Message::Tick);
    alive.store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(!pending(&app));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// A crash or error first has the probe check the network (a connection loss waits for it
/// to return); a sign-in after a game that fails on the network retries once when it is
/// back, and never again; any other failure is not retried; a probe that gives up drops it.
#[test]
fn an_unclean_game_end_waits_for_the_network_and_a_failed_sign_in_after_game_never_loops() {
    let (mut app, key) = resign_app("resign-network");
    let network = "Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).";
    let error = launch_log::classify_loader_end(&launch_log::LoaderEnd::Error("PlayOnline session ended (POL result -1025). Sign in again to reconnect.".into()), Duration::from_secs(900));
    game_ends(&mut app, 0, error.clone());
    assert_eq!(app.accounts[0].network_wait, Some(NetworkWait::AfterGame));
    assert!(app.accounts[0].resign.unwrap().network);
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY * 2).is_empty(), "not before the network");
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert!(!app.accounts[0].resign.unwrap().network);
    assert!(app.launches.as_ref().unwrap().is_empty());
    assert!(app.take_resigns(Instant::now()).is_empty(), "the delay still applies");
    let keys = app.take_resigns(Instant::now() + RESIGN_DELAY);
    app.start_resigns(keys, launch_timing::LaunchTrigger::SignInAfterGame);
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
    let fail = |app: &mut App, trigger, error: &str| {
        let row = &mut app.accounts[0];
        (row.busy, row.preparing_session, row.launch_trigger) = (true, true, Some(trigger));
        app.worker_event(WorkerEvent::Complete(app.accounts[0].key.clone(), Err(error.to_owned())));
    };
    fail(&mut app, launch_timing::LaunchTrigger::SignInAfterGame, "Sign-in failed");
    assert!(app.accounts[0].network_wait.is_none(), "not a network failure");
    fail(&mut app, launch_timing::LaunchTrigger::SignInAfterGame, network);
    assert_eq!(app.accounts[0].network_wait, Some(NetworkWait::AfterGameRetry));
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    let launches = app.launches.as_ref().unwrap();
    assert_eq!((launches.len(), launches[1].1, launches[1].3), (2, true, Some(launch_timing::LaunchTrigger::SignInAfterGameRetry)));
    fail(&mut app, launch_timing::LaunchTrigger::SignInAfterGameRetry, network);
    assert!(app.accounts[0].network_wait.is_none() && app.accounts[0].resign.is_none(), "no loop");
    assert_eq!(app.launches.as_ref().unwrap().len(), 2);
    // The network never came back.
    game_ends(&mut app, 0, error);
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), false).for_launch(&key, generation));
    assert!(app.accounts[0].resign.is_none() && app.accounts[0].network_wait.is_none());
    assert!(app.accounts[0].status_text().ends_with("The network did not come back. Sign in again once it is."), "{}", app.accounts[0].status_text());
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty());
    assert_eq!(app.launches.as_ref().unwrap().len(), 2);
}

/// Accounts that are not pre-signed (not yet known to sign in without a code, or OTP ones)
/// keep round 2: nothing signs in after their game, and their key is parked again once the
/// session has ended (07d §1.5 "Re-park"). A pre-signed account is not parked while its
/// sign-in after the game is coming.
#[test]
fn accounts_without_the_setting_still_re_park_after_their_game() {
    let (mut app, presigned) = resign_app("resign-park");
    let parked = app.accounts[1].key.clone();
    game_ends(&mut app, 1, signed_out());
    assert!(app.accounts[1].resign.is_none() && app.accounts[1].status == "Game closed normally");
    assert!(app.park_keys().iter().any(|(row, ..)| *row == parked));
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_some());
    assert!(!app.park_keys().iter().any(|(row, ..)| *row == presigned), "its sign-in after the game is coming");
    assert!(app.launches.as_ref().unwrap().is_empty());
}

/// Review finding (2026-10-13): a known code-less account that Crystal will not sign in by
/// itself now parks like any other, so its next Play starts warm: after a sign-in after its
/// game that a maintenance refused, a sign-out (`decline_resign`), or a startup sign-in that
/// failed. While a pre-sign-in is coming (startup sign-ins still to be taken, one waiting for
/// a closing session, a sign-in after its game, either's network retry) it does not.
#[test]
fn a_pre_signed_account_with_no_sign_in_coming_parks() {
    let (mut app, key) = resign_app("presign-park");
    let parks = |app: &App| app.park_keys().iter().any(|(row, ..)| *row == key);
    assert!(app.presigned(&key) && parks(&app), "idle, signed out, nothing coming");
    app.startup_signin_pending = true;
    assert!(!parks(&app), "before the startup sign-ins are taken");
    app.startup_signin_pending = false;
    app.startup_waiting.push((key.clone(), Instant::now()));
    assert!(!parks(&app), "a startup sign-in waits for a closing session");
    app.startup_waiting.clear();
    for wait in [NetworkWait::StartupRetry, NetworkWait::AfterGame, NetworkWait::AfterGameRetry] {
        app.accounts[0].network_wait = Some(wait);
        assert!(!parks(&app), "{wait:?}");
    }
    app.accounts[0].network_wait = None;
    // A sign-in after its game is pending, then dropped: a maintenance refused it.
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_some() && !parks(&app));
    app.accounts[0].drop_resign();
    assert!(parks(&app), "nothing is coming now: its next Play starts warm");
    // A code on the row (the user's, for their own Play): Crystal does not sign it in either.
    app.accounts[0].otp = "123456".into();
    app.startup_signin_pending = true;
    assert!(parks(&app));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

fn session_lost() -> launch_log::ExitReport {
    launch_log::classify_loader_end(&launch_log::LoaderEnd::Error("PlayOnline session ended (POL result -1025). Sign in again to reconnect.".into()), Duration::from_secs(900))
}

/// Review finding: a connection loss longer than STARTUP_WAIT_LIMIT still signs back in when
/// the network returns (the probe waits up to NETWORK_PROBE_LIMIT); only a probe that never
/// reports is given up, after that limit.
#[test]
fn a_long_connection_loss_still_signs_back_in_when_the_network_returns() {
    let (mut app, key) = resign_app("resign-long-outage");
    game_ends(&mut app, 0, session_lost());
    let ended = app.accounts[0].resign.unwrap().ended;
    assert!(app.take_resigns(ended + STARTUP_WAIT_LIMIT + Duration::from_secs(1)).is_empty());
    assert!(app.accounts[0].resign.is_some() && app.accounts[0].network_wait == Some(NetworkWait::AfterGame), "still waiting for the probe");
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    let now = Instant::now();
    assert!(app.take_resigns(now).is_empty(), "the delay still applies");
    assert_eq!(app.take_resigns(now + RESIGN_DELAY), vec![key.clone()]);
    // A probe that never reports.
    game_ends(&mut app, 0, session_lost());
    let ended = app.accounts[0].resign.unwrap().ended;
    assert!(app.take_resigns(ended + session_worker::NETWORK_PROBE_LIMIT).is_empty());
    assert!(app.accounts[0].resign.is_some());
    assert!(app.take_resigns(ended + session_worker::NETWORK_PROBE_LIMIT + STARTUP_WAIT_LIMIT).is_empty());
    assert!(app.accounts[0].resign.is_none() && app.accounts[0].network_wait.is_none());
}

/// Review finding: Crystal opened during the game queues its startup sign-in for the end of
/// that session; the game's close makes it the delayed "sign-in after game", never a startup
/// sign-in one Tick after the sign-out.
#[test]
fn a_startup_sign_in_waiting_on_the_game_becomes_the_delayed_sign_in_after_it() {
    let (mut app, key) = resign_app("resign-waiting");
    app.startup_waiting.push((key.clone(), Instant::now()));
    game_ends(&mut app, 0, signed_out());
    assert!(app.startup_waiting.is_empty() && app.accounts[0].resign.is_some());
    let _ = app.update(Message::Tick);
    assert!(app.launches.as_ref().unwrap().is_empty(), "no sign-in right after the sign-out");
    let keys = app.take_resigns(Instant::now() + RESIGN_DELAY);
    app.start_resigns(keys, launch_timing::LaunchTrigger::SignInAfterGame);
    let launches = app.launches.as_ref().unwrap();
    assert_eq!((&launches[0].0, launches[0].3), (&vec![key], Some(launch_timing::LaunchTrigger::SignInAfterGame)));
}

/// Review finding: during a known maintenance or outage nothing signs back in by itself:
/// not after the game, not pending, not as the network retry.
#[test]
fn no_sign_in_after_game_during_maintenance_or_an_outage() {
    let outage = |app: &mut App, on: bool| {
        let snapshot = app.services.as_mut().unwrap();
        snapshot.availability = if on { services::Availability::Outage } else { services::Availability::Unknown };
        snapshot.checked_at = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
        snapshot.notices.clear();
    };
    let (mut app, key) = resign_app("resign-outage");
    outage(&mut app, true);
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_none());
    outage(&mut app, false);
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_some());
    outage(&mut app, true);
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty() && app.accounts[0].resign.is_none());
    app.accounts[0].network_wait = Some(NetworkWait::AfterGameRetry);
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert!(app.launches.as_ref().unwrap().is_empty());
}

/// 2026-10-13 05:08:17 UTC: the sign-in after the game ran during All Worlds Maintenance
/// because the status page still showed the worlds up. The window refuses it now, and the
/// row says why; an early end (all up 15+ minutes into the window) allows it again.
#[test]
fn no_sign_in_after_game_during_an_official_maintenance_window() {
    let (mut app, key) = resign_app("resign-window");
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(8 * 60)));
    assert!(!app.resign_allowed(&key));
    game_ends(&mut app, 0, signed_out());
    let row = &app.accounts[0];
    assert!(row.resign.is_none());
    assert!(row.status_text().contains("No sign-in after the game while the worlds are down.") && row.status_text().contains("All Worlds Maintenance (Oct. 12)"), "{}", row.status_text());
    // Pending when the window starts: dropped with the note.
    status_read(&mut app, services::Availability::Operational, None);
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_some());
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(60)));
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty() && app.accounts[0].resign.is_none());
    assert!(app.accounts[0].status_text().contains("No sign-in after the game while the worlds are down."), "{}", app.accounts[0].status_text());
    assert!(!app.accounts[0].status.contains("Signing back in"), "{}", app.accounts[0].status);
    // The network retry is refused the same way.
    app.accounts[0].network_wait = Some(NetworkWait::AfterGameRetry);
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    // Ended early: the page shows every world up 20 minutes in.
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(20 * 60)));
    assert!(app.resign_allowed(&key));
    // The maintenance icon past the window's end still refuses.
    status_read(&mut app, services::Availability::Maintenance, None);
    assert!(!app.resign_allowed(&key));
    assert!(app.launches.as_ref().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Review finding: an account edit also cancels the pending network retry of a failed
/// sign-in after the game, and an edit whose settings save failed still declines.
#[test]
fn an_edit_cancels_a_network_retry_and_a_failed_settings_save_still_declines() {
    let network = "Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).";
    let (mut app, key) = resign_app("resign-retry-edit");
    let row = &mut app.accounts[0];
    (row.busy, row.preparing_session, row.launch_trigger) = (true, true, Some(launch_timing::LaunchTrigger::SignInAfterGame));
    app.worker_event(WorkerEvent::Complete(key.clone(), Err(network.into())));
    assert_eq!(app.accounts[0].network_wait, Some(NetworkWait::AfterGameRetry));
    let _ = app.update(Message::EditAccount(key.clone()));
    let saved: Vec<Account> = app.accounts.iter().map(|r| r.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(saved.clone())));
    assert!(app.accounts[0].network_wait.is_none());
    assert!(!app.accounts[0].status.contains("Crystal will try again"), "{}", app.accounts[0].status);
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert!(app.launches.as_ref().unwrap().is_empty());
    // The settings save fails (a file where its folder belongs).
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_some());
    std::fs::create_dir_all(&app.config.experiment).unwrap();
    let _ = std::fs::remove_dir_all(app.config.experiment.join("iced-launcher"));
    std::fs::write(app.config.experiment.join("iced-launcher"), b"").unwrap();
    let _ = app.update(Message::EditAccount(key.clone()));
    let _ = app.update(Message::AccountsSaved(Ok(saved)));
    assert!(app.status.starts_with("Accounts saved, but launcher settings were not saved"), "{}", app.status);
    assert!(app.accounts[0].resign.is_none());
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty());
    assert!(app.launches.as_ref().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Review finding: an edit while pre-signed and idle declines nothing about a game started
/// from that session afterwards (a pre-signed Play): its end still signs back in.
#[test]
fn an_edit_before_the_game_still_signs_back_in_after_it() {
    let (mut app, key) = resign_app("resign-edit-idle");
    let (alive, game) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
    let row = &mut app.accounts[0];
    (row.pid, row.alive, row.game_running) = (Some(77), Some(alive.clone()), Some(game.clone()));
    let _ = app.update(Message::Tick);
    let _ = app.update(Message::EditAccount(key.clone()));
    let saved = app.accounts.iter().map(|r| r.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(saved)));
    assert!(app.accounts[0].resign_declined);
    game.store(true, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].game_seen && !app.accounts[0].resign_declined);
    alive.store(false, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].resign.is_some());
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Review finding: "fully ended" includes the host process. A session whose worker closed
/// while its loader still runs (a pipe failure) waits for the loader to exit.
#[test]
fn a_sign_in_after_game_waits_for_the_old_host_process_to_exit() {
    let (mut app, key) = resign_app("resign-host");
    RUNNING_HOSTS.with(|hosts| hosts.borrow_mut().push(77));
    game_ends(&mut app, 0, signed_out());
    let resign = app.accounts[0].resign.unwrap();
    assert_eq!(resign.host.map(|host| host.pid), Some(77));
    assert!(resign.free_since.is_none(), "host still running");
    assert!(app.take_resigns(Instant::now() + RESIGN_DELAY * 2).is_empty());
    RUNNING_HOSTS.with(|hosts| hosts.borrow_mut().clear());
    let now = Instant::now();
    assert!(app.take_resigns(now).is_empty());
    assert_eq!(app.take_resigns(now + RESIGN_DELAY), vec![key]);
}

#[test]
fn queued_play_waits_for_vault_and_file_operations_then_uses_signed_in_host() {
    let mut app = app();
    app.preview = false;
    // An explicit missing install keeps this test entirely offline and
    // exercises the normal launch validation before any start request.
    let missing = std::env::temp_dir().join(format!("crystal-no-queued-game-{}", std::process::id()));
    assert!(!missing.exists());
    app.settings.install_selected = true;
    app.settings.install_path = missing.to_string_lossy().into_owned();
    app.settings.account_installs.clear();
    app.vault_saving = true;
    let row = &mut app.accounts[0];
    row.busy = true;
    row.preparing_session = true;
    let clicked = Instant::now() - Duration::from_secs(2);
    row.queued_play = Some(clicked);
    let key = row.key.clone();
    app.worker_event(WorkerEvent::Complete(key, Ok(LaunchResult {
        pid: 123, detail: "fixture".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: Arc::new(AtomicBool::new(false)), log: PathBuf::new(),
        outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })));
    assert_eq!(app.accounts[0].queued_play, Some(clicked));
    assert!(!app.accounts[0].can_play());
    app.vault_saving = false;
    app.vault_ready = false;
    app.start_queued_plays();
    assert_eq!(app.accounts[0].queued_play, Some(clicked));
    app.vault_ready = true;
    app.maintenance = Some(MaintenanceKind::Repair);
    app.start_queued_plays();
    assert_eq!(app.accounts[0].queued_play, Some(clicked));
    app.maintenance = None;
    app.start_queued_plays();
    let row = &app.accounts[0];
    assert_eq!(row.pid, Some(123), "keep the original signed-in host");
    assert!(row.queued_play.is_none());
    assert!(row.problem && row.status.contains("FFXi.dll"), "normal launch validation still runs");
    assert!(app.receiver.try_recv().is_err(), "never start a second sign-in worker");
}

/// A Play queued behind a pre-sign-in is re-checked when this window releases it: an outage
/// or maintenance begun since the click makes it wait for FFXI ("Wait and play") instead of
/// staying queued on the sign-in.
#[test]
fn a_queued_play_waits_at_release_when_downtime_began_since_its_click() {
    for (page, window) in [(services::Availability::Outage, None), (services::Availability::Operational, Some(Duration::from_secs(60)))] {
        let mut app = app();
        app.preview = false;
        let clicked = Instant::now() - Duration::from_secs(2);
        let row = &mut app.accounts[0];
        row.pid = Some(123);
        row.alive = Some(Arc::new(AtomicBool::new(true)));
        row.game_running = Some(Arc::new(AtomicBool::new(false)));
        row.queued_play = Some(clicked);
        status_read(&mut app, page, window);
        assert_eq!(app.service_availability(), if window.is_some() { services::Availability::Maintenance } else { page });
        app.start_queued_plays();
        let row = &app.accounts[0];
        assert!(row.queued_play.is_none(), "{page:?}");
        assert!(!row.busy && !row.batch_pending && row.pid == Some(123));
        assert_eq!(row.status, app.wait_headline());
        assert!(app.is_waiting(&row.key) && app.status == row.status);
        assert!(app.receiver.try_recv().is_err());
        app.start_queued_plays();
        assert!(app.accounts[0].queued_play.is_none(), "not re-queued");
        assert_eq!(app.waiting_plays.len(), 1);
    }
}

/// A fresh status read whose page shows `page`; with `window`, an official all-worlds
/// maintenance window that began that long ago covers now.
fn status_read(app: &mut App, page: services::Availability, window: Option<Duration>) {
    let now = chrono::Utc::now();
    let snapshot = app.services.as_mut().unwrap();
    snapshot.availability = page;
    snapshot.availability_note = "Official status page reports all 16 production worlds up.".into();
    snapshot.notices.clear();
    snapshot.windows = window.into_iter().map(|since| services::MaintenanceWindow {
        title: "All Worlds Maintenance (Oct. 12)".into(),
        start: now - chrono::Duration::from_std(since).unwrap(),
        end: now + chrono::Duration::minutes(60),
    }).collect();
    snapshot.checked_at = now.format("%Y-%m-%d %H:%M:%S UTC").to_string();
}

/// 2026-10-13 05:07 UTC: Play clicked seconds after Crystal opened, during All Worlds
/// Maintenance, while the status page still showed the worlds up. The Play is held for the
/// first status read, which finds maintenance: it then waits for FFXI ("Wait and play"); a
/// sign-in only is never held.
#[test]
fn a_play_before_the_first_status_read_is_held_then_waits_during_maintenance() {
    let mut app = app();
    app.preview = false;
    app.status_read_done = false;
    let key = app.accounts[0].key.clone();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert_eq!(app.held_plays.len(), 1);
    assert_eq!(app.accounts[0].status, "Checking whether FINAL FANTASY XI is up…");
    assert_eq!(app.status, "Checking whether FINAL FANTASY XI is up…");
    assert!(!app.accounts[0].busy && app.receiver.try_recv().is_err(), "nothing started");
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert_eq!(app.held_plays.len(), 1, "a second click is the same held Play");
    app.launches = Some(Vec::new());
    let other = app.accounts[1].key.clone();
    app.start_launch(vec![other.clone()], true, Instant::now(), None);
    assert_eq!(app.launches.as_ref().unwrap().len(), 1, "a sign-in only is not held");
    app.launches = None;
    // The read lands: the page still shows every world up, but the window covers now.
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    let snapshot = app.services.clone().unwrap();
    app.worker_event(WorkerEvent::Services(app.news_generation, i18n::current(), services::Update::Status(snapshot)));
    assert!(app.status_read_done && app.held_plays.is_empty());
    assert_eq!(app.service_availability(), services::Availability::Maintenance);
    assert!(app.service_note().contains("All Worlds Maintenance (Oct. 12)") && app.service_note().contains("still shows the worlds up"));
    let row = &app.accounts[0];
    assert!(!row.busy && row.pid.is_none() && row.queued_play.is_none());
    assert_eq!(row.status, "FFXI is down for maintenance. Starting as soon as it's back…");
    assert!(app.is_waiting(&key) && app.status == row.status);
    assert!(app.receiver.try_recv().is_err(), "no sign-in worker");
}

/// The held Play starts once the read finds the worlds up, on every path: a cold Play and
/// a Play queued behind a pre-sign-in in flight.
#[test]
fn a_held_play_starts_after_the_first_status_read_finds_the_worlds_up() {
    let mut app = app();
    app.preview = false;
    app.status_read_done = false;
    app.launches = Some(Vec::new());
    let key = app.accounts[0].key.clone();
    let clicked = Instant::now();
    app.start_launch(vec![key.clone()], false, clicked, None);
    assert!(app.launches.as_ref().unwrap().is_empty());
    status_read(&mut app, services::Availability::Operational, None);
    let snapshot = app.services.clone().unwrap();
    let _ = app.update(Message::ServiceStatusLoaded(app.news_generation, i18n::current(), Ok(snapshot)));
    assert_eq!(app.launches.as_ref().unwrap(), &vec![(vec![key.clone()], false, clicked, None)]);
    assert_ne!(app.accounts[0].status, "Checking whether FINAL FANTASY XI is up…");
    // Queued during a pre-sign-in: held, then queued on it with the original click.
    let mut app = self::app();
    app.preview = false;
    app.status_read_done = false;
    let row = &mut app.accounts[0];
    (row.busy, row.preparing_session) = (true, true);
    let clicked = Instant::now();
    app.start_launch(vec![key.clone()], false, clicked, None);
    assert!(app.accounts[0].queued_play.is_none() && app.held_plays.len() == 1);
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(20 * 60)));
    let snapshot = app.services.clone().unwrap();
    let _ = app.update(Message::ServiceStatusLoaded(app.news_generation, i18n::current(), Ok(snapshot)));
    assert_eq!(app.service_availability(), services::Availability::Operational, "ended early");
    assert_eq!(app.accounts[0].queued_play, Some(clicked));
}

/// A read that fails or is slow never blocks: the held Play proceeds after STATUS_HOLD_LIMIT,
/// and later Plays are not held again.
#[test]
fn a_held_play_proceeds_when_the_first_status_read_times_out() {
    let mut app = app();
    app.preview = false;
    app.status_read_done = false;
    app.launches = Some(Vec::new());
    let key = app.accounts[0].key.clone();
    let clicked = Instant::now();
    app.start_launch(vec![key.clone()], false, clicked, None);
    let since = app.held_plays[0].since;
    app.release_held_plays(since + STATUS_HOLD_LIMIT - Duration::from_millis(1));
    assert!(app.launches.as_ref().unwrap().is_empty() && app.held_plays.len() == 1);
    app.held_plays[0].since = Instant::now() - STATUS_HOLD_LIMIT;
    let _ = app.update(Message::Tick);
    assert!(app.held_plays.is_empty() && app.status_hold_spent && !app.status_read_done);
    assert_eq!(app.launches.as_ref().unwrap()[0], (vec![key.clone()], false, clicked, None));
    let second = app.accounts[1].key.clone();
    app.start_launch(vec![second.clone()], false, clicked, None);
    assert_eq!(app.launches.as_ref().unwrap()[1].0, vec![second], "not held again");
    // A failed read releases at once.
    let mut app = self::app();
    app.preview = false;
    app.status_read_done = false;
    app.launches = Some(Vec::new());
    app.start_launch(vec![key.clone()], false, clicked, None);
    let _ = app.update(Message::ServiceStatusLoaded(app.news_generation, i18n::current(), Err("timed out".into())));
    assert_eq!(app.launches.as_ref().unwrap().len(), 1);
}

/// Review findings: a held Play released into an account save, a deleted account, or a
/// paused launch was lost or left a blank row; a held Play let a launcher update restart.
#[test]
fn a_held_play_survives_saves_and_deletes_and_blocks_a_launcher_restart() {
    let mut app = app();
    app.preview = false;
    app.status_read_done = false;
    let (key, other) = (app.accounts[0].key.clone(), app.accounts[1].key.clone());
    let before = app.accounts[0].status.clone();
    app.status = "Ready.".into();
    assert!(app.can_restart_launcher());
    app.start_launch(vec![key.clone(), other.clone()], false, Instant::now(), None);
    assert!(!app.can_restart_launcher(), "a held Play is pending launch work");
    // The second account is deleted and a save runs while the read lands: still held.
    app.accounts.retain(|row| row.key != other);
    app.vault_saving = true;
    status_read(&mut app, services::Availability::Operational, None);
    app.note_status_read();
    assert_eq!(app.held_plays.len(), 1, "held through the save");
    app.vault_saving = false;
    app.launches = Some(Vec::new());
    let _ = app.update(Message::Tick);
    assert!(app.held_plays.is_empty());
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![key.clone()], "the deleted account is left out");
    assert_eq!(app.accounts[0].status, before, "the row says what it said before the hold");
    assert_eq!(app.status, "Ready.");
    // The hold text follows a language switch, and the release still recognises it.
    let mut app = self::app();
    app.preview = false;
    app.status_read_done = false;
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    let english = app.accounts[0].status.clone();
    i18n::set(i18n::Lang::Ja);
    app.retranslate(i18n::Lang::En);
    assert_ne!(app.accounts[0].status, english);
    assert_eq!(app.accounts[0].status, t("Checking whether FINAL FANTASY XI is up…"));
    assert_eq!(app.status, t("Checking whether FINAL FANTASY XI is up…"));
    app.launches = Some(Vec::new());
    app.note_status_read();
    assert_ne!(app.accounts[0].status, t("Checking whether FINAL FANTASY XI is up…"));
    i18n::set(i18n::Lang::En);
}

/// A read of the play flag, as `read_play_flag` delivers it, made `ago` before now.
fn flag_read(app: &mut App, ago: Duration, result: Result<play_flag::Flag, String>) {
    let at = chrono::Utc::now() - chrono::Duration::from_std(ago).unwrap();
    let _ = app.update(Message::PlayFlagLoaded(play_flag::Reading { at, result, patch: None }));
}

const SERVER_STATUS: &str = "The game is unavailable for play during maintenance.";

/// An app during All Worlds Maintenance (a posted window seven minutes in, the status page
/// still showing the worlds up), whose Plays are recorded instead of started.
fn maintenance_app() -> App {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    assert_eq!(app.main_action(), MainAction::Maintenance);
    app
}

/// Owner decision 2026-10-13, "Wait and play": the main button sets the selected accounts
/// waiting, a second click adds nothing, and they start together through the normal start
/// path once the play flag says 1, even inside the posted window.
#[test]
fn wait_and_play_waits_during_maintenance_and_starts_when_the_flag_says_ffxi_is_back() {
    let mut app = maintenance_app();
    for row in &mut app.accounts { row.selected = false; }
    app.accounts[0].selected = true;
    app.accounts[1].selected = true;
    let (first, second) = (app.accounts[0].key.clone(), app.accounts[1].key.clone());
    assert_eq!(app.wait_keys(), vec![first.clone(), second.clone()]);
    assert_eq!(app.update(Message::MainAction).units(), 0);
    assert!(app.is_waiting(&first) && app.is_waiting(&second) && app.waiting_plays.len() == 1);
    assert!(!app.show_details && app.wait_keys().is_empty());
    assert_eq!(app.accounts[0].status, "FFXI is down for maintenance. Starting as soon as it's back…");
    assert!(app.launches.as_ref().unwrap().is_empty() && app.receiver.try_recv().is_err(), "nothing signs in while waiting");
    // A second click (main button or the row's Launch) is the same wait; the button then
    // explains the downtime.
    let _ = app.update(Message::MainAction);
    app.start_launch(vec![first.clone()], false, Instant::now(), None);
    assert_eq!(app.waiting_plays.len(), 1);
    assert_eq!(app.waiting_plays[0].keys, vec![first.clone(), second.clone()]);
    assert!(app.show_details);
    // The first Tick reads the flag at once, then one read at a time.
    assert_eq!(app.update(Message::Tick).units(), 1, "an immediate read");
    assert_eq!(app.update(Message::Tick).units(), 0, "one in flight");
    // 0: still waiting, with the server's line under the headline.
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Down(Some(SERVER_STATUS.into()))));
    assert!(app.is_waiting(&first) && app.launches.as_ref().unwrap().is_empty());
    let detail = app.wait_detail();
    assert!(detail.starts_with("Last checked: ") && detail.ends_with(SERVER_STATUS), "{detail}");
    assert_eq!(app.update(Message::Tick).units(), 0, "the next read is a minute later");
    // 1 inside the posted window: FFXI is up, and both start together.
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(app.waiting_plays.is_empty());
    assert_eq!(app.main_action(), MainAction::Play);
    let launches = app.launches.as_ref().unwrap();
    assert_eq!(launches.len(), 1);
    assert_eq!((&launches[0].0, launches[0].1, launches[0].3), (&vec![first.clone(), second.clone()], false, None));
    // Nothing waits: no more reads.
    assert_eq!(app.update(Message::Tick).units(), 0);
}

/// The flag wins both ways while fresh; without a fresh read the official status decides, and
/// the row says why.
#[test]
fn a_waiting_play_follows_the_official_status_when_the_flag_is_stale_or_unreadable() {
    let mut app = maintenance_app();
    let key = app.accounts[0].key.clone();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    // 0 while the status page shows every world up 20 minutes in (Round 4: ended early).
    flag_read(&mut app, Duration::from_secs(30), Ok(play_flag::Flag::Down(None)));
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(20 * 60)));
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key), "a fresh 0 keeps it waiting");
    // A later read fails: the 0 read 30 s ago still decides.
    flag_read(&mut app, Duration::ZERO, Err("timed out".into()));
    assert!(app.is_waiting(&key) && app.launches.as_ref().unwrap().is_empty());
    // Once no read is fresh, the official status decides: maintenance still, and the row says
    // the flag could not be read.
    let mut app = maintenance_app();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    flag_read(&mut app, Duration::ZERO, Err("The play flag server answered HTTP 503.".into()));
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key));
    assert!(app.service_note().contains("All Worlds Maintenance (Oct. 12)"));
    assert!(app.wait_detail().contains("The play flag could not be read (The play flag server answered HTTP 503.), so this follows the official status."),
        "{}", app.wait_detail());
    // ... until the official status says up: the next read decides first, and when it fails
    // too, the official status alone starts the Play.
    status_read(&mut app, services::Availability::Operational, None);
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key), "between reads only a fresh 1 starts it");
    flag_read(&mut app, Duration::ZERO, Err("timed out".into()));
    assert!(!app.is_waiting(&key));
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![key.clone()]);
    // A stale 1 (over two minutes old) proves nothing.
    let mut app = maintenance_app();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    flag_read(&mut app, Duration::from_secs(150), Ok(play_flag::Flag::Up));
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key) && app.launches.as_ref().unwrap().is_empty());
}

/// Review findings: a wait is released only on evidence that FFXI is up (a fresh 1, or the
/// official status right after a read), never on Unknown; after the PC wakes the read due
/// then decides, not the stale status before it; and a reading that has just gone stale
/// waits for the read that follows it instead of the official status deciding between reads.
#[test]
fn a_wait_is_released_only_on_evidence_that_ffxi_is_up() {
    let mut app = maintenance_app();
    let key = app.accounts[0].key.clone();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert_eq!(app.update(Message::Tick).units(), 1);
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Down(None)));
    // The PC slept 40 minutes, past the posted end of a maintenance that was extended: the
    // status read and the flag reading are stale, and no window covers now.
    let long_ago = chrono::Utc::now() - chrono::Duration::minutes(40);
    let snapshot = app.services.as_mut().unwrap();
    snapshot.windows.clear();
    snapshot.checked_at = long_ago.format("%Y-%m-%d %H:%M:%S UTC").to_string();
    app.play_flag = play_flag::Known::default();
    app.play_flag.note(play_flag::Reading { at: long_ago, result: Ok(play_flag::Flag::Down(None)), patch: None });
    assert_eq!(app.service_availability(), services::Availability::Unknown);
    app.flag_schedule.tick(long_ago);
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key) && app.launches.as_ref().unwrap().is_empty(), "the wake's read decides");
    // That read fails: still nothing says FFXI is up.
    flag_read(&mut app, Duration::ZERO, Err("timed out".into()));
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key) && app.launches.as_ref().unwrap().is_empty(), "Unknown is not up");
    // 0 (the extended maintenance), then 1.
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Down(None)));
    assert!(app.is_waiting(&key));
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(!app.is_waiting(&key));
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![key.clone()]);
    // The page shows every world up 20 minutes into the window (Round 4: ended early) while
    // the flag said 0 just over two minutes ago, and the read after it failed: between reads
    // the official status alone starts nothing; the read that follows decides.
    let mut app = maintenance_app();
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(20 * 60)));
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Down(None)));
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&key));
    let now = chrono::Utc::now();
    app.play_flag = play_flag::Known::default();
    app.play_flag.note(play_flag::Reading { at: now - chrono::Duration::seconds(121), result: Ok(play_flag::Flag::Down(None)), patch: None });
    app.play_flag.note(play_flag::Reading { at: now - chrono::Duration::seconds(61), result: Err("timed out".into()), patch: None });
    assert_eq!(app.service_availability(), services::Availability::Operational);
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key), "not between reads");
    // That read fails too: the official status decides, as the fallback does.
    flag_read(&mut app, Duration::ZERO, Err("timed out".into()));
    assert!(!app.is_waiting(&key));
}

/// Review finding: the play flag is FFXI's maintenance switch, so a 1 never ends an outage
/// (every world down outside maintenance): the wait goes on until the status page shows the
/// worlds back, and no sign-in after a game follows meanwhile.
#[test]
fn an_outage_waits_for_the_worlds_even_when_the_play_flag_is_1() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    status_read(&mut app, services::Availability::Outage, None);
    let key = app.accounts[0].key.clone();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&key));
    assert_eq!(app.accounts[0].status, "FFXI's worlds are down. Starting as soon as they're back…");
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    let _ = app.update(Message::Tick);
    assert!(app.is_waiting(&key) && app.launches.as_ref().unwrap().is_empty());
    assert_eq!(app.service_availability(), services::Availability::Outage);
    assert_eq!(app.main_action(), MainAction::Offline);
    app.settings.account_no_otp.insert(key.clone());
    app.settings.sign_in_after_game = true;
    assert!(app.resign_eligible(&key) && !app.resign_allowed(&key), "no sign-in after a game while the worlds are down");
    // The worlds are back.
    status_read(&mut app, services::Availability::Operational, None);
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(!app.is_waiting(&key));
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![key]);
}

/// Review finding: a waiting Play whose startup sign-in is refused (a verdict on the account)
/// stops waiting, and its row shows the refusal: a release would only sign in again into the
/// same verdict. A network failure, which retries by itself, keeps the wait.
#[test]
fn a_waiting_play_whose_sign_in_is_refused_stops_waiting_and_says_why() {
    for (failure, waits) in [("Sign-in failed", false),
        ("Game loader error during startup: Native PlayOnline login failed (state 39, POL result -19). (Windower via crystal-loader).", true)] {
        let mut app = maintenance_app();
        let key = app.accounts[0].key.clone();
        let row = &mut app.accounts[0];
        (row.busy, row.preparing_session, row.launch_trigger) = (true, true, Some(launch_timing::LaunchTrigger::StartupSignIn));
        app.start_launch(vec![key.clone()], false, Instant::now(), None);
        assert!(app.is_waiting(&key));
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(failure.to_owned())));
        assert_eq!(app.is_waiting(&key), waits, "{failure}");
        if !waits {
            assert!(app.accounts[0].problem && app.accounts[0].status == failure);
            flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
            assert!(app.launches.as_ref().unwrap().is_empty(), "no second login");
        }
    }
}

/// Review finding: starting the first released group can pause launches (an interrupted
/// update it finishes first); a later group then keeps waiting instead of being dropped.
#[test]
fn a_later_wait_group_keeps_waiting_when_an_earlier_one_pauses_launches() {
    let mut app = maintenance_app();
    // The real start path, which finishes the interrupted update (a temp folder) first.
    app.launches = None;
    app.viewer = None;
    let (first, second) = (app.accounts[0].key.clone(), app.accounts[1].key.clone());
    app.start_launch(vec![first.clone()], false, Instant::now(), None);
    app.start_launch(vec![second.clone()], false, Instant::now(), None);
    assert_eq!(app.waiting_plays.len(), 2);
    assert!(app.receiver.try_recv().is_err());
    let root = std::env::temp_dir().join(format!("crystal-wait-recover-{}", backend::unique_id().unwrap()));
    std::fs::create_dir_all(root.join(".xi-updater")).unwrap();
    std::fs::write(root.join(".xi-updater").join("transaction.json"), b"{}").unwrap();
    app.settings.install_path = root.to_string_lossy().into_owned();
    app.settings.install_selected = true;
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(app.recovering.is_some(), "the first group finishes the interrupted update first");
    assert!(!app.is_waiting(&first) && app.is_waiting(&second), "the second keeps waiting");
    // The recovery worker answers (it refuses the fixture journal); nothing signed in.
    let event = app.receiver.recv_timeout(Duration::from_secs(30)).ok();
    assert!(matches!(event, Some(WorkerEvent::InterruptedUpdate(_))));
    let _ = std::fs::remove_dir_all(root);
}

/// The flag is read only while a Play waits or FFXI is in maintenance (for `$patch_enable`,
/// patch_watch), once at the start, every minute, and again at once after the PC wakes.
#[test]
fn the_flag_is_read_only_while_waiting_or_in_maintenance_and_again_after_a_resume() {
    let mut app = maintenance_app();
    assert_eq!(app.update(Message::Tick).units(), 1, "maintenance: a read with nothing waiting");
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Down(None)));
    assert_eq!(app.update(Message::Tick).units(), 0);
    let key = app.accounts[0].key.clone();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert_eq!(app.update(Message::Tick).units(), 0, "a wait adds no read");
    assert_eq!(app.update(Message::Tick).units(), 0);
    // Review finding: more waits (other rows, Cancel then Play again) send no extra reads.
    let others: Vec<String> = app.accounts[1..].iter().map(|row| row.key.clone()).collect();
    for other in &others { app.start_launch(vec![other.clone()], false, Instant::now(), None); }
    let _ = app.update(Message::CancelWait(vec![others[0].clone()]));
    app.start_launch(vec![others[0].clone()], false, Instant::now(), None);
    assert_eq!(app.waiting_plays.len(), others.len() + 1);
    assert_eq!(app.update(Message::Tick).units(), 0);
    // The last Tick was 40 minutes ago by the wall clock: the PC slept.
    app.flag_schedule.tick(chrono::Utc::now() - chrono::Duration::minutes(40));
    assert_eq!(app.update(Message::Tick).units(), 1, "a read at resume");
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Down(None)));
    // Cancelled, and the maintenance over: no wait, no read, even at a resume.
    let _ = app.update(Message::CancelWait([vec![key.clone()], others].concat()));
    assert!(app.waiting_plays.is_empty());
    status_read(&mut app, services::Availability::Operational, None);
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(!app.patch_watch.active());
    app.flag_schedule.tick(chrono::Utc::now() - chrono::Duration::minutes(40));
    assert_eq!(app.update(Message::Tick).units(), 0);
}

/// Cancel drops the wait (and its row says what an idle row says); a waiting Play blocks a
/// launcher self-update restart, as a held Play does, since waits are not saved.
#[test]
fn cancel_ends_a_wait_and_a_wait_blocks_a_launcher_restart() {
    let mut app = maintenance_app();
    let (first, second) = (app.accounts[0].key.clone(), app.accounts[1].key.clone());
    assert!(app.can_restart_launcher());
    app.start_launch(vec![first.clone(), second.clone()], false, Instant::now(), None);
    assert!(!app.can_restart_launcher(), "a waiting Play is pending launch work");
    let _ = app.update(Message::CancelWait(vec![first.clone()]));
    assert!(!app.is_waiting(&first) && app.is_waiting(&second));
    assert_eq!(app.accounts[0].status, "Ready to launch");
    assert!(!app.can_restart_launcher());
    assert_eq!(app.status, "FFXI is down for maintenance. Starting as soon as it's back…", "the other still waits");
    let _ = app.update(Message::CancelWait(vec![second.clone()]));
    assert!(app.waiting_plays.is_empty() && app.can_restart_launcher());
    // Review finding: nothing waits, so the status line no longer promises a start; it says
    // why FFXI is closed.
    assert_eq!(app.status, app.service_note());
    assert!(app.status.contains("All Worlds Maintenance (Oct. 12)"), "{}", app.status);
    // A late Cancel for a row that no longer waits leaves what it says.
    app.accounts[1].status = "FFXI is back — enter your one-time password to start".into();
    let _ = app.update(Message::CancelWait(vec![second.clone()]));
    assert_eq!(app.accounts[1].status, "FFXI is back — enter your one-time password to start");
    // A deleted account no longer waits (nor reads the flag, nor holds a restart back).
    app.start_launch(vec![second.clone()], false, Instant::now(), None);
    app.accounts.retain(|row| row.key != second);
    let _ = app.update(Message::Tick);
    assert!(app.waiting_plays.is_empty() && app.can_restart_launcher());
    // Nothing starts when FFXI is back.
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    let _ = app.update(Message::Tick);
    assert!(app.launches.as_ref().unwrap().is_empty());
    // FFXI is up (the flag read just now): a Play starts at once.
    app.start_launch(vec![first.clone()], false, Instant::now(), None);
    assert!(app.launches.as_ref().unwrap().len() == 1 && app.waiting_plays.is_empty(), "FFXI is up: no wait");
    // The update a Play runs in its session blocks it too.
    app.play_updates.begin(PathBuf::from(r"C:\fixture\FINAL FANTASY XI"), &first, "30261006_0".into(), Instant::now(), Vec::new(), false);
    assert!(!app.can_restart_launcher());
}

/// A Play that goes ahead (FFXI up, the wait's own release held back by paused launches)
/// replaces that account's wait, so the wait cannot start it a second time.
#[test]
fn a_play_that_goes_ahead_replaces_its_accounts_wait() {
    let mut app = app();
    app.preview = false;
    let (key, other) = (app.accounts[0].key.clone(), app.accounts[1].key.clone());
    // Private-server rows: their Plays start no worker in tests (`start_private`).
    app.accounts[0].account.mode = AccountMode::Private;
    app.accounts[1].account.mode = AccountMode::Private;
    app.waiting_plays.push(WaitingPlay { keys: vec![key.clone(), other.clone()], otp: Vec::new(), armed: Instant::now(), codes: Vec::new() });
    assert!(!app.service_down());
    app.start_launch_now(vec![key.clone()], false, Instant::now(), None, false);
    assert!(!app.is_waiting(&key) && app.is_waiting(&other));
    assert!(app.receiver.try_recv().is_err());
    // A sign-in only replaces nothing.
    app.start_launch_now(vec![other.clone()], true, Instant::now(), None, false);
    assert!(app.is_waiting(&other));
}

/// A wait that began OTP_KEEP or more ago.
fn age_waits(app: &mut App) {
    for wait in &mut app.waiting_plays { wait.armed = Instant::now() - OTP_KEEP - Duration::from_secs(1); }
}

/// Never a sign-in with a stale one-time code: a code typed before the wait goes off the row
/// and is dropped once stale, and an account that signs in with one asks for a new code when
/// FFXI is back. A signed-in session needs no code and starts.
#[test]
fn an_otp_account_asks_for_a_new_code_when_ffxi_is_back() {
    let mut app = maintenance_app();
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    // A code typed for this Play; a code taken by the last sign-in.
    app.accounts[0].otp = "123456".into();
    app.accounts[1].otp_spent = true;
    // Signed in at startup with a code; its session is idle at the main menu.
    let row = &mut app.accounts[2];
    (row.pid, row.otp_spent) = (Some(4242), true);
    row.alive = Some(Arc::new(AtomicBool::new(true)));
    row.game_running = Some(Arc::new(AtomicBool::new(false)));
    app.start_launch(keys.clone(), false, Instant::now(), None);
    assert!(keys.iter().all(|key| app.is_waiting(key)));
    assert!(app.accounts[0].otp.is_empty(), "the typed code goes with the wait");
    assert_eq!(app.waiting_plays[0].otp, vec![keys[0].clone()]);
    assert_eq!(app.waiting_plays[0].codes.len(), 1);
    // Stale: the Tick drops it.
    age_waits(&mut app);
    let _ = app.update(Message::Tick);
    assert!(app.waiting_plays[0].codes.is_empty());
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    let launches = app.launches.as_ref().unwrap();
    assert_eq!(launches.len(), 1);
    assert_eq!((&launches[0].0, launches[0].3), (&vec![keys[2].clone()], None), "the signed-in session starts");
    for row in &app.accounts[..2] {
        assert_eq!(row.status, "FFXI is back — enter your one-time password to start");
        assert!(row.problem && row.otp.is_empty() && !app.is_waiting(&row.key));
    }
    assert!(app.waiting_plays.is_empty());
    // A code typed during a long wait is stale by the release too.
    let mut app = maintenance_app();
    app.start_launch(vec![keys[0].clone()], false, Instant::now(), None);
    app.accounts[0].otp = "654321".into();
    age_waits(&mut app);
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(app.launches.as_ref().unwrap().is_empty() && app.accounts[0].otp.is_empty());
    assert_eq!(app.accounts[0].status, "FFXI is back — enter your one-time password to start");
    // Review finding: an account known (from an earlier window) to sign in with a code, with
    // none typed and no sign-in this run, asks for one instead of signing in without it.
    let mut app = maintenance_app();
    app.settings.account_otp.insert(keys[1].clone());
    app.start_launch(vec![keys[1].clone()], false, Instant::now(), None);
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert!(app.launches.as_ref().unwrap().is_empty());
    assert_eq!(app.accounts[1].status, "FFXI is back — enter your one-time password to start");
}

/// Review finding: the code typed in answer to "FFXI is back" while the official status still
/// lags (the flag reading behind the release has gone stale) waits for the immediate read,
/// which says 1: that code is seconds old and starts the Play, not a second prompt.
#[test]
fn a_code_typed_for_a_play_that_waits_is_used_when_ffxi_is_back_at_once() {
    let mut app = maintenance_app();
    let key = app.accounts[0].key.clone();
    app.accounts[0].otp = "123456".into();
    app.accounts[0].otp_spent = true;
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    assert!(app.is_waiting(&key) && app.accounts[0].otp.is_empty());
    assert_eq!(app.update(Message::Tick).units(), 1, "the immediate read");
    flag_read(&mut app, Duration::ZERO, Ok(play_flag::Flag::Up));
    assert_eq!(app.launches.as_ref().unwrap()[0].0, vec![key.clone()]);
    assert_eq!(app.accounts[0].otp, "123456", "the code goes to the sign-in");
    assert!(!app.accounts[0].problem && app.waiting_plays.is_empty());
    // A Cancel drops the kept code with the wait.
    let mut app = maintenance_app();
    app.accounts[0].otp = "654321".into();
    app.start_launch(vec![key.clone()], false, Instant::now(), None);
    let _ = app.update(Message::CancelWait(vec![key.clone()]));
    assert!(app.waiting_plays.is_empty() && app.accounts[0].otp.is_empty());
}

/// Review finding: whether an account signs in with a one-time code is learned from its
/// sessions and kept in Settings (never the code), so a later window knows it.
#[test]
fn an_account_whose_sign_in_took_a_code_is_remembered_as_one() {
    let mut app = app();
    app.preview = false;
    let root = std::env::temp_dir().join(format!("crystal-otp-account-{}", backend::unique_id().unwrap()));
    app.config.experiment = root.clone();
    let key = app.accounts[0].key.clone();
    // `trigger` None: a reconnect to an earlier window's session.
    let completed = |app: &mut App, spent: bool, trigger: Option<launch_timing::LaunchTrigger>| {
        let row = &mut app.accounts[0];
        (row.busy, row.otp_spent, row.launch_trigger) = (true, spent, trigger);
        let key = row.key.clone();
        app.worker_event(WorkerEvent::Complete(key, Ok(LaunchResult {
            pid: 41, detail: "signed in".into(), alive: Arc::new(AtomicBool::new(true)),
            game_running: Arc::new(AtomicBool::new(false)), log: PathBuf::new(),
            outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
        })));
    };
    let signed_in = |app: &mut App, spent: bool| completed(app, spent, Some(launch_timing::LaunchTrigger::Connect));
    signed_in(&mut app, true);
    assert!(app.settings.account_otp.contains(&key) && !app.settings.presigns(&key));
    assert!(load_settings(&app.config).unwrap().account_otp.contains(&key), "saved");
    signed_in(&mut app, false);
    assert!(app.settings.account_otp.is_empty() && load_settings(&app.config).unwrap().account_otp.is_empty());
    // 2026-10-13: and that it signs in without one, which makes it pre-signed from now on.
    assert!(app.settings.account_no_otp.contains(&key) && load_settings(&app.config).unwrap().presigns(&key));
    signed_in(&mut app, true);
    assert!(app.settings.account_no_otp.is_empty() && !load_settings(&app.config).unwrap().presigns(&key));
    // A reconnect never teaches "no code" (an older worker's record defaults to none); a code
    // its record names is still learned.
    app.settings.account_otp.clear();
    completed(&mut app, false, None);
    assert!(!app.settings.account_otp.contains(&key) && !app.settings.account_no_otp.contains(&key));
    completed(&mut app, true, None);
    assert!(app.settings.account_otp.contains(&key));
    // Review finding: nor does a Play that reuses such a session (Launch all over idle
    // sessions, or a Play queued on one still preparing) without signing in again: its
    // record (a 1.0.8 worker's) never said whether that sign-in took a code.
    app.settings.account_otp.clear();
    for trigger in [launch_timing::LaunchTrigger::BatchPlay, launch_timing::LaunchTrigger::QueuedPlay] {
        app.accounts[0].otp_unknown = true;
        completed(&mut app, false, Some(trigger));
        assert!(!app.settings.account_otp.contains(&key) && !app.settings.account_no_otp.contains(&key), "{trigger:?}");
    }
    // A sign-in this window starts is known again (start_launch and start_together clear it).
    app.accounts[0].otp_unknown = false;
    completed(&mut app, false, Some(launch_timing::LaunchTrigger::BatchPlay));
    assert!(app.settings.account_no_otp.contains(&key));
    let _ = std::fs::remove_dir_all(root);
}

/// Owner decision 2026-10-13: Crystal signs in by itself exactly the retail accounts it knows
/// sign in without a one-time code (their latest successful sign-in took none), while the
/// one switch is on. OTP accounts, accounts never seen signing in, and rows with a code
/// typed or spent are not; private-server accounts never.
#[test]
fn presign_eligibility_table() {
    let mut app = app();
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    let (known, otp, unknown) = (&keys[0], &keys[1], &keys[2]);
    app.settings.account_no_otp.insert(known.clone());
    app.settings.account_otp.insert(otp.clone());
    assert!(app.settings.auto_sign_in, "on by default");
    assert_eq!(keys.iter().map(|key| app.presigned(key)).collect::<Vec<_>>(), [true, false, false]);
    // Migrated from a file before 2026-10-13: "Sign in when Crystal opens" was on.
    let mut migrated: Settings = serde_json::from_str(&format!(r#"{{"install_path":"","account_startup_signin":["{unknown}"]}}"#)).unwrap();
    migrated.migrate_startup_signin();
    app.settings.account_no_otp.extend(migrated.account_no_otp);
    assert!(app.presigned(unknown));
    // A code typed on the row (for the user's own Play), or spent by its last sign-in.
    app.accounts[0].otp = "123456".into();
    assert!(!app.presigned(known), "never a pre-sign-in with a typed code");
    app.accounts[0].otp.clear();
    app.accounts[0].otp_spent = true;
    assert!(!app.presigned(known));
    app.accounts[0].otp_spent = false;
    // Private-server accounts never.
    app.accounts[0].account.mode = AccountMode::Private;
    assert!(!app.presigned(known));
    app.accounts[0].account.mode = AccountMode::Retail;
    // Never both: the latest sign-in wins.
    assert!(app.settings.note_sign_in(known, true));
    assert!(!app.presigned(known) && app.settings.account_otp.contains(known) && !app.settings.account_no_otp.contains(known));
    assert!(!app.settings.note_sign_in(known, true), "nothing changed");
    assert!(app.settings.note_sign_in(known, false));
    assert!(app.presigned(known) && !app.settings.account_otp.contains(known));
    // The switch off: none.
    app.settings.auto_sign_in = false;
    assert!(keys.iter().all(|key| !app.presigned(key)));
}

/// Owner decision 2026-10-13: an account's first Play teaches Crystal. One that signed in
/// without a code is eligible for startup sign-in; one that took
/// a code is not, and neither is one whose sign-in failed.
#[test]
fn a_first_play_teaches_whether_the_account_is_pre_signed() {
    let mut app = app();
    app.preview = false;
    app.launches = Some(Vec::new());
    app.config.experiment = std::env::temp_dir().join(format!("crystal-first-play-{}", backend::unique_id().unwrap()));
    let sign_in = |app: &mut App, index: usize, code: bool, result: Result<(), &str>| {
        let row = &mut app.accounts[index];
        (row.busy, row.otp_spent, row.launch_trigger) = (true, code, Some(launch_timing::LaunchTrigger::Play));
        let key = row.key.clone();
        app.worker_event(WorkerEvent::Complete(key, result.map(|()| LaunchResult {
            pid: 41, detail: "Client started".into(), alive: Arc::new(AtomicBool::new(true)),
            game_running: Arc::new(AtomicBool::new(true)), log: PathBuf::new(),
            outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
        }).map_err(str::to_owned)));
    };
    let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
    assert!(keys.iter().all(|key| !app.presigned(key)), "unknown accounts are not pre-signed");
    sign_in(&mut app, 0, false, Ok(()));
    sign_in(&mut app, 1, true, Ok(()));
    sign_in(&mut app, 2, false, Err("Game exited during startup with code 0 (Windower via crystal-loader)."));
    // Rows 0 and 1 are playing; their games end.
    let known = |app: &App| keys.iter().map(|key| app.settings.presigns(key)).collect::<Vec<_>>();
    assert_eq!(known(&app), [true, false, false]);
    assert!(app.settings.account_otp.contains(&keys[1]) && !app.settings.account_otp.contains(&keys[2]), "a failure teaches nothing");
    let _ = app.update(Message::Tick);
    for row in &mut app.accounts[..2] {
        row.outcome = Some(Arc::new(std::sync::Mutex::new(Some(signed_out()))));
        row.alive.as_ref().unwrap().store(false, Ordering::Release);
    }
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].resign.is_none() && app.accounts[0].signed_out, "closing the game keeps POL closed by default");
    assert!(app.accounts[1].resign.is_none(), "an OTP account does not");
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Owner decision 2026-10-13: a pre-sign-in (no code) that the member login refused may be an
/// account that now uses a one-time code. It is marked as one and stops being pre-signed: no
/// retry, no sign-in after its next game, none at the next start, and its row says why. Its
/// next sign-in without a code that works makes it pre-signed again.
#[test]
fn a_refused_pre_sign_in_marks_the_account_otp_and_is_not_retried() {
    let refused = "Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -1036). (Windower via crystal-loader).";
    for trigger in [launch_timing::LaunchTrigger::StartupSignIn, launch_timing::LaunchTrigger::SignInAfterGame,
        launch_timing::LaunchTrigger::StartupRetry, launch_timing::LaunchTrigger::SignInAfterGameRetry] {
        assert!(trigger.is_presign());
        let (mut app, key) = resign_app("presign-refused");
        let row = &mut app.accounts[0];
        // What start_launch leaves on a pre-sign-in's row.
        (row.busy, row.preparing_session, row.launch_trigger, row.presign_login) = (true, true, Some(trigger), true);
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(refused.into())));
        let row = &app.accounts[0];
        assert!(row.network_wait.is_none() && row.resign.is_none() && !row.busy, "{trigger:?}");
        assert_eq!(row.status_text(), format!("{refused} · This account may need a one-time password now: Crystal signs it in only when you press Play."));
        assert!(app.settings.account_otp.contains(&key) && !app.settings.presigns(&key), "{trigger:?}");
        assert!(load_settings(&app.config).unwrap().account_otp.contains(&key), "saved");
        // Nothing signs it in by itself any more.
        let _ = app.update(Message::Tick);
        game_ends(&mut app, 0, signed_out());
        assert!(app.accounts[0].resign.is_none() && app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty());
        app.startup_signin_pending = true;
        assert!(app.take_startup_signins().is_empty() && app.startup_waiting.is_empty());
        assert!(app.launches.as_ref().unwrap().is_empty(), "never retried");
        let _ = std::fs::remove_dir_all(&app.config.experiment);
    }
    // A refusal of a sign-in that took a code teaches nothing; a network failure is no refusal.
    let (mut app, key) = resign_app("presign-refused-other");
    let row = &mut app.accounts[0];
    (row.busy, row.otp_spent, row.launch_trigger) = (true, true, Some(launch_timing::LaunchTrigger::Play));
    app.worker_event(WorkerEvent::Complete(key.clone(), Err(refused.into())));
    assert!(!app.settings.account_otp.contains(&key) && app.settings.account_no_otp.contains(&key));
    app.accounts[0].otp_spent = false;
    let row = &mut app.accounts[0];
    (row.busy, row.launch_trigger, row.presign_login) = (true, Some(launch_timing::LaunchTrigger::StartupSignIn), true);
    app.worker_event(WorkerEvent::Complete(key.clone(),
        Err("Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -19). (Windower via crystal-loader).".into())));
    assert!(app.settings.presigns(&key) && app.accounts[0].network_wait == Some(NetworkWait::StartupRetry));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Review findings (2026-10-13): only a refused member login counts, and only of a sign-in
/// Crystal started by itself. A failure of the network-level connection (before any password
/// is sent: PlayOnline down for its own maintenance, unreachable), a cancelled login or an
/// older loader's unnamed stage leaves a known code-less account pre-signed; so does a refusal
/// of the user's own Play or Connect without a code (Wait and play then does not ask that
/// account for a code it does not use). A refused pre-sign-in that a Play queued on still
/// marks it.
#[test]
fn only_a_refused_member_login_of_crystals_own_sign_in_marks_the_account_otp() {
    let refused = "Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -1036). (Windower via crystal-loader).";
    for failure in ["Game loader error during startup: Native PlayOnline network connection failed (state -1025, POL result -1036). (Windower via crystal-loader).",
        "Game loader error during startup: Native PlayOnline login was cancelled at state -1 (POL result -1036). (Windower via crystal-loader).",
        "Game loader error during startup: Native PlayOnline login failed (state 39, POL result -1036). (Windower via crystal-loader)."] {
        let (mut app, key) = resign_app("presign-not-refused");
        let row = &mut app.accounts[0];
        (row.busy, row.preparing_session, row.launch_trigger, row.presign_login) = (true, true, Some(launch_timing::LaunchTrigger::StartupSignIn), true);
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(failure.into())));
        assert!(app.settings.presigns(&key) && !app.settings.account_otp.contains(&key), "{failure}");
        assert_eq!(app.accounts[0].status_text(), failure, "no 'may need a one-time password' note");
        let _ = std::fs::remove_dir_all(&app.config.experiment);
    }
    // The user's own sign-ins: Play, Connect, a parked one, a batch Play.
    for trigger in [launch_timing::LaunchTrigger::Play, launch_timing::LaunchTrigger::Connect,
        launch_timing::LaunchTrigger::ParkedPlay, launch_timing::LaunchTrigger::BatchPlay, launch_timing::LaunchTrigger::ReissuedPlay] {
        assert!(!trigger.is_presign());
        let (mut app, key) = resign_app("user-refused");
        let row = &mut app.accounts[0];
        (row.busy, row.launch_trigger) = (true, Some(trigger));
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(refused.into())));
        assert!(app.settings.presigns(&key) && !app.settings.account_otp.contains(&key), "{trigger:?}");
        let _ = std::fs::remove_dir_all(&app.config.experiment);
    }
    // A Play queued on a pre-sign-in (start_together keeps the existing session's
    // presign_login): that pre-sign-in's refusal arrives as the Play's, and marks it.
    let (mut app, key) = resign_app("queued-refused");
    let row = &mut app.accounts[0];
    (row.busy, row.batch_requested, row.launch_trigger, row.presign_login) = (true, true, Some(launch_timing::LaunchTrigger::QueuedPlay), true);
    app.worker_event(WorkerEvent::Complete(key.clone(), Err(refused.into())));
    assert!(app.settings.account_otp.contains(&key) && !app.settings.presigns(&key));
    assert!(app.accounts[0].status_text().ends_with("This account may need a one-time password now: Crystal signs it in only when you press Play."));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Review findings (2026-10-13): a batch reports one account's failure to every account in it
/// (session_batch::coordinate). Another account's refused member login marks no other account
/// as one that needs a one-time code: not one that got its session (Ready) first, even if the
/// raw refusal reached it, and not one told in the coordinator's own words. Only the refused
/// account's own pre-sign-in marks it.
#[test]
fn a_batch_marks_only_the_refused_account_otp() {
    let refused = "Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -1036). (Windower via crystal-loader).";
    let (mut app, ready) = resign_app("batch-refused");
    let (refused_key, told) = (app.accounts[1].key.clone(), app.accounts[2].key.clone());
    app.settings.account_no_otp.extend([refused_key.clone(), told.clone()]);
    // Three pre-sign-ins in flight, each with a Play queued on it, started together.
    for row in &mut app.accounts[..3] {
        (row.busy, row.batch_pending, row.batch_requested, row.presign_login) = (true, true, true, true);
        row.launch_trigger = Some(launch_timing::LaunchTrigger::QueuedPlay);
    }
    app.worker_event(WorkerEvent::Complete(ready.clone(), Ok(LaunchResult {
        pid: 70, detail: "Signed in.".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: Arc::new(AtomicBool::new(false)), log: PathBuf::from("batch.log"),
        outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })));
    assert!(app.accounts[0].pid.is_some());
    app.worker_event(WorkerEvent::Complete(ready.clone(), Err(refused.into())));
    app.worker_event(WorkerEvent::Complete(refused_key.clone(), Err(refused.into())));
    let others = t("Another selected account's PlayOnline sign-in was refused, so FFXI was not started.");
    app.worker_event(WorkerEvent::Complete(told.clone(), Err(others.into())));
    assert!(app.settings.presigns(&ready) && !app.settings.account_otp.contains(&ready), "it got its session");
    assert!(app.settings.presigns(&told) && !app.settings.account_otp.contains(&told));
    assert!(app.settings.account_otp.contains(&refused_key) && !app.settings.presigns(&refused_key));
    assert!(!app.accounts[0].status_text().contains("may need a one-time password") && !app.accounts[2].status_text().contains("may need a one-time password"));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

const TEST_TOTP_KEY: &str = "JBSWY3DPEHPK3PXP";

/// An account with an authenticator key needs no typed code: it is signed in when Crystal
/// opens and again after its game, like an account without codes, even one known to take
/// codes or whose last typed code was spent, and its row shows no code boxes. The switch and
/// a code the user typed still win.
#[test]
fn an_authenticator_key_makes_an_otp_account_sign_in_by_itself() {
    let (mut app, key) = resign_app("authenticator-presign");
    app.settings.note_sign_in(&key, true);
    assert!(!app.presigned(&key) && account_row::needs_code(&app, &app.accounts[0]), "an OTP account without a key");
    app.accounts[0].account.totp_key = TEST_TOTP_KEY.into();
    assert!(app.accounts[0].authenticator_ready());
    assert!(app.presigned(&key) && app.resign_eligible(&key));
    assert!(!account_row::needs_code(&app, &app.accounts[0]));
    assert_eq!(account_row::row_state(&app, &app.accounts[0]), account_row::RowState::Idle);
    assert!(!account_row::row_view(&app, &app.accounts[0]).otp, "no code boxes on its row");
    // A spent typed code does not matter: the key makes a fresh one.
    app.accounts[0].otp_spent = true;
    assert!(app.presigned(&key) && !wants_typed_code(&app.settings, &app.accounts[0]));
    app.accounts[0].otp_spent = false;
    // The user's choices win: the switch, and a code typed for their own Play.
    app.settings.auto_sign_in = false;
    assert!(!app.presigned(&key));
    app.settings.auto_sign_in = true;
    app.accounts[0].otp = "123456".into();
    assert!(!app.presigned(&key));
    app.accounts[0].otp.clear();
    // When Crystal opens, and after its game.
    app.startup_signin_pending = true;
    assert_eq!(app.take_startup_signins(), vec![key.clone()]);
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_some(), "it signs back in after its game");
    // Its own sign-in without a typed code works: the account stays known to take codes
    // (should the key be removed) and stays eligible.
    let row = &mut app.accounts[0];
    (row.busy, row.resign, row.launch_trigger) = (true, None, Some(launch_timing::LaunchTrigger::StartupSignIn));
    app.worker_event(WorkerEvent::Complete(key.clone(), Ok(LaunchResult {
        pid: 41, detail: "Signed in.".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: Arc::new(AtomicBool::new(false)), log: PathBuf::new(),
        outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })));
    assert!(app.settings.account_otp.contains(&key) && app.presigned(&key));
    // A failure that never reached a verdict retries once the network is back, as for an
    // account without codes: the key makes the retry's code.
    let row = &mut app.accounts[0];
    (row.pid, row.alive, row.game_running) = (None, None, None);
    (row.busy, row.otp_spent, row.launch_trigger, row.presign_login) = (true, true, Some(launch_timing::LaunchTrigger::StartupSignIn), true);
    app.worker_event(WorkerEvent::Complete(key.clone(),
        Err("Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -19). (Windower via crystal-loader).".into())));
    assert_eq!(app.accounts[0].network_wait, Some(NetworkWait::StartupRetry));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// A code made from the key that PlayOnline refuses: the row says so in one short line and
/// shows the code boxes, so the next attempt can be a typed code. Nothing signs the account
/// in again by itself (no retry, none after its game, none at the next start), whatever
/// started the refused sign-in. A sign-in without a typed code that works, or an edited
/// key, makes the key usable again.
#[test]
fn a_refused_authenticator_code_falls_back_to_a_typed_code_and_is_not_retried() {
    i18n::set(i18n::Lang::En);
    let refused = "Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -1036). (Windower via crystal-loader).";
    for trigger in [launch_timing::LaunchTrigger::StartupSignIn, launch_timing::LaunchTrigger::SignInAfterGame,
        launch_timing::LaunchTrigger::Play, launch_timing::LaunchTrigger::QueuedPlay] {
        let (mut app, key) = resign_app("authenticator-refused");
        app.accounts[0].account.totp_key = TEST_TOTP_KEY.into();
        let row = &mut app.accounts[0];
        (row.busy, row.preparing_session, row.launch_trigger, row.presign_login) = (true, true, Some(trigger), trigger.is_presign());
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(refused.into())));
        let row = &app.accounts[0];
        assert!(row.authenticator_refused && !row.authenticator_ready(), "{trigger:?}");
        assert_eq!(row.status_text(), "Authenticator code refused. Check the key.", "{trigger:?}");
        assert!(row.network_wait.is_none() && row.resign.is_none() && row.queued_play.is_none(), "{trigger:?}");
        assert_eq!(account_row::row_state(&app, row), account_row::RowState::NeedsCode, "{trigger:?}");
        let view = account_row::row_view(&app, row);
        assert!(view.otp && view.detail == "Authenticator code refused. Check the key.", "{trigger:?}");
        assert!(!app.presigned(&key) && wants_typed_code(&app.settings, row), "{trigger:?}");
        // Nothing signs it in by itself any more.
        let _ = app.update(Message::Tick);
        game_ends(&mut app, 0, signed_out());
        assert!(app.accounts[0].resign.is_none() && app.take_resigns(Instant::now() + RESIGN_DELAY).is_empty(), "{trigger:?}");
        app.startup_signin_pending = true;
        assert!(app.take_startup_signins().is_empty() && app.startup_waiting.is_empty(), "{trigger:?}");
        app.maintenance_sign_ins();
        assert!(app.launches.as_ref().unwrap().is_empty(), "never retried: {trigger:?}");
        // A typed code is the next attempt.
        app.accounts[0].otp = "123456".into();
        assert!(matches!(account_row::row_view(&app, &app.accounts[0]).action, Some((_, Message::SignIn(_), true))));
        app.accounts[0].otp.clear();
        let _ = std::fs::remove_dir_all(&app.config.experiment);
    }
    // A refusal of a typed code says nothing about the key.
    let (mut app, key) = resign_app("authenticator-typed-refused");
    app.accounts[0].account.totp_key = TEST_TOTP_KEY.into();
    let row = &mut app.accounts[0];
    (row.busy, row.otp_spent, row.launch_trigger) = (true, true, Some(launch_timing::LaunchTrigger::Play));
    app.worker_event(WorkerEvent::Complete(key.clone(), Err(refused.into())));
    assert!(!app.accounts[0].authenticator_refused && app.accounts[0].status == refused);
    // A sign-in without a typed code that works clears a refusal.
    app.accounts[0].authenticator_refused = true;
    let row = &mut app.accounts[0];
    (row.busy, row.otp_spent, row.launch_trigger) = (true, false, Some(launch_timing::LaunchTrigger::Play));
    app.worker_event(WorkerEvent::Complete(key.clone(), Ok(LaunchResult {
        pid: 41, detail: "Client started".into(), alive: Arc::new(AtomicBool::new(true)),
        game_running: Arc::new(AtomicBool::new(true)), log: PathBuf::new(),
        outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
    })));
    assert!(app.accounts[0].authenticator_ready());
    // An edited key is tried afresh.
    app.accounts[0].authenticator_refused = true;
    let mut accounts: Vec<Account> = app.accounts.iter().map(|row| row.account.clone()).collect();
    accounts[0].totp_key = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ".into();
    app.replace_accounts(accounts.clone());
    assert!(app.accounts[0].authenticator_ready());
    // The same key saved again keeps the refusal.
    app.accounts[0].authenticator_refused = true;
    app.replace_accounts(accounts);
    assert!(app.accounts[0].authenticator_refused);
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// The account sheet saves the key as typed from a setup page (spaces, lowercase, padding)
/// in its stored form, refuses one with other characters with a short reason (the sheet
/// stays open), and clears it when emptied.
#[test]
fn the_account_sheet_saves_the_authenticator_key_in_its_stored_form() {
    i18n::set(i18n::Lang::En);
    let mut app = app();
    let key = app.accounts[0].key.clone();
    let _ = app.update(Message::EditAccount(key.clone()));
    for (typed, error) in [("jbsw y3dp ehpk 3px1", totp::KeyError::Characters), ("jbsw y3dp", totp::KeyError::Length)] {
        let _ = app.update(Message::EditorChanged(Field::TotpKey, typed.into()));
        let _ = app.update(Message::SaveAccount);
        assert_eq!(app.status, error.message(), "{typed}");
        assert_eq!(app.editor.as_ref().unwrap().account.totp_key, typed, "kept for correction");
    }
    let _ = app.update(Message::EditorChanged(Field::TotpKey, "jbsw y3dp ehpk 3pxp ====".into()));
    let _ = app.update(Message::SaveAccount);
    assert_eq!(app.editor.as_ref().unwrap().account.totp_key, TEST_TOTP_KEY);
    let _ = app.update(Message::EditorChanged(Field::TotpKey, "   ".into()));
    let _ = app.update(Message::SaveAccount);
    assert!(app.editor.as_ref().unwrap().account.totp_key.is_empty());
}

/// Owner decision 2026-10-13: the Settings switch, off, stops Crystal's own sign-ins at its
/// start, after a game, a startup sign-in waiting for a closing session and a startup network
/// retry, and parks every account instead. Sessions already signed in stay. It is saved.
#[test]
fn the_switch_off_stops_startup_and_after_game_sign_ins() {
    let (mut app, key) = resign_app("presign-switch");
    let _ = app.update(Message::AutoSignIn(false));
    assert!(!load_settings(&app.config).unwrap().auto_sign_in, "saved");
    app.startup_signin_pending = true;
    assert!(app.take_startup_signins().is_empty());
    app.startup_issued = None; // no real park host from the Ticks below
    assert!(app.park_keys().iter().any(|(row, ..)| *row == key), "it parks like any other account");
    game_ends(&mut app, 0, signed_out());
    assert!(app.accounts[0].resign.is_none());
    // Pending when it was turned off.
    let _ = app.update(Message::AutoSignIn(true));
    app.startup_waiting.push((key.clone(), Instant::now()));
    app.accounts[0].network_wait = Some(NetworkWait::StartupRetry);
    assert!(!app.park_keys().iter().any(|(row, ..)| *row == key), "a pre-sign-in is coming");
    let _ = app.update(Message::AutoSignIn(false));
    assert!(app.take_waiting_startups(Instant::now()).is_empty() && app.startup_waiting.is_empty());
    let generation = app.accounts[0].launch_generation;
    app.worker_event(WorkerEvent::NetworkBack(key.clone(), true).for_launch(&key, generation));
    assert!(app.launches.as_ref().unwrap().is_empty());
    // A signed-in session is not signed out.
    let alive = Arc::new(AtomicBool::new(true));
    let row = &mut app.accounts[0];
    (row.pid, row.alive, row.game_running) = (Some(77), Some(alive.clone()), Some(Arc::new(AtomicBool::new(false))));
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].pid.is_some() && alive.load(Ordering::Acquire));
    // On again: the next start signs it in.
    let _ = app.update(Message::AutoSignIn(true));
    app.accounts[0].pid = None;
    app.accounts[0].alive = None;
    app.accounts[0].game_running = None;
    app.startup_signin_pending = true;
    assert_eq!(app.take_startup_signins(), vec![key]);
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Files before 2026-10-13 load: an account that had "Sign in when Crystal opens" is known to
/// sign in without a code (unless its latest sign-in took one), the switch is on, and the old
/// field is never written again. New fields round-trip.
#[test]
fn the_ah_server_choice_is_saved_and_unknown_values_fall_back() {
    let root = std::env::temp_dir().join(format!("crystal-ah-server-{}", backend::unique_id().unwrap()));
    let config = crate::backend::Config { experiment: root.clone(), repository: root.clone(), game_install: None, dat_overlays: Vec::new() };
    let path = settings_path(&config);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"install_path":"C:\\Games\\FFXI"}"#).unwrap();
    let old = load_settings(&config).unwrap();
    assert!(old.ah_server.is_none());
    assert_eq!(crate::ffxiah::saved_ah_server(old.ah_server.as_deref()), crate::ffxiah::DEFAULT_AH_SERVER);

    let mut app = app();
    app.preview = false;
    app.config = config.clone();
    app.settings = old;
    let _ = app.update(Message::AhServerChanged("Bismarck"));
    assert_eq!(app.ah_server, "Bismarck");
    let reloaded = load_settings(&config).unwrap();
    assert_eq!(crate::ffxiah::saved_ah_server(reloaded.ah_server.as_deref()), "Bismarck");

    assert_eq!(crate::ffxiah::saved_ah_server(Some("Nowhere")), crate::ffxiah::DEFAULT_AH_SERVER);
    assert_eq!(crate::ffxiah::saved_ah_server(None), crate::ffxiah::DEFAULT_AH_SERVER);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn old_startup_sign_in_settings_migrate_and_new_ones_round_trip() {
    let root = std::env::temp_dir().join(format!("crystal-presign-migrate-{}", backend::unique_id().unwrap()));
    let config = crate::backend::Config { experiment: root.clone(), repository: root.clone(), game_install: None, dat_overlays: Vec::new() };
    let path = settings_path(&config);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"install_path":"C:\\Games\\FFXI","account_startup_signin":["a","b"],"account_otp":["b"]}"#).unwrap();
    let loaded = load_settings(&config).unwrap();
    assert!(loaded.auto_sign_in);
    assert_eq!(loaded.account_no_otp, BTreeSet::from(["a".to_owned()]));
    assert_eq!(loaded.account_otp, BTreeSet::from(["b".to_owned()]), "a code taken since is newer evidence");
    assert!(loaded.presigns("a") && !loaded.presigns("b") && !loaded.presigns("c"));
    save_settings(&config, &loaded).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("account_startup_signin"), "{text}");
    let mut reloaded = load_settings(&config).unwrap();
    assert_eq!((reloaded.account_no_otp.clone(), reloaded.account_otp.clone(), reloaded.auto_sign_in), (loaded.account_no_otp.clone(), loaded.account_otp.clone(), true));
    reloaded.auto_sign_in = false;
    save_settings(&config, &reloaded).unwrap();
    assert!(!load_settings(&config).unwrap().auto_sign_in);
    let _ = std::fs::remove_dir_all(root);
}

/// A batch (Windower or Ashita accounts started together, or Plays released from a wait)
/// whose W20 asks for an update reports each account Ready, then Failed
/// (session_batch::coordinate). Their sessions stay signed in: the update runs in them, and
/// its success starts every account through the signed-in path with no second check.
#[test]
fn a_batch_refused_for_an_update_runs_it_in_the_sessions_the_batch_kept() {
    let (mut app, root) = play_update_app("batch-kept", Some("30260904_1"));
    let keys = vec![app.accounts[0].key.clone(), app.accounts[1].key.clone()];
    // What start_together leaves on the rows, then the coordinator's events.
    let clicked = Instant::now();
    for row in &mut app.accounts[..2] {
        (row.busy, row.batch_pending, row.batch_requested) = (true, true, true);
        row.launch_trigger = Some(launch_timing::LaunchTrigger::BatchPlay);
        row.play_timing = Some(launch_timing::Timeline::begin_at(clicked));
    }
    for (at, key) in keys.iter().enumerate() {
        app.worker_event(WorkerEvent::Complete(key.clone(), Ok(LaunchResult {
            pid: 70 + at as u32, detail: "Signed in.".into(), alive: Arc::new(AtomicBool::new(true)),
            game_running: Arc::new(AtomicBool::new(false)), log: root.join("batch.log"),
            outcome: Arc::new(std::sync::Mutex::new(None)), timing: String::new(),
        })));
    }
    let message = patch::gate(patch::LaunchCheck::Update(refused_report())).unwrap_err();
    for key in &keys {
        app.accounts.iter_mut().find(|row| row.key == *key).unwrap().batch_requested = true;
        app.worker_event(WorkerEvent::Complete(key.clone(), Err(message.clone())));
    }
    assert_eq!(app.play_updates.spawned.len(), 1);
    assert_eq!(app.play_updates.jobs[0].waiters.iter().map(|w| w.key.clone()).collect::<Vec<_>>(), keys);
    assert!(app.refused_plays.is_empty() && app.maintenance.is_none(), "no Downloads update, nobody signed out");
    assert!(app.accounts[..2].iter().all(|row| row.busy && row.pid.is_some() && !row.problem));
    assert!(!app.can_restart_launcher(), "an update in progress is pending launch work");
    let job = generation(&app);
    app.worker_event(WorkerEvent::PlayUpdate(job, play_update::Event::Finished(Ok(play_update::Done { summary: "Updated.".into(), report: None }))));
    let started: Vec<_> = app.launches.as_ref().unwrap().iter().map(|(keys, _, _, trigger)| (keys.clone(), *trigger)).collect();
    assert_eq!(started, keys.iter().map(|key| (vec![key.clone()], Some(launch_timing::LaunchTrigger::ReissuedPlay))).collect::<Vec<_>>());
    assert_eq!(app.w20_skipped, keys, "each starts without a second W20");
    std::fs::remove_dir_all(root).unwrap();
}

/// Review finding: a language switch replaced the status with the cached headlines
/// (Unknown), and the first-read hold was already spent, so Play went through during
/// maintenance until the new language's read landed.
#[test]
fn a_language_switch_keeps_the_play_gate() {
    let root = std::env::var_os("CRYSTAL_DATA_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join(format!("crystal-language-gate-{}", backend::unique_id().unwrap()));
    let mut app = app();
    app.config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    i18n::set(i18n::Lang::En);
    app.preview = false;
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(7 * 60)));
    assert!(app.service_down());
    let _ = app.update(Message::SetLanguage(i18n::Lang::Ja));
    assert!(app.service_down(), "still maintenance after the switch");
    app.launches = None;
    app.start_launch(vec![app.accounts[0].key.clone()], false, Instant::now(), None);
    assert!(!app.accounts[0].busy && app.receiver.try_recv().is_err(), "refused");
    let _ = app.update(Message::SetLanguage(i18n::Lang::En));
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

/// Review finding: a refused sign-in after the game came back as the startup sign-in that
/// was waiting for that session.
#[test]
fn a_refused_sign_in_after_the_game_cancels_a_waiting_startup_sign_in() {
    let (mut app, key) = resign_app("resign-refused-waiting");
    status_read(&mut app, services::Availability::Operational, Some(Duration::from_secs(8 * 60)));
    app.startup_waiting.push((key.clone(), Instant::now()));
    game_ends(&mut app, 0, signed_out());
    assert!(app.startup_waiting.is_empty());
    let _ = app.update(Message::Tick);
    assert!(app.launches.as_ref().unwrap().is_empty() && app.receiver.try_recv().is_err());
    let _ = std::fs::remove_dir_all(&app.config.experiment);
}

#[test]
fn startup_signin_waits_for_unknown_accounts_and_selection_runs_only_once_after_vault_load() {
    let older: Settings = serde_json::from_str(r#"{"install_path":"C:\\Games\\FFXI"}"#).unwrap();
    assert!(older.auto_sign_in && older.account_no_otp.is_empty(), "on, but no account is known yet");
    let mut app = app();
    app.preview = false;
    app.startup_signin_pending = true;
    app.vault_ready = false;
    let first = app.accounts[0].key.clone();
    app.settings.account_no_otp = app.accounts.iter().map(|r| r.key.clone()).collect();
    app.accounts[1].busy = true; // Reconnecting to an existing background worker.
    app.accounts[2].account.mode = AccountMode::Private;
    assert!(app.take_startup_signins().is_empty());
    assert!(app.startup_signin_pending);
    app.vault_ready = true;
    app.installation_loading = true;
    assert!(app.take_startup_signins().is_empty());
    assert!(app.startup_signin_pending);
    app.installation_loading = false;
    assert_eq!(app.take_startup_signins(), vec![first]);
    app.accounts[1].busy = false;
    assert!(app.take_startup_signins().is_empty(), "refresh and client exit must not sign in again");
}

/// Finding: a startup key queued while its row reconnects to the previous session is
/// dropped quietly once that session is back signed in; 60 s later its status is not
/// overwritten with "skipped". A row still busy past the limit says it was skipped.
#[test]
fn a_reconnected_startup_row_keeps_its_status() {
    let mut app = app();
    app.preview = false;
    app.startup_signin_pending = true;
    let key = app.accounts[0].key.clone();
    app.settings.account_no_otp.insert(key.clone());
    app.settings.account_no_otp.insert(app.accounts[1].key.clone());
    app.accounts[0].busy = true; // Reconnecting to its background worker.
    app.accounts[1].busy = true; // Its previous host still closing.
    assert!(app.take_startup_signins().is_empty());
    assert_eq!(app.startup_waiting.len(), 2);
    // The reconnect completes: signed in, FFXI closed.
    let row = &mut app.accounts[0];
    (row.busy, row.pid, row.game_running) = (false, Some(99), Some(Arc::new(AtomicBool::new(false))));
    row.status = "Signed in to PlayOnline · FFXI is closed".into();
    let later = Instant::now() + STARTUP_WAIT_LIMIT + Duration::from_secs(1);
    assert!(app.take_waiting_startups(later).is_empty());
    assert_eq!(app.accounts[0].status, "Signed in to PlayOnline · FFXI is closed");
    assert_eq!(app.accounts[1].status, "Startup sign-in skipped: a session is still running.");
    assert!(app.startup_waiting.is_empty());
}

#[test]
fn startup_signin_skips_existing_sessions_previews_and_file_operations() {
    let mut app = app();
    app.preview = false;
    app.startup_signin_pending = true;
    app.settings.account_no_otp.insert(app.accounts[0].key.clone());
    app.accounts[0].pid = Some(99);
    app.accounts[0].game_running = Some(Arc::new(AtomicBool::new(false)));
    assert!(app.take_startup_signins().is_empty());
    // A social-only action on that session must not even schedule StartGame.
    app.connect(vec![app.accounts[0].key.clone()]);
    assert!(!app.accounts[0].busy);
    assert!(app.accounts[0].play_timing.is_none());
    assert!(app.receiver.try_recv().is_err());
    app.accounts[0].pid = None;
    app.startup_signin_pending = true;
    app.preview = true;
    assert!(app.take_startup_signins().is_empty());
    app.preview = false;
    app.startup_signin_pending = true;
    app.maintenance = Some(MaintenanceKind::Repair);
    assert!(app.take_startup_signins().is_empty());
}

/// What Crystal learned about a login's sign-ins stays with that login: an edit that keeps
/// it keeps the knowledge, a changed login (a new account key) learns it again, and a deleted
/// account's goes with it.
#[test]
fn sign_in_knowledge_follows_the_login_and_deleted_accounts() {
    let mut app = app();
    let old_key = app.accounts[0].key.clone();
    let other_key = app.accounts[1].key.clone();
    app.settings.account_no_otp.extend([old_key.clone(), other_key.clone()]);
    app.settings.account_otp.insert(app.accounts[2].key.clone());
    let _ = app.update(Message::EditAccount(other_key.clone()));
    let _ = app.update(Message::EditorChanged(Field::Label, "Renamed".into()));
    let saved = app.accounts.iter().map(|r| r.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(saved)));
    assert!(app.settings.presigns(&other_key), "the same login keeps what was learned");
    let _ = app.update(Message::EditAccount(old_key.clone()));
    let _ = app.update(Message::EditorChanged(Field::PolId, "NEW12345".into()));
    let edited = app.editor.as_ref().unwrap().account.clone();
    let new_key = account_key(&edited);
    let _ = app.update(Message::AccountsSaved(Ok(vec![edited])));
    assert!(app.settings.account_no_otp.is_empty() && app.settings.account_otp.is_empty(), "deleted accounts are forgotten");
    assert!(!app.settings.presigns(&new_key), "a changed login is unknown until it signs in");
}

#[test]
fn ready_session_timing_waits_for_game_start_and_is_cleared_on_refusal() {
    let mut app = app();
    let game = Arc::new(AtomicBool::new(false));
    let row = &mut app.accounts[0];
    row.pid = Some(99);
    row.alive = Some(Arc::new(AtomicBool::new(true)));
    row.game_running = Some(game.clone());
    row.busy = true;
    row.play_timing = Some(launch_timing::Timeline::begin_at(Instant::now() - Duration::from_secs(1)));
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].play_timing.is_some());
    game.store(true, Ordering::Release);
    let _ = app.update(Message::Tick);
    assert!(app.accounts[0].play_timing.is_none());
    assert!(!app.accounts[0].busy);
    assert!(app.accounts[0].timing.starts_with("Game started "));
    assert!(!app.accounts[0].timing.contains("Signed in"), "do not reuse the earlier sign-in timing");
    let row = &mut app.accounts[0];
    row.play_timing = Some(launch_timing::Timeline::begin());
    let key = row.key.clone();
    let report = patch::PatchReport { state: patch::PatchState::Unknown, local_version: String::new(), remote_version: String::new(), detail: String::new(), files_verified: None };
    app.worker_event(WorkerEvent::StartGame(key.clone(), Some(report.clone()), Err("Version check refused".into())));
    assert!(app.accounts[0].play_timing.is_none());
    assert!(app.accounts[0].problem);
    // An accepted start was W20 done: the Play's timeline shows it (07c §5.3).
    app.accounts[0].play_timing = Some(launch_timing::Timeline::begin());
    app.worker_event(WorkerEvent::StartGame(key.clone(), Some(report), Ok("Game version matches.".into())));
    assert!(app.accounts[0].play_timing.as_ref().unwrap().elapsed_at(launch_timing::Milestone::GameChecked).is_some());
    // A session that never reached its main menu ran no W20 and keeps the shown report.
    app.patch = None;
    app.worker_event(WorkerEvent::StartGame(key, None, Err("PlayOnline did not return to its main menu in time, so FFXI was not started.".into())));
    assert!(app.patch.is_none() && app.accounts[0].problem);
}

#[test]
fn vanilla_selection_in_home_and_the_editor_uses_the_same_direct_launch_plan() {
    let mut app = app();
    let key = app.accounts[0].key.clone();
    let other = app.accounts[1].account.launch_with;
    let _ = app.set_account_launcher(std::slice::from_ref(&key), crate::backend::LaunchWith::Vanilla);
    let account = &app.accounts[0].account;
    assert_eq!(app.settings.launch_plan(account).unwrap().tool, pol_less::Tool::Vanilla);
    assert_eq!(app.accounts[1].account.launch_with, other);
    app.editor = Some(Editor { key: Some(key), account: account.clone(),
        game_install: String::new(), dat_overlays: Vec::new(), auto_login: pol_less::AutoLogin::default(), flags: EditorFlags::default() });
    let _ = app.update(Message::EditorLaunchWith(Some(crate::backend::LaunchWith::Windower)));
    assert_eq!(app.editor.as_ref().unwrap().account.launch_with, Some(crate::backend::LaunchWith::Windower));
    let _ = app.update(Message::EditorLaunchWith(Some(crate::backend::LaunchWith::Vanilla)));
    assert_eq!(app.settings.launch_plan(&app.editor.as_ref().unwrap().account).unwrap().tool, pol_less::Tool::Vanilla);
    assert_eq!(app.accounts[0].account.launch_with, Some(crate::backend::LaunchWith::Vanilla));
}

/// The sheet's "Uses a one-time password" is what Crystal knows about the login: on, it is
/// never signed in by itself; off, the next sign-in teaches it again.
#[test]
fn the_sheet_one_time_password_switch_sets_what_crystal_knows() {
    let mut app = app();
    let key = app.accounts[0].key.clone();
    app.settings.account_no_otp.insert(key.clone());
    assert!(app.settings.presigns(&key) || !app.settings.auto_sign_in);
    let values: Vec<_> = app.accounts.iter().map(|row| row.account.clone()).collect();
    let _ = app.update(Message::EditAccount(key.clone()));
    assert!(!app.editor.as_ref().unwrap().flags.uses_otp);
    let _ = app.update(Message::EditorFlag(EditorFlag::UsesOtp, true));
    let _ = app.update(Message::AccountsSaved(Ok(values.clone())));
    assert!(app.settings.account_otp.contains(&key) && !app.settings.account_no_otp.contains(&key));
    assert!(!app.settings.presigns(&key));
    let _ = app.update(Message::EditAccount(key.clone()));
    assert!(app.editor.as_ref().unwrap().flags.uses_otp);
    let _ = app.update(Message::EditorFlag(EditorFlag::UsesOtp, false));
    let _ = app.update(Message::AccountsSaved(Ok(values)));
    assert!(!app.settings.account_otp.contains(&key) && !app.settings.account_no_otp.contains(&key));
    // A new account's sheet signs in in the background by default.
    let _ = app.update(Message::NewAccount);
    assert!(app.editor.as_ref().unwrap().flags.sign_in_now);
}

#[test]
fn save_failure_keeps_account_state_and_editor() {
    let mut app = app();
    app.editor = Some(Editor::blank());
    app.vault_saving = true;
    let _ = app.update(Message::AccountsSaved(Err("Save failed".into())));
    assert_eq!(app.accounts.len(), 3);
    assert!(app.editor.is_some());
    assert!(!app.vault_saving);
}

#[test]
fn dragging_accounts_persists_order_without_rewriting_credentials_or_live_state() {
    let mut app = app();
    let root = std::env::temp_dir().join(format!("crystal-account-order-{}", backend::unique_id().unwrap()));
    app.config.experiment = root.clone();
    app.preview = false;
    app.page = Page::Home;
    let values: Vec<_> = app.accounts.iter().map(|row| row.account.clone()).collect();
    backend::save_accounts(&app.config, &values).unwrap();
    let vault = root.join("iced-launcher/accounts.dpapi");
    let ciphertext = std::fs::read(&vault).unwrap();
    let original: Vec<_> = app.accounts.iter().map(|row| row.key.clone()).collect();
    let alive = Arc::new(AtomicBool::new(true));
    let running = Arc::new(AtomicBool::new(true));
    app.accounts[0].pid = Some(77);
    app.accounts[0].alive = Some(alive.clone());
    app.accounts[0].game_running = Some(running.clone());
    app.accounts[0].launch_generation = 19;
    app.accounts[0].otp = "123456".into();
    let _ = app.update(Message::AccountDragStart(original[0].clone()));
    let _ = app.update(Message::AccountDragOver(original[2].clone()));
    let _ = app.update(Message::AccountDragFinish);
    let expected = vec![original[1].clone(), original[2].clone(), original[0].clone()];
    assert_eq!(app.accounts.iter().map(|row| row.key.clone()).collect::<Vec<_>>(), expected);
    let moved = &app.accounts[2];
    assert!(Arc::ptr_eq(moved.alive.as_ref().unwrap(), &alive));
    assert!(Arc::ptr_eq(moved.game_running.as_ref().unwrap(), &running));
    assert_eq!((moved.pid, moved.launch_generation, moved.otp.as_str()), (Some(77), 19, "123456"));
    assert_eq!(std::fs::read(&vault).unwrap(), ciphertext);
    assert_eq!(load_settings(&app.config).unwrap().account_order, expected);
    let mut reopened = self::app();
    reopened.config.experiment = root.clone();
    reopened.settings = load_settings(&app.config).unwrap();
    reopened.replace_accounts(backend::load_accounts(&app.config).unwrap());
    assert_eq!(reopened.accounts.iter().map(|row| row.key.clone()).collect::<Vec<_>>(), expected);
    assert_eq!(std::fs::read(vault).unwrap(), ciphertext);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn account_drags_start_from_the_rail_on_every_page() {
    for page in [Page::Home, Page::Friends, Page::Maintenance, Page::Settings] {
        let mut app = app();
        app.page = page;
        let key = app.accounts[0].key.clone();
        let _ = app.update(Message::AccountDragStart(key.clone()));
        assert_eq!(app.account_drag.source.as_deref(), Some(key.as_str()));
    }
}

#[test]
fn controller_shoulders_follow_the_top_bar_through_features() {
    let mut app = app();
    app.page = Page::Maintenance;
    let _ = app.update(Message::ControllerPage(1));
    assert!(app.features_view.open && app.page == Page::Maintenance);
    let _ = app.update(Message::ControllerPage(1));
    assert!(!app.features_view.open && app.page == Page::Settings);
    let _ = app.update(Message::ControllerPage(-1));
    assert!(app.features_view.open);
    let _ = app.update(Message::ControllerPage(-1));
    assert!(!app.features_view.open && app.page == Page::Maintenance);
}

#[test]
fn canceled_or_failed_account_drags_restore_the_previous_order() {
    let mut app = app();
    app.page = Page::Home;
    let original: Vec<_> = app.accounts.iter().map(|row| row.key.clone()).collect();
    let _ = app.update(Message::AccountDragStart(original[0].clone()));
    let _ = app.update(Message::AccountDragOver(original[2].clone()));
    let _ = app.update(Message::ControllerBack);
    assert!(app.account_drag.source.is_none());
    assert_eq!(app.accounts.iter().map(|row| row.key.clone()).collect::<Vec<_>>(), original);
    let root = std::env::temp_dir().join(format!("crystal-order-failure-{}", backend::unique_id().unwrap()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("iced-launcher"), b"blocked settings directory").unwrap();
    app.config.experiment = root.clone();
    app.preview = false;
    let _ = app.update(Message::AccountDragStart(original[0].clone()));
    let _ = app.update(Message::AccountDragOver(original[2].clone()));
    let _ = app.update(Message::AccountDragFinish);
    assert_eq!(app.accounts.iter().map(|row| row.key.clone()).collect::<Vec<_>>(), original);
    assert!(app.settings.account_order.is_empty());
    assert!(app.status.starts_with("Account order could not be saved:"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pol_handle_setup_survives_an_account_reload_and_can_be_cleared() {
    let mut app=app();
    let key=app.accounts[0].key.clone();
    let _=app.update(Message::EditAccount(key.clone()));
    let _=app.update(Message::EditorChanged(Field::PolHandleNames,"PublicOne, PublicTwo".into()));
    let mut accounts:Vec<_>=app.accounts.iter().map(|r| r.account.clone()).collect();
    accounts[0]=app.editor.as_ref().unwrap().account.clone();
    let _=app.update(Message::AccountsSaved(Ok(accounts.clone())));
    assert_eq!(app.settings.account_pol_handles.get(&key).map(String::as_str),Some("PublicOne, PublicTwo"));
    let decoded:Settings=serde_json::from_str(&serde_json::to_string(&app.settings).unwrap()).unwrap();
    app.settings=decoded;
    for account in &mut accounts { account.pol_handle_names.clear(); }
    let _=app.update(Message::AccountsLoaded(Ok(accounts)));
    assert_eq!(app.accounts[0].account.pol_handle_names,"PublicOne, PublicTwo");
    let _=app.update(Message::EditAccount(key.clone()));
    let _=app.update(Message::EditorChanged(Field::PolHandleNames,String::new()));
    let mut accounts:Vec<_>=app.accounts.iter().map(|r| r.account.clone()).collect();
    accounts[0]=app.editor.as_ref().unwrap().account.clone();
    let _=app.update(Message::AccountsSaved(Ok(accounts)));
    assert!(!app.settings.account_pol_handles.contains_key(&key));
    assert!(app.accounts[0].account.pol_handle_names.is_empty());
}

#[test]
fn profile_edits_during_play_keep_the_original_session_and_lock_its_login_identity() {
    let mut app = app();
    let keys: Vec<_> = app.accounts.iter().map(|row| row.key.clone()).collect();
    let alive = Arc::new(AtomicBool::new(true));
    let running = Arc::new(AtomicBool::new(true));
    app.accounts[0].pid = Some(77);
    app.accounts[0].alive = Some(alive.clone());
    app.accounts[0].game_running = Some(running.clone());
    app.accounts[0].launch_generation = 22;
    let _ = app.update(Message::EditAccount(keys[0].clone()));
    assert!(app.editor.is_some());
    let _ = app.update(Message::EditorChanged(Field::PolId, "DIFF1234".into()));
    assert_eq!(account_key(&app.editor.as_ref().unwrap().account), keys[0]);
    let _ = app.update(Message::EditorProfile(Some("Next launch".into())));
    let _ = app.update(Message::EditorChanged(Field::Label, "Updated label".into()));
    assert_ne!(app.accounts[0].account.label, "Updated label");
    let edited = app.editor.as_ref().unwrap().account.clone();
    let mut values: Vec<_> = app.accounts.iter().map(|row| row.account.clone()).collect();
    values[0] = edited;
    let _ = app.update(Message::AccountsSaved(Ok(values)));
    assert_eq!(app.accounts.iter().map(|row| row.key.clone()).collect::<Vec<_>>(), keys);
    let row = &app.accounts[0];
    assert_eq!(row.account.windower_profile.as_deref(), Some("Next launch"));
    assert_eq!(row.account.label, "Updated label");
    assert_eq!((row.pid, row.launch_generation), (Some(77), 22));
    assert!(Arc::ptr_eq(row.alive.as_ref().unwrap(), &alive));
    assert!(Arc::ptr_eq(row.game_running.as_ref().unwrap(), &running));
    assert!(row.is_playing());
}
#[test]
fn saved_account_folders_are_isolated_and_loading_accounts_does_not_replace_them() {
    let mut app = app();
    let first = app.accounts[0].account.clone();
    let second = app.accounts[1].account.clone();
    let first_key = account_key(&first);
    let path = r"C:\Games\Private Server";
    app.settings.install_path = r"C:\Games\Retail".into();
    app.settings.install_selected = true;
    app.editor = Some(Editor { key: Some(first_key.clone()), account: first.clone(), game_install: path.into(), dat_overlays: vec![r"C:\Mods\High Priority".into(), r"C:\Mods\Fallback".into()], auto_login: pol_less::AutoLogin::default(), flags: EditorFlags::default() });
    let values: Vec<_> = app.accounts.iter().map(|r| r.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(values.clone())));
    let config = app.settings.config_for(&app.config, &first);
    assert_eq!(config.game_directory(), PathBuf::from(path));
    assert!(config.dat_overlays.is_empty(), "retail injectors do not use the disabled native overlays");
    assert_eq!(app.settings.account_overlays[&first_key], [r"C:\Mods\High Priority", r"C:\Mods\Fallback"], "saved legacy overlay paths are kept");
    let other = app.settings.config_for(&app.config, &second);
    assert_eq!(other.game_directory(), PathBuf::from(r"C:\Games\Retail"));
    assert!(other.dat_overlays.is_empty());
    assert_eq!(other.experiment, config.experiment, "account storage is independent of the chosen game installation");
    let persisted = serde_json::to_vec(&app.settings).unwrap();
    let reloaded: Settings = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(reloaded.config_for(&app.config, &first).game_directory(), PathBuf::from(path));
    let _ = app.update(Message::AccountsLoaded(Ok(values)));
    assert_eq!(app.settings.account_installs[&first_key], path);
    let _ = app.update(Message::EditAccount(first_key.clone()));
    let _ = app.update(Message::AccountInstallChanged(String::new()));
    let values = app.accounts.iter().map(|r| r.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(values)));
    assert_eq!(app.settings.config_for(&app.config, &first).game_directory(), PathBuf::from(r"C:\Games\Retail"));
}

#[test]
fn account_key_edits_migrate_preferences_and_deletion_removes_them() {
    let mut app = app();
    let previous = app.accounts[0].key.clone();
    app.settings.selected_accounts = Some(BTreeSet::from([previous.clone(), "obsolete".into()]));
    app.settings.account_auto_away.insert(previous.clone(), 30);
    app.settings.account_auto_away.insert("obsolete".into(), 60);
    app.settings.account_installs.insert(previous.clone(), r"C:\Games\Account".into());
    app.settings.account_overlays.insert(previous.clone(), vec![r"C:\Mods\Account".into()]);
    app.settings.account_no_otp.insert(previous.clone());
    app.settings.account_order = vec![previous.clone(), app.accounts[1].key.clone(), "obsolete".into()];
    let auto_login = pol_less::AutoLogin { enabled: true, character_slot: 3 };
    app.settings.account_auto_login.insert(previous.clone(), auto_login);
    let _ = app.update(Message::EditAccount(previous.clone()));
    let _ = app.update(Message::EditorChanged(Field::PolId, "XYZ99999".into()));
    let edited = app.editor.as_ref().unwrap().account.clone();
    let key = account_key(&edited);
    let mut values: Vec<_> = app.accounts.iter().filter(|row| row.key != previous).map(|row| row.account.clone()).collect();
    values.push(edited);
    let _ = app.update(Message::AccountsSaved(Ok(values)));
    assert_eq!(app.settings.selected_accounts, Some(BTreeSet::from([key.clone()])));
    assert_eq!(app.settings.account_auto_away, HashMap::from([(key.clone(), 30)]));
    assert_eq!(app.settings.account_auto_login, HashMap::from([(key.clone(), auto_login)]));
    let reloaded: Settings = serde_json::from_slice(&serde_json::to_vec(&app.settings).unwrap()).unwrap();
    assert_eq!(reloaded.account_auto_login, app.settings.account_auto_login);
    assert_eq!(app.settings.account_installs.get(&key).map(String::as_str), Some(r"C:\Games\Account"));
    assert!(app.settings.account_overlays.contains_key(&key));
    assert!(!app.settings.account_no_otp.contains(&key), "a changed login learns its sign-ins again");
    assert_eq!(app.settings.account_order, [key.clone(), app.accounts.iter().find(|row| row.key != key).unwrap().key.clone()]);
    assert!(app.accounts.iter().find(|row| row.key == key).unwrap().selected);
    for exists in [app.settings.account_installs.contains_key(&previous), app.settings.account_overlays.contains_key(&previous),
        app.settings.account_no_otp.contains(&previous)] { assert!(!exists); }

    let values = app.accounts.iter().filter(|row| row.key != key).map(|row| row.account.clone()).collect();
    let _ = app.update(Message::AccountsSaved(Ok(values)));
    assert_eq!(app.settings.selected_accounts, Some(BTreeSet::new()));
    assert!(app.settings.account_auto_away.is_empty() && app.settings.account_installs.is_empty());
    assert!(app.settings.account_overlays.is_empty() && app.settings.account_no_otp.is_empty());
    assert!(app.settings.account_auto_login.is_empty());
    assert!(!app.settings.account_order.contains(&key));
}

#[test]
fn account_settings_failure_can_retry_after_the_vault_saved_a_new_key() {
    let root = std::env::var_os("CRYSTAL_DATA_ROOT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
        .join(format!("crystal-account-retry-{}", backend::unique_id().unwrap()));
    std::fs::create_dir_all(&root).unwrap();
    let blocker = root.join("iced-launcher");
    std::fs::write(&blocker, b"settings parent is temporarily unavailable").unwrap();
    let mut app = app();
    app.config = Config { experiment: root.clone(), repository: root.join("runtime"), game_install: None, dat_overlays: Vec::new() };
    let previous = app.accounts[0].key.clone();
    app.settings.selected_accounts = Some(BTreeSet::from([previous.clone()]));
    app.settings.account_auto_away.insert(previous.clone(), 30);
    let _ = app.update(Message::EditAccount(previous.clone()));
    let _ = app.update(Message::EditorChanged(Field::PolId, "XYZ99999".into()));
    let edited = app.editor.as_ref().unwrap().account.clone();
    let key = account_key(&edited);
    let mut values: Vec<_> = app.accounts.iter().filter(|row| row.key != previous).map(|row| row.account.clone()).collect();
    values.push(edited);
    app.preview = false;
    let _ = app.update(Message::AccountsSaved(Ok(values.clone())));
    assert_eq!(app.editor.as_ref().unwrap().key.as_ref(), Some(&key));
    assert_eq!(app.settings.account_auto_away.get(&key), Some(&30));
    assert!(app.status.contains("launcher settings were not saved"));

    std::fs::remove_file(blocker).unwrap();
    // Creating this Task does not execute the vault write or launch a request.
    assert_eq!(app.update(Message::SaveAccount).units(), 1, "the editor must recognize its saved identity");
    let _ = app.update(Message::AccountsSaved(Ok(values)));
    assert!(app.editor.is_none() && !app.vault_saving);
    let saved = load_settings(&app.config).unwrap();
    assert_eq!(saved.account_auto_away.get(&key), Some(&30));
    assert_eq!(saved.selected_accounts, Some(BTreeSet::from([key])));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_folder_dialog_does_not_change_a_different_account_editor() {
    let mut app = app();
    let first_key = app.accounts[0].key.clone();
    let second_key = app.accounts[1].key.clone();
    let _ = app.update(Message::EditAccount(second_key));
    let _ = app.update(Message::PickedAccountFolder(first_key.clone(), Some(PathBuf::from(r"C:\Wrong account"))));
    let _ = app.update(Message::PickedAccountOverlay(first_key, Some(PathBuf::from(r"C:\Wrong overlay"))));
    assert!(app.editor.as_ref().unwrap().game_install.is_empty());
    assert!(app.editor.as_ref().unwrap().dat_overlays.is_empty());
}
#[test]
fn closed_child_becomes_launchable_without_reusing_pid() {
    let mut app = app();
    app.accounts[0].pid = Some(99);
    app.accounts[0].alive = Some(Arc::new(AtomicBool::new(false)));
    let _ = app.update(Message::Tick);
    assert_eq!(app.accounts[0].pid, None);
    assert_eq!(app.launchable_count(), 3);
}
#[test]
fn malicious_and_non_http_links_cannot_open() {
    assert!(allowed_link("https://www.ffxiah.com/"));
    assert!(allowed_link("https://wiki.ffo.jp/"));
    assert!(allowed_link("https://www.playonline.com/ff11us/"));
    // The Japanese site's article and "View all news" links open too, and only on its host.
    assert!(allowed_link(services::sources(i18n::Lang::Ja).home));
    assert!(allowed_link("https://www.playonline.com/pcd2/topics/ff11/detail/40431/detail.html"));
    assert!(allowed_link("https://www.playonline.com/ff11/polnews/news27774.shtml"));
    assert!(allowed_link("https://sqex.to/1Twec"));
    // Crystal's own Discord invite, and nothing else on that site.
    assert!(allowed_link(DISCORD_INVITE));
    assert_eq!(DISCORD_INVITE, "https://discord.gg/b344cmuy7y");
    for url in [
        "https://discord.gg/",
        "https://discord.gg/another",
        "https://discord.gg/b344cmuy7y/x",
        "http://discord.gg/b344cmuy7y",
        "https://discord.com/invite/b344cmuy7y",
        "https://www.playonline.com.evil.test/ff11/",
        "https://evil.test/pcd2/topics/ff11/detail/40431/detail.html",
        "http://www.playonline.com/ff11/",
        "file:///C:/Windows/System32/cmd.exe",
        "https://www.playonline.com.evil.test/",
        "https://x@www.ffxiah.com/",
        "https://wiki.ffo.jp.evil.test/",
        "http://wiki.ffo.jp/",
        "https://user@wiki.ffo.jp/",
        "javascript:alert(1)",
    ] {
        assert!(!allowed_link(url));
    }
}

/// Live 2026-11-13: three accounts were switched from Ashita to Windower while their Ashita
/// sessions were signed in. Each Play was refused by the session ("before switching launch
/// methods"), but the refusal sank the whole batch and no row said why. The refused row now
/// says so, keeps no start flag, and can play again once signed out.
#[test]
fn a_play_refused_for_a_changed_launcher_flags_nothing_and_says_so() {
    crate::i18n::set(crate::i18n::Lang::En);
    let (mut app, root) = play_update_app("changed-launcher", None);
    app.launches = None;
    let key = idle_signed_in(&mut app, 0, 301);
    let config = app.settings.config_for(&app.config, &app.accounts[0].account);
    session_worker::save_live_record_with_tool_for_test(&config, &app.accounts[0].account, pol_less::Tool::Ashita);
    app.accounts[0].account.launch_with = Some(backend::LaunchWith::Windower);
    let _ = app.update(Message::Launch(key.clone()));
    let row = &app.accounts[0];
    assert!(row.problem && !row.busy && !row.batch_pending && !row.batch_requested && row.queued_play.is_none(), "{}", row.status);
    assert!(row.can_play(), "nothing holds the row");
    assert!(row.status.contains("before switching launch methods"), "{}", row.status);
    assert!(app.status.contains("before switching launch methods"), "{}", app.status);
    let view = crate::account_row::row_view(&app, &app.accounts[0]);
    assert!(view.detail.contains("before switching launch methods"), "{}", view.detail);
    assert!(matches!(view.action, Some((_, Message::Launch(_), true))), "Play is still offered");
    std::fs::remove_dir_all(root).unwrap();
}

/// One refused account in a multi-account Play leaves the others to start together.
#[test]
fn one_refused_account_does_not_stop_the_others() {
    let (mut app, root) = play_update_app("one-refused", None);
    let count = app.accounts.len();
    assert!(count >= 3, "{count}");
    let mut keys = Vec::new();
    for index in 0..count {
        keys.push(idle_signed_in(&mut app, index, 310 + index as u32));
        app.accounts[index].account.launch_with = Some(backend::LaunchWith::Ashita);
        let config = app.settings.config_for(&app.config, &app.accounts[index].account);
        session_worker::save_live_record_with_tool_for_test(&config, &app.accounts[index].account, pol_less::Tool::Ashita);
    }
    app.accounts[1].account.launch_with = Some(backend::LaunchWith::Windower);
    let left = app.refuse_changed_launchers(keys.clone());
    let expected: Vec<String> = keys.iter().enumerate().filter(|(index, _)| *index != 1).map(|(_, key)| key.clone()).collect();
    assert_eq!(left, expected);
    assert!(app.accounts[1].problem && app.accounts.iter().enumerate().all(|(index, row)| index == 1 || !row.problem));
    assert_eq!(app.coordinated_keys(&left).len(), count - 1, "the others still go as one batch");
    std::fs::remove_dir_all(root).unwrap();
}