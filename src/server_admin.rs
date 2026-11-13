//! Running a LandSandBoat server's launcher side from Crystal (Settings → Server,
//! `server_admin_page`).
//!
//! Talks to the server's admin API (`/api/launcher/admin/`, see `tools/launcher` in the server),
//! which needs the token set as `launcher.ADMIN_TOKEN` there. The server and token are kept
//! encrypted for this Windows user, like saved accounts.
//!
//! Also builds the verification manifest (`verify/<client version>.json`) from this PC's FFXI
//! installation, in the same form as `New-FfxiVerifyManifest.ps1`, for uploading.

use crate::backend::Config;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use zeroize::Zeroize;

// ---- saved login ----

/// Deliberately has no Debug implementation; the token is cleared on drop.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Login {
    pub host: String,
    pub token: String,
}

impl Drop for Login {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

fn login_path(config: &Config) -> PathBuf {
    config.experiment.join("iced-launcher").join("server-admin.dpapi")
}

pub(crate) fn load_login(config: &Config) -> Option<Login> {
    let encrypted = fs::read(login_path(config)).ok()?;
    let plain = crate::backend::unprotect(&encrypted).ok()?;
    serde_json::from_slice(&plain[..]).ok()
}

pub(crate) fn save_login(config: &Config, login: &Login) -> Result<(), String> {
    let mut plain = serde_json::to_vec(login).map_err(|e| e.to_string())?;
    let encrypted = crate::backend::protect(&plain);
    plain.zeroize();
    let encrypted = encrypted?;
    let path = login_path(config);
    let temporary = path.with_extension("tmp");
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
        .and_then(|_| fs::write(&temporary, &encrypted))
        .and_then(|_| fs::rename(&temporary, &path))
        .map_err(|e| format!("Could not save the server admin login: {e}"))
}

pub(crate) fn forget_login(config: &Config) {
    let _ = fs::remove_file(login_path(config));
}

// ---- admin API ----

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Status {
    pub players_online: u32,
    pub client_version: String,
    pub lsb_commit: String,
    pub gate: Gate,
    pub xiloader: Loader,
    pub verify: VerifyFiles,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Gate {
    pub update_approved: bool,
    pub maintenance: bool,
    /// login.MAINT_MODE: the real login lock, set on the server only.
    pub login_maint_mode: bool,
    pub motd: String,
    /// Which of the above are set from this screen rather than settings/launcher.lua.
    pub overridden: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Loader {
    pub live: Option<LiveLoader>,
    pub staged: Vec<String>,
    pub archived: Vec<String>,
    pub held: Option<String>,
    pub request_pending: bool,
    pub last_result: Option<LoaderResult>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub(crate) struct LiveLoader {
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct LoaderResult {
    pub id: Option<String>,
    pub action: Option<String>,
    pub version: Option<String>,
    pub ok: bool,
    pub message: String,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct VerifyFiles {
    pub current_present: bool,
    pub versions: Vec<String>,
}

/// `https://<host>/api/launcher/admin/<path>`, for a host given the way private-server
/// accounts give one.
pub(crate) fn admin_url(host: &str, path: &str) -> Result<reqwest::Url, String> {
    let mut url = crate::server_manifest::manifest_url(host)
        .ok_or_else(|| "Enter the server's host name, e.g. ffxi.example.com (not an IP address or a URL).".to_owned())?;
    url.set_path(&format!("/api/launcher/admin/{path}"));
    Ok(url)
}

const REPLY_LIMIT: usize = 1024 * 1024;

async fn call(
    login: &Login,
    method: reqwest::Method,
    path: &str,
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let url = admin_url(&login.host, path)?;
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(concat!("CrystalLauncher/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client
        .request(method, url)
        .bearer_auth(&login.token)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            format!("{} did not answer in time.", login.host)
        } else {
            format!("Could not reach {}.", login.host)
        }
    })?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|_| format!("The answer from {} was cut off.", login.host))?;
    if bytes.len() > REPLY_LIMIT {
        return Err(format!("The answer from {} is larger than expected.", login.host));
    }
    if status.is_success() {
        return Ok(bytes.to_vec());
    }
    Err(match status.as_u16() {
        401 => "The server refused the admin token. Check launcher.ADMIN_TOKEN on the server.".to_owned(),
        404 => format!(
            "{} has no admin API: set launcher.ADMIN_TOKEN and restart xi_world, and add the admin location to nginx.",
            login.host
        ),
        _ => match serde_json::from_slice::<serde_json::Value>(&bytes).ok().and_then(|v| v["error"].as_str().map(str::to_owned)) {
            Some(error) => format!("The server refused it: {error}."),
            None => format!("{} answered {status}.", login.host),
        },
    })
}

fn parse_status(bytes: &[u8]) -> Result<Status, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("The server's admin status could not be read: {e}."))
}

pub(crate) async fn fetch_status(login: Login) -> Result<Status, String> {
    parse_status(&call(&login, reqwest::Method::GET, "status", None, Duration::from_secs(15)).await?)
}

/// A change to the update gate, maintenance or message. `None` fields are left alone; `Some(None)`
/// clears that override, so the server's settings/launcher.lua value applies again.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GateChange {
    pub update_approved: Option<Option<bool>>,
    pub maintenance: Option<Option<bool>>,
    pub motd: Option<Option<String>>,
}

impl GateChange {
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        if let Some(value) = self.update_approved {
            map.insert("updateApproved".into(), value.map_or(serde_json::Value::Null, serde_json::Value::Bool));
        }
        if let Some(value) = self.maintenance {
            map.insert("maintenance".into(), value.map_or(serde_json::Value::Null, serde_json::Value::Bool));
        }
        if let Some(value) = &self.motd {
            map.insert("motd".into(), value.clone().map_or(serde_json::Value::Null, serde_json::Value::String));
        }
        serde_json::Value::Object(map)
    }
}

