#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("Crystal Launcher is Windows-only");

mod backend;
mod account_import;
// The launcher <-> loader wire contract, shared with crystal-loader source.
#[path = "../crystal-loader/src/b64.rs"]
#[allow(dead_code)]
mod b64;
#[path = "../crystal-loader/src/wire.rs"]
#[allow(dead_code)]
mod wire;
mod account_import_ui;
mod account_order;
mod account_row;
mod discovery;
mod dns;
mod effect;
mod controller_nav;
mod dgvoodoo;
mod feature_check;
mod features;
mod reshade;
mod friends;
mod social;
mod game_config;
mod ffxiah;
mod footprint;
mod fsutil;
mod gamepad;
mod i18n;
mod installer;
mod launch_log;
mod launch_timing;
mod launch_tools;
mod open_with_game;
mod launcher_close;
mod prereqs;
mod settings_page;
mod official_versions;
mod operation;
mod palette;
mod patch;
mod patch_watch;
mod play_flag;
mod play_update;
mod pol_less;
mod portraits;
mod preview;
mod privacy;
mod private_server;
mod retail_region;
mod services;
mod self_update;
mod server_manifest;
mod session_worker;
mod launcher_change;
mod start_watch;
mod startup_prefetch;
mod startup_task;
mod stock_socket;
mod tick_gate;
mod totp;
mod tray;
mod runtime;
mod slint_host;
mod updater;
mod viewer;
mod win;
mod window_chrome;
mod host;
mod instance;
mod settings;
mod shell;

use backend::{account_key, Account, AccountMode, Config, LaunchResult};
use i18n::{t, tf};

use effect::{Edge, Effect, Screenshot, Size, WindowId, WindowOp};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};
use host::{host_running, process_created, session_loader, HostId};
use instance::claim_instance;
use settings::{load_settings, load_settings_and_language, plain_text_path, save_settings, Settings};
use shell::{open_log_path, open_url, shell_open};
#[cfg(test)]
use host::RUNNING_HOSTS;
#[cfg(test)]
use settings::settings_path;
#[cfg(test)]
use shell::allowed_link;

#[derive(Clone, Copy)]
pub(crate) enum Field {
    Label,
    Server,
    PolId,
    PolPassword,
    SeId,
    SePassword,
    PolHandleNames,
    TotpKey,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Page {
    Home,
    Friends,
    Maintenance,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MaintenanceKind {
    Check,
    Update,
    Repair,
    Download,
    Extract,
    Setup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MainAction {
    Play,
    Maintenance,
    Offline,
    Working,
}
impl MaintenanceKind {
    pub fn blocks_launch(self) -> bool {
        matches!(self, Self::Update | Self::Repair | Self::Setup)
    }
    pub fn cancellable(self) -> bool {
        !matches!(self, Self::Extract | Self::Setup)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Check => t("Checking files"),
            Self::Update => t("Updating PlayOnline and FFXI"),
            Self::Repair => t("Repairing files"),
            Self::Download => t("Downloading official installer"),
            Self::Extract => t("Extracting official installer"),
            Self::Setup => t("Official setup is running"),
        }
    }
}

/// What the Downloads page's "Last result" line reports, kept apart from its
/// (translated) text so the page never has to match words in a language.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum ResultTone {
    #[default]
    Done,
    /// Nothing has been checked yet.
    Unverified,
    /// The operation failed or was cancelled.
    Stopped,
}

pub(crate) struct AccountRow {
    pub key: String,
    pub account: Account,
    pub selected: bool,
    pub otp: String,
    pub busy: bool,
    signing_out: bool,
    pub shutting_down: bool,
    /// How the shut-down in flight (`shutting_down`) ends the game.
    shutdown_mode: session_worker::game_shutdown::Mode,
    /// Background sign-in can accept Play without starting another login.
    preparing_session: bool,
    /// The current sign-in thread's completion flag, set just before its Complete is sent:
    /// launcher close stops waiting for a sign-in that ended before any worker was seen.
    sign_in_done: Option<Arc<AtomicBool>>,
    queued_play: Option<Instant>,
    /// A coordinated Play owns this row even if an older startup sign-in finishes.
    batch_pending: bool,
    /// This window is awaiting its batch callback; ownership may not be published yet.
    batch_requested: bool,
    launch_generation: u64,
    pub status: String,
    /// The note shown after `status` while `status` is still `note_on` (`status_text`).
    note: Option<RowNote>,
    note_on: String,
    pub pid: Option<u32>,
    /// `pid`'s creation time, read when the session's result arrived (`HostId`).
    host_created: Option<u64>,
    alive: Option<Arc<AtomicBool>>,
    game_running: Option<Arc<AtomicBool>>,
    /// Launch diagnostics: the client's stderr log, how it ended, and whether that was a problem.
    pub log: Option<PathBuf>,
    outcome: Option<Arc<std::sync::Mutex<Option<launch_log::ExitReport>>>>,
    pub problem: bool,
    /// How long the running launch took, e.g. "Signed in 2.8s · game started 9.4s".
    pub timing: String,
    /// A Play from an already authenticated session, measured separately from sign-in.
    play_timing: Option<launch_timing::Timeline>,
    /// How the last client ended, while `status` still holds its (English) summary.
    exit: Option<launch_log::ExitReport>,
    /// What started the sign-in in flight (pol-spec 07d §4.2), for its failure handling.
    launch_trigger: Option<launch_timing::LaunchTrigger>,
    /// A sign-in that failed on the network starts again once DNS answers (07d §5.4-5.5).
    network_wait: Option<NetworkWait>,
    /// The sign-in in flight (or last) took the row's one-time code, which is now spent: no
    /// automatic second sign-in may follow it, since it would go without a code and be refused
    /// (07d §5.4) until the user types a new one.
    otp_spent: bool,
    /// The row's session came from an earlier window's worker whose record does not say whether
    /// its sign-in took a code (workers before the field existed write none), so `otp_spent` is
    /// a guess: nothing that session does teaches that the account signs in without one.
    otp_unknown: bool,
    /// The sign-in in flight (or last) is one Crystal started by itself without a code
    /// (`LaunchTrigger::is_presign`), also when a Play has since queued on it: only such a
    /// sign-in's refusal marks the account as one that may need a code (owner decision
    /// 2026-10-13). A user's own Play or Connect never changes what Crystal knows that way.
    presign_login: bool,
    /// PlayOnline refused a code made from the account's authenticator key (no code typed): the
    /// key is likely wrong. Until a sign-in without a typed code works or the key is edited, the
    /// row asks for a typed code and Crystal signs nothing in by itself, so a wrong key never
    /// costs a second refused login.
    authenticator_refused: bool,
    /// The running session's game was seen running: its end is a game end.
    game_seen: bool,
    /// The user signed out or edited the account during the running session: its end signs
    /// nothing back in.
    resign_declined: bool,
    /// An explicit sign-out, or a game closed without automatic sign-in: keep
    /// this account free of both signed-in and parked hosts until the next Play.
    signed_out: bool,
    /// A sign-in after the game closed, waiting for its session to end fully.
    resign: Option<Resign>,
}

/// A session whose game ran has ended: its row's key, how the game ended, and its host.
type GameEnd = (String, Option<launch_log::ExitKind>, Option<HostId>);

/// An optional fresh sign-in after the game and its previous POL host close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Resign {
    /// The row's launch generation at the game's end: a launch since (Play, Connect) wins.
    generation: u64,
    /// When the game's end was seen.
    ended: Instant,
    /// Since when the row has been free (host exited, sign-out done, worker record closed).
    free_since: Option<Instant>,
    /// An unclean end: the connectivity probe must see the network first.
    network: bool,
    /// The ended session's host (loader). Its worker's record closing is not enough: a
    /// pipe failure ends the session while a hung loader may still hold its polcore session.
    host: Option<HostId>,
}

/// Cooldown after both the previous worker and host have ended. Keep the
/// three-second minimum for automatic re-sign-in; explicit Play starts through
/// its ordinary launch path. Native authentication has its own pacing.
const RESIGN_DELAY: Duration = Duration::from_secs(3);

/// What a row does when its connectivity probe succeeds (pol-spec 07d §5.4-5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NetworkWait {
    /// Re-issue the Play queued behind a failed pre-sign-in, keeping its click time.
    Reissue(Instant),
    /// The one retry of a startup sign-in.
    StartupRetry,
    /// A game ended uncleanly: its sign-in after the game waits for the network first.
    AfterGame,
    /// The one retry of a sign-in after a game.
    AfterGameRetry,
}

/// A Play clicked before this window's first status read finished (owner, 2026-10-13: Play
/// at 05:07:40 UTC, 19 s into Crystal's start, went through during All Worlds Maintenance).
struct HeldPlay {
    keys: Vec<String>,
    clicked: Instant,
    trigger: Option<launch_timing::LaunchTrigger>,
    since: Instant,
    /// What each held row and the status line said before the hold, restored at release.
    rows: Vec<(String, String)>,
    status: String,
}

/// The longest a Play waits for the first status read. A read that fails or takes longer
/// establishes nothing, and the Play proceeds as without one.
const STATUS_HOLD_LIMIT: Duration = Duration::from_secs(10);

/// A Play that found FFXI down (owner decision 2026-10-13, "Wait and play"): instead of being
/// refused, it starts as soon as FFXI is back (`release_waiting_plays`). Accounts clicked together
/// wait, and start, together. Waits live in this window only and are never saved: a Crystal
/// restart drops them, and the user presses Play again.
struct WaitingPlay {
    keys: Vec<String>,
    /// Accounts whose one-time code was typed for this Play: they sign in with a code.
    otp: Vec<String>,
    /// When the wait began.
    armed: Instant,
    /// The codes typed for this Play, taken off the rows. A release within OTP_KEEP of `armed`
    /// (the immediate flag read, when the official status lags behind a flag already at 1)
    /// still uses them; after that they are stale and dropped (`expire_wait_codes`), and the
    /// release asks the user for a new one.
    codes: Vec<(String, Zeroizing<String>)>,
}

/// How long a one-time code typed for a Play that then waits stays usable for that Play.
const OTP_KEEP: Duration = Duration::from_secs(20);

/// A sign-in its W2U stopped because PlayOnline needs an update (stock updates the Viewer at
/// Connect, before any credentials go out): it starts again once PlayOnline is updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefusedSignIn {
    pub key: String,
    /// The Play's click; for a sign-in only, when it was stopped.
    clicked: Instant,
    /// `None`: a Play, which goes on to start the game. Otherwise a sign-in only, by what
    /// started it (startup, after a game, maintenance, Connect).
    sign_in: Option<launch_timing::LaunchTrigger>,
}

/// How long a W2U's verdict stands in for the update's own version request. Stock updates
/// right after its W2U; a later update (an answer to "Sign out and update" left waiting) asks
/// again, as a new Connect would.
const VIEWER_VERDICT_FRESH: Duration = Duration::from_secs(600);
/// How often a Tick looks whether the PlayOnline update sign-ins wait for can start.
const VIEWER_TEND_EVERY: Duration = Duration::from_secs(1);

/// How long a startup sign-in waits for a still-running session of its account to end
/// before it is skipped (07d §5.5: the previous game's host may take seconds to close).
const STARTUP_WAIT_LIMIT: Duration = Duration::from_secs(60);
impl Drop for AccountRow {
    fn drop(&mut self) {
        self.otp.zeroize();
    }
}
/// Whether `row`'s next sign-in waits for a code typed on it: the account is known to take
/// one-time codes, or its last typed code was spent, and no authenticator key fills them in;
/// or PlayOnline refused its key's code (another would be refused too).
fn wants_typed_code(settings: &Settings, row: &AccountRow) -> bool {
    row.authenticator_refused
        || !row.authenticator_ready() && (row.otp_spent || settings.account_otp.contains(&row.key))
}

impl AccountRow {
    /// The account's one-time codes come from its saved authenticator key, which PlayOnline has
    /// not refused: none of its sign-ins waits for a typed code.
    pub(crate) fn authenticator_ready(&self) -> bool {
        self.account.mode == AccountMode::Retail && !self.account.totp_key.is_empty() && !self.authenticator_refused
    }
    pub(crate) fn is_playing(&self) -> bool {
        self.pid.is_some() && self.game_running.as_ref().is_none_or(|g| g.load(Ordering::Acquire))
    }
    pub(crate) fn can_play(&self) -> bool {
        !self.shutting_down && !self.signing_out && !self.batch_pending && !self.is_playing() && self.queued_play.is_none() && (!self.busy || self.preparing_session)
    }
    /// A running retail game that a shut-down can still end (not already closing).
    pub(crate) fn can_shut_down(&self) -> bool {
        self.account.mode == AccountMode::Retail && self.is_playing() && !self.shutting_down && !self.signing_out && !self.busy
    }
    fn queued_signin_play(&self) -> bool {
        self.busy && self.preparing_session && self.queued_play.is_some() && !self.batch_pending && !self.is_playing()
    }
    pub(crate) fn game_start_pending(&self) -> bool {
        !self.signing_out && (self.batch_pending || self.queued_play.is_some() || self.busy && !self.preparing_session)
    }
    pub(crate) fn can_sign_out(&self) -> bool {
        self.pid.is_some() && !self.shutting_down && !self.signing_out && !self.busy && !self.is_playing()
            && !self.batch_pending && self.queued_play.is_none()
    }
    /// The status line to show. A client's exit report and the row's note are worded in the
    /// current language here; any later status replaces both.
    pub(crate) fn status_text(&self) -> std::borrow::Cow<'_, str> {
        if self.shutting_down {
            return match self.shutdown_mode {
                session_worker::game_shutdown::Mode::Graceful => t("Shutting down FFXI…"),
                session_worker::game_shutdown::Mode::Instant => t("Closing FFXI…"),
            }.into();
        }
        if self.signing_out { return t("Signing out of PlayOnline…").into(); }
        let status: std::borrow::Cow<'_, str> = match &self.exit {
            Some(report) if report.summary == self.status => report.message().into(),
            _ => self.status.as_str().into(),
        };
        match self.shown_note() {
            Some(note) => format!("{status} · {}", note.text()).into(),
            None => status,
        }
    }
    /// The note after the status, while the status it was added to still shows.
    fn shown_note(&self) -> Option<&RowNote> {
        self.note.as_ref().filter(|_| self.note_on == self.status)
    }
    /// Put `note` after the current status (`None`: none).
    fn set_note(&mut self, note: Option<RowNote>) {
        self.note_on.clone_from(&self.status);
        self.note = note;
    }
    /// Show how the client ended.
    fn set_exit(&mut self, report: launch_log::ExitReport) {
        self.problem = report.is_problem();
        self.status = report.summary.clone();
        self.exit = Some(report);
    }
    /// The user acted on the account (sign-out, edit): no sign-in after its game, pending or
    /// still to come from the running session.
    fn decline_resign(&mut self) {
        self.resign_declined = self.pid.is_some() || self.busy;
        self.drop_resign();
    }
    /// Forget a pending sign-in after a game, or its one network retry, and its promise on the row.
    fn drop_resign(&mut self) {
        self.resign = None;
        if self.network_wait == Some(NetworkWait::AfterGame) { self.network_wait = None; }
        if self.network_wait == Some(NetworkWait::AfterGameRetry) {
            self.network_wait = None;
            if self.shown_note() == Some(&RowNote::NetworkRetry) { self.note = None; }
        }
        if self.shown_note() == Some(&RowNote::ResignAfterGame) { self.note = None; }
    }
}

/// A short line after a row's status, kept apart from it so it follows the language and is
/// removed by what it is, not by matching words.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RowNote {
    /// A Play waits for the sign-in in flight.
    QueuedPlay,
    /// The row signs back in after its game closed.
    ResignAfterGame,
    /// A sign-in that failed on the network starts again once DNS answers.
    NetworkRetry,
    /// That retry gave up.
    NetworkGone,
    /// The one-time code was spent by a sign-in that did not finish.
    OtpSpent,
    /// A sign-in Crystal started by itself without a code was refused.
    MayNeedOtp,
    /// No sign-in after the game during a maintenance or outage (with the service's note).
    ResignRefused(String),
}

impl RowNote {
    fn text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::QueuedPlay => t("FFXI will start when sign-in finishes.").into(),
            Self::ResignAfterGame => t("Signing back in after the game closed.").into(),
            Self::NetworkRetry => t("Crystal will try again when the network is back.").into(),
            Self::NetworkGone => t("The network did not come back. Sign in again once it is.").into(),
            Self::OtpSpent => t("That one-time code was used. Enter a new one, then press Play.").into(),
            Self::MayNeedOtp => t("This account may need a one-time password now: Crystal signs it in only when you press Play.").into(),
            Self::ResignRefused(note) => note.as_str().into(),
        }
    }
}

pub(crate) struct Editor {
    pub key: Option<String>,
    pub account: Account,
    pub game_install: String,
    pub dat_overlays: Vec<String>,
    pub auto_login: pol_less::AutoLogin,
    pub flags: EditorFlags,
}

/// The account sheet's switches that are not account fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EditorFlags {
    /// "Uses a one-time password": saved as what Crystal knows about the login
    /// (`Settings::account_otp`), so it is never signed in by itself.
    pub uses_otp: bool,
    /// New accounts: sign in to PlayOnline in the background once saved.
    pub sign_in_now: bool,
    pub show_id: bool,
    pub show_password: bool,
    /// A saved account's password field is read-only until Change.
    pub change_password: bool,
    pub show_totp_key: bool,
    /// A saved account's authenticator key, when it has one, is read-only until Change.
    pub change_totp_key: bool,
    /// Private-server accounts: run xiloader without `--hairpin` (`Settings::account_no_hairpin`).
    pub no_hairpin: bool,
}

/// Which of `EditorFlags` an `EditorFlag` message sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorFlag {
    UsesOtp,
    SignInNow,
    ShowId,
    ShowPassword,
    ChangePassword,
    ShowTotpKey,
    ChangeTotpKey,
    NoHairpin,
}

impl Editor {
    /// A fresh sheet for a new account: background sign-in on by default.
    fn blank() -> Self {
        Editor {
            key: None,
            account: blank_account(),
            game_install: String::new(),
            dat_overlays: Vec::new(),
            auto_login: pol_less::AutoLogin::default(),
            flags: EditorFlags { sign_in_now: true, ..Default::default() },
        }
    }
}

/// The Viewer refreshes its last-input stamp from window input (app 10060039)
/// for Auto-Away; Crystal forwards window input at most this often.
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(5);
static LAST_ACTIVITY: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// How long a Friends request may go without a result before the page says so. Shorter than
/// the loader's mail deadlines (MAIL_FRAMES per attempt, up to 3 attempts for a send): a result
/// after it still counts (`App::friends_expired`).
const FRIENDS_REQUEST_LIMIT: Duration = Duration::from_secs(45);

fn activity_due(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|at| now.saturating_duration_since(at) < ACTIVITY_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

/// `--preview-idle`: the preview's sample data with the live window's idle behaviour (its
/// own tick rate, the controller thread, no capture), for measuring the idle footprint
/// without accounts, sign-ins or network traffic.
pub(crate) fn preview_idle() -> bool {
    preview::has("--preview-idle")
}

/// Keyboard or pointer input reached the window: a `Message::UserInput`, at most once per
/// ACTIVITY_INTERVAL (every front end reports input through this one throttle).
pub(crate) fn user_activity() -> Option<Message> {
    activity_due(&mut LAST_ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()), Instant::now()).then_some(Message::UserInput)
}

#[derive(Clone)]
enum WorkerEvent {
    /// Ignore a startup/reconnect result superseded by a coordinated Play.
    Launch(String, u64, Box<WorkerEvent>),
    Progress(String, String),
    Complete(String, Result<LaunchResult, String>),
    /// A news update, tagged with its request and the language its feeds were read in.
    Services(u64, i18n::Lang, services::Update),
    Friends(u64, String, bool, Option<friends::Snapshot>),
    /// A signed-in session's Play: W20's report (absent when it never ran) and the outcome.
    StartGame(String, Option<patch::PatchReport>, Result<String, String>),
    /// A row's connectivity probe (07d §5.5): the patch server resolves again (`true`), or
    /// it gave up after `session_worker::NETWORK_PROBE_LIMIT` (`false`). Sent `for_launch`.
    NetworkBack(String, bool),
    /// A line for the status bar that outlives the account's progress lines.
    Notice(String),
    WindowerProfileProgress(String),
    Viewer(u64, viewer::Status),
    /// An interrupted update found at a Play was finished or undone (`Ok`), or could not
    /// be: the held launch then starts, or its rows say why.
    InterruptedUpdate(Box<Recovered>),
    /// A Play-driven update's thread (its job generation), proposal 09 phase 2.
    PlayUpdate(u64, play_update::Event),
    /// A maintenance version check of a game folder from this account's session (patch_watch):
    /// the W20's report, whether it said "current", and its phases; `Err` when none was sent.
    PatchCheck(PathBuf, String, Result<(patch::PatchReport, bool, Option<String>), String>),
    /// Window layouts, hotkeys, recovery, notifications (`features::Runtime`).
    Features(features::Event),
}
/// A launch waiting for something local (an interrupted update's recovery) before it starts.
#[derive(Clone, Debug)]
struct HeldLaunch {
    keys: Vec<String>,
    social_only: bool,
    clicked: Instant,
    trigger: Option<launch_timing::LaunchTrigger>,
}
/// The outcome of finishing interrupted updates for a held launch.
#[derive(Clone)]
struct Recovered {
    held: HeldLaunch,
    /// The folders whose journals were finished or undone.
    folders: Vec<PathBuf>,
    /// Every parked host was closed first (PlayOnline's folder was among them, or a retry).
    parks_closed: bool,
    result: Result<(), String>,
    /// After a success, the Settings game folder's and PlayOnline's local state again.
    game: Option<patch::PatchReport>,
    viewer: Option<viewer::Status>,
}
impl WorkerEvent {
    fn for_launch(self, key: &str, generation: u64) -> Self {
        Self::Launch(key.to_owned(), generation, Box::new(self))
    }
}

/// Sends one launch's events, tagged with its row and launch generation (`for_launch`): a
/// newer launch of the row makes them stale.
#[derive(Clone)]
struct LaunchTx {
    tx: tick_gate::EventSender,
    key: String,
    generation: u64,
    /// A sign-in's `AccountRow::sign_in_done`, set before its Complete goes out.
    done: Option<Arc<AtomicBool>>,
}
impl LaunchTx {
    fn new(tx: tick_gate::EventSender, key: String, generation: u64) -> Self {
        Self { tx, key, generation, done: None }
    }
    fn send(&self, event: WorkerEvent) {
        let _ = self.tx.send(event.for_launch(&self.key, self.generation));
    }
    fn progress(&self, line: &str) {
        self.send(WorkerEvent::Progress(self.key.clone(), line.to_owned()));
    }
    fn complete(&self, result: Result<LaunchResult, String>) {
        if let Some(done) = &self.done { done.store(true, Ordering::Release); }
        self.send(WorkerEvent::Complete(self.key.clone(), result));
    }
}

/// The window's timers and listeners for the current state (`App::schedule`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Schedule {
    /// The logic Tick's rate (`tick_gate` drops the ones that would change nothing).
    pub tick: Duration,
    /// Poll the controller thread.
    pub gamepad: bool,
    /// An account row is being dragged: raw pointer input goes to `account_order`.
    pub account_drag: bool,
    /// A held controller direction repeats (`Message::ControllerTick` every 50 ms).
    pub controller_repeat: bool,
}

#[derive(Clone)]
pub(crate) enum Message {
    Tick,
    ControllerTick,
    ControllerBack,
    ControllerPage(i8),
    ControllerPlay,
    ControllerNavigation(bool),
    SelfUpdate(self_update::Msg),
    WindowResized(Size),
    MinimizeWindow,
    MaximizeWindow,
    // native chrome, title double-click and edge resizing.
    WindowOpened(WindowId),
    WindowClosed(WindowId),
    /// The notification-area icon (or a second Crystal start) asked for something.
    Tray(tray::Command),
    /// After hiding: give the freed renderer's pages back to Windows.
    TrimMemory,
    /// Settings → System: closing the window keeps Crystal in the notification area.
    CloseToTray(bool),
    /// Settings → System: start Crystal in the background when Windows starts.
    StartWithWindows(bool),
    /// The scheduled task was registered (`true`) or removed, or why not.
    StartupTaskSet(bool, Result<(), String>),
    NativeWindow(isize),
    WindowMaximized(bool),
    TitlePressed,
    TitleMoved,
    TitleReleased,
    ResizeWindow(Edge),
    Navigate(Page),
    FriendsAccount(String),
    FriendsConnect,
    FriendsPlay,
    FriendsSignOut,
    /// One account row's Shut down.
    ShutdownAccount(String),
    ShutdownSelected,
    ShutdownAll,
    GameShutdownFinished(String, u64, Result<(), String>),
    AccountSignOut(String),
    AccountSignedOut(String, u64, Result<(), String>),
    FriendsStatus(session_worker::NativeControl),
    FriendsStatusGroups(bool),
    FriendsAutoAway(u8),
    /// Keyboard or mouse input reached the window (throttled at the source).
    UserInput,
    /// The window gained (`true`) or lost the keyboard focus (minimising loses it).
    WindowFocus(bool),
    FriendsQuery(String),
    /// Open a conversation, the Mailbox or the block list (None: nothing).
    FriendsSelect(Option<friends::Pane>),
    /// Fold or unfold the Online (0) / Offline (1) section.
    FriendsSection(usize),
    FriendsStatusOptions,
    FriendsAddSubject(bool),
    FriendsSocial(social::Request),
    FriendsRemove(friends::Friend),
    FriendsRemoveConfirm,
    FriendsSubject(String),
    FriendsMessageEdit(String),
    FriendsSend,
    FriendsDraftClose,
    ToggleDetails,
    ToggleAccount(String, bool),
    AccountDragStart(String),
    AccountDragOver(String),
    AccountDragFinish,
    AccountDragCancel,
    EditWindowerProfiles,
    WindowerProfilesClosed(Result<(), String>),
    Launch(String),
    MainAction,
    LaunchAll,
    /// Sign this account in to PlayOnline in the background (no game), with its typed code.
    SignIn(String),
    /// Home's Character select: automatic character login on (true) or off for Play's accounts.
    SetCharacterSelect(bool),
    OtpChanged(String, String),
    NewAccount,
    EditAccount(String),
    EditorChanged(Field, String),
    EditorRegion(u8),
    // the editor's Launch section (None = use the default).
    EditorLaunchWith(Option<crate::backend::LaunchWith>),
    EditorAutoLogin(bool),
    EditorFlag(EditorFlag, bool),
    EditorCharacterSlot(u8),
    AccountImport(account_import_ui::Msg),
    EditorBootIni(Option<String>),
    EditorProfile(Option<String>),
    SaveAccount,
    RequestDeleteAccount(String),
    CancelDeleteAccount,
    DeleteAccount(String),
    CloseEditor,
    ToggleSettings,
    SetLightTheme(bool),
    SetLanguage(i18n::Lang),
    SetPrivacy(bool),
    /// Settings → System: "Sign accounts in when Crystal opens".
    AutoSignIn(bool),
    SignInAfterGame(bool),
    /// Settings → System: the shut-down action ends the game host at once.
    CloseGamesInstantly(bool),
    /// Settings → System: Home's item and wiki search.
    ItemSearch(bool),
    SetLaunchWith(crate::backend::LaunchWith),
    WikiQueryChanged(String),
    AhQueryChanged(String),
    SearchWiki,
    SearchAh,
    AhResults(u64, Result<ffxiah::Results, String>),
    CloseAh,
    /// Open the portrait picker for this account (its key), or close it.
    PortraitPicker(Option<String>),
    /// The installed portraits, listed off the UI thread.
    PortraitsListed(std::sync::Arc<[u16]>),
    PortraitPage(usize),
    PortraitPick(u16),
    AhServerChanged(&'static str),
    NewsFilter(crate::services::NewsTab),
    InstallPathChanged(String),
    SelectInstallPath,
    FindInstallation,
    InstallationFound(u64, bool, Option<PathBuf>),
    InstallerPathChanged(String),
    AccountInstallChanged(String),
    PickedAccountFolder(String, Option<PathBuf>),
    PickedAccountOverlay(String, Option<PathBuf>),
    SelectInstallerPath,
    /// Open the native folder (or loader file) picker for a path field.
    Browse(PickTarget),
    Picked(PickTarget, Option<PathBuf>),
    StartMaintenance(MaintenanceKind),
    MaintenanceFinished(MaintenanceKind, Result<String, String>, Option<patch::PatchReport>),
    CancelMaintenance,
    /// Show or hide Downloads → Troubleshooting (Check files, Repair files).
    ToggleTroubleshooting,
    SaveSettings,
    Refresh,
    OpenUrl(String),
    OpenLog(String),
    OpenLogsFolder(String),
    AccountsLoaded(Result<Vec<Account>, String>),
    AccountsSaved(Result<Vec<Account>, String>),
    /// The finished news refresh, its request generation and language.
    ServicesLoaded(u64, i18n::Lang, Result<services::ServiceSnapshot, String>),
    ServiceStatusLoaded(u64, i18n::Lang, Result<services::ServiceSnapshot, String>),
    /// A read of FFXI's play flag, made while a Play waits.
    PlayFlagLoaded(play_flag::Reading),
    /// Cancel these accounts' "Wait and play".
    CancelWait(Vec<String>),
    /// A row's Cancel for a start that waits (`start_watch`).
    CancelStart(String),
    /// Pause the Play-driven update with this generation (its downloads are kept).
    CancelPlayUpdate(u64),
    /// A Play's PlayOnline update may sign the idle accounts out (the row asked once).
    ConfirmViewerUpdate,
    PatchLoaded(u64, patch::PatchReport),
    ViewerLoaded(u64, viewer::Status),
    /// The PlayOnline editions the registry names (`retail_region::installed_editions`).
    PolEditions(Vec<retail_region::RetailRegion>),
    /// Downloads → Install: the official installer's region (0 US, 1 EU, 2 JP).
    InstallerRegion(u8),
    CloseRequested,
    ConfirmClose,
    CancelClose,
    PreviewReady(Screenshot),
    /// The launcher features' screens (`features::ui`).
    Feature(features::ui::Msg),
    // The Settings page features own their state and messages in their modules.
    SettingsTab(settings_page::Tab),
    GameConfig(game_config::Msg),
    Prereqs(prereqs::Msg),
    Gamepad(gamepad::Msg),
    OpenSettingsTab(settings_page::Tab), // banner Fix → Settings tab
    LaunchTools(launch_tools::Msg),
    LaunchToolsScanned(launch_tools::Scan),
    OpenWithGame(open_with_game::Msg),
    /// The programs a game start opened: what failed.
    OpenWithGameDone(open_with_game::Report),
    Dgvoodoo(dgvoodoo::Msg),
    Reshade(reshade::Msg),
}
/// A path field that has a native Browse dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickTarget {
    GameFolder,
    AccountGameFolder,
    AccountDatFolder,
    InstallerFolder,
    AshitaFolder,
    WindowerFolder,
    /// A file, not a folder: the loader's pol.exe.
    LoaderFile,
    /// A file: the program at this index in Open with the game.
    ProgramFile(usize),
}

// Debug for logs and tests, but input messages can contain account passwords.
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LauncherMessage (payload redacted)")
    }
}

pub(crate) struct App {
    pub viewport: Size,
    pub maximized: bool,
    pub title_held: bool, // title pressed, drag not yet started
    pub page: Page,
    pub show_details: bool,
    pub accounts: Vec<AccountRow>,
    pub account_drag: account_order::Drag,
    pub editor: Option<Editor>,
    pub account_import: account_import_ui::State,
    pub services: Option<services::ServiceSnapshot>,
    pub news_loading: bool,
    /// The language of the news refresh in flight (meaningful while `news_loading`).
    news_lang: i18n::Lang,
    news_generation: u64,
    pub news_error: String,
    startup_warning: Option<String>,
    /// settings.json could not be read at start: nothing Crystal saves by itself replaces it
    /// (the warning above is shown once; this stays until the user saves settings).
    settings_damaged: bool,
    pub patch: Option<patch::PatchReport>,
    pub patch_loading: bool,
    pub installation_loading: bool,
    installation_generation: u64,
    pub viewer: Option<viewer::Status>,
    pub viewer_loading: bool,
    viewer_generation: u64,
    /// The PlayOnline editions the registry names; `pol_editions` adds the found US Viewer.
    pol_editions: Vec<retail_region::RetailRegion>,
    pub settings: Settings,
    pub show_settings: bool,
    pub status: String,
    pub vault_ready: bool,
    /// Why the saved accounts could not be read (the rail says so in place of its first-run state).
    vault_error: Option<String>,
    startup_signin_pending: bool,
    /// Startup sign-ins whose row was still busy with a previous session, and since when
    /// (07d §5.5): each starts once its row is free, or is skipped after STARTUP_WAIT_LIMIT.
    startup_waiting: Vec<(String, Instant)>,
    /// When the startup sign-ins were issued (the parks start after them, 07d §5.7).
    startup_issued: Option<Instant>,
    /// The `launcher start` history line was written (07d §4.4).
    launcher_start_logged: bool,
    /// This window's first official status read has finished (either way): until it has, a
    /// Play has only a missing or cached status to go on, so it is held (`hold_for_status`).
    status_read_done: bool,
    /// A hold reached STATUS_HOLD_LIMIT without a read: later Plays are not held either.
    status_hold_spent: bool,
    /// Plays clicked before the first status read, waiting for it.
    held_plays: Vec<HeldPlay>,
    /// Rows that say "Starting": their flags as last logged, and the stuck-start net.
    start_watch: start_watch::StartWatch,
    /// Idle sessions to sign out and in again for a changed launcher (`launcher_change`).
    launcher_changes: launcher_change::LauncherChanges,
    /// Plays waiting for FFXI to come back ("Wait and play").
    waiting_plays: Vec<WaitingPlay>,
    /// The last read of FFXI's play flag, and when the next is due (only while a Play waits or
    /// FFXI is in maintenance).
    play_flag: play_flag::Known,
    flag_schedule: play_flag::Schedule,
    /// Play is the update (proposal 09 phase 2): one update job per game folder.
    pub(crate) play_updates: play_update::Jobs,
    /// The update as soon as it is out: maintenance's version checks (patch_watch.rs).
    patch_watch: patch_watch::Watch,
    /// Tests: record `start_launch` calls instead of starting workers.
    #[cfg(test)]
    launches: Option<Vec<(Vec<String>, bool, Instant, Option<launch_timing::LaunchTrigger>)>>,
    /// Tests: recorded starts that went without a W20 (`skip_w20`).
    #[cfg(test)]
    w20_skipped: Vec<String>,
    /// Parked warm hosts for cold Plays, one per account, tool and profile (07d §1.5).
    parks: pol_less::park::Pool<session_worker::ParkWorker>,
    /// Each wanted park key's last fingerprint; re-read every FINGERPRINT_EVERY.
    park_prints: std::collections::BTreeMap<pol_less::park::ParkKey, pol_less::park::Fingerprint>,
    /// When the fingerprints were read; `None` re-reads them at the next tend (after a
    /// settings save, a profile-editor close or an installation change).
    park_checked: Option<Instant>,
    /// The last pool tend (at most once a second).
    park_tended: Option<Instant>,
    pub confirm_close: bool,
    closing: bool,
    pub pending_delete: Option<String>,
    pub wiki_query: String,
    pub ah_query: String,
    pub ah_lookup: ffxiah::Lookup,
    /// The portrait picker, while open.
    pub portrait_picker: Option<portraits::Picker>,
    /// Each account's own portrait as its PlayOnline session last reported it (this run only).
    pub own_portraits: HashMap<String, u16>,
    pub ah_server: &'static str,
    pub news_filter: crate::services::NewsTab,
    pub maintenance: Option<MaintenanceKind>,
    pub maintenance_progress: operation::Progress,
    pub maintenance_result: String,
    pub installer_error: Option<String>,
    pub maintenance_tone: ResultTone,
    pub maintenance_cancelled: bool,
    /// What the running Update covers: (PlayOnline, FINAL FANTASY XI).
    update_scope: (bool, bool),
    /// Downloads → Troubleshooting is expanded.
    pub troubleshooting_open: bool,
    /// An interrupted update is being finished before a launch: `Some(true)` when that
    /// includes PlayOnline's folder (whose files parked hosts hold).
    recovering: Option<bool>,
    /// The game folder `patch` describes when it is not the Settings one: an account's own
    /// folder whose W20 refused a Play. The main button's Update updates that folder.
    patch_folder: Option<PathBuf>,
    /// The official versions real version checks last told (`official_versions`), read again
    /// whenever a check's outcome reaches the window.
    official: official_versions::Store,
    /// Sign-ins the W2U stopped because PlayOnline needs an update, waiting for that update to
    /// start (`tend_viewer_update`), and those the running Update took: started again when it
    /// succeeds (proposal 09).
    refused_plays: Vec<RefusedSignIn>,
    reissue_after_update: Vec<RefusedSignIn>,
    /// The official PlayOnline version the last sign-in's W2U named, and when its refusal
    /// reached the window: the update that follows it sends no version request of its own.
    viewer_asked: Option<(String, Instant)>,
    /// When a Tick last looked whether the PlayOnline update that sign-ins wait for can start.
    viewer_tended: Option<Instant>,
    /// The official version the running PlayOnline update installs, and the one the last
    /// successful one installed: a W2U that still asks for that version starts no second update.
    viewer_target: Option<String>,
    viewer_installed: Option<String>,
    /// A PlayOnline update succeeded in this window: a sign-in Crystal starts again (or by
    /// itself) that local evidence still calls out of date (`viewer::Status::stale`, an old
    /// core) starts no second update, so nothing loops; a user's own Play still does.
    viewer_updated: bool,
    active_operation: Option<operation::Operation>,
    config: Config,
    sender: tick_gate::EventSender,
    receiver: mpsc::Receiver<WorkerEvent>,
    /// The launcher window; `None` while Crystal runs in the background (notification area
    /// icon only, no surface or renderer).
    window: Option<WindowId>,
    /// The last park tend found every wanted park parked (or none wanted): nothing for the
    /// next Tick to start (`tick_gate`).
    parks_settled: bool,
    /// Start with Windows: the scheduled task's registration is being changed.
    startup_task_busy: bool,
    vault_saving: bool,
    patch_generation: u64,
    refreshed: Instant,
    preview: bool,
    preview_capture_sent: bool,
    pub settings_tab: settings_page::Tab,
    pub game_config: game_config::State,
    pub prereqs: prereqs::State,
    pub gamepad: gamepad::State,
    pub launch_tools: launch_tools::State,
    open_gate: open_with_game::BatchGate,
    pub windower_editor_open: bool,
    pub friends: friends::State,
    /// The Friends request whose time limit passed without a result, still matched against
    /// later reads: the loader's own deadlines are longer (a send retries for minutes).
    friends_expired: Option<String>,
    friends_generation: u64,
    friends_checked: Instant,
    pub dgvoodoo: dgvoodoo::State,
    pub reshade: reshade::State,
    pub self_update: self_update::State,
    /// The automatic restart that installs a downloaded update (`self_update::auto`).
    pub(crate) auto_restart: self_update::auto::AutoRestart,
    pub controller_nav: controller_nav::State,
    /// The launcher features beside Play (`features`); `None` in previews and tests.
    pub(crate) features: Option<features::Runtime>,
    /// The signed-in accounts the features last heard of (notifications follow them).
    features_sessions: Vec<String>,
    /// The features' screens' own state (`features::ui`).
    pub(crate) features_view: features::ui::State,
}

