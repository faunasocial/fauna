//! Error types for the fauna-mls crate.

use thiserror::Error;

/// Errors that can occur in the MLS layer.
#[derive(Debug, Error)]
pub enum MlsError {
    /// An error from the OpenMLS library.
    #[error("OpenMLS error: {0}")]
    OpenMls(String),

    /// An error from the storage layer.
    #[error("storage error: {0}")]
    Storage(String),

    /// The on-disk MLS state database was written by a **newer** build whose
    /// breaking schema change this build predates (`version-compatibility.md`
    /// § 2.2; the verdict is [`crate::version::SchemaVerdict::Incompatible`]).
    ///
    /// Distinct from a generic [`Self::Storage`] failure because the two demand
    /// opposite responses. A storage error is a broken store; this is an
    /// **intact** store this build is too old to read — the user's conversations
    /// are all still there, nothing was written, and the fix is to update the
    /// app. Surfacing it as a generic failure would invite exactly the reaction
    /// I1 forbids: treating the MLS state as lost and recreating it empty, which
    /// destroys every conversation permanently.
    ///
    /// Dormant until a breaking `mls.db` change ships (both numbers are `1`).
    #[error(
        "MLS state database needs a newer build (schema_version={db_v}, min_reader_version={db_min}, this build={bin_v}) — the state is intact; update the app"
    )]
    SchemaIncompatible { db_v: u16, db_min: u16, bin_v: u16 },

    /// Another live instance holds the **conversations-engine role** over this
    /// MLS state (`crate::storage::StateServedElsewhere` — the role lock of
    /// `account-data-plane.md` § Multi-instance concurrency, ruled 2026-08-15).
    ///
    /// Distinct from [`Self::Storage`] for the same reason
    /// [`Self::SchemaIncompatible`] is: the two demand opposite responses. A
    /// storage error is a broken store; this is an **intact** store another
    /// instance is serving right now — the app's conversations surface refuses
    /// honestly ("served in another instance", the `error-message`
    /// conventions), never reports a failure, and everything
    /// non-conversations proceeds normally.
    #[error(
        "this account's conversations are served in another instance of this app — refusing a second MLS engine over the same state"
    )]
    ServedElsewhere,

    /// This engine's **conversations-engine role was handed over** to a
    /// successor over the same state (`crate::engine::MlsEngine::retire`), so it
    /// must not touch the group any more — not seal, not decrypt, not originate
    /// a commit.
    ///
    /// The storage poison (`crate::storage::MlsStateRetired`) covers the
    /// database; this covers the part that is not in the database. MLS epochs
    /// and sender ratchets live in the in-memory openMLS provider, so a retired
    /// engine that kept sealing would advance a ratchet nobody can persist
    /// while its successor seals from the snapshot taken at the retire point —
    /// two engines on one leaf emitting overlapping
    /// `(sender-ratchet secret, generation)` pairs, which is precisely the fork
    /// the role's exclusivity exists to prevent
    /// (`account-data-plane.md` § Multi-instance concurrency).
    ///
    /// ⚠ A caller walking a message log must treat this as **terminal**, never
    /// as a per-record skip: it says nothing about the record and everything
    /// about the engine, so skipping past it durably advances a cursor over
    /// messages nothing folded.
    #[error(
        "this engine's conversations-engine role was handed over to a newer engine — it must not touch the group; its holder should have been dropped"
    )]
    Retired,

    /// An error encoding or decoding a message.
    #[error("encoding error: {0}")]
    Encoding(String),

    /// The requested channel was not found.
    #[error("channel not found: {0}")]
    ChannelNotFound(String),

    /// A policy check failed (e.g. DM must have exactly 2 members).
    #[error("policy violation: {0}")]
    PolicyViolation(String),

    /// A group's own leaf **names nobody** — the group loads out of a
    /// snapshot, but its own leaf index is absent from `members()`, which is
    /// the ordinary shape of every group an identity was evicted from (a
    /// remove-old another member committed; `forget_group` is voluntary, so
    /// the entry stays). The mid-session per-group adoption door
    /// (`crate::state_replica::ProviderReplica::import_group_into`) refuses
    /// such a group: it imports only what *positively* seats this identity,
    /// because — unlike the whole-KV launch restore, where a blank seat is
    /// examined-and-clean and the sibling groups carry the identity question —
    /// a per-group import has no sibling to answer for it (`devices.md`
    /// § Cross-device MLS group-state sync → *A sibling-joined group is adopted
    /// mid-session by a targeted import*).
    ///
    /// Distinct from [`Self::PolicyViolation`] because the two call for
    /// opposite reactions from a caller's log: a group seated under **another**
    /// identity's leaf is a predecessor's snapshot mid-session — an anomaly
    /// worth a warning every time — while a blank seat is a state every evicted
    /// member's snapshot carries forever and is met again on every sibling
    /// flush. Benign; callers log it below `warn`.
    #[error("not seated: {0}")]
    NotSeated(String),

    /// A leaf-credential binding check failed (security review MLS-2): an MLS
    /// leaf's `BasicCredential` did not equal its leaf signature key (a forged
    /// actor identity), or an uploaded KeyPackage's credential did not match the
    /// authenticated uploader. Distinct from a benign [`PolicyViolation`] — this
    /// is an attack signal (a patched in-group client attempting impersonation),
    /// so callers (e.g. the nest `keypackage.upload` handler) can log it as such.
    #[error("credential binding violation: {0}")]
    CredentialBindingViolation(String),

    /// `process_commit` met a commit authored by this identity's **own leaf**
    /// on the group's current epoch — MLS cannot process an own-leaf message
    /// (no sender-ratchet secrets for self), and this device did not author it,
    /// so **another of the user's devices advanced the epoch**. The
    /// cross-device **resync signal** (`docs/goal/behavior/devices.md`
    /// § Cross-device MLS group-state sync, design §3): refetch the `provider`
    /// replica and reload the group. `epoch` is the commit message's epoch
    /// (from the unprotected header), for the crash-window pending-merge
    /// equality check.
    #[error(
        "own-leaf commit on the current epoch (epoch {epoch}): another device advanced the group"
    )]
    OwnLeafCommit { epoch: u64 },

    /// `process_commit` met a commit that is **intrinsically invalid** — no
    /// member of the group can ever apply it, so the group's canonical state
    /// never advances past it either: malformed bytes, a non-commit body on
    /// the commit rail, or a validation verdict computed on state every
    /// honest member shares. The epoch check runs *first* in openMLS
    /// (`validate_framing`, both directions → `WrongEpoch`, classified as
    /// [`Self::PastEpochCommit`]/[`Self::FutureEpochCommit`] instead), so any
    /// commit reaching deeper validation is at exactly this device's epoch —
    /// but **being at the same epoch does not make every verdict shared**:
    /// staging a commit's update path needs the receiver's own *private*
    /// epoch decryption keypairs, device-local state whose absence must
    /// classify as a local failure, never as this variant. Only errors
    /// judged on the bytes plus the group's canonical shared view (public
    /// tree, epoch context, membership/confirmation keys) map here, each
    /// openMLS variant classified explicitly — see
    /// `MlsEngine::process_commit`.
    ///
    /// Distinct from a *local* failure ([`Self::OpenMls`]/[`Self::Storage`],
    /// e.g. absent own key material, or a merge that failed after validation)
    /// because the two demand opposite cursor behavior (`devices.md` § Cross-
    /// device MLS group-state sync, Rule 2): an ingest cursor may safely
    /// advance past an intrinsically invalid record — stalling on it would
    /// let one garbage record from any in-group member wedge every other
    /// member's walk forever (a remote DoS) — while a locally-failed commit
    /// is a transition the group may have incorporated and this device did
    /// not, which a cursor must stop before.
    #[error("intrinsically invalid commit (no member can apply it): {0}")]
    InvalidCommit(String),

    /// `process_commit` met a commit for an epoch the group has already
    /// advanced past — this device's own already-merged commit coming back
    /// around the poll, or a replayed/duplicate foreign commit. Safe for the
    /// inbound driver to skip quietly (the state it would produce is already
    /// held).
    #[error("commit for an already-advanced epoch — already merged or replayed")]
    PastEpochCommit,

    /// `process_commit` met a commit for an epoch **ahead** of the group's — this
    /// device never applied the commit that would have bridged the gap, so its
    /// ratchet is stranded: MLS members cannot skip epochs, and MLS can never
    /// re-produce a commit for a transition that already happened.
    ///
    /// Distinct from [`Self::PastEpochCommit`] because openMLS reports **both**
    /// directions as `ValidationError::WrongEpoch`, and quietly skipping this one is
    /// how a hole in the ingest log becomes an invisible, permanent strand: every
    /// subsequent foreign commit is also future-epoch, so nothing ever recovers and
    /// nothing ever complains.
    ///
    /// Reachable when the durable ingest cursor outran the durable `provider` state
    /// it indexes — the torn-save hazard (`devices.md` § Cross-device MLS
    /// group-state sync). Rule 2 makes tears unrepresentable; this variant is the
    /// regression guard that makes any future tear **loud** rather than silent.
    #[error(
        "commit for a future epoch {epoch} (group is behind): this device skipped a commit and cannot catch up"
    )]
    FutureEpochCommit { epoch: u64 },

    /// `process_commit` met a commit that decrypted and staged cleanly but is
    /// refused by the **folder-channel commit policy** (owner-managed roster —
    /// `federation.md` § Cross-nest shared folders + channel append): the staged
    /// commit carries proposals (a roster/group-state change) and its
    /// MLS-authenticated committer is not the channel's recorded folder owner
    /// ([`crate::MlsEngine::folder_channel_owner`]). A bare self-`Update`
    /// commit (no proposals — the device-owned-epoch takeover of `devices.md`
    /// § Cross-device MLS group-state sync) is never this variant.
    ///
    /// Deterministic for every honest member holding the same owner marker, so
    /// the inbound driver skips it like [`Self::InvalidCommit`] — the group's
    /// canonical (honest) state never advances past it. Unlike `InvalidCommit`,
    /// the verdict is reached **after** PrivateMessage decryption consumed the
    /// committer's sender-ratchet generation, so the engine records the
    /// commit's identity durably and answers a later re-walk of the same bytes
    /// with this variant **before** decryption — without that memo the
    /// every-launch folder-rail re-walk would misclassify the replay as a
    /// local `SecretReuseError` and stall the channel forever.
    #[error("commit refused by the folder commit policy: {reason}")]
    PolicyRefusedCommit { reason: String },

    /// A Welcome addressed **only to key packages this device does not hold** —
    /// the expected multi-device steady state, not a fault
    /// (`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync →
    /// *Who may consume a Welcome — any device that holds its key*).
    ///
    /// Every device of an account consumes every Welcome push, but only the
    /// devices whose provider holds the addressed one-time (or last-resort)
    /// init key can join from it. A device launched **before** a sibling minted
    /// the pool package a Welcome addresses never had that key; a device that
    /// already joined through it consumed it (a one-time package is deleted by
    /// its first join, a last-resort package never is). Both are the same
    /// verdict for the same reason — this device cannot join this Welcome and
    /// re-delivery will not change that — and the group reaches it by the other
    /// door instead: the targeted import, once the minting sibling's flush
    /// lands.
    ///
    /// Distinct from [`Self::OpenMls`] for the reason
    /// [`Self::PastEpochCommit`] is distinct from [`Self::InvalidCommit`]: the
    /// two demand opposite *reporting*. A failed `StagedWelcome` is a genuine
    /// ingest fault worth an operator's attention; this is the steady state of
    /// every account with more than one device, so a caller logs it **below
    /// `warn`** and leaves the row un-acked exactly as before. Reported by our
    /// own door — [`crate::MlsEngine`] asks the provider whether it holds a key
    /// package for any ref the Welcome addresses — never by matching on an
    /// openMLS error's text.
    #[error("welcome addressed to no key package this device holds")]
    NotAddressedToThisDevice,

    /// This device holds no epoch-scoped secret for the epoch asked about: the
    /// group is at another epoch now, and this device never kept that one —
    /// it joined after it, or advanced past it on a build (or a target, wasm)
    /// that kept no per-epoch secrets. Final for that epoch: content sealed
    /// under it stays sealed here. Named apart from [`Self::OpenMls`] because
    /// it is an expected state of a room with history, not a library fault.
    #[error("no secret held for epoch {epoch} of this group")]
    EpochSecretNotHeld { epoch: u64 },
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, MlsError>;
