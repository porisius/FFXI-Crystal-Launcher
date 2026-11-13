//! The Settings page's and its dialogs' view model (ui/settings*.slint): `wire` turns the
//! window's callbacks into core messages,
//! `sync` pushes the core's state into `SettingsView` after each batch of messages.
//!
//! As in view.rs, `sync` sets a property only when its value differs and updates list models
//! row by row; the parts with lists inside their rows (the game options, the Windows
//! components, the controller's bindings, the import drafts) are compared as plain data first,
//! so a batch that changed nothing redraws nothing and a changed row keeps its controls (and
//! the controller's focus). Only the shown tab is synced, and the parts that are costly to
//! build (the update line under privacy mode, the launch tools, the feature check, the game
//! options) are rebuilt only when what they are built from moved.
use super::{
    Act, Compat, ComponentCard, Components, Dg, Draft, FeatureOff, GcBar, GcCard, GcRow, Glyph, ImportState, MainWindow, PadBinding, PadChoice,
    PadKey, PadNote, PadState, PadSwitch, Program, Rs, ServerState, SettingsStrings, SettingsView, Shade, SystemState, ToolRow, ToolsState,
};
use crate::game_config::front as gcf;
use crate::gamepad::front as gp;
use crate::i18n::{t, tf, Lang};
use crate::prereqs::front as pf;
use crate::settings_page::Tab;
use crate::backend::LaunchWith;
use crate::{App, Message, Page, PickTarget};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// settings_page::Tab's order.
const TABS: [Tab; 5] = [Tab::Graphics, Tab::Sound, Tab::Controller, Tab::System, Tab::Server];
/// The import dialog's regions, as its buttons name them.
const REGIONS: [&str; 3] = ["US", "EU", "JP"];

/// What the callbacks act on, as of the last sync, and the models the window draws from.
struct State {
    lang: Option<Lang>,
    tabs: Rc<VecModel<SharedString>>,
    languages: Rc<VecModel<SharedString>>,
    launchers: Rc<VecModel<SharedString>>,
    boot_items: Rc<VecModel<SharedString>>,
    profile_items: Rc<VecModel<SharedString>>,
    boot_values: Vec<String>,
    profile_values: Vec<String>,
    update_action: Option<crate::self_update::front::Action>,
    /// The update line as the core words it, and whether privacy mode was on, when last shown.
    update_key: Option<(String, bool)>,
    /// What the launch tools were built from (`tools_key`), when last shown.
    tools_key: Option<u64>,
    programs: Rc<VecModel<Program>>,
    /// When the feature check was last looked at, and what it said.
    compat_at: Option<Instant>,
    compat: Option<Option<crate::feature_check::Check>>,
    features_off: Rc<VecModel<FeatureOff>>,
    prereqs: Option<pf::Front>,
    component_cards: Rc<VecModel<ComponentCard>>,
    /// The game options as last shown, and for which tab.
    gc: Option<(gcf::Front, bool)>,
    /// What they were built from: the game options' revision, the tab and privacy mode.
    gc_key: Option<(u64, bool, bool)>,
    gc_regions: Rc<VecModel<SharedString>>,
    gc_backups: Rc<VecModel<SharedString>>,
    gc_left: Rc<VecModel<GcCard>>,
    gc_right: Rc<VecModel<GcCard>>,
    /// Each card's rows model, kept across syncs so a changed row is updated in place.
    gc_rows_left: Vec<Rc<VecModel<GcRow>>>,
    gc_rows_right: Vec<Rc<VecModel<GcRow>>>,
    /// Every row by its id (left cards first, then right).
    gc_rows: Vec<gcf::Row>,
    /// The controller page as last shown.
    pad: Option<gp::Front>,
    pad_choices: Rc<VecModel<PadChoice>>,
    pad_keys: Rc<VecModel<PadKey>>,
    pad_left: Rc<VecModel<PadBinding>>,
    pad_right: Rc<VecModel<PadBinding>>,
    pad_switches: Rc<VecModel<PadSwitch>>,
    pad_notes: Rc<VecModel<PadNote>>,
    /// The small drawn chips by their SVG document, and the controller picture last drawn.
    chips: HashMap<String, slint::Image>,
    picture: Option<(String, slint::Image)>,
    import_sources: Rc<VecModel<SharedString>>,
    import_warnings: Rc<VecModel<SharedString>>,
    import_drafts: Rc<VecModel<Draft>>,
    drafts: Vec<(String, String, Vec<String>)>,
    /// Settings → Server's staged xiloader versions, as its Publish buttons name them.
    server_staged: Rc<VecModel<SharedString>>,
    staged_values: Vec<String>,
}