impl App {
    /// The daemon's boot: the state, then its window, unless Crystal starts in the background
    /// (`--background`, Start with Windows), where only the notification-area icon shows.
    fn new() -> (Self, Effect<Message>) {
        let (mut app, task) = Self::boot();
        // `--preview-idle` measures the background too: its close button hides the window.
        if preview_idle() { app.settings.close_to_tray = true; }
        let live = !app.preview || preview_idle();
        let background = live && std::env::args().any(|a| a == startup_task::BACKGROUND_ARG);
        if live { tray::start(); }
        // `--preview-idle` too, so the idle footprint is measured with the features' threads.
        if live { app.start_features(); }
        if background {
            tray::set_icon(true);
            launch_log::append_history(&app.config.history_path(), "Crystal", &format!(
                "background start: pid {}, no window", std::process::id()), None);
            // Once the startup sign-ins and checks have run.
            return (app, Effect::batch([task, App::trim_later(Duration::from_secs(30))]));
        }
        let open = app.open_window();
        (app, Effect::batch([task, open]))
    }

    /// The one field list: inert values, no disk or network reads. `boot` sets what a real
    /// start differs in; tests use it as it is (sample data, `preview` as the no-I/O switch).
    fn base(config: Config, settings: Settings, sender: tick_gate::EventSender, receiver: mpsc::Receiver<WorkerEvent>, dgvoodoo: dgvoodoo::State) -> Self {
        Self {
            viewport: Size::new(1200.0, 820.0),
            maximized: false,
            title_held: false,
            page: Page::Home,
            show_details: false,
            accounts: Vec::new(),
            account_drag: account_order::Drag::default(),
            windower_editor_open: false,
            editor: None,
            account_import: account_import_ui::State::default(),
            services: None,
            news_loading: false,
            news_lang: i18n::Lang::En,
            news_generation: 0,
            news_error: String::new(),
            startup_warning: None,
            settings_damaged: false,
            patch: None,
            patch_loading: false,
            installation_loading: false,
            installation_generation: 0,
            viewer: None,
            viewer_loading: false,
            viewer_generation: 0,
            pol_editions: Vec::new(),
            settings,
            show_settings: false,
            status: String::new(),
            vault_ready: true,
            vault_error: None,
            startup_signin_pending: false,
            startup_waiting: Vec::new(),
            startup_issued: None,
            launcher_start_logged: false,
            status_read_done: true,
            status_hold_spent: false,
            held_plays: Vec::new(),
            start_watch: start_watch::StartWatch::default(),
            launcher_changes: launcher_change::LauncherChanges::default(),
            waiting_plays: Vec::new(),
            play_flag: play_flag::Known::default(),
            flag_schedule: play_flag::Schedule::default(),
            play_updates: play_update::Jobs::default(),
            patch_watch: patch_watch::Watch::default(),
            #[cfg(test)]
            launches: None,
            #[cfg(test)]
            w20_skipped: Vec::new(),
            parks: Default::default(),
            park_prints: Default::default(),
            park_checked: None,
            park_tended: None,
            confirm_close: false,
            closing: false,
            pending_delete: None,
            wiki_query: String::new(),
            ah_query: String::new(),
            ah_lookup: ffxiah::Lookup::default(),
            portrait_picker: None,
            own_portraits: HashMap::new(),
            ah_server: crate::ffxiah::DEFAULT_AH_SERVER,
            news_filter: crate::services::NewsTab::Latest,
            maintenance: None,
            maintenance_progress: operation::Progress::default(),
            maintenance_result: String::new(),
            installer_error: None,
            maintenance_tone: ResultTone::default(),
            maintenance_cancelled: false,
            update_scope: (true, true),
            troubleshooting_open: false,
            recovering: None,
            patch_folder: None,
            official: official_versions::Store::default(),
            refused_plays: Vec::new(),
            reissue_after_update: Vec::new(),
            viewer_asked: None,
            viewer_tended: None,
            viewer_target: None,
            viewer_installed: None,
            viewer_updated: false,
            active_operation: None,
            config,
            sender,
            receiver,
            window: None,
            parks_settled: true,
            startup_task_busy: false,
            vault_saving: false,
            patch_generation: 0,
            refreshed: Instant::now(),
            preview: true,
            preview_capture_sent: false,
            settings_tab: settings_page::Tab::default(),
            game_config: game_config::State::default(),
            prereqs: prereqs::State::default(),
            gamepad: gamepad::State::default(),
            launch_tools: launch_tools::State::default(),
            open_gate: Default::default(),
            friends: friends::State::default(),
            friends_expired: None,
            friends_generation: 0,
            friends_checked: Instant::now(),
            dgvoodoo,
            reshade: reshade::State::default(),
            self_update: self_update::State::default(),
            auto_restart: self_update::auto::AutoRestart::default(),
            controller_nav: controller_nav::State::default(),
            features: None,
            features_sessions: Vec::new(),
            features_view: features::ui::State::default(),
        }
    }

    fn boot() -> (Self, Effect<Message>) {
        let config = Config::default();
        let preview = preview::flags().is_some();
        let (settings, startup_warning) = if preview {
            (Settings::default(), None)
        } else {
            match load_settings_and_language(&config) {
                Ok(settings) => (settings, None),
                Err(error) => (
                    Settings::default(),
                    Some(tf("{error} The original settings file was kept; defaults are being used.", &[("error", &error)])),
                ),
            }
        };
        // Previews are English unless --preview-ja, whatever Windows is set to.
        i18n::set(if preview {
            if preview::has("--preview-ja") { i18n::Lang::Ja } else { i18n::Lang::En }
        } else {
            settings.language.unwrap_or_else(i18n::system_default)
        });
        privacy::set(if preview { preview::has("--preview-private") } else { settings.privacy });
        let (sender, receiver) = mpsc::channel();
        // With dgVoodoo on and not yet installed, its download starts in the background now,
        // so the first Play doesn't wait for it.
        let dgvoodoo = dgvoodoo::State::new(settings.dgvoodoo);
        let mut app = Self::base(config.clone(), settings, tick_gate::EventSender::new(sender), receiver, dgvoodoo);
        app.viewport = initial_window_size();
        app.ah_server = crate::ffxiah::saved_ah_server(app.settings.ah_server.as_deref());
        app.preview = preview;
        app.settings_damaged = startup_warning.is_some();
        app.startup_warning = startup_warning;
        app.services = if preview { None } else { services::cached(&news_cache_path(&config, i18n::current()), i18n::current()) };
        app.news_lang = i18n::current();
        app.status = t("Loading saved accounts…").into();
        app.vault_ready = false;
        app.startup_signin_pending = !preview;
        app.status_read_done = preview;
        app.maintenance_result = t("Files have not been checked. Version availability alone does not verify file integrity.").into();
        app.maintenance_tone = ResultTone::Unverified;
        if !preview { app.official = official_versions::load(&official_versions::path_for(&config)); }
        app.self_update = self_update::initial(&config);
        let tools = app.launch_tools.refresh(&app.settings);
        // The game's settings are read once, as soon as the app runs (a few registry values).
        let game_config = Effect::done(Message::GameConfig(game_config::Msg::Load));
        if preview {
            let (app, task) = preview::boot(app);
            return (app, Effect::batch([tools, game_config, task]));
        }
        let accounts = Effect::perform(
            async move {
                tokio::task::spawn_blocking(move || backend::load_or_import_accounts(&config))
                    .await
                    .unwrap_or_else(|_| Err(t("Could not load saved accounts.").into()))
            },
            Message::AccountsLoaded,
        );
        let checks = app.refresh();
        let installation = app.find_installation(true);
        let viewer = app.start_viewer_check();
        let launcher = if app.settings.launcher_auto_updates && !matches!(app.self_update, self_update::State::Error(_)) { Effect::done(Message::SelfUpdate(self_update::Msg::Check)) } else { Effect::none() };
        // Start with Windows on: its task still starts this exe (it may have moved).
        let startup = if app.settings.start_with_windows { app.startup_task_busy = true; app.startup_task(None) } else { Effect::none() };
        // The private servers' manifests read again, so an update hold set since is known.
        let experiment = app.config.experiment.clone();
        let manifests = Effect::future(async move {
            server_manifest::refresh_known(experiment).await;
            Effect::none()
        });
        (app, Effect::batch([accounts, checks, viewer, launcher, installation, startup, tools, game_config, manifests]))
    }

    /// What the window's timers and listeners should be after this update, in no toolkit's
    /// terms; the host (`slint_host`) runs them to match.
    pub(crate) fn schedule(&self) -> Schedule {
        // Every tick rebuilds and redraws the whole view, so poll fast only while
        // work is in flight and idle along at 1 Hz otherwise.
        let busy = self.preview && !preview_idle()
            || self.maintenance.is_some()
            || self.active_operation.is_some()
            || self.vault_saving
            || self.news_loading
            || self.patch_loading
            || self.installation_loading
            || self.viewer_loading
            || self.friends.pending.is_some()
            || !self.held_plays.is_empty()
            || !self.play_updates.jobs.is_empty()
            || self.accounts.iter().any(|a| a.busy || a.batch_pending);
        let tick = Duration::from_millis(if busy { 100 } else { 1000 });
        // Ticks that would change nothing are not sent (each message redraws the window).
        tick_gate::set_quiet(!busy && self.tick_quiet(), self.watched_flags(), self.next_timed_work());
        Schedule {
            tick,
            // The controller thread polls only while there is a window to drive.
            gamepad: self.window.is_some(),
            account_drag: self.account_drag.source.is_some(),
            controller_repeat: self.controller_nav.repeating(),
        }
    }

    /// The Start with Windows task is being registered or removed (its switch waits).
    pub(crate) fn startup_task_busy(&self) -> bool { self.startup_task_busy }

    pub fn launchable_count(&self) -> usize {
        self.launch_keys(false).len()
    }
    pub fn selected_count(&self) -> usize {
        self.launch_keys(true).len()
    }
    fn launch_keys(&self, selected_only: bool) -> Vec<String> {
        if self.launch_blocked() {
            return Vec::new();
        }
        // Row keys are unique (`replace_accounts`). Private-server accounts play too: their
        // launches leave the PlayOnline steps out (`start_launch`).
        self.accounts
            .iter()
            .filter(|r| r.can_play() && (!selected_only || r.selected))
            .map(|r| r.key.clone())
            .collect()
    }
    /// Whether `key`'s Play waits for FFXI to come back.
    pub(crate) fn is_waiting(&self, key: &str) -> bool {
        self.waiting_plays.iter().any(|wait| wait.keys.iter().any(|waiting| waiting == key))
    }
    /// The Play-driven update `key` takes part in: its Play asked for it, or waits for it
    /// (`true`: it is one of the job's waiters; `false`: its Play waits for the folder).
    pub(crate) fn play_update_of(&self, key: &str) -> Option<(&play_update::Job, bool)> {
        if self.play_updates.jobs.is_empty() { return None; }
        if let Some(job) = self.play_updates.for_key(key) { return Some((job, true)); }
        let row = self.accounts.iter().find(|row| row.key == key && row.queued_play.is_some())?;
        let folder = self.play_folder(row)?;
        self.play_updates.for_folder(&folder).map(|job| (job, false))
    }
    /// The selected accounts "Wait and play" would set waiting: launchable, not waiting yet.
    pub(crate) fn wait_keys(&self) -> Vec<String> {
        // FFXI's maintenance is the retail servers': a private server's Play never waits for it.
        self.launch_keys(true).into_iter()
            .filter(|key| !self.is_waiting(key) && self.accounts.iter().any(|row| row.key == *key && row.account.mode == AccountMode::Retail))
            .collect()
    }
    fn has_sessions(&self) -> bool {
        self.accounts.iter().any(|r| r.shutting_down || r.signing_out || r.busy || r.batch_pending || r.pid.is_some()
            || !self.preview && session_worker::find(&self.config, &r.account).is_some())
    }

    fn update_sessions(&self) -> Option<Vec<session_worker::BackgroundSession>> {
        let mut sessions = Vec::new();
        for row in &self.accounts {
            if row.shutting_down || row.signing_out || row.batch_pending || row.queued_play.is_some() || row.is_playing()
                || row.busy && !row.preparing_session { return None; }
            let session = (!self.preview).then(|| session_worker::find(&self.config, &row.account)).flatten();
            if let Some(session) = session {
                if !session.can_prepare_update() { return None; }
                sessions.push(session);
            } else if row.pid.is_some() || row.busy { return None; }
        }
        Some(sessions)
    }

    pub(crate) fn sessions_block_update(&self) -> bool { self.update_sessions().is_none() }
    pub fn launch_blocked(&self) -> bool {
        self.closing || self.installation_loading || self.files_changing() || self.recovering.is_some() || self.windower_editor_open
    }

    /// Game, PlayOnline, ReShade or Crystal files are being written: what a parked pol.exe
    /// (polcore.dll loaded) must not hold open. A read-only installation scan is not, and
    /// neither is finishing an interrupted update in a game folder only.
    fn files_changing(&self) -> bool {
        self.reshade.busy
            || self.recovering == Some(true)
            || self.maintenance.is_some_and(MaintenanceKind::blocks_launch)
            || matches!(self.self_update, self_update::State::Applying)
    }

