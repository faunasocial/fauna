//! Conflict **review list** for the Folders sub-page — rendered off
//! `DevicesSnapshot.conflicts` (fetched with `include_resolved`). Conflicts
//! auto-resolve on the detecting device (file-sync.md § Conflicts, ratified
//! 2026-07-10): a clean text three-way merge where possible, else
//! latest-writer-wins, with the losing version always retained in version
//! history — so nothing here blocks on the user. Each auto-resolved row shows
//! the resolution (`conflict-type-badge`: "Merged" / "Latest kept"), the file +
//! winner (`conflict-file-info`), and a one-tap **"use the other version"**
//! (`conflict-resolve-button`) that re-points the file at the retained losing
//! version via `DevicesMachine::use_other_version` (the § File Versions restore
//! record — itself reversible).
//!
//! A still-unresolved row (the sync engine's report when its local-version
//! upload fails) renders informationally — conflict type, file,
//! and an "awaiting device" caption, no button: resolution happens on the
//! detecting device, though another device can still resolve it
//! (`fauna.sync.conflicts.resolve`).
//!
//! The legacy keep-local/remote/both `conflict-resolution-panel` and the
//! per-candidate chooser this file used to render are retired with the
//! auto-resolve track (ui/folders.md § Conflicts).

use fauna_ui_ids as ids;
use std::sync::Arc;

use adw::prelude::*;

use fauna_devices_machine::{ConflictSummary, DevicesMachine};

use crate::async_helper;
use crate::i18n::strings;
use crate::testid::set_test_id;

/// Build the conflicts section — a `PreferencesGroup` over a `ListBox`. The
/// group is hidden by [`update_conflict_list`] whenever no conflicts exist
/// (devices.md § Layout & flow: "shown when sync conflicts exist").
pub fn build_conflicts_section() -> (adw::PreferencesGroup, gtk::ListBox) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();

    let group = adw::PreferencesGroup::builder()
        .title(strings::devices::conflicts::SECTION_TITLE)
        .visible(false)
        .build();
    group.add(&list);
    (group, list)
}

/// Rebuild the review list from `snapshot.conflicts`, toggling the enclosing
/// `group`'s visibility on whether any remain.
pub fn update_conflict_list(
    group: &adw::PreferencesGroup,
    list_box: &gtk::ListBox,
    conflicts: &[ConflictSummary],
    machine: &Arc<DevicesMachine>,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    group.set_visible(!conflicts.is_empty());
    for c in conflicts {
        list_box.append(&build_conflict_row(c, machine));
    }
}

fn build_conflict_row(c: &ConflictSummary, machine: &Arc<DevicesMachine>) -> gtk::ListBoxRow {
    // `accessible_role(Group)` so AT-SPI keeps the card discoverable (same
    // rationale as the device cards — see roster.rs).
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();

    // Badge: the resolution on a review row; the conflict type on an
    // unresolved row. Derived by the shared `conflict_badge_label` (the single
    // source across all seven apps — folders.md § Where logic lives); an
    // unknown/live type degrades to the raw wire string via `resolve`'s
    // missing-key fallback, exactly as the old local `_ => raw` arm did.
    let badge_text = fauna_folders_machine::conflict_badge_label(
        c.resolution.as_deref(),
        c.resolved_at,
        &c.conflict_type,
    )
    .resolve(strings::lookup);
    let badge = gtk::Label::builder()
        .label(&badge_text)
        .halign(gtk::Align::Start)
        .css_classes(if c.resolved_at.is_some() {
            ["heading", "success"]
        } else {
            ["heading", "warning"]
        })
        .build();
    set_test_id(&badge, ids::CONFLICT_TYPE_BADGE);
    card.append(&badge);

    // File info: path (+ the winning head on a resolved row) — precomputed on
    // the snapshot transcribe (`ConflictSummary::file_info`), rendered verbatim.
    let info = gtk::Label::builder()
        .label(&c.file_info)
        .halign(gtk::Align::Start)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    set_test_id(&info, ids::CONFLICT_FILE_INFO);
    card.append(&info);

    match (&c.winning_manifest_hash, c.has_other_version) {
        // Auto-resolved with a retained loser: the one-tap re-point.
        (Some(_), true) => card.append(&use_other_version_button(machine, c.id)),
        // Resolved but nothing to re-point at (candidate-free (mark-only) resolve) —
        // nothing actionable; version history is the finer surface.
        (Some(_), false) => {}
        // Unresolved report — informational only; the detecting
        // device resolves it. No blocking chooser here.
        (None, _) => {
            let awaiting = gtk::Label::builder()
                .label(strings::devices::conflicts::AWAITING_DEVICE)
                .halign(gtk::Align::Start)
                .css_classes(["caption", "dim-label"])
                .build();
            card.append(&awaiting);
        }
    }

    gtk::ListBoxRow::builder()
        .child(&card)
        .activatable(false)
        .build()
}

/// The `conflict-resolve-button`, re-purposed as the review re-point: forwards
/// `(conflict_id, this device's id)` to `DevicesMachine::use_other_version`
/// (which picks the latest retained non-winning candidate and records the
/// shared restore). Self-refreshes on success (observer tick); a failure lands
/// in the page `error-message` via the machine.
fn use_other_version_button(machine: &Arc<DevicesMachine>, id: i64) -> gtk::Button {
    let btn = gtk::Button::builder()
        .label(strings::devices::conflicts::USE_OTHER_VERSION)
        .halign(gtk::Align::Start)
        .css_classes(["flat"])
        .build();
    set_test_id(&btn, ids::CONFLICT_RESOLVE_BUTTON);
    // ⚠ The re-point rides the CHANGE-RECORD seam, not the conflict plane:
    // `DevicesMachine::use_other_version` calls `restore_file_version` →
    // `SyncClient::restore_version` → `changes_record`, and never
    // `resolve_conflict` (whose `fauna.sync.conflicts.resolve` is reachable
    // only from `DevicesMachine::resolve_conflict`, which no app calls today).
    // So this button declares what Media's restore-confirm declares — the same
    // shared re-point seam, one kind (`views/media/detail.rs`).
    crate::offline_gate::declare_wire_kind(&btn, "fauna.sync.changes.record");
    let machine = Arc::clone(machine);
    btn.connect_clicked(move |_| {
        // The device-id derivation is the only client glue (same idiom as the
        // Media restore); the restore record is attributed to this device.
        let device_id = match crate::sync::device_id() {
            Ok(raw) => fauna_core::hex32::encode(&raw),
            Err(e) => {
                tracing::warn!("use_other_version: no sync device id: {e}");
                return;
            }
        };
        let machine = Arc::clone(&machine);
        async_helper::run_on_tokio(
            async move { machine.use_other_version(id, device_id).await },
            |_| {},
        );
    });
    btn
}
