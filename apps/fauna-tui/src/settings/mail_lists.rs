//! The Settings → Mail → Lists sub-page (`docs/goal/behavior/mail-mass-mailing.md`
//! § `mail-lists` page UX; `tests/e2e-unified/ui.yaml` `mail-lists` page + its
//! `mail-lists-list` component).
//!
//! A person runs **their own** mailing lists here — a list is a sixth alias kind,
//! so Lists sit beside Aliases under mail settings. Each row carries the list's
//! name and posting address, its subscribed-member count, when it last sent, and
//! the user's own running quota meter; the row's controls edit it, open its
//! members, or delete it (cascading the members).
//!
//! **A dumb renderer over the shared machine.** Every decision is
//! `fauna_client_mail_settings::MailListsMachine`'s: the wire→view projection
//! (`project_list_row` — address composition, the friendly-name fallback), the
//! user-tier domain derivation (`derive_list_domains`), the refresh-after-mutate
//! sequencing. This module paints `MailListsSnapshot` and dispatches
//! `MailListsAction`. Direct Rust, no FFI hop.
//!
//! **The error bridge is load-bearing** — the `mail_aliases.rs` reasoning: a
//! dispatch failure arrives on the *snapshot* (`MailListsSnapshot.error`), not
//! the dispatch's return value, so the fold copies it onto `App::errors`.
//! Without it a rejected create (`conflicts_with_existing_alias`,
//! `reserved_local_part`, a `recipients_per_send` over the admin ceiling) would
//! paint no error and have no effect — the dropped-command shape testing.md
//! point 11 forbids.
//!
//! **Row leaves are painted FLAT-indexed, not `.within(..)`-scoped.** The
//! authority is the shared action file, not a house style:
//! `actions/mail_lists.py` reads and clicks every row control with a plain
//! `click(id, index)` / `get_text(id, index)`, so declaring containment here
//! would make every one of those reads resolve to nothing while the page painted
//! perfectly (the `restore-history-item` bug, inverted). This is the opposite
//! choice from `mail_aliases.rs`, whose action file *does* pass `scope=` — read
//! the action file before choosing the row shape; neither is the default.
//!
//! **Every row paints every control**, unconditionally. That keeps the row's id
//! families sharing one index space, so `delete_list(i)` and `open_members(i)`
//! address the same row `names()[i]` names. linux does the same (its member-page
//! twin toggles *sensitivity* rather than omitting the control) — the shape that
//! avoids the conditionally-omitted-control index hazard the aliases page hit.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    ListDraft, ListView, MailListsAction, MailListsMachine, MailListsSnapshot,
    archive_url_needs_confirm,
};
use fauna_i18n::strings::mail_lists as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture, SelectTarget};

/// The Lists sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// `MailAliasesState` shape: construction is sync and cheap (the RPC is
/// `hydrate()`), it carries interior mutability, and holding it as an `Arc` is
/// what lets an `Op` own only `Arc`s and cross a `tokio::spawn`. Session-scoped —
/// `clear_session` drops it, so a stale machine never outlives a sign-out.
#[derive(Default)]
pub struct MailListsState {
    /// The shared machine. `None` pre-login.
    pub machine: Option<Arc<MailListsMachine>>,
    /// The last snapshot the page painted. `None` until the nav-edge hydrate
    /// folds one — the list then paints its honest empty state.
    pub snapshot: Option<MailListsSnapshot>,

