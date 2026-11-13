//! Launcher manifests published by private servers.
//!
//! A LandSandBoat server can publish `https://<server>/manifest.json` (its world server builds
//! it; see `tools/launcher` in the server). When a private-server account's server publishes
//! one, a launch:
//! - runs the xiloader the server names, downloaded once per release and checked against the
//!   manifest's SHA-256 (LandSandBoat refuses other xiloader versions at login anyway),
//! - passes the server's key fingerprint, so xiloader 3 neither asks the player to trust the
//!   server (a prompt nobody would see: the loader runs without a window) nor connects to a
//!   server presenting another key,
//! - passes the server's login ports.
//!
//! A server without a manifest launches as before, with `xiloader.exe` from the game folder.
//! The last manifest read is kept, so a launch still works while the server's web side is down.

use crate::backend::Config;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const MANIFEST_NAME: &str = "manifest.json";
const SUPPORTED_SCHEMA: u32 = 1;
const MANIFEST_LIMIT: usize = 256 * 1024;
const LOADER_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Manifest {
    pub schema_version: u32,
    #[serde(default)]
    pub ports: Option<Ports>,
    #[serde(default)]
    pub client: Option<ClientInfo>,
    #[serde(default)]
    pub xiloader: Option<XiloaderEntry>,
    #[serde(default)]
    pub maintenance: bool,
    #[serde(default)]
    pub motd: String,
    #[serde(default)]
    pub server_fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientInfo {
    #[serde(default)]
    pub required_version: String,
    /// False while the server holds players back from a new FFXI client it doesn't support yet.
    #[serde(default = "approved")]
    pub update_approved: bool,
    /// "lsb-month" (LandSandBoat's own rule: the first 6 characters) or "exact".
    #[serde(default)]
    pub comparison_mode: String,
    /// The server's login.VER_LOCK: 0 no check, 1 must match, 2 this version or newer.
    #[serde(default = "newer_allowed")]
    pub ver_lock: u8,
}

fn approved() -> bool {
    true
}

fn newer_allowed() -> u8 {
    2
}

/// What the installed client means for logging in, judged the way the server's login does.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ClientVerdict {
    Fine,
    /// The server will refuse this client: don't launch.
    Refused(String),
    /// Likely fine, but worth a line in the launch status.
    Note(String),
}

pub(crate) fn client_verdict(installed: &str, client: &ClientInfo, host: &str) -> ClientVerdict {
    let required = client.required_version.trim();
    if required.is_empty() || installed.is_empty() || client.ver_lock == 0 {
        return ClientVerdict::Fine;
    }
    // LandSandBoat compares the first 6 characters (year and month of the patch)
    let key = |version: &str| -> String {
        if client.comparison_mode.eq_ignore_ascii_case("exact") {
            version.to_owned()
        } else {
            version.chars().take(6).collect()
        }
    };
    let (have, want) = (key(installed), key(required));
    if have == want {
        return ClientVerdict::Fine;
    }
    if have < want {
        return ClientVerdict::Refused(format!(
            "Your FINAL FANTASY XI client is {installed}, older than {host} accepts ({required}). Update FFXI first (Maintenance → Update), then launch again."
        ));
    }
    if client.ver_lock == 1 {
        return ClientVerdict::Refused(format!(
            "Your FINAL FANTASY XI client is {installed}, newer than {host} supports ({required}). The server has to be updated before this client can log in there."
        ));
    }
    ClientVerdict::Note(format!(
        "Your FFXI client ({installed}) is newer than {host} expects ({required}). It's allowed in, but if something misbehaves the server may not support this patch yet."
    ))
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct Ports {
    pub data: u16,
    pub view: u16,
    pub auth: u16,
    pub profile: u16,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct XiloaderEntry {
    #[serde(default)]
    pub version: String,
    pub url: String,
    pub sha256: String,
}

/// Extra xiloader arguments that come from the server's manifest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoaderOptions {
    /// `SHA256:<base64>`, passed as `--fingerprint`.
    pub fingerprint: Option<String>,
    /// Passed as `--dataport`, `--viewport`, `--authport` and `--profileport`.
    pub ports: Option<Ports>,
}

impl LoaderOptions {
    pub(crate) fn arguments(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(ports) = self.ports {
            for (flag, port) in [
                ("--dataport", ports.data),
                ("--viewport", ports.view),
                ("--authport", ports.auth),
                ("--profileport", ports.profile),
            ] {
                args.push(flag.to_owned());
                args.push(port.to_string());
            }
        }
        if let Some(fingerprint) = &self.fingerprint {
            args.push("--fingerprint".to_owned());
            args.push(fingerprint.clone());
        }
        args
    }
}

/// What a private-server launch runs, and what to tell the player about it.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub loader: PathBuf,
    pub options: LoaderOptions,
    /// Lines for the launch status and log, e.g. maintenance or the server's message.
    pub notes: Vec<String>,
    /// Lines for the launch log only, e.g. why no manifest was used.
    pub details: Vec<String>,
}

