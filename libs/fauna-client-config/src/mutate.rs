//! Mutation helpers for the backup-destination state and the muted-keywords
//! normalizer — the minimal in-place edits the client machines perform: the
//! backups page adds/edits/removes a destination on the box's
//! `fauna.state.backup` state, and every muted-words write normalizes its list
//! here. The helpers do **not** seal, sign or stamp — the plane door does.

use fauna_core::backup_state::BackupState;
use fauna_core::data::{BackupDestination, MutedKeyword, Timestamp, UnattestedVerdict};

// ── Backup-destination helpers ──
//
// Create/edit/remove a cross-location backup destination is a gesture on the
// box's `fauna.state.backup` state, written through
// [`crate::backup_store::mutate_backup`] — there is **no** destination CRUD
// WS-RPC (`docs/goal/behavior/backup-destinations.md` § State & data shape →
// Create / edit / remove protocol). The helpers edit a [`BackupState`] and
// stamp nothing: the plane door stamps the list above the stored row.
//
// A logical "destination" the user sees is the destination *nest*, identified
// by `destination_id`; it expands to one `BackupDestination` row per reserved
// folder kind backed up there (`__mail`, and future `__conv/<hex>` etc.), all
// sharing that `destination_id`. So `add` appends a single row (the caller adds
// one per reserved set — for v1 that is just `__mail`), while `edit` and
// `remove` operate on every row of a `destination_id` because the edited fields
// (label, URL) and the removal are nest-level.

/// Append one backup-destination row.
///
/// Mirrors `add_mail_credential`: a plain push. The caller is responsible for
/// `(destination_id, folder_name)` uniqueness (the enroll flow resolves the
/// destination identity first). A destination that backs up several reserved
/// sets calls this once per set, sharing the `destination_id`.
pub fn add_backup_destination(state: &mut BackupState, destination: BackupDestination) {
    state.backup.destinations.push(destination);
}

/// Update the user-facing label and URL on every row of the destination with
/// `destination_id`. Returns `true` if at least one row matched.
///
/// Edit changes only nest-level fields (`display_name`, `destination_nest_url`)
/// — `docs/goal/behavior/backup-destinations.md` § State & data shape → Edit. It does **not**
/// touch `destination_actor_pubkey`: a same-nest URL change keeps the identity,
/// and a *different* nest is handled by the dialog as remove + re-add, not edit.
pub fn edit_backup_destination(
    state: &mut BackupState,
    destination_id: &str,
    display_name: Option<String>,
    destination_nest_url: String,
) -> bool {
    let mut matched = false;
    for dest in state
        .backup
        .destinations
        .iter_mut()
        .filter(|d| d.destination_id == destination_id)
    {
        dest.display_name = display_name.clone();
        dest.destination_nest_url = destination_nest_url.clone();
        matched = true;
    }
    matched
}

/// Remove every row of the destination with `destination_id`. Returns `true`
/// if at least one row was removed.
///
/// **When the destination is under review, the removal IS the verdict** — the
/// *Remove* half of the post-succession adjudication pair, which reuses this
/// button rather than minting a second removal path
/// (`succession-aftermath.md` § Adjudicating what the aftermath carries across).
/// So every open mark on the destination is recorded `Removed` before the rows
/// go: the row is what a stale peer's newer list resurrects, and the recorded
/// verdict is what makes the read fold prune it again. An **ordinary**
/// removal (no open mark) writes no mark — there is no raising event to key a
/// verdict on, and latest-wins already propagates it.
pub fn remove_backup_destination(state: &mut BackupState, destination_id: &str) -> bool {
    for mark in state
        .marks
        .iter_mut()
        .filter(|m| m.destination_id == destination_id && m.verdict.is_open())
    {
        mark.verdict = UnattestedVerdict::Removed;
    }
    let before = state.backup.destinations.len();
    state
        .backup
        .destinations
        .retain(|d| d.destination_id != destination_id);
    state.backup.destinations.len() != before
}

