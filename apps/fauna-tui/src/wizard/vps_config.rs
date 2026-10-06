//! Stage 5: VPS configuration (`onboarding.md` § 5).
//!
//! ui.yaml `onboarding.vps_config`: `vps-provider-row` (indexed),
//! `vps-provider-link`, `vps-provider-open-browser-button`,
//! `vps-provider-help-text`, `vps-credentials-form`, `vps-verify-button`,
//! `vps-location-picker` (indexed), `vps-config-mail-mode-toggle`,
//! `vps-config-update-channel-row` (keyed by channel id),
//! `vps-server-type-radio` (indexed), `vps-config-back-button`,
//! `vps-config-continue-button`.
//!
//! Everything reads a shared getter:
//!   * credential fields ← `visible_vps_fields()`
//!   * server-type label ← the shared `server_type_label` helper, so all apps
//!     show one canonical `{id} — {vcpu} vCPU / {mem} GB / {disk} GB disk /
//!     {price}/mo` (`onboarding.md:229`)
//!   * the mail-mode RAM gate ← `server_type_allowed_for_mail(st, enable_mail)`
//!   * verify/continue affordances ← `can_verify_vps()` / `can_continue_vps()`,
//!     and the reason a dead Continue is dead ← `vps_continue_blocked_reason()`
//!     (rule-5 chrome, no ui.yaml id of its own)
//!
//! **`visible_vps_fields()` is the point of divergence from linux.** linux
//! re-derives the very same filter + `FieldMeta → FieldMetaPlain` mapping inline
//! in `rebuild_provider_section` (the getter has *zero* call sites anywhere in
//! `apps/fauna-linux/`), which is exactly the per-app re-derivation the shared
//! getter exists to prevent. tui calls the getter; linux is re-pointed at it in
//! this same commit rather than tui copying the drift onto a 7th client.
//!
//! Mail-vs-social mode is decided **here**, before the box boots, because
//! cloud-init needs the mail intent *and* the RAM tier together
//! (`onboarding.md:231`): mail ON forces a ≥ 2 GB plan (the clamd signature DB
//! alone is ~1.5 GB), and turning it OFF unlocks the cheap 1 GB tier.
//!
//! The **update channel** is decided here for the same reason: the channel's
//! image tag is written into that same cloud-init
//! (`onboarding-provisioning.md` § 5). Three rows, all shown to everyone, one
//! always selected (`stable` until the user picks another).

use fauna_i18n::strings::common;
use fauna_i18n::strings::onboarding::vps_config as t;
use fauna_onboarding_machine::{server_type_allowed_for_mail, server_type_label};
use fauna_provisioning::cloud_init::UpdateChannel;
use fauna_provisioning::providers_generated::{Capability, PROVIDERS};
use fauna_ui_ids as ids;

use super::{Action, Element, Field, SelectTarget, Wizard, WizardField, key, localized};

pub fn title() -> String {
    t::TITLE.to_string()
}

pub fn description() -> Vec<String> {
    vec![
        t::SERVER_TYPE_RADIO_LEGEND.to_string(),
        t::MAIL_MODE_DESC.to_string(),
    ]
}

