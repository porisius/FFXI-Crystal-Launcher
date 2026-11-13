//! The Home page and title bar's view model: `wire` turns the window's callbacks into core
//! messages, `sync` pushes the core's state into its properties after each batch of messages.
//!
//! Everything shown comes from `App`; every action is a core message. `sync` sets a property only when its value differs, updates list models
//! row by row, and builds each news picture once, so a batch that changed nothing redraws
//! nothing.
use super::sync::{private, put, strings_model, sync_model};
use super::{send, AccountLine, Footer, Glyph, Ink, LampState, MainWindow, NewsCard, RailNote, SetupStep, Sheet, SheetStrings, Strings, View};
use crate::backend::AccountMode;
use crate::i18n::{t, tf, Lang};
use crate::account_row::RowState;
use crate::backend::LaunchWith;
use crate::services::NewsTab;
use crate::{App, EditorFlag, Field, MainAction, Message, Page};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::ffxiah::AH_SERVERS;
use crate::play_update::{maintenance_percent, remaining_time};
const NEWS_TABS: [NewsTab; 4] = [NewsTab::Latest, NewsTab::Updates, NewsTab::Events, NewsTab::Maintenance];
const PAGES: [Page; 4] = [Page::Home, Page::Friends, Page::Maintenance, Page::Settings];
/// The Character select choice for the accounts Play starts (the rail's More menu).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CharacterSelect {
    Automatic,
    AskEachTime,
}
impl std::fmt::Display for CharacterSelect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CharacterSelect::Automatic => t("Automatic"),
            CharacterSelect::AskEachTime => t("Ask me each time"),
        })
    }
}
const EDITOR_LAUNCH: [Option<LaunchWith>; 4] = [None, Some(LaunchWith::Windower), Some(LaunchWith::Ashita), Some(LaunchWith::Vanilla)];
const FIELDS: [Field; 8] = [Field::Label, Field::Server, Field::PolId, Field::PolPassword, Field::SeId, Field::SePassword, Field::PolHandleNames, Field::TotpKey];
/// A one-time code's digits.
const CODE_LEN: usize = 6;

/// What an OTP box keeps of its text: digits only, six at most.
pub(super) fn otp_digits(value: &str) -> String {
    value.chars().filter(char::is_ascii_digit).take(CODE_LEN).collect()
}

/// What the callbacks act on, as of the last sync, and the models the window draws from.
struct State {
    lang: Option<Lang>,
    privacy: bool,
    light: bool,
    row_keys: Vec<String>,
    row_actions: Vec<Option<Message>>,
    /// When each row's game started (its session clock), by row.
    row_started: Vec<Option<std::time::Instant>>,
    /// What each entry of the rail's More menu sends.
    menu: Vec<Message>,
    /// The cards' then the older headlines' links (open-news numbers them in that order).
    news_urls: Vec<String>,
    /// What each first-run check's action sends, in order.
    setup_actions: Vec<Vec<Message>>,
    editor_key: Option<String>,
    flags: crate::EditorFlags,
    tool: i32,
    tool_values: Vec<Option<String>>,
    accounts: Rc<VecModel<AccountLine>>,
    notes: Rc<VecModel<RailNote>>,
    rail_menu: Rc<VecModel<SharedString>>,
    news: Rc<VecModel<NewsCard>>,
    news_older: Rc<VecModel<NewsCard>>,
    setup: Rc<VecModel<SetupStep>>,
    news_tabs: Rc<VecModel<SharedString>>,
    launch_items: Rc<VecModel<SharedString>>,
    launch_choices: Rc<VecModel<SharedString>>,
    private_launch_choices: Rc<VecModel<SharedString>>,
    tool_items: Rc<VecModel<SharedString>>,
    slot_items: Rc<VecModel<SharedString>>,
    /// News pictures by `NewsImage::id`, pruned to the ones shown.
    images: HashMap<u64, Image>,
}