pub(crate) async fn change_gate(login: Login, change: GateChange) -> Result<Status, String> {
    let body = serde_json::to_vec(&change.to_json()).map_err(|e| e.to_string())?;
    parse_status(&call(&login, reqwest::Method::PUT, "state", Some(body), Duration::from_secs(15)).await?)
}

/// Asks the server to promote a staged xiloader (`"promote"`) or roll back (`"rollback"`);
/// `version` None means the newest. The server carries it out within a few seconds.
pub(crate) async fn xiloader_request(login: Login, action: &'static str, version: Option<String>) -> Result<(), String> {
    let body = serde_json::to_vec(&serde_json::json!({ "action": action, "version": version })).map_err(|e| e.to_string())?;
    call(&login, reqwest::Method::POST, "xiloader", Some(body), Duration::from_secs(15)).await.map(|_| ())
}

pub(crate) async fn upload_verify(login: Login, client_version: String, manifest: Vec<u8>) -> Result<(), String> {
    call(&login, reqwest::Method::PUT, &format!("verify/{client_version}"), Some(manifest), Duration::from_secs(120))
        .await
        .map(|_| ())
}

// ---- verification manifest ----

/// What New-FfxiVerifyManifest.ps1 leaves out: per-user data, wrappers and loaders, logs.
pub(crate) const EXCLUDED: &[&str] = &[
    "USER",
    "TEMP",
    "D3D8.dll",
    "dgVoodoo.conf",
    "dgVoodooCpl.exe",
    "hookLog.log",
    "xiloader.exe",
    "patch2.cfg",
    "SYS/err000.bin",
    "SYS/err001.bin",
    "SYS/err002.bin",
    "SYS/err003.bin",
    "SYS/err004.bin",
    "SYS/err005.bin",
    "SYS/err006.bin",
    "SYS/err007.bin",
    "SYS/info00.bin",
    "SYS/info02.bin",
];

/// Checked by the launcher's quick verification.
const QUICK: &[&str] = &["patch.cfg", "patch.ver", "FFXiMain.dll", "FFXi.dll", "ROM/0/0.dat"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyManifest {
    schema_version: u32,
    client_version: String,
    generated_utc: String,
    excluded_prefixes: Vec<String>,
    files: Vec<VerifyEntry>,
}

#[derive(Serialize)]
struct VerifyEntry {
    path: String,
    size: u64,
    sha256: String,
    quick: bool,
}

/// The client version in patch.cfg: the last `YYYYMMDD_N` inside its first `{ … }` block.
/// patch.cfg is what was downloaded, not what was applied: `installed_client_version` reads
/// the applied version first and falls back to this.
pub(crate) fn client_version(patch_cfg: &str) -> Option<String> {
    let start = patch_cfg.find('{')? + 1;
    let block = &patch_cfg[start..start + patch_cfg[start..].find('}')?];
    let bytes = block.as_bytes();
    let word = |i: usize| bytes.get(i).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let mut found = None;
    let mut i = 0;
    while i + 10 <= bytes.len() {
        let date = bytes[i..i + 8].iter().all(u8::is_ascii_digit) && bytes[i + 8] == b'_';
        let digits = bytes[i + 9..].iter().take_while(|b| b.is_ascii_digit()).count();
        if date && digits > 0 && (i == 0 || !word(i - 1)) && !word(i + 9 + digits) {
            found = Some(&block[i..i + 9 + digits]);
            i += 9 + digits;
        } else {
            i += 1;
        }
    }
    found.map(str::to_owned)
}

fn excluded(relative: &str) -> bool {
    EXCLUDED.iter().any(|prefix| {
        relative.eq_ignore_ascii_case(prefix)
            || (relative.len() > prefix.len()
                && relative.as_bytes()[prefix.len()] == b'/'
                && relative[..prefix.len()].eq_ignore_ascii_case(prefix))
    })
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf, u64)>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if excluded(&relative) {
            continue;
        }
        if kind.is_dir() {
            collect(root, &path, out)?;
        } else if kind.is_file() {
            out.push((relative, path, entry.metadata()?.len()));
        }
    }
    Ok(())
}