// ---- pure parts ----

/// `https://<server>/manifest.json` for a server given by host name. IP addresses have no
/// certificate to check a manifest against, so they launch the classic way.
pub(crate) fn manifest_url(server: &str) -> Option<reqwest::Url> {
    let server = server.trim().trim_end_matches('.');
    if server.is_empty() || server.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    let url = reqwest::Url::parse(&format!("https://{server}/{MANIFEST_NAME}")).ok()?;
    // Anything that isn't a bare host name ("host:port", "host/path", "user@host") changes the
    // parsed URL, and is not looked up.
    let plain = url.domain().is_some_and(|domain| domain.eq_ignore_ascii_case(server))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == format!("/{MANIFEST_NAME}")
        && url.query().is_none();
    plain.then_some(url)
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Manifest, String> {
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|error| format!("The server's launcher manifest is not valid: {error}."))?;
    if manifest.schema_version != SUPPORTED_SCHEMA {
        return Err(format!(
            "The server's launcher manifest uses version {} of the format; this launcher reads version {SUPPORTED_SCHEMA}.",
            manifest.schema_version
        ));
    }
    Ok(manifest)
}

/// Lower-case hex, if `text` is a SHA-256 in hex.
pub(crate) fn valid_sha256(text: &str) -> Option<String> {
    let text = text.trim();
    (text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())).then(|| text.to_ascii_lowercase())
}

/// `SHA256:` plus 43 characters of unpadded base64, the form xiloader 3 prints and compares.
pub(crate) fn valid_fingerprint(text: &str) -> Option<String> {
    let text = text.trim();
    let body = match text.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("SHA256:") => &text[7..],
        _ => text,
    };
    let body = body.trim_end_matches('=');
    let base64 = |b: u8| b.is_ascii_alphanumeric() || b == b'+' || b == b'/';
    (body.len() == 43 && body.bytes().all(base64)).then(|| format!("SHA256:{body}"))
}

/// The xiloader download URL, only over HTTPS and from the host that served the manifest.
pub(crate) fn loader_url(entry: &XiloaderEntry, manifest: &reqwest::Url) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(entry.url.trim())
        .map_err(|_| "The server's launcher manifest has an invalid xiloader address.".to_owned())?;
    let same_host = url.scheme() == "https"
        && url.host_str().zip(manifest.host_str()).is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && url.port() == manifest.port()
        && url.username().is_empty()
        && url.password().is_none();
    if !same_host {
        return Err(format!(
            "The server's launcher manifest points xiloader at {}, which is not HTTPS on {}; it was not downloaded.",
            url,
            manifest.host_str().unwrap_or_default()
        ));
    }
    Ok(url)
}

