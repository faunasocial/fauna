//! Stage 4d: choose providers (`onboarding-provisioning.md` § 3d).
//!
//! ui.yaml `onboarding.provider_choice`: the goal lines
//! `provider-goal-row[domain|dns|vps]` (with `provider-goal-select[...]` only
//! when more than one connected provider can cover the goal), the provider
//! rows `provider-row[<provider-id>|manual]`, `dns-buy-domain-checkbox` and
//! `dns-no-provider-message` (moved from `dns_config`), the status line and
//! Back / Continue — and, while a provider row is open, its `provider-dialog`
//! (link, open-in-browser, help, `provider-credentials-form-{field}`,
//! Connect / Disconnect, the registrar note or contact form, a status line,
//! Close).
//!
//! **Every rule is the machine's** (§ 3d *Where logic lives*):
//!
//! | Question | Shared getter |
//! |---|---|
//! | Which goals, covered by whom, at what price? | `goal_rows()` |
//! | What can each provider do, is it connected, why can't it cover a goal? | `provider_rows()` |
//! | Offer the buy box? | `can_offer_buy_domain()` |
//! | Which fields does a dialog show? | `visible_provider_fields(id)` |
//! | Can Connect be pressed, and if not why? | `connect_provider_possibility(id)` |
//! | Registrar note / contact form? | `provider_registrar_notes_key(id)` / `provider_shows_contact_form(id)` |
//! | Why is Continue dead? | `providers_continue_blocked_reason()` |
//!
//! Which dialog is open is the one piece of state this page keeps
//! (`Wizard::provider_dialog`): it is presentation, like the mail page's
//! armed disable dialog, and decides nothing about coverage.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::dns_config as dns_t;
use fauna_i18n::strings::onboarding::provider_choice as t;
use fauna_onboarding_machine::{
    CapabilityPlain, FieldTypePlain, GoalCover, GoalRow, ProviderGoal, ProviderRow,
};
use fauna_provisioning::providers_generated::PROVIDERS;
use fauna_ui_ids as ids;

use super::{
    Action, ContactField, Element, Field, SelectTarget, Wizard, WizardField, key, localized,
};

/// The manual row's key in `provider-row[...]` and `provider-goal-select[dns]`.
pub const MANUAL: &str = "manual";

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::DESCRIPTION.to_string()]
}

/// A provider's display name, or the id for one the registry does not know.
pub fn provider_name(id: &str) -> String {
    if id == MANUAL {
        return t::MANUAL_ROW.to_string();
    }
    PROVIDERS
        .iter()
        .find(|p| p.id.as_str() == id)
        .map(|p| key(p.display_name_key))
        .unwrap_or_else(|| id.to_string())
}

fn goal_key(goal: ProviderGoal) -> &'static str {
    match goal {
        ProviderGoal::Domain => "domain",
        ProviderGoal::Dns => "dns",
        ProviderGoal::Vps => "vps",
    }
}

fn cover_key(cover: &GoalCover) -> String {
    match cover {
        GoalCover::Provider { provider_id } => provider_id.clone(),
        GoalCover::Manual => MANUAL.to_string(),
    }
}

/// The handle's domain, as the goal lines name it.
fn domain(w: &Wizard) -> String {
    w.machine.provider_goal_domain()
}

fn goal_line(w: &Wizard, row: &GoalRow) -> String {
    let d = domain(w);
    let name = match row.goal {
        ProviderGoal::Domain => t::goal_domain(&d),
        ProviderGoal::Dns => t::goal_dns(&d),
        ProviderGoal::Vps => t::GOAL_VPS.to_string(),
    };
    let status = match &row.cover {
        Some(c) => t::goal_covered_by(&provider_name(&cover_key(c))),
        None => t::GOAL_OPEN.to_string(),
    };
    let mut line = format!("{name} — {status}");
    if let Some(price) = row.price_cents {
        let cur = row.currency.clone().unwrap_or_else(|| "USD".to_string());
        let first = fauna_onboarding_machine::format_price(price, cur.clone());
        let priced = match row.renewal_cents {
            Some(r) => t::goal_price(&first, &fauna_onboarding_machine::format_price(r, cur)),
            None => t::goal_price_first_year(&first),
        };
        line.push_str(" · ");
        line.push_str(&priced);
    }
    line
}

