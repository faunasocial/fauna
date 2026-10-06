//! The Settings → Mail → List members sub-page
//! (`docs/goal/behavior/mail-mass-mailing.md` § `mail-list-members` page;
//! `tests/e2e-unified/ui.yaml` `mail-list-members` page + its
//! `mail-list-members-list` component).
//!
//! A person manages the members of **one** of their mailing lists here: the
//! subscribed/unsubscribed summary, adding a member, bulk-importing addresses,
//! and per-member unsubscribe / resubscribe.
//!
//! **A dumb renderer over the shared machine.** Every decision is
//! `fauna_client_mail_settings::MailListMembersMachine`'s: the wire→view
//! projection (`project_member_row` — the status is *derived* from
//! `unsubscribed_at`, the wire carries no status field), the line-splitting of a
//! pasted import, the refresh-after-mutate sequencing. The status label itself is
//! the shared `member_status_label`, so tui reads the same two-arm i18n map as
//! the six apps before it. Direct Rust, no FFI hop.
//!
//! **The machine is per-list, so it is rebuilt whenever the selection changes.**
//! Unlike every sibling settings page (whose machine is built once at
//! `attach_session`), `build_mail_list_members_machine` takes the `list_id_hex`
//! it is scoped to — a members machine is only ever valid for one list. The nav
//! edge therefore rebuilds it, which is also what makes the page correct after a
//! list is deleted out from under a stale selection.
//!
//! **How the page is entered, and why it has to work with no selection.**
//! `settings.md` § Navigation model gives `mail-list-members` its **own** rail
//! entry, and `actions/mail_list_members.py::navigate` reaches it with a bare nav
//! patch carrying no list id. So an entry with nothing selected must still land
//! somewhere honest: it selects the user's first list, or — with no lists at all
//! — paints an empty state and no machine, rather than hydrating against a
//! fabricated id. (linux instead wires its page to an all-zero
//! `PLACEHOLDER_LIST_ID_HEX` and leaves its View-members button with no click
//! handler at all — "inert in the embedded settings seed" — so tui is *setting*
//! this prior art rather than porting it.)
//!
//! **Row leaves are painted FLAT-indexed**, and **every row paints both the
//! unsubscribe and the resubscribe control** (enabled by status, never omitted).
//! Both choices come from the shared action file:
//! `actions/mail_list_members.py` indexes rows with a plain `click(id, index)`,
//! and it indexes `unsubscribe(i)` / `resubscribe(i)` against the address list —
//! which is only correct while both families paint once per row. Omitting the
//! inapplicable control would compress one family's index space against the
//! other and silently act on the wrong member. linux paints both and toggles
//! sensitivity for exactly this reason.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client_mail_settings::{
    MailListMembersMachine, MailListMembersSnapshot, MemberStatus, MemberView, member_status_label,
};
use fauna_i18n::strings::mail_lists as t;

use super::{Action, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture};

/// The List-members sub-page's state.
///
/// Session-scoped like every sibling — `clear_session` drops it, so a stale
/// machine never outlives a sign-out.
#[derive(Default)]
pub struct MailListMembersState {
    /// The machine for [`Self::selected`]. Rebuilt on the nav edge whenever the
    /// selection changes (it is scoped to one `list_id`); `None` when the user
    /// has no lists to select.
    pub machine: Option<Arc<MailListMembersMachine>>,
    /// The `list_id_hex` this page is scoped to. Set by the `mail-lists` row's
    /// View-members button, or defaulted to the first list on a bare nav.
    pub selected: Option<String>,
    /// The last snapshot the page painted.
    pub snapshot: Option<MailListMembersSnapshot>,

    // ── the add-member sheet (an inline reveal, not a modal) ──
    pub show_add_form: bool,
    /// `mail-list-members-add-sheet-address-input`.
    pub address_input: String,