/// Attach one ordinary folder to a destination: append the destination's
/// per-folder coverage row — the same identity fields as its existing rows,
/// with `folder_name` = the nest-derived `__folder/<hex>/<id>` set name
/// (`docs/goal/behavior/backup-destinations.md` § Ordinary-folder coverage).
/// Returns `false` — writing nothing — when the destination has no row to
/// clone the identity from, or when the coverage row already exists
/// (idempotent re-attach).
///
/// Deliberately **not** [`add_backup_destination`] at the call site: the row
/// model's per-rail dimension gains a second value here, and the identity
/// fields must be the enrolled destination's own, never caller-supplied — a
/// coverage row whose URL or pubkey drifted from its siblings would make one
/// "destination" dial two nests.
///
/// `label` is the folder's display name and sealed label as the owner's
/// coverage listing carries them ([`FolderCoverageLabel`]) — what the
/// nest-held pull-back names the restored folder from after a box loss. A
/// re-attach of an existing row refreshes it (a renamed folder's next attach
/// records the new name) and returns `true` when that changed the row; a label
/// field the listing did not carry erases nothing, exactly as the custodian
/// store keeps its name (`segment-backup-protocol.md` § *Where a restored
/// folder's name comes from*).
pub fn attach_backup_destination_folder(
    state: &mut BackupState,
    destination_id: &str,
    folder_set: &str,
    label: &FolderCoverageLabel,
) -> bool {
    let destinations = &mut state.backup.destinations;
    if let Some(row) = destinations
        .iter_mut()
        .find(|d| d.destination_id == destination_id && d.folder_name == folder_set)
    {
        let before = (row.folder_display_name.clone(), row.folder_label.clone());
        if label.display_name.is_some() {
            row.folder_display_name = label.display_name.clone();
        }
        if label.label.is_some() {
            row.folder_label = label.label.clone();
        }
        return (row.folder_display_name.clone(), row.folder_label.clone()) != before;
    }
    let Some(template) = destinations
        .iter()
        .find(|d| d.destination_id == destination_id)
        .cloned()
    else {
        return false;
    };
    destinations.push(BackupDestination {
        folder_name: folder_set.to_string(),
        added_at: Timestamp::now_secs().max(0) as u64,
        // The template may be another folder's coverage row: its label is
        // that folder's, never this one's.
        folder_display_name: label.display_name.clone(),
        folder_label: label.label.clone(),
        ..template
    });
    true
}

/// A covered folder's name as the owner's coverage listing carried it at
/// attach: the display name (absent once the source's row holds none) and the
/// set's `name_hash` + `name_sealed` pair. Both optional, and an absent field
/// never erases a recorded one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderCoverageLabel {
    pub display_name: Option<String>,
    pub label: Option<fauna_core::data::CoveredFolderLabel>,
}

impl FolderCoverageLabel {
    /// The label off one `fauna.backup.destination.list` coverage entry. A
    /// blank name, a hash that is not 32 bytes or an empty seal is no label,
    /// and so is one over its row cap (`fauna_core::backup_state`'s
    /// `MAX_COVERED_FOLDER_*`): the attach still lands, and the folder restores
    /// reported unnamed rather than the coverage row refusing the list write.
    pub fn of_listing(covered: &fauna_protocol::backup::CoveredFolder) -> Self {
        use fauna_core::backup_state::{
            MAX_COVERED_FOLDER_NAME_BYTES, MAX_COVERED_FOLDER_SEAL_BYTES,
        };
        let label = match (&covered.name_hash, &covered.name_sealed) {
            (Some(hash), Some(sealed))
                if !sealed.is_empty() && sealed.len() <= MAX_COVERED_FOLDER_SEAL_BYTES =>
            {
                <[u8; 32]>::try_from(&hash[..]).ok().map(|name_hash| {
                    fauna_core::data::CoveredFolderLabel {
                        name_hash,
                        name_sealed: sealed.to_vec(),
                    }
                })
            }
            _ => None,
        };
        Self {
            display_name: covered
                .name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty() && n.len() <= MAX_COVERED_FOLDER_NAME_BYTES)
                .map(str::to_string),
            label,
        }
    }
}