/// What a capability lets the provider do, in the row's words. `Zone` is a
/// field kind only, never a provider capability, so it names nothing.
fn capability_label(c: &CapabilityPlain) -> Option<&'static str> {
    match c {
        CapabilityPlain::Registrar => Some(t::CAPABILITY_REGISTRAR),
        CapabilityPlain::Dns => Some(t::CAPABILITY_DNS),
        CapabilityPlain::Vps => Some(t::CAPABILITY_VPS),
        CapabilityPlain::Zone => None,
    }
}

/// A provider row's text: its name, what it can do, whether it is connected.
fn row_text(row: &ProviderRow) -> String {
    let caps: Vec<&str> = row
        .capabilities
        .iter()
        .filter_map(capability_label)
        .collect();
    let state = if row.connecting {
        t::STATE_CONNECTING
    } else if row.connected {
        t::STATE_CONNECTED
    } else {
        t::STATE_AVAILABLE
    };
    format!(
        "{} — {} · {state}",
        provider_name(&row.provider_id),
        caps.join(", ")
    )
}

/// A reason or note from the can-cover computation. `resolve_nested`, not
/// `localized`: its `{provider}` argument is the provider's display-name key.
fn reason_text(text: &fauna_core::localized::LocalizedText) -> String {
    text.resolve_nested(fauna_i18n::strings::lookup)
}

