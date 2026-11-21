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
//!   `vps-config-update-channel-row` (container, with one radio per channel
//!   named `vps-config-update-channel-row[<channel_id>]`),
//!   `vps-config-back-button`, `vps-config-continue-button`.
//!
//! All widgets are built once up front; the refresh closure handles every
//! reactive update — provider eligibility, per-provider section rebuild
//! on selection change, location dropdown population from
//! `cfg.locations`, server-type radio rebuild from `cfg.server_types`.
//!
//! Replaces the legacy `nest_server.rs` and `nest_provision.rs`.

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{Button, CheckButton, Label, Orientation};

use crate::testid::set_test_id;
use fauna_onboarding_machine::{
    OnboardingMachine, server_type_allowed_for_mail, server_type_label,
};
use fauna_provisioning::cloud_init::UpdateChannel;
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

    // -- The server choices (location, mail mode, SSH door, server type), the
    //    same widgets `nest_provisioning`'s choices region shows
    //    (`onboarding-provisioning.md` § 3d: § 5's rules apply wherever those
    //    controls are shown). Shown here once a provider is selected — the
    //    location and offers exist only after its verify.
    let choices = build_choices(m.clone());
    choices.provider_part.set_visible(false);
    root.append(&choices.provider_part);

    // -- Update channel: which builds the box's updater follows. Outside the
    //    per-provider section on purpose — the choice does not depend on the
    //    provider, so it is shown before one is picked (and tui renders it the
    //    same way). Built once; the choices' refresh re-syncs the selection
    //    from the machine, never a per-app copy. --
    root.append(&choices.channel_box);

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
            choices
                .provider_part
                .set_visible(cfg.selected_provider_id.is_some());
            (choices.refresh)();
            update_provider_section_state(&provider_section, m.clone());
            super::generic_provider_form::refresh_hosted_auth_buttons(
                &hosted_auth_buttons.borrow(),
                &m,
                &super::generic_provider_form::CredTarget::Vps,
            );
            // One answer paints both halves (`ui/README.md` § Offer only
            // what is possible, rule 6): sensitive exactly while possible,
            // the blocked reason on the line beside it.
            crate::possible::paint(
                &continue_btn,
                Some(&continue_reason),
                m.vps_continue_possibility(),
            );

            let msg = m.error_message().filter(|msg| !msg.is_empty());
            crate::settings::render_error_label(&error_label, msg.as_deref());
        }
    });

    (root, refresh)
}

/// The server choices — location, mail mode, SSH door, server type and update
/// channel — shared by this page and `nest_provisioning`'s choices region
/// (`onboarding-provisioning.md` § 3d *The choices on the setting-things-up
/// page*: "§ 5's rules ... apply wherever those controls are shown"). One set
/// of widgets and one refresh, so the two pages cannot drift apart (tui's
/// `vps_config::choices` is the same split).
pub(super) struct ServerChoices {
    /// Location, mail mode (+ its description), SSH door (+ its warning) and
    /// the server-type radios.
    pub provider_part: gtk::Box,
    /// `vps-config-update-channel-row`.
    pub channel_box: gtk::Box,
    /// Re-read every control from the machine. A repaint records nothing:
    /// each toggle's handler stands down while it runs.
    pub refresh: Rc<dyn Fn()>,
}