impl State {
    fn new() -> Self {
        Self {
            lang: None,
            privacy: false,
            light: false,
            row_keys: Vec::new(),
            row_actions: Vec::new(),
            row_started: Vec::new(),
            menu: Vec::new(),
            news_urls: Vec::new(),
            setup_actions: Vec::new(),
            editor_key: None,
            flags: Default::default(),
            tool: 0,
            tool_values: Vec::new(),
            accounts: Rc::new(VecModel::default()),
            notes: Rc::new(VecModel::default()),
            rail_menu: Rc::new(VecModel::default()),
            news: Rc::new(VecModel::default()),
            news_older: Rc::new(VecModel::default()),
            setup: Rc::new(VecModel::default()),
            news_tabs: Rc::new(VecModel::default()),
            launch_items: Rc::new(VecModel::default()),
            launch_choices: Rc::new(VecModel::default()),
            private_launch_choices: Rc::new(VecModel::default()),
            tool_items: Rc::new(VecModel::default()),
            slot_items: Rc::new(VecModel::default()),
            images: HashMap::new(),
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Reads the last sync's state (never while a message is being delivered).
fn read<R>(f: impl FnOnce(&State) -> Option<R>) -> Option<R> {
    STATE.with(|cell| cell.borrow().as_ref().and_then(f))
}

/// Sends the message `f` picks from the last sync's state, after releasing it.
fn act(f: impl FnOnce(&State) -> Option<Message>) {
    if let Some(message) = read(f) {
        send(message);
    }
}

fn index(i: i32) -> Option<usize> {
    usize::try_from(i).ok()
}

/// Connect the page's callbacks (the host wires the window chrome's own).
pub(super) fn wire(ui: &MainWindow) {
    // A new window: fresh models (the old window's went with it) and every value set again.
    let state = State::new();
    let view = ui.global::<View>();
    view.set_accounts(ModelRc::from(state.accounts.clone()));
    view.set_notes(ModelRc::from(state.notes.clone()));
    view.set_rail_menu(ModelRc::from(state.rail_menu.clone()));
    view.set_news(ModelRc::from(state.news.clone()));
    view.set_news_older(ModelRc::from(state.news_older.clone()));
    view.set_setup(ModelRc::from(state.setup.clone()));
    view.set_news_tabs(ModelRc::from(state.news_tabs.clone()));
    view.set_launch_items(ModelRc::from(state.launch_items.clone()));
    view.set_launch_choices(ModelRc::from(state.launch_choices.clone()));
    view.set_private_launch_choices(ModelRc::from(state.private_launch_choices.clone()));
    view.set_tool_items(ModelRc::from(state.tool_items.clone()));
    view.set_slot_items(ModelRc::from(state.slot_items.clone()));
    view.set_ah_servers(ModelRc::new(VecModel::from(AH_SERVERS.iter().map(|(s, _)| SharedString::from(*s)).collect::<Vec<_>>())));
    view.set_regions(ModelRc::new(VecModel::from(vec![SharedString::from("US"), "EU".into(), "JP".into()])));
    STATE.with(|cell| *cell.borrow_mut() = Some(state));

    // ---- title bar ----
    view.on_navigate(|i| { if let Some(page) = index(i).and_then(|i| PAGES.get(i)) { send(Message::Navigate(*page)); } });
    view.on_toggle_privacy(|| act(|s| Some(Message::SetPrivacy(!s.privacy))));
    view.on_toggle_theme(|| act(|s| Some(Message::SetLightTheme(!s.light))));
    view.on_open_discord(|| send(Message::OpenUrl(crate::DISCORD_INVITE.into())));
    view.on_banner_clicked(|| send(Message::OpenSettingsTab(crate::settings_page::Tab::System)));

    // ---- news ----
    view.on_news_picked(|i| { if let Some(tab) = index(i).and_then(|i| NEWS_TABS.get(i)) { send(Message::NewsFilter(*tab)); } });
    view.on_refresh_news(|| send(Message::Refresh));
    view.on_view_all_news(|| send(Message::OpenUrl(crate::services::sources(crate::i18n::current()).home.into())));
    view.on_open_news(|i| act(|s| s.news_urls.get(index(i)?).map(|url| Message::OpenUrl(url.clone()))));
    view.on_setup_action(|i| {
        for message in read(|s| s.setup_actions.get(index(i)?).cloned()).unwrap_or_default() {
            send(message);
        }
    });

    // ---- accounts ----
    view.on_toggle_account(|i, on| act(|s| s.row_keys.get(index(i)?).map(|key| Message::ToggleAccount(key.clone(), on))));
    view.on_edit_account(|i| act(|s| s.row_keys.get(index(i)?).map(|key| Message::EditAccount(key.clone()))));
    view.on_otp_edited(|i, value| {
        // The boxes take digits only, six at most; the sixth signs in (the row's Sign in, when
        // the core offers it for that code).
        let code = otp_digits(value.as_str());
        let full = code.len() == CODE_LEN;
        act(|s| s.row_keys.get(index(i)?).map(|key| Message::OtpChanged(key.clone(), code)));
        if full {
            act(|s| s.row_actions.get(index(i)?).cloned().flatten().filter(|message| matches!(message, Message::SignIn(_))));
        }
    });
    view.on_row_action(|i| act(|s| s.row_actions.get(index(i)?).cloned().flatten()));
    view.on_rail_menu_picked(|i| act(|s| s.menu.get(index(i)?).cloned()));
    view.on_import_accounts(|| send(Message::AccountImport(crate::account_import_ui::Msg::Open)));
    view.on_add_account(|| send(Message::NewAccount));
    view.on_drag_start(|i| act(|s| s.row_keys.get(index(i)?).map(|key| Message::AccountDragStart(key.clone()))));
    view.on_drag_over(|i| act(|s| s.row_keys.get(index(i)?).map(|key| Message::AccountDragOver(key.clone()))));

    // ---- look up ----
    view.on_wiki_edited(|value| send(Message::WikiQueryChanged(value.into())));
    view.on_wiki_search(|| send(Message::SearchWiki));
    view.on_ah_edited(|value| send(Message::AhQueryChanged(value.into())));
    view.on_ah_search(|| send(Message::SearchAh));
    view.on_ah_server_picked(|i| { if let Some(server) = index(i).and_then(|i| AH_SERVERS.get(i)) { send(Message::AhServerChanged(server.0)); } });
    view.on_open_wiki(|| send(Message::OpenUrl(wiki_site().1.into())));
    view.on_open_ffxiah(|| send(Message::OpenUrl("https://www.ffxiah.com/".into())));

    // ---- footer ----
    view.on_toggle_details(|| send(Message::ToggleDetails));
    view.on_launch_picked(|i| { if let Some(tool) = index(i).and_then(|i| LaunchWith::ALL.get(i)) { send(Message::SetLaunchWith(*tool)); } });
    view.on_shut_down_selected(|| send(Message::ShutdownSelected));
    view.on_main_action(|| send(Message::MainAction));

    // ---- account sheet ----
    view.on_close_sheet(|| send(Message::CloseEditor));
    view.on_save_sheet(|| send(Message::SaveAccount));
    view.on_sheet_edited(|i, value| { if let Some(field) = index(i).and_then(|i| FIELDS.get(i)) { send(Message::EditorChanged(*field, value.into())); } });
    view.on_sheet_folder_edited(|value| send(Message::AccountInstallChanged(value.into())));
    view.on_sheet_region(|i| { if (0..3).contains(&i) { send(Message::EditorRegion(i as u8)); } });
    view.on_sheet_launch(|i| { if let Some(value) = index(i).and_then(|i| EDITOR_LAUNCH.get(i)) { send(Message::EditorLaunchWith(*value)); } });
    view.on_sheet_flag(|i, on| act(|s| Some(match i {
        0 => Message::EditorFlag(EditorFlag::UsesOtp, on),
        1 => Message::EditorFlag(EditorFlag::SignInNow, on),
        2 => Message::EditorFlag(EditorFlag::ShowId, !s.flags.show_id),
        3 => Message::EditorFlag(EditorFlag::ShowPassword, !s.flags.show_password),
        4 => Message::EditorFlag(EditorFlag::ChangePassword, true),
        5 => Message::EditorFlag(EditorFlag::ShowTotpKey, !s.flags.show_totp_key),
        6 => Message::EditorFlag(EditorFlag::ChangeTotpKey, true),
        7 => Message::EditorFlag(EditorFlag::NoHairpin, on),
        _ => return None,
    })));
    view.on_sheet_auto_login(|on| send(Message::EditorAutoLogin(on)));
    view.on_sheet_slot(|i| { if (0..16).contains(&i) { send(Message::EditorCharacterSlot(i as u8 + 1)); } });
    view.on_sheet_tool(|i| act(|s| {
        let value = s.tool_values.get(index(i)?)?.clone();
        match s.tool {
            1 => Some(Message::EditorBootIni(value)),
            2 => Some(Message::EditorProfile(value)),
            _ => None,
        }
    }));
    view.on_edit_profiles(|| send(Message::EditWindowerProfiles));
    view.on_browse_folder(|| send(Message::Browse(crate::PickTarget::AccountGameFolder)));
    view.on_sign_out(|| act(|s| s.editor_key.clone().map(Message::AccountSignOut)));
    view.on_view_log(|| act(|s| s.editor_key.clone().map(Message::OpenLog)));
    view.on_open_logs(|| act(|s| s.editor_key.clone().map(Message::OpenLogsFolder)));
    view.on_delete_account(|| act(|s| s.editor_key.clone().map(Message::RequestDeleteAccount)));
    view.on_choose_portrait(|| act(|s| s.editor_key.clone().map(|key| Message::PortraitPicker(Some(key)))));
}

/// The window closed: its models and news pictures go with it (nothing stays in the background).
pub(super) fn forget() {
    STATE.with(|cell| cell.borrow_mut().take());
}


/// Push `app`'s state into the window. Called after every batch of messages: only what
/// changed is set, so an unchanged state redraws nothing.
pub(super) fn sync(ui: &MainWindow, app: &App) {
    let view = ui.global::<View>();
    let sky_needed = STATE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let state = slot.get_or_insert_with(State::new);
        state.privacy = app.settings.privacy;
        state.light = app.settings.light_theme;
        let lang = crate::i18n::current();
        if state.lang != Some(lang) {
            state.lang = Some(lang);
            put!(view, get_strings, set_strings, strings());
            put!(view, get_sheet_strings, set_sheet_strings, sheet_strings());
            put!(view, get_mixed, set_mixed, t("Mixed").into());
            strings_model(&state.news_tabs, NEWS_TABS.iter().map(|tab| tab_label(*tab).to_owned()));
            strings_model(&state.launch_items, LaunchWith::ALL.iter().map(ToString::to_string));
            strings_model(&state.launch_choices, [t("Use default"), "Windower", "Ashita", t("Vanilla")].map(str::to_owned));
            // EDITOR_LAUNCH's first three: no choice is xiloader on its own.
            strings_model(&state.private_launch_choices, ["xiloader", "Windower", "Ashita"].map(str::to_owned));
            strings_model(&state.slot_items, (1u8..=16).map(|number| tf("Slot {number}", &[("number", &number)])));
            put!(view, get_serif_greeting, set_serif_greeting, lang != Lang::Ja);
            // The sky's day and moon in the new language.
            super::sky::update(ui);
        }
        shell(&view, app);
        news(&view, app, state);
        setup(&view, app, state);
        accounts(&view, app, state);
        look_up(&view, app);
        footer(&view, app, state);
        sheet(&view, app, state);
        (app.page == Page::Home && !app.features_view.open) || state.row_started.iter().any(Option::is_some)
    });
    // Outside the state borrow: waking the sky catches the session clocks up.
    super::sky::needed(ui, sky_needed);
}

/// A session clock: "3h 12m", or "12m" in the first hour.
fn session_clock(elapsed: std::time::Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match minutes / 60 {
        0 => tf("{minutes}m", &[("minutes", &minutes)]),
        hours => tf("{hours}h {minutes}m", &[("hours", &hours), ("minutes", &(minutes % 60))]),
    }
}

/// Brings the rows' session clocks up to the minute (the sky's timer calls this: a quiet window
/// sends no ticks while games run).
pub(super) fn tick_clocks() {
    STATE.with(|cell| {
        let slot = cell.borrow();
        let Some(state) = slot.as_ref() else { return };
        for (i, started) in state.row_started.iter().enumerate() {
            let Some(started) = started else { continue };
            let Some(mut row) = slint::Model::row_data(&*state.accounts, i) else { continue };
            let clock = SharedString::from(session_clock(started.elapsed()));
            if row.clock != clock {
                row.clock = clock;
                slint::Model::set_row_data(&*state.accounts, i, row);
            }
        }
    });
}

fn strings() -> Strings {
    let (wiki_title, _, wiki_placeholder) = wiki_site();
    Strings {
        home: t("Home").into(),
        friends: t("Friends").into(),
        downloads: t("Downloads").into(),
        settings: t("Settings").into(),
        privacy: t("Privacy mode").into(),
        privacy_on_tip: t("Privacy mode is on. Click to turn it off.").into(),
        privacy_off_tip: t("Blur names, IDs and messages").into(),
        light_theme: t("Light theme").into(),
        dark_theme: t("Dark theme").into(),
        discord: t("Join our Discord").into(),
        greeting: t("Welcome back to Vana’diel.").into(),
        greeting_first: t("Welcome to Vana’diel.").into(),
        news: t("PlayOnline news").into(),
        refresh_news: t("Refresh news").into(),
        view_all_news: t("View all news").into(),
        all_news: t("All news").into(),
        before_you_play: t("Before you play").into(),
        accounts: t("Accounts").into(),
        add_account: t("Add account").into(),
        no_accounts: t("No accounts yet").into(),
        no_accounts_body: t("Add the PlayOnline account you play on.\nIts password stays encrypted on this PC.").into(),
        import_from_pol: t("Import from PlayOnline").into(),
        features: t("Features").into(),
        more: t("More").into(),
        nothing_picked: t("Nothing picked").into(),
        one_time_code: t("One-time code").into(),
        enter: t("Enter").into(),
        code: t("Code").into(),
        sign_in: t("Sign in").into(),
        look_up: t("Look up").into(),
        open_website: t("Open the website").into(),
        wiki_title: wiki_title.into(),
        wiki_placeholder: wiki_placeholder.into(),
        find_item: t("Find an item").into(),
        launch_with: t("Launch with").into(),
    }
}

fn sheet_strings() -> SheetStrings {
    SheetStrings {
        account: t("Account").to_uppercase().into(),
        launch: t("Launch").to_uppercase().into(),
        status: t("Status").to_uppercase().into(),
        after_adding: t("After adding").to_uppercase().into(),
        display_name: t("Display name").into(),
        display_placeholder: t("How it appears in Crystal, e.g. Mule").into(),
        private_note: "Private server: logs in through xiloader. When the server publishes a launcher manifest, Crystal uses its xiloader, ports and key.".into(),
        hairpin: "Use hairpin (--hairpin)".into(),
        hairpin_note: "The game connects to the server name entered above instead of the address the server reports (often its public one), which is what a server on your own network needs. Turn it off for a server whose zones run on other hosts than its login.".into(),
        server: t("Server").into(),
        server_placeholder: t("Server hostname").into(),
        show: t("Show").into(),
        hide: t("Hide").into(),
        change: t("Change").into(),
        se_id: t("Square Enix ID").into(),
        se_id_placeholder: t("Square Enix account ID").into(),
        se_password: t("Square Enix password").into(),
        password_placeholder: t("Password").into(),
        pol_password_placeholder: t("PlayOnline password").into(),
        region: t("Region").into(),
        region_note: t("EU and JP sign-in is experimental.").into(),
        uses_otp: t("Uses a one-time password (security token)").into(),
        totp_key: t("Authenticator key").into(),
        totp_key_placeholder: t("Optional").into(),
        totp_key_note: t("From Square Enix's authenticator setup. Codes are filled in for you.").into(),
        handles: t("PlayOnline handle setup (optional)").into(),
        handles_placeholder: t("Public handle name").into(),
        handles_note: t("Registers your characters on first login. Up to eight per handle, separated by commas.").into(),
        saved_note: t("Saved encrypted on this PC. Sent only to PlayOnline.").into(),
        launch_with: t("Launch with").into(),
        auto_login: t("Log in to a character automatically").into(),
        slot: t("Character slot").into(),
        slot_note: t("Unavailable slots stay at character selection.").into(),
        folder: t("Game folder for this account").into(),
        folder_placeholder: t("Use the launcher's default game folder").into(),
        folder_note: t("Leave blank to use the default.").into(),
        active_note: t("Changes apply on the next launch.").into(),
        last_play: t("Last Play:").into(),
        view_log: t("View log").into(),
        open_logs: t("Open logs folder").into(),
        sign_out: t("Sign out").into(),
        sign_in_now: t("Sign in now").into(),
        delete: t("Delete account").into(),
        cancel: t("Cancel").into(),
        edit_profiles: t("Edit profiles in Windower").into(),
        close: t("Close").into(),
        portrait: t("Portrait").into(),
    }
}

/// The wiki for the launcher's language: its name, home page and search placeholder.
fn wiki_site() -> (&'static str, &'static str, &'static str) {
    match crate::i18n::current() {
        Lang::En => ("BG Wiki", "https://www.bg-wiki.com/", t("Search BG Wiki")),
        Lang::Ja => ("FF11用語辞典", "https://wiki.ffo.jp/", t("Search FF11 Dictionary")),
    }
}

/// The top bar and the Windows components banner.
fn shell(view: &View, app: &App) {
    let page = PAGES.iter().position(|page| *page == app.page).unwrap_or(0) as i32;
    put!(view, get_page, set_page, page);
    put!(view, get_privacy, set_privacy, app.settings.privacy);
    put!(view, get_look_up, set_look_up, app.settings.item_search);
    // The Windows components banner.
    let banner = crate::prereqs::banner_text(&app.prereqs);
    put!(view, get_banner, set_banner, banner.is_some());
    if let Some(banner) = banner {
        put!(view, get_banner_lead, set_banner_lead, banner.lead.into());
        put!(view, get_banner_rest, set_banner_rest, banner.rest.into());
        put!(view, get_banner_action, set_banner_action, banner.button.into());
        put!(view, get_banner_restart, set_banner_restart, banner.restart);
    }
}

fn tab_label(tab: NewsTab) -> &'static str {
    match tab {
        NewsTab::Latest => t("Latest"),
        NewsTab::Updates => t("Updates"),
        NewsTab::Events => t("Events"),
        NewsTab::Maintenance => t("Maintenance"),
    }
}

