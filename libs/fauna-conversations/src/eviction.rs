//! Cross-group eviction — *remove this person from every group of mine they are
//! currently in*, and the outcome type that decides what verdict that earns.
//!
//! **Why this exists as its own operation.** The unattested-member review
//! renders one flag in three places (`identity-succession.md` § Propagation →
//! *MLS groups*). Two of them — a group member list, a contacts row — show the
//! person *inside* a row that already owns a removal affordance, so their
//! *Remove* routes to it and no second removal mechanism is minted. The other
//! two surfaces (the ephemeral kit-side pass, the permanent review view) render
//! a person **outside any one group**, so there is no row-local affordance to
//! route to and their *Remove* can only mean "everywhere". That is this module.
//!
//! **Why it lives in `fauna-conversations` rather than beside the succession
//! ceremony.** The sweep driver sits in `fauna-client-recovery` because it needs
//! a nest connection and is *about* a succession. This one is not: it is a
//! statement about group membership — the manager already owns both the thread
//! roster and [`ConversationsManager::remove_participant`], and a second
//! producer of review items is already in sight (the witness's unverifiable arm,
//! § Propagation), with no succession anywhere in its story. Keeping it here
//! means the review surfaces on all 7 apps call one manager method over the FFI
//! they already hold, instead of each app wiring a recovery-crate seam into a
//! conversations screen.

use crate::address::TypedAddress;
use crate::thread::ThreadId;
use fauna_core::data::UnattestedVerdict;
use fauna_core::identity::ActorId;
use serde::Serialize;

/// What a cross-group eviction actually achieved, per group.
///
/// Deliberately **not** a boolean. A partial eviction is the ordinary outcome
/// under a flaky link — removing someone from 3 of their 5 groups — and the
/// whole honesty of the review surface turns on not rounding that to success.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CrossGroupEviction {
    /// Groups the person was in and is no longer, this call.
    pub evicted: Vec<ThreadId>,
    /// Groups the person is still in because the removal failed, with the
    /// backend's reason. A retry targets exactly these, for free — see
    /// [`Self::earned_verdict`].
    pub failed: Vec<EvictionFailure>,
    /// Seats this driver cannot clear from here at all — § Propagation rule
    /// (5)'s typed, rendered fact (`identity-succession.md`): a raised seat
    /// never escapes silently and never lets `Removed` be earned while it
    /// stands. Unlike [`Self::failed`] these are not retryable *here*; each
    /// class names the room its remedy lives in, and either remedy converges —
    /// once taken, the seat is out of the raised span at the next press.
    pub unreachable: Vec<UnreachableSeat>,
}

/// One group a [`CrossGroupEviction`] could not clear, with the backend's
/// reason. A named record rather than a `(ThreadId, String)` tuple — UniFFI
/// has no `Lower`/`Lift` impl for a bare tuple, only for records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct EvictionFailure {
    pub thread: ThreadId,
    pub reason: String,
}

/// One raised seat the cross-group eviction cannot clear from here — the
/// channel named as hex (there is no `ThreadId`; that absence is what makes
/// the seat unreachable) and the **typed** class, so every app renders its own
/// localized remedy instead of shared Rust minting user-facing English.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct UnreachableSeat {
    pub channel_hex: String,
    pub class: UnreachableSeatClass,
}

/// Why a raised seat is not clearable from the review surface — § Propagation
/// rule (5)'s two blocking classes plus the room policy's refusal, and
/// deliberately only those three: a scheduling seat never blocks (no Commit is
/// ever applied to a scheduling channel, so nothing can be planted there and
/// nothing needs removing), so a variant for it here would be unrepresentable
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum UnreachableSeatClass {
    /// A governed room (`conversation-rooms.md` § Roles and authorization)
    /// in which the viewer's role does not permit a Remove — a plain member
    /// cannot evict anyone, and nobody evicts the owner. The remedy is the
    /// room's owner or an admin acting from their own device; a Remove the
    /// policy forbids is refused **before** any commit is authored, so it is
    /// a typed fact here, never a silent skip or a retryable failure.
    NotPermittedByRoomPolicy,
    /// A chat group of the owner's whose thread↔channel binding has not been
    /// rebuilt on this device — removable once the thread syncs here, or from
    /// a device that has it.
    ChatGroupNoThreadHere,
    /// A shared folder channel — removal is the set owner's key-rotating
    /// remove in the folder's own sharing surface; a member of someone
    /// else's set removes nobody but can leave the set. Either way the seat is
    /// out of the raised span at the next press.
    FolderChannel,
}

impl CrossGroupEviction {
    /// Did every raised seat the person holds actually clear?
    ///
    /// True for an eviction that freed *nobody* — see [`Self::earned_verdict`].
    /// False while any [`Self::unreachable`] seat stands: those are raised
    /// seats too, and `Removed` earned over one would be the surface going
    /// quiet on a live seat — rule (3)'s harm through the class door.
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty() && self.unreachable.is_empty()
    }

    /// The verdict this eviction earns, or `None` when the review item must
    /// stay [`UnattestedVerdict::Open`].
    ///
    /// **This method is the point of the whole type, and callers must not
    /// reconstruct its judgment.** [`UnattestedVerdict::Removed`] is defined as
    /// "the person *was* removed from the groups that raised them"
    /// (`fauna_core::data`), so writing it after a partial eviction records
    /// something false: the surface goes quiet — an adjudicated item is kept at
    /// rest and no longer rendered — while the person is still seated in the
    /// groups that failed. That is the exact failure the review exists to
    /// prevent, arrived at from the other direction.
    ///
    /// So the verdict is earned only by a **complete** eviction, and a partial
    /// one leaves the item open. Nothing is lost by that and nothing is
    /// re-asked twice: the successful removals are durable, and which groups a
    /// flagged person sits in is deliberately **not** stored (§ Implementation
    /// status today — it is re-derived from the owner's own engine at render
    /// time), so the next press re-derives the roster and finds only the groups
    /// that failed. Retrying is idempotent and converges without a resume
    /// cursor, a per-group verdict, or any state this design refused to keep.
    ///
    /// **An eviction that frees nobody earns [`UnattestedVerdict::Removed`]
    /// too**, and this is not an edge case to guard — it is the common one on
    /// the permanent view, which by construction holds a backlog someone
    /// postponed, and a flagged person may have left every group in the
    /// meantime. "Removed from every group they were in" is vacuously true of
    /// zero groups, the owner's judgment about that person is just as real, and
    /// refusing the verdict there would strand an item nobody can ever close.
    pub fn earned_verdict(&self) -> Option<UnattestedVerdict> {
        self.is_complete().then_some(UnattestedVerdict::Removed)
    }
}

/// Whether `addr` is the Fauna address of `person`.
///
/// Membership is compared on the **actor id**, never on the handle: the whole
/// review exists because an identity a thief seated in a group may wear any
/// handle it likes, and a handle is not an identity. A participant with no
/// actor id (every non-Fauna rail) is nobody's match.
///
/// The caller matches **one** participant per group, which is sound rather than
/// lucky: a credential cannot be seated twice in an MLS group, pinned by
/// `fauna-mls`'s `succession_ceremony::a_credential_cannot_be_seated_twice`. If
/// that ever stopped holding, an actor wearing two chips in one roster would be
/// evicted from one of them and reported as fully removed — so a change there
/// owes this a second look.
pub(crate) fn is_person(addr: &TypedAddress, person: &ActorId) -> bool {
    matches!(addr, TypedAddress::Fauna { actor_id, .. } if actor_id == person)
}
