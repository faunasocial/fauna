//! Stage 4: DNS configuration (`onboarding.md` § 4).
//!
//! ui.yaml `onboarding.dns_config` — required: `dns-buy-domain-checkbox`,
//! `dns-same-provider-checkbox`, `dns-provider-row` (indexed),
//! `dns-set-up-later-button`, `dns-provider-link`,
//! `dns-provider-open-browser-button`, `dns-provider-help-text`,
//! `dns-credentials-form`, `dns-verify-button`, `dns-status-text`,
//! `dns-tld-price-display`, `dns-config-back-button`,
//! `dns-config-continue-button`. Optional: `dns-price-confirm-checkbox`,
//! `dns-no-provider-message`, `dns-registrar-notes-text`, `dns-contact-form`
//! + its 9 `dns-contact-*-input` fields.
//!
//! **Every visibility rule on this page is a shared getter, never re-derived
//! here** (`onboarding.md` § 4 — "none of those rules is re-derived in the
//! per-app shell"):
//!
//! | Question | Shared getter |
//! |---|---|
//! | Which credential fields show? | `visible_dns_fields()` |
//! | Is this provider button eligible? | `dns_provider_eligible(id)` |
//! | And if not, **why** is it dead? | `dns_provider_ineligible_reason(id)` |
//! | Show the WHOIS contact form? | `should_show_contact_form()` |
//! | Show the registrar notes line? | `should_show_registrar_notes()` |
//! | Show the "no registrar carries .tld" line? | `should_show_no_provider_message()` |
//! | What does the status line say? | `dns_status_text_key()` |
//! | Can we verify / continue? | `can_verify_dns()` / `can_continue_dns()` |
//!
//! (Section anchors, not line numbers, on purpose: this header's former
//! `onboarding.md:NNN` citations had all rotted past their claims as the doc
//! grew, which is the failure mode a cold reader can't detect.)
//!
//! `buy_domain` is **machine-derived, not client-toggled** (`onboarding.md` § 4):
//! the machine seeds it from the handle-check outcome when it lands on this
//! page. The checkbox renders `dns_config().buy_domain` and forwards explicit
//! user overrides; it never computes the initial value.
//!
//! Two divergences from linux, both deliberate:
//!   * linux never builds `dns-no-provider-message` at all, though the machine
//!     has exposed `should_show_no_provider_message()` (tested) all along. tui
//!     renders it — copying the gap would just spread it to a 7th client.
//!   * the WHOIS labels resolve against the page-scoped
//!     `onboarding.dns_config.contact_fields.*` keys (the set web already uses,
//!     and the richer English: E.164 and ISO-3166 hints). linux's older
//!     `registrar.contact.*` keys are drift, re-pointed in this same commit.

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::dns_config as t;
use fauna_onboarding_machine::{CredentialForm, FieldTypePlain};
use fauna_provisioning::providers_generated::{Capability, PROVIDERS};
use fauna_ui_ids as ids;

use super::{Action, ContactField, Element, Field, Wizard, WizardField, key, localized};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![t::SET_UP_LATER_WARNING.to_string()]
}

/// The label for one contact field, from the page-scoped key set.
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