impl State {
    fn new() -> Self {
        Self {
            lang: None,
            tabs: Rc::new(VecModel::default()),
            server_staged: Rc::new(VecModel::default()),
            staged_values: Vec::new(),
            languages: Rc::new(VecModel::default()),
            launchers: Rc::new(VecModel::default()),
            boot_items: Rc::new(VecModel::default()),
            profile_items: Rc::new(VecModel::default()),
            boot_values: Vec::new(),
            profile_values: Vec::new(),
            update_action: None,
            update_key: None,
            tools_key: None,
            programs: Rc::new(VecModel::default()),
            compat_at: None,
            compat: None,
            features_off: Rc::new(VecModel::default()),
            prereqs: None,
            component_cards: Rc::new(VecModel::default()),
            gc: None,
            gc_key: None,
            gc_regions: Rc::new(VecModel::default()),
            gc_backups: Rc::new(VecModel::default()),
            gc_left: Rc::new(VecModel::default()),
            gc_right: Rc::new(VecModel::default()),
            gc_rows_left: Vec::new(),
            gc_rows_right: Vec::new(),
            gc_rows: Vec::new(),
            pad: None,
            pad_choices: Rc::new(VecModel::default()),
            pad_keys: Rc::new(VecModel::default()),
            pad_left: Rc::new(VecModel::default()),
            pad_right: Rc::new(VecModel::default()),
            pad_switches: Rc::new(VecModel::default()),
            pad_notes: Rc::new(VecModel::default()),
            chips: HashMap::new(),
            picture: None,
            import_sources: Rc::new(VecModel::default()),
            import_warnings: Rc::new(VecModel::default()),
            import_drafts: Rc::new(VecModel::default()),
            drafts: Vec::new(),
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The window closed: the page's models go with it (nothing stays in the background).
pub(super) fn forget() {
    STATE.with(|cell| cell.borrow_mut().take());
}

#[cfg(test)]
thread_local! {
    /// The messages the callbacks sent (tests have no host to deliver them to).
    static SENT: RefCell<Vec<Message>> = const { RefCell::new(Vec::new()) };
}

/// Delivers `message` to the core.
fn emit(message: Message) {
    #[cfg(test)]
    SENT.with(|sent| sent.borrow_mut().push(message));
    #[cfg(not(test))]
    super::send(message);
}

/// Sends the message `f` picks from the last sync's state, after releasing it.
fn act(f: impl FnOnce(&State) -> Option<Message>) {
    if let Some(message) = STATE.with(|cell| cell.borrow().as_ref().and_then(f)) {
        emit(message);
    }
}

fn index(i: i32) -> Option<usize> {
    usize::try_from(i).ok()
}

/// Connect the Settings page's and dialogs' callbacks.
pub(super) fn wire(ui: &MainWindow) {
    let state = State::new();
    let view = ui.global::<SettingsView>();
    view.set_tabs(ModelRc::from(state.tabs.clone()));
    view.set_server_staged(ModelRc::from(state.server_staged.clone()));
    view.set_languages(ModelRc::from(state.languages.clone()));
    view.set_launchers(ModelRc::from(state.launchers.clone()));
    view.set_boot_items(ModelRc::from(state.boot_items.clone()));
    view.set_profile_items(ModelRc::from(state.profile_items.clone()));
    view.set_programs(ModelRc::from(state.programs.clone()));
    view.set_features_off(ModelRc::from(state.features_off.clone()));
    view.set_component_cards(ModelRc::from(state.component_cards.clone()));
    view.set_gc_regions(ModelRc::from(state.gc_regions.clone()));
    view.set_gc_backups(ModelRc::from(state.gc_backups.clone()));
    view.set_gc_left(ModelRc::from(state.gc_left.clone()));
    view.set_gc_right(ModelRc::from(state.gc_right.clone()));
    view.set_import_sources(ModelRc::from(state.import_sources.clone()));
    view.set_import_warnings(ModelRc::from(state.import_warnings.clone()));
    view.set_import_drafts(ModelRc::from(state.import_drafts.clone()));
    view.set_pad_choices(ModelRc::from(state.pad_choices.clone()));
    view.set_pad_keys(ModelRc::from(state.pad_keys.clone()));
    view.set_pad_left(ModelRc::from(state.pad_left.clone()));
    view.set_pad_right(ModelRc::from(state.pad_right.clone()));
    view.set_pad_switches(ModelRc::from(state.pad_switches.clone()));
    view.set_pad_notes(ModelRc::from(state.pad_notes.clone()));
    view.set_regions(ModelRc::new(VecModel::from(REGIONS.iter().map(|r| SharedString::from(*r)).collect::<Vec<_>>())));
    view.set_version(env!("CARGO_PKG_VERSION").into());
    STATE.with(|cell| *cell.borrow_mut() = Some(state));

    // ---- header and tabs ----
    view.on_pick_tab(|i| { if let Some(tab) = index(i).and_then(|i| TABS.get(i)) { emit(Message::SettingsTab(*tab)); } });
    view.on_set_privacy(|on| emit(Message::SetPrivacy(on)));
    view.on_set_language(|i| { if let Some(lang) = index(i).and_then(|i| Lang::ALL.get(i)) { emit(Message::SetLanguage(*lang)); } });

    // ---- Server ----
    use crate::server_admin_page::Msg as Server;
    let server = |msg: Server| emit(Message::ServerAdmin(msg));
    view.on_server_host_edited(move |value| server(Server::HostInput(value.into())));
    view.on_server_token_edited(move |value| server(Server::TokenInput(value.into())));
    view.on_server_motd_edited(move |value| server(Server::MotdInput(value.into())));
    view.on_server_connect(move || server(Server::Connect));
    view.on_server_forget(move || server(Server::Forget));
    view.on_server_refresh(move || server(Server::Refresh));
    view.on_server_approve(move |on| server(Server::SetUpdateApproved(on)));
    view.on_server_maintenance(move |on| server(Server::SetMaintenance(on)));
    view.on_server_save_motd(move || server(Server::SaveMotd));
    view.on_server_clear(move || server(Server::ClearOverrides));
    view.on_server_publish(|i| act(|s| s.staged_values.get(index(i)?).map(|version| Message::ServerAdmin(Server::Xiloader("promote", Some(version.clone()))))));
    view.on_server_rollback(move || server(Server::Xiloader("rollback", None)));
    view.on_server_verify(move || server(Server::GenerateVerify));
    view.on_server_cancel_verify(move || server(Server::CancelVerify));

    // ---- System ----
    view.on_update_action(|| act(|s| s.update_action.map(|action| Message::SelfUpdate(action.msg()))));
    view.on_update_auto(|on| emit(Message::SelfUpdate(crate::self_update::Msg::AutoCheck(on))));
    view.on_update_restart(|on| emit(Message::SelfUpdate(crate::self_update::Msg::AutoRestart(on))));
    view.on_set_nav(|on| emit(Message::ControllerNavigation(on)));
    view.on_set_sign_in(|on| emit(Message::AutoSignIn(on)));
    view.on_set_keep(|on| emit(Message::SignInAfterGame(on)));
    view.on_set_instant(|on| emit(Message::CloseGamesInstantly(on)));
    view.on_set_item_search(|on| emit(Message::ItemSearch(on)));
    view.on_set_start(|on| emit(Message::StartWithWindows(on)));
    view.on_set_tray(|on| emit(Message::CloseToTray(on)));

    use crate::launch_tools::Msg as Tools;
    view.on_tools_detect(|| emit(Message::LaunchTools(Tools::Detect)));
    view.on_tools_default(|i| { if let Some(tool) = index(i).and_then(|i| LaunchWith::ALL.get(i)) { emit(Message::LaunchTools(Tools::DefaultLaunchWith(*tool))); } });
    view.on_tools_path(|which, value| {
        let value: String = value.into();
        match which {
            0 => emit(Message::LaunchTools(Tools::AshitaPath(value))),
            1 => emit(Message::LaunchTools(Tools::WindowerPath(value))),
            2 => emit(Message::LaunchTools(Tools::LoaderPath(value))),
            _ => {}
        }
    });
    view.on_tools_browse(|which| {
        let target = match which {
            0 => PickTarget::AshitaFolder,
            1 => PickTarget::WindowerFolder,
            2 => PickTarget::LoaderFile,
            _ => return,
        };
        emit(Message::Browse(target));
    });
    view.on_tools_boot(|i| act(|s| s.boot_values.get(index(i)?).map(|value| Message::LaunchTools(Tools::BootIni(value.clone())))));
    view.on_tools_profile(|i| act(|s| s.profile_values.get(index(i)?).map(|value| Message::LaunchTools(Tools::Profile(value.clone())))));
    view.on_edit_profiles(|| emit(Message::EditWindowerProfiles));

    use crate::open_with_game::Msg as Programs;
    view.on_program_path(|i, value| { if let Some(i) = index(i) { emit(Message::OpenWithGame(Programs::Path(i, value.into()))); } });
    view.on_program_args(|i, value| { if let Some(i) = index(i) { emit(Message::OpenWithGame(Programs::Args(i, value.into()))); } });
    view.on_program_enabled(|i, on| { if let Some(i) = index(i) { emit(Message::OpenWithGame(Programs::Enabled(i, on))); } });
    view.on_program_browse(|i| { if let Some(i) = index(i) { emit(Message::Browse(PickTarget::ProgramFile(i))); } });
    view.on_program_remove(|i| { if let Some(i) = index(i) { emit(Message::OpenWithGame(Programs::Remove(i))); } });
    view.on_program_add(|| emit(Message::OpenWithGame(Programs::Add)));

    let prereq = |pick: fn(&pf::Card) -> Option<&pf::Button>, i: i32| {
        act(|s| {
            let card = s.prereqs.as_ref()?.cards.get(index(i)?)?;
            pick(card).filter(|button| button.enabled).map(|button| Message::Prereqs(button.msg.msg()))
        })
    };
    view.on_component_button(move |i| prereq(|card| card.button.as_ref(), i));
    view.on_component_not_now(move |i| prereq(|card| card.confirm.as_ref().map(|c| &c.not_now), i));
    view.on_component_confirm(move |i| prereq(|card| card.confirm.as_ref().map(|c| &c.confirm), i));
    view.on_component_cancel(move |i| prereq(|card| card.progress.as_ref().and_then(|p| p.cancel.as_ref()), i));
    view.on_components_check(|| act(|s| s.prereqs.as_ref().filter(|f| f.check_again.enabled).map(|f| Message::Prereqs(f.check_again.msg.msg()))));
    view.on_components_manual(|| emit(Message::Prereqs(pf::MsgOf::ToggleManual.msg())));
    view.on_components_features(|| emit(Message::Prereqs(pf::MsgOf::OpenWindowsFeatures.msg())));

    // ---- Graphics ----
    use crate::dgvoodoo::Msg as Dgv;
    view.on_dg_toggle(|on| emit(Message::Dgvoodoo(Dgv::Toggle(on))));
    view.on_dg_open(|| emit(Message::Dgvoodoo(Dgv::OpenSettings)));
    view.on_dg_remove(|| emit(Message::Dgvoodoo(Dgv::Remove)));
    view.on_dg_retry(|| emit(Message::Dgvoodoo(Dgv::Toggle(true))));
    view.on_dg_reset(|| emit(Message::Dgvoodoo(Dgv::Reset)));
    view.on_dg_import(|| emit(Message::Dgvoodoo(Dgv::Import)));
    use crate::reshade::Msg as Reshade;
    view.on_rs_toggle(|on| emit(Message::Reshade(Reshade::Toggle(on))));
    view.on_rs_setup(|| emit(Message::Reshade(Reshade::Setup)));
    view.on_rs_preset(|| emit(Message::Reshade(Reshade::ChoosePreset)));
    view.on_rs_folder(|| emit(Message::Reshade(Reshade::OpenFolder)));
    view.on_rs_website(|| emit(Message::Reshade(Reshade::Website)));

    use crate::game_config::Msg as Gc;
    let gc = |msg: Gc| Message::GameConfig(msg);
    view.on_gc_region(move |i| act(|s| {
        let (front, _) = s.gc.as_ref()?;
        (index(i)? != front.region && !front.locked()).then_some(())?;
        front.regions.get(index(i)?).map(|(_, region)| gc(Gc::Region(*region)))
    }));
    view.on_gc_defaults(move || emit(gc(Gc::Defaults)));
    view.on_gc_backup(move |i| act(|s| s.gc.as_ref()?.0.backups.get(index(i)?).map(|entry| gc(Gc::LoadBackup(entry.clone())))));
    view.on_gc_discard(move || act(|s| s.gc.as_ref().filter(|(f, _)| f.pending > 0).map(|_| gc(Gc::Discard))));
    view.on_gc_apply(move || act(|s| s.gc.as_ref().filter(|(f, _)| f.pending > 0 && f.writable).map(|_| gc(Gc::Apply))));
    view.on_gc_dismiss(move || emit(gc(Gc::DismissNotice)));
    view.on_gc_pick(move |id, i| act(|s| match &s.gc_rows.get(index(id)?)?.control {
        gcf::Control::Choice { picks, .. } => picks.get(index(i)?).map(|pick| gc(pick.msg())),
        _ => None,
    }));
    view.on_gc_toggle(move |id, on| act(|s| match &s.gc_rows.get(index(id)?)?.control {
        gcf::Control::Switch { key, .. } => Some(gc(Gc::Set(*key, u32::from(on)))),
        _ => None,
    }));
    view.on_gc_slide(move |id, value| act(|s| match &s.gc_rows.get(index(id)?)?.control {
        gcf::Control::Slider { target, .. } => Some(gc(target.msg(value))),
        _ => None,
    }));
    view.on_gc_folder(move |value| emit(gc(Gc::ScreenshotDir(value.into()))));
    view.on_gc_folder_default(move || emit(gc(Gc::ScreenshotDir(String::new()))));

    // ---- Controller ----
    let pad = |msg: crate::gamepad::Msg| Message::Gamepad(msg);
    view.on_pad_select(move |id| { if let Some(id) = index(id) { emit(pad(gp::select(id))); } });
    view.on_pad_retry(move || emit(pad(gp::retry())));
    view.on_pad_rumble(move || emit(pad(gp::rumble())));
    view.on_pad_reset(move || emit(pad(gp::reset())));
    view.on_pad_press(move |id| act(|s| s.pad.as_ref()?.bindings.get(index(id)?).map(|binding| pad(gp::press(binding)))));
    view.on_pad_unbind(move |id| act(|s| s.pad.as_ref()?.bindings.get(index(id)?).filter(|b| b.capturing).map(|binding| pad(gp::unbind(binding)))));
    view.on_pad_switch(move |i, on| act(|s| {
        let switch = s.pad.as_ref()?.switches.get(index(i)?)?;
        switch.enabled.then(|| pad(gp::set_flag(switch.flag, on)))
    }));

    // ---- dialogs ----
    // Tab inside a dialog cycles through the dialog's own controls.
    let window = ui.as_weak();
    view.on_dialog_tab(move |forward| {
        if let Some(ui) = window.upgrade() {
            super::pad::cycle(ui.window(), forward);
        }
    });
    view.on_close_keep(|| emit(Message::CancelClose));
    view.on_close_now(|| emit(Message::ConfirmClose));
    use crate::account_import_ui::Msg as Import;
    let import = |msg: Import| Message::AccountImport(msg);
    view.on_import_close(move || emit(import(Import::Close)));
    view.on_import_source(move |i| { if let Some(i) = index(i) { emit(import(Import::Source(i))); } });
    view.on_import_path_edited(move |value| emit(import(Import::Path(value.into()))));
    view.on_import_companion_edited(move |value| emit(import(Import::CompanionPath(value.into()))));
    view.on_import_browse(move |companion| emit(import(Import::Browse(companion))));
    view.on_import_region(move |i| { if (0..3).contains(&i) { emit(import(Import::Region(i as u8))); } });
    view.on_import_use_companion(move |on| emit(import(Import::Companion(on))));
    view.on_import_review(move |i| { if let Some(i) = index(i) { emit(import(Import::Review(i))); } });
    view.on_import_read(move || emit(import(Import::Read)));
}

/// Sets a property only when the new value differs from the shown one.
macro_rules! put {
    ($view:expr, $get:ident, $set:ident, $value:expr) => {{
        let value = $value;
        if $view.$get() != value {
            $view.$set(value);
        }
    }};
}

/// Updates `model` to `rows` in place: only rows that differ are replaced.
fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let old = model.row_count();
    for (i, row) in rows.iter().enumerate() {
        if i < old {
            if model.row_data(i).as_ref() != Some(row) {
                model.set_row_data(i, row.clone());
            }
        } else {
            model.push(row.clone());
        }
    }
    for i in (rows.len()..old).rev() {
        model.remove(i);
    }
}

fn strings_model(model: &VecModel<SharedString>, items: impl IntoIterator<Item = String>) {
    sync_model(model, items.into_iter().map(SharedString::from).collect());
}

/// Updates `model` from plain `rows`, comparing them with `last` (the rows as last shown) and
/// building only the rows that changed: rows holding lists compare by those lists' identity
/// in Slint, so they are compared as plain data here instead.
fn sync_built<R: PartialEq + Clone, S: Clone + 'static>(model: &VecModel<S>, last: &mut Vec<R>, rows: Vec<R>, mut build: impl FnMut(usize, &R) -> S) {
    let old = model.row_count();
    for (i, row) in rows.iter().enumerate() {
        if i < old {
            if last.get(i) != Some(row) {
                model.set_row_data(i, build(i, row));
            }
        } else {
            model.push(build(i, row));
        }
    }
    for i in (rows.len()..old).rev() {
        model.remove(i);
    }
    *last = rows;
}

fn strings_list(items: &[impl AsRef<str>]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(items.iter().map(|item| SharedString::from(item.as_ref())).collect::<Vec<_>>()))
}

