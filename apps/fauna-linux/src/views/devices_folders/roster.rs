use fauna_ui_ids as ids;
use std::sync::Arc;

use adw::prelude::*;

use fauna_devices_machine::{DeviceSummary, DevicesMachine};

use crate::async_helper;
use crate::i18n::strings;
use crate::i18n::strings::{common, status};
use crate::testid::set_test_id;

/// Build the page-level "peer identity" section — a single, non-per-row copy
/// control for THIS client's own actor ID (`devices.md` § Layout & flow point
/// 2; ui.yaml `peer-actor-id-copy-btn` is `indexed: false`), for handing to a
/// new device being paired. Mirrors the Settings → Account identity row
/// (`views/status.rs`'s `status-actor-id-copy-btn`).
///
pub fn build_identity_section(actor_id: Option<&str>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(common::IDENTITY)
        .build();
    let row = adw::ActionRow::builder()
        .title(common::ACTOR_ID)
        .subtitle(actor_id.unwrap_or(status::identity::NOT_CONFIGURED))
        .subtitle_selectable(true)
        .build();
    let copy_btn = gtk::Button::with_label(strings::devices::COPY_ACTOR_ID);
    copy_btn.add_css_class("flat");
    set_test_id(&copy_btn, ids::PEER_ACTOR_ID_COPY_BTN);
    match actor_id.map(str::to_string) {
        Some(id) => {
            copy_btn.connect_clicked(move |_| crate::clipboard::copy_text(&id));
        }
        // No id minted yet — a control that cannot succeed is not offered.
        None => copy_btn.set_sensitive(false),
    }
    row.add_suffix(&copy_btn);
    group.add(&row);
    group
}

/// Build the devices section — a PreferencesGroup containing the device list box.
///
/// The list starts empty; the Devices sub-page render loop calls [`update_device_list`]
/// on every `DevicesMachine` observer tick to populate cards from
/// `snapshot.devices`.
pub fn build_devices_section() -> (adw::PreferencesGroup, gtk::ListBox) {
    // --- Device list ---
    let device_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let placeholder = adw::StatusPage::builder()
        .title(strings::devices::NO_DEVICES)
        .icon_name("computer-symbolic")
        .build();
    device_list.set_placeholder(Some(&placeholder));

    let devices_group = adw::PreferencesGroup::builder()
        .title(strings::devices::MY_DEVICES)
        .build();
    devices_group.add(&device_list);

    (devices_group, device_list)
}

/// Rebuild the device list from `snapshot.devices`. Called from the Devices sub-page
/// render loop on every `DevicesMachine` observer tick. Each device becomes a
/// card with name, online/offline status, and a remove button that forwards its
/// list index to `DevicesMachine::remove_device`.
pub fn update_device_list(
    list_box: &gtk::ListBox,
    devices: &[DeviceSummary],
    machine: &Arc<DevicesMachine>,
    local_device_id: Option<&str>,
    keyed_principals: Option<&std::collections::BTreeSet<[u8; 32]>>,
) {
    // Clear existing rows.
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    for (index, device) in devices.iter().enumerate() {
        let row = build_device_card(
            index as u32,
            device,
            machine,
            local_device_id,
            keyed_principals,
        );
        list_box.append(&row);
    }
}