/// xiloader 3 is the first to take `--fingerprint`; older ones stop at an unknown option.
fn takes_fingerprint(version: &str) -> bool {
    let version = version.trim().trim_start_matches(['v', 'V']);
    version.split('.').next().and_then(|major| major.parse::<u32>().ok()).is_some_and(|major| major >= 3)
}

/// The server's xiloader always lives at one path under its own name, so a Windows Firewall
/// rule (which is per path) and anything that looks for an `xiloader.exe` process keep working
/// across updates.
const LOADER_NAME: &str = "xiloader.exe";

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_sha256(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn notes_for(manifest: &Manifest) -> Vec<String> {
    let mut notes = Vec::new();
    if manifest.maintenance {
        notes.push("The server is in maintenance; only GM accounts can log in right now.".to_owned());
    }
    let motd = manifest.motd.trim();
    if !motd.is_empty() {
        notes.push(format!("Server message: {}", motd.chars().take(300).collect::<String>()));
    }
    notes
}

/// Writes through a temporary file in the same folder, so a reader sees the old file or the
/// new one, never part of one.
/// Puts a new xiloader at `path`. A copy still running from an earlier launch can't be
/// overwritten on Windows, but it can be renamed: it's moved aside and removed on a later run.
fn install_loader(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if write_replacing(path, bytes).is_ok() {
        return Ok(());
    }
    let aside = path.with_extension(format!("old-{}", std::process::id()));
    fs::rename(path, &aside)?;
    if let Err(error) = write_replacing(path, bytes) {
        let _ = fs::rename(&aside, path);
        return Err(error);
    }
    Ok(())
}

fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = fs::File::create(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|_| file.sync_all()))
        .and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

// ---- network ----

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(concat!("CrystalLauncher/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| error.to_string())
}

/// The body of a 200 answer, refused past `limit` bytes.
async fn get_limited(client: &reqwest::Client, url: reqwest::Url, limit: usize) -> Result<Vec<u8>, String> {
    let mut response = client.get(url).send().await.map_err(|error| {
        if error.is_timeout() { "it did not answer in time".to_owned() } else { "it could not be reached".to_owned() }
    })?;
    if !response.status().is_success() {
        return Err(format!("it answered {}", response.status()));
    }
    if response.content_length().is_some_and(|size| size > limit as u64) {
        return Err("the file is larger than expected".to_owned());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "the download was interrupted".to_owned())? {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return Err("the file is larger than expected".to_owned());
        }
    }
    Ok(body)
}

// ---- launch ----

/// Where this server's manifest and xiloader copies are kept.
fn server_directory(config: &Config, host: &str) -> PathBuf {
    private_servers(&config.experiment).join(host.to_ascii_lowercase())
}

/// The FFXI version this folder has applied (its `patch.ver`, as Maintenance shows it), or
/// "" when it can't be read: an unknown version never stops a launch.
fn installed_version(game: &Path) -> String {
    let version = crate::patch::local_report(game).local_version;
    looks_like_version(&version).then_some(version).unwrap_or_default()
}

/// `30261006_0`: eight digits, an underscore and a build number.
fn looks_like_version(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10 && bytes[..8].iter().all(u8::is_ascii_digit) && bytes[8] == b'_'
        && bytes[9..].iter().all(u8::is_ascii_digit)
}

fn classic(game: &Path, notes: Vec<String>, details: Vec<String>) -> Result<Prepared, String> {
    let loader = game.join("xiloader.exe");
    if !loader.is_file() {
        return Err(format!(
            "xiloader.exe was not found in the selected FINAL FANTASY XI folder: {}",
            game.display()
        ));
    }
    Ok(Prepared { loader, options: LoaderOptions::default(), notes, details })
}