/// `text` with whatever identifies someone masked while privacy mode is on.
fn private(app: &App, text: &str) -> String {
    if !crate::privacy::on() {
        return text.to_owned();
    }
    crate::privacy::split(text, &crate::privacy::identities(app))
        .into_iter()
        .map(|(part, hide)| if hide { crate::privacy::mask(&part) } else { part })
        .collect()
}

/// Push `app`'s state into the window. The page's parts are synced while it shows; the
/// dialogs always.
pub(super) fn sync(ui: &MainWindow, app: &App) {
    let view = ui.global::<SettingsView>();
    STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let state = slot.get_or_insert_with(State::new);
        let lang = crate::i18n::current();
        if state.lang != Some(lang) {
            state.lang = Some(lang);
            put!(view, get_strings, set_strings, strings());
            // Server is English, like the rest of Crystal's private-server text.
            strings_model(&state.tabs, [t("Graphics"), t("Sound & interface"), t("Controller"), t("System"), "Server"].map(str::to_owned));
            strings_model(&state.languages, Lang::ALL.iter().map(ToString::to_string));
            strings_model(&state.launchers, LaunchWith::ALL.iter().map(ToString::to_string));
            // Worded lists follow the language on the next sync.
            state.prereqs = None;
            state.gc = None;
            state.gc_key = None;
            state.update_key = None;
            state.tools_key = None;
            state.compat_at = None;
            state.compat = None;
            state.pad = None;
            state.drafts.clear();
        }
        dialogs(&view, app, state);
        if app.page == Page::Settings {
            page(&view, app, state);
        }
    });
}

fn strings() -> SettingsStrings {
    SettingsStrings {
        title: t("Settings").into(),
        description: t("Game options, controller setup and the Windows components FINAL FANTASY XI needs.").into(),
        privacy: t("Privacy mode").into(),
        privacy_tip: t("Blurs names, IDs and messages for screenshots.").into(),
        language: t("Language").into(),
        updates_title: t("Launcher updates").into(),
        updates_auto: t("Check for launcher updates when Crystal opens").into(),
        updates_note: t("Updates are signed and verified before installation. Running games stay open.").into(),
        updates_restart: t("Restart automatically to install updates").into(),
        updates_restart_note: t("Only when no game is running and Crystal is not in use, after a 10-second notice.").into(),
        nav_label: t("Navigate Crystal with a controller").into(),
        nav_note_1: t("D-pad or left stick moves focus. Bottom face button selects; right face button goes back. Shoulders change pages; Start activates Play or Update.").into(),
        nav_note_2: t("Select a dropdown or slider, adjust it with the D-pad, then select again to finish. Text entry and Windows file dialogs use the keyboard and mouse.").into(),
        sign_in_label: t("Sign accounts in when Crystal opens").into(),
        sign_in_note: t("Accounts with a one-time password sign in when you press Play, unless they have an authenticator key.").into(),
        keep_label: t("Keep PlayOnline signed in after FFXI closes").into(),
        keep_note: t("Keeps the session for the next Play. Applies to new sign-ins.").into(),
        instant_label: t("Close games instantly").into(),
        instant_note: t("Ends the game at once, like Windower's terminate.").into(),
        item_search_label: t("Item and wiki search").into(),
        start_label: t("Start Crystal in the background when Windows starts").into(),
        tray_label: t("Closing the window keeps Crystal running in the notification area").into(),
        tray_note: t("Quit from the notification icon to sign them out.").into(),
        tools_title: t("Launch tools").into(),
        tools_sub: t("Leave a tool folder empty to find it automatically.").into(),
        detect_again: t("Detect again").into(),
        default_launcher: t("Default launcher").into(),
        default_note: t("Retail accounts set to Use default start with this. Each account can choose its own launcher, boot config and profile in its account settings.").into(),
        boot_label: t("Base boot config").into(),
        boot_placeholder: t("Built-in minimal config").into(),
        profile_label: t("Windower profile").into(),
        profile_placeholder: t("Windower's default profile").into(),
        edit_profiles: t("Edit profiles in Windower").into(),
        profile_note: t("Video and sound settings come from this profile. Launches through Windower start one at a time.").into(),
        admin_note: t("Ashita needs administrator rights: run the launcher as administrator, or accept Windows' prompt at each launch.").into(),
        found: t("Found").into(),
        not_found: t("Not found").into(),
        owg_title: t("Open with the game").into(),
        owg_sub: t("Started once when the first game opens, with the same rights as the game.").into(),
        program_path: t("Program path").into(),
        arguments: t("Arguments (optional)").into(),
        enabled: t("Enabled").into(),
        remove: t("Remove").into(),
        add_program: t("Add program").into(),
        compat_title: t("Game compatibility").into(),
        compat_none: t("Checked the next time a game starts.").into(),
        components_title: t("Windows components").into(),
        check_again: t("Check again").into(),
        components_intro: t("FINAL FANTASY XI is a 32-bit DirectX 8 game that relies on these parts of Windows. Checking is read-only: nothing is downloaded or changed until you press Install or Enable, and every download must be signed by Microsoft before it runs.").into(),
        will_run: t("Will run").into(),
        manual_toggle: t("How to enable it manually").into(),
        open_features: t("Open Windows Features").into(),
        about_name: "Crystal Launcher".into(),
        about_version: format!("Version {} · GPL-3.0-only", env!("CARGO_PKG_VERSION")).into(),
        about_1: t("Independent launcher. Not affiliated with or endorsed by Square Enix.").into(),
        about_2: t("Code licensing does not grant rights to game artwork or trademarks. See the included LICENSE and THIRD_PARTY.md.").into(),
        about_3: "FINAL FANTASY XI © 2002 - 2020 SQUARE ENIX CO., LTD. All Rights Reserved.".into(),
        about_4: "FINAL FANTASY — registered trademark of Square Enix Holdings Co., Ltd.".into(),
        dg_title: t("Use dgVoodoo (DirectX 8 → 11 wrapper)").into(),
        dg_body: t("Draws the game through Direct3D 11, which can fix black screens, stutter and alt-tab problems on newer graphics cards. Crystal downloads it from the official GitHub release, checks it, and sets it up at each launch.").into(),
        dg_open: t("Open dgVoodoo settings").into(),
        dg_reset: t("Reset to Crystal defaults").into(),
        try_again: t("Try again").into(),
        rs_body: t("Optional color correction, sharpening and shader presets. Requires dgVoodoo; available for Vanilla, Windower and Ashita.").into(),
        rs_setup: t("Set up ReShade").into(),
        rs_preset: t("Choose preset…").into(),
        rs_folder: t("Open ReShade folder").into(),
        rs_website: t("ReShade website").into(),
        gc_reading: t("Reading the game's settings…").into(),
        gc_for: t("Settings for").into(),
        gc_defaults: t("Restore defaults").into(),
        gc_no_backups: t("No backups yet").into(),
        gc_restore: t("Restore backup").into(),
        gc_discard: t("Discard").into(),
        gc_apply: t("Apply").into(),
        gc_clean: t("No unsaved changes").into(),
        gc_menu_note: t("Menu sizes applied here are kept for Vanilla and Ashita. Windower uses its selected profile's resolution and UI scale.").into(),
        gc_locked_note: t("Apply or discard your changes to switch where settings are saved.").into(),
        gc_admin: t("Run Crystal Launcher as administrator to save registry settings.").into(),
        gc_use_default: t("Use default").into(),
        gc_folder_placeholder: t("The game's own screenshot folder").into(),
        pad_retry: t("Retry").into(),
        pad_live: t("Live test").into(),
        pad_vibrate: t("Test vibration").into(),
        pad_bindings: t("Button bindings").into(),
        pad_reset: t("Reset to default").into(),
        pad_unbind: t("Unbind").into(),
        pad_options: t("Game options").into(),
        close_title: t("Close the launcher?").into(),
        close_body: t("Account changes are still being saved. Wait for saving to finish before closing.").into(),
        close_keep: t("Keep launcher open").into(),
        close_now: t("Close now").into(),
        import_title: t("Import accounts").into(),
        import_sub: t("Your source files stay unchanged.").into(),
        import_intro: t("Import from PlayOnline's member list or Forest.").into(),
        import_note: t("Some sources have no passwords; enter them in the account editor.").into(),
        import_detected: t("Detected account files").into(),
        import_file: t("Account file").into(),
        import_file_placeholder: t("Choose an account file").into(),
        browse: t("Browse…").into(),
        region: t("Region").into(),
        import_companion: t("Match Forest profiles with a PlayOnline member file").into(),
        import_companion_file: t("PlayOnline member file").into(),
        import_empty: t("No saved accounts were found in this file.").into(),
        import_review: t("Review account").into(),
        close: t("Close").into(),
    }
}

fn page(view: &SettingsView, app: &App, state: &mut State) {
    let tab = TABS.iter().position(|tab| *tab == app.settings_tab).unwrap_or(0);
    put!(view, get_tab, set_tab, tab as i32);
    system(view, app, state);
    match app.settings_tab {
        Tab::Graphics => {
            graphics(view, app);
            game_options(view, app, state, crate::game_config::Section::Graphics);
        }
        Tab::Sound => game_options(view, app, state, crate::game_config::Section::Sound),
        Tab::System => {
            tools(view, app, state);
            programs(view, app, state);
            compat(view, state, app);
            components(view, app, state);
        }
        Tab::Controller => controller(view, app, state),
        Tab::Server => server(view, app, state),
    }
}