    // ── the add/edit sheet (an inline reveal, not a modal — the
    // `mail-add-credential` idiom every app uses; the state protocol cannot open
    // a separate window) ──
    /// Whether the add/edit sheet is open.
    pub show_add_form: bool,
    /// `Some(list_id_hex)` = the sheet is in **edit** mode for that row (submit
    /// dispatches `Update`); `None` = add mode (submit dispatches `Create`).
    /// The posting address is immutable on an existing list — the nest treats it
    /// like an alias `kind` — so the local-part and domain inputs are inert while
    /// editing.
    pub editing: Option<String>,
    /// `mail-lists-add-sheet-name-input` — the list's friendly name (≤64 chars).
    pub name_input: String,
    /// `mail-lists-add-sheet-local-part-input` — the address the list sends from.
    pub local_part_input: String,
    /// `mail-lists-add-sheet-domain-picker` — the selected domain. Seeded from
    /// the snapshot's first derived domain when the sheet opens.
    pub domain_input: String,
    /// `mail-lists-add-sheet-description-input` (optional).
    pub description_input: String,
    /// `mail-lists-add-sheet-list-help-url-input` (optional; the List-Help header).
    pub list_help_url_input: String,
    /// `mail-lists-add-sheet-list-archive-url-input` (optional; List-Archive).
    pub list_archive_url_input: String,
    /// `mail-lists-add-sheet-per-send-cap-input` (optional, ≤ the admin ceiling).
    /// Parsed at submit; an unparseable entry is simply `None`, matching linux —
    /// the nest is the authority on the ceiling and rejects an over-cap value.
    pub per_send_cap_input: String,

    // ── two-click inline confirm (no modal; ui.yaml scopes no separate confirm
    // id to this page, so the arm lives on the button itself) ──
    /// The `list_id_hex` whose delete button is armed. Delete cascades every
    /// member row, so it gets the two-click gate `actions/mail_lists.py::
    /// delete_list` drives (it clicks the same button twice).
    pub delete_armed: Option<String>,
    /// The List-Archive URL the sheet's Submit is armed for. A link off the
    /// user's own server is published to every recipient, so the first Submit
    /// relabels with `mail_lists.archive_off_server_confirm` and saves nothing
    /// (`mail-mass-mailing.md` § Don't do these; the decision is the shared
    /// `archive_url_needs_confirm`). Keyed by the URL, so editing the link after
    /// arming disarms without any extra bookkeeping.
    pub archive_confirm_armed: Option<String>,
}

impl MailListsState {
    /// Build the page's shared machine from the session's WS handle. Infallible
    /// (unlike Mail, which decodes a secret) — the seam is user-tier and derives
    /// the owning actor nest-side.
    pub(super) fn build(nest: Arc<fauna_client::NestClient>) -> Self {
        Self {
            machine: Some(Arc::new(
                fauna_client_mail_settings::rpc_glue::build_mail_lists_machine(nest),
            )),
            ..Default::default()
        }
    }

    /// The row for `list_id_hex` in the last painted snapshot.
    pub(super) fn list(&self, list_id_hex: &str) -> Option<&ListView> {
        self.snapshot
            .as_ref()?
            .lists
            .iter()
            .find(|l| l.list_id_hex == list_id_hex)
    }

    /// The domains the add-sheet picker offers (the machine's user-tier
    /// derivation — never an admin RPC).
    fn domains(&self) -> &[String] {
        self.snapshot
            .as_ref()
            .map(|s| s.local_domains.as_slice())
            .unwrap_or(&[])
    }

    /// Open the sheet in **add** mode, seeded with the first derived domain.
    pub(super) fn open_add_form(&mut self) {
        let domain = self.domains().first().cloned().unwrap_or_default();
        self.reset_form();
        self.show_add_form = true;
        self.domain_input = domain;
    }

    /// Open the sheet in **edit** mode, seeded from the row.
    pub(super) fn open_edit_form(&mut self, view: &ListView) {
        self.reset_form();
        self.show_add_form = true;
        self.editing = Some(view.list_id_hex.clone());
        self.name_input = view.friendly_name.clone();
        self.local_part_input = view.local_part.clone();
        self.domain_input = view.local_domain.clone();
        self.description_input = view.description.clone();
        self.list_help_url_input = view.list_help_url.clone();
        self.list_archive_url_input = view.list_archive_url.clone();
        self.per_send_cap_input = view
            .recipients_per_send
            .map(|c| c.to_string())
            .unwrap_or_default();
    }

