//! The client-side account-state core: the shared seams every feature machine
//! injects to read and write the account plane's kinds ([`store_seam`] — the
//! succession ledger, the deployment seeds, the custody ceremony, the follows,
//! the preference cluster, the backup state, the mail custody), the
//! storage-agnostic record edits behind them ([`preference_records`], the
//! backup-destination helpers), and the legs built on them (backup enrollment,
//! the deployment-seed custody leg, the succession raises, box recovery).
//!
//! Every value here rests on the account plane. The `__config` blob this crate
//! once sealed and stored over `fauna.config.{get,put}` retired at closure step
//! (6) (`config-dissolution.md` § The `__config` dissolution schedule → *The
//! closure order*).

// The cross-crate uniffi namespace for this crate's records (today only
// `MutedWordsSnapshot`, the muted-words page read). Feature-gated so it exists
// only in the native FFI build — `fauna-ffi`'s `muted-keywords` feature turns it
// on, and the Go mail-bridge's `--no-default-features` build leaves it off, so
// the checked-in Go bindings stay byte-identical. Mirrors
// `fauna-client-mail-settings` / the machine crates.
#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!("fauna_client_config");

mod backup_enroll;
mod backup_store;
mod custody_leg;
#[cfg(feature = "account-port")]
pub mod custody_port;
mod dav_context;
mod filter_marks;
mod follows;
#[cfg(feature = "account-port")]
pub mod follows_port;
mod grant_marks;
#[cfg(feature = "account-port")]
pub mod mail_port;
mod member_reviews;
mod mutate;
mod muted_words;
mod nostr_npub_confirm;
pub mod preference_records;
mod recovery;
mod rotate;
mod store_seam;
mod succession_aftermath;
#[cfg(feature = "account-port")]
pub mod succession_ledger_port;
#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers;
#[cfg(test)]
mod test_nest;

pub use backup_enroll::{
    CustodianEnrollError, CustodianEnrollment, DEFAULT_BACKUP_FOLDER, DeregisterError, EnrollError,
    FolderCoverageError, FolderDestinationPlace, ReconcileError, ResolvedDestination,
    StatusReadError, attach_folder_to_destination, custodian_reenrollment,
    deregister_backup_destination, detach_folder_from_destination, enroll_backup_destination,
    enroll_client_custodian, keep_backup_destination_at_rest, list_folder_destinations,
    read_backup_status, reconcile_backup_enrollment, reenroll_custodian_after_reseed,
};
pub use backup_store::{
    BackupWriteError, load_backup_state, load_backup_state_refiled, mutate_backup,
    raise_succession_destination_marks, refile_rotated_box_list,
};
pub use custody_leg::{
    CustodyLegReport, CustodySeedFold, DeploymentSeedCustody, LinkedCustodyLeg,
    PlaneSeedRotationError, SEED_PUBLISH_WAIT, StoreFold, SupersessionMarks, custody_leg_warning,
    rotate_deployment_seed_on_plane, run_deployment_seed_custody_leg,
    run_deployment_seed_custody_leg_over, run_linked_deployment_seed_custody_leg,
};
pub use dav_context::dav_store_context;
/// One muted keyword — term and weight — re-exported beside the page record
/// that carries it.
pub use fauna_core::data::MutedKeyword;
pub use filter_marks::{
    FilterMarkRaise, SuccessionTime, decide_filter_mark, inherited_filter_ids, load_filter_marks,
    raise_succession_filter_marks,
};
pub use follows::{load_followed_folders, save_follow, save_unfollow};
pub use grant_marks::keep_grant_mark;
pub use member_reviews::{
    MemberReviewRaise, decide_member_review, load_member_reviews, raise_succession_member_reviews,
};
pub use mutate::{
    add_backup_destination, attach_backup_destination_folder, detach_backup_destination_folder,
    edit_backup_destination, keep_backup_destination, normalize_muted_keywords,
    remove_backup_destination,
};
pub use muted_words::MutedWordsSnapshot;
pub use nostr_npub_confirm::{npub_confirmation_owed, npub_confirmation_owed_for};
pub use recovery::{
    RecoverableBox, recoverable_box_ids_in, recoverable_boxes_in, selfhosted_recovery_command,
    selfhosted_recovery_command_in,
};
pub use rotate::{SeedRotation, seed_rotation_verdict};
pub use store_seam::{
    BackupStateStore, CustodyCeremonyStore, DeploymentSeedStore, FolderCustodyCut, FollowsStore,
    KindManifestStore, LEDGER_AWAITING_SIBLING, LEDGER_NOT_READY, LEDGER_READY_WAIT, MailStore,
    NoLedgerStore, PreferenceStore, ResolvingLedgerStore, SharedPreferenceStore, StoreError,
    SuccessionLedgerStore,
};
pub use succession_aftermath::{
    BackupRegrantError, BackupRegrantOutcome, BackupRegrantProgress, regrant_nest_backup_key,
};

// Re-export the at-rest key type so consumers depend on this crate's
// surface rather than reaching into `fauna_core::crypto` directly.
pub use fauna_core::crypto::BackupKey;

/// Derive a [`BackupKey`] from a 32-byte Ed25519 identity seed.
///
/// Thin wrapper over [`fauna_core::crypto::BackupKey::derive`] so callers
/// of this crate need not reach into `fauna_core::crypto` directly. The
/// derivation is BLAKE3 `derive_key` with the domain-separation context
/// `"fauna backup encryption key 2026-03-12"`; the same seed always
/// yields the same key, so every device in the user's fleet seals/unseals
/// against an identical key.
pub fn backup_key_from_seed(seed: &[u8; 32]) -> BackupKey {
    BackupKey::derive(seed)
}
