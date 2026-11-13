//! The launcher's saved settings (`iced-launcher/settings.json`): the struct, its defaults and
//! the capped, atomic file load and save.

use crate::backend::{self, account_key, Account, Config};
use crate::i18n::{self, t};
use crate::{fsutil, open_with_game, pol_less, reshade};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Settings {
    pub install_path: String,
    #[serde(default)]
    pub light_theme: bool,
    #[serde(default)]
    pub install_selected: bool,
    #[serde(default = "default_installer_path")]
    pub installer_path: String,
    #[serde(default)]
    pub installer_selected: bool,
    /// Downloads → Install: the official installer's region (0 US, 1 EU, 2 JP). Absent = an
    /// installed client's region, else US (`App::installer_region`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_region: Option<u8>,
    #[serde(default)]
    pub(crate) selected_accounts: Option<BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_order: Vec<String>,
    /// Public setup names; account credentials stay in the unchanged DPAPI vault.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub account_pol_handles: HashMap<String, String>,
    // The default launcher (Settings → System → Launch tools), for accounts whose own
    // launch_with is None. Home's Launch with edits the shown account's own choice.
    #[serde(default)]
    pub launch_with: crate::backend::LaunchWith,
    // Settings → System → Launch tools. Empty means detect / default.
    #[serde(default)]
    pub ashita_path: String,
    #[serde(default)]
    pub windower_path: String,
    #[serde(default)]
    pub ashita_boot_ini: String,
    #[serde(default)]
    pub windower_profile: String,
    #[serde(default)]
    pub loader_path: String,
    /// Non-secret per-account installation overrides; the encrypted vault stays compatible.
    #[serde(default)]
    pub account_installs: HashMap<String, String>,
    #[serde(default)]
    pub account_overlays: HashMap<String, Vec<String>>,
    /// Per-account PlayOnline Auto-Away minutes (0 = off); absent = 10.
    #[serde(default)]
    pub account_auto_away: HashMap<String, u8>,
    /// Files before 2026-10-13: the per-account "Sign in when Crystal opens" choice. Read only to
    /// migrate it (`migrate_startup_signin`) and never written again.
    #[serde(default, rename = "account_startup_signin", skip_serializing)]
    pub(crate) legacy_startup_signin: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub account_auto_login: HashMap<String, pol_less::AutoLogin>,
    /// Accounts whose last successful sign-in took a one-time code (learned, never the code):
    /// a "Wait and play" released after a restart asks for a code instead of signing in
    /// without one, and Crystal never signs them in by itself.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub account_otp: BTreeSet<String>,
    /// Accounts whose last successful sign-in took no one-time code (learned the same way; never
    /// in `account_otp` too). Only these are signed in by Crystal itself (`presigns`). An account
    /// in neither set has not been seen signing in yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub account_no_otp: BTreeSet<String>,
    /// Private-server accounts that run xiloader without `--hairpin` (it is on by default).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub account_no_hairpin: BTreeSet<String>,
    /// Sign known accounts in when the launcher opens.
    #[serde(default = "default_true")]
    pub auto_sign_in: bool,
    /// Retain native POL after a clean game exit; older hosts can sign back in.
    /// Older settings leave this off so closing a game also closes its host.
    #[serde(default)]
    pub sign_in_after_game: bool,
    /// Settings → System: Crystal's shut-down action ends its game host at once (like
    /// Windower's //terminate) instead of the in-game /shutdown countdown. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close_games_instantly: bool,
    /// The launcher's own UI language (Settings page). Absent = follow the
    /// Windows display language. The game's language is never changed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<i18n::Lang>,
    /// Screenshot privacy: names, IDs, mail and the user folder are shown
    /// blurred (`privacy.rs`). Display only. Absent = off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub privacy: bool,
    /// Settings → Graphics. On by default (older files without it too): Crystal
    /// installs dgVoodoo and puts it next to the loader at each launch.
    #[serde(default = "default_true")]
    pub dgvoodoo: bool,
    #[serde(default)]
    pub reshade: reshade::Preferences,
    #[serde(default = "default_true")]
    pub launcher_auto_updates: bool,
    /// Settings → System: a downloaded launcher update installs by itself when no game runs
    /// and nobody is using the window (`self_update::auto`). On by default.
    #[serde(default = "default_true")]
    pub launcher_auto_restart: bool,
    #[serde(default = "default_true")]
    pub controller_navigation: bool,
    /// Settings → System: Home's item (FFXIAH) and wiki search. Off hides it and nothing runs.
    #[serde(default = "default_true")]
    pub item_search: bool,
    /// Settings → System: the window's close button keeps Crystal running in the
    /// notification area (sessions stay signed in). Off: closing quits, as before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close_to_tray: bool,
    /// Settings → System: a logon scheduled task starts Crystal with `--background`
    /// (`startup_task`). Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_with_windows: bool,
    /// Settings → System → Open with the game: programs started once per Play batch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_with_game: Vec<open_with_game::Program>,
    /// Home → FFXIAH look-up: the chosen world. Absent or unknown = `DEFAULT_AH_SERVER`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ah_server: Option<String>,
}
fn default_true() -> bool {
    true
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            install_path: String::new(),
            install_selected: false,
            light_theme: false,
            installer_path: default_installer_path(),
            installer_selected: false,
            installer_region: None,
            selected_accounts: None,
            account_order: Vec::new(),
            account_pol_handles: HashMap::new(),
            launch_with: crate::backend::LaunchWith::default(),
            ashita_path: String::new(),
            windower_path: String::new(),
            ashita_boot_ini: String::new(),
            windower_profile: String::new(),
            loader_path: String::new(),
            account_installs: HashMap::new(),
            account_overlays: HashMap::new(),
            account_auto_away: HashMap::new(),
            legacy_startup_signin: BTreeSet::new(),
            account_auto_login: HashMap::new(),
            account_otp: BTreeSet::new(),
            account_no_otp: BTreeSet::new(),
            account_no_hairpin: BTreeSet::new(),
            auto_sign_in: true,
            sign_in_after_game: false,
            close_games_instantly: false,
            item_search: true,
            language: None,
            privacy: false,
            dgvoodoo: true,
            reshade: reshade::Preferences::default(),
            launcher_auto_updates: true,
            launcher_auto_restart: true,
            controller_navigation: true,
            close_to_tray: false,
            start_with_windows: false,
            open_with_game: Vec::new(),
            ah_server: None,
        }
    }
}

