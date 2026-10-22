//! The parked warm host (pol-spec 07d §1, §5; 07a parked-warm-host, conservative
//! variant): Windower/Ashita/Vanilla starts the host exactly as a Play does, the host
//! gets a credential-less `park` line, creates polcore and waits; the credentials go
//! only at Play. Nothing reaches Square Enix while parked.
//!
//! Three parts: the host side the park worker drives (`park`, `Parked`), the window's pool
//! of park workers (`Pool`, one per `ParkKey`), and what decides a re-park (`fingerprint`).

use super::{AutoLogin, Prepared, SignedIn, Tool};
use crate::backend::{Account, Config, LaunchResult};
use crate::i18n::{t, tf};
use crate::launch_timing::{Milestone, Timeline};
use super::pipe::LoaderEvent;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

/// The first park waits this long after the startup sign-ins were issued, so they take
/// the Windower turn first (07d §1.5, §5.7).
pub(crate) const PARK_START_DELAY: Duration = Duration::from_secs(2);
/// `park` sent → `parked`. polcore creation is 0.1-0.7 s (08 §2); past this the park failed.
pub(crate) const PARKED_LIMIT: Duration = Duration::from_secs(30);
/// Least gap between two parked hosts' credentials: N member logins must never start
/// within milliseconds from one IP (07a parked-warm-host parity condition 3; today's
/// Windower stagger is 1.4-2.9 s).
pub(crate) const CREDENTIAL_STAGGER: Duration = Duration::from_millis(1500);
/// Plus a uniformly random 0..this per delivery.
pub(crate) const CREDENTIAL_JITTER: Duration = Duration::from_millis(500);
/// Consecutive failures for one key before the pool stops re-parking it, until its
/// fingerprint changes or Crystal restarts.
pub(crate) const PARK_FAILURE_LIMIT: u32 = 3;
/// After `--session-worker`: this worker parks first and reads its Play request later.
pub(crate) const PARK_ARG: &str = "--park";
/// Launcher → loader: a Play is queued behind this pre-sign-in, so the loader skips the
/// Viewer gaps still to come (07d §4.3). No answer.
pub(crate) const PLAY_PENDING_LINE: &str = "{\"type\":\"native_play_pending\"}\n";
/// The park failure for a loader whose hello lacks `park`, `native_auth` or `viewer_check`
/// (07d §1.6): that key is not parked again until its fingerprint changes.
pub(crate) const UNSUPPORTED: &str = "unsupported loader";
/// How often the window compares the parked keys' fingerprints with the files (07d §1.5).
pub(crate) const FINGERPRINT_EVERY: Duration = Duration::from_secs(10);
/// How long the window waits for a closed parked worker to clean up before terminating it.
pub(crate) const CLOSE_WAIT: Duration = Duration::from_secs(2);
/// A park the window closed while it was still starting (its stdin ended before `Parked`):
/// stopped before the tool starts, or with Windower stopped first once it has (07a).
pub(crate) const ABORTED: &str = "The park was closed before it finished.";
/// While parking, how often the wait for `parked` looks at the abort flag.
const ABORT_POLL: Duration = Duration::from_millis(50);

/// One parked host per account, tool and tool profile (Windower profile, Ashita boot ini;
/// empty for Vanilla): Hook takes the Windower profile at injection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ParkKey {
    /// `backend::account_key`.
    pub account: String,
    pub tool: &'static str,
    pub profile: String,
}
impl ParkKey {
    pub(crate) fn new(account: String, tool: Tool, profile: String) -> Self {
        Self { account, tool: tool.name(), profile }
    }
    /// The key of `account` launched with `plan`.
    pub(crate) fn for_plan(account: &Account, plan: &super::Plan) -> Self {
        let profile = match plan.tool {
            Tool::Windower => plan.settings.windower_profile.trim(),
            Tool::Ashita => plan.settings.boot_ini.trim(),
            Tool::Vanilla => "",
        };
        Self::new(crate::backend::account_key(account), plan.tool, profile.to_owned())
    }
}

/// Everything a parked host was started with, local files only. Any difference means
/// re-park (07d §1.5): the `park` line itself (folders, auto-login, FFXI settings), the
/// tool settings (folders, profile, boot ini, loader, dgVoodoo, ReShade), the loader's
/// size and mtime, patch.ver (Viewer and FFXI), polcore.dll, for Windower its settings.xml
/// (by content), Windower.exe, Hook.dll and plugins\*.dll, and for Ashita its base boot ini.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    /// (what, value) in a fixed order; values are digests, sizes or mtimes, never secrets.
    pub parts: Vec<(&'static str, String)>,
}
impl Fingerprint {
    /// The names whose values differ, for the log line `re-park: <names> changed`. A part
    /// now `UNSETTLED` (a file mid-save) is no change yet; the next check sees the result.
    pub(crate) fn changed(&self, now: &Fingerprint) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.parts.iter()
            .filter(|part| !now.parts.contains(part) && !now.parts.iter().any(|(n, value)| *n == part.0 && value == UNSETTLED))
            .map(|(n, _)| *n).collect();
        names.extend(now.parts.iter().filter(|(n, _)| !self.parts.iter().any(|(m, _)| m == n)).map(|(n, _)| *n));
        names.dedup();
        names
    }
}

/// Size and mtime of one file, or "missing".
fn stamp(path: &Path) -> String {
    std::fs::metadata(path).ok().map_or_else(|| "missing".into(), |meta| format!("{} {:?}", meta.len(), meta.modified().ok()))
}

/// A fingerprint value for a file being rewritten (it doesn't parse yet).
const UNSETTLED: &str = "unsettled";

/// Windower's settings.xml by what it says (`ffxi_settings::settings_digest`), not its mtime:
/// every Windower Play ends with Windower's exit save, which rewrites the file unchanged and
/// must not re-park every sibling account that shares the folder.
fn settings_xml(path: &Path) -> String {
    match std::fs::metadata(path) {
        Err(_) => "missing".into(),
        Ok(meta) if meta.len() > 4 * 1024 * 1024 => stamp(path),
        Ok(_) => std::fs::read(path).ok().and_then(|bytes| super::ffxi_settings::settings_digest(&bytes))
            .map_or_else(|| UNSETTLED.into(), |digest| format!("{digest:016x}")),
    }
}

