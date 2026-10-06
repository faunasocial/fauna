//! The `fauna.state.folder-keys` size pins (`config-dissolution.md` § Phases
//! and gates → *Bounded rows*): every row family, well past the widest a
//! production writer produces it, seals under HALF the plane's per-entry cap —
//! measured with the writer door's own `sealed_envelope_len`,
//! generation-sealed as the kind is, and printed so a regression shows its
//! bytes.
//!
//! No family grows with use: a set's generation history — the part that does,
//! one generation per member removal, never prunable — is one row per
//! generation, so the pin that matters is that a whole set of any history
//! length is many rows of one fixed size. The staging's MLS Remove commit is
//! the one field sized by something other than the value's shape (the
//! group's ratchet tree); it is pinned at 16 KiB, several times a Remove
//! commit in a full thousand-member tree.

use fauna_core::account_entry_crypto::{EntryPlaintext, sealed_envelope_len};
use fauna_core::data::{
    FolderKeyCustody, FolderPendingRemoval, FoldersConfig, ForeignFolder, ForeignResidency,
};
use fauna_core::folder_key_rows::FolderKeyRecord;
use fauna_core::folder_keys::{ContentKeyGeneration, FolderContentKeys};
use fauna_core::identity::ActorId;
use fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES;
use fauna_protocol::merge_policy::KIND_FOLDER_KEYS;

/// The sealed length of `record`'s row, asserted under half the cap.
fn assert_row_fits(what: &str, key: &str, record: &FolderKeyRecord) -> usize {
    let plaintext = EntryPlaintext {
        kind: KIND_FOLDER_KEYS.to_string(),
        key: key.to_string(),
        merge_meta: None,
        value: record.encode().expect("encode").into(),
        tombstone: false,
    };
    let sealed = sealed_envelope_len(&plaintext, true).expect("measure");
    eprintln!(
        "size pin: the {what} row seals to {sealed} B (half cap {} B)",
        MAX_STATE_ENTRY_BYTES / 2
    );
    assert!(
        sealed <= MAX_STATE_ENTRY_BYTES / 2,
        "the {what} row {key} seals to {sealed} B, over half the {MAX_STATE_ENTRY_BYTES} B cap"
    );
    sealed
}

fn generation(version: u64) -> ContentKeyGeneration {
    ContentKeyGeneration {
        version,
        key: [0xAB; 32].into(),
        rotated_at: u64::MAX - 2_000 + version,
    }
}

#[test]
fn folder_keys_rows_seal_under_half_the_entry_cap() {
    let long = "n".repeat(4096);
    // A set with 1,000 member removals behind it, a 4 KiB name, every
    // optional field filled.
    // The canonical shape `rotate` and the merge keep: newest current, the
    // rest most-recent first.
    let keys = FolderContentKeys {
        current: generation(1_000),
        prior: (1..1_000u64).rev().map(generation).collect(),
    };
    let custody = FoldersConfig {
        sets: vec![FolderKeyCustody {
            channel_id: Some([0xC1; 32]),
            keys: Some(keys),
            set_nonce: Some([0x01; 32]),
            name: Some(long.clone()),
            created_at: u64::MAX,
            retired_at: Some(u64::MAX),
            lifted_at: Some(u64::MAX),
            minted_by: Some(ActorId([0xFF; 32])),
            replaces: Some([0xFF; 32]),
            retired_by_pick: Some([0xFF; 32]),
            served_at: Some(u64::MAX),
            unserved_at: Some(u64::MAX),
        }],
        pending_removals: vec![FolderPendingRemoval {
            channel_id: [0xC1; 32],
            name: long.clone(),
            removed_member: ActorId([0x55; 32]),
            new_generation: generation(1_001),
            commit: Some(vec![0xC0; 16 * 1024]),
            gated_attempted: true,
        }],
        foreign_sets: vec![ForeignFolder {
            channel_id: [0x77; 32],
            mls_group_id: vec![0x78; 256],
            home_nest_url: format!("https://{long}.example"),
            home_nest_actor_id: Some("a".repeat(64)),
            set_name: Some(long.clone()),
            access: Some("writer".into()),
            content_key_floor: Some(u64::MAX),
            residency: Some(ForeignResidency {
                metadata_only: true,
                stamped_at: u64::MAX,
            }),
            owner_handle: Some(long.clone()),
            owner_domain: Some(format!("{long}.example")),
            accepted_at: u64::MAX,
            left_at: Some(u64::MAX),
            updated_at: u64::MAX,
        }],
    };
    let rows = custody.rows().expect("rows");
    assert_eq!(rows.len(), 1 + 1_000 + 1 + 1, "one row per generation");
    let mut widest_generation = 0;
    for (key, record) in &rows {
        let what = match record {
            FolderKeyRecord::Set(_) => "set (4 KiB name)",
            FolderKeyRecord::Generation(_) => "generation",
            FolderKeyRecord::Removal(_) => "staging (4 KiB name, 16 KiB commit)",
            FolderKeyRecord::Foreign(_) => "foreign set (4 KiB url and name)",
        };
        let sealed = assert_row_fits(what, key, record);
        if let FolderKeyRecord::Generation(_) = record {
            widest_generation = widest_generation.max(sealed);
        }
    }
    // A generation row is small and fixed: a thousand of them is a thousand
    // rows of the same size, never one row growing toward the cap.
    assert!(
        widest_generation < 512,
        "a generation row seals to {widest_generation} B"
    );
}