/// Decides what a private-server launch runs, downloading the server's xiloader when needed.
/// Runs on the launch's worker thread.
pub(crate) fn prepare(
    config: &Config,
    server: &str,
    game: &Path,
    progress: &mut dyn FnMut(&str),
) -> Result<Prepared, String> {
    if cfg!(test) {
        return classic(game, Vec::new(), Vec::new());
    }
    let Some(url) = manifest_url(server) else {
        return classic(game, Vec::new(), vec!["The server is an IP address, so no launcher manifest was looked up.".to_owned()]);
    };
    let host = url.host_str().unwrap_or_default().to_owned();
    let directory = server_directory(config, &host);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Could not start the network runtime: {error}"))?;

    progress("Checking the server's launcher manifest…");
    let fresh = client(Duration::from_secs(8))
        .and_then(|client| runtime.block_on(get_limited(&client, url.clone(), MANIFEST_LIMIT)))
        .and_then(|bytes| parse(&bytes).map(|manifest| (manifest, bytes)));

    let cache = directory.join(MANIFEST_NAME);
    let mut notes = Vec::new();
    let mut details = Vec::new();
    let manifest = match fresh {
        Ok((manifest, bytes)) => {
            let _ = fs::create_dir_all(&directory).and_then(|_| write_replacing(&cache, &bytes));
            manifest
        }
        Err(error) => match fs::read(&cache).ok().and_then(|bytes| parse(&bytes).ok()) {
            Some(manifest) => {
                notes.push(format!("The server's launcher manifest could not be read ({error}); using the last one read."));
                manifest
            }
            // Most servers publish no manifest: launch the classic way.
            None => {
                details.push(format!("No launcher manifest from {host} ({error}); using xiloader.exe from the game folder."));
                return classic(game, notes, details);
            }
        },
    };
    notes.extend(notes_for(&manifest));

    // The same version rule the server's login applies, so a client it would refuse is caught
    // here with a clear reason instead of a failed login
    if let Some(client) = &manifest.client {
        let installed = installed_version(game);
        match client_verdict(&installed, client, &host) {
            ClientVerdict::Fine => {}
            ClientVerdict::Refused(reason) => return Err(reason),
            ClientVerdict::Note(note) => notes.push(note),
        }
    }

    let ports = manifest.ports;
    // A blank hash is a server that couldn't hash its xiloader (older LSB manifests send one
    // when the file is missing): the same as naming none
    let entry = manifest.xiloader.as_ref().filter(|entry| !entry.sha256.trim().is_empty());
    let Some(entry) = entry else {
        details.push(format!("{host}'s launcher manifest names no xiloader; using xiloader.exe from the game folder."));
        let mut prepared = classic(game, notes, details)?;
        prepared.options.ports = ports;
        return Ok(prepared);
    };
    let sha256 = valid_sha256(&entry.sha256)
        .ok_or_else(|| "The server's launcher manifest has an invalid xiloader SHA-256.".to_owned())?;
    let label = if entry.version.trim().is_empty() { "xiloader".to_owned() } else { format!("xiloader {}", entry.version.trim()) };

    let managed = directory.join(LOADER_NAME);
    let in_game_folder = game.join("xiloader.exe");
    details.push(format!("{host}'s launcher manifest names {label} ({sha256})."));
    if file_sha256(&managed).as_deref() != Some(sha256.as_str()) {
        // A matching copy in the game folder saves the download, but is copied rather than run
        // from there, so the path stays the same for the next update
        let local = fs::read(&in_game_folder).ok().filter(|bytes| sha256_hex(bytes) == sha256);
        let bytes = match local {
            Some(bytes) => {
                details.push(format!("Copied {label} from the game folder."));
                bytes
            }
            None => download_loader(entry, &url, &sha256, &label, &host, &runtime, progress)?,
        };
        fs::create_dir_all(&directory)
            .and_then(|_| install_loader(&managed, &bytes))
            .map_err(|error| format!("Could not save {label}: {error}"))?;
        notes.push(format!("Installed {label}."));
    }
    remove_stale_loaders(&directory);
    let loader = managed;

    let fingerprint = manifest
        .server_fingerprint
        .as_deref()
        .and_then(valid_fingerprint)
        .filter(|_| takes_fingerprint(&entry.version));
    if fingerprint.is_none() {
        details.push("No server key fingerprint passed: the manifest has none, or its xiloader predates 3.0.".to_owned());
    }
    Ok(Prepared { loader, options: LoaderOptions { fingerprint, ports }, notes, details })
}