/// The fingerprint of `plan` for `config` (07d §1.5): stat calls and the small settings reads
/// the park line itself needs. No network, nothing secret.
pub(crate) fn fingerprint(config: &Config, plan: &super::Plan) -> Fingerprint {
    let install = super::install_directory(config);
    let pol = crate::discovery::find(crate::discovery::Product::PlayOnline, None);
    let mut parts = vec![
        ("game folder", format!("{install:?}")),
        ("PlayOnline folder", format!("{pol:?}")),
        ("auto-login", format!("{:?}", plan.auto_login.normalized())),
        ("launch tools", serde_json::to_string(&plan.settings).unwrap_or_default()),
        ("loader", super::tools::loader_path(&plan.settings.loader).map_or_else(|| "missing".into(), |loader| format!("{} {}", loader.display(), stamp(&loader)))),
        ("FFXI patch.ver", install.as_ref().map_or_else(|_| "missing".into(), |dir| stamp(&dir.join("patch.ver")))),
        ("Viewer patch.ver", pol.as_ref().map_or_else(|| "missing".into(), |dir| stamp(&dir.join("patch.ver")))),
        ("polcore.dll", pol.as_ref().map_or_else(|| "missing".into(), |dir| stamp(&dir.join(r"viewer\com\polcore.dll")))),
    ];
    match plan.tool {
        Tool::Windower => {
            let folder = super::tools::detect(Tool::Windower, &plan.settings.windower);
            parts.push(("settings.xml", folder.as_ref().map_or_else(|| "missing".into(), |dir| settings_xml(&dir.join("settings.xml")))));
            // Windower's own update records change only while it starts: not a reason to re-park.
            let files = folder.as_deref().map(super::tools::windower_files).unwrap_or_default();
            for name in ["Hook.dll", "Windower.exe"] {
                parts.push((if name == "Hook.dll" { "Hook.dll" } else { "Windower.exe" },
                    format!("{:?}", files.iter().find(|(file, _)| file == name).and_then(|(_, stamp)| *stamp))));
            }
            parts.push(("Windower plugins", format!("{:?}", files.iter().filter(|(file, _)| file.starts_with("plugins\\")).collect::<Vec<_>>())));
        }
        Tool::Ashita => {
            // The base boot ini each Ashita start copies (pol_less prepare): Ashita's profile.
            let name = Some(plan.settings.boot_ini.trim()).filter(|name| !name.is_empty()).unwrap_or(super::tools::DEFAULT_BOOT_INI);
            let ini = super::tools::detect(Tool::Ashita, &plan.settings.ashita).map(|dir| dir.join("config").join("boot").join(name));
            parts.push(("Ashita boot ini", ini.as_deref().map_or_else(|| "missing".into(), stamp)));
            parts.push(("menu size", crate::game_config::retail_menu(config)
                .map_or_else(|_| UNSETTLED.into(), |size| format!("{size:?}"))));
        }
        Tool::Vanilla => parts.push(("menu size", crate::game_config::retail_menu(config)
            .map_or_else(|_| UNSETTLED.into(), |size| format!("{size:?}")))),
    }
    Fingerprint { parts }
}

/// The `park` line (`\n`-terminated): no credentials, keys, records or per-Play flags.
/// Field order is fixed; crystal-loader proto.rs tests parse this exact shape.
pub(crate) fn park_line(install_dir: &Path, pol_install_dir: Option<&Path>, auto_login: AutoLogin,
    ffxi_settings_json: Option<&str>) -> Result<Zeroizing<Vec<u8>>, String> {
    let install = install_dir.to_str().ok_or("The game folder's path is not valid Unicode.")?;
    let failed = |_| "Could not encode the park line.".to_owned();
    let pol = serde_json::to_string(&pol_install_dir.map(|p| p.to_string_lossy().into_owned())).map_err(failed)?;
    let line = format!("{{\"type\":\"park\",\"v\":{},\"region\":\"US\",\"lang\":\"US\",\"pol_install_dir\":{pol},\"install_dir\":{},\"auto_login\":{}}}\n",
        super::pipe::PROTOCOL, serde_json::to_string(install).map_err(failed)?, serde_json::to_string(&auto_login.normalized()).map_err(failed)?);
    let line = Zeroizing::new(line.into_bytes());
    match ffxi_settings_json {
        Some(json) => super::ffxi_settings::splice(line, json),
        None => Ok(line),
    }
}

// ---- the host side, in the park worker (`--session-worker --park`, 07d §1.4) ----

/// What `Event::Parked` reports: the loader and tool processes and how long the park took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ParkedInfo {
    pub loader_pid: u32,
    /// Windower.exe, Ashita-cli or (Vanilla) the host itself; `None` when not ours to watch.
    pub tool_pid: Option<u32>,
    pub polcore_ms: Option<u64>,
    /// Park start → verified hello.
    pub hello_ms: u64,
}

/// A parked host owned by this park worker: the tool started, the loader verified, `park`
/// sent and `parked` received. Nothing reached Square Enix and no credential left the worker.
pub(crate) struct Parked {
    p: Prepared,
    log: super::SessionLog,
    host: super::Host,
    /// The loader by handle, so `kill` never reaches a process that reused its PID.
    process: LoaderProcess,
    since: Instant,
    limits: super::Timeouts,
    pub(crate) info: ParkedInfo,
}