/// Settings → Server (`server_admin_page::front`).
fn server(view: &SettingsView, app: &App, state: &mut State) {
    let front = crate::server_admin_page::front(app);
    let admin = &app.server_admin;
    put!(view, get_server, set_server, ServerState {
        connected: front.connected,
        busy: front.busy,
        host_editable: crate::privacy::editable(),
        error: crate::privacy::mask(&front.error).into(),
        notice: front.notice.into(),
        has_status: front.has_status,
        online: front.online.into(),
        versions: front.versions.into(),
        approved: front.approved,
        maintenance: front.maintenance,
        maint_locked: front.maint_locked,
        motd_saved: front.motd_saved.into(),
        overridden: front.overridden.into(),
        live: front.live.into(),
        can_request: front.can_request,
        has_archived: front.has_archived,
        pending: front.pending,
        held: front.held.into(),
        last_result: front.last_result.into(),
        last_ok: front.last_ok,
        verify_present: front.verify_present,
        verify_chip: front.verify_chip.into(),
        game_folder: front.game_folder.into(),
        verify_running: front.verify_running,
        verify_line: front.verify_line.into(),
        verify_fraction: front.verify_fraction,
        verify_action: front.verify_action.into(),
    });
    if state.staged_values != front.staged {
        strings_model(&state.server_staged, front.staged.iter().cloned());
        state.staged_values = front.staged;
    }
    put!(view, get_server_host, set_server_host, crate::privacy::mask(&admin.host).into());
    put!(view, get_server_token, set_server_token, SharedString::from(admin.token.as_str()));
    put!(view, get_server_motd, set_server_motd, SharedString::from(admin.motd.as_str()));
}

/// The header's Privacy mode and language, and System's switches and launcher updates (the
/// update line only while System shows, and masked again only when it or privacy mode moved).
fn system(view: &SettingsView, app: &App, state: &mut State) {
    let set = &app.settings;
    let mut shown = view.get_system();
    let mut system = SystemState {
        privacy: set.privacy,
        language: Lang::ALL.iter().position(|lang| *lang == crate::i18n::current()).map_or(-1, |i| i as i32),
        updates_auto: set.launcher_auto_updates,
        updates_restart: set.launcher_auto_restart,
        nav: set.controller_navigation,
        sign_in: set.auto_sign_in,
        keep: set.sign_in_after_game,
        instant: set.close_games_instantly,
        item_search: set.item_search,
        start: set.start_with_windows,
        start_enabled: !app.startup_task_busy(),
        tray: set.close_to_tray,
        updates_status: std::mem::take(&mut shown.updates_status),
        updates_action: std::mem::take(&mut shown.updates_action),
    };
    if app.settings_tab == Tab::System {
        let (status, action) = crate::self_update::front::status(app);
        state.update_action = action.map(|(_, action)| action);
        system.updates_action = action.map(|(label, _)| label).unwrap_or_default().into();
        let key = (status, crate::privacy::on());
        if state.update_key.as_ref() != Some(&key) {
            system.updates_status = private(app, &key.0).into();
            state.update_key = Some(key);
        }
    }
    put!(view, get_system, set_system, system);
}

/// launch_tools::tool_row's state.
fn tool_row(name: &str, found: Option<&std::path::Path>, value: &str, placeholder: &str, missing_auto: &str, missing_set: &str) -> ToolRow {
    let (caption, caption_path) = match found {
        Some(path) => (crate::privacy::mask(&path.display().to_string()), true),
        None => ((if value.trim().is_empty() { missing_auto } else { missing_set }).to_owned(), false),
    };
    ToolRow {
        name: name.into(),
        found: found.is_some(),
        placeholder: placeholder.into(),
        editable: crate::privacy::editable(),
        caption: caption.into(),
        caption_path,
    }
}

/// Everything the launch tools card is built from, hashed (no copies kept).
fn tools_key(app: &App) -> u64 {
    let set = &app.settings;
    let found = &app.launch_tools.detected;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (&found.ashita, &found.windower, &found.loader, &found.boot_inis, &found.profiles).hash(&mut hasher);
    (&set.ashita_boot_ini, &set.windower_profile, &set.ashita_path, &set.windower_path, &set.loader_path).hash(&mut hasher);
    LaunchWith::ALL.iter().position(|tool| *tool == set.launch_with.retail()).hash(&mut hasher);
    (crate::privacy::on(), crate::privacy::editable(), app.windower_editor_open).hash(&mut hasher);
    hasher.finish()
}

fn tools(view: &SettingsView, app: &App, state: &mut State) {
    let key = tools_key(app);
    if state.tools_key == Some(key) {
        return;
    }
    state.tools_key = Some(key);
    let set = &app.settings;
    let found = &app.launch_tools.detected;
    // Ashita's base boot config: the configs found, the saved one first if it is missing.
    let mut boot: Vec<(String, String)> = found.boot_inis.iter().map(|name| (name.clone(), name.clone())).collect();
    let current_ini = if set.ashita_boot_ini.trim().is_empty() {
        crate::pol_less::tools::DEFAULT_BOOT_INI.to_owned()
    } else {
        set.ashita_boot_ini.trim().to_owned()
    };
    let mut boot_current = boot.iter().position(|(value, _)| value.eq_ignore_ascii_case(&current_ini));
    let selected_ini = boot_current.is_some();
    if boot_current.is_none() && !set.ashita_boot_ini.trim().is_empty() {
        boot.insert(0, (current_ini.clone(), tf("{name} (missing)", &[("name", &current_ini)])));
        // The placeholder shows here: the missing entry is listed, not chosen.
        boot_current = None;
    }
    let boot_note = if found.ashita.is_none() {
        t("Set Ashita's folder to choose one.").to_owned()
    } else if selected_ini {
        t("Copied for each account as config\\boot\\crystal-<account>.ini, with only the boot file and command pointed at the loader. Passwords are never written to Ashita's files.").to_owned()
    } else if set.ashita_boot_ini.trim().is_empty() {
        t("No default.ini here: a minimal boot config is used. Choose one of yours to keep its fonts, window and game settings.").to_owned()
    } else {
        tf("{name} is not in Ashita's config\\boot folder.", &[("name", &current_ini)])
    };
    // Windower's profile: its default, the profiles found, the saved one last if it is missing.
    let mut profiles: Vec<(String, String)> = vec![(String::new(), t("Windower's default profile").to_owned())];
    profiles.extend(found.profiles.iter().map(|name| (name.clone(), name.clone())));
    let current_profile = set.windower_profile.trim();
    let profile_current = match profiles.iter().position(|(value, _)| value == current_profile) {
        Some(i) => i,
        None => {
            profiles.push((current_profile.to_owned(), tf("{name} (not in settings.xml)", &[("name", &current_profile)])));
            profiles.len() - 1
        }
    };
    strings_model(&state.boot_items, boot.iter().map(|(_, label)| label.clone()));
    strings_model(&state.profile_items, profiles.iter().map(|(_, label)| label.clone()));
    state.boot_values = boot.into_iter().map(|(value, _)| value).collect();
    state.profile_values = profiles.into_iter().map(|(value, _)| value).collect();
    put!(view, get_tools, set_tools, ToolsState {
        default_current: LaunchWith::ALL.iter().position(|tool| *tool == set.launch_with.retail()).map_or(-1, |i| i as i32),
        ashita: tool_row(
            crate::pol_less::Tool::Ashita.name(),
            found.ashita.as_deref(),
            &set.ashita_path,
            t("Detect automatically (e.g. C:\\Games\\Final Fantasy XI Programs\\AshitaV4)"),
            t("Not found in the usual folders. Enter the folder that contains Ashita-cli.exe."),
            t("This folder has no Ashita-cli.exe."),
        ),
        windower: tool_row(
            crate::pol_less::Tool::Windower.name(),
            found.windower.as_deref(),
            &set.windower_path,
            t("Detect automatically (e.g. C:\\Games\\Final Fantasy XI Programs\\Windower 4)"),
            t("Not found in the usual folders. Enter the folder that contains Windower.exe."),
            t("This folder has no Windower.exe."),
        ),
        loader: tool_row(
            t("Crystal loader"),
            found.loader.as_deref(),
            &set.loader_path,
            t("pol.exe in the launcher's crystal folder (override for development)"),
            t("pol.exe is not in the launcher's crystal folder. Reinstall the launcher, or enter the loader's path."),
            t("No file at this path."),
        ),
        boot_current: boot_current.map_or(-1, |i| i as i32),
        boot_note: boot_note.into(),
        profile_current: profile_current as i32,
        edit_enabled: found.windower.is_some() && !app.windower_editor_open,
    });
    put!(view, get_ashita_path, set_ashita_path, crate::privacy::mask_path(&set.ashita_path).into());
    put!(view, get_windower_path, set_windower_path, crate::privacy::mask_path(&set.windower_path).into());
    put!(view, get_loader_path, set_loader_path, crate::privacy::mask_path(&set.loader_path).into());
}

fn programs(view: &SettingsView, app: &App, state: &mut State) {
    put!(view, get_programs_editable, set_programs_editable, crate::privacy::editable());
    let rows = app
        .settings
        .open_with_game
        .iter()
        .map(|program| Program { path: crate::privacy::mask_path(&program.path).into(), args: program.args.as_str().into(), enabled: program.enabled })
        .collect();
    sync_model(&state.programs, rows);
}

/// The loader's feature check: looked at no more often than feature_check's own cache refreshes
/// (two seconds), and drawn again only when it says something new.
fn compat(view: &SettingsView, state: &mut State, app: &App) {
    if state.compat_at.is_some_and(|at| at.elapsed() < Duration::from_secs(2)) {
        return;
    }
    state.compat_at = Some(Instant::now());
    let check = crate::feature_check::current(&crate::dgvoodoo::data_root(), &app.settings.loader_path);
    if state.compat.as_ref() == Some(&check) {
        return;
    }
    state.compat = Some(check.clone());
    let compat = match &check {
        None => Compat { has: false, builds: SharedString::new(), chip: SharedString::new(), ok: true },
        Some(check) => Compat {
            has: true,
            builds: check.builds.as_str().into(),
            chip: if check.off.is_empty() {
                tf("All {count} features available", &[("count", &check.total)])
            } else {
                tf("{off} of {count} features off", &[("off", &check.off.len()), ("count", &check.total)])
            }
            .into(),
            ok: check.off.is_empty(),
        },
    };
    put!(view, get_compat, set_compat, compat);
    let off = check
        .map(|check| check.off.into_iter().map(|(name, reason)| FeatureOff { name: name.into(), reason: reason.into() }).collect())
        .unwrap_or_default();
    sync_model(&state.features_off, off);
}

fn prereq_shade(tone: pf::Tone) -> Shade {
    match tone {
        pf::Tone::Sub => Shade::Sub,
        pf::Tone::Ok => Shade::Ok,
        pf::Tone::Warn => Shade::Warn,
        pf::Tone::Danger => Shade::Danger,
        pf::Tone::Accent => Shade::Accent,
    }
}

fn prereq_glyph(glyph: pf::Glyph) -> Glyph {
    match glyph {
        pf::Glyph::Refresh => Glyph::Refresh,
        pf::Glyph::CheckCircle => Glyph::CheckCircle,
        pf::Glyph::AlertCircle => Glyph::AlertCircle,
        pf::Glyph::Download => Glyph::Download,
        pf::Glyph::Wrench => Glyph::Wrench,
        pf::Glyph::Close => Glyph::Close,
    }
}