    // ── the bulk paste-import sheet ──
    /// Whether the import sheet is open. Mutually exclusive with the add sheet
    /// (linux's `open_import_sheet` hides the other).
    pub show_import_form: bool,
    /// `mail-list-members-import-sheet-input` — one address per line.
    pub import_input: String,
    /// Whether a submit has happened since the sheet was last opened, gating the
    /// import tally. Cleared when the sheet reopens so a stale batch's summary
    /// never greets the next paste — the `mail_aliases` convention (the machine
    /// clears `last_import` only on the *next dispatch*, and reopening is not
    /// one, so this local flag is what makes the reopen honest).
    pub import_result_shown: bool,
}

impl MailListMembersState {
    /// Point the page at `list_id_hex` and build its machine. A no-op when the
    /// selection is already current, so a repeat nav does not churn the machine
    /// (and does not drop the snapshot it already painted).
    ///
    /// Returns whether a rebuild happened.
    pub(super) fn select(
        &mut self,
        nest: Arc<fauna_client::NestClient>,
        list_id_hex: &str,
        list_name: &str,
    ) -> bool {
        if self.selected.as_deref() == Some(list_id_hex) && self.machine.is_some() {
            return false;
        }
        match fauna_client_mail_settings::rpc_glue::build_mail_list_members_machine(
            nest,
            list_id_hex.to_string(),
            list_name.to_string(),
        ) {
            Ok(m) => {
                self.machine = Some(Arc::new(m));
                self.selected = Some(list_id_hex.to_string());
                // The previous list's members must not linger under the new
                // heading while the hydrate is in flight.
                self.snapshot = None;
                true
            }
            Err(_) => {
                // A malformed id is the only failure mode. Drop the selection
                // rather than keep a machine that cannot answer.
                self.machine = None;
                self.selected = None;
                self.snapshot = None;
                false
            }
        }
    }

    /// Drop the selection entirely (no lists to show).
    pub(super) fn clear_selection(&mut self) {
        self.machine = None;
        self.selected = None;
        self.snapshot = None;
    }

    pub(super) fn open_add_form(&mut self) {
        self.reset_form();
        self.show_add_form = true;
    }

    pub(super) fn open_import_form(&mut self) {
        self.reset_form();
        self.show_import_form = true;
    }

    /// Drop every local draft. Called on the nav edge so a half-typed address or
    /// a stale import tally never survives a nav-away.
    pub(super) fn reset_form(&mut self) {
        self.show_add_form = false;
        self.show_import_form = false;
        self.address_input.clear();
        self.import_input.clear();
        self.import_result_shown = false;
    }

    /// The addresses a pasted import describes — one per line, blanks dropped.
    pub(super) fn import_addresses(&self) -> Vec<String> {
        self.import_input
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }
}