    /// Drop every local draft + armed confirm. Called on the nav edge so a
    /// half-typed list or an armed delete never survives a nav-away.
    pub(super) fn reset_form(&mut self) {
        self.show_add_form = false;
        self.editing = None;
        self.name_input.clear();
        self.local_part_input.clear();
        self.domain_input.clear();
        self.description_input.clear();
        self.list_help_url_input.clear();
        self.list_archive_url_input.clear();
        self.per_send_cap_input.clear();
        self.delete_armed = None;
        self.archive_confirm_armed = None;
    }

    /// The List-Archive URL a submit must confirm first, or `None` when it saves
    /// on the first press — the shared machine's decision, fed the user's own
    /// domains and (when editing) the URL the list already stores.
    pub(super) fn archive_needing_confirm(&self) -> Option<String> {
        let url = self.list_archive_url_input.trim().to_string();
        let saved = self
            .editing
            .as_deref()
            .and_then(|id| self.list(id))
            .map(|v| v.list_archive_url.clone());
        archive_url_needs_confirm(url.clone(), saved, self.domains().to_vec()).then_some(url)
    }

    /// Whether the sheet's Submit is armed for the URL it currently holds.
    pub(super) fn archive_confirm_is_armed(&self) -> bool {
        self.archive_confirm_armed.is_some()
            && self.archive_confirm_armed == self.archive_needing_confirm()
    }

    /// The draft the sheet's inputs currently describe.
    fn draft(&self) -> ListDraft {
        ListDraft {
            friendly_name: self.name_input.trim().to_string(),
            local_part: self.local_part_input.trim().to_string(),
            local_domain: self.domain_input.trim().to_string(),
            description: self.description_input.trim().to_string(),
            list_help_url: self.list_help_url_input.trim().to_string(),
            list_archive_url: self.list_archive_url_input.trim().to_string(),
            // An unparseable cap is `None` — the nest owns the ceiling. Via the
            // shared mail-knob validator (`value-formatting.md` § Mail-knob
            // validation), whose rule this had duplicated byte-for-byte; the
            // `u32` parse rejects a negative on its own, so unlike this page's
            // alias-sheet twin there was no live bug behind the duplication.
            recipients_per_send: fauna_core::format::parse_count(&self.per_send_cap_input),
        }
    }

    /// The action a submit dispatches, or `None` when the sheet cannot yet make
    /// a legal request. Create needs both halves of the posting address; edit
    /// keeps the address it already has, so it needs neither.
    pub(super) fn submit_action(&self) -> Option<MailListsAction> {
        let draft = self.draft();
        match &self.editing {
            Some(list_id_hex) => Some(MailListsAction::Update {
                list_id_hex: list_id_hex.clone(),
                draft,
            }),
            None => {
                if draft.local_part.is_empty() || draft.local_domain.is_empty() {
                    return None;
                }
                Some(MailListsAction::Create { draft })
            }
        }
    }
}