fn prereq_act(button: Option<&pf::Button>) -> Act {
    match button {
        Some(button) => Act { text: button.label.into(), glyph: prereq_glyph(button.glyph), primary: button.primary, enabled: button.enabled },
        None => Act { text: SharedString::new(), glyph: Glyph::None, primary: false, enabled: false },
    }
}

fn component_card(card: &pf::Card) -> ComponentCard {
    let confirm = card.confirm.as_ref();
    let progress = card.progress.as_ref();
    let result = card.result.as_ref();
    ComponentCard {
        name: card.name.into(),
        glyph: prereq_glyph(card.glyph),
        shade: prereq_shade(card.tone),
        label: card.label.into(),
        version: card.version.clone().unwrap_or_default().into(),
        why: card.why.into(),
        checked: card.checked.clone().unwrap_or_default().into(),
        has_button: card.button.is_some(),
        button: prereq_act(card.button.as_ref()),
        confirm: confirm.is_some(),
        heading: confirm.map(|c| c.heading.as_str()).unwrap_or_default().into(),
        steps: strings_list(confirm.map(|c| c.steps.as_slice()).unwrap_or_default()),
        command: confirm.map(|c| c.command).unwrap_or_default().into(),
        permission: confirm.map(|c| c.permission).unwrap_or_default().into(),
        not_now: prereq_act(confirm.map(|c| &c.not_now)),
        confirm_act: prereq_act(confirm.map(|c| &c.confirm)),
        progress: progress.is_some(),
        head: progress.map(|p| p.head.as_str()).unwrap_or_default().into(),
        detail: progress.map(|p| p.detail.as_str()).unwrap_or_default().into(),
        fraction: progress.and_then(|p| p.fraction).unwrap_or(-1.0),
        can_cancel: progress.is_some_and(|p| p.cancel.is_some()),
        cancel: prereq_act(progress.and_then(|p| p.cancel.as_ref())),
        has_result: result.is_some(),
        result_glyph: result.map_or(Glyph::None, |r| prereq_glyph(r.0)),
        result_shade: result.map_or(Shade::Sub, |r| prereq_shade(r.1)),
        result: result.map(|r| r.2.as_str()).unwrap_or_default().into(),
        has_manual: card.manual.is_some(),
        manual_open: card.manual == Some(true),
        manual_steps: if card.manual.is_some() { strings_list(&pf::manual_steps()) } else { ModelRc::default() },
    }
}

fn components(view: &SettingsView, app: &App, state: &mut State) {
    let front = pf::front(&app.prereqs);
    if state.prereqs.as_ref() == Some(&front) {
        return;
    }
    put!(view, get_components, set_components, Components {
        summary: front.summary.as_str().into(),
        shade: prereq_shade(front.tone),
        glyph: prereq_glyph(front.glyph),
        when: front.when.as_str().into(),
        check_enabled: front.check_again.enabled,
    });
    let mut last = state.prereqs.take().map(|f| f.cards).unwrap_or_default();
    sync_built(&state.component_cards, &mut last, front.cards.clone(), |_, card| component_card(card));
    state.prereqs = Some(front);
}

/// A drawn chip from its SVG document, decoded once per document while the page shows.
fn chip_image(chips: &mut HashMap<String, slint::Image>, svg: &str) -> slint::Image {
    if let Some(image) = chips.get(svg) {
        return image.clone();
    }
    // A handful of documents per controller and theme; a long session's lit variants stay few.
    if chips.len() >= 128 {
        chips.clear();
    }
    let image = slint::Image::load_from_svg_data(svg.as_bytes()).unwrap_or_default();
    chips.insert(svg.to_owned(), image.clone());
    image
}

fn pad_binding(id: usize, binding: &gp::Binding, chips: &mut HashMap<String, slint::Image>) -> PadBinding {
    let (kind, text, lit, picture) = match &binding.chip {
        gp::Chip::Words(words) => (0, SharedString::from(*words), false, slint::Image::default()),
        gp::Chip::Keycap { label, lit } => (1, label.as_str().into(), *lit, slint::Image::default()),
        gp::Chip::Picture { svg, label } => (2, label.as_str().into(), false, chip_image(chips, svg)),
    };
    PadBinding {
        id: id as i32,
        label: binding.label.into(),
        changed: binding.changed,
        capturing: binding.capturing,
        prompt: binding.prompt.into(),
        kind,
        text,
        lit,
        picture,
        live: binding.live,
    }
}

/// Settings > Controller: rebuilt only when the page's data moved; the live
/// picture is decoded again only when what it shows changed.
fn controller(view: &SettingsView, app: &App, state: &mut State) {
    let front = gp::front(app);
    if state.pad.as_ref() == Some(&front) {
        return;
    }
    let badge = chip_image(&mut state.chips, &front.badge);
    let picture = match &state.picture {
        Some((svg, image)) if *svg == front.picture => image.clone(),
        _ => {
            let image = slint::Image::load_from_svg_data(front.picture.as_bytes()).unwrap_or_default();
            state.picture = Some((front.picture.clone(), image.clone()));
            image
        }
    };
    put!(view, get_pad, set_pad, PadState {
        badge,
        title: front.title.as_str().into(),
        sub: front.sub.as_str().into(),
        status: front.status.into(),
        retry: front.retry,
        picture,
        connected: front.connected,
        caption: front.caption.into(),
        lt_label: front.triggers[0].0.into(),
        lt: front.triggers[0].1,
        rt_label: front.triggers[1].0.into(),
        rt: front.triggers[1].1,
        left_stick: front.sticks[0].as_str().into(),
        right_stick: front.sticks[1].as_str().into(),
        rumble: front.rumble,
        bindings_sub: front.bindings_sub.as_str().into(),
    });
    sync_model(&state.pad_choices, front.pads.iter().map(|(id, name, active)| PadChoice { id: *id as i32, name: name.as_str().into(), active: *active }).collect());
    sync_model(&state.pad_keys, front.extras.iter().map(|(label, lit)| PadKey { label: (*label).into(), lit: *lit }).collect());
    // The rows split into two columns, the first one taking the odd one.
    let half = front.bindings.len().div_ceil(2);
    let last = state.pad.take().map(|f| f.bindings).unwrap_or_default();
    let (mut last_left, mut last_right): (Vec<gp::Binding>, Vec<gp::Binding>) = if last.len() == front.bindings.len() {
        (last[..half].to_vec(), last[half..].to_vec())
    } else {
        (Vec::new(), Vec::new())
    };
    let chips = &mut state.chips;
    sync_built(&state.pad_left, &mut last_left, front.bindings[..half].to_vec(), |i, binding| pad_binding(i, binding, chips));
    sync_built(&state.pad_right, &mut last_right, front.bindings[half..].to_vec(), |i, binding| pad_binding(half + i, binding, chips));
    sync_model(&state.pad_switches, front.switches.iter().map(|switch| PadSwitch { label: switch.label.into(), on: switch.on, enabled: switch.enabled }).collect());
    sync_model(&state.pad_notes, front.notes.iter().map(|(text, tone)| PadNote {
        text: text.as_str().into(),
        shade: match tone {
            gp::Tone::Sub => Shade::Sub,
            gp::Tone::Warning => Shade::PalWarn,
            gp::Tone::Success => Shade::PalOk,
            gp::Tone::Danger => Shade::PalDanger,
        },
    }).collect());
    state.pad = Some(front);
}