/// Detach one folder's coverage row — exactly the `(destination_id,
/// folder_name)` pair, never the destination's other rows. Returns `true` if a
/// row was removed.
///
/// Deliberately **not** [`remove_backup_destination`]: that helper is
/// destination-level and records a `Removed` adjudication verdict on the
/// destination's open unattested marks — a per-folder detach is not a verdict
/// about the destination and must not close its review.
pub fn detach_backup_destination_folder(
    state: &mut BackupState,
    destination_id: &str,
    folder_set: &str,
) -> bool {
    let before = state.backup.destinations.len();
    state
        .backup
        .destinations
        .retain(|d| !(d.destination_id == destination_id && d.folder_name == folder_set));
    state.backup.destinations.len() != before
}

// ── Post-succession adjudication (succession-aftermath.md § Re-key scope →
//    *Adjudicating what the aftermath carries across*) ──
//
// The seed is account access, so the seed thief the succession ceremony
// answers could have added a destination pointing at a box of their choosing
// at any point in the pre-succession window, indistinguishable from one the
// owner added. The ratified answer mirrors the MLS sweep's treatment of leaves
// it cannot vouch for: re-register immediately (backups restarting is the
// aftermath's whole point and must not become user-gated), then *report* every
// carried-across destination until the owner keeps or removes it. The raise is
// the post-store-ready pass's
// ([`crate::backup_store::raise_succession_destination_marks`]); the verdicts
// are the two helpers here.

/// Record the owner's **Keep** on every open mark of the destination with
/// `destination_id`. Returns `true` if at least one open mark was actually
/// closed, so a caller can tell a real adjudication from a no-op.
///
/// Keep closes *this* raising event, never the row forever: a later succession
/// raises its own mark, because a verdict about one compromise window cannot
/// vouch for the row across the next one.
///
/// Answers **the destination, not the row** — one destination the user sees
/// expands to one row per reserved folder kind backed up there, which is why
/// the mark is keyed on `destination_id` at all and why
/// [`edit_backup_destination`] edits every row of it.
///
/// ⚠ **Records the verdict; never deletes the mark** (2026-08-11, the lift onto
/// the shared adjudication encoding — [`keep_grant_mark`] says the same for its
/// plane). Deleting it made an answered mark indistinguishable from one never
/// raised, which broke both under a re-run of the raiser and across two of the
/// owner's devices.
pub fn keep_backup_destination(state: &mut BackupState, destination_id: &str) -> bool {
    let mut adjudicated = false;
    for mark in state
        .marks
        .iter_mut()
        .filter(|m| m.destination_id == destination_id && m.verdict.is_open())
    {
        mark.verdict = UnattestedVerdict::Kept;
        adjudicated = true;
    }
    adjudicated
}

// ── Tier-1 muted-keywords (frame Q3) ──
//
// The user's private, sealed muted-keywords word-list. Edited on the
// `muted-words` Settings sub-page (all 7 apps), applied client-side
// post-decrypt over conversation bodies via
// `fauna_core::keyword::body_excludes_matches`. The
// `fauna.state.moderation` entry's merge rule resolves concurrent edits across
// the device fleet.