/// The un-id'd line beneath a row: why it cannot cover the goals it cannot
/// (rule 5), from the one can-cover computation. A capability the provider
/// lacks for a goal it was never offered for reads the same everywhere, so a
/// reason repeated across goals is said once.
fn reason_line(row: &ProviderRow) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for r in [&row.domain_reason, &row.dns_reason, &row.vps_reason]
        .into_iter()
        .flatten()
    {
        let text = reason_text(r);
        if !seen.contains(&text) {
            seen.push(text);
        }
    }
    (!seen.is_empty()).then(|| seen.join(" "))
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let m = &w.machine;
    let mut out = Vec::new();

    // The goals at the top, in page order.
    let goals = m.goal_rows();
    out.push(Element::label(ids::PROVIDER_GOAL_ROW, ""));
    for row in &goals {
        let g = goal_key(row.goal);
        let mut el = Element::label(format!("provider-goal-row[{g}]"), goal_line(w, row))
            .attr(
                "state",
                if row.cover.is_some() {
                    "covered"
                } else {
                    "open"
                },
            )
            .within(ids::PROVIDER_GOAL_ROW, 0);
        if let Some(c) = &row.cover {
            el = el.attr("provider", cover_key(c));
        }
        out.push(el);
        // The goal's un-id'd note — DNS covered at a provider whose zone the
        // domain does not point at yet (§ 3d, *A zone the domain does not
        // point at*).
        if let Some(note) = &row.note {
            out.push(Element::chrome(reason_text(note)).within(ids::PROVIDER_GOAL_ROW, 0));
        }
        // The choice, only where there is one to make.
        if row.candidates.len() > 1 {
            let selected = row.cover.as_ref().map(cover_key).unwrap_or_default();
            out.push(
                Element::select(
                    format!("provider-goal-select[{g}]"),
                    selected.clone(),
                    SelectTarget::ProviderGoalCover(row.goal),
                    row.candidates.clone(),
                )
                .display_value(provider_name(&selected))
                .labelled(t::GOAL_SELECT)
                .within(ids::PROVIDER_GOAL_ROW, 0),
            );
        }
    }

    // The buy box, offered only for a domain that can be bought.
    if m.can_offer_buy_domain() {
        let buying = m.dns_config().buy_domain;
        out.push(Element::checkbox(
            ids::DNS_BUY_DOMAIN_CHECKBOX,
            dns_t::BUY_DOMAIN_CHECKBOX,
            buying,
            Action::ToggleBuyDomain(!buying),
        ));
    }
    if m.should_show_no_provider_message() {
        let tld = fauna_onboarding_machine::handle_tld(m.current_handle()).unwrap_or_default();
        out.push(Element::label(
            ids::DNS_NO_PROVIDER_MESSAGE,
            dns_t::no_provider_carries_tld(&tld),
        ));
    }

    // The providers, then the manual row.
    let dns_cover = goals
        .iter()
        .find(|g| g.goal == ProviderGoal::Dns)
        .and_then(|g| g.cover.clone());
    out.push(Element::label(ids::PROVIDER_ROW, t::PROVIDERS_HEADING));
    for row in m.provider_rows() {
        let id = row.provider_id.clone();
        let manual = id == MANUAL;
        let (text, connected, action) = if manual {
            let chosen = dns_cover == Some(GoalCover::Manual);
            (
                format!("{} — {}", t::MANUAL_ROW, t::MANUAL_ROW_HELP),
                chosen,
                Action::SelectGoalCover(ProviderGoal::Dns, GoalCover::Manual),
            )
        } else {
            (
                row_text(&row),
                row.connected,
                Action::OpenProviderDialog(id.clone()),
            )
        };
        let mut el = Element::button(format!("provider-row[{id}]"), text, true, action)
            .attr("state", if connected { "connected" } else { "available" })
            .within(ids::PROVIDER_ROW, 0);
        if manual {
            // The manual row covers DNS only; the pick's answer is the
            // machine's (`ui/README.md` § Offer only what is possible, rule
            // 6), the same one `select_goal_cover` asks first.
            el = el.possible(m.goal_cover_possibility(ProviderGoal::Dns, GoalCover::Manual));
        }
        out.push(el);
        if row.held {
            out.push(Element::chrome(t::HELD_CREDENTIAL).within(ids::PROVIDER_ROW, 0));
        }
        // The manual row's server and registration reasons restate what the
        // row is (DNS only); only its DNS reason explains anything.
        let reason = if manual {
            row.dns_reason.as_ref().map(reason_text)
        } else {
            reason_line(&row)
        };
        if let Some(reason) = reason {
            out.push(Element::chrome(reason).within(ids::PROVIDER_ROW, 0));
        }
        // The open dialog paints directly under its own row.
        if w.provider_dialog.as_deref() == Some(id.as_str()) {
            out.extend(dialog(w, &row));
        }
    }

    // Nested args: `status_contact_missing`'s `{provider}` is the registrar's
    // display-name key, so the status line and the seam's reason name it.
    let answer = m
        .providers_continue_possibility()
        .resolving_nested_args(fauna_i18n::strings::lookup);
    out.push(
        Element::label(
            ids::PROVIDER_CHOICE_STATUS_TEXT,
            answer.reason().map(localized).unwrap_or_default(),
        )
        .attr("explains-unset", ""),
    );
    out.push(Element::button(
        ids::PROVIDER_CHOICE_BACK_BUTTON,
        common::BACK,
        true,
        Action::Back,
    ));
    // Enabled is the seam's to decide (`ui/README.md` § Offer only what is
    // possible, rule 6): the button declares the machine's answer.
    out.push(
        Element::button(
            ids::PROVIDER_CHOICE_CONTINUE_BUTTON,
            common::CONTINUE,
            true,
            Action::ContinueFromProviders,
        )
        .possible(answer),
    );
    out
}

/// The label for one contact field — the page-scoped key set `dns_config`'s
/// form already uses (§ 3d: the form's rules unchanged, only its place).
fn contact_label(f: ContactField) -> &'static str {
    use fauna_i18n::strings::onboarding::dns_config::contact_fields as c;
    match f {
        ContactField::FirstName => c::FIRST_NAME,
        ContactField::LastName => c::LAST_NAME,
        ContactField::Email => c::EMAIL,
        ContactField::Phone => c::PHONE,
        ContactField::Address1 => c::ADDRESS1,
        ContactField::City => c::CITY,
        ContactField::State => c::STATE,
        ContactField::PostalCode => c::POSTAL_CODE,
        ContactField::Country => c::COUNTRY,
    }
}

