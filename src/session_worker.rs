//! The game session belongs to a headless copy of Crystal, not the window.
//! Credentials travel once through anonymous stdin; disk records contain only status.
//! Closing the window retires idle sessions; running games keep their own workers.

pub(crate) mod game_shutdown;

use crate::{
    backend::{self, Account, Config, LaunchResult},
    launch_log::ExitReport,
    pol_less::{Plan, Tool},
};
use crate::i18n::t;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

const MAX_FRAME: usize = 64 * 1024;
const PREFIX: &str = "CRYSTAL-WORKER ";
#[path = "../crystal-loader/src/start_gate.rs"]
mod start_gate;
#[path = "session_batch.rs"]
mod batch;
pub(crate) use batch::{BatchAccount, batch_pending, launch_together, run_batch};
#[path = "session_wake.rs"]
mod wake;
pub(crate) use wake::CommandWake;

pub(crate) fn listen_for_commands() -> Option<CommandWake> {
    CommandWake::listen(identity(std::process::id())?).ok()
}

#[derive(Serialize, Deserialize)]
struct BatchControl { worker: Identity, token: String }

pub(crate) fn take_native_batch(directory: &Path) -> Option<Vec<u8>> {
    let path = directory.join("native-batch.json");
    let bytes = read_bounded(&path, 1024)?;
    fs::remove_file(&path).ok()?;
    let request: BatchControl = serde_json::from_slice(&bytes).ok()?;
    if Some(request.worker) != identity(std::process::id()) { return None; }
    // Also refuses stale commands whose coordinator canceled or died.
    start_gate::Participant::open(&request.token).ok()?;
    Some(format!("{{\"type\":\"native_batch_game\",\"token\":{}}}\n", serde_json::to_string(&request.token).ok()?).into_bytes())
}

#[derive(Serialize, Deserialize)]
struct Request {
    config: Config,
    account: Account,
    otp: String,
    via: Option<Plan>,
    elapsed_ms: u64,
    #[serde(default)]
    clicked_clock: Option<crate::launch_timing::MonotonicStamp>,
    /// Private-server accounts: pass `--hairpin` to xiloader. A request from an older window
    /// has none, and gets the old behavior (on).
    #[serde(default = "hairpin_on")]
    hairpin: bool,
}
const fn hairpin_on() -> bool { true }

/// This worker's request asked for `--hairpin` (`Request::hairpin`). A worker serves one
/// request, so the sign-in reads it here instead of through every login signature.
static PRIVATE_HAIRPIN: AtomicBool = AtomicBool::new(true);