/// The feeds carry no category field, so the tabs go by title words.
fn news_matches(item: &crate::services::NewsItem, tab: NewsTab) -> bool {
    let title = item.title.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| title.contains(word));
    match tab {
        NewsTab::Updates => any(&["update", "trial", "バージョンアップ", "アップデート", "更新", "トライアル"]),
        NewsTab::Events => any(&["event", "campaign", "happenings", "bonanza", "イベント", "キャンペーン", "異変", "ボナンザ"]),
        NewsTab::Latest | NewsTab::Maintenance => true,
    }
}

/// A post's category, by its title words (the feeds carry none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Update,
    Event,
    Maintenance,
    Community,
    /// Anything the words don't place.
    PlayOnline,
}

impl Category {
    fn of(item: &crate::services::NewsItem) -> Self {
        let title = item.title.to_ascii_lowercase();
        let any = |words: &[&str]| words.iter().any(|word| title.contains(word));
        if any(&["maintenance", "outage", "メンテナンス", "障害"]) {
            Category::Maintenance
        } else if news_matches(item, NewsTab::Updates) {
            Category::Update
        } else if any(&["bonanza", "community", "contest", "ボナンザ", "コミュニティ", "コンテスト"]) {
            Category::Community
        } else if news_matches(item, NewsTab::Events) {
            Category::Event
        } else {
            Category::PlayOnline
        }
    }

    /// Its word, the colour it is written in, and its default picture (NewsArt's order).
    fn look(self) -> (&'static str, Ink, i32) {
        match self {
            Category::Update => (t("Update"), Ink::Ok, 0),
            Category::Event => (t("Event"), Ink::Info, 1),
            Category::Maintenance => (t("Maintenance"), Ink::Danger, 2),
            Category::Community => (t("Community"), Ink::Warn, 3),
            Category::PlayOnline => (t("PlayOnline"), Ink::Sub, 4),
        }
    }
}

/// The month and day in any date text the official sources use.
fn news_month_day(raw: &str) -> Option<(u32, u32)> {
    use chrono::Datelike;
    let normalized = raw.replace(" JST", " +0900");
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(&normalized).or_else(|_| chrono::DateTime::parse_from_rfc3339(raw)) {
        return Some((date.month(), date.day()));
    }
    let prefix = raw.split(" [").next().unwrap_or(raw);
    if let Ok(date) = chrono::NaiveDateTime::parse_from_str(prefix, "%b. %d, %Y %H:%M") {
        return Some((date.month(), date.day()));
    }
    let ymd = raw.trim().split(['(', '（', ' ']).next()?;
    let date = chrono::NaiveDate::parse_from_str(ymd, "%Y.%m.%d").ok()?;
    Some((date.month(), date.day()))
}

