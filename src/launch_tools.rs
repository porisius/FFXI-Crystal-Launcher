//! Settings → System → Launch tools: where Ashita v4, Windower 4 and crystal-loader are,
//! which Ashita boot config and Windower profile a POL-less launch starts from. Empty
//! fields mean "find it automatically"; what was found (or not) is shown under each field.

use crate::backend::Account;
use crate::i18n::{t, tf};
use crate::icons::Icon;
use crate::pol_less::{self, Detected, Plan, Tool, ToolSettings};
use crate::ui::LaunchWith;
use crate::prereqs::{action, alpha, card, chip, divider, ok_tone, warn_tone};
use crate::ui::{colors, icon_control, profile_menu_style, profile_pick_style, scale, text_field, Colors, BODY, RADIUS_INNER, SEMI};
use crate::{App, Message, Settings};
use iced::widget::{column, container, row, text};
use iced::{Alignment, Border, Element, Font, Length};

const MONO: Font = Font::with_name("Consolas");

/// No profile or custom executable arguments: this opens Windower's own editor
/// rather than asking it to start another game or force an update.
pub(crate) fn profile_editor_command(folder: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(folder.join(Tool::Windower.exe()));
    command.current_dir(folder).stdin(std::process::Stdio::null());
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

pub(crate) fn edit_profiles(folder: &std::path::Path, mut progress: impl FnMut(&str)) -> Result<(), String> {
    use std::io::BufRead;
    let mut command = std::process::Command::new(std::env::current_exe()
        .map_err(|_| t("Could not open Windower's profile editor."))?);
    command.arg("--windower-profile-editor-worker").arg(folder)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|_| t("Could not open Windower's profile editor."))?;
    let output = child.stdout.take().ok_or(t("Could not monitor Windower's profile editor."))?;
    let mut finished = None;
    for line in std::io::BufReader::new(output).lines() {
        let line = line.map_err(|_| t("Could not monitor Windower's profile editor."))?;
        let Some(event) = line.strip_prefix(EDITOR_PREFIX).and_then(|line| serde_json::from_str::<EditorEvent>(line).ok()) else { continue; };
        match event { EditorEvent::Progress(line) => progress(&line), EditorEvent::Finished(result) => finished = Some(result) }
    }
    let status = child.wait().map_err(|_| t("Could not monitor Windower's profile editor."))?;
    finished.unwrap_or_else(|| Err(if status.success() { t("Could not monitor Windower's profile editor.") }
        else { t("Could not open Windower's profile editor.") }.into()))
}

#[derive(Default)]
pub(crate) struct State {
    pub detected: Detected,
    generation: u64,
    detected_for: Option<(u64, ToolSettings)>,
    pending: Option<ToolSettings>,
    worker: Option<std::sync::mpsc::Receiver<(u64, ToolSettings, Detected)>>,
}