    /// What the running file operation is called: an Update names what it updates.
    pub(crate) fn maintenance_label(&self) -> &'static str {
        match (self.maintenance, self.update_scope) {
            (Some(MaintenanceKind::Update), (true, false)) => t("Updating PlayOnline"),
            (Some(MaintenanceKind::Update), (false, true)) => t("Updating FINAL FANTASY XI"),
            (Some(kind), _) => kind.label(),
            (None, _) => t("Working"),
        }
    }

    /// What an Update now covers: PlayOnline when its last sign-in check or local state
    /// says it needs one, FINAL FANTASY XI when its version check did, or when nothing
    /// says so (the Play button's Update). A game update never asks for PlayOnline's file
    /// list, as stock never does (capture 2026-10-13 00:49).
    fn update_scope(&self) -> (bool, bool) {
        let viewer = self.viewer.as_ref().is_some_and(viewer::Status::requires_update);
        let game = self.patch.as_ref().is_some_and(|report| report.state == patch::PatchState::Required);
        (viewer, game || !viewer)
    }

    /// The Play gate's verdict and the note that explains it: the official status (Round 4),
    /// over which a fresh read of FFXI's play flag wins both ways (`play_flag::decide`).
    fn service_status(&self) -> (services::Availability, String) {
        if self.preview {
            return self.services.as_ref().map_or((services::Availability::Unknown, String::new()),
                |snapshot| (snapshot.availability, snapshot.availability_note.clone()));
        }
        let now = chrono::Utc::now();
        let official = self.services.as_ref().map_or((services::Availability::Unknown, String::new()), |snapshot| snapshot.recent_status(now));
        play_flag::decide(&self.play_flag, official, now)
    }

    pub fn service_availability(&self) -> services::Availability {
        self.service_status().0
    }

    /// The note that explains `service_availability`, as a waiting Play shows it.
    pub fn service_note(&self) -> String {
        self.service_status().1
    }

    fn service_down(&self) -> bool {
        matches!(self.service_availability(), services::Availability::Maintenance | services::Availability::Outage)
    }

    pub fn main_action(&self) -> MainAction {
        if self.maintenance.is_some() || self.reshade.busy || matches!(self.self_update, self_update::State::Applying) { return MainAction::Working; }
        // An update FFXI or PlayOnline needs no longer turns the button into Update: Play is
        // the update (proposal 09 phase 2).
        match self.service_availability() {
            services::Availability::Maintenance => MainAction::Maintenance,
            services::Availability::Outage => MainAction::Offline,
            _ => MainAction::Play,
        }
    }

    fn run_main_action(&mut self) -> Effect<Message> {
        if self.reshade.busy {
            self.page = Page::Settings;
            self.settings_tab = settings_page::Tab::Graphics;
            return Effect::none();
        }
        // Resolve again on click: a status refresh may have changed the action.
        let action = self.main_action();
        match action {
            MainAction::Play => self.play_from_home("main", true),
            MainAction::Working => {
                self.page = Page::Maintenance;
                self.show_settings = false;
                self.show_details = false;
            }
            // "Wait and play": the selected accounts wait for FFXI (`wait_for_service`). With
            // nothing left to set waiting, the button explains the downtime.
            MainAction::Maintenance | MainAction::Offline => {
                let keys = self.wait_keys();
                if self.preview || keys.is_empty() { self.show_details = true; } else { self.play(keys); }
            }
        }
        Effect::none()
    }

    /// Open the native dialog off the UI thread, starting from what the field holds.
    fn browse(&mut self, target: PickTarget) -> Effect<Message> {
        let blocked = match target {
            PickTarget::GameFolder | PickTarget::InstallerFolder => self.maintenance.is_some(),
            _ => false,
        };
        if self.preview || blocked {
            return Effect::none();
        }
        if target == PickTarget::AccountGameFolder || target == PickTarget::AccountDatFolder {
            let Some(editor) = &self.editor else { return Effect::none(); };
            if self.vault_saving { return Effect::none(); }
            if target == PickTarget::AccountDatFolder && editor.dat_overlays.len() >= 8 { return Effect::none(); }
            let current = if target == PickTarget::AccountDatFolder {
                editor.dat_overlays.last().unwrap_or(&editor.game_install)
            } else { &editor.game_install };
            let start = dialog_start(current, false);
            let key = account_key(&editor.account);
            return Effect::perform(pick_path(target, start), move |picked| {
                if target == PickTarget::AccountDatFolder { Message::PickedAccountOverlay(key.clone(), picked) }
                else { Message::PickedAccountFolder(key.clone(), picked) }
            });
        }
        let current = match target {
            PickTarget::GameFolder => &self.settings.install_path,
            PickTarget::InstallerFolder => &self.settings.installer_path,
            PickTarget::AshitaFolder => &self.settings.ashita_path,
            PickTarget::WindowerFolder => &self.settings.windower_path,
            PickTarget::LoaderFile => &self.settings.loader_path,
            PickTarget::ProgramFile(i) => self.settings.open_with_game.get(i).map_or(&EMPTY, |p| &p.path),
            PickTarget::AccountGameFolder | PickTarget::AccountDatFolder => unreachable!(),
        };
        static EMPTY: String = String::new();
        let start = dialog_start(current, matches!(target, PickTarget::LoaderFile | PickTarget::ProgramFile(_)));
        Effect::perform(pick_path(target, start), move |picked| {
            Message::Picked(target, picked)
        })
    }

    /// A dialog answer: put the chosen path in its field. The game and installer
    /// folders then go through the same validation and confirmation as before.
    fn picked(&mut self, target: PickTarget, picked: Option<PathBuf>) -> Effect<Message> {
        let Some(picked) = picked else { return Effect::none() };
        let text = backend::plain_windows_path(&picked).to_string_lossy().into_owned();
        match target {
            PickTarget::GameFolder | PickTarget::InstallerFolder => {
                if self.maintenance.is_some() {
                    return Effect::none();
                }
                let installer = target == PickTarget::InstallerFolder;
                if installer {
                    self.settings.installer_path = text;
                    self.settings.installer_selected = false;
                } else {
                    let _ = self.update(Message::InstallPathChanged(text));
                }
                self.select_path(installer);
            }
            PickTarget::AshitaFolder => return self.update(Message::LaunchTools(launch_tools::Msg::AshitaPath(text))),
            PickTarget::WindowerFolder => return self.update(Message::LaunchTools(launch_tools::Msg::WindowerPath(text))),
            PickTarget::LoaderFile => return self.update(Message::LaunchTools(launch_tools::Msg::LoaderPath(text))),
            PickTarget::ProgramFile(i) => {
                let _ = self.update(Message::OpenWithGame(open_with_game::Msg::Path(i, text)));
            }
            PickTarget::AccountGameFolder | PickTarget::AccountDatFolder => {}
        }
        Effect::none()
    }

    fn select_path(&mut self, installer: bool) {
        if self.preview || self.maintenance.is_some() || self.reshade.busy {
            return;
        }
        if installer { self.installer_error = None; }
        let raw = if installer {
            &self.settings.installer_path
        } else {
            &self.settings.install_path
        };
        let path = match selected_folder(raw, !installer) {
            Ok(path) => path,
            Err(error) => {
                if installer { self.installer_error = Some(error.clone()); }
                self.status = error;
                return;
            }
        };
        // Installer archives have their own validated names and staging files. Their
        // folder may contain (or be inside) the selected game installation.
        let mut settings = self.settings.clone();
        if installer {
            settings.installer_path = path.to_string_lossy().into_owned();
            settings.installer_selected = true;
        } else {
            settings.install_path = path.to_string_lossy().into_owned();
            settings.install_selected = true;
        }
        match save_settings(&self.config, &settings) {
            Ok(()) => {
                self.settings = settings;
                self.status = if installer {
                    tf("Selected installer download folder: {path}", &[("path", &path.display())])
                } else {
                    tf("Selected game installation folder: {path}", &[("path", &path.display())])
                };
                if !installer {
                    self.installation_generation = self.installation_generation.wrapping_add(1);
                    self.installation_loading = false;
                    self.patch = None;
                    self.patch_folder = None;
                    self.maintenance_result =
                        t("Game folder selected. Play checks its version before FINAL FANTASY XI starts.")
                            .into();
                    self.maintenance_tone = ResultTone::Unverified;
                }
            }
            Err(error) => {
                if installer { self.installer_error = Some(error.clone()); }
                self.status = error;
            }
        }
    }

    fn start_maintenance(&mut self, kind: MaintenanceKind) -> Effect<Message> {
        self.start_maintenance_scoped(kind, false)
    }

    /// `viewer_only`: a sign-in's PlayOnline update, PlayOnline's alone: after the W2U that asked
    /// for it (`updater::update_viewer_after_connect`), or with its own version request when
    /// that answer is old (`run_combined(Some(viewer), None, ..)`). The game's own update
    /// follows the Play in its session. Passed, never stored, so an Update refused here cannot
    /// leave a later Downloads Update PlayOnline-only.
    fn start_maintenance_scoped(&mut self, kind: MaintenanceKind, viewer_only: bool) -> Effect<Message> {
        if self.preview || self.maintenance.is_some() {
            return Effect::none();
        }
        // A private server that hasn't approved the new FFXI client holds the game's update in
        // the folder its accounts play from (lifted on the server's side). PlayOnline's own
        // update, when one is due, still runs on its own.
        let mut viewer_only = viewer_only;
        let mut held = None;
        if kind == MaintenanceKind::Update && !viewer_only {
            let target = self.update_account_folder(kind).unwrap_or_else(|| PathBuf::from(&self.settings.install_path));
            if let Some(hold) = self.update_hold(&target) {
                if !self.update_scope().0 {
                    self.status = hold;
                    return Effect::none();
                }
                viewer_only = true;
                held = Some(hold);
            }
        }
        let game_operation = matches!(
            kind,
            MaintenanceKind::Check | MaintenanceKind::Update | MaintenanceKind::Repair
        );
        if game_operation && self.installation_loading { return Effect::none(); }
        // A Play-driven update holds its folder's updater lock and its accounts' sessions.
        if kind.blocks_launch() && !self.play_updates.jobs.is_empty() {
            self.status = t("Wait for the FINAL FANTASY XI update to finish.").into();
            if !game_operation { self.installer_error = Some(self.status.clone()); }
            return Effect::none();
        }
        let account_folder = self.update_account_folder(kind);
        if !game_operation {
            // Pressing an installer step selects its current folder. The old
            // extra confirmation left all three buttons disabled on first use.
            self.settings.installer_selected = false;
            self.select_path(true);
            if !self.settings.installer_selected { return Effect::none(); }
        } else if !self.settings.install_selected && account_folder.is_none() && !viewer_only {
            // PlayOnline's own update never touches a game folder: it needs none selected.
            self.status =
                t("Select the target folder explicitly before starting this operation.").into();
            return Effect::none();
        }
        let idle_sessions = if matches!(kind, MaintenanceKind::Update | MaintenanceKind::Repair) {
            self.update_sessions()
        } else { Some(Vec::new()) };
        if kind.blocks_launch() && (idle_sessions.is_none()
            || !matches!(kind, MaintenanceKind::Update | MaintenanceKind::Repair) && self.has_sessions()) {
            self.status = t("Close all games and sign out PlayOnline sessions before updating, repairing, or running official setup. Existing sessions were kept.").into();
            if !game_operation { self.installer_error = Some(self.status.clone()); }
            return Effect::none();
        }
        if kind.blocks_launch() && !self.vault_ready {
            self.status = t("Wait for saved accounts and running sessions to be checked before changing game files.").into();
            if !game_operation { self.installer_error = Some(self.status.clone()); }
            return Effect::none();
        }
        // A parked pol.exe has polcore.dll loaded: close every park before files change (07d §1.5),
        // off the interface thread, as the operation's first step.
        let parks = if kind.blocks_launch() { self.parks.drain(None) } else { Vec::new() };
        // The sign-ins that wait for PlayOnline's update start again when this one succeeds
        // (whoever started it).
        if kind == MaintenanceKind::Update {
            self.reissue_after_update = std::mem::take(&mut self.refused_plays);
            // The row's "Sign out and update" asked for this Update.
            self.play_updates.viewer_confirm = None;
        }
        let install = account_folder.unwrap_or_else(|| PathBuf::from(&self.settings.install_path));
        let dir = PathBuf::from(&self.settings.installer_path);
        let installer_region = self.installer_region();
        let op = operation::Operation::default();
        // The update or repair reads and writes the game folder: its I/O, not ours.
        startup_prefetch::cancel();
        if matches!(kind, MaintenanceKind::Update | MaintenanceKind::Repair) { op.begin_update(); }
        let idle_sessions = idle_sessions.unwrap_or_default();
        if !idle_sessions.is_empty() {
            op.progress(crate::operation::Stage::SignOut, 0, idle_sessions.len() as u64,
                t("Signing out idle PlayOnline sessions before updating"));
        }
        // Check and Repair cover both components; an Update what needs it.
        let (update_viewer, update_game) = if kind != MaintenanceKind::Update { (true, true) }
            else if viewer_only { (true, false) } else { self.update_scope() };
        self.update_scope = (update_viewer, update_game);
        // The update a sign-in's W2U just asked for uses that W2U's answer, as stock's does;
        // each answer once. Any other Update asks the server itself.
        let asked = self.viewer_asked.take().filter(|(version, at)| kind == MaintenanceKind::Update && viewer_only
            && at.elapsed() <= VIEWER_VERDICT_FRESH && patch::valid_viewer_version(version)).map(|(version, _)| version);
        #[cfg(test)]
        if kind == MaintenanceKind::Update && viewer_only { self.play_updates.viewer_started.push(asked.clone()); }
        self.viewer_target.clone_from(&asked);
        self.active_operation = Some(op.clone());
        self.maintenance = Some(kind);
        self.maintenance_cancelled = false;
        self.maintenance_progress = op.snapshot();
        self.maintenance_result.clear();
        self.maintenance_tone = ResultTone::Done;
        self.status = self.maintenance_label().into();
        if let Some(hold) = held { self.status = format!("{} {hold}", self.status); }
        if game_operation && update_viewer { self.viewer_generation += 1; self.viewer_loading = true; }
        let viewer_generation = self.viewer_generation;
        let previous_viewer_version = self.viewer.as_ref().map(|v| v.version.remote_version.clone())
            .filter(|version| patch::valid_viewer_version(version));
        let sender = self.sender.clone();
        let (official_path, preview) = (official_versions::path_for(&self.config), self.preview);
        let backup_config = self.config.clone();
        Effect::perform(
            async move {
                if let Err(error) = close_parks_now(parks).await {
                    return (Err(error), None);
                }
                if !idle_sessions.is_empty() {
                    if let Err(error) = session_worker::sign_out_for_update(idle_sessions, &op).await {
                        return (Err(error), None);
                    }
                }
                let result = match kind {
                MaintenanceKind::Check | MaintenanceKind::Update | MaintenanceKind::Repair => {
                    let mode = match kind {
                        MaintenanceKind::Check => updater::Mode::Check,
                        MaintenanceKind::Update => updater::Mode::Update,
                        _ => updater::Mode::Repair,
                    };
                    // An Update asked the official server for its version; the interface shows
                    // that answer instead of sending a request of its own. Check is offline.
                    let folder = install.clone();
                    // USER data first, before any file changes (features::backups).
                    if mode != updater::Mode::Check && update_game && !preview {
                        let (config, install) = (backup_config.clone(), install.clone());
                        let kind = if mode == updater::Mode::Update { features::backups::Maintenance::Update } else { features::backups::Maintenance::Repair };
                        let _ = tokio::task::spawn_blocking(move || features::runtime::backup_before_maintenance_logged(&config, &install, kind)).await;
                    }
                    let pol_folder = if update_viewer { patch::install_folder(updater::Product::Viewer) } else { None };
                    let result = match (pol_folder.clone(), update_viewer, asked) {
                        (None, true, _) => Err(updater::CombinedFailure::from(t("PlayOnline Viewer is not installed. Run official setup and select PlayOnline Viewer.").to_owned())),
                        (Some(pol), true, Some(official)) => updater::update_viewer_after_connect(pol, official, op).await,
                        (pol, ..) => updater::run_combined(pol, update_game.then_some(install), mode, op).await,
                    };
                    // Refresh POL even after a partial failure: its verified
                    // transaction may have completed before the game stopped.
                    if let Some(pol) = pol_folder {
                        let remote = if mode == updater::Mode::Check { previous_viewer_version } else { match &result {
                            Ok(report) => report.viewer.as_ref().map(|viewer| viewer.version.clone()),
                            Err(error) => error.viewer_version.clone().or(previous_viewer_version) } };
                        let _ = tokio::task::spawn_blocking(move || {
                            // An Update or Repair asked the server (Check sent nothing): remember it.
                            if let Some(version) = remote.as_deref().filter(|_| mode != updater::Mode::Check && !preview) {
                                official_versions::learn_playonline_at(&official_path, version, official_versions::now());
                            }
                            let _ = sender.send(WorkerEvent::Viewer(viewer_generation, viewer::after_check(pol, remote.as_deref())));
                        }).await;
                    } else if update_viewer {
                        let _ = sender.send(WorkerEvent::Viewer(viewer_generation, viewer::unavailable(
                            t("PlayOnline Viewer is not installed. Run official setup and select PlayOnline Viewer.").into())));
                    }
                    return match result {
                        Ok(report) => {
                            let summary = report.summary();
                            let shown = match report.game {
                                Some(game) => tokio::task::spawn_blocking(move || patch::report_after_operation(&folder, &game, mode)).await.ok(),
                                None => None,
                            };
                            (Ok(summary), shown)
                        }
                        Err(error) => (Err(error.message), None),
                    };
                }
                MaintenanceKind::Download => installer::download(dir, installer_region, op).await,
                MaintenanceKind::Extract | MaintenanceKind::Setup => {
                    tokio::task::spawn_blocking(move || {
                        op.check_cancelled()?;
                        // No stage: the page names the step from `kind`.
                        op.progress(crate::operation::Stage::None, 0, 0, dir.display().to_string());
                        if kind == MaintenanceKind::Extract {
                            installer::extract(&dir, installer_region)?;
                            Ok(tf("Extractor finished. Choose Run setup. If you extracted elsewhere, browse to that {folder} folder first.",
                                &[("folder", &installer::official(installer_region).folder)]))
                        } else {
                            installer::install(&dir, installer_region)?;
                            Ok(t("Setup closed. Crystal will look for the installed FFXI folder. PlayOnline Viewer installs only when selected in the wizard. Then press Update to install the official updates.").into())
                        }
                    }).await.unwrap_or_else(|_| Err(t("Installer worker could not complete.").into()))
                }
                };
                (result, None)
            },
            move |(result, shown)| Message::MaintenanceFinished(kind, result, shown),
        )
    }

    /// Starts reading the news feeds in the UI language showing now, unless a
    /// read for that language is already running. A read for the other language
    /// (the user just switched) is left to finish unheard: its updates carry its
    /// language and are dropped.
    fn start_news(&mut self) -> Effect<Message> {
        let lang = i18n::current();
        if self.news_loading && self.news_lang == lang {
            return Effect::none();
        }
        self.news_loading = true;
        self.news_lang = lang;
        self.news_generation = self.news_generation.wrapping_add(1);
        let generation = self.news_generation;
        self.refreshed = Instant::now();
        let sender = self.sender.clone();
        let cache = news_cache_path(&self.config, lang);
        Effect::perform(
            services::refresh_with_progress(lang, Some(cache), move |update| {
                let _ = sender.send(WorkerEvent::Services(generation, lang, update));
            }),
            move |result| Message::ServicesLoaded(generation, lang, result),
        )
    }

    /// How often the official status is read again: each minute while FFXI is down.
    fn status_interval(&self) -> Duration {
        Duration::from_secs(if self.service_down() { 60 } else { 300 })
    }

    fn poll_service_status(&mut self) -> Effect<Message> {
        let interval = self.status_interval();
        if self.preview || self.news_loading || self.refreshed.elapsed() < interval {
            return Effect::none();
        }
        let lang = i18n::current();
        self.news_loading = true;
        self.news_lang = lang;
        self.news_generation = self.news_generation.wrapping_add(1);
        let generation = self.news_generation;
        self.refreshed = Instant::now();
        Effect::perform(services::refresh_status(lang), move |result| Message::ServiceStatusLoaded(generation, lang, result))
    }

    fn refresh(&mut self) -> Effect<Message> {
        if self.preview {
            return Effect::none();
        }
        self.refreshed = Instant::now();
        Effect::batch([self.start_news(), self.read_installed_version()])
    }

    fn find_installation(&mut self, automatic: bool) -> Effect<Message> {
        if self.preview || self.maintenance.is_some() || self.installation_loading {
            return Effect::none();
        }
        self.installation_generation = self.installation_generation.wrapping_add(1);
        let generation = self.installation_generation;
        self.installation_loading = true;
        let preferred = automatic.then(|| PathBuf::from(&self.settings.install_path));
        if !automatic { self.status = t("Finding FINAL FANTASY XI…").into(); }
        Effect::perform(async move {
            tokio::task::spawn_blocking(move || patch::find_game_installation(preferred.as_deref()))
                .await.ok().flatten()
        }, move |folder| Message::InstallationFound(generation, automatic, folder))
    }

    fn read_installed_version(&mut self) -> Effect<Message> {
        if self.preview { return Effect::none(); }
        self.patch_loading = true;
        self.patch_generation += 1;
        let generation = self.patch_generation;
        let install = PathBuf::from(&self.settings.install_path);
        // Reads the installed record only. The server is asked when Play is pressed.
        Effect::perform(
            async move {
                tokio::task::spawn_blocking(move || patch::local_report(&install))
                    .await
                    .unwrap_or_else(|e| patch::PatchReport {
                        state: patch::PatchState::Unknown,
                        local_version: t("Unavailable").into(),
                        remote_version: t("Not checked").into(),
                        detail: tf("Could not read local patch metadata: {error}", &[("error", &e)]),
                        files_verified: None,
                    })
            },
            move |r| Message::PatchLoaded(generation, r),
        )
    }

    fn cancel_account_drag(&mut self) {
        let drag = std::mem::take(&mut self.account_drag);
        if drag.source.is_some() {
            account_order::apply(&mut self.accounts, &drag.original, |row| row.key.as_str());
        }
    }

    fn finish_account_drag(&mut self) {
        let drag = std::mem::take(&mut self.account_drag);
        if drag.source.is_none() { return; }
        let order: Vec<_> = self.accounts.iter().map(|row| row.key.clone()).collect();
        if order == drag.original { return; }
        let mut settings = self.settings.clone();
        settings.account_order = order;
        if !self.preview {
            if let Err(error) = save_settings(&self.config, &settings) {
                account_order::apply(&mut self.accounts, &drag.original, |row| row.key.as_str());
                self.status = tf("Account order could not be saved: {error}", &[("error", &error)]);
                return;
            }
        }
        self.settings = settings;
        self.status = t("Account order saved.").into();
        self.features_order_changed();
    }

    fn replace_accounts(&mut self, values: Vec<Account>) {
        self.cancel_account_drag();
        let mut old: HashMap<String, AccountRow> = std::mem::take(&mut self.accounts)
            .into_iter()
            .map(|r| (r.key.clone(), r))
            .collect();
        let mut seen = BTreeSet::new();
        for mut account in values {
            let key = account_key(&account);
            if let Some(names) = self.settings.account_pol_handles.get(&key) {
                account.pol_handle_names = names.clone();
            }
            if !seen.insert(key.clone()) {
                continue;
            }
            let mut row = if let Some(mut row) = old.remove(&key) {
                // A changed (or removed) authenticator key is tried afresh.
                if row.account.totp_key != account.totp_key { row.authenticator_refused = false; }
                row.account = account;
                row
            } else {
                AccountRow {
                    selected: self
                        .settings
                        .selected_accounts
                        .as_ref()
                        .is_none_or(|s| s.contains(&key)),
                    key,
                    status: t("Ready to launch").into(),
                    account,
                    otp: String::new(),
                    busy: false,
                    signing_out: false,
                    shutting_down: false,
                    shutdown_mode: session_worker::game_shutdown::Mode::Graceful,
                    preparing_session: false,
                    sign_in_done: None,
                    queued_play: None,
                    batch_pending: false,
                    batch_requested: false,
                    launch_generation: 0,
                    note: None,
                    note_on: String::new(),
                    pid: None,
                    host_created: None,
                    alive: None,
                    game_running: None,
                    log: None,
                    outcome: None,
                    problem: false,
                    timing: String::new(),
                    play_timing: None,
                    exit: None,
                    launch_trigger: None,
                    network_wait: None,
                    otp_spent: false,
                    otp_unknown: false,
                    presign_login: false,
                    authenticator_refused: false,
                    game_seen: false,
                    resign_declined: false,
                    signed_out: false,
                    resign: None,
                }
            };
            if !row.batch_requested {
                row.batch_pending = session_worker::batch_pending(&self.config, &row.account);
            }
            let reconnect = if row.pid.is_none() && !row.busy && !row.shutting_down {
                session_worker::find(&self.config, &row.account)
            } else {
                None
            };
            let key = row.key.clone();
            if let Some(session) = reconnect {
                row.busy = true;
                row.preparing_session = session.preparing_social();
                row.sign_in_done = None;
                // A code spent in an earlier window is still spent (no sign-in after its game).
                // An older worker's record does not say; its "no code" teaches nothing.
                let used = session.otp_used();
                (row.otp_spent, row.otp_unknown) = (used.unwrap_or(false), used.is_none());
                // Whatever that window started it as, this one did not sign it in.
                row.presign_login = false;
                // A reconnect is no Play: nothing an earlier batch left classifies its end.
                row.launch_trigger = None;
                row.launch_generation = row.launch_generation.wrapping_add(1);
                let generation = row.launch_generation;
                row.status = t("Reconnecting to the background session…").into();
                let tx = LaunchTx::new(self.sender.clone(), key.clone(), generation);
                let spawned = std::thread::Builder::new().name("session-reconnect".into()).spawn(move || {
                    let result = session.reconnect(|message| tx.progress(message));
                    tx.complete(result);
                });
                if spawned.is_err() {
                    row.busy = false;
                    row.status = t("Could not reconnect to the background session.").into();
                }
            }
            self.accounts.push(row);
        }
        account_order::apply(&mut self.accounts, &self.settings.account_order, |row| row.key.as_str());
        self.reset_missing_friends_account();
    }

    fn save_accounts(&mut self, values: Vec<Account>) -> Effect<Message> {
        if !self.vault_ready || self.vault_saving || self.preview {
            return Effect::none();
        }
        self.vault_saving = true;
        self.status = t("Saving account…").into();
        let config = self.config.clone();
        Effect::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    backend::save_accounts(&config, &values)?;
                    Ok(values)
                })
                .await
                .unwrap_or_else(|_| Err(t("Could not save accounts.").into()))
            },
            Message::AccountsSaved,
        )
    }

    /// A Play clicked now.
    fn play(&mut self, keys: Vec<String>) {
        self.start_launch(keys, false, Instant::now(), None);
    }

    /// A sign-in only (Connect, a new account's "Sign in now") clicked now.
    fn connect(&mut self, keys: Vec<String>) {
        self.start_launch(keys, true, Instant::now(), None);
    }

    /// Local Viewer readiness for the UI. Stock sends no W2U (:54000) without a Connect, so
    /// launcher start and setup never query the server; each sign-in asks once (07c §3).
    fn start_viewer_check(&mut self) -> Effect<Message> {
        if self.preview || self.viewer_loading { return Effect::none(); }
        self.viewer_loading = true;
        self.viewer_generation += 1;
        let generation = self.viewer_generation;
        Effect::batch([
            Effect::perform(async move {
                tokio::task::spawn_blocking(viewer::local_status).await
                    .unwrap_or_else(|e| viewer::unavailable(e.to_string()))
            }, move |report| Message::ViewerLoaded(generation, report)),
            Effect::perform(async { tokio::task::spawn_blocking(|| retail_region::installed_editions(None)).await.unwrap_or_default() },
                Message::PolEditions),
        ])
    }

    /// The PlayOnline editions on this PC: the registry's, and US when the US Viewer was found.
    pub(crate) fn pol_editions(&self) -> Vec<retail_region::RetailRegion> {
        let mut editions = self.pol_editions.clone();
        if self.viewer.as_ref().is_some_and(|status| status.folder.is_some()) && !editions.contains(&retail_region::RetailRegion::Us) {
            editions.insert(0, retail_region::RetailRegion::Us);
        }
        editions
    }

    /// The region Downloads installs: the one chosen there, else an installed client's, else US.
    pub(crate) fn installer_region(&self) -> retail_region::RetailRegion {
        self.settings.installer_region.and_then(retail_region::RetailRegion::from_account)
            .or_else(|| self.pol_editions().first().copied())
            .unwrap_or(retail_region::RetailRegion::Us)
    }

    /// The installer folder follows the region while it is one of the default folders; a folder
    /// the user picked stays.
    fn follow_installer_region(&mut self) {
        if retail_region::RetailRegion::ALL.into_iter().any(|region| self.settings.installer_path == settings::installer_folder(region)) {
            self.settings.installer_path = settings::installer_folder(self.installer_region());
        }
    }

    /// A retail account whose region is not an installed PlayOnline edition: what to set it to.
    pub(crate) fn region_mismatch(&self, account: &Account) -> Option<String> {
        (account.mode == AccountMode::Retail).then(|| retail_region::mismatch(account.region, &self.pol_editions())).flatten()
    }

    fn set_viewer_status(&mut self, generation: u64, report: viewer::Status) {
        if generation == self.viewer_generation { self.viewer_loading = false; self.viewer = Some(report); }
        self.reload_official();
    }

    fn coordinated_keys(&self, keys: &[String]) -> Vec<String> {
        let mut seen = BTreeSet::new();
        keys.iter().filter(|key| seen.insert((*key).clone())).filter(|key| {
            self.accounts.iter().any(|row| row.key == **key && row.can_play()
                && row.account.mode == AccountMode::Retail && self.settings.launch_plan(&row.account).is_some())
        }).cloned().collect()
    }

    fn batch_sessions(&self, keys: Vec<String>) -> Vec<(String, Option<session_worker::BackgroundSession>)> {
        keys.into_iter().map(|key| {
            let session = self.accounts.iter().find(|row| row.key == key).and_then(|row| {
                let config = self.settings.config_for(&self.config, &row.account);
                session_worker::find(&config, &row.account)
            });
            (key, session)
        }).collect()
    }

    fn start_together(&mut self, mut keys: Vec<(String, Option<session_worker::BackgroundSession>)>, clicked: Instant, trigger: launch_timing::LaunchTrigger) {
        // In Home order, whatever order they were picked in: the coordinator starts the games
        // in the order it is given, so their taskbar buttons come up in Home order.
        keys.sort_by_key(|(key, _)| self.accounts.iter().position(|row| row.key == *key));
        self.log_play_click("batch", &keys.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>());
        self.prefetch_startup_files(&keys.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>());
        let mut jobs = Vec::new();
        let mut generations = HashMap::new();
        for (key, session) in keys {
            let Some(row) = self.accounts.iter_mut().find(|row| row.key == key
                && (row.can_play() || row.queued_signin_play() && row.queued_play == Some(clicked))) else { continue; };
            let Some(mut via) = self.launch_tools.launch_plan(&self.settings, &row.account) else { continue; };
            via.trigger = trigger;
            row.launch_trigger = Some(trigger);
            row.network_wait = None;
            let config = self.settings.config_for(&self.config, &row.account);
            // The pre-sign-in already used the row's code unless a new one was typed since (or an
            // authenticator key makes a fresh one for any second sign-in).
            let presign_otp_spent = row.otp_spent && row.otp.is_empty() && !row.authenticator_ready();
            row.otp_spent = presign_otp_spent || !row.otp.is_empty();
            // An existing session keeps what is known of its own sign-in (a pre-sign-in's refusal
            // still arrives here); a new one is this Play's own sign-in.
            let job = match session {
                Some(session) => session_worker::BatchAccount::existing(key.clone(), config, row.account.clone(),
                    std::mem::take(&mut row.otp), via, &session).with_otp_spent(presign_otp_spent),
                None if row.busy || row.pid.is_some() => {
                    row.queued_play = None;
                    row.problem = true;
                    row.status = t("The background session ended before Play could start. Sign in again.").into();
                    continue;
                }
                None => {
                    (row.otp_unknown, row.presign_login) = (false, false);
                    session_worker::BatchAccount::new(key.clone(), config, row.account.clone(), std::mem::take(&mut row.otp), via)
                }
            };
            jobs.push(job);
            row.signed_out = false;
            row.game_seen = false;
            row.resign_declined = false;
            row.launch_generation = row.launch_generation.wrapping_add(1);
            generations.insert(key, row.launch_generation);
            row.batch_pending = true;
            row.batch_requested = true;
            row.busy = true;
            row.preparing_session = false;
            row.sign_in_done = None;
            row.queued_play = None;
            row.problem = false;
            row.exit = None;
            row.timing.clear();
            row.play_timing = Some(launch_timing::Timeline::begin_at(clicked));
            row.status = t("Preparing accounts to start FFXI together…").into();
        }
        let count = jobs.len();
        if count == 0 { return; }
        // A batch signs its accounts in itself (cold): their parked hosts close (07d §1.5).
        let batch_keys: Vec<String> = jobs.iter().map(|job| job.key.clone()).collect();
        self.close_parks_for(&batch_keys);
        // The batch owner is another headless Crystal process. The interface only
        // receives its progress; its lifetime does not control the game handoff.
        let sender = self.sender.clone();
        let worker_generations = generations.clone();
        let spawned = std::thread::Builder::new().name("xi-launch-together".into()).spawn(move || {
            let mut completed = BTreeSet::new();
            let result = session_worker::launch_together(jobs, clicked, |key, message| {
                if let Some(generation) = worker_generations.get(key) {
                    let _ = sender.send(WorkerEvent::Progress(key.to_owned(), message.to_owned()).for_launch(key, *generation));
                }
            }, |key, result| {
                completed.insert(key.to_owned());
                if let Some(generation) = worker_generations.get(key) {
                    let _ = sender.send(WorkerEvent::Complete(key.to_owned(), result).for_launch(key, *generation));
                }
            });
            if let Err(error) = result {
                for (key, generation) in worker_generations {
                    if !completed.contains(&key) {
                        let _ = sender.send(WorkerEvent::Complete(key.clone(), Err(error.clone())).for_launch(&key, generation));
                    }
                }
            }
        });
        if spawned.is_err() {
            for key in generations.keys() {
                if let Some(row) = self.accounts.iter_mut().find(|row| &row.key == key) {
                    row.batch_pending = false;
                    row.batch_requested = false;
                    row.busy = false;
                    row.play_timing = None;
                    row.problem = true;
                    row.status = t("Could not start the coordinated launch worker.").into();
                }
            }
            self.status = t("Could not start the coordinated launch worker.").into();
        } else {
            self.status = tf("Preparing {count} accounts in parallel; FFXI will start when every account is ready.", &[("count", &count)]);
        }
    }

    /// `trigger`: what started this launch, for its log (07d §4.2); `None` names it by the
    /// path it takes (Play, Connect, pre-signed, queued or batch Play).
    /// Every Play path (single, pre-signed, queued behind a pre-sign-in, batch, a reissue)
    /// comes through here and is held until this window's first status read has finished.
    /// A sign-in only (startup, Connect) is never gated: PlayOnline is up during FFXI
    /// maintenance and a stock user can sign in then.
    fn start_launch(&mut self, keys: Vec<String>, social_only: bool, clicked: Instant, trigger: Option<launch_timing::LaunchTrigger>) {
        // Private-server accounts have no PlayOnline sign-in, so nothing below (the Viewer's
        // update, FFXI's maintenance, the coordinated batch) applies: they start at once, and a
        // sign-in only (startup, Connect, after a game) leaves them alone.
        let (private, keys): (Vec<String>, Vec<String>) = keys.into_iter().partition(|key|
            self.accounts.iter().any(|row| row.key == *key && row.account.mode == AccountMode::Private));
        if !social_only {
            for key in &private { self.start_private(key, clicked); }
        }
        if keys.is_empty() { return; }
        self.prefetch_startup_files(&keys);
        // While PlayOnline updates for sign-ins its W2U stopped (or is about to), another
        // sign-in would only be stopped the same way: it waits for that one update instead.
        let keys = if self.viewer_update_pending() {
            let (joining, keys): (Vec<String>, Vec<String>) = keys.into_iter().partition(|key|
                self.accounts.iter().any(|row| row.key == *key && row.pid.is_none() && !row.busy && row.can_play()
                    && row.account.mode == AccountMode::Retail));
            let sign_in = social_only.then(|| trigger.unwrap_or(launch_timing::LaunchTrigger::Connect));
            for key in &joining { self.note_refused(key, clicked, sign_in); }
            if keys.is_empty() { return; }
            keys
        } else { keys };
        // A Play on a session with a maintenance check out waits for its answer
        // (`start_queued_plays`): one W20 at a time.
        let (held, keys): (Vec<String>, Vec<String>) = keys.into_iter().partition(|key| !social_only && self.patch_watch.in_flight(key));
        for row in self.accounts.iter_mut().filter(|row| held.contains(&row.key)) {
            row.queued_play = Some(clicked);
            row.status = t("Waiting for the update…").into();
        }
        if keys.is_empty() { return; }
        if !social_only && self.hold_for_status(&keys, clicked, trigger) { return; }
        self.start_launch_now(keys, social_only, clicked, trigger, false);
    }

    /// A sign-in of `key` waits for PlayOnline's update (its W2U stopped it, or it was started
    /// while that update was pending): the update starts it again when it succeeds. `sign_in`:
    /// `None` for a Play, else what started the sign-in only. A Play is never replaced by a
    /// sign-in only.
    fn note_refused(&mut self, key: &str, clicked: Instant, sign_in: Option<launch_timing::LaunchTrigger>) {
        // The running update starts the ones it took; one not started yet, those still waiting.
        let waiting = if self.viewer_update_running() { &mut self.reissue_after_update } else { &mut self.refused_plays };
        match waiting.iter_mut().find(|known| known.key == key) {
            Some(known) if sign_in.is_some() && known.sign_in.is_none() => {}
            Some(known) => (known.clicked, known.sign_in) = (clicked, sign_in),
            None => waiting.push(RefusedSignIn { key: key.to_owned(), clicked, sign_in }),
        }
        // (The row that asks to sign accounts out keeps its question.)
        let asks = self.play_updates.viewer_confirm.as_ref().is_some_and(|(asking, _)| asking == key);
        if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && !row.busy && !asks) {
            row.status = t("Waiting to update PlayOnline…").into();
            row.problem = false;
        }
        if self.maintenance.is_none() { self.status = t("Waiting to update PlayOnline…").into(); }
    }

    /// PlayOnline alone is being updated (a sign-in's update, or Downloads' when only
    /// PlayOnline needs one).
    pub(crate) fn viewer_update_running(&self) -> bool {
        self.maintenance == Some(MaintenanceKind::Update) && self.update_scope == (true, false)
    }

    /// Sign-ins wait for a PlayOnline update that is running or about to start.
    fn viewer_update_pending(&self) -> bool {
        !self.refused_plays.is_empty() || self.viewer_update_running()
    }

    /// `key`'s sign-in waits for PlayOnline's update: `Some(true)` while that update runs,
    /// `Some(false)` while it is still to start.
    pub(crate) fn viewer_update_of(&self, key: &str) -> Option<bool> {
        if self.viewer_update_running() && self.reissue_after_update.iter().any(|refused| refused.key == key) { return Some(true); }
        self.refused_plays.iter().any(|refused| refused.key == key).then_some(false)
    }

    /// Whether `key`'s sign-ins take a one-time code, as its last successful one did (or as a
    /// code-less one the login refused suggests). Kept in Settings (only this yes or no, never a
    /// code), so a later window knows it before any sign-in there: a "Wait and play" released
    /// then asks for a code instead of signing in without one, and only an account known to
    /// need none is signed in by Crystal itself (`Settings::presigns`).
    fn note_otp_account(&mut self, key: &str, uses: bool) {
        if self.settings.note_sign_in(key, uses) && !self.preview {
            // Unsaved, it still holds for this window.
            let _ = save_settings(&self.config, &self.settings);
        }
    }

    /// The game folder `key`'s launches use when it is not the Settings one (the account's
    /// own folder), for the Update its W20 refusal asks for.
    fn account_game_folder(&self, key: &str) -> Option<PathBuf> {
        let row = self.accounts.iter().find(|row| row.key == key)?;
        let folder = self.settings.config_for(&self.config, &row.account).game_install?;
        let plain = |path: &Path| backend::plain_windows_path(path).to_string_lossy().trim_end_matches(['\\', '/']).to_ascii_lowercase();
        let settings = PathBuf::from(&self.settings.install_path);
        (!self.settings.install_selected || plain(&folder) != plain(&settings)).then_some(folder)
    }

    /// Why updating the FFXI folder `target` is on hold: a private-server account plays from it
    /// and its server's last manifest says the new client isn't approved yet.
    fn update_hold(&self, target: &Path) -> Option<String> {
        if self.preview { return None; }
        self.accounts
            .iter()
            .filter(|row| row.account.mode == AccountMode::Private)
            .filter(|row| play_update::same_folder(&self.settings.config_for(&self.config, &row.account).game_directory(), target))
            .find_map(|row| server_manifest::update_hold(&self.config, &row.account.server))
    }

    /// The game folder an operation of `kind` changes when it is not the Settings one: an
    /// Update the W20 of an account with its own folder asked for updates that folder.
    fn update_account_folder(&self, kind: MaintenanceKind) -> Option<PathBuf> {
        self.patch_folder.clone().filter(|_| kind == MaintenanceKind::Update && self.update_scope().1)
    }

    /// A W20 verdict for `key`'s game folder becomes the window's FFXI report.
    fn set_patch_from_row(&mut self, key: &str, report: patch::PatchReport) {
        self.patch_folder = self.account_game_folder(key);
        self.show_patch(report);
        self.reload_official();
    }

    /// `report` becomes the window's FFXI report; a read still in flight no longer replaces it.
    fn show_patch(&mut self, report: patch::PatchReport) {
        self.patch_generation += 1;
        self.patch_loading = false;
        self.patch = Some(report);
    }

    /// `status` becomes the window's PlayOnline report; a read still in flight no longer replaces it.
    fn show_viewer(&mut self, status: viewer::Status) {
        self.viewer_generation += 1;
        self.viewer_loading = false;
        self.viewer = Some(status);
    }

    /// The game folder `row`'s Play uses (its own, else the Settings one), when it is one.
    fn play_folder(&self, row: &AccountRow) -> Option<PathBuf> {
        pol_less::install_directory(&self.settings.config_for(&self.config, &row.account)).ok()
    }

    /// `row`'s game folder is being updated for a Play (its own or another account's).
    fn folder_updating(&self, row: &AccountRow) -> bool {
        !self.play_updates.jobs.is_empty()
            && self.play_folder(row).is_some_and(|folder| self.play_updates.for_folder(&folder).is_some())
    }

    /// Every signed-in account of `folder`, for an update's wait before installing.
    fn update_members(&self, folder: &Path) -> Vec<play_update::Member> {
        self.accounts.iter().filter(|row| row.pid.is_some())
            .filter(|row| self.play_folder(row).is_some_and(|own| play_update::same_folder(&own, folder)))
            .map(|row| {
                let config = self.settings.config_for(&self.config, &row.account);
                play_update::Member { key: row.key.clone(), label: row.account.label.clone(), host: row.pid,
                    session: (!self.preview).then(|| session_worker::find(&config, &row.account)).flatten(),
                    playing: row.is_playing() }
            }).collect()
    }

    /// Play is the update (proposal 09 phase 2): the W20 of `key`'s Play said its game folder
    /// needs an update (`report`). Its session stays signed in at the main menu while the
    /// folder updates (one job per folder: another account's refusal joins it), then the game
    /// starts with no second version check. `false` when no in-session update applies (no
    /// session, no official version, a file operation running): the row keeps the refusal.
    /// `batch`: a batch's refusal, which every account of the batch was told; one whose own
    /// folder is already that version starts on its own instead. `probe`: maintenance's version
    /// check asked (patch_watch): the same update runs with no Play waiting on it, so no game
    /// starts after it.
    fn begin_play_update(&mut self, key: &str, report: &patch::PatchReport, clicked: Instant, batch: bool, probe: bool) -> bool {
        if self.preview || self.closing { return false; }
        let Some(row) = self.accounts.iter().find(|row| row.key == key && row.pid.is_some()) else { return false };
        let Some(folder) = self.play_folder(row) else { return false };
        // A private server's update hold (`update_hold`): the row keeps the refusal and says why.
        if let Some(hold) = self.update_hold(&folder) {
            if probe {
                launch_log::append_history(&self.config.history_path(), "Crystal", &format!("maintenance update: held, {hold}"), None);
            } else if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) {
                row.status = hold;
                row.problem = true;
            }
            return false;
        }
        let Some(official) = patch::in_session_update(&folder, report) else {
            // A batch account whose own folder is current was refused for another's update.
            let current = !probe && batch && report.state == patch::PatchState::Required && !row.is_playing()
                && patch::installed_for_request(&folder) == report.remote_version;
            if current { self.start_launch_now(vec![key.to_owned()], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay), false); }
            return current;
        };
        let directory = self.settings.config_for(&self.config, &row.account).account_directory(&row.account);
        // The W20 ran in a session worker or the batch coordinator: its FIN spaces the update's
        // first connection (one in this window noted its own).
        let closed = if self.preview { None } else { patch::take_update_close(&directory) };
        let members = self.update_members(&folder);
        if let Some(job) = self.play_updates.for_folder_mut(&folder) {
            if !probe && !job.has(key) { job.waiters.push(play_update::Waiter { key: key.to_owned(), clicked, fallback: false }); }
            *job.members.lock().unwrap_or_else(|e| e.into_inner()) = members;
            if let Some(at) = closed { updater::note_patch_closed(updater::Product::Ffxi, at); }
        } else {
            // A Downloads operation holds the folder (or PlayOnline's): the refusal stands.
            if self.maintenance.is_some() || self.recovering.is_some() { return false; }
            let job = self.play_updates.begin(folder.clone(), key, official.clone(), clicked, members, probe);
            let (generation, op, shared) = (job.generation, job.op.clone(), job.members.clone());
            // The update reads and writes the game folder: its I/O, not the start-up pre-read's.
            startup_prefetch::cancel();
            #[cfg(test)]
            { let _ = (op, shared); self.play_updates.spawned.push((folder.clone(), official.clone(), closed)); }
            #[cfg(not(test))]
            if let Err(error) = play_update::spawn(generation, folder.clone(), official.clone(), closed, op, shared, self.sender.clone()) {
                self.play_updates.take(generation);
                self.status = error;
                return false;
            }
            #[cfg(test)]
            let _ = generation;
            if probe {
                launch_log::append_history(&self.config.history_path(), "Crystal",
                    &format!("maintenance update: started, official {official}"), None);
            }
        }
        if probe {
            self.status = t("Downloading the FFXI update").into();
            return true;
        }
        if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) {
            row.busy = true;
            row.preparing_session = false;
            row.problem = false;
            row.status = t("Updating FINAL FANTASY XI…").into();
        }
        self.status = tf("{account}: FINAL FANTASY XI needs an update to {version}. Updating it now; you stay signed in.",
            &[("account", &self.accounts.iter().find(|row| row.key == key).map_or("", |row| row.account.label.as_str())), ("version", &official)]);
        true
    }

    /// A Play of `key` while its folder updates for another account: it waits at the main
    /// menu (a cold Play signs in first) and `start_queued_plays` releases it afterwards.
    fn wait_for_folder_update(&mut self, key: &str, clicked: Instant) {
        let cold = self.accounts.iter().any(|row| row.key == key && row.pid.is_none() && !row.busy);
        if cold { self.start_launch_now(vec![key.to_owned()], true, clicked, None, false); }
        if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && (row.pid.is_some() || row.busy)) {
            row.queued_play = Some(clicked);
            row.problem = false;
            row.status = t("Waiting for the update…").into();
        }
    }

    /// What a Play-driven update's thread reported.
    fn play_update_event(&mut self, generation: u64, event: play_update::Event) {
        use play_update::Event;
        let Some(job) = self.play_updates.get_mut(generation) else { return };
        match event {
            Event::Waiting(on) => job.blocked = Some(on),
            Event::Installing => { job.blocked = None; job.installing = true; }
            Event::SignedOut(key) => {
                match job.waiters.iter_mut().find(|waiter| waiter.key == key) {
                    Some(waiter) => waiter.fallback = true,
                    None => job.signed_out.push(key),
                }
            }
            Event::Finished(result) => {
                if let Some(job) = self.play_updates.take(generation) { self.finish_play_update(job, result); }
            }
        }
    }

    fn finish_play_update(&mut self, job: play_update::Job, result: Result<play_update::Done, String>) {
        // Installed, or cancelled: nothing more until the next flip. Failed (a network drop, a
        // full disk): checked again on the schedule, resuming from the verified downloads.
        if result.is_err() && !job.cancelled { self.patch_watch.checked(&job.folder, false, Instant::now(), patch_watch::jitter()); }
        else { self.patch_watch.updated(&job.folder, result.is_ok()); }
        if job.probe {
            let line = match &result {
                Ok(done) => format!("maintenance update: installed {}: {}", job.official, done.summary),
                Err(error) => format!("maintenance update: stopped: {error}"),
            };
            launch_log::append_history(&self.config.history_path(), "Crystal", &line, None);
        }
        match result {
            Ok(done) => {
                if let Some(report) = done.report {
                    self.patch_folder = self.account_game_folder(&job.owner);
                    self.show_patch(report);
                }
                self.learn_from_update(&job.folder, Some(&job.official), None);
                self.status = if self.patch_watch.installed(&job.folder) { t("Update installed · waiting for servers").into() }
                    else { tf("FINAL FANTASY XI updated: {summary}", &[("summary", &done.summary)]) };
                for key in &job.signed_out {
                    if let Some(row) = self.accounts.iter_mut().find(|row| row.key == *key && !row.busy) {
                        row.status = t("Signed out of PlayOnline so the update could install.").into();
                    }
                    // Held at the main menu again for the worlds' opening (maintenance_sign_ins).
                    self.patch_watch.forget_signed(key);
                }
                for waiter in job.waiters {
                    let Some(row) = self.accounts.iter_mut().find(|row| row.key == waiter.key) else { continue };
                    row.busy = false;
                    let signed_in = row.pid.is_some() && !waiter.fallback;
                    if !signed_in && wants_typed_code(&self.settings, row) && row.otp.is_empty() {
                        // Its host was signed out for the install: a new sign-in needs a new code.
                        row.status = t("FINAL FANTASY XI is updated. Enter a new one-time code, then press Play.").into();
                        row.problem = true;
                        continue;
                    }
                    // Still signed in: the game starts through the signed-in path with no second
                    // W20 (`skip_w20`). Signed out (the fallback): one new sign-in, as a Play.
                    // The skip is this start's only: one that waits, pauses or is refused here
                    // runs its own W20 when it goes ahead later.
                    self.start_launch_now(vec![waiter.key], false, waiter.clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay), signed_in);
                }
            }
            Err(error) => {
                // Cancel, a network drop, or anything else: the verified downloads stay, and the
                // next Play continues from them.
                let paused = t("Update paused. Press Play to continue.");
                for waiter in &job.waiters {
                    if let Some(row) = self.accounts.iter_mut().find(|row| row.key == waiter.key) {
                        row.busy = false;
                        row.problem = !job.cancelled;
                        row.play_timing = None;
                        row.status = paused.into();
                    }
                }
                // Plays that waited for it would only ask for it again: they wait for the user.
                // Not after maintenance's update, which no one asked for: their own W20 runs.
                for row in self.accounts.iter_mut().filter(|row| !job.probe && row.queued_play.is_some() && !job.has(&row.key)) {
                    let folder = pol_less::install_directory(&self.settings.config_for(&self.config, &row.account)).ok();
                    if folder.is_some_and(|folder| play_update::same_folder(&folder, &job.folder)) {
                        row.queued_play = None;
                        row.status = paused.into();
                    }
                }
                self.status = if job.cancelled { paused.into() } else { tf("Update paused: {error}", &[("error", &error)]) };
            }
        }
    }

    /// Cancel pauses a Play-driven update: what was downloaded stays verified for the next Play.
    fn cancel_play_update(&mut self, generation: u64) {
        let Some(job) = self.play_updates.get_mut(generation) else { return };
        job.op.cancel();
        job.cancelled = true;
        self.status = t("Pausing the update…").into();
    }

    /// Play is the PlayOnline update too. Sign-ins their W2U stopped because PlayOnline is out
    /// of date wait in `refused_plays` (a Play, a batch's accounts, a startup or maintenance
    /// sign-in alike); once nothing else is signing in, PlayOnline updates once for all of them
    /// (its own update only, `start_viewer_update`) and each starts again when it succeeds
    /// (`reissue_after_update`). Looked at after every sign-in's result and each Tick, so the
    /// accounts of one multi-account Play share one update whichever is refused first.
    /// Idle accounts must be signed out for it (their hosts load PlayOnline's files): a row
    /// asks once. A running game is never closed for it: the rows say so and stop waiting.
    fn tend_viewer_update(&mut self) {
        if self.refused_plays.is_empty() || self.preview || self.closing { return; }
        let accounts = &self.accounts;
        self.refused_plays.retain(|refused| accounts.iter().any(|row| row.key == refused.key));
        if self.refused_plays.is_empty() { self.play_updates.viewer_confirm = None; return; }
        // One file operation at a time: this waits for the one running.
        if self.maintenance.is_some() || !self.play_updates.jobs.is_empty() || self.launch_blocked()
            || !self.vault_ready || self.vault_saving { return; }
        let game = self.accounts.iter().any(|row| row.is_playing());
        // Sign-ins and game starts in flight reach their own verdict first.
        if !game && self.accounts.iter().any(|row| row.busy || row.batch_pending || row.shutting_down || row.signing_out
            || row.queued_play.is_some()) { return; }
        // While a row asks, the sessions it asks about are read again once a second at most.
        if self.play_updates.viewer_confirm.is_some() && self.viewer_tended.is_some_and(|at| at.elapsed() < VIEWER_TEND_EVERY) { return; }
        self.viewer_tended = Some(Instant::now());
        match self.update_sessions().filter(|_| !game) {
            None => {
                self.play_updates.viewer_confirm = None;
                self.refuse_viewer_update(t("PlayOnline needs an update. Close FINAL FANTASY XI on every account, then press Play.").into());
            }
            Some(sessions) if !sessions.is_empty() => {
                let asked = self.play_updates.viewer_confirm.as_ref().is_some_and(|(key, count)|
                    *count == sessions.len() && self.refused_plays.iter().any(|refused| refused.key == *key));
                if asked { return; }
                let key = self.refused_plays[0].key.clone();
                self.play_updates.viewer_confirm = Some((key.clone(), sessions.len()));
                if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) {
                    row.status = tf("PlayOnline must sign out {count} accounts to update.", &[("count", &sessions.len())]);
                    row.problem = true;
                }
            }
            Some(_) => self.start_viewer_update(),
        }
    }

    /// PlayOnline's update for the sign-ins that wait for it; its task goes out with the next
    /// Tick. An update that cannot start says why on their rows, and they stop waiting.
    fn start_viewer_update(&mut self) {
        self.play_updates.viewer_confirm = None;
        // Another file operation began meanwhile: the sign-ins keep waiting (`tend_viewer_update`).
        if self.maintenance.is_some() || self.launch_blocked() || !self.vault_ready { return; }
        let task = self.start_maintenance_scoped(MaintenanceKind::Update, true);
        if self.viewer_update_running() {
            self.play_updates.tasks.push(task);
            launch_log::append_history(&self.config.history_path(), "Crystal", &format!("PlayOnline update: started for {} sign-ins, official {}",
                self.reissue_after_update.len(), self.viewer_target.as_deref().unwrap_or("to be asked")), None);
            for refused in &self.reissue_after_update {
                if let Some(row) = self.accounts.iter_mut().find(|row| row.key == refused.key && !row.busy) {
                    row.status = t("Updating PlayOnline").into();
                    row.problem = false;
                }
            }
        } else {
            self.refuse_viewer_update(self.status.clone());
        }
    }

    /// PlayOnline is updated: every sign-in that waited for it starts again as what it was. The
    /// accounts of one Play (a batch shares its click) start together again; a sign-in only
    /// starts as what started it, when that still applies. Each is a new sign-in (the stopped
    /// one sent no credentials): an account that takes a typed one-time code asks for a new one.
    fn reissue_refused(&mut self, reissue: Vec<RefusedSignIn>) {
        use launch_timing::LaunchTrigger;
        let now = Instant::now();
        let mut starts: Vec<(Vec<String>, bool, Instant, LaunchTrigger)> = Vec::new();
        for refused in reissue {
            let settings = &self.settings;
            let Some(row) = self.accounts.iter_mut().find(|row| row.key == refused.key && !row.busy && row.pid.is_none()) else { continue };
            if wants_typed_code(settings, row) && row.otp.is_empty() {
                row.status = t("PlayOnline is updated. Enter a new one-time code, then press Play.").into();
                row.problem = true;
                continue;
            }
            if !row.can_play() { continue; }
            row.status = t("Ready to launch").into();
            row.problem = false;
            let (social_only, clicked, trigger) = match refused.sign_in {
                None => (false, refused.clicked, LaunchTrigger::ReissuedPlay),
                Some(trigger) => (true, now, trigger),
            };
            match starts.iter_mut().find(|(_, social, at, known)| *social == social_only && *at == clicked && *known == trigger) {
                Some((keys, ..)) => keys.push(refused.key),
                None => starts.push((vec![refused.key], social_only, clicked, trigger)),
            }
        }
        for (keys, social_only, clicked, trigger) in starts {
            // Crystal's own sign-ins only while it may still make them (the switch, a code
            // learned since, the worlds' state).
            let keys: Vec<String> = match trigger {
                _ if !social_only => keys,
                // The maintenance watch signs them in again by its own rules (`maintenance_sign_ins`).
                LaunchTrigger::MaintenanceSignIn => {
                    for key in &keys { self.patch_watch.forget_signed(key); }
                    Vec::new()
                }
                LaunchTrigger::SignInAfterGame | LaunchTrigger::SignInAfterGameRetry => keys.into_iter().filter(|key| self.resign_allowed(key)).collect(),
                trigger if trigger.is_presign() => keys.into_iter().filter(|key| self.presigned(key)).collect(),
                _ => keys,
            };
            if !keys.is_empty() { self.start_launch(keys, social_only, clicked, Some(trigger)); }
        }
    }

    /// PlayOnline's update cannot run now: the sign-ins that waited for it say why and wait no
    /// longer (nothing starts by itself later).
    fn refuse_viewer_update(&mut self, shown: String) {
        for refused in std::mem::take(&mut self.refused_plays) {
            if let Some(row) = self.accounts.iter_mut().find(|row| row.key == refused.key && !row.busy) {
                row.status = shown.clone();
                row.problem = true;
            }
        }
        self.status = shown;
    }

    /// The game folder the window's FFXI report describes.
    pub(crate) fn shown_game_folder(&self) -> PathBuf {
        self.patch_folder.clone().unwrap_or_else(|| PathBuf::from(&self.settings.install_path))
    }

    /// The official versions real checks have told, read again: the W20 and W2U run on
    /// other threads and in session workers, which write them where `official_versions` says.
    fn reload_official(&mut self) {
        if !self.preview { self.official = official_versions::load(&official_versions::path_for(&self.config)); }
    }

    /// An Update or Repair asked the server for its version (the updater's own request):
    /// remembered like a Play's check. Nothing is sent for this.
    fn learn_from_update(&mut self, folder: &Path, game: Option<&str>, playonline: Option<&str>) {
        if self.preview { return; }
        let path = official_versions::path_for(&self.config);
        let now = official_versions::now();
        if let Some(version) = game { official_versions::learn_game_at(&path, folder, version, now); }
        if let Some(version) = playonline { official_versions::learn_playonline_at(&path, version, now); }
        self.reload_official();
    }

    /// The remembered official version for the FFXI folder on screen.
    pub(crate) fn learned_game(&self) -> Option<&official_versions::Learned> {
        self.official.game(&self.shown_game_folder())
    }

    /// Holds a Play clicked before the first status read (in flight, or not yet started)
    /// instead of gating it on a missing or cached status. Off the interface thread: the read
    /// is async, and `release_held_plays` runs when it lands or at STATUS_HOLD_LIMIT.
    fn hold_for_status(&mut self, incoming: &[String], clicked: Instant, trigger: Option<launch_timing::LaunchTrigger>) -> bool {
        if self.preview || self.status_read_done || self.status_hold_spent { return false; }
        let held: BTreeSet<&String> = self.held_plays.iter().flat_map(|play| &play.keys).collect();
        let keys: Vec<String> = incoming.iter().filter(|key| !held.contains(key)).cloned().collect();
        // Nothing new to hold: a repeat click on held keys stays held; nothing at all goes on to
        // the normal refusal instead of a "checking" line nothing will clear.
        if keys.is_empty() { return incoming.iter().any(|key| held.contains(key)); }
        let checking = t("Checking whether FINAL FANTASY XI is up…");
        let mut rows = Vec::new();
        for row in self.accounts.iter_mut().filter(|row| keys.contains(&row.key)) {
            rows.push((row.key.clone(), std::mem::replace(&mut row.status, checking.into())));
            row.problem = false;
        }
        let before = std::mem::replace(&mut self.status, checking.into());
        let status = if before == checking { self.held_plays.first().map(|play| play.status.clone()).unwrap_or_default() } else { before };
        self.held_plays.push(HeldPlay { keys, clicked, trigger, since: Instant::now(), rows, status });
        true
    }

    /// Held Plays go through the normal gate once the first status read has finished; one held
    /// for STATUS_HOLD_LIMIT proceeds as without a read (Unknown never blocks). An account save
    /// in progress keeps them held (`start_launch_now` would drop them), and an account deleted
    /// meanwhile is left out.
    fn release_held_plays(&mut self, now: Instant) {
        if self.held_plays.is_empty() || self.vault_saving { return; }
        let read = self.status_read_done;
        let (due, waiting): (Vec<HeldPlay>, Vec<HeldPlay>) = std::mem::take(&mut self.held_plays).into_iter()
            .partition(|play| read || now.saturating_duration_since(play.since) >= STATUS_HOLD_LIMIT);
        self.held_plays = waiting;
        if !read && !due.is_empty() { self.status_hold_spent = true; }
        let checking = t("Checking whether FINAL FANTASY XI is up…");
        for play in due {
            // Rows that still say they are checking get back what they said; the launch (or its
            // refusal) replaces that.
            for (key, before) in play.rows {
                if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && row.status == checking) { row.status = before; }
            }
            if self.status == checking { self.status = play.status; }
            let keys: Vec<String> = play.keys.into_iter().filter(|key| self.accounts.iter().any(|row| row.key == *key)).collect();
            if !keys.is_empty() { self.start_launch_now(keys, false, play.clicked, play.trigger, false); }
        }
    }

    /// PlayOnline's folder and, for a Play, each launching account's game folder that holds
    /// an interrupted update's journal (one metadata read per folder). A sign-in only
    /// (startup, Connect) never touches a game folder, so its journals wait for a Play.
    fn interrupted_updates(&self, keys: &[String], social_only: bool) -> Vec<PathBuf> {
        let interrupted = |folder: &Path| std::fs::symlink_metadata(folder.join(".xi-updater").join("transaction.json")).is_ok();
        let mut folders: Vec<PathBuf> = self.viewer.as_ref().and_then(|status| status.folder.clone())
            .filter(|folder| interrupted(folder)).into_iter().collect();
        if social_only { return folders; }
        for row in self.accounts.iter().filter(|row| keys.contains(&row.key)) {
            if let Some(folder) = self.settings.config_for(&self.config, &row.account).game_install {
                if interrupted(&folder) && !folders.contains(&folder) { folders.push(folder); }
            }
        }
        folders
    }

    /// Finish or undo the interrupted updates off the UI thread, then start the held launch
    /// (`WorkerEvent::InterruptedUpdate`). Parked hosts hold PlayOnline's files, so they close
    /// first when PlayOnline's folder is among `folders` or `close_parks` asks (a retry after
    /// a park kept the liveness check from passing); a game folder alone leaves other
    /// accounts their parks.
    fn finish_interrupted_updates(&mut self, folders: Vec<PathBuf>, held: HeldLaunch, close_parks: bool) {
        let note = t("Finishing an interrupted update…");
        for row in self.accounts.iter_mut().filter(|row| held.keys.contains(&row.key) && !row.busy) {
            row.status = note.into();
            row.problem = false;
        }
        self.status = note.into();
        let pol_folder = self.viewer.as_ref().and_then(|status| status.folder.clone());
        let parks_closed = close_parks || pol_folder.as_ref().is_some_and(|pol| folders.contains(pol));
        self.recovering = Some(parks_closed);
        let parks = if parks_closed { self.parks.drain(None) } else { Vec::new() };
        let sender = self.sender.clone();
        let keys = held.keys.clone();
        let install = (!self.preview && self.settings.install_selected && !self.settings.install_path.trim().is_empty())
            .then(|| PathBuf::from(&self.settings.install_path));
        let spawned = std::thread::Builder::new().name("finish-update".into()).spawn(move || {
            let result = pol_less::park::close_workers_and_wait(parks, pol_less::park::EXIT_WAIT)
                .and_then(|()| folders.iter().try_for_each(|folder| updater::recover_interrupted(folder).map(|_| ())));
            // The window's reports said "interrupted": read them again.
            let (game, viewer) = if result.is_ok() {
                (install.map(|install| patch::local_report(&install)), Some(viewer::local_status()))
            } else { (None, None) };
            let _ = sender.send(WorkerEvent::InterruptedUpdate(Box::new(Recovered { held, folders, parks_closed, result, game, viewer })));
        });
        if spawned.is_err() {
            self.recovering = None;
            self.refuse_interrupted(&keys, t("Could not start the worker that finishes it.").into());
        }
    }

    fn refuse_interrupted(&mut self, keys: &[String], error: String) {
        let shown = tf("Could not finish an interrupted update: {error}", &[("error", &error)]);
        for row in self.accounts.iter_mut().filter(|row| keys.contains(&row.key) && !row.busy) {
            row.status = shown.clone();
            row.problem = true;
            row.queued_play = None;
        }
        self.status = shown;
    }

    fn interrupted_update_finished(&mut self, done: Recovered) {
        let Recovered { held, folders, parks_closed, result, game, viewer } = done;
        self.recovering = None;
        // Only a report that said "interrupted" is replaced: a server verdict stays.
        let interrupted = |state: patch::PatchState| state == patch::PatchState::Interrupted;
        if let Some(report) = game.filter(|_| self.patch_folder.is_none() && self.patch.as_ref().is_none_or(|p| interrupted(p.state))) {
            self.show_patch(report);
        }
        if let Some(status) = viewer.filter(|_| self.viewer.as_ref().is_none_or(|v| interrupted(v.version.state))) {
            self.show_viewer(status);
        }
        match result {
            // Never a loop: a journal that came back is reported, not finished again.
            Ok(()) if !self.interrupted_updates(&held.keys, held.social_only).is_empty() =>
                self.refuse_interrupted(&held.keys, t("Another update started meanwhile.").into()),
            Ok(()) => {
                let keys: Vec<String> = held.keys.into_iter().filter(|key| self.accounts.iter().any(|row| row.key == *key)).collect();
                if !keys.is_empty() { self.start_launch_now(keys, held.social_only, held.clicked, held.trigger, false); }
            }
            // A parked host (an elevated one cannot be inspected) may be what kept the
            // liveness check from passing: once more with the parks closed.
            Err(_) if !parks_closed && !self.parks.is_empty() => self.finish_interrupted_updates(folders, held, true),
            Err(error) => self.refuse_interrupted(&held.keys, error),
        }
    }

    /// This window's first status read has finished, whatever it found.
    fn note_status_read(&mut self) {
        self.status_read_done = true;
        self.release_held_plays(Instant::now());
    }

    /// A Play that finds FFXI down for maintenance (or every world down) waits for it instead of
    /// being refused (owner decision 2026-10-13, "Wait and play"): at a click, at a held Play's
    /// release and at a queued Play's release alike. The wait itself sends nothing but one read
    /// of the play flag a minute (`read_play_flag`); only while that flag's `$patch_enable` is 1
    /// does a waiting account that needs no code sign in, for the version check
    /// (`tend_patch_watch`). An account already waiting is not added again. A
    /// one-time code typed for the Play goes with it (`WaitingPlay::codes`).
    fn wait_for_service(&mut self, keys: &[String]) {
        let headline = self.wait_headline();
        let mut wait = WaitingPlay { keys: Vec::new(), otp: Vec::new(), armed: Instant::now(), codes: Vec::new() };
        for row in self.accounts.iter_mut().filter(|row| keys.contains(&row.key)) {
            row.queued_play = None;
            if !row.can_play() || row.account.mode != AccountMode::Retail || wait.keys.contains(&row.key)
                || self.waiting_plays.iter().any(|waiting| waiting.keys.contains(&row.key)) { continue; }
            if !row.otp.is_empty() {
                wait.codes.push((row.key.clone(), Zeroizing::new(std::mem::take(&mut row.otp))));
                wait.otp.push(row.key.clone());
            }
            if !row.busy {
                row.status = headline.into();
                row.problem = false;
            }
            wait.keys.push(row.key.clone());
        }
        if wait.keys.is_empty() { return; }
        self.waiting_plays.push(wait);
        self.status = headline.into();
    }

    /// Codes typed for waiting Plays are stale OTP_KEEP after the wait began: dropped (zeroed).
    fn expire_wait_codes(&mut self) {
        for wait in self.waiting_plays.iter_mut().filter(|wait| wait.armed.elapsed() > OTP_KEEP) { wait.codes.clear(); }
    }

    /// These accounts no longer wait; codes kept for them are dropped. Once nothing waits, the
    /// status line stops promising a start.
    fn unwait(&mut self, keys: &[String]) {
        for wait in &mut self.waiting_plays {
            wait.keys.retain(|key| !keys.contains(key));
            wait.otp.retain(|key| !keys.contains(key));
            wait.codes.retain(|(key, _)| !keys.contains(key));
        }
        self.waiting_plays.retain(|wait| !wait.keys.is_empty());
        let headlines = [t("FFXI is down for maintenance. Starting as soon as it's back…"), t("FFXI's worlds are down. Starting as soon as they're back…")];
        if self.waiting_plays.is_empty() && headlines.contains(&self.status.as_str()) {
            let note = self.service_note();
            self.status = if self.service_down() && !note.is_empty() { note }
                else { t("Ready. Select accounts or use Launch all. Games continue when you close Crystal.").into() };
        }
    }

    /// What a waiting Play says: maintenance, or every world down.
    pub(crate) fn wait_headline(&self) -> &'static str {
        if self.service_availability() == services::Availability::Outage {
            t("FFXI's worlds are down. Starting as soon as they're back…")
        } else {
            t("FFXI is down for maintenance. Starting as soon as it's back…")
        }
    }

    /// The line under a waiting row's headline: when the play flag was last read, and the
    /// server's own status line, or the official status the gate falls back on.
    pub(crate) fn wait_detail(&self) -> String {
        let Some(checked) = self.play_flag.checked() else { return String::new() };
        let time = checked.with_timezone(&chrono::Local).format("%H:%M:%S").to_string();
        let mut parts = vec![tf("Last checked: {time}", &[("time", &time)])];
        // The server's own line, or what the gate goes by (the official status, or an outage
        // the flag does not end).
        match self.play_flag.fresh(chrono::Utc::now()) {
            Some(play_flag::Flag::Down(Some(status))) => parts.push(status.clone()),
            _ => parts.push(self.service_note()),
        }
        parts.retain(|part| !part.is_empty());
        parts.join(" · ")
    }

    /// Cancel drops these accounts' waits; their rows say what an idle row says.
    fn cancel_wait(&mut self, keys: &[String]) {
        let keys: Vec<String> = keys.iter().filter(|key| self.is_waiting(key)).cloned().collect();
        self.unwait(&keys);
        for row in self.accounts.iter_mut().filter(|row| keys.contains(&row.key) && !row.busy) {
            row.problem = false;
            row.status = if row.pid.is_some() { t("Signed in to PlayOnline · FFXI is closed") } else { t("Ready to launch") }.into();
        }
    }

    /// One read of FFXI's play flag when one is due: only while a Play waits, one at a time,
    /// every minute, and at once when the first wait starts or the PC wakes (`play_flag::Schedule`).
    fn read_play_flag(&mut self) -> Effect<Message> {
        let now = Instant::now();
        if self.preview || !self.flag_schedule.due(self.flag_wanted(), now) { return Effect::none(); }
        self.flag_schedule.begin(now);
        Effect::perform(play_flag::read(), Message::PlayFlagLoaded)
    }

    /// The play flag is read while a Play waits or FFXI is in maintenance: the official status
    /// says so, or a fresh flag of 0 does. While the official status still shows maintenance
    /// after a flag of 1, the reads keep that 1 fresh.
    fn flag_wanted(&self) -> bool {
        !self.waiting_plays.is_empty() || self.patch_watch.active()
            || self.service_availability() == services::Availability::Maintenance
            || self.services.as_ref().is_some_and(|snapshot|
                snapshot.recent_status(chrono::Utc::now()).0 == services::Availability::Maintenance)
    }

    /// The update as soon as it is out (patch_watch.rs): while FFXI is in maintenance and the
    /// play flag's `$patch_enable` is 1, accounts Crystal may sign in by itself (and those whose
    /// Play waits, without a code) sign in and wait at the main menu, and each game folder's
    /// version check runs from one of them on the watch's schedule. Nothing outside maintenance,
    /// and only an affirmative end (a fresh flag of 1, or an official "up" that holds) ends it.
    fn tend_patch_watch(&mut self) {
        if self.preview { return; }
        let now = chrono::Utc::now();
        let seen = match (self.play_flag.fresh(now), self.service_availability()) {
            (Some(play_flag::Flag::Up), _) => patch_watch::Seen::Open,
            (_, services::Availability::Maintenance) => patch_watch::Seen::Closed,
            (_, services::Availability::Operational | services::Availability::PartialOutage) => patch_watch::Seen::Up,
            _ => patch_watch::Seen::Unknown,
        };
        let before = self.patch_watch.last_patch();
        self.patch_watch.observe(seen, self.play_flag.patch(now), Instant::now());
        if let (Some(now), true) = (self.patch_watch.last_patch(), self.patch_watch.last_patch() != before) {
            let was = before.map_or("unknown", |was| if was { "1" } else { "0" });
            launch_log::append_history(&self.config.history_path(), "Crystal",
                &format!("play flag: $patch_enable {was} → {}", u8::from(now)), None);
        }
        if !self.patch_watch.ready() { return; }
        self.maintenance_sign_ins();
        self.start_patch_checks();
    }

    /// Maintenance sign-ins, held at the main menu for the version check: once per account per
    /// trigger (the watch's cycle), through the startup sign-in's path (a refused login stops
    /// Crystal signing that account in; no network retry). Never an account that takes a code.
    fn maintenance_sign_ins(&mut self) {
        if !self.vault_ready || self.vault_saving || self.launch_blocked() || self.confirm_close || self.startup_signin_pending { return; }
        let candidates: Vec<String> = self.accounts.iter().filter(|row| {
            if self.patch_watch.signed(&row.key) { return false; }
            // Known to need no code, as Crystal's own sign-ins (`Settings::presigns`), or its
            // codes come from its authenticator key.
            let waits_without_code = self.waiting_plays.iter().any(|wait| wait.keys.contains(&row.key) && !wait.otp.contains(&row.key))
                && row.account.mode == AccountMode::Retail && row.otp.is_empty()
                && (row.authenticator_ready() || !row.otp_spent
                    && self.settings.account_no_otp.contains(&row.key) && !self.settings.account_otp.contains(&row.key));
            (self.presigned_row(row) || waits_without_code)
                && !self.startup_waiting.iter().any(|(key, _)| *key == row.key) && self.startup_row_free(&row.key)
        }).map(|row| row.key.clone()).collect();
        let keys: Vec<String> = candidates.into_iter().filter(|key| self.patch_watch.sign_in_once(key)).collect();
        if keys.is_empty() { return; }
        launch_log::append_history(&self.config.history_path(), "Crystal", &format!("maintenance sign-in: {} accounts", keys.len()), None);
        self.start_launch(keys, true, Instant::now(), Some(launch_timing::LaunchTrigger::MaintenanceSignIn));
    }

    /// The version checks that are due: one per game folder, from an account signed in and idle
    /// at the main menu that plays from it. Not while that folder updates or files change.
    fn start_patch_checks(&mut self) {
        if self.maintenance.is_some() || self.recovering.is_some() || self.launch_blocked() { return; }
        let now = Instant::now();
        let ready: Vec<(String, PathBuf)> = self.accounts.iter()
            .filter(|row| row.account.mode == AccountMode::Retail && account_row::signed_in(row) && row.queued_play.is_none()
                && !self.patch_watch.in_flight(&row.key))
            .filter_map(|row| Some((row.key.clone(), self.play_folder(row)?)))
            .collect();
        for (key, folder) in ready {
            if self.patch_watch.due(&folder, now) && self.play_updates.for_folder(&folder).is_none() {
                self.spawn_patch_check(key, folder);
            }
        }
    }

    /// One version check (W20 only) of `folder` from `key`'s session at its idle main menu.
    fn spawn_patch_check(&mut self, key: String, folder: PathBuf) {
        let Some(row) = self.accounts.iter().find(|row| row.key == key) else { return };
        let (config, account, log) = (self.settings.config_for(&self.config, &row.account), row.account.clone(), row.log.clone());
        self.patch_watch.begin_check(&folder, &key);
        self.status = t("Checking for the FFXI update").into();
        #[cfg(test)]
        { let _ = (config, account, log); self.patch_watch.checks.push((folder, key)); }
        #[cfg(not(test))]
        {
            let sender = self.sender.clone();
            let install = folder.clone();
            // Still ready after the wait for the main menu (maintenance, `$patch_enable` 1).
            let gate = self.patch_watch.gate();
            let thread_key = key.clone();
            let spawned = std::thread::Builder::new().name("xi-patch-check".into()).spawn(move || {
                let key = thread_key;
                let still_ready = || gate.load(std::sync::atomic::Ordering::SeqCst);
                let result = session_worker::check_at_main_menu(&config, &account, still_ready, || patch::check_before_launch_timed(&install))
                    .map(|(check, timing)| {
                        if let Some(log) = &log {
                            launch_log::append_launcher_lines(log, &launch_timing::check_lines("FFXI check (W20) in maintenance, no game start", &timing));
                        }
                        let current = matches!(check, patch::LaunchCheck::Start(_));
                        let (patch::LaunchCheck::Start(report) | patch::LaunchCheck::Update(report) | patch::LaunchCheck::Unverified(report)) = check;
                        (report, current, timing.phases())
                    });
                let _ = sender.send(WorkerEvent::PatchCheck(install, key, result));
            });
            if spawned.is_err() {
                self.patch_watch.answered(&key);
                self.patch_watch.checked(&folder, false, Instant::now(), patch_watch::jitter());
            }
        }
    }

    /// A maintenance version check answered: "update required" starts the update Play's W20
    /// would (no Play waits on it); anything else checks again on the watch's schedule.
    fn patch_checked(&mut self, folder: PathBuf, key: String, result: Result<(patch::PatchReport, bool, Option<String>), String>) {
        if self.preview { return; }
        self.patch_watch.answered(&key);
        let now = Instant::now();
        let checking = self.status == t("Checking for the FFXI update");
        let history = self.config.history_path();
        let mut required = false;
        let updating = match result {
            Err(error) => {
                launch_log::append_history(&history, "Crystal", &format!("maintenance update check (W20): not sent: {error}"), None);
                false
            }
            Ok((report, current, phases)) => {
                let verdict = if current { format!("current, {}", report.remote_version) }
                    else if report.state == patch::PatchState::Required {
                        format!("update required, installed {}, official {}", report.local_version, report.remote_version)
                    } else { format!("no verdict: {}", report.detail) };
                let phases = phases.map(|phases| format!(" ({phases})")).unwrap_or_default();
                launch_log::append_history(&history, "Crystal", &format!("maintenance update check (W20): {verdict}{phases}"), None);
                self.set_patch_from_row(&key, report.clone());
                // An answer that comes once the update may no longer be checked for (maintenance
                // over, `$patch_enable` 0) starts nothing: the next Play's own W20 does.
                required = !current && report.state == patch::PatchState::Required && self.patch_watch.ready();
                required && self.begin_play_update(&key, &report, now, false, true)
            }
        };
        self.patch_watch.checked(&folder, updating, now, patch_watch::jitter());
        if required && !updating {
            // Told to update with no update possible: no more W20s until the next flip.
            launch_log::append_history(&history, "Crystal", "maintenance update: not started; no more checks until $patch_enable flips", None);
            self.patch_watch.updated(&folder, false);
        }
        if checking && !updating { self.status = self.service_note(); }
    }

    /// FFXI is known to be up: a fresh play flag of 1, or without one an official verdict of up.
    /// Unknown (a stale or failed status read, no read since the PC woke) is not.
    fn service_up(&self) -> bool {
        matches!(self.service_availability(), services::Availability::Operational | services::Availability::PartialOutage)
    }

    /// Waiting Plays start once FFXI is known to be back (`service_up`), each group through the
    /// normal start path (`start_launch`: the multi-account gate and stagger, a sign-in where
    /// there is no session). `from_read`: a flag read has just landed. Between reads only a fresh
    /// play flag of 1 releases, so a reading that has just gone stale, or a PC that has just
    /// woken, waits for the read that is due instead of the official status deciding alone. An
    /// account that signs in with a one-time code starts only with a code typed within OTP_KEEP;
    /// otherwise it waits for the user (an older code is stale, and no code would be refused).
    fn release_waiting_plays(&mut self, from_read: bool) {
        if self.waiting_plays.is_empty() || self.preview { return; }
        let flag_up = self.play_flag.fresh(chrono::Utc::now()) == Some(&play_flag::Flag::Up);
        if !(from_read || flag_up) || !self.service_up() { return; }
        let otp_note = t("FFXI is back — enter your one-time password to start");
        for wait in std::mem::take(&mut self.waiting_plays) {
            // Checked for each group: starting one (an interrupted update it finishes first) can
            // pause launches, and the next group then keeps waiting.
            // A session with a maintenance check out waits for its answer: one W20 at a time.
            if !self.vault_ready || self.vault_saving || self.launch_blocked() || wait.keys.iter().any(|key| self.patch_watch.in_flight(key)) {
                self.waiting_plays.push(wait);
                continue;
            }
            let codes_fresh = wait.armed.elapsed() <= OTP_KEEP;
            let mut keys = Vec::new();
            for row in self.accounts.iter_mut().filter(|row| wait.keys.contains(&row.key)) {
                if row.pid.is_some() || row.busy {
                    keys.push(row.key.clone());
                    continue;
                }
                if codes_fresh && row.otp.is_empty() {
                    if let Some((_, code)) = wait.codes.iter().find(|(key, _)| *key == row.key) { row.otp = code.as_str().to_owned(); }
                }
                let otp = wait.otp.contains(&row.key) || row.otp_spent || !row.otp.is_empty()
                    || self.settings.account_otp.contains(&row.key) || row.authenticator_refused;
                if otp && !(codes_fresh && !row.otp.is_empty()) {
                    row.otp.zeroize();
                    // An authenticator key makes a fresh code in place of a stale typed one.
                    if row.authenticator_ready() {
                        keys.push(row.key.clone());
                        continue;
                    }
                    row.status = otp_note.into();
                    row.problem = true;
                    self.status = otp_note.into();
                } else {
                    keys.push(row.key.clone());
                }
            }
            if keys.is_empty() { continue; }
            // An update one of them needs follows its Play in its session (phase 2).
            self.start_launch(keys, false, Instant::now(), None);
        }
    }

    /// `skip_w20`: a signed-in start right after its session installed the update its W20 asked
    /// for (stock checks nothing between that update and GameStart). A start this defers
    /// (a wait, a pause, a held launch) runs its own W20 later.
    fn start_launch_now(&mut self, keys: Vec<String>, social_only: bool, clicked: Instant, trigger: Option<launch_timing::LaunchTrigger>,
        skip_w20: bool) {
        use launch_timing::LaunchTrigger;
        // A Play the downtime gate below sets waiting is not recorded: it waits, as it would.
        #[cfg(test)]
        if self.launches.is_some() && !self.windower_editor_open && (social_only || self.preview || !self.service_down()) {
            // A recorded start skips the W20 as the signed-in start would.
            if skip_w20 { self.w20_skipped.extend(keys.iter().cloned()); }
            if let Some(launches) = &mut self.launches { launches.push((keys, social_only, clicked, trigger)); }
            return;
        }
        if self.launch_blocked() {
            self.status = if self.windower_editor_open {
                t("Close Windower's profile editor before starting another session.")
            } else {
                t("Game launches are paused while files are being changed or official setup is running.")
            }.into();
            return;
        }
        if self.preview || !self.vault_ready || self.vault_saving {
            return;
        }
        if !social_only && self.service_down() {
            self.wait_for_service(&keys);
            return;
        }
        // A Play that goes ahead now replaces a wait for the same accounts (one whose release
        // was held back while launches were paused), so the wait cannot start them a second time.
        if !social_only { self.unwait(&keys); }
        // An interrupted update in a folder this launch uses is finished (or undone) first,
        // instead of refusing the launch; the launch then goes on by itself.
        let interrupted = self.interrupted_updates(&keys, social_only);
        if !interrupted.is_empty() {
            self.finish_interrupted_updates(interrupted, HeldLaunch { keys, social_only, clicked, trigger }, false);
            return;
        }
        // A Play whose game folder is updating for another account waits for that update before
        // its own version check: signed in at the main menu (a cold Play signs in first), then
        // released by `start_queued_plays` once the update is done.
        let keys = if social_only { keys } else {
            let (waiting, keys): (Vec<String>, Vec<String>) = keys.into_iter().partition(|key|
                self.accounts.iter().find(|row| row.key == *key && row.can_play())
                    .and_then(|row| self.play_folder(row)).is_some_and(|folder| self.play_updates.for_folder(&folder).is_some()));
            for key in waiting { self.wait_for_folder_update(&key, clicked); }
            if keys.is_empty() { return; }
            keys
        };
        // A signed-in session that cannot take this Play's plan (its launcher was changed since it
        // signed in) is refused on its own row: it must not fail a batch for the other accounts.
        let keys = if social_only { keys } else {
            let keys = self.refuse_changed_launchers(keys);
            if keys.is_empty() { return; }
            keys
        };
        if !social_only {
            let together = self.coordinated_keys(&keys);
            if together.len() > 1 {
                let sessions = self.batch_sessions(together.clone());
                let unpublished = sessions.iter().any(|(key, session)| session.is_none()
                    && self.accounts.iter().any(|row| row.key == *key && (row.busy || row.pid.is_some())));
                if !unpublished {
                    self.start_together(sessions, clicked, trigger.unwrap_or(LaunchTrigger::BatchPlay));
                    return;
                }
            }
            // A batch (and a Play queued behind a batch-capable pre-sign-in) is released to the
            // game by the separate batch-coordinator process when the sign-in reaches the main
            // menu. That process cannot see this window's status reads, so the gate above (after
            // the first-read hold) is the guard for it; a maintenance that begins between the
            // click and that release is not re-checked. A Play this window queues itself is
            // re-checked at its release (`start_queued_plays`).
            if let [key] = together.as_slice() {
                if self.accounts.iter().any(|row| row.key == *key && row.busy && row.preparing_session) {
                    let sessions = self.batch_sessions(together);
                    if sessions[0].1.as_ref().is_some_and(session_worker::BackgroundSession::supports_batch_start) {
                        self.start_together(sessions, clicked, trigger.unwrap_or(LaunchTrigger::QueuedPlay));
                        return;
                    }
                }
            }
        }
        let count = keys.len();
        if count == 0 {
            self.status = t("Choose an account that is ready to launch.").into();
            return;
        }
        // Launch timing counts from the Play click, not from when each worker thread gets going.
        // Which tools this batch starts, for the status line.
        let mut tools: Vec<&'static str> = Vec::new();
        let mut all_native = true;
        let mut private = Vec::new();
        // One worker per account: every sign-in starts now and runs in parallel; only the
        // Ashita/Windower start inside each worker waits its turn (pol_less::TurnGate: the
        // machine-wide windower.lock and ashita.lock in session_worker).
        for key in keys {
            let Some(row) = self
                .accounts
                .iter_mut()
                .find(|r| r.key == key && r.can_play())
            else {
                continue;
            };
            if row.busy {
                if !social_only {
                    row.queued_play = Some(clicked);
                    row.status = t("FFXI will start when sign-in finishes.").into();
                    // The pre-sign-in skips the Viewer pauses still to come (07d §4.3).
                    if !self.preview {
                        let (config, account) = (self.settings.config_for(&self.config, &row.account), row.account.clone());
                        let _ = std::thread::Builder::new().name("play-pending".into())
                            .spawn(move || { let _ = session_worker::note_play_pending(&config, &account); });
                    }
                }
                continue;
            }
            let account = row.account.clone();
            if account.mode == AccountMode::Private {
                // Every Play goes through `start_launch`, which starts these on their own.
                if !social_only { private.push(key.clone()); }
                continue;
            }
            // Signing in again must never turn an existing Friends session into a game launch.
            if social_only && row.pid.is_some() { continue; }
            let config = self.settings.config_for(&self.config, &account);
            if let Some(plan) = self.settings.launch_plan(&account) {
                if let Some(session) = session_worker::find(&config, &account) {
                    if let Err(error) = session.ensure_launch_plan(&plan) {
                        row.status = error;
                        row.problem = true;
                        continue;
                    }
                }
            }
            row.launch_generation = row.launch_generation.wrapping_add(1);
            let generation = row.launch_generation;
            row.signed_out = false;
            row.game_seen = false;
            row.resign_declined = false;
            if row.pid.is_some() {
                // The stock FFXI version check runs immediately before the game starts, here
                // as well as in a Play that signs in: off the interface thread, once the
                // session's Viewer main menu is up and idle (07c §5.3), then the request.
                let install = match pol_less::install_directory_for(&config, account.region) {
                    Ok(install) => install,
                    Err(error) => { row.status = error; row.problem = true; continue; }
                };
                row.busy = true;
                row.problem = false;
                row.timing.clear();
                row.play_timing = Some(launch_timing::Timeline::begin_at(clicked));
                row.status = t("Checking the game version…").into();
                let tx = LaunchTx::new(self.sender.clone(), key.clone(), generation);
                // the signed-in session's loader starts the game; put the wrapper
                // beside it (or take Crystal's copy away) before the start.
                let dgvoodoo = self.settings.dgvoodoo;
                let loader_setting = self.settings.loader_path.clone();
                let host = row.pid.map(|pid| HostId { pid, created: row.host_created });
                // The trigger and W20 lines go to the session's own log (07d §4.2, §5.3;
                // 08 §4 item 6: they were dropped on this thread).
                let log = row.log.clone();
                let trigger = trigger.unwrap_or(LaunchTrigger::PreSignedPlay);
                // This session's update just finished (phase 2): stock starts the game with no
                // second version check, and no patch-host lookup either.
                let region = account.region;
                let spawned = std::thread::Builder::new().name("xi-start-game".into()).spawn(move || {
                    if !skip_w20 { crate::dns::warm(retail_region::game_patch_host(region)); }
                    if let Some(log) = &log {
                        launch_log::append_launcher_lines(log, &pol_less::launch_clock_lines(&launch_timing::Timeline::begin_at(clicked), trigger));
                    }
                    if let Some(loader) = session_loader(host, &loader_setting) {
                        let _ = dgvoodoo::sync_for_launch(&loader, dgvoodoo);
                    }
                    let mut report = None;
                    let result = session_worker::start_signed_in(&config, &account, &mut |line| tx.progress(line), || {
                        if skip_w20 {
                            if let Some(log) = &log {
                                launch_log::append_launcher_lines(log, &["FFXI check (W20): skipped; this session just installed the update the Play's W20 asked for.".to_owned()]);
                            }
                            return Ok(t("FINAL FANTASY XI was updated in this session; starting it.").into());
                        }
                        // US: the stock check as always. EU/JP: the same frame to their region's
                        // name for the host, and an update left to the official Viewer.
                        let (check, timing) = patch::check_before_launch_at(&install, retail_region::game_patch_host(region));
                        if let Some(log) = &log {
                            launch_log::append_launcher_lines(log, &launch_timing::check_lines("FFXI check (W20)", &timing));
                        }
                        if region == 0 {
                            report = Some(match &check {
                                patch::LaunchCheck::Start(r) | patch::LaunchCheck::Update(r) | patch::LaunchCheck::Unverified(r) => r.clone(),
                            });
                        }
                        retail_region::game_gate(region, patch::gate(check))
                    });
                    tx.send(WorkerEvent::StartGame(tx.key.clone(), report, result));
                });
                if spawned.is_err() {
                    row.busy = false;
                    row.problem = true;
                    row.play_timing = None;
                    row.status = t("Could not start the game version check.").into();
                }
                continue;
            }
            // the account's own launcher, boot config and profile,
            // over the Launch tools defaults. All retail routes use the same session worker.
            let mut via = self.launch_tools.launch_plan(&self.settings, &account);
            let trigger = trigger.unwrap_or(if social_only { LaunchTrigger::Connect } else { LaunchTrigger::Play });
            // Named before a parked host renames it (ParkedConnect).
            let presign_login = social_only && trigger.is_presign();
            // Re-read the host's dependencies at Play: an edit can precede the next periodic
            // retirement. A mismatch closes off-thread and this Play signs in cold.
            let parked = via.as_ref().and_then(|plan| {
                let key = pol_less::park::ParkKey::for_plan(&account, plan);
                match self.parks.take_for_play(&row.key, &key, || pol_less::park::fingerprint(&config, plan)) {
                    Ok(worker) => worker,
                    Err(worker) => { close_parks_later(vec![worker]); None }
                }
            });
            let deliveries = self.parks.deliveries.clone();
            // A sign-in-only launch here is a pre-sign-in (startup or the explicit sign-in): its
            // Viewer pauses cost nothing at Play, so the loader restores them (07c §4, §5.4).
            if let Some(plan) = &mut via { plan.social_only = social_only; plan.viewer_gaps = social_only; plan.trigger = trigger; }
            // Cold if the parked host turns out to be gone: named as the click (07d §1.6).
            let cold = via.clone();
            let trigger = match (&parked, social_only) {
                (Some(_), false) => LaunchTrigger::ParkedPlay,
                (Some(_), true) => LaunchTrigger::ParkedConnect,
                (None, _) => trigger,
            };
            if let Some(plan) = &mut via { plan.trigger = trigger; }
            row.launch_trigger = Some(trigger);
            row.network_wait = None;
            let tool = via.as_ref().map_or(t("the native client"), |plan| plan.tool.name());
            all_native &= via.is_none();
            if !tools.contains(&tool) {
                tools.push(tool);
            }
            let viewer_generation = self.viewer_generation;
            // Wiped wherever the sign-in thread drops it (a gate refusal, a spawn failure).
            let mut otp = Zeroizing::new(std::mem::take(&mut row.otp));
            row.otp_spent = !otp.is_empty();
            (row.otp_unknown, row.presign_login) = (false, presign_login);
            row.busy = true;
            row.preparing_session = social_only;
            row.problem = false;
            row.timing.clear();
            row.play_timing = None;
            row.status = t("Starting sign-in…").into();
            let done = Arc::new(AtomicBool::new(false));
            row.sign_in_done = Some(done.clone());
            let tx = LaunchTx { done: Some(done), ..LaunchTx::new(self.sender.clone(), key.clone(), generation) };
            // installed here first when it isn't yet; the session worker then puts
            // it next to the loader it runs. A failed install never stops the launch.
            let dgvoodoo = self.settings.dgvoodoo && via.is_some();
            let reshade = self.settings.reshade.enabled;
            let result = std::thread::Builder::new()
                .name("xi-account-login".into())
                .spawn(move || {
                    // Local readiness only; the sign-in's own W2U runs at Connect (07c §3).
                    // Only a blocking local state replaces the Viewer row: a passing one has
                    // no server verdict and would erase the updater's.
                    // The gate is the US Viewer's: an EU/JP account signs in with its own
                    // region's Viewer, which `retail_region` finds and reads in the sign-in.
                    if retail_region::us_viewer_gates(account.mode == AccountMode::Retail, account.region) {
                        let viewer = viewer::local_status();
                        if let Err(error) = viewer::gate(&viewer) {
                            let _ = tx.tx.send(WorkerEvent::Viewer(viewer_generation, viewer));
                            tx.complete(Err(error));
                            return;
                        }
                    }
                    if dgvoodoo {
                        let installing = tx.clone();
                        let installed = dgvoodoo::ensure_installed_blocking(move |line| installing.progress(&line));
                        // ReShade cannot run without it: the launch stops here with one reason,
                        // before any sign-in work.
                        if let (Err(error), true) = (&installed, reshade) {
                            let error = format!("{} {error}", t("dgVoodoo is not installed yet."));
                            tx.complete(Err(error));
                            return;
                        }
                        if let Err(error) = installed {
                            let _ = tx.tx.send(WorkerEvent::Notice(tf(
                                "dgVoodoo could not be installed, so this launch uses the game's own DirectX 8. {error}",
                                &[("error", &error)],
                            )));
                        }
                    }
                    let mut progress = |line: &str| tx.progress(line);
                    let result = match (parked, via) {
                        (Some(worker), Some(plan)) => {
                            // Parked deliveries from this window go ≥ 1.5 s (+ jitter) apart (07a
                            // parity condition 3): N member logins never start within milliseconds.
                            // The slot is taken once the host has parked, where the credentials leave.
                            match worker.play(&config, account.clone(), std::mem::take(&mut *otp), plan, clicked, &deliveries, &mut progress) {
                                Ok(result) => result,
                                Err(otp) => session_worker::launch(&config, account, otp, cold, clicked, &mut progress),
                            }
                        }
                        (_, via) => session_worker::launch(&config, account, std::mem::take(&mut *otp), via, clicked, &mut progress),
                    };
                    tx.complete(result);
                });
            if result.is_err() {
                row.busy = false;
                row.preparing_session = false;
                row.sign_in_done = None;
                // The code never left: none was spent (it was wiped with the thread's closure).
                row.otp_spent = false;
                row.status = t("Could not start the sign-in worker.").into();
            }
        }
        self.status = launch_status(social_only, all_native, count, &tools);
        for key in private { self.start_private(&key, clicked); }
    }

    /// Start a private-server account's game: xiloader on its own, or through the account's
    /// Windower or Ashita, in the account's own session worker (`session_worker::launch_private`).
    fn start_private(&mut self, key: &str, clicked: Instant) {
        #[cfg(test)]
        if let Some(launches) = &mut self.launches {
            launches.push((vec![key.to_owned()], false, clicked, None));
        }
        // Unit tests never start a worker: the test binary is not Crystal.
        if cfg!(test) { return; }
        if self.launch_blocked() {
            self.status = if self.windower_editor_open {
                t("Close Windower's profile editor before starting another session.")
            } else {
                t("Game launches are paused while files are being changed or official setup is running.")
            }.into();
            return;
        }
        if self.preview || !self.vault_ready || self.vault_saving { return; }
        let hairpin = !self.settings.account_no_hairpin.contains(key);
        let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && row.can_play()) else { return };
        if row.busy || row.pid.is_some() { return; }
        let account = row.account.clone();
        let config = self.settings.config_for(&self.config, &account);
        let via = self.launch_tools.launch_plan(&self.settings, &account);
        let launcher = via.as_ref().map_or("xiloader", |plan| plan.tool.name());
        row.launch_generation = row.launch_generation.wrapping_add(1);
        let generation = row.launch_generation;
        row.signed_out = false;
        row.game_seen = false;
        row.resign_declined = false;
        row.launch_trigger = None;
        row.network_wait = None;
        row.busy = true;
        row.preparing_session = false;
        row.problem = false;
        row.timing.clear();
        row.play_timing = None;
        // xiloader takes the account's own one-time code when one was typed on its row.
        let otp = std::mem::take(&mut row.otp);
        row.otp_spent = !otp.is_empty();
        row.otp_unknown = false;
        row.status = format!("Starting the private-server client through {launcher}…");
        let label = account.label.clone();
        let tx = LaunchTx::new(self.sender.clone(), key.to_owned(), generation);
        let spawned = std::thread::Builder::new().name("xi-private-login".into()).spawn(move || {
            let result = session_worker::launch_private(&config, account, otp, via, hairpin, clicked, |line| tx.progress(line));
            tx.complete(result);
        });
        if spawned.is_err() {
            row.busy = false;
            row.problem = true;
            row.otp_spent = false;
            row.status = "Could not start the private-server launch worker.".into();
            return;
        }
        self.status = format!("{label}: starting the private-server client through {launcher}…");
    }

    /// The Play `keys` less those whose signed-in session cannot take this Play's plan (the
    /// account's launcher or its profile changed since it signed in): each of those rows says
    /// why (`ensure_launch_plan`) and is left free to play once signed out, and the others go on.
    fn refuse_changed_launchers(&mut self, keys: Vec<String>) -> Vec<String> {
        let refused: Vec<(String, String)> = keys.iter().filter_map(|key| {
            let row = self.accounts.iter().find(|row| row.key == *key && row.can_play())?;
            let plan = self.settings.launch_plan(&row.account)?;
            let config = self.settings.config_for(&self.config, &row.account);
            let error = session_worker::find(&config, &row.account)?.ensure_launch_plan(&plan).err()?;
            Some((key.clone(), error))
        }).collect();
        for (key, error) in &refused {
            if let Some(row) = self.accounts.iter_mut().find(|row| row.key == *key) {
                row.status = error.clone();
                row.problem = true;
            }
        }
        if let Some((key, error)) = refused.first() {
            let label = self.accounts.iter().find(|row| row.key == *key).map_or_else(String::new, |row| row.account.label.clone());
            self.status = tf("{account}: {message}", &[("account", &label), ("message", error)]);
        }
        keys.into_iter().filter(|key| !refused.iter().any(|(refused, _)| refused == key)).collect()
    }

    /// A capable worker can check the version during the rest of sign-in. Older
    /// loaders keep the signed-in path, with the same version and native idle gates.
    /// Each queued Play is released here, by this window, so availability is checked again at
    /// its release: one that a maintenance or outage begun since its click finds leaves its
    /// sign-in and waits for FFXI instead (`wait_for_service`), never left queued on it.
    fn start_queued_plays(&mut self) {
        if self.preview || !self.vault_ready || self.vault_saving || self.launch_blocked() { return; }
        // A Play waiting for its folder's update stays queued until that update is done.
        let preparing: Vec<_> = self.accounts.iter().filter_map(|row| {
            if !row.queued_signin_play() || row.account.mode != AccountMode::Retail
                || self.settings.launch_plan(&row.account).is_none() || self.folder_updating(row) { return None; }
            let config = self.settings.config_for(&self.config, &row.account);
            let session = session_worker::find(&config, &row.account)?;
            if !session.supports_batch_start() { return None; }
            Some((row.key.clone(), row.queued_play?, session))
        }).collect();
        if self.service_down() {
            let keys: Vec<String> = preparing.into_iter().map(|(key, _, _)| key)
                .chain(self.accounts.iter().filter(|row| row.queued_play.is_some() && !row.busy && row.pid.is_some() && !row.is_playing())
                    .map(|row| row.key.clone()))
                .collect();
            if !keys.is_empty() { self.wait_for_service(&keys); }
            return;
        }
        for (key, clicked, session) in preparing { self.start_together(vec![(key, Some(session))], clicked, launch_timing::LaunchTrigger::QueuedPlay); }
        let held: Vec<String> = self.accounts.iter()
            .filter(|row| row.queued_play.is_some() && (self.patch_watch.in_flight(&row.key) || self.folder_updating(row)))
            .map(|row| row.key.clone()).collect();
        let queued: Vec<_> = self.accounts.iter_mut().filter_map(|row| {
            if row.busy || row.pid.is_none() || row.is_playing() || held.contains(&row.key) { return None; }
            row.queued_play.take().map(|clicked| (row.key.clone(), clicked))
        }).collect();
        for (key, clicked) in queued { self.start_launch(vec![key], false, clicked, None); }
    }

    /// Consume the startup action once the vault and the installation are ready. Account
    /// refreshes and client exits never retry it. A row still busy with a previous session
    /// (its host closing, as at 18:59:55 in 08 §1 row 9) keeps its key pending until that
    /// session ends (`take_waiting_startups`); one already signed in and idle needs nothing.
    fn take_startup_signins(&mut self) -> Vec<String> {
        if !self.startup_signin_pending || !self.vault_ready || self.installation_loading || self.windower_editor_open { return Vec::new(); }
        self.startup_signin_pending = false;
        // The parks start PARK_START_DELAY after this, startup sign-ins or not (07d §5.7).
        let now = Instant::now();
        self.startup_issued = Some(now);
        if self.preview || self.vault_saving || self.launch_blocked() { return Vec::new(); }
        let keys: Vec<String> = self.accounts.iter().filter(|row| self.presigned_row(row)
            && !(row.pid.is_some() && !row.busy && !row.is_playing())).map(|row| row.key.clone()).collect();
        let mut start = Vec::new();
        for key in keys {
            if self.startup_row_free(&key) { start.push(key); continue; }
            if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) {
                row.status = t("Startup sign-in waits for the previous session to end.").into();
            }
            self.startup_waiting.push((key, now));
        }
        start
    }

    /// One `launch-history.log` line per window start, at the first account load (07d §4.4),
    /// so measurements can tell launcher restarts apart.
    fn log_launcher_start(&mut self) {
        if self.launcher_start_logged || self.preview { return; }
        self.launcher_start_logged = true;
        let startup = self.accounts.iter().filter(|row| self.presigned_row(row)).count();
        let parks = self.park_keys().len();
        let exe = std::env::current_exe().ok().and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned())).unwrap_or_default();
        launch_log::append_history(&self.config.history_path(), "Crystal", &format!("launcher start: pid {}, {exe}, v{}, startup sign-in {startup}, parks {parks}",
            std::process::id(), env!("CARGO_PKG_VERSION")), None);
        let keys: Vec<String> = self.accounts.iter().map(|row| row.key.clone()).collect();
        self.prefetch_startup_files(&keys);
    }

    /// Warm the file cache with the startup files of these rows' game folders
    /// (`startup_prefetch`): at Crystal's start and at every sign-in or Play. Not while any
    /// game runs (its folder is warm, and its I/O comes first) or files are being changed.
    fn prefetch_startup_files(&self, keys: &[String]) {
        // Not in unit tests: the pre-read is a detached, process-wide thread that appends to
        // the fixture's history log after the test has moved on (and removed its folder).
        if cfg!(test) { return; }
        if self.preview || self.launch_blocked() || self.maintenance.is_some() || self.accounts.iter().any(|row| row.is_playing()) { return; }
        let mut games: Vec<PathBuf> = Vec::new();
        for row in self.accounts.iter().filter(|row| keys.contains(&row.key)) {
            let game = self.settings.config_for(&self.config, &row.account).game_directory();
            if !games.contains(&game) { games.push(game); }
        }
        // One pre-read runs at a time; a second folder waits for the next sign-in.
        if let Some(game) = games.first() {
            let _ = startup_prefetch::start(game, &self.config.startup_prefetch_path(), &self.config.history_path());
        }
    }

    /// The accounts a parked warm host would serve (07d §1.5 "Who"): retail with a tool plan,
    /// no session of any kind, and no pre-sign-in coming (`presign_coming`). That is OTP
    /// accounts, ones not yet seen signing in, every account while the sign-in switch is off,
    /// and a pre-signed one Crystal will not sign in by itself now (a sign-in after its game
    /// refused during a maintenance, a sign-out, a startup sign-in that failed or was skipped):
    /// its next Play starts warm instead of cold. A pre-sign-in that comes later takes the park.
    fn park_keys(&self) -> Vec<(String, pol_less::park::ParkKey, pol_less::Plan)> {
        // A parked host loads PlayOnline's core: none while PlayOnline is known to need its
        // update (its core may be too old to park, and the update closes every park anyway).
        if self.viewer_update_pending() || self.viewer.as_ref().is_some_and(|status| status.version.state == patch::PatchState::Required) {
            return Vec::new();
        }
        self.accounts.iter()
            // An experimental EU/JP account signs in cold (`retail_region`): never parked.
            .filter(|row| row.account.mode == AccountMode::Retail && row.account.region == 0 && !row.signed_out && !self.presign_coming(row)
                && row.pid.is_none() && !row.busy && row.can_play())
            .filter_map(|row| {
                let plan = self.launch_tools.launch_plan(&self.settings, &row.account)?;
                Some((row.key.clone(), pol_less::park::ParkKey::for_plan(&row.account, &plan), plan))
            })
            .collect()
    }

    /// Crystal will sign this pre-signed row in by itself: the startup sign-ins are still to be
    /// taken, or one waits for a closing session, a sign-in after its game is pending, or either
    /// waits for the network to retry.
    fn presign_coming(&self, row: &AccountRow) -> bool {
        self.presigned_row(row) && (self.startup_signin_pending || row.resign.is_some()
            || self.startup_waiting.iter().any(|(key, _)| *key == row.key)
            || matches!(row.network_wait, Some(NetworkWait::StartupRetry | NetworkWait::AfterGame | NetworkWait::AfterGameRetry)))
    }

    /// The row has no session of any kind: no host, no worker, nothing queued or batched.
    fn startup_row_free(&self, key: &str) -> bool {
        self.accounts.iter().any(|row| row.key == key && row.pid.is_none() && !row.busy && row.can_play()
            && (self.preview || session_worker::find(&self.settings.config_for(&self.config, &row.account), &row.account).is_none()))
    }

    /// Pending startup sign-ins whose previous session has ended (07d §5.5 step 2); one still
    /// waiting after STARTUP_WAIT_LIMIT is dropped with a row status.
    fn take_waiting_startups(&mut self, now: Instant) -> Vec<String> {
        if self.startup_waiting.is_empty() || self.preview || !self.vault_ready || self.vault_saving || self.launch_blocked() { return Vec::new(); }
        let mut start = Vec::new();
        for (key, since) in std::mem::take(&mut self.startup_waiting) {
            // The switch turned off, or the account learned to need a code, meanwhile.
            if !self.presigned(&key) { continue; }
            // A row that was reconnecting to its previous session is now signed in and idle:
            // that session is what the startup sign-in would give; nothing to do or say.
            if self.accounts.iter().any(|row| row.key == key && row.pid.is_some() && !row.busy && !row.is_playing()) { continue; }
            if self.startup_row_free(&key) {
                start.push(key);
            } else if now.saturating_duration_since(since) >= STARTUP_WAIT_LIMIT {
                // A game still running keeps its own status ("continue in game").
                if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && !(row.pid.is_some() && !row.busy)) {
                    row.status = t("Startup sign-in skipped: a session is still running.").into();
                }
            } else {
                self.startup_waiting.push((key, since));
            }
        }
        start
    }

    /// Start startup sign-ins (or their one network retry) as pre-sign-ins.
    fn start_startup(&mut self, keys: Vec<String>, trigger: launch_timing::LaunchTrigger) {
        if !keys.is_empty() { self.start_launch(keys, true, Instant::now(), Some(trigger)); }
    }

    fn start_ready_startup_signins(&mut self) {
        let keys = self.take_startup_signins();
        self.start_startup(keys, launch_timing::LaunchTrigger::StartupSignIn);
    }

    /// Whether Crystal may sign `key` in by itself now (owner decision 2026-10-13): a retail
    /// account it knows signs in without a one-time code, with the switch on (`Settings::presigns`),
    /// whose row's last sign-in took no code and that has none typed. A code typed on the row is
    /// the user's, for their own Play: Crystal never signs in with a stored or stale one.
    fn presigned(&self, key: &str) -> bool {
        self.accounts.iter().any(|row| row.key == key && self.presigned_row(row))
    }

    /// An account with an authenticator key counts as one that needs no typed code: the switch
    /// alone decides, until PlayOnline refuses one of its codes (then never, until the key is
    /// edited or a sign-in without a typed code works).
    fn presigned_row(&self, row: &AccountRow) -> bool {
        let known = if row.account.totp_key.is_empty() {
            self.settings.presigns(&row.key) && !row.otp_spent
        } else {
            self.settings.auto_sign_in && !row.authenticator_refused
        };
        row.account.mode == AccountMode::Retail && !row.signed_out && known && row.otp.is_empty()
    }

    /// Whether `key` may sign back in after its game by itself (see `Resign`): a pre-signed
    /// account (`presigned`). An OTP account's code is spent; a login without one would be
    /// refused. Never during a known maintenance or outage: the end was the service's, and a
    /// login then is one no user makes (the pre-sign-in path skips start_launch's downtime gate).
    fn resign_allowed(&self, key: &str) -> bool {
        self.resign_eligible(key) && !self.service_down()
    }

    /// `resign_allowed` but for the service's state.
    fn resign_eligible(&self, key: &str) -> bool {
        self.settings.sign_in_after_game && self.presigned(key)
    }

    /// The row of a sign-in after a game that the maintenance or outage refused says so.
    fn note_resign_refused(&mut self, key: &str) {
        let note = tf("No sign-in after the game while the worlds are down. {note}", &[("note", &self.service_note())]);
        if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) { row.set_note(Some(RowNote::ResignRefused(note))); }
    }

    /// A session whose game ran has ended. A pre-signed account signs back in once that
    /// session has fully ended (`take_resigns`). A normal quit or FFXI's usual -14 waits only for
    /// RESIGN_DELAY; a crash or error first has the connectivity probe check the network, so a
    /// connection loss waits for it to return (07d §5.5). A worker that vanished without a report
    /// is left alone: its host may still be running. `host` is the ended session's loader.
    fn note_game_end(&mut self, key: &str, kind: Option<launch_log::ExitKind>, host: Option<HostId>) {
        use launch_log::ExitKind;
        if !self.settings.sign_in_after_game {
            self.startup_waiting.retain(|(waiting, _)| waiting != key);
            if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) {
                row.signed_out = true;
                row.drop_resign();
            }
            return;
        }
        if self.preview || kind == Some(ExitKind::WorkerEnded) { return; }
        if !self.resign_allowed(key) {
            if self.resign_eligible(key) {
                // A startup sign-in waiting for this session would otherwise run on the next
                // Tick: the very sign-in after the game this refuses.
                self.startup_waiting.retain(|(waiting, _)| waiting != key);
                self.note_resign_refused(key);
            }
            return;
        }
        let clean = matches!(kind, Some(ExitKind::Normal | ExitKind::Code { code: -14 }));
        // A startup sign-in still waiting for this session (Crystal opened during the game)
        // becomes this delayed sign-in after the game, not an immediate startup sign-in.
        self.startup_waiting.retain(|(waiting, _)| waiting != key);
        let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) else { return };
        row.resign = Some(Resign { generation: row.launch_generation, ended: Instant::now(), free_since: None, network: !clean, host });
        row.set_note(Some(RowNote::ResignAfterGame));
        if !clean {
            row.network_wait = Some(NetworkWait::AfterGame);
            let generation = row.launch_generation;
            self.spawn_network_probe(key.to_owned(), generation);
        }
    }

    /// Sign-ins after a game that start now: each once its row has been free (no host, no
    /// worker, nothing queued) for RESIGN_DELAY, reusing the startup sign-in's wait for a
    /// closing session (07d §5.5). One the user overtook (a launch since, a sign-out, an edit),
    /// no longer pre-signed (the switch turned off, or the account learned to need a code), or
    /// whose row stayed busy for STARTUP_WAIT_LIMIT is dropped.
    fn take_resigns(&mut self, now: Instant) -> Vec<String> {
        let keys: Vec<String> = self.accounts.iter().filter(|row| row.resign.is_some()).map(|row| row.key.clone()).collect();
        // Only while Crystal stays open: not while it asks to close.
        let held = self.preview || !self.vault_ready || self.vault_saving || self.launch_blocked() || self.confirm_close;
        let mut start = Vec::new();
        let mut refused = Vec::new();
        for key in keys {
            let allowed = self.resign_allowed(&key);
            let down = !allowed && self.resign_eligible(&key);
            let free = !held && self.startup_row_free(&key);
            let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) else { continue };
            let Some(mut resign) = row.resign else { continue };
            let waited = now.saturating_duration_since(resign.ended);
            if down && row.launch_generation == resign.generation {
                row.drop_resign();
                refused.push(key);
                continue;
            }
            // A connection loss waits for the probe (up to NETWORK_PROBE_LIMIT), whose
            // NetworkBack ends that wait either way and restarts `ended`; the slack only covers
            // a probe that never reports.
            if !allowed || row.launch_generation != resign.generation
                || resign.network && waited >= session_worker::NETWORK_PROBE_LIMIT + STARTUP_WAIT_LIMIT
                || !resign.network && resign.free_since.is_none() && waited >= STARTUP_WAIT_LIMIT {
                row.drop_resign();
                continue;
            }
            if resign.network { continue; }
            // Fully ended: the host process exited too, not only its worker record.
            let free = free && !resign.host.is_some_and(host_running);
            resign.free_since = free.then(|| resign.free_since.unwrap_or(now));
            row.resign = Some(resign);
            if resign.free_since.is_some_and(|since| now.saturating_duration_since(since) >= RESIGN_DELAY) {
                row.resign = None;
                start.push(key);
            }
        }
        for key in refused { self.note_resign_refused(&key); }
        start
    }

    /// Start sign-ins after a game (or their one network retry) as pre-sign-ins, together and
    /// so staggered like the startup sign-ins. A Play meanwhile queues behind them (07d §5.4).
    fn start_resigns(&mut self, keys: Vec<String>, trigger: launch_timing::LaunchTrigger) {
        if keys.is_empty() { return; }
        self.start_startup(keys.clone(), trigger);
        for row in self.accounts.iter_mut().filter(|row| row.busy && keys.contains(&row.key)) {
            row.set_note(Some(RowNote::ResignAfterGame));
        }
    }

    /// The parked warm hosts (07d §1.5, §5.7), on each Tick: their reports, then closing
    /// what is no longer wanted or changed, then at most one new park.
    fn tend_parks(&mut self, now: Instant) {
        use pol_less::park::{self, Note};
        if self.preview { return; }
        for (_, row, note) in self.parks.poll() {
            match note {
                Note::Parked(_) => {}
                // An older loader: expected, and not parked again until something changes.
                Note::Failed(why) if why == park::UNSUPPORTED => {}
                // An old PlayOnline core: no park works until PlayOnline is updated, so none
                // starts again (`park_keys`), and Play runs the update.
                Note::Failed(why) if viewer::core_check_outdated(&why) => {
                    let shown = viewer::stale(self.viewer.take());
                    let message = viewer::stale_message(&shown.version.local_version);
                    self.show_viewer(shown);
                    self.park_note(&row, &message);
                }
                Note::Failed(why) => self.park_note(&row, &why),
                Note::Ended => self.park_note(&row, t("The parked game host ended.")),
            }
        }
        // polcore.dll is loaded in a parked pol.exe: no parks while files change (07d §1.5).
        // A read-only installation scan (opening Downloads, Crystal start) only holds new parks
        // back below; closing for it sent a Play right after it down the cold path (live
        // 2026-10-12 23:17:55).
        if self.files_changing() {
            let closing = self.parks.drain(None);
            if !closing.is_empty() {
                launch_log::append_history(&self.config.history_path(), "Crystal", "warm hosts closed: game or Crystal files are changing", None);
            }
            close_parks_later(closing);
            // Re-tended at the Tick rate once the files settle.
            self.parks_settled = false;
            return;
        }
        // The profile editor needs the profile guard the Windower parks hold.
        if self.windower_editor_open { self.close_parks_in_background(Some(pol_less::Tool::Windower.name())); }
        if self.park_tended.is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(1)) { return; }
        self.park_tended = Some(now);
        // Nor while an interrupted update is being finished: no new host may join it.
        if !self.vault_ready || self.installation_loading || self.startup_signin_pending || self.recovering.is_some() { return; }
        // The startup sign-ins take the Windower turn first (§5.7).
        let Some(issued) = self.startup_issued else { return };
        if now.saturating_duration_since(issued) < park::PARK_START_DELAY { self.parks_settled = false; return; }
        let refresh = self.park_checked.is_none_or(|at| now.saturating_duration_since(at) >= park::FINGERPRINT_EVERY);
        if refresh { self.park_checked = Some(now); }
        let candidates: Vec<_> = self.park_keys().into_iter()
            .filter(|(_, _, plan)| !(self.windower_editor_open && plan.tool == pol_less::Tool::Windower))
            .filter_map(|(row, key, plan)| {
                let account = self.accounts.iter().find(|r| r.key == row)?.account.clone();
                let config = self.settings.config_for(&self.config, &account);
                session_worker::find(&config, &account).is_none().then_some((row, key, plan, account, config))
            })
            .collect();
        let mut wanted = Vec::new();
        for (row, key, plan, account, config) in candidates {
            if refresh || !self.park_prints.contains_key(&key) {
                self.park_prints.insert(key.clone(), park::fingerprint(&config, &plan));
            }
            let fingerprint = self.park_prints[&key].clone();
            wanted.push(park::Wanted { key, row, fingerprint, payload: (config, account, plan) });
        }
        self.park_prints.retain(|key, _| wanted.iter().any(|want| want.key == *key));
        let retired = self.parks.retire(&wanted);
        let mut closing = Vec::new();
        for (key, names, worker) in retired {
            let label = self.accounts.iter().find(|row| row.account.mode == AccountMode::Retail && account_key(&row.account) == key.account)
                .map_or_else(String::new, |row| row.account.label.clone());
            let note = if names.is_empty() { "warm host closed: account no longer idle or eligible".to_owned() } else { format!("re-park: {} changed", names.join(", ")) };
            launch_log::append_history(&self.config.history_path(), &label, &note, None);
            closing.push(worker);
        }
        close_parks_later(closing);
        if let Some(next) = self.parks.next(&wanted, now) {
            let (config, account, plan) = &next.payload;
            let (key, row, fingerprint) = (next.key.clone(), next.row.clone(), next.fingerprint.clone());
            match session_worker::spawn_park(config, account, plan) {
                Ok(worker) => self.parks.insert(key, row, fingerprint, worker),
                Err(error) => self.park_note(&row, &error),
            }
        }
        self.parks_settled = self.parks.settled(&wanted);
    }

    /// A non-blocking row note for a park that failed; Play still works, cold.
    fn park_note(&mut self, row: &str, why: &str) {
        if let Some(row) = self.accounts.iter_mut().find(|r| r.key == row && !r.busy && r.pid.is_none()) {
            row.status = tf("Warm start unavailable: {error}", &[("error", &why)]);
        }
    }

    /// Close parked hosts (`tool`: only that tool's) off the UI thread.
    fn close_parks_in_background(&mut self, tool: Option<&'static str>) {
        close_parks_later(self.parks.drain(tool));
    }

    fn controller_settings_targets(&self) -> Vec<(Config, Account)> {
        self.accounts.iter().filter(|row| row.account.mode == AccountMode::Retail && row.is_playing()
            && row.alive.as_ref().is_none_or(|alive| alive.load(Ordering::Acquire)))
            .map(|row| {
                let config = self.settings.config_for(&self.config, &row.account);
                (config, row.account.without_secrets())
            }).collect()
    }

    fn apply_controller_settings(&self, revision: u64, values: std::collections::BTreeMap<String, String>) -> Effect<Message> {
        let targets: Vec<_> = self.controller_settings_targets().into_iter().map(|(config, account)|
            session_worker::find(&config, &account).ok_or_else(||
                t("The game session ended before controller settings could be sent.").to_owned())).collect();
        if targets.is_empty() {
            return Effect::done(Message::Gamepad(gamepad::Msg::LiveResult(revision,
                Ok(t("Saved. Changes apply when you launch FFXI.").into()))));
        }
        Effect::perform(async move {
            tokio::task::spawn_blocking(move || {
                let count = targets.len();
                let results: Vec<_> = std::thread::scope(|scope| {
                    targets.into_iter().map(|session| {
                        let values = &values;
                        scope.spawn(move || session.and_then(|session|
                            session_worker::request_controller_settings_for_session(&session, values.clone())))
                    }).collect::<Vec<_>>().into_iter().map(|thread| thread.join()
                        .unwrap_or_else(|_| Err(t("Could not apply controller settings.").into()))).collect()
                });
                if let Some(error) = results.iter().find_map(|result| result.as_ref().err()) {
                    let applied = results.iter().filter(|result| result.is_ok()).count();
                    Err(tf("Saved. Applied to {applied} of {count} running games. {error}",
                        &[("applied", &applied), ("count", &count), ("error", error)]))
                } else {
                    Ok(tf("Saved. Applied to running games ({count}).", &[("count", &count)]))
                }
            }).await.unwrap_or_else(|_| Err(t("Saved. Could not confirm changes in running games.").into()))
        }, move |result| Message::Gamepad(gamepad::Msg::LiveResult(revision, result)))
    }

    /// Close every parked host now (exit, file changes): each worker ends Windower first,
    /// then its host; past CLOSE_WAIT it is terminated (07d §1.5 "Kill").
    fn close_parks(&mut self) {
        pol_less::park::close_workers(self.parks.drain(None));
    }

    /// The running games a shut-down can still end (only the checked accounts' with selected).
    pub(crate) fn shutdown_count(&self, selected: bool) -> usize {
        self.accounts.iter().filter(|row| (!selected || row.selected) && row.can_shut_down()).count()
    }

    /// Ends one account's game (only), the checked accounts' games, or every running game.
    fn shutdown_games(&mut self, selected: bool, only: Option<&str>) -> Effect<Message> {
        if self.closing { return Effect::none(); }
        let mode = session_worker::game_shutdown::Mode::from_setting(self.settings.close_games_instantly);
        let mut tasks = Vec::new();
        for row in &mut self.accounts {
            if (selected && !row.selected) || only.is_some_and(|key| row.key != key) || !row.can_shut_down() { continue; }
            let key = row.key.clone();
            let generation = row.launch_generation;
            let config = self.settings.config_for(&self.config, &row.account);
            let session = (!self.preview).then(|| session_worker::find(&config, &row.account)).flatten();
            row.game_seen = true;
            if !self.settings.sign_in_after_game {
                row.decline_resign();
                row.signed_out = true;
            }
            row.network_wait = None;
            row.shutting_down = true;
            row.shutdown_mode = mode;
            row.problem = false;
            row.status = row.status_text().into_owned();
            if self.preview {
                tasks.push(Effect::done(Message::GameShutdownFinished(key, generation, Ok(()))));
            } else {
                tasks.push(Effect::perform(async move {
                    tokio::task::spawn_blocking(move || match session {
                        Some(session) => mode.run(&session),
                        None => Err(t("The selected game session ended before shutdown could be requested.").into()),
                    }).await.unwrap_or_else(|_| Err(t("Could not request game shutdown.").into()))
                }, move |result| Message::GameShutdownFinished(key.clone(), generation, result)));
            }
        }
        Effect::batch(tasks)
    }

    fn sign_out_account(&mut self, key: String) -> Effect<Message> {
        self.startup_waiting.retain(|(waiting, _)| *waiting != key);
        let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) else { return Effect::none() };
        row.decline_resign();
        row.signed_out = true;
        row.network_wait = None;
        if !row.can_sign_out() { return Effect::none(); }
        let generation = row.launch_generation;
        let config = self.settings.config_for(&self.config, &row.account);
        let session = (!self.preview).then(|| session_worker::find(&config, &row.account)).flatten();
        row.busy = true;
        row.signing_out = true;
        row.preparing_session = false;
        row.problem = false;
        row.status = t("Signing out of PlayOnline…").into();
        if self.preview { return Effect::done(Message::AccountSignedOut(key, generation, Ok(()))); }
        Effect::perform(async move {
            tokio::task::spawn_blocking(move || match session {
                Some(session) => session.sign_out(),
                None => Ok(()),
            }).await.unwrap_or_else(|error| Err(error.to_string()))
        }, move |result| Message::AccountSignedOut(key.clone(), generation, result))
    }

    fn begin_close(&mut self) -> Effect<Message> {
        self.confirm_close = false;
        self.closing = true;
        startup_prefetch::cancel();
        self.startup_signin_pending = false;
        self.startup_waiting.clear();
        let mut sessions = Vec::new();
        for row in &mut self.accounts {
            row.decline_resign();
            row.signed_out = true;
            row.network_wait = None;
            if !self.preview {
                let config = self.settings.config_for(&self.config, &row.account);
                if let Some(session) = session_worker::IdleSessionClose::for_account(&config, &row.account, row.busy && row.preparing_session) {
                    sessions.push(match &row.sign_in_done {
                        Some(done) => session.with_sign_in_done(done.clone()),
                        None => session,
                    });
                }
            }
        }
        let parks = self.parks.drain(None);
        let history = self.config.history_path();
        if !self.preview {
            launch_log::append_history(&history, "Crystal", &format!(
                "launcher close: pid {}, idle sessions {}, parks {}", std::process::id(), sessions.len(), parks.len()), None);
        }
        if !self.preview && (!sessions.is_empty() || !parks.is_empty()) {
            let started = launcher_close::start(move || {
                let started = Instant::now();
                pol_less::park::close_workers(parks);
                let result = session_worker::close_idle_sessions(sessions);
                launch_log::append_history(&history, "Crystal", &format!(
                    "launcher close cleanup: pid {}, {} ms, {}", std::process::id(), started.elapsed().as_millis(),
                    if result.is_ok() { "completed" } else { "failed" }), None);
                result
            });
            if let Err(error) = started {
                self.closing = false;
                self.status = tf("Could not start PlayOnline cleanup: {error}", &[("error", &error.to_string())]);
                return Effect::none();
            }
        }
        tray::set_icon(false);
        // Destroy the window and release its renderer now. main() keeps the
        // cleanup owner alive until sign-out and final profile saves finish.
        match self.window.take() {
            Some(id) => Effect::window(id, WindowOp::Close).chain(Effect::Exit),
            None => {
                tray::wake_ui_thread_soon();
                Effect::Exit
            }
        }
    }

    /// Open the window (at start, or back from the background) at its last size.
    fn open_window(&mut self) -> Effect<Message> {
        let id = WindowId::unique();
        self.window = Some(id);
        Effect::window(id, WindowOp::Open { size: self.viewport, maximized: self.maximized })
    }

    /// Close the window but keep everything else running: sessions stay signed in, parks,
    /// waits and timers continue, and the renderer (with its GPU device) is released.
    fn hide_window(&mut self) -> Effect<Message> {
        let Some(id) = self.window.take() else { return Effect::none() };
        self.auto_restart.focused = false;
        self.cancel_account_drag();
        self.title_held = false;
        window_chrome::detach();
        tray::set_icon(true);
        if !self.preview {
            launch_log::append_history(&self.config.history_path(), "Crystal", &format!(
                "background: window closed, pid {}, {}", std::process::id(),
                footprint::memory(None).map_or_else(|| "memory unknown".to_owned(), footprint::Memory::text)), None);
        }
        Effect::batch([Effect::window(id, WindowOp::Close), Self::trim_later(Duration::from_secs(3))])
    }

    /// Back from the background, or forward when the window is open.
    fn show_window(&mut self) -> Effect<Message> {
        tray::set_icon(false);
        match self.window {
            Some(id) => Effect::window(id, WindowOp::Minimize(false)).chain(Effect::window(id, WindowOp::Focus)),
            None => self.open_window(),
        }
    }

    /// Register (`Some(true)`) or remove the Start with Windows task off the UI thread, or
    /// (`None`, at start with the switch on) check it still starts this exe.
    fn startup_task(&self, on: Option<bool>) -> Effect<Message> {
        let scratch = self.config.experiment.join("iced-launcher");
        Effect::perform(async move {
            tokio::task::spawn_blocking(move || {
                let exe = std::env::current_exe().map_err(|_| t("Could not locate Crystal's program file.").to_owned())?;
                let user = startup_task::current_user().ok_or_else(|| t("Could not identify the Windows user.").to_owned())?;
                let mut run = startup_task_runner();
                match on {
                    Some(on) => startup_task::set(on, &exe, &user, &scratch, &mut run),
                    None => startup_task::ensure(&exe, &user, &scratch, &mut run).map(|_| ()),
                }
            }).await.unwrap_or_else(|_| Err(t("Could not change Crystal's startup task.").into()))
        }, move |result| Message::StartupTaskSet(on.unwrap_or(true), result))
    }

    /// The freed renderer's pages go back to Windows once the window is gone.
    fn trim_later(after: Duration) -> Effect<Message> {
        Effect::perform(async move { tokio::time::sleep(after).await }, |_| Message::TrimMemory)
    }

    /// The window's close button: to the notification area when Settings say so, else quit.
    fn request_quit(&mut self) -> Effect<Message> {
        if matches!(self.self_update, self_update::State::Applying) {
            self.status = t("Waiting for the launcher update helper…").into();
            return Effect::none();
        }
        if self.reshade.busy {
            self.page = Page::Settings;
            self.settings_tab = settings_page::Tab::Graphics;
            self.status = t("Finish ReShade setup before closing Crystal.").into();
            return Effect::none();
        }
        if self.prereqs.busy() {
            self.page = Page::Settings;
            self.settings_tab = settings_page::Tab::System;
            self.status = t("Wait for Windows component setup to finish, or cancel its download, before closing.").into();
            return Effect::none();
        }
        if self.maintenance.is_some() {
            self.page = Page::Maintenance;
            self.show_settings = false;
            self.show_details = false;
            self.status = t("Wait for the operation to finish, or request Cancel, before closing the launcher.").into();
            return Effect::none();
        }
        if self.vault_saving {
            self.confirm_close = true;
            Effect::none()
        } else {
            self.begin_close()
        }
    }

    /// The Tick has no timed work: nothing waits to start, retry or expire (`tick_gate`).
    /// Busy states are checked by the caller.
    fn tick_quiet(&self) -> bool {
        self.page != Page::Friends
            && !self.startup_signin_pending && self.startup_waiting.is_empty()
            && self.waiting_plays.is_empty() && self.play_updates.jobs.is_empty() && self.launcher_changes.is_empty()
            && self.recovering.is_none() && !self.closing && self.parks_settled
            && self.accounts.iter().all(|row| row.resign.is_none() && row.network_wait.is_none()
                && row.queued_play.is_none() && !row.shutting_down && !row.signing_out && !row.batch_requested)
    }

    /// When a quiet window's next periodic Tick work is due: the official status read,
    /// (with parked hosts) their fingerprint check, and in maintenance the play flag's read
    /// and the next version check.
    fn next_timed_work(&self) -> Option<Instant> {
        let status = (!self.preview).then(|| self.refreshed + self.status_interval());
        let parks = (!self.parks.is_empty()).then(|| self.park_checked.map_or_else(Instant::now, |at| at + pol_less::park::FINGERPRINT_EVERY));
        let flag = (!self.preview && self.flag_wanted()).then(|| self.flag_schedule.next()).flatten();
        // A downloaded update's restart countdown, postponement or input grace (none otherwise).
        let restart = (!self.preview).then(|| { let now = Instant::now(); self.auto_restart.next_due(self.auto_conditions(now), now) }).flatten();
        status.into_iter().chain(parks).chain(flag).chain(self.patch_watch.next_due()).chain(restart).min()
    }

    /// The session flags whose change a quiet window must see at once: a session or game
    /// that ends (or a game that starts inside a signed-in session).
    fn watched_flags(&self) -> Vec<(Arc<AtomicBool>, bool)> {
        let mut flags = Vec::new();
        for row in &self.accounts {
            for flag in [&row.alive, &row.game_running].into_iter().flatten() {
                flags.push((flag.clone(), flag.load(Ordering::Acquire)));
            }
        }
        flags
    }

    /// Close parked hosts for `keys` off the UI thread (a batch signs them in cold).
    fn close_parks_for(&mut self, keys: &[String]) {
        let mut closing = Vec::new();
        for row in self.accounts.iter().filter(|row| keys.contains(&row.key)) {
            if let Some(plan) = self.settings.launch_plan(&row.account) {
                closing.extend(self.parks.take(&row.key, &pol_less::park::ParkKey::for_plan(&row.account, &plan)));
            }
        }
        close_parks_later(closing);
    }

    /// One connectivity probe for a row whose sign-in failed on the network (07d §5.5):
    /// DNS only, off the UI thread. Unit tests deliver `NetworkBack` themselves.
    fn spawn_network_probe(&self, key: String, generation: u64) {
        if self.preview || cfg!(test) { return; }
        let tx = LaunchTx::new(self.sender.clone(), key, generation);
        let _ = std::thread::Builder::new().name("network-probe".into()).spawn(move || {
            let back = session_worker::wait_for_network(Instant::now() + session_worker::NETWORK_PROBE_LIMIT);
            tx.send(WorkerEvent::NetworkBack(tx.key.clone(), back));
        });
    }

    /// What `account` launches with (its own choice, else the default).
    pub fn launcher_for(&self, account: &Account) -> crate::backend::LaunchWith {
        self.settings.launch_with_for(account)
    }

    /// The accounts Home's Play acts on: the selected ones, else the first.
    pub(crate) fn play_target_keys(&self) -> Vec<String> {
        let selected: Vec<String> = self.accounts.iter().filter(|r| r.selected).map(|r| r.key.clone()).collect();
        if selected.is_empty() { self.accounts.first().map(|r| r.key.clone()).into_iter().collect() } else { selected }
    }

    /// Set these retail accounts' own launcher (Home's Launch with) and save the vault.
    fn set_account_launcher(&mut self, keys: &[String], choice: crate::backend::LaunchWith) -> Effect<Message> {
        let changes = |r: &AccountRow| keys.contains(&r.key) && r.account.mode == AccountMode::Retail && r.account.launch_with != Some(choice);
        if choice == crate::backend::LaunchWith::Native || !self.accounts.iter().any(changes) {
            return Effect::none();
        }
        if self.preview {
            for row in &mut self.accounts {
                if changes(row) { row.account.launch_with = Some(choice); }
            }
            return Effect::none();
        }
        if !self.vault_ready || self.vault_saving {
            self.status = t("Wait for the accounts to finish saving, then choose again.").into();
            return Effect::none();
        }
        let values = self
            .accounts
            .iter()
            .map(|r| {
                let mut account = r.account.clone();
                if changes(r) {
                    account.launch_with = Some(choice);
                }
                account
            })
            .collect();
        self.save_accounts(values)
    }

    /// Open an account's latest log (or its logs folder) in the default app.
    fn open_log(&mut self, key: &str, folder: bool) {
        if self.preview {
            return;
        }
        let Some(row) = self.accounts.iter().find(|r| r.key == key) else {
            return;
        };
        let logs = self.config.logs_directory(&row.account);
        let target = if folder { Some(logs.clone()) } else { row.log.clone() };
        let opened = match target {
            Some(target) => open_log_path(&logs, &target),
            None => Err(t("There is no log for this account yet.").into()),
        };
        if let Err(error) = opened {
            self.status = error;
        }
    }

    fn search_ah(&mut self) -> Effect<Message> {
        let generation = self.ah_lookup.begin(&self.ah_query, self.ah_server);
        if self.preview {
            self.ah_lookup.finish(generation, Ok(ffxiah::Results::default()));
            return Effect::none();
        }
        let query = self.ah_lookup.query.clone();
        let world = self.ah_lookup.world;
        Effect::perform(ffxiah::search(query, world), move |result| Message::AhResults(generation, result))
    }

    fn reset_friends(&mut self, account: Option<String>) {
        self.friends_generation = self.friends_generation.wrapping_add(1);
        self.friends = friends::State { account, ..Default::default() };
        self.friends_expired = None;
    }

    fn reset_missing_friends_account(&mut self) {
        if self.friends.account.as_ref().is_some_and(|key| {
            !self.accounts.iter().any(|row| &row.key == key && row.account.mode == AccountMode::Retail)
        }) {
            self.reset_friends(None);
        }
    }

    fn load_friends(&mut self) {
        self.reset_missing_friends_account();
        if self.preview || self.friends.loading { return; }
        if self.friends.account.is_none() {
            self.friends.account = self.accounts.iter().find(|a| a.pid.is_some() && a.account.mode == AccountMode::Retail)
                .or_else(|| self.accounts.iter().find(|a| a.account.mode == AccountMode::Retail)).map(|a| a.key.clone());
        }
        let (Some((account, config)), Some(key)) = (self.friends_target(), self.friends.account.clone()) else { return };
        let sender = self.sender.clone();
        self.friends.loading = true;
        self.friends_generation = self.friends_generation.wrapping_add(1);
        let generation = self.friends_generation;
        self.friends_checked = Instant::now();
        if std::thread::Builder::new().name("friends-view".into()).spawn(move || {
            let (connected, snapshot) = session_worker::friends_snapshot(&config, &account);
            let _ = sender.send(WorkerEvent::Friends(generation, key, connected, snapshot));
        }).is_err() {
            self.friends.loading = false;
            self.friends.error = t("Could not read the friends session.").into();
        }
    }

    /// Ask for the next message the open conversation needs, if any.
    fn drive_sync(&mut self) -> Effect<Message> {
        if self.preview {
            return Effect::none();
        }
        if let Some(request) = self.friends.next_sync() {
            let task = self.update(Message::FriendsSocial(request));
            if self.friends.pending.is_none() {
                // A momentary collision with the session worker's own pass over the request
                // files: the next poll asks again. Any other refusal stops until the user acts.
                let transient = [t("Another PlayOnline request is being submitted."), t("Wait for the current PlayOnline request to finish.")];
                if transient.contains(&self.friends.error.as_str()) { self.friends.error.clear(); } else { self.friends.mail.stalled = true; }
            }
            return task;
        }
        // Once Sent lists the message just sent, it shows from there.
        if self.friends.pending.is_none() && self.friends.outgoing.as_ref().is_some_and(|o| o.sent) {
            self.friends.outgoing = None;
        }
        Effect::none()
    }

    /// A read of the Friends account's session: the page's state, and the end of the request
    /// in flight when the worker reports it done (or the session is gone).
    fn friends_read(&mut self, connected: bool, snapshot: Option<friends::Snapshot>) {
        self.friends.connected = connected;
        let mut finished = None;
        if let Some((id, _)) = &self.friends.pending {
            let completed = snapshot.as_ref().is_some_and(|s| s.social.id.as_ref() == Some(id) && !s.social.busy);
            if completed || !connected {
                finished = completed.then(|| snapshot.clone()).flatten();
                if !completed {
                    self.friends.mail.reading = None;
                    self.friends.outgoing = None;
                }
                self.friends.pending = None;
            }
        } else if let Some(id) = &self.friends_expired {
            // A request reported late, after its time limit: what it did counts after all.
            let completed = snapshot.as_ref().is_some_and(|s| s.social.id.as_ref() == Some(id) && !s.social.busy);
            if completed || !connected {
                if completed {
                    finished = snapshot.clone();
                    self.friends.mail.stalled = false;
                    if self.friends.error == t("PlayOnline has not reported completion. Check messages or the friend list before repeating this request.") { self.friends.error.clear(); }
                } else {
                    self.friends.outgoing = None;
                }
                self.friends_expired = None;
            }
        }
        if let (Some(key), Some(portrait)) = (&self.friends.account, snapshot.as_ref().and_then(|s| s.own_portrait)) {
            if self.own_portraits.get(key) != Some(&portrait) { self.own_portraits.insert(key.clone(), portrait); }
        }
        self.friends.snapshot = snapshot;
        if let Some(done) = finished {
            self.friends.completed(&done);
        }
        // Off the page a read only follows the request in flight; the next
        // conversation request waits until the page is open again.
        if self.page == Page::Friends {
            let _ = self.drive_sync();
        }
    }

    /// A Friends request the session accepted is now the one in flight.
    fn friends_request_started(&mut self, id: String, reading: Option<String>) {
        self.friends.pending = Some((id, Instant::now()));
        // The expired request can no longer be matched (the session now reports this one): a
        // draft it was sending is dropped, not left marked as sending.
        if self.friends_expired.take().is_some() && self.friends.outgoing.as_ref().is_some_and(|o| !o.sent) {
            self.friends.outgoing = None;
        }
        self.friends.mail.reading = reading;
        self.friends.error.clear();
    }

    /// The request in flight has had no result for FRIENDS_REQUEST_LIMIT. A late window (a Tick
    /// held up, or the page left so nothing read the session) may simply not have looked: read
    /// the session once now, and report a missing completion only if it is still missing.
    fn expire_friends_request(&mut self) {
        if !self.friends.pending.as_ref().is_some_and(|(_, started)| started.elapsed() >= FRIENDS_REQUEST_LIMIT) { return; }
        if !self.preview {
            if let Some((account, config)) = self.friends_target() {
                let (connected, snapshot) = session_worker::friends_snapshot(&config, &account);
                self.friends_read(connected, snapshot);
            }
        }
        // A request the read just started is not the one that expired.
        if self.friends.pending.as_ref().is_some_and(|(_, started)| started.elapsed() >= FRIENDS_REQUEST_LIMIT) {
            // The loader may still finish it (a send retries for minutes): a later read that
            // reports it done is reconciled (`friends_read`), and a send's draft stays matched.
            self.friends_expired = self.friends.pending.take().map(|(id, _)| id);
            self.friends.mail.stalled = true;
            self.friends.mail.reading = None;
            self.friends.error = t("PlayOnline has not reported completion. Check messages or the friend list before repeating this request.").into();
        }
    }

    /// Each Tick: what the session flags say now. Rows whose game started are noted, and rows
    /// whose session ended are cleared; returns the game ends (key, how it ended, its host) and
    /// whether a game was newly seen.
    fn observe_sessions(&mut self) -> (Vec<GameEnd>, bool) {
        let mut game_ends = Vec::new();
        let mut game_newly_seen = false;
        // For `features`: games seen starting, and games ended (key, report, closed by Crystal).
        let (mut started, mut ended) = (Vec::new(), Vec::new());
        for row in &mut self.accounts {
            if !row.batch_requested {
                row.batch_pending = session_worker::batch_pending(&self.config, &row.account);
            }
            if row.pid.is_some() && row.alive.as_ref().is_some_and(|a| a.load(Ordering::Acquire)) {
                // A sign-out or edit before this game (a pre-signed Play since) declines
                // nothing about it.
                if row.is_playing() && !std::mem::replace(&mut row.game_seen, true) {
                    game_newly_seen = true;
                    started.push(row.key.clone());
                    row.resign_declined = false;
                    row.signed_out = false;
                    if !self.preview {
                        startup_prefetch::game_started(&self.settings.config_for(&self.config, &row.account).game_directory(),
                            &self.config.startup_prefetch_path());
                    }
                }
                if row.is_playing() && !row.batch_pending {
                    if let Some(mut timing) = row.play_timing.take() {
                        timing.stage("game start observed by launcher");
                        timing.mark(launch_timing::Milestone::GameStarted);
                        row.timing = timing.row_summary();
                        row.status = t("Client started · continue in game").into();
                        if let Some(log) = &row.log { timing.append_to(&launch_timing::sibling_path(log)); }
                    }
                }
                if row.busy && row.is_playing() && !row.batch_pending { row.busy = false; }
                // A start the version check refused keeps its message on the row.
                if !row.busy && !row.batch_pending && row.queued_play.is_none() && !row.is_playing() && !row.problem { row.status = t("Signed in to PlayOnline · FFXI is closed").into(); }
            }
            if row
                .alive
                .as_ref()
                .is_some_and(|a| !a.load(Ordering::Acquire))
            {
                let host = row.pid.take().map(|pid| HostId { pid, created: row.host_created.take() });
                row.alive = None;
                row.game_running = None;
                // An account whose host was signed out for its update still waits for it.
                row.busy = row.batch_requested || self.play_updates.for_key(&row.key).is_some();
                row.preparing_session = false;
                row.queued_play = None;
                row.timing.clear();
                row.play_timing = None;
                let report = row
                    .outcome
                    .take()
                    .and_then(|slot| slot.lock().ok().and_then(|mut r| r.take()));
                if row.game_seen { ended.push((row.key.clone(), report.clone(), row.shutting_down || row.signing_out)); }
                if row.game_seen && !self.settings.sign_in_after_game { row.signed_out = true; }
                if std::mem::take(&mut row.game_seen) && !std::mem::take(&mut row.resign_declined) {
                    game_ends.push((row.key.clone(), report.as_ref().map(|r| r.kind.clone()), host));
                }
                row.resign_declined = false;
                match report {
                    Some(report) => row.set_exit(report),
                    None => {
                        row.problem = false;
                        row.status = t("Client closed").into();
                    }
                }
            }
        }
        self.features_observed(started, ended);
        (game_ends, game_newly_seen)
    }

    fn worker_event(&mut self, event: WorkerEvent) {
        // Any report for a row: its start is not stuck (`watch_starts`).
        if let WorkerEvent::Progress(key, _) | WorkerEvent::StartGame(key, ..) | WorkerEvent::Complete(key, _) = &event {
            self.note_start_activity(key);
        }
        match event {
            WorkerEvent::Viewer(generation, report) => self.set_viewer_status(generation, report),
            WorkerEvent::InterruptedUpdate(done) => self.interrupted_update_finished(*done),
            WorkerEvent::PlayUpdate(generation, event) => self.play_update_event(generation, event),
            WorkerEvent::PatchCheck(folder, key, result) => self.patch_checked(folder, key, result),
            WorkerEvent::Launch(key, generation, event) => {
                if self.accounts.iter().any(|row| row.key == key && row.launch_generation == generation) {
                    self.worker_event(*event);
                }
            }
            WorkerEvent::Friends(generation, key, connected, snapshot) => {
                if generation == self.friends_generation && self.friends.account.as_ref() == Some(&key) {
                    self.friends.loading = false;
                    self.friends_read(connected, snapshot);
                }
            }
            // A language switch can supersede a read even after switching back.
            WorkerEvent::Services(generation, lang, update) => {
                if generation == self.news_generation && lang == i18n::current() {
                    let status = matches!(update, services::Update::Status(_));
                    services::apply(&mut self.services, update);
                    if status { self.note_status_read(); }
                }
            }
            WorkerEvent::StartGame(key, report, result) => {
                // The W20 refused this Play for an update: it runs now, in this signed-in session.
                let update = report.clone().filter(|report| result.is_err() && report.state == patch::PatchState::Required);
                let clicked = self.accounts.iter().find(|r| r.key == key)
                    .and_then(|row| row.play_timing.as_ref().map(launch_timing::Timeline::started)).unwrap_or_else(Instant::now);
                // No report: the session never reached its idle main menu, so W20 never ran.
                if let Some(report) = report { self.set_patch_from_row(&key, report); }
                if let Some(row) = self.accounts.iter_mut().find(|r| r.key == key) {
                    match result {
                        Ok(note) => {
                            if let Some(timing) = &mut row.play_timing { timing.mark(launch_timing::Milestone::GameChecked); }
                            row.status = t("Starting FFXI from the signed-in PlayOnline session…").into();
                            self.status = tf("{account}: {message}", &[("account", &row.account.label), ("message", &note)]);
                        }
                        Err(error) => {
                            row.busy = false;
                            row.problem = true;
                            row.play_timing = None;
                            row.status = error.clone();
                            self.status = tf("{account}: {message}", &[("account", &row.account.label), ("message", &error)]);
                        }
                    }
                }
                if let Some(report) = update { self.begin_play_update(&key, &report, clicked, false, false); }
            }
            WorkerEvent::Notice(message) => self.status = message,
            WorkerEvent::Features(event) => self.features_event(event),
            WorkerEvent::WindowerProfileProgress(message) => {
                if self.windower_editor_open { self.status = message; }
            }
            WorkerEvent::Progress(key, status) => {
                if let Some(row) = self.accounts.iter_mut().find(|r| r.key == key) {
                    row.status = status;
                    row.set_note(if row.queued_play.is_some() {
                        Some(RowNote::QueuedPlay)
                    } else if matches!(row.launch_trigger, Some(launch_timing::LaunchTrigger::SignInAfterGame | launch_timing::LaunchTrigger::SignInAfterGameRetry)) {
                        Some(RowNote::ResignAfterGame)
                    } else { None });
                }
            }
            WorkerEvent::NetworkBack(key, back) => {
                let Some(row) = self.accounts.iter_mut().find(|r| r.key == key) else { return };
                let Some(wait) = row.network_wait.take() else { return };
                // An unclean game end: the sign-in after it now waits for its delay (Tick), or is
                // dropped when the network did not come back.
                if wait == NetworkWait::AfterGame {
                    if let Some(resign) = row.resign.as_mut().filter(|_| back) {
                        resign.network = false;
                        resign.ended = Instant::now();
                        return;
                    }
                    row.drop_resign();
                    if !back && !row.busy && row.pid.is_none() { row.set_note(Some(RowNote::NetworkGone)); }
                    return;
                }
                // No retry is coming after all: the row stops promising one.
                if row.shown_note() == Some(&RowNote::NetworkRetry) && (!back || row.busy || row.pid.is_some() || !row.can_play()) {
                    row.set_note((!back).then_some(RowNote::NetworkGone));
                }
                // The user may have started something else meanwhile; that wins.
                if !back || row.busy || row.pid.is_some() || !row.can_play() { return; }
                match wait {
                    NetworkWait::Reissue(clicked) => self.start_launch(vec![key], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay)),
                    // Unless the switch turned off (or the account learned to need a code) meanwhile.
                    NetworkWait::StartupRetry if self.presigned(&key) => self.start_startup(vec![key], launch_timing::LaunchTrigger::StartupRetry),
                    NetworkWait::StartupRetry => {}
                    NetworkWait::AfterGameRetry if self.resign_allowed(&key) => self.start_resigns(vec![key], launch_timing::LaunchTrigger::SignInAfterGameRetry),
                    NetworkWait::AfterGameRetry if self.resign_eligible(&key) => self.note_resign_refused(&key),
                    NetworkWait::AfterGameRetry | NetworkWait::AfterGame => {}
                }
            }
            WorkerEvent::Complete(key, result) => {
                let mut follow_up = None;
                let mut viewer_outdated = None;
                // A W20 verdict for this row's folder, and a Play an Update should start again.
                let mut game_report = None;
                // A sign-in the W2U stopped for a PlayOnline update, which that update starts
                // again: (click, what started a sign-in only).
                let mut refused_sign_in = None;
                // A W20 refusal whose update runs in the account's session: (report, click, batch).
                let mut play_update = None;
                // Whether the account's sign-in took a one-time code, learned from a session.
                let mut uses_otp = None;
                // A sign-in that reached a verdict on the account (no network or host failure that
                // is retried): a Play waiting on it waits no longer.
                let mut verdict = false;
                // A sign-in refused because local evidence says PlayOnline is out of date
                // (`viewer::Status::stale`, or the host's core check found an old core):
                // `Some(whether the user's own Play started it)`. Its update asks the server.
                let mut stale_refusal = None;
                // The host's own words for an old core, kept for the row should no update follow.
                let mut core_refusal = None;
                let viewer_local = self.viewer.as_ref().map(|status| status.version.local_version.clone());
                if let Some(row) = self.accounts.iter_mut().find(|r| r.key == key) {
                    let batch = row.batch_requested;
                    row.busy = false;
                    row.preparing_session = false;
                    row.batch_requested = false;
                    row.batch_pending = session_worker::batch_pending(&self.config, &row.account);
                    let trigger = row.launch_trigger.take();
                    match result {
                        Ok(result) => {
                            // A reconnect (no trigger) to an older worker's session reads "no code"
                            // from a record that never said: only a sign-in this window started
                            // teaches that an account needs none. Neither does a Play that reused
                            // such a session (a batch, or a Play queued on it): it signed nothing in.
                            uses_otp = (row.otp_spent || trigger.is_some() && !row.otp_unknown).then_some(row.otp_spent);
                            if trigger.is_some() && !row.otp_spent && !row.account.totp_key.is_empty() {
                                // Its own sign-in without a typed code: the authenticator's code
                                // worked. The account does take codes (kept, should the key go).
                                row.authenticator_refused = false;
                                uses_otp = Some(true);
                            }
                            // The signed-in session stays up; the stock check refused the game start.
                            let refused = patch::parse_update_required(&result.detail);
                            if row.pid != Some(result.pid) { row.host_created = process_created(result.pid); }
                            row.pid = Some(result.pid);
                            row.alive = Some(result.alive);
                            row.game_running = Some(result.game_running);
                            // A batch reports each prepared account Ready, then Failed for all
                            // when the batch failed (session_batch::coordinate), such as for an
                            // update its W20 asked for: what started it classifies that failure
                            // too. A game that started needs nothing more.
                            if batch && !row.is_playing() { row.launch_trigger = trigger; }
                            row.log = Some(result.log);
                            row.outcome = Some(result.outcome);
                            row.problem = refused.is_some();
                            row.status = if let Some(report) = refused {
                                // Only a Play runs the W20: its update runs in this session.
                                let clicked = row.play_timing.as_ref().map_or_else(Instant::now, launch_timing::Timeline::started);
                                play_update = Some((report.clone(), clicked, false));
                                game_report = Some(report);
                                result.detail.clone()
                            } else if !row.is_playing() {
                                t("Signed in to PlayOnline · FFXI is closed").into()
                            } else if result.timing.is_empty() {
                                result.detail.clone()
                            } else {
                                t("Client started · continue in game").into()
                            };
                            row.timing = result.timing;
                            self.status = tf("{account}: {message}", &[("account", &row.account.label), ("message", &result.detail)]);
                        }
                        Err(error) => {
                            // No session came of it, so nothing for a sign-out or edit to decline.
                            row.resign_declined = false;
                            let queued = row.queued_play.take();
                            // The row shows the reason; the log path stays behind View log.
                            let (message, log) = launch_log::split_log(&error);
                            // The host's core check found an old PlayOnline core (an old or fresh
                            // install): that is PlayOnline's update to run, and the row says so.
                            let reworded = viewer::core_check_outdated(message)
                                .then(|| viewer::stale_message(viewer_local.as_deref().unwrap_or(t("Unavailable"))));
                            if reworded.is_some() { core_refusal = Some(message.to_owned()); }
                            let message = reworded.as_deref().unwrap_or(message);
                            let update = patch::parse_update_required(message);
                            // 07d §5.4-5.5: a sign-in that never reached a verdict on the account
                            // (no network, or its host went away before authentication) starts
                            // once more: a queued Play as a cold Play, a startup sign-in after the
                            // network is back. A refused password, OTP or update never costs a
                            // second login, and neither does a spent one-time code: that second
                            // login would go without one and be refused.
                            let network = update.is_none() && launch_log::network_failure(message);
                            let transient = update.is_none() && launch_log::transient_failure(message)
                                && !session_worker::reached_authentication(&self.settings.config_for(&self.config, &row.account), &row.account);
                            verdict = !network && !transient;
                            // An authenticator key makes a fresh code for the retry.
                            let otp_spent = row.otp_spent && row.otp.is_empty() && !row.authenticator_ready();
                            let wanted = match (queued, trigger) {
                                (Some(clicked), _) if network => Some((NetworkWait::Reissue(clicked), true)),
                                (Some(clicked), _) if transient => Some((NetworkWait::Reissue(clicked), false)),
                                (None, Some(launch_timing::LaunchTrigger::StartupSignIn)) if network => Some((NetworkWait::StartupRetry, true)),
                                // One retry per game end; the retry's own failure ends it.
                                (None, Some(launch_timing::LaunchTrigger::SignInAfterGame)) if network => Some((NetworkWait::AfterGameRetry, true)),
                                _ => None,
                            };
                            match wanted {
                                Some(_) if otp_spent => {}
                                Some((wait, true)) => row.network_wait = Some(wait),
                                Some((wait, false)) => follow_up = Some((wait, false)),
                                None => {}
                            }
                            // The sign-in's W2U said PlayOnline is out of date: so does the
                            // window's PlayOnline status.
                            viewer_outdated = viewer::outdated_versions(message);
                            let stale = viewer_outdated.is_none() && viewer::local_update_required(message);
                            let play = queued.is_some() || matches!(trigger, Some(launch_timing::LaunchTrigger::Play
                                | launch_timing::LaunchTrigger::ParkedPlay | launch_timing::LaunchTrigger::PreSignedPlay
                                | launch_timing::LaunchTrigger::QueuedPlay | launch_timing::LaunchTrigger::BatchPlay
                                | launch_timing::LaunchTrigger::ReissuedPlay));
                            if stale {
                                // Crystal's own start again (after an update) is not the user's click.
                                stale_refusal = Some(play && trigger != Some(launch_timing::LaunchTrigger::ReissuedPlay));
                            }
                            // PlayOnline's update follows whatever started this sign-in (a Play, a
                            // batch, Crystal's own sign-in, Connect) and starts it again, as stock's
                            // Viewer updates at Connect; the game's update (a batch's W20) runs in
                            // the session the batch kept signed in. A batch's accounts keep its click.
                            if viewer_outdated.is_some() || stale {
                                let clicked = row.play_timing.as_ref().map_or_else(|| queued.unwrap_or_else(Instant::now), launch_timing::Timeline::started);
                                refused_sign_in = Some((clicked, (!play).then(|| trigger.unwrap_or(launch_timing::LaunchTrigger::Connect))));
                            }
                            if let Some(report) = update.as_ref().filter(|_| play || batch) {
                                let clicked = row.play_timing.as_ref().map_or_else(|| queued.unwrap_or_else(Instant::now), launch_timing::Timeline::started);
                                play_update = Some((report.clone(), clicked, batch));
                            }
                            game_report = update;
                            // Owner decision 2026-10-13: a sign-in Crystal started by itself without
                            // a code that the member login refused may be an account that now uses
                            // one. It counts as one from now on: Crystal no longer signs it in by
                            // itself, and nothing above retries it. Its next sign-in that works
                            // without a code undoes this. Only the row's own refusal counts: a
                            // batch reports another account's failure to every account in it, and
                            // one that got its session (Ready) was not refused.
                            let refused = row.presign_login && !row.otp_spent && row.pid.is_none()
                                && launch_log::sign_in_refused(message);
                            let stops_presigning = refused && self.settings.presigns(&key);
                            if refused { uses_otp = Some(true); }
                            // The row's own member login refused a code its authenticator key made
                            // (no code was typed): the key is likely wrong. Nothing signs it in
                            // again by itself; the row asks for a typed code (`needs_code`).
                            let key_refused = row.authenticator_ready() && !row.otp_spent && row.pid.is_none()
                                && launch_log::sign_in_refused(message);
                            if key_refused { row.authenticator_refused = true; }
                            row.status = if key_refused { t("Authenticator code refused. Check the key.").into() } else { message.to_owned() };
                            row.set_note(if row.network_wait.is_some() {
                                Some(RowNote::NetworkRetry)
                            } else if wanted.is_some() && otp_spent {
                                Some(RowNote::OtpSpent)
                            } else if key_refused {
                                None
                            } else if stops_presigning {
                                Some(RowNote::MayNeedOtp)
                            } else { None });
                            row.problem = true;
                            if log.is_some() {
                                row.log = log;
                            }
                            if let Some(wait) = row.network_wait { follow_up = Some((wait, true)); }
                            let shown = if reworded.is_some() { message } else { error.as_str() };
                            self.status = tf("{account}: {message}", &[("account", &row.account.label), ("message", &shown)]);
                        }
                    }
                }
                if let Some(report) = game_report { self.set_patch_from_row(&key, report); }
                // The session worker's W20 and W2U wrote what the server said.
                self.reload_official();
                if let Some(uses) = uses_otp { self.note_otp_account(&key, uses); }
                // A waiting Play whose sign-in (startup, Connect) was refused keeps the refusal on
                // its row instead of waiting to sign in again into the same verdict: a refused
                // password or account never costs a second login. One stopped for a PlayOnline
                // update keeps waiting: that update answers it, and no login was sent.
                if verdict && viewer_outdated.is_none() && stale_refusal.is_none() && self.is_waiting(&key) { self.unwait(std::slice::from_ref(&key)); }
                if let Some(by_user) = stale_refusal {
                    // Local evidence, no server verdict: the update asks the server for its version
                    // itself (no `viewer_asked`), the request a W2U at Connect would have been. One
                    // that already ran here and left it so starts no second one by itself.
                    if by_user || !self.viewer_updated {
                        let shown = viewer::stale(self.viewer.take());
                        self.show_viewer(shown);
                        if let Some((clicked, sign_in)) = refused_sign_in { self.note_refused(&key, clicked, sign_in); }
                    } else if let Some(original) = core_refusal {
                        // PlayOnline was just updated and its core still fails the check: the
                        // host's own reason, not another update.
                        if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key) { row.status = original; }
                    }
                }
                if let Some((local, remote)) = viewer_outdated {
                    // An update that just installed this very version and is asked for again
                    // changed nothing the server accepts: the row keeps the refusal, no loop.
                    // And only a version the server named is an update to run (a local refusal, such
                    // as another updater at work, names none).
                    let update = self.viewer_installed.as_deref() != Some(remote.as_str()) && patch::valid_viewer_version(&remote);
                    if update {
                        // The W2U closed before its verdict left the session worker: the
                        // update's first connection keeps stock's gap from now, at the least.
                        updater::note_patch_closed(updater::Product::Viewer, Instant::now());
                        self.viewer_asked = Some((remote.clone(), Instant::now()));
                    }
                    let outdated = viewer::outdated(self.viewer.take(), local, remote);
                    self.show_viewer(outdated);
                    if let Some((clicked, sign_in)) = refused_sign_in.filter(|_| update) { self.note_refused(&key, clicked, sign_in); }
                }
                // This result may be the last one PlayOnline's update waited for.
                self.tend_viewer_update();
                if let Some((report, clicked, batch)) = play_update { self.begin_play_update(&key, &report, clicked, batch, false); }
                match follow_up {
                    Some((_, true)) => {
                        let generation = self.accounts.iter().find(|r| r.key == key).map_or(0, |row| row.launch_generation);
                        self.spawn_network_probe(key, generation);
                    }
                    Some((NetworkWait::Reissue(clicked), false)) => self.start_launch(vec![key], false, clicked, Some(launch_timing::LaunchTrigger::ReissuedPlay)),
                    _ => {}
                }
                self.start_queued_plays();
            }
        }
    }

    pub(crate) fn can_restart_launcher(&self) -> bool {
        !self.closing && self.maintenance.is_none() && self.active_operation.is_none() && !self.prereqs.busy()
            && !self.dgvoodoo.busy() && !self.reshade.busy && self.friends.pending.is_none()
            && self.vault_ready && !self.startup_signin_pending && !self.vault_saving
            && !self.startup_task_busy && !self.windower_editor_open
            && self.held_plays.is_empty()
            // A waiting Play, the update a Play runs in its session, or a sign-in waiting for
            // PlayOnline's update is pending launch work (none is saved across a restart).
            && self.waiting_plays.is_empty() && self.play_updates.jobs.is_empty() && self.refused_plays.is_empty()
            && !self.accounts.iter().any(|a| a.shutting_down || a.signing_out || a.busy || a.batch_pending)
    }

    fn poll_controller(&mut self, capturing: bool) {
        let allowed = self.settings.controller_navigation && window_chrome::is_foreground()
            && !capturing && !self.gamepad.capturing() && !matches!(self.self_update, self_update::State::Applying);
        if self.controller_nav.poll(self.gamepad.controller_pad(), allowed, Instant::now())
            && activity_due(&mut LAST_ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()), Instant::now()) {
            let _ = self.update(Message::UserInput);
        }
    }

    /// A dialog is over the page (the account sheet counts separately: `controller_modal`).
    fn dialog_open(&self) -> bool {
        self.confirm_close || self.pending_delete.is_some() || self.account_import.open
            || self.show_settings || self.show_details || self.ah_lookup.open || self.portrait_picker.is_some()
    }

    pub(crate) fn controller_modal(&self) -> bool {
        self.dialog_open() || self.editor.is_some()
    }

    /// The open account sheet, unless the vault is being saved (edits wait for that).
    fn editor_mut(&mut self) -> Option<&mut Editor> {
        if self.vault_saving { None } else { self.editor.as_mut() }
    }

    /// The Friends page's account and its launch config, as owned values.
    fn friends_target(&self) -> Option<(Account, Config)> {
        let row = self.accounts.iter().find(|row| self.friends.account.as_ref() == Some(&row.key))?;
        Some((row.account.clone(), self.settings.config_for(&self.config, &row.account)))
    }

    /// A command for the launcher window, when there is one.
    fn on_window(&self, command: WindowOp<Message>) -> Effect<Message> {
        self.window.map_or(Effect::none(), |id| Effect::window(id, command))
    }

    /// Save the settings as they are now (never in a preview); a failure shows on the status line.
    fn persist_settings(&mut self) {
        if self.preview { return; }
        if let Err(error) = save_settings(&self.config, &self.settings) { self.status = error; }
    }

    /// Handles `message`, then looks again at the automatic restart for a downloaded update
    /// (`self_update::auto`): every event that can change its answer (a game ending, work
    /// finishing, focus, input, the tray) arrives as a message, so it needs no polling.
    fn update(&mut self, message: Message) -> Effect<Message> {
        let effect = self.handle(message);
        if self.preview { return effect; }
        let auto = self.auto_restart_step(Instant::now());
        Effect::batch([effect, auto])
    }

    /// A game runs or is starting on some account: in a new session, or relaunched inside one
    /// already signed in (a batch Play after FFXI returned to the Viewer).
    pub(crate) fn game_active(&self) -> bool {
        self.accounts.iter().any(|row| row.is_playing() || row.game_start_pending())
    }

    /// Whether `msg` may restart Crystal now. A download that finishes restarts at once only
    /// while no game runs or starts: "Update and restart" may be pressed before a Play whose
    /// games are still starting when the download lands. Otherwise the update stays Ready and
    /// the automatic restart (`self_update::auto`) installs it once the games are over.
    fn self_update_can_restart(&self, msg: &self_update::Msg) -> bool {
        self.can_restart_launcher() && !(matches!(msg, self_update::Msg::Downloaded(_)) && self.game_active())
    }

    /// What the automatic restart sees now.
    fn auto_conditions(&self, now: Instant) -> self_update::auto::Conditions {
        self_update::auto::Conditions {
            enabled: self.settings.launcher_auto_restart,
            ready: matches!(self.self_update, self_update::State::Ready(_)),
            can_restart: self.can_restart_launcher(),
            game_running: self.game_active(),
            dialog: self.controller_modal(),
            user_active: self.auto_restart.user_active(self.window.is_some(), now),
        }
    }

    /// Starts, stops or finishes the restart countdown; logs each change with the version.
    fn auto_restart_step(&mut self, now: Instant) -> Effect<Message> {
        use self_update::auto::Step;
        let conditions = self.auto_conditions(now);
        let step = self.auto_restart.step(conditions, now);
        let version = match &self.self_update { self_update::State::Ready(stage) => stage.version().to_owned(), _ => String::new() };
        let history = self.config.history_path();
        let log = |line: &str| if !self.preview { launch_log::append_history(&history, "Crystal", line, None); };
        match step {
            Step::Wait => Effect::none(),
            Step::Scheduled => {
                log(&format!("self-update: auto-restart scheduled for {version} in {} s", self_update::auto::COUNTDOWN.as_secs()));
                // Hidden in the notification area: the pill cannot be seen, so a balloon says it.
                if self.window.is_none() && !self.preview {
                    features::notify::balloon(
                        t("Crystal Launcher is restarting to update"),
                        &tf("Crystal Launcher {version} installs in {seconds} s. Open Crystal to postpone it.",
                            &[("version", &version), ("seconds", &self_update::auto::COUNTDOWN.as_secs())]));
                }
                Effect::none()
            }
            Step::Cancelled => {
                log(&format!("self-update: auto-restart for {version} waits (Crystal or a game is busy)"));
                Effect::none()
            }
            Step::Apply => {
                log(&format!("self-update: auto-restart applied {version}"));
                Effect::done(Message::SelfUpdate(self_update::Msg::Apply))
            }
        }
    }

    fn handle(&mut self, message: Message) -> Effect<Message> {
        if self.closing && !matches!(&message,
            Message::WindowResized(_) | Message::WindowMaximized(_)
            | Message::MinimizeWindow | Message::MaximizeWindow | Message::TitlePressed
            | Message::TitleMoved | Message::TitleReleased | Message::ResizeWindow(_)) {
            return Effect::none();
        }
        // The helper is preparing to replace this process. Do not start new
        // writes or work between its readiness check and our exit. Results of work already
        // running still land (they clear busy flags and touch nothing the update replaces), so a
        // helper that then fails leaves nothing stuck.
        if matches!(self.self_update, self_update::State::Applying)
            && !matches!(&message, Message::SelfUpdate(self_update::Msg::Applied(_)) | Message::CloseRequested
                | Message::StartupTaskSet(..) | Message::WindowerProfilesClosed(_) | Message::ServicesLoaded(..)
                | Message::ServiceStatusLoaded(..) | Message::PatchLoaded(..) | Message::ViewerLoaded(..) | Message::AhResults(..))
        {
            return Effect::none();
        }
        match message {
            Message::SelfUpdate(self_update::Msg::AutoCheck(on)) => {
                self.settings.launcher_auto_updates = on;
                self.persist_settings();
                if on && !self.preview { return Effect::done(Message::SelfUpdate(self_update::Msg::Check)); }
            }
            Message::SelfUpdate(self_update::Msg::AutoRestart(on)) => {
                self.settings.launcher_auto_restart = on;
                self.persist_settings();
            }
            Message::SelfUpdate(self_update::Msg::Postpone) => {
                self.auto_restart.postpone(Instant::now());
                if let (false, self_update::State::Ready(stage)) = (self.preview, &self.self_update) {
                    launch_log::append_history(&self.config.history_path(), "Crystal", &format!(
                        "self-update: auto-restart postponed for {} ({} h or the next start)", stage.version(),
                        self_update::auto::POSTPONE.as_secs() / 3600), None);
                }
            }
            // Signed-in idle sessions are separate worker processes: they stay signed in and the
            // new version reconnects to them at start (`session_worker::find`); accounts with no
            // session sign in through the normal startup sign-in. Parked hosts close first.
            Message::SelfUpdate(self_update::Msg::Applied(Ok(()))) => { self.close_parks(); tray::set_icon(false); tray::wake_ui_thread_soon(); return Effect::Exit; }
            Message::SelfUpdate(msg) => {
                if !self.preview {
                    let can_restart = self.self_update_can_restart(&msg);
                    let task = self_update::update(&mut self.self_update, msg, &self.config, can_restart).map(Message::SelfUpdate);
                    // The update replaces this exe: no parked host may outlive it (07d §1.5).
                    // They close off the interface thread before the helper starts.
                    if matches!(self.self_update, self_update::State::Applying) {
                        let parks = self.parks.drain(None);
                        return Effect::future(async move {
                            match close_parks_now(parks).await {
                                Ok(()) => task,
                                Err(error) => Effect::done(Message::SelfUpdate(self_update::Msg::Applied(Err(error)))),
                            }
                        });
                    }
                    return task;
                }
            }
            Message::ControllerTick => self.poll_controller(false),
            Message::ControllerNavigation(on) => {
                self.settings.controller_navigation = on;
                self.controller_nav.clear();
                self.persist_settings();
            }
            // Like the per-account choice it replaces: sessions already signed in stay, and a
            // sign-in in flight finishes; a startup sign-in still waiting for a closing session,
            // a sign-in after a game not yet started and a startup network retry are dropped at
            // their next check. Turned on, it signs nothing in now: from the next game's end, or
            // the next start. Parked warm hosts follow it (`park_keys`).
            Message::AutoSignIn(on) => {
                self.settings.auto_sign_in = on;
                self.park_checked = None;
                self.persist_settings();
            }
            Message::SignInAfterGame(on) => {
                self.settings.sign_in_after_game = on;
                if !on { for row in &mut self.accounts { row.drop_resign(); } }
                self.persist_settings();
            }
            Message::ItemSearch(on) => {
                self.settings.item_search = on;
                if !on { self.ah_lookup.open = false; }
                self.persist_settings();
            }
            Message::CloseGamesInstantly(on) => {
                self.settings.close_games_instantly = on;
                self.persist_settings();
            }
            Message::ControllerPlay => {
                if !self.controller_modal() { return self.update(Message::MainAction); }
            }
            Message::ControllerPage(step) => {
                if !self.controller_modal() {
                    // The top bar's order; Features (None) is a page over the one underneath.
                    let stops = [Some(Page::Home), Some(Page::Friends), Some(Page::Maintenance), None, Some(Page::Settings)];
                    let current = if self.features_view.open { None } else { Some(self.page) };
                    let index = stops.iter().position(|p| *p == current).unwrap_or(0);
                    self.controller_nav.clear();
                    return match stops[(index as i32 + step as i32).rem_euclid(stops.len() as i32) as usize] {
                        Some(page) => self.update(Message::Navigate(page)),
                        None => self.update(Message::Feature(features::ui::Msg::Open(true))),
                    };
                }
            }
            Message::ControllerBack => {
                if self.account_drag.source.is_some() { self.cancel_account_drag(); return Effect::none(); }
                if self.gamepad.capturing() { return self.update(Message::Gamepad(gamepad::Msg::CancelCapture)); }
                if self.confirm_close { return self.update(Message::CancelClose); }
                if self.pending_delete.is_some() { return self.update(Message::CancelDeleteAccount); }
                if self.portrait_picker.is_some() { return self.update(Message::PortraitPicker(None)); }
                // A hotkey field that is recording stops; the page stays.
                if self.features_view.open && self.features_view.recording.is_some() { return self.update(Message::Feature(features::ui::Msg::HotkeyCancel)); }
                if self.features_view.open { return self.update(Message::Feature(features::ui::Msg::Open(false))); }
                if self.editor.is_some() { return self.update(Message::CloseEditor); }
                if self.account_import.open { return self.import_update(account_import_ui::Msg::Close); }
                if self.show_settings { return self.update(Message::ToggleSettings); }
                if self.show_details { return self.update(Message::ToggleDetails); }
                if self.ah_lookup.open { return self.update(Message::CloseAh); }
                if self.page == Page::Friends && self.friends.remove.is_some() { return self.update(Message::FriendsDraftClose); }
                if self.page == Page::Friends && self.friends.pane.is_some() { return self.update(Message::FriendsSelect(None)); }
                if self.page != Page::Home { return self.update(Message::Navigate(Page::Home)); }
            }
            Message::FriendsAccount(key) => {
                self.reset_friends(Some(key));
                self.load_friends();
            }
            Message::FriendsQuery(query) => self.friends.query = query,
            Message::FriendsSelect(pane) => {
                let opened = self.friends.pane != pane;
                self.friends.select(pane.clone());
                // The Mailbox lists Inbox page 1 when it has nothing current, or, as it opens, when
                // the session last listed another folder or page (a conversation's sync ends on Sent).
                if pane == Some(friends::Pane::Mailbox) && self.friends.pending.is_none() && self.friends.snapshot.as_ref().is_some_and(|s| s.social.ready && !s.social.busy && s.game_running == Some(false)
                    && (s.social.inbox_unread.is_none() || s.social.inbox_changed || (opened && (s.social.folder != 0 || s.social.page != 0)))) {
                    return self.update(Message::FriendsSocial(social::Request::Inbox { folder:0, page:0 }));
                }
                return self.drive_sync();
            }
            Message::FriendsSection(index) => { if let Some(folded) = self.friends.collapsed.get_mut(index) { *folded = !*folded; } }
            Message::FriendsStatusOptions => self.friends.status_options = !self.friends.status_options,
            Message::FriendsAddSubject(open) => { if let Some(draft) = &mut self.friends.composer { draft.show_subject = open; } }
            Message::FriendsRemove(friend) => self.friends.remove = Some(friend),
            Message::FriendsRemoveConfirm => {
                let Some(friend) = self.friends.remove.clone() else { return Effect::none() };
                let Some(uid) = friend.uid else { self.friends.remove = None; return Effect::none() };
                // A request in flight (the bar pauses conversation sync, so it ends soon): the bar
                // stays open to confirm again once it has.
                if !self.preview && self.friends.pending.is_some() {
                    self.friends.error = t("Wait for the current PlayOnline request to finish.").into();
                    return Effect::none();
                }
                self.friends.error.clear();
                let task = self.update(Message::FriendsSocial(social::Request::Remove { alternate: friend.alternate, slot: friend.slot, uid }));
                // The bar closes once the request went out or was refused for good; a momentary
                // collision with the session worker leaves it open to confirm again.
                let collided = [t("Another PlayOnline request is being submitted."), t("Wait for the current PlayOnline request to finish.")]
                    .contains(&self.friends.error.as_str());
                if self.friends.pending.is_some() || !collided { self.friends.remove = None; }
                return task;
            }
            Message::FriendsDraftClose => {
                self.friends.remove = None;
                return self.drive_sync();
            }
            Message::FriendsSubject(subject) => { if let Some(draft) = &mut self.friends.composer { draft.subject = subject.chars().take(512).collect(); } }
            Message::FriendsMessageEdit(text) => { if let Some(draft) = &mut self.friends.composer { draft.text = text; } }
            Message::FriendsSend => {
                let ready = self.friends.connected && self.friends.pending.is_none()
                    && self.friends.snapshot.as_ref().is_some_and(|s| s.game_running == Some(false) && s.social.ready && !s.social.busy);
                if let (true, Some(request)) = (ready, self.friends.send_request()) {
                    let draft = if let social::Request::Send { uid, subject, message, .. } = &request {
                        Some(friends::Outgoing { uid: *uid, subject: subject.clone(), message: message.clone(), sent: false })
                    } else { None };
                    let task = self.update(Message::FriendsSocial(request));
                    // Set once the request went out (after FriendsSocial dropped any earlier draft).
                    if self.friends.pending.is_some() { self.friends.outgoing = draft; }
                    return task;
                }
            }
            Message::FriendsSocial(request) => {
                if !self.preview && self.friends.pending.is_none() {
                    if let Some((account, config)) = self.friends_target() {
                        if matches!(request, social::Request::Check {}) { self.friends.mail.stalled = false; }
                        let reading = if let social::Request::Read { header } = &request { Some(social::message_key(header)) } else { None };
                        match session_worker::request_social(&config, &account, request) {
                            Ok(id) => self.friends_request_started(id, reading),
                            Err(error) => self.friends.error = error,
                        }
                        self.load_friends();
                    }
                }
            }
            Message::FriendsConnect => {
                if let Some(key) = self.friends.account.clone() { self.connect(vec![key]); }
            }
            Message::FriendsPlay => {
                if let Some(key) = self.friends.account.clone() { self.play(vec![key]); }
            }
            Message::FriendsSignOut => {
                if let Some(key) = self.friends.account.clone() { return self.sign_out_account(key); }
            }
            Message::ShutdownAccount(key) => return self.shutdown_games(false, Some(&key)),
            Message::ShutdownSelected => return self.shutdown_games(true, None),
            Message::ShutdownAll => return self.shutdown_games(false, None),
            Message::GameShutdownFinished(key, generation, result) => {
                if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && row.launch_generation == generation) {
                    row.shutting_down = false;
                    if let Err(error) = result {
                        row.problem = true;
                        row.status = error;
                    } else if row.game_seen {
                        row.problem = false;
                        row.status = if self.settings.sign_in_after_game {
                            t("FFXI closed. Returning to PlayOnline…")
                        } else { t("FFXI and PlayOnline closed.") }.into();
                        // The monitor publishes the exit report before `alive = false`.
                        // Let Tick consume both, whether it runs before or after this result,
                        // so automatic sign-in is scheduled once with the actual exit reason.
                        if self.preview {
                            row.alive = Some(Arc::new(AtomicBool::new(false)));
                        }
                    }
                }
            }
            Message::AccountSignOut(key) => return self.sign_out_account(key),
            Message::AccountSignedOut(key, generation, result) => {
                if let Some(row) = self.accounts.iter_mut().find(|row| row.key == key && row.launch_generation == generation) {
                    row.signing_out = false;
                    row.busy = false;
                    row.problem = result.is_err();
                    row.status = result.as_ref().err().cloned().unwrap_or_else(|| t("Signed out of PlayOnline").into());
                    if result.is_ok() {
                        row.pid = None;
                        row.alive = None;
                        row.game_running = None;
                        row.outcome = None;
                        row.exit = None;
                        row.timing.clear();
                    }
                    let signed_out = result.is_ok();
                    if self.friends.account.as_ref() == Some(&key) {
                        self.friends.error = result.err().unwrap_or_default();
                        self.load_friends();
                    }
                    // A sign-out for a changed launcher signs back in.
                    self.launcher_signed_out(&key, signed_out);
                }
            }
            Message::FriendsStatus(action) => {
                if !self.preview && matches!(action, session_worker::NativeControl::Visible | session_worker::NativeControl::Away | session_worker::NativeControl::Invisible) {
                    if let Some((account, config)) = self.friends_target() {
                        self.friends.error = session_worker::request_native_status(&config, &account, action, !self.friends.skip_groups).err().unwrap_or_default();
                    }
                }
            }
            Message::FriendsStatusGroups(groups) => self.friends.skip_groups = !groups,
            Message::FriendsAutoAway(minutes) => {
                if !self.preview && pol_less::AUTO_AWAY_CHOICES.contains(&minutes) {
                    if let Some((account, _)) = self.friends_target() {
                        self.settings.account_auto_away.insert(account_key(&account), minutes);
                        if let Err(error) = save_settings(&self.config, &self.settings) { self.friends.error = error; }
                    }
                }
            }
            Message::WindowFocus(focused) => self.auto_restart.focused = focused && self.window.is_some(),
            Message::UserInput => {
                self.auto_restart.last_input = Some(Instant::now());
                // Only signed-in sessions whose game is closed; the worker
                // forwards at most one native_activity per note.
                if !self.preview {
                    let idle: Vec<_> = self.accounts.iter()
                        .filter(|r| r.account.mode == AccountMode::Retail && r.pid.is_some() && !r.busy && !r.is_playing())
                        .map(|r| (self.settings.config_for(&self.config, &r.account), r.account.clone()))
                        .collect();
                    if !idle.is_empty() {
                        let _ = std::thread::Builder::new().name("native-activity".into()).spawn(move || {
                            for (config, account) in idle { let _ = session_worker::note_native_activity(&config, &account); }
                        });
                    }
                }
            }
            Message::SettingsTab(tab) => {
                self.settings_tab = tab;
                if tab == settings_page::Tab::Graphics { return reshade::update(self, reshade::Msg::Refresh); }
            }
            // open a Settings tab from elsewhere (the components banner).
            Message::OpenSettingsTab(tab) => {
                self.settings_tab = tab;
                return self.update(Message::Navigate(Page::Settings));
            }
            Message::GameConfig(m) => {
                return game_config::update(&mut self.game_config, m).map(Message::GameConfig)
            }
            Message::Prereqs(m) => return prereqs::update(&mut self.prereqs, m).map(Message::Prereqs),
            Message::Dgvoodoo(m) => return dgvoodoo::update(self, m),
            Message::Reshade(m) => return reshade::update(self, m),
            Message::Gamepad(m) => {
                let captured = self.gamepad.capturing();
                let task = gamepad::update(&mut self.gamepad, m, &self.config).map(Message::Gamepad);
                self.poll_controller(captured);
                if self.gamepad.take_persisted_change() {
                    self.park_checked = None;
                    self.close_parks_in_background(None);
                }
                if let Some((revision, values)) = self.gamepad.take_live_update() {
                    return Effect::batch([task, self.apply_controller_settings(revision, values)]);
                }
                return task;
            },
            Message::LaunchTools(m) => {
                self.park_checked = None;
                if launch_tools::apply(&mut self.settings, m) { self.persist_settings(); }
                return self.launch_tools.refresh(&self.settings);
            }
            Message::LaunchToolsScanned(scan) => return self.launch_tools.scanned(scan),
            Message::OpenWithGame(m) => {
                if open_with_game::apply(&mut self.settings, m) { self.persist_settings(); }
            }
            Message::OpenWithGameDone(report) => {
                for line in open_with_game::log_lines(&report) {
                    launch_log::append_history(&self.config.history_path(), "Crystal", &format!("open with game: {line}"), None);
                }
                if let Some(notice) = open_with_game::notice(&report) { self.status = notice; }
            }
            Message::EditWindowerProfiles => {
                if self.preview ||self.windower_editor_open { return Effect::none(); }
                self.windower_editor_open = true;
                // Idle signed-in owners retain the same profile guard as parks.
                // Let their native sign-out and final Windower save finish first.
                self.close_parks_in_background(Some(pol_less::Tool::Windower.name()));
                self.status = t("Edit profiles in Windower. Saved changes apply on the next fresh game launch.").into();
                let preferred = self.settings.windower_path.clone();
                let sessions = self.accounts.iter().filter_map(|row|
                    session_worker::find(&self.settings.config_for(&self.config, &row.account), &row.account)).collect();
                let sender = self.sender.clone();
                return Effect::perform(async move {
                    tokio::task::spawn_blocking(move || {
                        let folder = pol_less::tools::detect(pol_less::Tool::Windower, &preferred)
                            .ok_or_else(|| t("Windower was not found. Set its folder in Settings → System → Launch tools.").to_owned())?;
                        session_worker::sign_out_for_profile_editor(sessions, |line| {
                            let _ = sender.send(WorkerEvent::WindowerProfileProgress(line.into()));
                        })?;
                        launch_tools::edit_profiles(&folder, |line| {
                            let _ = sender.send(WorkerEvent::WindowerProfileProgress(line.into()));
                        })
                    }).await.unwrap_or_else(|_| Err(t("Could not open Windower's profile editor.").into()))
                }, Message::WindowerProfilesClosed);
            }
            Message::WindowerProfilesClosed(result) => {
                self.windower_editor_open = false;
                self.park_checked = None;
                if let Err(error) = result { self.status = error; }
                return self.launch_tools.refresh(&self.settings);
            }
            Message::Tick => {
                // A long gap since the last Tick is a resume from sleep: a waiting Play checks again.
                self.flag_schedule.tick(chrono::Utc::now());
                // No periodic patch-host lookups: stock looks pc001W2U.pol.com up only right
                // before a W20 (the W20's own lookup, or xi-start-game's warm-up at Play).
                if self.startup_signin_pending && self.vault_ready && !self.installation_loading {
                    self.start_ready_startup_signins();
                }
                let keys = self.take_waiting_startups(Instant::now());
                self.start_startup(keys, launch_timing::LaunchTrigger::StartupSignIn);
                // A request in flight is followed closely, so a conversation
                // fills message by message; otherwise every two seconds. It is
                // followed off the page too: its time limit runs on every page.
                let every = if self.friends.pending.is_some() { Duration::from_millis(250) } else { Duration::from_secs(2) };
                if (self.page == Page::Friends || self.friends.pending.is_some()) && self.friends_checked.elapsed() >= every {
                    self.load_friends();
                }
                if let Some(op) = &self.active_operation {
                    self.maintenance_progress = op.snapshot();
                }
                while let Ok(event) = self.receiver.try_recv() {
                    self.worker_event(event);
                }
                // After the results that already arrived: a completion waiting in the queue
                // is not a request PlayOnline never answered.
                self.expire_friends_request();
                self.release_held_plays(Instant::now());
                let (game_ends, game_newly_seen) = self.observe_sessions();
                for (key, kind, host) in game_ends { self.note_game_end(&key, kind, host); }
                // Once per Play batch, after the first game reports started; off the UI thread,
                // and never part of the launch itself.
                let game_active = self.accounts.iter().any(|row| row.game_seen);
                let open_programs = if self.open_gate.observe(game_active, game_newly_seen) && !self.preview
                    && self.settings.open_with_game.iter().any(|p| p.enabled) {
                    let programs = self.settings.open_with_game.clone();
                    Effect::perform(async move { tokio::task::spawn_blocking(move || open_with_game::run(programs)).await.unwrap_or_default() },
                        Message::OpenWithGameDone)
                } else { Effect::none() };
                let keys = self.take_resigns(Instant::now());
                self.start_resigns(keys, launch_timing::LaunchTrigger::SignInAfterGame);
                self.start_queued_plays();
                // After every release: what changed on the rows, and a start nothing answers.
                self.watch_starts(Instant::now());
                let accounts = &self.accounts;
                // A deleted account no longer waits.
                let deleted: Vec<String> = self.waiting_plays.iter().flat_map(|wait| &wait.keys)
                    .filter(|key| !accounts.iter().any(|row| row.key == **key)).cloned().collect();
                self.unwait(&deleted);
                self.expire_wait_codes();
                self.release_waiting_plays(false);
                self.tend_patch_watch();
                // The PlayOnline update sign-ins wait for starts once nothing else is in flight.
                self.tend_viewer_update();
                // Work a worker event started (a sign-in's PlayOnline update).
                let started = Effect::batch(std::mem::take(&mut self.play_updates.tasks));
                let flag = self.read_play_flag();
                self.tend_parks(Instant::now());
                if self.preview
                    && !preview_idle()
                    && !self.preview_capture_sent
                    && !self.news_loading
                    && self.refreshed.elapsed() > Duration::from_secs(
                        std::env::var("XI_PREVIEW_DELAY_SECS").ok()
                            .and_then(|value| value.parse::<u64>().ok()).unwrap_or(2).clamp(2, 30))
                {
                    self.preview_capture_sent = true;
                    return self.on_window(WindowOp::screenshot(Message::PreviewReady));
                }
                let relaunch = self.tend_launcher_changes(Instant::now());
                return Effect::batch([started, flag, open_programs, relaunch, self.poll_service_status()]);
            }
            Message::PlayFlagLoaded(reading) => {
                self.flag_schedule.finish();
                self.play_flag.note(reading);
                self.release_waiting_plays(true);
                self.tend_patch_watch();
            }
            Message::CancelWait(keys) => self.cancel_wait(&keys),
            Message::CancelStart(key) => self.clear_start(&key, true),
            Message::CancelPlayUpdate(generation) => self.cancel_play_update(generation),
            Message::ConfirmViewerUpdate => {
                if self.play_updates.viewer_confirm.take().is_some() {
                    self.start_viewer_update();
                    return Effect::batch(std::mem::take(&mut self.play_updates.tasks));
                }
            }
            Message::AccountDragStart(key) => {
                // The rail shows on every page, so a reorder starts from any of them; a dialog
                // over the rail blocks it.
                if self.vault_ready && !self.vault_saving && !self.show_settings && self.portrait_picker.is_none()
                    && self.editor.is_none() && !self.account_import.open && !self.confirm_close && self.pending_delete.is_none()
                    && self.accounts.iter().any(|row| row.key == key) {
                    self.cancel_account_drag();
                    self.account_drag.original = self.accounts.iter().map(|row| row.key.clone()).collect();
                    self.account_drag.source = Some(key);
                }
            }
            Message::AccountDragOver(target) => {
                if let Some(source) = self.account_drag.source.as_ref() {
                    if let (Some(from), Some(to)) = (self.accounts.iter().position(|row| &row.key == source),
                        self.accounts.iter().position(|row| row.key == target)) {
                        if from != to {
                            let row = self.accounts.remove(from);
                            self.accounts.insert(to, row);
                        }
                    }
                }
            }
            Message::AccountDragFinish => self.finish_account_drag(),
            Message::AccountDragCancel => self.cancel_account_drag(),
            Message::ToggleAccount(key, value) => {
                if let Some(row) = self.accounts.iter_mut().find(|r| r.key == key) {
                    row.selected = value;
                }
                self.settings.selected_accounts = Some(
                    self.accounts
                        .iter()
                        .filter(|r| r.selected)
                        .map(|r| r.key.clone())
                        .collect(),
                );
                self.persist_settings();
            }
            Message::WindowResized(size) => {
                self.viewport = size;
                // Maximise/restore arrives as a resize; keep the chrome's
                // restore glyph and resize edges in step with it.
                return self.on_window(WindowOp::is_maximized(Message::WindowMaximized));
            }
            Message::WindowMaximized(maximized) => self.maximized = maximized,
            Message::WindowOpened(id) => {
                if self.window == Some(id) { return self.on_window(WindowOp::native_handle(Message::NativeWindow)); }
            }
            Message::NativeWindow(hwnd) => {
                window_chrome::attach(hwnd, self.settings.light_theme)
            }
            Message::MinimizeWindow => return self.on_window(WindowOp::Minimize(true)),
            Message::MaximizeWindow => return self.on_window(WindowOp::ToggleMaximize),
            Message::TitlePressed => {
                self.title_held = !window_chrome::title_press_is_double();
                if !self.title_held { return self.on_window(WindowOp::ToggleMaximize); }
            }
            Message::TitleMoved => {
                if std::mem::take(&mut self.title_held) { return self.on_window(WindowOp::Drag); }
            }
            Message::TitleReleased => self.title_held = false,
            Message::ResizeWindow(edge) => return self.on_window(WindowOp::DragResize(edge)),
            Message::Navigate(page) => {
                self.cancel_account_drag();
                self.page = page;
                // Features is a page of its own: going anywhere leaves it.
                if self.features_view.open { self.features_view.open = false; self.features_view.recording = None; self.features_view.touch(); }
                self.show_settings = false;
                self.show_details = false;
                if page == Page::Friends { self.load_friends(); }
                if page == Page::Settings && self.settings_tab == settings_page::Tab::Graphics {
                    return reshade::update(self, reshade::Msg::Refresh);
                }
                if page == Page::Maintenance && !self.settings.install_selected {
                    return self.find_installation(true);
                }
            }
            Message::ToggleDetails => self.show_details = !self.show_details,
            Message::Launch(key) => {
                if let Some(features) = &self.features { features.cancel_relaunch(&key); }
                if self.features_view.offers.remove(&key).is_some() { self.features_view.touch(); }
                self.play_row(key)
            }
            Message::MainAction => return self.run_main_action(),
            Message::LaunchAll => self.play_from_home("all", false),
            Message::SignIn(key) => {
                if self.accounts.iter().any(|row| row.key == key && row.account.mode == AccountMode::Retail && row.can_play() && row.pid.is_none()) {
                    self.connect(vec![key]);
                }
            }
            Message::SetCharacterSelect(on) => {
                let mut settings = self.settings.clone();
                for key in self.play_target_keys() {
                    let mut choice = settings.account_auto_login.get(&key).copied().unwrap_or_default().normalized();
                    choice.enabled = on;
                    if choice == pol_less::AutoLogin::default() { settings.account_auto_login.remove(&key); }
                    else { settings.account_auto_login.insert(key, choice); }
                }
                if !self.preview {
                    if let Err(error) = save_settings(&self.config, &settings) { self.status = error; return Effect::none(); }
                }
                self.settings = settings;
            }
            Message::OtpChanged(key, value) => {
                if let Some(row) = self.accounts.iter_mut().find(|r| r.key == key && !r.busy) {
                    row.otp.zeroize();
                    row.otp = value;
                }
            }
            Message::NewAccount => {
                self.cancel_account_drag();
                if self.vault_ready && !self.vault_saving {
                    self.status = t("Enter account details, then save.").into();
                    let mut editor = Editor::blank();
                    // A new account has no chosen region yet: the one installed edition, if one.
                    if let Some(edition) = retail_region::only_edition(&self.pol_editions()) { editor.account.region = edition.index(); }
                    self.editor = Some(editor);
                }
            }
            Message::PolEditions(editions) => {
                self.pol_editions = editions;
                self.follow_installer_region();
            }
            Message::InstallerRegion(region) => {
                if self.maintenance.is_none() && retail_region::RetailRegion::from_account(region).is_some() {
                    self.settings.installer_region = Some(region);
                    self.follow_installer_region();
                    self.persist_settings();
                }
            }
            Message::AccountImport(msg) => return self.import_update(msg),
            Message::EditAccount(key) => {
                self.cancel_account_drag();
                if self.vault_saving { return Effect::none(); }
                if let Some(row) = self
                    .accounts
                    .iter()
                    .find(|r| r.key == key)
                {
                    self.editor = Some(Editor {
                        game_install: self.settings.account_installs.get(&key).cloned().unwrap_or_default(),
                        dat_overlays: self.settings.account_overlays.get(&key).cloned().unwrap_or_default(),
                        auto_login: self.settings.account_auto_login.get(&key).copied().unwrap_or_default().normalized(),
                        flags: EditorFlags {
                            uses_otp: self.settings.account_otp.contains(&key),
                            no_hairpin: self.settings.account_no_hairpin.contains(&key),
                            ..Default::default()
                        },
                        key: Some(key),
                        account: row.account.clone(),
                    });
                    self.status = t("Edit account details.").into();
                }
            }
            Message::EditorChanged(field, value) => {
                if matches!(field, Field::PolId | Field::Server) && self.editor.as_ref().and_then(|editor| editor.key.as_ref())
                    .is_some_and(|key| self.accounts.iter().any(|row| &row.key == key && (row.busy || row.pid.is_some()))) {
                    return Effect::none();
                }
                if let Some(editor) = self.editor_mut() {
                    let destination = match field {
                        Field::Label => &mut editor.account.label,
                        Field::Server => &mut editor.account.server,
                        Field::PolId => &mut editor.account.pol_id,
                        Field::PolPassword => &mut editor.account.pol_password,
                        Field::SeId => &mut editor.account.se_id,
                        Field::SePassword => &mut editor.account.se_password,
                        Field::PolHandleNames => &mut editor.account.pol_handle_names,
                        // Kept as typed until Save normalizes it (`totp::normalize`).
                        Field::TotpKey => &mut editor.account.totp_key,
                    };
                    destination.zeroize();
                    *destination = value;
                }
            }
            Message::EditorRegion(region) => {
                if let Some(editor) = self.editor_mut() { editor.account.region = region; }
            }
            Message::EditorAutoLogin(enabled) => {
                if let Some(editor) = self.editor_mut() { editor.auto_login.enabled = enabled; }
            }
            Message::EditorFlag(flag, on) => {
                if let Some(editor) = self.editor_mut() {
                    let flags = &mut editor.flags;
                    match flag {
                        EditorFlag::UsesOtp => flags.uses_otp = on,
                        EditorFlag::SignInNow => flags.sign_in_now = on,
                        EditorFlag::ShowId => flags.show_id = on,
                        EditorFlag::ShowPassword => flags.show_password = on,
                        EditorFlag::ChangePassword => flags.change_password = on,
                        EditorFlag::ShowTotpKey => flags.show_totp_key = on,
                        EditorFlag::ChangeTotpKey => flags.change_totp_key = on,
                        EditorFlag::NoHairpin => flags.no_hairpin = on,
                    }
                }
            }
            Message::EditorCharacterSlot(slot) => {
                if let Some(editor) = self.editor_mut().filter(|_| (1..=16).contains(&slot)) { editor.auto_login.character_slot = slot; }
            }
            Message::SaveAccount => {
                // The authenticator key as typed (spaces, lowercase, padding) is saved in its
                // stored form; a key that is not base32 keeps the sheet open with the reason.
                if let Some(editor) = self.editor.as_mut() {
                    let key = &mut editor.account.totp_key;
                    if key.trim().is_empty() || editor.account.mode != AccountMode::Retail {
                        key.zeroize();
                    } else {
                        match totp::normalize(key) {
                            Ok(normalized) => { key.zeroize(); key.push_str(&normalized); }
                            Err(error) => { self.status = error.message().into(); return Effect::none(); }
                        }
                    }
                }
                if let Some(editor) = &self.editor {
                    if self.settings.launch_with_for(&editor.account) == crate::backend::LaunchWith::Native {
                        if editor.dat_overlays.len() > 8 { self.status = t("Choose at most eight DAT overlay folders.").into(); return Effect::none(); }
                        for path in &editor.dat_overlays {
                            if let Err(error) = selected_folder(path, true) { self.status = error; return Effect::none(); }
                        }
                    }
                    if !editor.game_install.trim().is_empty() {
                        let valid = selected_folder(&editor.game_install, true).and_then(|path| {
                            if path.join("FFXiMain.dll").is_file() && path.join("FFXi.dll").is_file() {
                                Ok(path)
                            } else {
                                Err(t("Choose the game folder containing FFXiMain.dll and FFXi.dll.").into())
                            }
                        });
                        if let Err(error) = valid { self.status = error; return Effect::none(); }
                    }
                    let key = account_key(&editor.account);
                    if editor.key.as_ref().is_some_and(|previous| *previous != key
                        && self.accounts.iter().any(|row| &row.key == previous && (row.busy || row.pid.is_some()))) {
                        self.status = t("Keep the account ID unchanged while its session is running.").into();
                        return Effect::none();
                    }
                    if self
                        .accounts
                        .iter()
                        .any(|r| r.key == key && Some(&r.key) != editor.key.as_ref())
                    {
                        self.status =
                            t("This login is already saved. Edit its existing account instead.")
                                .into();
                        return Effect::none();
                    }
                    let mut replaced = false;
                    let mut values: Vec<Account> = self.accounts.iter().map(|row| {
                        if Some(&row.key) == editor.key.as_ref() {
                            replaced = true;
                            editor.account.clone()
                        } else { row.account.clone() }
                    }).collect();
                    if !replaced { values.push(editor.account.clone()); }
                    return self.save_accounts(values);
                }
            }
            Message::RequestDeleteAccount(key) => {
                if self.vault_ready && !self.vault_saving && self.accounts.iter().any(|row| row.key == key && !row.busy && row.pid.is_none()) {
                    self.pending_delete = Some(key);
                }
            }
            Message::CancelDeleteAccount => self.pending_delete = None,
            Message::DeleteAccount(key) => {
                if self.pending_delete.as_deref() != Some(key.as_str()) {
                    return Effect::none();
                }
                self.pending_delete = None;
                if self
                    .accounts
                    .iter()
                    .any(|r| r.key == key && (r.busy || r.pid.is_some()))
                {
                    return Effect::none();
                }
                let values = self
                    .accounts
                    .iter()
                    .filter(|r| r.key != key)
                    .map(|r| r.account.clone())
                    .collect();
                return self.save_accounts(values);
            }
            Message::CloseEditor => {
                if !self.vault_saving {
                    self.editor = None;
                    if self.account_import.batch.is_some() { self.account_import.open = true; }
                }
            }
            Message::ToggleSettings => self.show_settings = !self.show_settings,
            Message::SetLightTheme(light) => {
                self.settings.light_theme = light;
                window_chrome::set_theme(light);
                self.persist_settings();
            }
            // Display only and at once: the next view draws pseudonyms.
            Message::SetPrivacy(on) => {
                self.settings.privacy = on;
                privacy::set(on);
                if let Some(features) = &self.features { features.set_privacy(on); }
                self.persist_settings();
            }
            // Applies at once: the next view reads the new language.
            Message::SetLanguage(lang) => {
                let before = i18n::current();
                self.settings.language = Some(lang);
                i18n::set(lang);
                self.retranslate(before);
                self.persist_settings();
                if lang != before {
                    // The official news is per language: drop the old language's
                    // headlines at once, show the new language's saved ones, and
                    // fetch fresh ones.
                    self.news_error.clear();
                    if !self.preview {
                        // The status (page reading, windows, read time) is the same in every
                        // language: keep it, or Play is ungated until the new read lands.
                        self.services = services::keep_status(self.services.as_ref(), services::cached(&news_cache_path(&self.config, lang), lang));
                        // A version-record read in flight words its placeholders in the old
                        // language (PatchLoaded matches them): read again, so it is dropped.
                        let patch = if self.patch_loading { self.read_installed_version() } else { Effect::none() };
                        return Effect::batch([self.start_news(), patch]);
                    }
                }
            }
            // Home's Launch with sets the own launcher of every account Play starts
            // (the selected ones, else the first).
            Message::SetLaunchWith(choice) => {
                let keys = self.play_target_keys();
                return self.set_account_launcher(&keys, choice);
            }
            Message::EditorLaunchWith(choice) => {
                if let Some(editor) = self.editor_mut().filter(|_| choice != Some(crate::backend::LaunchWith::Native)) { editor.account.launch_with = choice; }
            }
            Message::EditorBootIni(choice) => {
                if let Some(editor) = self.editor_mut() { editor.account.ashita_boot_ini = choice; }
            }
            Message::EditorProfile(choice) => {
                if let Some(editor) = self.editor_mut() { editor.account.windower_profile = choice; }
            }
            Message::WikiQueryChanged(value) => self.wiki_query = value,
            Message::AhQueryChanged(value) => self.ah_query = value,
            Message::AhServerChanged(value) => {
                self.ah_server = value;
                if !self.preview && self.settings.ah_server.as_deref() != Some(value) {
                    self.settings.ah_server = Some(value.to_owned());
                    // Unsaved, it still holds for this window.
                    let _ = save_settings(&self.config, &self.settings);
                }
                if self.ah_lookup.open { return self.search_ah(); }
            }
            Message::AhResults(generation, result) => self.ah_lookup.finish(generation, result),
            Message::CloseAh => self.ah_lookup.open = false,
            Message::PortraitPicker(None) => self.portrait_picker = None,
            Message::PortraitPicker(Some(key)) => {
                if !self.accounts.iter().any(|row| row.key == key && row.account.mode == AccountMode::Retail) { return Effect::none(); }
                self.portrait_picker = Some(portraits::Picker { account: key, ..Default::default() });
                // Every sheet is read once (a few hundred ms), off the UI thread; later opens reuse it.
                return Effect::perform(
                    async { tokio::task::spawn_blocking(portraits::available).await.unwrap_or_else(|_| std::sync::Arc::from([])) },
                    Message::PortraitsListed,
                );
            }
            Message::PortraitsListed(ids) => {
                if let Some(picker) = &mut self.portrait_picker {
                    // Privacy mode does not open on the page that would point at your own.
                    let current = if privacy::on() { None } else { self.own_portraits.get(&picker.account).copied() };
                    picker.listed(ids, current);
                }
            }
            Message::PortraitPage(page) => {
                if let Some(picker) = &mut self.portrait_picker {
                    if page < picker.pages() { picker.page = page; }
                }
            }
            Message::PortraitPick(id) => {
                // Nothing is written to PlayOnline until the portrait field is confirmed.
                if !portraits::PICKER_ENABLED || self.preview { return Effect::none(); }
                let Some(picker) = &mut self.portrait_picker else { return Effect::none() };
                if !picker.ids.as_ref().is_some_and(|ids| ids.contains(&id)) { return Effect::none(); }
                let Some(row) = self.accounts.iter().find(|row| row.key == picker.account) else { return Effect::none() };
                let config = self.settings.config_for(&self.config, &row.account);
                match session_worker::request_native_portrait(&config, &row.account, id) {
                    Ok(()) => self.portrait_picker = None,
                    Err(error) => picker.error = Some(error),
                }
            }
            Message::NewsFilter(value) => self.news_filter = value,
            Message::SearchAh if !self.settings.item_search => {}
            Message::SearchAh => return self.search_ah(),
            Message::SearchWiki if !self.settings.item_search => {}
            Message::SearchWiki => {
                let url = crate::ffxiah::wiki_search_url(&self.wiki_query);
                if !self.preview {
                    if let Err(error) = open_url(url.as_str()) {
                        self.status = error;
                    }
                }
            }
            Message::InstallPathChanged(value) => {
                if self.maintenance.is_none() {
                    self.installation_generation = self.installation_generation.wrapping_add(1);
                    self.installation_loading = false;
                    self.settings.install_path = value;
                    self.settings.install_selected = false;
                    self.patch = None;
                    self.patch_folder = None;
                    self.patch_generation += 1;
                    self.patch_loading = false;
                    self.maintenance_result =
                        t("Installation path changed. Save settings to select this folder.").into();
                    self.maintenance_tone = ResultTone::Unverified;
                }
            }
            Message::SelectInstallPath => self.select_path(false),
            Message::FindInstallation => return self.find_installation(false),
            Message::InstallationFound(generation, automatic, folder) => {
                if generation != self.installation_generation { return Effect::none(); }
                self.park_checked = None;
                self.installation_loading = false;
                if self.preview || self.maintenance.is_some() { return Effect::none(); }
                let Some(folder) = folder else {
                    if !automatic { self.status = t("No installed FFXI folder was found. Run official setup or use Browse.").into(); }
                    self.start_ready_startup_signins();
                    return Effect::none();
                };
                let path = folder.to_string_lossy().into_owned();
                if self.settings.install_selected && self.settings.install_path == path {
                    if !automatic { self.status = tf("Found FINAL FANTASY XI: {path}", &[("path", &folder.display())]); }
                    self.start_ready_startup_signins();
                    return Effect::none();
                }
                let _ = self.update(Message::InstallPathChanged(path));
                // Recovery defaults must not replace an unreadable settings file
                // merely because startup found an installation.
                if automatic && self.settings_damaged {
                    self.settings.install_selected = true;
                } else {
                    self.select_path(false);
                }
                if self.settings.install_selected {
                    if !automatic { self.status = tf("Found FINAL FANTASY XI: {path}", &[("path", &folder.display())]); }
                    self.start_ready_startup_signins();
                    return self.read_installed_version();
                }
                self.start_ready_startup_signins();
            }
            Message::InstallerPathChanged(value) => {
                if self.maintenance.is_none() {
                    self.installer_error = None;
                    self.settings.installer_path = value;
                    self.settings.installer_selected = false;
                }
            }
            Message::SelectInstallerPath => self.select_path(true),
            Message::Browse(target) => return self.browse(target),
            Message::Picked(target, picked) => return self.picked(target, picked),
            Message::AccountInstallChanged(value) => {
                if let Some(editor) = self.editor_mut() { editor.game_install = value; }
            }
            Message::PickedAccountFolder(key, picked) => {
                if let Some(editor) = self.editor_mut() {
                    if account_key(&editor.account) == key {
                        if let Some(path) = picked { editor.game_install = backend::plain_windows_path(&path).to_string_lossy().into_owned(); }
                    }
                }
            }
            Message::PickedAccountOverlay(key, picked) => {
                if let Some(editor) = self.editor_mut() {
                    if account_key(&editor.account) == key && editor.dat_overlays.len() < 8 {
                        if let Some(path) = picked {
                            let path = backend::plain_windows_path(&path).to_string_lossy().into_owned();
                            if !editor.dat_overlays.iter().any(|p| p.eq_ignore_ascii_case(&path)) { editor.dat_overlays.push(path); }
                        }
                    }
                }
            }
            Message::StartMaintenance(kind) => return self.start_maintenance(kind),
            Message::ToggleTroubleshooting => self.troubleshooting_open = !self.troubleshooting_open,
            Message::CancelMaintenance => {
                if self.maintenance.is_some_and(MaintenanceKind::cancellable) {
                    if let Some(op) = &self.active_operation {
                        op.cancel();
                        self.maintenance_cancelled = true;
                        self.status = t("Cancellation requested; waiting for the current file operation to finish safely.").into();
                    }
                }
            }
            Message::MaintenanceFinished(kind, result, shown) => {
                if self.maintenance == Some(kind) {
                    let setup_finished = kind == MaintenanceKind::Setup && result.is_ok();
                    if matches!(kind, MaintenanceKind::Check | MaintenanceKind::Update | MaintenanceKind::Repair) { self.viewer_loading = false; }
                    if matches!(kind, MaintenanceKind::Download | MaintenanceKind::Extract | MaintenanceKind::Setup) {
                        self.installer_error = result.as_ref().err().cloned();
                    }
                    if let Some(op) = self.active_operation.take() {
                        self.maintenance_progress = op.snapshot();
                    }
                    self.maintenance = None;
                    self.maintenance_tone = if result.is_err() { ResultTone::Stopped }
                        else if shown.as_ref().is_some_and(|report| report.files_verified == Some(false) || report.files_verified.is_none() && report.state != patch::PatchState::Current) { ResultTone::Unverified }
                        else { ResultTone::Done };
                    // An Update started from refused Plays starts them again once it succeeded
                    // (the update signed their sessions out: each is a new sign-in).
                    let reissue = if kind == MaintenanceKind::Update { std::mem::take(&mut self.reissue_after_update) } else { Vec::new() };
                    let updated = result.is_ok();
                    let failure = result.as_ref().err().cloned();
                    self.maintenance_result =
                        result.unwrap_or_else(|error| tf("Operation stopped: {error}", &[("error", &error)]));
                    self.status = self.maintenance_result.clone();
                    // No report came back: read the installed record again, so a Check never
                    // leaves the installed version at a placeholder.
                    let refresh = shown.is_none() && matches!(kind, MaintenanceKind::Check | MaintenanceKind::Update | MaintenanceKind::Repair);
                    if let Some(report) = shown {
                        // An Update reports the folder it updated (an account's own one when
                        // its W20 asked); Check and Repair the Settings folder.
                        if kind != MaintenanceKind::Update { self.patch_folder = None; }
                        // An Update or Repair told the official version (Check's is a placeholder).
                        let official = report.remote_version.clone();
                        self.show_patch(report);
                        let folder = self.shown_game_folder();
                        self.learn_from_update(&folder, Some(&official), None);
                    }
                    if kind == MaintenanceKind::Update && !reissue.is_empty() && !self.preview {
                        let line = match &failure {
                            None => format!("PlayOnline update: installed; starting {} sign-ins again", reissue.len()),
                            Some(error) => format!("PlayOnline update: stopped: {error}"),
                        };
                        launch_log::append_history(&self.config.history_path(), "Crystal", &line, None);
                    }
                    // The version a sign-in's update installed (`viewer_installed`).
                    let target = self.viewer_target.take();
                    if updated && kind == MaintenanceKind::Update && self.update_scope.0 { self.viewer_installed = target; self.viewer_updated = true; }
                    if updated {
                        self.reissue_refused(reissue);
                    } else if let Some(error) = failure.filter(|_| kind == MaintenanceKind::Update) {
                        // The sign-in that asked for a PlayOnline update it could not finish says why.
                        let cancelled = self.maintenance_cancelled;
                        let shown = if cancelled { t("Update paused. Press Play to continue.").to_owned() }
                            else if error == updater::viewer_in_use() { error }
                            else { tf("PlayOnline could not be updated: {error}", &[("error", &error)]) };
                        for refused in &reissue {
                            if let Some(row) = self.accounts.iter_mut().find(|row| row.key == refused.key && !row.busy) {
                                row.status = shown.clone();
                                row.problem = !cancelled;
                            }
                        }
                    }
                    if refresh { return self.refresh(); }
                    if setup_finished {
                        return Effect::batch([self.find_installation(false), self.start_viewer_check()]);
                    }
                }
            }
            Message::SaveSettings => {
                if self.preview {
                    return Effect::none();
                }
                // Settings holds the only game-folder field: saving a typed folder selects it.
                // A folder that is not a game installation keeps the dialog open with the reason.
                if !self.settings.install_selected && !self.settings.install_path.trim().is_empty()
                    && self.maintenance.is_none() && !self.reshade.busy {
                    self.select_path(false);
                    if !self.settings.install_selected { return Effect::none(); }
                }
                match save_settings(&self.config, &self.settings) {
                    Ok(()) => {
                        self.settings_damaged = false;
                        self.park_checked = None;
                        self.show_settings = false;
                        self.status = t("Settings saved. Checking the installed version…").into();
                        return self.refresh();
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::Refresh => {
                if !self.news_loading && !self.patch_loading {
                    return self.refresh();
                }
            }
            Message::AccountsLoaded(result) => {
                match result {
                    Ok(values) => {
                        self.replace_accounts(values);
                        self.vault_ready = true;
                        self.vault_error = None;
                        self.status = t("Ready. Select accounts or use Launch all. Games continue when you close Crystal.").into();
                        self.log_launcher_start();
                        self.start_ready_startup_signins();
                    }
                    Err(error) => {
                        self.vault_ready = false;
                        self.status = tf("Saved accounts could not be loaded: {error} The existing vault was kept.", &[("error", &error)]);
                        self.vault_error = Some(self.status.clone());
                    }
                }
                if let Some(warning) = self.startup_warning.take() {
                    self.status = tf("{warning} {status}", &[("warning", &warning), ("status", &self.status)]);
                }
            }
            Message::AccountsSaved(result) => {
                self.vault_saving = false;
                match result {
                    Ok(values) => {
                        let mut settings = self.settings.clone();
                        let saved: BTreeSet<String> = values.iter().map(account_key).collect();
                        settings.account_pol_handles.retain(|key, _| saved.contains(key));
                        for account in &values {
                            let key = account_key(account);
                            if account.pol_handle_names.trim().is_empty() { settings.account_pol_handles.remove(&key); }
                            else { settings.account_pol_handles.insert(key, account.pol_handle_names.clone()); }
                        }
                        // A login edit changes the account key. Keep its non-editor
                        // preferences with the account before removing obsolete keys.
                        if let Some(editor) = &self.editor {
                            let key = account_key(&editor.account);
                            if let Some(previous) = editor.key.as_ref().filter(|old| **old != key && saved.contains(&key)) {
                                if let Some(minutes) = settings.account_auto_away.remove(previous) {
                                    settings.account_auto_away.insert(key.clone(), minutes);
                                }
                                if let Some(choice) = settings.account_auto_login.remove(previous) {
                                    settings.account_auto_login.insert(key.clone(), choice);
                                }
                                if settings.account_no_hairpin.remove(previous) {
                                    settings.account_no_hairpin.insert(key.clone());
                                }
                                if let Some(selected) = &mut settings.selected_accounts {
                                    if selected.remove(previous) { selected.insert(key); }
                                }
                                for ordered in &mut settings.account_order {
                                    if ordered == previous { *ordered = account_key(&editor.account); }
                                }
                            }
                        }
                        settings.account_installs.retain(|key, _| saved.contains(key));
                        settings.account_overlays.retain(|key, _| saved.contains(key));
                        settings.account_auto_away.retain(|key, _| saved.contains(key));
                        settings.account_auto_login.retain(|key, _| saved.contains(key));
                        // Learned from sign-ins of that login: a changed login learns it again.
                        settings.account_otp.retain(|key| saved.contains(key));
                        settings.account_no_otp.retain(|key| saved.contains(key));
                        settings.account_no_hairpin.retain(|key| saved.contains(key));
                        settings.account_order.retain(|key| saved.contains(key));
                        if let Some(selected) = &mut settings.selected_accounts { selected.retain(|key| saved.contains(key)); }
                        if self.play_updates.viewer_confirm.as_ref().is_some_and(|(key, _)| !saved.contains(key)) { self.play_updates.viewer_confirm = None; }
                        if let Some(editor) = &self.editor {
                            let key = account_key(&editor.account);
                            if saved.contains(&key) {
                                let path = editor.game_install.trim();
                                if path.is_empty() { settings.account_installs.remove(&key); }
                                else { settings.account_installs.insert(key.clone(), plain_text_path(path)); }
                                if editor.dat_overlays.is_empty() { settings.account_overlays.remove(&key); }
                                else { settings.account_overlays.insert(key.clone(), editor.dat_overlays.iter().map(|p| plain_text_path(p)).collect()); }
                                // "Uses a one-time password": Crystal then never signs it in by
                                // itself; cleared, the next sign-in teaches it again.
                                if editor.flags.uses_otp {
                                    settings.account_no_otp.remove(&key);
                                    settings.account_otp.insert(key.clone());
                                } else {
                                    settings.account_otp.remove(&key);
                                }
                                if editor.flags.no_hairpin && editor.account.mode == AccountMode::Private {
                                    settings.account_no_hairpin.insert(key.clone());
                                } else {
                                    settings.account_no_hairpin.remove(&key);
                                }
                                if editor.auto_login != pol_less::AutoLogin::default() {
                                    settings.account_auto_login.insert(key, editor.auto_login.normalized());
                                } else { settings.account_auto_login.remove(&key); }
                            }
                        }
                        // An edited account signs nothing back in by itself (a removed one has no row).
                        let edited: Vec<String> = self.editor.iter().flat_map(|editor| [Some(account_key(&editor.account)), editor.key.clone()]).flatten().collect();
                        if (self.editor.is_some() || settings.account_installs != self.settings.account_installs || settings.account_overlays != self.settings.account_overlays
                            || settings.account_auto_away != self.settings.account_auto_away || settings.account_auto_login != self.settings.account_auto_login || settings.selected_accounts != self.settings.selected_accounts
                            || settings.account_order != self.settings.account_order
                            || settings.account_pol_handles != self.settings.account_pol_handles
                            || settings.account_otp != self.settings.account_otp
                            || settings.account_no_otp != self.settings.account_no_otp
                            || settings.account_no_hairpin != self.settings.account_no_hairpin) && !self.preview {
                            if let Err(error) = save_settings(&self.config, &settings) {
                                if let Some(editor) = &mut self.editor {
                                    let key = account_key(&editor.account);
                                    if saved.contains(&key) {
                                        // The vault already saved this identity. A retry
                                        // must edit it instead of reporting a duplicate.
                                        editor.key = Some(key);
                                    } else {
                                        self.editor = None;
                                    }
                                }
                                // Keep the pending preferences for a retry or the next
                                // settings save; account storage is already committed.
                                self.settings = settings;
                                self.replace_accounts(values);
                                // The vault saved the edit: it declines a sign-in after the game too.
                                for row in self.accounts.iter_mut().filter(|row| edited.contains(&row.key)) { row.decline_resign(); }
                                self.status = tf("Accounts saved, but launcher settings were not saved: {error}", &[("error", &error)]);
                                return Effect::none();
                            }
                        }
                        self.settings = settings;
                        self.replace_accounts(values);
                        for row in self.accounts.iter_mut().filter(|row| edited.contains(&row.key)) { row.decline_resign(); }
                        // An idle session whose launcher or profile changed signs out and in again.
                        self.note_launcher_changes(Instant::now());
                        // A new account's "Sign in now", under the
                        // Settings switch and never for a one-time-password account.
                        let sign_in = self.editor.as_ref()
                            .filter(|editor| editor.key.is_none() && editor.flags.sign_in_now && self.settings.auto_sign_in
                                && (!editor.flags.uses_otp || !editor.account.totp_key.is_empty()))
                            .map(|editor| account_key(&editor.account));
                        self.editor = None;
                        self.park_checked = None;
                        if let Some(key) = sign_in {
                            if self.accounts.iter().any(|row| row.key == key && row.account.mode == AccountMode::Retail && row.can_play() && row.pid.is_none()) {
                                self.connect(vec![key]);
                            }
                        }
                        if self.account_import.batch.is_some() { self.account_import.open = true; }
                        self.status = t("Accounts saved.").into();
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::ServicesLoaded(generation, lang, _) | Message::ServiceStatusLoaded(generation, lang, _)
                if generation != self.news_generation || lang != i18n::current() => {
                // The newest request owns `news_loading` and the displayed data.
            }
            Message::ServicesLoaded(_, _, result) => {
                self.news_loading = false;
                // let the live feed paint before a preview capture; a
                // capture taken on the next tick replays the previous frame
                // (stale images, text whose paragraphs were already dropped).
                if self.preview {
                    self.refreshed = Instant::now();
                }
                match result {
                    Ok(mut snapshot) => {
                        // --preview-news-pictures (with --preview-live-news): only headlines that have a picture.
                        // The README shots show Home ready to play whatever the live status is that day.
                        if self.preview && preview::has("--preview-news-pictures") {
                            snapshot.news.retain(|item| item.image.is_some());
                            snapshot.availability = services::Availability::Operational;
                            snapshot.windows.clear();
                        }
                        services::apply(&mut self.services, services::Update::Headlines { items: snapshot.news.clone(), note: snapshot.news_note.clone() });
                        services::apply(&mut self.services, services::Update::Status(snapshot));
                        self.news_error.clear();
                    }
                    Err(error) => self.news_error = error,
                }
                self.note_status_read();
            }
            Message::ViewerLoaded(generation, report) => self.set_viewer_status(generation, report),
            Message::ServiceStatusLoaded(_, _, result) => {
                self.news_loading = false;
                match result {
                    Ok(snapshot) => services::apply(&mut self.services, services::Update::Status(snapshot)),
                    Err(error) => {
                        // The page is unread; an official window still covering now stays maintenance.
                        if let Some(snapshot) = &mut self.services {
                            snapshot.availability = services::Availability::Unknown;
                            snapshot.availability_note = tf("{note} Official world status unavailable: {error}.",
                                &[("note", &t("Official world status is unavailable. This does not establish a game outage. Check official notices.")), ("error", &error)]);
                        }
                    }
                }
                self.note_status_read();
            }
            Message::PatchLoaded(generation, mut result) => {
                self.reload_official();
                if generation == self.patch_generation {
                    self.patch_loading = false;
                    // A version-record read contains no new server or file-check evidence.
                    // Keep the last check while that record is unchanged and readable, and
                    // an account's own folder's verdict (this read is the Settings folder's).
                    let keep_checked = self.patch_folder.is_some() || result.state == patch::PatchState::Unknown && result.remote_version == t("Not checked") && self.patch.as_ref().is_some_and(|previous| {
                        previous.local_version == result.local_version &&
                            (previous.state != patch::PatchState::Unknown || previous.files_verified.is_some())
                    });
                    if result.remote_version == t("Not checked") {
                        if let Some(previous) = self.patch.as_ref().filter(|p| p.local_version == result.local_version) {
                            result.remote_version.clone_from(&previous.remote_version);
                            if result.state == patch::PatchState::Required && result.local_version == t("Not initialized") {
                                result.files_verified = previous.files_verified;
                            }
                        }
                    }
                    if !keep_checked { self.patch = Some(result); }
                }
            }
            Message::OpenUrl(url) => {
                if !self.preview {
                    if let Err(error) = open_url(&url) {
                        self.status = error;
                    }
                }
            }
            Message::OpenLog(key) => self.open_log(&key, false),
            Message::OpenLogsFolder(key) => self.open_log(&key, true),
            Message::CloseRequested => {
                if self.settings.close_to_tray && (!self.preview || preview_idle()) && !matches!(self.self_update, self_update::State::Applying) {
                    return self.hide_window();
                }
                return self.request_quit();
            }
            Message::Tray(tray::Command::Open) => return self.show_window(),
            Message::Tray(tray::Command::Quit) => {
                let quit = self.request_quit();
                // Something holds the quit (an update, a save): the window says what.
                if !self.closing && self.window.is_none() { return Effect::batch([quit, self.show_window()]); }
                return quit;
            }
            Message::WindowClosed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                    window_chrome::detach();
                    tray::set_icon(true);
                }
            }
            Message::TrimMemory => {
                if self.window.is_none() && (!self.preview || preview_idle()) {
                    let before = footprint::memory(None);
                    footprint::trim(None);
                    let after = footprint::memory(None);
                    let show = |m: Option<footprint::Memory>| m.map_or_else(|| "unknown".to_owned(), footprint::Memory::text);
                    launch_log::append_history(&self.config.history_path(), "Crystal", &format!(
                        "background: trimmed, pid {}, {} -> {}", std::process::id(), show(before), show(after)), None);
                }
            }
            Message::CloseToTray(on) => {
                self.settings.close_to_tray = on;
                self.persist_settings();
            }
            Message::StartWithWindows(on) => {
                if self.preview { self.settings.start_with_windows = on; return Effect::none(); }
                if self.startup_task_busy { return Effect::none(); }
                self.startup_task_busy = true;
                return self.startup_task(Some(on));
            }
            Message::StartupTaskSet(on, result) => {
                self.startup_task_busy = false;
                match result {
                    Ok(()) => {
                        self.settings.start_with_windows = on;
                        self.persist_settings();
                    }
                    Err(error) => self.status = error,
                }
            }
            Message::ConfirmClose => {
                if self.maintenance.is_none() && !self.prereqs.busy() && !self.reshade.busy {
                    return self.begin_close();
                }
                self.confirm_close = false;
            }
            Message::CancelClose => self.confirm_close = false,
            Message::Feature(msg) => return self.feature_update(msg),
            Message::PreviewReady(screenshot) => {
                // Each scene (and its light variant) gets its own file.
                let light = self.settings.light_theme;
                let filename = preview::capture_name(light, self.maintenance.is_some());
                // XI_PREVIEW_DIR lets parallel worktrees keep their previews apart.
                let dir = std::env::var_os("XI_PREVIEW_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.config.experiment.join("iced-launcher"));
                let mut path = if self.page == Page::Settings {
                    dir.join(format!(
                        "preview-settings-{}{}.png",
                        self.settings_tab.flag(),
                        if light { "-light" } else { "" }
                    ))
                } else {
                    dir.join(filename)
                };
                // --preview-private captures get their own -private files.
                if privacy::on() {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    path.set_file_name(format!("{stem}-private.png"));
                }
                // --preview-ja captures get their own -ja files.
                if i18n::current() == i18n::Lang::Ja {
                    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                    path.set_file_name(format!("{stem}-ja.png"));
                }
                self.status = match save_screenshot(&path, screenshot) {
                    Ok(()) => format!("Preview saved: {}", path.display()),
                    Err(e) => e,
                };
                return Effect::Exit;
            }
        }
        Effect::none()
    }

    /// After a language change, the stored lines that are fixed sentences
    /// follow it (texts with values or from other processes keep theirs until
    /// they next change).
    fn retranslate(&mut self, from: i18n::Lang) {
        let swap = |text: &mut String, keys: &[&'static str]| {
            if let Some(shown) = retranslated(text, from, keys) {
                *text = shown.into();
            }
        };
        swap(&mut self.maintenance_result, &RESULT_TEXTS);
        swap(&mut self.status, &STATUS_TEXTS);
        for row in &mut self.accounts {
            let noted = row.note_on == row.status;
            swap(&mut row.status, &ROW_TEXTS);
            if noted { row.note_on.clone_from(&row.status); }
        }
        if let Some(patch) = &mut self.patch {
            swap(&mut patch.local_version, &PATCH_TEXTS);
            swap(&mut patch.remote_version, &PATCH_TEXTS);
            swap(&mut patch.detail, &PATCH_TEXTS);
        }
        if let Some(viewer) = &mut self.viewer { viewer.retranslate(from); }
    }
}

fn blank_account() -> Account {
    Account {
        label: String::new(),
        mode: AccountMode::Retail,
        server: String::new(),
        pol_id: String::new(),
        pol_password: String::new(),
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

/// Where a Browse dialog opens: the field's folder (or the file's folder), else the
/// nearest existing parent, else the dialog's own default.
fn dialog_start(raw: &str, file: bool) -> Option<PathBuf> {
    let mut path = PathBuf::from(raw.trim());
    if raw.trim().is_empty() || !path.is_absolute() {
        return None;
    }
    if file {
        path.pop();
    }
    let mut dir = Some(path.as_path());
    while let Some(candidate) = dir {
        if candidate.is_dir() {
            return Some(backend::plain_windows_path(candidate));
        }
        dir = candidate.parent();
    }
    None
}

async fn pick_path(target: PickTarget, start: Option<PathBuf>) -> Option<PathBuf> {
    let mut dialog = rfd::AsyncFileDialog::new();
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    let handle = match target {
        PickTarget::LoaderFile => {
            dialog
                .set_title(t("Choose the Crystal loader (pol.exe)"))
                .add_filter(t("Executable"), &["exe"])
                .pick_file()
                .await
        }
        PickTarget::ProgramFile(_) => {
            dialog
                .set_title(t("Choose a program to open with the game"))
                .add_filter(t("Executable"), &["exe"])
                .pick_file()
                .await
        }
        _ => {
            let title = match target {
                PickTarget::GameFolder | PickTarget::AccountGameFolder => t("Choose the FINAL FANTASY XI folder"),
                PickTarget::AccountDatFolder => t("Choose a DAT overlay folder containing ROM or sound folders"),
                PickTarget::InstallerFolder => t("Choose the download or extracted setup folder"),
                PickTarget::AshitaFolder => t("Choose the Ashita folder"),
                _ => t("Choose the Windower folder"),
            };
            dialog.set_title(title).pick_folder().await
        }
    };
    handle.map(|h| h.path().to_path_buf())
}

fn selected_folder(raw: &str, must_exist: bool) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw.trim());
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(
            t("Enter an absolute folder path without parent-directory (..) components.").into(),
        );
    }
    // Plain `C:\...`, not canonicalize's `\\?\C:\...`: this is stored and shown.
    match std::fs::canonicalize(&path).map(|p| backend::plain_windows_path(&p)) {
        Ok(path) if path.is_dir() => Ok(path),
        Ok(_) => Err(t("Select a folder, not a file.").into()),
        Err(_) if must_exist => {
            Err(t("Choose the existing FINAL FANTASY XI installation folder.").into())
        }
        Err(_) => {
            // Resolve the existing parent so a new download folder uses its actual location.
            let mut ancestor = path.as_path();
            let mut suffix = Vec::new();
            loop {
                if let Ok(mut resolved) =
                    std::fs::canonicalize(ancestor).map(|p| backend::plain_windows_path(&p))
                {
                    if !resolved.is_dir() {
                        return Err(t("The download folder's parent is not a directory.").into());
                    }
                    for part in suffix.into_iter().rev() {
                        resolved.push(part);
                    }
                    return Ok(resolved);
                }
                suffix.push(
                    ancestor
                        .file_name()
                        .ok_or(t("The folder path could not be resolved."))?
                        .to_owned(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or(t("The folder path could not be resolved."))?;
            }
        }
    }
}

fn initial_window_size() -> Size {
    // TEMP: XI_PREVIEW_SIZE=WxH for responsive checks.
    if let Some((w, h)) = std::env::var("XI_PREVIEW_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))
    }) {
        return Size::new(w, h);
    }
    if preview::has("--preview-small") {
        Size::new(900.0, 650.0)
    } else if preview::has("--preview-wide") {
        // a large window, to check the layout reflows at 1.0.
        Size::new(1920.0, 1080.0)
    } else {
        Size::new(1440.0, 900.0)
    }
}

/// The headline cache for `lang`: one file per language, so one language's
/// headlines are never shown in the other.
fn news_cache_path(config: &Config, lang: i18n::Lang) -> PathBuf {
    services::cache_path(&config.experiment.join("iced-launcher/news-cache.json"), lang)
}
/// The status line for a launch of `count` accounts through `tools` (none: the native client).
fn launch_status(social_only: bool, all_native: bool, count: usize, tools: &[&str]) -> String {
    if social_only { return t("Signing in to PlayOnline…").into(); }
    match (all_native, count == 1) {
        (true, true) => t("Starting 1 account in parallel. Continue in each game window after sign-in.").into(),
        (true, false) => tf("Starting {count} accounts in parallel. Continue in each game window after sign-in.", &[("count", &count)]),
        (false, true) => tf("Starting 1 account through {tools}.", &[("tools", &join_and(tools))]),
        (false, false) => tf("Starting {count} accounts through {tools}.", &[("count", &count), ("tools", &join_and(tools))]),
    }
}

/// "A", "A and B", "A and B and C"; each join is a translatable sentence part.
fn join_and(names: &[&str]) -> String {
    let mut names = names.iter();
    let mut joined = names.next().map(|first| first.to_string()).unwrap_or_default();
    for next in names {
        joined = tf("{a} and {b}", &[("a", &joined), ("b", next)]);
    }
    joined
}

/// Fixed sentences the window keeps as text and re-words on a language change
/// (each is a `t("…")` key where it is set).
const RESULT_TEXTS: [&str; 3] = [
    "Files have not been checked. Version availability alone does not verify file integrity.",
    "Game folder selected. Play checks its version before FINAL FANTASY XI starts.",
    "Installation path changed. Save settings to select this folder.",
];
const STATUS_TEXTS: [&str; 13] = [
    "Checking whether FINAL FANTASY XI is up…",
    "FFXI is down for maintenance. Starting as soon as it's back…",
    "FFXI's worlds are down. Starting as soon as they're back…",
    "FFXI is back — enter your one-time password to start",
    "Loading saved accounts…",
    "Ready. Select accounts or use Launch all. Games continue when you close Crystal.",
    "Accounts saved.",
    "Enter account details, then save.",
    "Edit account details.",
    "Settings saved. Checking the installed version…",
    "Design preview — synthetic accounts. No account vault or game connection was accessed.",
    "Finding FINAL FANTASY XI…",
    "No installed FFXI folder was found. Run official setup or use Browse.",
];
const ROW_TEXTS: [&str; 11] = [
    "Checking whether FINAL FANTASY XI is up…",
    "FFXI is down for maintenance. Starting as soon as it's back…",
    "FFXI's worlds are down. Starting as soon as they're back…",
    "FFXI is back — enter your one-time password to start",
    "Ready to launch",
    "Signed in to PlayOnline · FFXI is closed",
    "Client started · continue in game",
    "Client closed",
    "Starting sign-in…",
    "Checking the game version…",
    "Reconnecting to the background session…",
];
const PATCH_TEXTS: [&str; 7] = [
    "Unavailable",
    "Not checked",
    "Not initialized",
    "FFXI has not completed its initial update. Choose Update to finish and create its installed-version record.",
    "The official patch server is asked when you press Play, before FINAL FANTASY XI starts.",
    "A newer official version is available.",
    "The launcher compares your installation with the official patch service.",
];

/// The current language's text for `text` when it is one of `keys` as it read
/// in `from`.
fn retranslated(text: &str, from: i18n::Lang, keys: &[&'static str]) -> Option<&'static str> {
    keys.iter()
        .find(|key| text == i18n::t_in(from, key))
        .map(|key| i18n::t_in(i18n::current(), key))
}

/// The Crystal Discord's invite (the top bar's Discord button; README, Support).
pub(crate) const DISCORD_INVITE: &str = "https://discord.gg/b344cmuy7y";

fn save_screenshot(path: &std::path::Path, screenshot: Screenshot) -> Result<(), String> {
    imagecrate::save_buffer_with_format(
        path,
        &screenshot.rgba,
        screenshot.width,
        screenshot.height,
        imagecrate::ExtendedColorType::Rgba8,
        imagecrate::ImageFormat::Png,
    )
    .map_err(|e| e.to_string())
}

/// Task Scheduler for Start with Windows; unit tests never register a real task.
#[cfg(not(test))]
fn startup_task_runner() -> impl FnMut(&[String]) -> Result<(bool, String), String> { startup_task::schtasks }
#[cfg(test)]
fn startup_task_runner() -> impl FnMut(&[String]) -> Result<(bool, String), String> {
    |_: &[String]| Err("unit tests do not run schtasks".to_owned())
}

/// Close park workers on their own thread, so a re-park or a suspension never holds the UI.
/// If the thread cannot start, dropping them closes their stdin: they clean up themselves.
fn close_parks_later(workers: Vec<session_worker::ParkWorker>) {
    if workers.is_empty() { return; }
    let _ = std::thread::Builder::new().name("park-close".into()).spawn(move || pol_less::park::close_workers(workers));
}

/// Close park workers off the interface thread and wait for them and their hosts to exit (a
/// park still starting included, up to park::EXIT_WAIT): the first step of work that changes
/// the files they hold (an Update, a Repair, setup, a launcher update). `Err`: one still runs,
/// so that work must not write.
async fn close_parks_now(workers: Vec<session_worker::ParkWorker>) -> Result<(), String> {
    if workers.is_empty() { return Ok(()); }
    tokio::task::spawn_blocking(move || pol_less::park::close_workers_and_wait(workers, pol_less::park::EXIT_WAIT)).await
        .unwrap_or_else(|_| Err(t("Could not close the parked game hosts.").into()))
}

fn main() {
    // Crystal runs elevated: DLLs it loads later come from System32 only, never from its own
    // folder or the working directory (the update helper too).
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
    if let Some(result) = self_update::cli(&std::env::args().collect::<Vec<_>>()) {
        if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--windower-profile-editor-worker") {
        let _ = load_settings_and_language(&Config::default());
        let args: Vec<_> = std::env::args_os().collect();
        if args.len() != 3 { std::process::exit(1); }
        std::process::exit(launch_tools::run_profile_editor_worker(std::path::Path::new(&args[2])));
    }
    if std::env::args().any(|a| a == "--batch-worker") {
        let _ = load_settings_and_language(&Config::default());
        std::process::exit(session_worker::run_batch());
    }
    if std::env::args().any(|a| a == "--session-worker") {
        // The worker writes progress and error text that the window shows, so it
        // speaks the window's language: the same saved setting, read the same way.
        let _ = load_settings_and_language(&Config::default());
        // A park worker (pol-spec 07d §1.4) parks a host first and reads its Play later.
        let code = if std::env::args().any(|a| a == pol_less::park::PARK_ARG) { session_worker::run_park() } else { session_worker::run() };
        pol_less::profile_access::finish_pending_launchers();
        std::process::exit(code);
    }
    let preview = preview::flags().is_some();
    // Before the single-instance check, so its error box is in the user's language too.
    if !preview {
        let _ = load_settings_and_language(&Config::default());
    }
    let _instance = if preview {
        None
    } else {
        match claim_instance() {
            Ok(Some(guard)) => Some(guard),
            Ok(None) => return,
            Err(error) => {
                let text = win::wide(&error);
                let title = win::wide("Crystal Launcher");
                unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                        std::ptr::null_mut(),
                        text.as_ptr(),
                        title.as_ptr(),
                        windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                    );
                }
                return;
            }
        }
    };
    // A verified update downloaded by an earlier run (closed from the window or the tray before
    // it installed) installs now, before anything starts: its helper waits for this process to
    // exit, then starts the new version.
    if !preview && self_update::apply_at_start(&Config::default()) { return; }
    tray::note_ui_thread();
    if let Err(error) = slint_host::run() { eprintln!("{error}"); }
    tray::shutdown();
    if let Err(error) = launcher_close::finish() {
        launch_log::append_history(&Config::default().history_path(), "Crystal", &format!(
            "launcher close failed: pid {}, {error}", std::process::id()), None);
    }
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
/// Background mode (window closed to the notification area) and the gated idle Tick.
#[cfg(test)]
#[path = "main_background_tests.rs"]
mod background_tests;