fn default_installer_path() -> String {
    installer_folder(crate::retail_region::RetailRegion::Us)
}

/// The default download folder for `region`'s official installer: `official-installer` (US, as
/// before regions), `official-installer-EU`, `official-installer-JP`.
pub(crate) fn installer_folder(region: crate::retail_region::RetailRegion) -> String {
    let name = match region {
        crate::retail_region::RetailRegion::Us => "official-installer".to_owned(),
        other => format!("official-installer-{}", other.wire()),
    };
    Config::default().experiment.join(name).to_string_lossy().into_owned()
}

/// `\\?\C:\x` -> `C:\x` for a path kept as text; anything else is left alone.
pub(crate) fn plain_text_path(raw: &str) -> String {
    backend::plain_windows_path(std::path::Path::new(raw)).to_string_lossy().into_owned()
}

impl Settings {
    /// Owner decision 2026-10-13: Crystal signs `key` in by itself (when it opens, and again
    /// after its game closes) only while the switch is on and it knows the account signs in
    /// without a one-time code: its last successful sign-in took none. An OTP account, and one
    /// not yet seen signing in (its first Play teaches it), signs in only when Play is pressed.
    pub(crate) fn presigns(&self, key: &str) -> bool {
        self.auto_sign_in && self.account_no_otp.contains(key) && !self.account_otp.contains(key)
    }

    /// What `key`'s latest sign-in taught: it took a one-time code (`uses`), or it went without
    /// one. Only this yes or no is kept, never a code. Whether anything changed.
    pub(crate) fn note_sign_in(&mut self, key: &str, uses: bool) -> bool {
        let (add, remove) = if uses { (&mut self.account_otp, &mut self.account_no_otp) } else { (&mut self.account_no_otp, &mut self.account_otp) };
        let removed = remove.remove(key);
        add.insert(key.to_owned()) || removed
    }

    /// Files before 2026-10-13 kept "Sign in when Crystal opens" per account. An account that had
    /// it was already signing in at startup without a code, so it is known to sign in without one,
    /// unless its latest sign-in since took a code (`account_otp` is newer evidence).
    pub(crate) fn migrate_startup_signin(&mut self) {
        for key in std::mem::take(&mut self.legacy_startup_signin) {
            if !self.account_otp.contains(&key) { self.account_no_otp.insert(key); }
        }
    }

