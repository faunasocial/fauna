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
//! A **group set's** review rows continue the same flat list (`group-set.md`
//! § Build design — the group set, decision 9): no nest holds them, so the
//! sync agent reads them off the set's own state DB on the status tick
//! (`crate::sync_agent::set_conflicts`), the shared
//! `ConflictSummary::of_retained` maps each once, `conflict-file-info` names
//! the set, and the button runs `RestoreRetainedVersion` on the agent, keyed
//! by (set, row id) — never `DevicesMachine::use_other_version`, which holds
//! no such row ([`Restore`]).
//!
//! The legacy keep-local/remote/both `conflict-resolution-panel` and the
//! per-candidate chooser this file used to render are retired with the
//! auto-resolve track (ui/folders.md § Conflicts).

use fauna_ui_ids as ids;
use std::sync::Arc;

use adw::prelude::*;

use fauna_devices_machine::{ConflictSummary, DevicesMachine};

use super::folders::GestureErrorSlot;
use crate::async_helper;
use crate::i18n::strings;
use crate::testid::set_test_id;

/// What a row's `conflict-resolve-button` runs — the one thing the two set
/// families do not share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Restore {
    /// A nest set's row: the § File Versions re-point through the devices
    /// machine, addressed by the nest's conflict id.
    Nest,
    /// A group set's row: the agent's restore of the edit this device kept,
    /// addressed by the set's `FolderRef` wire form and the row's local id.
    Retained { folder_id: String },
}

/// One row of the review list: the shared summary and its gesture.
pub struct ReviewRow {
    pub summary: ConflictSummary,
    pub restore: Restore,
}

/// The page's flat review list: the nest's rows, then each group set's (in the
/// order the sets are listed), every group row mapped through the shared
/// `ConflictSummary::of_retained` under its set's page label.
#[cfg_attr(not(feature = "p2p-share"), allow(unused_variables))]
pub fn review_rows(
    nest: &[ConflictSummary],
    #[cfg(feature = "p2p-share")] group_scopes: &[crate::offline_share::GroupScopeView],
    #[cfg(feature = "p2p-share")] set_conflicts: &fauna_client_sync::agent::SetConflicts,
) -> Vec<ReviewRow> {
    let mut rows: Vec<ReviewRow> = nest
        .iter()
        .map(|c| ReviewRow {
            summary: c.clone(),
            restore: Restore::Nest,
        })
        .collect();
    #[cfg(feature = "p2p-share")]
    for scope in group_scopes {
        let folder_id = scope.folder_id();
        let label = super::folders::group_scope_label(scope);
        for row in set_conflicts.get(&folder_id).into_iter().flatten() {
            rows.push(ReviewRow {
                summary: ConflictSummary::of_retained(&label, row),
                restore: Restore::Retained {
                    folder_id: folder_id.clone(),
                },
            });
        }
    }
    rows
}

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

/// Rebuild the review list from [`review_rows`], toggling the enclosing
/// `group`'s visibility on whether any remain. `error_slot` is the page's own
/// `error-message` slot, where a group restore's refusal lands (the machine
/// never sees that gesture).
pub fn update_conflict_list(
    group: &adw::PreferencesGroup,
    list_box: &gtk::ListBox,
    rows: &[ReviewRow],
    machine: &Arc<DevicesMachine>,
    error_slot: &GestureErrorSlot,
) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    group.set_visible(!rows.is_empty());
    for row in rows {
        list_box.append(&build_conflict_row(row, machine, error_slot));
    }
}