fn graphics(view: &SettingsView, app: &App) {
    use crate::dgvoodoo::front::{Actions, Chip};
    let dg = crate::dgvoodoo::front::front(app);
    let (progress_label, progress_detail, fraction) = dg.progress.clone().unwrap_or_default();
    put!(view, get_dg, set_dg, Dg {
        on: dg.on,
        chip: dg.chip.into(),
        chip_shade: match dg.chip_tone {
            Chip::Accent => Shade::Accent,
            Chip::Ok => Shade::GfxOk,
            Chip::Sub => Shade::Sub,
        },
        progress: dg.progress.is_some(),
        progress_label: progress_label.into(),
        progress_detail: progress_detail.into(),
        fraction,
        error: dg.error.map(|error| private(app, &error)).unwrap_or_default().into(),
        actions: match dg.actions {
            None => 0,
            Some(Actions::Installed { .. }) => 1,
            Some(Actions::Retry) => 2,
        },
        remove_enabled: matches!(dg.actions, Some(Actions::Installed { remove: true })),
        note: dg.note.into(),
        info: private(app, &dg.info.join("\n")).into(),
        import: dg.import.unwrap_or_default().into(),
    });

    // reshade::view's state.
    let rs = &app.reshade;
    let can_edit = crate::reshade::editable(app);
    let detail = if rs.busy {
        rs.status.clone()
    } else if !can_edit {
        t("Close games and sign out PlayOnline sessions before changing ReShade.").into()
    } else if !app.settings.dgvoodoo {
        t("Enable dgVoodoo before using ReShade.").into()
    } else if !rs.status.is_empty() {
        rs.status.clone()
    } else if rs.installed {
        tf("Installed v{version}", &[("version", &crate::reshade::VERSION)])
    } else {
        t("Not installed").into()
    };
    let preset = if rs.preset.is_empty() {
        String::new()
    } else {
        let name = std::path::Path::new(&rs.preset).file_name().and_then(|p| p.to_str()).unwrap_or(&rs.preset);
        name.split_once('-')
            .filter(|(hash, _)| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .map_or(name, |(_, name)| name)
            .to_owned()
    };
    put!(view, get_rs, set_rs, Rs {
        on: app.settings.reshade.enabled,
        enabled: can_edit,
        setup_enabled: can_edit && app.settings.dgvoodoo,
        preset_enabled: can_edit && rs.installed,
        folder_enabled: !rs.busy,
        detail: detail.into(),
        preset: preset.into(),
        error: rs.error.as_ref().map(|error| private(app, error)).unwrap_or_default().into(),
    });
}

fn gc_shade(tone: crate::game_config::Tone) -> Shade {
    use crate::game_config::Tone;
    match tone {
        Tone::Info => Shade::Accent,
        Tone::Success => Shade::GfxOk,
        Tone::Warning => Shade::GfxWarn,
        Tone::Error => Shade::GfxError,
    }
}

fn gc_row(id: usize, row: &gcf::Row) -> GcRow {
    let mut out = GcRow {
        id: id as i32,
        kind: 0,
        label: row.label.into(),
        help: row.help.into(),
        changed: row.changed,
        items: ModelRc::default(),
        current: -1,
        on: false,
        value: 0.0,
        minimum: 0.0,
        maximum: 1.0,
        step: 1.0,
        has_reset: false,
        reset: 0.0,
        shown: SharedString::new(),
        text: SharedString::new(),
        editable: false,
        filled: false,
    };
    match &row.control {
        gcf::Control::Choice { labels, current, .. } => {
            out.items = strings_list(labels);
            out.current = *current as i32;
        }
        gcf::Control::Switch { on, .. } => {
            out.kind = 1;
            out.on = *on;
        }
        gcf::Control::Slider { value, min, max, step, reset, shown, .. } => {
            out.kind = 2;
            out.value = *value;
            out.minimum = *min;
            out.maximum = *max;
            out.step = *step;
            out.has_reset = reset.is_some();
            out.reset = reset.unwrap_or_default();
            out.shown = shown.as_str().into();
        }
        gcf::Control::Info(text) => {
            out.kind = 3;
            out.text = (*text).into();
        }
        gcf::Control::Folder { shown, editable, filled } => {
            out.kind = 4;
            out.text = shown.as_str().into();
            out.editable = *editable;
            out.filled = *filled;
        }
    }
    out
}

/// One column of option cards: each card keeps its rows model; only changed rows are rebuilt.
fn gc_column(cards_model: &VecModel<GcCard>, rows_models: &mut Vec<Rc<VecModel<GcRow>>>, last: Option<&[gcf::Card]>, cards: &[gcf::Card], first_id: &mut usize) {
    rows_models.truncate(cards.len());
    while rows_models.len() < cards.len() {
        rows_models.push(Rc::new(VecModel::default()));
    }
    for (i, card) in cards.iter().enumerate() {
        let start = *first_id;
        *first_id += card.rows.len();
        let mut previous = last.and_then(|cards| cards.get(i)).map(|c| c.rows.clone()).unwrap_or_default();
        sync_built(&rows_models[i], &mut previous, card.rows.clone(), |n, row| gc_row(start + n, row));
        let shown = GcCard { title: card.title.into(), subtitle: card.subtitle.into(), rows: ModelRc::from(rows_models[i].clone()) };
        if i < cards_model.row_count() {
            if cards_model.row_data(i).as_ref() != Some(&shown) {
                cards_model.set_row_data(i, shown);
            }
        } else {
            cards_model.push(shown);
        }
    }
    for i in (cards.len()..cards_model.row_count()).rev() {
        cards_model.remove(i);
    }
}

fn game_options(view: &SettingsView, app: &App, state: &mut State, section: crate::game_config::Section) {
    let graphics = section == crate::game_config::Section::Graphics;
    // The front is built from the game options alone (and privacy mode, which masks the folder).
    let key = (app.game_config.revision(), graphics, crate::privacy::on());
    if state.gc_key == Some(key) {
        return;
    }
    state.gc_key = Some(key);
    let front = gcf::front(&app.game_config, section);
    if state.gc.as_ref().is_some_and(|(last, tab)| *tab == graphics && *last == front) {
        return;
    }
    let pending = front.pending;
    let notice = front.notice.clone();
    put!(view, get_gc, set_gc, GcBar {
        loaded: front.loaded,
        region: front.region as i32,
        locked: front.locked(),
        pending: pending > 0,
        pending_text: (if pending == 1 { t("1 unsaved change").to_owned() } else { tf("{count} unsaved changes", &[("count", &pending)]) }).into(),
        has_backups: !front.backups.is_empty(),
        discard_enabled: pending > 0,
        apply_enabled: pending > 0 && front.writable,
        location: front.location.as_str().into(),
        locked_note: front.locked() && front.regions.len() > 1,
        not_writable: !front.writable,
        notice: notice.is_some(),
        notice_shade: notice.as_ref().map_or(Shade::Accent, |(tone, _)| gc_shade(*tone)),
        notice_success: notice.as_ref().is_some_and(|(tone, _)| *tone == crate::game_config::Tone::Success),
        notice_text: notice.map(|(_, text)| text).unwrap_or_default().into(),
    });
    strings_model(&state.gc_regions, front.regions.iter().map(|(label, _)| label.clone()));
    strings_model(&state.gc_backups, front.backups.iter().map(|entry| entry.label.clone()));
    let last = state.gc.take().filter(|(_, tab)| *tab == graphics).map(|(front, _)| front);
    let mut id = 0;
    gc_column(&state.gc_left, &mut state.gc_rows_left, last.as_ref().map(|f| f.left.as_slice()), &front.left, &mut id);
    gc_column(&state.gc_right, &mut state.gc_rows_right, last.as_ref().map(|f| f.right.as_slice()), &front.right, &mut id);
    state.gc_rows = front.left.iter().chain(&front.right).flat_map(|card| card.rows.iter().cloned()).collect();
    state.gc = Some((front, graphics));
}

/// The close confirmation and the import dialog, stacked over the account sheet.
fn dialogs(view: &SettingsView, app: &App, state: &mut State) {
    put!(view, get_close_open, set_close_open, app.confirm_close);
    let import = &app.account_import;
    let open = import.open && !app.confirm_close && app.pending_delete.is_none() && app.editor.is_none();
    put!(view, get_import_open, set_import_open, open);
    if !open {
        // A closed dialog keeps no file paths in the window.
        put!(view, get_import_path, set_import_path, SharedString::new());
        put!(view, get_import_companion, set_import_companion, SharedString::new());
        return;
    }
    let sources: Vec<String> = import
        .sources
        .iter()
        .map(|source| {
            let kind = match source.kind {
                crate::account_import::SourceKind::Forest => "Forest",
                crate::account_import::SourceKind::PlayOnline => "PlayOnline",
            };
            format!("{kind} · {}", crate::privacy::mask_path(&source.path.to_string_lossy()))
        })
        .collect();
    strings_model(&state.import_sources, sources);
    put!(view, get_import, set_import, ImportState {
        busy: import.busy,
        show_sources: !import.sources.is_empty() && !import.busy,
        source_current: import.sources.iter().position(|s| s.path.to_string_lossy() == import.path).map_or(-1, |i| i as i32),
        editable: !import.busy && crate::privacy::editable(),
        region: i32::from(import.region),
        use_companion: import.use_companion,
        error: import.error.as_str().into(),
        no_drafts: import.batch.as_ref().is_some_and(|batch| batch.drafts.is_empty()),
        read_label: (if import.busy { t("Reading…") } else { t("Read accounts") }).into(),
        read_enabled: !import.busy && !import.path.trim().is_empty(),
    });
    // --preview-import-end: the account list scrolled to its end (clamped by the dialog).
    put!(view, get_import_scroll, set_import_scroll, if crate::preview::has("--preview-import-end") { 100_000.0 } else { 0.0 });
    put!(view, get_import_path, set_import_path, crate::privacy::mask_path(&import.path).into());
    put!(view, get_import_companion, set_import_companion, crate::privacy::mask_path(&import.companion).into());
    let batch = import.batch.as_ref();
    strings_model(&state.import_warnings, batch.map(|b| b.warnings.clone()).unwrap_or_default());
    let drafts: Vec<(String, String, Vec<String>)> = batch
        .map(|b| {
            b.drafts
                .iter()
                .map(|draft| {
                    let missing = draft.missing_fields().into_iter().map(t).collect::<Vec<_>>().join(", ");
                    let detail = if missing.is_empty() { t("Ready to review").into() } else { tf("Missing: {fields}", &[("fields", &missing)]) };
                    (crate::privacy::mask(&draft.label), detail, draft.notes.clone())
                })
                .collect()
        })
        .unwrap_or_default();
    let mut last = std::mem::take(&mut state.drafts);
    sync_built(&state.import_drafts, &mut last, drafts, |_, (label, detail, notes)| Draft {
        label: label.as_str().into(),
        detail: detail.as_str().into(),
        notes: strings_list(notes),
    });
    state.drafts = last;
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};

    struct Headless(Rc<MinimalSoftwareWindow>);
    impl slint::platform::Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    /// A Slint window on the software renderer (no window on screen), wired and synced.
    fn window(app: &App) -> MainWindow {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        window.set_size(slint::PhysicalSize::new(1440, 900));
        let _ = slint::platform::set_platform(Box::new(Headless(window.clone())));
        let ui = MainWindow::new().unwrap();
        wire(&ui);
        super::super::view::wire(&ui);
        super::super::view::sync(&ui, app);
        sync(&ui, app);
        ui.show().unwrap();
        ui
    }

    fn sent() -> Vec<Message> {
        SENT.with(|sent| std::mem::take(&mut *sent.borrow_mut()))
    }

    fn settings_app(tab: Tab) -> App {
        let mut app = crate::tests::app();
        app.page = Page::Settings;
        app.settings_tab = tab;
        app
    }

    #[test]
    fn the_system_tab_shows_the_settings_and_its_controls_send_their_messages() {
        let mut app = settings_app(Tab::System);
        app.settings.auto_sign_in = true;
        app.settings.close_to_tray = false;
        app.settings.open_with_game = vec![crate::open_with_game::Program { path: r"C:\Tools\Voice.exe".into(), args: "-m".into(), enabled: true }];
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        assert_eq!(view.get_tab(), 3);
        assert_eq!(view.get_tabs().row_count(), 5);
        let system = view.get_system();
        assert!(system.sign_in && !system.tray);
        assert_eq!(view.get_strings().title, t("Settings"));
        assert_eq!(view.get_programs().row_count(), 1);
        assert_eq!(view.get_programs().row_data(0).unwrap().path, r"C:\Tools\Voice.exe");
        assert_eq!(view.get_component_cards().row_count(), 3);
        assert_eq!(view.get_launchers().row_count(), LaunchWith::ALL.len());
        sent();
        view.invoke_set_sign_in(false);
        view.invoke_set_tray(true);
        view.invoke_pick_tab(0);
        view.invoke_tools_default(0);
        view.invoke_tools_browse(2);
        view.invoke_program_enabled(0, false);
        view.invoke_program_add();
        view.invoke_set_language(1);
        let messages = sent();
        assert!(matches!(messages[0], Message::AutoSignIn(false)));
        assert!(matches!(messages[1], Message::CloseToTray(true)));
        assert!(matches!(messages[2], Message::SettingsTab(Tab::Graphics)));
        assert!(matches!(messages[3], Message::LaunchTools(crate::launch_tools::Msg::DefaultLaunchWith(LaunchWith::Vanilla))));
        assert!(matches!(messages[4], Message::Browse(PickTarget::LoaderFile)));
        assert!(matches!(messages[5], Message::OpenWithGame(crate::open_with_game::Msg::Enabled(0, false))));
        assert!(matches!(messages[6], Message::OpenWithGame(crate::open_with_game::Msg::Add)));
        assert!(matches!(messages[7], Message::SetLanguage(Lang::Ja)));
    }

    /// Settings → Server shows the server's state once connected, and its buttons send their
    /// messages (a Publish button names its staged version).
    #[test]
    fn the_server_tab_shows_the_servers_state_and_its_controls_send_their_messages() {
        let mut app = settings_app(Tab::Server);
        app.server_admin.login = Some(crate::server_admin::Login { host: "ffxi.example.test".into(), token: "x".repeat(32) });
        app.server_admin.host = "ffxi.example.test".into();
        app.server_admin.motd = "Patch day".into();
        app.server_admin.status = Some(serde_json::from_str(r#"{
            "playersOnline": 3, "clientVersion": "30261006_0", "lsbCommit": "131cf71ed6",
            "gate": {"updateApproved": true, "maintenance": false, "motd": "Patch day"},
            "xiloader": {"live": {"version": "3.0.0", "sha256": "68A1DE4112046AFF"}, "staged": ["3.0.1", "3.0.2"], "archived": ["2.2.0"]},
            "verify": {"currentPresent": true}
        }"#).unwrap());
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        assert_eq!(view.get_tab(), 4);
        assert_eq!(view.get_tabs().row_data(4).unwrap(), "Server");
        let server = view.get_server();
        assert!(server.connected && server.has_status && server.approved && !server.maintenance);
        assert_eq!(server.online, "3 online");
        assert_eq!(server.verify_chip, "30261006_0 ready");
        assert_eq!(view.get_server_host(), "ffxi.example.test");
        assert_eq!(view.get_server_motd(), "Patch day");
        assert_eq!(view.get_server_staged().row_count(), 2);
        sent();
        view.invoke_server_publish(1);
        view.invoke_server_approve(false);
        view.invoke_server_rollback();
        view.invoke_server_verify();
        let messages = sent();
        use crate::server_admin_page::Msg as Server;
        assert!(matches!(&messages[0], Message::ServerAdmin(Server::Xiloader("promote", Some(version))) if version == "3.0.2"));
        assert!(matches!(messages[1], Message::ServerAdmin(Server::SetUpdateApproved(false))));
        assert!(matches!(messages[2], Message::ServerAdmin(Server::Xiloader("rollback", None))));
        assert!(matches!(messages[3], Message::ServerAdmin(Server::GenerateVerify)));
    }

    #[test]
    fn privacy_masks_the_paths_and_makes_them_read_only() {
        crate::privacy::set(true);
        let mut app = settings_app(Tab::System);
        app.settings.windower_path = r"D:\Games\Windower".into();
        app.settings.open_with_game = vec![crate::open_with_game::Program { path: r"C:\Tools\Voice.exe".into(), args: String::new(), enabled: true }];
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        assert_eq!(view.get_windower_path(), crate::privacy::mask_path(r"D:\Games\Windower"));
        assert!(!view.get_windower_path().contains("Games"));
        assert!(!view.get_tools().windower.editable);
        assert!(!view.get_programs_editable());
        assert!(!view.get_programs().row_data(0).unwrap().path.contains("Tools"));
        crate::privacy::set(false);
    }

    #[test]
    fn an_unchanged_state_sets_nothing_and_a_changed_option_updates_its_row_in_place() {
        let mut app = settings_app(Tab::Graphics);
        app.game_config = gcf::preview_state();
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        let gc = view.get_gc();
        assert!(gc.loaded, "the preview reads synthetic game options");
        let left = view.get_gc_left();
        assert_eq!(left.row_count(), 2);
        let rows = left.row_data(0).unwrap().rows;
        let aspect = rows.row_data(3).unwrap();
        assert_eq!(aspect.kind, 1);
        // A second sync with nothing changed keeps every model and row.
        sync(&ui, &app);
        assert!(view.get_gc_left().row_data(0).unwrap().rows == rows, "the card keeps its rows model");
        // The switch sends its message; the core's change shows in the same row.
        sent();
        view.invoke_gc_toggle(aspect.id, !aspect.on);
        let messages = sent();
        let Some(Message::GameConfig(msg)) = messages.into_iter().next() else { panic!("a game option message") };
        let _ = crate::game_config::update(&mut app.game_config, msg);
        sync(&ui, &app);
        let after = view.get_gc_left().row_data(0).unwrap().rows;
        assert!(after == rows, "the same rows model");
        assert_eq!(after.row_data(3).unwrap().on, !aspect.on);
        assert!(after.row_data(3).unwrap().changed);
        // A slider row and a dropdown row send their own messages.
        let gamma = view.get_gc_left().row_data(1).unwrap().rows.row_data(0).unwrap();
        assert_eq!(gamma.kind, 2);
        view.invoke_gc_slide(gamma.id, 0.5);
        let window_mode = rows.row_data(0).unwrap();
        view.invoke_gc_pick(window_mode.id, 0);
        let messages = sent();
        assert!(matches!(messages[0], Message::GameConfig(crate::game_config::Msg::Gamma(g)) if (g - 0.5).abs() < 1e-6));
        assert!(matches!(messages[1], Message::GameConfig(crate::game_config::Msg::Set(..))));
        assert!(view.get_dg().chip != "");
    }

    #[test]
    fn the_close_and_import_dialogs_stack_over_the_sheet() {
        let mut app = crate::tests::app();
        app.confirm_close = true;
        app.account_import.open = true;
        app.account_import.path = r"C:\POL\login_w.bin".into();
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        assert!(view.get_close_open());
        assert!(!view.get_import_open(), "the close confirmation takes precedence");
        sent();
        view.invoke_close_keep();
        view.invoke_close_now();
        let messages = sent();
        assert!(matches!(messages[0], Message::CancelClose));
        assert!(matches!(messages[1], Message::ConfirmClose));
        app.confirm_close = false;
        sync(&ui, &app);
        assert!(view.get_import_open());
        assert_eq!(view.get_import_path(), r"C:\POL\login_w.bin");
        assert!(view.get_import().read_enabled);
        view.invoke_import_region(2);
        view.invoke_import_read();
        view.invoke_import_close();
        let messages = sent();
        assert!(matches!(messages[0], Message::AccountImport(crate::account_import_ui::Msg::Region(2))));
        assert!(matches!(messages[1], Message::AccountImport(crate::account_import_ui::Msg::Read)));
        assert!(matches!(messages[2], Message::AccountImport(crate::account_import_ui::Msg::Close)));
    }

    use super::super::pad;

    /// Whole-page renders of every Settings tab (the window as tall as the page, software
    /// renderer) into CRYSTAL_SLINT_SHOTS: `cargo test --no-default-features
    /// render_the_settings_pages -- --ignored`. Run alone: it switches the language.
    #[test]
    #[ignore]
    fn render_the_settings_pages() {
        let Some(dir) = std::env::var_os("CRYSTAL_SLINT_SHOTS").map(std::path::PathBuf::from) else { return };
        std::fs::create_dir_all(&dir).unwrap();
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let _ = slint::platform::set_platform(Box::new(Headless(window.clone())));
        let ui = MainWindow::new().unwrap();
        wire(&ui);
        super::super::view::wire(&ui);
        ui.show().unwrap();
        let shots = [
            ("graphics", Tab::Graphics, false, false, false),
            ("graphics", Tab::Graphics, true, false, false),
            ("sound", Tab::Sound, false, false, false),
            ("sound", Tab::Sound, true, false, false),
            ("controller", Tab::Controller, false, false, false),
            ("controller", Tab::Controller, true, false, false),
            ("system", Tab::System, false, false, false),
            ("system", Tab::System, true, false, false),
            ("system", Tab::System, false, true, false),
            ("system", Tab::System, true, false, true),
            ("controller", Tab::Controller, false, false, true),
        ];
        for (name, tab, light, private, ja) in shots {
            crate::i18n::set(if ja { Lang::Ja } else { Lang::En });
            crate::privacy::set(private);
            let mut app = settings_app(tab);
            app.settings.light_theme = light;
            app.game_config = gcf::preview_state();
            ui.global::<super::super::Theme>().set_light(light);
            super::super::view::sync(&ui, &app);
            sync(&ui, &app);
            // Taller than any tab: the scroller shows the whole page.
            let (width, height) = (1440u32, if tab == Tab::System { 2500u32 } else { 2000 });
            window.set_size(slint::PhysicalSize::new(width, height));
            let mut pixels = vec![slint::Rgb8Pixel::default(); (width * height) as usize];
            window.request_redraw();
            window.draw_if_needed(|renderer| { renderer.render(&mut pixels, width as usize); });
            let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
            let file = format!("{name}-full-{}{}{}.png", if light { "light" } else { "dark" }, if private { "-private" } else { "" }, if ja { "-ja" } else { "" });
            imagecrate::save_buffer_with_format(dir.join(file), &bytes, width, height, imagecrate::ExtendedColorType::Rgb8, imagecrate::ImageFormat::Png).unwrap();
        }
        crate::i18n::set(Lang::En);
        crate::privacy::set(false);
    }

    /// The names of the controls the focus visits going round with Tab once.
    fn round(w: &slint::Window) -> Vec<String> {
        let mut seen = Vec::new();
        pad::cycle(w, true);
        let first = pad::focused(w);
        for _ in 0..400 {
            seen.push(pad::focused(w).unwrap_or_default());
            pad::cycle(w, true);
            if pad::focused(w) == first { break; }
        }
        seen
    }

    fn missing(seen: &[String], expected: &[&str]) -> Vec<String> {
        expected.iter().filter(|name| !seen.iter().any(|s| s == *name)).map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_controller_walker_reaches_every_system_control() {
        let mut app = settings_app(Tab::System);
        app.settings.open_with_game = vec![crate::open_with_game::Program { path: r"C:\Tools\Voice.exe".into(), args: String::new(), enabled: true }];
        let ui = window(&app);
        let w = ui.window();
        let seen = round(w);
        let (ashita, windower) = (crate::pol_less::Tool::Ashita.name(), crate::pol_less::Tool::Windower.name());
        let mut expected = vec![
            t("Privacy mode"), t("Language"), t("Graphics"), t("Controller"), t("System"),
            t("Check for launcher updates when Crystal opens"), t("Navigate Crystal with a controller"),
            t("Sign accounts in when Crystal opens"), t("Keep PlayOnline signed in after FFXI closes"),
            t("Start Crystal in the background when Windows starts"), t("Closing the window keeps Crystal running in the notification area"),
            t("Detect again"), t("Default launcher"), ashita, t("Base boot config"), windower, t("Windower profile"), t("Crystal loader"),
            t("Program path"), t("Arguments (optional)"), t("Enabled"), t("Remove"), t("Add program"),
        ];
        // Check again takes presses only while no check runs.
        if ui.global::<SettingsView>().get_components().check_enabled { expected.push(t("Check again")); }
        assert_eq!(missing(&seen, &expected), Vec::<String>::new(), "visited: {seen:?}");
        // Each tool's field and its Browse button are both stops.
        assert_eq!(seen.iter().filter(|s| *s == windower).count(), 2, "{seen:?}");
        // The spatial walk from nothing focused gets going too.
        let w = ui.window();
        assert!(pad::step(w, pad::Way::Down));
        assert!(pad::step(w, pad::Way::Down));
    }

    #[test]
    fn the_controller_walker_reaches_the_graphics_controls_and_adjusts_a_slider() {
        let mut app = settings_app(Tab::Graphics);
        app.game_config = gcf::preview_state();
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        // An unsaved change enables Apply and Discard.
        let aspect = view.get_gc_left().row_data(0).unwrap().rows.row_data(3).unwrap();
        view.invoke_gc_toggle(aspect.id, !aspect.on);
        let Some(Message::GameConfig(msg)) = sent().into_iter().next() else { panic!("a game option message") };
        let _ = crate::game_config::update(&mut app.game_config, msg);
        sync(&ui, &app);
        let w = ui.window();
        let gamma = view.get_gc_left().row_data(1).unwrap().rows.row_data(0).unwrap();
        let window_mode = view.get_gc_left().row_data(0).unwrap().rows.row_data(0).unwrap();
        let seen = round(w);
        let expected = [
            t("Use dgVoodoo (DirectX 8 → 11 wrapper)"), "ReShade", t("Set up ReShade"), t("ReShade website"),
            t("Restore defaults"), t("Discard"), t("Apply"), gamma.label.as_str(), window_mode.label.as_str(), aspect.label.as_str(),
        ];
        assert_eq!(missing(&seen, &expected), Vec::<String>::new(), "visited: {seen:?}");
        let regions = view.get_gc_regions();
        if regions.row_count() > 1 {
            // Every region but the one in use (and only while nothing is pending).
            assert!(!seen.iter().any(|s| s.as_str() == regions.row_data(1).unwrap().as_str()), "a pending change holds the region");
        }
        // Select the slider, step right with the D-pad: the gamma message, one step up.
        assert!(pad::focus_named(w, &gamma.label));
        sent();
        assert!(pad::press(w));
        assert!(pad::step(w, pad::Way::Right));
        let messages = sent();
        let expected = (gamma.value + gamma.step).min(gamma.maximum);
        assert!(
            matches!(messages.as_slice(), [Message::GameConfig(crate::game_config::Msg::Gamma(g))] if (g - expected).abs() < 1e-4),
            "one Gamma({expected}) message"
        );
        assert_eq!(pad::focused(w).as_deref(), Some(gamma.label.as_str()), "the engaged slider keeps the focus");
        // Select again finishes: the D-pad moves the focus once more.
        assert!(pad::press(w));
        assert!(pad::step(w, pad::Way::Down));
        assert!(sent().is_empty());
        assert_ne!(pad::focused(w).as_deref(), Some(gamma.label.as_str()));
    }

    #[test]
    fn a_dropdown_opens_and_picks_from_the_controller() {
        let app = settings_app(Tab::System);
        let ui = window(&app);
        let w = ui.window();
        assert!(pad::focus_named(w, t("Language")));
        assert!(pad::press(w), "A opens the list");
        // The open list's entries are the only stops; the chosen language has the focus.
        let names = pad::names(w);
        assert_eq!(names.len(), Lang::ALL.len(), "{names:?}");
        assert_eq!(pad::focused(w), Some(Lang::ALL[0].to_string()));
        sent();
        assert!(pad::step(w, pad::Way::Down));
        assert!(pad::press(w), "A picks");
        let messages = sent();
        assert!(matches!(messages.as_slice(), [Message::SetLanguage(lang)] if *lang == Lang::ALL[1]), "{}", messages.len());
    }

    #[test]
    fn the_restore_backup_menu_is_a_dropdown_a_controller_can_drive() {
        let mut app = settings_app(Tab::Graphics);
        app.game_config = gcf::preview_state();
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        if !view.get_gc().has_backups {
            // The preview has no backups: the menu is not shown.
            assert!(!pad::names(ui.window()).iter().any(|n| n == t("Restore backup")));
            return;
        }
        let w = ui.window();
        assert!(pad::focus_named(w, t("Restore backup")));
        assert_eq!(pad::focused(w).as_deref(), Some(t("Restore backup")), "{:?}", pad::names(w));
        assert!(pad::press(w));
        sent();
        assert!(pad::press(w));
        assert!(matches!(sent().as_slice(), [Message::GameConfig(crate::game_config::Msg::LoadBackup(_))]));
    }

    #[test]
    fn an_open_dialog_keeps_the_controller_and_tab_inside_it() {
        let mut app = settings_app(Tab::System);
        let ui = window(&app);
        let w = ui.window();
        // Focus a page switch, then the close confirmation opens over the page.
        assert!(pad::focus_named(w, t("Navigate Crystal with a controller")));
        app.confirm_close = true;
        sync(&ui, &app);
        let names = pad::names(w);
        assert_eq!(names, vec![t("Keep launcher open").to_owned(), t("Close now").to_owned()], "only the dialog's controls");
        assert_eq!(pad::focused(w).as_deref(), Some(t("Keep launcher open")), "the safe choice takes the focus");
        // Neither the walker, nor Tab, nor A reaches the page under the dim.
        let mut visited = Vec::new();
        for way in [pad::Way::Up, pad::Way::Left, pad::Way::Down, pad::Way::Right, pad::Way::Up] {
            pad::step(w, way);
            visited.push(pad::focused(w).unwrap_or_default());
            pad::cycle(w, true);
            visited.push(pad::focused(w).unwrap_or_default());
        }
        assert!(visited.iter().all(|name| names.contains(name)), "{visited:?}");
        let tab: slint::SharedString = slint::platform::Key::Tab.into();
        for _ in 0..3 {
            w.dispatch_event(slint::platform::WindowEvent::KeyPressed { text: tab.clone() });
            w.dispatch_event(slint::platform::WindowEvent::KeyReleased { text: tab.clone() });
            assert!(names.contains(&pad::focused(w).unwrap_or_default()), "Tab stays in the dialog");
        }
        sent();
        assert!(pad::focus_named(w, t("Keep launcher open")));
        assert!(pad::press(w));
        assert!(matches!(sent().as_slice(), [Message::CancelClose]));
        assert!(!pad::focus_named(w, t("Navigate Crystal with a controller")), "the page's switches are not stops");
    }

    #[test]
    fn the_controller_tab_shows_the_pad_page_and_sends_its_messages() {
        let mut app = settings_app(Tab::Controller);
        let ui = window(&app);
        let view = ui.global::<SettingsView>();
        let state = view.get_pad();
        let front = gp::front(&app);
        assert_eq!(state.title, front.title.as_str());
        assert_eq!(state.caption, front.caption);
        assert_eq!(view.get_pad_left().row_count() + view.get_pad_right().row_count(), crate::gamepad::front::front(&app).bindings.len());
        assert_eq!(view.get_pad_left().row_count(), front.bindings.len().div_ceil(2));
        assert_eq!(view.get_pad_switches().row_count(), 4);
        assert!(view.get_pad_notes().iter().any(|note| note.text.starts_with(t("Stored in {source}").split('{').next().unwrap())));
        // A second sync with nothing changed keeps every row.
        let left = view.get_pad_left();
        let first = left.row_data(0).unwrap();
        sync(&ui, &app);
        assert!(view.get_pad_left().row_data(0).unwrap() == first);
        // The controls send their messages.
        sent();
        view.invoke_pad_press(first.id);
        view.invoke_pad_reset();
        view.invoke_pad_switch(0, !view.get_pad_switches().row_data(0).unwrap().on);
        view.invoke_pad_rumble();
        view.invoke_pad_retry();
        let messages = sent();
        use crate::gamepad::Msg as G;
        assert!(matches!(messages[0], Message::Gamepad(G::Capture(_))));
        assert!(matches!(messages[1], Message::Gamepad(G::Reset)));
        assert!(matches!(messages[2], Message::Gamepad(G::SetFlag(..))));
        assert!(matches!(messages[3], Message::Gamepad(G::Rumble)));
        assert!(matches!(messages[4], Message::Gamepad(G::Apply)));
        // Capturing: the row shows its prompt; pressing it again cancels; Unbind clears it.
        let _ = crate::gamepad::update(&mut app.gamepad, G::Capture(front.bindings[0].row), &app.config);
        sync(&ui, &app);
        let row = view.get_pad_left().row_data(0).unwrap();
        assert!(row.capturing && row.prompt != "");
        view.invoke_pad_press(row.id);
        view.invoke_pad_unbind(row.id);
        let messages = sent();
        assert!(matches!(messages[0], Message::Gamepad(G::CancelCapture)));
        assert!(matches!(messages[1], Message::Gamepad(G::Clear(_))));
        // Every row, switch and button is a controller stop.
        let names = pad::names(ui.window());
        for name in [t("Reset to default"), t("Use a controller in FFXI"), t("Keep working in the background"), row.label.as_str()] {
            assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
        }
    }

    /// The UI thread's cost of one controller change on the Controller page (update, sync,
    /// one software frame at 1440x900): `cargo test controller_change_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn controller_change_cost() {
        use crate::gamepad::{detect::{BatteryState, Family, Input, Pad}, Msg as G};
        use std::time::Instant;
        let mut app = settings_app(Tab::Controller);
        let window = super::super::sync::headless::window(1440, 900);
        let ui = MainWindow::new().unwrap();
        wire(&ui);
        super::super::view::wire(&ui);
        ui.show().unwrap();
        let pad = |input| Pad {
            id: 0, name: "DualSense Wireless Controller".into(), family: Family::DualSense, vid: Some(0x054C), pid: Some(0x0CE6),
            wireless: Some(true), battery: None, battery_source: None, battery_state: BatteryState::Unknown,
            battery_level: None, rumble: false, input,
        };
        let (mut update, mut synced, mut drawn) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..120 {
            let a = (i as f32 * 0.21).sin();
            let input = Input { buttons: if i % 10 < 5 { 1 } else { 0 }, lx: (a * 50.0).round() / 50.0, ly: 0.3, lt: (i % 7) as f32 / 7.0, ..Input::default() };
            let t = Instant::now();
            let _ = app.update(Message::Gamepad(G::Pads(vec![pad(input)])));
            update.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            super::super::view::sync(&ui, &app);
            sync(&ui, &app);
            synced.push(t.elapsed().as_secs_f64() * 1e3);
            let t = Instant::now();
            super::super::sync::headless::draw(&window);
            drawn.push(t.elapsed().as_secs_f64() * 1e3);
        }
        let stat = |v: &mut Vec<f64>| { let s = v[10..].to_vec(); let mut s = s; s.sort_by(f64::total_cmp); (s[s.len() / 2], s[s.len() * 95 / 100], s[s.len() - 1]) };
        println!("update p50/p95/max {:.2?} ms", stat(&mut update));
        println!("sync   p50/p95/max {:.2?} ms", stat(&mut synced));
        println!("draw   p50/p95/max {:.2?} ms", stat(&mut drawn));
    }
}