/// Paint the `mail-lists` page.
pub(super) fn mail_lists_elements(state: &SettingsState) -> Vec<Element> {
    let l = &state.mail_lists;
    let snapshot = l.snapshot.as_ref();
    // Creating a list needs a domain to create it on. The machine derives the
    // options from rows the user already owns; with mail not enabled there are
    // none and the nest would reject, so the affordance is disabled and the page
    // says why (linux's `has_domain`).
    let domains = l.domains();
    let has_domain = !domains.is_empty();

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        Element::chrome(t::DESCRIPTION),
        Element::gesture_button(
            ids::MAIL_LISTS_ADD_BUTTON,
            t::ADD_BUTTON,
            has_domain,
            Gesture::Settings(Action::MailListsOpenAdd),
        ),
    ];
    // The honest reason the add affordance is dead, once the snapshot has
    // resolved and carries no error of its own to show instead.
    if snapshot.is_some() && !has_domain {
        els.push(Element::chrome(t::NO_DOMAIN));
    }

    // ── the add/edit sheet ──
    if l.show_add_form {
        let editing = l.editing.is_some();
        els.push(Element::chrome(if editing {
            t::EDIT
        } else {
            t::FORM_TITLE
        }));
        els.push(
            Element::input(
                ids::MAIL_LISTS_ADD_SHEET_NAME_INPUT,
                l.name_input.clone(),
                Field::Settings(SettingsField::MailListName),
            )
            .labelled(t::NAME_PLACEHOLDER),
        );
        // The posting address is immutable once the list exists (the nest treats
        // it like an alias `kind`), so both halves are inert in edit mode —
        // present for ui.yaml conformance, matching how the aliases sheet keeps
        // its kind picker read-only while editing.
        els.push(
            Element::input(
                ids::MAIL_LISTS_ADD_SHEET_LOCAL_PART_INPUT,
                l.local_part_input.clone(),
                Field::Settings(SettingsField::MailListLocalPart),
            )
            .labelled(t::LOCAL_PART_PLACEHOLDER),
        );
        els.push(Element::select(
            ids::MAIL_LISTS_ADD_SHEET_DOMAIN_PICKER,
            l.domain_input.clone(),
            SelectTarget::MailListDomain,
            domains.to_vec(),
        ));
        els.push(
            Element::input(
                ids::MAIL_LISTS_ADD_SHEET_DESCRIPTION_INPUT,
                l.description_input.clone(),
                Field::Settings(SettingsField::MailListDescription),
            )
            .labelled(t::DESCRIPTION_PLACEHOLDER),
        );
        els.push(
            Element::input(
                ids::MAIL_LISTS_ADD_SHEET_LIST_HELP_URL_INPUT,
                l.list_help_url_input.clone(),
                Field::Settings(SettingsField::MailListHelpUrl),
            )
            .labelled(t::LIST_HELP_PLACEHOLDER),
        );
        els.push(
            Element::input(
                ids::MAIL_LISTS_ADD_SHEET_LIST_ARCHIVE_URL_INPUT,
                l.list_archive_url_input.clone(),
                Field::Settings(SettingsField::MailListArchiveUrl),
            )
            .labelled(t::LIST_ARCHIVE_PLACEHOLDER),
        );
        els.push(
            Element::input(
                ids::MAIL_LISTS_ADD_SHEET_PER_SEND_CAP_INPUT,
                l.per_send_cap_input.clone(),
                Field::Settings(SettingsField::MailListPerSendCap),
            )
            .labelled(t::PER_SEND_PLACEHOLDER),
        );
        // The off-server archive confirm relabels Submit in place — the two-click
        // confirm's only visible affordance (`common.md` § Two-click confirm).
        els.push(Element::gesture_button(
            ids::MAIL_LISTS_ADD_SHEET_SUBMIT_BUTTON,
            if l.archive_confirm_is_armed() {
                t::ARCHIVE_OFF_SERVER_CONFIRM
            } else {
                t::SUBMIT
            },
            true,
            Gesture::Settings(Action::MailListsSubmit { editing }),
        ));
        els.push(Element::gesture_button(
            ids::MAIL_LISTS_ADD_SHEET_CANCEL_BUTTON,
            t::CANCEL,
            true,
            Gesture::Settings(Action::MailListsCancelForm),
        ));
    }

    // ── the list ──
    // Pre-hydrate the page does not know whether there are lists, so it must
    // not claim "No lists yet" — and the loading line doubles as rule 5's
    // reason for the dead Add button above (`mail_aliases`'s reasoning).
    let lists: &[ListView] = snapshot.map(|s| s.lists.as_slice()).unwrap_or(&[]);
    if snapshot.is_none() {
        els.push(Element::chrome(t::LOADING));
    } else if lists.is_empty() {
        els.push(Element::chrome(t::EMPTY));
    }
    for (i, view) in lists.iter().enumerate() {
        els.extend(list_row_elements(l, i, view));
    }

    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// One list row, painted FLAT-indexed — see the module docs for why (the shared