/// Build a single device card row. `index` is the device's position in the
/// snapshot list — the key `DevicesMachine::remove_device` resolves to a device
/// id.
fn build_device_card(
    index: u32,
    device: &DeviceSummary,
    machine: &Arc<DevicesMachine>,
    local_device_id: Option<&str>,
    keyed_principals: Option<&std::collections::BTreeSet<[u8; 32]>>,
) -> gtk::ListBoxRow {
    // `accessible_role(Group)` at construction: a plain `gtk::Box` defaults
    // to role `Generic`, which Linux AT-SPI omits from the tree under some
    // compositors, so `set_test_id`'s Description never resolves. `Group`
    // keeps the card discoverable (same pattern as the onboarding provider
    // rows; a known Linux AT-SPI Box-discoverability quirk, tracked internally).
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    set_test_id(&card, ids::DEVICE_CARD);

    // --- Left: name + status ---
    let info = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .hexpand(true)
        .build();

    let name_label = gtk::Label::builder()
        .label(&device.label)
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    set_test_id(&name_label, ids::DEVICE_NAME);

    // Shared online→label map (`devices.online` / `devices.offline`); the dot
    // *color* below stays a per-app render. See devices.md § Where logic lives.
    let status_text =
        fauna_core::format::device_status_label(device.online).resolve(strings::lookup);
    let status_css: &[&str] = if device.online {
        &["success"]
    } else {
        &["dim-label"]
    };
    let status_label = gtk::Label::builder()
        .label(status_text)
        .halign(gtk::Align::Start)
        .css_classes(status_css)
        .build();
    set_test_id(&status_label, ids::DEVICE_STATUS);

    info.append(&name_label);
    info.append(&status_label);

    // The guardian-enrolled-device marker (`family-safety.md` § Full visibility
    // for young children, Slice F). The ward's OWN list renders it — the child
    // must always see which device their guardian enrolled (transparency by
    // construction); always false on an unsupervised account, so the badge is
    // simply absent there.
    if device.guardian_marked {
        let badge = gtk::Label::builder()
            .label(strings::devices::GUARDIAN_MARKED_BADGE)
            .halign(gtk::Align::Start)
            .css_classes(["caption", "accent"])
            .build();
        set_test_id(&badge, ids::DEVICE_GUARDIAN_MARK_BADGE);
        info.append(&badge);
    }

    // Not mutually exclusive with the guardian badge above — a guardian
    // marking their own enrolled device can legitimately carry both
    // (`devices.md` § This-device marker).
    if local_device_id == Some(device.device_id.as_str()) {
        let badge = gtk::Label::builder()
            .label(strings::devices::THIS_DEVICE_BADGE)
            .halign(gtk::Align::Start)
            .css_classes(["caption", "accent"])
            .build();
        set_test_id(&badge, ids::DEVICE_THIS_MARK_BADGE);
        info.append(&badge);
    }

    // The keyless-posture marker (`devices.md` § Custody facet piece 1):
    // DERIVED bundle-key reach — the row's enrolled principal holds no
    // generation wrap at the resolved tip. The join and its fail-safes (no
    // tip, a row with no enrolled principal, an undecodable principal — each marking nothing) are
    // the shared `keyless_posture` rule's. Never stored or asked: no toggle.
    if fauna_devices_machine::keyless_posture(keyed_principals, device.principal.as_deref()) {
        let badge = gtk::Label::builder()
            .label(strings::devices::KEYLESS_POSTURE_BADGE)
            .halign(gtk::Align::Start)
            .css_classes(["caption", "dim-label"])
            .build();
        set_test_id(&badge, ids::DEVICE_KEYLESS_POSTURE_BADGE);
        info.append(&badge);
    }

    // One chip per folder this device carries, stating its place in that set
    // (`DeviceSummary.folders`, the roster slice's own field) — composed from
    // the SAME place labels the create wizard's checkboxes carry
    // (`fauna_core::format::device_place_label`, nested resolve). Indexed: a device in
    // three sets paints three chips; nothing renders when the device carries
    // no sets. Reference: apple `DevicesContent.swift`'s `RoleBadge` row
    // (`devices.md` § Element table — `device-folder-role-badge`).
    for fs in &device.folders {
        let label_text =
            fauna_core::format::device_place_label(fs.originates, fs.accepts, fs.applies_deletes)
                .resolve_nested(strings::lookup);
        let chip = gtk::Label::builder()
            .label(label_text)
            .halign(gtk::Align::Start)
            .css_classes(["caption", "accent"])
            .build();
        set_test_id(&chip, ids::DEVICE_FOLDER_ROLE_BADGE);
        info.append(&chip);
    }

    // `device-p2p-participation-toggle` (`p2p.md` § Per-device participation
    // — rule 5's off switch; ID user-approved 2026-09-25). Drawn exactly as
    // the machine painted the row — own-ness, checked, label, actionable —
    // so the app never re-derives which row is its own; the machine also
    // picks the arm the click takes and paints any refusal on
    // `error-message`. Reference: tui `settings/devices.rs`.
    let paint = device.participation_paint();
    let participation = gtk::CheckButton::builder()
        .label(paint.label.resolve(strings::lookup))
        .active(paint.checked)
        .sensitive(paint.actionable)
        .halign(gtk::Align::Start)
        .build();
    set_test_id(&participation, ids::DEVICE_P2P_PARTICIPATION_TOGGLE);
    {
        let machine = Arc::clone(machine);
        let on = !paint.checked;
        // The gesture re-snapshots through the machine's observer tick
        // (success refreshes, a refusal sets the error), which rebuilds this
        // card from the new paint — GTK's own flip of the check is never
        // the state of record.
        participation.connect_toggled(move |_| {
            let machine = Arc::clone(&machine);
            async_helper::run_on_tokio(
                async move { machine.set_p2p_participation(index, on).await },
                |_| {},
            );
        });
    }
    info.append(&participation);

    // --- Right: remove button ---
    // `set_test_id` also sets the tooltip (to the ID) — for an icon-only
    // button that's the AT-SPI accessible name, which the bridge matches
    // on — so we don't set a separate `tooltip_text`.
    let remove_btn = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    set_test_id(&remove_btn, ids::DEVICE_REMOVE_BUTTON);
    crate::offline_gate::declare_wire_kind(&remove_btn, "fauna.sync.devices.delete");

    let machine = Arc::clone(machine);
    remove_btn.connect_clicked(move |_| {
        let machine = Arc::clone(&machine);
        // `remove_device` deletes the device then self-refreshes (observer tick),
        // so the page re-renders without the row.
        async_helper::run_on_tokio(async move { machine.remove_device(index).await }, |_| {});
    });

    card.append(&info);
    card.append(&remove_btn);

    gtk::ListBoxRow::builder()
        .child(&card)
        .activatable(false)
        .build()
}
