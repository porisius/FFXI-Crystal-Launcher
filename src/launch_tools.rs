//! Settings → System → Launch tools: where Ashita v4, Windower 4 and crystal-loader are,
//! which Ashita boot config and Windower profile a POL-less launch starts from. Empty
//! fields mean "find it automatically"; what was found (or not) is shown under each field.

use crate::backend::Account;
use crate::i18n::t;
use crate::pol_less::{self, Detected, Plan, Tool, ToolSettings};
use crate::backend::LaunchWith;
use crate::{Message, Settings};
use crate::effect::Effect;

/// No profile or custom executable arguments: this opens Windower's own editor
/// rather than asking it to start another game or force an update.
pub(crate) fn profile_editor_command(folder: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(folder.join(Tool::Windower.exe()));
    // Not the worker's stdout: std would duplicate that pipe into Windower and everything it
    // starts, holding the window's read end open after the worker exits.
    command.current_dir(folder).stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    command
}

#[derive(serde::Serialize, serde::Deserialize)]
enum EditorEvent { Progress(String), Finished(Result<(), String>) }
const EDITOR_PREFIX: &str = "CRYSTAL-PROFILE ";

/// The helper owns the profile lock until Windower closes, even if Crystal's
/// window has closed. It never reads or receives account credentials.
pub(crate) fn run_profile_editor_worker(folder: &std::path::Path) -> i32 {
    use std::io::Write;
    let emit = |event: EditorEvent| {
        if let Ok(line) = serde_json::to_string(&event) {
            let _ = writeln!(std::io::stdout().lock(), "{EDITOR_PREFIX}{line}");
        }
    };
    let result = (|| {
        if !folder.is_absolute() || !folder.join("Windower.exe").is_file() {
            return Err(t("Could not open Windower's profile editor.").to_owned());
        }
        let _access = pol_less::profile_access::editing(folder, &mut |line| emit(EditorEvent::Progress(line.into())))?;
        let mut child = profile_editor_command(folder).spawn()
            .map_err(|_| t("Could not open Windower's profile editor.").to_owned())?;
        emit(EditorEvent::Progress(t("Edit profiles in Windower. Saved changes apply on the next fresh game launch.").into()));
        let waited = child.wait();
        if waited.is_err() { let _ = child.kill(); let _ = child.wait(); }
        let status = waited.map_err(|_| t("Could not monitor Windower's profile editor.").to_owned())?;
        if !status.success() { return Err(t("Windower's profile editor exited with an error.").into()); }
        Ok(())
    })();
    let code = if result.is_ok() { 0 } else { 1 };
    emit(EditorEvent::Finished(result));
    code
}

/// The worker's events up to its Finished line. Stops there, not at EOF, so a stray holder
/// of the pipe's write end can never delay the result.
fn editor_result(output: impl std::io::BufRead, progress: &mut dyn FnMut(&str)) -> Result<Option<Result<(), String>>, String> {
    for line in output.lines() {
        let line = line.map_err(|_| t("Could not monitor Windower's profile editor."))?;
        let Some(event) = line.strip_prefix(EDITOR_PREFIX).and_then(|line| serde_json::from_str::<EditorEvent>(line).ok()) else { continue; };
        match event {
            EditorEvent::Progress(line) => progress(&line),
            EditorEvent::Finished(result) => return Ok(Some(result)),
        }
    }
    Ok(None)
}

pub(crate) fn edit_profiles(folder: &std::path::Path, mut progress: impl FnMut(&str)) -> Result<(), String> {
    let mut command = std::process::Command::new(std::env::current_exe()
        .map_err(|_| t("Could not open Windower's profile editor."))?);
    command.arg("--windower-profile-editor-worker").arg(folder)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|_| t("Could not open Windower's profile editor."))?;
    let output = child.stdout.take().ok_or(t("Could not monitor Windower's profile editor."))?;
    let finished = editor_result(std::io::BufReader::new(output), &mut progress)?;
    let status = child.wait().map_err(|_| t("Could not monitor Windower's profile editor."))?;
    finished.unwrap_or_else(|| Err(if status.success() { t("Could not monitor Windower's profile editor.") }
        else { t("Could not open Windower's profile editor.") }.into()))
}

/// A finished scan: its generation, and what it found for which settings (None if it failed).
pub(crate) type Scan = (u64, Option<(ToolSettings, Detected)>);

#[derive(Default)]
pub(crate) struct State {
    pub detected: Detected,
    generation: u64,
    detected_for: Option<(u64, ToolSettings)>,
    pending: Option<ToolSettings>,
    scanning: bool,
}