/// Normalize a muted-keywords list to its canonical stored shape: trim each
/// term, drop blanks, collapse case-insensitive duplicates keeping the
/// first-seen entry (spelling and weight), and clamp each weight into
/// `[MUTED_KEYWORDS_PENALTY, 0]` (`fauna_core::scoring::clamp_muted_keyword_weight`).
/// Pure + order-preserving, so the stored list reads back in entry order
/// (minus trimmed noise and case-dupes).
///
/// Public because it is the ONE normalizer every writer shares
/// (`crate::preference_records::set_muted_keywords`, which the account-plane
/// surface `fauna_account_plane::preference_surfaces` applies before sealing its
/// plane entry), so every write stores the same bytes for the same input.
pub fn normalize_muted_keywords(keywords: Vec<MutedKeyword>) -> Vec<MutedKeyword> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for entry in keywords {
        let trimmed = entry.keyword.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Dedupe key is the lowercased term (matching's case-insensitive), but the
        // pushed value preserves the first-seen spelling for display.
        if seen.insert(trimmed.to_lowercase()) {
            out.push(MutedKeyword {
                keyword: trimmed.to_string(),
                weight: fauna_core::scoring::clamp_muted_keyword_weight(entry.weight),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(id: &str, url: &str, folder: &str, name: Option<&str>) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            destination_nest_url: url.to_string(),
            destination_actor_pubkey: [7u8; 32],
            folder_name: folder.to_string(),
            added_at: 1700000000,
            display_name: name.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn add_appends() {
        let mut st = BackupState::default();
        add_backup_destination(
            &mut st,
            dest("d1", "https://a.example/", "__mail", Some("A")),
        );
        assert_eq!(st.backup.destinations.len(), 1);
        assert_eq!(st.backup.destinations[0].destination_id, "d1");
    }

    #[test]
    fn edit_updates_label_and_url_on_matching_rows_only() {
        let mut st = BackupState::default();
        // Two rows of the same destination nest (different reserved sets) + an
        // unrelated destination.
        add_backup_destination(
            &mut st,
            dest("d1", "https://a.example/", "__mail", Some("A")),
        );
        add_backup_destination(
            &mut st,
            dest("d1", "https://a.example/", "__conv", Some("A")),
        );
        add_backup_destination(
            &mut st,
            dest("d2", "https://b.example/", "__mail", Some("B")),
        );

        let matched = edit_backup_destination(
            &mut st,
            "d1",
            Some("Renamed".to_string()),
            "https://a2.example/".to_string(),
        );
        assert!(matched);

        // Both d1 rows updated; d2 untouched.
        for d in st
            .backup
            .destinations
            .iter()
            .filter(|d| d.destination_id == "d1")
        {
            assert_eq!(d.display_name.as_deref(), Some("Renamed"));
            assert_eq!(d.destination_nest_url, "https://a2.example/");
            // pubkey + folder_name preserved (edit is nest-level only).
            assert_eq!(d.destination_actor_pubkey, [7u8; 32]);
        }
        let d2 = st
            .backup
            .destinations
            .iter()
            .find(|d| d.destination_id == "d2")
            .unwrap();
        assert_eq!(d2.display_name.as_deref(), Some("B"));
        assert_eq!(d2.destination_nest_url, "https://b.example/");
    }

    #[test]
    fn edit_missing_destination_returns_false() {
        let mut st = BackupState::default();
        add_backup_destination(&mut st, dest("d1", "https://a.example/", "__mail", None));
        assert!(!edit_backup_destination(
            &mut st,
            "nope",
            None,
            "https://x/".to_string()
        ));
    }

    #[test]
    fn remove_drops_all_rows_of_destination_and_returns_bool() {
        let mut st = BackupState::default();
        add_backup_destination(&mut st, dest("d1", "https://a.example/", "__mail", None));
        add_backup_destination(&mut st, dest("d1", "https://a.example/", "__conv", None));
        add_backup_destination(&mut st, dest("d2", "https://b.example/", "__mail", None));

        assert!(remove_backup_destination(&mut st, "d1"));
        assert_eq!(st.backup.destinations.len(), 1);
        assert_eq!(st.backup.destinations[0].destination_id, "d2");

        // No-op remove still returns false.
        assert!(!remove_backup_destination(&mut st, "d1"));
    }
}