/// Park `account`'s host for `plan` (07d §1.4 steps 1-4): prepare as a launch does (the
/// Windower profile guard included), dgVoodoo, tool and hello under the tool's turn, then the
/// `park` line and `parked` within PARKED_LIMIT. `account` carries no passwords. `abort` is
/// set when the window closes the park meanwhile (its stdin ended): the park stops at the
/// next step, never leaving Windower running without its worker.
pub(crate) fn park(config: &Config, plan: &super::Plan, account: &Account, abort: &'static std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(&str)) -> Result<Parked, String> {
    let mut timing = Timeline::begin_at(Instant::now());
    let p = super::prepare_with_progress(config, plan, account, &mut timing, progress)?;
    if let Some(line) = crate::dgvoodoo::sync_for_launch(&p.loader, plan.settings.dgvoodoo) {
        progress(&line);
    }
    let profile = ParkKey::for_plan(account, plan).profile;
    park_prepared(p, &profile, plan.auto_login, timing, progress, &mut super::spawn_park_injector, &super::TURNS,
        super::Timeouts { abort: Some(abort), ..super::TIMEOUTS }, PARKED_LIMIT)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn park_prepared(mut p: Prepared, profile: &str, auto_login: AutoLogin, mut timing: Timeline, progress: &mut dyn FnMut(&str),
    spawn: &mut super::Spawner<'_>, turns: &'static super::Turns, limits: super::Timeouts, parked_limit: Duration) -> Result<Parked, String> {
    let started = Instant::now();
    let mut log = super::open_launch_log(&p, &mut timing, Some(&format!("park start: {} {profile}", p.tool.name())))?;
    let log_path = p.session.log.clone();
    let host = match super::start_host(
        &mut p,
        &mut log,
        &mut timing,
        progress,
        spawn,
        turns,
        limits,
    ) {
        Ok(host) => host,
        Err(error) if error == UNSUPPORTED => return Err(error),
        Err(error) => return Err(super::fail(&mut log, &log_path, error)),
    };
    let tool_pid = match &host.injector.injector { super::Injector::Child(child) => Some(child.id()), _ => None };
    let supported = host.pipe.supports_park() && host.pipe.supports_native_auth() && host.pipe.supports_native_social() && host.pipe.supports_viewer_check();
    let info = ParkedInfo { loader_pid: host.pid, tool_pid, polcore_ms: None, hello_ms: started.elapsed().as_millis() as u64 };
    let process = LoaderProcess::open(host.pid);
    let mut parked = Parked { p, log, host, process, since: started, limits, info };
    if limits.aborted() {
        parked.log.line(ABORTED);
        parked.kill();
        return Err(ABORTED.into());
    }
    if !supported {
        parked.log.line("This crystal-loader cannot park a host (its hello has no park capability); closing it.");
        parked.kill();
        return Err(UNSUPPORTED.into());
    }
    parked.log.line(&format!("crystal-loader connected: PID {}, image verified.", parked.info.loader_pid));
    parked.log.reached(&mut timing, Milestone::Hello);
    let pol = crate::discovery::find(crate::discovery::Product::PlayOnline, None);
    let line = park_line(&parked.p.install_dir, pol.as_deref(), auto_login, parked.p.ffxi_settings.as_ref().map(|settings| settings.json.as_str()));
    if let Some(settings) = &parked.p.ffxi_settings { super::note_ffxi_settings(&parked.p, settings, &mut parked.log); }
    let sent = line.and_then(|line| parked.host.pipe.send(&line, Instant::now() + limits.send)
        .map_err(|_| t("crystal-loader did not accept the park line.").to_owned()));
    if let Err(error) = sent {
        parked.log.line(&error);
        parked.kill();
        return Err(error);
    }
    parked.log.line("Park line sent (no credentials); waiting for polcore.");
    let deadline = Instant::now() + parked_limit;
    let ended = loop {
        if limits.aborted() { break ABORTED.into(); }
        // In slices, so a park the window closed meanwhile stops within ABORT_POLL.
        let slice = if limits.abort.is_some() { deadline.min(Instant::now() + ABORT_POLL) } else { deadline };
        match parked.host.pipe.next_event(Some(slice)) {
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut && slice < deadline => {}
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => break tf("The game host did not park within {seconds} s.", &[("seconds", &parked_limit.as_secs())]),
            Err(_) => break t("The parked game host ended.").into(),
            Ok(Err(message)) => parked.log.line(&message),
            Ok(Ok(LoaderEvent::Parked { polcore_ms })) => {
                parked.info.polcore_ms = polcore_ms;
                parked.since = Instant::now();
                parked.log.line(&format!("Host parked after {:.2}s (polcore {}); waiting for Play with no timeout.",
                    started.elapsed().as_secs_f64(), polcore_ms.map_or_else(|| "time unknown".into(), |ms| format!("{ms} ms"))));
                return Ok(parked);
            }
            Ok(Ok(LoaderEvent::Error(message))) => break message,
            Ok(Ok(LoaderEvent::Closed | LoaderEvent::Exit(_))) => break t("The game host ended before it parked.").into(),
            Ok(Ok(_)) => {}
        }
    };
    parked.log.line(&ended);
    parked.kill();
    Err(ended)
}

impl Parked {
    /// While parked: `Some(why)` once the host is gone (the pipe closed, an error or an exit),
    /// `None` when nothing happened within `wait`.
    pub(crate) fn watch(&mut self, wait: Duration) -> Option<String> {
        match self.host.pipe.next_event(Some(Instant::now() + wait)) {
            Err(error) if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => None,
            Err(_) | Ok(Ok(LoaderEvent::Closed | LoaderEvent::Exit(_))) => Some(t("The parked game host ended.").into()),
            Ok(Ok(LoaderEvent::Error(message))) => {
                self.log.line(&format!("Loader reported an error: {message}"));
                Some(message)
            }
            Ok(Err(message)) => { self.log.line(&message); None }
            Ok(Ok(_)) => None,
        }
    }

    /// No Play is coming (the window closed the park, 07d §1.4 "EOF before frame 2"): end
    /// Windower.exe first, so its `Core.Stop(save: true)` never rewrites settings.xml (07a),
    /// then the loader. A loader that cannot be terminated (an elevated Ashita host) exits
    /// on its own when its pipe closes. The profile guard is released last.
    pub(crate) fn kill(self) {
        let Parked { mut log, host, process, .. } = self;
        let super::Host { pipe, mut injector, .. } = host;
        log.line("The park ended before Play; closing Windower and the parked host. Nothing was sent.");
        injector.injector.stop();
        if !process.terminate() { log.line("The parked host could not be terminated; its pipe closes instead."); }
        drop(pipe);
        drop(injector);
    }

    /// Play (07d §1.4 frame 2, §5.2): the per-Play flags and credentials go to the parked
    /// host, then the session goes on exactly as a cold one. A host lost before the
    /// credentials went out never got them: `cold` runs today's cold launch instead.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn play(mut self, config: &Config, plan: &super::Plan, account: Account, otp: Zeroizing<String>, clicked: Instant,
        progress: &mut dyn FnMut(&str), cold: impl FnOnce(Account, Zeroizing<String>, &mut dyn FnMut(&str)) -> Result<LaunchResult, String>) -> Result<LaunchResult, String> {
        // A Play that fails before its credentials go out ends the park like an EOF would.
        let refuse = |parked: Parked, error: String| { parked.kill(); Err(error) };
        if account.region != 0 {
            return refuse(self, t("Retail login currently supports the verified US install only. EU and JP protocol region values are not verified yet.").into());
        }
        crate::patch::warm_dns();
        crate::dns::warm(crate::dns::POL_LOGIN_HOST);
        let fallback = (account.clone(), otp.clone());
        let mut auth = match super::native_auth::NativeAuth::new(&account, otp, config.account_directory(&account)) {
            Ok(auth) => auth,
            Err(error) => return refuse(self, error),
        };
        super::configure(&mut auth, plan);
        drop(account);
        let mut timing = Timeline::begin_at(clicked);
        self.p.trigger = plan.trigger;
        super::clock_lines(&mut self.log, &timing, plan.trigger);
        self.log.line(&format!("parked for {:.1} s", self.since.elapsed().as_secs_f64()));
        let lost = |parked: Parked, why: &str, progress: &mut dyn FnMut(&str)| {
            let mut parked = parked;
            parked.log.line(&format!("parked host lost before Play ({why}); starting cold"));
            parked.kill();
            cold(fallback.0, fallback.1, progress)
        };
        if let Some(why) = self.watch(Duration::ZERO) {
            drop(auth);
            return lost(self, &why, progress);
        }
        // dgVoodoo may have been installed since the park (a cold Play syncs at its start):
        // FFXiMain loads d3d8.dll beside the loader only at GameStart, so placing it now counts.
        if let Some(line) = crate::dgvoodoo::sync_for_launch(&self.p.loader, plan.settings.dgvoodoo) {
            self.log.line(&line);
        }
        let supervision = match super::supervise(&self.p, auth) {
            Ok(supervision) => supervision,
            Err(error) => return refuse(self, error),
        };
        let line = match supervision.auth.credentials_line() {
            Ok(line) => line,
            Err(error) => { drop(supervision); return refuse(self, error); }
        };
        progress(t("Handing the session to the game..."));
        super::publish_batch_capability(&self.p, self.host.pipe.supports_native_batch(), &mut self.log);
        let sent = self.host.pipe.send(&line, Instant::now() + self.limits.send);
        drop(line);
        if sent.is_err() {
            drop(supervision);
            return lost(self, "the pipe did not take the credentials", progress);
        }
        self.log.line("Credentials sent; launcher copy cleared.");
        self.log.reached(&mut timing, Milestone::CredentialsSent);
        let Parked { p, mut log, host, limits, .. } = self;
        let super::Host { pipe, pid, injector } = host;
        if p.tool == Tool::Windower {
            // Windower.exe exits once the game window shows; keep the profile guard until then.
            // The credentials went out, so the session goes on whatever happens here.
            let now = Instant::now();
            if let Err(error) = super::observe_windower_exit(injector, now, now + limits.injector_exit, p.session.log.clone()) {
                log.line(&error);
            }
        } else {
            drop(injector);
        }
        super::finish(supervision, log, pipe, pid, timing, progress, limits)
    }
}