/// "Sep 26" / "9月26日", or the text as it came.
fn news_date(raw: &str) -> String {
    let Some((month, day)) = news_month_day(raw) else { return raw.to_owned() };
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match crate::i18n::current() {
        Lang::Ja => format!("{month}月{day}日"),
        Lang::En => format!("{} {day}", MONTHS[month as usize - 1]),
    }
}

// ---- headline wrapping ----
// The card's two-line teaser is wrapped here rather than by the text engine:
// Slint's wrap-and-elide misplaces lines of Japanese set in a fallback face.

/// Approximate advance of `ch` in ems, slightly generous so a line judged to fit does.
fn glyph_em(ch: char) -> f32 {
    match ch {
        ' ' => 0.28,
        'i' | 'j' | 'l' | 'I' | '.' | ',' | ':' | ';' | '\'' | '’' | '!' | '|' => 0.26,
        'f' | 't' | 'r' | '(' | ')' | '[' | ']' | '"' | '“' | '”' | '/' => 0.36,
        '-' => 0.40,
        's' | 'c' | 'z' | 'J' => 0.47,
        'm' | 'M' | 'W' | '—' => 0.9,
        'w' | '&' | '%' | '@' => 0.76,
        'a'..='z' => 0.56,
        'A'..='Z' => 0.66,
        '0'..='9' => 0.56,
        '\u{1100}'..='\u{115f}'
        | '\u{2e80}'..='\u{a4cf}'
        | '\u{ac00}'..='\u{d7a3}'
        | '\u{f900}'..='\u{faff}'
        | '\u{fe30}'..='\u{fe6f}'
        | '\u{ff00}'..='\u{ff60}'
        | '\u{ffe0}'..='\u{ffe6}' => 1.0,
        _ => 0.62,
    }
}

fn text_em(line: &str) -> f32 {
    line.chars().map(glyph_em).sum()
}

/// Word-wraps `text` into at most `max_lines` lines of `width` px at `size` px; text beyond
/// the last line ends in an ellipsis. Words too long for a line break between characters.
fn wrap_lines(text: &str, width: f32, size: f32, max_lines: usize) -> Vec<String> {
    let max_em = (width / size.max(1.0)).max(1.0);
    let mut words: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        if text_em(word) <= max_em {
            words.push(word.to_owned());
            continue;
        }
        let mut piece = String::new();
        for ch in word.chars() {
            if !piece.is_empty() && text_em(&piece) + glyph_em(ch) > max_em {
                words.push(std::mem::take(&mut piece));
            }
            piece.push(ch);
        }
        if !piece.is_empty() {
            words.push(piece);
        }
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in &words {
        if current.is_empty() {
            current.push_str(word);
        } else if text_em(&current) + glyph_em(' ') + text_em(word) <= max_em {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::replace(&mut current, word.clone()));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if max_lines > 0 && lines.len() > max_lines {
        lines.truncate(max_lines);
        let last = lines.last_mut().expect("max_lines > 0");
        while !last.is_empty() && text_em(last) + glyph_em('…') > max_em {
            match last.rfind(' ') {
                Some(cut) => last.truncate(cut),
                None => {
                    last.pop();
                }
            }
        }
        let trimmed = last.trim_end_matches(|ch: char| ch.is_whitespace() || ",;:-–—".contains(ch)).len();
        last.truncate(trimmed);
        last.push('…');
    }
    lines
}

/// A news picture, built once per image id.
fn picture(images: &mut HashMap<u64, Image>, source: &crate::services::NewsImage) -> Option<Image> {
    let expected = source.width as usize * source.height as usize * 4;
    if source.width == 0 || source.height == 0 || source.pixels.len() < expected {
        return None;
    }
    Some(
        images
            .entry(source.id)
            .or_insert_with(|| {
                let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&source.pixels[..expected], source.width, source.height);
                Image::from_rgba8(buffer)
            })
            .clone(),
    )
}

/// Newest first by the date text's month and day (a month after this one is last year's);
/// undated items keep their place after the dated ones.
fn by_date(items: &mut [(&crate::services::NewsItem, Category)], month: u32) {
    let key = |item: &crate::services::NewsItem| news_month_day(&item.date).map(|(m, d)| (m <= month, m, d));
    items.sort_by(|a, b| key(b.0).cmp(&key(a.0)));
}

fn news(view: &View, app: &App, state: &mut State) {
    // Three cards beside the rail (two in the small window), as wide as the main column allows
    // (theme.slint's rail-w and page-x, the cards' 20 px gap), and up to four older headlines
    // (two small). First run shows the cards alone, without teasers.
    let small = super::small(app);
    let (rail, margin, cols, lines) = if small { (300.0, 32.0, 2, 2) } else { (356.0, 40.0, 3, 4) };
    let width = (app.viewport.width - rail - 2.0 * margin).max(160.0);
    let card_w = ((width - 20.0 * (cols - 1) as f32) / cols as f32).floor();
    let first_run = first_run(app);
    let lines = if first_run { 0 } else { lines };
    put!(view, get_news_tab, set_news_tab, NEWS_TABS.iter().position(|tab| *tab == app.news_filter).unwrap_or(0) as i32);

    let month = {
        use chrono::Datelike;
        chrono::Local::now().month()
    };
    let this_month = app
        .services
        .iter()
        .flat_map(|snapshot| snapshot.news.iter().chain(&snapshot.notices))
        .filter(|item| news_month_day(&item.date).is_some_and(|(m, _)| m == month))
        .count();
    let note = match this_month {
        0 => String::new(),
        1 => t("1 notice this month").to_owned(),
        _ => tf("{count} notices this month", &[("count", &this_month)]),
    };
    put!(view, get_news_note, set_news_note, note.into());

    // The tab's posts in feed order: the first ones are the cards, the rest the older lines.
    // Latest's older lines take the maintenance notices in too, newest first.
    let mut picked: Vec<(&crate::services::NewsItem, Category)> = Vec::new();
    let mut older: Vec<(&crate::services::NewsItem, Category)> = Vec::new();
    if let Some(snapshot) = &app.services {
        let posts: Vec<_> = if app.news_filter == NewsTab::Maintenance {
            snapshot.notices.iter().map(|item| (item, Category::Maintenance)).collect()
        } else {
            snapshot.news.iter().filter(|item| news_matches(item, app.news_filter)).map(|item| (item, Category::of(item))).collect()
        };
        let split = posts.len().min(cols);
        picked.extend_from_slice(&posts[..split]);
        older.extend_from_slice(&posts[split..]);
        if app.news_filter == NewsTab::Latest {
            older.extend(snapshot.notices.iter().map(|item| (item, Category::Maintenance)));
            by_date(&mut older, month);
        }
        older.truncate(lines);
    }
    let shown: Vec<u64> = picked.iter().filter_map(|(item, _)| item.image.as_ref().map(|image| image.id)).collect();
    state.images.retain(|id, _| shown.contains(id));
    state.news_urls.clear();
    let mut cards = Vec::with_capacity(picked.len());
    for (item, kind) in &picked {
        let image = item.image.as_ref().and_then(|source| picture(&mut state.images, source));
        let (label, ink, art) = kind.look();
        cards.push(NewsCard {
            // Elided here to the card's width (16 px semibold): the renderer's own elision
            // overruns the card on CJK headlines.
            title: wrap_lines(&item.title, card_w, 16.5, 1).into_iter().next().unwrap_or_default().into(),
            headline: item.title.as_str().into(),
            teaser: if first_run { String::new() } else { wrap_lines(&item.summary, card_w, 13.0, 2).join("\n") }.into(),
            kind: label.into(),
            kind_ink: ink,
            date: news_date(&item.date).into(),
            has_image: image.is_some(),
            image: image.unwrap_or_default(),
            notice: *kind == Category::Maintenance,
            art,
        });
        state.news_urls.push(item.url.clone());
    }
    let mut rows = Vec::with_capacity(older.len());
    for (item, kind) in &older {
        let (label, ink, art) = kind.look();
        rows.push(NewsCard {
            title: item.title.as_str().into(),
            kind: label.into(),
            kind_ink: ink,
            date: news_date(&item.date).into(),
            notice: *kind == Category::Maintenance,
            art,
            ..Default::default()
        });
        state.news_urls.push(item.url.clone());
    }
    let (news_state, title, body) = if !picked.is_empty() {
        (0, "", "")
    } else if app.news_loading {
        (1, t("Loading PlayOnline news…"), "")
    } else if !app.news_error.is_empty() {
        (2, t("PlayOnline news is unavailable"), app.news_error.as_str())
    } else {
        (3, t("No articles in this category."), t("Try another tab."))
    };
    put!(view, get_news_state, set_news_state, news_state);
    put!(view, get_news_empty_title, set_news_empty_title, title.into());
    put!(view, get_news_empty_body, set_news_empty_body, body.into());
    sync_model(&state.news, cards);
    sync_model(&state.news_older, rows);
}

