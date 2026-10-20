//! The game session belongs to a headless copy of Crystal, not the iced window.
//! Credentials travel once through anonymous stdin; disk records contain only status.
//! Closing the window retires idle sessions; running games keep their own workers.

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
        atomic::{AtomicBool, AtomicU64, Ordering},
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

/// Frame 1 of a park worker (07d §1.4): what to park, no credentials. `account` has its
/// passwords cleared; its POL ID stays for the account's key and folders.
#[derive(Serialize, Deserialize)]
struct ParkRequest {
    config: Config,
    account: Account,
    plan: Plan,
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
}

// The login thread and late loader supervisor can both update this process's record.
static RECORD_WRITE: Mutex<()> = Mutex::new(());
static STATUS_WRITE_ID: AtomicU64 = AtomicU64::new(0);

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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(path)
}

/// Windower uses fixed named IPC objects: keep spawn-to-hello serialized across workers.
pub(crate) fn windower_turn(progress: &mut dyn FnMut(&str)) -> Result<File, String> {
    let root = std::env::var_os("CRYSTAL_DATA_ROOT")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .ok_or(t("Could not locate the Windower launch lock directory."))?;
    let path = PathBuf::from(root).join("Crystal Launcher/locks/windower.lock");
    let mut announced = false;
    loop {
        match exclusive(&path) {
            Ok(lock) => return Ok(lock),
            Err(error) if error.raw_os_error() == Some(32) => {
                if !announced {
                    progress(t("Waiting for the previous Windower start to reach the game..."));
                    announced = true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return Err(t("Could not acquire the Windower launch lock.").into()),
        }
    }
}

#[cfg(windows)]
fn identity(pid: u32) -> Option<Identity> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{
            GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != 258 {
        return None;
    }
    let mut created = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut exited, mut kernel, mut user) = (created, created, created);
    if unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    Some(Identity {
        pid,
        created: (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime),
    })
}

#[cfg(not(windows))]
fn identity(_pid: u32) -> Option<Identity> {
    None
}

fn read_record(path: &Path) -> Option<Record> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take((MAX_FRAME + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_FRAME {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn save_record(path: &Path, record: &Record) -> Result<(), String> {
    let _guard = RECORD_WRITE.lock().map_err(|_| "Could not lock the session status.")?;
    save_json(path, record)?;
    Ok(())
}

fn update_record(path: &Path, worker: Identity, update: impl FnOnce(&mut Record)) -> Result<(), String> {
    let _guard = RECORD_WRITE.lock().map_err(|_| "Could not lock the session status.")?;
    let mut record = read_record(path).filter(|r| r.worker == worker)
        .ok_or("The session status belongs to a different worker.")?;
    update(&mut record);
    save_json(path, &record)?;
    Ok(())
}

/// Called only for the native loader's typed authenticated event, never a progress label.
pub(crate) fn publish_authenticated(directory: &Path) -> Result<(), String> {
    let worker = identity(std::process::id()).ok_or("Could not identify the authenticated worker.")?;
    update_record(&directory.join("session.json"), worker, |record| {
        if matches!(record.state, Event::Progress(_) | Event::Started(_)) { record.authenticated = true; }
    })
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
        authenticated: true, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
    // A shared .tmp name lets concurrent writers replace or rename each other's data.
    // Each publication owns its staging file, including cleanup after a failed replace.
    let (temporary, mut file) = loop {
        let mut name = path.file_name().ok_or_else(|| t("Could not save the session status.").to_owned())?.to_os_string();
        name.push(format!(".{}.{}.tmp", std::process::id(), STATUS_WRITE_ID.fetch_add(1, Ordering::Relaxed)));
        let temporary = path.with_file_name(name);
        match OpenOptions::new().write(true).create_new(true).open(&temporary) {
            Ok(file) => break (PendingStatusFile(temporary), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(status_write_error(t("Could not save the session status."), path, error)),
        }
    };
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| status_write_error(t("Could not save the session status."), path, error))?;
    drop(file);
    replace_status_file(&temporary.0, path)
        .map_err(|error| status_write_error(t("Could not replace the session status."), path, error))
}

struct PendingStatusFile(PathBuf);
impl Drop for PendingStatusFile {
    fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
}

fn status_write_error(message: &str, path: &Path, error: io::Error) -> String {
    // These files contain only status; naming the file and OS error makes a failed
    // publication actionable without logging any account or session payload.
    format!("{message} {}: {error}", path.display())
}

fn replace_status_file(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let wide_path = |path: &Path| -> io::Result<Vec<u16>> {
            let filename = path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing status filename"))?;
            let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
            // Windows canonicalization supplies the extended drive/UNC prefix.
            // Resolve only the parent: the target need not exist, and replacing
            // a link must replace that directory entry rather than its referent.
            let absolute = fs::canonicalize(parent)?.join(filename);
            Ok(absolute.as_os_str().encode_wide().chain(Some(0)).collect())
        };
        let source = wide_path(source)?;
        let target = wide_path(target)?;
        let deadline = Instant::now() + Duration::from_millis(150);
        let mut pause = Duration::from_millis(5);
        loop {
            if unsafe { MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            ) } != 0 { return Ok(()); }
            let error = io::Error::last_os_error();
            // A scanner or reader without delete sharing may briefly hold the target.
            // Permanent permission failures still return promptly and keep the old file.
            if !matches!(error.raw_os_error(), Some(5 | 32 | 33)) || Instant::now() >= deadline {
                return Err(error);
            }
            std::thread::sleep(pause.min(deadline.saturating_duration_since(Instant::now())));
            pause = (pause * 2).min(Duration::from_millis(20));
        }
    }
    #[cfg(not(windows))]
    fs::rename(source, target)
}

#[derive(Serialize, Deserialize)]
struct FriendCache { worker: Identity, snapshot: crate::friends::Snapshot }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NativeControl { StartGame, SignOut, Visible, Away, Invisible }
impl NativeControl {
    pub(crate) fn name(self) -> &'static str {
        match self { Self::StartGame => "start_game", Self::SignOut => "sign_out", Self::Visible => "visible", Self::Away => "away", Self::Invisible => "invisible" }
    }
}
/// `groups`: a status change also goes to my groups (Viewer 10040f39, 404
/// when 385() > 0). The Viewer's default for its checkbox is not verified.
#[derive(Serialize, Deserialize)]
struct ControlRequest { worker: Identity, action: NativeControl, #[serde(default = "yes")] groups: bool }
fn yes() -> bool { true }

const CONTROLLER_FRAME_LIMIT: usize = 4096;
const CONTROLLER_READY_LIMIT: Duration = Duration::from_secs(2);
const CONTROLLER_RESULT_LIMIT: Duration = Duration::from_secs(5);
const CONTROLLER_MESSAGE_LIMIT: usize = 512;

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

fn controller_message_valid(message: &str) -> bool {
    message.len() <= CONTROLLER_MESSAGE_LIMIT && !message.chars().any(char::is_control)
}

#[cfg(windows)]
fn controller_clock_ms() -> u64 {
    #[link(name = "kernel32")]
    unsafe extern "system" { fn GetTickCount64() -> u64; }
    // SAFETY: this process-independent clock takes no pointers or handles.
    unsafe { GetTickCount64() }
}
#[cfg(not(windows))]
fn controller_clock_ms() -> u64 { 0 }

fn controller_result_path(directory: &Path, request_id: u64) -> PathBuf {
    directory.join(if request_id == 0 { "controller-settings-ready.json".to_owned() }
        else { format!("controller-settings-result-{request_id}.json") })
}

fn read_controller_result(directory: &Path, worker: Identity, request_id: u64) -> Option<ControllerResult> {
    let result: ControllerResult = serde_json::from_slice(&read_bounded(&controller_result_path(directory, request_id), CONTROLLER_FRAME_LIMIT)?).ok()?;
    (result.worker == worker && result.request_id == request_id && controller_message_valid(&result.message)).then_some(result)
}

fn controller_session(session: &BackgroundSession) -> Result<(), String> {
    if !session.active() { return Err("The game session ended before controller settings could be confirmed.".into()); }
    if session.game_running() != Some(true) { return Err("Start FFXI before applying controller settings to this account.".into()); }
    Ok(())
}

fn owned_controller_session(directory: &Path) -> Option<BackgroundSession> {
    let worker = identity(std::process::id())?;
    let session = BackgroundSession { path: directory.join("session.json"), worker };
    session.active().then_some(session)
}

fn controller_command_lock(directory: &Path) -> Result<File, String> {
    let until = Instant::now() + Duration::from_millis(150);
    loop {
        match exclusive(&directory.join("controller-settings.lock")) {
            Ok(lock) => return Ok(lock),
            Err(error) if matches!(error.raw_os_error(), Some(32 | 33)) && Instant::now() < until =>
                std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return Err("Could not access the controller settings command.".into()),
        }
    }
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
    request_controller_settings_for_session_within(session, settings, CONTROLLER_READY_LIMIT, CONTROLLER_RESULT_LIMIT)
}

#[cfg(test)]
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
        if let Some(ready) = read_controller_result(&directory, session.worker, 0) {
            if !ready.ok { return Err(no_live_controller_settings().into()); }
            break;
        }
        if Instant::now() >= until_ready { return Err(no_live_controller_settings().into()); }
        std::thread::sleep(Duration::from_millis(10).min(until_ready.saturating_duration_since(Instant::now())));
    }
    let random = crate::pol_less::pipe::random_hex().map_err(|_| "Could not identify the controller settings request.")?;
    let request_id = u64::from_str_radix(&random[..16], 16).unwrap_or(0).max(1);
    let deadline_ms = controller_clock_ms().checked_add(u64::try_from(result_limit.as_millis()).unwrap_or(u64::MAX))
        .filter(|deadline| *deadline != 0).ok_or("Could not time the controller settings request.")?;
    let request = ControllerRequest { worker: session.worker, request_id, settings, deadline_ms };
    let bytes = serde_json::to_vec(&request).map_err(|_| "Could not encode live controller settings.")?;
    if bytes.len() > CONTROLLER_FRAME_LIMIT { return Err("Live controller settings are too large.".into()); }
    let _pending = PendingControllerRequest { directory: directory.to_path_buf(), worker: session.worker, request_id };
    {
        let _lock = controller_command_lock(&directory)?;
        controller_session(session)?;
        let path = directory.join("controller-settings.json");
        if let Some(bytes) = read_bounded(&path, CONTROLLER_FRAME_LIMIT) {
            if serde_json::from_slice::<ControllerRequest>(&bytes).ok().is_some_and(|old|
                old.worker == session.worker && old.request_id != request_id && old.deadline_ms > controller_clock_ms()) {
                return Err("Wait for the previous controller settings request to finish.".into());
            }
        }
        save_json(&path, &request)?;
    }
    wake::signal(session.worker);
    loop {
        controller_session(session)?;
        if let Some(result) = read_controller_result(&directory, session.worker, request_id) {
            controller_session(session)?;
            return if result.ok { Ok(()) } else { Err(if result.message.is_empty() {
                "The game could not apply controller settings.".to_owned()
            } else { result.message }) };
        }
        if controller_clock_ms() >= deadline_ms {
            return Err("The game did not confirm controller settings in time.".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Consume exactly this worker's one-shot command while its game is running.
pub(crate) fn take_controller_settings(directory: &Path) -> Option<Vec<u8>> {
    let _lock = exclusive(&directory.join("controller-settings.lock")).ok()?;
    let path = directory.join("controller-settings.json");
    let request: ControllerRequest = serde_json::from_slice(&read_bounded(&path, CONTROLLER_FRAME_LIMIT)?).ok()?;
    let session = owned_controller_session(directory)?;
    if request.worker != session.worker { return None; }
    if request.request_id == 0 || request.deadline_ms == 0 || request.deadline_ms <= controller_clock_ms()
        || request.deadline_ms.saturating_sub(controller_clock_ms()) > CONTROLLER_RESULT_LIMIT.as_millis() as u64
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
    (request.deadline_ms > controller_clock_ms()).then_some(frame)
}

/// id 0 announces capability; every other reply is scoped to a single request.
pub(crate) fn publish_controller_result(directory: &Path, request_id: u64, ok: bool, message: &str) -> Result<(), String> {
    if !controller_message_valid(message) { return Err("Invalid controller settings result.".into()); }
    let _lock = controller_command_lock(directory)?;
    let session = owned_controller_session(directory).ok_or("The controller settings result belongs to an inactive worker.")?;
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
    let _batch = exclusive(&config.account_directory(account).join("batch.lock"))
        .map_err(|_| "This account is waiting for its batch to start.")?;
    if !request.valid() { return Err(t("Invalid PlayOnline request.").into()); }
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    let directory = config.account_directory(account);
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
    let temporary = PendingStatusFile(directory.join("native-social.json.tmp"));
    let mut file = File::create(&temporary.0).map_err(|_| t("Could not save the PlayOnline request."))?;
    file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|_| t("Could not save the PlayOnline request."))?;
    drop(file);
    fs::rename(&temporary.0, directory.join("native-social.json")).map_err(|_| t("Could not submit the PlayOnline request."))?;
    drop(_lock);
    wake::signal(session.worker);
    Ok(id)
}

pub(crate) fn take_social(directory: &Path) -> Option<(SocialRequest, File)> {
    // Hold the publication lock until the worker publishes busy=true. Removing
    // the one-shot file first must not expose an idle cache to a second window.
    let lock = exclusive(&directory.join("native-social.lock")).ok()?;
    let path = directory.join("native-social.json");
    let bytes = Zeroizing::new(read_bounded(&path, 64 * 1024)?);
    // A reader can deny deletion on Windows. Never deliver the message while
    // its one-shot file survives: the next idle tick would send it again.
    fs::remove_file(&path).ok()?;
    let request: SocialRequest = serde_json::from_slice(&bytes).ok()?;
    (Some(request.worker) == identity(std::process::id()) && request.request.valid()
        && request.id.len() == 32 && request.id.bytes().all(|b| b.is_ascii_hexdigit())).then_some((request, lock))
}

pub(crate) fn request_native_control(config: &Config, account: &Account, action: NativeControl) -> Result<(), String> {
    request_native_status(config, account, action, true)
}

pub(crate) fn request_native_status(config: &Config, account: &Account, action: NativeControl, groups: bool) -> Result<(), String> {
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    request_session_control(&session, action, groups)
}

fn no_native_controls() -> &'static str { t("This session does not support native PlayOnline controls. Launch it using the current build.") }
fn request_session_control(session: &BackgroundSession, action: NativeControl, groups: bool) -> Result<(), String> {
    let directory = session.path.parent().ok_or(t("This account has no active PlayOnline session."))?;
    let _batch = exclusive(&directory.join("batch.lock"))
        .map_err(|_| "This account is waiting for its batch to start.")?;
    // Shares the social publication lock so a control cannot race a message
    // submission or the worker's read/remove of the previous control.
    let _command = exclusive(&directory.join("native-social.lock"))
        .map_err(|_| t("Another PlayOnline request is being submitted."))?;
    if !session.active() { return Err(t("This account has no active PlayOnline session.").into()); }
    let snapshot = friend_cache(directory, session.worker).map_err(|_| no_native_controls())?;
    let startup_sign_out = matches!(action, NativeControl::SignOut)
        && snapshot.as_ref().and_then(|snapshot| snapshot.game_running).is_none()
        && read_record(&session.path).is_some_and(|record| record.worker == session.worker && record.social_only);
    if !startup_sign_out && snapshot.as_ref().and_then(|snapshot| snapshot.game_running) != Some(false) {
        return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
    }
    if snapshot.is_some_and(|snapshot| snapshot.social.busy) || directory.join("native-social.json").exists()
        || directory.join("native-control.json").exists() {
        return Err(t("Wait for the current PlayOnline request to finish.").into());
    }
    save_json(&directory.join("native-control.json"), &ControlRequest { worker: session.worker, action, groups })?;
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
    let directory = config.account_directory(account);
    // Pauses still to come in a pre-sign-in are skipped; nothing to do once they are over.
    let _ = note_play_pending(config, account);
    let mut announced = false;
    let mut idle = || -> Result<(), String> {
        loop {
            if !session.active() { return Err(t("This account has no active PlayOnline session.").into()); }
            if session.game_running() == Some(true) {
                return Err(t("Close FFXI before starting another game or signing out of PlayOnline.").into());
            }
            // A cache that can never show the main menu: the old immediate refusal, not 90 s
            // and a misleading timeout. A missing one is normal before the first publish.
            if friend_cache(&directory, session.worker).is_err() { return Err(no_native_controls().into()); }
            if main_menu_idle(&directory, session.worker) { return Ok(()); }
            if Instant::now() >= deadline {
                return Err(t("PlayOnline did not return to its main menu in time, so FFXI was not started.").into());
            }
            if !announced {
                waiting(t("Waiting for the PlayOnline main menu before starting FFXI…"));
                announced = true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    idle()?;
    let note = check()?;
    // Only a refusal that idle can end is retried (a request began during the check, or a
    // cache read raced its replacement); any other failure, such as a control file that
    // cannot be written, is reported at once.
    let transient = [t("Wait for the current PlayOnline request to finish."), t("Another PlayOnline request is being submitted."),
        no_native_controls(), t("Close FFXI before starting another game or signing out of PlayOnline."),
        t("This account has no active PlayOnline session.")];
    loop {
        match request_session_control(&session, NativeControl::StartGame, true) {
            Ok(()) => return Ok(note),
            Err(error) if batch_pending(config, account) || Instant::now() >= deadline
                || !transient.contains(&error.as_str()) => return Err(error),
            Err(_) => {
                std::thread::sleep(Duration::from_millis(20));
                idle()?;
            }
        }
    }
}

/// The action and its "also for my groups" choice.
pub(crate) fn take_native_control(directory: &Path) -> Option<(NativeControl, bool)> {
    take_native_control_matching(directory, false)
}

pub(crate) fn take_startup_sign_out(directory: &Path) -> bool {
    take_native_control_matching(directory, true).is_some()
}

fn take_native_control_matching(directory: &Path, sign_out_only: bool) -> Option<(NativeControl, bool)> {
    let _lock = exclusive(&directory.join("native-social.lock")).ok()?;
    let path = directory.join("native-control.json");
    let bytes = read_bounded(&path, 1024)?;
    let request = serde_json::from_slice::<ControlRequest>(&bytes).ok();
    if sign_out_only && request.as_ref().is_some_and(|request|
        !matches!(request.action, NativeControl::SignOut)
        || read_bounded(&directory.join("native-play-pending.json"), 1024)
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
    let directory = config.account_directory(account);
    let idle = read_friend_cache(&directory, session.worker).is_some_and(|s| s.game_running == Some(false));
    if !idle { return Ok(()); }
    save_json(&directory.join("native-activity.json"), &session.worker)?;
    wake::signal(session.worker);
    Ok(())
}

pub(crate) fn take_native_activity(directory: &Path) -> bool {
    let path = directory.join("native-activity.json");
    let Some(bytes) = read_bounded(&path, 1024) else { return false };
    if fs::remove_file(&path).is_err() { return false; }
    serde_json::from_slice::<Identity>(&bytes).ok() == identity(std::process::id())
}

/// A Play is queued behind this account's pre-sign-in (pol-spec 07d §4.3): its worker sends
/// the loader `native_play_pending` once, so the Viewer pauses still to come are skipped.
/// Harmless when the pauses are over. Local only.
pub(crate) fn note_play_pending(config: &Config, account: &Account) -> Result<(), String> {
    let session = find(config, account).ok_or(t("This account has no active PlayOnline session."))?;
    save_json(&config.account_directory(account).join("native-play-pending.json"), &session.worker)?;
    wake::signal(session.worker);
    Ok(())
}

pub(crate) fn take_play_pending(directory: &Path) -> bool {
    let path = directory.join("native-play-pending.json");
    let Some(bytes) = read_bounded(&path, 1024) else { return false };
    if fs::remove_file(&path).is_err() { return false; }
    serde_json::from_slice::<Identity>(&bytes).ok() == identity(std::process::id())
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

#[cfg(test)]
pub(crate) fn save_play_pending_for_test(directory: &Path) {
    save_json(&directory.join("native-play-pending.json"), &identity(std::process::id()).unwrap()).unwrap();
}

fn read_friend_cache(directory: &Path, worker: Identity) -> Option<crate::friends::Snapshot> {
    friend_cache(directory, worker).ok().flatten()
}

const FRIEND_CACHE_LIMIT: usize = 512 * 1024;
/// `Ok(None)`: missing, not readable just now, or another worker's file — the normal state
/// before this worker's first publish. `Err`: present but unusable until rewritten
/// (oversized, not a cache, or this worker's with out-of-bounds contents).
fn friend_cache(directory: &Path, worker: Identity) -> Result<Option<crate::friends::Snapshot>, ()> {
    let path = directory.join("friends.json");
    if fs::metadata(&path).is_ok_and(|m| m.len() > FRIEND_CACHE_LIMIT as u64) { return Err(()); }
    let Some(bytes) = read_bounded(&path, FRIEND_CACHE_LIMIT) else { return Ok(None) };
    let cache: FriendCache = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if cache.worker != worker { return Ok(None); }
    (cache.snapshot.entries.len() <= 300
        && cache.snapshot.entries.iter().all(|f| f.name.len() <= 256 && f.location.as_ref().is_none_or(|s| s.len() <= 256))
        && cache.snapshot.error.as_ref().is_none_or(|e| e.len() <= 4096)
        && cache.snapshot.session_error.as_ref().is_none_or(|e| e.len() <= 4096)
        && cache.snapshot.social.valid()).then_some(Some(cache.snapshot)).ok_or(())
}

pub(crate) fn publish_friends(directory: &Path, snapshot: &crate::friends::Snapshot) -> Result<(), String> {
    let worker = identity(std::process::id()).ok_or(t("Could not identify the friends session."))?;
    fs::create_dir_all(directory).map_err(|_| t("Could not save the friend list."))?;
    save_json(&directory.join("friends.json"), &FriendCache { worker, snapshot: snapshot.clone() })?;

    // Full idle is also the authenticated() fallback for a loader without typed milestones.

    Ok(())
}

/// Returns only a snapshot owned by this exact, still-running account worker.
pub(crate) fn friends_snapshot(config: &Config, account: &Account) -> (bool, Option<crate::friends::Snapshot>) {
    let Some(session) = find(config, account) else { return (false, None) };
    (true, read_friend_cache(&config.account_directory(account), session.worker))
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

pub(crate) fn run() -> i32 {
    run_with(|config, account, otp, via, clicked, progress| {
        backend::login_with_progress(config, account, otp, via, clicked, progress)
    })
}

type Login<'a> = dyn FnOnce(&Config, Account, String, Option<Plan>, Instant, &mut dyn FnMut(&str)) -> Result<LaunchResult, String> + 'a;

/// One length-prefixed JSON frame from the window (credentials travel only this way).
fn read_frame<T: serde::de::DeserializeOwned>(input: &mut impl Read) -> Result<T, String> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length).map_err(|_| t("Missing session request."))?;
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(t("Session request is too large.").into());
    }
    let mut bytes = Zeroizing::new(vec![0u8; length]);
    input.read_exact(&mut bytes).map_err(|_| t("Incomplete session request."))?;
    serde_json::from_slice(&bytes).map_err(|_| t("Invalid session request.").into())
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
            },
        )?;
        let clicked = crate::launch_timing::MonotonicStamp::restore(request.clicked_clock, request.elapsed_ms);
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
        let _ = update_record(&path, worker, |record| record.state = started.clone());
        emit(&started);
        while launch.alive.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let ended = Event::Finished(launch.outcome.lock().ok().and_then(|slot| slot.clone()));
        let _ = update_record(&path, worker, |record| record.state = ended.clone());
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
    let request = loop {
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
            let (message, log) = crate::launch_log::split_log(error);
            let message: String = message.chars().take(240).collect();
            crate::launch_log::append_history(&history, &label, &message, log.as_deref());
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
}

/// Start a park worker for `account` and `plan`; frame 1 carries no password and no OTP.
pub(crate) fn spawn_park(config: &Config, account: &Account, plan: &Plan) -> Result<ParkWorker, String> {
    let mut command = Command::new(std::env::current_exe().map_err(|_| t("Could not locate Crystal's session worker."))?);
    command.arg("--session-worker").arg(crate::pol_less::park::PARK_ARG);
    spawn_park_command(command, config, account, plan)
}

/// Frame 1: no password, no OTP; the plan's per-Play fields are not used before frame 2.
fn park_frame(config: &Config, account: &Account, plan: &Plan) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut account = account.clone();
    account.pol_password.zeroize();
    account.se_password.zeroize();
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
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|_| t("Could not start Crystal's background session worker."))?;
    let started = (|| -> Result<(Identity, std::process::ChildStdin, std::process::ChildStdout), String> {
        let worker = identity(child.id()).ok_or(t("Could not identify Crystal's background session worker."))?;
        let mut stdin = child.stdin.take().ok_or(t("Missing session progress pipe."))?;
        stdin.write_all(&(bytes.len() as u32).to_le_bytes()).and_then(|_| stdin.write_all(&bytes))
            .map_err(|_| t("Could not hand credentials to the background session worker."))?;
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
            let mut line = String::new();
            match output.by_ref().take((MAX_FRAME + 1) as u64).read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(count) if count > MAX_FRAME => return,
                Ok(_) => {}
            }
            let Some(event) = line.trim_end().strip_prefix(PREFIX).and_then(|json| serde_json::from_str::<Event>(json).ok()) else { continue };
            if sender.send(event).is_err() { return; }
        }
    });
    if reader.is_err() {
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return Err(t("Could not start Crystal's background session worker.").into());
    }
    Ok(ParkWorker { child, stdin: Some(stdin), events, worker, ended: false, parked: false })
}

impl crate::pol_less::park::Worker for ParkWorker {
    fn poll(&mut self) -> Option<crate::pol_less::park::Note> {
        use crate::pol_less::park::{Note, ParkedInfo};
        loop {
            match self.events.try_recv() {
                Ok(Event::Parked { loader_pid, tool_pid, polcore_ms, hello_ms }) => {
                    self.parked = true;
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
        };
        let bytes = Zeroizing::new(serde_json::to_vec(&request).map_err(|_| otp.to_string())?);
        drop(request);
        if bytes.len() > MAX_FRAME { return Ok(Err(t("Session request is too large.").into())); }
        let sent = self.stdin.as_mut().map_or(Err(io::ErrorKind::BrokenPipe.into()), |stdin| {
            stdin.write_all(&(bytes.len() as u32).to_le_bytes()).and_then(|_| stdin.write_all(&bytes)).and_then(|_| stdin.flush())
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


fn monitor(
    path: PathBuf,
    worker: Identity,
    alive: Arc<AtomicBool>,
    game_running: Arc<AtomicBool>,
    outcome: Arc<Mutex<Option<ExitReport>>>,
) {
    let mut previous_friends = None;
    loop {
        let modified = path.parent().and_then(|d| fs::metadata(d.join("friends.json")).ok()).and_then(|m| m.modified().ok());
        if modified != previous_friends {
            if let Some(snapshot) = path.parent().and_then(|d| read_friend_cache(d, worker)) {
                if let Some(game) = snapshot.game_running { game_running.store(game, Ordering::Release); }
                previous_friends = modified;
            }
        }
        if let Some(record) = read_record(&path).filter(|r| r.worker == worker) {
            match record.state {
                Event::Finished(report) => {
                    if let Ok(mut slot) = outcome.lock() {
                        *slot = report;
                    }
                    break;
                }
                Event::Failed(error) => {
                    if let Ok(mut slot) = outcome.lock() {
                        *slot = Some(crate::launch_log::classify_exit(&error, Some(1), None));
                    }
                    break;
                }
                _ => {}
            }
        }
        if identity(worker.pid) != Some(worker) {
            if let Ok(mut slot) = outcome.lock() {
                *slot = Some(ExitReport::worker_ended());
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    alive.store(false, Ordering::Release);
    game_running.store(false, Ordering::Release);
}

fn track(path: PathBuf, worker: Identity, started: Started) -> Result<LaunchResult, String> {
    let alive = Arc::new(AtomicBool::new(true));
    let game_running = Arc::new(AtomicBool::new(path.parent().and_then(|d| read_friend_cache(d, worker)).and_then(|s| s.game_running).unwrap_or(true)));
    let outcome = Arc::new(Mutex::new(None));
    let (tracked, ended, game) = (alive.clone(), outcome.clone(), game_running.clone());
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
    let path = record_path(config, account);
    let record = read_record(&path)?;
    if identity(record.worker.pid) != Some(record.worker) {
        return None;
    }
    if !matches!(record.state, Event::Started(_) | Event::Progress(_)) {
        return None;
    }
    Some(BackgroundSession {
        path,
        worker: record.worker,
    })
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
        let record = read_record(&self.path).filter(|record|
            record.worker == self.worker && identity(self.worker.pid) == Some(self.worker)
            && matches!(record.state, Event::Progress(_) | Event::Started(_)))
            .ok_or_else(|| t("The background session ended before Play could start. Sign in again.").to_owned())?;
        // Old workers do not identify their tool. They can reconnect as before,
        // but cannot promise Vanilla because their host may already contain hooks.
        if record.launch_tool.is_some_and(|tool| tool != requested)
            || record.launch_tool.is_none() && requested == Tool::Vanilla {
            return Err(t("Sign out of PlayOnline before switching launch methods, then launch again.").into());
        }
        Ok(())
    }

    /// The session's launch log, once its worker published `Started`.
    pub(crate) fn log(&self) -> Option<PathBuf> {
        read_record(&self.path).filter(|record| record.worker == self.worker)
            .and_then(|record| match record.state { Event::Started(started) => Some(started.log), _ => None })
    }

    fn active(&self) -> bool {
        identity(self.worker.pid) == Some(self.worker)
            && read_record(&self.path).is_some_and(|record| record.worker == self.worker
                && matches!(record.state, Event::Progress(_) | Event::Started(_)))
    }

    pub(crate) fn supports_batch_start(&self) -> bool {
        identity(self.worker.pid) == Some(self.worker)
            && read_record(&self.path).is_some_and(|record| record.worker == self.worker
                && record.native_batch && matches!(record.state, Event::Progress(_) | Event::Started(_)))
    }

    /// A known idle session, or a sign-in explicitly started without a game.
    /// Unknown game state on an ordinary launch must never be treated as idle.
    pub(crate) fn can_prepare_update(&self) -> bool {
        if !self.active() { return false; }
        let snapshot = self.path.parent().and_then(|directory| read_friend_cache(directory, self.worker));
        match snapshot.and_then(|snapshot| snapshot.game_running) {
            Some(running) => !running,
            None => read_record(&self.path).is_some_and(|record| record.worker == self.worker && record.social_only),
        }
    }

    /// Whether this session's sign-in took a one-time code (see `Record::otp_used`); `None`
    /// when its worker's record does not say (an older worker, or the record is unreadable).
    pub(crate) fn otp_used(&self) -> Option<bool> {
        read_record(&self.path).filter(|record| record.worker == self.worker).and_then(|record| record.otp_used)
    }

    /// A reopened window can queue Play on a sign-in-only worker without starting another.
    pub(crate) fn preparing_social(&self) -> bool {
        identity(self.worker.pid) == Some(self.worker)
            && read_record(&self.path).is_some_and(|record| record.worker == self.worker
                && record.social_only && matches!(record.state, Event::Progress(_) | Event::Started(_)))
            && !self.path.parent().and_then(|directory| read_friend_cache(directory, self.worker))
                .is_some_and(|snapshot| snapshot.social.ready && snapshot.game_running.is_some())
    }

    /// Status only: no Play gates on authentication (W20 waits for the main menu, 07c §5.2).
    #[cfg(test)]
    fn authenticated(&self) -> bool {
        identity(self.worker.pid) == Some(self.worker)
            && read_record(&self.path).is_some_and(|record| record.worker == self.worker
                && matches!(record.state, Event::Progress(_) | Event::Started(_))
                && (record.authenticated
                    // An older worker has no typed milestone; full idle remains safe.
                    || self.path.parent().and_then(|directory| read_friend_cache(directory, self.worker))
                        .is_some_and(|snapshot| snapshot.game_running == Some(false) && snapshot.social.ready)))
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
}

impl IdleSessionClose {
    fn known(session: BackgroundSession) -> Self {
        Self { path: session.path, worker: Some(session.worker), requested: false }
    }

    pub(crate) fn for_account(config: &Config, account: &Account, preparing: bool) -> Option<Self> {
        if let Some(session) = find(config, account) {
            return session.can_prepare_update().then(|| Self::known(session));
        }
        let path = record_path(config, account);
        if let Some(record) = read_record(&path).filter(|record|
            identity(record.worker.pid) == Some(record.worker)
            && matches!(record.state, Event::Finished(_) | Event::Failed(_))) {
            return Some(Self { path, worker: Some(record.worker), requested: true });
        }
        preparing.then(|| Self { path: record_path(config, account), worker: None, requested: false })
    }

    /// True when the captured worker ended, or a game/queued Play needs to keep
    /// running. Completion records alone precede Windower's final profile save.
    fn finished(&mut self) -> bool {
        let Some(worker) = self.worker else {
            if let Some(record) = read_record(&self.path).filter(|record|
                identity(record.worker.pid) == Some(record.worker)
                && matches!(record.state, Event::Progress(_) | Event::Started(_))) {
                self.worker = Some(record.worker);
            }
            return false;
        };
        if identity(worker.pid) != Some(worker) { return true; }
        let session = BackgroundSession { path: self.path.clone(), worker };
        if self.requested { return session.game_running() == Some(true); }
        if !session.active() { return false; }
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
        let ready = session.can_prepare_update() && snapshot.is_none_or(|snapshot| !snapshot.social.busy)
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
    let mut otp = Zeroizing::new(otp);
    // Local readiness only: the Viewer's W2U goes out at Connect, inside this sign-in
    // (pol-spec 07c §1, §3), never before it.
    if account.mode == backend::AccountMode::Retail && find(config, &account).is_none() {
        crate::viewer::gate(&crate::viewer::local_status())?;
    }
    launch_preflighted(config, account, std::mem::take(&mut *otp), via, clicked, progress)
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
    operation.progress("SignOut", 0, total, t("Signing out idle PlayOnline sessions before updating"));
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
            let ready = read_friend_cache(directory, session.worker).is_some_and(|snapshot|
                snapshot.game_running == Some(false) && !snapshot.social.busy
                && !directory.join("native-social.json").exists());
            if ready {
                request_session_control(session, NativeControl::SignOut, true)?;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    loop {
        operation.check_cancelled()?;
        let active: Vec<_> = sessions.iter().filter(|session| session.active()).collect();
        operation.progress("SignOut", total - active.len() as u64, total,
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
                let ready = read_friend_cache(directory, session.worker).is_some_and(|snapshot|
                    snapshot.game_running == Some(false) && !snapshot.social.busy)
                    && !directory.join("native-social.json").exists();
                if ready {
                    request_session_control(&session, NativeControl::SignOut, true)?;
                    requested = true;
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
    otp: String,
    via: Option<Plan>,
    clicked: Instant,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    let mut otp = Zeroizing::new(otp);
    let mut command = Command::new(
        std::env::current_exe().map_err(|_| t("Could not locate Crystal's session worker."))?,
    );
    command.arg("--session-worker");
    launch_command(
        command,
        config,
        account,
        std::mem::take(&mut *otp),
        via,
        clicked,
        progress,
    )
}

fn launch_command(
    mut command: Command,
    config: &Config,
    account: Account,
    otp: String,
    via: Option<Plan>,
    clicked: Instant,
    mut progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    let mut otp = Zeroizing::new(otp);
    if via.as_ref().is_some_and(|p| !p.social_only) && batch_pending(config, &account) {
        return Err("This account is waiting for its batch to start.".into());
    }
    if let Some(session) = find(config, &account) {
        if let Some(plan) = &via { session.ensure_launch_tool(plan.tool)?; }
        if via.as_ref().is_some_and(|p| !p.social_only)
            && read_friend_cache(&config.account_directory(&account), session.worker).is_some_and(|s| s.game_running == Some(false)) {
            // This session is already signed in: its main menu, then the stock W20, then
            // StartGame (07c §5.3). A fresh Play checks inside its own worker. The trigger
            // and W20 lines go to the session's own log (07d §4.2, 08 §4 item 6).
            let log = session.log();
            let clock = crate::launch_timing::Timeline::begin_at(clicked);
            if let Some(log) = &log {
                crate::launch_log::append_launcher_lines(log, &crate::pol_less::launch_clock_lines(&clock, crate::launch_timing::LaunchTrigger::PreSignedPlay));
            }
            start_signed_in(config, &account, &mut progress, || {
                let (verdict, timing) = crate::patch::gate_before_start_timed(&crate::pol_less::install_directory(config)?);
                if let Some(log) = &log { crate::launch_log::append_launcher_lines(log, &crate::launch_timing::check_lines("FFXI check (W20)", &timing)); }
                verdict
            })?;
        }
        return session.reconnect(progress);
    }
    let _launch_claim = if via.as_ref().is_some_and(|p| !p.social_only) {
        Some(exclusive(&config.account_directory(&account).join("batch.lock"))
            .map_err(|_| "This account is waiting for its batch to start.")?)
    } else { None };
    let path = record_path(config, &account);
    let request = Request {
        config: config.clone(),
        account,
        otp: std::mem::take(&mut *otp),
        via,
        elapsed_ms: clicked.elapsed().as_millis().min(u64::MAX as u128) as u64,
        clicked_clock: crate::launch_timing::MonotonicStamp::capture(clicked),
    };
    let bytes = Zeroizing::new(
        serde_json::to_vec(&request).map_err(|_| t("Could not prepare the session request."))?,
    );
    drop(request);
    if bytes.len() > MAX_FRAME {
        return Err(t("Session request is too large.").into());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|_| t("Could not start Crystal's background session worker."))?;
    let Some(worker) = identity(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(t("Could not identify Crystal's background session worker.").into());
    };
    let sent = (|| -> io::Result<()> {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("Missing worker pipe"))?;
        input.write_all(&(bytes.len() as u32).to_le_bytes())?;
        input.write_all(&bytes)?;
        Ok(())
    })();
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
        let mut line = String::new();
        let count = output
            .by_ref()
            .take((MAX_FRAME + 1) as u64)
            .read_line(&mut line)
            .map_err(|_| t("Could not read session progress."))?;
        if count == 0 {
            return Err(t("The background session worker ended before starting the game.").into());
        }
        if count > MAX_FRAME {
            return Err(t("Session progress is too large.").into());
        }
        let Some(json) = line.trim_end().strip_prefix(PREFIX) else {
            continue;
        };
        let event: Event = serde_json::from_str(json).map_err(|_| t("Invalid session progress."))?;
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn controller_fixture(root: &Path, game_running: Option<bool>) -> (Config, Account, PathBuf, Identity) {
        let config = config(root); let account = account();
        let directory = config.account_directory(&account); fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("fixture game".into()),
            social_only: false, authenticated: true, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
            assert!(frame["deadline_ms"].as_u64().unwrap() > controller_clock_ms());
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
        assert!(publish_controller_result(&directory, 0, true, &"x".repeat(CONTROLLER_MESSAGE_LIMIT + 1)).is_err());
        assert!(publish_controller_result(&directory, 0, true, "bad\nmessage").is_err());
    }

    #[test]
    fn controller_consumer_discards_expiry_and_requires_exact_process_creation_identity() {
        let fixture = Fixture::new();
        let (config, account, directory, worker) = controller_fixture(&fixture.0, Some(true));
        let path = directory.join("controller-settings.json");
        let stale = Identity { created: worker.created + 1, ..worker };
        let mut request = ControllerRequest { worker: stale, request_id: 42, settings: controller_values(), deadline_ms: controller_clock_ms() + 1000 };
        save_json(&path, &request).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        assert!(path.exists(), "an old worker cannot delete another worker's command");
        request.worker = worker;
        request.deadline_ms = controller_clock_ms().saturating_sub(1);
        save_json(&path, &request).unwrap();
        assert!(take_controller_settings(&directory).is_none());
        assert!(!path.exists(), "an expired owned command is discarded");
        request.deadline_ms = controller_clock_ms() + 1000;
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
                social_only: false, authenticated: true, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
            &config, account.clone(), String::new(), None, Instant::now(), |_| {}).unwrap();
        let captured = find(&config, &account).unwrap();
        fs::write(fixture.0.join("stop"), []).unwrap();
        until(|| !launch.alive.load(Ordering::Acquire) && identity(captured.worker.pid) != Some(captured.worker));
        let directory = config.account_directory(&account);
        let replacement = identity(std::process::id()).unwrap();
        save_record(&record_path(&config, &account), &Record { worker: replacement, state: Event::Progress("replacement game".into()),
            social_only: false, authenticated: true, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
                authenticated: true, native_batch: false, otp_used: Some(used), launch_tool: None }).unwrap();
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
                authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: stored };
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
    fn switching_to_vanilla_stops_before_reconnect_version_checks_or_game_control() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let path = record_path(&config, &account); fs::create_dir_all(path.parent().unwrap()).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&path, &Record { worker, state: Event::Progress("Fixture Windower host".into()),
            social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_tool: Some(Tool::Windower) }).unwrap();
        let original = fs::read(&path).unwrap();
        let plan = Plan { tool: Tool::Vanilla, settings: Default::default(),
            social_only: false, invisible_login: false, auto_away_minutes: 10, auto_login: crate::pol_less::AutoLogin::default(), viewer_gaps: false, trigger: Default::default() };
        let error = launch_command(Command::new("must-not-spawn.exe"), &config, account.clone(), String::new(),
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
                social_only: true, authenticated: false, native_batch: true, otp_used: Some(false), launch_tool: None }).unwrap();
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
                    social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_tool: None }).unwrap();

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
        assert!(!session.preparing_social() && !session.authenticated() && !session.supports_batch_start());
        update_record(&path, worker, |record| record.social_only = true).unwrap();
        assert!(session.preparing_social());
        assert!(!session.authenticated());
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
        assert!(session.authenticated() && session.preparing_social() && session.supports_batch_start());
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
        assert!(!session.authenticated() && !session.supports_batch_start());
        save_record(&path, &Record { worker: Identity { created: 0, ..worker }, state: Event::Progress("reused PID".into()), social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_tool: None }).unwrap();
        assert!(publish_authenticated(&directory).is_err());
        assert!(publish_native_batch(&directory, true).is_err());
        assert!(!session.preparing_social() && !session.authenticated() && !session.supports_batch_start());
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
        save_record(&path, &Record { worker, state: Event::Progress("pre-sign-in".into()), social_only: true, authenticated: true, native_batch: true, otp_used: Some(false), launch_tool: None }).unwrap();
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
            authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None };
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
        save_record(&path, &Record { worker, state: Event::Progress("fixture".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("fixture".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
                *ended.lock().unwrap() = Some(crate::launch_log::classify_exit(
                    "[recomp] quit",
                    Some(0),
                    None,
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
        Plan { tool: Tool::Windower, settings: Default::default(), social_only: false, invisible_login: false, auto_away_minutes: 10,
            auto_login: crate::pol_less::AutoLogin::default(), viewer_gaps: false, trigger: Default::default() }
    }

    fn play_request(config: &Config, account: Account, trigger: crate::launch_timing::LaunchTrigger) -> Request {
        Request { config: config.clone(), account, otp: "123456".into(), via: Some(Plan { trigger, ..park_plan() }), elapsed_ms: 0, clicked_clock: None }
    }

    /// 07d §1.4: frame 1 never carries a password or OTP.
    #[test]
    fn the_park_frame_carries_no_secret() {
        let fixture = Fixture::new();
        let mut plan = park_plan();
        plan.trigger = crate::launch_timing::LaunchTrigger::ParkedPlay;
        let bytes = park_frame(&config(&fixture.0), &account(), &plan).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("fixture-password-never-on-disk") && !text.contains("\"otp\""), "{text}");
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
        let mut account = account();
        account.mode = backend::AccountMode::Retail;
        account.server.clear();
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0), &config, account.clone(), "123456".into(), None, Instant::now(), |_| {}).unwrap();
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
                invisible_login: false, auto_away_minutes: 0, auto_login: Default::default(),
                viewer_gaps: false, trigger: Default::default() };
            let started = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
                &config, account.clone(), String::new(), Some(plan), Instant::now(), |_| {}).unwrap();
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
                &config, account.clone(), String::new(), None, Instant::now(), |_| {}).unwrap();
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
            invisible_login: false, auto_away_minutes: 0, auto_login: Default::default(),
            viewer_gaps: false, trigger: Default::default() };
        let first = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), String::new(), Some(plan(Tool::Vanilla)), Instant::now(), |_| {}).unwrap();
        let session = find(&config, &account).unwrap();
        assert!(session.ensure_launch_tool(Tool::Windower).is_err());
        session.sign_out().unwrap();
        until(|| !first.alive.load(Ordering::Acquire));
        assert!(!first.alive.load(Ordering::Acquire) && find(&config, &account).is_none());
        let second = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), String::new(), Some(plan(Tool::Windower)), Instant::now(), |_| {}).unwrap();
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
            invisible_login: false, auto_away_minutes: 0, auto_login: Default::default(),
            viewer_gaps: false, trigger: Default::default() };
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account, String::new(), Some(plan), Instant::now(), |_| {}).unwrap();
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
            &config, account.clone(), String::new(), None, Instant::now(), |_| {}).unwrap();
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
            invisible_login: false, auto_away_minutes: 0, auto_login: Default::default(),
            viewer_gaps: false, trigger: Default::default() };
        let _launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), String::new(), Some(plan), Instant::now(), |_| {}).unwrap();
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
        let mut account = account();
        account.mode = backend::AccountMode::Retail;
        account.server.clear();
        let launch = launch_command(child_command("session_worker::tests::worker_fixture", &fixture.0),
            &config, account.clone(), String::new(), None, Instant::now(), |_| {}).unwrap();
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

        // The real main-button dispatcher now accepts this previously blocked
        // automatic sign-in. Dropping its task avoids any real updater network
        // or disk work; exercise its native preparation below with the same op.
        assert_eq!(app.update(crate::Message::MainAction).units(), 1);
        assert!(app.page == crate::Page::Maintenance);
        assert!(app.maintenance == Some(crate::MaintenanceKind::Update));
        assert!(app.launch_blocked());
        let operation = app.active_operation.as_ref().unwrap();
        assert_eq!(operation.snapshot().stage, "SignOut");
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
            &idle_config, account.clone(), String::new(), None, Instant::now(), |_| {}).unwrap();
        let game = launch_command(child_command("session_worker::tests::worker_fixture", &game_fixture.0),
            &game_config, account.clone(), String::new(), None, Instant::now(), |_| {}).unwrap();
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
            social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None };
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
            "123456".into(),
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
            "123456".into(),
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
                native_batch: false, otp_used: Some(false), launch_tool: None,
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
        save_record(&path, &Record { worker, state: Event::Progress("signed in".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
        let snapshot = crate::friends::Snapshot {
            entries: vec![crate::friends::Friend { name: "Alice".into(), alternate: false, slot: 199, uid:None, online: Some(true), location: Some("Vana'diel".into()), character: None }],
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
        save_record(&path, &Record { worker, state: Event::Finished(None), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
        assert!(!friends_snapshot(&config, &account).0);
    }

    #[test]
    fn social_requests_are_atomic_one_shot_and_bound_to_the_native_session() {
        let fixture = Fixture::new(); let config = config(&fixture.0); let account = account();
        let directory = config.account_directory(&account); fs::create_dir_all(&directory).unwrap();
        let worker = identity(std::process::id()).unwrap();
        save_record(&record_path(&config, &account), &Record { worker, state: Event::Progress("signed in".into()), social_only: false, authenticated: false, native_batch: false, otp_used: Some(false), launch_tool: None }).unwrap();
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