fn build_conflict_row(
    row: &ReviewRow,
    machine: &Arc<DevicesMachine>,
    error_slot: &GestureErrorSlot,
) -> gtk::ListBoxRow {
    let c = &row.summary;
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

    // `has_other_version` alone gates the button: a nest row derives it true
    // only with a winning head, and a group set's row whose winner is a delete
    // has no head and still retains the edit (tui's `conflict_elements`).
    if c.has_other_version {
        card.append(&match &row.restore {
            Restore::Nest => use_other_version_button(machine, c.id),
            Restore::Retained { folder_id } => {
                restore_retained_button(folder_id.clone(), c.id, error_slot)
            }
        });
    } else {
        match &c.winning_manifest_hash {
            // Resolved but nothing to re-point at (candidate-free (mark-only)
            // resolve) — nothing actionable; version history is the finer surface.
            Some(_) => {}
            // Unresolved report — informational only; the detecting
            // device resolves it. No blocking chooser here.
            None => {
                let awaiting = gtk::Label::builder()
                    .label(strings::devices::conflicts::AWAITING_DEVICE)
                    .halign(gtk::Align::Start)
                    .css_classes(["caption", "dim-label"])
                    .build();
                card.append(&awaiting);
            }
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

/// The `conflict-resolve-button` on a group set's review row: the agent
/// restores the edit this device kept (`crate::sync_agent::restore_retained_version`).
/// No wire kind to declare — the gesture never leaves the device, it rides the
/// local agent socket. The row goes when the agent's re-read no longer holds it;
/// a refusal (the bytes are gone, or the disk holds an edit not yet lifted)
/// writes nothing, keeps the row and lands on the page's `error-message`.
fn restore_retained_button(
    folder_id: String,
    conflict_id: i64,
    error_slot: &GestureErrorSlot,
) -> gtk::Button {
    let btn = gtk::Button::builder()
        .label(strings::devices::conflicts::USE_OTHER_VERSION)
        .halign(gtk::Align::Start)
        .css_classes(["flat"])
        .build();
    set_test_id(&btn, ids::CONFLICT_RESOLVE_BUTTON);
    let error_slot = error_slot.clone();
    btn.connect_clicked(move |_| {
        error_slot.clear();
        let error_slot = error_slot.clone();
        crate::sync_agent::restore_retained_version(
            folder_id.clone(),
            conflict_id,
            move |reason| error_slot.hold(strings::devices::error_use_other_version(&reason)),
        );
    });
    btn
}

#[cfg(all(test, feature = "p2p-share"))]
mod tests {
    use super::*;

    fn scope(byte: u8, name: Option<&str>) -> crate::offline_share::GroupScopeView {
        crate::offline_share::GroupScopeView {
            scope_id: [byte; 32],
            name: name.map(str::to_string),
            short_id: "abcd".to_string(),
            member_count: 2,
            shared_by: None,
        }
    }

    /// A group set's row joins the flat list after the nest's, through the
    /// shared mapping, its gesture keyed by the set (`group-set.md` § Build
    /// design — the group set, decision 9).
    #[test]
    fn a_group_review_row_follows_the_nest_rows_and_restores_by_set() {
        let holiday = scope(0x71, Some("Holiday"));
        let folder_id = holiday.folder_id();
        let mut readings = fauna_client_sync::agent::SetConflicts::new();
        readings.insert(
            folder_id.clone(),
            vec![fauna_core::sync::RetainedConflict {
                id: 9,
                path: "same.txt".into(),
                winner_manifest: Some("abcdef0123456789".into()),
                ..Default::default()
            }],
        );
        let nest = ConflictSummary::of_retained("nest-set", &Default::default());
        let rows = review_rows(&[nest], &[holiday], &readings);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].restore, Restore::Nest);
        assert_eq!(rows[1].restore, Restore::Retained { folder_id });
        assert_eq!(rows[1].summary.id, 9);
        assert_eq!(rows[1].summary.file_info, "Holiday: same.txt → abcdef01");
        assert!(rows[1].summary.has_other_version);
    }

    /// A winning delete leaves no head, and the kept edit is still offered.
    #[test]
    fn a_group_review_row_over_a_winning_delete_keeps_the_gesture() {
        let set = scope(0x72, None);
        let mut readings = fauna_client_sync::agent::SetConflicts::new();
        readings.insert(
            set.folder_id(),
            vec![fauna_core::sync::RetainedConflict {
                id: 3,
                path: "gone.txt".into(),
                ..Default::default()
            }],
        );
        let rows = review_rows(&[], &[set], &readings);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].summary.winning_manifest_hash.is_none());
        assert!(rows[0].summary.has_other_version);
    }

    /// A reading for a set the page does not list renders nothing.
    #[test]
    fn a_reading_for_an_unlisted_set_renders_no_row() {
        let mut readings = fauna_client_sync::agent::SetConflicts::new();
        readings.insert(
            scope(0x73, None).folder_id(),
            vec![fauna_core::sync::RetainedConflict::default()],
        );
        assert!(review_rows(&[], &[scope(0x74, None)], &readings).is_empty());
    }
}
