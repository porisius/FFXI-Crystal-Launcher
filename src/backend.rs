//! Account storage and launch validation. DPAPI protects the saved vault;
//! retail credentials cross private pipes to the installed POL core.

use sha1::{Digest, Sha1};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::{Zeroize, Zeroizing};

use crate::i18n::{t, tf};
use crate::launch_log::ExitReport;
use crate::launch_timing::Timeline;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub experiment: PathBuf,
    pub repository: PathBuf,
    pub game_install: Option<PathBuf>,
    #[serde(default)]
    pub dat_overlays: Vec<PathBuf>,
}
impl Default for Config {
    fn default() -> Self {
        let executable =
            std::env::current_exe().unwrap_or_else(|_| PathBuf::from("Crystal-Launcher.exe"));
        let user_data = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Crystal Launcher");
        let mut config = Self::from_locations(&executable, &user_data);
        if let Some(root) = std::env::var_os("CRYSTAL_DATA_ROOT") {
            config.experiment = PathBuf::from(root);
        }
        config
    }
}
impl Config {
    /// Keep existing checkout data in place; packaged copies use this Windows user's data folder.
    fn from_locations(executable: &Path, user_data: &Path) -> Self {
        let executable_dir = executable.parent().unwrap_or_else(|| Path::new("."));
        let checkout = executable_dir
            .ancestors()
            .find(|path| path.join("iced-launcher/Cargo.toml").is_file());
        let package = checkout.unwrap_or(executable_dir);
        let bundled_runtime = package.join("native-runtime");
        let legacy_runtime = package
            .parent()
            .unwrap_or(package)
            .join("xi-on-mac-windows");
        let repository = if !bundled_runtime.exists() && checkout.is_some() {
            legacy_runtime
        } else {
            bundled_runtime
        };
        Self {
            experiment: checkout.unwrap_or(user_data).to_path_buf(),
            repository,
            game_install: None,
            dat_overlays: Vec::new(),
        }
    }
    /// The install the client runs, as a plain Win32 path. `fs::canonicalize` (the folder
    /// picker) yields `\\?\C:\...`; the client hands `--game` to FFXiMain as its install folder,
    /// and its guest path layer reads `\\?\C:\x` as `C:\?\C:\x`, so the game finds none of its
    /// files and GameStart returns at once (exit code 0, no graphics).
    pub fn game_directory(&self) -> PathBuf {
        self.game_install
            .as_deref()
            .map(plain_windows_path)
            .unwrap_or_else(|| self.experiment.join("client"))
    }
    fn vault_path(&self) -> PathBuf {
        self.experiment.join("iced-launcher/accounts.dpapi")
    }
    /// Where this account's per-launch client logs are written.
    pub fn logs_directory(&self, account: &Account) -> PathBuf {
        self.account_directory(account).join("logs")
    }
    /// One line per launch, next to the launcher's other data.
    pub fn history_path(&self) -> PathBuf {
        self.experiment.join("iced-launcher/launch-history.log")
    }
    pub(crate) fn account_directory(&self, account: &Account) -> PathBuf {
        let key = account_key(account);
        let mapping: std::collections::BTreeMap<String, String> =
            fs::read(self.experiment.join("iced-launcher/directories.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
        let directory = mapping
            .get(&key)
            .filter(|v| v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .cloned()
            .unwrap_or(key);
        self.experiment.join("test-launcher/data").join(directory)
    }
    fn legacy_directory_key(&self, account: &Account) -> String {
        let mut hash = Sha1::new();
        hash.update(match account.mode {
            AccountMode::Private => b"private\0",
            AccountMode::Retail => b"retail\0\0",
        });
        hash.update(account.server.as_bytes());
        hash.update([0]);
        hash.update(account.pol_id.as_bytes());
        format!("{:x}", hash.finalize())
    }
    fn preserve_legacy_directories(&self, accounts: &[Account]) -> Result<(), String> {
        let path = self.experiment.join("iced-launcher/directories.json");
        let mut mapping: std::collections::BTreeMap<String, String> = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| t("Account directory mappings could not be read."))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(_) => return Err(t("Account directory mappings could not be read.").into()),
        };
        for account in accounts {
            let key = account_key(account);
            let legacy = self.legacy_directory_key(account);
            let directory = if self
                .experiment
                .join("test-launcher/data")
                .join(&legacy)
                .is_dir()
            {
                legacy
            } else {
                key.clone()
            };
            mapping.entry(key).or_insert(directory);
        }
        fs::create_dir_all(path.parent().unwrap())
            .map_err(|_| t("Could not create launcher data directory."))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(
            &temporary,
            serde_json::to_vec(&mapping)
                .map_err(|_| t("Could not encode account directory mappings."))?,
        )
        .map_err(|_| t("Could not save account directory mappings."))?;
        replace_file(&temporary, &path)
    }
}

/// `\\?\C:\a` -> `C:\a`, `\\?\UNC\srv\share` -> `\\srv\share`; anything else unchanged.
/// Paths that only work in verbatim form (over MAX_PATH, trailing dots) stay verbatim.
pub fn plain_windows_path(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    let plain = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
            rest.to_owned()
        } else {
            return path.to_path_buf();
        }
    } else {
        return path.to_path_buf();
    };
    if plain.len() >= 260
        || plain
            .split('\\')
            .any(|c| c == "." || c == ".." || c.ends_with('.') || c.ends_with(' '))
    {
        return path.to_path_buf();
    }
    PathBuf::from(plain)
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AccountMode {
    Private,
    Retail,
}