    pub(crate) fn config_for(&self, base: &Config, account: &Account) -> Config {
        let mut config = base.clone();
        if self.install_selected { config.game_install = Some(PathBuf::from(&self.install_path)); }
        if let Some(path) = self.account_installs.get(&account_key(account)).filter(|p| !p.trim().is_empty()) {
            config.game_install = Some(backend::plain_windows_path(PathBuf::from(path).as_path()));
        }
        config.dat_overlays = if self.launch_with_for(account) == crate::backend::LaunchWith::Native {
            self.account_overlays.get(&account_key(account)).into_iter().flatten()
                .map(|p| backend::plain_windows_path(PathBuf::from(p).as_path())).collect()
        } else { Vec::new() };
        config
    }

    /// Saved settings may hold `\\?\` paths from older builds' folder picker.
    pub(crate) fn plain_paths(&mut self) {
        for choice in self.account_auto_login.values_mut() { *choice = choice.normalized(); }
        for path in self.account_installs.values_mut() { *path = plain_text_path(path); }
        for overlays in self.account_overlays.values_mut() { for path in overlays { *path = plain_text_path(path); } }
        for field in [
            &mut self.install_path,
            &mut self.installer_path,
            &mut self.ashita_path,
            &mut self.windower_path,
            &mut self.loader_path,
        ] {
            let plain = plain_text_path(field);
            if plain != *field {
                *field = plain;
            }
        }
    }
}

pub(crate) fn settings_path(config: &Config) -> PathBuf {
    config.experiment.join("iced-launcher/settings.json")
}

/// Loads the saved settings and sets the UI language from them: the Windows
/// display language until a choice is saved. The language is set before the
/// load, so a load error is already in it. The window and the background
/// session worker both start this way, so they always agree.
pub(crate) fn load_settings_and_language(config: &Config) -> Result<Settings, String> {
    i18n::set(i18n::system_default());
    let settings = load_settings(config)?;
    if let Some(language) = settings.language {
        i18n::set(language);
    }
    Ok(settings)
}

pub(crate) fn load_settings(config: &Config) -> Result<Settings, String> {
    let bytes = match read_settings_bytes(&settings_path(config))? {
        Some(bytes) => bytes,
        None => return Ok(Settings::default()),
    };
    if bytes.len() > 1024 * 1024 {
        return Err(t("Settings file is too large.").into());
    }
    let mut settings: Settings =
        serde_json::from_slice(&bytes).map_err(|_| t("Settings could not be read."))?;
    settings.plain_paths();
    settings.migrate_startup_signin();
    settings.launch_with = settings.launch_with.retail();
    Ok(settings)
}
pub(crate) fn save_settings(config: &Config, settings: &Settings) -> Result<(), String> {
    use std::io::Write;
    let path = settings_path(config);
    let data = serde_json::to_vec_pretty(settings).map_err(|_| t("Could not encode settings."))?;
    if data.len() > 1024 * 1024 { return Err(t("Launcher settings are too large to save.").into()); }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|_| t("Could not create the launcher settings folder."))?;
    if let Some(previous) = read_settings_bytes(&path)? {
        if serde_json::from_slice::<Settings>(&previous).is_err() {
            let backup = path.with_extension(format!("invalid-{}.json", backend::unique_id()?));
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(backup)
                .map_err(|_| t("Could not preserve the damaged settings file; it was kept unchanged."))?;
            file.write_all(&previous).and_then(|_| file.sync_all())
                .map_err(|_| t("Could not preserve the damaged settings file; it was kept unchanged."))?;
        }
    }
    fsutil::write_atomic(&path, &data).map_err(|failed| {
        if failed.replacing { t("Could not replace launcher settings.") } else { t("Could not write launcher settings.") }.into()
    })
}

fn read_settings_bytes(path: &std::path::Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(t("Launcher settings are unavailable.").into()),
    };
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|_| t("Could not read launcher settings."))?;
    if bytes.len() > 1024 * 1024 { return Err(t("Launcher settings file is too large; it was kept unchanged.").into()); }
    Ok(Some(bytes))
}