// ---- where servers are kept ----

fn private_servers(experiment: &Path) -> PathBuf {
    experiment.join("iced-launcher").join("private-servers")
}

fn download_loader(
    entry: &XiloaderEntry,
    manifest: &reqwest::Url,
    sha256: &str,
    label: &str,
    host: &str,
    runtime: &tokio::runtime::Runtime,
    progress: &mut dyn FnMut(&str),
) -> Result<Vec<u8>, String> {
    let download = loader_url(entry, manifest)?;
    progress(&format!("Downloading {label} from {host}…"));
    let bytes = client(Duration::from_secs(120))
        .and_then(|client| runtime.block_on(get_limited(&client, download, LOADER_LIMIT)))
        .map_err(|error| format!("Could not download {label} from {host}: {error}."))?;
    if sha256_hex(&bytes) != sha256 {
        return Err(format!("The {label} download from {host} does not match the SHA-256 in its manifest, so it was not used."));
    }
    if !bytes.starts_with(b"MZ") {
        return Err(format!("The {label} download from {host} is not a Windows program, so it was not used."));
    }
    Ok(bytes)
}

/// Copies moved aside while running (`xiloader.old-*`) and the per-version copies an earlier
/// build kept (`xiloader-<hash>.exe`). One still running is left for next time.
fn remove_stale_loaders(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.starts_with("xiloader.old-") || (name.starts_with("xiloader-") && name.ends_with(".exe")) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "schemaVersion": 1,
        "serverName": "My Home LSB",
        "serverHost": "ffxi.example.com",
        "ports": { "data": 54230, "view": 54001, "auth": 54231, "profile": 51220 },
        "client": { "requiredVersion": "30261006_0", "comparisonMode": "lsb-month", "updateApproved": true },
        "xiloader": {
            "version": "3.0.0",
            "url": "https://ffxi.example.com/downloads/xiloader.exe",
            "sha256": "68A1DE4112046AFF9F0E2C8B5A7D3E1F4C6B8A9D0E1F2A3B4C5D6E7F8A9B0C1D"
        },
        "verification": { "url": "https://ffxi.example.com/verify/30261006_0.json", "quickEnabled": true, "fullEnabled": true },
        "maintenance": false,
        "motd": "",
        "lanCidrs": ["192.168.2.0/24"],
        "hairpinOnLan": true,
        "serverFingerprint": "SHA256:eQmR2ar3Lq/CHlKETW+sZ8JqMDS48h/M/RIH8UmR0E4",
        "lsbCommit": "131cf71ed6"
    }"#;

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    #[test]
    fn manifest_url_is_only_built_for_a_bare_host_name() {
        assert_eq!(manifest_url(" ffxi.example.com ").unwrap().as_str(), "https://ffxi.example.com/manifest.json");
        assert_eq!(manifest_url("FFXI.Example.com.").unwrap().as_str(), "https://ffxi.example.com/manifest.json");
        for server in ["", "192.168.2.10", "::1", "ffxi.example.com:8080", "ffxi.example.com/evil", "user@ffxi.example.com", "ffxi.example.com?x", "a b"] {
            assert!(manifest_url(server).is_none(), "{server:?} should not be looked up");
        }
    }

    #[test]
    fn a_full_lsb_manifest_parses_and_unknown_fields_are_ignored() {
        let manifest = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(manifest.ports, Some(Ports { data: 54230, view: 54001, auth: 54231, profile: 51220 }));
        assert_eq!(manifest.xiloader.as_ref().unwrap().version, "3.0.0");
        assert!(!manifest.maintenance);
    }

    #[test]
    fn other_schema_versions_and_broken_json_are_refused() {
        assert!(parse(br#"{"schemaVersion": 2}"#).unwrap_err().contains("version 2"));
        assert!(parse(b"not json").is_err());
        assert!(parse(br#"{"schemaVersion": 1, "ports": {"data": 70000, "view": 1, "auth": 1, "profile": 1}}"#).is_err());
        // Everything but the schema version is optional
        assert_eq!(parse(br#"{"schemaVersion": 1}"#).unwrap().xiloader, None);
    }

    #[test]
    fn sha256_and_fingerprint_are_normalized_or_refused() {
        let hex = "68A1DE4112046AFF9F0E2C8B5A7D3E1F4C6B8A9D0E1F2A3B4C5D6E7F8A9B0C1D";
        assert_eq!(valid_sha256(hex).unwrap(), hex.to_ascii_lowercase());
        assert!(valid_sha256(&hex[1..]).is_none());
        assert!(valid_sha256(&format!("{}Z", &hex[1..])).is_none());

        let body = "eQmR2ar3Lq/CHlKETW+sZ8JqMDS48h/M/RIH8UmR0E4";
        let expected = format!("SHA256:{body}");
        assert_eq!(valid_fingerprint(&expected).as_deref(), Some(expected.as_str()));
        assert_eq!(valid_fingerprint(&format!(" sha256:{body}= ")).as_deref(), Some(expected.as_str()));
        assert_eq!(valid_fingerprint(body).as_deref(), Some(expected.as_str()));
        assert!(valid_fingerprint("SHA256:short").is_none());
        assert!(valid_fingerprint(&format!("SHA256:{}!", &body[1..])).is_none());
        assert!(valid_fingerprint(&format!("MD5:{body}")).is_none());
    }

    #[test]
    fn xiloader_is_only_downloaded_over_https_from_the_manifest_host() {
        let manifest = url("https://ffxi.example.com/manifest.json");
        let entry = |address: &str| XiloaderEntry { version: "3.0.0".into(), url: address.into(), sha256: String::new() };
        assert!(loader_url(&entry("https://FFXI.example.com/downloads/xiloader.exe"), &manifest).is_ok());
        for address in [
            "http://ffxi.example.com/downloads/xiloader.exe",
            "https://evil.example.com/xiloader.exe",
            "https://ffxi.example.com.evil.test/xiloader.exe",
            "https://ffxi.example.com:8443/xiloader.exe",
            "https://user@ffxi.example.com/xiloader.exe",
            "/downloads/xiloader.exe",
        ] {
            assert!(loader_url(&entry(address), &manifest).is_err(), "{address} should be refused");
        }
    }

    #[test]
    fn client_versions_are_judged_like_lsb_login() {
        let client = |required: &str, mode: &str, ver_lock: u8| ClientInfo {
            required_version: required.into(),
            update_approved: true,
            comparison_mode: mode.into(),
            ver_lock,
        };
        let lsb = client("30261006_0", "lsb-month", 2);
        // Same month, any build: LSB only looks at the first 6 characters
        assert_eq!(client_verdict("30261006_0", &lsb, "h"), ClientVerdict::Fine);
        assert_eq!(client_verdict("30261015_2", &lsb, "h"), ClientVerdict::Fine);
        assert!(matches!(client_verdict("30260910_0", &lsb, "h"), ClientVerdict::Refused(r) if r.contains("older")));
        assert!(matches!(client_verdict("30261106_0", &lsb, "h"), ClientVerdict::Note(_)));
        // VER_LOCK 1: newer is refused too; VER_LOCK 0: nothing is
        assert!(matches!(client_verdict("30261106_0", &client("30261006_0", "lsb-month", 1), "h"), ClientVerdict::Refused(r) if r.contains("newer")));
        assert_eq!(client_verdict("30200101_0", &client("30261006_0", "lsb-month", 0), "h"), ClientVerdict::Fine);
        // Exact mode compares the whole string
        assert!(matches!(client_verdict("30261006_0", &client("30261006_1", "exact", 2), "h"), ClientVerdict::Refused(_)));
        // Unknown installed version or no requirement: don't get in the way
        assert_eq!(client_verdict("", &lsb, "h"), ClientVerdict::Fine);
        assert_eq!(client_verdict("30200101_0", &client("", "lsb-month", 2), "h"), ClientVerdict::Fine);
        // An LSB manifest without verLock (older server build) means the default, 2
        let parsed = parse(br#"{"schemaVersion":1,"client":{"requiredVersion":"30261006_0","comparisonMode":"lsb-month"}}"#).unwrap();
        assert_eq!(parsed.client.unwrap().ver_lock, 2);
    }

    #[test]
    fn a_folder_without_a_readable_version_judges_nothing() {
        let dir = std::env::temp_dir().join(format!("crystal-no-version-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(installed_version(&dir), "");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_version_stamp_counts_as_the_installed_version() {
        assert!(looks_like_version("30261006_0"));
        assert!(looks_like_version("30261006_12"));
        for text in ["", "Unavailable", "30261006", "30261006_", "3026100a_0", "30261006-0", "30261006_0x"] {
            assert!(!looks_like_version(text), "{text:?}");
        }
    }

    #[test]
    fn a_blank_xiloader_hash_counts_as_no_xiloader() {
        let manifest = parse(br#"{"schemaVersion": 1, "xiloader": {"version": "", "url": "/downloads/xiloader.exe", "sha256": ""}}"#).unwrap();
        assert!(manifest.xiloader.as_ref().filter(|entry| !entry.sha256.trim().is_empty()).is_none());
    }

    #[test]
    fn only_xiloader_3_and_later_get_a_fingerprint() {
        assert!(takes_fingerprint("3.0.0"));
        assert!(takes_fingerprint("v3.1.2"));
        assert!(takes_fingerprint("10.0"));
        assert!(!takes_fingerprint("2.2.0"));
        assert!(!takes_fingerprint(""));
        assert!(!takes_fingerprint("latest"));
    }

    #[test]
    fn loader_options_become_xiloader_arguments() {
        assert!(LoaderOptions::default().arguments().is_empty());
        let options = LoaderOptions {
            fingerprint: Some("SHA256:abc".into()),
            ports: Some(Ports { data: 1, view: 2, auth: 3, profile: 4 }),
        };
        assert_eq!(
            options.arguments(),
            ["--dataport", "1", "--viewport", "2", "--authport", "3", "--profileport", "4", "--fingerprint", "SHA256:abc"]
        );
    }

    #[test]
    fn maintenance_and_message_become_notes() {
        let mut manifest = parse(SAMPLE.as_bytes()).unwrap();
        assert!(notes_for(&manifest).is_empty());
        manifest.maintenance = true;
        manifest.motd = "  Back at 6pm  ".into();
        assert_eq!(
            notes_for(&manifest),
            ["The server is in maintenance; only GM accounts can log in right now.", "Server message: Back at 6pm"]
        );
    }

    #[test]
    fn files_are_hashed_and_replaced_whole() {
        let dir = std::env::temp_dir().join(format!("crystal-manifest-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOADER_NAME);
        install_loader(&path, b"MZ first").unwrap();
        install_loader(&path, b"MZ second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"MZ second");
        assert_eq!(file_sha256(&path).unwrap(), sha256_hex(b"MZ second"));

        // Leftovers: a copy moved aside while running, and an earlier build's per-version copy
        fs::write(dir.join("xiloader.old-1234"), b"MZ old").unwrap();
        fs::write(dir.join("xiloader-68a1de4112046aff.exe"), b"MZ older").unwrap();
        fs::write(dir.join("manifest.json"), b"{}").unwrap();
        remove_stale_loaders(&dir);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["manifest.json", "xiloader.exe"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