/// Whether the private-server launch this worker is running passes `--hairpin`.
pub(crate) fn private_hairpin() -> bool {
    PRIVATE_HAIRPIN.load(Ordering::Acquire)
}
impl Drop for Request {
    fn drop(&mut self) {
        self.otp.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    pid: u32,
    created: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Started {
    pid: u32,
    detail: String,
    log: PathBuf,
    timing: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Event {
    Progress(String),
    Started(Started),
    Failed(String),
    Finished(Option<ExitReport>),
    /// A park worker's host is parked (pol-spec 07d §1.4 step 5). Stdout only: never in
    /// session.json, which a park worker writes only from frame 2.
    Parked { loader_pid: u32, tool_pid: Option<u32>, polcore_ms: Option<u64>, hello_ms: u64 },
    /// The park failed or its host ended before Play; the worker exits.
    ParkFailed(String),
}
impl Event {
    /// A session record in this state still owns its session.
    fn live(&self) -> bool { matches!(self, Event::Progress(_) | Event::Started(_)) }
}

/// Frame 1 of a park worker (07d §1.4): what to park, no credentials. `account` has its
/// passwords cleared; its POL ID stays for the account's key and folders.
#[derive(Serialize, Deserialize)]
struct ParkRequest {
    config: Config,
    account: Account,
    plan: Plan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LaunchChoice {
    profile: String,
    auto_login: crate::pol_less::AutoLogin,
}
impl LaunchChoice {
    fn from_plan(plan: &Plan) -> Self {
        let profile = match plan.tool {
            Tool::Windower => plan.settings.windower_profile.trim(),
            Tool::Ashita => plan.settings.boot_ini.trim(),
            Tool::Vanilla => "",
        };
        Self { profile: profile.to_owned(), auto_login: plan.auto_login.normalized() }
    }
}

#[derive(Serialize, Deserialize)]
struct Record {
    worker: Identity,
    state: Event,
    /// Nonsecret intent and milestone; older workers default to no early readiness.
    #[serde(default)]
    social_only: bool,
    #[serde(default)]
    authenticated: bool,
    #[serde(default)]
    native_batch: bool,
    /// The session's sign-in took a one-time code (spent now), so a reopened window still
    /// signs nothing back in by itself after its game. Every worker since writes it; older
    /// workers' records (1.0.8 and before) do not say (`None`), so a reopened window learns
    /// nothing from them about whether the account needs a code.
    #[serde(default)]
    otp_used: Option<bool>,
    /// A signed-in host keeps its injected tool until it signs out.
    #[serde(default)]
    launch_tool: Option<Tool>,
    #[serde(default)]
    launch_choice: Option<LaunchChoice>,
}

// The login thread and late loader supervisor can both update this process's record.
static RECORD_WRITE: Mutex<()> = Mutex::new(());

fn record_path(config: &Config, account: &Account) -> PathBuf {
    config.account_directory(account).join("session.json")
}

/// An open handle with deny-sharing is also released if a worker crashes.
fn exclusive(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(false);
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path)
}

/// How long a Windower start waits for the previous one: the game host's own acquire of
/// the same lock gives up after this too (crystal-loader windower_start.rs).
pub(crate) const WINDOWER_TURN_LIMIT: Duration = Duration::from_secs(90);

/// How long an Ashita start waits for the previous one: past the previous start's own
/// connect and hello limits, so a turn only ends early when a holder is stuck.
pub(crate) const ASHITA_TURN_LIMIT: Duration = Duration::from_secs(90);

fn locks_directory() -> Result<PathBuf, String> {
    let root = std::env::var_os("CRYSTAL_DATA_ROOT")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .ok_or(t("Could not locate the Windower launch lock directory."))?;
    Ok(PathBuf::from(root).join("Crystal Launcher/locks"))
}

/// The machine-wide Windower launch lock file (`windower_turn_at`).
pub(crate) fn windower_lock_path() -> Result<PathBuf, String> {
    Ok(locks_directory()?.join("windower.lock"))
}

/// The machine-wide Ashita launch lock file (`ashita_turn_at`), shared by every Crystal
/// build on the machine.
pub(crate) fn ashita_lock_path() -> Result<PathBuf, String> {
    Ok(locks_directory()?.join("ashita.lock"))
}

/// Windower uses fixed named IPC objects: keep spawn-to-hello serialized across workers.
/// The turn at `path` stops when `abort` says so (`Err(abort_error)`) and when the
/// previous holder keeps the lock past `limit`. The lock is never forced or broken.
pub(crate) fn windower_turn_at(path: &Path, progress: &mut dyn FnMut(&str), abort: &dyn Fn() -> Option<String>,
    limit: Duration) -> Result<File, String> {
    launch_turn_at(path, t("Waiting for the previous Windower start to reach the game..."),
        t("Could not acquire the Windower launch lock."), progress, abort, limit)
}

/// Ashita's POL plugins share files in Ashita's folder (pivot rewrites `config\pivot\pivot.ini`
/// as it loads, inside each game host's start-up): two hosts starting together can make a
/// plugin throw, and that host ends before crystal-loader runs. Serialized like Windower's.
pub(crate) fn ashita_turn_at(path: &Path, progress: &mut dyn FnMut(&str), abort: &dyn Fn() -> Option<String>,
    limit: Duration) -> Result<File, String> {
    launch_turn_at(path, t("Waiting for the previous Ashita start to reach the game..."),
        t("Could not acquire the Ashita launch lock."), progress, abort, limit)
}

fn launch_turn_at(path: &Path, waiting: &str, failed: &str, progress: &mut dyn FnMut(&str),
    abort: &dyn Fn() -> Option<String>, limit: Duration) -> Result<File, String> {
    let deadline = Instant::now() + limit;
    let mut announced = false;
    loop {
        if let Some(error) = abort() { return Err(error); }
        match exclusive(path) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.raw_os_error() == Some(32) && Instant::now() < deadline => {
                if !announced {
                    progress(waiting);
                    announced = true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return Err(failed.into()),
        }
    }
}

/// A running process's exact identity (its pid and creation time).
fn identity(pid: u32) -> Option<Identity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Foundation::WAIT_TIMEOUT, System::Threading::WaitForSingleObject};
    open_identity(pid).filter(|(process, _)| unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_TIMEOUT)
        .map(|(_, identity)| identity)
}

/// A handle that can wait on the process, and its identity. Running or not.
fn open_identity(pid: u32) -> Option<(std::os::windows::io::OwnedHandle, Identity)> {
    use crate::win::process::{open_with_created, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE};
    let (process, created) = open_with_created(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)?;
    Some((process, Identity { pid, created }))
}

fn read_record(path: &Path) -> Option<Record> {
    serde_json::from_slice(&read_bounded(path, MAX_FRAME)?).ok()
}

/// `path`'s record while its exact worker is running and still owns its session; with
/// `worker`, only that worker's.
fn live_record(path: &Path, worker: Option<Identity>) -> Option<Record> {
    read_record(path).filter(|record| record.state.live() && worker.is_none_or(|worker| record.worker == worker)
        && identity(record.worker.pid) == Some(record.worker))
}

fn save_record(path: &Path, record: &Record) -> Result<(), String> {
    let _guard = RECORD_WRITE.lock().map_err(|_| "Could not lock the session status.")?;
    save_json(path, record)?;
    Ok(())
}

/// Records whose authenticated milestone is not on disk yet: the next update of that
/// record that succeeds carries it, whatever else it changes. A worker owns one record;
/// keying by path keeps parallel tests (and any other path) from clearing it.
static AUTHENTICATED_PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn authenticated_pending(path: &Path) -> bool {
    AUTHENTICATED_PENDING.lock().is_ok_and(|pending| pending.iter().any(|p| p == path))
}

fn set_authenticated_pending(path: &Path) {
    if let Ok(mut pending) = AUTHENTICATED_PENDING.lock() {
        if !pending.iter().any(|p| p == path) { pending.push(path.to_path_buf()); }
    }
}

fn clear_authenticated_pending(path: &Path) {
    if let Ok(mut pending) = AUTHENTICATED_PENDING.lock() { pending.retain(|p| p != path); }
}

fn update_record(path: &Path, worker: Identity, update: impl FnOnce(&mut Record)) -> Result<(), String> {
    let _guard = RECORD_WRITE.lock().map_err(|_| "Could not lock the session status.")?;
    let mut record = read_record(path).filter(|r| r.worker == worker)
        .ok_or("The session status belongs to a different worker.")?;
    let authenticated = authenticated_pending(path);
    if authenticated && record.state.live() { record.authenticated = true; }
    update(&mut record);
    save_json(path, &record)?;
    if authenticated { clear_authenticated_pending(path); }
    Ok(())
}

/// Whether `path` still holds `worker`'s record, or may again (unreadable just now):
/// a record update worth retrying.
fn record_still_ours(path: &Path, worker: Identity) -> bool {
    read_record(path).is_none_or(|record| record.worker == worker)
}

/// Called only for the native loader's typed authenticated event, never a progress label.
/// One attempt, never a sleep (this thread also carries the main-menu event): a write that
/// fails stays pending for this worker's next record update (the per-tick Started retry
/// included), so the milestone is not lost to one busy moment.
pub(crate) fn publish_authenticated(directory: &Path) -> Result<(), String> {
    let worker = identity(std::process::id()).ok_or("Could not identify the authenticated worker.")?;
    let path = directory.join("session.json");
    set_authenticated_pending(&path);
    let result = update_record(&path, worker, |record| if record.state.live() { record.authenticated = true });
    // Written, or another worker's record (this milestone has nowhere to go).
    if result.is_ok() || !record_still_ours(&path, worker) { clear_authenticated_pending(&path); }
    result
}

/// The account's last sign-in reached authentication (its record's milestone): a host or
/// worker lost after that already had its login taken, so its loss is no reason to sign in
/// again by itself (07d §5.4). A missing or unreadable record says no.
pub(crate) fn reached_authentication(config: &Config, account: &Account) -> bool {
    read_record(&record_path(config, account)).is_some_and(|record| record.authenticated)
}

#[cfg(test)]
pub(crate) fn save_authenticated_record_for_test(config: &Config, account: &Account) {
    let worker = identity(std::process::id()).unwrap();
    fs::create_dir_all(config.account_directory(account)).unwrap();
    save_record(&record_path(config, account), &Record { worker, state: Event::Failed("fixture".into()), social_only: true,
        authenticated: true, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
}

/// Published only after the verified loader hello reports this capability.
pub(crate) fn publish_native_batch(directory: &Path, supported: bool) -> Result<(), String> {
    let worker = identity(std::process::id()).ok_or("Could not identify the session worker.")?;
    update_record(&directory.join("session.json"), worker, |record| {
        if matches!(record.state, Event::Progress(_) | Event::Started(_)) { record.native_batch = supported; }
    })
}

fn save_json(path: &Path, record: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(record).map_err(|_| t("Could not encode the session status."))?;
    crate::fsutil::write_atomic(path, &bytes).map_err(|failed| {
        let message = if failed.replacing { t("Could not replace the session status.") } else { t("Could not save the session status.") };
        // These files contain only status; naming the file and OS error makes a failed
        // publication actionable without logging any account or session payload.
        format!("{message} {}: {}", path.display(), failed.error)
    })
}

#[derive(Serialize, Deserialize)]
struct FriendCache { worker: Identity, snapshot: crate::friends::Snapshot }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeControl {
    StartGame, SignOut, Visible, Away, Invisible,
    /// Change the selected handle's portrait (`wire::PORTRAIT_FIELD_CONFIRMED` only).
    Portrait(u16),
}
impl NativeControl {
    pub(crate) fn name(self) -> &'static str {
        match self { Self::StartGame => "start_game", Self::SignOut => "sign_out", Self::Visible => "visible", Self::Away => "away", Self::Invisible => "invisible", Self::Portrait(_) => "portrait" }
    }
}
/// `groups`: a status change also goes to my groups (Viewer 10040f39, 404
/// when 385() > 0). The Viewer's default for its checkbox is not verified.
#[derive(Serialize, Deserialize)]
struct ControlRequest { worker: Identity, action: NativeControl, #[serde(default = "yes")] groups: bool }
fn yes() -> bool { true }

const CONTROLLER_FRAME_LIMIT: usize = 4096;

// One-shot command channels (controller settings, game shutdown): a capability reply at
// id 0, one request file, one reply per request id.
/// How long a requester waits for the capability reply.
const ONE_SHOT_READY_LIMIT: Duration = Duration::from_secs(2);
/// The longest a request may stay deliverable (its `deadline_ms`).
const ONE_SHOT_DISPATCH_LIMIT: Duration = Duration::from_secs(5);
const ONE_SHOT_MESSAGE_LIMIT: usize = 512;

fn one_shot_message_valid(message: &str) -> bool {
    message.len() <= ONE_SHOT_MESSAGE_LIMIT && !message.chars().any(char::is_control)
}

/// `<prefix>-ready.json` for the capability reply (id 0), else `<prefix>-result-<id>.json`.
fn one_shot_reply_path(directory: &Path, prefix: &str, id: u64) -> PathBuf {
    directory.join(if id == 0 { format!("{prefix}-ready.json") } else { format!("{prefix}-result-{id}.json") })
}

/// A fresh nonzero request id.
fn one_shot_id() -> Option<u64> {
    let random = crate::pol_less::pipe::random_hex().ok()?;
    Some(u64::from_str_radix(random.get(..16)?, 16).unwrap_or(0).max(1))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControllerRequest {
    worker: Identity,
    request_id: u64,
    settings: BTreeMap<String, String>,
    /// GetTickCount64 is shared by the sender, worker and native game host.
    deadline_ms: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControllerResult { worker: Identity, request_id: u64, ok: bool, message: String }

fn controller_values_valid(settings: &BTreeMap<String, String>) -> bool {
    (2..=3).contains(&settings.len()) && settings.contains_key("padmode000") && settings.contains_key("padsin000")
        && settings.iter().all(|(name, value)| {
        matches!(name.as_str(), "padmode000" | "padsin000" | "padguid000")
            && (name == "padguid000" || !value.is_empty())
            && value.len() <= 1024 && value.bytes().all(|byte| (32..=126).contains(&byte))
    })
}

fn request_clock_ms() -> u64 {
    #[link(name = "kernel32")]
    unsafe extern "system" { fn GetTickCount64() -> u64; }
    // SAFETY: this process-independent clock takes no pointers or handles.
    unsafe { GetTickCount64() }
}

fn controller_result_path(directory: &Path, request_id: u64) -> PathBuf {
    one_shot_reply_path(directory, "controller-settings", request_id)
}

fn read_controller_result(directory: &Path, worker: Identity, request_id: u64) -> Option<ControllerResult> {
    let result: ControllerResult = serde_json::from_slice(&read_bounded(&controller_result_path(directory, request_id), CONTROLLER_FRAME_LIMIT)?).ok()?;
    (result.worker == worker && result.request_id == request_id && one_shot_message_valid(&result.message)).then_some(result)
}

fn controller_session(session: &BackgroundSession) -> Result<(), String> {
    if !session.active() { return Err("The game session ended before controller settings could be confirmed.".into()); }
    let game_running = session.path.parent().and_then(|directory| read_friend_cache(directory, session.worker))
        .and_then(|snapshot| snapshot.game_running);
    if game_running != Some(true) { return Err("Start FFXI before applying controller settings to this account.".into()); }
    Ok(())
}

fn owned_session(directory: &Path) -> Option<BackgroundSession> {
    let worker = identity(std::process::id())?;
    let session = BackgroundSession { path: directory.join("session.json"), worker };
    session.active().then_some(session)
}

/// A one-shot channel's short command lock, waited for briefly while the other side holds it.
fn command_lock(path: &Path) -> Option<File> {
    let until = Instant::now() + Duration::from_millis(150);
    loop {
        match exclusive(path) {
            Ok(lock) => return Some(lock),
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) && Instant::now() < until =>
                std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return None,
        }
    }
}

fn controller_command_lock(directory: &Path) -> Result<File, String> {
    command_lock(&directory.join("controller-settings.lock")).ok_or_else(|| "Could not access the controller settings command.".into())
}

fn no_live_controller_settings() -> &'static str {
    "This session does not support live controller settings. Close FFXI and launch it again with the current Crystal build."
}

struct PendingControllerRequest { directory: PathBuf, worker: Identity, request_id: u64 }
impl Drop for PendingControllerRequest {
    fn drop(&mut self) {
        let Ok(_lock) = controller_command_lock(&self.directory) else { return };
        let path = self.directory.join("controller-settings.json");
        let request = read_bounded(&path, CONTROLLER_FRAME_LIMIT).and_then(|bytes| serde_json::from_slice::<ControllerRequest>(&bytes).ok());
        if request.is_some_and(|request| request.worker == self.worker && request.request_id == self.request_id) {
            let _ = fs::remove_file(path);
        }
        if read_controller_result(&self.directory, self.worker, self.request_id).is_some() {
            let _ = fs::remove_file(controller_result_path(&self.directory, self.request_id));
        }
    }
}

/// Live game settings use their own command channel; POL social/control jobs
/// neither block this request nor receive its values.
/// Use the session selected by the UI; a replacement worker is never retargeted.
pub(crate) fn request_controller_settings_for_session(session: &BackgroundSession, settings: BTreeMap<String, String>) -> Result<(), String> {
    request_controller_settings_for_session_within(session, settings, ONE_SHOT_READY_LIMIT, ONE_SHOT_DISPATCH_LIMIT)
}

#[cfg(test)]
fn request_controller_settings_within(config: &Config, account: &Account, settings: BTreeMap<String, String>, ready_limit: Duration, result_limit: Duration) -> Result<(), String> {
    if !controller_values_valid(&settings) { return Err("Invalid live controller settings.".into()); }
    let session = find(config, account).ok_or("This account has no active game session.")?;
    request_controller_settings_for_session_within(&session, settings, ready_limit, result_limit)
}

fn request_controller_settings_for_session_within(session: &BackgroundSession, settings: BTreeMap<String, String>, ready_limit: Duration, result_limit: Duration) -> Result<(), String> {
    if !controller_values_valid(&settings) { return Err("Invalid live controller settings.".into()); }
    controller_session(session)?;
    let directory = session.path.parent().ok_or("The selected game session has no account directory.")?;
    // The consumer never acquires this delivery lock. Holding the short command
    // lock while awaiting a reply would prevent it from consuming the request.
    let _delivery = exclusive(&directory.join("controller-settings-delivery.lock"))
        .map_err(|_| "Another controller settings request is already running.")?;
    let until_ready = Instant::now() + ready_limit;
    loop {
        controller_session(session)?;
        if let Some(ready) = read_controller_result(directory, session.worker, 0) {
            if !ready.ok { return Err(no_live_controller_settings().into()); }
            break;
        }
        if Instant::now() >= until_ready { return Err(no_live_controller_settings().into()); }
        std::thread::sleep(Duration::from_millis(10).min(until_ready.saturating_duration_since(Instant::now())));
    }
    let request_id = one_shot_id().ok_or("Could not identify the controller settings request.")?;
    let deadline_ms = request_clock_ms().checked_add(u64::try_from(result_limit.as_millis()).unwrap_or(u64::MAX))
        .filter(|deadline| *deadline != 0).ok_or("Could not time the controller settings request.")?;
    let request = ControllerRequest { worker: session.worker, request_id, settings, deadline_ms };
    let bytes = serde_json::to_vec(&request).map_err(|_| "Could not encode live controller settings.")?;
    if bytes.len() > CONTROLLER_FRAME_LIMIT { return Err("Live controller settings are too large.".into()); }
    let _pending = PendingControllerRequest { directory: directory.to_path_buf(), worker: session.worker, request_id };
    {
        let _lock = controller_command_lock(directory)?;
        controller_session(session)?;
        let path = directory.join("controller-settings.json");
        if let Some(bytes) = read_bounded(&path, CONTROLLER_FRAME_LIMIT) {
            if serde_json::from_slice::<ControllerRequest>(&bytes).ok().is_some_and(|old|
                old.worker == session.worker && old.request_id != request_id && old.deadline_ms > request_clock_ms()) {
                return Err("Wait for the previous controller settings request to finish.".into());
            }
        }
        save_json(&path, &request)?;
    }
    wake::signal(session.worker);
    loop {
        controller_session(session)?;
        if let Some(result) = read_controller_result(directory, session.worker, request_id) {
            controller_session(session)?;
            return if result.ok { Ok(()) } else { Err(if result.message.is_empty() {
                "The game could not apply controller settings.".to_owned()
            } else { result.message }) };
        }
        if request_clock_ms() >= deadline_ms {
            return Err("The game did not confirm controller settings in time.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Consume exactly this worker's one-shot command while its game is running.
pub(crate) fn take_controller_settings(directory: &Path) -> Option<Vec<u8>> {
    let path = directory.join("controller-settings.json");
    // The idle supervisor asks every tick: no lock (shared with publishers) while nothing
    // is queued. A command written after this check is taken next tick, or on its wake.
    if !path.exists() { return None; }
    let _lock = exclusive(&directory.join("controller-settings.lock")).ok()?;
    let request: ControllerRequest = serde_json::from_slice(&read_bounded(&path, CONTROLLER_FRAME_LIMIT)?).ok()?;
    let session = owned_session(directory)?;
    if request.worker != session.worker { return None; }
    if request.request_id == 0 || request.deadline_ms == 0 || request.deadline_ms <= request_clock_ms()
        || request.deadline_ms.saturating_sub(request_clock_ms()) > ONE_SHOT_DISPATCH_LIMIT.as_millis() as u64
        || !controller_values_valid(&request.settings) {
        let _ = fs::remove_file(&path);
        return None;
    }
    controller_session(&session).ok()?;
    let mut frame = serde_json::to_vec(&serde_json::json!({
        "type": "controller_settings", "request_id": request.request_id,
        "settings": request.settings, "deadline_ms": request.deadline_ms,
    })).ok()?;
    if frame.len() >= CONTROLLER_FRAME_LIMIT { return None; }
    frame.push(b'\n');
    // A denied deletion must never deliver the same command twice.
    fs::remove_file(&path).ok()?;
    controller_session(&session).ok()?;
    (request.deadline_ms > request_clock_ms()).then_some(frame)
}

/// Before request `keep`'s reply, remove other `<prefix>-result-<id>.json` replies: a requester that timed out
/// before its reply arrived never removes that reply. One requester at a time holds the
/// channel's delivery lock, so no reply anyone still waits for is removed. Callers hold the
/// channel's command lock, like the requester's cleanup.
fn sweep_results(directory: &Path, prefix: &str, keep: u64) {
    // A capability announcement (id 0) leaves replies alone.
    if keep == 0 { return; }
    let Ok(entries) = fs::read_dir(directory) else { return };
    let start = format!("{prefix}-result-");
    let kept = format!("{start}{keep}.json");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name != kept && name.strip_prefix(&start).and_then(|rest| rest.strip_suffix(".json"))
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// id 0 announces capability; every other reply is scoped to a single request.
pub(crate) fn publish_controller_result(directory: &Path, request_id: u64, ok: bool, message: &str) -> Result<(), String> {
    if !one_shot_message_valid(message) { return Err("Invalid controller settings result.".into()); }
    let _lock = controller_command_lock(directory)?;
    let session = owned_session(directory).ok_or("The controller settings result belongs to an inactive worker.")?;
    sweep_results(directory, "controller-settings", request_id);
    save_json(&controller_result_path(directory, request_id), &ControllerResult {
        worker: session.worker, request_id, ok, message: message.to_owned(),
    })?;
    if !session.active() { return Err("The game session ended before controller settings could be confirmed.".into()); }
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub(crate) struct SocialRequest {
    worker: Identity,
    pub id: String,
    pub request: crate::social::Request,
}
impl SocialRequest {
    pub fn line(&self) -> Result<Zeroizing<Vec<u8>>, String> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(64 * 1024));
        bytes.extend_from_slice(b"{\"type\":\"native_social\",\"id\":\"");
        bytes.extend_from_slice(self.id.as_bytes());
        bytes.extend_from_slice(b"\",");
        // English: it lands in the social state, which `social::localize` shows.
        let request = Zeroizing::new(serde_json::to_vec(&self.request).map_err(|_| "Could not encode the PlayOnline request.")?);
        bytes.extend_from_slice(&request[1..]);
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// Publish one account-bound request atomically. A second window cannot replace
/// a pending send, and an old process's request cannot reach a new session.
pub(crate) fn request_social(config: &Config, account: &Account, request: crate::social::Request) -> Result<String, String> {
    let directory = config.account_directory(account);
    let _batch = exclusive(&directory.join("batch.lock"))
        .map_err(|_| "This account is waiting for its batch to start.")?;
    if !request.valid() { return Err(t("Invalid PlayOnline request.").into()); }
    let session = find_in(&directory).ok_or(t("This account has no active PlayOnline session."))?;
    let _lock = exclusive(&directory.join("native-social.lock")).map_err(|_| t("Another PlayOnline request is being submitted."))?;
    let snapshot = read_friend_cache(&directory, session.worker).ok_or(t("Launch this account with the current build to use PlayOnline messages."))?;
    if snapshot.game_running != Some(false) || !snapshot.social.ready {
        return Err(t("Return from FFXI to the signed-in PlayOnline session first.").into());
    }
    if snapshot.social.busy || directory.join("native-social.json").exists()
        || directory.join("native-control.json").exists() {
        return Err(t("Wait for the current PlayOnline request to finish.").into());
    }
    let id = crate::pol_less::pipe::random_hex().map_err(|_| t("Could not create the PlayOnline request identity."))?;
    let request = SocialRequest { worker: session.worker, id: id.clone(), request };
    let bytes = Zeroizing::new(serde_json::to_vec(&request).map_err(|_| t("Could not encode the PlayOnline request."))?);
    crate::fsutil::write_atomic(&directory.join("native-social.json"), &bytes).map_err(|failed| {
        if failed.replacing { t("Could not submit the PlayOnline request.") } else { t("Could not save the PlayOnline request.") }
    })?;
    // Wake only once nothing the worker's take_social locks is still held.
    drop(_lock);
    drop(_batch);
    wake::signal(session.worker);
    Ok(id)
}

pub(crate) fn take_social(directory: &Path) -> Option<(SocialRequest, File)> {
    // Hold the publication lock until the worker publishes busy=true. Removing
    // the one-shot file first must not expose an idle cache to a second window.
    let path = directory.join("native-social.json");
    if !path.exists() { return None; } // As take_controller_settings.
    let lock = exclusive(&directory.join("native-social.lock")).ok()?;
    let bytes = Zeroizing::new(read_bounded(&path, 64 * 1024)?);
    // A reader can deny deletion on Windows. Never deliver the message while
    // its one-shot file survives: the next idle tick would send it again.
    fs::remove_file(&path).ok()?;
    let request: SocialRequest = serde_json::from_slice(&bytes).ok()?;
    (Some(request.worker) == identity(std::process::id()) && request.request.valid()
        && request.id.len() == 32 && request.id.bytes().all(|b| b.is_ascii_hexdigit())).then_some((request, lock))
}

#[cfg(test)]
pub(crate) fn request_native_control(config: &Config, account: &Account, action: NativeControl) -> Result<(), String> {
    request_native_status(config, account, action, true)
}

pub(crate) fn request_native_status(config: &Config, account: &Account, action: NativeControl, groups: bool) -> Result<(), String> {
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    Ok(request_session_control(&session, action, groups)?)
}

/// The user picked `portrait` for this account's selected handle. Refused before anything is
/// queued until the portrait field is confirmed, so nothing is written on a guess.
pub(crate) fn request_native_portrait(config: &Config, account: &Account, portrait: u16) -> Result<(), String> {
    if !crate::wire::PORTRAIT_FIELD_CONFIRMED { return Err(crate::portraits::unavailable().into()); }
    if portrait > crate::wire::PORTRAIT_MASK { return Err(crate::i18n::tf("PlayOnline could not change your portrait ({code}).", &[("code", &-5120)])); }
    request_native_status(config, account, NativeControl::Portrait(portrait), true)
}

fn no_native_controls() -> &'static str { t("This session does not support native PlayOnline controls. Launch it using the current build.") }

/// Why a control was not queued.
#[derive(Debug)]
enum ControlRefusal {
    /// A state an idle session ends by itself: a request in flight or still queued, a lock
    /// held just now, a cache being replaced, the game not closed yet.
    Busy(&'static str),
    /// The account belongs to a pending batch or game start.
    Batch,
    /// Anything else, such as a control file that cannot be written.
    Final(String),
}
impl From<ControlRefusal> for String {
    fn from(refusal: ControlRefusal) -> Self {
        match refusal {
            ControlRefusal::Busy(message) => message.into(),
            ControlRefusal::Batch => "This account is waiting for its batch to start.".into(),
            ControlRefusal::Final(message) => message,
        }
    }
}

fn request_session_control(session: &BackgroundSession, action: NativeControl, groups: bool) -> Result<(), ControlRefusal> {
    use ControlRefusal::Busy;
    let directory = session.path.parent().ok_or(Busy(t("This account has no active PlayOnline session.")))?;
    let _batch = exclusive(&directory.join("batch.lock")).map_err(|_| ControlRefusal::Batch)?;
    // Shares the social publication lock so a control cannot race a message
    // submission or the worker's read/remove of the previous control.
    let _command = exclusive(&directory.join("native-social.lock"))
        .map_err(|_| Busy(t("Another PlayOnline request is being submitted.")))?;
    if !session.active() { return Err(Busy(t("This account has no active PlayOnline session."))); }
    let snapshot = friend_cache(directory, session.worker).map_err(|_| Busy(no_native_controls()))?;
    let startup_sign_out = matches!(action, NativeControl::SignOut)
        && snapshot.as_ref().and_then(|snapshot| snapshot.game_running).is_none()
        && read_record(&session.path).is_some_and(|record| record.worker == session.worker && record.social_only);
    if !startup_sign_out && snapshot.as_ref().and_then(|snapshot| snapshot.game_running) != Some(false) {
        return Err(Busy(t("Close FFXI before starting another game or signing out of PlayOnline.")));
    }
    if snapshot.is_some_and(|snapshot| snapshot.social.busy) || directory.join("native-social.json").exists()
        || directory.join("native-control.json").exists() {
        return Err(Busy(t("Wait for the current PlayOnline request to finish.")));
    }
    save_json(&directory.join("native-control.json"), &ControlRequest { worker: session.worker, action, groups })
        .map_err(ControlRefusal::Final)?;
    // take_native_control takes native-social.lock: a wake while it is held would be missed.
    drop(_command);
    drop(_batch);
    wake::signal(session.worker);
    Ok(())
}

/// How long a Play on a signed-in session waits for its Viewer main menu: the
/// pre-sign-in's restored UI pauses (≤ 2.65 s, 07c §8.6) and an in-flight social
/// request, whose stock policy limit is 60 s.
const MAIN_MENU_LIMIT: Duration = Duration::from_secs(90);

/// "Main menu reached and idle" as processes without the loader pipe see it (07c §2):
/// FFXI closed, `social.ready` (the loader sends `native_social_ready` only after
/// `native_main_menu`), no social request in flight and no command still queued.
fn main_menu_idle(directory: &Path, worker: Identity) -> bool {
    read_friend_cache(directory, worker).is_some_and(|s| s.game_running == Some(false) && s.social.ready && !s.social.busy)
        && !directory.join("native-social.json").exists() && !directory.join("native-control.json").exists()
}

/// A Play on a session already signed in (07c §5.3): wait for its main menu, run the stock
/// FFXI check (`check`, W20 :53001), then StartGame. The Viewer's Play button exists only at
/// an idle main menu, so a busy or still-publishing session queues rather than refuses, and
/// nothing is cancelled. A start refused because a request began during the check waits for
/// idle again within the same bound. `Ok(note)`: the worker has the StartGame.
pub(crate) fn start_signed_in(config: &Config, account: &Account, waiting: &mut dyn FnMut(&str),
    check: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    start_signed_in_within(config, account, MAIN_MENU_LIMIT, waiting, check)
}

fn start_signed_in_within(config: &Config, account: &Account, limit: Duration, waiting: &mut dyn FnMut(&str),
    check: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
    let deadline = Instant::now() + limit;
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    let directory = session.directory().to_path_buf();
    // Pauses still to come in a pre-sign-in are skipped; nothing to do once they are over.
    let _ = post_worker_note(&directory, session.worker, PLAY_PENDING_NOTE);
    let mut announced = false;
    let mut idle = || wait_main_menu_idle(&session, &directory, deadline, &mut announced, &mut *waiting);
    idle()?;
    let note = check()?;
    // Only a refusal that idle can end is retried (a request began during the check, or a
    // cache read raced its replacement); any other failure, such as a control file that
    // cannot be written, is reported at once.
    loop {
        match request_session_control(&session, NativeControl::StartGame, true) {
            Ok(()) => return Ok(note),
            Err(refusal @ ControlRefusal::Busy(_)) if batch_pending(config, account) || Instant::now() >= deadline => return Err(refusal.into()),
            Err(refusal @ (ControlRefusal::Batch | ControlRefusal::Final(_))) => return Err(refusal.into()),
            Err(ControlRefusal::Busy(_)) => {
                std::thread::sleep(Duration::from_millis(20));
                idle()?;
            }
        }
    }
}

/// Until `session` is at its idle main menu, by `deadline`. `waiting` hears once that it waits.
fn wait_main_menu_idle(session: &BackgroundSession, directory: &Path, deadline: Instant, announced: &mut bool,
    waiting: &mut dyn FnMut(&str)) -> Result<(), String> {
    loop {
        if !session.active() { return Err(t("This account has no active PlayOnline session.").into()); }
        if session.game_running() == Some(true) {
            return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
        }
        // A cache that can never show the main menu: the old immediate refusal, not 90 s
        // and a misleading timeout. A missing one is normal before the first publish.
        if friend_cache(directory, session.worker).is_err() { return Err(no_native_controls().into()); }
        if main_menu_idle(directory, session.worker) { return Ok(()); }
        if Instant::now() >= deadline {
            return Err(t("PlayOnline did not return to its main menu in time, so FFXI was not started.").into());
        }
        if !*announced {
            waiting(t("Waiting for the PlayOnline main menu before starting FFXI…"));
            *announced = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The FFXI version check alone (`check`, W20 :53001) from a signed-in session at its idle main
/// menu, with no game start after it: the Viewer's "Check Version" during maintenance, while
/// `$playbutton_enable` is 0 and `$patch_enable` is 1. Nothing else is sent to the session.
/// `still_ready` is asked again after the wait for the main menu: no W20 once it says no.
#[cfg_attr(test, allow(dead_code))] // tests record checks instead of starting them
pub(crate) fn check_at_main_menu<T>(config: &Config, account: &Account, still_ready: impl Fn() -> bool, check: impl FnOnce() -> T) -> Result<T, String> {
    check_at_main_menu_within(config, account, MAIN_MENU_LIMIT, still_ready, check)
}

fn check_at_main_menu_within<T>(config: &Config, account: &Account, limit: Duration, still_ready: impl Fn() -> bool,
    check: impl FnOnce() -> T) -> Result<T, String> {
    let deadline = Instant::now() + limit;
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    wait_main_menu_idle(&session, session.directory(), deadline, &mut false, &mut |_| {})?;
    if !still_ready() { return Err("maintenance or $patch_enable 1 ended while waiting for the main menu".into()); }
    Ok(check())
}

/// The action and its "also for my groups" choice.
pub(crate) fn take_native_control(directory: &Path) -> Option<(NativeControl, bool)> {
    take_native_control_matching(directory, false)
}

pub(crate) fn take_startup_sign_out(directory: &Path) -> bool {
    take_native_control_matching(directory, true).is_some()
}

fn take_native_control_matching(directory: &Path, sign_out_only: bool) -> Option<(NativeControl, bool)> {
    let path = directory.join("native-control.json");
    if !path.exists() { return None; } // As take_controller_settings.
    let _lock = exclusive(&directory.join("native-social.lock")).ok()?;
    let bytes = read_bounded(&path, 1024)?;
    let request = serde_json::from_slice::<ControlRequest>(&bytes).ok();
    if sign_out_only && request.as_ref().is_some_and(|request|
        !matches!(request.action, NativeControl::SignOut)
        || read_bounded(&directory.join(PLAY_PENDING_NOTE), 1024)
            .and_then(|bytes| serde_json::from_slice::<Identity>(&bytes).ok()) == Some(request.worker)) {
        return None;
    }
    fs::remove_file(&path).ok()?;
    let request = request?;
    (Some(request.worker) == identity(std::process::id())).then_some((request.action, request.groups))
}

/// The window saw input while this account's session is signed in and idle.
/// The worker forwards it as `native_activity` (the Viewer's last-input
/// stamp for auto-Away, app 10060039). Callers throttle; a pending note is
/// simply replaced.
pub(crate) fn note_native_activity(config: &Config, account: &Account) -> Result<(), String> {
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    let directory = session.directory();
    let idle = read_friend_cache(directory, session.worker).is_some_and(|s| s.game_running == Some(false));
    if !idle { return Ok(()); }
    post_worker_note(directory, session.worker, NATIVE_ACTIVITY_NOTE)
}

pub(crate) fn take_native_activity(directory: &Path) -> bool {
    take_worker_note(directory, NATIVE_ACTIVITY_NOTE)
}

const NATIVE_ACTIVITY_NOTE: &str = "native-activity.json";
const PLAY_PENDING_NOTE: &str = "native-play-pending.json";

/// A one-shot note to one worker: its identity, so another worker never takes it.
fn post_worker_note(directory: &Path, worker: Identity, name: &str) -> Result<(), String> {
    save_json(&directory.join(name), &worker)?;
    wake::signal(worker);
    Ok(())
}

/// Consume a note; true when it was for this worker.
fn take_worker_note(directory: &Path, name: &str) -> bool {
    let path = directory.join(name);
    let Some(bytes) = read_bounded(&path, 1024) else { return false };
    if fs::remove_file(&path).is_err() { return false; }
    serde_json::from_slice::<Identity>(&bytes).ok() == identity(std::process::id())
}

/// A Play is queued behind this account's pre-sign-in (pol-spec 07d §4.3): its worker sends
/// the loader `native_play_pending` once, so the Viewer pauses still to come are skipped.
/// Harmless when the pauses are over. Local only.
pub(crate) fn note_play_pending(config: &Config, account: &Account) -> Result<(), String> {
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    post_worker_note(session.directory(), session.worker, PLAY_PENDING_NOTE)
}

pub(crate) fn take_play_pending(directory: &Path) -> bool {
    take_worker_note(directory, PLAY_PENDING_NOTE)
}

/// How often, and how long at most, a sign-in that failed on the network waits for it to
/// come back before its one retry (pol-spec 07d §5.5).
pub(crate) const NETWORK_PROBE_EVERY: Duration = Duration::from_secs(3);
pub(crate) const NETWORK_PROBE_LIMIT: Duration = Duration::from_secs(300);

/// Block until the patch server's name resolves again (a fresh lookup that skips the DNS
/// cache, every `NETWORK_PROBE_EVERY`) or `until`. DNS only, through the user's resolver:
/// nothing reaches Square Enix. Unit tests never look anything up: the network is "back".
pub(crate) fn wait_for_network(until: Instant) -> bool {
    #[cfg(test)]
    { let _ = (until, NETWORK_PROBE_EVERY); true }
    #[cfg(not(test))]
    loop {
        let asked = Instant::now();
        if crate::dns::resolve_fresh(crate::dns::VIEWER_PATCH_HOST, NETWORK_PROBE_EVERY).is_ok() { return true; }
        if Instant::now() >= until { return false; }
        std::thread::sleep(NETWORK_PROBE_EVERY.saturating_sub(asked.elapsed()).min(until.saturating_duration_since(Instant::now())));
    }
}

/// A signed-in session whose worker is this test process (`find` reports it live).
#[cfg(test)]
pub(crate) fn save_live_record_for_test(config: &Config, account: &Account) {
    let worker = identity(std::process::id()).unwrap();
    fs::create_dir_all(config.account_directory(account)).unwrap();
    save_record(&record_path(config, account), &Record { worker, state: Event::Progress("signed in".into()), social_only: true,
        authenticated: true, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
}

/// `save_live_record_for_test` for a session started with `tool`.
#[cfg(test)]
pub(crate) fn save_live_record_with_tool_for_test(config: &Config, account: &Account, tool: Tool) {
    let worker = identity(std::process::id()).unwrap();
    fs::create_dir_all(config.account_directory(account)).unwrap();
    save_record(&record_path(config, account), &Record { worker, state: Event::Progress("signed in".into()), social_only: true,
        authenticated: true, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: Some(tool) }).unwrap();
}

#[cfg(test)]
pub(crate) fn save_play_pending_for_test(directory: &Path) {
    save_json(&directory.join(PLAY_PENDING_NOTE), &identity(std::process::id()).unwrap()).unwrap();
}

fn read_friend_cache(directory: &Path, worker: Identity) -> Option<crate::friends::Snapshot> {
    friend_cache(directory, worker).ok().flatten()
}

const FRIEND_CACHE_LIMIT: usize = 512 * 1024;
/// `Ok(None)`: missing, not readable just now, or another worker's file — the normal state
/// before this worker's first publish. `Err`: present but unusable until rewritten
/// (oversized, not a cache, or this worker's with out-of-bounds contents).
fn friend_cache(directory: &Path, worker: Identity) -> Result<Option<crate::friends::Snapshot>, ()> {
    let Some(cache) = read_cache_file(directory)? else { return Ok(None) };
    if cache.worker != worker { return Ok(None); }
    cache_in_bounds(&cache.snapshot).then_some(Some(cache.snapshot)).ok_or(())
}

fn read_cache_file(directory: &Path) -> Result<Option<FriendCache>, ()> {
    let path = directory.join("friends.json");
    if fs::metadata(&path).is_ok_and(|m| m.len() > FRIEND_CACHE_LIMIT as u64) { return Err(()); }
    let Some(bytes) = read_bounded(&path, FRIEND_CACHE_LIMIT) else { return Ok(None) };
    serde_json::from_slice(&bytes).map(Some).map_err(|_| ())
}

fn cache_in_bounds(snapshot: &crate::friends::Snapshot) -> bool {
    snapshot.entries.len() <= 300
        && snapshot.entries.iter().all(|f| f.name.len() <= 256 && f.location.as_ref().is_none_or(|s| s.len() <= 256))
        && snapshot.error.as_ref().is_none_or(|e| e.len() <= 4096)
        && snapshot.session_error.as_ref().is_none_or(|e| e.len() <= 4096)
        && snapshot.social.valid()
}

/// The published friend cache of any worker: `(worker, snapshot)`, the worker an opaque value
/// (pid, creation stamp) that changes when the account's session restarts. For observers (the
/// notifier) that watch a directory without owning a session; same bounds as `friend_cache`.
pub(crate) fn published_friends(directory: &Path) -> Option<((u32, u64), crate::friends::Snapshot)> {
    let cache = read_cache_file(directory).ok().flatten()?;
    cache_in_bounds(&cache.snapshot).then(|| ((cache.worker.pid, cache.worker.created), cache.snapshot))
}
pub(crate) fn publish_friends(directory: &Path, snapshot: &crate::friends::Snapshot) -> Result<(), String> {
    let worker = identity(std::process::id()).ok_or(t("Could not identify the friends session."))?;
    fs::create_dir_all(directory).map_err(|_| t("Could not save the friend list."))?;
    save_json(&directory.join("friends.json"), &FriendCache { worker, snapshot: snapshot.clone() })?;
    Ok(())
}

/// Returns only a snapshot owned by this exact, still-running account worker.
pub(crate) fn friends_snapshot(config: &Config, account: &Account) -> (bool, Option<crate::friends::Snapshot>) {
    let Some(session) = find(config, account) else { return (false, None) };
    (true, read_friend_cache(session.directory(), session.worker))
}

fn read_bounded(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path).ok()?.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

fn emit(event: &Event) {
    if let Ok(line) = serde_json::to_string(event) {
        // A closed UI pipe must never stop the keepalive owner.
        let _ = writeln!(io::stdout().lock(), "{PREFIX}{line}");
    }
}

/// The window sees a worker (or batch coordinator) end only as EOF on its stdout pipe. Its
/// std handles are inheritable, so every process it spawns later (session workers, Windower,
/// Ashita, pol.exe) would hold a copy of the pipe's write end and keep that EOF away until
/// the last of them exits. Called first thing at each headless entry point. This covers
/// raw CreateProcess/ShellExecute children only: std's `Command` duplicates an inherited
/// stdout/stderr as a new inheritable handle regardless of this flag, so every `Command`
/// these processes spawn must set stdout and stderr explicitly (see `pol_less::tools`).
fn private_std_handles() {
    {
        type Handle = *mut core::ffi::c_void;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetStdHandle(which: u32) -> Handle;
            fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
        }
        const HANDLE_FLAG_INHERIT: u32 = 1;
        // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE.
        for which in [-10i32 as u32, -11i32 as u32, -12i32 as u32] {
            // SAFETY: GetStdHandle takes no pointers; the handle it returns is this
            // process's own, and only its inherit flag changes.
            unsafe {
                let handle = GetStdHandle(which);
                if !handle.is_null() && handle as isize != -1 { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0); }
            }
        }
    }
}

pub(crate) fn run() -> i32 {
    private_std_handles();
    run_with(|config, account, otp, via, clicked, progress| {
        backend::login_with_progress(config, account, otp, via, clicked, progress)
    })
}

type Login<'a> = dyn FnOnce(&Config, Account, String, Option<Plan>, Instant, &mut dyn FnMut(&str)) -> Result<LaunchResult, String> + 'a;

/// One length-prefixed JSON frame from the window (credentials travel only this way).
fn read_frame<T: serde::de::DeserializeOwned>(input: &mut impl Read) -> Result<T, String> {
    read_frame_limited(input, MAX_FRAME).map_err(|error| match error {
        FrameError::Missing => t("Missing session request."),
        FrameError::TooLarge => t("Session request is too large."),
        FrameError::Incomplete => t("Incomplete session request."),
        FrameError::Invalid => t("Invalid session request."),
    }.into())
}

enum FrameError { Missing, TooLarge, Incomplete, Invalid }

/// A frame of at most `limit` bytes, held zeroized while it is decoded.
fn read_frame_limited<T: serde::de::DeserializeOwned>(input: &mut impl Read, limit: usize) -> Result<T, FrameError> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length).map_err(|_| FrameError::Missing)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > limit { return Err(FrameError::TooLarge); }
    let mut bytes = Zeroizing::new(vec![0u8; length]);
    input.read_exact(&mut bytes).map_err(|_| FrameError::Incomplete)?;
    serde_json::from_slice(&bytes).map_err(|_| FrameError::Invalid)
}

fn run_with(
    login: impl FnOnce(
        &Config,
        Account,
        String,
        Option<Plan>,
        Instant,
        &mut dyn FnMut(&str),
    ) -> Result<LaunchResult, String>,
) -> i32 {
    let result = (|| -> Result<(), String> {
        let mut stdin = io::stdin().lock();
        let request: Request = read_frame(&mut stdin)?;
        drop(stdin);
        serve(request, Box::new(login), &mut emit)
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            emit(&Event::Failed(error));
            1
        }
    }
}

/// The session for one request, from the account's session lock to the game's end:
/// today's worker, and a park worker from its frame 2.
fn serve(mut request: Request, login: Box<Login<'_>>, emit: &mut dyn FnMut(&Event)) -> Result<(), String> {
    {
        let path = record_path(&request.config, &request.account);
        let _account_lock = exclusive(&path.with_extension("lock")).map_err(|_| {
            t("This account already has a background session. Reopen Crystal to reconnect.")
        })?;
        let worker =
            identity(std::process::id()).ok_or(t("Could not identify the session worker."))?;
        save_record(
            &path,
            &Record {
                worker,
                state: Event::Progress(t("Starting sign-in…").into()),
                social_only: request.via.as_ref().is_some_and(|plan| plan.social_only),
                authenticated: false,
                native_batch: false,
                otp_used: Some(!request.otp.is_empty()),
                launch_tool: request.via.as_ref().map(|plan| plan.tool),
                launch_choice: request.via.as_ref().map(LaunchChoice::from_plan),
            },
        )?;
        let clicked = crate::launch_timing::MonotonicStamp::restore(request.clicked_clock, request.elapsed_ms);
        PRIVATE_HAIRPIN.store(request.hairpin, Ordering::Release);
        let launch = {
            let mut progress = |message: &str| emit(&Event::Progress(message.to_owned()));
            login(
                &request.config,
                request.account.clone(),
                std::mem::take(&mut request.otp),
                request.via.clone(),
                clicked,
                &mut progress,
            )
        };
        drop(request); // Do not retain credentials for the lifetime of the game.
        let launch = match launch {
            Ok(launch) => launch,
            Err(error) => {
                let _ = update_record(&path, worker, |record| record.state = Event::Failed(error.clone()));
                return Err(error);
            }
        };
        let started = Event::Started(Started {
            pid: launch.pid,
            detail: launch.detail.clone(),
            log: launch.log.clone(),
            timing: launch.timing.clone(),
        });
        // Readers wait on Started (reconnect, shutdown, the launch log): a write that a
        // scanner or reader blocked is retried each tick until it lands.
        let mut published = update_record(&path, worker, |record| record.state = started.clone()).is_ok();
        emit(&started);
        while launch.alive.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(100));
            if (!published || authenticated_pending(&path)) && record_still_ours(&path, worker) {
                let state = (!published).then(|| started.clone());
                published |= update_record(&path, worker, |record| if let Some(state) = state { record.state = state }).is_ok();
            }
        }
        let ended = Event::Finished(launch.outcome.lock().ok().and_then(|slot| slot.clone()));
        // A reader also sees a stale Started from an ended worker as ended (its identity is
        // gone); a few retries still keep the record's outcome.
        let mut pause = Duration::from_millis(50);
        while update_record(&path, worker, |record| record.state = ended.clone()).is_err()
            && pause <= Duration::from_millis(800) && record_still_ours(&path, worker) {
            std::thread::sleep(pause);
            pause *= 2;
        }
        emit(&ended);
        Ok(())
    }
}

// ---- the park worker (`--session-worker --park`, pol-spec 07d §1.4) ----

/// What a park worker drives: `pol_less::park::Parked`, or a fake in tests.
pub(crate) trait ParkHost {
    fn info(&self) -> crate::pol_less::park::ParkedInfo;
    fn watch(&mut self, wait: Duration) -> Option<String>;
    fn kill(self);
    #[allow(clippy::too_many_arguments)]
    fn play(self, config: &Config, plan: &Plan, account: Account, otp: Zeroizing<String>, clicked: Instant,
        progress: &mut dyn FnMut(&str), cold: Box<dyn FnOnce(Account, Zeroizing<String>, &mut dyn FnMut(&str)) -> Result<LaunchResult, String> + '_>)
        -> Result<LaunchResult, String>;
}
impl ParkHost for crate::pol_less::park::Parked {
    fn info(&self) -> crate::pol_less::park::ParkedInfo { self.info }
    fn watch(&mut self, wait: Duration) -> Option<String> { crate::pol_less::park::Parked::watch(self, wait) }
    fn kill(self) { crate::pol_less::park::Parked::kill(self) }
    fn play(self, config: &Config, plan: &Plan, account: Account, otp: Zeroizing<String>, clicked: Instant,
        progress: &mut dyn FnMut(&str), cold: Box<dyn FnOnce(Account, Zeroizing<String>, &mut dyn FnMut(&str)) -> Result<LaunchResult, String> + '_>)
        -> Result<LaunchResult, String> {
        crate::pol_less::park::Parked::play(self, config, plan, account, otp, clicked, progress, cold)
    }
}

/// Set when this park worker's stdin ends before frame 2 (the window closed the park).
static PARK_CLOSED: AtomicBool = AtomicBool::new(false);

pub(crate) fn run_park() -> i32 {
    private_std_handles();
    run_park_with(io::stdin(), |config, plan, account, closed| {
        crate::pol_less::park::park(config, plan, account, closed, &mut |_| {})
    }, emit, |config, account, otp, via, clicked, progress| {
        backend::login_with_progress(config, account, otp, via, clicked, progress)
    }, &PARK_CLOSED)
}

/// After a host parks, how often the worker looks for frame 2 and at the host: a Play's
/// credentials leave within this of the window's staggered write (07a parity condition 3).
const FRAME_POLL: Duration = Duration::from_millis(20);

/// Frame 1, park, `Parked`; then frame 2 (the Play) or EOF, watching the host meanwhile.
/// Stdin is read from frame 1 on: an EOF while still parking sets `closed`, and the park
/// stops at its next step with Windower stopped first, never orphaned by a terminate (the
/// window leaves a parking worker to exit on its own). EOF before frame 2 kills Windower and
/// then the host; a host that ends while parked is `ParkFailed`. From frame 2 this is
/// today's worker (`serve`), the parked host signing in.
fn run_park_with<H: ParkHost>(
    mut input: impl Read + Send + 'static,
    park: impl FnOnce(&Config, &Plan, &Account, &'static AtomicBool) -> Result<H, String>,
    mut emit: impl FnMut(&Event),
    cold: impl Fn(&Config, Account, String, Option<Plan>, Instant, &mut dyn FnMut(&str)) -> Result<LaunchResult, String>,
    closed: &'static AtomicBool,
) -> i32 {
    let parking: ParkRequest = match read_frame(&mut input) {
        Ok(request) => request,
        Err(error) => { emit(&Event::ParkFailed(error)); return 1; }
    };
    let (sender, frames) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new().name("park-frame".into()).spawn(move || {
        let frame = read_frame::<Request>(&mut input).ok();
        if frame.is_none() { closed.store(true, Ordering::Release); }
        let _ = sender.send(frame);
    });
    if reader.is_err() { emit(&Event::ParkFailed(t("Could not start Crystal's background session worker.").into())); return 1; }
    let mut host = match park(&parking.config, &parking.plan, &parking.account, closed) {
        Ok(host) => host,
        Err(error) => { emit(&Event::ParkFailed(error)); return 1; }
    };
    let info = host.info();
    emit(&Event::Parked { loader_pid: info.loader_pid, tool_pid: info.tool_pid, polcore_ms: info.polcore_ms, hello_ms: info.hello_ms });
    // A host parked for a while gives its untouched pages back (worker and loader; priority
    // stays normal, so a Play's warm start is not slowed).
    let parked = Instant::now();
    let mut trimmed = cfg!(test);
    let request = loop {
        if !trimmed && parked.elapsed() >= crate::footprint::IDLE_AFTER {
            trimmed = true;
            crate::footprint::trim(None);
            crate::footprint::trim(Some(info.loader_pid));
        }
        match frames.try_recv() {
            Ok(Some(request)) => break request,
            // The window closed the park (or died): nothing was sent, nothing will be.
            Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => { host.kill(); return 0; }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if let Some(why) = host.watch(FRAME_POLL) {
            emit(&Event::ParkFailed(why));
            host.kill();
            return 1;
        }
    };
    // The Play must be for the account, tool and profile this host was parked for.
    let same = backend::account_key(&request.account) == backend::account_key(&parking.account)
        && request.via.as_ref().is_some_and(|plan| crate::pol_less::park::ParkKey::for_plan(&request.account, plan)
            == crate::pol_less::park::ParkKey::for_plan(&parking.account, &parking.plan));
    let history = request.config.history_path();
    let label = request.account.label.clone();
    let login = Box::new(move |config: &Config, account: Account, otp: String, via: Option<Plan>, clicked: Instant, progress: &mut dyn FnMut(&str)| {
        let Some(plan) = via.clone().filter(|_| same) else {
            host.kill();
            return cold(config, account, otp, via, clicked, progress);
        };
        // Host lost before the credentials went out: today's cold path, named as one.
        let went_cold = std::cell::Cell::new(false);
        let fallback = Box::new(|account: Account, otp: Zeroizing<String>, progress: &mut dyn FnMut(&str)| {
            went_cold.set(true);
            let mut plan = plan.clone();
            plan.trigger = if plan.social_only { crate::launch_timing::LaunchTrigger::Connect } else { crate::launch_timing::LaunchTrigger::Play };
            cold(config, account, otp.to_string(), Some(plan), clicked, progress)
        });
        let result = host.play(config, &plan, account, Zeroizing::new(otp), clicked, progress, fallback);
        // Like `backend::login_with_progress`, a launch that ends in an error is recorded,
        // once: the cold fallback (`backend::login_with_progress`) records its own.
        if let (Err(error), false) = (&result, went_cold.get()) {
            crate::launch_log::record_failure(&history, &label, error);
        }
        result
    });
    match serve(request, login, &mut emit) {
        Ok(()) => 0,
        Err(error) => { emit(&Event::Failed(error)); 1 }
    }
}

/// The window's handle on one park worker (07d §1.5): its stdin (closing it ends the park),
/// its events, and its identity for tracking after a Play.
pub(crate) struct ParkWorker {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    events: std::sync::mpsc::Receiver<Event>,
    worker: Identity,
    ended: bool,
    /// It reported `Parked`: its host is up and reads frame 2 within FRAME_POLL.
    parked: bool,
    /// The host it reported parked, by a SYNCHRONIZE handle opened at that report, so a close
    /// can wait for that pol.exe itself (`wait_exit`). None when it can't be opened (an
    /// elevated host) or names this process (test loaders).
    host: Option<std::os::windows::io::OwnedHandle>,
}

/// Start a park worker for `account` and `plan`; frame 1 carries no password and no OTP.
pub(crate) fn spawn_park(config: &Config, account: &Account, plan: &Plan) -> Result<ParkWorker, String> {
    let mut command = session_worker_command()?;
    command.arg(crate::pol_less::park::PARK_ARG);
    spawn_park_command(command, config, account, plan)
}

/// Frame 1: no password, no OTP; the plan's per-Play fields are not used before frame 2.
fn park_frame(config: &Config, account: &Account, plan: &Plan) -> Result<Zeroizing<Vec<u8>>, String> {
    let account = account.without_secrets();
    let mut plan = plan.clone();
    plan.trigger = Default::default();
    Ok(Zeroizing::new(serde_json::to_vec(&ParkRequest { config: config.clone(), account, plan })
        .map_err(|_| t("Could not prepare the session request."))?))
}

/// A park worker running `command` instead of Crystal (window tests).
#[cfg(test)]
pub(crate) fn spawn_test_park(command: Command, config: &Config, account: &Account, plan: &Plan) -> Result<ParkWorker, String> {
    spawn_park_command(command, config, account, plan)
}

fn spawn_park_command(mut command: Command, config: &Config, account: &Account, plan: &Plan) -> Result<ParkWorker, String> {
    let bytes = park_frame(config, account, plan)?;
    let mut child = spawn_hidden(&mut command).map_err(|_| t("Could not start Crystal's background session worker."))?;
    let started = (|| -> Result<(Identity, std::process::ChildStdin, std::process::ChildStdout), String> {
        let worker = identity(child.id()).ok_or(t("Could not identify Crystal's background session worker."))?;
        let mut stdin = child.stdin.take().ok_or(t("Missing session progress pipe."))?;
        write_frame(&mut stdin, &bytes).map_err(|_| t("Could not hand credentials to the background session worker."))?;
        Ok((worker, stdin, child.stdout.take().ok_or(t("Missing session progress pipe."))?))
    })();
    let (worker, stdin, stdout) = match started {
        Ok(started) => started,
        Err(error) => { let _ = child.kill(); let _ = child.wait(); return Err(error); }
    };
    let (sender, events) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new().name("park-events".into()).spawn(move || {
        let mut output = BufReader::new(stdout);
        loop {
            match next_line::<Event>(&mut output, PREFIX) {
                Ok(Line::Event(event)) => if sender.send(event).is_err() { return },
                Ok(Line::Other | Line::Invalid) => {}
                Ok(Line::Eof | Line::TooLarge) | Err(_) => return,
            }
        }
    });
    if reader.is_err() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(t("Could not start Crystal's background session worker.").into());
    }
    Ok(ParkWorker { child, stdin: Some(stdin), events, worker, ended: false, parked: false, host: None })
}

impl crate::pol_less::park::Worker for ParkWorker {
    fn poll(&mut self) -> Option<crate::pol_less::park::Note> {
        use crate::pol_less::park::{Note, ParkedInfo};
        loop {
            match self.events.try_recv() {
                Ok(Event::Parked { loader_pid, tool_pid, polcore_ms, hello_ms }) => {
                    self.parked = true;
                    if loader_pid != std::process::id() {
                        use std::os::windows::io::FromRawHandle;
                        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
                        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, loader_pid) };
                        self.host = (!raw.is_null()).then(|| unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw as _) });
                    }
                    return Some(Note::Parked(ParkedInfo { loader_pid, tool_pid, polcore_ms, hello_ms }));
                }
                Ok(Event::ParkFailed(why)) => return Some(Note::Failed(why)),
                Ok(_) => {}
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return (!std::mem::replace(&mut self.ended, true)).then_some(Note::Ended);
                }
            }
        }
    }
    fn release(&mut self) { self.stdin = None; }
    fn exited(&mut self) -> bool { !matches!(self.child.try_wait(), Ok(None)) }
    fn terminate(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    fn parked(&self) -> bool { self.parked }
    /// The worker's and its parked host's process handles, together.
    fn wait_exit(&mut self, deadline: Instant) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Foundation::{HANDLE, WAIT_OBJECT_0}, System::Threading::WaitForMultipleObjects};
        let mut handles: Vec<HANDLE> = vec![self.child.as_raw_handle() as HANDLE];
        handles.extend(self.host.as_ref().map(|host| host.as_raw_handle() as HANDLE));
        let wait = deadline.saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128 - 1) as u32;
        let result = unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 1, wait) };
        result.wrapping_sub(WAIT_OBJECT_0) < handles.len() as u32
    }
}

impl ParkWorker {
    /// Adopt this parked worker for a Play or Connect (07d §1.5): frame 2 with the credentials
    /// and the click, then its progress exactly like `launch_command`. `Err(otp)`: the worker
    /// is gone or would not take the frame, so nothing was delivered; launch cold instead.
    /// Frame 2 goes only once the host has parked (a parked host reads it within FRAME_POLL),
    /// and only at its slot in `deliveries`: the stagger spaces the credentials themselves,
    /// not writes a still-parking worker would sit on (07a parity condition 3).
    pub(crate) fn play(mut self, config: &Config, account: Account, otp: String, via: Plan, clicked: Instant,
        deliveries: &Mutex<Option<Instant>>, mut progress: impl FnMut(&str)) -> Result<Result<LaunchResult, String>, String> {
        use crate::pol_less::park::Worker;
        let otp = Zeroizing::new(otp);
        if self.exited() || self.stdin.is_none() { return Err(otp.to_string()); }
        if !via.social_only && batch_pending(config, &account) {
            return Ok(Err("This account is waiting for its batch to start.".into()));
        }
        let _launch_claim = if !via.social_only {
            match exclusive(&config.account_directory(&account).join("batch.lock")) {
                Ok(claim) => Some(claim),
                Err(_) => return Ok(Err("This account is waiting for its batch to start.".into())),
            }
        } else { None };
        // Still parking: its own limits bound this wait. A park that fails delivered nothing.
        while !self.parked {
            match self.events.recv() {
                Ok(Event::Parked { .. }) => self.parked = true,
                Ok(Event::ParkFailed(_)) | Err(_) => {
                    let _ = self.child.wait();
                    return Err(otp.to_string());
                }
                Ok(_) => {}
            }
        }
        let wait = crate::pol_less::park::reserve_delivery(deliveries, Instant::now(), crate::pol_less::park::jitter());
        if !wait.is_zero() { std::thread::sleep(wait); }
        let path = record_path(config, &account);
        let request = Request {
            config: config.clone(),
            account,
            otp: otp.to_string(),
            via: Some(via),
            elapsed_ms: clicked.elapsed().as_millis().min(u64::MAX as u128) as u64,
            clicked_clock: crate::launch_timing::MonotonicStamp::capture(clicked),
            hairpin: true,
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&request).map_err(|_| otp.to_string())?);
        drop(request);
        if bytes.len() > MAX_FRAME { return Ok(Err(t("Session request is too large.").into())); }
        let sent = self.stdin.as_mut().map_or(Err(io::ErrorKind::BrokenPipe.into()), |stdin| {
            write_frame(stdin, &bytes).and_then(|_| stdin.flush())
        });
        drop(bytes);
        if sent.is_err() { return Err(otp.to_string()); }
        // From here the worker owns the session; its stdin no longer matters.
        self.stdin = None;
        // A park that fails (or a worker that ends) before frame 2 was taken up delivered
        // nothing: the Play goes cold. The host reports progress before any credential leaves.
        let mut otp = Some(otp);
        loop {
            match self.events.recv() {
                Ok(Event::Progress(message)) => { otp = None; progress(&message) }
                Ok(Event::Failed(error)) => {
                    let _ = self.child.wait();
                    return Ok(Err(error));
                }
                Ok(Event::Started(started)) => return Ok(track(path, self.worker, started)),
                Ok(Event::Finished(_)) => return Ok(Err(t("The background session ended before starting the game.").into())),
                Ok(Event::ParkFailed(_)) | Err(_) if otp.is_some() => {
                    let _ = self.child.wait();
                    return Err(otp.map(|otp| otp.to_string()).unwrap_or_default());
                }
                Ok(Event::Parked { .. } | Event::ParkFailed(_)) => {}
                Err(_) => return Ok(Err(t("The background session worker ended before starting the game.").into())),
            }
        }
    }
}


/// Holds only weak handles: a monitor whose `alive` nobody holds any more (a later Play on
/// the same retained session tracks it again) stops instead of polling until the session ends.
fn monitor(
    path: PathBuf,
    worker: Identity,
    alive: std::sync::Weak<AtomicBool>,
    game_running: std::sync::Weak<AtomicBool>,
    outcome: std::sync::Weak<Mutex<Option<ExitReport>>>,
) {
    let mut previous_friends = None;
    loop {
        // `alive` is what every holder watches; the other two may be dropped earlier.
        let Some(alive) = alive.upgrade() else { return };
        let (game_running, outcome) = (game_running.upgrade(), outcome.upgrade());
        let modified = path.parent().and_then(|d| fs::metadata(d.join("friends.json")).ok()).and_then(|m| m.modified().ok());
        if modified != previous_friends {
            if let Some(snapshot) = path.parent().and_then(|d| read_friend_cache(d, worker)) {
                if let (Some(game), Some(running)) = (snapshot.game_running, &game_running) { running.store(game, Ordering::Release); }
                previous_friends = modified;
            }
        }
        let report = match read_record(&path).filter(|r| r.worker == worker).map(|record| record.state) {
            Some(Event::Finished(report)) => Some(report),
            Some(Event::Failed(_)) => Some(Some(ExitReport::other(Some(1)))),
            _ if identity(worker.pid) != Some(worker) => Some(Some(ExitReport::worker_ended())),
            _ => None,
        };
        if let Some(report) = report {
            if let Some(Ok(mut slot)) = outcome.as_ref().map(|outcome| outcome.lock()) { *slot = report; }
            alive.store(false, Ordering::Release);
            if let Some(running) = &game_running { running.store(false, Ordering::Release); }
            return;
        }
        drop((alive, game_running, outcome));
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn track(path: PathBuf, worker: Identity, started: Started) -> Result<LaunchResult, String> {
    let alive = Arc::new(AtomicBool::new(true));
    let game_running = Arc::new(AtomicBool::new(path.parent().and_then(|d| read_friend_cache(d, worker)).and_then(|s| s.game_running).unwrap_or(true)));
    let outcome = Arc::new(Mutex::new(None));
    let (tracked, ended, game) = (Arc::downgrade(&alive), Arc::downgrade(&outcome), Arc::downgrade(&game_running));
    std::thread::Builder::new()
        .name("session-status".into())
        .spawn(move || monitor(path, worker, tracked, game, ended))
        .map_err(|_| t("Could not watch the background session."))?;
    Ok(LaunchResult {
        pid: started.pid,
        detail: started.detail,
        log: started.log,
        timing: started.timing,
        alive,
        game_running,
        outcome,
    })
}

#[derive(Clone)]
pub(crate) struct BackgroundSession {
    path: PathBuf,
    worker: Identity,
}

/// Find an exact living worker; a reused PID or a completed record cannot reattach.
pub(crate) fn find(config: &Config, account: &Account) -> Option<BackgroundSession> {
    find_in(&config.account_directory(account))
}

/// `find` for an account directory already resolved.
fn find_in(directory: &Path) -> Option<BackgroundSession> {
    let path = directory.join("session.json");
    let worker = live_record(&path, None)?.worker;
    Some(BackgroundSession { path, worker })
}

impl BackgroundSession {
    pub(crate) fn sign_out(self) -> Result<(), String> {
        if !self.can_prepare_update() {
            return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
        }
        close_idle_sessions(vec![IdleSessionClose::known(self.clone())])?;
        // A Play from another window can win while we wait. Never claim that
        // account signed out, or stop the game it started.
        if identity(self.worker.pid) == Some(self.worker) {
            return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
        }
        Ok(())
    }

    fn uses_windower(&self) -> bool {
        read_record(&self.path).is_some_and(|record| record.worker == self.worker
            && record.launch_tool == Some(Tool::Windower))
    }

    pub(crate) fn game_running(&self) -> Option<bool> {
        if !self.active() { return None; }
        self.path.parent().and_then(|directory| read_friend_cache(directory, self.worker))
            .and_then(|snapshot| snapshot.game_running)
    }
    pub(crate) fn ensure_launch_tool(&self, requested: Tool) -> Result<(), String> {
        let record = self.live_record()
            .ok_or_else(|| t("The background session ended before Play could start. Sign in again.").to_owned())?;
        // Old workers do not identify their tool. They can reconnect as before,
        // but cannot promise Vanilla because their host may already contain hooks.
        if record.launch_tool.is_some_and(|tool| tool != requested)
            || record.launch_tool.is_none() && requested == Tool::Vanilla {
            return Err(t("Sign out of PlayOnline before switching launch methods, then launch again.").into());
        }
        Ok(())
    }

    /// A retained owner cannot silently change its injected profile or login slot.
    pub(crate) fn ensure_launch_plan(&self, plan: &Plan) -> Result<(), String> {
        self.ensure_launch_tool(plan.tool)?;
        let record = read_record(&self.path).filter(|record| record.worker == self.worker)
            .ok_or_else(|| t("The background session ended before Play could start. Sign in again.").to_owned())?;
        if record.launch_choice.is_some_and(|choice| choice != LaunchChoice::from_plan(plan)) {
            return Err(t("Sign out of PlayOnline to apply the changed profile or character login settings.").into());
        }
        Ok(())
    }

    /// The session's launch log, once its worker published `Started`.
    pub(crate) fn log(&self) -> Option<PathBuf> {
        read_record(&self.path).filter(|record| record.worker == self.worker)
            .and_then(|record| match record.state { Event::Started(started) => Some(started.log), _ => None })
    }

    fn live_record(&self) -> Option<Record> { live_record(&self.path, Some(self.worker)) }

    /// The account directory holding this session's files (the record's folder).
    fn directory(&self) -> &Path { self.path.parent().unwrap_or(Path::new(".")) }

    fn active(&self) -> bool { self.live_record().is_some() }

    pub(crate) fn supports_batch_start(&self) -> bool {
        self.live_record().is_some_and(|record| record.native_batch)
    }

    /// A known idle session, or a sign-in explicitly started without a game.
    /// Unknown game state on an ordinary launch must never be treated as idle.
    pub(crate) fn can_prepare_update(&self) -> bool {
        let Some(record) = self.live_record() else { return false };
        let snapshot = self.path.parent().and_then(|directory| read_friend_cache(directory, self.worker));
        match snapshot.and_then(|snapshot| snapshot.game_running) {
            Some(running) => !running,
            None => record.social_only,
        }
    }

    /// Whether this session's sign-in took a one-time code (see `Record::otp_used`); `None`
    /// when its worker's record does not say (an older worker, or the record is unreadable).
    pub(crate) fn otp_used(&self) -> Option<bool> {
        read_record(&self.path).filter(|record| record.worker == self.worker).and_then(|record| record.otp_used)
    }

    /// A reopened window can queue Play on a sign-in-only worker without starting another.
    pub(crate) fn preparing_social(&self) -> bool {
        self.live_record().is_some_and(|record| record.social_only)
            && !self.path.parent().and_then(|directory| read_friend_cache(directory, self.worker))
                .is_some_and(|snapshot| snapshot.social.ready && snapshot.game_running.is_some())
    }

    /// Wait outside the UI thread if the previous window closed during sign-in.
    pub(crate) fn reconnect(self, mut progress: impl FnMut(&str)) -> Result<LaunchResult, String> {
        let mut previous = String::new();
        loop {
            if let Some(record) = read_record(&self.path) {
                if record.worker != self.worker { return Err(t("The background session worker changed.").into()); }
                match record.state {
                    Event::Started(started) => return track(self.path, self.worker, started),
                    Event::Failed(error) => return Err(error),
                    Event::Finished(report) => {
                        return Err(report
                            .map_or_else(|| t("The background session ended.").into(), |r| r.message()))
                    }
                    Event::Progress(message) if message != previous => {
                        progress(&message);
                        previous = message;
                    }
                    _ => {}
                }
            }
            if identity(self.worker.pid) != Some(self.worker) {
                return Err(t("The background session worker ended.").into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Captures only the session path and process identity, never credentials. A
/// sign-in thread may not have published its worker yet when the window closes.
pub(crate) struct IdleSessionClose {
    path: PathBuf,
    worker: Option<Identity>,
    requested: bool,
    /// Set by the sign-in thread just before it reports completion: a sign-in that ended
    /// before any worker was seen leaves nothing to wait for (main.rs `AccountRow::sign_in_done`).
    sign_in_done: Option<Arc<AtomicBool>>,
}

impl IdleSessionClose {
    fn known(session: BackgroundSession) -> Self {
        Self { path: session.path, worker: Some(session.worker), requested: false, sign_in_done: None }
    }

    /// The row's sign-in thread sets `done` (Release) just before its completion is sent.
    pub(crate) fn with_sign_in_done(mut self, done: Arc<AtomicBool>) -> Self {
        self.sign_in_done = Some(done);
        self
    }

    pub(crate) fn for_account(config: &Config, account: &Account, preparing: bool) -> Option<Self> {
        let directory = config.account_directory(account);
        if let Some(session) = find_in(&directory) {
            return session.can_prepare_update().then(|| Self::known(session));
        }
        let path = directory.join("session.json");
        if let Some(record) = read_record(&path).filter(|record|
            identity(record.worker.pid) == Some(record.worker)
            && matches!(record.state, Event::Finished(_) | Event::Failed(_))) {
            return Some(Self { path, worker: Some(record.worker), requested: true, sign_in_done: None });
        }
        preparing.then_some(Self { path, worker: None, requested: false, sign_in_done: None })
    }

    /// True when the captured worker ended, or a game/queued Play needs to keep
    /// running. Completion records alone precede Windower's final profile save.
    fn finished(&mut self) -> bool {
        let Some(worker) = self.worker else {
            // A sign-in still running: adopt its worker once it shows one. A worker that
            // already failed or finished is waited on until its process exits, no longer.
            match read_record(&self.path).filter(|record| identity(record.worker.pid) == Some(record.worker)) {
                Some(record) if record.state.live() => self.worker = Some(record.worker),
                Some(record) => { self.worker = Some(record.worker); self.requested = true; }
                None => return self.sign_in_done.as_ref().is_some_and(|done| done.load(Ordering::Acquire)),
            }
            return false;
        };
        if identity(worker.pid) != Some(worker) { return true; }
        let session = BackgroundSession { path: self.path.clone(), worker };
        if self.requested { return session.game_running() == Some(true); }
        let Some(record) = session.live_record() else { return false };
        let Some(directory) = self.path.parent() else { return false };
        let snapshot = read_friend_cache(directory, worker);
        if snapshot.as_ref().is_some_and(|snapshot| snapshot.game_running == Some(true)) { return true; }
        let command = directory.join("native-control.json");
        if command.exists() {
            if let Some(control) = read_bounded(&command, 1024).and_then(|bytes| serde_json::from_slice::<ControlRequest>(&bytes).ok())
                .filter(|control| control.worker == worker) {
                match control.action {
                    NativeControl::StartGame => return true,
                    NativeControl::SignOut => self.requested = true,
                    _ => {},
                }
            }
            return false;
        }
        // can_prepare_update on this poll's record and cache (a running game returned above).
        let can_prepare = snapshot.as_ref().and_then(|snapshot| snapshot.game_running).map_or(record.social_only, |running| !running);
        let ready = can_prepare && snapshot.is_none_or(|snapshot| !snapshot.social.busy)
            && !directory.join("native-social.json").exists();
        if ready && request_session_control(&session, NativeControl::SignOut, true).is_ok() {
            self.requested = true;
        }
        false
    }
}

/// Sign out each captured idle owner through polcore and wait for its worker to
/// exit. Recheck game state and command locks on every submission; no process is
/// terminated, and an active game or a queued Play wins over launcher shutdown.
pub(crate) fn close_idle_sessions(mut sessions: Vec<IdleSessionClose>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        sessions.retain_mut(|session| !session.finished());
        if sessions.is_empty() { return Ok(()); }
        if Instant::now() >= deadline {
            return Err(t("Timed out signing out of PlayOnline. Crystal stayed open; running games were kept.").into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(crate) fn launch(
    config: &Config,
    account: Account,
    otp: String,
    via: Option<Plan>,
    clicked: Instant,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    let otp = Zeroizing::new(otp);
    // Local readiness only: the Viewer's W2U goes out at Connect, inside this sign-in
    // (pol-spec 07c §1, §3), never before it.
    // The US Viewer's readiness; an EU/JP account uses its own region's Viewer (`retail_region`).
    if account.mode == backend::AccountMode::Retail && find(config, &account).is_none()
        && crate::retail_region::us_viewer_gates(true, account.region) {
        crate::viewer::gate(&crate::viewer::local_status())?;
    }
    launch_preflighted(config, account, otp, via, clicked, progress)
}

/// Ready for the SignOut an update or the profile editor sends: the sign-in finished and
/// the session is idle (cache says no game, not busy), with no request in flight or queued.
/// Unlike launcher close, a sign-in still running is waited out, never cancelled.
fn sign_out_ready(session: &BackgroundSession, directory: &Path) -> bool {
    session.can_prepare_update()
        && read_friend_cache(directory, session.worker)
            .is_some_and(|snapshot| snapshot.game_running == Some(false) && !snapshot.social.busy)
        && !directory.join("native-social.json").exists() && !directory.join("native-control.json").exists()
}

/// Finish idle POL sessions through their native sign-out path before file work.
/// Returning success means every captured owner ended; the updater independently
/// checks for clients again before changing either installation.
pub(crate) async fn sign_out_for_update(sessions: Vec<BackgroundSession>, operation: &crate::operation::Operation) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    operation.check_cancelled()?;
    if sessions.iter().any(|session| session.active() && !session.can_prepare_update()) {
        return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
    }
    let total = sessions.len() as u64;
    operation.progress(crate::operation::Stage::SignOut, 0, total, t("Signing out idle PlayOnline sessions before updating"));
    for session in &sessions {
        while session.active() {
            operation.check_cancelled()?;
            if !session.can_prepare_update() {
                return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
            }
            if Instant::now() >= deadline {
                return Err(t("Timed out waiting for PlayOnline to sign out. No update was started.").into());
            }
            let directory = session.path.parent().unwrap();
            // A refusal that waiting ends (a request queued or in flight) is "not yet".
            if sign_out_ready(session, directory) {
                match request_session_control(session, NativeControl::SignOut, true) {
                    Ok(()) => break,
                    Err(ControlRefusal::Final(error)) => return Err(error),
                    Err(ControlRefusal::Busy(_) | ControlRefusal::Batch) => {}
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    loop {
        operation.check_cancelled()?;
        let active: Vec<_> = sessions.iter().filter(|session| session.active()).collect();
        operation.progress(crate::operation::Stage::SignOut, total - active.len() as u64, total,
            t("Signing out idle PlayOnline sessions before updating"));
        if active.is_empty() { return Ok(()); }
        if active.iter().any(|session| !session.can_prepare_update()) {
            return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
        }
        if Instant::now() >= deadline {
            return Err(t("Timed out waiting for PlayOnline to sign out. No update was started.").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Retire idle Windower owners before its editor reads settings.xml. A worker
/// can publish Finished before its Windower child saves, so wait for the actual
/// worker exit, not just its session record. Active games keep running.
pub(crate) fn sign_out_for_profile_editor(sessions: Vec<BackgroundSession>, mut progress: impl FnMut(&str)) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    for session in sessions.into_iter().filter(BackgroundSession::uses_windower) {
        if !session.can_prepare_update() { continue; }
        progress(t("Signing out idle PlayOnline sessions so Windower can edit profiles..."));
        let mut requested = false;
        while identity(session.worker.pid) == Some(session.worker) {
            if session.active() && !requested {
                // A Play queued elsewhere wins over the editor; never replace it
                // with SignOut, and never stop a game that started meanwhile.
                if !session.can_prepare_update() { break; }
                let directory = session.path.parent().ok_or(t("Could not locate the PlayOnline session."))?;
                let queued = directory.join("native-control.json");
                if queued.exists() {
                    return Err(t("Wait for the current PlayOnline request to finish, then edit profiles.").into());
                }
                if sign_out_ready(&session, directory) {
                    match request_session_control(&session, NativeControl::SignOut, true) {
                        Ok(()) => requested = true,
                        Err(ControlRefusal::Final(error)) => return Err(error),
                        Err(ControlRefusal::Busy(_) | ControlRefusal::Batch) => {}
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(t("Timed out waiting for Windower to save profiles. No editor was opened.").into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(())
}

// The batch observer already checked the installed Viewer before handing off
// the anonymous request. Its detached coordinator must not repeat that query
// in every child process. Native component validation still runs in each host.
fn launch_preflighted(
    config: &Config,
    account: Account,
    otp: Zeroizing<String>,
    via: Option<Plan>,
    clicked: Instant,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    launch_command(session_worker_command()?, config, account, otp, via, clicked, progress)
}

/// This executable as a headless session worker.
fn session_worker_command() -> Result<Command, String> {
    self_command("--session-worker").map_err(|_| t("Could not locate Crystal's session worker.").into())
}

/// This executable with `arg`: a headless worker of this build.
fn self_command(arg: &str) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg(arg);
    Ok(command)
}

/// Start a headless child: stdin and stdout piped, no stderr, no console window.
fn spawn_hidden(command: &mut Command) -> io::Result<std::process::Child> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    command.spawn()
}

/// One length-prefixed frame, the way `read_frame` reads it.
fn write_frame(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    output.write_all(&(bytes.len() as u32).to_le_bytes())?;
    output.write_all(bytes)
}

/// One line of a headless child's stdout.
enum Line<T> {
    /// The child closed its stdout.
    Eof,
    /// Longer than MAX_FRAME.
    TooLarge,
    /// Not one of ours (no `prefix`).
    Other,
    Event(T),
    /// Ours, but not a valid event.
    Invalid,
}

fn next_line<T: serde::de::DeserializeOwned>(input: &mut impl BufRead, prefix: &str) -> io::Result<Line<T>> {
    let mut line = String::new();
    let count = (&mut *input).take((MAX_FRAME + 1) as u64).read_line(&mut line)?;
    Ok(if count == 0 { Line::Eof } else if count > MAX_FRAME { Line::TooLarge } else {
        match line.trim_end().strip_prefix(prefix) {
            None => Line::Other,
            Some(json) => serde_json::from_str(json).map_or(Line::Invalid, Line::Event),
        }
    })
}

fn launch_command(
    command: Command,
    config: &Config,
    account: Account,
    otp: Zeroizing<String>,
    via: Option<Plan>,
    clicked: Instant,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    launch_command_with(command, config, account, otp, via, true, clicked, progress)
}

/// A private-server account's launch, in its own session worker like a retail one: xiloader
/// on its own (`via` None) or through Windower or Ashita. No PlayOnline readiness applies.
pub(crate) fn launch_private(
    config: &Config,
    account: Account,
    otp: String,
    via: Option<Plan>,
    hairpin: bool,
    clicked: Instant,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    if account.mode != backend::AccountMode::Private {
        return Err("Only private-server accounts launch through xiloader.".into());
    }
    launch_command_with(session_worker_command()?, config, account, Zeroizing::new(otp), via, hairpin, clicked, progress)
}

#[allow(clippy::too_many_arguments)]
fn launch_command_with(
    mut command: Command,
    config: &Config,
    account: Account,
    mut otp: Zeroizing<String>,
    via: Option<Plan>,
    hairpin: bool,
    clicked: Instant,
    mut progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    if via.as_ref().is_some_and(|p| !p.social_only) && batch_pending(config, &account) {
        return Err("This account is waiting for its batch to start.".into());
    }
    let directory = config.account_directory(&account);
    if let Some(session) = find_in(&directory) {
        if let Some(plan) = &via { session.ensure_launch_plan(plan)?; }
        if via.as_ref().is_some_and(|p| !p.social_only)
            && read_friend_cache(&directory, session.worker).is_some_and(|s| s.game_running == Some(false)) {
            // This session is already signed in: its main menu, then the stock W20, then
            // StartGame (07c §5.3). A fresh Play checks inside its own worker. The trigger
            // and W20 lines go to the session's own log (07d §4.2, 08 §4 item 6).
            let log = session.log();
            let clock = crate::launch_timing::Timeline::begin_at(clicked);
            if let Some(log) = &log {
                crate::launch_log::append_launcher_lines(log, &crate::pol_less::launch_clock_lines(&clock, crate::launch_timing::LaunchTrigger::PreSignedPlay));
            }
            start_signed_in(config, &account, &mut progress, || {
                // An EU/JP session: the same check to its region's name for the host, and an
                // update left to the official Viewer (`retail_region`).
                if !crate::retail_region::us_viewer_gates(account.mode == backend::AccountMode::Retail, account.region) {
                    let (check, timing) = crate::patch::check_before_launch_at(&crate::pol_less::install_directory_for(config, account.region)?, crate::retail_region::game_patch_host(account.region));
                    if let Some(log) = &log { crate::launch_log::append_launcher_lines(log, &crate::launch_timing::check_lines("FFXI check (W20)", &timing)); }
                    return crate::retail_region::game_gate(account.region, crate::patch::gate(check));
                }
                let (verdict, timing) = crate::patch::gate_before_start_timed(&crate::pol_less::install_directory(config)?);
                if let Some(log) = &log { crate::launch_log::append_launcher_lines(log, &crate::launch_timing::check_lines("FFXI check (W20)", &timing)); }
                if verdict.as_ref().is_err_and(|message| crate::patch::is_update_refusal(message)) {
                    crate::patch::save_update_close(&directory);
                }
                verdict
            })?;
        }
        return session.reconnect(progress);
    }
    let _launch_claim = if via.as_ref().is_some_and(|p| !p.social_only) {
        Some(exclusive(&directory.join("batch.lock"))
            .map_err(|_| "This account is waiting for its batch to start.")?)
    } else { None };
    let path = directory.join("session.json");
    let request = Request {
        config: config.clone(),
        account,
        otp: std::mem::take(&mut *otp),
        via,
        elapsed_ms: clicked.elapsed().as_millis().min(u64::MAX as u128) as u64,
        clicked_clock: crate::launch_timing::MonotonicStamp::capture(clicked),
        hairpin,
    };
    let bytes = Zeroizing::new(
        serde_json::to_vec(&request).map_err(|_| t("Could not prepare the session request."))?,
    );
    drop(request);
    if bytes.len() > MAX_FRAME {
        return Err(t("Session request is too large.").into());
    }
    let mut child = spawn_hidden(&mut command)
        .map_err(|_| t("Could not start Crystal's background session worker."))?;
    let Some(worker) = identity(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(t("Could not identify Crystal's background session worker.").into());
    };
    let sent = child.stdin.take().ok_or_else(|| io::Error::other("Missing worker pipe"))
        .and_then(|mut input| write_frame(&mut input, &bytes));
    drop(bytes);
    if sent.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(t("Could not hand credentials to the background session worker.").into());
    }
    let output = child
        .stdout
        .take()
        .ok_or(t("Missing session progress pipe."))?;
    let mut output = BufReader::new(output);
    loop {
        let event = match next_line::<Event>(&mut output, PREFIX).map_err(|_| t("Could not read session progress."))? {
            Line::Eof => return Err(t("The background session worker ended before starting the game.").into()),
            Line::TooLarge => return Err(t("Session progress is too large.").into()),
            Line::Other => continue,
            Line::Invalid => return Err(t("Invalid session progress.").into()),
            Line::Event(event) => event,
        };
        match event {
            Event::Progress(message) => progress(&message),
            Event::Failed(error) => {
                let _ = child.wait();
                return Err(error);
            }
            Event::Started(started) => {
                // Dropping Child/stdout does not terminate the worker. Its session and
                // credentials never depend on this observer remaining open.
                drop(output);
                return track(path, worker, started);
            }
            Event::Finished(_) => {
                return Err(t("The background session ended before starting the game.").into())
            }
            // Only a park worker sends these, and only before its frame 2.
            Event::Parked { .. } | Event::ParkFailed(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller_fixture(root: &Path, game_running: Option<bool>) -> (Config, Account, PathBuf, Identity) {
        let config = config(root); let account = account();
        let directory = config.account_directory(&account); fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("fixture game".into()),
            social_only: false, authenticated: true, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        publish_friends(&directory, &crate::friends::Snapshot { game_running, ..Default::default() }).unwrap();
        (config, account, directory, worker)
    }

    fn controller_values() -> BTreeMap<String, String> {
        [("padmode000".to_owned(), "1,0,0,0,0,1".to_owned()), ("padsin000".to_owned(), "0,1,2,3".to_owned())].into()
    }

    #[test]
    fn controller_settings_round_trip_without_taking_the_pol_social_lock() {
        let fixture = Fixture::new();
        let (config, account, directory, worker) = controller_fixture(&fixture.0, Some(true));
        fs::write(directory.join("native-social.json"), b"unrelated social request").unwrap();
        fs::write(directory.join("native-control.json"), b"unrelated POL control").unwrap();
        let _social_lock = exclusive(&directory.join("native-social.lock")).unwrap();
        let consumer_dir = directory.clone();
        let consumer = std::thread::spawn(move || {
            // Capability may arrive while the requester holds its delivery lock.
            publish_controller_result(&consumer_dir, 0, true, "Controller settings are available.").unwrap();
            let until = Instant::now() + Duration::from_secs(2);
            let frame = loop {
                if let Some(frame) = take_controller_settings(&consumer_dir) { break frame; }
                assert!(Instant::now() < until, "the command must be consumable while the producer awaits its reply");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(exclusive(&consumer_dir.join("controller-settings-delivery.lock")).is_err());
            assert!(take_controller_settings(&consumer_dir).is_none(), "deliver exactly once");
            let frame: serde_json::Value = serde_json::from_slice(&frame).unwrap();
            assert_eq!(frame["type"], "controller_settings");
            assert!(frame.get("worker").is_none());
            assert_eq!(frame["settings"], serde_json::to_value(controller_values()).unwrap());
            assert!(frame["deadline_ms"].as_u64().unwrap() > request_clock_ms());
            publish_controller_result(&consumer_dir, frame["request_id"].as_u64().unwrap(), true, "Controller settings applied.").unwrap();
        });
        request_controller_settings_within(&config, &account, controller_values(), Duration::from_secs(1), Duration::from_secs(1)).unwrap();
        consumer.join().unwrap();
        assert_eq!(fs::read(directory.join("native-social.json")).unwrap(), b"unrelated social request");
        assert_eq!(fs::read(directory.join("native-control.json")).unwrap(), b"unrelated POL control");
        assert!(!directory.join("controller-settings.json").exists());
        assert!(read_controller_result(&directory, worker, 0).unwrap().ok);
        assert!(!fs::read_dir(&directory).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with("controller-settings-result-")));
    }

    #[test]
    fn controller_settings_require_a_live_game_supported_worker_and_complete_values() {
        let fixture = Fixture::new();
        let (config, account, directory, worker) = controller_fixture(&fixture.0, Some(false));
        let request = |values| request_controller_settings_within(&config, &account, values, Duration::from_millis(20), Duration::from_millis(50));
        assert!(request(controller_values()).unwrap_err().contains("Start FFXI"));
        publish_friends(&directory, &crate::friends::Snapshot { game_running: None, ..Default::default() }).unwrap();
        assert!(request(controller_values()).is_err());
        publish_friends(&directory, &crate::friends::Snapshot { game_running: Some(true), ..Default::default() }).unwrap();
        for values in [BTreeMap::new(), [("padmode000".to_owned(), "1".to_owned())].into(),
            [("padmode000".to_owned(), "".to_owned()), ("padsin000".to_owned(), "0".to_owned())].into(),
            [("unrelated".to_owned(), "1".to_owned()), ("padsin000".to_owned(), "0".to_owned())].into()] {
            assert!(request(values).unwrap_err().contains("Invalid live controller settings"));
        }
        let stale = Identity { created: worker.created + 1, ..worker };
        save_json(&controller_result_path(&directory, 0), &ControllerResult { worker: stale, request_id: 0, ok: true, message: String::new() }).unwrap();
        assert!(request(controller_values()).unwrap_err().contains("launch it again"), "an old worker cannot announce capability");
        publish_controller_result(&directory, 0, false, "This host is unsupported.").unwrap();
        assert!(request(controller_values()).unwrap_err().contains("launch it again"));
        assert!(!directory.join("controller-settings.json").exists());
        assert!(publish_controller_result(&directory, 0, true, &"x".repeat(ONE_SHOT_MESSAGE_LIMIT + 1)).is_err());
        assert!(publish_controller_result(&directory, 0, true, "bad\nmessage").is_err());
    }

    #[test]
    fn controller_consumer_discards_expiry_and_requires_exact_process_creation_identity() {
        let fixture = Fixture::new();
        let (config, account, directory, worker) = controller_fixture(&fixture.0, Some(true));
        let path = directory.join("controller-settings.json");
        let stale = Identity { created: worker.created + 1, ..worker };
        let mut request = ControllerRequest { worker: stale, request_id: 42, settings: controller_values(), deadline_ms: request_clock_ms() + 1000 };
        save_json(&path, &request).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        assert!(path.exists(), "an old worker cannot delete another worker's command");
        request.worker = worker;
        request.deadline_ms = request_clock_ms().saturating_sub(1);
        save_json(&path, &request).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        assert!(!path.exists(), "an expired owned command is discarded");
        request.deadline_ms = request_clock_ms() + 1000;
        request.request_id = 0;
        save_json(&path, &request).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        request.request_id = 42;
        save_json(&path, &request).unwrap();
        let _publication = exclusive(&directory.join("controller-settings.lock")).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        drop(_publication);
        update_record(&record_path(&config, &account), worker, |record| record.state = Event::Finished(None)).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        assert!(publish_controller_result(&directory, 42, true, "Applied.").is_err());
        assert!(path.exists());
    }

    #[test]
    fn controller_timeout_removes_queued_commands_without_claiming_the_game_applied_them() {
        let fixture = Fixture::new();
        let (config, account, directory, _) = controller_fixture(&fixture.0, Some(true));
        publish_controller_result(&directory, 0, true, "Ready.").unwrap();
        let error = request_controller_settings_within(&config, &account, controller_values(), Duration::from_millis(20), Duration::from_millis(40)).unwrap_err();
        assert!(error.contains("did not confirm"));
        assert!(!directory.join("controller-settings.json").exists());
        assert!(take_controller_settings(&directory).is_none());
    }

    #[test]
    fn controller_wait_ignores_a_stale_success_and_returns_the_current_workers_refusal() {
        let fixture = Fixture::new();
        let (config, account, directory, worker) = controller_fixture(&fixture.0, Some(true));
        publish_controller_result(&directory, 0, true, "Ready.").unwrap();
        let reply_dir = directory.clone();
        let consumer = std::thread::spawn(move || {
            let command = reply_dir.join("controller-settings.json");
            until(|| command.exists());
            let frame = loop {
                if let Some(frame) = take_controller_settings(&reply_dir) { break frame; }
                std::thread::sleep(Duration::from_millis(5));
            };
            let frame: serde_json::Value = serde_json::from_slice(&frame).unwrap();
            let id = frame["request_id"].as_u64().unwrap();
            let stale = Identity { created: worker.created + 1, ..worker };
            save_json(&controller_result_path(&reply_dir, id), &ControllerResult { worker: stale, request_id: id, ok: true, message: "Applied.".into() }).unwrap();
            std::thread::sleep(Duration::from_millis(30));
            publish_controller_result(&reply_dir, id, false, "The game refused the controller reload.").unwrap();
        });
        let error = request_controller_settings_within(&config, &account, controller_values(), Duration::from_millis(50), Duration::from_secs(1)).unwrap_err();
        consumer.join().unwrap();
        assert_eq!(error, "The game refused the controller reload.");
    }

    #[test]
    fn controller_wait_rejects_replaced_sessions_and_cleanup_preserves_foreign_commands() {
        let fixture = Fixture::new();
        let (config, account, directory, worker) = controller_fixture(&fixture.0, Some(true));
        publish_controller_result(&directory, 0, true, "Ready.").unwrap();
        let replaced_dir = directory.clone();
        let replacement = std::thread::spawn(move || {
            let command = replaced_dir.join("controller-settings.json");
            until(|| command.exists());
            let _lock = controller_command_lock(&replaced_dir).unwrap();
            let pending: ControllerRequest = serde_json::from_slice(&read_bounded(&command, CONTROLLER_FRAME_LIMIT).unwrap()).unwrap();
            let new_worker = Identity { created: worker.created + 1, ..worker };
            save_json(&command, &ControllerRequest { worker: new_worker, ..pending }).unwrap();
            save_record(&replaced_dir.join("session.json"), &Record { worker: new_worker, state: Event::Progress("replacement".into()),
                social_only: false, authenticated: true, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        });
        assert!(request_controller_settings_within(&config, &account, controller_values(), Duration::from_millis(50), Duration::from_secs(1)).unwrap_err().contains("session ended"));
        replacement.join().unwrap();
        let preserved: ControllerRequest = serde_json::from_slice(&fs::read(directory.join("controller-settings.json")).unwrap()).unwrap();
        assert_ne!(preserved.worker, worker, "cleanup removes only its exact queued request");
        assert!(read_controller_result(&directory, preserved.worker, 0).is_none());
    }

    #[test]
    fn controller_settings_never_retarget_a_session_replaced_before_dispatch() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
        let captured = find(&config, &account).unwrap();
        fs::write(fixture.0.join("stop"), []).unwrap();
        until(|| !launch.alive.load(Ordering::Acquire) && identity(captured.worker.pid) != Some(captured.worker));
        let directory = config.account_directory(&account);
        let replacement = identity(std::process::id()).unwrap();
        save_record(&record_path(&config, &account), &Record { worker: replacement, state: Event::Progress("replacement game".into()),
            social_only: false, authenticated: true, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        publish_friends(&directory, &crate::friends::Snapshot { game_running: Some(true), ..Default::default() }).unwrap();
        publish_controller_result(&directory, 0, true, "Ready.").unwrap();
        assert_eq!(find(&config, &account).unwrap().worker, replacement, "a different live worker is discoverable");
        assert!(request_controller_settings_for_session(&captured, controller_values()).unwrap_err().contains("session ended"));
        assert!(!directory.join("controller-settings.json").exists(), "the captured request must never reach its replacement");
    }

    #[test]
    fn status_publication_supports_long_unicode_paths_and_replaces_existing_data() {
        use std::os::windows::ffi::OsStrExt;
        let fixture = Fixture::new();
        let mut directory = fixture.0.clone();
        for index in 0..7 {
            directory.push(format!("crystal-status-試験-{index}-{}", "segment".repeat(5)));
        }
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("friends.json");
        assert!(path.as_os_str().encode_wide().count() > 300);
        for revision in 0..3 {
            save_json(&path, &serde_json::json!({"revision": revision})).unwrap();
            let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved["revision"], revision);
        }
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }

    /// Review finding: a reopened window learns that the session's sign-in spent a one-time
    /// code, so it signs nothing back in after that game. A record from a worker before the
    /// field (1.0.8) does not say either way, rather than "no code".
    #[test]
    fn a_reconnected_session_remembers_a_spent_one_time_code() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let worker = identity(std::process::id()).unwrap();
        let path = record_path(&config, &account); fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = serde_json::json!({ "worker": worker, "state": Event::Progress("Fixture signing in".into()) });
        let record: Record = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(record.otp_used, None, "older records do not say");
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(find(&config, &account).unwrap().otp_used(), None);
        for used in [false, true] {
            save_record(&path, &Record { worker, state: Event::Progress("signed in".into()), social_only: true,
                authenticated: true, native_batch: false, otp_used: Some(used), launch_choice: None, launch_tool: None }).unwrap();
            assert_eq!(find(&config, &account).unwrap().otp_used(), Some(used));
            // What every worker writes now reads back the same.
            let text = fs::read_to_string(&path).unwrap();
            assert!(text.contains(&format!("\"otp_used\":{used}")), "{text}");
        }
    }

    #[test]
    fn session_reuse_requires_the_original_launch_method_and_an_owned_live_worker() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let worker = identity(std::process::id()).unwrap();
        let path = record_path(&config, &account); fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old: Record = serde_json::from_value(serde_json::json!({
            "worker": worker, "state": Event::Progress("Fixture signing in".into()),
        })).unwrap();
        assert!(old.launch_tool.is_none(), "older records remain readable");
        for stored in [None, Some(Tool::Windower), Some(Tool::Ashita), Some(Tool::Vanilla)] {
            let record = Record { worker, state: old.state.clone(), social_only: false,
                authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: stored };
            save_record(&path, &record).unwrap();
            let before = fs::read(&path).unwrap();
            let session = find(&config, &account).unwrap();
            for requested in [Tool::Windower, Tool::Ashita, Tool::Vanilla] {
                let compatible = stored.map_or(requested != Tool::Vanilla, |tool| tool == requested);
                assert_eq!(session.ensure_launch_tool(requested).is_ok(), compatible, "{stored:?} → {requested:?}");
            }
            assert_eq!(fs::read(&path).unwrap(), before, "checking must not sign out or change the session");
            update_record(&path, worker, |record| record.authenticated = true).unwrap();
            assert_eq!(read_record(&path).unwrap().launch_tool, stored, "milestones retain the launch method");
        }
        let session = find(&config, &account).unwrap();
        update_record(&path, worker, |record| record.worker.created += 1).unwrap();
        assert!(session.ensure_launch_tool(Tool::Vanilla).is_err(), "a replacement cannot inherit the old identity");
    }

    #[test]
    fn retained_sessions_require_a_fresh_signin_for_a_changed_profile_or_character_slot() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let path = record_path(&config, &account); fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut plan = park_plan();
        plan.settings.windower_profile = "Alpha".into();
        save_record(&path, &Record { worker: identity(std::process::id()).unwrap(),
            state: Event::Progress("Fixture retained owner".into()), social_only: true,
            authenticated: true, native_batch: true, otp_used: Some(false), launch_tool: Some(Tool::Windower),
            launch_choice: Some(LaunchChoice::from_plan(&plan)) }).unwrap();
        let session = find(&config, &account).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(session.ensure_launch_plan(&plan).is_ok());
        plan.settings.windower_profile = "Beta".into();
        assert!(session.ensure_launch_plan(&plan).is_err());
        plan.settings.windower_profile = "Alpha".into();
        plan.auto_login.enabled = true;
        plan.auto_login.character_slot = 2;
        assert!(session.ensure_launch_plan(&plan).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn switching_to_vanilla_stops_before_reconnect_version_checks_or_game_control() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let path = record_path(&config, &account); fs::create_dir_all(path.parent().unwrap()).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("Fixture Windower host".into()),
            social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: Some(Tool::Windower) }).unwrap();
        let original = fs::read(&path).unwrap();
        let plan = Plan { tool: Tool::Vanilla, settings: Default::default(),
            social_only: false, auto_away_minutes: 10, auto_login: crate::pol_less::AutoLogin::default(), keep_session: false, viewer_gaps: false, trigger: Default::default() };
        let error = launch_command(Command::new("must-not-spawn.exe"), &config, account.clone(), Zeroizing::default(),
            Some(plan), Instant::now(), |_| panic!("must stop before reconnect")).err().unwrap();
        assert!(error.contains("before switching launch methods"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!config.account_directory(&account).join("native-control.json").exists());
    }

    #[test]
    fn reconnect_rejects_a_replaced_or_ended_worker() {
        for ended in [false, true] {
            let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
            let path = record_path(&config, &account); fs::create_dir_all(path.parent().unwrap()).unwrap();
            let worker = identity(std::process::id()).unwrap();
            save_record(&path, &Record { worker, state: Event::Progress("still authenticating".into()),
                social_only: true, authenticated: false, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
            let session = find(&config, &account).unwrap();
            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let observer = std::thread::spawn(move || {
                let result = session.reconnect(|_| { let _ = progress_tx.send(()); });
                result_tx.send(result.map(|_| ())).unwrap();
            });
            progress_rx.recv_timeout(Duration::from_secs(5)).unwrap();

            assert!(matches!(result_rx.recv_timeout(Duration::from_millis(30)), Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
                "an unfinished record cannot complete reconnect");
            if ended {
                update_record(&path, worker, |record| record.state = Event::Finished(None)).unwrap();
            } else {
                save_record(&path, &Record { worker: Identity { created: worker.created + 1, ..worker },
                    state: Event::Started(Started { pid: worker.pid, detail: "replacement".into(), log: fixture.0.join("fixture.log"), timing: String::new() }),
                    social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();

            }
            let error = result_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
            assert!(error.contains(if ended { "ended" } else { "changed" }), "{error}");
            observer.join().unwrap();
        }
    }

    #[test]
    fn status_replacement_preserves_old_data_and_recovers_from_windows_reader_locks() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let fixture = Fixture::new();
        let path = fixture.0.join("friends.json");
        save_json(&path, &serde_json::json!({"ready": false})).unwrap();
        let held_reader = OpenOptions::new().read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(&path).unwrap();

        // Exercise a real Windows replacement failure instead of mocking a file error.
        let error = save_json(&path, &serde_json::json!({"ready": true})).unwrap_err();
        assert!(error.contains("friends.json") && error.contains("os error"), "{error}");
        let previous: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(previous["ready"], false);
        assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|e| e == "tmp")));

        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            drop(held_reader);
        });
        save_json(&path, &serde_json::json!({"ready": true})).unwrap();
        release.join().unwrap();
        let current: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(current["ready"], true);
    }

    #[test]
    fn concurrent_status_publications_use_independent_staging_files() {
        let fixture = Fixture::new();
        let path = fixture.0.join("friends.json");
        let start = std::sync::Barrier::new(6);
        std::thread::scope(|scope| {
            for writer in 0..6 {
                let (path, start) = (&path, &start);
                scope.spawn(move || {
                    start.wait();
                    for revision in 0..10 {
                        save_json(path, &serde_json::json!({"writer": writer, "revision": revision, "data": "x".repeat(4096)})).unwrap();
                    }
                });
            }
        });
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["revision"], 9);
        assert_eq!(saved["data"].as_str().unwrap().len(), 4096);
        assert!(!fs::read_dir(&fixture.0).unwrap().any(|entry| entry.unwrap().path().extension().is_some_and(|e| e == "tmp")));
    }

    #[test]
    fn typed_startup_state_defaults_safely_and_preserves_late_started_records() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let directory = config.account_directory(&account); let path = record_path(&config, &account);
        fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        let old: Record = serde_json::from_value(serde_json::json!({
            "worker": worker, "state": Event::Progress("Authenticated: a label is not evidence".into()),
        })).unwrap();
        assert!(!old.authenticated && !old.social_only && !old.native_batch);
        save_record(&path, &old).unwrap();
        let session = find(&config, &account).unwrap();
        assert!(!session.preparing_social() && !read_record(&path).unwrap().authenticated && !session.supports_batch_start());
        update_record(&path, worker, |record| record.social_only = true).unwrap();
        assert!(session.preparing_social());
        assert!(!read_record(&path).unwrap().authenticated);
        // Whichever writer arrives first, authentication only changes its milestone and
        // login completion only changes its event. Neither may replace the other's update.
        std::thread::scope(|scope| {
            scope.spawn(|| publish_authenticated(&directory).unwrap());
            scope.spawn(|| publish_native_batch(&directory, true).unwrap());
            scope.spawn(|| update_record(&path, worker, |record| record.state = Event::Started(Started {
                pid: worker.pid, detail: "Late startup still pending".into(), log: fixture.0.join("fixture.log"), timing: String::new(),
            })).unwrap());
        });
        let record = read_record(&path).unwrap();
        assert!(record.authenticated && record.social_only && record.native_batch && matches!(record.state, Event::Started(_)));
        assert!(session.preparing_social() && session.supports_batch_start());
        publish_native_batch(&directory, false).unwrap();
        assert!(!session.supports_batch_start(), "a loader without the capability keeps the old single-start path");
        let mut snapshot = crate::friends::Snapshot { game_running: Some(false), ..Default::default() };
        snapshot.social.ready = true;
        publish_friends(&directory, &snapshot).unwrap();
        assert!(!session.preparing_social());
        update_record(&path, worker, |record| { record.state = Event::Finished(None); record.authenticated = false; }).unwrap();
        publish_authenticated(&directory).unwrap();
        publish_native_batch(&directory, true).unwrap();
        assert!(!read_record(&path).unwrap().authenticated, "an ended session stays ended");
        assert!(!session.supports_batch_start());
        save_record(&path, &Record { worker: Identity { created: 0, ..worker }, state: Event::Progress("reused PID".into()), social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        assert!(publish_authenticated(&directory).is_err());
        assert!(publish_native_batch(&directory, true).is_err());
        assert!(!session.preparing_social() && !session.supports_batch_start());
        assert_ne!(read_record(&path).unwrap().worker, worker);
    }

    /// pol-spec 07c §5.3: a Play on a signed-in session waits (never refused) for its idle
    /// main menu, then W20, then StartGame; a request that starts during W20 delays the start
    /// until idle again rather than cancelling anything.
    #[test]
    fn signed_in_play_waits_for_the_idle_main_menu_then_checks_then_starts() {
        use std::sync::atomic::AtomicUsize;
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let path = record_path(&config, &account);
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("pre-sign-in".into()), social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut snapshot = crate::friends::Snapshot { game_running: Some(false), ..Default::default() };
        publish_friends(&directory, &snapshot).unwrap();
        let checks = Arc::new(AtomicUsize::new(0));
        let waiting = Arc::new(Mutex::new(Vec::new()));
        let play = {
            let (config, account, directory, checks, waiting) = (config.clone(), account.clone(), directory.clone(), checks.clone(), waiting.clone());
            std::thread::spawn(move || start_signed_in_within(&config, &account, Duration::from_secs(10),
                &mut |line| waiting.lock().unwrap().push(line.to_owned()), || {
                    checks.fetch_add(1, Ordering::SeqCst);
                    // A message submitted while W20 runs: the start must wait for it.
                    fs::write(directory.join("native-social.json"), b"queued fixture request").unwrap();
                    Ok("Fixture version matches.".into())
                }))
        };
        let quiet = |why: &str| {
            std::thread::sleep(Duration::from_millis(80));
            assert_eq!(checks.load(Ordering::SeqCst), 0, "{why}");
            assert!(!play.is_finished(), "{why}: queued, not refused");
        };
        quiet("idle before the main menu's social readiness");
        snapshot.social.ready = true; snapshot.social.busy = true;
        publish_friends(&directory, &snapshot).unwrap();
        quiet("a social request in flight");
        snapshot.social.busy = false;
        publish_friends(&directory, &snapshot).unwrap();
        until(|| checks.load(Ordering::SeqCst) == 1);
        std::thread::sleep(Duration::from_millis(80));
        assert!(!directory.join("native-control.json").exists(), "no start while the request is pending");
        assert!(!play.is_finished());
        fs::remove_file(directory.join("native-social.json")).unwrap();
        assert_eq!(play.join().unwrap().unwrap(), "Fixture version matches.");
        assert_eq!(take_native_control(&directory), Some((NativeControl::StartGame, true)));
        assert_eq!(checks.load(Ordering::SeqCst), 1, "one W20 per Play");
        assert_eq!(waiting.lock().unwrap().len(), 1, "one status line while queued");

        // Bounded: a session that never reaches its main menu is not started and runs no W20.
        snapshot.social.ready = false;
        publish_friends(&directory, &snapshot).unwrap();
        let error = start_signed_in_within(&config, &account, Duration::from_millis(100), &mut |_| {}, || panic!("no W20 before the main menu")).unwrap_err();
        assert!(error.contains("main menu"), "{error}");
        snapshot.game_running = Some(true);
        publish_friends(&directory, &snapshot).unwrap();
        let error = start_signed_in_within(&config, &account, Duration::from_secs(10), &mut |_| {}, || panic!("no W20 while FFXI runs")).unwrap_err();
        assert!(error.contains("Close FFXI"), "{error}");
        // A cache that can never show the main menu keeps the old immediate, specific refusal.
        for unusable in [b"not a friend cache".to_vec(), vec![b' '; 512 * 1024 + 1]] {
            fs::write(directory.join("friends.json"), unusable).unwrap();
            let asked = Instant::now();
            let error = start_signed_in_within(&config, &account, Duration::from_secs(10), &mut |_| {}, || panic!("no W20 without native controls")).unwrap_err();
            assert!(error.contains("does not support native PlayOnline controls"), "{error}");
            assert!(asked.elapsed() < Duration::from_secs(5));
        }
        update_record(&path, worker, |record| record.state = Event::Finished(None)).unwrap();
    }

    /// Maintenance's "Check Version": one W20 at the idle main menu and nothing after it, no
    /// StartGame and no play-pending note. Away from the main menu, no W20 at all.
    #[test]
    fn a_maintenance_version_check_never_starts_the_game() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let path = record_path(&config, &account);
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("pre-sign-in".into()), social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut snapshot = crate::friends::Snapshot { game_running: Some(false), ..Default::default() };
        snapshot.social.ready = true;
        publish_friends(&directory, &snapshot).unwrap();
        let mut checks = 0;
        let verdict = check_at_main_menu_within(&config, &account, Duration::from_secs(10), || true, || { checks += 1; "current" });
        assert_eq!(verdict, Ok("current"));
        assert_eq!(checks, 1, "one W20");
        assert_eq!(take_native_control(&directory), None, "no GameStart after the probe W20");
        assert!(!directory.join(PLAY_PENDING_NOTE).exists());
        // Review finding: the gate is asked again after the wait for the main menu; once
        // maintenance or `$patch_enable` 1 is over, no W20.
        let asked = std::cell::Cell::new(0);
        let error = check_at_main_menu_within(&config, &account, Duration::from_secs(10), || { asked.set(asked.get() + 1); false },
            || -> &str { panic!("no W20 once the check is no longer ready") }).unwrap_err();
        assert!(error.contains("ended") && asked.get() == 1, "{error}");
        snapshot.social.ready = false;
        publish_friends(&directory, &snapshot).unwrap();
        let error = check_at_main_menu_within(&config, &account, Duration::from_millis(100), || panic!("asked only at the main menu"),
            || -> &str { panic!("no W20 before the main menu") }).unwrap_err();
        assert!(error.contains("main menu"), "{error}");
        update_record(&path, worker, |record| record.state = Event::Finished(None)).unwrap();
    }

    #[test]
    fn startup_close_can_queue_sign_out_before_the_first_native_status_report() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let path = record_path(&config, &account);
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        let mut record = Record { worker, state: Event::Progress("signing in".into()), social_only: true,
            authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None };
        save_record(&path, &record).unwrap();
        let mut closing = IdleSessionClose::for_account(&config, &account, false).unwrap();
        assert!(!closing.finished());
        assert!(closing.requested, "sign-out must not wait for a friends or idle report");
        assert!(take_startup_sign_out(&directory));
        assert!(!take_startup_sign_out(&directory));
        record.social_only = false;
        save_record(&path, &record).unwrap();
        assert!(IdleSessionClose::for_account(&config, &account, false).is_none());
        assert!(request_native_control(&config, &account, NativeControl::SignOut).is_err());
        record.social_only = true;
        save_record(&path, &record).unwrap();
        publish_friends(&directory, &crate::friends::Snapshot { game_running: Some(true), ..Default::default() }).unwrap();
        assert!(IdleSessionClose::for_account(&config, &account, false).is_none());
        assert!(request_native_control(&config, &account, NativeControl::SignOut).is_err());
    }

    #[test]
    fn startup_close_consumes_only_its_workers_sign_out_and_preserves_queued_play() {
        let fixture = Fixture::new();
        let directory = &fixture.0;
        let worker = identity(std::process::id()).unwrap();
        let path = directory.join("native-control.json");
        let control = |action, owner| save_json(&path, &ControlRequest { worker: owner, action, groups: true }).unwrap();
        control(NativeControl::StartGame, worker);
        assert!(!take_startup_sign_out(directory));
        assert_eq!(take_native_control(directory), Some((NativeControl::StartGame, true)));
        control(NativeControl::Away, worker);
        assert!(!take_startup_sign_out(directory));
        assert_eq!(take_native_control(directory), Some((NativeControl::Away, true)));
        control(NativeControl::SignOut, worker);
        save_play_pending_for_test(directory);
        assert!(!take_startup_sign_out(directory));
        assert!(path.exists());
        assert!(take_play_pending(directory));
        assert!(take_startup_sign_out(directory));
        control(NativeControl::SignOut, Identity { created: 0, ..worker });
        assert!(!take_startup_sign_out(directory));
        assert!(!path.exists(), "discard a stale worker's command");
    }

    #[test]
    fn control_requests_require_social_idle_and_the_exact_worker_identity() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let path = record_path(&config, &account);
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("fixture".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut snapshot = crate::friends::Snapshot { game_running: Some(true), ..Default::default() };
        publish_friends(&directory, &snapshot).unwrap();
        assert!(request_native_control(&config, &account, NativeControl::SignOut).is_err());
        assert!(!directory.join("native-control.json").exists());
        snapshot.game_running = Some(false);
        snapshot.social.ready = true;
        publish_friends(&directory, &snapshot).unwrap();
        request_native_control(&config, &account, NativeControl::StartGame).unwrap();
        assert!(request_native_control(&config, &account, NativeControl::SignOut).is_err(), "a queued Play must not be replaced");
        assert!(request_social(&config, &account, crate::social::Request::Check {}).is_err(), "mail cannot race a queued control");
        let publication = exclusive(&directory.join("native-social.lock")).unwrap();
        assert_eq!(take_native_control(&directory), None, "the consumer uses the publisher's lock");
        drop(publication);
        assert_eq!(take_native_control(&directory), Some((NativeControl::StartGame, true)));
        assert_eq!(take_native_control(&directory), None);
        request_native_status(&config, &account, NativeControl::Away, false).unwrap();
        assert_eq!(take_native_control(&directory), Some((NativeControl::Away, false)));
        // Requests written before the groups choice existed apply to groups.
        fs::write(directory.join("native-control.json"), serde_json::json!({ "worker": worker, "action": "visible" }).to_string()).unwrap();
        assert_eq!(take_native_control(&directory), Some((NativeControl::Visible, true)));
        let stale = Identity { created: worker.created + 1, ..worker };
        save_json(&directory.join("native-control.json"), &ControlRequest { worker: stale, action: NativeControl::SignOut, groups: true }).unwrap();
        assert_eq!(take_native_control(&directory), None);
        save_json(&directory.join("friends.json"), &FriendCache { worker: stale, snapshot }).unwrap();
        assert!(request_native_control(&config, &account, NativeControl::StartGame).is_err());
    }

    #[test]
    fn an_unconfirmed_portrait_queues_nothing_even_for_an_idle_session() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("fixture".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut snapshot = crate::friends::Snapshot { game_running: Some(false), ..Default::default() };
        snapshot.social.ready = true;
        publish_friends(&directory, &snapshot).unwrap();
        assert!(!crate::wire::PORTRAIT_FIELD_CONFIRMED);
        assert_eq!(request_native_portrait(&config, &account, 12), Err(crate::portraits::unavailable().to_owned()));
        assert!(!directory.join("native-control.json").exists(), "nothing reaches the loader");
        // The request shape the loader receives once confirmed.
        save_json(&directory.join("native-control.json"), &ControlRequest { worker, action: NativeControl::Portrait(12), groups: true }).unwrap();
        assert_eq!(take_native_control(&directory), Some((NativeControl::Portrait(12), true)));
    }

    #[test]
    fn one_shot_commands_are_not_delivered_until_windows_allows_removal() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        let fixture = Fixture::new();
        let directory = &fixture.0;
        let worker = identity(std::process::id()).unwrap();
        let deny_removal = |path: &Path| OpenOptions::new().read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path).unwrap();

        let social = directory.join("native-social.json");
        save_json(&social, &SocialRequest { worker, id: "1".repeat(32), request: crate::social::Request::Send {
            alternate: false, slot: 0, uid: 1, subject: "fixture".into(), message: "Deliver once".into(),
        } }).unwrap();
        let observer = deny_removal(&social);
        assert!(take_social(directory).is_none(), "retaining the file must not send the message");
        assert!(social.exists());
        drop(observer);
        assert!(take_social(directory).is_some());
        assert!(take_social(directory).is_none(), "a consumed message is never replayed");

        let control = directory.join("native-control.json");
        save_json(&control, &ControlRequest { worker, action: NativeControl::StartGame, groups: false }).unwrap();
        let observer = deny_removal(&control);
        assert_eq!(take_native_control(directory), None);
        drop(observer);
        assert_eq!(take_native_control(directory), Some((NativeControl::StartGame, false)));
        assert_eq!(take_native_control(directory), None);

        let activity = directory.join("native-activity.json");
        save_json(&activity, &worker).unwrap();
        let observer = deny_removal(&activity);
        assert!(!take_native_activity(directory));
        drop(observer);
        assert!(take_native_activity(directory));
        assert!(!take_native_activity(directory));

        let batch = directory.join("native-batch.json");
        let (_gate, tokens) = start_gate::Coordinator::new(&crate::pol_less::pipe::random_hex().unwrap(), 1).unwrap();
        save_json(&batch, &BatchControl { worker, token: tokens[0].clone() }).unwrap();
        let observer = deny_removal(&batch);
        assert!(take_native_batch(directory).is_none());
        drop(observer);
        assert!(take_native_batch(directory).is_some());
        assert!(take_native_batch(directory).is_none());
    }

    #[test]
    fn activity_reaches_only_this_idle_worker() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(&directory).unwrap();
        assert!(note_native_activity(&config, &account).is_err(), "no session");
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("fixture".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut snapshot = crate::friends::Snapshot { game_running: Some(true), ..Default::default() };
        publish_friends(&directory, &snapshot).unwrap();
        note_native_activity(&config, &account).unwrap();
        assert!(!take_native_activity(&directory), "FFXI owns the session");
        snapshot.game_running = Some(false);
        publish_friends(&directory, &snapshot).unwrap();
        note_native_activity(&config, &account).unwrap();
        assert!(take_native_activity(&directory));
        assert!(!take_native_activity(&directory));
        let stale = Identity { created: worker.created + 1, ..worker };
        save_json(&directory.join("native-activity.json"), &stale).unwrap();
        assert!(!take_native_activity(&directory));
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::var_os("CRYSTAL_DATA_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join(format!("session-worker-{}", backend::unique_id().unwrap()));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::write(self.0.join("stop"), []);
        }
    }
    fn config(root: &Path) -> Config {
        Config {
            experiment: root.to_path_buf(),
            repository: root.to_path_buf(),
            game_install: None,
            dat_overlays: Vec::new(),
        }
    }
    fn account() -> Account {
        Account {
            label: "Fixture".into(),
            mode: backend::AccountMode::Private,
            server: "localhost".into(),
            pol_id: "fixture".into(),
            pol_password: "fixture-password-never-on-disk".into(),
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
    fn child_command(test: &str, root: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("CRYSTAL_SESSION_FIXTURE", root);
        command
    }
    fn until(mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !check() {
            assert!(Instant::now() < deadline, "fixture timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "executed only in a fixture subprocess"]
    fn worker_fixture() {
        let root = PathBuf::from(std::env::var_os("CRYSTAL_SESSION_FIXTURE").unwrap());
        let code = run_with(|config, account, mut otp, _via, _clicked, progress| {
            otp.zeroize();
            progress("Fixture authenticated");
            let alive = Arc::new(AtomicBool::new(true));
            let social = root.join("social-fixture").exists();
            let game_running = Arc::new(AtomicBool::new(!social));
            let pol_directory = config.account_directory(&account);
            if social {
                publish_friends(&pol_directory, &crate::friends::Snapshot { game_running: Some(false), ..Default::default() }).unwrap();
            }
            let outcome = Arc::new(Mutex::new(None));
            let (tracked, ended, directory) = (alive.clone(), outcome.clone(), root.clone());
            let game = game_running.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                let mut beat = 0;
                while !directory.join("stop").exists() && Instant::now() < deadline {
                    if social {
                        match take_native_control(&pol_directory).map(|(action, _)| action) {
                            Some(NativeControl::StartGame) => {
                                game.store(true, Ordering::Release);
                                publish_friends(&pol_directory, &crate::friends::Snapshot { game_running: Some(true), ..Default::default() }).unwrap();
                            }
                            Some(NativeControl::SignOut) => break,
                            Some(_) => {},
                            None => {},
                        }
                        if directory.join("game-exited").exists() {
                            fs::remove_file(directory.join("game-exited")).unwrap();
                            game.store(false, Ordering::Release);
                            publish_friends(&pol_directory, &crate::friends::Snapshot { game_running: Some(false), ..Default::default() }).unwrap();
                        }
                    }
                    beat += 1;
                    let _ = fs::write(directory.join("heartbeat"), beat.to_string());
                    std::thread::sleep(Duration::from_millis(50));
                }
                *ended.lock().unwrap() = Some(crate::launch_log::classify_loader_end(
                    &crate::launch_log::LoaderEnd::SignedOut,
                    Duration::from_secs(900),
                ));
                tracked.store(false, Ordering::Release);
            });
            while root.join("hold-start").exists()
                && !root.join("release-start").exists()
                && alive.load(Ordering::Acquire)
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(LaunchResult {
                pid: std::process::id(),
                detail: "Fixture started".into(),
                log: config.experiment.join("fixture.log"),
                timing: "Fixture timing".into(),
                alive,
                game_running,
                outcome,
            })
        });
        // Model the retained Windower observer: Finished is already published,
        // but its launcher still owns a final settings.xml save.
        if root.join("hold-final-profile-save").exists() {
            fs::write(root.join("final-profile-save-pending"), []).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !root.join("finish-profile-save").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            fs::write(root.join("final-profile-save-complete"), []).unwrap();
        }
        std::process::exit(code);
    }

    // ---- the park worker (pol-spec 07d §1.4) ----

    use crate::pol_less::park::{Note, ParkedInfo, Worker};

    /// A parked host stand-in: records kills and the Play, and ends its session when told.
    struct FakeHost { root: PathBuf, dies: bool }
    impl ParkHost for FakeHost {
        fn info(&self) -> ParkedInfo { ParkedInfo { loader_pid: 4242, tool_pid: Some(4343), polcore_ms: Some(400), hello_ms: 1900 } }
        fn watch(&mut self, wait: Duration) -> Option<String> {
            if self.dies { return Some("The parked game host ended.".into()); }
            std::thread::sleep(wait.min(Duration::from_millis(20)));
            None
        }
        fn kill(self) { fs::write(self.root.join("killed"), []).unwrap(); }
        fn play(self, config: &Config, plan: &Plan, _account: Account, otp: Zeroizing<String>, _clicked: Instant,
            progress: &mut dyn FnMut(&str), _cold: Box<dyn FnOnce(Account, Zeroizing<String>, &mut dyn FnMut(&str)) -> Result<LaunchResult, String> + '_>)
            -> Result<LaunchResult, String> {
            fs::write(self.root.join("credentials"), format!("{} {}", plan.trigger.name(), otp.as_str())).unwrap();
            progress("Fixture parked sign-in");
            let alive = Arc::new(AtomicBool::new(true));
            let (tracked, root) = (alive.clone(), self.root.clone());
            std::thread::spawn(move || {
                while !root.join("stop").exists() { std::thread::sleep(Duration::from_millis(20)); }
                tracked.store(false, Ordering::Release);
            });
            Ok(LaunchResult { pid: std::process::id(), detail: "Fixture parked start".into(), log: config.experiment.join("fixture.log"),
                timing: String::new(), alive, game_running: Arc::new(AtomicBool::new(false)), outcome: Arc::new(Mutex::new(None)) })
        }
    }

    /// A parked host lost after frame 2, before the credentials: its Play goes cold.
    struct LostHost;
    impl ParkHost for LostHost {
        fn info(&self) -> ParkedInfo { ParkedInfo { loader_pid: 4242, tool_pid: None, polcore_ms: None, hello_ms: 0 } }
        fn watch(&mut self, wait: Duration) -> Option<String> { std::thread::sleep(wait.min(Duration::from_millis(20))); None }
        fn kill(self) {}
        fn play(self, _: &Config, _: &Plan, account: Account, otp: Zeroizing<String>, _: Instant,
            progress: &mut dyn FnMut(&str), cold: Box<dyn FnOnce(Account, Zeroizing<String>, &mut dyn FnMut(&str)) -> Result<LaunchResult, String> + '_>)
            -> Result<LaunchResult, String> {
            cold(account, otp, progress)
        }
    }

    /// Finding: a parked Play that fell back cold and failed is one launch-history line, not
    /// two (the cold launch records its own, as `backend::login_with_progress` does).
    #[test]
    fn a_failed_cold_fallback_is_recorded_once() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let mut bytes = frame(&ParkRequest { config: config.clone(), account: account(), plan: park_plan() });
        bytes.extend(frame(&play_request(&config, account(), crate::launch_timing::LaunchTrigger::ParkedPlay)));
        let history = config.history_path();
        let code = run_park_with(Feed { bytes, at: 0, hold: None }, |_, _, _, _| Ok(LostHost), |_| {}, |config, account, _, _, _, _| {
            crate::launch_log::append_history(&config.history_path(), &account.label, "fixture cold refused", None);
            Err("fixture cold refused".into())
        }, closed_flag());
        assert_eq!(code, 1);
        let text = fs::read_to_string(history).unwrap();
        assert_eq!(text.matches("fixture cold refused").count(), 1, "{text}");
    }

    fn frame(value: &impl Serialize) -> Vec<u8> {
        let bytes = serde_json::to_vec(value).unwrap();
        let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
        framed.extend(bytes);
        framed
    }

    /// Stdin that holds after its bytes until `release` is dropped (the window keeps it open).
    struct Feed { bytes: Vec<u8>, at: usize, hold: Option<std::sync::mpsc::Receiver<()>> }
    impl Read for Feed {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.at == self.bytes.len() {
                if let Some(hold) = self.hold.take() { let _ = hold.recv(); }
                return Ok(0);
            }
            let count = buffer.len().min(self.bytes.len() - self.at);
            buffer[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
            self.at += count;
            Ok(count)
        }
    }

    fn park_plan() -> Plan {
        Plan { tool: Tool::Windower, settings: Default::default(), social_only: false, auto_away_minutes: 10,
            auto_login: crate::pol_less::AutoLogin::default(), keep_session: false, viewer_gaps: false, trigger: Default::default() }
    }

    fn play_request(config: &Config, account: Account, trigger: crate::launch_timing::LaunchTrigger) -> Request {
        Request { config: config.clone(), account, otp: "123456".into(), via: Some(Plan { trigger, ..park_plan() }), elapsed_ms: 0, clicked_clock: None, hairpin: true }
    }

    /// A request from a window that predates the hairpin choice keeps xiloader's --hairpin on.
    #[test]
    fn a_request_without_a_hairpin_choice_keeps_hairpin_on() {
        let fixture = Fixture::new();
        let mut request = play_request(&config(&fixture.0), account(), crate::launch_timing::LaunchTrigger::Play);
        request.hairpin = false;
        let mut value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["hairpin"], serde_json::Value::Bool(false));
        assert!(!serde_json::from_value::<Request>(value.clone()).unwrap().hairpin);
        value.as_object_mut().unwrap().remove("hairpin");
        assert!(serde_json::from_value::<Request>(value).unwrap().hairpin);
    }

    /// 07d §1.4: frame 1 never carries a password, OTP or authenticator key.
    #[test]
    fn the_park_frame_carries_no_secret() {
        let fixture = Fixture::new();
        let mut plan = park_plan();
        plan.trigger = crate::launch_timing::LaunchTrigger::ParkedPlay;
        let mut account = account();
        account.totp_key = "JBSWY3DPEHPK3PXP".into();
        let bytes = park_frame(&config(&fixture.0), &account, &plan).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("fixture-password-never-on-disk") && !text.contains("\"otp\"") && !text.contains("JBSWY3DPEHPK3PXP"), "{text}");
        let request: ParkRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!((request.account.pol_id.as_str(), request.plan.trigger), ("fixture", crate::launch_timing::LaunchTrigger::Play));
    }

    /// 07d §1.4: `Parked` after the park; EOF before frame 2 kills the host and nothing else
    /// happens; a host that ends while parked is `ParkFailed`; a failed park says why.
    #[test]
    fn a_park_worker_reports_parked_then_closes_on_eof_or_host_loss() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let first = frame(&ParkRequest { config: config.clone(), account: account(), plan: park_plan() });
        let mut events = Vec::new();
        let code = run_park_with(Feed { bytes: first.clone(), at: 0, hold: None }, |_, _, _, _| Ok(FakeHost { root: fixture.0.clone(), dies: false }),
            |event| events.push(event.clone()), |_, _, _, _, _, _| panic!("no Play"), closed_flag());
        assert_eq!(code, 0);
        assert!(matches!(events.as_slice(), [Event::Parked { loader_pid: 4242, tool_pid: Some(4343), polcore_ms: Some(400), hello_ms: 1900 }]), "{events:?}");
        assert!(fixture.0.join("killed").exists() && !record_path(&config, &account()).exists(), "no session record before Play");
        fs::remove_file(fixture.0.join("killed")).unwrap();
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let mut events = Vec::new();
        let code = run_park_with(Feed { bytes: first.clone(), at: 0, hold: Some(hold) }, |_, _, _, _| Ok(FakeHost { root: fixture.0.clone(), dies: true }),
            |event| events.push(event.clone()), |_, _, _, _, _, _| panic!("no Play"), closed_flag());
        drop(release);
        assert_eq!(code, 1);
        assert!(matches!(events.as_slice(), [Event::Parked { .. }, Event::ParkFailed(why)] if why.contains("ended")), "{events:?}");
        assert!(fixture.0.join("killed").exists());
        let mut events = Vec::new();
        assert_eq!(run_park_with(Feed { bytes: first.clone(), at: 0, hold: None }, |_, _, _, _| Err::<FakeHost, _>(crate::pol_less::park::UNSUPPORTED.into()),
            |event| events.push(event.clone()), |_, _, _, _, _, _| panic!("no Play"), closed_flag()), 1);
        assert!(matches!(events.as_slice(), [Event::ParkFailed(why)] if why == crate::pol_less::park::UNSUPPORTED), "{events:?}");
        // Finding: stdin ending while the park is still starting reaches the park itself, so it
        // can stop Windower before its loader (no orphaned Windower.exe rewriting settings.xml).
        let mut events = Vec::new();
        let saw = std::cell::Cell::new(false);
        assert_eq!(run_park_with(Feed { bytes: first, at: 0, hold: None }, |_, _, _, closed| {
            let until = Instant::now() + Duration::from_secs(5);
            while !closed.load(Ordering::Acquire) && Instant::now() < until { std::thread::sleep(Duration::from_millis(5)); }
            saw.set(closed.load(Ordering::Acquire));
            Err::<FakeHost, _>(crate::pol_less::park::ABORTED.into())
        }, |event| events.push(event.clone()), |_, _, _, _, _, _| panic!("no Play"), closed_flag()), 1);
        assert!(saw.get(), "the park saw the window close it");
    }

    /// 07d §1.4 frame 2: the parked host signs in as a normal worker (session record, Started,
    /// Finished); a Play for another account goes cold and the host is killed.
    #[test]
    fn frame_two_signs_in_through_the_parked_host_or_goes_cold_for_another_key() {
        use crate::launch_timing::LaunchTrigger;
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        fs::write(fixture.0.join("stop"), []).unwrap();
        let mut bytes = frame(&ParkRequest { config: config.clone(), account: account(), plan: park_plan() });
        bytes.extend(frame(&play_request(&config, account(), LaunchTrigger::ParkedPlay)));
        let mut events = Vec::new();
        let code = run_park_with(Feed { bytes, at: 0, hold: None }, |_, _, _, _| Ok(FakeHost { root: fixture.0.clone(), dies: false }),
            |event| events.push(event.clone()), |_, _, _, _, _, _| panic!("the parked host was alive"), closed_flag());
        assert_eq!(code, 0);
        assert_eq!(fs::read_to_string(fixture.0.join("credentials")).unwrap(), "parked Play 123456");
        assert!(matches!(events.as_slice(), [Event::Parked { .. }, Event::Progress(line), Event::Started(_), Event::Finished(None)] if line == "Fixture parked sign-in"), "{events:?}");
        assert!(matches!(read_record(&record_path(&config, &account())).unwrap().state, Event::Finished(None)));
        assert!(!fixture.0.join("killed").exists());
        // Another account: never the parked host.
        let mut other = account();
        other.pol_id = "other".into();
        let mut bytes = frame(&ParkRequest { config: config.clone(), account: account(), plan: park_plan() });
        bytes.extend(frame(&play_request(&config, other, LaunchTrigger::ParkedPlay)));
        let cold = std::cell::Cell::new(false);
        let code = run_park_with(Feed { bytes, at: 0, hold: None }, |_, _, _, _| Ok(FakeHost { root: fixture.0.clone(), dies: false }),
            |_| {}, |_, account, otp, _, _, _| { cold.set(account.pol_id == "other" && otp == "123456"); Err("fixture cold".into()) }, closed_flag());
        assert_eq!(code, 1);
        assert!(cold.get() && fixture.0.join("killed").exists());
    }

    #[test]
    #[ignore = "executed only in a fixture subprocess"]
    fn park_worker_fixture() {
        let root = PathBuf::from(std::env::var_os("CRYSTAL_SESSION_FIXTURE").unwrap());
        let code = run_park_with(io::stdin(), |_, _, _, closed| {
            // `hold-park`: a park still starting (Windower's turn, hello, polcore) until
            // `release-park`, or until the window closes it.
            if root.join("hold-park").exists() {
                fs::write(root.join("parking"), []).unwrap();
                while !root.join("release-park").exists() {
                    if closed.load(Ordering::Acquire) {
                        fs::write(root.join("aborted"), []).unwrap();
                        return Err(crate::pol_less::park::ABORTED.into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Ok(FakeHost { root: root.clone(), dies: false })
        }, emit, |_, _, _, _, _, _| Err("fixture: no cold launch".into()), &PARK_CLOSED);
        std::process::exit(code);
    }

    /// A fresh `closed` flag for one in-process `run_park_with`.
    fn closed_flag() -> &'static AtomicBool { Box::leak(Box::new(AtomicBool::new(false))) }

    /// 07d §1.5 adoption, across processes: the window's handle sees `Parked`, writes frame 2
    /// and tracks the session; closing a worker before Play kills its host; a worker that is
    /// gone hands the OTP back for a cold Play.
    #[test]
    fn the_window_adopts_closes_or_falls_back_from_a_park_worker() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let parked = |worker: &mut ParkWorker| until(|| matches!(worker.poll(), Some(Note::Parked(ParkedInfo { loader_pid: 4242, .. }))));
        let mut worker = spawn_park_command(child_command("session_worker::tests::park_worker_fixture", &fixture.0), &config, &account(), &park_plan()).unwrap();
        parked(&mut worker);
        let mut via = park_plan();
        via.trigger = crate::launch_timing::LaunchTrigger::ParkedPlay;
        let launch = worker.play(&config, account(), "123456".into(), via, Instant::now(), &Mutex::new(None), |_| {}).unwrap().unwrap();
        assert!(launch.alive.load(Ordering::Acquire));
        assert_eq!(fs::read_to_string(fixture.0.join("credentials")).unwrap(), "parked Play 123456");
        fs::write(fixture.0.join("stop"), []).unwrap();
        until(|| !launch.alive.load(Ordering::Acquire));
        assert!(!fixture.0.join("killed").exists(), "a host that got credentials is never killed");
        fs::remove_file(fixture.0.join("stop")).unwrap();
        // Closed before Play: the worker kills its host and exits.
        let mut worker = spawn_park_command(child_command("session_worker::tests::park_worker_fixture", &fixture.0), &config, &account(), &park_plan()).unwrap();
        parked(&mut worker);
        crate::pol_less::park::close_workers(vec![worker]);
        assert!(fixture.0.join("killed").exists());
        // Gone: the OTP comes back, nothing was delivered.
        let mut worker = spawn_park_command(child_command("session_worker::tests::park_worker_fixture", &fixture.0), &config, &account(), &park_plan()).unwrap();
        worker.terminate();
        assert_eq!(worker.play(&config, account(), "654321".into(), park_plan(), Instant::now(), &Mutex::new(None), |_| panic!("no progress")).err().as_deref(), Some("654321"));
    }

    /// Finding: a Play that adopts a still-parking worker must not take its stagger slot (or
    /// write frame 2) until that host has parked: the slot is where the credentials leave.
    /// And closing a still-parking worker makes it abort its park itself, Windower first.
    #[test]
    fn a_still_parking_worker_takes_its_stagger_slot_once_parked_and_aborts_when_closed() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        fs::write(fixture.0.join("hold-park"), []).unwrap();
        let worker = spawn_park_command(child_command("session_worker::tests::park_worker_fixture", &fixture.0), &config, &account(), &park_plan()).unwrap();
        let deliveries = Arc::new(Mutex::new(None));
        let mut via = park_plan();
        via.trigger = crate::launch_timing::LaunchTrigger::ParkedPlay;
        let (play_config, slots) = (config.clone(), deliveries.clone());
        let play = std::thread::spawn(move || worker.play(&play_config, account(), "123456".into(), via, Instant::now(), &slots, |_| {})
            .map(|launch| launch.map(|launch| launch.alive)));
        until(|| fixture.0.join("parking").exists());
        std::thread::sleep(Duration::from_millis(300));
        assert!(deliveries.lock().unwrap().is_none(), "no slot while the host is still parking");
        assert!(!fixture.0.join("credentials").exists());
        let released = Instant::now();
        fs::write(fixture.0.join("release-park"), []).unwrap();
        let alive = play.join().unwrap().unwrap().unwrap();
        let slot = deliveries.lock().unwrap().expect("reserved");
        assert!(slot >= released, "the slot is taken after the park, not at the click");
        assert_eq!(fs::read_to_string(fixture.0.join("credentials")).unwrap(), "parked Play 123456");
        fs::write(fixture.0.join("stop"), []).unwrap();
        until(|| !alive.load(Ordering::Acquire));
        fs::remove_file(fixture.0.join("stop")).unwrap();
        for file in ["release-park", "parking", "credentials"] { let _ = fs::remove_file(fixture.0.join(file)); }
        // Closed while still parking: no terminate; the worker aborts its own park and exits.
        let worker = spawn_park_command(child_command("session_worker::tests::park_worker_fixture", &fixture.0), &config, &account(), &park_plan()).unwrap();
        until(|| fixture.0.join("parking").exists());
        assert!(!worker.parked());
        crate::pol_less::park::close_workers(vec![worker]);
        until(|| fixture.0.join("aborted").exists());
        assert!(!fixture.0.join("killed").exists(), "the park never finished");
    }

    #[test]
    fn background_social_session_survives_ui_reconnect_and_game_exit() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0), &config, account.clone(), Zeroizing::new("123456".into()), None, Instant::now(), |_| {}).unwrap();
        assert!(!launch.game_running.load(Ordering::Acquire));
        assert!(launch.alive.load(Ordering::Acquire));
        let session = find(&config, &account).unwrap();
        let reconnected = session.reconnect(|_| {}).unwrap();
        assert_eq!(reconnected.pid, launch.pid);
        assert!(!reconnected.game_running.load(Ordering::Acquire));
        request_native_control(&config, &account, NativeControl::StartGame).unwrap();
        until(|| launch.game_running.load(Ordering::Acquire) && reconnected.game_running.load(Ordering::Acquire));
        assert!(request_native_control(&config, &account, NativeControl::SignOut).is_err());
        fs::write(fixture.0.join("game-exited"), []).unwrap();
        until(|| !launch.game_running.load(Ordering::Acquire) && !reconnected.game_running.load(Ordering::Acquire));
        assert!(launch.alive.load(Ordering::Acquire));
        request_native_control(&config, &account, NativeControl::SignOut).unwrap();
        until(|| !launch.alive.load(Ordering::Acquire));
        assert!(find(&config, &account).is_none());
    }

    #[test]
    fn profile_editor_retires_idle_windower_workers_but_preserves_games_and_other_tools() {
        let idle_one = Fixture::new();
        let idle_two = Fixture::new();
        let game_fixture = Fixture::new();
        let ashita_fixture = Fixture::new();
        let account = account();
        let launch = |fixture: &Fixture, tool| {
            fs::write(fixture.0.join("social-fixture"), []).unwrap();
            let config = config(&fixture.0);
            let plan = Plan { tool, settings: Default::default(), social_only: true,
                auto_away_minutes: 0, auto_login: Default::default(),
                keep_session: false, viewer_gaps: false, trigger: Default::default() };
            let started = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
                &config, account.clone(), Zeroizing::default(), Some(plan), Instant::now(), |_| {}).unwrap();
            (config, started)
        };
        let (one_config, one) = launch(&idle_one, Tool::Windower);
        let (two_config, two) = launch(&idle_two, Tool::Windower);
        let (game_config, game) = launch(&game_fixture, Tool::Windower);
        let (ashita_config, ashita) = launch(&ashita_fixture, Tool::Ashita);
        request_native_control(&game_config, &account, NativeControl::StartGame).unwrap();
        until(|| game.game_running.load(Ordering::Acquire));
        let sessions = [&one_config, &two_config, &game_config, &ashita_config].iter()
            .map(|config| find(config, &account).unwrap()).collect();
        let mut progress = Vec::new();
        sign_out_for_profile_editor(sessions, |message| progress.push(message.to_owned())).unwrap();
        assert_eq!(progress.len(), 2);
        until(|| !one.alive.load(Ordering::Acquire) && !two.alive.load(Ordering::Acquire));
        assert!(!one.alive.load(Ordering::Acquire) && !two.alive.load(Ordering::Acquire));
        assert!(game.alive.load(Ordering::Acquire) && game.game_running.load(Ordering::Acquire));
        assert!(ashita.alive.load(Ordering::Acquire));
        assert!(!game_config.account_directory(&account).join("native-control.json").exists());
        assert!(!ashita_config.account_directory(&account).join("native-control.json").exists());
        for fixture in [&game_fixture, &ashita_fixture] { fs::write(fixture.0.join("stop"), []).unwrap(); }
        until(|| !game.alive.load(Ordering::Acquire) && !ashita.alive.load(Ordering::Acquire));
    }

    #[test]
    fn launcher_close_waits_for_an_unreadable_idle_snapshot_to_recover() {
        let fixture = Fixture::new();
        let (config, account, directory, _) = controller_fixture(&fixture.0, Some(false));
        let mut pending = IdleSessionClose::for_account(&config, &account, false).unwrap();
        fs::remove_file(directory.join("friends.json")).unwrap();
        assert!(!pending.finished(), "a missing cache is not evidence that its owner closed or started a game");
        assert!(!pending.requested && !directory.join("native-control.json").exists());
        publish_friends(&directory, &crate::friends::Snapshot { game_running: Some(false), ..Default::default() }).unwrap();
        assert!(!pending.finished() && pending.requested);
        let command: ControlRequest = serde_json::from_slice(&fs::read(directory.join("native-control.json")).unwrap()).unwrap();
        assert_eq!(command.action, NativeControl::SignOut);
    }

    #[test]
    fn launcher_close_signs_out_idle_sessions_and_keeps_the_other_game_running() {
        let fixtures = [Fixture::new(), Fixture::new(), Fixture::new()];
        let account = account();
        let mut launches = Vec::new();
        let mut sessions = Vec::new();
        let mut workers = Vec::new();
        for (index, fixture) in fixtures.iter().enumerate() {
            fs::write(fixture.0.join("social-fixture"), []).unwrap();
            let config = config(&fixture.0);
            let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
                &config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
            if index == 2 {
                request_native_control(&config, &account, NativeControl::StartGame).unwrap();
                until(|| launch.game_running.load(Ordering::Acquire));
            }
            workers.push(find(&config, &account).unwrap().worker);
            if let Some(session) = IdleSessionClose::for_account(&config, &account, false) { sessions.push(session); }
            launches.push(launch);
        }
        assert_eq!(sessions.len(), 2, "a running game is excluded from shutdown");
        close_idle_sessions(sessions).unwrap();
        for index in 0..2 {
            until(|| !launches[index].alive.load(Ordering::Acquire));
            assert!(!launches[index].alive.load(Ordering::Acquire));
            assert_ne!(identity(workers[index].pid), Some(workers[index]), "wait for the actual worker exit");
        }
        assert!(launches[2].alive.load(Ordering::Acquire) && launches[2].game_running.load(Ordering::Acquire));
        assert!(!config(&fixtures[2].0).account_directory(&account).join("native-control.json").exists());
    }

    #[test]
    fn account_signout_allows_a_fresh_worker_with_another_launch_method() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        let plan = |tool| Plan { tool, settings: Default::default(), social_only: true,
            auto_away_minutes: 0, auto_login: Default::default(),
            keep_session: false, viewer_gaps: false, trigger: Default::default() };
        let first = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), Some(plan(Tool::Vanilla)), Instant::now(), |_| {}).unwrap();
        let session = find(&config, &account).unwrap();
        assert!(session.ensure_launch_tool(Tool::Windower).is_err());
        session.sign_out().unwrap();
        until(|| !first.alive.load(Ordering::Acquire));
        assert!(!first.alive.load(Ordering::Acquire) && find(&config, &account).is_none());
        let second = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), Some(plan(Tool::Windower)), Instant::now(), |_| {}).unwrap();
        assert_ne!(second.pid, first.pid);
        assert!(find(&config, &account).unwrap().ensure_launch_tool(Tool::Windower).is_ok());
    }

    #[test]
    fn launcher_close_catches_a_startup_worker_that_has_not_published_yet() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        assert!(IdleSessionClose::for_account(&config, &account, false).is_none());
        let pending = IdleSessionClose::for_account(&config, &account, true).unwrap();
        assert!(pending.worker.is_none());
        let waiter = std::thread::spawn(move || close_idle_sessions(vec![pending]));
        let plan = Plan { tool: Tool::Vanilla, settings: Default::default(), social_only: true,
            auto_away_minutes: 0, auto_login: Default::default(),
            keep_session: false, viewer_gaps: false, trigger: Default::default() };
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account, Zeroizing::default(), Some(plan), Instant::now(), |_| {}).unwrap();
        waiter.join().unwrap().unwrap();
        until(|| !launch.alive.load(Ordering::Acquire));
    }

    #[test]
    fn launcher_close_waits_for_windower_to_finish_saving_and_does_not_overwrite_play() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        fs::write(fixture.0.join("hold-final-profile-save"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        let _launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
        let session = find(&config, &account).unwrap();
        let worker = session.worker;
        // A Play queued by another window has priority over closing an idle owner.
        let command_lock = exclusive(&config.account_directory(&account).join("native-social.lock")).unwrap();
        save_json(&config.account_directory(&account).join("native-control.json"),
            &ControlRequest { worker, action: NativeControl::StartGame, groups: true }).unwrap();
        let mut pending = IdleSessionClose::known(session.clone());
        assert!(pending.finished());
        drop(command_lock);
        until(|| session.game_running() == Some(true));
        assert!(session.clone().sign_out().is_err(), "manual sign-out also preserves the game");
        fs::write(fixture.0.join("game-exited"), []).unwrap();
        until(|| session.game_running() == Some(false));
        let (sent, received) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || { sent.send(session.sign_out()).unwrap(); });
        until(|| fixture.0.join("final-profile-save-pending").exists());
        assert_eq!(identity(worker.pid), Some(worker));
        let mut closing_again = IdleSessionClose::for_account(&config, &account, false).unwrap();
        assert!(closing_again.requested && !closing_again.finished(), "closing during a sign-out waits for the same worker");
        assert!(matches!(received.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        fs::write(fixture.0.join("finish-profile-save"), []).unwrap();
        received.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        waiter.join().unwrap();
        assert!(fixture.0.join("final-profile-save-complete").exists());
        assert_ne!(identity(worker.pid), Some(worker));
    }

    #[test]
    fn profile_editor_waits_for_the_final_save_after_a_finished_record() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        fs::write(fixture.0.join("hold-final-profile-save"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        let plan = Plan { tool: Tool::Windower, settings: Default::default(), social_only: true,
            auto_away_minutes: 0, auto_login: Default::default(),
            keep_session: false, viewer_gaps: false, trigger: Default::default() };
        let _launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), Some(plan), Instant::now(), |_| {}).unwrap();
        let session = find(&config, &account).unwrap();
        let worker = session.worker;
        let (send, received) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = sign_out_for_profile_editor(vec![session], |_| {});
            send.send(result).unwrap();
        });
        until(|| fixture.0.join("final-profile-save-pending").exists());
        assert!(find(&config, &account).is_none(), "Finished is already published");
        assert!(matches!(received.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        assert_eq!(identity(worker.pid), Some(worker));
        fs::write(fixture.0.join("finish-profile-save"), []).unwrap();
        received.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        waiter.join().unwrap();
        assert!(fixture.0.join("final-profile-save-complete").exists());
        assert_ne!(identity(worker.pid), Some(worker));
    }

    #[tokio::test]
    async fn main_update_prepares_an_automatic_social_session_and_waits_for_native_signout() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
        let mut app = crate::tests::app();
        app.preview = false;
        app.config = config.clone();
        app.settings.install_selected = true;
        app.settings.account_no_otp.insert(backend::account_key(&account));
        app.patch.as_mut().unwrap().state = crate::patch::PatchState::Required;
        app.accounts.truncate(1);
        let row = &mut app.accounts[0];
        row.key = backend::account_key(&account);
        row.account = account.clone();
        row.pid = Some(launch.pid);
        row.alive = Some(launch.alive.clone());
        row.game_running = Some(launch.game_running.clone());
        assert!(app.has_sessions());
        assert!(!app.sessions_block_update());

        // A Play refused because PlayOnline is out of date asks once before its update signs
        // this idle automatic sign-in out; the answer starts PlayOnline's update. Dropping its
        // task avoids any real updater network or disk work; exercise its native preparation
        // below with the same op.
        // (Another account's Play: this one is signed in.)
        app.accounts.push(crate::tests::app().accounts.remove(1));
        let refused = app.accounts[1].key.clone();
        app.note_refused(&refused, Instant::now(), None);
        app.tend_viewer_update();
        assert_eq!(app.play_updates.viewer_confirm, Some((refused, 1)));
        assert!(app.maintenance.is_none(), "nothing signs out before the answer");
        assert_eq!(app.update(crate::Message::ConfirmViewerUpdate).units(), 1);
        assert!(app.maintenance == Some(crate::MaintenanceKind::Update));
        assert!(app.play_updates.viewer_confirm.is_none());
        assert!(app.launch_blocked());
        let operation = app.active_operation.as_ref().unwrap();
        assert_eq!(operation.snapshot().stage, crate::operation::Stage::SignOut);
        assert!(launch.alive.load(Ordering::Acquire));
        sign_out_for_update(app.update_sessions().unwrap(), operation).await.unwrap();
        until(|| !launch.alive.load(Ordering::Acquire));
        assert!(!launch.alive.load(Ordering::Acquire));
        assert!(find(&config, &account).is_none());
        assert_eq!(operation.snapshot().done, 1);
    }

    #[tokio::test]
    async fn update_signout_preserves_running_games_and_cancelled_idle_sessions() {
        let idle_fixture = Fixture::new();
        let game_fixture = Fixture::new();
        for fixture in [&idle_fixture, &game_fixture] {
            fs::write(fixture.0.join("social-fixture"), []).unwrap();
        }
        let idle_config = config(&idle_fixture.0);
        let game_config = config(&game_fixture.0);
        let account = account();
        let idle = launch_command(child_command("session_worker::tests::worker_fixture", &idle_fixture.0),
            &idle_config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
        let game = launch_command(child_command("session_worker::tests::worker_fixture", &game_fixture.0),
            &game_config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
        request_native_control(&game_config, &account, NativeControl::StartGame).unwrap();
        until(|| game.game_running.load(Ordering::Acquire));

        let operation = crate::operation::Operation::default();
        assert!(sign_out_for_update(vec![find(&idle_config, &account).unwrap(),
            find(&game_config, &account).unwrap()], &operation).await.is_err());
        assert!(idle.alive.load(Ordering::Acquire) && game.alive.load(Ordering::Acquire));
        assert!(!idle_config.account_directory(&account).join("native-control.json").exists());

        operation.cancel();
        assert!(sign_out_for_update(vec![find(&idle_config, &account).unwrap()], &operation).await.is_err());
        assert!(idle.alive.load(Ordering::Acquire) && game.game_running.load(Ordering::Acquire));
        sign_out_for_update(vec![find(&idle_config, &account).unwrap()],
            &crate::operation::Operation::default()).await.unwrap();
        until(|| !idle.alive.load(Ordering::Acquire));
        assert!(!idle.alive.load(Ordering::Acquire));
        assert!(game.alive.load(Ordering::Acquire) && game.game_running.load(Ordering::Acquire));
    }

    /// Audit 2026-11-09: a status change still queued for the worker made the update's
    /// sign-out fail ("Wait for the current PlayOnline request to finish.") instead of
    /// waiting the few ms until the worker takes it.
    #[tokio::test]
    async fn update_signout_waits_out_a_queued_status_control() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("social-fixture"), []).unwrap();
        let config = config(&fixture.0);
        let account = account();
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), Zeroizing::default(), None, Instant::now(), |_| {}).unwrap();
        let session = find(&config, &account).unwrap();
        let directory = config.account_directory(&account);
        // The worker cannot take the Away control while this lock is held.
        let held = exclusive(&directory.join("native-social.lock")).unwrap();
        save_json(&directory.join("native-control.json"),
            &ControlRequest { worker: session.worker, action: NativeControl::Away, groups: true }).unwrap();
        let operation = crate::operation::Operation::default();
        let mut signing_out = std::pin::pin!(sign_out_for_update(vec![session], &operation));
        assert!(tokio::time::timeout(Duration::from_millis(300), &mut signing_out).await.is_err(), "still waiting, not refused");
        drop(held);
        signing_out.await.unwrap();
        until(|| !launch.alive.load(Ordering::Acquire));
    }

    #[test]
    fn windower_turn_gives_up_after_its_limit_and_stops_when_aborted() {
        let fixture = Fixture::new();
        let path = fixture.0.join("locks/windower.lock");
        let held = exclusive(&path).unwrap();
        let mut waited = Vec::new();
        let started = Instant::now();
        let error = windower_turn_at(&path, &mut |line| waited.push(line.to_owned()), &|| None, Duration::from_millis(100)).unwrap_err();
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert_eq!(error, t("Could not acquire the Windower launch lock."));
        assert_eq!(waited.len(), 1, "the wait is announced once");
        let error = windower_turn_at(&path, &mut |_| {}, &|| Some("aborted".into()), Duration::from_secs(60)).unwrap_err();
        assert_eq!(error, "aborted");
        drop(held);
        assert!(windower_turn_at(&path, &mut |_| {}, &|| None, Duration::from_millis(100)).is_ok());
    }

    /// Live 2026-11-10: two of five Ashita hosts started in the same millisecond had pivot throw
    /// in their start-up and ended before crystal-loader ran. One Ashita start at a time, in
    /// its own lock: a Windower start never waits for it.
    #[test]
    fn ashita_turn_is_its_own_machine_wide_lock() {
        let fixture = Fixture::new();
        let (ashita, windower) = (fixture.0.join("locks/ashita.lock"), fixture.0.join("locks/windower.lock"));
        let held = ashita_turn_at(&ashita, &mut |_| panic!("the first start doesn't wait"), &|| None, Duration::from_millis(100)).unwrap();
        let mut waited = Vec::new();
        let error = ashita_turn_at(&ashita, &mut |line| waited.push(line.to_owned()), &|| None, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error, t("Could not acquire the Ashita launch lock."));
        assert_eq!(waited, [t("Waiting for the previous Ashita start to reach the game...")]);
        assert!(windower_turn_at(&windower, &mut |_| panic!("Windower has its own turn"), &|| None, Duration::from_millis(100)).is_ok());
        drop(held);
        assert!(ashita_turn_at(&ashita, &mut |_| {}, &|| None, Duration::from_millis(100)).is_ok());
        assert!(ashita_lock_path().unwrap().ends_with("Crystal Launcher/locks/ashita.lock"));
    }

    /// Audit 2026-11-09: closing during a sign-in that failed before any worker was seen
    /// waited the whole 30 s.
    #[test]
    fn launcher_close_does_not_wait_for_a_sign_in_that_ended_without_a_worker() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let done = Arc::new(AtomicBool::new(false));
        let mut pending = IdleSessionClose::for_account(&config, &account, true).unwrap().with_sign_in_done(done.clone());
        assert!(!pending.finished(), "the sign-in may still spawn its worker");
        done.store(true, Ordering::Release);
        assert!(pending.finished());
        // The close itself returns as soon as the sign-in thread reports, not at 30 s.
        let done = Arc::new(AtomicBool::new(false));
        let pending = IdleSessionClose::for_account(&config, &account, true).unwrap().with_sign_in_done(done.clone());
        let started = Instant::now();
        let reporter = std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(100)); done.store(true, Ordering::Release); });
        assert!(close_idle_sessions(vec![pending]).is_ok());
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        reporter.join().unwrap();
        // A worker that already failed is waited on until it exits, and not signed out.
        let worker = identity(std::process::id()).unwrap();
        fs::create_dir_all(config.account_directory(&account)).unwrap();
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Failed("fixture".into()), social_only: true,
            authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut pending = IdleSessionClose::for_account(&config, &account, false).unwrap();
        pending.worker = None;
        pending.requested = false;
        assert!(!pending.finished());
        assert!(pending.worker == Some(worker) && pending.requested);
        assert!(!pending.finished(), "its process is still running");
    }

    #[test]
    fn a_late_reply_is_swept_by_the_next_publish_but_capability_and_current_replies_stay() {
        let fixture = Fixture::new();
        for name in ["controller-settings-ready.json", "controller-settings-result-7.json", "controller-settings-result-8.json",
            "controller-settings-result-x.json", "game-shutdown-result-7.json"] {
            fs::write(fixture.0.join(name), []).unwrap();
        }
        sweep_results(&fixture.0, "controller-settings", 0);
        assert!(fixture.0.join("controller-settings-result-7.json").exists(), "a capability announcement sweeps nothing");
        sweep_results(&fixture.0, "controller-settings", 8);
        assert!(!fixture.0.join("controller-settings-result-7.json").exists());
        for kept in ["controller-settings-ready.json", "controller-settings-result-8.json", "controller-settings-result-x.json", "game-shutdown-result-7.json"] {
            assert!(fixture.0.join(kept).exists(), "{kept}");
        }
    }

    #[test]
    fn a_monitor_whose_launch_result_is_gone_stops() {
        let fixture = Fixture::new();
        let (alive, game, outcome) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(true)), Arc::new(Mutex::new(None)));
        let worker = identity(std::process::id()).unwrap();
        let (weak_alive, weak_game, weak_outcome) = (Arc::downgrade(&alive), Arc::downgrade(&game), Arc::downgrade(&outcome));
        drop((alive, game, outcome));
        let path = fixture.0.join("session.json");
        let monitor = std::thread::spawn(move || monitor(path, worker, weak_alive, weak_game, weak_outcome));
        until(|| monitor.is_finished());
    }

    /// Audit 2026-11-09: a milestone write that a reader blocked was lost, so a host lost
    /// after authentication could be signed in again.
    #[test]
    fn an_authenticated_milestone_blocked_by_a_reader_lands_with_the_next_record_update() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let directory = config.account_directory(&account); fs::create_dir_all(&directory).unwrap();
        let path = record_path(&config, &account);
        let worker = identity(std::process::id()).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("signing in".into()), social_only: true,
            authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        // A reader without delete sharing: the replace fails for as long as it is open.
        let reader = OpenOptions::new().read(true).share_mode(1).open(&path).unwrap();
        assert!(publish_authenticated(&directory).is_err());
        drop(reader);
        assert!(!read_record(&path).unwrap().authenticated);
        update_record(&path, worker, |record| record.native_batch = true).unwrap();
        let record = read_record(&path).unwrap();
        assert!(record.authenticated && record.native_batch);
        assert!(!authenticated_pending(&path));
    }

    #[test]
    fn update_preparation_requires_known_idle_state_or_an_explicit_social_signin() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        let path = record_path(&config, &account);
        let mut record = Record { worker, state: Event::Progress("Fixture signing in".into()),
            social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None };
        save_record(&path, &record).unwrap();
        let session = find(&config, &account).unwrap();
        assert!(!session.can_prepare_update());
        record.social_only = true;
        save_record(&path, &record).unwrap();
        assert!(session.can_prepare_update());
        let mut snapshot = crate::friends::Snapshot { game_running: Some(true), ..Default::default() };
        publish_friends(&directory, &snapshot).unwrap();
        assert!(!session.can_prepare_update());
        snapshot.game_running = Some(false);
        publish_friends(&directory, &snapshot).unwrap();
        assert!(session.can_prepare_update());
        record.worker.created += 1;
        save_record(&path, &record).unwrap();
        assert!(!session.can_prepare_update(), "a reused process identity cannot authorize sign-out");
    }

    #[test]
    #[ignore = "executed only in a fixture subprocess"]
    fn parent_fixture() {
        let root = PathBuf::from(std::env::var_os("CRYSTAL_SESSION_FIXTURE").unwrap());
        let launch = launch_command(
            child_command("session_worker::tests::worker_fixture", &root),
            &config(&root),
            account(),
            Zeroizing::new("123456".into()),
            None,
            Instant::now(),
            |_| {},
        )
        .unwrap();
        fs::write(root.join("parent-result"), launch.pid.to_string()).unwrap();
        // The process exits with a living worker and no game-session owner left here.
    }

    #[test]
    fn session_survives_parent_exit_reopens_and_records_clean_exit() {
        let fixture = Fixture::new();
        let status = child_command("session_worker::tests::parent_fixture", &fixture.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let config = config(&fixture.0);
        let path = record_path(&config, &account());
        until(|| {
            fs::read_to_string(fixture.0.join("heartbeat"))
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .is_some_and(|n| n >= 5)
        });
        let record = read_record(&path).unwrap();
        assert_eq!(
            identity(record.worker.pid),
            Some(record.worker),
            "worker outlives its parent"
        );
        let reconnected = find(&config, &account())
            .unwrap()
            .reconnect(|_| {})
            .unwrap();
        assert_eq!(
            reconnected.pid.to_string(),
            fs::read_to_string(fixture.0.join("parent-result")).unwrap()
        );
        assert_eq!(reconnected.timing, "Fixture timing");
        // A duplicate Play uses the living worker; this nonexistent command must never spawn.
        let duplicate = launch_command(
            Command::new("nonexistent-crystal-fixture"),
            &config,
            account(),
            Zeroizing::new("123456".into()),
            None,
            Instant::now(),
            |_| {},
        )
        .unwrap();
        assert_eq!(duplicate.pid, reconnected.pid);
        let disk = fs::read_to_string(&path).unwrap();
        assert!(!disk.contains("fixture-password-never-on-disk"));
        assert!(!disk.contains("123456"));
        fs::write(fixture.0.join("stop"), []).unwrap();
        until(|| !reconnected.alive.load(Ordering::Acquire));
        assert!(!reconnected
            .outcome
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_problem());
        assert!(find(&config, &account()).is_none());
        until(|| identity(record.worker.pid).is_none());
    }

    #[test]
    fn closing_during_sign_in_reconnects_after_the_game_starts() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("hold-start"), []).unwrap();
        let mut parent = child_command("session_worker::tests::parent_fixture", &fixture.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        until(|| fixture.0.join("heartbeat").exists());
        parent.kill().unwrap();
        parent.wait().unwrap();
        let config = config(&fixture.0);
        let session = find(&config, &account()).unwrap();
        let resumed = std::thread::spawn(move || session.reconnect(|_| {}));
        fs::write(fixture.0.join("release-start"), []).unwrap();
        let launch = resumed.join().unwrap().unwrap();
        assert_eq!(launch.timing, "Fixture timing");
        assert!(launch.alive.load(Ordering::Acquire));
        fs::write(fixture.0.join("stop"), []).unwrap();
        until(|| !launch.alive.load(Ordering::Acquire));
    }

    #[test]
    fn stale_or_reused_process_identity_cannot_reattach() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let path = record_path(&config, &account());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut worker = identity(std::process::id()).unwrap();
        worker.created += 1;
        save_record(
            &path,
            &Record {
                worker,
                state: Event::Progress("old session".into()),
                social_only: false,
                authenticated: false,
                native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None,
            },
        )
        .unwrap();
        assert!(find(&config, &account()).is_none());
    }

