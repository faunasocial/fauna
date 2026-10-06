//! Stage 6 of handle-first onboarding: VPS configuration.
//!
//! ui.yaml IDs (lines ~886-905):
//!   `page-heading`, `error-message`, `vps-provider-row` (container, with
//!   per-provider buttons named `vps-provider-row[<provider_id>]`),
//!   `vps-provider-link`, `vps-provider-open-browser-button`,
//!   `vps-provider-help-text`, `vps-credentials-form`,
//!   `vps-verify-button`, `vps-location-picker` (indexed dropdown,
//!   one option per region returned by verify), `vps-server-type-radio`
//!   (indexed by position; ≤5 from `cfg.server_types`),
//!   `vps-config-back-button`, `vps-config-continue-button`.
//!
//! All widgets are built once up front; the refresh closure handles every
//! reactive update — provider eligibility, per-provider section rebuild
//! on selection change, location dropdown population from
//! `cfg.locations`, server-type radio rebuild from `cfg.server_types`.
//!
//! Replaces the legacy `nest_server.rs` and `nest_provision.rs`.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, CheckButton, Label, Orientation};

use crate::testid::set_test_id;
use fauna_onboarding_machine::{
    OnboardingMachine, server_type_allowed_for_mail, server_type_label,
};
use fauna_provisioning::{Capability, PROVIDERS};

use crate::async_helper;
use crate::i18n::resolve_key as resolve;

pub fn build(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    let root = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(12)
        .build();
    root.set_margin_top(32);
    root.set_margin_bottom(32);
    root.set_margin_start(48);
    root.set_margin_end(48);

    // -- Title --
    let title = Label::new(Some(&resolve("onboarding.vps_config.title")));
    title.add_css_class("title-2");
    title.set_halign(gtk::Align::Start);
    set_test_id(&title, ids::PAGE_HEADING);
    root.append(&title);

    // -- Error label --
    let error_label = Label::new(None);
    error_label.add_css_class("error");
    error_label.set_wrap(true);
    error_label.set_halign(gtk::Align::Start);
    set_test_id(&error_label, ids::ERROR_MESSAGE);
    error_label.set_visible(false);
    root.append(&error_label);

    // -- Provider row: one button per provider with [vps] capability AND
    //    non-empty curated_offers. Same accessible_role(Group) trick as
    //    dns_config so AT-SPI on Linux exposes the container reliably.
    let provider_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&provider_row, ids::VPS_PROVIDER_ROW);
    root.append(&provider_row);

    for p in PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&Capability::Vps) && !p.curated_offers.is_empty())
    {
        let btn = Button::with_label(&resolve(p.display_name_key));
        let pid = p.id.as_str().to_string();
        set_test_id(&btn, &format!("vps-provider-row[{pid}]"));
        {
            let m = m.clone();
            let pid_for_click = pid.clone();
            btn.connect_clicked(move |_| m.select_vps_provider(pid_for_click.clone()));
        }
        provider_row.append(&btn);
    }

    // -- Per-provider section: rebuilt on provider change. --
    let provider_section = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    root.append(&provider_section);

    // -- Continue / Back row --
    let row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    row.set_margin_top(12);

    let back_btn = Button::with_label(&resolve("common.back"));
    set_test_id(&back_btn, ids::VPS_CONFIG_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| m.back());
    }
    row.append(&back_btn);

    let continue_btn = Button::with_label(&resolve("common.continue"));
    continue_btn.add_css_class("suggested-action");
    set_test_id(&continue_btn, ids::VPS_CONFIG_CONTINUE_BUTTON);
    {
        let m = m.clone();
        let error = error_label.clone();
        continue_btn.connect_clicked(move |btn| {
            error.set_visible(false);
            // Disable while provisioning runs; the next refresh tick
            // re-enables on completion (machine clears is_loading).
            btn.set_sensitive(false);
            let m = m.clone();
            let error = error.clone();
            async_helper::run_on_tokio(async move { m.continue_from_vps().await }, move |result| {
                if let Err(e) = result {
                    error.set_text(&format!("{e}"));
                    error.set_visible(true);
                }
                // On Ok, the machine has transitioned to
                // NestProvisioning; the orchestrator's observer-driven
                // refresh swaps the stack to that page. The user reviews
                // the price summary there, then clicks "Buy and set up"
                // (which calls `start_provisioning()`) to actually run
                // the orchestrator.
            });
        });
    }
    row.append(&continue_btn);
    root.append(&row);

    // Un-id'd chrome beneath the Continue/Back row, rule 5 (`ui/README.md`
    // rule 5): explains why Continue is dead. Shape copied from
    // `settings/atproto.rs`'s `depth_reason`.
    let continue_reason = Label::builder()
        .visible(false)
        .wrap(true)
        .halign(gtk::Align::End)
        .css_classes(["dim-label"])
        .build();
    root.append(&continue_reason);

    // Track which provider the section was last built for so we only
    // rebuild on change (avoids destroying half-typed credentials on
    // every observer tick).
    let last_pid: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    // `hosted-auth` field buttons currently in the credentials form,
    // rebuilt by `rebuild_provider_section` alongside the form's other
    // rows and repainted every tick below — the generic renderer that
    // built them owns no tick of its own.
    let hosted_auth_buttons: Rc<RefCell<Vec<super::generic_provider_form::HostedAuthHandle>>> =
        Rc::new(RefCell::new(Vec::new()));

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let last_pid = last_pid.clone();
        let provider_section = provider_section.clone();
        let continue_btn = continue_btn.clone();
        let continue_reason = continue_reason.clone();
        let error_label = error_label.clone();
        let hosted_auth_buttons = hosted_auth_buttons.clone();
        move || {
            let cfg = m.vps_config();
            if *last_pid.borrow() != cfg.selected_provider_id {
                *hosted_auth_buttons.borrow_mut() =
                    rebuild_provider_section(&provider_section, m.clone());
                *last_pid.borrow_mut() = cfg.selected_provider_id.clone();
            }
            update_provider_section_state(&provider_section, m.clone());
            super::generic_provider_form::refresh_hosted_auth_buttons(
                &hosted_auth_buttons.borrow(),
                &m,
                "vps",
            );
            continue_btn.set_sensitive(m.can_continue_vps());
            match m.vps_continue_blocked_reason() {
                Some(text) => {
                    continue_reason.set_text(&text.resolve(crate::i18n::strings::lookup));
                    continue_reason.set_visible(true);
                }
                None => continue_reason.set_visible(false),
            }

            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}