/// No account is added yet (and the vault has been read, so this is not just startup).
fn first_run(app: &App) -> bool {
    app.vault_ready && app.accounts.is_empty()
}

/// Home's first-run checks: the account, the game files and the Windows components, each with
/// its one action. "Add account" stays the rail's alone; the account check offers the import.
fn setup(view: &View, app: &App, state: &mut State) {
    let first_run = first_run(app);
    put!(view, get_first_run, set_first_run, first_run);
    let vault_error = app.vault_error.as_deref().filter(|_| !app.vault_ready).map(|error| private(app, error)).unwrap_or_default();
    put!(view, get_vault_error, set_vault_error, vault_error.into());
    if !first_run || app.page != Page::Home {
        state.setup_actions.clear();
        sync_model(&state.setup, Vec::new());
        return;
    }
    let mut steps = Vec::with_capacity(3);
    let mut actions = Vec::with_capacity(3);

    steps.push(SetupStep {
        glyph: Glyph::User,
        ink: Ink::Info,
        title: t("Account").into(),
        body: t("None added yet.").into(),
        detail: "".into(),
        detail_mono: false,
        action: t("Import from PlayOnline").into(),
        action_glyph: Glyph::Download,
        action_enabled: app.vault_ready && !app.vault_saving,
    });
    actions.push(vec![Message::AccountImport(crate::account_import_ui::Msg::Open)]);

    // The game: its version state and folder, and Check files (on Downloads, which shows it run).
    let no_folder = app.settings.install_path.trim().is_empty();
    let (glyph, ink, body) = match app.patch.as_ref().map(|p| p.state) {
        _ if no_folder => (Glyph::AlertCircle, Ink::Warn, t("Not selected")),
        Some(crate::patch::PatchState::Current) => (Glyph::CheckCircle, Ink::Ok, t("Up to date")),
        Some(crate::patch::PatchState::Required) => (Glyph::Download, Ink::Warn, t("Update available")),
        Some(crate::patch::PatchState::Interrupted) => (Glyph::AlertCircle, Ink::Warn, t("Interrupted update")),
        _ if app.patch_loading => (Glyph::Refresh, Ink::Sub, t("Checking version…")),
        _ => (Glyph::AlertCircle, Ink::Sub, t("Not checked")),
    };
    let can_check = app.maintenance.is_none() && !app.installation_loading && app.settings.install_selected;
    let (action, action_glyph) = if can_check { (t("Check files"), Glyph::FileCheck) } else { (t("Open Downloads"), Glyph::Download) };
    steps.push(SetupStep {
        glyph,
        ink,
        title: t("FINAL FANTASY XI").into(),
        body: body.into(),
        detail: if no_folder { String::new() } else { crate::privacy::mask_path(&app.settings.install_path) }.into(),
        detail_mono: true,
        action: action.into(),
        action_glyph,
        action_enabled: true,
    });
    let mut open = vec![Message::Navigate(Page::Maintenance)];
    if can_check { open.push(Message::StartMaintenance(crate::MaintenanceKind::Check)); }
    actions.push(open);

    // The Windows components: what is missing or pending, how many are ready, and the fix on
    // Settings → System (the rail's note and the banner open the same place).
    let ready = crate::prereqs::ready_count(&app.prereqs);
    let banner = crate::prereqs::banner_text(&app.prereqs);
    let (glyph, ink, body, action) = match (&ready, &banner) {
        (None, _) => (Glyph::Refresh, Ink::Sub, t("Checking…").to_owned(), t("View")),
        (_, Some(b)) if b.restart => (Glyph::Refresh, Ink::Warn, format!("{}{}", b.lead, b.rest), b.button),
        (_, Some(b)) => (Glyph::AlertCircle, Ink::Warn, tf("Missing: {names}", &[("names", &b.lead)]), b.button),
        (_, None) => (Glyph::CheckCircle, Ink::Ok, t("All installed.").to_owned(), t("View")),
    };
    steps.push(SetupStep {
        glyph,
        ink,
        title: t("Windows components").into(),
        body: body.into(),
        detail: ready.map(|(ready, total)| tf("{ready} of {total} ready", &[("ready", &ready), ("total", &total)])).unwrap_or_default().into(),
        detail_mono: crate::i18n::current() == Lang::En,
        action: action.into(),
        action_glyph: Glyph::Wrench,
        action_enabled: true,
    });
    actions.push(vec![Message::OpenSettingsTab(crate::settings_page::Tab::System)]);

    state.setup_actions = actions;
    sync_model(&state.setup, steps);
}
fn row_ink(state: RowState) -> Ink {
    match state {
        RowState::Closing | RowState::Waiting | RowState::Updating | RowState::ViewerConfirm | RowState::ViewerUpdating
            | RowState::Starting | RowState::SigningIn => Ink::Warn,
        RowState::Playing => Ink::Info,
        RowState::SignedIn => Ink::Ok,
        RowState::Private | RowState::Idle => Ink::Sub,
        RowState::NeedsCode | RowState::Problem => Ink::Danger,
    }
}

/// The crystal lamp a row's state lights: grey signed out, rising while it signs in, starts or
/// updates, blue when ready, bright in game, amber when it needs you, coral on an error.
fn row_lamp(state: RowState) -> LampState {
    match state {
        RowState::Playing => LampState::Game,
        RowState::SignedIn => LampState::Ready,
        RowState::Starting | RowState::SigningIn | RowState::Updating | RowState::ViewerUpdating => LampState::Rising,
        RowState::NeedsCode | RowState::ViewerConfirm => LampState::Code,
        RowState::Problem => LampState::Error,
        RowState::Closing | RowState::Waiting | RowState::Private | RowState::Idle => LampState::Off,
    }
}

/// The account's own PlayOnline portrait, once its session reported one (the silhouette in
/// privacy mode); `None` shows no picture.
fn own_picture(app: &App, key: &str) -> Option<slint::Image> {
    app.own_portraits.get(key).and_then(|id| crate::portraits::image(Some(*id)))
}

fn accounts(view: &View, app: &App, state: &mut State) {
    let total = app.accounts.len();
    let ticked = app.accounts.iter().filter(|row| row.selected).count();
    put!(view, get_accounts_note, set_accounts_note, tf("{ticked} of {total} picked", &[("ticked", &ticked), ("total", &total)]).into());
    let can_add = app.vault_ready && !app.vault_saving;
    put!(view, get_can_add, set_can_add, can_add);
    let editing = app.editor.as_ref().and_then(|editor| editor.key.as_deref());
    let privacy = crate::privacy::on();
    state.row_keys.clear();
    state.row_actions.clear();
    state.row_started.clear();
    let mut lines = Vec::with_capacity(total);
    for item in &app.accounts {
        let row = crate::account_row::row_view(app, item);
        let row_state = crate::account_row::row_state(app, item);
        let working = item.busy || item.shutting_down || item.signing_out || item.game_start_pending();
        let (action, action_soft, action_enabled, message) = match row.action {
            Some((label, message, soft)) => (label, soft, true, Some(message)),
            None if row.otp => (t("Sign in").to_owned(), true, false, None),
            None => (String::new(), false, false, None),
        };
        let started = (row_state == RowState::Playing).then(|| item.play_timing.as_ref().map(|timing| timing.started())).flatten();
        // The lamp rises with the row's own progress where it has one; a sign-in shows halfway,
        // a start (signed in, the game opening) most of the way.
        let fill = row.progress.unwrap_or(if row_state == RowState::Starting { 0.75 } else { 0.5 });
        let code_len = item.otp.chars().count().min(CODE_LEN);
        let picture = own_picture(app, &item.key);
        lines.push(AccountLine {
            has_picture: picture.is_some(),
            picture: picture.unwrap_or_default(),
            name: crate::privacy::mask(&item.account.label).into(),
            checked: item.selected,
            editing: editing == Some(item.key.as_str()),
            name_enabled: !app.vault_saving,
            ink: row_ink(row_state),
            label: private(app, &row.label).into(),
            detail: private(app, &row.detail).into(),
            progress: row.progress.unwrap_or(-1.0),
            percent: row.progress.map(|fraction| format!("{:.0}%", fraction * 100.0)).unwrap_or_default().into(),
            working: working && row.progress.is_none(),
            otp: row.otp,
            otp_text: item.otp.as_str().into(),
            otp_shown: if privacy { "•".repeat(code_len) } else { item.otp.chars().take(CODE_LEN).collect() }.into(),
            otp_len: code_len as i32,
            action: action.into(),
            action_soft,
            action_enabled,
            dragging: app.account_drag.source.as_deref() == Some(item.key.as_str()),
            lamp: row_lamp(row_state),
            fill: fill.clamp(0.0, 1.0),
            clock: started.map(|at| session_clock(at.elapsed())).unwrap_or_default().into(),
        });
        state.row_keys.push(item.key.clone());
        state.row_actions.push(message);
        state.row_started.push(started);
    }
    sync_model(&state.accounts, lines);
    put!(view, get_dragging, set_dragging, app.account_drag.source.is_some());
}