/// The parked loader by handle (PROCESS_TERMINATE | SYNCHRONIZE), opened right after its
/// verified hello while it holds our pipe: a later kill can never reach a process that reused
/// its PID. Empty for this process (the tests' fake loaders say hello with the test's own PID)
/// or when it cannot be opened (an elevated Ashita host).
struct LoaderProcess {
    #[cfg(windows)]
    handle: Option<std::os::windows::io::OwnedHandle>,
}
impl LoaderProcess {
    fn open(pid: u32) -> LoaderProcess {
        #[cfg(windows)]
        {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
            let raw = if pid == std::process::id() { std::ptr::null_mut() } else { unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) } };
            LoaderProcess { handle: (!raw.is_null()).then(|| unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw as _) }) }
        }
        #[cfg(not(windows))]
        { let _ = pid; LoaderProcess {} }
    }

    /// TerminateProcess through the handle, unless the host already ended. False without a
    /// handle or when it would not end.
    fn terminate(self) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::{TerminateProcess, WaitForSingleObject}};
            let Some(handle) = self.handle else { return false };
            let raw = handle.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            unsafe { WaitForSingleObject(raw, 0) == WAIT_OBJECT_0 || TerminateProcess(raw, 1) != 0 }
        }
        #[cfg(not(windows))]
        false
    }
}

// ---- the window's pool (07d §1.5) ----

/// What a park worker reported since the last poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    Parked(ParkedInfo),
    Failed(String),
    /// The worker process ended without a report.
    Ended,
}

/// A park worker as the pool sees it (`session_worker::ParkWorker`; fakes in tests).
pub(crate) trait Worker: Send + 'static {
    fn poll(&mut self) -> Option<Note>;
    /// Close its stdin: a worker that never got frame 2 kills Windower, then the host.
    fn release(&mut self);
    fn exited(&mut self) -> bool;
    fn terminate(&mut self);
    /// It reported `Parked` (polled here or by the pool).
    fn parked(&self) -> bool;
}

