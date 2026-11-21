//! The admin Aliases sub-page (`admin-aliases`) — the admin external-forwarders
//! surface (`admin.md` § 4 / `mail-aliases.md` § Kind 7): an address on a hosted
//! local domain forwarding to an external destination with no local mailbox.
//!
//! A dumb renderer of the shared `ForwarderMachine`
//! (`fauna-client-mail-settings::forwarders`): the add form's submit dispatches
//! `fauna.bridges.create_forwarder`, each row's delete dispatches
//! `delete_forwarder`, both re-read via `list_forwarders` + `list_local_domains`
//! (the picker options) — the shell (`super`) owns the machine, ops and folds;
//! this file is paint only.
//!
//! Errors route to the **page-scoped** `admin-aliases-action-error` label (painted
//! here from the snapshot's `error`), NOT the app-wide `error-message` (`admin.md`
//! § 4). The forwarder rows are FLAT indexed leaf ids (one `-row-address` /
//! `-row-target` / `-row-delete-button` per forwarder, in order) — the
//! notifications flat-sibling convention the dashboard's stat cards also use, read
//! by `get_text(id, index=i)` / `count(id)`.

use fauna_i18n::strings::admin as t;
use fauna_ui_ids as ids;

use super::{Action, AdminField, AdminState};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