pub fn elements(w: &Wizard) -> Vec<Element> {
    let m = &w.machine;
    let cfg = m.dns_config();
    let selected = cfg.selected_provider_id.clone();

    let mut out = vec![
        Element::checkbox(
            ids::DNS_BUY_DOMAIN_CHECKBOX,
            t::BUY_DOMAIN_CHECKBOX,
            cfg.buy_domain,
            Action::ToggleBuyDomain(!cfg.buy_domain),
        ),
        Element::checkbox(
            ids::DNS_SAME_PROVIDER_CHECKBOX,
            t::SAME_PROVIDER_CHECKBOX,
            cfg.same_provider_for_vps,
            Action::ToggleSameProviderForVps(!cfg.same_provider_for_vps),
        ),
    ];

    // `dns-provider-row` is the *container*; each provider button is
    // `dns-provider-row[<provider_id>]` — the cross-app ID shape (the shared
    // tests click `dns-provider-row[cloudflare]` by that literal id, and read
    // its enabled state). The bracket is part of the element id here, not a
    // positional scope index.
    //
    // Iterating the generated providers table is not a re-derivation (it is
    // compile-time data) — but *eligibility* is a rule, and that comes from the
    // machine's `dns_provider_eligible`.
    out.push(Element::label(ids::DNS_PROVIDER_ROW, ""));
    for p in PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&Capability::Dns))
    {
        let id = p.id.as_str();
        let is_selected = selected.as_deref() == Some(id);
        // One provider is chosen, so the rows are a `Radio` group, not a
        // checkbox stack (`apps/tui.md` § Rendering → *Control vocabulary*,
        // rule 1). Zero marked before a pick is fine here: nothing is chosen
        // yet, and choosing is exactly what the page asks.
        let mut row = Element::radio_gesture(
            format!("dns-provider-row[{id}]"),
            key(p.display_name_key),
            is_selected,
            crate::element::Gesture::Wizard(Action::SelectDnsProvider(id.to_string())),
        )
        .attr("state", if is_selected { "on" } else { "off" })
        .within(ids::DNS_PROVIDER_ROW, 0);
        row.enabled = m.dns_provider_eligible(id.to_string());
        out.push(row);

        // A DIM row is the only thing a terminal can say on its own, and "this
        // is dead" is not a reason (`apps/tui.md` § Rendering → *Control
        // vocabulary* rule 3; `ui/README.md` § Copy comprehensibility rule 5).
        // The reason comes from the machine — the same place the verdict does,
        // so the two can never disagree and no shell re-derives the capability
        // rule (priority #2) — and paints as an un-id'd chrome line at the
        // row's own indent, i.e. directly beneath it: adjacency is how this
        // client expresses "explains the thing above" (the constrained-channel
        // principle). Un-id'd follows the rule-5 precedent
        // `devices.wizard.name_required` set on the folder wizard, and keeps
        // ui.yaml's element set unchanged.
        if let Some(reason) = m.dns_provider_ineligible_reason(id.to_string()) {
            out.push(Element::chrome(localized(&reason)).within(ids::DNS_PROVIDER_ROW, 0));
        }
    }

    // `should_show_no_provider_message()` is a TLD property known from the
    // handle-check probe alone, so it shows up-front — it does NOT wait for a
    // provider to be selected and verified (`onboarding.md` § 4).
    if m.should_show_no_provider_message() {
        let tld = fauna_onboarding_machine::handle_tld(m.current_handle()).unwrap_or_default();
        out.push(Element::label(
            ids::DNS_NO_PROVIDER_MESSAGE,
            t::no_provider_carries_tld(&tld),
        ));
    }

    if let Some(p) = selected
        .as_deref()
        .and_then(|id| PROVIDERS.iter().find(|p| p.id.as_str() == id))
    {
        out.push(Element::label(ids::DNS_PROVIDER_LINK, p.signup_url));
        out.push(Element::button(
            ids::DNS_PROVIDER_OPEN_BROWSER_BUTTON,
            t::OPEN_IN_BROWSER,
            true,
            Action::OpenSignupUrl(p.signup_url.to_string()),
        ));
        out.push(Element::label(ids::DNS_PROVIDER_HELP_TEXT, key(p.help_key)));

        if m.should_show_registrar_notes()
            && let Some(notes) = p.registrar_notes_key
        {
            out.push(Element::label(ids::DNS_REGISTRAR_NOTES_TEXT, key(notes)));
        }

        // `dns-credentials-form` is the container; each field is
        // `dns-credentials-form-{field.id}` (`onboarding.md` § Element IDs — the ID shape
        // every app uses, so a shared helper resolves the field globally).
        out.push(Element::label(ids::DNS_CREDENTIALS_FORM, ""));
        for f in m.visible_dns_fields() {
            let element_id = format!("dns-credentials-form-{}", f.id);
            // A `hosted-auth` field is a button, not an input — the bundled
            // provider's hosted sign-in (`onboarding.md` § 4). Same derived
            // id as every field; the label is the machine's sign-in state.
            if f.field_type == FieldTypePlain::HostedAuth {
                out.push(
                    super::hosted_auth_button(m, CredentialForm::Dns, element_id, &f.id)
                        .labelled(key(&f.label_key))
                        .within(ids::DNS_CREDENTIALS_FORM, 0),
                );
                continue;
            }
            out.push(
                Element::input(
                    element_id,
                    w.field(WizardField::DnsCred(f.id.clone())),
                    Field::Wizard(WizardField::DnsCred(f.id.clone())),
                )
                .labelled(key(&f.label_key))
                .within(ids::DNS_CREDENTIALS_FORM, 0),
            );
        }

        out.push(Element::button(
            ids::DNS_VERIFY_BUTTON,
            fauna_i18n::strings::provisioning::VERIFY_CREDENTIALS,
            m.can_verify_dns(),
            Action::VerifyDns,
        ));
    }

    // Also the provider group's `explains-unset` line (walk.rs I5): at entry no
    // provider row is marked, and this is the line that says why — nothing is
    // chosen yet, and choosing is what the page asks (`status_pick_provider`).
    // The rows carry no `group` attr, so the page's single undeclared group is
    // `""`. Since the contract resolves to the PAINTED text, the empty-key
    // regression this getter had in exactly that state would red I5 too.
    out.push(
        Element::label(ids::DNS_STATUS_TEXT, localized(&m.dns_status_text_key()))
            .attr("explains-unset", ""),
    );

    // The price display and its confirm checkbox only mean anything once the
    // registrar has quoted a price — i.e. `provider_status()` is the buyable
    // variant, which is the machine's own conclusion, not ours.
    if let fauna_onboarding_machine::ProviderStatus::UnregisteredBuyable {
        price_cents,
        currency,
    } = m.provider_status()
    {
        let price = fauna_onboarding_machine::format_price(
            price_cents,
            currency.unwrap_or_else(|| "USD".to_string()),
        );
        out.push(Element::label(ids::DNS_TLD_PRICE_DISPLAY, price));
        out.push(Element::checkbox(
            ids::DNS_PRICE_CONFIRM_CHECKBOX,
            fauna_i18n::strings::registrar::PRICE_CONFIRM,
            cfg.price_agreed,
            Action::ConfirmPrice,
        ));
    }

    // The WHOIS contact form — Gandi today (`requires_contact`); Porkbun uses
    // account-level contacts and surfaces the notes line instead.
    if m.should_show_contact_form() {
        out.push(Element::label(
            ids::DNS_CONTACT_FORM,
            t::CONTACT_FORM_HEADING,
        ));
        for f in ContactField::ALL {
            out.push(
                Element::input(
                    f.id(),
                    w.field(WizardField::Contact(f)),
                    Field::Wizard(WizardField::Contact(f)),
                )
                .labelled(contact_label(f))
                .within(ids::DNS_CONTACT_FORM, 0),
            );
        }
    }

    out.push(Element::button(
        ids::DNS_SET_UP_LATER_BUTTON,
        t::SET_UP_LATER,
        true,
        Action::DnsSetUpLater,
    ));
    out.push(Element::button(
        ids::DNS_CONFIG_BACK_BUTTON,
        common::BACK,
        true,
        Action::Back,
    ));
    out.push(Element::button(
        ids::DNS_CONFIG_CONTINUE_BUTTON,
        common::CONTINUE,
        m.can_continue_dns(),
        Action::ContinueFromDns,
    ));
    out
}