fn look_up(view: &View, app: &App) {
    put!(view, get_wiki_query, set_wiki_query, app.wiki_query.as_str().into());
    put!(view, get_ah_query, set_ah_query, app.ah_query.as_str().into());
    put!(view, get_ah_server, set_ah_server, AH_SERVERS.iter().position(|(server, _)| *server == app.ah_server).map_or(-1, |i| i as i32));
}

// ---- the footer play bar ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayState {
    Play,
    Starting,
    Playing,
}

fn play_state(starting: bool, running: usize, launchable: usize) -> PlayState {
    if launchable > 0 {
        PlayState::Play
    } else if starting {
        PlayState::Starting
    } else if running > 0 {
        PlayState::Playing
    } else {
        PlayState::Play
    }
}

fn shared<T: PartialEq + Copy>(values: impl IntoIterator<Item = T>) -> Option<T> {
    let mut values = values.into_iter();
    let first = values.next()?;
    values.all(|value| value == first).then_some(first)
}

fn maintenance_status(app: &App) -> String {
    let progress = &app.maintenance_progress;
    let mut parts = vec![app.maintenance_label().to_owned()];
    if let Some(percent) = maintenance_percent(progress) {
        parts.push(format!("{percent}%"));
    }
    if let Some(left) = progress.overall.map_or(progress.remaining, |overall| overall.remaining).filter(|left| !left.is_zero()) {
        parts.push(tf("{time} left", &[("time", &remaining_time(left))]));
    }
    parts.join(" · ")
}

/// The current (or next) all-worlds maintenance window, in local time: "11/12 14:00 to 17:00".
fn maintenance_window(app: &App) -> Option<String> {
    use chrono::Local;
    let now = chrono::Utc::now();
    let window = app.services.as_ref()?.windows.iter().filter(|w| w.end > now).min_by_key(|w| w.start)?;
    let (start, end) = (window.start.with_timezone(&Local), window.end.with_timezone(&Local));
    let end = if start.date_naive() == end.date_naive() { end.format("%H:%M") } else { end.format("%m/%d %H:%M") };
    Some(tf("{start} to {end}", &[("start", &start.format("%m/%d %H:%M")), ("end", &end)]))
}

