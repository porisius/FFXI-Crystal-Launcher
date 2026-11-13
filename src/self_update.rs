//! Signed launcher releases, downloaded separately from the game updater.
//! The release signature covers the version, URL, archive length and hash.
mod apply;
/// Restarting by itself to install a downloaded update, when nobody would notice.
pub(crate) mod auto;
pub(crate) mod model;
/// The settings card's status as plain data for the Slint front end.
pub(crate) mod front;

use crate::{
    backend::Config,
    i18n::{t, tf},
    App,
};
use crate::effect::Effect;
use reqwest::{Client, Url};
use ring::signature::{self, KeyPair};
use serde::{Deserialize, Serialize};
use crate::pol_less::tools::embedded::{hex, sha256_hex};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The GitHub repository (`owner/name`) Crystal updates itself from. Its releases must be signed
/// with the private key matching assets/update-public-key.hex (see docs/RELEASING.md). Set
/// CRYSTAL_RELEASE_REPO when building to publish from another repository.
const REPO: &str = match option_env!("CRYSTAL_RELEASE_REPO") {
    Some(repo) => repo,
    None => "porisius/FFXI-Crystal-Launcher",
};

fn index_url() -> String {
    format!("https://api.github.com/repos/{REPO}/releases?per_page=10")
}

fn release_path() -> String {
    format!("/{REPO}/releases/download/")
}

