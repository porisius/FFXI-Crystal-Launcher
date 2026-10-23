use crate::{account_import::{self, ImportBatch, ImportDraft, ImportSource}, backend::{Account, AccountMode}, i18n::t, App, Editor, Message};
use iced::Task;

#[derive(Default)]
pub(crate) struct State {
    pub open: bool,
    pub busy: bool,
    pub sources: Vec<ImportSource>,
    pub path: String,
    pub companion: String,
    pub use_companion: bool,
    pub region: u8,
    pub batch: Option<ImportBatch>,
    pub error: String,
    generation: u64,
}

#[derive(Clone)]
pub(crate) enum Msg {
    Open, Close, Browse(bool), Picked(u64, bool, Option<std::path::PathBuf>),
    Sources(u64, Vec<ImportSource>), Source(usize), Path(String),
    Companion(bool), CompanionPath(String), Region(u8), Read,
    Loaded(u64, Result<ImportBatch, String>), Review(usize),
}

impl App {
    pub(crate) fn import_update(&mut self, msg: Msg) -> Task<Message> {
        let state = &mut self.account_import;
        match msg {
            Msg::Open if self.vault_ready && !self.vault_saving => {
                state.open = true;
                state.busy = true;
                state.generation = state.generation.wrapping_add(1);
                let generation = state.generation;
                return Task::perform(async {
                    tokio::task::spawn_blocking(account_import::detect_sources).await.unwrap_or_default()
                }, move |sources| Message::AccountImport(Msg::Sources(generation, sources)));
            }
            Msg::Close => { state.open = false; state.busy = false; state.batch = None; state.generation = state.generation.wrapping_add(1); }
            Msg::Sources(generation, sources) if state.open && generation == state.generation => {
                state.busy = false;
                if state.path.is_empty() {
                    if let Some(source) = sources.first() {
                        state.path = source.path.to_string_lossy().into_owned();
                        state.region = source.region.unwrap_or(0);
                    }
                }
                if state.companion.is_empty() {
                    if let Some(source) = sources.iter().find(|s| s.kind == account_import::SourceKind::PlayOnline) {
                        state.companion = source.path.to_string_lossy().into_owned();
                    }
                }
                state.sources = sources;
            }
            Msg::Source(index) if !state.busy => {
                if let Some(source) = state.sources.get(index) {
                    state.path = source.path.to_string_lossy().into_owned();
                    state.region = source.region.unwrap_or(0);
                    state.batch = None;
                    state.error.clear();
                }
            }
            Msg::Path(path) if !state.busy => { state.path = path; state.batch = None; state.error.clear(); }
            Msg::CompanionPath(path) if !state.busy => { state.companion = path; state.batch = None; }
            Msg::Companion(on) if !state.busy => { state.use_companion = on; state.batch = None; }
            Msg::Region(region) if !state.busy && region <= 2 => { state.region = region; state.batch = None; }
            Msg::Browse(companion) if !state.busy => {
                state.busy = true;
                state.generation = state.generation.wrapping_add(1);
                let generation = state.generation;
                return Task::perform(async move {
                    rfd::AsyncFileDialog::new().add_filter("PlayOnline / Forest", &["bin", "dat"])
                        .pick_file().await.map(|f| f.path().to_path_buf())
                }, move |path| Message::AccountImport(Msg::Picked(generation, companion, path)));
            }
            Msg::Picked(generation, companion, path) if state.open && generation == state.generation => {
                state.busy = false;
                if let Some(path) = path {
                    let path = path.to_string_lossy().into_owned();
                    if companion { state.companion = path; } else { state.path = path; }
                    state.batch = None;
                    state.error.clear();
                }
            }
            Msg::Read if state.open && !state.busy && !state.path.trim().is_empty() => {
                state.busy = true;
                state.batch = None;
                state.error.clear();
                state.generation = state.generation.wrapping_add(1);
                let generation = state.generation;
                let path = crate::plain_text_path(state.path.trim());
                let companion = state.use_companion.then(|| crate::plain_text_path(state.companion.trim()));
                let region = state.region;
                return Task::perform(async move {
                    tokio::task::spawn_blocking(move || {
                        let mut batch = account_import::import_file(std::path::Path::new(&path), region)?;
                        if let Some(path) = companion {
                            let members = account_import::import_file(std::path::Path::new(&path), region)?;
                            account_import::fill_from_pol(&mut batch, &members);
                            batch.warnings.extend(members.warnings.iter().cloned());
                        }
                        Ok(batch)
                    }).await.unwrap_or_else(|_| Err(t("Account import could not complete.").into()))
                }, move |result| Message::AccountImport(Msg::Loaded(generation, result)));
            }
            Msg::Loaded(generation, result) if state.open && generation == state.generation => {
                state.busy = false;
                match result { Ok(batch) => state.batch = Some(batch), Err(error) => state.error = error }
            }
            Msg::Review(index) if state.open && !state.busy && self.vault_ready && !self.vault_saving => {
                if let Some(draft) = state.batch.as_ref().and_then(|b| b.drafts.get(index)) {
                    let mut matches = self.accounts.iter().filter(|row| matches_existing(draft, &row.account));
                    let existing = matches.next();
                    if matches.next().is_some() {
                        state.error = t("More than one saved account matches. Edit the saved account directly.").into();
                        return Task::none();
                    }
                    if existing.is_some_and(|row| row.busy || row.pid.is_some()) {
                        state.error = t("Close this account's game and sign out before editing its login.").into();
                        return Task::none();
                    }
                    let key = existing.map(|row| row.key.clone());
                    let account = merge_draft(draft, existing.map(|row| &row.account));
                    let auto_login = key.as_ref().and_then(|key| self.settings.account_auto_login.get(key)).copied()
                        .unwrap_or(crate::pol_less::AutoLogin { enabled: false, character_slot: draft.character_slot.unwrap_or(1) }).normalized();
                    self.editor = Some(Editor {
                        game_install: key.as_ref().and_then(|key| self.settings.account_installs.get(key)).cloned().unwrap_or_default(),
                        dat_overlays: key.as_ref().and_then(|key| self.settings.account_overlays.get(key)).cloned().unwrap_or_default(),
                        auto_login,
                        hairpin: true,
                        key,
                        account,
                    });
                    state.open = false;
                    self.status = t("Review imported account details, complete missing fields, then save. Existing saved fields were kept.").into();
                }
            }
            _ => {}
        }
        Task::none()
    }
}