fn footer(view: &View, app: &App, state: &mut State) {
    let current = app.accounts.iter().find(|a| a.selected).or_else(|| app.accounts.first());
    let main_action = app.main_action();
    let targets: Vec<&crate::AccountRow> = {
        let selected: Vec<_> = app.accounts.iter().filter(|a| a.selected).collect();
        if selected.is_empty() { current.into_iter().collect() } else { selected }
    };
    let running = targets.iter().filter(|a| a.is_playing()).count();
    let play_state = play_state(targets.iter().any(|a| a.game_start_pending()), running, app.selected_count());
    let playing_row = targets.iter().find(|a| a.is_playing());
    let starting_row = targets.iter().find(|a| a.game_start_pending());
    let play_update = targets.iter().find_map(|a| crate::play_update::line(app, &a.key));
    let (status, detail, tone): (String, String, i32) = if app.accounts.is_empty() {
        (t("Add an account to play").into(), String::new(), 4)
    } else if app.maintenance.is_some() {
        // The status names the job, its share and time left; the stage would repeat it.
        (maintenance_status(app), String::new(), 2)
    } else if let Some(update) = &play_update {
        (update.headline.clone(), String::new(), 2)
    } else if let Some(update) = targets.iter().find_map(|a| crate::play_update::viewer_line(app, &a.key)) {
        // A sign-in waits for PlayOnline's update to start (running, the file operation above says so).
        (update.headline, String::new(), 2)
    } else if let Some(update) = (main_action == MainAction::Maintenance)
        .then(|| targets.iter().find_map(|a| crate::play_update::watch_line(app, &a.key))).flatten() {
        (t("Maintenance").into(), update.headline, 2)
    } else if matches!(main_action, MainAction::Maintenance | MainAction::Offline) && targets.iter().any(|a| app.is_waiting(&a.key)) {
        ((if main_action == MainAction::Maintenance { t("Maintenance") } else { t("Worlds offline") }).into(), app.wait_headline().into(), 2)
    } else if main_action == MainAction::Maintenance {
        // The official window when a notice stated one; else where to read about it.
        let detail = maintenance_window(app).unwrap_or_else(|| t("View official maintenance notices").into());
        (t("Maintenance").into(), detail, 1)
    } else if main_action == MainAction::Offline {
        (t("Worlds offline").into(), t("View official world status").into(), 1)
    } else if let (PlayState::Playing, Some(row)) = (play_state, playing_row) {
        let who = if running > 1 { tf("{running} accounts", &[("running", &running)]) } else { crate::privacy::mask(&row.account.label) };
        (tf("Playing · {who}", &[("who", &who)]), String::new(), 0)
    } else if let (PlayState::Starting, Some(row)) = (play_state, starting_row) {
        let step = if row.status.is_empty() { t("Signing in").into() } else { row.status_text().into_owned() };
        (tf("Starting · {name}", &[("name", &crate::privacy::mask(&row.account.label))]), step, 2)
    } else {
        let verified = app.patch.as_ref().is_some_and(|p| p.files_verified == Some(true));
        let (s, d, tone) = match app.patch.as_ref().map(|p| p.state) {
            Some(crate::patch::PatchState::Current) => (t("Ready to play"), if verified { t("Game files verified") } else { "" }, 0),
            Some(crate::patch::PatchState::Required) => (t("Update available"), "", 1),
            _ if app.viewer.as_ref().is_some_and(crate::viewer::Status::requires_update) => (t("PlayOnline update needed"), "", 1),
            Some(crate::patch::PatchState::Interrupted) => (t("Interrupted update"), "", 1),
            _ if app.patch_loading => (t("Checking version…"), "", 2),
            _ if verified => (t("Files verified"), "", 0),
            _ => (t("Ready to launch"), "", 3),
        };
        (s.into(), d.into(), tone)
    };
    let badge_ink = if tone == 1 { Ink::Warn } else if tone == 0 { Ink::Ok } else { Ink::Sub };
    let badge_fill = match tone { 0 => 3, 1 => 1, _ => 2 };
    let wait_label = (app.vault_ready && !app.wait_keys().is_empty())
        .then(|| t("Wait and play"))
        .or_else(|| targets.iter().any(|a| app.is_waiting(&a.key)).then(|| t("Waiting for FFXI…")));
    let count = app.selected_count();
    let (label, ink): (String, Ink) = match main_action {
        MainAction::Play if play_update.is_some() => (play_update.as_ref().map(|update| update.label.clone()).unwrap_or_default(), Ink::Sub),
        MainAction::Maintenance | MainAction::Offline if wait_label.is_some() => (wait_label.unwrap_or_default().to_owned(), badge_ink),
        MainAction::Maintenance => (t("Maintenance").to_owned(), badge_ink),
        MainAction::Offline => (t("Worlds offline").to_owned(), badge_ink),
        MainAction::Working if app.reshade.busy => (t("ReShade setup").to_owned(), Ink::Sub),
        // The status line above names the job: the button keeps its share, unelided.
        MainAction::Working => (
            match maintenance_percent(&app.maintenance_progress) {
                Some(percent) if app.maintenance.is_some() => format!("{percent}%"),
                _ => app.maintenance_label().to_owned(),
            },
            Ink::Sub,
        ),
        MainAction::Play => match play_state {
            PlayState::Playing => (t("Playing").to_owned(), Ink::Ok),
            PlayState::Starting => (t("Starting…").to_owned(), Ink::Sub),
            PlayState::Play if count > 0 => (tf("Play {count}", &[("count", &count)]), Ink::PlayText),
            PlayState::Play => (t("Play").to_owned(), Ink::Sub),
        },
    };
    let glyph = match main_action {
        MainAction::Play if play_update.is_some() => 3,
        MainAction::Maintenance => 4,
        MainAction::Offline => 5,
        MainAction::Working => 2,
        MainAction::Play => match play_state {
            PlayState::Play => 0,
            PlayState::Playing => 1,
            PlayState::Starting => 2,
        },
    };
    let enabled = main_action != MainAction::Play || app.vault_ready && count > 0;
    let fill = match main_action {
        MainAction::Maintenance | MainAction::Offline => badge_fill,
        MainAction::Working => 2,
        MainAction::Play if play_state == PlayState::Playing => 3,
        _ if !enabled => 2,
        _ => 0,
    };
    // Who Play starts, under it in the rail; with none left to start (all in game), the picks
    // themselves, so the line agrees with the header's count and Shut down selected.
    let picked = |startable: bool| {
        app.accounts
            .iter()
            .filter(|row| row.selected && row.account.mode == AccountMode::Retail && (!startable || row.can_play()))
            .map(|row| crate::privacy::mask(&row.account.label))
            .collect::<Vec<_>>()
    };
    let names = Some(picked(true)).filter(|names| !names.is_empty()).unwrap_or_else(|| picked(false)).join(t(", "));
    let can_shutdown = app.shutdown_count(true) > 0;
    let shutdown_pending = app.accounts.iter().any(|a| a.selected && a.shutting_down);
    let busy = !can_shutdown && shutdown_pending;
    let retail: Vec<&&crate::AccountRow> = targets.iter().filter(|row| row.account.mode == AccountMode::Retail).collect();
    // The launcher picker always shows (with nothing to start, the default launcher).
    let launcher = if retail.is_empty() { Some(app.settings.launch_with) } else { shared(retail.iter().map(|row| app.launcher_for(&row.account))) };
    // Character select, for the retail accounts Play starts (None: they differ).
    let select = (!retail.is_empty()).then(|| shared(retail.iter().map(|row| app.settings.account_auto_login.get(&row.key).copied().unwrap_or_default().enabled)));
    let footer = Footer {
        status: private(app, &status).into(),
        badge: tone,
        shutdown: can_shutdown || shutdown_pending,
        shutdown_busy: busy,
        shutdown_enabled: can_shutdown,
        shutdown_label: (if busy { t("Shutting down…") } else { t("Shut down selected") }).into(),
        launch_current: launcher.and_then(|tool| LaunchWith::ALL.iter().position(|t| *t == tool)).map_or(-1, |i| i as i32),
        play_label: label.into(),
        play_glyph: glyph,
        play_ink: ink,
        play_fill: fill,
        play_enabled: enabled,
        names: names.into(),
    };
    put!(view, get_footer, set_footer, footer);

    // The line under the status: its detail. The Windows components check is its own line
    // under the block (View.banner), since it opens Settings, not Connection & activity.
    let mut notes = Vec::with_capacity(1);
    if !detail.is_empty() {
        let glyph = if main_action == MainAction::Maintenance { Glyph::Wrench } else if tone == 0 { Glyph::CheckCircle } else { Glyph::Info };
        // "View official …" opens Connection & activity with those links: it reads as a link.
        let link = detail == t("View official maintenance notices") || detail == t("View official world status");
        notes.push(RailNote { glyph, text: private(app, &detail).into(), ink: if link { Ink::Accent } else { Ink::Ink } });
    }
    sync_model(&state.notes, notes);

    // The rail's More menu: what Home's header and the footer offered besides Play.
    let mut menu: Vec<(String, Message)> = Vec::new();
    if app.vault_ready && !app.vault_saving {
        menu.push((t("Import").into(), Message::AccountImport(crate::account_import_ui::Msg::Open)));
    }
    if app.launchable_count() > 1 && app.vault_ready {
        menu.push((t("Play all").into(), Message::LaunchAll));
    }
    if app.shutdown_count(false) > 0 {
        menu.push((t("Shut down all").into(), Message::ShutdownAll));
    }
    // Character select names its choice; picking it switches to the other one.
    if let Some(choice) = select {
        let (shown, next) = match choice {
            Some(true) => (CharacterSelect::Automatic.to_string(), false),
            Some(false) => (CharacterSelect::AskEachTime.to_string(), true),
            None => (t("Mixed").to_owned(), true),
        };
        menu.push((format!("{} · {shown}", t("Character select")), Message::SetCharacterSelect(next)));
    }
    menu.push((t("Install & repair").into(), Message::Navigate(Page::Maintenance)));
    menu.push((t("Help & support").into(), Message::OpenUrl("https://support.na.square-enix.com/".into())));
    strings_model(&state.rail_menu, menu.iter().map(|(label, _)| label.clone()));
    state.menu = menu.into_iter().map(|(_, message)| message).collect();
}

// ---- the account sheet ----

/// The picker's entries: "Default (…)" first, then the names found on disk, then the account's own value
/// if it is no longer there. Returns the labels, values and the selected index.
fn picks(default_label: String, found: &[String], current: Option<&String>, missing: impl Fn(&str) -> String) -> (Vec<String>, Vec<Option<String>>, i32) {
    let mut labels = vec![default_label];
    let mut values = vec![None];
    for name in found {
        labels.push(name.clone());
        values.push(Some(name.clone()));
    }
    let selected = match current {
        None => 0,
        Some(name) => match values.iter().position(|v| v.as_ref().is_some_and(|v| v.eq_ignore_ascii_case(name))) {
            Some(i) => i,
            None => {
                labels.push(missing(name));
                values.push(Some(name.clone()));
                values.len() - 1
            }
        },
    };
    (labels, values, selected as i32)
}

/// Whether the account sheet shows: open, and no close or delete confirmation over it.
pub(super) fn sheet_shown(app: &App) -> bool {
    app.editor.is_some() && !app.confirm_close && app.pending_delete.is_none() && app.portrait_picker.is_none()
}