/// This fork numbers its releases `<upstream version>-lsb.<n>` (1.2.7-lsb.1, then 1.2.7-lsb.2
/// for a fix of its own, 1.2.8-lsb.1 after merging upstream's 1.2.8). Exactly that suffix is a
/// normal release; any other suffix (1.2.8-lsb.1.beta.1) is a pre-release for testers.
fn stable(version: &semver::Version) -> bool {
    let pre = version.pre.as_str();
    pre.is_empty()
        || pre
            .strip_prefix("lsb.")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
const PUBLIC_KEY: &str = include_str!("../assets/update-public-key.hex");
const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
const MAX_METADATA: usize = 512 * 1024;
const FILES: [&str; 5] = [
    "Crystal-Launcher.exe",
    "README.md",
    "LICENSE",
    "THIRD_PARTY.md",
    "THIRD_PARTY_LICENSES.txt",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    format: u32,
    platform: String,
    version: String,
    url: String,
    size: u64,
    sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Release {
    payload: Payload,
    /// `payload.version`, parsed once by verify().
    version: semver::Version,
    envelope: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct Stage {
    release: Release,
    directory: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum State {
    #[default]
    Idle,
    Checking,
    Current,
    NotPublished,
    Available(Release),
    Downloading,
    Ready(Stage),
    Applying,
    Error(String),
}

#[derive(Clone, Debug)]
pub(crate) enum Msg {
    Check,
    Checked(Result<Option<Release>, String>),
    Download,
    Downloaded(Result<Stage, String>),
    Apply,
    Applied(Result<(), String>),
    AutoCheck(bool),
    /// Settings → System: restart by itself to install a downloaded update (`auto`).
    AutoRestart(bool),
    /// "Not now" on the restart countdown (`auto::AutoRestart::postpone`).
    Postpone,
}

/// One line in Crystal's launch history (`launch_log`), never in a preview.
fn log(config: &Config, line: &str) {
    crate::launch_log::append_history(&config.history_path(), "Crystal", line, None);
}

impl Stage {
    pub(crate) fn version(&self) -> &str {
        &self.release.payload.version
    }
}

fn last_error_path(data: &Path) -> PathBuf {
    data.join("launcher-updates").join("last-error.txt")
}

pub(crate) fn initial(config: &Config) -> State {
    // Design previews read the real data folder; they must not consume or clean it.
    if std::env::args().any(|a| a.starts_with("--preview")) {
        return State::Idle;
    }
    let state = take_last_error(&config.experiment);
    let failed = matches!(state, State::Error(_));
    // A download an earlier run finished and verified is used again, never fetched twice.
    // After a failed install it is discarded: the next check starts clean.
    let kept = if failed {
        None
    } else {
        trusted_key()
            .ok()
            .and_then(|key| staged(&config.experiment, &key, &current_version()))
    };
    clean_stale(
        &config.experiment,
        std::env::current_exe().ok().as_deref(),
        failed,
        kept.as_ref().map(|stage| stage.directory.as_path()),
    );
    match kept {
        Some(stage) => {
            log(config, &format!("self-update: reusing the verified {} download", stage.version()));
            State::Ready(stage)
        }
        None => state,
    }
}

/// A download an earlier run staged, verified again from disk as if it had just arrived: the
/// metadata's signature against `key`, a version newer than `current`, and the archive's
/// length and SHA-256. The metadata is written only after the archive verified (`stage`), so
/// an interrupted download has none and fails here.
fn load_stage(directory: &Path, key: &[u8; 32], current: &semver::Version) -> Result<Stage, String> {
    let metadata = directory.join("launcher-update.json");
    if !fs::symlink_metadata(&metadata).is_ok_and(|m| m.is_file()) {
        return Err(t("Launcher update download is incomplete.").into());
    }
    let release = verify(&read_metadata(&metadata)?, key)?;
    if release.version <= *current {
        return Err(t("Launcher updates cannot install an older or identical version.").into());
    }
    verify_archive(&directory.join("release.zip"), &release)?;
    Ok(Stage { release, directory: directory.to_owned() })
}

/// The newest staged download that still verifies (`load_stage`), if any.
fn staged(data: &Path, key: &[u8; 32], current: &semver::Version) -> Option<Stage> {
    let mut best: Option<Stage> = None;
    for entry in fs::read_dir(data.join("launcher-updates")).ok()?.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        if let Ok(stage) = load_stage(&entry.path(), key, current) {
            if best.as_ref().is_none_or(|b| stage.release.version > b.release.version) {
                best = Some(stage);
            }
        }
    }
    best
}

/// Before the window or any account work: a verified update an earlier run downloaded but did
/// not install (Crystal was closed, from the window or the tray, while it waited) is installed
/// now. `true`: the helper is ready and waiting for this process to exit, so main() returns
/// and the helper starts the new version. Running games and session workers are never touched.
pub(crate) fn apply_at_start(config: &Config) -> bool {
    if std::env::args().any(|a| a.starts_with("--preview")) {
        return false;
    }
    let Ok(key) = trusted_key() else { return false };
    start_staged(config, &key, &current_version(), apply::start)
}

fn start_staged(
    config: &Config,
    key: &[u8; 32],
    current: &semver::Version,
    start: impl FnOnce(&Stage) -> Result<(), String>,
) -> bool {
    let data = &config.experiment;
    // The last install failed: this start reports it and discards the download (`initial`),
    // so a broken update is never retried at every start.
    if last_error_path(data).exists() {
        return false;
    }
    let Some(stage) = staged(data, key, current) else { return false };
    match start(&stage) {
        Ok(()) => {
            log(config, &format!("self-update: applied {} at start", stage.version()));
            true
        }
        Err(error) => {
            log(config, &format!("self-update: could not apply {} at start: {error}", stage.version()));
            let path = last_error_path(data);
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(path, error);
            false
        }
    }
}

/// A failed helper run is reported on the next start only, so later starts check again.
fn take_last_error(data: &Path) -> State {
    let path = last_error_path(data);
    let mut text = String::new();
    let Ok(file) = fs::File::open(&path) else {
        return State::Idle;
    };
    let read = file.take(32 * 1024).read_to_string(&mut text).is_ok();
    let _ = fs::remove_file(&path);
    if read && !text.is_empty() {
        State::Error(text)
    } else {
        State::Idle
    }
}

fn clear_last_error(data: &Path) {
    let _ = fs::remove_file(last_error_path(data));
}

/// Best effort: staging folders from earlier runs (all but `keep`, the verified download
/// reused), and package backups beside the exe once no failure needs them; a backup whose
/// restore failed is never removed. Root-level notes in launcher-updates stay.
fn clean_stale(data: &Path, exe: Option<&Path>, failed: bool, keep: Option<&Path>) {
    if let Ok(entries) = fs::read_dir(data.join("launcher-updates")) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) && keep != Some(entry.path().as_path()) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    // Finished update helpers' folders (`apply::start`); only an elevated launcher can remove them.
    if let Ok(entries) = crate::prereqs::program_data().and_then(fs::read_dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(apply::WORK_PREFIX) {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    let Some(folder) = exe.and_then(Path::parent) else {
        return;
    };
    if failed {
        return; // The error text may name the backup holding the previous package.
    }
    if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(".crystal-previous-")
                && entry.file_type().is_ok_and(|t| t.is_dir())
                && !entry.path().join(apply::RESTORE_FAILED).exists()
                && !entry.path().join(apply::AWAITING_START).exists()
            {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}

/// Once the window loop has run: this version starts, so an update helper waiting to roll it
/// back keeps nothing (`apply::AWAITING_START`).
pub(crate) fn started() {
    if std::env::args().any(|a| a.starts_with("--preview")) {
        return;
    }
    let Some(folder) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_owned)) else {
        return;
    };
    for entry in fs::read_dir(folder).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(".crystal-previous-") {
            let _ = fs::remove_file(entry.path().join(apply::AWAITING_START));
        }
    }
}

fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

fn read_metadata(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut data = Vec::new();
    file.take(MAX_METADATA as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > MAX_METADATA {
        return Err(t("Launcher update metadata is too large.").into());
    }
    Ok(data)
}

fn bytes<const N: usize>(hex: &str) -> Result<[u8; N], String> {
    if hex.len() != N * 2 || !hex.is_ascii() {
        return Err(t("Invalid launcher update signature or checksum.").into());
    }
    let mut output = [0; N];
    for (i, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| (b as char).to_digit(16).map(|x| x as u8);
        output[i] = digit(pair[0])
            .zip(digit(pair[1]))
            .map(|(a, b)| (a << 4) | b)
            .ok_or(t("Invalid launcher update signature or checksum."))?;
    }
    Ok(output)
}

fn trusted_key() -> Result<[u8; 32], String> {
    let key = bytes(PUBLIC_KEY.trim())?;
    if key == [0; 32] {
        return Err(t("Launcher release signing has not been configured.").into());
    }
    Ok(key)
}

fn asset_url(url: &Url, extension: &str) -> bool {
    // GitHub may spell the owner or name with other capitals than REPO
    let prefix = release_path();
    let path = url.path();
    let rest = (path.len() > prefix.len()
        && path.is_char_boundary(prefix.len())
        && path[..prefix.len()].eq_ignore_ascii_case(&prefix))
    .then(|| &path[prefix.len()..]);
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !path.contains('%')
        && rest.is_some_and(|rest| {
            let parts: Vec<_> = rest.split('/').collect();
            parts.len() == 2
                && !parts[0].is_empty()
                && !parts[1].is_empty()
                && parts[1].ends_with(extension)
        })
}

fn download_host(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(
            url.host_str(),
            Some(
                "github.com"
                    | "api.github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

fn client() -> Result<Client, String> {
    Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && download_host(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("Launcher update redirect left GitHub's HTTPS release hosts")
            }
        }))
        .user_agent(concat!("Crystal-Launcher/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())
}

fn verify(envelope: &[u8], key: &[u8; 32]) -> Result<Release, String> {
    if envelope.len() > MAX_METADATA {
        return Err(t("Launcher update metadata is too large.").into());
    }
    let signed: Envelope =
        serde_json::from_slice(envelope).map_err(|_| t("Invalid launcher update metadata."))?;
    signature::UnparsedPublicKey::new(&signature::ED25519, key)
        .verify(signed.payload.as_bytes(), &bytes::<64>(&signed.signature)?)
        .map_err(|_| t("Launcher update signature did not verify."))?;
    let payload: Payload = serde_json::from_str(&signed.payload)
        .map_err(|_| t("Invalid launcher update metadata."))?;
    let version = semver::Version::parse(&payload.version)
        .map_err(|_| t("Invalid launcher update version."))?;
    if payload.format != 1
        || payload.platform != "windows-x86_64"
        || version.major > u32::MAX as u64
        || payload.size == 0
        || payload.size > MAX_ARCHIVE
        || !asset_url(
            &Url::parse(&payload.url).map_err(|_| t("Invalid launcher update URL."))?,
            ".zip",
        )
    {
        return Err(t("Launcher update is not a supported Windows release.").into());
    }
    bytes::<32>(&payload.sha256)?;
    Ok(Release {
        payload,
        version,
        envelope: envelope.to_vec(),
    })
}

async fn body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(t("Launcher update metadata is too large.").into());
    }
    let mut output = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if output.len().saturating_add(chunk.len()) > limit {
            return Err(t("Launcher update metadata is too large.").into());
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

#[derive(Deserialize)]
struct PublishedRelease {
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

async fn check() -> Result<Option<Release>, String> {
    let key = trusted_key()?;
    let client = client()?;
    let index = client
        .get(index_url())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if index.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(t("GitHub launcher releases are unavailable. Private builds need the supplied installer.").into());
    }
    let releases: Vec<PublishedRelease> = serde_json::from_slice(
        &body(
            index.error_for_status().map_err(|e| e.to_string())?,
            MAX_METADATA,
        )
        .await?,
    )
    .map_err(|_| t("GitHub returned invalid launcher release metadata."))?;
    let mut candidates = Vec::new();
    // Pre-releases are for testers (builds that are pre-releases themselves); the GitHub API
    // never lists drafts to anonymous callers.
    let tester = !stable(&current_version());
    for release in releases
        .into_iter()
        .filter(|r| offered(r, tester))
        .take(10)
    {
        let Some(asset) = release
            .assets
            .into_iter()
            .find(|a| a.name == "launcher-update.json")
        else {
            continue;
        };
        candidates.push(fetch_release(&client, &asset.browser_download_url, &key).await);
    }
    newest(candidates, &current_version())
}

/// Whether a published release is considered: never a draft, a flagged pre-release only on
/// a pre-release build (`newest` then compares versions).
fn offered(release: &PublishedRelease, tester: bool) -> bool {
    !release.draft && (tester || !release.prerelease)
}

async fn fetch_release(client: &Client, url: &str, key: &[u8; 32]) -> Result<Release, String> {
    let url = Url::parse(url).map_err(|_| t("Invalid launcher update URL."))?;
    if !asset_url(&url, ".json") {
        return Err(t("Invalid launcher update URL.").into());
    }
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    verify(&body(response, MAX_METADATA).await?, key)
}

/// The highest verified release. A broken release is skipped; its error is reported only
/// when no release verifies. A stable build never picks a signed pre-release version.
fn newest(
    candidates: Vec<Result<Release, String>>,
    current: &semver::Version,
) -> Result<Option<Release>, String> {
    let mut newest: Option<Release> = None;
    let mut first_error = None;
    for candidate in candidates {
        match candidate {
            Ok(release) if stable(current) && !stable(&release.version) => {}
            Ok(release) => {
                if newest.as_ref().is_none_or(|r| release.version > r.version) {
                    newest = Some(release);
                }
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    match (newest, first_error) {
        (None, Some(error)) => Err(error),
        (newest, _) => Ok(newest),
    }
}

fn verify_archive(path: &Path, release: &Release) -> Result<(), String> {
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != release.payload.size {
        return Err(t("Launcher update download is incomplete.").into());
    }
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| e.to_string())?;
    if hash.finalize().as_slice() != bytes::<32>(&release.payload.sha256)? {
        return Err(t("Launcher update checksum did not match.").into());
    }
    Ok(())
}

/// Exact names only: no paths, duplicate entries, symlinks or unexpected files.
fn unpack(archive: &Path, directory: &Path) -> Result<(), String> {
    let file = fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    if zip.len() != FILES.len() {
        return Err(t("Launcher update package has unexpected files.").into());
    }
    let mut found = std::collections::BTreeSet::new();
    let mut expanded = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_owned();
        expanded = expanded.saturating_add(entry.size());
        if !FILES.contains(&name.as_str())
            || !found.insert(name.clone())
            || entry.is_dir()
            || entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000)
            || expanded > MAX_ARCHIVE
        {
            return Err(t("Launcher update package has unexpected files.").into());
        }
        let path = directory.join(&name);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let count = std::io::copy(&mut (&mut entry).take(MAX_ARCHIVE + 1), &mut output)
            .map_err(|e| e.to_string())?;
        if count > MAX_ARCHIVE || count != entry.size() {
            return Err(t("Launcher update package is incomplete.").into());
        }
        output.sync_all().map_err(|e| e.to_string())?;
    }
    let mut file = fs::File::open(directory.join(FILES[0])).map_err(|e| e.to_string())?;
    let mut header = vec![0; 1024];
    let count = file.read(&mut header).map_err(|e| e.to_string())?;
    header.truncate(count);
    if !windows_executable(&header) {
        return Err(t("Launcher update is not a 64-bit Windows executable.").into());
    }
    Ok(())
}

fn windows_executable(header: &[u8]) -> bool {
    if header.len() < 64 || &header[..2] != b"MZ" {
        return false;
    }
    let offset = u32::from_le_bytes(header[60..64].try_into().unwrap()) as usize;
    header
        .get(offset..offset.saturating_add(6))
        .is_some_and(|pe| pe == b"PE\0\0\x64\x86")
}

async fn download(release: Release, data: PathBuf) -> Result<Stage, String> {
    // Recheck the signed fields before touching the staging directory.
    let release = verify(&release.envelope, &trusted_key()?)?;
    let directory = data.join("launcher-updates").join(format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let staged = stage(&release, &directory).await;
    if staged.is_err() {
        let _ = fs::remove_dir_all(&directory);
    }
    staged.map(|()| Stage { release, directory })
}

async fn stage(release: &Release, directory: &Path) -> Result<(), String> {
    let archive = directory.join("release.zip");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(|e| e.to_string())?;
    let mut response = client()?
        .get(&release.payload.url)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if response
        .content_length()
        .is_some_and(|n| n != release.payload.size)
    {
        return Err(t("Launcher update download has an unexpected length.").into());
    }
    let mut downloaded = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if downloaded > release.payload.size {
            return Err(t("Launcher update download has an unexpected length.").into());
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    verify_archive(&archive, release)?;
    fs::write(directory.join("launcher-update.json"), &release.envelope)
        .map_err(|e| e.to_string())?;
    let extracted = directory.join("verified");
    fs::create_dir(&extracted).map_err(|e| e.to_string())?;
    unpack(&archive, &extracted)
}

type Action = Option<(&'static str, Msg)>;

/// Status text and action for the states the banner and the settings card share.
fn shared_status(app: &App) -> Option<(String, Action)> {
    Some(match &app.self_update {
        State::Available(release) => (
            tf(
                "Crystal Launcher {version} is available.",
                &[("version", &release.payload.version)],
            ),
            Some((t("Update and restart"), Msg::Download)),
        ),
        State::Downloading => (
            t("Downloading and verifying the launcher update…").into(),
            None,
        ),
        State::Applying => (t("Restarting Crystal Launcher…").into(), None),
        _ => return None,
    })
}

fn restart_action(app: &App) -> Action {
    app.can_restart_launcher()
        .then_some((t("Restart to update"), Msg::Apply))
}

pub(crate) fn update(state: &mut State, msg: Msg, config: &Config, can_restart: bool) -> Effect<Msg> {
    match msg {
        // Never while busy, and never over a found or downloaded update (that would drop it).
        Msg::Check
            if matches!(
                state,
                State::Idle | State::Current | State::NotPublished | State::Error(_)
            ) =>
        {
            *state = State::Checking;
            return Effect::perform(check(), Msg::Checked);
        }
        Msg::Checked(result) => {
            if result.is_ok() {
                clear_last_error(&config.experiment);
            }
            *state = match result {
                Ok(Some(release)) if release.version > current_version() => {
                    State::Available(release)
                }
                Ok(Some(_)) => State::Current,
                Ok(None) => State::NotPublished,
                Err(error) => State::Error(error),
            };
        }
        Msg::Download => {
            if let State::Available(release) = state {
                let release = release.clone();
                *state = State::Downloading;
                return Effect::perform(
                    download(release, config.experiment.clone()),
                    Msg::Downloaded,
                );
            }
        }
        Msg::Downloaded(result) => {
            if result.is_ok() {
                clear_last_error(&config.experiment);
            }
            *state = match result {
                Ok(stage) => {
                    // Kept on disk: if it is not installed now, the next start installs it.
                    log(config, &format!("self-update: downloaded and verified {}", stage.version()));
                    State::Ready(stage)
                }
                Err(error) => State::Error(error),
            };
            // The user asked for "Update and restart": restart now when nothing would be lost.
            if can_restart && matches!(state, State::Ready(_)) {
                return Effect::done(Msg::Apply);
            }
        }
        Msg::Apply if can_restart => {
            if let State::Ready(stage) = state {
                let stage = stage.clone();
                log(config, &format!("self-update: installing {} and restarting", stage.version()));
                *state = State::Applying;
                return Effect::perform(
                    async move {
                        tokio::task::spawn_blocking(move || apply::start(&stage))
                            .await
                            .unwrap_or_else(|_| {
                                Err(t("The launcher update helper could not start.").into())
                            })
                    },
                    Msg::Applied,
                );
            }
        }
        Msg::Applied(Err(error)) => {
            log(config, &format!("self-update: install failed: {error}"));
            *state = State::Error(error);
        }
        _ => {}
    }
    Effect::none()
}

/// Release tooling and the post-exit helper run before account storage or UI.
pub(crate) fn cli(args: &[String]) -> Option<Result<(), String>> {
    match args.get(1).map(String::as_str) {
        Some("--apply-launcher-update") => Some(apply::run(args)),
        Some("--create-launcher-update-key") => Some((|| {
            let path = PathBuf::from(
                args.get(2)
                    .ok_or("An absolute private-key path is required.")?,
            );
            if !path.is_absolute() {
                return Err("An absolute private-key path is required.".into());
            }
            let document =
                signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
                    .map_err(|_| "Windows could not generate the release signing key.")?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| e.to_string())?;
            file.write_all(document.as_ref())
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            let key = signature::Ed25519KeyPair::from_pkcs8(document.as_ref())
                .map_err(|_| "Invalid generated key.")?;
            println!("{}", hex(key.public_key().as_ref()));
            Ok(())
        })()),
        Some("--sign-launcher-release") => Some(sign_release(args)),
        _ => None,
    }
}

fn sign_release(args: &[String]) -> Result<(), String> {
    if args.len() != 7 {
        return Err(
            "Expected: private-key version package-folder archive-URL output-folder".into(),
        );
    }
    let key_path = Path::new(&args[2]);
    let version = &args[3];
    let package = Path::new(&args[4]);
    let url = &args[5];
    let output = Path::new(&args[6]);
    if !key_path.is_absolute()
        || !package.is_absolute()
        || !output.is_absolute()
        || version != env!("CARGO_PKG_VERSION")
    {
        return Err("Use absolute paths and the version of this built launcher.".into());
    }
    let url = Url::parse(url).map_err(|e| e.to_string())?;
    if !asset_url(&url, ".zip") {
        return Err("The archive URL must belong to Crystal's GitHub releases.".into());
    }
    let private = zeroize::Zeroizing::new(fs::read(key_path).map_err(|e| e.to_string())?);
    let key = signature::Ed25519KeyPair::from_pkcs8(&private)
        .map_err(|_| "Invalid release signing key.")?;
    if key.public_key().as_ref() != trusted_key()? {
        return Err("Signing key does not match this launcher's public key.".into());
    }
    fs::create_dir_all(output).map_err(|e| e.to_string())?;
    let filename = url
        .path_segments()
        .and_then(|mut parts| parts.next_back())
        .ok_or("Missing archive name.")?;
    let archive = output.join(filename);
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive)
        .map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    for name in FILES {
        let path = package.join(name);
        if !path.is_file() {
            return Err(format!("Package is missing {name}."));
        }
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .map_err(|e| e.to_string())?;
        std::io::copy(
            &mut fs::File::open(path).map_err(|e| e.to_string())?,
            &mut zip,
        )
        .map_err(|e| e.to_string())?;
    }
    zip.finish()
        .map_err(|e| e.to_string())?
        .sync_all()
        .map_err(|e| e.to_string())?;
    let archive_bytes = fs::read(&archive).map_err(|e| e.to_string())?;
    let payload = Payload {
        format: 1,
        platform: "windows-x86_64".into(),
        version: version.clone(),
        url: url.to_string(),
        size: archive_bytes.len() as u64,
        sha256: sha256_hex(&archive_bytes),
    };
    let payload = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let signed = Envelope {
        signature: hex(key.sign(payload.as_bytes()).as_ref()),
        payload,
    };
    let metadata = serde_json::to_vec_pretty(&signed).map_err(|e| e.to_string())?;
    verify(&metadata, &trusted_key()?)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("launcher-update.json"))
        .map_err(|e| e.to_string())?;
    file.write_all(&metadata)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    println!("Signed {filename} and launcher-update.json; nothing was uploaded.");
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn signed(version: &str) -> (Vec<u8>, [u8; 32]) {
        let key = signature::Ed25519KeyPair::from_seed_unchecked(&[17; 32]).unwrap();
        let payload = Payload { format: 1, platform: "windows-x86_64".into(), version: version.into(),
            url: format!("https://github.com/{REPO}/releases/download/v0.2.0/Crystal-Launcher.zip"),
            size: 400, sha256: "ab".repeat(32) };
        let payload = serde_json::to_string(&payload).unwrap();
        let envelope = Envelope {
            signature: hex(key.sign(payload.as_bytes()).as_ref()),
            payload,
        };
        (
            serde_json::to_vec(&envelope).unwrap(),
            key.public_key().as_ref().try_into().unwrap(),
        )
    }

    fn release(version: &str) -> Release {
        let (data, key) = signed(version);
        verify(&data, &key).unwrap()
    }

    /// A finished download of `version` in `data`'s staging folder `name`, as `stage` leaves it:
    /// the archive, then the signed metadata that names its length and hash.
    fn staged_download(data: &Path, name: &str, version: &str, archive: &[u8]) -> ([u8; 32], PathBuf) {
        let key = signature::Ed25519KeyPair::from_seed_unchecked(&[17; 32]).unwrap();
        let payload = Payload { format: 1, platform: "windows-x86_64".into(), version: version.into(),
            url: format!("https://github.com/{REPO}/releases/download/v9/Crystal-Launcher.zip"),
            size: archive.len() as u64, sha256: sha256_hex(archive) };
        let payload = serde_json::to_string(&payload).unwrap();
        let envelope = Envelope { signature: hex(key.sign(payload.as_bytes()).as_ref()), payload };
        let directory = data.join("launcher-updates").join(name);
        fs::create_dir_all(directory.join("verified")).unwrap();
        fs::write(directory.join("release.zip"), archive).unwrap();
        fs::write(directory.join("launcher-update.json"), serde_json::to_vec(&envelope).unwrap()).unwrap();
        (key.public_key().as_ref().try_into().unwrap(), directory)
    }

    #[test]
    fn a_verified_download_is_kept_across_restarts_and_reused() {
        let root = scratch("reuse");
        let current = semver::Version::parse("1.2.1").unwrap();
        let (key, directory) = staged_download(&root, "100-1", "1.2.2", &[3; 600]);
        let stage = staged(&root, &key, &current).expect("the finished download is reused");
        assert_eq!(stage.version(), "1.2.2");
        assert_eq!(stage.directory, directory);
        // The start that reuses it keeps it; every other staging folder goes.
        fs::create_dir_all(root.join("launcher-updates/200-2")).unwrap();
        clean_stale(&root, None, false, Some(&stage.directory));
        assert!(directory.join("release.zip").exists());
        assert!(!root.join("launcher-updates/200-2").exists());
        // The newest of two verified downloads wins.
        staged_download(&root, "300-3", "1.3.0", &[4; 500]);
        assert_eq!(staged(&root, &key, &current).unwrap().version(), "1.3.0");
        // Another key's signature, or the running version itself, is not reused.
        assert!(staged(&root, &[9; 32], &current).is_none());
        assert!(staged(&root, &key, &semver::Version::parse("2.0.0").unwrap()).is_none(), "older than the running version");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_and_corrupt_downloads_are_discarded() {
        let root = scratch("corrupt");
        let current = semver::Version::parse("1.2.1").unwrap();
        // Interrupted: the archive arrived in part and the metadata was never written.
        let (key, partial) = staged_download(&root, "1-1", "1.2.2", &[1; 300]);
        fs::remove_file(partial.join("launcher-update.json")).unwrap();
        // Same length, one byte changed.
        let (_, changed) = staged_download(&root, "2-2", "1.2.2", &[2; 300]);
        let mut bytes = fs::read(changed.join("release.zip")).unwrap();
        bytes[10] ^= 1;
        fs::write(changed.join("release.zip"), bytes).unwrap();
        // Truncated.
        let (_, short) = staged_download(&root, "3-3", "1.2.2", &[3; 300]);
        fs::write(short.join("release.zip"), [3; 100]).unwrap();
        // Tampered metadata.
        let (_, tampered) = staged_download(&root, "4-4", "1.2.2", &[4; 300]);
        let text = fs::read_to_string(tampered.join("launcher-update.json")).unwrap();
        fs::write(tampered.join("launcher-update.json"), text.replace("1.2.2", "1.9.9")).unwrap();
        assert!(staged(&root, &key, &current).is_none());
        clean_stale(&root, None, false, None);
        for directory in [partial, changed, short, tampered] {
            assert!(!directory.exists(), "{}", directory.display());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_staged_update_installs_at_the_next_start_once() {
        let root = scratch("at-start");
        let config = Config { experiment: root.clone(), repository: root.clone(), game_install: None, dat_overlays: vec![] };
        let current = semver::Version::parse("1.2.1").unwrap();
        // Nothing staged: the start goes on as usual.
        assert!(!start_staged(&config, &[0; 32], &current, |_| panic!("nothing to install")));
        let (key, directory) = staged_download(&root, "5-5", "1.2.2", &[5; 400]);
        let mut started = None;
        assert!(start_staged(&config, &key, &current, |stage| { started = Some(stage.directory.clone()); Ok(()) }));
        assert_eq!(started.as_ref(), Some(&directory), "the helper gets the verified download");
        // A helper that fails leaves its error for this start to show, and the next start
        // reports it and discards the download instead of trying again.
        assert!(!start_staged(&config, &key, &current, |_| Err("helper failed".into())));
        assert_eq!(fs::read_to_string(last_error_path(&root)).unwrap(), "helper failed");
        assert!(!start_staged(&config, &key, &current, |_| panic!("never retried after a failure")));
        let state = take_last_error(&root);
        assert!(matches!(&state, State::Error(e) if e == "helper failed"));
        clean_stale(&root, None, true, None);
        assert!(!directory.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// A ready update for other modules' tests (the pill, the automatic restart).
    pub(crate) fn fixture_stage(version: &str) -> Stage {
        Stage { release: release(version), directory: std::env::temp_dir().join("crystal-fixture-stage") }
    }

    #[test]
    fn the_ready_update_applies_when_asked_and_crystal_is_free() {
        let root = scratch("apply");
        let config = Config { experiment: root.clone(), repository: root.clone(), game_install: None, dat_overlays: vec![] };
        let mut state = State::Ready(fixture_stage("9.0.0"));
        let _ = update(&mut state, Msg::Apply, &config, false);
        assert!(matches!(state, State::Ready(_)), "work in flight: it waits");
        let effect = update(&mut state, Msg::Apply, &config, true);
        assert!(matches!(state, State::Applying));
        assert!(effect.is_async(), "the helper starts off the interface thread");
        fs::remove_dir_all(root).unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "crystal-update-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn one_broken_release_does_not_hide_a_valid_newer_one() {
        let current = semver::Version::parse("0.2.0").unwrap();
        let picked = newest(
            vec![Ok(release("0.5.0")), Err("bad signature".into()), Ok(release("0.4.1"))],
            &current,
        )
        .unwrap()
        .unwrap();
        assert_eq!(picked.payload.version, "0.5.0");
        // A backport published later still loses to the higher version.
        let picked = newest(vec![Ok(release("0.4.2")), Ok(release("0.5.0"))], &current)
            .unwrap()
            .unwrap();
        assert_eq!(picked.payload.version, "0.5.0");
        assert_eq!(
            newest(vec![Err("first".into()), Err("second".into())], &current).unwrap_err(),
            "first"
        );
        assert!(newest(vec![], &current).unwrap().is_none());
    }

    #[test]
    fn lsb_numbered_releases_are_stable_and_ordered() {
        let v = |text: &str| semver::Version::parse(text).unwrap();
        assert!(stable(&v("1.2.7")) && stable(&v("1.2.7-lsb.1")) && stable(&v("1.2.7-lsb.12")));
        assert!(!stable(&v("1.2.8-lsb.1.beta.1")) && !stable(&v("1.2.8-beta.1")) && !stable(&v("1.2.8-lsb")));
        // The fork's sequence, oldest first, including the 1.1.1 friends have now
        let order = ["1.1.1", "1.2.7-lsb.1", "1.2.7-lsb.2", "1.2.7-lsb.10", "1.2.8-lsb.1"].map(v);
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
        // A -lsb install takes the next -lsb release, but not a beta
        let current = v("1.2.7-lsb.1");
        let picked = newest(vec![Ok(release("1.2.8-lsb.1.beta.1")), Ok(release("1.2.7-lsb.2"))], &current)
            .unwrap()
            .unwrap();
        assert_eq!(picked.payload.version, "1.2.7-lsb.2");
    }

    #[test]
    fn stable_builds_skip_signed_pre_release_versions() {
        let stable = semver::Version::parse("0.2.0").unwrap();
        let picked = newest(vec![Ok(release("0.3.0-beta.1")), Ok(release("0.2.1"))], &stable)
            .unwrap()
            .unwrap();
        assert_eq!(picked.payload.version, "0.2.1");
        let beta = semver::Version::parse("0.3.0-beta.1").unwrap();
        let picked = newest(vec![Ok(release("0.3.0-beta.2"))], &beta).unwrap().unwrap();
        assert_eq!(picked.payload.version, "0.3.0-beta.2");
        let flagged: Vec<PublishedRelease> =
            serde_json::from_str(r#"[{"prerelease":true,"assets":[]},{"assets":[]}]"#).unwrap();
        assert!(flagged[0].prerelease && !flagged[1].prerelease);
        assert!(!offered(&flagged[0], false) && offered(&flagged[1], false));
        assert!(offered(&flagged[0], true) && offered(&flagged[1], true), "a beta build is offered the next beta");
    }

    #[test]
    fn a_failed_update_is_reported_once_and_staging_is_cleaned() {
        let root = scratch("stale");
        let data = root.join("data");
        let updates = data.join("launcher-updates");
        fs::create_dir_all(updates.join("123-456/verified")).unwrap();
        fs::write(updates.join("last-registration-warning.txt"), "warning").unwrap();
        fs::write(updates.join("last-error.txt"), "helper failed").unwrap();
        let install = root.join("install");
        fs::create_dir_all(install.join(".crystal-previous-1-2")).unwrap();
        fs::create_dir_all(install.join("keep")).unwrap();
        let exe = install.join("Crystal-Launcher.exe");

        let state = take_last_error(&data);
        assert!(matches!(&state, State::Error(e) if e == "helper failed"));
        clean_stale(&data, Some(&exe), true, None);
        assert!(!updates.join("123-456").exists());
        assert!(updates.join("last-registration-warning.txt").exists());
        // The failure may name this backup, so it survives the start that reports it.
        assert!(install.join(".crystal-previous-1-2").exists());

        assert!(matches!(take_last_error(&data), State::Idle));
        fs::create_dir_all(install.join(".crystal-previous-3-4")).unwrap();
        fs::write(install.join(".crystal-previous-3-4").join(apply::RESTORE_FAILED), "").unwrap();
        fs::create_dir_all(install.join(".crystal-previous-5-6")).unwrap();
        fs::write(install.join(".crystal-previous-5-6").join(apply::AWAITING_START), "").unwrap();
        clean_stale(&data, Some(&exe), false, None);
        assert!(!install.join(".crystal-previous-1-2").exists());
        assert!(install.join(".crystal-previous-3-4").exists(), "its restore failed: the only old copy");
        assert!(install.join(".crystal-previous-5-6").exists(), "the helper may still roll back to it");
        assert!(install.join("keep").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checking_never_replaces_a_found_or_downloaded_update() {
        let root = scratch("ready");
        let config = Config {
            experiment: root.clone(),
            repository: root.clone(),
            game_install: None,
            dat_overlays: vec![],
        };
        let stage = Stage {
            release: release("9.0.0"),
            directory: root.join("stage"),
        };
        for mut state in [
            State::Ready(stage),
            State::Available(release("9.0.0")),
            State::Checking,
            State::Downloading,
            State::Applying,
        ] {
            let before = format!("{state:?}");
            let _ = update(&mut state, Msg::Check, &config, false);
            assert_eq!(format!("{state:?}"), before);
        }
        let mut state = State::Error("old".into());
        let _ = update(&mut state, Msg::Check, &config, false);
        assert!(matches!(state, State::Checking));
        fs::create_dir_all(root.join("launcher-updates")).unwrap();
        fs::write(last_error_path(&root), "old").unwrap();
        let _ = update(&mut state, Msg::Checked(Ok(None)), &config, false);
        assert!(matches!(state, State::NotPublished));
        assert!(!last_error_path(&root).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn signature_rejects_tampering_and_other_keys() {
        let (data, key) = signed("0.2.0");
        assert_eq!(verify(&data, &key).unwrap().payload.version, "0.2.0");
        let mut envelope: Envelope = serde_json::from_slice(&data).unwrap();
        envelope.payload = envelope.payload.replace("0.2.0", "0.3.0");
        assert!(verify(&serde_json::to_vec(&envelope).unwrap(), &key).is_err());
        assert!(verify(&data, &[0; 32]).is_err());
    }

    #[test]
    fn only_crystals_https_release_assets_are_accepted() {
        let good = format!("https://github.com/{REPO}/releases/download/v1/Crystal-Launcher.zip");
        let good = good.as_str();
        assert!(asset_url(&Url::parse(&good.to_ascii_lowercase()).unwrap(), ".zip"), "GitHub may change the capitals");
        assert!(asset_url(&Url::parse(good).unwrap(), ".zip"));
        for bad in [
            good.replace("https:", "http:"),
            good.replace(REPO, "someone/Crystal-Launcher"),
            good.replace(REPO, "Offset437/FFXI-Crystal-Launcher"),
            format!("{good}?target=other"),
            good.replace("github.com", "github.com.evil.example"),
            good.replace("/v1/", "/%2e%2e/"),
            good.replace("https://", "https://user@"),
        ] {
            assert!(!asset_url(&Url::parse(&bad).unwrap(), ".zip"), "{bad}");
        }
    }

    #[test]
    fn wrong_platform_and_unbounded_packages_fail_even_when_signed() {
        let (data, key) = signed("0.2.0");
        let signer = signature::Ed25519KeyPair::from_seed_unchecked(&[17; 32]).unwrap();
        let original: Envelope = serde_json::from_slice(&data).unwrap();
        for replacement in [
            original.payload.replace("windows-x86_64", "linux"),
            original
                .payload
                .replace("\"size\":400", "\"size\":999999999999"),
        ] {
            let envelope = Envelope {
                signature: hex(signer.sign(replacement.as_bytes()).as_ref()),
                payload: replacement,
            };
            assert!(verify(&serde_json::to_vec(&envelope).unwrap(), &key).is_err());
        }
    }

    #[test]
    fn executable_header_is_checked_without_unbounded_offsets() {
        let mut header = [0u8; 256];
        header[..2].copy_from_slice(b"MZ");
        header[60..64].copy_from_slice(&128u32.to_le_bytes());
        header[128..134].copy_from_slice(b"PE\0\0\x64\x86");
        assert!(windows_executable(&header));
        header[132..134].copy_from_slice(&0x14cu16.to_le_bytes());
        assert!(!windows_executable(&header));
        header[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(!windows_executable(&header));
    }

    #[test]
    fn downloaded_archives_reject_same_size_changes_and_truncation() {
        let root = std::env::temp_dir().join(format!(
            "crystal-update-hash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let archive = root.join("release.zip");
        let (metadata, key) = signed("0.2.0");
        let mut release = verify(&metadata, &key).unwrap();
        let original = vec![7; 400];
        release.payload.sha256 = sha256_hex(&original);
        fs::write(&archive, &original).unwrap();
        verify_archive(&archive, &release).unwrap();
        let mut changed = original;
        changed[99] ^= 1;
        fs::write(&archive, &changed).unwrap();
        assert!(verify_archive(&archive, &release).is_err());
        fs::write(&archive, &changed[..100]).unwrap();
        assert!(verify_archive(&archive, &release).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extraction_accepts_the_package_and_rejects_paths_and_foreign_files() {
        let root = std::env::temp_dir().join(format!(
            "crystal-update-zip-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let mut header = [0u8; 256];
        header[..2].copy_from_slice(b"MZ");
        header[60..64].copy_from_slice(&128u32.to_le_bytes());
        header[128..134].copy_from_slice(b"PE\0\0\x64\x86");
        for (index, replacement) in [None, Some("../README.md"), Some("surprise.exe")]
            .iter()
            .enumerate()
        {
            let archive = root.join(format!("case-{index}.zip"));
            let directory = root.join(format!("case-{index}"));
            fs::create_dir(&directory).unwrap();
            let mut zip = zip::ZipWriter::new(fs::File::create(&archive).unwrap());
            for (file_index, name) in FILES.iter().enumerate() {
                let name = if file_index == 1 {
                    replacement.unwrap_or(name)
                } else {
                    name
                };
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(if file_index == 0 { &header } else { b"notice" })
                    .unwrap();
            }
            zip.finish().unwrap();
            let result = unpack(&archive, &directory);
            assert_eq!(result.is_ok(), replacement.is_none());
        }
        assert!(!root.join("README.md").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