pub(super) fn build_choices(m: Arc<OnboardingMachine>) -> ServerChoices {
    let provider_part = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(8)
        .build();
    // A repaint's `set_active` fires `toggled`; the handlers ignore it while
    // this is set, so a resync is never read as the person's choice.
    let syncing = Rc::new(Cell::new(false));

    // Location picker — populated by the refresh from cfg.locations once the
    // provider's server check listed them. A cross-app uniform decision: all
    // apps render the location list as a dropdown, not radios.
    // Native `gtk::DropDown` (the agent actuates `select` by display label);
    // `DropDown` has no id↔option map, so the selected index maps back to
    // `loc.id` via the machine's own `cfg.locations` order, which the reseed
    // keeps 1:1 with the model. `select_vps_location` is idempotent, so the
    // notify that fires while the reseed restores the selection is harmless. A
    // change re-lists the offers for the new location: the setter starts that
    // listing on the machine's own runtime, whatever thread calls it, and the
    // result arrives through the observer.
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
    provider_part.append(&location_picker);

    // Mail-vs-social mode toggle — decided before the box boots so cloud-init
    // knows whether to provision the scanner sidecars + mail ports. Drives the
    // server-type RAM gate below (mail ON ⇒ only ≥2 GB plans). The default is
    // the machine's (`provision_mail_mode_enabled()`); onboarding.md §5.
    let mail_mode_toggle =
        CheckButton::with_label(&resolve("onboarding.vps_config.mail_mode_label"));
    mail_mode_toggle.set_active(m.provision_mail_mode_enabled());
    set_test_id(&mail_mode_toggle, ids::VPS_CONFIG_MAIL_MODE_TOGGLE);
    {
        let m = m.clone();
        let syncing = syncing.clone();
        mail_mode_toggle.connect_toggled(move |c| {
            if !syncing.get() {
                m.set_provision_mail_mode(c.is_active());
            }
        });
    }
    provider_part.append(&mail_mode_toggle);

    let mail_mode_desc = Label::new(Some(&resolve("onboarding.vps_config.mail_mode_desc")));
    mail_mode_desc.set_wrap(true);
    mail_mode_desc.set_halign(gtk::Align::Start);
    mail_mode_desc.add_css_class("dim-label");
    provider_part.append(&mail_mode_desc);

    // The SSH door, beside the mail mode and default off; its warning shows
    // only while it is on, directly under it (`onboarding-provisioning.md`
    // § 5).
    let ssh_door_toggle = CheckButton::with_label(&resolve("onboarding.vps_config.ssh_door_label"));
    ssh_door_toggle.set_active(m.provision_ssh_door_enabled());
    set_test_id(&ssh_door_toggle, ids::VPS_CONFIG_SSH_DOOR_TOGGLE);
    let ssh_door_warning = Label::new(Some(&resolve("onboarding.vps_config.ssh_door_warning")));
    ssh_door_warning.set_wrap(true);
    ssh_door_warning.set_halign(gtk::Align::Start);
    ssh_door_warning.add_css_class("warning");
    set_test_id(&ssh_door_warning, ids::VPS_CONFIG_SSH_DOOR_WARNING);
    ssh_door_warning.set_visible(m.provision_ssh_door_enabled());
    {
        let m = m.clone();
        let syncing = syncing.clone();
        let ssh_door_warning = ssh_door_warning.clone();
        ssh_door_toggle.connect_toggled(move |c| {
            if !syncing.get() {
                m.set_provision_ssh_door(c.is_active());
            }
            ssh_door_warning.set_visible(c.is_active());
        });
    }
    provider_part.append(&ssh_door_toggle);
    provider_part.append(&ssh_door_warning);

    // Server-type radio container (≤5; indexed by position). No public test
    // ID: architectural rule #6 forbids per-app IDs and no test scopes
    // against this container.
    let server_types_box = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    provider_part.append(&server_types_box);

    let (channel_box, channel_radios) = build_update_channel_rows(m.clone());

    let refresh: Rc<dyn Fn()> = Rc::new(move || {
        let cfg = m.vps_config();
        syncing.set(true);
        let mail = m.provision_mail_mode_enabled();
        if mail_mode_toggle.is_active() != mail {
            mail_mode_toggle.set_active(mail);
        }
        let door = m.provision_ssh_door_enabled();
        if ssh_door_toggle.is_active() != door {
            ssh_door_toggle.set_active(door);
        }
        ssh_door_warning.set_visible(door);
        syncing.set(false);

        sync_update_channel_rows(&m, &channel_radios, m.provision_update_channel());

        // Reseed only when the option set actually changed — rebuilding the
        // model each tick would needlessly reset the selection. Index stays
        // 1:1 with cfg.locations so the handler can map selected() → loc.id.
        let want: Vec<&str> = cfg.locations.iter().map(|l| l.name.as_str()).collect();
        let have: Vec<String> = location_picker
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
            location_picker.set_model(Some(&gtk::StringList::new(&want)));
        }
        location_picker.set_visible(!cfg.locations.is_empty());
        if let Some(id) = cfg.selected_location_id.as_deref()
            && let Some(i) = cfg.locations.iter().position(|l| l.id == id)
            && location_picker.selected() != i as u32
        {
            location_picker.set_selected(i as u32);
        }

        // RAM gate: when mail is ON, only plans with mem_gb ≥ 2 are offered
        // (the scanners need the RAM). Filter before indexing so the radio
        // test-ids stay 0-based over the shown set, the same shape every app
        // renders (shared gate helper).
        rebuild_radio_group(
            &server_types_box,
            cfg.server_types
                .iter()
                .filter(|st| server_type_allowed_for_mail((*st).clone(), mail))
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
    });

    ServerChoices {
        provider_part,
        channel_box,
        refresh,
    }
}