impl std::fmt::Display for AccountMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Private => t("Private server"),
            Self::Retail => t("Retail"),
        })
    }
}

/// The supervisor tracks the exact child, so a reused Windows PID cannot
/// incorrectly mark a closed client as running.
#[derive(Clone, Debug)]
pub struct LaunchResult {
    pub pid: u32,
    pub detail: String,
    pub alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// FFXI can end while the genuine PlayOnline session remains signed in.
    pub game_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The client's stderr log for this launch.
    pub log: PathBuf,
    /// Filled by the supervisor, before `alive` turns false, with how the client ended.
    pub outcome: std::sync::Arc<std::sync::Mutex<Option<ExitReport>>>,
    /// The account row's one-line launch timing, e.g. "Signed in 2.8s · game started 9.4s".
    pub timing: String,
}

pub fn account_key(account: &Account) -> String {
    let mut hash = Sha1::new();
    hash.update(match account.mode {
        AccountMode::Private => b"private\0",
        AccountMode::Retail => b"retail\0\0",
    });
    if account.mode == AccountMode::Private {
        hash.update(account.server.trim().to_ascii_lowercase());
    }
    hash.update([0]);
    hash.update(if account.mode == AccountMode::Retail {
        account.pol_id.to_ascii_uppercase()
    } else {
        account.pol_id.clone()
    });
    format!("{:x}", hash.finalize())
}

/// Deliberately has no Debug implementation. Login fields are cleared on drop.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Account {
    pub label: String,
    pub mode: AccountMode,
    pub server: String,
    pub pol_id: String,
    pub pol_password: String,
    pub se_id: String,
    pub se_password: String,
    /// UI preference only: US/EU/JP. It is not used as a native protocol byte.
    pub region: u8,
    /// How this account starts the game; `None` follows the global default (Home / Launch
    /// tools). Private-server accounts always use the native client.
    pub launch_with: Option<LaunchWith>,
    /// The Ashita boot config (a file name in `config\boot`) this account's ini is based on;
    /// `None` uses the Launch tools default.
    pub ashita_boot_ini: Option<String>,
    /// The Windower profile this account starts with; `None` uses the Launch tools default.
    pub windower_profile: Option<String>,
}
pub(crate) use crate::ui::LaunchWith;
impl Account {
    /// What this account launches with, given the global default.
    pub(crate) fn effective_launch_with(&self, default: LaunchWith) -> LaunchWith {
        match self.mode {
            AccountMode::Private => LaunchWith::Native,
            AccountMode::Retail => self
                .launch_with
                .filter(|tool| *tool != LaunchWith::Native)
                .unwrap_or(default)
                .retail(),
        }
    }
    fn has_launch_settings(&self) -> bool {
        self.launch_with.is_some()
            || self.ashita_boot_ini.is_some()
            || self.windower_profile.is_some()
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        self.pol_id.zeroize();
        self.pol_password.zeroize();
        self.se_id.zeroize();
        self.se_password.zeroize();
    }
}