/// Rebuild the per-provider section's widget tree (link, open-browser, help,
/// credentials form, verify button, location combo, server-type radios).
/// Called only when the selected provider changes. Returns the
/// `hosted-auth` field buttons the credentials form just built, for the
/// caller's own per-tick refresh.
fn rebuild_provider_section(
    section: &gtk::Box,
    m: Arc<OnboardingMachine>,
) -> Vec<super::generic_provider_form::HostedAuthHandle> {
    while let Some(child) = section.first_child() {
        section.remove(&child);
    }
    let cfg = m.vps_config();
    let pid = match cfg.selected_provider_id.clone() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let provider = match PROVIDERS.iter().find(|p| p.id.as_str() == pid) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let link = gtk::LinkButton::with_label(provider.signup_url, provider.signup_url);
    set_test_id(&link, ids::VPS_PROVIDER_LINK);
    section.append(&link);

    let open = Button::with_label(&resolve("onboarding.dns_config.open_in_browser"));
    set_test_id(&open, ids::VPS_PROVIDER_OPEN_BROWSER_BUTTON);
    {
        let url = provider.signup_url.to_string();
        open.connect_clicked(move |_| {
            crate::url_opener::open(&url);
        });
    }
    section.append(&open);

    let help = Label::new(Some(&resolve(provider.help_key)));
    help.set_wrap(true);
    help.set_halign(gtk::Align::Start);
    set_test_id(&help, ids::VPS_PROVIDER_HELP_TEXT);
    section.append(&help);

    // Credentials form — kind="vps" routes change handlers to m.set_vps_cred.
    // The visible-field rule is the machine's (`visible_vps_fields()`: the
    // selected provider's fields filtered by `Capability::Vps`), not ours. This
    // used to be a hand-rolled copy of that filter *plus* a hand-rolled
    // `FieldMeta → FieldMetaPlain` mapping — a per-app re-derivation of a rule
    // that already had a shared getter (whose doc comment even says it exists
    // "so per-app view code is a one-liner instead of a duplicated
    // client-side filter"). dns_config next door always called its
    // `visible_dns_fields()` twin; this is the same call.
    let visible = m.visible_vps_fields();
    let (form, hosted_auth_buttons) =
        super::generic_provider_form::build_creds_only(&visible, m.clone(), "vps");
    set_test_id(&form, ids::VPS_CREDENTIALS_FORM);
    section.append(&form);

    let verify_btn = Button::with_label(&resolve("provisioning.verify_credentials"));
    set_test_id(&verify_btn, ids::VPS_VERIFY_BUTTON);
    {
        let m = m.clone();
        verify_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let m = m.clone();
            async_helper::run_on_tokio(async move { m.verify_vps().await }, |_| {});
        });
    }
    section.append(&verify_btn);

    // Location picker — populated by update_provider_section_state from
    // cfg.locations after verify_vps succeeds. Cross-app uniform
    // decision: all apps render the location list
    // as a dropdown, not radios. Native `gtk::DropDown` (the agent
    // actuates `select` by display label); `DropDown` has no id↔option
    // map, so the selected index maps back to `loc.id` via the machine's
    // own `cfg.locations` order, which the reseed below keeps 1:1 with
    // the model. `select_vps_location` is idempotent, so the notify that
    // fires while the reseed restores the selection is harmless.
    let location_picker = gtk::DropDown::from_strings(&[]);
    set_test_id(&location_picker, ids::VPS_LOCATION_PICKER);
    location_picker.set_visible(false);
    {
        let m = m.clone();
        location_picker.connect_selected_notify(move |d| {
            if let Some(loc) = m.vps_config().locations.get(d.selected() as usize) {
                m.select_vps_location(loc.id.clone());
            }
        });
    }
    section.append(&location_picker);

    // Mail-vs-social mode toggle — decided here (before the box boots) so
    // cloud-init knows whether to provision the scanner sidecars + mail ports.
    // Drives the server-type RAM gate just below (mail ON ⇒ only ≥2 GB plans).
    // Default reads `provision_mail_mode_enabled()` (handle real-domain default);
    // user-driven thereafter — no per-tick resync, so no re-entrancy guard is
    // needed (the section rebuilds, re-reading the stored choice, only on
    // provider change). See onboarding.md §5.
    let mail_mode_toggle =
        CheckButton::with_label(&resolve("onboarding.vps_config.mail_mode_label"));
    mail_mode_toggle.set_active(m.provision_mail_mode_enabled());
    set_test_id(&mail_mode_toggle, ids::VPS_CONFIG_MAIL_MODE_TOGGLE);
    {
        let m = m.clone();
        mail_mode_toggle.connect_toggled(move |c| m.set_provision_mail_mode(c.is_active()));
    }
    section.append(&mail_mode_toggle);

    let mail_mode_desc = Label::new(Some(&resolve("onboarding.vps_config.mail_mode_desc")));
    mail_mode_desc.set_wrap(true);
    mail_mode_desc.set_halign(gtk::Align::Start);
    mail_mode_desc.add_css_class("dim-label");
    section.append(&mail_mode_desc);

    // Server-type radio container (≤5; indexed by position).
    let server_types_box = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    // Internal pivot for `update_provider_section_state`'s sibling walk;
    // not a public test ID. Architectural rule #6 forbids per-app IDs,
    // and no test scopes against this container — keep it discoverable
    // via `widget_name()` only, no AT-SPI Description / tooltip exposure.
    server_types_box.set_widget_name("vps-server-types-container");
    section.append(&server_types_box);

    hosted_auth_buttons
}