impl State {
    /// Scan again for these settings. Filesystem scans and profile reads stay off
    /// the UI thread; one runs at a time, and edits made during it supersede its result.
    pub(crate) fn refresh(&mut self, settings: &Settings) -> Effect<Message> {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some(settings.tool_settings());
        self.start_pending()
    }

    /// A scan finished: keep it if nothing changed since, then start the next one.
    pub(crate) fn scanned(&mut self, (generation, result): Scan) -> Effect<Message> {
        self.scanning = false;
        if let Some((settings, detected)) = result.filter(|_| generation == self.generation) {
            self.detected = detected;
            self.detected_for = Some((generation, settings));
        }
        self.start_pending()
    }

    fn start_pending(&mut self) -> Effect<Message> {
        if self.scanning { return Effect::none(); }
        let Some(settings) = self.pending.take() else { return Effect::none(); };
        let generation = self.generation;
        self.scanning = true;
        Effect::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let detected = pol_less::detect_all(&settings);
                    (settings, detected)
                }).await.ok()
            },
            move |result| Message::LaunchToolsScanned((generation, result)),
        )
    }

    /// Carry a still-valid directory into the headless worker instead of making
    /// every account repeat the UI's inventory scan. Saved preferences stay as
    /// they are; the worker revalidates the path and reads the profile itself.
    pub(crate) fn launch_plan(&self, settings: &Settings, account: &Account) -> Option<Plan> {
        let mut plan = settings.launch_plan(account)?;
        let Some((generation, discovered)) = &self.detected_for else { return Some(plan); };
        if *generation != self.generation
            || discovered.ashita != settings.ashita_path
            || discovered.windower != settings.windower_path
            || discovered.loader != settings.loader_path
        {
            return Some(plan);
        }
        let (configured, folder) = match plan.tool {
            Tool::Ashita => (&mut plan.settings.ashita, self.detected.ashita.as_deref()),
            Tool::Windower => (&mut plan.settings.windower, self.detected.windower.as_deref()),
            Tool::Vanilla => return Some(plan),
        };
        if configured.trim().is_empty() {
            if let Some(folder) = folder.and_then(|folder| pol_less::tools::validated_folder(plan.tool, folder)) {
                if let Some(path) = folder.to_str() { *configured = path.to_owned(); }
            }
        }
        Some(plan)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Msg {
    AshitaPath(String),
    WindowerPath(String),
    BootIni(String),
    Profile(String),
    LoaderPath(String),
    Detect,
    /// What accounts set to "Use default" launch with.
    DefaultLaunchWith(LaunchWith),
}

/// Apply a change to the settings; true when they should be saved.
pub(crate) fn apply(settings: &mut Settings, msg: Msg) -> bool {
    let field = match msg {
        Msg::DefaultLaunchWith(choice) => {
            settings.launch_with = choice.retail();
            return true;
        }
        Msg::AshitaPath(v) => (&mut settings.ashita_path, v),
        Msg::WindowerPath(v) => (&mut settings.windower_path, v),
        Msg::BootIni(v) => (&mut settings.ashita_boot_ini, v),
        Msg::Profile(v) => (&mut settings.windower_profile, v),
        Msg::LoaderPath(v) => (&mut settings.loader_path, v),
        Msg::Detect => return false,
    };
    *field.0 = field.1;
    true
}

impl Settings {
    pub(crate) fn tool_settings(&self) -> ToolSettings {
        ToolSettings {
            ashita: self.ashita_path.clone(),
            windower: self.windower_path.clone(),
            boot_ini: self.ashita_boot_ini.clone(),
            windower_profile: self.windower_profile.clone(),
            loader: self.loader_path.clone(),
            dgvoodoo: self.dgvoodoo,
            reshade: self.reshade.clone(),
        }
    }

    /// Retail choices fall back to an available launcher. Private-server records
    /// are retained but cannot launch while the native client is unavailable.
    pub(crate) fn launch_with_for(&self, account: &Account) -> LaunchWith {
        account.effective_launch_with(self.launch_with)
    }

    /// The retail launch for `account`, or `None` for the unavailable legacy client. The account's own
    /// boot config and Windower profile win over the defaults set here.
    pub(crate) fn launch_plan(&self, account: &Account) -> Option<Plan> {
        let tool = match self.launch_with_for(account) {
            LaunchWith::Native => return None,
            LaunchWith::Vanilla => Tool::Vanilla,
            LaunchWith::Ashita => Tool::Ashita,
            LaunchWith::Windower => Tool::Windower,
        };
        let mut settings = self.tool_settings();
        if let Some(ini) = &account.ashita_boot_ini {
            settings.boot_ini = ini.clone();
        }
        if let Some(profile) = &account.windower_profile {
            settings.windower_profile = profile.clone();
        }
        Some(Plan { tool, settings, social_only: false,
            auto_away_minutes: self.auto_away_for(account), auto_login: self.auto_login_for(account), keep_session: self.sign_in_after_game, viewer_gaps: false, trigger: Default::default() })
    }

    /// The account's Auto-Away minutes (0 = off), the Viewer's 10 by default.
    pub(crate) fn auto_away_for(&self, account: &Account) -> u8 {
        self.account_auto_away.get(&crate::backend::account_key(account)).copied()
            .filter(|m| crate::pol_less::AUTO_AWAY_CHOICES.contains(m))
            .unwrap_or(crate::pol_less::default_auto_away())
    }

    pub(crate) fn auto_login_for(&self, account: &Account) -> pol_less::AutoLogin {
        self.account_auto_login.get(&crate::backend::account_key(account)).copied()
            .unwrap_or_default().normalized()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn profile_editor_opens_the_chosen_windower_without_launch_or_update_arguments() {
        let folder = PathBuf::from(r"D:\Tools\Windower");
        let command = profile_editor_command(&folder);
        assert_eq!(command.get_program(), folder.join("Windower.exe"));
        assert_eq!(command.get_current_dir(), Some(folder.as_path()));
        assert_eq!(command.get_args().count(), 0);
        assert_eq!(command.get_envs().count(), 0);
    }

    /// A process that inherited the pipe keeps it open after the worker exits: the reader must
    /// stop at Finished instead of waiting for EOF.
    #[test]
    fn the_editor_result_is_taken_at_finished_without_waiting_for_eof() {
        struct NeverEnds(std::io::Cursor<Vec<u8>>);
        impl std::io::Read for NeverEnds {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.read(buf)? {
                    0 => panic!("read past the Finished line"),
                    n => Ok(n),
                }
            }
        }
        let event = |e: &EditorEvent| format!("{EDITOR_PREFIX}{}\n", serde_json::to_string(e).unwrap());
        let text = format!("noise\n{}{}", event(&EditorEvent::Progress("Editing".into())), event(&EditorEvent::Finished(Ok(()))));
        let mut seen = Vec::new();
        let result = editor_result(std::io::BufReader::with_capacity(1, NeverEnds(std::io::Cursor::new(text.into_bytes()))),
            &mut |line| seen.push(line.to_owned()));
        assert_eq!(result, Ok(Some(Ok(()))));
        assert_eq!(seen, ["Editing"]);
    }

    #[test]
    fn a_completed_discovery_cannot_replace_newer_preferences() {
        let mut state = State { generation: 2, scanning: true, ..State::default() };
        let _ = state.scanned((1, Some((ToolSettings::default(), Detected { windower: Some(PathBuf::from(r"C:\Old Windower")), ..Detected::default() }))));
        assert!(state.detected.windower.is_none());
        assert!(state.detected_for.is_none());
        assert!(!state.scanning);

        state.scanning = true;
        let _ = state.scanned((2, Some((ToolSettings::default(), Detected { windower: Some(PathBuf::from(r"D:\Current Windower")), ..Detected::default() }))));
        assert_eq!(state.detected.windower, Some(PathBuf::from(r"D:\Current Windower")));
        assert_eq!(state.detected_for.as_ref().map(|(generation, _)| *generation), Some(2));
    }

    fn completed_discovery(settings: &Settings, detected: Detected) -> State {
        let mut state = State { generation: 1, scanning: true, ..State::default() };
        let _ = state.scanned((1, Some((settings.tool_settings(), detected))));
        state
    }

    fn tool_fixture() -> PathBuf {
        let root = std::env::temp_dir().join(format!("crystal-launch-path-{}", crate::backend::unique_id().unwrap()));
        for (folder, exe) in [("Windower", "Windower.exe"), ("Ashita", "Ashita-cli.exe")] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join(exe), []).unwrap();
        }
        root
    }

    #[test]
    fn runtime_plan_reuses_discovered_directories_without_saving_or_selecting_a_profile() {
        let root = tool_fixture();
        let settings = Settings { launch_with: LaunchWith::Windower, ..Settings::default() };
        let saved = serde_json::to_vec(&settings).unwrap();
        let state = completed_discovery(&settings, Detected {
            windower: Some(root.join("Windower")), ashita: Some(root.join("Ashita")),
            profiles: vec!["Named profile".into()], ..Detected::default()
        });
        let mut account = retail();
        let plan = state.launch_plan(&settings, &account).unwrap();
        let windower = pol_less::tools::validated_folder(Tool::Windower, &root.join("Windower")).unwrap();
        assert_eq!(PathBuf::from(&plan.settings.windower), windower);
        assert!(plan.settings.windower_profile.is_empty(), "the unnamed default remains selected");
        // The worker's ordinary path validation accepts the serialized hint.
        let handed: Plan = serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(pol_less::tools::detect(handed.tool, &handed.settings.windower), Some(windower));

        account.windower_profile = Some("Per-account profile".into());
        assert_eq!(state.launch_plan(&settings, &account).unwrap().settings.windower_profile, "Per-account profile");
        account.launch_with = Some(LaunchWith::Ashita);
        account.ashita_boot_ini = Some("account.ini".into());
        let plan = state.launch_plan(&settings, &account).unwrap();
        assert_eq!(pol_less::tools::detect(plan.tool, &plan.settings.ashita),
            pol_less::tools::validated_folder(Tool::Ashita, &root.join("Ashita")));
        assert_eq!(plan.settings.boot_ini, "account.ini");
        account.launch_with = Some(LaunchWith::Vanilla);
        let plan = state.launch_plan(&settings, &account).unwrap();
        assert!(plan.settings.windower.is_empty() && plan.settings.ashita.is_empty());
        assert_eq!(serde_json::to_vec(&settings).unwrap(), saved);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_plan_preserves_explicit_paths_even_when_they_need_normal_fallback() {
        let root = tool_fixture();
        let selected = root.join("Selected Windower");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::write(selected.join("Windower.exe"), []).unwrap();
        for selected in [selected, root.join("missing selection")] {
            let settings = Settings { launch_with: LaunchWith::Windower,
                windower_path: selected.to_string_lossy().into_owned(), ..Settings::default() };
            let state = completed_discovery(&settings, Detected { windower: Some(root.join("Windower")), ..Detected::default() });
            assert_eq!(state.launch_plan(&settings, &retail()).unwrap().settings.windower, settings.windower_path);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_plan_rejects_stale_discovery_generation_settings_and_executable() {
        let root = tool_fixture();
        let settings = Settings { launch_with: LaunchWith::Windower, ..Settings::default() };
        let mut state = completed_discovery(&settings, Detected { windower: Some(root.join("Windower")), ..Detected::default() });
        state.generation += 1;
        assert!(state.launch_plan(&settings, &retail()).unwrap().settings.windower.is_empty());
        state.generation -= 1;
        state.detected_for.as_mut().unwrap().1.windower = "previous selection".into();
        assert!(state.launch_plan(&settings, &retail()).unwrap().settings.windower.is_empty());
        state.detected_for.as_mut().unwrap().1 = settings.tool_settings();
        std::fs::remove_file(root.join("Windower/Windower.exe")).unwrap();
        assert!(state.launch_plan(&settings, &retail()).unwrap().settings.windower.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn edits_land_in_the_right_setting() {
        let mut settings = Settings::default();
        assert!(apply(&mut settings, Msg::AshitaPath(r"C:\A".into())));
        assert!(apply(&mut settings, Msg::WindowerPath(r"C:\W".into())));
        assert!(apply(&mut settings, Msg::BootIni("main.ini".into())));
        assert!(apply(&mut settings, Msg::Profile("Mule".into())));
        assert!(apply(&mut settings, Msg::LoaderPath(r"C:\L\pol.exe".into())));
        assert!(!apply(&mut settings, Msg::Detect));
        let tools = settings.tool_settings();
        assert_eq!(
            (tools.ashita.as_str(), tools.windower.as_str(), tools.boot_ini.as_str(), tools.windower_profile.as_str(), tools.loader.as_str()),
            (r"C:\A", r"C:\W", "main.ini", "Mule", r"C:\L\pol.exe")
        );
        // Old settings files without these fields still load, with everything on "detect".
        let old: Settings = serde_json::from_str(r#"{"install_path":"C:\\x"}"#).unwrap();
        assert!(old.ashita_path.is_empty() && old.loader_path.is_empty());
        assert!(apply(&mut settings, Msg::DefaultLaunchWith(LaunchWith::Ashita)));
        assert_eq!(settings.launch_with, LaunchWith::Ashita);
    }

    fn retail() -> Account {
        Account {
            label: "Main".into(),
            mode: crate::backend::AccountMode::Retail,
            server: String::new(),
            pol_id: "ABCD1234".into(),
            pol_password: "synthetic".into(),
            se_id: "synthetic".into(),
            se_password: "synthetic".into(),
            region: 0,
            launch_with: None,
            ashita_boot_ini: None,
            windower_profile: None,
            pol_handle_names: String::new(),
            totp_key: String::new(),
        }
    }

    #[test]
    fn launches_use_the_accounts_settings_over_the_defaults() {
        let settings = Settings {
            launch_with: LaunchWith::Windower,
            ashita_boot_ini: "shared.ini".into(),
            windower_profile: "Shared".into(),
            ashita_path: r"C:\A".into(),
            ..Settings::default()
        };
        // "Use default": the global launcher, boot config and profile.
        let mut account = retail();
        let plan = settings.launch_plan(&account).unwrap();
        assert_eq!(plan.tool, Tool::Windower);
        assert_eq!((plan.settings.windower_profile.as_str(), plan.settings.boot_ini.as_str()), ("Shared", "shared.ini"));
        // The account's own profile wins; folders still come from Launch tools.
        account.windower_profile = Some("Mule".into());
        let plan = settings.launch_plan(&account).unwrap();
        assert_eq!(plan.settings.windower_profile, "Mule");
        assert_eq!(plan.settings.ashita, r"C:\A");
        // Its own launcher and boot config.
        account.launch_with = Some(LaunchWith::Ashita);
        account.ashita_boot_ini = Some("mule.ini".into());
        let plan = settings.launch_plan(&account).unwrap();
        assert_eq!((plan.tool, plan.settings.boot_ini.as_str()), (Tool::Ashita, "mule.ini"));
        assert_eq!(settings.launch_with_for(&account), LaunchWith::Ashita);
        // Legacy retail Native choices use the available default launcher.
        account.launch_with = Some(LaunchWith::Native);
        assert_eq!(settings.launch_plan(&account).unwrap().tool, Tool::Windower);
        // Private-server accounts start xiloader through their own Windower or Ashita choice,
        // or on its own (no plan); never through the global default or Vanilla.
        let mut private = retail();
        private.mode = crate::backend::AccountMode::Private;
        private.launch_with = Some(LaunchWith::Windower);
        assert_eq!(settings.launch_with_for(&private), LaunchWith::Windower);
        assert_eq!(settings.launch_plan(&private).unwrap().tool, Tool::Windower);
        private.launch_with = None;
        assert_eq!(settings.launch_with_for(&private), LaunchWith::Native);
        assert!(settings.launch_plan(&private).is_none());
        private.launch_with = Some(LaunchWith::Vanilla);
        assert!(settings.launch_plan(&private).is_none());
    }

    #[test]
    fn auto_away_is_per_account_with_the_viewer_default_and_choices() {
        let mut settings = Settings { launch_with: LaunchWith::Windower, ..Settings::default() };
        let account = retail();
        assert_eq!(settings.launch_plan(&account).unwrap().auto_away_minutes, 10);
        settings.account_auto_away.insert(crate::backend::account_key(&account), 0);
        assert_eq!(settings.launch_plan(&account).unwrap().auto_away_minutes, 0, "off");
        settings.account_auto_away.insert(crate::backend::account_key(&account), 60);
        assert_eq!(settings.auto_away_for(&account), 60);
        settings.account_auto_away.insert(crate::backend::account_key(&account), 25);
        assert_eq!(settings.auto_away_for(&account), 10, "not a Viewer choice");
        let mut other = retail();
        other.pol_id = "WXYZ9876".into();
        assert_eq!(settings.auto_away_for(&other), 10);
        // Older saved plans keep the Viewer default.
        let plan: Plan = serde_json::from_value(serde_json::json!({
            "tool": "Ashita", "settings": { "ashita": "", "windower": "", "boot_ini": "", "windower_profile": "", "loader": "" }
        })).unwrap();
        assert_eq!(plan.auto_away_minutes, 10);
    }

    #[test]
    fn vanilla_is_saved_as_a_default_and_as_an_account_override() {
        let mut settings = Settings::default();
        assert!(apply(&mut settings, Msg::DefaultLaunchWith(LaunchWith::Vanilla)));
        let settings: Settings = serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        let mut account = retail();
        assert_eq!(settings.launch_plan(&account).unwrap().tool, Tool::Vanilla);
        account.launch_with = Some(LaunchWith::Ashita);
        assert_eq!(settings.launch_plan(&account).unwrap().tool, Tool::Ashita);
        account.launch_with = Some(LaunchWith::Vanilla);
        let plan = Settings::default().launch_plan(&account).unwrap();
        assert_eq!(plan.tool, Tool::Vanilla);
        assert!(!plan.social_only);
        assert_eq!(plan.auto_away_minutes, 10);
        account.mode = crate::backend::AccountMode::Private;
        assert!(settings.launch_plan(&account).is_none());
    }
}