/// RFC 3339 UTC time, e.g. 2026-11-09T20:41:07Z.
pub(crate) fn utc_timestamp(seconds_since_epoch: u64) -> String {
    let days = (seconds_since_epoch / 86_400) as i64;
    let rest = seconds_since_epoch % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// The FFXI version `game` has applied (patch.ver), else the one its patch.cfg names.
pub(crate) fn installed_client_version(game: &Path) -> Option<String> {
    let applied = crate::server_manifest::installed_version(game);
    if !applied.is_empty() {
        return Some(applied);
    }
    let patch_cfg = fs::read(game.join("patch.cfg")).ok()?;
    client_version(&String::from_utf8_lossy(&patch_cfg))
}

/// Builds verify/<client version>.json from the FFXI installation at `game`. Hashes every file
/// (several GB), so it runs off the interface thread; `progress` gets (bytes done, bytes total)
/// and returns false to stop.
pub(crate) fn generate_verify(game: &Path, version: &str, mut progress: impl FnMut(u64, u64) -> bool) -> Result<(String, Vec<u8>), String> {
    if !game.join("patch.cfg").is_file() {
        return Err(format!("patch.cfg was not found in {}. Choose the FINAL FANTASY XI folder.", game.display()));
    }
    let version = version.to_owned();

    let mut files = Vec::new();
    collect(game, game, &mut files).map_err(|e| format!("Could not list {}: {e}", game.display()))?;
    files.sort_by_key(|(relative, ..)| relative.to_ascii_lowercase());
    let total: u64 = files.iter().map(|(.., size)| size).sum();

    let mut done = 0u64;
    let mut buffer = vec![0u8; 1 << 20];
    let mut entries = Vec::with_capacity(files.len());
    if !progress(0, total) {
        return Err("Stopped.".into());
    }
    for (relative, path, size) in files {
        let mut file = fs::File::open(&path).map_err(|e| format!("Could not read {relative}: {e}"))?;
        let mut hasher = Sha256::new();
        loop {
            let read = file.read(&mut buffer).map_err(|e| format!("Could not read {relative}: {e}"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            done += read as u64;
            if !progress(done, total) {
                return Err("Stopped.".into());
            }
        }
        entries.push(VerifyEntry {
            quick: QUICK.iter().any(|q| q.eq_ignore_ascii_case(&relative)),
            sha256: format!("{:X}", hasher.finalize()),
            path: relative,
            size,
        });
    }

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let manifest = VerifyManifest {
        schema_version: 1,
        client_version: version.clone(),
        generated_utc: utc_timestamp(now),
        excluded_prefixes: EXCLUDED.iter().map(|p| (*p).to_owned()).collect(),
        files: entries,
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    Ok((version, json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_url_is_built_from_a_bare_host_name() {
        assert_eq!(admin_url("ffxi.example.com", "status").unwrap().as_str(), "https://ffxi.example.com/api/launcher/admin/status");
        assert_eq!(
            admin_url("ffxi.example.com", "verify/30261006_0").unwrap().as_str(),
            "https://ffxi.example.com/api/launcher/admin/verify/30261006_0"
        );
        for host in ["", "192.168.2.10", "https://ffxi.example.com", "ffxi.example.com:8088", "ffxi.example.com/x"] {
            assert!(admin_url(host, "status").is_err(), "{host:?}");
        }
    }

    #[test]
    fn status_from_the_server_parses() {
        let status = parse_status(br#"{
            "playersOnline": 7, "clientVersion": "30261006_0", "lsbCommit": "131cf71ed6",
            "gate": {"updateApproved": false, "maintenance": true, "loginMaintMode": false, "motd": "Patch day", "overridden": ["updateApproved"]},
            "xiloader": {"live": {"version": "3.0.0", "sha256": "68A1"}, "staged": ["3.0.1"], "archived": ["2.2.0"], "held": null,
                         "requestPending": false, "lastResult": {"id": "1", "action": "promote", "version": "3.0.0", "ok": true, "message": "published xiloader 3.0.0", "finishedAt": "2026-11-09T20:00:00+00:00"}},
            "verify": {"currentPresent": true, "versions": ["30261006_0"]},
            "somethingNewer": 1
        }"#).unwrap();
        assert_eq!(status.players_online, 7);
        assert!(!status.gate.update_approved && status.gate.maintenance);
        assert_eq!(status.xiloader.live.as_ref().unwrap().version, "3.0.0");
        assert_eq!(status.xiloader.staged, ["3.0.1"]);
        assert!(status.xiloader.last_result.as_ref().unwrap().ok);
        assert!(status.verify.current_present);
        assert!(parse_status(b"<html>").is_err());
    }

    #[test]
    fn gate_changes_only_send_what_changed() {
        assert_eq!(GateChange::default().to_json(), serde_json::json!({}));
        let change = GateChange { update_approved: Some(Some(false)), maintenance: Some(None), motd: Some(Some("Hi".into())) };
        assert_eq!(change.to_json(), serde_json::json!({"updateApproved": false, "maintenance": null, "motd": "Hi"}));
    }

    #[test]
    fn client_version_comes_from_the_first_patch_cfg_block() {
        let cfg = "Version\n{\n  30260910_0\n  30261006_0\n}\n{\n  30270101_9\n}\n";
        assert_eq!(client_version(cfg).as_deref(), Some("30261006_0"));
        assert_eq!(client_version("{ x30261006_0 30261006_12 }").as_deref(), Some("30261006_12"));
        assert_eq!(client_version("{ nothing here }"), None);
        assert_eq!(client_version("30261006_0 but no block"), None);
    }

    #[test]
    fn exclusions_match_whole_path_segments() {
        assert!(excluded("USER"));
        assert!(excluded("user/0/abc.dat"));
        assert!(excluded("SYS/err003.bin"));
        assert!(excluded("xiloader.exe"));
        assert!(!excluded("USERS.txt"));
        assert!(!excluded("ROM/0/0.dat"));
        assert!(!excluded("SYS/err008.bin"));
    }

    #[test]
    fn timestamps_are_utc_rfc3339() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_timestamp(1_794_256_867), "2026-11-09T20:41:07Z");
    }

    #[test]
    fn a_verify_manifest_is_built_from_an_install() {
        let game = std::env::temp_dir().join(format!("crystal-verify-{}", std::process::id()));
        for dir in ["ROM/0", "USER/0", "SYS"] {
            fs::create_dir_all(game.join(dir)).unwrap();
        }
        fs::write(game.join("patch.cfg"), "{\n 30261006_0\n}\n").unwrap();
        fs::write(game.join("FFXiMain.dll"), b"main").unwrap();
        fs::write(game.join("ROM/0/0.dat"), vec![7u8; 3 << 20]).unwrap();
        fs::write(game.join("USER/0/keys.dat"), b"personal").unwrap();
        fs::write(game.join("SYS/err000.bin"), b"log").unwrap();
        fs::write(game.join("xiloader.exe"), b"MZ").unwrap();

        let mut last = (0, 0);
        let (version, json) = generate_verify(&game, "30261006_0", |done, total| {
            last = (done, total);
            true
        })
        .unwrap();
        assert_eq!(version, "30261006_0");
        let doc: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(doc["schemaVersion"], 1);
        assert_eq!(doc["clientVersion"], "30261006_0");
        let paths: Vec<&str> = doc["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["FFXiMain.dll", "patch.cfg", "ROM/0/0.dat"]);
        let main = &doc["files"][0];
        assert_eq!(main["sha256"], format!("{:X}", Sha256::digest(b"main")));
        assert_eq!(main["quick"], true);
        assert_eq!(main["size"], 4);
        assert_eq!(last.0, last.1);
        let expected: u64 = ["FFXiMain.dll", "patch.cfg", "ROM/0/0.dat"].iter().map(|f| fs::metadata(game.join(f)).unwrap().len()).sum();
        assert_eq!(last.1, expected, "only the included files count toward progress");
        assert_eq!(generate_verify(&game, "30261006_0", |_, _| false).unwrap_err(), "Stopped.");
        fs::remove_dir_all(&game).unwrap();

        assert!(generate_verify(Path::new("/no/such/ffxi"), "30261006_0", |_, _| true).unwrap_err().contains("patch.cfg"));
    }
}