pub fn elements(w: &Wizard) -> Vec<Element> {
    let m = &w.machine;
    let cfg = m.vps_config();
    let selected = cfg.selected_provider_id.clone();
    let enable_mail = m.provision_mail_mode_enabled();

    // `vps-provider-row` is the *container*; each provider button is
    // `vps-provider-row[<provider_id>]` — the cross-app ID shape (linux
    // `set_test_id(&btn, &format!("vps-provider-row[{pid}]"))`, and the shared
    // tests click `vps-provider-row[hetzner]` by that literal id). The bracket
    // here is part of the element id, not a positional scope index.
    let mut out = vec![Element::label(ids::VPS_PROVIDER_ROW, "")];

    // Only providers with a VPS capability *and* curated offers — ui.yaml's
    // `generated_filter` for this page.
    for p in PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&Capability::Vps) && !p.curated_offers.is_empty())
    {
        let id = p.id.as_str();
        let is_selected = selected.as_deref() == Some(id);
        // One-of-N → `Radio`, not a checkbox stack (vocabulary rule 1) — the
        // same conversion as the dns-provider rows.
        out.push(
            Element::radio_gesture(
                format!("vps-provider-row[{id}]"),
                key(p.display_name_key),
                is_selected,
                crate::element::Gesture::Wizard(Action::SelectVpsProvider(id.to_string())),
            )
            .attr("state", if is_selected { "on" } else { "off" })
            .within(ids::VPS_PROVIDER_ROW, 0),
        );
    }

    if let Some(p) = selected
        .as_deref()
        .and_then(|id| PROVIDERS.iter().find(|p| p.id.as_str() == id))
    {
        out.push(Element::label(ids::VPS_PROVIDER_LINK, p.signup_url));
        out.push(Element::button(
            ids::VPS_PROVIDER_OPEN_BROWSER_BUTTON,
            fauna_i18n::strings::onboarding::dns_config::OPEN_IN_BROWSER,
            true,
            Action::OpenSignupUrl(p.signup_url.to_string()),
        ));
        out.push(Element::label(ids::VPS_PROVIDER_HELP_TEXT, key(p.help_key)));

        out.push(Element::label(ids::VPS_CREDENTIALS_FORM, ""));
        for f in m.visible_vps_fields() {
            let element_id = format!("vps-credentials-form-{}", f.id);
            // `hosted-auth` → a button (the twin of dns_config's), so a bundled
            // provider picked here directly signs in the same way.
            if f.field_type == fauna_onboarding_machine::FieldTypePlain::HostedAuth {
                out.push(
                    super::hosted_auth_button(
                        m,
                        fauna_onboarding_machine::CredentialForm::Vps,
                        element_id,
                        &f.id,
                    )
                    .labelled(key(&f.label_key))
                    .within(ids::VPS_CREDENTIALS_FORM, 0),
                );
                continue;
            }
            out.push(
                Element::input(
                    element_id,
                    w.field(WizardField::VpsCred(f.id.clone())),
                    Field::Wizard(WizardField::VpsCred(f.id.clone())),
                )
                .labelled(key(&f.label_key))
                .within(ids::VPS_CREDENTIALS_FORM, 0),
            );
        }

        out.push(Element::button(
            ids::VPS_VERIFY_BUTTON,
            fauna_i18n::strings::provisioning::VERIFY_CREDENTIALS,
            m.can_verify_vps(),
            Action::VerifyVps,
        ));
    }

    // Locations and server types only exist post-verify — `verify_vps` is what
    // populates them from the provider's API.
    //
    // The picker is a single `<select>`-shaped element, NOT one element per
    // location: its text is the *selected* location's **display name** and
    // `select` takes that same name. That is the cross-app contract (apple
    // had to fix exactly this — it was returning the id).
    if !cfg.locations.is_empty() {
        let selected_name = cfg
            .selected_location_id
            .as_deref()
            .and_then(|id| cfg.locations.iter().find(|l| l.id == id))
            .map(|l| l.name.clone())
            .unwrap_or_default();
        out.push(
            Element::select(
                ids::VPS_LOCATION_PICKER,
                selected_name,
                SelectTarget::VpsLocation,
                cfg.locations.iter().map(|l| l.name.clone()).collect(),
            )
            // Prompted, or the picker paints a bare `< city >` that never says
            // it chooses the server's location (copy-audit corpus, 2026-08-04).
            .labelled(t::LOCATION_HEADING),
        );
    }

    out.push(Element::checkbox(
        ids::VPS_CONFIG_MAIL_MODE_TOGGLE,
        t::MAIL_MODE_LABEL,
        enable_mail,
        Action::SetProvisionMailMode(!enable_mail),
    ));

    // `vps-config-update-channel-row` is the *container*; each channel is
    // `vps-config-update-channel-row[<channel id>]` — keyed, like the provider
    // rows. One-of-N → `Radio`. The rows declare their own `group`: this page
    // has other radios (providers, server types), and exactly one channel is
    // always selected, which must not count against theirs (walk.rs I5).
    //
    // Each row carries its one-line description in its own text: a terminal
    // has no secondary line under a radio, and a channel named only "Dev" does
    // not say what choosing it means (`ui/README.md` § Copy comprehensibility).
    out.push(Element::label(
        ids::VPS_CONFIG_UPDATE_CHANNEL_ROW,
        t::UPDATE_CHANNEL_HEADING,
    ));
    let channel = m.provision_update_channel();
    for c in UpdateChannel::ALL {
        let (label, desc) = match c {
            UpdateChannel::Stable => (
                t::UPDATE_CHANNEL_STABLE_LABEL,
                t::UPDATE_CHANNEL_STABLE_DESC,
            ),
            UpdateChannel::Test => (t::UPDATE_CHANNEL_TEST_LABEL, t::UPDATE_CHANNEL_TEST_DESC),
            UpdateChannel::Dev => (t::UPDATE_CHANNEL_DEV_LABEL, t::UPDATE_CHANNEL_DEV_DESC),
        };
        let is_selected = c == channel;
        out.push(
            Element::radio_gesture(
                format!("vps-config-update-channel-row[{}]", c.id()),
                format!("{label} — {desc}"),
                is_selected,
                crate::element::Gesture::Wizard(Action::SetProvisionUpdateChannel(c)),
            )
            .attr("state", if is_selected { "on" } else { "off" })
            .attr("group", "update-channel")
            .within(ids::VPS_CONFIG_UPDATE_CHANNEL_ROW, 0),
        );
    }

    // `vps-server-type-radio[i]` — indexed **by position** (unlike the provider
    // rows, which are keyed by provider id). Filtered by the RAM gate *before*
    // indexing — a mail box never even shows a sub-2 GB plan, rather than
    // rendering it present-but-disabled (linux's `rebuild_radio_group` call
    // site filters the same way, for the same reason: keep the shown-set
    // 0-based rather than leaking a hidden-plan gap into the index).
    for (i, st) in cfg
        .server_types
        .iter()
        .filter(|st| server_type_allowed_for_mail((*st).clone(), enable_mail))
        .enumerate()
    {
        let selected_type = cfg.selected_server_type_id.as_deref() == Some(st.id.as_str());
        out.push(
            Element::radio_gesture(
                format!("vps-server-type-radio[{i}]"),
                server_type_label(st.clone()),
                selected_type,
                crate::element::Gesture::Wizard(Action::SelectVpsServerType(st.id.clone())),
            )
            .attr("state", if selected_type { "on" } else { "off" }),
        );
    }

    out.push(Element::button(
        ids::VPS_CONFIG_BACK_BUTTON,
        common::BACK,
        true,
        Action::Back,
    ));
    // A DIM Continue is the only thing a terminal can say on its own, and "this
    // is dead" is not a reason (`apps/tui.md` § Rendering → *Control
    // vocabulary* rule 3; `ui/README.md` § Copy comprehensibility rule 5). This
    // page had **no** explanatory surface whatsoever — unlike dns_config, whose
    // `dns-status-text` merely went blank in its gating state — so a first-time
    // user met four unmarked provider names and a dead Continue that said
    // nothing at all.
    //
    // The reason comes from the machine, beside the verdict that greys the
    // button, so the two cannot disagree and no shell re-derives the gating
    // rule (priority #2). It paints as un-id'd chrome directly ABOVE the
    // button, which is how this client expresses "explains the thing below"
    // (the constrained-channel principle) and keeps ui.yaml's element set for
    // `vps_config` unchanged — the same rule-5 precedent as the folder
    // wizard's `devices.wizard.name_required` and the dns-provider rows'
    // ineligibility lines.
    //
    // Above rather than below because Continue is the LAST element on the page:
    // a line after it would sit off the bottom of the eyeshot the rule asks for.
    //
    // It doubles as the provider group's `explains-unset` line (walk.rs I5):
    // in the entry state those four rows paint `( )` with none marked, and this
    // is the line that says why — the user hasn't chosen yet, and choosing is
    // what the page asks. The rows carry no `group` attr, so the page's single
    // undeclared group is `""`.
    if let Some(reason) = m.vps_continue_blocked_reason() {
        out.push(Element::chrome(localized(&reason)).attr("explains-unset", ""));
    }
    out.push(Element::button(
        ids::VPS_CONFIG_CONTINUE_BUTTON,
        common::CONTINUE,
        m.can_continue_vps(),
        Action::ContinueFromVps,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_onboarding_machine::OnboardingStep;

    /// **Rule 5 on the page that had no explanatory surface at all.** Every
    /// state in which `vps-config-continue-button` is dead paints a line saying
    /// what to do about it, and the four states say four different things —
    /// the user's next act is pick / verify / choose where / choose how big
    /// (`ui/README.md` § Copy comprehensibility rule 5 + Q2).
    ///
    /// The twin of `dns_config.rs`'s
    /// `the_entry_status_line_explains_the_disabled_continue`, for the sibling
    /// page one step later in the same wizard.
    #[test]
    fn every_state_that_kills_continue_says_what_to_do_about_it() {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::VpsConfig);
        let m = &app.wizard.machine;

        let continue_enabled = |els: &[Element]| {
            els.iter()
                .find(|e| e.id == "vps-config-continue-button")
                .expect("continue")
                .enabled
        };
        // The explainer is un-id'd chrome, so it is found by the role it
        // declares — the same handle walk.rs's I5 resolves it through.
        let reason = |els: &[Element]| {
            els.iter()
                .find(|e| e.attrs.iter().any(|(k, _)| k == "explains-unset"))
                .map(|e| e.text.clone())
        };

        let els = elements(&app.wizard);
        assert!(
            !continue_enabled(&els),
            "precondition: nothing is chosen, so Continue is dead"
        );
        assert_eq!(reason(&els).as_deref(), Some(t::STATUS_PICK_PROVIDER));

        // Picked but unverified: the next act is Verify, not picking again.
        m.select_vps_provider("hetzner".into());
        let els = elements(&app.wizard);
        assert!(!continue_enabled(&els), "still dead until verify lands");
        assert_eq!(reason(&els).as_deref(), Some(t::STATUS_VERIFY_CREDENTIALS));
    }

    /// The update-channel rows: all three always paint, exactly one is
    /// selected (`stable` before any choice), and a row's gesture selects it —
    /// which is what reaches `CloudInitParams::image_tag`.
    #[test]
    fn update_channel_rows_show_all_three_and_follow_the_choice() {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::VpsConfig);
        let m = &app.wizard.machine;

        let rows = |els: &[Element]| -> Vec<(String, bool)> {
            els.iter()
                .filter(|e| e.id.starts_with("vps-config-update-channel-row["))
                .map(|e| {
                    let on = e.attrs.iter().any(|(k, v)| k == "state" && v == "on");
                    (e.id.clone(), on)
                })
                .collect()
        };
        let els = elements(&app.wizard);
        assert_eq!(
            rows(&els),
            [
                ("vps-config-update-channel-row[stable]".to_string(), true),
                ("vps-config-update-channel-row[test]".to_string(), false),
                ("vps-config-update-channel-row[dev]".to_string(), false),
            ],
            "all three channels, before any provider is chosen, stable selected"
        );
        assert!(
            els.iter()
                .filter(|e| e.id.starts_with("vps-config-update-channel-row["))
                .all(|e| e
                    .attrs
                    .iter()
                    .any(|(k, v)| k == "group" && v == "update-channel")),
            "the rows declare their own radio group (walk.rs I5)"
        );

        m.set_provision_update_channel(UpdateChannel::Dev);
        let on: Vec<_> = rows(&elements(&app.wizard))
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(id, _)| id)
            .collect();
        assert_eq!(on, ["vps-config-update-channel-row[dev]"]);
        assert_eq!(m.provision_update_channel().image_tag(), "dev");
    }

    /// A live Continue paints NO explanation — `None`, not a blank line.
    ///
    /// This is the regression that made the sibling page's fix necessary in the
    /// first place: `dns_status_text_key` returned an empty *key* for its entry
    /// state, so every app dutifully painted an empty line. Modelling "there
    /// is nothing to explain" as `Option::None` rather than `""` is what makes
    /// that unrepresentable here, and this pins it.
    #[test]
    fn a_live_continue_explains_nothing_rather_than_painting_a_blank_line() {
        let app = crate::app::tests::test_app();
        app.wizard
            .machine
            .set_step_for_test(OnboardingStep::VpsConfig);
        app.wizard.machine.set_vps_state_for_test(|v| {
            v.selected_provider_id = Some("hetzner".into());
            v.verified = true;
            v.selected_location_id = Some("fsn1".into());
            v.selected_server_type_id = Some("cx22".into());
        });

        let els = elements(&app.wizard);
        assert!(
            els.iter()
                .find(|e| e.id == "vps-config-continue-button")
                .expect("continue")
                .enabled,
            "precondition: the seeded config satisfies every gate"
        );
        assert!(
            !els.iter()
                .any(|e| e.attrs.iter().any(|(k, _)| k == "explains-unset")),
            "a live control owes no explanation, and must not paint an empty one"
        );
    }
}
