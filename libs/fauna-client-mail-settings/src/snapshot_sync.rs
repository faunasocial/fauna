//! Background re-snapshot loop hook. **Skeleton** — not needed for the
//! v1 snapshot and intentionally empty.
//!
//! The v1 snapshot carries only the MSEK-derived recipient-mail
//! keypair(s) (`fauna_mls::wrapped_blob::build_mls_snapshot_plaintext`),
//! which change **only** when MSEK rotates (hard-revoke) — and the
//! rotate-mail-keys flow (`rotation.rs`) already re-provisions the
//! snapshot then. Ordinary MLS state changes (joins / commits / leaf
//! updates) do **not** alter the v1 snapshot, so no per-commit
//! background refresh is required today.
//!
//! Authority: `docs/goal/behavior/mail-credentials.md` § Trigger
//! taxonomy § "MLS state change". This hook becomes load-bearing only
//! when the snapshot grows to carry genuine OpenMLS group/epoch state
//! (additive per the `MlsSnapshotPlaintext` doc-comment). At that point
//! the loop, once the openMLS provider exposes a state-event surface:
//!
//! 1. Awaits a state-event from the provider.
//! 2. Builds a new snapshot from the current MSEK history *plus* the
//!    exported group/epoch state (a shared export over `MlsEngine`).
//! 3. Reads MSEK from the mail custody (`fauna.state.mail`) via `MailStore`.
//! 4. Seals the snapshot under MSEK via `wrap::seal_snapshot_under_msek`.
//! 5. Calls `NestClient::provision_mls_snapshot_blob` (idempotent
//!    atomic replace).
//! 6. Repeats.

/// Tracking task name for telemetry / future structured logging.
pub const SNAPSHOT_SYNC_TASK: &str = "mail-snapshot-sync";

// The driver function will land here once the openMLS state-event
// surface exists. Phase A leaves the hook empty by design — the
// crate compiles + tests pass without it, and the per-app glue
// layer can stub it cheaply.