fn validate_account(account: &Account) -> Result<(), String> {
    if account.label.trim().is_empty() || account.label.len() > 256 || account.label.contains('\0')
    {
        return Err(t("Enter an account label (up to 256 bytes).").into());
    }
    if account.region > 2 {
        return Err(t("Choose US, EU or JP.").into());
    }
    if account.server.len() > 253 || account.se_id.len() > 16 || account.se_password.len() > 512 {
        return Err(t("An account field exceeds its supported length.").into());
    }
    if let Some(name) = &account.ashita_boot_ini {
        if !crate::pol_less::tools::valid_boot_ini_name(name) {
            return Err(t(
                "The Ashita boot config must be an .ini file name in Ashita's config\\boot folder.",
            )
            .into());
        }
    }
    if let Some(name) = &account.windower_profile {
        if name.is_empty() || !crate::pol_less::tools::valid_profile(name) {
            return Err(
                t("The Windower profile name can't contain quotes or control characters.").into(),
            );
        }
    }
    match account.mode {
        AccountMode::Private => {
            validate_host(&account.server)?;
            if account.pol_id.is_empty()
                || account.pol_id.len() > 63
                || account.pol_id.contains('\0')
            {
                return Err(t("Enter a private-server username (up to 63 UTF-8 bytes).").into());
            }
            if account.pol_password.is_empty()
                || account.pol_password.len() > 127
                || account.pol_password.contains('\0')
            {
                return Err(t("Enter a private-server password (up to 127 UTF-8 bytes).").into());
            }
        }
        AccountMode::Retail => {
            if account.pol_id.len() != 8
                || !account.pol_id.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return Err(t("Enter the eight-character PlayOnline ID.").into());
            }
            if account.pol_password.is_empty()
                || account.pol_password.len() > 15
                || !account
                    .pol_password
                    .bytes()
                    .all(|b| (32..=126).contains(&b))
            {
                return Err(t("Enter the PlayOnline password (up to 15 ASCII characters).").into());
            }
            if account.se_id.is_empty()
                || account.se_id.len() > 16
                || !account.se_id.bytes().all(|b| (33..=126).contains(&b))
            {
                return Err(t("Enter the Square Enix ID (up to 16 ASCII characters).").into());
            }
            if account.se_password.is_empty()
                || account.se_password.len() > 512
                || account.se_password.contains('\0')
            {
                return Err(t("Enter the Square Enix password.").into());
            }
        }
    }
    Ok(())
}
fn validate_host(server: &str) -> Result<(), String> {
    let host = server.trim();
    if host.is_empty()
        || host.len() > 253
        || !host.bytes().any(|b| b.is_ascii_alphanumeric())
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
    {
        return Err(t("Enter the private server's IPv4 address or host name.").into());
    }
    Ok(())
}
const VAULT_LIMIT: usize = 4 * 1024 * 1024;
/// Version 1: the six login fields. Version 2 adds the per-account launch settings. Vaults
/// whose accounts have no launch settings are still written as version 1, so an older
/// launcher can read them.
const VAULT_V1: &[u8; 8] = b"XIAC001\n";
const VAULT_V2: &[u8; 8] = b"XIAC002\n";
/// Bytes per account before its variable-length text: mode, region and six length prefixes;
/// version 2 adds the launch byte and two more prefixes.
const ACCOUNT_FIXED_V1: usize = 26;
const ACCOUNT_FIXED_V2: usize = ACCOUNT_FIXED_V1 + 9;

fn launch_with_code(value: Option<LaunchWith>) -> u8 {
    match value {
        None => 0,
        Some(LaunchWith::Native) => 1,
        Some(LaunchWith::Windower) => 2,
        Some(LaunchWith::Ashita) => 3,
        Some(LaunchWith::Vanilla) => 4,
    }
}
fn launch_with_from_code(code: u8) -> Result<Option<LaunchWith>, String> {
    Ok(match code {
        0 => None,
        1 => Some(LaunchWith::Native),
        2 => Some(LaunchWith::Windower),
        3 => Some(LaunchWith::Ashita),
        4 => Some(LaunchWith::Vanilla),
        _ => return Err(t("Invalid saved launch setting.").into()),
    })
}