/// One provider's dialog — the sign-in pop-up, painted under its row.
fn dialog(w: &Wizard, row: &ProviderRow) -> Vec<Element> {
    let m = &w.machine;
    let id = row.provider_id.clone();
    let within = |e: Element| e.within(ids::PROVIDER_DIALOG, 0);
    let mut out = vec![Element::label(ids::PROVIDER_DIALOG, provider_name(&id))];
    let Some(p) = PROVIDERS.iter().find(|p| p.id.as_str() == id) else {
        return out;
    };
    out.push(within(Element::label(
        ids::PROVIDER_DIALOG_LINK,
        p.signup_url,
    )));
    out.push(within(Element::button(
        ids::PROVIDER_DIALOG_OPEN_BROWSER_BUTTON,
        dns_t::OPEN_IN_BROWSER,
        true,
        Action::OpenSignupUrl(p.signup_url.to_string()),
    )));
    out.push(within(Element::label(
        ids::PROVIDER_DIALOG_HELP_TEXT,
        key(p.help_key),
    )));
    if let Some(notes) = m.provider_registrar_notes_key(id.clone()) {
        out.push(within(Element::label(
            ids::DNS_REGISTRAR_NOTES_TEXT,
            key(&notes),
        )));
    }

    out.push(within(Element::label(ids::PROVIDER_CREDENTIALS_FORM, "")));
    for f in m.visible_provider_fields(id.clone()) {
        let element_id = format!("provider-credentials-form-{}", f.id);
        if f.field_type == FieldTypePlain::HostedAuth {
            out.push(
                Element::button(
                    element_id,
                    m.provider_hosted_auth_button_text(id.clone(), f.id.clone()),
                    true,
                    Action::ProviderHostedAuth(id.clone(), f.id.clone()),
                )
                .possible(m.provider_hosted_auth_possibility(id.clone(), f.id.clone()))
                .labelled(key(&f.label_key))
                .within(ids::PROVIDER_CREDENTIALS_FORM, 0),
            );
            continue;
        }
        let field = WizardField::ProviderCred(id.clone(), f.id.clone());
        out.push(
            Element::input(element_id, w.field(field.clone()), Field::Wizard(field))
                .labelled(key(&f.label_key))
                .within(ids::PROVIDER_CREDENTIALS_FORM, 0),
        );
    }

    if row.connected {
        out.push(within(Element::button(
            ids::PROVIDER_DISCONNECT_BUTTON,
            t::DISCONNECT,
            true,
            Action::DisconnectProvider(id.clone()),
        )));
    } else {
        out.push(within(
            Element::button(
                ids::PROVIDER_CONNECT_BUTTON,
                t::CONNECT,
                true,
                Action::ConnectProvider(id.clone()),
            )
            .possible(m.connect_provider_possibility(id.clone())),
        ));
    }

    // The WHOIS contact form of a registrar that requires one.
    if m.provider_shows_contact_form(id.clone()) {
        out.push(within(Element::label(
            ids::DNS_CONTACT_FORM,
            dns_t::CONTACT_FORM_HEADING,
        )));
        for f in ContactField::ALL {
            let field = WizardField::ProviderContact(f);
            out.push(
                Element::input(f.id(), w.field(field.clone()), Field::Wizard(field))
                    .labelled(contact_label(f))
                    .within(ids::DNS_CONTACT_FORM, 0),
            );
        }
    }

    let status = if row.connecting {
        t::DIALOG_CONNECTING.to_string()
    } else if let Some(e) = &row.error {
        e.clone()
    } else if row.connected {
        t::DIALOG_CONNECTED.to_string()
    } else {
        String::new()
    };
    out.push(within(Element::label(
        ids::PROVIDER_DIALOG_STATUS_TEXT,
        status,
    )));
    out.push(within(Element::button(
        ids::PROVIDER_DIALOG_CLOSE_BUTTON,
        t::CLOSE,
        true,
        Action::CloseProviderDialog,
    )));
    out
}