/// The `vps-config-update-channel-row` container and one radio per channel,
/// keyed `vps-config-update-channel-row[<channel id>]`, in
/// `UpdateChannel::ALL` order. Each radio shows its label and one-line
/// description; a click calls the shared setter. Returns the radios so the
/// refresh closure can repaint the selection.
fn build_update_channel_rows(m: Arc<OnboardingMachine>) -> (gtk::Box, ChannelRows) {
    let container = gtk::Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&container, ids::VPS_CONFIG_UPDATE_CHANNEL_ROW);

    let heading = Label::new(Some(&resolve(
        "onboarding.vps_config.update_channel_heading",
    )));
    heading.add_css_class("heading");
    heading.set_halign(gtk::Align::Start);
    container.append(&heading);

    let selected = m.provision_update_channel();
    let syncing = Rc::new(Cell::new(false));
    let mut radios: Vec<(UpdateChannel, CheckButton)> = Vec::new();
    for c in UpdateChannel::ALL {
        let radio = match radios.first() {
            None => CheckButton::new(),
            Some((_, first)) => CheckButton::builder().group(first).build(),
        };
        radio.set_label(Some(&channel_row_label(&m, c)));
        set_test_id(
            &radio,
            &format!("vps-config-update-channel-row[{}]", c.id()),
        );
        // Exactly one on at any time; activate before connecting so seeding the
        // selection does not echo back into the setter.
        radio.set_active(c == selected);
        mark_radio_state(&radio, c == selected);
        {
            let m = m.clone();
            let syncing = syncing.clone();
            radio.connect_toggled(move |r| {
                if syncing.get() || !r.is_active() {
                    return;
                }
                m.set_provision_update_channel(c);
            });
        }
        container.append(&radio);
        radios.push((c, radio));
    }
    (container, ChannelRows { radios, syncing })
}

/// The channel radios, and the guard that keeps a repaint from reading as a
/// click: `set_active` on a grouped `CheckButton` fires `toggled`, and an
/// unguarded handler would record the repainted default as the user's
/// explicit choice — which no later registry reading can move again.
struct ChannelRows {
    radios: Vec<(UpdateChannel, CheckButton)>,
    syncing: Rc<Cell<bool>>,
}

/// One channel row's text. The shared getters pick the keys (the label's, and
/// the description's — the no-build wording when the registry has no build on
/// the channel, else the compat-line wording at the product major); the app
/// only resolves them.
fn channel_row_label(m: &OnboardingMachine, c: UpdateChannel) -> String {
    format!(
        "{} — {}",
        resolve(c.label_key()),
        resolve(&m.update_channel_description_key_for_row(c))
    )
}

/// Repaint the channel radios from the machine's selection and descriptions
/// (the shared getters are the only source; nothing is cached here — the
/// registry probe lands after the page is built, so the text follows it). A
/// repaint records nothing: the toggled handlers stand down while it runs.
fn sync_update_channel_rows(m: &OnboardingMachine, rows: &ChannelRows, selected: UpdateChannel) {
    rows.syncing.set(true);
    for (c, radio) in &rows.radios {
        let label = channel_row_label(m, *c);
        if radio.label().as_deref() != Some(label.as_str()) {
            radio.set_label(Some(&label));
        }
        let on = *c == selected;
        if radio.is_active() != on {
            radio.set_active(on);
        }
        mark_radio_state(radio, on);
    }
    rows.syncing.set(false);
}