/// action file reads these with a plain `click(id, index)`).
///
/// Every control paints on every row, so the row's id families share one index
/// space: `names()[i]` and `delete_list(i)` address the same list. Every
/// control's *dispatch* carries the row's `list_id_hex`, never an index, so a
/// list that re-orders under a fresh snapshot can still never delete the wrong
/// list in production.
fn list_row_elements(l: &MailListsState, _i: usize, view: &ListView) -> Vec<Element> {
    let id = &view.list_id_hex;
    let armed = l.delete_armed.as_deref() == Some(id.as_str());
    vec![
        Element::label(ids::MAIL_LISTS_LIST_ITEM, view.friendly_name.clone()),
        // Name + posting address, the pair `mail-mass-mailing.md` § Layout wants
        // ("Bob's Weekly — bob-weekly@<our-domain>").
        Element::label(
            ids::MAIL_LISTS_LIST_ITEM_NAME,
            format!("{} — {}", view.friendly_name, view.address),
        ),
        Element::label(
            ids::MAIL_LISTS_LIST_ITEM_MEMBER_COUNT,
            view.member_count.to_string(),
        ),
        // `None` = never sent. An empty string rather than a fabricated date —
        // the same honesty the projection keeps for the optional URLs. The plain
        // `%Y-%m-%d` local date the rest of the tui renders (a terminal has no
        // locale-aware date widget), the `mail_aliases::hits_text` split.
        Element::label(
            ids::MAIL_LISTS_LIST_ITEM_LAST_SEND,
            view.last_send_at_ms
                .filter(|ms| *ms > 0)
                .map(|ms| crate::format::epoch_secs_date((ms / 1000).max(0) as u64))
                .unwrap_or_default(),
        ),
        // The user's own quota meter: sends today / recipients today.
        Element::label(
            ids::MAIL_LISTS_LIST_ITEM_QUOTA,
            format!("{}/{}", view.sends_today, view.recipients_today),
        ),
        Element::gesture_button(
            ids::MAIL_LISTS_LIST_ITEM_EDIT_BUTTON,
            t::EDIT,
            true,
            Gesture::Settings(Action::MailListsOpenEdit(id.clone())),
        ),
        Element::gesture_button(
            ids::MAIL_LISTS_LIST_ITEM_MEMBERS_BUTTON,
            t::MEMBERS,
            true,
            Gesture::Settings(Action::MailListsOpenMembers(id.clone())),
        ),
        Element::gesture_button(
            ids::MAIL_LISTS_LIST_ITEM_DELETE_BUTTON,
            // The armed state relabels in place — the two-click confirm's only
            // visible affordance, since ui.yaml scopes no confirm id here — and
            // says the cascade out loud: the members go with the list
            // (`mail-mass-mailing.md` § The list as an alias row).
            if armed { t::DELETE_CONFIRM } else { t::DELETE },
            true,
            Gesture::Settings(Action::MailListsDelete(id.clone())),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(id: &str, name: &str, local_part: &str) -> ListView {
        ListView::from_parts(
            &hex::decode(id).unwrap(),
            name,
            local_part,
            "example.com",
            "",
            3,
            None,
            0,
            0,
            "",
            "",
            None,
        )
    }

    fn state_with(lists: Vec<ListView>, domains: Vec<String>) -> SettingsState {
        let mut s = SettingsState::default();
        s.mail_lists.snapshot = Some(MailListsSnapshot {
            lists,
            local_domains: domains,
            status: fauna_client_mail_settings::ListsStatus::Idle,
            error: None,
        });
        s
    }

    #[test]
    fn the_page_paints_its_static_ui_yaml_ids() {
        let state = state_with(vec![], vec!["example.com".into()]);
        let ids: Vec<String> = mail_lists_elements(&state)
            .into_iter()
            .map(|e| e.id)
            .collect();
        assert!(ids.iter().any(|i| i == "page-heading"));
        assert!(ids.iter().any(|i| i == "mail-lists-add-button"));
    }

    #[test]
    fn the_add_sheet_paints_every_ui_yaml_field() {
        let mut state = state_with(vec![], vec!["example.com".into()]);
        state.mail_lists.open_add_form();
        let ids: Vec<String> = mail_lists_elements(&state)
            .into_iter()
            .map(|e| e.id)
            .collect();
        for id in [
            "mail-lists-add-sheet-name-input",
            "mail-lists-add-sheet-local-part-input",
            "mail-lists-add-sheet-domain-picker",
            "mail-lists-add-sheet-description-input",
            "mail-lists-add-sheet-list-help-url-input",
            "mail-lists-add-sheet-list-archive-url-input",
            "mail-lists-add-sheet-per-send-cap-input",
            "mail-lists-add-sheet-submit-button",
            "mail-lists-add-sheet-cancel-button",
        ] {
            assert!(ids.iter().any(|i| i == id), "add sheet is missing {id}");
        }
    }

    #[test]
    fn opening_the_add_sheet_seeds_the_first_derived_domain() {
        // Without this the picker paints empty and a submit would send an empty
        // `local_domain` the nest rejects — with nothing on screen explaining it.
        let mut state = state_with(vec![], vec!["example.com".into(), "second.example".into()]);
        state.mail_lists.open_add_form();
        assert_eq!(state.mail_lists.domain_input, "example.com");
    }

    #[test]
    fn the_add_affordance_is_dead_with_no_domain_and_says_why() {
        let state = state_with(vec![], vec![]);
        let els = mail_lists_elements(&state);
        let add = els
            .iter()
            .find(|e| e.id == "mail-lists-add-button")
            .expect("add button paints");
        assert!(!add.enabled, "no domain ⇒ nothing legal to create");
        assert!(
            els.iter().any(|e| e.text == t::NO_DOMAIN),
            "the page must say why the affordance is dead"
        );
    }

    /// The pre-hydrate half of the rule-5 pair above: same dead button, other
    /// reason, and no "No lists yet" claim the page cannot yet stand behind.
    #[test]
    fn the_un_hydrated_page_says_it_is_loading_instead_of_claiming_no_lists() {
        let els = mail_lists_elements(&SettingsState::default());
        let add = els
            .iter()
            .find(|e| e.id == "mail-lists-add-button")
            .expect("add button paints");
        assert!(!add.enabled, "pre-hydrate there is no domain to create on");
        assert!(
            els.iter().any(|e| e.text == t::LOADING),
            "an un-hydrated page must say why the affordance is dead"
        );
        assert!(
            !els.iter().any(|e| e.text == t::EMPTY),
            "an un-hydrated page must NOT claim the user has no lists"
        );
    }

    #[test]
    fn every_row_paints_every_control_so_the_index_spaces_align() {
        // The hazard this pins: if a control were conditionally omitted, the
        // control's occurrence-index would compress relative to the row's name
        // index and `delete_list(i)` would hit the row *after* the intended one.
        let state = state_with(
            vec![
                view(&"a1".repeat(16), "First", "first"),
                view(&"b2".repeat(16), "Second", "second"),
            ],
            vec!["example.com".into()],
        );
        let els = mail_lists_elements(&state);
        for id in [
            "mail-lists-list-item",
            "mail-lists-list-item-name",
            "mail-lists-list-item-member-count",
            "mail-lists-list-item-last-send",
            "mail-lists-list-item-quota",
            "mail-lists-list-item-edit-button",
            "mail-lists-list-item-members-button",
            "mail-lists-list-item-delete-button",
        ] {
            assert_eq!(
                els.iter().filter(|e| e.id == id).count(),
                2,
                "{id} must paint once per row so every family shares one index space"
            );
        }
    }

    #[test]
    fn row_leaves_are_flat_not_scoped() {
        // `actions/mail_lists.py` reads rows with a plain `click(id, index)`, so
        // declaring containment would make every scoped read resolve to nothing
        // while the page painted perfectly.
        let state = state_with(
            vec![view(&"a1".repeat(16), "First", "first")],
            vec!["example.com".into()],
        );
        for e in mail_lists_elements(&state) {
            if e.id.starts_with("mail-lists-list-item") {
                assert!(
                    e.path.is_empty(),
                    "{} must paint flat — the shared action file indexes it directly",
                    e.id
                );
            }
        }
    }

    #[test]
    fn the_row_name_carries_both_the_friendly_name_and_the_posting_address() {
        let state = state_with(
            vec![view(&"a1".repeat(16), "Bob's Weekly", "bob-weekly")],
            vec!["example.com".into()],
        );
        let name = mail_lists_elements(&state)
            .into_iter()
            .find(|e| e.id == "mail-lists-list-item-name")
            .expect("row paints");
        assert!(name.text.contains("Bob's Weekly"));
        assert!(name.text.contains("bob-weekly@example.com"));
    }

    #[test]
    fn a_never_sent_list_paints_an_empty_last_send_not_a_fabricated_date() {
        let state = state_with(
            vec![view(&"a1".repeat(16), "First", "first")],
            vec!["example.com".into()],
        );
        let last = mail_lists_elements(&state)
            .into_iter()
            .find(|e| e.id == "mail-lists-list-item-last-send")
            .expect("row paints");
        assert_eq!(last.text, "");
    }

    #[test]
    fn delete_takes_two_clicks_and_arming_relabels_in_place() {
        let mut state = state_with(
            vec![view(&"a1".repeat(16), "First", "first")],
            vec!["example.com".into()],
        );
        let unarmed = mail_lists_elements(&state)
            .into_iter()
            .find(|e| e.id == "mail-lists-list-item-delete-button")
            .expect("row paints");
        assert_eq!(unarmed.text, t::DELETE);

        state.mail_lists.delete_armed = Some("a1".repeat(16));
        let armed = mail_lists_elements(&state)
            .into_iter()
            .find(|e| e.id == "mail-lists-list-item-delete-button")
            .expect("row paints");
        assert_eq!(
            armed.text,
            t::DELETE_CONFIRM,
            "the armed state must be visible and name the member cascade"
        );
    }

    #[test]
    fn create_needs_both_halves_of_the_posting_address() {
        let mut state = state_with(vec![], vec!["example.com".into()]);
        state.mail_lists.open_add_form();
        state.mail_lists.name_input = "Bob's Weekly".into();
        // Domain seeded, local-part still empty.
        assert!(state.mail_lists.submit_action().is_none());
        state.mail_lists.local_part_input = "bob-weekly".into();
        assert!(matches!(
            state.mail_lists.submit_action(),
            Some(MailListsAction::Create { .. })
        ));
    }

    #[test]
    fn editing_submits_an_update_keyed_by_the_rows_id_and_needs_no_address() {
        let v = view(&"a1".repeat(16), "First", "first");
        let mut state = state_with(vec![v.clone()], vec!["example.com".into()]);
        state.mail_lists.open_edit_form(&v);
        state.mail_lists.local_part_input.clear();
        state.mail_lists.domain_input.clear();
        match state.mail_lists.submit_action() {
            Some(MailListsAction::Update { list_id_hex, .. }) => {
                assert_eq!(list_id_hex, "a1".repeat(16));
            }
            other => panic!("expected an Update, got {other:?}"),
        }
    }

    #[test]
    fn the_edit_sheet_is_seeded_from_the_row() {
        let v = ListView::from_parts(
            &hex::decode("a1".repeat(16)).unwrap(),
            "Bob's Weekly",
            "bob-weekly",
            "example.com",
            "A newsletter",
            3,
            None,
            0,
            0,
            "https://example.com/help",
            "",
            Some(2500),
        );
        let mut state = state_with(vec![v.clone()], vec!["example.com".into()]);
        state.mail_lists.open_edit_form(&v);
        let l = &state.mail_lists;
        assert_eq!(l.name_input, "Bob's Weekly");
        assert_eq!(l.local_part_input, "bob-weekly");
        assert_eq!(l.domain_input, "example.com");
        assert_eq!(l.description_input, "A newsletter");
        assert_eq!(l.list_help_url_input, "https://example.com/help");
        assert_eq!(l.per_send_cap_input, "2500");
        // An absent cap must seed an empty input, not "0" — 0 would be a real
        // (and absurd) cap the nest would then enforce.
        assert_eq!(l.list_archive_url_input, "");
    }

    #[test]
    fn an_unparseable_per_send_cap_submits_as_absent() {
        let mut state = state_with(vec![], vec!["example.com".into()]);
        state.mail_lists.open_add_form();
        state.mail_lists.local_part_input = "bob-weekly".into();
        state.mail_lists.per_send_cap_input = "lots".into();
        match state.mail_lists.submit_action() {
            Some(MailListsAction::Create { draft }) => {
                assert_eq!(draft.recipients_per_send, None)
            }
            other => panic!("expected a Create, got {other:?}"),
        }
    }

    #[test]
    fn the_nav_edge_reset_drops_drafts_and_the_armed_delete() {
        let mut state = state_with(
            vec![view(&"a1".repeat(16), "First", "first")],
            vec!["example.com".into()],
        );
        state.mail_lists.open_add_form();
        state.mail_lists.name_input = "half typed".into();
        state.mail_lists.delete_armed = Some("a1".repeat(16));
        state.mail_lists.reset_form();
        assert!(!state.mail_lists.show_add_form);
        assert!(state.mail_lists.name_input.is_empty());
        assert!(
            state.mail_lists.delete_armed.is_none(),
            "an armed delete must never survive a nav-away"
        );
    }

    fn submit_label(state: &SettingsState) -> String {
        mail_lists_elements(state)
            .into_iter()
            .find(|e| e.id == ids::MAIL_LISTS_ADD_SHEET_SUBMIT_BUTTON)
            .expect("submit button")
            .text
    }

    #[test]
    fn an_off_server_archive_link_relabels_submit_only_while_armed_for_it() {
        let mut state = state_with(vec![], vec!["example.com".into()]);
        state.mail_lists.open_add_form();
        state.mail_lists.local_part_input = "news".into();
        state.mail_lists.list_archive_url_input = "https://archive.example.net/news".into();
        assert_eq!(
            state.mail_lists.archive_needing_confirm().as_deref(),
            Some("https://archive.example.net/news")
        );
        assert_eq!(submit_label(&state), t::SUBMIT, "unarmed until pressed");
        state.mail_lists.archive_confirm_armed = Some("https://archive.example.net/news".into());
        assert_eq!(submit_label(&state), t::ARCHIVE_OFF_SERVER_CONFIRM);
        // Editing the link after arming disarms: the arm was for the old link.
        state.mail_lists.list_archive_url_input = "https://other.example.net/news".into();
        assert_eq!(submit_label(&state), t::SUBMIT);
        // A link on the user's own server never asks.
        state.mail_lists.list_archive_url_input = "https://example.com/news".into();
        assert!(state.mail_lists.archive_needing_confirm().is_none());
    }

    #[test]
    fn re_saving_the_off_server_link_a_list_already_stores_does_not_ask_again() {
        let v = ListView::from_parts(
            &hex::decode("a1".repeat(16)).unwrap(),
            "News",
            "news",
            "example.com",
            "",
            3,
            None,
            0,
            0,
            "",
            "https://archive.example.net/news",
            None,
        );
        let mut state = state_with(vec![v.clone()], vec!["example.com".into()]);
        state.mail_lists.open_edit_form(&v);
        assert!(state.mail_lists.archive_needing_confirm().is_none());
        state.mail_lists.list_archive_url_input = "https://elsewhere.example.net/news".into();
        assert!(state.mail_lists.archive_needing_confirm().is_some());
    }
}