impl State {
    pub(crate) fn refresh(&mut self, settings: &Settings) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some(settings.tool_settings());
        self.start_pending();
    }

    /// Called from the ordinary UI tick. Filesystem scans and profile reads
    /// stay off the UI thread; edits made during a scan supersede its result.
    pub(crate) fn poll(&mut self) {
        let result = self.worker.as_ref().map(|worker| worker.try_recv());
        match result {
            Some(Ok((generation, settings, detected))) => {
                if generation == self.generation {
                    self.detected = detected;
                    self.detected_for = Some((generation, settings));
                }
                self.worker = None;
            }
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => self.worker = None,
            _ => {}
        }
        self.start_pending();
    }

    fn start_pending(&mut self) {
        if self.worker.is_some() { return; }
        let Some(settings) = self.pending.take() else { return; };
        let generation = self.generation;
        let (sender, receiver) = std::sync::mpsc::channel();
        if std::thread::Builder::new().name("launch-tool-discovery".into()).spawn(move || {
            let detected = pol_less::detect_all(&settings);
            let _ = sender.send((generation, settings, detected));
        }).is_ok() {
            self.worker = Some(receiver);
        }
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
        Msg::Detect => {
            crate::discovery::invalidate();
            return false;
        }
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

    /// Retail choices fall back to the configured default. Private-server accounts default
    /// to Native (direct xiloader) and may explicitly choose Ashita or Windower.
    pub(crate) fn launch_with_for(&self, account: &Account) -> LaunchWith {
        account.effective_launch_with(self.launch_with)
    }

    /// The selected front-end plan, or `None` for direct private xiloader.
    /// The account's own boot config and Windower profile win over the defaults set here.
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
        Some(Plan { tool, settings, social_only: false, invisible_login: false,
            auto_away_minutes: self.auto_away_for(account), auto_login: self.auto_login_for(account), viewer_gaps: false, trigger: Default::default() })
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

/// A dropdown entry: the stored value and what the list shows.
#[derive(Clone, PartialEq, Eq)]
struct Choice {
    value: String,
    label: String,
}
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn status<'a>(c: Colors, found: bool) -> Element<'a, Message> {
    if found {
        chip(t("Found").into(), ok_tone(c))
    } else {
        chip(t("Not found").into(), warn_tone(c))
    }
}

/// Under a field: where the tool was found, or what to do.
fn caption<'a>(c: Colors, found: Option<&std::path::Path>, missing: String) -> Element<'a, Message> {
    match found {
        Some(path) => crate::privacy::path(path.display().to_string(), 12.0, MONO, c.sub),
        None => text(missing).font(BODY).size(12).color(warn_tone(c)).into(),
    }
}

fn labelled<'a>(c: Colors, label: &'a str, chip: Option<Element<'a, Message>>, body: Element<'a, Message>, s: f32) -> Element<'a, Message> {
    let mut head = column![text(label).font(SEMI).size(14).color(c.ink)].spacing(6);
    if let Some(chip) = chip {
        head = head.push(chip);
    }
    row![head.width(170.0 * s), container(body).width(Length::Fill)]
        .spacing(16)
        .align_y(Alignment::Start)
        .into()
}

fn picker<'a>(
    c: Colors,
    light: bool,
    options: Vec<Choice>,
    selected: Option<Choice>,
    placeholder: &str,
    on_select: fn(String) -> Msg,
) -> Element<'a, Message> {
    let base = profile_pick_style(c.well, Border { color: c.line, width: 1.0, radius: RADIUS_INNER.into() }, false, light);
    crate::controller_nav::pick_list(options, selected, move |choice: Choice| Message::LaunchTools(on_select(choice.value)))
        .placeholder(placeholder.to_owned())
        .font(BODY)
        .text_size(14)
        .padding([10, 14])
        .width(Length::Fill)
        .style(move |theme, state| iced::widget::pick_list::Style { handle_color: c.sub, ..base(theme, state) })
        .menu_style(profile_menu_style(light))
        .into()
}