fn sheet(view: &View, app: &App, state: &mut State) {
    // The close and delete confirmations stack above the sheet: it hides behind them.
    put!(view, get_sheet_open, set_sheet_open, sheet_shown(app));
    let Some(editor) = &app.editor else {
        state.editor_key = None;
        // A closed sheet keeps no sign-in details in the window.
        put!(view, get_sheet_label, set_sheet_label, SharedString::new());
        put!(view, get_sheet_server, set_sheet_server, SharedString::new());
        put!(view, get_sheet_pol_id, set_sheet_pol_id, SharedString::new());
        put!(view, get_sheet_pol_password, set_sheet_pol_password, SharedString::new());
        put!(view, get_sheet_se_id, set_sheet_se_id, SharedString::new());
        put!(view, get_sheet_se_password, set_sheet_se_password, SharedString::new());
        put!(view, get_sheet_handles, set_sheet_handles, SharedString::new());
        put!(view, get_sheet_totp_key, set_sheet_totp_key, SharedString::new());
        put!(view, get_sheet_folder, set_sheet_folder, SharedString::new());
        return;
    };
    state.editor_key = editor.key.clone();
    state.flags = editor.flags;
    let account = &editor.account;
    let flags = editor.flags;
    let privacy = crate::privacy::on();
    let retail = account.mode == AccountMode::Retail;
    let editing = editor.key.is_some();
    let row = editor.key.as_ref().and_then(|key| app.accounts.iter().find(|row| &row.key == key));
    let active = row.is_some_and(|row| row.busy || row.pid.is_some());
    let can_type = !editing || flags.change_password;
    // A saved key is guarded like the password; an account without one takes a key at once.
    let key_can_type = !editing || flags.change_totp_key || row.is_none_or(|row| row.account.totp_key.is_empty());

    // Launch.
    let default = app.settings.launch_with;
    let launch_index = if retail {
        EDITOR_LAUNCH
            .iter()
            .position(|value| *value == account.launch_with.filter(|tool| *tool != LaunchWith::Native))
            .unwrap_or(0) as i32
    } else {
        // xiloader, Windower, Ashita (`private-launch-choices`).
        match account.launch_with { Some(LaunchWith::Windower) => 1, Some(LaunchWith::Ashita) => 2, _ => 0 }
    };
    let launch_hint: String = if !retail {
        match account.effective_launch_with(default) {
            LaunchWith::Windower => "Windower starts xiloader and loads into it, with the profile below.".into(),
            LaunchWith::Ashita => "Ashita starts xiloader and loads into it, with the boot config below.".into(),
            _ => "Runs xiloader on its own, without Windower or Ashita.".into(),
        }
    } else { match account.launch_with {
        None => tf("Default launcher: {default}.", &[("default", &default)]),
        Some(LaunchWith::Native) => tf("The former native client is unavailable. This retail account now uses {default}.", &[("default", &default)]),
        Some(LaunchWith::Vanilla) => t("Starts FFXI directly, without Windower or Ashita.").into(),
        Some(tool) => tf("Signs in without PlayOnline and starts the game through {tool}.", &[("tool", &tool)]),
    } };
    let found = &app.launch_tools.detected;
    let (tool, tool_label, tool_note, labels, values, tool_current) = match account.effective_launch_with(default) {
        LaunchWith::Ashita => {
            let base = app.settings.ashita_boot_ini.trim();
            let base = if base.is_empty() { crate::pol_less::tools::DEFAULT_BOOT_INI } else { base };
            let (labels, values, current) = picks(tf("Default ({name})", &[("name", &base)]), &found.boot_inis, account.ashita_boot_ini.as_ref(), |name| tf("{name} (missing)", &[("name", &name)]));
            let note = if found.ashita.is_none() { t("Ashita was not found. Set its folder in Settings → System → Launch tools.").to_owned() } else { String::new() };
            (1, t("Ashita boot config"), note, labels, values, current)
        }
        LaunchWith::Windower => {
            let base = app.settings.windower_profile.trim();
            let base = if base.is_empty() { t("Windower's default profile") } else { base };
            let (labels, values, current) = picks(tf("Default ({name})", &[("name", &base)]), &found.profiles, account.windower_profile.as_ref(), |name| tf("{name} (not in settings.xml)", &[("name", &name)]));
            let note = if found.windower.is_none() {
                t("Windower was not found. Set its folder in Settings → System → Launch tools.").to_owned()
            } else if account.windower_profile.is_none() && app.settings.windower_profile.trim().is_empty() {
                // What "Default" means here was easy to miss: not one of the named profiles.
                "Default starts Windower's unnamed default profile, not one of your named ones. Pick a profile to use its settings and keybinds.".to_owned()
            } else { String::new() };
            (2, t("Windower profile"), note, labels, values, current)
        }
        LaunchWith::Native | LaunchWith::Vanilla => (0, "", String::new(), Vec::new(), Vec::new(), -1),
    };
    state.tool = tool;
    state.tool_values = values;
    strings_model(&state.tool_items, labels);

    // Status (saved accounts) / After adding (new ones).
    let (has_status, status_ink, status_label, status_detail, sign_out_enabled, timing, has_log, log_enabled) = match row {
        Some(row) => {
            let line = crate::account_row::row_view(app, row);
            let idle = !row.busy && row.pid.is_none();
            (
                true,
                row_ink(crate::account_row::row_state(app, row)),
                private(app, &line.label),
                private(app, &line.detail),
                row.can_sign_out() && !app.launch_blocked(),
                row.timing.clone(),
                row.log.is_some(),
                idle,
            )
        }
        None => (false, Ink::Sub, String::new(), String::new(), false, String::new(), false, false),
    };
    // An authenticator key fills in the codes: such an account signs in like one without codes.
    let codes_typed = flags.uses_otp && account.totp_key.trim().is_empty();
    let allowed = app.settings.auto_sign_in && !codes_typed;
    let sign_in_note = if !app.settings.auto_sign_in {
        t("Automatic sign-in is off in Settings → System.")
    } else if codes_typed {
        t("One-time password accounts sign in when you type a code on Home.")
    } else {
        ""
    };

    // The Region field as set now against the installed editions.
    let region_mismatch = app.region_mismatch(account);
    let region_fix = region_mismatch.as_ref().and_then(|_| crate::retail_region::only_edition(&app.pol_editions()));
    let picture = editor.key.as_deref().and_then(|key| own_picture(app, key));
    let sheet = Sheet {
        portrait_shown: editing && retail && row.is_some(),
        has_picture: picture.is_some(),
        picture: picture.unwrap_or_default(),
        editing,
        caption: (if editing { t("Account settings") } else { t("New account") }).to_uppercase().into(),
        title: (if editing { crate::privacy::mask(&account.label) } else { t("Add account").to_owned() }).into(),
        retail,
        active,
        label_masked: privacy,
        id_label: (if retail { t("PlayOnline ID") } else { t("Username") }).into(),
        id_placeholder: (if retail { t("Your PlayOnline ID") } else { t("Account ID") }).into(),
        id_masked: privacy || editing && !flags.show_id,
        id_button: (if flags.show_id { t("Hide") } else { t("Show") }).into(),
        password_label: (if retail { t("PlayOnline password") } else { t("Password") }).into(),
        password_masked: privacy || !flags.show_password,
        password_enabled: can_type,
        password_button: (if !can_type { t("Change") } else if flags.show_password { t("Hide") } else { t("Show") }).into(),
        se_id_masked: privacy,
        handles_masked: privacy,
        region: i32::from(account.region.min(2)),
        region_mismatch: region_mismatch.unwrap_or_default().into(),
        region_fix: region_fix.map(|edition| tf("Set to {edition}", &[("edition", &edition.wire())])).unwrap_or_default().into(),
        region_fix_index: region_fix.map_or(0, |edition| i32::from(edition.index())),
        uses_otp: flags.uses_otp,
        totp_key_masked: privacy || !flags.show_totp_key,
        totp_key_enabled: key_can_type,
        totp_key_button: (if !key_can_type { t("Change") } else if flags.show_totp_key { t("Hide") } else { t("Show") }).into(),
        launch_index,
        launch_hint: launch_hint.into(),
        tool,
        tool_label: tool_label.into(),
        tool_current,
        tool_note: tool_note.into(),
        edit_profiles_enabled: found.windower.is_some() && !app.windower_editor_open,
        hairpin: !flags.no_hairpin,
        auto_login: editor.auto_login.enabled,
        slot: i32::from(editor.auto_login.character_slot.clamp(1, 16)) - 1,
        folder_masked: privacy,
        folder_enabled: true,
        browse_enabled: !app.vault_saving,
        has_status,
        status_ink,
        status_label: status_label.into(),
        status_detail: status_detail.into(),
        sign_out_enabled,
        timing: timing.into(),
        has_log,
        log_enabled,
        after_adding: row.is_none() && retail,
        sign_in_now: flags.sign_in_now && allowed,
        sign_in_now_enabled: allowed,
        sign_in_note: sign_in_note.into(),
        app_status: private(app, &app.status).into(),
        delete_enabled: !active && !app.vault_saving,
        save_label: (if editing { t("Save") } else { t("Add account") }).into(),
        save_enabled: !app.vault_saving,
    };
    put!(view, get_sheet, set_sheet, sheet);
    put!(view, get_sheet_label, set_sheet_label, account.label.as_str().into());
    put!(view, get_sheet_server, set_sheet_server, account.server.as_str().into());
    put!(view, get_sheet_pol_id, set_sheet_pol_id, account.pol_id.as_str().into());
    put!(view, get_sheet_pol_password, set_sheet_pol_password, account.pol_password.as_str().into());
    put!(view, get_sheet_se_id, set_sheet_se_id, account.se_id.as_str().into());
    put!(view, get_sheet_se_password, set_sheet_se_password, account.se_password.as_str().into());
    put!(view, get_sheet_handles, set_sheet_handles, account.pol_handle_names.as_str().into());
    put!(view, get_sheet_totp_key, set_sheet_totp_key, account.totp_key.as_str().into());
    put!(view, get_sheet_folder, set_sheet_folder, editor.game_install.as_str().into());
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