// Length-prefixed UTF-8 avoids unprotected JSON escape/reallocation buffers.
fn encode_accounts(accounts: &[Account]) -> Result<Zeroizing<Vec<u8>>, String> {
    let v2 = accounts.iter().any(Account::has_launch_settings);
    /// Version 2's text fields; empty stands for "use the default".
    fn launch_fields(account: &Account) -> [&String; 2] {
        static EMPTY: String = String::new();
        [
            account.ashita_boot_ini.as_ref().unwrap_or(&EMPTY),
            account.windower_profile.as_ref().unwrap_or(&EMPTY),
        ]
    }
    let mut size = 12usize;
    for account in accounts {
        validate_account(account)?;
        size = size
            .checked_add(if v2 {
                ACCOUNT_FIXED_V2
            } else {
                ACCOUNT_FIXED_V1
            })
            .ok_or(t("Saved accounts are too large."))?;
        for field in [
            &account.label,
            &account.server,
            &account.pol_id,
            &account.pol_password,
            &account.se_id,
            &account.se_password,
        ] {
            size = size
                .checked_add(field.len())
                .ok_or(t("Saved accounts are too large."))?;
        }
        if v2 {
            for field in launch_fields(account) {
                size = size
                    .checked_add(field.len())
                    .ok_or(t("Saved accounts are too large."))?;
            }
        }
    }
    if size > VAULT_LIMIT {
        return Err(t("The saved-account vault is too large.").into());
    }
    let count: u32 = accounts
        .len()
        .try_into()
        .map_err(|_| t("Too many saved accounts."))?;
    let mut plaintext = Zeroizing::new(Vec::with_capacity(size));
    plaintext.extend_from_slice(if v2 { VAULT_V2 } else { VAULT_V1 });
    plaintext.extend_from_slice(&count.to_le_bytes());
    let put = |plaintext: &mut Zeroizing<Vec<u8>>, field: &String| {
        plaintext.extend_from_slice(&(field.len() as u32).to_le_bytes());
        plaintext.extend_from_slice(field.as_bytes());
    };
    for account in accounts {
        plaintext.push(match account.mode {
            AccountMode::Private => 0,
            AccountMode::Retail => 1,
        });
        plaintext.push(account.region);
        for field in [
            &account.label,
            &account.server,
            &account.pol_id,
            &account.pol_password,
            &account.se_id,
            &account.se_password,
        ] {
            put(&mut plaintext, field);
        }
        if v2 {
            plaintext.push(launch_with_code(account.launch_with));
            for field in launch_fields(account) {
                put(&mut plaintext, field);
            }
        }
    }
    debug_assert_eq!(plaintext.len(), size);
    Ok(plaintext)
}
fn decode_accounts(mut plaintext: &[u8]) -> Result<Vec<Account>, String> {
    fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8], String> {
        if input.len() < length {
            return Err(t("The saved-account vault is truncated.").into());
        }
        let (field, rest) = input.split_at(length);
        *input = rest;
        Ok(field)
    }
    fn field(input: &mut &[u8], limit: usize) -> Result<Zeroizing<String>, String> {
        let length = u32::from_le_bytes(take(input, 4)?.try_into().unwrap()) as usize;
        if length > limit {
            return Err(t("A saved-account field is too large.").into());
        }
        let text = std::str::from_utf8(take(input, length)?)
            .map_err(|_| t("A saved-account field is invalid UTF-8."))?;
        Ok(Zeroizing::new(text.to_owned()))
    }
    if plaintext.len() > VAULT_LIMIT {
        return Err(t("Unsupported saved-account vault format.").into());
    }
    let v2 = match take(&mut plaintext, 8)? {
        magic if magic == VAULT_V1 => false,
        magic if magic == VAULT_V2 => true,
        _ => return Err(t("Unsupported saved-account vault format.").into()),
    };
    let count = u32::from_le_bytes(take(&mut plaintext, 4)?.try_into().unwrap()) as usize;
    if count
        > plaintext.len()
            / if v2 {
                ACCOUNT_FIXED_V2
            } else {
                ACCOUNT_FIXED_V1
            }
    {
        return Err(t("Invalid saved-account count.").into());
    }
    let mut accounts = Vec::new();
    accounts
        .try_reserve_exact(count)
        .map_err(|_| t("Could not allocate saved accounts."))?;
    for _ in 0..count {
        let mode = match take(&mut plaintext, 1)?[0] {
            0 => AccountMode::Private,
            1 => AccountMode::Retail,
            _ => return Err(t("Invalid saved-account type.").into()),
        };
        let region = take(&mut plaintext, 1)?[0];
        let mut label = field(&mut plaintext, 256)?;
        let mut server = field(&mut plaintext, 253)?;
        let mut pol_id = field(&mut plaintext, 63)?;
        let mut pol_password = field(&mut plaintext, 127)?;
        let mut se_id = field(&mut plaintext, 16)?;
        let mut se_password = field(&mut plaintext, 512)?;
        let (mut launch_with, mut ashita_boot_ini, mut windower_profile) = (None, None, None);
        if v2 {
            launch_with = launch_with_from_code(take(&mut plaintext, 1)?[0])?;
            // Not secrets; empty means "use the default".
            let text = |value: Zeroizing<String>| (!value.is_empty()).then(|| value.to_string());
            ashita_boot_ini = text(field(&mut plaintext, 255)?);
            windower_profile = text(field(&mut plaintext, 128)?);
        }
        let account = Account {
            label: std::mem::take(&mut *label),
            mode,
            server: std::mem::take(&mut *server),
            pol_id: std::mem::take(&mut *pol_id),
            pol_password: std::mem::take(&mut *pol_password),
            se_id: std::mem::take(&mut *se_id),
            se_password: std::mem::take(&mut *se_password),
            region,
            launch_with,
            ashita_boot_ini,
            windower_profile,
        };
        validate_account(&account)?;
        accounts.push(account);
    }
    if !plaintext.is_empty() {
        return Err(t("Unexpected bytes in the saved-account vault.").into());
    }
    Ok(accounts)
}

/// Appends the `Log: <path>` marker that `launch_log::split_log` reads back to find the log.
/// The marker is a protocol token, so it stays English whatever the UI language.
pub(crate) fn with_log(message: impl AsRef<str>, log: &Path) -> String {
    format!("{} Log: {}", message.as_ref(), log.display())
}

/// Called by the launcher UI, never by its offline smoke check.
pub fn load_accounts(config: &Config) -> Result<Vec<Account>, String> {
    let path = config.vault_path();
    load_accounts_file(&path)
}

/// First-run import is read-only. The original Test 2 vault is never rewritten.
pub fn load_or_import_accounts(config: &Config) -> Result<Vec<Account>, String> {
    let accounts = if config.vault_path().exists() {
        load_accounts(config)?
    } else {
        load_accounts_file(&config.experiment.join("test-launcher/accounts.dpapi"))?
    };
    config.preserve_legacy_directories(&accounts)?;
    Ok(accounts)
}

fn load_accounts_file(path: &Path) -> Result<Vec<Account>, String> {
    let metadata = match fs::metadata(&path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(t("Could not open the saved-account vault.").into()),
    };
    if metadata.len() > 4 * 1024 * 1024 {
        return Err(t("The saved-account vault is too large.").into());
    }
    let mut encrypted = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take(VAULT_LIMIT as u64 + 1)
                .read_to_end(&mut encrypted)
        })
        .map_err(|_| t("Could not read the saved-account vault."))?;
    if encrypted.len() > VAULT_LIMIT {
        return Err(t("The saved-account vault is too large.").into());
    }
    let plaintext = unprotect(&encrypted)?;
    decode_accounts(&plaintext)
}

