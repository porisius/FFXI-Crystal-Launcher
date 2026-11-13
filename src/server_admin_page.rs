//! Settings → Server: the admin side of a LandSandBoat server (`server_admin`), its state and
//! messages. `slint_host::settings` draws it (`ui/settings_server.slint`).
//!
//! Shown to everyone, but does nothing until a server's admin token is entered. Its words are
//! English, like the rest of Crystal's private-server text.

use crate::effect::Effect;
use crate::server_admin::{self, GateChange, Login, Status};
use crate::{App, Message};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroize;

/// A verification manifest being built: bytes hashed of the total, read by the page as it draws.
#[derive(Debug, Default)]
pub(crate) struct Hashing {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancelled: AtomicBool,
}

#[derive(Clone, Debug)]
pub(crate) enum VerifyJob {
    Idle,
    Hashing { id: u64, progress: Arc<Hashing> },
    Uploading(String),
}

pub(crate) struct State {
    pub login: Option<Login>,
    /// The server and token fields as typed.
    pub host: String,
    pub token: String,
    pub status: Option<Status>,
    pub loading: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub motd: String,
    pub verify: VerifyJob,
    next_id: u64,
    /// The saved login was read (once, when the tab first shows).
    loaded: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            login: None,
            host: String::new(),
            token: String::new(),
            status: None,
            loading: false,
            error: None,
            notice: None,
            motd: String::new(),
            verify: VerifyJob::Idle,
            next_id: 1,
            loaded: false,
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl State {
    /// Work is in flight (the window polls fast while it is).
    pub(crate) fn busy(&self) -> bool {
        self.loading || !matches!(self.verify, VerifyJob::Idle)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Msg {
    /// The tab is shown: read the saved login and the server's status the first time.
    Opened,
    HostInput(String),
    TokenInput(String),
    Connect,
    Forget,
    Refresh,
    Loaded(Result<Status, String>),
    SetUpdateApproved(bool),
    SetMaintenance(bool),
    MotdInput(String),
    SaveMotd,
    ClearOverrides,
    /// `"promote"` a staged version, or `"rollback"` (newest archived when `None`).
    Xiloader(&'static str, Option<String>),
    XiloaderQueued(Result<(), String>),
    GenerateVerify,
    CancelVerify,
    VerifyBuilt(u64, Result<(String, Vec<u8>), String>),
    VerifyUploaded(Result<String, String>),
}

fn sa(msg: Msg) -> Message {
    Message::ServerAdmin(msg)
}

fn fetch(login: &Login) -> Effect<Message> {
    Effect::perform(server_admin::fetch_status(login.clone()), |result| sa(Msg::Loaded(result)))
}

/// This PC's copy of the server's manifest follows a change at once: it holds this PC's
/// FFXI updates too (`server_manifest::update_hold`).
fn refresh_manifests(app: &App) -> Effect<Message> {
    let experiment = app.config.experiment.clone();
    Effect::future(async move {
        crate::server_manifest::refresh_known(experiment).await;
        Effect::none()
    })
}

fn change(state: &mut State, change: GateChange) -> Effect<Message> {
    let Some(login) = state.login.clone() else { return Effect::none() };
    state.loading = true;
    state.notice = None;
    state.error = None;
    Effect::perform(server_admin::change_gate(login, change), |result| sa(Msg::Loaded(result)))
}

/// The FFXI folder a verification manifest is built from: the one selected in Crystal.
pub(crate) fn game_folder(app: &App) -> PathBuf {
    if app.settings.install_path.trim().is_empty() {
        app.config.game_directory()
    } else {
        crate::backend::plain_windows_path(std::path::Path::new(&app.settings.install_path))
    }
}

pub(crate) fn update(app: &mut App, msg: Msg) -> Effect<Message> {
    if app.preview && !matches!(msg, Msg::HostInput(_) | Msg::TokenInput(_) | Msg::MotdInput(_)) {
        return Effect::none();
    }
    match msg {
        Msg::Opened => {
            let state = &mut app.server_admin;
            if state.loaded { return Effect::none(); }
            state.loaded = true;
            // Unit tests never read the real data folder.
            if cfg!(test) { return Effect::none(); }
            if let Some(login) = server_admin::load_login(&app.config) {
                state.host = login.host.clone();
                state.loading = true;
                let effect = fetch(&login);
                state.login = Some(login);
                return effect;
            }
        }
        Msg::HostInput(value) => app.server_admin.host = value.trim().to_owned(),
        Msg::TokenInput(mut value) => {
            app.server_admin.token.zeroize();
            app.server_admin.token = value.trim().to_owned();
            value.zeroize();
        }
        Msg::Connect => {
            let state = &mut app.server_admin;
            if state.loading { return Effect::none(); }
            let token = if state.token.is_empty() {
                state.login.as_ref().map(|login| login.token.clone()).unwrap_or_default()
            } else {
                std::mem::take(&mut state.token)
            };
            if let Err(error) = server_admin::admin_url(&state.host, "status") {
                state.error = Some(error);
                return Effect::none();
            }
            if token.len() < 32 {
                state.error = Some("Enter the admin token from launcher.ADMIN_TOKEN on the server (32 characters or more).".into());
                return Effect::none();
            }
            let login = Login { host: state.host.clone(), token };
            if let Err(error) = server_admin::save_login(&app.config, &login) {
                state.error = Some(error);
                return Effect::none();
            }
            state.loading = true;
            state.error = None;
            state.notice = None;
            let effect = fetch(&login);
            state.login = Some(login);
            return effect;
        }
        Msg::Forget => {
            server_admin::forget_login(&app.config);
            if let VerifyJob::Hashing { progress, .. } = &app.server_admin.verify {
                progress.cancelled.store(true, Ordering::Relaxed);
            }
            let mut fresh = State::default();
            fresh.loaded = true;
            app.server_admin = fresh;
        }
        Msg::Refresh => {
            let state = &mut app.server_admin;
            if let Some(login) = state.login.clone().filter(|_| !state.loading) {
                state.loading = true;
                state.notice = None;
                return fetch(&login);
            }
        }
        Msg::Loaded(result) => {
            let state = &mut app.server_admin;
            state.loading = false;
            match result {
                Ok(status) => {
                    state.motd = status.gate.motd.clone();
                    state.status = Some(status);
                    state.error = None;
                    return refresh_manifests(app);
                }
                Err(error) => state.error = Some(error),
            }
        }
        Msg::SetUpdateApproved(on) => {
            return change(&mut app.server_admin, GateChange { update_approved: Some(Some(on)), ..Default::default() });
        }
        Msg::SetMaintenance(on) => {
            return change(&mut app.server_admin, GateChange { maintenance: Some(Some(on)), ..Default::default() });
        }
        Msg::MotdInput(value) => app.server_admin.motd = value,
        Msg::SaveMotd => {
            let motd = app.server_admin.motd.trim().to_owned();
            return change(&mut app.server_admin, GateChange { motd: Some(Some(motd)), ..Default::default() });
        }
        Msg::ClearOverrides => {
            return change(&mut app.server_admin, GateChange { update_approved: Some(None), maintenance: Some(None), motd: Some(None) });
        }
        Msg::Xiloader(action, version) => {
            let state = &mut app.server_admin;
            let Some(login) = state.login.clone().filter(|_| !state.loading) else { return Effect::none() };
            state.loading = true;
            state.notice = None;
            state.error = None;
            return Effect::perform(server_admin::xiloader_request(login, action, version), |result| sa(Msg::XiloaderQueued(result)));
        }
        Msg::XiloaderQueued(result) => {
            let state = &mut app.server_admin;
            state.loading = false;
            match result {
                Ok(()) => {
                    state.notice = Some("Sent. The server switches xiloader within a few seconds.".into());
                    if let Some(login) = state.login.clone() {
                        // The request runs on the server: its result shows once it has had a moment.
                        state.loading = true;
                        return Effect::perform(
                            async move {
                                tokio::time::sleep(Duration::from_secs(4)).await;
                                server_admin::fetch_status(login).await
                            },
                            |result| sa(Msg::Loaded(result)),
                        );
                    }
                }
                Err(error) => state.error = Some(error),
            }
        }
        Msg::GenerateVerify => {
            let game = game_folder(app);
            let state = &mut app.server_admin;
            if !matches!(state.verify, VerifyJob::Idle) || state.login.is_none() { return Effect::none(); }
            let Some(expected) = state.status.as_ref().map(|status| status.client_version.clone()) else { return Effect::none() };
            state.error = None;
            state.notice = None;
            // The file is named for the version the server asks for; one built from another
            // version would never be read.
            let installed = server_admin::installed_client_version(&game);
            if installed.as_deref() != Some(expected.as_str()) {
                state.error = Some(match installed {
                    Some(installed) => format!("This PC's FINAL FANTASY XI is {installed}, but the server expects {expected}. Build it from a folder updated to exactly {expected}."),
                    None => format!("Could not tell which FINAL FANTASY XI version {} has. Choose the game folder in Downloads.", game.display()),
                });
                return Effect::none();
            }
            let id = state.next_id;
            state.next_id += 1;
            let progress = Arc::new(Hashing::default());
            state.verify = VerifyJob::Hashing { id, progress: progress.clone() };
            return Effect::perform(
                async move {
                    tokio::task::spawn_blocking(move || {
                        server_admin::generate_verify(&game, &expected, |done, total| {
                            progress.done.store(done, Ordering::Relaxed);
                            progress.total.store(total, Ordering::Relaxed);
                            // false stops the build.
                            !progress.cancelled.load(Ordering::Relaxed)
                        })
                    })
                    .await
                    .unwrap_or_else(|_| Err("Building the verification manifest stopped unexpectedly.".into()))
                },
                move |result| sa(Msg::VerifyBuilt(id, result)),
            );
        }
        Msg::CancelVerify => {
            if let VerifyJob::Hashing { progress, .. } = &app.server_admin.verify {
                progress.cancelled.store(true, Ordering::Relaxed);
            }
        }
        Msg::VerifyBuilt(id, result) => {
            let state = &mut app.server_admin;
            if !matches!(state.verify, VerifyJob::Hashing { id: running, .. } if running == id) {
                return Effect::none();
            }
            match (result, state.login.clone()) {
                (Ok((version, json)), Some(login)) => {
                    state.verify = VerifyJob::Uploading(version.clone());
                    return Effect::perform(server_admin::upload_verify(login, version.clone(), json), move |result| {
                        sa(Msg::VerifyUploaded(result.map(|()| version)))
                    });
                }
                (Err(error), _) => {
                    state.verify = VerifyJob::Idle;
                    state.error = Some(error);
                }
                (Ok(_), None) => state.verify = VerifyJob::Idle,
            }
        }
        Msg::VerifyUploaded(result) => {
            let state = &mut app.server_admin;
            state.verify = VerifyJob::Idle;
            match result {
                Ok(version) => {
                    state.notice = Some(format!("Uploaded the verification manifest for {version}."));
                    if let Some(login) = state.login.clone() {
                        state.loading = true;
                        return fetch(&login);
                    }
                }
                Err(error) => state.error = Some(error),
            }
        }
    }
    Effect::none()
}

/// The first 12 characters of a hash, for display.
pub(crate) fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

/// What the page shows, in its own words (`slint_host::settings::server`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Front {
    pub connected: bool,
    pub busy: bool,
    pub error: String,
    pub notice: String,
    pub has_status: bool,
    pub online: String,
    pub versions: String,
    pub approved: bool,
    pub maintenance: bool,
    pub maint_locked: bool,
    pub motd_saved: String,
    pub overridden: String,
    pub live: String,
    pub staged: Vec<String>,
    pub can_request: bool,
    pub has_archived: bool,
    pub pending: bool,
    pub held: String,
    pub last_result: String,
    pub last_ok: bool,
    pub verify_present: bool,
    pub verify_chip: String,
    pub game_folder: String,
    pub verify_running: bool,
    pub verify_line: String,
    pub verify_fraction: f32,
    pub verify_action: String,
}

pub(crate) fn front(app: &App) -> Front {
    let state = &app.server_admin;
    let mut front = Front {
        connected: state.login.is_some(),
        busy: state.loading,
        error: state.error.clone().unwrap_or_default(),
        notice: state.notice.clone().unwrap_or_default(),
        game_folder: crate::privacy::mask_path(&game_folder(app).display().to_string()),
        ..Front::default()
    };
    match &state.verify {
        VerifyJob::Idle => {}
        VerifyJob::Hashing { progress, .. } => {
            let (done, total) = (progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
            front.verify_running = true;
            front.verify_fraction = if total == 0 { 0.0 } else { (done as f64 / total as f64) as f32 };
            front.verify_line = if progress.cancelled.load(Ordering::Relaxed) {
                "Stopping…".into()
            } else {
                format!("Hashing game files… {}% (several minutes for a full install)", (front.verify_fraction * 100.0) as u32)
            };
        }
        VerifyJob::Uploading(version) => {
            front.verify_running = true;
            front.verify_fraction = 1.0;
            front.verify_line = format!("Uploading {version}…");
        }
    }
    let Some(status) = &state.status else { return front };
    front.has_status = true;
    front.online = format!("{} online", status.players_online);
    front.versions = format!("Client {} · LSB {}", status.client_version, status.lsb_commit);
    let gate = &status.gate;
    front.approved = gate.update_approved;
    front.maintenance = gate.maintenance;
    front.maint_locked = gate.login_maint_mode;
    front.motd_saved = gate.motd.clone();
    if !gate.overridden.is_empty() {
        front.overridden = format!("Set from here: {}. Clearing goes back to settings/launcher.lua.", gate.overridden.join(", "));
    }
    let loader = &status.xiloader;
    front.live = match &loader.live {
        Some(live) => format!("Live: {} ({})", if live.version.is_empty() { "unknown version" } else { &live.version }, short(&live.sha256)),
        None => "Live: none (the server can't read its xiloader)".to_owned(),
    };
    front.staged = loader.staged.clone();
    front.can_request = !state.loading && !loader.request_pending;
    front.has_archived = !loader.archived.is_empty();
    front.pending = loader.request_pending;
    front.held = loader.held.as_ref().map(|held| format!("Held back after a rollback: {held} and older.")).unwrap_or_default();
    if let Some(result) = &loader.last_result {
        front.last_result = format!("Last request: {}", result.message);
        front.last_ok = result.ok;
    }
    front.verify_present = status.verify.current_present;
    front.verify_chip = format!("{} {}", status.client_version, if front.verify_present { "ready" } else { "missing" });
    front.verify_action = if front.verify_present { "Build and upload again" } else { "Build and upload" }.into();
    front
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> App {
        let mut app = crate::tests::app();
        app.preview = false;
        app.server_admin.login = Some(Login { host: "ffxi.example.test".into(), token: "x".repeat(32) });
        app
    }

    fn status() -> Status {
        serde_json::from_str(r#"{
            "playersOnline": 7, "clientVersion": "30261006_0", "lsbCommit": "131cf71ed6",
            "gate": {"updateApproved": false, "maintenance": true, "loginMaintMode": false, "motd": "Patch day", "overridden": ["updateApproved"]},
            "xiloader": {"live": {"version": "3.0.0", "sha256": "68A1DE4112046AFF9F0E"}, "staged": ["3.0.1"], "archived": ["2.2.0"],
                         "requestPending": false, "lastResult": {"ok": false, "message": "no such version"}},
            "verify": {"currentPresent": false, "versions": []}
        }"#).unwrap()
    }

    #[test]
    fn hashes_are_shortened_for_display() {
        assert_eq!(short("68A1DE4112046AFF9F0E"), "68A1DE411204");
        assert_eq!(short("ab"), "ab");
    }

    #[test]
    fn the_page_says_nothing_until_connected_then_shows_the_servers_state() {
        let mut app = crate::tests::app();
        app.preview = false;
        let view = front(&app);
        assert!(!view.connected && !view.has_status);
        app = connected();
        let _ = update(&mut app, Msg::Loaded(Ok(status())));
        let front = front(&app);
        assert!(front.connected && front.has_status && !front.busy);
        assert_eq!(front.online, "7 online");
        assert_eq!(front.versions, "Client 30261006_0 · LSB 131cf71ed6");
        assert!(!front.approved && front.maintenance && !front.maint_locked);
        assert_eq!(front.motd_saved, "Patch day");
        assert_eq!(app.server_admin.motd, "Patch day", "the message field starts from the server's");
        assert!(front.overridden.contains("updateApproved"));
        assert_eq!(front.live, "Live: 3.0.0 (68A1DE411204)");
        assert_eq!(front.staged, ["3.0.1"]);
        assert!(front.can_request && front.has_archived && !front.pending);
        assert_eq!(front.last_result, "Last request: no such version");
        assert!(!front.last_ok);
        assert_eq!(front.verify_chip, "30261006_0 missing");
    }

    #[test]
    fn a_short_token_or_a_bad_host_is_refused_before_anything_is_saved() {
        let mut app = crate::tests::app();
        app.preview = false;
        let _ = update(&mut app, Msg::HostInput(" https://ffxi.example.test ".into()));
        let _ = update(&mut app, Msg::TokenInput("y".repeat(40)));
        let _ = update(&mut app, Msg::Connect);
        assert!(app.server_admin.error.as_deref().unwrap().contains("host name"));
        let _ = update(&mut app, Msg::HostInput("ffxi.example.test".into()));
        let _ = update(&mut app, Msg::TokenInput("short".into()));
        let _ = update(&mut app, Msg::Connect);
        assert!(app.server_admin.error.as_deref().unwrap().contains("32 characters"));
        assert!(app.server_admin.login.is_none() && !app.server_admin.loading);
    }

    #[test]
    fn a_failed_read_keeps_the_last_state_and_says_why() {
        let mut app = connected();
        let _ = update(&mut app, Msg::Loaded(Ok(status())));
        let _ = update(&mut app, Msg::Loaded(Err("Could not reach ffxi.example.test.".into())));
        let view = front(&app);
        assert!(view.has_status);
        assert_eq!(view.error, "Could not reach ffxi.example.test.");
    }

    #[test]
    fn a_verify_build_needs_the_version_the_server_expects() {
        let mut app = connected();
        let _ = update(&mut app, Msg::Loaded(Ok(status())));
        app.settings.install_path = std::env::temp_dir().join("crystal-no-ffxi-here").to_string_lossy().into_owned();
        let _ = update(&mut app, Msg::GenerateVerify);
        assert!(matches!(app.server_admin.verify, VerifyJob::Idle));
        assert!(app.server_admin.error.as_deref().unwrap().contains("Could not tell"), "{:?}", app.server_admin.error);
    }

    #[test]
    fn an_old_build_result_is_ignored_and_progress_shows() {
        let mut app = connected();
        let progress = Arc::new(Hashing::default());
        progress.done.store(25, Ordering::Relaxed);
        progress.total.store(100, Ordering::Relaxed);
        app.server_admin.verify = VerifyJob::Hashing { id: 2, progress };
        let view = front(&app);
        assert!(view.verify_running && view.verify_line.contains("25%"));
        let _ = update(&mut app, Msg::VerifyBuilt(1, Err("old".into())));
        assert!(matches!(app.server_admin.verify, VerifyJob::Hashing { id: 2, .. }));
        let _ = update(&mut app, Msg::CancelVerify);
        assert!(front(&app).verify_line.starts_with("Stopping"));
        let _ = update(&mut app, Msg::VerifyBuilt(2, Err("Stopped.".into())));
        assert!(matches!(app.server_admin.verify, VerifyJob::Idle));
        assert_eq!(app.server_admin.error.as_deref(), Some("Stopped."));
    }
}