pub(crate) fn view(app: &App) -> Element<'_, Message> {
    let light = app.settings.light_theme;
    let c = colors(light);
    let s = scale(app);
    let set = &app.settings;
    let found = &app.launch_tools.detected;

    let header = row![
        column![
            text(t("Launch tools")).font(SEMI).size(18).color(c.ink),
            text(t("All retail launch methods use the installed PlayOnline core through Crystal. Vanilla starts FFXI directly; Windower and Ashita use their own profiles. Leave a tool folder empty to find it automatically."))
                .font(BODY)
                .size(12.5)
                .color(c.sub)
        ]
        .spacing(6)
        .width(Length::Fill),
        action(c, Icon::Refresh, t("Detect again"), false, Some(Message::LaunchTools(Msg::Detect)))
    ]
    .spacing(16)
    .align_y(Alignment::Center);

    // A text field (still editable) with a native Browse button beside it.
    let path_field = |placeholder: &str, value: &str, on: fn(String) -> Msg, pick: crate::PickTarget| {
        row![
            text_field(placeholder, &crate::privacy::mask_path(value))
                .on_input_maybe(crate::privacy::editable().then_some(move |v| Message::LaunchTools(on(crate::privacy::unmask_path(v))))),
            icon_control(c, Icon::Folder, true, Some(Message::Browse(pick)))
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    };

    // The launcher accounts set to "Use default" start with.
    let default_style = profile_pick_style(c.well, Border { color: c.line, width: 1.0, radius: RADIUS_INNER.into() }, false, light);
    let default_launcher = labelled(
        c,
        t("Default launcher"),
        None,
        column![
            crate::controller_nav::pick_list(LaunchWith::ALL, Some(set.launch_with.retail()), |choice| Message::LaunchTools(Msg::DefaultLaunchWith(choice)))
                .font(BODY)
                .text_size(14)
                .padding([10, 14])
                .width(Length::Fill)
                .style(move |theme, state| iced::widget::pick_list::Style { handle_color: c.sub, ..default_style(theme, state) })
                .menu_style(profile_menu_style(light)),
            text(t("Retail accounts set to Use default start with this. Each account can choose its own launcher, boot config and profile in its account settings."))
                .font(BODY)
                .size(12)
                .color(c.sub)
        ]
        .spacing(6)
        .into(),
        s,
    );

    // Ashita: folder and base boot config.
    let ashita = labelled(
        c,
        Tool::Ashita.name(),
        Some(status(c, found.ashita.is_some())),
        column![
            path_field(t("Detect automatically (e.g. C:\\Games\\Final Fantasy XI Programs\\AshitaV4)"), &set.ashita_path, Msg::AshitaPath, crate::PickTarget::AshitaFolder),
            caption(
                c,
                found.ashita.as_deref(),
                if set.ashita_path.trim().is_empty() {
                    t("Not found in the usual folders. Enter the folder that contains Ashita-cli.exe.").into()
                } else {
                    t("This folder has no Ashita-cli.exe.").into()
                },
            )
        ]
        .spacing(6)
        .into(),
        s,
    );
    let mut inis: Vec<Choice> = found
        .boot_inis
        .iter()
        .map(|name| Choice { value: name.clone(), label: name.clone() })
        .collect();
    let current_ini = if set.ashita_boot_ini.trim().is_empty() {
        pol_less::tools::DEFAULT_BOOT_INI.to_owned()
    } else {
        set.ashita_boot_ini.trim().to_owned()
    };
    let selected_ini = inis.iter().find(|choice| choice.value.eq_ignore_ascii_case(&current_ini)).cloned();
    if selected_ini.is_none() && !set.ashita_boot_ini.trim().is_empty() {
        let missing = Choice { value: current_ini.clone(), label: tf("{name} (missing)", &[("name", &current_ini)]) };
        inis.insert(0, missing);
    }
    let boot_note = if found.ashita.is_none() {
        t("Set Ashita's folder to choose one.").to_owned()
    } else if selected_ini.is_some() {
        t("Copied for each account as config\\boot\\crystal-<account>.ini, with only the boot file and command pointed at the loader. Passwords are never written to Ashita's files.").to_owned()
    } else if set.ashita_boot_ini.trim().is_empty() {
        t("No default.ini here: a minimal boot config is used. Choose one of yours to keep its fonts, window and game settings.").to_owned()
    } else {
        tf("{name} is not in Ashita's config\\boot folder.", &[("name", &current_ini)])
    };
    let boot = labelled(
        c,
        t("Base boot config"),
        None,
        column![
            picker(c, light, inis, selected_ini, t("Built-in minimal config"), Msg::BootIni),
            text(boot_note).font(BODY).size(12).color(c.sub)
        ]
        .spacing(6)
        .into(),
        s,
    );

    // Windower: folder and profile.
    let windower = labelled(
        c,
        Tool::Windower.name(),
        Some(status(c, found.windower.is_some())),
        column![
            path_field(t("Detect automatically (e.g. C:\\Games\\Final Fantasy XI Programs\\Windower 4)"), &set.windower_path, Msg::WindowerPath, crate::PickTarget::WindowerFolder),
            caption(
                c,
                found.windower.as_deref(),
                if set.windower_path.trim().is_empty() {
                    t("Not found in the usual folders. Enter the folder that contains Windower.exe.").into()
                } else {
                    t("This folder has no Windower.exe.").into()
                },
            )
        ]
        .spacing(6)
        .into(),
        s,
    );
    let default_profile = Choice { value: String::new(), label: t("Windower's default profile").into() };
    let mut profiles = vec![default_profile.clone()];
    profiles.extend(found.profiles.iter().map(|name| Choice { value: name.clone(), label: name.clone() }));
    let current_profile = set.windower_profile.trim();
    let mut selected_profile = profiles.iter().find(|choice| choice.value == current_profile).cloned();
    if selected_profile.is_none() {
        let missing = Choice { value: current_profile.to_owned(), label: tf("{name} (not in settings.xml)", &[("name", &current_profile)]) };
        profiles.push(missing.clone());
        selected_profile = Some(missing);
    }
    let profile = labelled(
        c,
        t("Windower profile"),
        None,
        column![
            picker(c, light, profiles, selected_profile, t("Windower's default profile"), Msg::Profile),
            action(c, Icon::Edit, t("Edit profiles in Windower"), false,
                (found.windower.is_some() && !app.windower_editor_open).then_some(Message::EditWindowerProfiles)),
            text(t("Video and sound settings come from this profile. Launches through Windower start one at a time."))
                .font(BODY)
                .size(12)
                .color(c.sub)
        ]
        .spacing(6)
        .into(),
        s,
    );

    // The loader.
    let loader = labelled(
        c,
        t("Crystal loader"),
        Some(status(c, found.loader.is_some())),
        column![
            path_field(t("pol.exe in the launcher's crystal folder (override for development)"), &set.loader_path, Msg::LoaderPath, crate::PickTarget::LoaderFile),
            caption(
                c,
                found.loader.as_deref(),
                if set.loader_path.trim().is_empty() {
                    t("pol.exe is not in the launcher's crystal folder. Reinstall the launcher, or enter the loader's path.").into()
                } else {
                    t("No file at this path.").into()
                },
            )
        ]
        .spacing(6)
        .into(),
        s,
    );

    let note = container(
        text(t("Ashita needs administrator rights: run the launcher as administrator, or accept Windows' prompt at each launch."))
            .font(BODY)
            .size(12)
            .color(c.sub),
    )
    .padding([8, 12])
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(alpha(c.accent, 0.06).into()),
        border: Border { color: alpha(c.accent, 0.22), width: 1.0, radius: 6.0.into() },
        ..Default::default()
    });

    container(card(
        c,
        column![header, divider(c), default_launcher, divider(c), ashita, boot, divider(c), windower, profile, divider(c), loader, note]
            .spacing(14.0 * s),
        s,
    ))
    .padding(iced::Padding::ZERO.right(14.0 * s))
    .width(Length::Fill)
    .into()
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

    #[test]
    fn a_completed_discovery_cannot_replace_newer_preferences() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut state = State { generation: 2, worker: Some(receiver), ..State::default() };
        sender.send((1, ToolSettings::default(), Detected { windower: Some(PathBuf::from(r"C:\Old Windower")), ..Detected::default() })).unwrap();
        state.poll();
        assert!(state.detected.windower.is_none());
        assert!(state.detected_for.is_none());

        let (sender, receiver) = std::sync::mpsc::channel();
        state.worker = Some(receiver);
        sender.send((2, ToolSettings::default(), Detected { windower: Some(PathBuf::from(r"D:\Current Windower")), ..Detected::default() })).unwrap();
        state.poll();
        assert_eq!(state.detected.windower, Some(PathBuf::from(r"D:\Current Windower")));
        assert_eq!(state.detected_for.as_ref().map(|(generation, _)| *generation), Some(2));
    }

    fn completed_discovery(settings: &Settings, detected: Detected) -> State {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut state = State { generation: 1, worker: Some(receiver), ..State::default() };
        sender.send((1, settings.tool_settings(), detected)).unwrap();
        state.poll();
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
        assert!(!settings.launch_plan(&account).unwrap().invisible_login);
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
        let mut private = retail();
        private.mode = crate::backend::AccountMode::Private;
        assert_eq!(settings.launch_with_for(&private), LaunchWith::Native);
        assert!(settings.launch_plan(&private).is_none());
        private.launch_with = Some(LaunchWith::Windower);
        assert_eq!(settings.launch_with_for(&private), LaunchWith::Windower);
        assert_eq!(settings.launch_plan(&private).unwrap().tool, Tool::Windower);
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
        assert!(!plan.social_only && !plan.invisible_login);
        assert_eq!(plan.auto_away_minutes, 10);
        account.mode = crate::backend::AccountMode::Private;
        assert!(settings.launch_plan(&account).is_none());
    }
}