pub(super) fn aliases_elements(state: &AdminState) -> Vec<Element> {
    let snap = state.forwarders_snapshot.as_ref();
    let local_domains: Vec<String> = snap.map(|s| s.local_domains.clone()).unwrap_or_default();
    let local_domains_all = local_domains.clone();

    let mut els = vec![
        Element::label(ids::ADMIN_ALIASES_HEADING, t::aliases_page::TITLE),
        Element::gesture_button(
            ids::ADMIN_NAV_BACK,
            t::EXIT,
            true,
            Gesture::Nav(Page::Conversations),
        )
        .nav_back(),
    ];

    // Page-scoped action error — painted ONLY when the machine's snapshot carries
    // one (the empty-doesn't-register discipline `ui::register_frame` relies on),
    // so `is_visible("admin-aliases-action-error")` is false on a clean page.
    if let Some(err) = snap
        .and_then(|s| s.error.as_deref())
        .filter(|e| !e.is_empty())
    {
        els.push(Element::label(ids::ADMIN_ALIASES_ACTION_ERROR, err));
    }

    // The forwarders section: the add form (domain picker + local-part + target +
    // submit) then the indexed list of existing forwarders (ui.yaml add-form-first
    // order). The `-section` view has no terminal chrome, so it paints as a heading
    // label (the `admin-factory-reset-section` shape).
    els.push(Element::label(
        ids::ADMIN_ALIASES_FORWARDERS_SECTION,
        t::aliases_page::FORWARDERS_TITLE,
    ));
    els.push(Element::chrome(t::aliases_page::FORWARDERS_DESC));
    els.push(
        Element::select(
            ids::ADMIN_ALIASES_FORWARDER_ADD_DOMAIN_SELECT,
            state.forwarder_add_domain.clone(),
            SelectTarget::ForwarderDomain,
            local_domains,
        )
        // Un-prompted this paints a bare `<  >` above the add form, answering
        // nothing (copy-audit corpus, 2026-08-04) — the same `Domain` label
        // linux's add-form picker carries.
        .labelled(t::aliases_page::FORWARDER_DOMAIN),
    );
    els.push(
        Element::input(
            ids::ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT,
            state.forwarder_add_pattern.clone(),
            Field::Admin(AdminField::ForwarderPattern),
        )
        .labelled(t::aliases_page::FORWARDER_LOCAL_PART),
    );
    els.push(
        Element::input(
            ids::ADMIN_ALIASES_FORWARDER_ADD_TARGET_INPUT,
            state.forwarder_add_target.clone(),
            Field::Admin(AdminField::ForwarderTarget),
        )
        .labelled(t::aliases_page::FORWARDER_TARGET),
    );
    els.push(Element::gesture_button(
        ids::ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON,
        t::aliases_page::CREATE_FORWARDER,
        true,
        Gesture::Admin(Action::CreateForwarder),
    ));
    // The one step that unblocks the form (`admin.md` § 4, *A forwarder needs
    // a hosted domain*; `ui/README.md` § Offer only what is possible, rule 3):
    // once the read resolved with no hosted domain, the jump to the DNS page
    // where one is added. The form's own reason names it.
    if snap.is_some()
        && !fauna_client_mail_settings::forwarders::forwarder_form_possibility(&local_domains_all)
            .is_possible()
    {
        els.push(Element::gesture_button(
            ids::ADMIN_ALIASES_FORWARDER_ADD_DOMAINS_STEP,
            t::aliases_page::OPEN_DOMAINS,
            true,
            Gesture::Admin(Action::Open(super::AdminPage::Dns)),
        ));
    }

    // The indexed forwarder rows — one address/target/delete triple per forwarder,
    // in registration (= visual) order. A delete carries the row's own hex alias id
    // so the gesture never re-derives it from a positional guess.
    for fw in snap.map(|s| s.forwarders.as_slice()).unwrap_or_default() {
        els.push(Element::label(
            ids::ADMIN_ALIASES_FORWARDER_ROW_ADDRESS,
            fw.address.clone(),
        ));
        els.push(Element::label(
            ids::ADMIN_ALIASES_FORWARDER_ROW_TARGET,
            fw.forward_target.clone(),
        ));
        els.push(Element::gesture_button(
            ids::ADMIN_ALIASES_FORWARDER_ROW_DELETE_BUTTON,
            t::aliases_page::DELETE_FORWARDER,
            true,
            Gesture::Admin(Action::DeleteForwarder {
                alias_id_hex: fw.alias_id_hex.clone(),
            }),
        ));
    }

    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_mail_settings::forwarders::{
        ForwarderStatus, ForwarderView, ForwardersSnapshot,
    };

    fn forwarder(pattern: &str, domain: &str, target: &str, id_hex: &str) -> ForwarderView {
        ForwarderView {
            alias_id_hex: id_hex.to_string(),
            local_domain: domain.to_string(),
            pattern: pattern.to_string(),
            address: format!("{pattern}@{domain}"),
            forward_target: target.to_string(),
        }
    }

    fn snapshot(
        forwarders: Vec<ForwarderView>,
        local_domains: &[&str],
        error: Option<&str>,
    ) -> ForwardersSnapshot {
        ForwardersSnapshot {
            forwarders,
            local_domains: local_domains.iter().map(|d| d.to_string()).collect(),
            status: ForwarderStatus::Idle,
            error: error.map(str::to_string),
        }
    }

    /// The page paints its ui.yaml element set in add-form-first order, then the
    /// indexed forwarder rows (address/target/delete per row). No action error →
    /// the `admin-aliases-action-error` label is absent (the empty-doesn't-register
    /// discipline).
    #[test]
    fn aliases_page_paints_addform_then_indexed_rows() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Aliases;
        app.admin.forwarders_snapshot = Some(snapshot(
            vec![
                forwarder(
                    "info",
                    "acme.test",
                    "real@elsewhere.test",
                    "00112233445566778899aabbccddeeff",
                ),
                forwarder(
                    "sales",
                    "acme.test",
                    "team@elsewhere.test",
                    "ffeeddccbbaa99887766554433221100",
                ),
            ],
            &["acme.test"],
            None,
        ));
        app.admin.forwarder_add_domain = "acme.test".to_string();

        let els = aliases_elements(&app.admin);
        let tagged: Vec<&str> = els
            .iter()
            .map(|e| e.id.as_str())
            .filter(|id| !id.is_empty())
            .collect();
        assert_eq!(
            tagged,
            vec![
                "admin-aliases-heading",
                "admin-nav-back",
                "admin-aliases-forwarders-section",
                "admin-aliases-forwarder-add-domain-select",
                "admin-aliases-forwarder-add-pattern-input",
                "admin-aliases-forwarder-add-target-input",
                "admin-aliases-forwarder-add-submit-button",
                // Row 0
                "admin-aliases-forwarder-row-address",
                "admin-aliases-forwarder-row-target",
                "admin-aliases-forwarder-row-delete-button",
                // Row 1
                "admin-aliases-forwarder-row-address",
                "admin-aliases-forwarder-row-target",
                "admin-aliases-forwarder-row-delete-button",
            ],
            "no action-error label on a clean snapshot; add-form precedes the rows"
        );

        // The first row's address/target read the projected `ForwarderView`.
        let addresses: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "admin-aliases-forwarder-row-address")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(addresses, vec!["info@acme.test", "sales@acme.test"]);
        let targets: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "admin-aliases-forwarder-row-target")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(targets, vec!["real@elsewhere.test", "team@elsewhere.test"]);

        // The domain picker offers the hosted domains and shows the current pick.
        let picker = els
            .iter()
            .find(|e| e.id == "admin-aliases-forwarder-add-domain-select")
            .expect("domain picker painted");
        assert_eq!(picker.text, "acme.test", "picker shows the picked domain");
    }

    /// A nest hosting `domains` and no forwarder — enough to resolve the read.
    struct DomainsNest(Vec<String>);

    #[async_trait::async_trait]
    impl fauna_client_mail_settings::forwarders::ForwarderNest for DomainsNest {
        async fn list_forwarders(
            &self,
        ) -> Result<
            Vec<fauna_protocol::bridge_routing::AliasRow>,
            fauna_client_mail_settings::NestError,
        > {
            Ok(vec![])
        }
        async fn list_local_domains(
            &self,
        ) -> Result<Vec<String>, fauna_client_mail_settings::NestError> {
            Ok(self.0.clone())
        }
        async fn create_forwarder(
            &self,
            _: String,
            _: String,
            _: String,
        ) -> Result<(), fauna_client_mail_settings::NestError> {
            panic!("a create the page could not offer reached the nest")
        }
        async fn delete_forwarder(
            &self,
            _: Vec<u8>,
        ) -> Result<(), fauna_client_mail_settings::NestError> {
            Ok(())
        }
    }

    fn app_with_domains(domains: &[&str]) -> crate::app::App {
        let machine = fauna_client_mail_settings::forwarders::ForwarderMachine::new(
            std::sync::Arc::new(DomainsNest(domains.iter().map(|d| d.to_string()).collect())),
        );
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(machine.hydrate())
            .unwrap();
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Aliases;
        app.admin.forwarders_snapshot = Some(machine.snapshot());
        app.admin.forwarders = Some(std::sync::Arc::new(machine));
        app.admin.forwarder_add_domain = domains.first().map(|d| d.to_string()).unwrap_or_default();
        app
    }

    fn answer_of(app: &crate::app::App, id: &str) -> Option<fauna_core::possibility::Possibility> {
        super::super::elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| e.possibility)
    }

    /// `admin.md` § 4, *A forwarder needs a hosted domain*: with none hosted
    /// the submit (and the form) is blocked naming DNS, the jump there is
    /// offered, and a press never sends an empty `local_domain`.
    #[test]
    fn no_hosted_domain_blocks_the_form_and_offers_dns() {
        use fauna_client_mail_settings::forwarders::NO_HOSTED_DOMAIN;
        let mut app = app_with_domains(&[]);
        for id in [
            ids::ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON,
            ids::ADMIN_ALIASES_FORWARDER_ADD_DOMAIN_SELECT,
            ids::ADMIN_ALIASES_FORWARDER_ADD_PATTERN_INPUT,
        ] {
            let answer = answer_of(&app, id).unwrap_or_else(|| panic!("{id} answered"));
            assert_eq!(
                answer.reason().map(|r| r.key.as_str()),
                Some(NO_HOSTED_DOMAIN),
                "{id}"
            );
        }
        let step = super::super::elements(&app)
            .into_iter()
            .find(|e| e.id == ids::ADMIN_ALIASES_FORWARDER_ADD_DOMAINS_STEP)
            .expect("the Domains step is offered");
        assert!(matches!(
            step.gesture(),
            Some(Gesture::Admin(Action::Open(super::super::AdminPage::Dns)))
        ));
        // The press asks again and refuses on `error-message`, sending nothing.
        assert!(super::super::apply_local(&mut app, Action::CreateForwarder).is_none());
        assert!(app.errors.contains_key(&Page::Admin));
    }

    #[test]
    fn a_hosted_domain_unblocks_the_form_and_drops_the_step() {
        let mut app = app_with_domains(&["acme.test"]);
        // A complete draft, so the submit's draft half answers too
        // (`ForwarderMachine::create_draft_possibility`).
        app.admin.forwarder_add_pattern = "sales".to_string();
        app.admin.forwarder_add_target = "team@example.org".to_string();
        assert!(
            answer_of(&app, ids::ADMIN_ALIASES_FORWARDER_ADD_SUBMIT_BUTTON)
                .expect("submit answered")
                .is_possible()
        );
        assert!(
            super::super::elements(&app)
                .iter()
                .all(|e| e.id != ids::ADMIN_ALIASES_FORWARDER_ADD_DOMAINS_STEP)
        );
    }

    /// A snapshot carrying an error paints the page-scoped `admin-aliases-action-error`
    /// label (NOT the app-wide `error-message`); a clean snapshot drops it.
    #[test]
    fn action_error_label_tracks_snapshot_error() {
        let mut app = crate::app::tests::test_app();
        app.admin.sub = super::super::AdminPage::Aliases;

        app.admin.forwarders_snapshot = Some(snapshot(
            vec![],
            &["acme.test"],
            Some("forward target is a hosted domain"),
        ));
        let err = aliases_elements(&app.admin)
            .into_iter()
            .find(|e| e.id == "admin-aliases-action-error")
            .expect("action-error painted when the snapshot carries one");
        assert_eq!(err.text, "forward target is a hosted domain");
        // The app-wide banner is untouched — errors route to the page element.
        assert!(!app.errors.contains_key(&Page::Admin));

        app.admin.forwarders_snapshot = Some(snapshot(vec![], &["acme.test"], None));
        assert!(
            aliases_elements(&app.admin)
                .iter()
                .all(|e| e.id != "admin-aliases-action-error"),
            "clean snapshot registers no action-error"
        );
    }

    /// The snapshot fold seeds the domain picker to a real hosted domain when the
    /// current pick is empty or no longer hosted, and stores the snapshot without
    /// touching the app-wide error map (errors are page-scoped here).
    #[test]
    fn forwarders_snapshot_seeds_picker_and_stays_off_the_banner() {
        let mut app = crate::app::tests::test_app();
        // Empty pick → seeded to the first hosted domain.
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::ForwardersSnapshot(snapshot(
                vec![],
                &["one.test", "two.test"],
                Some("boom"),
            )),
        );
        assert_eq!(app.admin.forwarder_add_domain, "one.test");
        assert!(app.admin.forwarders_snapshot.is_some());
        assert!(
            !app.errors.contains_key(&Page::Admin),
            "a forwarder error never lands on the app-wide error-message"
        );

        // A human's pick that is still hosted survives a re-snapshot.
        app.admin.forwarder_add_domain = "two.test".to_string();
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::ForwardersSnapshot(snapshot(
                vec![],
                &["one.test", "two.test"],
                None,
            )),
        );
        assert_eq!(app.admin.forwarder_add_domain, "two.test");

        // A pick that is no longer hosted is re-seeded to the first hosted domain.
        super::super::apply_outcome(
            &mut app,
            super::super::Outcome::ForwardersSnapshot(snapshot(vec![], &["one.test"], None)),
        );
        assert_eq!(app.admin.forwarder_add_domain, "one.test");
    }
}