pub fn save_accounts(config: &Config, accounts: &[Account]) -> Result<(), String> {
    let plaintext = encode_accounts(accounts)?;
    let encrypted = protect(&plaintext)?;
    if encrypted.len() > VAULT_LIMIT {
        return Err(t("The encrypted saved-account vault is too large.").into());
    }
    let path = config.vault_path();
    let directory = path.parent().ok_or(t("Invalid saved-account location."))?;
    fs::create_dir_all(directory)
        .map_err(|_| t("Could not create the saved-account directory."))?;
    let temporary = directory.join(format!("accounts-{}.tmp", unique_id()?));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|_| t("Could not create the encrypted saved-account file."))?;
        file.write_all(&encrypted)
            .and_then(|_| file.sync_all())
            .map_err(|_| t("Could not write the encrypted saved-account file."))?;
        drop(file);
        replace_file(&temporary, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn protect(input: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB},
    };
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input
            .len()
            .try_into()
            .map_err(|_| t("Account data is too large."))?,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    unsafe {
        // DPAPI's default scope binds this vault to the current Windows user.
        if CryptProtectData(
            &input_blob,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return Err(tf(
                "Windows could not protect the saved accounts (error {code}).",
                &[("code", &windows_sys::Win32::Foundation::GetLastError())],
            ));
        }
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        Ok(bytes)
    }
}
#[cfg(windows)]
fn unprotect(input: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
        },
    };
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: input
            .len()
            .try_into()
            .map_err(|_| t("Account data is too large."))?,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output: CRYPT_INTEGER_BLOB = unsafe { std::mem::zeroed() };
    unsafe {
        if CryptUnprotectData(
            &input_blob,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        ) == 0
        {
            return Err(tf(
                "Windows could not decrypt these accounts for the current user (error {code}).",
                &[("code", &windows_sys::Win32::Foundation::GetLastError())],
            ));
        }
        let bytes = Zeroizing::new(
            std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec(),
        );
        std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize).zeroize();
        LocalFree(output.pbData as _);
        Ok(bytes)
    }
}
#[cfg(not(windows))]
fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err(t("Saved accounts require Windows DPAPI.").into())
}
#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Zeroizing<Vec<u8>>, String> {
    Err(t("Saved accounts require Windows DPAPI.").into())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(t("Could not replace the saved-account vault; its previous version was kept.").into())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination)
        .map_err(|_| t("Could not replace the saved-account vault.").into())
}