    #[test]
    fn friends_cache_is_account_and_session_scoped_and_refresh_is_one_shot() {
        let fixture = Fixture::new();
        let config = config(&fixture.0);
        let account = account();
        let directory = config.account_directory(&account);
        let path = record_path(&config, &account);
        fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("signed in".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let snapshot = crate::friends::Snapshot {
            entries: vec![crate::friends::Friend { name: "Alice".into(), alternate: false, slot: 199, uid:None, online: Some(true), location: Some("Vana'diel".into()), character: None, portrait: None }],
            fetched_at: 1, ..crate::friends::Snapshot::default()
        };
        publish_friends(&directory, &snapshot).unwrap();
        let (connected, received) = friends_snapshot(&config, &account);
        assert!(connected);
        assert_eq!(received.unwrap().entries[0].name, "Alice");
        let mut other = account.clone();
        other.pol_id = "another-account".into();
        assert!(friends_snapshot(&config, &other).1.is_none());
        let mut stale = worker;
        stale.created += 1;
        save_json(&directory.join("friends.json"), &FriendCache { worker: stale, snapshot }).unwrap();
        assert!(friends_snapshot(&config, &account).1.is_none());
        fs::write(directory.join("friends.json"), vec![b' '; 512 * 1024 + 1]).unwrap();
        assert!(friends_snapshot(&config, &account).1.is_none());
        save_record(&path, &Record { worker, state: Event::Finished(None), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        assert!(!friends_snapshot(&config, &account).0);
    }

    #[test]
    fn social_requests_are_atomic_one_shot_and_bound_to_the_native_session() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let directory = config.account_directory(&account); fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("signed in".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_choice: None, launch_tool: None }).unwrap();
        let mut snapshot = crate::friends::Snapshot { game_running: Some(false), ..Default::default() };
        assert!(request_social(&config, &account, crate::social::Request::Inbox { folder:0, page:0 }).is_err());
        snapshot.social.ready = true; publish_friends(&directory, &snapshot).unwrap();
        let id = request_social(&config, &account, crate::social::Request::Send { alternate:false, slot:1, uid:u64::MAX, subject:"あ\"".into(), message:"line\nline".into() }).unwrap();
        assert!(request_social(&config, &account, crate::social::Request::Inbox { folder:0, page:0 }).is_err());
        assert!(request_native_control(&config, &account, NativeControl::StartGame).is_err());
        let (request, lock) = take_social(&directory).unwrap(); assert_eq!(request.id, id); assert!(take_social(&directory).is_none());
        assert!(request_social(&config, &account, crate::social::Request::Inbox { folder:0, page:0 }).is_err(), "the consume/publication gap must remain locked");
        assert!(request_native_control(&config, &account, NativeControl::StartGame).is_err(), "a control also respects the consume/publication gap");
        snapshot.social.begin(id.clone(), &request.request); publish_friends(&directory, &snapshot).unwrap(); drop(lock);
        assert!(request_social(&config, &account, crate::social::Request::Inbox { folder:0, page:0 }).is_err(), "the published busy state must guard the request after unlocking");
        let line: serde_json::Value = serde_json::from_slice(&request.line().unwrap()).unwrap();
        assert_eq!(line["type"], "native_social"); assert_eq!(line["uid"].as_u64(), Some(u64::MAX)); assert_eq!(line["message"], "line\nline");
        let mut stale = worker; stale.created += 1;
        save_json(&directory.join("native-social.json"), &SocialRequest { worker:stale, id:id.clone(), request:crate::social::Request::Inbox { folder:0, page:0 } }).unwrap();
        assert!(take_social(&directory).is_none());
        snapshot.social.busy = true; publish_friends(&directory, &snapshot).unwrap();
        assert!(request_social(&config, &account, crate::social::Request::Inbox { folder:0, page:0 }).is_err());
        snapshot.social.busy = false; snapshot.game_running = Some(true); publish_friends(&directory, &snapshot).unwrap();
        assert!(request_social(&config, &account, crate::social::Request::Inbox { folder:0, page:0 }).is_err());
    }

    #[test]
    #[ignore = "executed only in a fixture subprocess"]
    fn lock_fixture() {
        let path =
            PathBuf::from(std::env::var_os("CRYSTAL_SESSION_FIXTURE").unwrap()).join("shared.lock");
        assert_eq!(exclusive(&path).unwrap_err().raw_os_error(), Some(32));
    }

    #[test]
    fn exclusive_lock_orders_workers_and_releases_after_owner_exit() {
        let fixture = Fixture::new();
        let path = fixture.0.join("shared.lock");
        let first = exclusive(&path).unwrap();
        assert!(
            child_command("session_worker::tests::lock_fixture", &fixture.0)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        );
        drop(first);
        assert!(exclusive(&path).is_ok());
    }
}