/// Per-tick reactive updates for widgets the section already owns. Walks
/// children matching by `widget_name()` (set via `set_test_id`).
fn update_provider_section_state(section: &gtk::Box, m: Arc<OnboardingMachine>) {
    let cfg = m.vps_config();
    let mut child = section.first_child();
    while let Some(c) = child {
        let next = c.next_sibling();
        match c.widget_name().as_str() {
            "vps-verify-button" => {
                if let Ok(b) = c.clone().downcast::<Button>() {
                    b.set_sensitive(m.can_verify_vps());
                }
            }
            "vps-location-picker" => {
                if let Ok(dropdown) = c.clone().downcast::<gtk::DropDown>() {
                    // Reseed only when the option set actually changed —
                    // rebuilding the model each tick would needlessly reset
                    // the selection. Index stays 1:1 with cfg.locations so
                    // the connect handler can map selected() → loc.id.
                    let want: Vec<&str> = cfg.locations.iter().map(|l| l.name.as_str()).collect();
                    let have: Vec<String> = dropdown
                        .model()
                        .map(|model| {
                            (0..model.n_items())
                                .filter_map(|i| {
                                    model.item(i).and_then(|o| {
                                        o.downcast::<gtk::StringObject>()
                                            .ok()
                                            .map(|s| s.string().to_string())
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    if have != want {
                        dropdown.set_model(Some(&gtk::StringList::new(&want)));
                    }
                    dropdown.set_visible(!cfg.locations.is_empty());
                    if let Some(id) = cfg.selected_location_id.as_deref()
                        && let Some(i) = cfg.locations.iter().position(|l| l.id == id)
                        && dropdown.selected() != i as u32
                    {
                        dropdown.set_selected(i as u32);
                    }
                }
            }
            "vps-server-types-container" => {
                if let Ok(container) = c.clone().downcast::<gtk::Box>() {
                    // RAM gate: when mail is ON, only plans with mem_gb ≥ 2 are
                    // offered (the scanners need the RAM). Filter before indexing
                    // so the radio test-ids stay 0-based over the shown set, the
                    // same shape every app renders (shared gate helper).
                    let enable_mail = m.provision_mail_mode_enabled();
                    rebuild_radio_group(
                        &container,
                        cfg.server_types
                            .iter()
                            .filter(|st| server_type_allowed_for_mail((*st).clone(), enable_mail))
                            .take(5)
                            .enumerate()
                            .map(|(i, st)| {
                                (
                                    format!("vps-server-type-radio[{i}]"),
                                    server_type_label(st.clone()),
                                    st.id.clone(),
                                    cfg.selected_server_type_id.as_deref() == Some(&st.id),
                                )
                            }),
                        {
                            let m = m.clone();
                            Rc::new(move |id| m.select_vps_server_type(id))
                        },
                    );
                }
            }
            _ => {}
        }
        child = next;
    }
}

/// Rebuild a radio group from snapshot entries. Wipes existing children
/// and rebuilds — radio groups are cheap and rebuild only happens after
/// verify or selection change, both of which already imply a UI redraw.
fn rebuild_radio_group<I, F>(container: &gtk::Box, entries: I, on_select: Rc<F>)
where
    I: Iterator<Item = (String, String, String, bool)>,
    F: Fn(String) + 'static,
{
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    let mut group: Option<CheckButton> = None;
    for (test_id, label, id, active) in entries {
        let radio = match &group {
            None => CheckButton::new(),
            Some(g) => CheckButton::builder().group(g).build(),
        };
        radio.set_label(Some(&label));
        set_test_id(&radio, &test_id);
        radio.set_active(active);
        let on_select = on_select.clone();
        let id_clone = id.clone();
        radio.connect_toggled(move |r| {
            if r.is_active() {
                (on_select)(id_clone.clone());
            }
        });
        container.append(&radio);
        if group.is_none() {
            group = Some(radio);
        }
    }
}