pub(crate) fn unique_id() -> Result<String, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| t("Invalid system clock."))?
        .as_nanos();
    Ok(format!(
        "{stamp}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Sign in and start the client. Every launch that ends in an error is also written to the
/// launch history; launches that start are recorded by their supervisor when the client exits.
/// Retail accounts use Crystal's polcore path; private-server accounts use xiloader.
pub fn login_with_progress(
    config: &Config,
    account: Account,
    otp: String,
    via: Option<crate::pol_less::Plan>,
    clicked: std::time::Instant,
    progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    let label = account.label.clone();
    let history = config.history_path();
    let result = login_inner(
        config,
        account,
        otp,
        via,
        Timeline::begin_at(clicked),
        progress,
    );
    if let Err(error) = &result {
        let (message, log) = crate::launch_log::split_log(error);
        let message: String = message.chars().take(240).collect();
        crate::launch_log::append_history(&history, &label, &message, log.as_deref());
    }
    result
}

fn login_inner(
    config: &Config,
    account: Account,
    otp: String,
    via: Option<crate::pol_less::Plan>,
    timing: Timeline,
    mut progress: impl FnMut(&str),
) -> Result<LaunchResult, String> {
    validate_account(&account)?;
    let otp = Zeroizing::new(otp);
    match account.mode {
        AccountMode::Private => {
            crate::private_server::launch(config, account, otp, timing, &mut progress)
        }
        AccountMode::Retail => {
            let plan = via.ok_or_else(|| t("The native client is temporarily unavailable. Retail accounts must launch through Windower or Ashita; saved private-server accounts are kept.").to_owned())?;
            crate::pol_less::launch(config, &plan, account, otp, timing, &mut progress)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_paths_follow_the_executable_and_keep_each_users_vault_separate() {
        let package =
            std::env::temp_dir().join(format!("crystal-package-{}", unique_id().unwrap()));
        let first_user = package.join("users/one");
        let second_user = package.join("users/two");
        let executable = package.join("portable/Crystal-Launcher.exe");
        let first = Config::from_locations(&executable, &first_user);
        let second = Config::from_locations(&executable, &second_user);
        assert_eq!(first.repository, package.join("portable/native-runtime"));
        assert!(
            first.game_install.is_none(),
            "a packaged launcher detects this PC's install instead of guessing a bundled client"
        );
        assert_eq!(first.experiment, first_user);
        assert_ne!(first.vault_path(), second.vault_path());
        assert!(first.vault_path().starts_with(&first_user));
    }

    #[cfg(windows)]
    #[test]
    fn moving_or_reinstalling_the_package_keeps_the_existing_encrypted_accounts() {
        let root = std::env::temp_dir().join(format!("crystal-reinstall-{}", unique_id().unwrap()));
        let data = root.join("user-data");
        let original = Config::from_locations(&root.join("install/Crystal-Launcher.exe"), &data);
        let replacement =
            Config::from_locations(&root.join("reinstalled/Crystal-Launcher.exe"), &data);
        save_accounts(&original, &[retail()]).unwrap();
        let before = fs::read(original.vault_path()).unwrap();
        assert_eq!(original.vault_path(), replacement.vault_path());
        let restored = load_or_import_accounts(&replacement).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].pol_id, "ABCD1234");
        assert_eq!(restored[0].pol_password, retail().pol_password);
        assert_eq!(fs::read(replacement.vault_path()).unwrap(), before);
        drop(restored);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checkout_builds_preserve_existing_accounts_and_prefer_a_bundled_runtime() {
        let root = std::env::temp_dir().join(format!("crystal-checkout-{}", unique_id().unwrap()));
        let project = root.join("test");
        fs::create_dir_all(project.join("iced-launcher")).unwrap();
        fs::write(project.join("iced-launcher/Cargo.toml"), "[package]").unwrap();
        let executable = project.join("iced-launcher/target/release/xi-launcher.exe");
        let config = Config::from_locations(&executable, &root.join("user-data"));
        assert_eq!(config.experiment, project);
        assert_eq!(config.repository, root.join("xi-on-mac-windows"));
        assert_eq!(
            config.vault_path(),
            project.join("iced-launcher/accounts.dpapi")
        );
        fs::create_dir(project.join("native-runtime")).unwrap();
        assert_eq!(
            Config::from_locations(&executable, &root.join("user-data")).repository,
            project.join("native-runtime")
        );
    }

    #[test]
    fn launch_routes_fail_safely_before_authentication() {
        let config = Config {
            experiment: PathBuf::from("missing-fixture"),
            repository: PathBuf::from("missing-fixture"),
            game_install: None,
            dat_overlays: Vec::new(),
        };
        let private = login_inner(
            &config,
            account(),
            String::new(),
            None,
            Timeline::begin(),
            |_| {},
        )
        .unwrap_err();
        assert!(private.contains("FINAL FANTASY XI folder"));

        let retail = login_inner(
            &config,
            retail(),
            String::new(),
            None,
            Timeline::begin(),
            |_| {},
        )
        .unwrap_err();
        assert!(retail.contains("temporarily unavailable"));
    }

    #[test]
    fn selected_install_changes_game_path_without_moving_account_storage() {
        let root =
            std::env::temp_dir().join(format!("xi-game-path-fixture-{}", unique_id().unwrap()));
        let game = root.join("Chosen Game With Spaces");
        let config = Config {
            experiment: root.clone(),
            repository: root.clone(),
            game_install: Some(game.clone()),
            dat_overlays: Vec::new(),
        };
        assert_eq!(config.game_directory(), game);
        assert!(config
            .account_directory(&account())
            .starts_with(root.join("test-launcher/data")));
    }

    fn account() -> Account {
        Account {
            label: "Synthetic test".into(),
            mode: AccountMode::Private,
            server: "127.0.0.1".into(),
            pol_id: "fixture".into(),
            pol_password: "synthetic secret".into(),
            se_id: String::new(),
            se_password: String::new(),
            region: 0,
            launch_with: None,
            ashita_boot_ini: None,
            windower_profile: None,
        }
    }
    fn retail() -> Account {
        Account {
            label: "Retail test".into(),
            mode: AccountMode::Retail,
            server: String::new(),
            pol_id: "ABCD1234".into(),
            pol_password: "synthetic".into(),
            se_id: "synthetic".into(),
            se_password: "synthetic".into(),
            region: 0,
            launch_with: None,
            ashita_boot_ini: None,
            windower_profile: None,
        }
    }
    /// A version-1 vault, byte for byte as launchers before per-account launch settings wrote it.
    fn version_one_vault(accounts: &[(u8, u8, [&str; 6])]) -> Vec<u8> {
        let mut bytes = b"XIAC001\n".to_vec();
        bytes.extend_from_slice(&(accounts.len() as u32).to_le_bytes());
        for (mode, region, fields) in accounts {
            bytes.push(*mode);
            bytes.push(*region);
            for field in fields {
                bytes.extend_from_slice(&(field.len() as u32).to_le_bytes());
                bytes.extend_from_slice(field.as_bytes());
            }
        }
        bytes
    }
    #[test]
    fn version_one_vaults_load_with_default_launch_settings() {
        let old = version_one_vault(&[
            (1, 0, ["Main", "", "ABCD1234", "polpass", "seid", "sepass"]),
            (
                0,
                0,
                ["Private", "play.example.net", "user", "secret", "", ""],
            ),
        ]);
        let accounts = decode_accounts(&old).unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(
            (accounts[0].label.as_str(), accounts[0].se_password.as_str()),
            ("Main", "sepass")
        );
        for account in &accounts {
            assert_eq!(account.launch_with, None);
            assert_eq!(account.ashita_boot_ini, None);
            assert_eq!(account.windower_profile, None);
        }
        // Saved again without launch settings, the vault stays version 1 (older launchers read it).
        let again = encode_accounts(&accounts).unwrap();
        assert_eq!(&again[..], &old[..]);
    }
    #[test]
    fn launch_settings_round_trip_in_a_version_two_vault() {
        let mut main = retail();
        main.launch_with = Some(LaunchWith::Windower);
        main.windower_profile = Some("Main Mule".into());
        let mut mule = retail();
        mule.pol_id = "WXYZ9876".into();
        mule.launch_with = Some(LaunchWith::Ashita);
        mule.ashita_boot_ini = Some("mule.ini".into());
        let plain = account();
        let encoded = encode_accounts(&[main, mule, plain]).unwrap();
        assert_eq!(&encoded[..8], b"XIAC002\n");
        let decoded = decode_accounts(&encoded).unwrap();
        assert_eq!(decoded[0].launch_with, Some(LaunchWith::Windower));
        assert_eq!(decoded[0].windower_profile.as_deref(), Some("Main Mule"));
        assert_eq!(decoded[0].ashita_boot_ini, None);
        assert_eq!(decoded[1].launch_with, Some(LaunchWith::Ashita));
        assert_eq!(decoded[1].ashita_boot_ini.as_deref(), Some("mule.ini"));
        assert_eq!(
            (
                decoded[2].launch_with.clone(),
                decoded[2].windower_profile.clone()
            ),
            (None, None)
        );
        assert_eq!(decoded[2].pol_password, "synthetic secret");
        for end in 0..encoded.len() {
            assert!(decode_accounts(&encoded[..end]).is_err());
        }
        let mut bad_launch = Zeroizing::new(encoded.to_vec());
        // The first account's launch byte follows its six fields.
        let fixed: usize = 12
            + 2
            + [
                "Retail test",
                "",
                "ABCD1234",
                "synthetic",
                "synthetic",
                "synthetic",
            ]
            .iter()
            .map(|f| 4 + f.len())
            .sum::<usize>();
        assert_eq!(bad_launch[fixed], 2);
        bad_launch[fixed] = 9;
        assert!(decode_accounts(&bad_launch).is_err());
    }
    #[test]
    fn launch_settings_are_validated() {
        let mut value = retail();
        value.ashita_boot_ini = Some(r"..\evil.ini".into());
        assert!(encode_accounts(&[value.clone()]).is_err());
        value.ashita_boot_ini = Some("notes.txt".into());
        assert!(encode_accounts(&[value.clone()]).is_err());
        value.ashita_boot_ini = Some("Main.ini".into());
        value.windower_profile = Some("a\" --executable=evil".into());
        assert!(encode_accounts(&[value.clone()]).is_err());
        value.windower_profile = Some(String::new());
        assert!(
            encode_accounts(&[value.clone()]).is_err(),
            "an empty profile is stored as None"
        );
        value.windower_profile = Some("Main".into());
        assert!(encode_accounts(&[value]).is_ok());
    }

    #[test]
    fn vanilla_vault_choice_round_trips_without_reassigning_older_choices() {
        let choices = [
            None,
            Some(LaunchWith::Native),
            Some(LaunchWith::Windower),
            Some(LaunchWith::Ashita),
            Some(LaunchWith::Vanilla),
        ];
        let accounts: Vec<_> = choices
            .iter()
            .map(|&launch_with| {
                let mut account = retail();
                account.launch_with = launch_with;
                account
            })
            .collect();
        for (code, &choice) in choices.iter().enumerate() {
            assert_eq!(launch_with_code(choice), code as u8);
            assert_eq!(launch_with_from_code(code as u8).unwrap(), choice);
        }
        let encoded = encode_accounts(&accounts).unwrap();
        let decoded = decode_accounts(&encoded).unwrap();
        assert_eq!(
            decoded.iter().map(|a| a.launch_with).collect::<Vec<_>>(),
            choices
        );
    }
    #[test]
    fn effective_launcher_follows_the_account_then_the_default() {
        let mut value = retail();
        assert_eq!(
            value.effective_launch_with(LaunchWith::Windower),
            LaunchWith::Windower
        );
        value.launch_with = Some(LaunchWith::Ashita);
        assert_eq!(
            value.effective_launch_with(LaunchWith::Windower),
            LaunchWith::Ashita
        );
        // Private servers always use the native client.
        let mut private = account();
        private.launch_with = Some(LaunchWith::Ashita);
        assert_eq!(
            private.effective_launch_with(LaunchWith::Windower),
            LaunchWith::Native
        );
    }
    #[test]
    fn imported_account_directory_survives_hostname_case_edit() {
        let directory =
            std::env::temp_dir().join(format!("xi-directories-fixture-{}", unique_id().unwrap()));
        let config = Config {
            experiment: directory.clone(),
            repository: directory.clone(),
            game_install: None,
            dat_overlays: Vec::new(),
        };
        let mut value = account();
        value.server = "Example.COM".into();
        let legacy = config
            .experiment
            .join("test-launcher/data")
            .join(config.legacy_directory_key(&value));
        fs::create_dir_all(&legacy).unwrap();
        config
            .preserve_legacy_directories(&[value.clone()])
            .unwrap();
        value.server = "example.com".into();
        assert_eq!(config.account_directory(&value), legacy);
        fs::remove_file(directory.join("iced-launcher/directories.json")).unwrap();
        fs::remove_dir(directory.join("iced-launcher")).unwrap();
        fs::remove_dir(legacy).unwrap();
        fs::remove_dir(directory.join("test-launcher/data")).unwrap();
        fs::remove_dir(directory.join("test-launcher")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn verbatim_paths_become_plain_win32_paths() {
        let plain = |s: &str| plain_windows_path(Path::new(s));
        assert_eq!(
            plain(r"\\?\C:\Games\SquareEnix\FINAL FANTASY XI"),
            PathBuf::from(r"C:\Games\SquareEnix\FINAL FANTASY XI")
        );
        assert_eq!(
            plain(r"\\?\UNC\nas\games\FFXI"),
            PathBuf::from(r"\\nas\games\FFXI")
        );
        assert_eq!(plain(r"C:\Games\FFXI"), PathBuf::from(r"C:\Games\FFXI"));
        // only valid in verbatim form: kept as is
        assert_eq!(
            plain(r"\\?\C:\odd.\FFXI"),
            PathBuf::from(r"\\?\C:\odd.\FFXI")
        );
        assert_eq!(
            plain(r"\\?\Volume{0}\FFXI"),
            PathBuf::from(r"\\?\Volume{0}\FFXI")
        );
        let long = format!(r"\\?\C:\{}", "a".repeat(300));
        assert_eq!(plain(&long), PathBuf::from(&long));
    }
    #[test]
    fn validation_rejects_bad_fields_before_login_or_storage() {
        let mut value = account();
        value.pol_password.push('\0');
        assert!(validate_account(&value).is_err());
        for server in ["", "a b", "x;calc.exe", "$(anything)", "--"] {
            assert!(validate_host(server).is_err());
        }
        value = account();
        value.mode = AccountMode::Retail;
        assert!(validate_account(&value).is_err());
    }
    #[test]
    fn account_data_is_separate_and_stable_when_label_changes() {
        let config = Config::default();
        let mut a = account();
        let mut b = account();
        b.pol_id = "second".into();
        assert_ne!(config.account_directory(&a), config.account_directory(&b));
        let before = config.account_directory(&a);
        a.label = "Renamed".into();
        assert_eq!(before, config.account_directory(&a));
    }
    #[test]
    fn vault_codec_rejects_partial_and_trailing_data_without_json_scratch() {
        let mut value = account();
        value.pol_password = "quote\"\\\n\u{00e9}".into();
        let encoded = encode_accounts(&[value]).unwrap();
        let decoded = decode_accounts(&encoded).unwrap();
        assert_eq!(decoded[0].pol_password, "quote\"\\\n\u{00e9}");
        for end in 0..encoded.len() {
            assert!(decode_accounts(&encoded[..end]).is_err());
        }
        let mut extra = Zeroizing::new(encoded.to_vec());
        extra.push(0);
        assert!(decode_accounts(&extra).is_err());
        let mut bad_count = Zeroizing::new(encoded.to_vec());
        bad_count[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode_accounts(&bad_count).is_err());
    }
    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip_replaces_encrypted_vault_and_rejects_corruption() {
        let directory =
            std::env::temp_dir().join(format!("ffxi-account-fixture-{}", unique_id().unwrap()));
        let config = Config {
            experiment: directory.clone(),
            repository: directory.clone(),
            game_install: None,
            dat_overlays: Vec::new(),
        };
        assert!(load_accounts(&config).unwrap().is_empty());
        save_accounts(&config, &[account()]).unwrap();
        let ciphertext = fs::read(config.vault_path()).unwrap();
        assert!(!ciphertext
            .windows(b"synthetic secret".len())
            .any(|part| part == b"synthetic secret"));
        let restored = load_accounts(&config).unwrap();
        assert_eq!(restored[0].pol_password, "synthetic secret");
        drop(restored);
        let mut with_settings = retail();
        with_settings.launch_with = Some(LaunchWith::Windower);
        with_settings.windower_profile = Some("Mule".into());
        save_accounts(&config, &[account(), with_settings]).unwrap();
        let restored = load_accounts(&config).unwrap();
        assert_eq!(restored[1].launch_with, Some(LaunchWith::Windower));
        assert_eq!(restored[1].windower_profile.as_deref(), Some("Mule"));
        drop(restored);
        save_accounts(&config, &[]).unwrap();
        assert!(load_accounts(&config).unwrap().is_empty());
        fs::write(config.vault_path(), b"invalid DPAPI blob").unwrap();
        assert!(load_accounts(&config).is_err());
        fs::remove_file(config.vault_path()).unwrap();
        fs::remove_dir(directory.join("iced-launcher")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