fn matches_existing(draft: &ImportDraft, account: &Account) -> bool {
    if account.mode != AccountMode::Retail { return false; }
    if !draft.pol_id.is_empty() { return draft.pol_id.eq_ignore_ascii_case(&account.pol_id); }
    if account.region != draft.region { return false; }
    if !draft.se_id.is_empty() { return draft.se_id.eq_ignore_ascii_case(&account.se_id); }
    !draft.label.is_empty() && draft.label == account.label
}

fn merge_draft(draft: &ImportDraft, existing: Option<&Account>) -> Account {
    let mut account = existing.cloned().unwrap_or_else(crate::blank_account);
    if existing.is_none() { account.region = draft.region; account.label.clear(); }
    for (target, source) in [(&mut account.label, &draft.label), (&mut account.pol_id, &draft.pol_id),
        (&mut account.pol_password, &draft.pol_password), (&mut account.se_id, &draft.se_id), (&mut account.se_password, &draft.se_password)] {
        if target.is_empty() { target.clone_from(source); }
    }
    if account.launch_with.is_none() { account.launch_with = draft.launch_with; }
    if account.ashita_boot_ini.is_none() { account.ashita_boot_ini.clone_from(&draft.ashita_boot_ini); }
    if account.windower_profile.is_none() { account.windower_profile.clone_from(&draft.windower_profile); }
    account
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_import_preserves_saved_credentials_and_launch_preferences() {
        let mut saved = crate::blank_account();
        saved.pol_id = "ABCD1234".into(); saved.pol_password = "saved-pol".into(); saved.se_id = "savedse".into();
        saved.se_password = "saved-se-password".into(); saved.windower_profile = Some("My profile".into());
        let mut draft = ImportDraft::default();
        draft.pol_id = "ABCD1234".into(); draft.se_password = "imported".into();
        draft.windower_profile = Some("Imported profile".into());
        assert!(matches_existing(&draft, &saved));
        let result = merge_draft(&draft, Some(&saved));
        assert_eq!(result.pol_password, saved.pol_password);
        assert_eq!(result.se_password, saved.se_password);
        assert_eq!(result.windower_profile, saved.windower_profile);
        assert_eq!(result.se_id, saved.se_id);
        draft.region = 2;
        assert!(matches_existing(&draft, &saved), "POL identity does not depend on the import region");
        assert_eq!(merge_draft(&draft, Some(&saved)).region, saved.region);
    }
}