/// Close parked workers: release them all, wait up to CLOSE_WAIT for them to clean up,
/// then terminate any parked one still running. A worker still parking is never terminated:
/// Windower.exe may already run as its child, and only the worker stops it before the
/// loader (07a: else `Core.Stop(save: true)` rewrites settings.xml). Its stdin ended, so it
/// aborts at its next step (`ABORTED`) and exits on its own.
pub(crate) fn close_workers<W: Worker>(mut workers: Vec<W>) {
    for worker in &mut workers { worker.release(); }
    let deadline = Instant::now() + CLOSE_WAIT;
    while workers.iter_mut().any(|worker| !worker.exited()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    for worker in &mut workers {
        while worker.poll().is_some() {}
        if worker.parked() && !worker.exited() { worker.terminate(); }
    }
}

struct Slot<W> {
    /// The account row that owns the key.
    row: String,
    fingerprint: Fingerprint,
    worker: W,
    parked: bool,
}

/// A key the window would have parked, with its row and current fingerprint.
pub(crate) struct Wanted<P> {
    pub key: ParkKey,
    pub row: String,
    pub fingerprint: Fingerprint,
    /// What starting it needs (the window's config, account and plan).
    pub payload: P,
}

/// The window's parked workers, one per key (07d §1.5). Parks start one at a time, each
/// only after its key has been wanted for PARK_START_DELAY; a key stops re-parking after
/// PARK_FAILURE_LIMIT failures in a row, or after an unsupported loader, until its
/// fingerprint changes.
pub(crate) struct Pool<W> {
    slots: BTreeMap<ParkKey, Slot<W>>,
    failures: BTreeMap<ParkKey, (u32, Fingerprint)>,
    unsupported: BTreeMap<ParkKey, Fingerprint>,
    wanted_since: BTreeMap<ParkKey, Instant>,
    /// When the last parked delivery may go (07d §1.5 stagger), shared with launch threads.
    pub(crate) deliveries: Arc<Mutex<Option<Instant>>>,
}

impl<W> Default for Pool<W> {
    fn default() -> Self {
        Self { slots: BTreeMap::new(), failures: BTreeMap::new(), unsupported: BTreeMap::new(), wanted_since: BTreeMap::new(),
            deliveries: Arc::new(Mutex::new(None)) }
    }
}

impl<W: Worker> Pool<W> {
    /// No parked (or parking) host.
    pub(crate) fn is_empty(&self) -> bool { self.slots.is_empty() }

    /// Reports since the last poll, as (key, row, note). A failure or a worker that ended
    /// removes its slot and counts against the key.
    pub(crate) fn poll(&mut self) -> Vec<(ParkKey, String, Note)> {
        let mut notes = Vec::new();
        for (key, slot) in &mut self.slots {
            while let Some(note) = slot.worker.poll() {
                if matches!(note, Note::Parked(_)) { slot.parked = true; }
                let done = !matches!(note, Note::Parked(_));
                notes.push((key.clone(), slot.row.clone(), note));
                if done { break; }
            }
        }
        for (key, _, note) in &notes {
            let Some(fingerprint) = self.slots.get(key).map(|slot| slot.fingerprint.clone()) else { continue };
            match note {
                Note::Parked(_) => { self.failures.remove(key); }
                Note::Failed(why) if why == UNSUPPORTED => {
                    self.unsupported.insert(key.clone(), fingerprint);
                    self.slots.remove(key);
                }
                Note::Failed(_) | Note::Ended => {
                    let count = self.failures.get(key).filter(|(_, at)| *at == fingerprint).map_or(0, |(count, _)| *count);
                    self.failures.insert(key.clone(), (count + 1, fingerprint));
                    self.slots.remove(key);
                }
            }
        }
        notes
    }

    /// Workers to close: keys no longer wanted, and keys whose fingerprint changed (with
    /// the names that changed, for `re-park: <names> changed`). Never a worker that was
    /// adopted: `take` already removed it.
    pub(crate) fn retire<P>(&mut self, wanted: &[Wanted<P>]) -> Vec<(ParkKey, Vec<&'static str>, W)> {
        let mut gone = Vec::new();
        let keys: Vec<ParkKey> = self.slots.keys().cloned().collect();
        for key in keys {
            let changed = match wanted.iter().find(|want| want.key == key) {
                None => Some(Vec::new()),
                Some(want) => {
                    let slot = &self.slots[&key];
                    if want.row != slot.row { Some(Vec::new()) }
                    else { Some(slot.fingerprint.changed(&want.fingerprint)).filter(|names| !names.is_empty()) }
                }
            };
            if let Some(names) = changed {
                let slot = self.slots.remove(&key).expect("listed");
                gone.push((key, names, slot.worker));
            }
        }
        self.wanted_since.retain(|key, _| wanted.iter().any(|want| want.key == *key));
        gone
    }

    /// The next key to park, if any may start now: none while another park is starting,
    /// none before its key was wanted for PARK_START_DELAY, none for a blocked key.
    pub(crate) fn next<'a, P>(&mut self, wanted: &'a [Wanted<P>], now: Instant) -> Option<&'a Wanted<P>> {
        for want in wanted {
            self.wanted_since.entry(want.key.clone()).or_insert(now);
        }
        if self.slots.values().any(|slot| !slot.parked) { return None; }
        wanted.iter().find(|want| {
            !self.slots.contains_key(&want.key)
                && self.wanted_since.get(&want.key).is_some_and(|since| now.saturating_duration_since(*since) >= PARK_START_DELAY)
                && self.unsupported.get(&want.key).is_none_or(|at| *at != want.fingerprint)
                && self.failures.get(&want.key).is_none_or(|(count, at)| *count < PARK_FAILURE_LIMIT || *at != want.fingerprint)
        })
    }

    pub(crate) fn insert(&mut self, key: ParkKey, row: String, fingerprint: Fingerprint, worker: W) {
        self.wanted_since.remove(&key);
        self.slots.insert(key, Slot { row, fingerprint, worker, parked: false });
    }

    /// Remove the row's worker for intentional closing, without adopting it for a Play.
    pub(crate) fn take(&mut self, row: &str, key: &ParkKey) -> Option<W> {
        if self.slots.get(key).is_some_and(|slot| slot.row == row) { self.slots.remove(key).map(|slot| slot.worker) } else { None }
    }

    /// Adopt only against a fingerprint read at this Play, without waiting for periodic
    /// retirement. An unsettled settings file cannot prove the host still matches.
    /// `Err(worker)` removes the stale host for closing off the interface thread; it must
    /// receive no credentials. A still-parking match waits for `Parked` before delivery.
    pub(crate) fn take_for_play(&mut self, row: &str, key: &ParkKey, current: impl FnOnce() -> Fingerprint) -> Result<Option<W>, W> {
        let Some(slot) = self.slots.get(key).filter(|slot| slot.row == row) else { return Ok(None) };
        let current = current();
        let matches = slot.fingerprint == current && !current.parts.iter().any(|(_, value)| value == UNSETTLED);
        let worker = self.slots.remove(key).expect("checked").worker;
        if matches { Ok(Some(worker)) } else { Err(worker) }
    }

    /// Every worker, for closing (exit, launch_blocked, profile editor).
    pub(crate) fn drain(&mut self, tool: Option<&'static str>) -> Vec<W> {
        let keys: Vec<ParkKey> = self.slots.keys().filter(|key| tool.is_none_or(|tool| key.tool == tool)).cloned().collect();
        keys.into_iter().filter_map(|key| self.slots.remove(&key).map(|slot| slot.worker)).collect()
    }

    /// Forget failures, so the next park of every key may start (a restart of Crystal, in
    /// effect); kept for completeness of the "or Crystal restarts" rule.
    #[cfg(test)]
    fn forget_failures(&mut self) { self.failures.clear(); self.unsupported.clear(); }
}

/// How long the next parked delivery must wait, reserving its slot: the first goes at once,
/// each later one ≥ CREDENTIAL_STAGGER + `jitter` after the previous (07a parity condition 3).
pub(crate) fn reserve_delivery(last: &Mutex<Option<Instant>>, now: Instant, jitter: Duration) -> Duration {
    let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
    let at = match *last {
        Some(previous) => (previous + CREDENTIAL_STAGGER + jitter.min(CREDENTIAL_JITTER)).max(now),
        None => now,
    };
    *last = Some(at);
    at.saturating_duration_since(now)
}