#[cfg(test)]
mod tests {
    use fauna_i18n::strings::onboarding::dns_config as t;
    use fauna_onboarding_machine::OnboardingStep;

    use crate::element::Element;

    /// Paint the DNS step with the two narrowing checkboxes in the given state,
    /// through the step router (so this proves the wired page, not a helper).
    fn paint(buy_domain: bool, same_provider_for_vps: bool) -> Vec<Element> {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::DnsConfig);
        app.wizard.machine.set_dns_state_for_test(|d| {
            d.buy_domain = buy_domain;
            d.same_provider_for_vps = same_provider_for_vps;
        });
        app.wizard.elements()
    }

    /// The row for `provider`, and the chrome line (if any) painted directly
    /// under it — adjacency is the terminal's channel for "this explains that"
    /// (`apps/tui.md` § Rendering → *the constrained-channel principle*).
    fn row_and_hint<'a>(els: &'a [Element], provider: &str) -> (&'a Element, Option<&'a str>) {
        let id = format!("dns-provider-row[{provider}]");
        let at = els.iter().position(|e| e.id == id).expect(&id);
        let hint = els
            .get(at + 1)
            .filter(|e| e.id.is_empty())
            .map(|e| e.text.as_str());
        (&els[at], hint)
    }

    /// A greyed-out provider row must say why on the same screen, and the
    /// reason must name the checkbox that re-opens it (`ui/README.md` § Copy
    /// comprehensibility rule 5 + Q4; `apps/tui.md` § Rendering → *Control
    /// vocabulary* rule 3 — DIM alone is not an explanation). A live corpus
    /// pass found these four rows dead and silent.
    #[test]
    fn a_disabled_provider_row_paints_its_reason_beneath_itself() {
        // Same-provider-for-VPS alone: the four registrars have no Vps.
        let els = paint(false, true);
        for registrar in ["cloudflare", "porkbun", "namecheap", "gandi"] {
            let (row, hint) = row_and_hint(&els, registrar);
            assert!(!row.enabled, "{registrar} has no Vps capability");
            assert_eq!(
                hint,
                Some(t::INELIGIBLE_NEEDS_VPS),
                "{registrar}'s dead row must explain itself"
            );
        }
        let (hetzner, hint) = row_and_hint(&els, "hetzner");
        assert!(hetzner.enabled, "hetzner has Vps");
        assert_eq!(hint, None, "an enabled row owes no explanation");

        // Buy-domain alone flips which rows are dead — and the reason with it.
        let els = paint(true, false);
        let (hetzner, hint) = row_and_hint(&els, "hetzner");
        assert!(!hetzner.enabled, "hetzner has no Registrar capability");
        assert_eq!(hint, Some(t::INELIGIBLE_NEEDS_REGISTRAR));
        let (cloudflare, hint) = row_and_hint(&els, "cloudflare");
        assert!(cloudflare.enabled, "cloudflare has Registrar");
        assert_eq!(hint, None);

        // Neither box: nothing is narrowed, so the list paints clean.
        let els = paint(false, false);
        for p in ["cloudflare", "porkbun", "namecheap", "gandi", "hetzner"] {
            let (row, hint) = row_and_hint(&els, p);
            assert!(row.enabled);
            assert_eq!(hint, None, "no hint clutter on a live list");
        }
    }

    /// The disabled Continue button owes a reason too — and at entry the status
    /// line was BLANK, so the page had a dead control and nothing explaining it
    /// (`ui/README.md` § Copy comprehensibility rule 5). The reason comes from
    /// the shared `dns_status_text_key()`, which every app renders, so this is
    /// one line of tui evidence for a seven-app fix.
    #[test]
    fn the_entry_status_line_explains_the_disabled_continue() {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::DnsConfig);

        let status = |els: &[Element]| {
            els.iter()
                .find(|e| e.id == "dns-status-text")
                .map(|e| e.text.clone())
                .expect("dns-status-text")
        };
        let continue_enabled = |els: &[Element]| {
            els.iter()
                .find(|e| e.id == "dns-config-continue-button")
                .expect("continue")
                .enabled
        };

        let els = app.wizard.elements();
        assert!(
            !continue_enabled(&els),
            "precondition: Continue is disabled"
        );
        assert_eq!(status(&els), t::STATUS_PICK_PROVIDER);
        assert!(!status(&els).is_empty(), "a blank line explains nothing");

        // Picked but unverified is a different condition, so a different line:
        // the next act is Verify, not picking again.
        app.wizard.machine.select_dns_provider("cloudflare".into());
        let els = app.wizard.elements();
        assert!(!continue_enabled(&els), "still disabled until verify lands");
        assert_eq!(status(&els), t::STATUS_VERIFY_CREDENTIALS);
    }

    /// Both boxes on kills every row — the state where saying why matters most,
    /// since the screen otherwise offers the user nothing at all. Each row still
    /// names its own single blocker, and the hint is a chrome line (no element
    /// id), matching the rule-5 precedent `devices.wizard.name_required` sets.
    #[test]
    fn every_row_dead_still_explains_each_row() {
        let els = paint(true, true);
        for (provider, expected) in [
            ("cloudflare", t::INELIGIBLE_NEEDS_VPS),
            ("porkbun", t::INELIGIBLE_NEEDS_VPS),
            ("namecheap", t::INELIGIBLE_NEEDS_VPS),
            ("gandi", t::INELIGIBLE_NEEDS_VPS),
            ("hetzner", t::INELIGIBLE_NEEDS_REGISTRAR),
        ] {
            let (row, hint) = row_and_hint(&els, provider);
            assert!(!row.enabled, "{provider} fails one of the two constraints");
            assert_eq!(hint, Some(expected), "{provider}");
        }
        // Un-id'd, so it never becomes a driver-queryable element behind
        // ui.yaml's back (rule A) — but it is real painted text the corpus sees.
        assert!(
            els.iter()
                .any(|e| e.id.is_empty() && e.text == t::INELIGIBLE_NEEDS_VPS)
        );
    }
}
