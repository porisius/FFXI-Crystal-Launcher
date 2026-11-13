//! What an account row says: its state in priority order and the line, progress, code field
//! and action each state shows (Home's rows and the account sheet's status).
use crate::backend::AccountMode;
use crate::i18n::{t, tf};
use crate::{App, Message};

/// A session that is signed in to PlayOnline and idle: its Play starts the game at once.
pub(crate) fn signed_in(row: &crate::AccountRow) -> bool {
    row.pid.is_some() && !row.is_playing() && !row.busy && !row.shutting_down && !row.game_start_pending()
}
/// What one account row shows: its state, the detail after it, an optional
/// progress bar, the inline one-time-password field and its action.
pub(crate) struct RowView {
    pub label: String,
    pub detail: String,
    pub progress: Option<f32>,
    pub otp: bool,
    /// The row's action: label, message, soft accent (true) or quiet.
    pub action: Option<(String, Message, bool)>,
}
/// The local clock time `at` happened, as "11:16".
fn clock_at(at: std::time::Instant) -> String {
    let ago = chrono::Duration::from_std(at.elapsed()).unwrap_or_default();
    (chrono::Local::now() - ago).format("%H:%M").to_string()
}
/// Whether the row is an account that needs a one-time code before it can sign in. An account
/// with an authenticator key needs none typed, unless PlayOnline refused one of its codes.
pub(crate) fn needs_code(app: &App, item: &crate::AccountRow) -> bool {
    item.account.mode == AccountMode::Retail
        && (!item.otp.is_empty() || item.authenticator_refused
            || !item.authenticator_ready() && app.settings.account_otp.contains(&item.key))
}
/// Which state an account row shows, in priority order (its colour needs only this).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowState { Closing, Waiting, Updating, ViewerConfirm, ViewerUpdating, Playing, Starting, SigningIn, SignedIn, Private, NeedsCode, Problem, Idle }
pub(crate) fn row_state(app: &App, item: &crate::AccountRow) -> RowState {
    if item.shutting_down || item.signing_out {
        RowState::Closing
    } else if app.is_waiting(&item.key) {
        RowState::Waiting
    } else if app.play_update_of(&item.key).is_some() {
        RowState::Updating
    } else if app.play_updates.viewer_confirm.as_ref().is_some_and(|(key, _)| *key == item.key) {
        RowState::ViewerConfirm
    } else if !item.busy && app.viewer_update_of(&item.key).is_some() {
        RowState::ViewerUpdating
    } else if item.is_playing() {
        RowState::Playing
    } else if item.game_start_pending() {
        RowState::Starting
    } else if item.busy {
        RowState::SigningIn
    } else if signed_in(item) {
        RowState::SignedIn
    } else if item.account.mode == AccountMode::Private {
        RowState::Private
    } else if needs_code(app, item) && !item.busy && item.pid.is_none() {
        RowState::NeedsCode
    } else if item.problem {
        RowState::Problem
    } else {
        RowState::Idle
    }
}
pub(crate) fn row_view(app: &App, item: &crate::AccountRow) -> RowView {
    let idle = !item.busy && item.pid.is_none();
    let state = row_state(app, item);
    let view = |label: String, detail: String| RowView {
        label,
        detail,
        progress: None,
        otp: false,
        action: None,
    };
    // Maintenance's update of this row's game folder, after the row's own state.
    let watch = || crate::play_update::watch_line(app, &item.key);
    match state {
        RowState::Closing => view(item.status_text().into_owned(), String::new()),
        RowState::Waiting => {
            let mut row = view(app.wait_headline().into(), app.wait_detail());
            row.action = Some((t("Cancel").into(), Message::CancelWait(vec![item.key.clone()]), false));
            if let Some(update) = watch() {
                (row.detail, row.progress) = (update.headline, update.fraction);
                // Cancel next to the download's progress stops the download; the wait's own
                // Cancel is back once it is done.
                if let Some(generation) = update.cancel { row.action = Some((t("Cancel").into(), Message::CancelPlayUpdate(generation), false)); }
            } else if let Some(update) = crate::play_update::viewer_line(app, &item.key) {
                // Its sign-in for the wait was stopped for PlayOnline's update.
                (row.detail, row.progress) = (update.headline, update.fraction);
            }
            row
        }
        RowState::Updating => {
            let Some(update) = crate::play_update::line(app, &item.key) else { return view(t("Not signed in").into(), String::new()) };
            let mut row = view(update.headline, String::new());
            row.progress = update.fraction;
            row.action = update.cancel.map(|generation| (t("Cancel").to_owned(), Message::CancelPlayUpdate(generation), false));
            row
        }
        RowState::ViewerConfirm => {
            let mut row = view(item.status_text().into_owned(), String::new());
            row.action = Some((t("Sign out and update").into(), Message::ConfirmViewerUpdate, true));
            row
        }
        RowState::ViewerUpdating => {
            let Some(update) = crate::play_update::viewer_line(app, &item.key) else { return view(t("Not signed in").into(), String::new()) };
            let mut row = view(update.headline, String::new());
            row.progress = update.fraction;
            if app.viewer_update_running() && !app.maintenance_cancelled {
                row.action = Some((t("Cancel").into(), Message::CancelMaintenance, false));
            }
            row
        }
        RowState::Playing => {
            let since = item
                .play_timing
                .as_ref()
                .map(|timing| tf("since {time}", &[("time", &clock_at(timing.started()))]))
                .unwrap_or_default();
            let mut row = view(t("In game").into(), since);
            if item.can_shut_down() {
                row.action = Some((t("Shut down").into(), Message::ShutdownAccount(item.key.clone()), false));
            }
            row
        }
        // The state alone, with the rail's working bar: the worker's step words repeat it.
        RowState::Starting => {
            let mut row = view(t("Starting").into(), String::new());
            // A start that has waited a while can be cleared by hand (`start_watch`).
            if app.start_cancellable(&item.key) {
                row.action = Some((t("Cancel").into(), Message::CancelStart(item.key.clone()), false));
            }
            row
        }
        RowState::SigningIn => view(t("Signing in").into(), String::new()),
        RowState::SignedIn => {
            // A Play this session refused (its launcher changed since it signed in) says why.
            let mut row = view(t("Signed in").into(), if item.problem { item.status_text().into_owned() } else { String::new() });
            if item.can_play() && app.vault_ready && !app.launch_blocked() {
                row.action = Some((t("Play now").into(), Message::Launch(item.key.clone()), true));
            }
            if let Some(update) = watch() {
                (row.detail, row.progress) = (update.headline, update.fraction);
                if let Some(generation) = update.cancel { row.action = Some((t("Cancel").into(), Message::CancelPlayUpdate(generation), false)); }
            }
            row
        }
        // A private-server account plays through xiloader: no sign-in step, no one-time code
        // field (xiloader takes the account's own). A failed launch says why, with its log.
        RowState::Private => {
            let detail = if item.problem { item.status_text().into_owned() } else { crate::privacy::mask(item.account.server.trim()) };
            let mut row = view(t("Private server").into(), detail);
            if idle && item.can_play() && app.vault_ready && !app.launch_blocked() {
                row.action = Some((t("Play").into(), Message::Launch(item.key.clone()), true));
            }
            row
        }
        RowState::NeedsCode => {
            let mut row = view(
                t("Needs a one-time password").into(),
                if item.problem { item.status_text().into_owned() } else { app.region_mismatch(&item.account).unwrap_or_default() },
            );
            row.otp = true;
            let can_sign_in = idle && app.vault_ready && !app.launch_blocked();
            if can_sign_in && !item.otp.trim().is_empty() {
                row.action = Some((t("Sign in").into(), Message::SignIn(item.key.clone()), true));
            }
            row
        }
        RowState::Problem => {
            let mut row = view(item.status_text().into_owned(), String::new());
            // Code boxes only where a code could be the answer: a refused member login, or a
            // spent code. A loader that never connected, a crash or a version stop asked for none
            // (live 2026-11-10: two timed-out rows looked like they wanted a code).
            // An authenticator key answers both.
            row.otp = idle && !item.authenticator_ready()
                && (crate::launch_log::sign_in_refused(&item.status) || item.otp_spent);
            if item.log.is_some() && idle {
                row.action = Some((t("View log").into(), Message::OpenLog(item.key.clone()), false));
            }
            row
        }
        RowState::Idle => {
            let detail = watch().map(|update| update.headline).or_else(|| app.region_mismatch(&item.account)).unwrap_or_default();
            let mut row = view(t("Not signed in").into(), detail);
            if idle && app.vault_ready && !app.launch_blocked() && item.can_play() {
                row.action = Some((t("Sign in").into(), Message::SignIn(item.key.clone()), false));
            }
            row
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private-server account offers Play (xiloader takes no PlayOnline sign-in), names its
    /// server, and says why its last launch failed.
    #[test]
    fn a_private_server_row_offers_play_and_names_its_server() {
        crate::i18n::set(crate::i18n::Lang::En);
        let mut app = crate::tests::app();
        app.vault_ready = true;
        let row = &mut app.accounts[2];
        row.account.mode = AccountMode::Private;
        row.account.server = "ffxi.example.test".into();
        let key = row.key.clone();
        let line = row_view(&app, &app.accounts[2]);
        assert_eq!(row_state(&app, &app.accounts[2]), RowState::Private);
        assert_eq!((line.label.as_str(), line.detail.as_str()), (t("Private server"), "ffxi.example.test"));
        assert!(!line.otp);
        assert!(matches!(&line.action, Some((_, Message::Launch(k), true)) if *k == key));
        app.accounts[2].problem = true;
        app.accounts[2].status = "xiloader was not found".into();
        assert_eq!(row_view(&app, &app.accounts[2]).detail, "xiloader was not found");
        app.accounts[2].busy = true;
        assert!(row_view(&app, &app.accounts[2]).action.is_none());
    }

    /// A signed-in account plays at once, an OTP account asks for its code and signs in only
    /// once one is typed, an idle one offers a background sign-in.
    #[test]
    fn home_rows_show_each_state_with_its_action() {
        crate::i18n::set(crate::i18n::Lang::En);
        let mut app = crate::tests::app();
        let keys: Vec<String> = app.accounts.iter().map(|row| row.key.clone()).collect();
        app.accounts[0].pid = Some(7);
        app.accounts[0].game_running = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        app.settings.account_otp.insert(keys[1].clone());
        let signed = row_view(&app, &app.accounts[0]);
        assert!(signed_in(&app.accounts[0]));
        assert_eq!(signed.label, t("Signed in"));
        assert!(matches!(&signed.action, Some((_, Message::Launch(key), true)) if *key == keys[0]));
        let otp = row_view(&app, &app.accounts[1]);
        assert!(otp.otp && otp.action.is_none() && otp.label == t("Needs a one-time password"));
        app.accounts[1].otp = "123456".into();
        assert!(matches!(row_view(&app, &app.accounts[1]).action, Some((_, Message::SignIn(_), true))));
        let idle = row_view(&app, &app.accounts[2]);
        assert!(!idle.otp && matches!(idle.action, Some((_, Message::SignIn(_), false))));
        // In game is playing, not signed in.
        app.accounts[0].game_running = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)));
        assert!(app.accounts[0].is_playing() && !signed_in(&app.accounts[0]));
    }

    /// Only an EU PlayOnline installed: a US row says to set EU, a new account starts as EU, a
    /// saved account keeps its own region until changed. Two editions: no automatic pick.
    #[test]
    fn a_region_mismatch_shows_on_the_row_and_a_new_account_takes_the_one_edition() {
        use crate::retail_region::RetailRegion;
        crate::i18n::set(crate::i18n::Lang::En);
        let mut app = crate::tests::app();
        assert_eq!(app.accounts[2].account.region, 0);
        assert!(row_view(&app, &app.accounts[2]).detail.is_empty(), "nothing known, nothing said");
        app.pol_editions = vec![RetailRegion::Eu];
        let note = "This PlayOnline is the EU version. Set this account to EU.";
        assert_eq!(row_view(&app, &app.accounts[2]).detail, note);
        let _ = app.update(Message::NewAccount);
        assert_eq!(app.editor.as_ref().map(|editor| editor.account.region), Some(1));
        let key = app.accounts[2].key.clone();
        let _ = app.update(Message::EditAccount(key));
        assert_eq!(app.editor.as_ref().map(|editor| editor.account.region), Some(0), "an explicit region is never changed silently");
        app.accounts[2].account.region = 1;
        assert!(row_view(&app, &app.accounts[2]).detail.is_empty());
        app.pol_editions = vec![RetailRegion::Us, RetailRegion::Eu];
        app.editor = None;
        let _ = app.update(Message::NewAccount);
        assert_eq!(app.editor.as_ref().map(|editor| editor.account.region), Some(0), "two editions: the default stays");
    }

    /// A signed-in row whose Play its session refused shows why, and still offers Play.
    #[test]
    fn a_signed_in_row_says_why_its_play_was_refused() {
        crate::i18n::set(crate::i18n::Lang::En);
        let mut app = crate::tests::app();
        let row = &mut app.accounts[0];
        row.pid = Some(7);
        row.game_running = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));
        assert!(row_view(&app, &app.accounts[0]).detail.is_empty(), "nothing to say without a problem");
        let refusal = t("Sign out of PlayOnline before switching launch methods, then launch again.");
        (app.accounts[0].problem, app.accounts[0].status) = (true, refusal.into());
        let view = row_view(&app, &app.accounts[0]);
        assert_eq!(row_state(&app, &app.accounts[0]), RowState::SignedIn);
        assert_eq!((view.label.as_str(), view.detail.as_str()), (t("Signed in"), refusal));
        assert!(matches!(view.action, Some((_, Message::Launch(_), true))));
    }

    /// A row's Shut down shows only while its own game runs, closes just that game with the
    /// current Close games instantly choice, and is gone while the close is in flight.
    #[test]
    fn a_running_game_row_offers_shut_down_that_ends_only_that_game_once() {
        use std::sync::{atomic::AtomicBool, Arc};
        crate::i18n::set(crate::i18n::Lang::En);
        let mut app = crate::tests::app();
        for (i, row) in app.accounts.iter_mut().enumerate() {
            row.pid = Some(100 + i as u32);
            row.busy = false;
            row.game_running = Some(Arc::new(AtomicBool::new(i < 2)));
            row.alive = Some(Arc::new(AtomicBool::new(true)));
        }
        let key = app.accounts[0].key.clone();
        let running = row_view(&app, &app.accounts[0]);
        assert_eq!(running.label, t("In game"));
        assert!(matches!(&running.action, Some((label, Message::ShutdownAccount(k), false)) if *label == t("Shut down") && *k == key));
        assert!(matches!(&row_view(&app, &app.accounts[2]).action, Some((_, Message::Launch(_), true))), "a signed-in row keeps Play now");
        // The click ends that account's game only, in the chosen mode.
        app.settings.close_games_instantly = true;
        assert_eq!(app.update(Message::ShutdownAccount(key)).units(), 1);
        assert!(app.accounts[0].shutting_down && !app.accounts[1].shutting_down);
        assert_eq!(app.accounts[0].status_text(), t("Closing FFXI…"));
        assert_eq!(row_state(&app, &app.accounts[0]), RowState::Closing);
        assert!(row_view(&app, &app.accounts[0]).action.is_none(), "no repeat click while it closes");
        let again = app.accounts[0].key.clone();
        assert_eq!(app.update(Message::ShutdownAccount(again)).units(), 0);
        // Not running: no Shut down.
        app.accounts[1].game_running = Some(Arc::new(AtomicBool::new(false)));
        assert!(!matches!(row_view(&app, &app.accounts[1]).action, Some((_, Message::ShutdownAccount(_), _))));
    }

    /// Live 2026-11-10: a loader that never connected left the row with code boxes, which read
    /// as "this account needs a one-time code". A failed row offers a code only after a refused
    /// member login or a spent code; any other failure shows its message and View log.
    #[test]
    fn only_a_refused_sign_in_or_a_spent_code_offers_code_boxes_on_a_failed_row() {
        crate::i18n::set(crate::i18n::Lang::En);
        let mut app = crate::tests::app();
        let row = &mut app.accounts[2];
        row.problem = true;
        row.log = Some(std::path::PathBuf::from("a.crystal.log"));
        row.status = "crystal-loader did not connect within 60 s. Check that Ashita-cli.exe in C:\\Ashita started.".into();
        let timed_out = row_view(&app, &app.accounts[2]);
        assert_eq!(row_state(&app, &app.accounts[2]), RowState::Problem);
        assert!(!timed_out.otp, "a timeout asks for no code");
        assert!(matches!(timed_out.action, Some((_, Message::OpenLog(_), false))));
        assert!(!app.settings.account_otp.contains(&app.accounts[2].key), "nothing marks the account");
        app.accounts[2].status = "Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -1036). (Ashita v4 via crystal-loader).".into();
        assert!(row_view(&app, &app.accounts[2]).otp, "a refused member login may want a code");
        app.accounts[2].status = "Game loader error during startup: Native PlayOnline member login failed (state 39, POL result -19). (Ashita v4 via crystal-loader).".into();
        assert!(!row_view(&app, &app.accounts[2]).otp, "a network failure is no verdict on the account");
        app.accounts[2].otp_spent = true;
        assert!(row_view(&app, &app.accounts[2]).otp, "a spent code asks for a new one");
    }
}