/// A uniformly random 0..=CREDENTIAL_JITTER (whole milliseconds).
pub(crate) fn jitter() -> Duration {
    let bits = super::pipe::random_hex().ok().and_then(|hex| u64::from_str_radix(&hex[..16.min(hex.len())], 16).ok()).unwrap_or(0);
    Duration::from_millis(bits % (CREDENTIAL_JITTER.as_millis() as u64 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_park_line_carries_host_fields_only_in_the_loaders_shape() {
        let line = park_line(Path::new(r"C:\Games\SquareEnix\FINAL FANTASY XI"), Some(Path::new(r"C:\Games\PlayOnlineViewer")),
            AutoLogin { enabled: true, character_slot: 3 }, Some(r#"{"0001":1920,"padmode000":"0,0,0,0,1,0"}"#)).unwrap();
        // crystal-loader proto.rs tests::PARK_LINE, byte for byte.
        assert_eq!(std::str::from_utf8(&line).unwrap(), concat!(r#"{"type":"park","v":1,"region":"US","lang":"US","pol_install_dir":"C:\\Games\\PlayOnlineViewer","install_dir":"C:\\Games\\SquareEnix\\FINAL FANTASY XI","auto_login":{"enabled":true,"character_slot":3},"ffxi_settings":{"0001":1920,"padmode000":"0,0,0,0,1,0"}}"#, "\n"));
        let value: serde_json::Value = serde_json::from_slice(&park_line(Path::new(r"C:\x"), None, AutoLogin::default(), None).unwrap()).unwrap();
        assert_eq!(value["pol_install_dir"], serde_json::Value::Null);
        for secret in ["native_auth", "v_key", "auth_code", "account_id", "pol_records", "content_ids", "start_game", "viewer_gaps", "auto_away_minutes"] {
            assert!(value.get(secret).is_none(), "{secret}");
        }
    }

    #[test]
    fn a_fingerprint_names_what_changed() {
        let before = Fingerprint { parts: vec![("settings.xml", "1".into()), ("Hook.dll", "a".into())] };
        let after = Fingerprint { parts: vec![("settings.xml", "2".into()), ("Hook.dll", "a".into()), ("polcore.dll", "x".into())] };
        assert_eq!(before.changed(&after), vec!["settings.xml", "polcore.dll"]);
        assert!(after.changed(&after).is_empty());
    }

    /// 07d §1.5: settings.xml, a plugin and the loader change the fingerprint; Windower's own
    /// update records do not.
    #[test]
    fn the_fingerprint_follows_the_files_a_park_depends_on() {
        let root = std::env::temp_dir().join(format!("crystal-park-print-{}", crate::backend::unique_id().unwrap()));
        let windower = root.join("Windower");
        std::fs::create_dir_all(windower.join("plugins")).unwrap();
        std::fs::create_dir_all(windower.join("updates")).unwrap();
        for file in ["Windower.exe", "Hook.dll", r"plugins\Timestamp.dll", r"updates\manifest.xml"] {
            std::fs::write(windower.join(file), b"v1").unwrap();
        }
        std::fs::write(windower.join("settings.xml"), SETTINGS).unwrap();
        let loader = root.join("pol.exe");
        std::fs::write(&loader, b"loader").unwrap();
        let config = Config { experiment: root.clone(), repository: root.clone(), game_install: Some(root.join("missing-game")), dat_overlays: Vec::new() };
        let plan = super::super::Plan { tool: Tool::Windower, social_only: false, invisible_login: false, auto_away_minutes: 10,
            auto_login: AutoLogin::default(), viewer_gaps: false, trigger: Default::default(),
            settings: super::super::ToolSettings { windower: windower.to_string_lossy().into_owned(), loader: loader.to_string_lossy().into_owned(), ..Default::default() } };
        let before = fingerprint(&config, &plan);
        assert_eq!(before, fingerprint(&config, &plan));
        std::fs::write(windower.join(r"updates\manifest.xml"), b"v2 updated").unwrap();
        assert!(before.changed(&fingerprint(&config, &plan)).is_empty(), "update records only change at a start");
        // Windower's exit save rewrites settings.xml as it read it (new mtime, maybe new
        // layout): not an edit, so siblings sharing the folder stay parked. A save caught
        // half-written is no change yet either.
        let before = fingerprint(&config, &plan);
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(windower.join("settings.xml"), SETTINGS.replace("\n    ", "\r\n\t").replace("<settings>", "<?xml version=\"1.0\"?>\r\n<settings>")).unwrap();
        assert!(before.changed(&fingerprint(&config, &plan)).is_empty(), "Windower's exit save is no edit");
        std::fs::write(windower.join("settings.xml"), &SETTINGS[..SETTINGS.len() / 2]).unwrap();
        assert!(before.changed(&fingerprint(&config, &plan)).is_empty(), "a save in progress is no change yet");
        for (file, contents, name) in [(windower.join("settings.xml"), SETTINGS.replace("1920", "2560"), "settings.xml"),
            (windower.join(r"plugins\Timestamp.dll"), "changed contents".into(), "Windower plugins"), (loader.clone(), "changed contents".into(), "loader")] {
            let previous = fingerprint(&config, &plan);
            std::fs::write(&file, contents).unwrap();
            assert_eq!(previous.changed(&fingerprint(&config, &plan)), vec![name]);
        }
        let mut other = plan.clone();
        other.auto_login.enabled = true;
        assert_eq!(fingerprint(&config, &plan).changed(&fingerprint(&config, &other)), vec!["auto-login"]);
        // Ashita: an edit to the base boot ini (Ashita's profile) re-parks too.
        let ashita = root.join("Ashita");
        std::fs::create_dir_all(ashita.join(r"config\boot")).unwrap();
        std::fs::write(ashita.join("Ashita-cli.exe"), b"cli").unwrap();
        std::fs::write(ashita.join(r"config\boot\main.ini"), b"[ashita.boot]\nfile = old\n").unwrap();
        let mut ashita_plan = plan.clone();
        ashita_plan.tool = Tool::Ashita;
        ashita_plan.settings.ashita = ashita.to_string_lossy().into_owned();
        ashita_plan.settings.boot_ini = "main.ini".into();
        let previous = fingerprint(&config, &ashita_plan);
        assert!(previous.parts.iter().any(|(name, value)| *name == "Ashita boot ini" && value != "missing"), "{previous:?}");
        std::fs::write(ashita.join(r"config\boot\main.ini"), b"[ashita.boot]\nfile = new, longer\n").unwrap();
        assert_eq!(previous.changed(&fingerprint(&config, &ashita_plan)), vec!["Ashita boot ini"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    const SETTINGS: &str = "<settings>\n    <profile name=\"\">\n    <width>1920</width>\n    <height>1080</height>\n    </profile>\n    <profile name=\"Main\">\n    <windowmode>Window</windowmode>\n    </profile>\n</settings>";

    /// 07d §1.5 stagger: deliveries ≥ 1.5 s (+ jitter) apart, the first at once.
    #[test]
    fn parked_deliveries_are_staggered() {
        let last = Mutex::new(None);
        let now = Instant::now();
        assert_eq!(reserve_delivery(&last, now, Duration::from_millis(200)), Duration::ZERO);
        assert_eq!(reserve_delivery(&last, now, Duration::from_millis(200)), Duration::from_millis(1700));
        // A third right away waits behind the second; the jitter never exceeds its bound.
        assert_eq!(reserve_delivery(&last, now + Duration::from_millis(100), Duration::from_secs(9)), Duration::from_millis(1700 + 2000 - 100));
        // Long after, no wait.
        assert_eq!(reserve_delivery(&last, now + Duration::from_secs(60), Duration::ZERO), Duration::ZERO);
        for _ in 0..50 { assert!(jitter() <= CREDENTIAL_JITTER); }
    }

    #[derive(Default)]
    struct Fake { notes: Vec<Note>, released: Arc<Mutex<Vec<String>>>, name: String, done: bool,
        /// Ignores its released stdin for now (a worker still in `park`, or a hung one).
        busy: bool, parked: bool, terminated: bool }
    impl Worker for Fake {
        fn poll(&mut self) -> Option<Note> {
            let note = (!self.notes.is_empty()).then(|| self.notes.remove(0));
            if matches!(note, Some(Note::Parked(_))) { self.parked = true; }
            note
        }
        fn release(&mut self) { self.released.lock().unwrap().push(self.name.clone()); self.done = !self.busy; }
        fn exited(&mut self) -> bool { self.done }
        fn terminate(&mut self) { self.done = true; self.terminated = true; }
        fn parked(&self) -> bool { self.parked }
    }

    /// Finding: closing a park still starting must never TerminateProcess its worker, which
    /// would orphan Windower.exe (and its settings.xml save); a parked one that hangs is.
    #[test]
    fn closing_never_terminates_a_worker_that_is_still_parking() {
        /// Records which workers were terminated.
        struct Watch(Fake, Arc<Mutex<Vec<String>>>);
        impl Worker for Watch {
            fn poll(&mut self) -> Option<Note> { self.0.poll() }
            fn release(&mut self) { self.0.release() }
            fn exited(&mut self) -> bool { self.0.exited() }
            fn terminate(&mut self) { self.1.lock().unwrap().push(self.0.name.clone()); self.0.terminate() }
            fn parked(&self) -> bool { self.0.parked() }
        }
        let terminated = Arc::new(Mutex::new(Vec::new()));
        let parked = Note::Parked(ParkedInfo { loader_pid: 1, tool_pid: None, polcore_ms: None, hello_ms: 0 });
        let started = Instant::now();
        close_workers(vec![
            Watch(Fake { name: "parking".into(), busy: true, ..Fake::default() }, terminated.clone()),
            Watch(Fake { name: "parked, hung".into(), busy: true, notes: vec![parked], ..Fake::default() }, terminated.clone()),
            Watch(Fake { name: "parked, clean".into(), parked: true, ..Fake::default() }, terminated.clone()),
        ]);
        assert!(started.elapsed() >= CLOSE_WAIT);
        assert_eq!(*terminated.lock().unwrap(), vec!["parked, hung".to_owned()], "a parking worker is left to abort on its own");
    }

    fn want(account: &str, print: &str) -> Wanted<()> {
        Wanted { key: ParkKey::new(account.into(), Tool::Windower, String::new()), row: account.into(),
            fingerprint: Fingerprint { parts: vec![("settings.xml", print.into())] }, payload: () }
    }

    #[test]
    fn play_rejects_changed_or_unsettled_settings_before_periodic_retirement() {
        let root = std::env::temp_dir().join(format!("crystal-park-adopt-{}", crate::backend::unique_id().unwrap()));
        let windower = root.join("Windower");
        std::fs::create_dir_all(&windower).unwrap();
        for file in ["Windower.exe", "Hook.dll"] { std::fs::write(windower.join(file), b"tool").unwrap(); }
        let loader = root.join("pol.exe");
        std::fs::write(&loader, b"loader").unwrap();
        for name in ["game-a", "game-b"] {
            let game = root.join(name);
            std::fs::create_dir_all(&game).unwrap();
            for file in ["FFXi.dll", "FFXiMain.dll"] { std::fs::write(game.join(file), b"game").unwrap(); }
        }
        let menu = root.join("iced-launcher/menu-playonlineus.json");
        std::fs::create_dir_all(menu.parent().unwrap()).unwrap();
        let config = Config { experiment: root.clone(), repository: root.clone(), game_install: Some(root.join("game-a")), dat_overlays: Vec::new() };
        let plan = super::super::Plan { tool: Tool::Windower, social_only: false, invisible_login: false, auto_away_minutes: 10,
            auto_login: AutoLogin::default(), viewer_gaps: false, trigger: Default::default(),
            settings: super::super::ToolSettings { windower: windower.to_string_lossy().into_owned(), loader: loader.to_string_lossy().into_owned(), ..Default::default() } };

        for change in ["game folder", "settings.xml", "menu size", "unsettled XML", "unsettled menu"] {
            std::fs::write(windower.join("settings.xml"), SETTINGS).unwrap();
            std::fs::write(&menu, b"[1600,900]").unwrap();
            let mut plan = plan.clone();
            if change.contains("menu") { plan.tool = Tool::Vanilla; }
            let before = fingerprint(&config, &plan);
            let key = ParkKey::new("edited".into(), plan.tool, String::new());
            let other = ParkKey::new("other".into(), plan.tool, String::new());
            let released = Arc::new(Mutex::new(Vec::new()));
            let mut pool = Pool::default();
            for (key, row) in [(&key, "edited"), (&other, "other")] {
                pool.insert(key.clone(), row.into(), before.clone(), Fake { name: row.into(), released: released.clone(), parked: true, ..Fake::default() });
            }
            assert!(matches!(pool.take_for_play("other", &key, || panic!("no file reads for another row")), Ok(None)), "only the owning row");
            let mut current_config = config.clone();
            match change {
                "game folder" => current_config.game_install = Some(root.join("game-b")),
                "settings.xml" => std::fs::write(windower.join("settings.xml"), SETTINGS.replace("1920", "2560")).unwrap(),
                "menu size" => std::fs::write(&menu, b"[1280,720]").unwrap(),
                "unsettled XML" => std::fs::write(windower.join("settings.xml"), &SETTINGS[..SETTINGS.len() / 2]).unwrap(),
                "unsettled menu" => std::fs::write(&menu, b"[1280").unwrap(),
                _ => unreachable!(),
            }
            // No poll or retire has run since the edit. The Play's fresh read must refuse
            // adoption, even though periodic retirement deliberately waits for XML saves.
            let current = fingerprint(&current_config, &plan);
            let stale = pool.take_for_play("edited", &key, || current).err().expect(change);
            assert_eq!(stale.name, "edited");
            assert!(pool.take("edited", &key).is_none(), "the stale host is removed");
            close_workers(vec![stale]);
            assert_eq!(*released.lock().unwrap(), vec!["edited".to_owned()]);
            let unchanged = pool.take_for_play("other", &other, || before).ok().flatten().expect("other parks stay untouched");
            assert_eq!(unchanged.name, "other");
        }
        // Matching two unsettled snapshots must also fail; equality is not proof of readiness.
        let key = ParkKey::new("unsettled".into(), Tool::Windower, String::new());
        let unsettled = Fingerprint { parts: vec![("settings.xml", UNSETTLED.into())] };
        let mut pool = Pool::default();
        pool.insert(key.clone(), "unsettled".into(), unsettled.clone(), Fake::default());
        assert!(pool.take_for_play("unsettled", &key, || unsettled).is_err());
        assert!(matches!(pool.take_for_play("unsettled", &key, || panic!("no file reads without a parked host")), Ok(None)));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// 07d §1.5: serial parks after the delay, one per key, adoption, re-park on a changed
    /// fingerprint only, failure limits, unsupported loaders and closing on demand.
    #[test]
    fn the_pool_parks_serially_and_follows_its_rules() {
        let mut pool: Pool<Fake> = Pool::default();
        let released = Arc::new(Mutex::new(Vec::new()));
        let fake = |name: &str, notes: Vec<Note>| Fake { notes, released: released.clone(), name: name.into(), ..Fake::default() };
        let parked = Note::Parked(ParkedInfo { loader_pid: 1, tool_pid: None, polcore_ms: Some(400), hello_ms: 1900 });
        let wanted = vec![want("A", "1"), want("B", "1")];
        let t0 = Instant::now();
        assert!(pool.next(&wanted, t0).is_none(), "not before PARK_START_DELAY");
        let first = pool.next(&wanted, t0 + PARK_START_DELAY).unwrap();
        assert_eq!(first.row, "A");
        pool.insert(first.key.clone(), first.row.clone(), first.fingerprint.clone(), fake("A", vec![]));
        assert!(pool.next(&wanted, t0 + PARK_START_DELAY).is_none(), "one park at a time");
        pool.slots.get_mut(&wanted[0].key).unwrap().worker.notes.push(parked.clone());
        assert_eq!(pool.poll().len(), 1);
        let second = pool.next(&wanted, t0 + PARK_START_DELAY).unwrap();
        assert_eq!(second.row, "B", "one per key");
        pool.insert(second.key.clone(), second.row.clone(), second.fingerprint.clone(), fake("B", vec![parked.clone()]));
        pool.poll();
        assert!(pool.next(&wanted, t0 + PARK_START_DELAY).is_none());
        // A Play adopts A's worker; the pool never closes it.
        assert!(pool.take("B", &wanted[0].key).is_none(), "only the owning row");
        let adopted = pool.take("A", &wanted[0].key).unwrap();
        assert_eq!(adopted.name, "A");
        assert!(pool.retire(&wanted[1..]).is_empty());
        // A's session ended: wanted again, re-parked after the delay.
        let later = t0 + Duration::from_secs(30);
        assert!(pool.next(&wanted, later).is_none());
        assert_eq!(pool.next(&wanted, later + PARK_START_DELAY).unwrap().row, "A");
        // B's fingerprint changed: closed, with the names for the log.
        let changed = vec![want("A", "1"), want("B", "2")];
        let gone = pool.retire(&changed);
        assert_eq!(gone.iter().map(|(key, names, _)| (key.account.as_str(), names.clone())).collect::<Vec<_>>(), vec![("B", vec!["settings.xml"])]);
        close_workers(gone.into_iter().map(|(.., worker)| worker).collect());
        assert_eq!(*released.lock().unwrap(), vec!["B".to_owned()]);
        // Failures: three in a row stop the key until its fingerprint changes.
        let mut pool: Pool<Fake> = Pool::default();
        let one = vec![want("D", "1")];
        let mut at = t0;
        let mut ready = |pool: &mut Pool<Fake>, wanted: &[Wanted<()>]| -> bool {
            at += Duration::from_secs(1);
            let _ = pool.next(wanted, at);
            at += PARK_START_DELAY;
            pool.next(wanted, at).is_some()
        };
        for attempt in 0..PARK_FAILURE_LIMIT {
            assert!(ready(&mut pool, &one), "attempt {attempt}");
            pool.insert(one[0].key.clone(), "D".into(), one[0].fingerprint.clone(),
                fake("D", vec![if attempt == 0 { Note::Ended } else { Note::Failed("tool not found".into()) }]));
            assert_eq!(pool.poll().len(), 1);
        }
        assert!(!ready(&mut pool, &one), "stopped after {PARK_FAILURE_LIMIT} failures");
        assert!(ready(&mut pool, &[want("D", "2")]), "a changed fingerprint parks again");
        // An unsupported loader is not parked again until the fingerprint changes.
        let one = vec![want("C", "1")];
        assert!(ready(&mut pool, &one));
        pool.insert(one[0].key.clone(), "C".into(), one[0].fingerprint.clone(), fake("C", vec![Note::Failed(UNSUPPORTED.into())]));
        pool.poll();
        assert!(!ready(&mut pool, &one));
        assert!(ready(&mut pool, &[want("C", "2")]));
        pool.forget_failures();
        assert!(ready(&mut pool, &one), "a restart forgets");
        // Closing on demand (exit, launch_blocked, editor) drains only what is asked.
        pool.insert(one[0].key.clone(), "C".into(), one[0].fingerprint.clone(), fake("C", vec![]));
        assert!(pool.drain(Some("Ashita")).is_empty());
        assert_eq!(pool.drain(None).len(), 1);
        assert!(pool.is_empty());
    }
}
