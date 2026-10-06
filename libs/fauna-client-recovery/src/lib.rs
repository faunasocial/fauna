//! The shared client core for identity succession and RecoveryKey recovery —
//! the logic behind the recovery-kit screen, the recovery-entry screen, the
//! Settings/Security replacement surfaces, and the "my identity was stolen"
//! ceremony, written once for all 7 apps (priority #2).
//!
//! **Owner goal doc:** [`docs/goal/behavior/identity-succession.md`]. Read
//! § The RecoveryKey, § Seed escrow, § The succession statement and
//! § Propagation before changing anything here; the rules this crate encodes
//! are security properties, not conventions.
//!
//! ## What lives here, and what deliberately does not
//!
//! Here: composing the twelve `fauna.recovery.*` kinds into the four ceremonies
//! a user actually performs, mapping refusal codes onto the states a screen
//! branches on, and the pure projections a banner renders. All of it transport-
//! generic over [`fauna_protocol::RpcRequester`], so native and wasm consume one
//! implementation.
//!
//! Not here, on purpose:
//!
//! - **Any persistence of the RecoveryKey.** `identity-succession.md:38` is
//!   iron-clad — the secret is displayed once and never stored on any device,
//!   on the account plane, or anywhere the identity seed unlocks. This crate therefore
//!   has no store seam at all, not even an optional one. See [`kit`].
//! - **UI.** No snapshots, no action enums, no ui.yaml IDs. The screens are per-
//!   app legs on top of this.
//! - **Most of the post-succession aftermath**: `BackupKey` corpus re-seal,
//!   capability-grant re-mint, `NestBackupKey` re-grant, the MSEK hard-revoke,
//!   contact re-point. Those need their own planes and are separate tracks;
//!   [`succession::succeed_identity`] stops at the statement landing and says
//!   so. (The per-group MLS add-successor/remove-old ceremony **did** land here
//!   — [`group_sweep`] — because it composes the same twelve kinds over the same
//!   requester; this list named it as absent until 2026-08-02.) The aftermath's
//!   first step, the successor's fresh kit, is [`kit::create_kit`] driven by the
//!   app at its post-succession hook, not a ceremony of its own.
//!
//! ## Which connection each ceremony needs
//!
//! This is a correctness property, not a detail. The scenarios this plane
//! serves are precisely the ones with **no working session** — a thief can
//! revoke every session and invoke the seed-signed emergency lockout — so the
//! ceremonies that answer a theft ride pre-identity kinds and must be driven
//! over an **anonymous** connector. Demanding a signed-in session for them
//! would let the attack disable its own remedy.
//!
//! | Ceremony | Connection |
//! |---|---|
//! | [`kit::create_kit`] | authenticated (USER-class submit + put) |
//! | [`restore::restore_seed`] | anonymous (there is no device left) |
//! | [`replacement::request_seed_alone_replacement`] / [`replacement::pending_replacement`] | authenticated |
//! | [`replacement::veto_pending_replacement`] | anonymous (the owner holds only the phrase) |
//! | [`succession::succeed_identity`] / [`succession::resolve_successor`] | anonymous |
//! | [`replacement::reseal_escrow_with_held_kit`] | authenticated |

/// The post-succession aftermath — the ordered pass (legs 1, 2, 4, 7, 6) a
/// successor's first authenticated session runs, with the barriers that make
/// the order mean something. The sibling half of [`ceremony`], and wasm-clean:
/// tui and web both drive it, and the five FFI apps inherit it.
#[cfg(feature = "aftermath")]
pub mod aftermath;
pub mod alerts;
/// The succession ceremony's app-side orchestration. Its three *driving*
/// functions are native-only (they dial real transports and open on-disk MLS
/// stores); its outcome types and the undecidable-arm wording are wasm-clean
/// and shared with web's own driver — see the module doc.
pub mod ceremony;
/// **The chain follows the link** — the registration-chain reconcile between
/// two nests that hold an account for one identity
/// (`identity-succession.md` § Enforcement on the home nest → *Every nest the
/// identity is linked to*). The body is `fauna_client_core::recovery_chain`:
/// its two callers, the Nests page's both-ends link and the runtime's
/// secondary leg, both sit below this crate.
pub use fauna_client_core::recovery_chain as chain_reconcile;
pub mod error;
pub mod group_sweep;
#[cfg(feature = "conversations-witness")]
pub mod harvest;
pub mod kit;
/// The post-store-ready half of the aftermath — the succession-ledger legs
/// every host runs once at its account-store-ready edge, off the durable
/// ceremony park.
#[cfg(feature = "aftermath")]
pub mod ledger_aftermath;
/// Clause (c) as one gesture — the seed-alone request and the veto at every
/// linked nest, over the host's [`linked_fanout::LinkedNestDial`].
pub mod linked_fanout;
pub mod nest;
pub mod replacement;
pub mod restore;
pub mod status;
pub mod succession;
pub mod witness;

pub use alerts::{
    alert_key, pending_replacement_alert_lines, refresh_pending_replacement_alert,
    sync_pending_replacement_alert,
};
pub use error::{RecoveryError, Result, codes};
pub use group_sweep::{
    GroupSweepOutcome, GroupSweepState, SweepReport, SweepRetryOutcome, retry_group_sweep,
    sweep_groups,
};
pub use kit::{
    EscrowOutcome, RecoveryKit, create_kit, create_kit_with_root, kit_display_uri,
    predecessor_seeds_from_rows,
};
pub use linked_fanout::{
    LinkedFanOut, LinkedNestAnswer, LinkedNestDial, LinkedNestOutcome,
    request_seed_alone_replacement_everywhere, veto_everywhere,
};
pub use nest::{Challenge, RecoveryClient, kinds};
pub use replacement::{
    PendingKit, PendingReplacement, REPLACE_GRACE_SECS, pending_replacement,
    request_seed_alone_replacement, request_seed_alone_replacement_at, reseal_escrow_with_held_kit,
    veto_pending_replacement, veto_pending_replacement_everywhere,
};
pub use restore::{ParsedKit, RestoredSeed, parse_kit, restore_seed};
// The escrow container's predecessor vocabulary, re-exported so an app threading
// the succession's kit ceremony (`create_kit`'s `predecessors`) or consuming a
// restore never needs its own `fauna-mls` dependency — the same reason
// `RecoveryError` and `SupersededNotice` are surfaced here rather than upstream.
pub use fauna_mls::wrapped_blob::{PredecessorSeed, PredecessorSeedOpened, PredecessorsOutcome};
pub use status::{
    RecoveryKitStatus, kit_status, kit_status_after_mint, reseal_escrow_with_status,
    veto_with_status,
};
pub use succession::{
    ReconciledSuccession, SuccessionAttempt, SuccessionHandoff, SuccessionOutcome,
    SupersededNotice, UnconfirmedSuccession, reconcile_succession, resolve_succession_line,
    resolve_successor, succeed_identity, succeed_with_held_kit,
};
pub use witness::{
    AnchoredWalk, ChainWitness, LineResolution, SuccessionAnchors, SuccessionChainSource, walk,
    walk_line, walk_outcome,
};
#[cfg(feature = "conversations-witness")]
pub use witness::{ThreadParticipantAnchors, UNREADABLE_STORE_BACKOFF_SECS};