/// Carry the row's `on`/`off` on an explicit `state` marker class —
/// `automation::agent::attr` prefers it to an unmarked `CheckButton`'s
/// `true`/`false`, and the cross-app test reads `get_attr(row, "state")`.
fn mark_radio_state(radio: &CheckButton, on: bool) {
    radio.remove_css_class("test-attr-state-on");
    radio.remove_css_class("test-attr-state-off");
    radio.add_css_class(if on {
        "test-attr-state-on"
    } else {
        "test-attr-state-off"
    });
}

/// Rebuild the per-provider section's widget tree (link, open-browser, help,
/// credentials form, verify button; the server choices are [`build_choices`]).
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

    // Credentials form — CredTarget::Vps routes change handlers to m.set_vps_cred.
    // The visible-field rule is the machine's (`visible_vps_fields()`: the
    // selected provider's fields filtered by `Capability::Vps`), not ours. This
    // used to be a hand-rolled copy of that filter *plus* a hand-rolled
    // `FieldMeta → FieldMetaPlain` mapping — a per-app re-derivation of a rule
    // that already had a shared getter (whose doc comment even says it exists
    // "so per-app view code is a one-liner instead of a duplicated
    // client-side filter"). dns_config next door always called its
    // `visible_dns_fields()` twin; this is the same call.
    let visible = m.visible_vps_fields();
    let (form, hosted_auth_buttons) = super::generic_provider_form::build_creds_only(
        &visible,
        m.clone(),
        &super::generic_provider_form::CredTarget::Vps,
    );
    set_test_id(&form, ids::VPS_CREDENTIALS_FORM);
    section.append(&form);

    let verify_btn = Button::with_label(&resolve("provisioning.verify_credentials"));
    set_test_id(&verify_btn, ids::VPS_VERIFY_BUTTON);
    {
        let m = m.clone();
        verify_btn.connect_clicked(move |_| {
            let m = m.clone();
            async_helper::run_on_tokio(async move { m.verify_vps().await }, |_| {});
        });
    }
    section.append(&verify_btn);

    hosted_auth_buttons
}

/// Per-tick reactive updates for widgets the section already owns. Walks
/// children matching by `widget_name()` (set via `set_test_id`); the server
/// choices refresh themselves ([`build_choices`]).
fn update_provider_section_state(section: &gtk::Box, m: Arc<OnboardingMachine>) {
    let mut child = section.first_child();
    while let Some(c) = child {
        if c.widget_name() == ids::VPS_VERIFY_BUTTON
            && let Ok(b) = c.clone().downcast::<Button>()
        {
            crate::possible::paint(&b, None, m.verify_vps_possibility());
        }
        child = c.next_sibling();
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
        mark_radio_state(&radio, active);
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

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_onboarding_machine::observer::NullObserver;

    /// Repainting the channel radios never records a choice: a page built once
    /// and shown again after a wizard reset (a radio other than the default
    /// still active) moves the selection back to the default without turning
    /// it into an explicit pick, so a later registry reading still moves the
    /// default (`onboarding-provisioning.md` § 5 → *What exists decides the
    /// default*).
    #[test]
    fn repainting_the_channel_rows_records_no_choice() {
        crate::testid::run_on_gtk_thread(|| {
            let m = OnboardingMachine::new(Arc::new(NullObserver));
            let (_root, refresh) = build(m.clone());
            m.set_provision_update_channel(UpdateChannel::Dev);
            refresh();
            m.reset();
            refresh();
            assert_eq!(
                m.vps_config().update_channel,
                None,
                "the repaint to the default recorded an explicit choice"
            );
            m.call_machine_method(
                "set_channel_availability_for_test".into(),
                r#"[{"channel":"stable","build":{"build":"no_build"}},{"channel":"test","build":{"build":"unknown"}},{"channel":"dev","build":{"build":"unknown"}}]"#.into(),
            );
            refresh();
            assert_eq!(m.provision_update_channel(), UpdateChannel::Test);
            assert!(m.vps_config().update_channel.is_none());
        });
    }
}