/// Paint the `mail-list-members` page.
pub(super) fn mail_list_members_elements(state: &SettingsState) -> Vec<Element> {
    let m = &state.mail_list_members;
    let snapshot = m.snapshot.as_ref();
    // With no list selected there is nothing to add a member *to*, so the
    // affordances are dead and the page says why rather than offering controls
    // whose every dispatch would be dropped.
    let has_list = m.machine.is_some();

    let heading = snapshot
        .map(|s| s.list_name.clone())
        .filter(|n| !n.trim().is_empty())
        .map(|n| format!("{n} · {}", t::MEMBERS_TITLE))
        .unwrap_or_else(|| t::MEMBERS_TITLE.to_string());

    let mut els = vec![
        Element::label(ids::PAGE_HEADING, heading),
        // The subscribed / unsubscribed counts. Painted from the snapshot's own
        // counts, which the nest computes over the whole list — never from the
        // rendered row count, which the page could be paginating.
        Element::label(
            ids::MAIL_LIST_MEMBERS_SUMMARY,
            match snapshot {
                Some(s) => t::summary_fmt(
                    &s.subscribed_count.to_string(),
                    &s.unsubscribed_count.to_string(),
                ),
                None => t::summary_fmt("0", "0"),
            },
        ),
        Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_ADD_BUTTON,
            t::ADD_MEMBER_BUTTON,
            has_list,
            Gesture::Settings(Action::MailListMembersOpenAdd),
        ),
        Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_IMPORT_BUTTON,
            t::IMPORT_BUTTON,
            has_list,
            Gesture::Settings(Action::MailListMembersOpenImport),
        ),
    ];
    // Rule 5's reason for the two dead affordances above. This used to borrow
    // the *Lists* page's `EMPTY` ("No lists yet"), which under a "Members"
    // heading reads as "this list has no members" — a present-but-wrong reason
    // is worse than a blank one, so each of the two real states says its own
    // thing: no list is open, versus the list's members are still arriving.
    if !has_list {
        els.push(Element::chrome(t::MEMBERS_NO_LIST));
    } else if snapshot.is_none() {
        els.push(Element::chrome(t::MEMBERS_LOADING));
    }

    // ── the add-member sheet ──
    if m.show_add_form {
        els.push(
            Element::input(
                ids::MAIL_LIST_MEMBERS_ADD_SHEET_ADDRESS_INPUT,
                m.address_input.clone(),
                Field::Settings(SettingsField::MailListMemberAddress),
            )
            .labelled(t::ADD_MEMBER_PLACEHOLDER),
        );
        els.push(Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_ADD_SHEET_SUBMIT_BUTTON,
            t::ADD_MEMBER_SUBMIT,
            true,
            Gesture::Settings(Action::MailListMembersAddSubmit),
        ));
        els.push(Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_ADD_SHEET_CANCEL_BUTTON,
            t::ADD_MEMBER_CANCEL,
            true,
            Gesture::Settings(Action::MailListMembersCancelForm),
        ));
    }

    // ── the bulk paste-import sheet ──
    if m.show_import_form {
        els.push(
            Element::input(
                ids::MAIL_LIST_MEMBERS_IMPORT_SHEET_INPUT,
                m.import_input.clone(),
                Field::Settings(SettingsField::MailListMemberImport),
            )
            .labelled(t::IMPORT_PLACEHOLDER),
        );
        els.push(Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_IMPORT_SHEET_SUBMIT_BUTTON,
            t::IMPORT_SUBMIT,
            true,
            Gesture::Settings(Action::MailListMembersImportSubmit),
        ));
        els.push(Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_IMPORT_SHEET_CANCEL_BUTTON,
            t::IMPORT_CANCEL,
            true,
            Gesture::Settings(Action::MailListMembersCancelForm),
        ));
        // The sheet stays open on submit and renders the outcome in place; the
        // local flag keeps a previous batch's summary from greeting a fresh
        // paste. `mail-list-members-import-result` — the aliases page's
        // `mail-aliases-import-result` twin, counts only (the wire reply
        // carries no per-line reasons yet).
        if m.import_result_shown
            && let Some(tally) = snapshot.and_then(|s| s.last_import.as_ref())
        {
            els.push(Element::label(
                ids::MAIL_LIST_MEMBERS_IMPORT_RESULT,
                t::import_result(
                    &tally.added.to_string(),
                    &tally.skipped_duplicate.to_string(),
                    &tally.skipped_invalid.to_string(),
                ),
            ));
        }
    }

    // ── the member list ──
    let members: &[MemberView] = snapshot.map(|s| s.members.as_slice()).unwrap_or(&[]);
    for view in members {
        els.extend(member_row_elements(view));
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

/// One member row, painted FLAT-indexed — see the module docs.
///
/// Both mutating controls paint on **every** row, enabled by status: the shared
/// action file indexes `unsubscribe(i)` / `resubscribe(i)` against the address
/// list, which is only correct while both families paint once per row. Each
/// control's *dispatch* carries the member's address, never an index.
fn member_row_elements(view: &MemberView) -> Vec<Element> {
    let subscribed = view.status == MemberStatus::Subscribed;
    let status = crate::wizard::localized(&member_status_label(view.status));
    vec![
        Element::label(ids::MAIL_LIST_MEMBERS_LIST_ITEM, view.address.clone()),
        Element::label(
            ids::MAIL_LIST_MEMBERS_LIST_ITEM_ADDRESS,
            view.address.clone(),
        ),
        Element::label(
            ids::MAIL_LIST_MEMBERS_LIST_ITEM_SUBSCRIBED_AT,
            view.subscribed_at_ms
                .filter(|ms| *ms > 0)
                .map(|ms| crate::format::epoch_secs_date((ms / 1000).max(0) as u64))
                .unwrap_or_default(),
        ),
        Element::label(ids::MAIL_LIST_MEMBERS_LIST_ITEM_STATUS, status),
        Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_LIST_ITEM_UNSUBSCRIBE_BUTTON,
            t::UNSUBSCRIBE,
            subscribed,
            Gesture::Settings(Action::MailListMembersUnsubscribe(view.address.clone())),
        ),
        Element::gesture_button(
            ids::MAIL_LIST_MEMBERS_LIST_ITEM_RESUBSCRIBE_BUTTON,
            t::RESUBSCRIBE,
            !subscribed,
            Gesture::Settings(Action::MailListMembersResubscribe(view.address.clone())),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(address: &str, status: MemberStatus) -> MemberView {
        MemberView {
            address: address.into(),
            subscribed_at_ms: Some(1_700_000_000_000),
            status,
        }
    }

    fn state_with(members: Vec<MemberView>) -> SettingsState {
        let mut s = SettingsState::default();
        s.mail_list_members.snapshot = Some(MailListMembersSnapshot {
            list_id_hex: "a1".repeat(16),
            list_name: "Bob's Weekly".into(),
            subscribed_count: members
                .iter()
                .filter(|m| m.status == MemberStatus::Subscribed)
                .count() as u32,
            unsubscribed_count: members
                .iter()
                .filter(|m| m.status == MemberStatus::Unsubscribed)
                .count() as u32,
            members,
            status: fauna_client_mail_settings::ListsStatus::Idle,
            error: None,
            last_import: None,
        });
        s
    }

    #[test]
    fn the_page_paints_its_static_ui_yaml_ids() {
        let state = state_with(vec![]);
        let ids: Vec<String> = mail_list_members_elements(&state)
            .into_iter()
            .map(|e| e.id)
            .collect();
        for id in [
            "page-heading",
            "mail-list-members-summary",
            "mail-list-members-add-button",
            "mail-list-members-import-button",
        ] {
            assert!(ids.iter().any(|i| i == id), "page is missing {id}");
        }
    }

    /// Rule 5 on a page whose reason used to be borrowed from another page.
    /// With no list open, both affordances are dead and the line must name
    /// *that* gate — not "No lists yet", which under a "Members" heading reads
    /// as a claim about this list's membership.
    #[test]
    fn with_no_list_open_the_page_names_that_gate_and_not_the_lists_empty_line() {
        let els = mail_list_members_elements(&SettingsState::default());
        for id in [
            "mail-list-members-add-button",
            "mail-list-members-import-button",
        ] {
            let el = els.iter().find(|e| e.id == id).expect("button paints");
            assert!(!el.enabled, "{id} must be disabled with no list open");
        }
        assert!(
            els.iter().any(|e| e.text == t::MEMBERS_NO_LIST),
            "the page must say why both affordances are dead"
        );
        assert!(
            !els.iter().any(|e| e.text == t::EMPTY),
            "the Lists page's empty line must not stand in as the reason here"
        );
    }

    #[test]
    fn both_sheets_paint_every_ui_yaml_field() {
        let mut state = state_with(vec![]);
        state.mail_list_members.open_add_form();
        let ids: Vec<String> = mail_list_members_elements(&state)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        for id in [
            "mail-list-members-add-sheet-address-input",
            "mail-list-members-add-sheet-submit-button",
            "mail-list-members-add-sheet-cancel-button",
        ] {
            assert!(ids.iter().any(|i| i == id), "add sheet is missing {id}");
        }

        state.mail_list_members.open_import_form();
        let ids: Vec<String> = mail_list_members_elements(&state)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        for id in [
            "mail-list-members-import-sheet-input",
            "mail-list-members-import-sheet-submit-button",
            "mail-list-members-import-sheet-cancel-button",
        ] {
            assert!(ids.iter().any(|i| i == id), "import sheet is missing {id}");
        }
    }

    #[test]
    fn the_two_sheets_are_mutually_exclusive() {
        let mut state = state_with(vec![]);
        state.mail_list_members.open_add_form();
        state.mail_list_members.open_import_form();
        assert!(!state.mail_list_members.show_add_form);
        assert!(state.mail_list_members.show_import_form);
    }

    #[test]
    fn every_row_paints_both_controls_so_the_index_spaces_align() {
        // The hazard this pins: the shared action file indexes `unsubscribe(i)`
        // and `resubscribe(i)` against the ADDRESS list, so omitting the
        // inapplicable control on a row would compress one family's index space
        // and silently act on the wrong member.
        let state = state_with(vec![
            member("a@example.net", MemberStatus::Subscribed),
            member("b@example.net", MemberStatus::Unsubscribed),
            member("c@example.net", MemberStatus::Subscribed),
        ]);
        let els = mail_list_members_elements(&state);
        for id in [
            "mail-list-members-list-item",
            "mail-list-members-list-item-address",
            "mail-list-members-list-item-subscribed-at",
            "mail-list-members-list-item-status",
            "mail-list-members-list-item-unsubscribe-button",
            "mail-list-members-list-item-resubscribe-button",
        ] {
            assert_eq!(
                els.iter().filter(|e| e.id == id).count(),
                3,
                "{id} must paint once per row so every family shares one index space"
            );
        }
    }

    #[test]
    fn the_two_controls_are_enabled_by_status_not_omitted() {
        let state = state_with(vec![
            member("a@example.net", MemberStatus::Subscribed),
            member("b@example.net", MemberStatus::Unsubscribed),
        ]);
        let els = mail_list_members_elements(&state);
        let unsub: Vec<bool> = els
            .iter()
            .filter(|e| e.id == "mail-list-members-list-item-unsubscribe-button")
            .map(|e| e.enabled)
            .collect();
        let resub: Vec<bool> = els
            .iter()
            .filter(|e| e.id == "mail-list-members-list-item-resubscribe-button")
            .map(|e| e.enabled)
            .collect();
        // Row 0 is subscribed: unsubscribe live, resubscribe dead. Row 1 inverse.
        assert_eq!(unsub, vec![true, false]);
        assert_eq!(resub, vec![false, true]);
    }

    #[test]
    fn row_leaves_are_flat_not_scoped() {
        let state = state_with(vec![member("a@example.net", MemberStatus::Subscribed)]);
        for e in mail_list_members_elements(&state) {
            if e.id.starts_with("mail-list-members-list-item") {
                assert!(
                    e.path.is_empty(),
                    "{} must paint flat — the shared action file indexes it directly",
                    e.id
                );
            }
        }
    }

    #[test]
    fn the_status_label_comes_from_the_shared_resolver() {
        let state = state_with(vec![
            member("a@example.net", MemberStatus::Subscribed),
            member("b@example.net", MemberStatus::Unsubscribed),
        ]);
        let labels: Vec<String> = mail_list_members_elements(&state)
            .iter()
            .filter(|e| e.id == "mail-list-members-list-item-status")
            .map(|e| e.text.clone())
            .collect();
        assert_eq!(
            labels,
            vec![
                crate::wizard::localized(&member_status_label(MemberStatus::Subscribed)),
                crate::wizard::localized(&member_status_label(MemberStatus::Unsubscribed)),
            ]
        );
    }

    #[test]
    fn the_summary_reads_the_snapshot_counts_not_the_rendered_rows() {
        // The nest counts over the whole list; the page could be paginating, so
        // counting painted rows would under-report.
        let mut state = state_with(vec![member("a@example.net", MemberStatus::Subscribed)]);
        if let Some(s) = state.mail_list_members.snapshot.as_mut() {
            s.subscribed_count = 900;
            s.unsubscribed_count = 12;
        }
        let summary = mail_list_members_elements(&state)
            .into_iter()
            .find(|e| e.id == "mail-list-members-summary")
            .expect("summary paints");
        assert!(summary.text.contains("900"));
        assert!(summary.text.contains("12"));
    }

    #[test]
    fn the_heading_names_the_list_it_is_scoped_to() {
        let state = state_with(vec![]);
        let heading = mail_list_members_elements(&state)
            .into_iter()
            .find(|e| e.id == "page-heading")
            .expect("heading paints");
        assert!(
            heading.text.contains("Bob's Weekly"),
            "a per-list page must name its list, got {:?}",
            heading.text
        );
    }

    #[test]
    fn with_no_list_selected_the_affordances_are_dead() {
        // The bare-nav case (the rail entry / the e2e's `navigate()`), for a user
        // with no lists at all: offering controls whose every dispatch would be
        // dropped is the shape testing.md point 11 forbids.
        let state = SettingsState::default();
        let els = mail_list_members_elements(&state);
        for id in [
            "mail-list-members-add-button",
            "mail-list-members-import-button",
        ] {
            let el = els.iter().find(|e| e.id == id).expect("paints");
            assert!(!el.enabled, "{id} must be dead with no list selected");
        }
    }

    #[test]
    fn import_splits_lines_and_drops_blanks() {
        let mut state = SettingsState::default();
        state.mail_list_members.import_input =
            "  a@example.net \n\n b@example.net\n   \nc@example.net".into();
        assert_eq!(
            state.mail_list_members.import_addresses(),
            vec!["a@example.net", "b@example.net", "c@example.net"]
        );
    }

    #[test]
    fn the_nav_edge_reset_drops_drafts_and_the_stale_import_tally() {
        let mut state = state_with(vec![]);
        state.mail_list_members.open_import_form();
        state.mail_list_members.import_input = "half@example.net".into();
        state.mail_list_members.import_result_shown = true;
        state.mail_list_members.reset_form();
        assert!(!state.mail_list_members.show_import_form);
        assert!(state.mail_list_members.import_input.is_empty());
        assert!(
            !state.mail_list_members.import_result_shown,
            "a previous batch's tally must not greet the next paste"
        );
    }

    #[test]
    fn the_import_tally_paints_under_its_id_with_the_shared_string() {
        let mut state = state_with(vec![]);
        state.mail_list_members.open_import_form();
        if let Some(snap) = state.mail_list_members.snapshot.as_mut() {
            snap.last_import = Some(fauna_client_mail_settings::ImportResult {
                added: 2,
                skipped_invalid: 3,
                skipped_duplicate: 1,
            });
        }
        let tally = |s: &SettingsState| {
            mail_list_members_elements(s)
                .into_iter()
                .find(|e| e.id == ids::MAIL_LIST_MEMBERS_IMPORT_RESULT)
        };
        assert!(
            tally(&state).is_none(),
            "no tally before this sheet's submit"
        );
        state.mail_list_members.import_result_shown = true;
        let el = tally(&state).expect("the tally paints after a submit");
        assert_eq!(el.text, t::import_result("2", "1", "3"));
    }

    #[test]
    fn clearing_the_selection_drops_the_snapshot_too() {
        // Otherwise the previous list's members would linger under a heading
        // that no longer names them.
        let mut state = state_with(vec![member("a@example.net", MemberStatus::Subscribed)]);
        state.mail_list_members.selected = Some("a1".repeat(16));
        state.mail_list_members.clear_selection();
        assert!(state.mail_list_members.snapshot.is_none());
        assert!(state.mail_list_members.selected.is_none());
    }
}