#[cfg(test)]
mod tests {
    use fauna_onboarding_machine::{OnboardingStep, ProviderConnection};

    use crate::element::Element;

    fn find<'a>(els: &'a [Element], id: &str) -> Option<&'a Element> {
        els.iter().find(|e| e.id == id)
    }

    fn page(app: &crate::app::App) -> Vec<Element> {
        app.page_elements()
    }

    /// At entry nothing is connected: the goal lines read open, every
    /// registry provider has a row plus the manual row, and the dead Continue
    /// says which goal is still open (rule 5).
    #[test]
    fn at_entry_the_goals_are_open_and_continue_says_why() {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_current_handle("alice@example.com".into());
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::ProviderChoice);
        let els = page(&app);
        for g in ["dns", "vps"] {
            let row = find(&els, &format!("provider-goal-row[{g}]")).expect(g);
            assert!(row.attrs.iter().any(|(k, v)| k == "state" && v == "open"));
        }
        assert!(
            find(&els, "provider-goal-row[domain]").is_none(),
            "registration is a goal only while a domain is bought"
        );
        assert!(find(&els, "provider-row[hetzner]").is_some());
        assert!(find(&els, "provider-row[manual]").is_some());
        let cont = find(&els, "provider-choice-continue-button").unwrap();
        assert!(!cont.enabled);
        let status = find(&els, "provider-choice-status-text").unwrap();
        assert!(!status.text.is_empty(), "a dead Continue owes a reason");
        assert!(find(&els, "provider-dialog").is_none(), "no dialog is open");
    }

    /// One connected provider that runs DNS for the domain and rents servers
    /// covers both goals, and Continue goes live.
    #[test]
    fn one_provider_covering_both_goals_makes_continue_live() {
        let app = crate::app::tests::test_app();
        let m = &app.wizard.machine;
        m.set_current_handle("alice@example.com".into());
        m.set_step_for_test(OnboardingStep::ProviderChoice);
        m.set_provider_connection_for_test(
            "hetzner".into(),
            ProviderConnection {
                connected: true,
                server_checked: true,
                zones: vec![fauna_provisioning::dns::DnsZone {
                    id: "z1".into(),
                    name: "example.com".into(),
                    nameservers: vec![],
                }],
                ..Default::default()
            },
        );
        let els = page(&app);
        for g in ["dns", "vps"] {
            let row = find(&els, &format!("provider-goal-row[{g}]")).expect(g);
            assert!(
                row.attrs
                    .iter()
                    .any(|(k, v)| k == "provider" && v == "hetzner"),
                "{g} is covered by hetzner"
            );
        }
        let hetzner = find(&els, "provider-row[hetzner]").unwrap();
        assert!(
            hetzner
                .attrs
                .iter()
                .any(|(k, v)| k == "state" && v == "connected")
        );
        assert!(
            find(&els, "provider-choice-continue-button")
                .unwrap()
                .enabled
        );
    }

    /// Pressing a provider row opens its dialog under it, with Connect dead
    /// until the required fields are typed; Close shuts it.
    #[test]
    fn a_row_opens_its_dialog_and_close_shuts_it() {
        let mut app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_current_handle("alice@example.com".into());
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::ProviderChoice);
        let _ = app
            .wizard
            .prepare(&super::Action::OpenProviderDialog("hetzner".into()));
        let els = page(&app);
        assert!(find(&els, "provider-dialog").is_some());
        let connect = find(&els, "provider-connect-button").expect("connect");
        assert!(!connect.enabled, "nothing typed yet");
        assert!(
            els.iter()
                .any(|e| e.id.starts_with("provider-credentials-form-")),
            "the dialog shows the provider's fields"
        );
        let _ = app.wizard.prepare(&super::Action::CloseProviderDialog);
        assert!(find(&page(&app), "provider-dialog").is_none());
    }
}
