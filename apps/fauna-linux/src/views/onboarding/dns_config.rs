//! Stage 5 of handle-first onboarding: DNS configuration.
//!
//! ui.yaml IDs (lines ~857-880):
//!   `page-heading`, `error-message`, `dns-buy-domain-checkbox`,
//!   `dns-same-provider-checkbox`, `dns-provider-row` (container, with
//!   per-provider buttons named `dns-provider-row[<provider_id>]`),
//!   `dns-set-up-later-button`, `dns-provider-link`,
//!   `dns-provider-open-browser-button`, `dns-provider-help-text`,
//!   `dns-credentials-form`, `dns-verify-button`, `dns-status-text`,
//!   `dns-tld-price-display`, `dns-price-confirm-checkbox`,
//!   `dns-config-back-button`, `dns-config-continue-button`.
//!
//! All widgets are built once up front; the refresh closure handles every
//! reactive update — provider-row button sensitivities, per-provider
//! section rebuild on selection change, button enablement, status text,
//! price display, conditional price-confirm checkbox visibility.
//!
//! Replaces the legacy `nest_mode_select.rs`, `nest_dns.rs`, and
//! `nest_registrar.rs` together.

use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, CheckButton, Label, Orientation};

use crate::testid::set_test_id;
use fauna_onboarding_machine::{OnboardingMachine, format_price};
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
    let title = Label::new(Some(&resolve("onboarding.dns_config.title")));
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

    // -- Two checkboxes --
    // Native gtk::CheckButtons, matching web's `<input type=checkbox>`. The
    // agent actuates them via activate(); same `:active` + connect_toggled API.
    let buy_check = CheckButton::with_label(&resolve("onboarding.dns_config.buy_domain_checkbox"));
    set_test_id(&buy_check, ids::DNS_BUY_DOMAIN_CHECKBOX);
    {
        let m = m.clone();
        buy_check.connect_toggled(move |b| m.toggle_buy_domain(b.is_active()));
    }
    root.append(&buy_check);

    let same_check =
        CheckButton::with_label(&resolve("onboarding.dns_config.same_provider_checkbox"));
    set_test_id(&same_check, ids::DNS_SAME_PROVIDER_CHECKBOX);
    {
        let m = m.clone();
        same_check.connect_toggled(move |b| m.toggle_same_provider_for_vps(b.is_active()));
    }
    root.append(&same_check);

    // -- Provider row (one button per DNS-capable provider). Eligibility is
    //    toggled in refresh based on the buy-domain / same-provider checkboxes.
    //
    // accessible_role(Group) makes the Box reliably discoverable in the
    // AT-SPI tree. Plain `gtk::Box::new` defaults to role `Generic`, which
    // AT-SPI on Linux can omit from the tree under some compositors —
    // tests that count children of `dns-provider-row` by ID would then
    // see zero matches. (See test_back_buttons.py for prior context.)
    let provider_row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&provider_row, ids::DNS_PROVIDER_ROW);
    root.append(&provider_row);

    // One (button, reason) column per provider — the reason is un-id'd chrome
    // beneath its own button (`ui/README.md` rule 5: a disabled control needs
    // an on-screen reason within eyeshot; shape copied from `settings/atproto.rs`'s
    // `depth_reason`). Kept in `provider_widgets` so refresh doesn't have to
    // re-derive the button↔provider mapping by walking GTK siblings.
    let mut provider_widgets: Vec<(Button, Label)> = Vec::new();
    for p in PROVIDERS
        .iter()
        .filter(|p| p.capabilities.contains(&Capability::Dns))
    {
        let column = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(4)
            .build();

        let btn = Button::with_label(&resolve(p.display_name_key));
        let pid = p.id.as_str().to_string();
        set_test_id(&btn, &format!("dns-provider-row[{pid}]"));
        {
            let m = m.clone();
            let pid_for_click = pid.clone();
            btn.connect_clicked(move |_| m.select_dns_provider(pid_for_click.clone()));
        }
        column.append(&btn);

        let reason = Label::builder()
            .visible(false)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        column.append(&reason);

        provider_row.append(&column);
        provider_widgets.push((btn, reason));
    }

    // -- Set up later button --
    let later_btn = Button::with_label(&resolve("onboarding.dns_config.set_up_later"));
    set_test_id(&later_btn, ids::DNS_SET_UP_LATER_BUTTON);
    {
        let m = m.clone();
        later_btn.connect_clicked(move |_| m.dns_set_up_later());
    }
    root.append(&later_btn);

    let later_warn = Label::new(Some(&resolve("onboarding.dns_config.set_up_later_warning")));
    later_warn.set_wrap(true);
    later_warn.set_halign(gtk::Align::Start);
    later_warn.add_css_class("fauna-muted");
    root.append(&later_warn);

    // -- Per-provider section: built ONCE with all widget skeletons in place.
    //    Provider selection updates content in-place (link uri, help text,
    //    inner credential entries) without removing/re-adding the outer
    //    widgets — keeps every test ID present in the AT-SPI tree from the
    //    moment the wizard renders.
    //
    //    The previous design rebuilt the whole section subtree on every
    //    provider change, which made test IDs like `dns-credentials-form`
    //    transiently absent and broke
    //    test_dns_config_select_provider_shows_credentials_form[linux].
    let provider_section_box = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    root.append(&provider_section_box);
    let provider_section = build_provider_section(&provider_section_box, m.clone());

    // -- Continue / Back row --
    let row = gtk::Box::builder()
        .orientation(Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    row.set_margin_top(12);

    let back_btn = Button::with_label(&resolve("common.back"));
    set_test_id(&back_btn, ids::DNS_CONFIG_BACK_BUTTON);
    {
        let m = m.clone();
        back_btn.connect_clicked(move |_| m.back());
    }
    row.append(&back_btn);

    let continue_btn = Button::with_label(&resolve("common.continue"));
    continue_btn.add_css_class("suggested-action");
    set_test_id(&continue_btn, ids::DNS_CONFIG_CONTINUE_BUTTON);
    {
        let m = m.clone();
        let error = error_label.clone();
        continue_btn.connect_clicked(move |_| {
            if let Err(e) = m.continue_from_dns() {
                crate::settings::render_error_label(&error, Some(&format!("{e}")));
            }
            // On Ok, the machine advances step → orchestrator swaps page.
        });
    }
    row.append(&continue_btn);
    root.append(&row);

    // Track which provider the section was last built for so we only
    // rebuild on change (avoids destroying half-typed credentials on
    // every observer tick). Combined with `same_provider_for_vps` since
    // that toggle changes which fields `visible_dns_fields()` returns
    // for providers that have both DNS and VPS capability (e.g. Hetzner) —
    // without re-rebuilding when it flips, the form keeps showing the
    // stale field set.
    let last_pid: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let last_same_provider: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

    let refresh: Rc<dyn Fn()> = Rc::new({
        let m = m.clone();
        let last_pid = last_pid.clone();
        let last_same_provider = last_same_provider.clone();
        let provider_section = provider_section.clone();
        let provider_widgets = provider_widgets.clone();
        let buy_check = buy_check.clone();
        let same_check = same_check.clone();
        let continue_btn = continue_btn.clone();
        let error_label = error_label.clone();
        let _provider_section_box = provider_section_box;
        move || {
            let cfg = m.dns_config();

            // buy_domain is machine-derived (set on submit_handle_check_continue
            // per target state step 4). Client renders the snapshot value;
            // user toggles forward via m.toggle_buy_domain.
            buy_check.set_active(cfg.buy_domain);
            same_check.set_active(cfg.same_provider_for_vps);

            // Provider-row eligibility — disable any DNS provider that lacks
            // a capability the user asked for, and paint why beside it. The
            // rules live in the shared machine (`dns_provider_eligible` /
            // `dns_provider_ineligible_reason`, one shortfall computation so
            // the two can never disagree) — this just maps them onto the
            // per-provider widgets.
            let dns_providers: Vec<_> = PROVIDERS
                .iter()
                .filter(|p| p.capabilities.contains(&Capability::Dns))
                .collect();
            for (p, (btn, reason)) in dns_providers.iter().zip(provider_widgets.iter()) {
                let pid = p.id.as_str().to_string();
                btn.set_sensitive(m.dns_provider_eligible(pid.clone()));
                match m.dns_provider_ineligible_reason(pid) {
                    Some(text) => {
                        reason.set_text(&text.resolve(crate::i18n::strings::lookup));
                        reason.set_visible(true);
                    }
                    None => reason.set_visible(false),
                }
            }

            // Per-provider section — apply provider change when EITHER the
            // selected provider id OR same_provider_for_vps changes.
            // Latter matters for providers with both DNS+VPS capability
            // (Hetzner): toggling same-provider flips the visible_dns_fields
            // result, and the form needs to repopulate to add/drop the
            // VPS-kinded fields.
            let pid_changed = *last_pid.borrow() != cfg.selected_provider_id;
            let same_provider_changed = *last_same_provider.borrow() != cfg.same_provider_for_vps;
            if pid_changed || same_provider_changed {
                apply_provider_change(
                    &provider_section,
                    cfg.selected_provider_id.as_deref(),
                    m.clone(),
                );
                *last_pid.borrow_mut() = cfg.selected_provider_id.clone();
                *last_same_provider.borrow_mut() = cfg.same_provider_for_vps;
            }
            // Always update the per-provider widgets that track machine state.
            update_provider_section_state(&provider_section, m.clone());

            continue_btn.set_sensitive(m.can_continue_dns());

            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}

/// Stable handles to every widget in the per-provider section. Built ONCE
/// up front by `build_provider_section`; reused for the lifetime of the
/// dns_config view.
#[derive(Clone)]
struct ProviderSection {
    link: gtk::LinkButton,
    help: Label,
    notes: Label,
    form: gtk::Box,
    verify_btn: Button,
    status: Label,
    price: Label,
    price_confirm: CheckButton,
    contact_form: gtk::Box,
    contact_refresh: Rc<dyn Fn()>,
    /// URL captured by the `open` button's click handler. Updated by
    /// `apply_provider_change` when the selected provider changes.
    captured_url: Rc<RefCell<Option<String>>>,
    /// `hosted-auth` field buttons currently in `form`, rebuilt by
    /// `apply_provider_change` alongside the form's other rows. Repainted
    /// every tick by `update_provider_section_state` — the generic
    /// renderer that built them owns no tick of its own.
    hosted_auth_buttons: Rc<RefCell<Vec<super::generic_provider_form::HostedAuthHandle>>>,
}

/// Build the per-provider section's widget skeleton ONCE. All test IDs
/// are present in the AT-SPI tree from the start; per-provider content is
/// applied later by `apply_provider_change`.
///
/// GTK4's AT-SPI surface only exposes visible widgets (a `set_visible(false)`
/// widget is NOT in the AT-SPI tree). So every element listed in ui.yaml's
/// `elements:` for dns_config (link, open, help, form, verify, status,
/// tld-price-display) is built visible-from-start with empty/placeholder
/// content. Truly conditional elements from ui.yaml's `optional_elements:`
/// (notes, price-confirm-checkbox, contact-form) start hidden — their
/// visibility is driven by per-tick state.
fn build_provider_section(parent: &gtk::Box, m: Arc<OnboardingMachine>) -> ProviderSection {
    // dns-provider-link: empty URI initially; LinkButton::set_uri lets us
    // swap when the provider changes without re-instancing the widget.
    let link = gtk::LinkButton::with_label("about:blank", "");
    set_test_id(&link, ids::DNS_PROVIDER_LINK);
    parent.append(&link);

    // dns-provider-open-browser-button: click handler captures captured_url
    // via Rc<RefCell<Option<String>>>; per-provider apply updates the cell.
    let captured_url: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let open = Button::with_label(&resolve("onboarding.dns_config.open_in_browser"));
    set_test_id(&open, ids::DNS_PROVIDER_OPEN_BROWSER_BUTTON);
    {
        let captured_url = captured_url.clone();
        open.connect_clicked(move |_| {
            if let Some(url) = captured_url.borrow().as_deref() {
                crate::url_opener::open(url);
            }
        });
    }
    parent.append(&open);

    let help = Label::new(None);
    help.set_wrap(true);
    help.set_halign(gtk::Align::Start);
    set_test_id(&help, ids::DNS_PROVIDER_HELP_TEXT);
    parent.append(&help);

    // Per-provider registrar notes (e.g. Porkbun's account-contact reminder).
    // ui.yaml `optional_elements`: visibility driven by
    // `update_provider_section_state` (depends on buy_domain + provider's
    // registrar_notes_key). Initially hidden.
    let notes = Label::new(None);
    notes.set_wrap(true);
    notes.set_halign(gtk::Align::Start);
    notes.add_css_class("fauna-muted");
    set_test_id(&notes, ids::DNS_REGISTRAR_NOTES_TEXT);
    notes.set_visible(false);
    parent.append(&notes);

    // dns-credentials-form: stable Box with role=Group; inner Entry rows
    // are populated by `apply_provider_change` from m.visible_dns_fields().
    // Visible from app start.
    let form = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&form, ids::DNS_CREDENTIALS_FORM);
    parent.append(&form);

    let verify_btn = Button::with_label(&resolve("provisioning.verify_credentials"));
    set_test_id(&verify_btn, ids::DNS_VERIFY_BUTTON);
    // Sensitivity is gated by m.can_verify_dns() in update_provider_section_state.
    verify_btn.set_sensitive(false);
    {
        let m = m.clone();
        verify_btn.connect_clicked(move |btn| {
            // Disable while the verify probe is in flight; the next refresh
            // tick re-enables based on `can_verify_dns()`.
            btn.set_sensitive(false);
            let m = m.clone();
            async_helper::run_on_tokio(async move { m.verify_dns().await }, |_| {});
        });
    }
    parent.append(&verify_btn);

    let status = Label::new(None);
    status.set_wrap(true);
    status.set_halign(gtk::Align::Start);
    set_test_id(&status, ids::DNS_STATUS_TEXT);
    parent.append(&status);

    let price = Label::new(None);
    price.set_halign(gtk::Align::Start);
    set_test_id(&price, ids::DNS_TLD_PRICE_DISPLAY);
    parent.append(&price);

    // ui.yaml `optional_elements`: visible only when buy_domain && buyable.
    let price_confirm = CheckButton::with_label(&resolve("registrar.price_confirm"));
    set_test_id(&price_confirm, ids::DNS_PRICE_CONFIRM_CHECKBOX);
    price_confirm.set_visible(false);
    {
        let m = m.clone();
        price_confirm.connect_toggled(move |b| {
            if b.is_active() {
                m.confirm_price();
            }
        });
    }
    parent.append(&price_confirm);

    // Contact form: built ONCE; refresh closure handles visibility +
    // pre-fill from cfg.contact each tick. ui.yaml `optional_elements`.
    let (contact_form, contact_refresh) = build_contact_form(m.clone());
    parent.append(&contact_form);

    ProviderSection {
        link,
        help,
        notes,
        form,
        verify_btn,
        status,
        price,
        price_confirm,
        contact_form,
        contact_refresh,
        captured_url,
        hosted_auth_buttons: Rc::new(RefCell::new(Vec::new())),
    }
}

/// Apply a provider change to the stable section. Updates link uri / help
/// text / captured open-button URL, rebuilds the form's inner Entry rows
/// from `m.visible_dns_fields()`, and toggles outer-widget visibility.
/// `pid == None` hides every per-provider widget without removing them.
fn apply_provider_change(s: &ProviderSection, pid: Option<&str>, m: Arc<OnboardingMachine>) {
    // Always clear inner form children — old per-provider entries shouldn't
    // bleed into the new provider's form.
    while let Some(child) = s.form.first_child() {
        s.form.remove(&child);
    }

    let provider = match pid.and_then(|p| PROVIDERS.iter().find(|x| x.id.as_str() == p)) {
        Some(p) => p,
        None => {
            // ui.yaml `elements:` (link/open/help/form/verify/status/price)
            // stay visible-but-empty; they're always in the AT-SPI tree per
            // ui.yaml. Reset their content to empty/placeholder.
            s.link.set_uri("about:blank");
            s.link.set_label("");
            s.help.set_text("");
            // Form: clear inner Entry rows.
            while let Some(child) = s.form.first_child() {
                s.form.remove(&child);
            }
            // ui.yaml `optional_elements:` (notes / price_confirm /
            // contact_form) and per-tick driven (price): hide.
            s.notes.set_visible(false);
            s.price_confirm.set_visible(false);
            s.contact_form.set_visible(false);
            *s.captured_url.borrow_mut() = None;
            s.hosted_auth_buttons.borrow_mut().clear();
            return;
        }
    };

    s.link.set_uri(provider.signup_url);
    s.link.set_label(provider.signup_url);
    *s.captured_url.borrow_mut() = Some(provider.signup_url.to_string());
    s.help.set_text(&resolve(provider.help_key));

    // Repopulate the form's inner Entry rows from visible_dns_fields().
    while let Some(child) = s.form.first_child() {
        s.form.remove(&child);
    }
    let handles = super::generic_provider_form::populate_creds_into(
        &s.form,
        &m.visible_dns_fields(),
        m.clone(),
        "dns",
    );
    *s.hosted_auth_buttons.borrow_mut() = handles;
    // notes / price / price_confirm / contact_form visibility is decided by
    // update_provider_section_state on every tick.
}

/// Per-tick reactive updates for the stable section's widgets.
fn update_provider_section_state(s: &ProviderSection, m: Arc<OnboardingMachine>) {
    let cfg = m.dns_config();

    s.verify_btn.set_sensitive(m.can_verify_dns());
    s.status.set_text(
        &m.dns_status_text_key()
            .resolve(crate::i18n::strings::lookup),
    );

    // Registrar-notes visibility is decided by the shared machine; the
    // key→i18n text rendering stays here (platform shell).
    if m.should_show_registrar_notes() {
        if let Some(key) = cfg
            .selected_provider_id
            .as_deref()
            .and_then(|p| PROVIDERS.iter().find(|x| x.id.as_str() == p))
            .and_then(|p| p.registrar_notes_key)
        {
            s.notes.set_text(&resolve(key));
        }
        s.notes.set_visible(true);
    } else {
        s.notes.set_visible(false);
    }

    // dns-tld-price-display: ui.yaml `elements:` item — always in the
    // AT-SPI tree. Empty text when no price quote, populated when buyable.
    use fauna_provisioning::registrar::RegistrarAvailability;
    if let Some(RegistrarAvailability::Buyable {
        price_cents,
        currency,
        ..
    }) = cfg.current_availability.as_ref()
    {
        s.price.set_text(&format_price(
            *price_cents,
            currency.clone().unwrap_or_else(|| "USD".into()),
        ));
    } else {
        s.price.set_text("");
    }

    let buyable = matches!(
        cfg.current_availability,
        Some(RegistrarAvailability::Buyable { .. })
    );
    s.price_confirm.set_visible(cfg.buy_domain && buyable);

    super::generic_provider_form::refresh_hosted_auth_buttons(
        &s.hosted_auth_buttons.borrow(),
        &m,
        "dns",
    );

    (s.contact_refresh)();
}

/// Build the WHOIS contact form. Returns the container plus a per-tick
/// refresh closure that updates visibility and pre-fills entries from
/// `cfg.contact` when set by `verify_dns()` via `fetch_default_contact()`.
///
/// Visibility: the caller (per-provider rebuild) sets `set_visible(false)`
/// up front; the refresh closure flips it true iff the provider requires
/// per-registration contact (`registrar_requires_contact == Some(true)`)
/// AND the current `provider_status()` is `UnregisteredBuyable`.
///
/// On every entry edit the form snapshots all 9 fields into a fresh
/// `ContactInfo` and calls `m.set_contact(c)` so the machine state stays
/// in sync — `can_continue_dns` reads `dns.contact.is_some()` and gates
/// the Continue button on it for `requires_contact` registrars.
fn build_contact_form(m: Arc<OnboardingMachine>) -> (gtk::Box, Rc<dyn Fn()>) {
    use fauna_provisioning::registrar::ContactInfo;

    let container = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(6)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&container, ids::DNS_CONTACT_FORM);
    container.set_visible(false);

    // Helper to build a labeled Entry and register its test ID.
    fn labeled_entry(parent: &gtk::Box, label_key: &str, test_id: &str) -> gtk::Entry {
        let row = gtk::Box::builder()
            .orientation(Orientation::Vertical)
            .spacing(2)
            .build();
        let lbl = Label::new(Some(&resolve(label_key)));
        lbl.set_halign(gtk::Align::Start);
        lbl.add_css_class("fauna-muted");
        row.append(&lbl);
        let entry = gtk::Entry::new();
        set_test_id(&entry, test_id);
        row.append(&entry);
        parent.append(&row);
        entry
    }

    let first_name = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.first_name",
        "dns-contact-first-name-input",
    );
    let last_name = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.last_name",
        "dns-contact-last-name-input",
    );
    let email = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.email",
        "dns-contact-email-input",
    );
    let phone = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.phone",
        "dns-contact-phone-input",
    );
    let address1 = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.address1",
        "dns-contact-address1-input",
    );
    let city = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.city",
        "dns-contact-city-input",
    );
    let state_entry = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.state",
        "dns-contact-state-input",
    );
    let postal = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.postal_code",
        "dns-contact-postal-code-input",
    );
    let country = labeled_entry(
        &container,
        "onboarding.dns_config.contact_fields.country",
        "dns-contact-country-input",
    );

    // Snapshot all 9 entries into a ContactInfo and push it to the machine.
    // Wired on every entry's `changed` signal — keeps the state simple
    // (no debounce; GTK Entry changed is cheap and the machine mutate is a
    // single Arc<RwLock> write).
    let snapshot_and_push = {
        let m = m.clone();
        let entries = (
            first_name.clone(),
            last_name.clone(),
            email.clone(),
            phone.clone(),
            address1.clone(),
            city.clone(),
            state_entry.clone(),
            postal.clone(),
            country.clone(),
        );
        Rc::new(move || {
            let c = ContactInfo {
                first_name: entries.0.text().to_string(),
                last_name: entries.1.text().to_string(),
                email: entries.2.text().to_string(),
                phone: entries.3.text().to_string(),
                address1: entries.4.text().to_string(),
                city: entries.5.text().to_string(),
                state: entries.6.text().to_string(),
                postal_code: entries.7.text().to_string(),
                country: entries.8.text().to_string(),
            };
            m.set_contact(c);
        })
    };

    for entry in [
        &first_name,
        &last_name,
        &email,
        &phone,
        &address1,
        &city,
        &state_entry,
        &postal,
        &country,
    ] {
        let push = snapshot_and_push.clone();
        entry.connect_changed(move |_| push());
    }

    // Refresh closure: visibility + pre-fill from cfg.contact.
    //
    // last_prefilled: tracks whether we've already copied cfg.contact into
    // the entries. Without this, every refresh tick after verify would
    // overwrite the user's edits. We only pre-fill on the cfg.contact
    // value transitioning from None to Some, or on a value change after
    // a fresh verify_dns.
    let last_prefilled: Rc<RefCell<Option<ContactInfo>>> = Rc::new(RefCell::new(None));
    let refresh: Rc<dyn Fn()> = Rc::new({
        let container = container.clone();
        let entries = (
            first_name,
            last_name,
            email,
            phone,
            address1,
            city,
            state_entry,
            postal,
            country,
        );
        let m = m.clone();
        let last_prefilled = last_prefilled.clone();
        move || {
            let visible = m.should_show_contact_form();
            container.set_visible(visible);

            if !visible {
                return;
            }

            let cfg = m.dns_config();
            if let Some(c) = cfg.contact.as_ref() {
                let prev = last_prefilled.borrow().clone();
                if prev.as_ref() != Some(c) {
                    entries.0.set_text(&c.first_name);
                    entries.1.set_text(&c.last_name);
                    entries.2.set_text(&c.email);
                    entries.3.set_text(&c.phone);
                    entries.4.set_text(&c.address1);
                    entries.5.set_text(&c.city);
                    entries.6.set_text(&c.state);
                    entries.7.set_text(&c.postal_code);
                    entries.8.set_text(&c.country);
                    *last_prefilled.borrow_mut() = Some(c.clone());
                }
            }
        }
    });

    (container, refresh)
}
