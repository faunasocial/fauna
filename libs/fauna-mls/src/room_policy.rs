//! The **room authorization policy** of an end-to-end room — the roles, join
//! rule, history policy and name a room's members enforce cryptographically
//! (`docs/goal/behavior/conversation-rooms.md` § Roles and authorization →
//! *End-to-end rooms — enforced cryptographically*, § Join rules and invites,
//! § History for joiners).
//!
//! # Where the policy lives, and why there
//!
//! The policy rides the **MLS group context** as a private-use extension
//! ([`ROOM_POLICY_EXTENSION_TYPE`]), and the group's `required_capabilities`
//! names that extension type. Three properties fall out of that placement,
//! and each one is load-bearing:
//!
//! 1. **Every member at an epoch agrees on it.** The group context is covered
//!    by the confirmation tag, so two members who merged the same commit hold
//!    byte-identical policy — the precondition for the commit verdict below
//!    being *deterministic* across members. A policy carried as an
//!    application message would be ordered by the nest log instead, and a
//!    newcomer could never tell "no policy yet delivered" from "this room has
//!    none".
//! 2. **A joiner receives it in the Welcome**, before it processes a single
//!    commit. There is no window in which a newcomer applies an open verdict
//!    to a room whose other members apply a policy.
//! 3. **Version skew is negotiated by MLS, never forked by us.** A leaf that
//!    does not advertise the extension type in its capabilities cannot be
//!    added to a room whose `required_capabilities` names it — openMLS refuses
//!    the Add for every member alike — and a room whose members do not all
//!    advertise it cannot install the policy. Every current app's package
//!    advertises it, so a package that does not is a non-conforming (patched
//!    or foreign) client's, and it is excluded from a policy-bearing room
//!    *loudly* (the creator's or inviter's add fails with a typed refusal)
//!    rather than seated as a member that silently merges the commits every
//!    honest member refuses — which would fork the group between honest
//!    members. There is no policy-less fallback for such a package: every
//!    end-to-end group is born governed, and the refusal is the outcome. A
//!    **policy-less** room (no policy, open commit processing, rendered with
//!    no roles) is a live shape of its own — a 1:1 conversation, a
//!    folder-share group (`fauna_client_folders`'s adapter mints those
//!    through the plain `create_group` and governs them by the owner-managed
//!    roster marker instead), or a group a peer minted without a policy —
//!    never the product of a fallback. The requirement is agreed group state
//!    like the policy itself, so a GroupContextExtensions commit that would
//!    drop it is refused by every member, and the
//!    honest add path refuses an unadvertised leaf by name on its own rather
//!    than delegating the check to the requirement.
//!
//! # The verdict
//!
//! [`judge_commit`] is a pure function of the policy the member currently
//! holds and the facts of one staged commit — the committer's
//! MLS-authenticated identity, the identities it adds and removes, and the
//! group context the commit would install. Every member computes it on the
//! same bytes and the same held policy, so every honest member reaches the
//! same verdict; the engine records a refusal in the same durable memo the
//! folder commit policy uses and answers a replay of the same bytes before
//! decryption (`engine.rs`, `POLICY_REFUSED_COMMIT_PREFIX`).
//!
//! # Successions
//!
//! A user's identity succession
//! (`docs/goal/behavior/succession-propagation.md` § Propagation → *MLS
//! groups*) is a two-commit ceremony — add-successor authored by the **old**
//! leaf, remove-old authored by the **new** leaf — and both halves are, on
//! their face, exactly the shapes a plain member's role forbids (an Add
//! under `invite`, a Remove of another member). The policy admits them
//! through one door: a [`RecordedSuccession`] appended to the extension by
//! the old leaf itself (a group-context change committed by `old`, naming
//! `new`). From then on the Add of `new` by `old` and the Remove of `old` by
//! `new` are permitted, and every **role resolves through the chain** —
//! the successor of the owner is the owner, the successor of an admin is an
//! admin — so an owner's succession never orphans the room and no policy
//! rewrite is needed for a member to inherit their predecessor's role.
//!
//! What that door deliberately admits, stated rather than glossed: whoever
//! holds the old key may *replace itself* with a fresh identity it controls.
//! That is no new power — the old key already read the room — and the
//! planted-successor residual it leaves behind after a *real* succession is
//! exactly the unattested-member review the succession sweep already raises
//! (`fauna_client_recovery::group_sweep`). The legitimacy of a succession is
//! a nest-anchored, time-based fact no in-group verdict can decide offline;
//! this module decides only *authorization*, deterministically.
//!
//! The **vouch** arm — an owner or admin recording a succession *for*
//! another member, the member-side re-add remedy — is the one place that
//! argument does not hold, because a vouch replaces *somebody else's* key.
//! So a voucher may record a succession only for a principal it **strictly
//! outranks**: the owner for anyone, an admin for a plain member, nobody for
//! the owner but the owner's own leaf.
//!
//! # Ownership transfer
//!
//! A voluntary hand-over (`conversation-rooms.md` § Roles and authorization
//! → *Ownership transfer*) is a policy change with **two** signatures, each
//! from a key that must consent: the outgoing owner's, because only the
//! owner may transfer, and the incoming owner's, because an owner cannot
//! leave or be removed and nobody is made one unasked. The ceremony is one
//! round trip. The outgoing owner builds the policy that names its successor
//! (version + 1, the successor out of the admin set), countersigns it
//! **bound to the room** ([`OwnershipCountersignature`] — a countersignature
//! for one room must never complete a hand-over of another room the same
//! owner holds at the same policy bytes), and posts it as a
//! [`RoomOwnershipOffer`] application message every member decrypts. The
//! named member's device signs the same policy as itself and commits it,
//! carrying the countersignature; every member admits the commit only when
//! the signer is the new owner, the countersigner is the owner the chain
//! resolves to, and both signatures verify. A stale offer — one whose
//! version no longer advances by one — is refused by every member and
//! dropped by the device that holds it; the owner offers again.

use ed25519_dalek::Signer;
use fauna_core::identity::{ActorId, ActorKeypair, verify_detached};
use serde::{Deserialize, Serialize};

use crate::error::{MlsError, Result};

/// The MLS extension type carrying the room policy in the group context.
/// Private-use range (RFC 9420 § 17.3: `0xF000`–`0xFFFF`). Frozen — every
/// policy-bearing room's `required_capabilities` names it.
pub const ROOM_POLICY_EXTENSION_TYPE: u16 = 0xF0A1;

/// Domain tag every room-policy signature covers, framed exactly like the
/// succession plane's records: `[tag_len: u8] ‖ tag ‖ canonical_dag_cbor(policy)`
/// (`fauna_core::recovery`, *Why detached, domain-tagged signatures*).
pub const TAG_ROOM_POLICY: &str = "fauna.room.policy.v1 2026-09-08";

/// The longest succession chain [`RoomPolicyExtension::resolve`] follows — a
/// bound on a malformed extension, never a limit an honest room reaches.
const MAX_SUCCESSION_HOPS: usize = 64;

/// The BLAKE3 `derive_key` context the **birth record** commits under —
/// frozen: it is half of every ceremony-born room's identity.
const ROOM_BIRTH_ID_CONTEXT: &str = "fauna.room.birth.id.v1 2026-09-09";

/// A room's birth record — "the creating principal's key + a salt"
/// (`conversation-rooms.md` § The room, the `room_id` row), canonically
/// encoded and hashed into the room's id by [`derive_room_id`].
///
/// The salt is what keeps the id unguessable and lets one principal found
/// many rooms; the owner is what binds the id to a key, so a room id is a
/// commitment to *who founded it* rather than a 32-byte name anyone may
/// claim. That commitment is the whole value of the ceremony: it is why a
/// nest can tell a room's founder from a caller that guessed an id, which is
/// what the floor roster's report door had no way to do before
/// (§ Implementation status today, the declared bootstrap bound).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomBirthCore {
    pub owner: ActorId,
    #[serde(with = "serde_bytes")]
    pub salt: Vec<u8>,
}

/// The content-derived id of the room `owner` founds with `salt`.
///
/// # Errors
/// Only on a core that fails canonical encoding — unreachable for honestly
/// constructed values.
pub fn derive_room_id(owner: &ActorId, salt: &[u8; 32]) -> Result<[u8; 32]> {
    let core = RoomBirthCore {
        owner: *owner,
        salt: salt.to_vec(),
    };
    Ok(blake3::derive_key(
        ROOM_BIRTH_ID_CONTEXT,
        &canonical(&core)?,
    ))
}

/// The opening bytes of every community room's **binding birth salt** —
/// frozen: through the room id, which commits to the salt, they commit the
/// room to the [`TAG_ROOM_POLICY_IN_ROOM`] room signature on every policy
/// version above 1 ([`RoomBinding`]).
///
/// Every room requires that signature, so the mark decides nothing a judge
/// reads; what it does is keep every room id a commitment to the rule.
/// [`verify_community_birth`] refuses a salt without it — the founder's own
/// check and the nest's at `room.create` alike — so no founder can mint a room
/// whose id reads as one that predates the binding. The other 24 bytes stay
/// random, which is all the salt ever needed to be — enough to keep the id
/// unguessable to those who do not already name the room.
pub const BINDING_SALT_MARK: [u8; 8] = *b"fauna:rb";

/// A binding birth salt ([`BINDING_SALT_MARK`]) around 24 bytes of `entropy`.
pub const fn binding_birth_salt(entropy: &[u8; 24]) -> [u8; 32] {
    let mut salt = [0u8; 32];
    let mut at = 0;
    while at < 32 {
        salt[at] = if at < 8 {
            BINDING_SALT_MARK[at]
        } else {
            entropy[at - 8]
        };
        at += 1;
    }
    salt
}

/// Whether `salt` is a binding birth salt — whether it opens with
/// [`BINDING_SALT_MARK`], the one shape of salt a community room is founded
/// with.
pub fn is_binding_birth_salt(salt: &[u8; 32]) -> bool {
    salt[..8] == BINDING_SALT_MARK
}

const ROOM_ENTRY_ID_CONTEXT: &str = "fauna.room.roster-entry.id.v1 2026-09-09";

/// One seating of a principal on a room's floor — the room plane's
/// **roster entry** in the recipient-set scheme's sense
/// (`account-data-taxonomy.md` § The recipient-set scheme, the roster-kind
/// bullet), hashed into the entry id by [`derive_room_entry_id`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomEntryCore {
    #[serde(with = "serde_bytes")]
    pub room: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub principal: Vec<u8>,
    /// The stamp of *this* seating, not of the principal's first one.
    pub seated_at_ms: i64,
}

/// The 32-byte roster-entry id for one seating — the slot every wrap of a
/// room generation key is sealed to.
///
/// Derived from the **seating stamp** as well as the room and the principal,
/// which is what makes the scheme's "re-admission is a fresh entry id, so
/// add-wins resurrection is unrepresentable" true on this plane: a principal
/// that is removed and later re-invited comes back on a *new* slot, so no
/// wrap minted for its old seat can be replayed at it, and the wraps of the
/// generations it was severed from stay inert rather than becoming reachable
/// again.
///
/// # Errors
/// Only on a core that fails canonical encoding — unreachable for honestly
/// constructed values.
pub fn derive_room_entry_id(
    room: &[u8; 32],
    principal: &[u8; 32],
    seated_at_ms: i64,
) -> Result<[u8; 32]> {
    let core = RoomEntryCore {
        room: room.to_vec(),
        principal: principal.to_vec(),
        seated_at_ms,
    };
    Ok(blake3::derive_key(
        ROOM_ENTRY_ID_CONTEXT,
        &canonical(&core)?,
    ))
}

/// The three roles, one vocabulary for every class
/// (`conversation-rooms.md` § Roles and authorization).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoomRole {
    Owner,
    Admin,
    Member,
}

impl RoomRole {
    /// Owner or admin — the two roles the table lets invite, remove, rename
    /// and set the join/history rules.
    pub fn is_admin_or_owner(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

/// Who may invite (`conversation-rooms.md` § Join rules and invites).
///
/// `Request` is the community class's rule and **never valid on an end-to-end
/// room** — a room with no reader who can vouch for a stranger cannot carry
/// it — so [`RoomPolicy::validate`] refuses it here.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JoinRule {
    /// Owner and admins invite. The default.
    Invite,
    /// Any member may invite.
    MemberInvite,
    /// Anyone who can name the room may ask; owner/admins approve.
    /// Community rooms only.
    Request,
}

/// What a newcomer sees of the room before their admission
/// (`conversation-rooms.md` § History for joiners).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HistoryPolicy {
    /// Nothing before admission — the MLS default, and every room's default.
    None,
    /// An existing member re-seals a history slice to the newcomer.
    Full,
}

/// The policy proper — what the roles table governs, signed as one record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPolicy {
    /// Monotonic: every accepted change is exactly `previous + 1`, so a
    /// replayed older policy can never be installed over a newer one.
    pub version: u64,
    /// Exactly one owner. Resolves through the recorded successions
    /// ([`RoomPolicyExtension::role_of`]).
    pub owner: ActorId,
    /// The admin set, sorted and deduplicated, never naming the owner.
    pub admins: Vec<ActorId>,
    /// The room's name; `None` for a room still labelled by its members.
    pub name: Option<String>,
    pub join_rule: JoinRule,
    pub history_policy: HistoryPolicy,
}

impl RoomPolicy {
    /// The policy every new end-to-end room is born with: its creator the
    /// owner, no admins, invite-only, no history for joiners.
    pub fn initial(owner: ActorId, name: Option<String>) -> Self {
        Self {
            version: 1,
            owner,
            admins: Vec::new(),
            name,
            join_rule: JoinRule::Invite,
            history_policy: HistoryPolicy::None,
        }
    }

    /// Structural validity — what a well-formed end-to-end policy looks like
    /// regardless of who signed it.
    pub fn validate(&self) -> Result<()> {
        if self.join_rule == JoinRule::Request {
            return Err(MlsError::PolicyViolation(
                "an end-to-end room cannot carry the `request` join rule".into(),
            ));
        }
        self.validate_structural()
    }

    /// Structural validity for a **community** room's policy — everything
    /// [`RoomPolicy::validate`] checks except the end-to-end class's refusal
    /// of the `request` join rule.
    ///
    /// The two differ in exactly one rule, and the rule belongs to the class
    /// rather than to the shape: `request` "is the community class's rule and
    /// **never valid on an end-to-end room** — a room with no reader who can
    /// vouch for a stranger cannot carry it" (`conversation-rooms.md` § Join
    /// rules and invites). A community room has such a reader — its home
    /// nest — so the same bytes that are malformed in an MLS group context
    /// are well formed on the floor. Splitting the check rather than
    /// loosening [`RoomPolicy::validate`] keeps the end-to-end path exactly
    /// as strict as it was.
    pub fn validate_community(&self) -> Result<()> {
        self.validate_structural()
    }

    /// The class-agnostic half — true of a well-formed policy in any class.
    fn validate_structural(&self) -> Result<()> {
        if self.admins.contains(&self.owner) {
            return Err(MlsError::PolicyViolation(
                "the owner is never listed in the admin set".into(),
            ));
        }
        let mut sorted = self.admins.clone();
        sorted.sort_unstable_by_key(|a| a.0);
        sorted.dedup();
        if sorted != self.admins {
            return Err(MlsError::PolicyViolation(
                "the admin set must be sorted and free of duplicates".into(),
            ));
        }
        if self.version == 0 {
            return Err(MlsError::PolicyViolation(
                "a policy's version starts at 1".into(),
            ));
        }
        Ok(())
    }

    /// Sign this policy as `signer`. The signature covers the canonical
    /// dag-cbor encoding under [`TAG_ROOM_POLICY`].
    pub fn sign(&self, signer: &ActorKeypair) -> Result<SignedRoomPolicy> {
        self.validate()?;
        self.sign_validated(signer)
    }

    /// [`Self::sign`] for a **community** room — validated by
    /// [`Self::validate_community`], which is the same check minus the
    /// end-to-end class's refusal of the `request` join rule — and bound to
    /// the room `room` by the [`TAG_ROOM_POLICY_IN_ROOM`] room signature.
    ///
    /// The counterpart of [`SignedRoomPolicy::verify_community_for`], and
    /// needed for the same reason it is: a community room may carry `request`
    /// (§ Join rules and invites — it has a reader who can vouch for a
    /// stranger), so signing one through [`Self::sign`] refuses a join rule its
    /// own class allows. A room's floor is where that verdict is enforced, and
    /// this is the signing half of the pair.
    ///
    /// Both signatures, always: the ordinary one is what
    /// [`SignedRoomPolicy::verify_signature_community`] checks, the room
    /// signature what keeps the version out of every other room. There is no
    /// community signing without the room.
    pub fn sign_community(
        &self,
        room: &[u8; 32],
        signer: &ActorKeypair,
    ) -> Result<SignedRoomPolicy> {
        self.validate_community()?;
        let mut signed = self.sign_validated(signer)?;
        signed.room_signature = Some(
            signer
                .signing_key()
                .sign(&room_signing_input(
                    TAG_ROOM_POLICY_IN_ROOM,
                    room,
                    &canonical(self)?,
                ))
                .to_bytes()
                .to_vec(),
        );
        Ok(signed)
    }

    /// The signing act itself, once, so the two class-validated entry points
    /// cannot drift on what a signature covers.
    fn sign_validated(&self, signer: &ActorKeypair) -> Result<SignedRoomPolicy> {
        let canonical = canonical(self)?;
        let signature = signer
            .signing_key()
            .sign(&signing_input(TAG_ROOM_POLICY, &canonical))
            .to_bytes()
            .to_vec();
        Ok(SignedRoomPolicy {
            policy: self.clone(),
            signer: signer.actor_id(),
            signature,
            countersignature: None,
            room_signature: None,
        })
    }

    /// Set the admin set from any order — sorted and deduplicated so the
    /// signed bytes are canonical.
    pub fn set_admins(&mut self, admins: impl IntoIterator<Item = ActorId>) {
        let mut v: Vec<ActorId> = admins.into_iter().filter(|a| *a != self.owner).collect();
        v.sort_unstable_by_key(|a| a.0);
        v.dedup();
        self.admins = v;
    }
}

/// A policy plus the detached signature of the principal that authored this
/// version. **The signer's role in the previous version decides which fields
/// it may have changed** ([`judge_commit`]); the signature itself only proves
/// the bytes are that principal's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRoomPolicy {
    pub policy: RoomPolicy,
    /// The principal whose key signed this version.
    pub signer: ActorId,
    /// Ed25519 over [`signing_input`] of [`TAG_ROOM_POLICY`] and the
    /// canonical policy.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
    /// The outgoing owner's countersignature on a version that hands the
    /// room to `signer` (module doc § Ownership transfer); absent on every
    /// other version, and absent from the canonical bytes when absent, so a
    /// policy without one encodes exactly as it did before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countersignature: Option<OwnershipCountersignature>,
    /// A **community** version's room signature: the signer's, over
    /// [`TAG_ROOM_POLICY_IN_ROOM`], the room id and the same canonical policy
    /// ([`RoomPolicy::sign_community`]). `signature` alone binds no room — the
    /// policy names none — so without this a version signed in one room is a
    /// valid version of every other room with the same owner. Absent on an
    /// end-to-end policy (its group context is its room) and on a community
    /// version signed before the field existed, and absent from the canonical
    /// bytes when absent, as `countersignature` is.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub room_signature: Option<Vec<u8>>,
}

/// Domain tag a community policy version's **room signature** covers —
/// `channel ‖ canonical(policy)`, framed as [`TAG_ROOM_OWNERSHIP_TRANSFER`]
/// is and for the reason it is: the room id in the signed bytes is what keeps
/// a version one room's owner signed from extending another room's chain.
pub const TAG_ROOM_POLICY_IN_ROOM: &str = "fauna.room.policy.in-room.v1 2026-09-22";

/// Domain tag the outgoing owner's transfer countersignature covers, framed
/// like [`TAG_ROOM_POLICY`] but over `channel ‖ canonical(policy)` — the
/// room id in the signed bytes is what keeps one room's hand-over from
/// completing another's.
pub const TAG_ROOM_OWNERSHIP_TRANSFER: &str = "fauna.room.policy.transfer.v1 2026-09-09";

/// The outgoing owner's consent to a hand-over: its detached signature over
/// the room's channel id and the canonical policy that names its successor
/// (module doc § Ownership transfer).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipCountersignature {
    /// The outgoing owner — the owner the chain resolved to when it signed.
    pub signer: ActorId,
    /// Ed25519 over [`room_signing_input`] of [`TAG_ROOM_OWNERSHIP_TRANSFER`],
    /// the channel id and the canonical policy.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl OwnershipCountersignature {
    /// Whether `signature` is `signer`'s over exactly `policy`, for the room
    /// whose channel id is `channel`.
    pub fn verify(&self, channel: &[u8; 32], policy: &RoomPolicy) -> Result<()> {
        let canonical = canonical(policy)?;
        if !verify_detached(
            &self.signer.0,
            &room_signing_input(TAG_ROOM_OWNERSHIP_TRANSFER, channel, &canonical),
            &self.signature,
        ) {
            return Err(MlsError::PolicyViolation(
                "ownership countersignature does not verify under its signer for this room".into(),
            ));
        }
        Ok(())
    }
}

impl RoomPolicy {
    /// Countersign this policy — which names the incoming owner — as the
    /// outgoing owner, bound to the room `channel`.
    pub fn countersign_transfer(
        &self,
        channel: &[u8; 32],
        signer: &ActorKeypair,
    ) -> Result<OwnershipCountersignature> {
        self.validate()?;
        let canonical = canonical(self)?;
        let signature = signer
            .signing_key()
            .sign(&room_signing_input(
                TAG_ROOM_OWNERSHIP_TRANSFER,
                channel,
                &canonical,
            ))
            .to_bytes()
            .to_vec();
        Ok(OwnershipCountersignature {
            signer: signer.actor_id(),
            signature,
        })
    }
}

/// What the outgoing owner posts on the channel: the policy naming its
/// successor and its own countersignature over it (module doc § Ownership
/// transfer). Carried as canonical dag-cbor bytes in
/// `GroupMetaMessage::OwnershipOffer`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomOwnershipOffer {
    pub policy: RoomPolicy,
    pub countersignature: OwnershipCountersignature,
}

impl RoomOwnershipOffer {
    /// The canonical dag-cbor bytes this offer rides the channel as.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        canonical(self)
    }

    /// Decode an offer off the channel. Says nothing about whether it is
    /// live — [`Self::verify_against`] does.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        fauna_cbor::decode_strict(bytes)
            .map_err(|e| MlsError::PolicyViolation(format!("ownership offer does not decode: {e}")))
    }

    /// Whether `me` may complete this offer against the policy `current` the
    /// room holds now: it names `me` as the incoming owner, its version is
    /// exactly the next one, and its countersignature is the current owner's
    /// over these bytes for room `channel`. The same conditions
    /// [`judge_commit`] applies to the commit the completion produces, asked
    /// first so a stale or foreign offer is dropped rather than committed.
    pub fn verify_against(
        &self,
        current: &RoomPolicyExtension,
        channel: &[u8; 32],
        me: &ActorId,
    ) -> Result<()> {
        if self.policy.owner != *me {
            return Err(MlsError::PolicyViolation(
                "the ownership offer names another member as the incoming owner".into(),
            ));
        }
        if self.policy.version != current.signed.policy.version + 1 {
            return Err(MlsError::PolicyViolation(format!(
                "the ownership offer is stale (offer version {}, room at {})",
                self.policy.version, current.signed.policy.version
            )));
        }
        if self.countersignature.signer != current.effective_owner() {
            return Err(MlsError::PolicyViolation(
                "the ownership offer is not countersigned by the room's owner".into(),
            ));
        }
        self.policy.validate()?;
        self.countersignature.verify(channel, &self.policy)
    }
}

impl SignedRoomPolicy {
    /// Whether `signature` is `signer`'s over exactly these policy bytes,
    /// the policy being a well-formed **end-to-end** one.
    pub fn verify_signature(&self) -> Result<()> {
        self.policy.validate()?;
        self.verify_signature_bytes()
    }

    /// The same, for a **community** room's policy — the floor's copy, which
    /// may carry the `request` join rule ([`RoomPolicy::validate_community`]).
    ///
    /// The ordinary signature only, which names no room: what every current
    /// reader checks. A judge of a version *for a room*
    /// asks [`Self::verify_community_for`].
    pub fn verify_signature_community(&self) -> Result<()> {
        self.policy.validate_community()?;
        self.verify_signature_bytes()
    }

    /// Whether this is a well-formed community policy version **of the room
    /// `binding` names**: the ordinary signature verifies
    /// ([`Self::verify_signature_community`]); and every version above 1
    /// carries a room signature that is the signer's for exactly this room
    /// ([`RoomBinding`]). Version 1 is bound by the birth salt instead
    /// ([`verify_community_birth`]); a room signature it carries must still be
    /// this room's — one for another room is positive evidence the version was
    /// signed elsewhere.
    pub fn verify_community_for(&self, binding: &RoomBinding) -> Result<()> {
        self.verify_signature_community()?;
        match &self.room_signature {
            Some(signature) => {
                if !verify_detached(
                    &self.signer.0,
                    &room_signing_input(
                        TAG_ROOM_POLICY_IN_ROOM,
                        &binding.room,
                        &canonical(&self.policy)?,
                    ),
                    signature,
                ) {
                    return Err(MlsError::PolicyViolation(
                        "this policy version was not signed for this room".into(),
                    ));
                }
                Ok(())
            }
            None if self.policy.version > 1 => Err(MlsError::PolicyViolation(
                "every community room binds each policy version above 1 to itself, and this \
                 one carries no room signature"
                    .into(),
            )),
            None => Ok(()),
        }
    }

    /// The signature check alone: these bytes are this signer's. Says
    /// nothing about whether the policy is well formed for any class — the
    /// two callers above pair it with the structural check their class owns.
    fn verify_signature_bytes(&self) -> Result<()> {
        let canonical = canonical(&self.policy)?;
        if !verify_detached(
            &self.signer.0,
            &signing_input(TAG_ROOM_POLICY, &canonical),
            &self.signature,
        ) {
            return Err(MlsError::PolicyViolation(
                "room policy signature does not verify under its signer".into(),
            ));
        }
        Ok(())
    }
}

/// Domain tag every room-invite signature covers, framed exactly like
/// [`TAG_ROOM_POLICY`] and the succession plane's records.
pub const TAG_ROOM_INVITE: &str = "fauna.room.invite.v1 2026-09-09";

/// One invitation into a room — "the inviter's signed act"
/// (`conversation-rooms.md` § Join rules and invites).
///
/// It is signed rather than merely authenticated because an invite outlives
/// the connection that carried it: a cross-nest invite reaches the invitee
/// through their *own* home nest, which cannot take the inviting nest's word
/// for who invited whom, and the invitee wants a record of the invitation it
/// accepted. Same-nest the nest also binds the signer to the authenticated
/// caller, so the two agree; the signature is what survives the boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomInvite {
    /// The room being joined.
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    /// Who is invited.
    pub invitee: ActorId,
    /// The role they are invited into — never `Owner`: a room has exactly
    /// one owner and it changes by transfer, not by invitation
    /// (§ Roles and authorization).
    pub role: RoomRole,
    /// The policy version the inviter read the join rule under. **No door
    /// compares this, on either leg, and that is a ruling rather than a
    /// gap** (`conversation-rooms.md` § Join rules and invites → *A
    /// cross-nest invitation*): the room's home nest judges an invitation
    /// against its *current* stored policy when it is issued and again when
    /// it is accepted, so a stale invite cannot bypass a join rule the room
    /// has since tightened — and the invitee's own nest, which relays a
    /// cross-nest acceptance, decides nothing about the room and so has no
    /// use for a version it cannot check. What the field is for is the
    /// invitee's device: it names the version the offer was made under,
    /// which the verified retained policy the newcomer later reads can be
    /// compared against.
    pub policy_version: u64,
}

impl RoomInvite {
    /// Structural validity — what a well-formed invite looks like whoever
    /// signed it.
    pub fn validate(&self) -> Result<()> {
        if self.room_id.len() != 32 {
            return Err(MlsError::PolicyViolation(
                "a room invite names a 32-byte room".into(),
            ));
        }
        if self.role == RoomRole::Owner {
            return Err(MlsError::PolicyViolation(
                "a room has exactly one owner, and it is transferred rather than invited".into(),
            ));
        }
        Ok(())
    }

    /// Sign this invite as `signer` — the inviter.
    pub fn sign(&self, signer: &ActorKeypair) -> Result<SignedRoomInvite> {
        self.validate()?;
        let canonical = canonical(self)?;
        let signature = signer
            .signing_key()
            .sign(&signing_input(TAG_ROOM_INVITE, &canonical))
            .to_bytes()
            .to_vec();
        Ok(SignedRoomInvite {
            invite: self.clone(),
            inviter: signer.actor_id(),
            signature,
        })
    }
}

/// An invite plus the detached signature of the principal that issued it.
/// **Whether that principal's role permits the invitation is decided by the
/// room, not by this record** — the signature only proves the bytes are the
/// inviter's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRoomInvite {
    pub invite: RoomInvite,
    /// The principal whose key signed this invitation.
    pub inviter: ActorId,
    /// Ed25519 over [`signing_input`] of [`TAG_ROOM_INVITE`] and the
    /// canonical invite.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl SignedRoomInvite {
    /// Whether `signature` is `inviter`'s over exactly these invite bytes.
    pub fn verify_signature(&self) -> Result<()> {
        self.invite.validate()?;
        let canonical = canonical(&self.invite)?;
        if !verify_detached(
            &self.inviter.0,
            &signing_input(TAG_ROOM_INVITE, &canonical),
            &self.signature,
        ) {
            return Err(MlsError::PolicyViolation(
                "room invite signature does not verify under its inviter".into(),
            ));
        }
        Ok(())
    }
}

/// Domain tag every labeler-set signature covers, framed exactly like
/// [`TAG_ROOM_POLICY`] and [`TAG_ROOM_INVITE`].
pub const TAG_ROOM_LABELERS: &str = "fauna.room.labelers.v1 2026-09-10";

/// The most labelers one room may name. Every named labeler runs on every
/// send, in the act that stores it (`content-scoring.md` § Timing — the label
/// lands before the fan-out), so the set is what bounds how much work one
/// message costs its home nest.
pub const MAX_ROOM_LABELERS: usize = 4;

/// The labeler artifact kinds a community room may name — the two that can
/// score a message. A `list` is keyed by post id and has nothing to say about
/// one (`content-moderation-and-ranking.md` § Tier-3 community models). The
/// home nest admits a set only over these; every app's editor offers only
/// these, from the one list.
pub const ROOM_LABELER_KINDS: [&str; 2] = [
    fauna_core::scoring::artifact_kind::WASM,
    fauna_core::scoring::artifact_kind::TEXT_MODEL,
];

/// Which transparent labelers a **community** room's home nest applies to the
/// room's messages (`conversation-rooms.md` § The three classes → *What the
/// home nest does with its read*, purpose 2) — the room-level twin of a user's
/// own labeler subscription.
///
/// **A sibling record, deliberately not a [`RoomPolicy`] field.** A policy is
/// verified over the canonical re-encode of what the verifier decoded
/// ([`SignedRoomPolicy::verify_signature`]), so a field an older app does not
/// know would make every policy carrying it fail that app's verification — and
/// an older app authoring the next `set_policy` from its own decode would drop
/// the field without knowing it had. A record of its own, set through its own
/// door, is invisible to every app that predates it, and a policy change can
/// never reset it. It is also scoped to the one class it means anything in:
/// an end-to-end room's policy rides its MLS group context, and the nest
/// reads nothing there to label.
///
/// Rule 6 of § Architectural rules governs it as "the rest" of the policy:
/// **the owner or an admin signs it**, members verify it, and the nest stores
/// it, refuses what the signature does not cover, and cannot author it.
/// Which ids are *admissible* — published, transparent, and of a kind that
/// can score a message — is the home nest's check against its own labeler
/// registry, not this record's: a well-signed set can still name something no
/// nest will run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomLabelers {
    /// The room this set governs — inside the signed bytes, so a set signed
    /// for one room never installs on another room the same admin holds.
    #[serde(with = "serde_bytes")]
    pub room_id: Vec<u8>,
    /// Monotonic from 1, every accepted change exactly `previous + 1`. A room
    /// that never named a labeler is at 0 and holds no record at all.
    pub version: u64,
    /// Published labeler ids (`fauna_core::scoring::AlgorithmLabeler::algorithm_id`),
    /// sorted and deduplicated so the signed bytes are canonical, at most
    /// [`MAX_ROOM_LABELERS`]. **Empty is a real set** — the way an admin stops
    /// the nest labelling a room it once labelled.
    pub labelers: Vec<ActorId>,
}

impl RoomLabelers {
    /// A set for `room_id` at `version`, from ids in any order — sorted and
    /// deduplicated here so an honest author always produces canonical bytes.
    pub fn new(
        room_id: [u8; 32],
        version: u64,
        labelers: impl IntoIterator<Item = ActorId>,
    ) -> Self {
        let mut labelers: Vec<ActorId> = labelers.into_iter().collect();
        labelers.sort_unstable_by_key(|a| a.0);
        labelers.dedup();
        Self {
            room_id: room_id.to_vec(),
            version,
            labelers,
        }
    }

    /// Structural validity — what a well-formed set looks like whoever signed
    /// it.
    pub fn validate(&self) -> Result<()> {
        if self.room_id.len() != 32 {
            return Err(MlsError::PolicyViolation(
                "a labeler set names a 32-byte room".into(),
            ));
        }
        if self.version == 0 {
            return Err(MlsError::PolicyViolation(
                "a labeler set's version starts at 1 — 0 is a room that names none".into(),
            ));
        }
        let mut sorted = self.labelers.clone();
        sorted.sort_unstable_by_key(|a| a.0);
        sorted.dedup();
        if sorted != self.labelers {
            return Err(MlsError::PolicyViolation(
                "a labeler set must be sorted and free of duplicates".into(),
            ));
        }
        if self.labelers.len() > MAX_ROOM_LABELERS {
            return Err(MlsError::PolicyViolation(format!(
                "a room names at most {MAX_ROOM_LABELERS} labelers"
            )));
        }
        Ok(())
    }

    /// Sign this set as `signer` — the room's owner or one of its admins.
    pub fn sign(&self, signer: &ActorKeypair) -> Result<SignedRoomLabelers> {
        self.validate()?;
        let canonical = canonical(self)?;
        let signature = signer
            .signing_key()
            .sign(&signing_input(TAG_ROOM_LABELERS, &canonical))
            .to_bytes()
            .to_vec();
        Ok(SignedRoomLabelers {
            labelers: self.clone(),
            signer: signer.actor_id(),
            signature,
        })
    }
}

/// A labeler set plus the detached signature of the principal that authored
/// this version. **Whether that principal's role permits it is decided by the
/// room's floor, not by this record** — the signature only proves the bytes
/// are the signer's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRoomLabelers {
    pub labelers: RoomLabelers,
    /// The principal whose key signed this version.
    pub signer: ActorId,
    /// Ed25519 over [`signing_input`] of [`TAG_ROOM_LABELERS`] and the
    /// canonical set.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl SignedRoomLabelers {
    /// Whether `signature` is `signer`'s over exactly these set bytes, and
    /// the set is well formed.
    pub fn verify_signature(&self) -> Result<()> {
        self.labelers.validate()?;
        let canonical = canonical(&self.labelers)?;
        if !verify_detached(
            &self.signer.0,
            &signing_input(TAG_ROOM_LABELERS, &canonical),
            &self.signature,
        ) {
            return Err(MlsError::PolicyViolation(
                "labeler set signature does not verify under its signer".into(),
            ));
        }
        Ok(())
    }
}

/// Domain tag a **floor delete record**'s signature is made under — a tag of
/// its own, so the signature can never be reframed as a policy, a labeler set
/// or a sealed message's signed core.
pub const TAG_ROOM_FLOOR_DELETE: &str = "fauna.room.floor-delete.v1 2026-09-19";

/// An owner's or admin's delete of another member's message in a **community
/// room** — the floor act (`conversation-rooms.md` § Roles and authorization →
/// *Delete any message — the mechanism* → *Community rooms*). It rides
/// *unsealed*, in [`ChannelEnvelope::RoomFloorDelete`], so the room's home nest
/// can judge it from the record alone, without reading anything: nothing in it
/// is content.
///
/// **The record proves who said it, never that they may.** Whether `author`
/// holds owner or admin is decided against the signed policy **of the version
/// the record names** — by the floor before it stores the record, and again by
/// every member before it paints a tombstone.
///
/// [`ChannelEnvelope::RoomFloorDelete`]: crate::types::ChannelEnvelope::RoomFloorDelete
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomFloorDelete {
    /// The room the record is for — bound under the signature, so a record
    /// lifted into another room does not verify there.
    #[serde(with = "serde_bytes")]
    pub room: Vec<u8>,
    /// The log position of the message being tombstoned.
    pub target_seq: u64,
    /// The principal making the delete; the signature is theirs.
    pub author: ActorId,
    /// The [`RoomPolicy::version`] the author acts under. A delete is judged
    /// by the policy it was made under, never by the policy a member holds
    /// "now" — this is what pins it.
    pub policy_version: u64,
}

impl RoomFloorDelete {
    /// Sign this record as `signer`, who must be its `author` — a record
    /// naming one principal under another's signature is never minted.
    pub fn sign(&self, signer: &ActorKeypair) -> Result<SignedRoomFloorDelete> {
        if signer.actor_id() != self.author {
            return Err(MlsError::PolicyViolation(
                "a floor delete record is signed by the author it names".into(),
            ));
        }
        let canonical = canonical(self)?;
        let signature = signer
            .signing_key()
            .sign(&signing_input(TAG_ROOM_FLOOR_DELETE, &canonical))
            .to_bytes()
            .to_vec();
        Ok(SignedRoomFloorDelete {
            record: self.clone(),
            signature,
        })
    }
}

/// A [`RoomFloorDelete`] plus its author's detached signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRoomFloorDelete {
    pub record: RoomFloorDelete,
    /// Ed25519 by `record.author` over [`signing_input`] of
    /// [`TAG_ROOM_FLOOR_DELETE`] and the canonical record.
    #[serde(with = "serde_bytes")]
    pub signature: Vec<u8>,
}

impl SignedRoomFloorDelete {
    /// The canonical dag-cbor bytes the envelope variant carries.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        canonical(self)
    }

    /// Strict-decode the bytes an envelope carried. Verifies nothing — call
    /// [`Self::verify`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        fauna_cbor::decode_strict(bytes).map_err(|e| {
            MlsError::PolicyViolation(format!("floor delete record does not decode: {e}"))
        })
    }

    /// Whether this record is for `room` and its signature is its author's
    /// over exactly these bytes. **The author's role is not judged here** —
    /// see [`RoomFloorDelete`].
    pub fn verify(&self, room: &[u8; 32]) -> Result<()> {
        if self.record.room.as_slice() != room.as_slice() {
            return Err(MlsError::PolicyViolation(
                "floor delete record names another room".into(),
            ));
        }
        let canonical = canonical(&self.record)?;
        if !verify_detached(
            &self.record.author.0,
            &signing_input(TAG_ROOM_FLOOR_DELETE, &canonical),
            &self.signature,
        ) {
            return Err(MlsError::PolicyViolation(
                "floor delete record signature does not verify under its author".into(),
            ));
        }
        Ok(())
    }
}

// ── the community class's policy chain ─────────────────────────

/// Whom a policy's *name* designates once successions are resolved. The one
/// input the community judges below cannot compute for themselves, because the
/// two sides that run them learn it differently: the room's home nest from its
/// own floor and succession tables, a member from succession statements it has
/// verified. A caller that resolves nothing passes [`names_only`], under which
/// a name designates only itself.
///
/// Two questions, because the two sides answer the first differently:
///
/// - [`Self::designates`] — may `actor` act under `name`? The home nest answers
///   with one seat (the newest live one along the name's line — it judges at
///   the time of the act). A member re-proving a chain later cannot know which
///   seat was live then, so it answers with the **line**: the name and every
///   verified successor after it ([`SuccessionLines`]). The nest's one seat is
///   always on that line, so the floor never admits a step its members refuse.
/// - [`Self::seat`] — one canonical identity per line, so two policies' owner
///   and admin sets compare as seats rather than as bytes.
///
/// Any `Fn(&ActorId) -> ActorId` is a single-valued designation: a name
/// designates exactly the seat the function returns.
pub trait SeatDesignation {
    /// The canonical seat `name` stands for.
    fn seat(&self, name: &ActorId) -> ActorId;

    /// Whether `name` designates `actor`.
    fn designates(&self, name: &ActorId, actor: &ActorId) -> bool {
        self.seat(name) == *actor
    }
}

impl<F: Fn(&ActorId) -> ActorId> SeatDesignation for F {
    fn seat(&self, name: &ActorId) -> ActorId {
        self(name)
    }
}

/// The designation a community judge is handed.
pub type SeatDesignee<'a> = &'a dyn SeatDesignation;

/// The [`SeatDesignee`] of a judge that resolves no succession: every name
/// stands for itself. Fails closed — a successor's signature is then a
/// stranger's.
pub fn names_only(name: &ActorId) -> ActorId {
    *name
}

/// The member side's [`SeatDesignation`]: the succession lines this device has
/// **verified itself** (`conversation-rooms.md` § Roles and authorization →
/// *Delete any message — the mechanism* → *A name designates its verified
/// line*). Empty, it is [`names_only`].
///
/// A name designates itself and every identity **after** it on a verified
/// line — never one before it. So a policy that still names a succeeded
/// identity is followed through its successors, and once a version names the
/// successor itself the retired key is off that name's line for good.
#[derive(Clone, Debug, Default)]
pub struct SuccessionLines {
    /// Each line oldest first, its root included. Never shorter than two.
    lines: Vec<Vec<ActorId>>,
}

impl SuccessionLines {
    /// Record the verified line rooted at `name`: `successors` oldest first,
    /// empty when the identity was never succeeded. **A held line only ever
    /// grows** (`conversation-rooms.md` § Roles and authorization → *A name
    /// designates its verified line* → *A verified line holds only so far*):
    /// a re-answer for a root already held is taken only when it EXTENDS the
    /// held line — its newest holder succeeded in turn — and a shorter or
    /// forking answer (a home nest now denying a succession this device
    /// verified) leaves the held line standing. Returns whether the
    /// designation grew — the one news worth judging a refusal again for.
    pub fn insert(&mut self, name: ActorId, successors: &[ActorId]) -> bool {
        if successors.is_empty() {
            return false;
        }
        if let Some(held) = self.lines.iter_mut().find(|line| line[0] == name) {
            let held_len = held.len() - 1;
            let extends = successors.len() > held_len && successors[..held_len] == held[1..];
            if !extends {
                return false;
            }
            held.extend_from_slice(&successors[held_len..]);
            return true;
        }
        let mut line = Vec::with_capacity(successors.len() + 1);
        line.push(name);
        line.extend_from_slice(successors);
        self.lines.push(line);
        true
    }
}

impl SeatDesignation for SuccessionLines {
    fn seat(&self, name: &ActorId) -> ActorId {
        self.lines
            .iter()
            .find(|line| line.contains(name))
            .map_or(*name, |line| line[line.len() - 1])
    }

    fn designates(&self, name: &ActorId, actor: &ActorId) -> bool {
        name == actor
            || self.lines.iter().any(|line| {
                line.iter()
                    .position(|id| id == name)
                    .is_some_and(|at| line[at + 1..].contains(actor))
            })
    }
}

/// `actor`'s rank under a **community** room's `policy`: owner when the
/// policy's owner name designates it, admin when any admin name does, member
/// otherwise. The community twin of [`RoomPolicyExtension::role_of`], which
/// resolves through the group context's own succession record instead.
pub fn community_role_of(
    policy: &RoomPolicy,
    actor: &ActorId,
    designee: SeatDesignee<'_>,
) -> RoomRole {
    if designee.designates(&policy.owner, actor) {
        return RoomRole::Owner;
    }
    if policy.admins.iter().any(|a| designee.designates(a, actor)) {
        return RoomRole::Admin;
    }
    RoomRole::Member
}

/// The room a community policy version is judged for — the one input that
/// binds a version above 1 to a room at all, since the policy itself names
/// none (`conversation-rooms.md` § Roles and authorization → *A fetched version
/// is anchored, never believed*).
///
/// Version 1 is bound by the birth salt, which derives the room id. Every
/// later version is bound by its **room signature**
/// ([`SignedRoomPolicy::room_signature`]), which every community room
/// requires: a version above 1 without one is refused, so the home nest can
/// neither strip the signature and serve what is left nor splice in a version
/// signed in another room of the same owner. Needing nothing but the room id,
/// the binding is the same for a judge holding the birth salt and a reader
/// that does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomBinding {
    room: [u8; 32],
}

impl RoomBinding {
    /// The binding of the room `room`.
    pub fn new(room: [u8; 32]) -> Self {
        Self { room }
    }

    /// The room this binding judges versions for.
    pub fn room(&self) -> &[u8; 32] {
        &self.room
    }
}

/// Verify a community room's **birth policy** — version 1 — and return the
/// room id it founds with `salt`.
///
/// This is what anchors the chain to the room: the id is a commitment to the
/// founder's key ([`derive_room_id`]), so a version 1 whose derived id is the
/// room's can only have been signed by the principal the room id names. The
/// founder signs under its own name, alone — a birth record names no admins,
/// since at birth nobody else is on the floor. Every later version is bound
/// by its room signature instead ([`RoomBinding`]).
pub fn verify_community_birth(signed: &SignedRoomPolicy, salt: &[u8; 32]) -> Result<[u8; 32]> {
    if !is_binding_birth_salt(salt) {
        return Err(MlsError::PolicyViolation(
            "a community room's birth salt must open with the binding mark".into(),
        ));
    }
    let room = derive_room_id(&signed.policy.owner, salt)?;
    signed.verify_community_for(&RoomBinding::new(room))?;
    if signed.signer != signed.policy.owner {
        return Err(MlsError::PolicyViolation(
            "a birth record's policy must be signed by the owner it names".into(),
        ));
    }
    if signed.policy.version != 1 {
        return Err(MlsError::PolicyViolation(
            "a birth record carries policy version 1".into(),
        ));
    }
    if !signed.policy.admins.is_empty() {
        return Err(MlsError::PolicyViolation(
            "a birth record names no admins — a room is founded with its owner alone".into(),
        ));
    }
    Ok(room)
}

/// Why [`judge_community_policy_step`] refused a change — three kinds, because
/// the home nest answers them differently (a malformed or mis-versioned offer
/// is the caller's mistake; a rank refusal is a permission the caller lacks)
/// while a member treats all three alike: the chain does not extend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyStepRefusal {
    /// The offer is not a well-formed community policy under its signer's
    /// signature.
    Malformed(String),
    /// The offer is not exactly one version on from the policy it would
    /// replace.
    Version { held: u64, offered: u64 },
    /// The signer's rank in the version before does not allow the change.
    Rank(&'static str),
}

impl std::fmt::Display for PolicyStepRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "room policy does not verify: {why}"),
            Self::Version { held, offered } => write!(
                f,
                "a policy change is exactly one version on: this room is at {held}, the \
                 offer is {offered}"
            ),
            Self::Rank(why) => f.write_str(why),
        }
    }
}

/// The roles table applied to one **community** policy change: may `next`
/// follow `prev`? The community twin of [`judge_extension_change`], and the
/// one judge both sides run — the home nest before it stores a change, a
/// member before it lets a fetched version grant anyone a rank.
///
/// - the signature is the signer's, over a well-formed community policy **of
///   the room `binding` names** ([`SignedRoomPolicy::verify_community_for`]) —
///   so a version one owner signed in another room of theirs is refused here;
/// - the version is exactly `prev + 1`;
/// - the signer's rank **under `prev`** decides what it may have changed: the
///   owner anything (handing the room on included — in this class a transfer
///   is the outgoing owner's own signed version), an admin everything but the
///   owner and the admin set, a member nothing.
///
/// Owner and admin sets are compared as the seats their names designate, so a
/// successor re-signing a policy under its own name changes nobody's rank.
///
/// What is **not** judged here is what only the floor knows: that the caller
/// is the signer, that every admin named is a live member, that an incoming
/// owner is a user homed on this nest. Those stay the home nest's doors'.
pub fn judge_community_policy_step(
    prev: &RoomPolicy,
    next: &SignedRoomPolicy,
    binding: &RoomBinding,
    designee: SeatDesignee<'_>,
) -> std::result::Result<(), PolicyStepRefusal> {
    next.verify_community_for(binding)
        .map_err(|e| PolicyStepRefusal::Malformed(e.to_string()))?;
    if next.policy.version != prev.version + 1 {
        return Err(PolicyStepRefusal::Version {
            held: prev.version,
            offered: next.policy.version,
        });
    }
    match community_role_of(prev, &next.signer, designee) {
        RoomRole::Owner => Ok(()),
        RoomRole::Admin => {
            let seats = |names: &[ActorId]| {
                let mut seats: Vec<[u8; 32]> = names.iter().map(|a| designee.seat(a).0).collect();
                seats.sort_unstable();
                seats.dedup();
                seats
            };
            if designee.seat(&next.policy.owner) != designee.seat(&prev.owner)
                || seats(&next.policy.admins) != seats(&prev.admins)
            {
                return Err(PolicyStepRefusal::Rank(
                    "only the owner changes the owner or the admin set",
                ));
            }
            Ok(())
        }
        RoomRole::Member => Err(PolicyStepRefusal::Rank(
            "a policy change is signed by the room's owner or an admin",
        )),
    }
}

/// A community room's signed policy versions, **anchored**: version 1 proven
/// to be the founder's for this room ([`verify_community_birth`]) and every
/// later version proven to be this room's and to follow the one before it
/// ([`judge_community_policy_step`] under the room's [`RoomBinding`]).
///
/// This is what lets a member grant a rank off a policy its home nest served.
/// A signature alone proves only that *somebody* signed the bytes, and the
/// nest chooses which bytes to serve — so a policy is believed only at the end
/// of a chain whose first link the room id itself commits to, and whose every
/// later link is signed for this room. The nest can withhold a link (the
/// member then fails closed); it cannot mint one, nor lift one from another
/// room.
#[derive(Clone, Debug)]
pub struct CommunityPolicyChain {
    binding: RoomBinding,
    /// `versions[i]` is policy version `i + 1`. Never empty.
    versions: Vec<SignedRoomPolicy>,
}

impl CommunityPolicyChain {
    /// Anchor a chain at `birth`, the room's version 1, under the room's
    /// birth `salt`.
    pub fn anchor(room: &[u8; 32], salt: &[u8; 32], birth: SignedRoomPolicy) -> Result<Self> {
        if verify_community_birth(&birth, salt)? != *room {
            return Err(MlsError::PolicyViolation(
                "this birth record founds another room".into(),
            ));
        }
        Ok(Self {
            binding: RoomBinding::new(*room),
            versions: vec![birth],
        })
    }

    /// The room this chain is anchored to.
    pub fn room(&self) -> &[u8; 32] {
        self.binding.room()
    }

    /// The newest version the chain has verified.
    pub fn head_version(&self) -> u64 {
        self.versions.len() as u64
    }

    /// Extend the chain by the version after its head. A refusal leaves the
    /// chain as it was.
    pub fn extend(&mut self, next: SignedRoomPolicy, designee: SeatDesignee<'_>) -> Result<()> {
        self.try_extend(next, designee)
            .map_err(|refusal| MlsError::PolicyViolation(refusal.to_string()))
    }

    /// [`Self::extend`], answering *why* a version was refused — a member that
    /// resolves successions lazily resolves more only on a
    /// [`PolicyStepRefusal::Rank`], the one refusal a succession can lift.
    pub fn try_extend(
        &mut self,
        next: SignedRoomPolicy,
        designee: SeatDesignee<'_>,
    ) -> std::result::Result<(), PolicyStepRefusal> {
        judge_community_policy_step(self.head(), &next, &self.binding, designee)?;
        self.versions.push(next);
        Ok(())
    }

    /// The newest policy the chain has verified.
    pub fn head(&self) -> &RoomPolicy {
        &self.versions[self.versions.len() - 1].policy
    }

    /// The anchored policy of `version`, when the chain reaches it.
    pub fn policy_at(&self, version: u64) -> Option<&RoomPolicy> {
        self.signed_at(version).map(|signed| &signed.policy)
    }

    /// The anchored version `version` as its signer signed it, when the chain
    /// reaches it — what a member amends and renders, never the bytes the home
    /// nest happens to serve as current.
    pub fn signed_at(&self, version: u64) -> Option<&SignedRoomPolicy> {
        let index = usize::try_from(version.checked_sub(1)?).ok()?;
        self.versions.get(index)
    }

    /// Every policy the chain has anchored, version 1 first.
    pub fn policies(&self) -> impl Iterator<Item = &RoomPolicy> {
        self.versions.iter().map(|signed| &signed.policy)
    }
}

/// One identity succession the room has recorded: `old` named `new` as its
/// successor, in a group-context change `old`'s own leaf committed (or a
/// principal outranking `old` vouched for). Roles resolve through these
/// ([`RoomPolicyExtension::role_of`]); the succession's own two commits are
/// admitted by them ([`judge_commit`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedSuccession {
    pub old: ActorId,
    pub new: ActorId,
}

/// The whole group-context extension: the signed policy and the append-only
/// succession record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomPolicyExtension {
    pub signed: SignedRoomPolicy,
    /// Append-only, in record order. Never contains two entries with the
    /// same `old`, and never an entry whose `new` is an earlier entry's `old`
    /// (a chain runs forward only).
    #[serde(default)]
    pub successions: Vec<RecordedSuccession>,
}

impl RoomPolicyExtension {
    /// A fresh extension around a signed policy, with no successions.
    pub fn new(signed: SignedRoomPolicy) -> Self {
        Self {
            signed,
            successions: Vec::new(),
        }
    }

    /// The canonical dag-cbor bytes this extension rides the group context
    /// as.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        canonical(self)
    }

    /// Decode and structurally validate extension bytes. A decode failure is
    /// a [`MlsError::PolicyViolation`]: the bytes are agreed group state, so
    /// every member fails them identically.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let ext: Self = fauna_cbor::decode_strict(bytes).map_err(|e| {
            MlsError::PolicyViolation(format!("room policy extension does not decode: {e}"))
        })?;
        ext.validate()?;
        Ok(ext)
    }

    /// Structural validity of the whole extension: a well-formed, correctly
    /// signed policy and a forward-only succession record.
    pub fn validate(&self) -> Result<()> {
        self.signed.verify_signature()?;
        let mut seen_old: Vec<ActorId> = Vec::with_capacity(self.successions.len());
        for s in &self.successions {
            if s.old == s.new {
                return Err(MlsError::PolicyViolation(
                    "a succession cannot name an identity as its own successor".into(),
                ));
            }
            if seen_old.contains(&s.old) {
                return Err(MlsError::PolicyViolation(
                    "an identity is succeeded at most once".into(),
                ));
            }
            if seen_old.contains(&s.new) {
                return Err(MlsError::PolicyViolation(
                    "a succession cannot point back at an already-succeeded identity".into(),
                ));
            }
            seen_old.push(s.old);
        }
        Ok(())
    }

    /// Follow the succession chain from `actor` to its current holder —
    /// `actor` itself when it was never succeeded.
    pub fn resolve(&self, actor: ActorId) -> ActorId {
        self.successor_chain(actor).last().copied().unwrap_or(actor)
    }

    /// Every identity on `actor`'s forward succession chain, immediate
    /// successor through the terminal holder, in hop order — `resolve`'s
    /// intermediate stops, not only its answer. Empty when `actor` was never
    /// succeeded.
    ///
    /// Callers that must decide whether a chain is **live** (some real
    /// successor is currently seated, not necessarily its terminal one — a
    /// parked chain can have a recorded hop whose successor has not joined
    /// yet, `ConversationsManager::apply_inbound_roster`'s drop arm) walk this
    /// whole list rather than `resolve`'s single terminal identity.
    pub fn successor_chain(&self, actor: ActorId) -> Vec<ActorId> {
        let mut chain = Vec::new();
        let mut current = actor;
        for _ in 0..MAX_SUCCESSION_HOPS {
            match self.successions.iter().find(|s| s.old == current) {
                Some(s) => {
                    chain.push(s.new);
                    current = s.new;
                }
                None => break,
            }
        }
        chain
    }

    /// The immediate predecessor `actor` succeeded, if any.
    pub fn predecessor_of(&self, actor: &ActorId) -> Option<ActorId> {
        self.successions
            .iter()
            .find(|s| s.new == *actor)
            .map(|s| s.old)
    }

    /// The successor `actor` named, if any.
    pub fn successor_of(&self, actor: &ActorId) -> Option<ActorId> {
        self.successions
            .iter()
            .find(|s| s.old == *actor)
            .map(|s| s.new)
    }

    /// The **effective** owner — the policy's owner resolved through the
    /// succession chain.
    pub fn effective_owner(&self) -> ActorId {
        self.resolve(self.signed.policy.owner)
    }

    /// `actor`'s effective role: owner if the policy's owner resolves to it,
    /// admin if any listed admin resolves to it, else member. A succeeded
    /// identity resolves *away* from its role (its successor holds it), which
    /// is what lets the successor evict it.
    pub fn role_of(&self, actor: &ActorId) -> RoomRole {
        if self.effective_owner() == *actor {
            return RoomRole::Owner;
        }
        if self
            .signed
            .policy
            .admins
            .iter()
            .any(|a| self.resolve(*a) == *actor)
        {
            return RoomRole::Admin;
        }
        RoomRole::Member
    }

    /// The extension with `succession` appended — what the old leaf commits
    /// before add-successor. Validates the result's chain shape.
    pub fn with_succession(&self, succession: RecordedSuccession) -> Result<Self> {
        let mut next = self.clone();
        next.successions.push(succession);
        next.validate()?;
        Ok(next)
    }
}

/// What one staged commit would do, read off the bytes by the engine — the
/// inputs [`judge_commit`] needs and nothing else, so the verdict is testable
/// without an MLS group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedCommitFacts {
    /// The room the commit is for — its channel id, the value a transfer
    /// countersignature is bound to.
    pub channel: [u8; 32],
    /// The committer's MLS-authenticated leaf identity.
    pub committer: ActorId,
    /// Identities of the leaves the commit adds (from each Add proposal's
    /// KeyPackage credential).
    pub adds: Vec<ActorId>,
    /// Identities of the leaves the commit removes, resolved against the
    /// pre-commit tree.
    pub removes: Vec<ActorId>,
    /// The room-policy extension the commit would install, when it carries a
    /// GroupContextExtensions proposal: `Some(Ok(_))` for a decodable one,
    /// `Some(Err(reason))` for one that does not decode or validate, `None`
    /// when the commit leaves the group context extensions alone. A commit
    /// that *drops* the extension is `Some(Err(_))`.
    pub next_extension: Option<std::result::Result<RoomPolicyExtension, String>>,
    /// The commit carries a proposal of a kind the policy has no rule for
    /// (PreSharedKey, ReInit, ExternalInit, …).
    pub other_proposals: bool,
}

/// [`judge_commit`]'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitVerdict {
    Permit,
    Refuse(String),
}

impl CommitVerdict {
    pub fn is_permit(&self) -> bool {
        matches!(self, Self::Permit)
    }
}

/// The roles table (`conversation-rooms.md` § Roles and authorization)
/// applied to one commit under the policy `current` — the verdict every
/// honest member computes before the commit touches its epoch.
///
/// Deterministic by construction: `current` is agreed group state and
/// `facts` is read off the commit bytes.
pub fn judge_commit(current: &RoomPolicyExtension, facts: &StagedCommitFacts) -> CommitVerdict {
    let committer = facts.committer;

    if facts.other_proposals {
        return CommitVerdict::Refuse(
            "a commit carrying a proposal kind the room policy has no rule for".into(),
        );
    }

    // A group-context change: the successions may grow (forward only, each
    // entry vouched for by its own old identity or by an owner/admin), and the
    // signed policy may change only as the signer's role allows.
    let next: std::borrow::Cow<'_, RoomPolicyExtension> = match &facts.next_extension {
        None => std::borrow::Cow::Borrowed(current),
        Some(Err(reason)) => {
            return CommitVerdict::Refuse(format!(
                "the commit would install an invalid room policy extension: {reason}"
            ));
        }
        Some(Ok(next)) => {
            if let Some(reason) = judge_extension_change(current, next, &committer, &facts.channel)
            {
                return CommitVerdict::Refuse(reason);
            }
            std::borrow::Cow::Borrowed(next)
        }
    };

    // Roles resolve through the successions the commit itself may have just
    // recorded, so the admissions below see a chain the same commit extended.
    let role = next.role_of(&committer);
    let join_rule = current.signed.policy.join_rule;

    for added in &facts.adds {
        let permitted = role.is_admin_or_owner()
            || join_rule == JoinRule::MemberInvite
            || next.successor_of(&committer) == Some(*added);
        if !permitted {
            return CommitVerdict::Refuse(format!(
                "Add of {} by {} ({:?}) is not permitted under the {:?} join rule",
                added.to_hex(),
                committer.to_hex(),
                role,
                join_rule
            ));
        }
    }

    for removed in &facts.removes {
        let is_own_predecessor = next.predecessor_of(&committer) == Some(*removed);
        let removed_role = next.role_of(removed);
        let permitted =
            is_own_predecessor || (role.is_admin_or_owner() && removed_role != RoomRole::Owner);
        if !permitted {
            let why = if removed_role == RoomRole::Owner {
                "the owner's membership is not removable until ownership is transferred"
            } else {
                "only an owner or admin removes a member"
            };
            return CommitVerdict::Refuse(format!(
                "Remove of {} by {} ({:?}) refused: {why}",
                removed.to_hex(),
                committer.to_hex(),
                role
            ));
        }
    }

    CommitVerdict::Permit
}

/// The group-context half of the verdict: `None` when the change from
/// `current` to `next`, committed by `committer`, is permitted.
fn judge_extension_change(
    current: &RoomPolicyExtension,
    next: &RoomPolicyExtension,
    committer: &ActorId,
    channel: &[u8; 32],
) -> Option<String> {
    // 1. Successions are append-only.
    if next.successions.len() < current.successions.len()
        || next.successions[..current.successions.len()] != current.successions[..]
    {
        return Some("the succession record is append-only".into());
    }
    // `next` validated its own chain shape; here only *who* may append: the
    // succeeded identity's own leaf, or a principal that strictly outranks
    // it (module doc § Successions, the vouch arm).
    let committer_role_now = current.role_of(committer);
    for appended in &next.successions[current.successions.len()..] {
        let self_record = appended.old == *committer;
        if !self_record && !outranks(committer_role_now, current.role_of(&appended.old)) {
            return Some(format!(
                "succession {} → {} may be recorded only by {} itself or by a principal \
                 outranking it",
                appended.old.to_hex(),
                appended.new.to_hex(),
                appended.old.to_hex()
            ));
        }
    }

    // 2. The signed policy: unchanged bytes need no further check.
    if next.signed == current.signed {
        return None;
    }
    let prev = &current.signed.policy;
    let new = &next.signed.policy;
    if next.signed.signer != *committer {
        return Some("a policy change is committed by the principal that signed it".into());
    }
    if new.version != prev.version + 1 {
        return Some(format!(
            "policy version must advance by exactly one ({} → {})",
            prev.version, new.version
        ));
    }
    // 3. An owner field that leaves the recorded chain is an ownership
    //    transfer (module doc § Ownership transfer): signed and committed by
    //    the incoming owner, countersigned by the outgoing one for this room.
    let outgoing = next.resolve(prev.owner);
    if new.owner != prev.owner && new.owner != outgoing {
        if next.signed.signer != new.owner {
            return Some(
                "an ownership transfer is signed and committed by the incoming owner".into(),
            );
        }
        let Some(countersignature) = &next.signed.countersignature else {
            return Some(
                "an ownership transfer carries the outgoing owner's countersignature".into(),
            );
        };
        if countersignature.signer != outgoing {
            return Some("the ownership countersignature is not the outgoing owner's".into());
        }
        if let Err(e) = countersignature.verify(channel, new) {
            return Some(e.to_string());
        }
        return None;
    }
    // The signer's role is read in the chain the commit installs, so an
    // owner's successor may re-sign right after recording its succession.
    let signer_role = next.role_of(&next.signed.signer);
    match signer_role {
        // The owner field, if it moved at all, moved along the chain (the
        // transfer arm above took every other move).
        RoomRole::Owner => None,
        RoomRole::Admin => {
            if new.owner != prev.owner || new.admins != prev.admins {
                return Some("only the owner changes the owner or the admin set".into());
            }
            None
        }
        RoomRole::Member => Some("a member cannot change the room policy".into()),
    }
}

/// Whether `voucher` strictly outranks `vouched` — owner over admin over
/// member; equal rank is never outranking.
fn outranks(voucher: RoomRole, vouched: RoomRole) -> bool {
    fn rank(role: RoomRole) -> u8 {
        match role {
            RoomRole::Owner => 2,
            RoomRole::Admin => 1,
            RoomRole::Member => 0,
        }
    }
    rank(voucher) > rank(vouched)
}

/// `[tag_len: u8] ‖ tag ‖ channel ‖ canonical` — [`signing_input`] with the
/// room's channel id in front of the policy bytes, so a transfer
/// countersignature is a statement about one room.
/// The signing input of a record bound to its room: `tag`, framed as
/// [`signing_input`] frames it, over `room ‖ canonical`.
fn room_signing_input(tag: &str, room: &[u8; 32], canonical: &[u8]) -> Vec<u8> {
    let mut bound = Vec::with_capacity(room.len() + canonical.len());
    bound.extend_from_slice(room);
    bound.extend_from_slice(canonical);
    signing_input(tag, &bound)
}

fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    fauna_cbor::encode_canonical(value)
        .map_err(|e| MlsError::Encoding(format!("room policy canonical encode: {e}")))
}

/// `[tag_len: u8] ‖ tag ‖ canonical` — the succession plane's injective
/// framing, reused verbatim so a room-policy signature can never be reframed
/// as a record of another plane.
fn signing_input(tag: &str, canonical: &[u8]) -> Vec<u8> {
    let tag_len = u8::try_from(tag.len()).expect("room policy domain tag is < 256 bytes");
    let mut out = Vec::with_capacity(1 + tag.len() + canonical.len());
    out.push(tag_len);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(canonical);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kp(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    // ── the community policy chain ─────────────────────────────

    const SALT: [u8; 32] = binding_birth_salt(&[0x5a; 24]);

    /// A room founded by `owner`, its birth policy, and the chain anchored on
    /// it.
    fn founded(owner: &ActorKeypair) -> ([u8; 32], SignedRoomPolicy, CommunityPolicyChain) {
        let room = derive_room_id(&owner.actor_id(), &SALT).unwrap();
        let birth = RoomPolicy::initial(owner.actor_id(), Some("square".into()))
            .sign_community(&room, owner)
            .unwrap();
        let chain = CommunityPolicyChain::anchor(&room, &SALT, birth.clone()).unwrap();
        (room, birth, chain)
    }

    /// `prev` one version on, edited by `edit`, signed by `signer` for `room`.
    fn next_version(
        room: &[u8; 32],
        prev: &RoomPolicy,
        signer: &ActorKeypair,
        edit: impl FnOnce(&mut RoomPolicy),
    ) -> SignedRoomPolicy {
        let mut policy = prev.clone();
        policy.version += 1;
        edit(&mut policy);
        policy.sign_community(room, signer).unwrap()
    }

    #[test]
    fn a_community_chain_anchors_only_on_the_founders_own_birth_record() {
        let owner = kp(1);
        let (room, birth, chain) = founded(&owner);
        assert_eq!(chain.head_version(), 1);
        assert_eq!(chain.policy_at(1), Some(&birth.policy));
        assert_eq!(chain.policy_at(0), None);
        assert_eq!(chain.policy_at(2), None);

        // The forgery the anchor exists for: a key the room id does not name
        // signs a perfectly well-formed version 1 for this room.
        let minted = kp(9);
        let forged = RoomPolicy::initial(minted.actor_id(), None)
            .sign_community(&room, &minted)
            .unwrap();
        assert!(CommunityPolicyChain::anchor(&room, &SALT, forged).is_err());
        // … and the true birth record under another salt, or for another room.
        assert!(
            CommunityPolicyChain::anchor(&room, &binding_birth_salt(&[0; 24]), birth.clone())
                .is_err()
        );
        assert!(CommunityPolicyChain::anchor(&[7u8; 32], &SALT, birth.clone()).is_err());

        // A birth record is version 1, signed by the owner it names, alone.
        let later = next_version(&room, &birth.policy, &owner, |_| {});
        assert!(verify_community_birth(&later, &SALT).is_err());
        let mut with_admin = RoomPolicy::initial(owner.actor_id(), None);
        with_admin.set_admins([kp(2).actor_id()]);
        assert!(
            verify_community_birth(&with_admin.sign_community(&room, &owner).unwrap(), &SALT)
                .is_err()
        );
        let by_another = RoomPolicy::initial(owner.actor_id(), None)
            .sign_community(&room, &kp(2))
            .unwrap();
        assert!(verify_community_birth(&by_another, &SALT).is_err());
    }

    #[test]
    fn a_community_chain_extends_only_as_the_signers_rank_in_the_version_before_allows() {
        let (owner, admin, member) = (kp(1), kp(2), kp(3));
        let (room, birth, mut chain) = founded(&owner);

        // v2: the owner appoints an admin.
        let v2 = next_version(&room, &birth.policy, &owner, |p| {
            p.set_admins([admin.actor_id()])
        });
        chain.extend(v2.clone(), &names_only).unwrap();
        assert_eq!(
            community_role_of(chain.policy_at(2).unwrap(), &admin.actor_id(), &names_only),
            RoomRole::Admin
        );
        // Judged by the version the act names: under v1 the admin held nothing.
        assert_eq!(
            community_role_of(chain.policy_at(1).unwrap(), &admin.actor_id(), &names_only),
            RoomRole::Member
        );

        // v3 offers, each refused and the chain left at v2: a member's; an
        // admin's that touches the admin set; one that skips a version; one
        // whose signature is not its signer's.
        let by_member = next_version(&room, &v2.policy, &member, |p| p.name = Some("mine".into()));
        assert!(chain.extend(by_member, &names_only).is_err());
        let admin_appoints = next_version(&room, &v2.policy, &admin, |p| {
            p.set_admins([admin.actor_id(), member.actor_id()])
        });
        assert!(chain.extend(admin_appoints, &names_only).is_err());
        let admin_takes_room = next_version(&room, &v2.policy, &admin, |p| {
            p.owner = admin.actor_id();
            p.admins.clear();
        });
        assert!(chain.extend(admin_takes_room, &names_only).is_err());
        let skipped = next_version(&room, &v2.policy, &owner, |p| p.version += 1);
        assert!(chain.extend(skipped, &names_only).is_err());
        let mut relabelled = next_version(&room, &v2.policy, &member, |_| {});
        relabelled.signer = owner.actor_id();
        assert!(chain.extend(relabelled, &names_only).is_err());
        assert_eq!(chain.head_version(), 2);

        // v3: the admin renames — the rest of the table is an admin's.
        let v3 = next_version(&room, &v2.policy, &admin, |p| p.name = Some("plaza".into()));
        chain.extend(v3.clone(), &names_only).unwrap();

        // v4: the owner hands the room on — in this class the outgoing owner's
        // own signed version — and stays on as an admin.
        let v4 = next_version(&room, &v3.policy, &owner, |p| {
            p.owner = member.actor_id();
            p.set_admins([owner.actor_id()]);
        });
        chain.extend(v4.clone(), &names_only).unwrap();
        // v5: the OLD owner's rank is now an admin's, judged under v4.
        let old_owner_reappoints = next_version(&room, &v4.policy, &owner, |p| {
            p.set_admins([admin.actor_id()])
        });
        assert!(chain.extend(old_owner_reappoints, &names_only).is_err());
        assert_eq!(chain.head_version(), 4);
    }

    /// Two rooms one owner founded — whose birth records differ only in the
    /// salt — each with the chain anchored on its own birth record.
    fn two_rooms(
        owner: &ActorKeypair,
        salt_b: [u8; 32],
        salt_c: [u8; 32],
    ) -> (
        [u8; 32],
        [u8; 32],
        SignedRoomPolicy,
        CommunityPolicyChain,
        CommunityPolicyChain,
    ) {
        let room_b = derive_room_id(&owner.actor_id(), &salt_b).unwrap();
        let room_c = derive_room_id(&owner.actor_id(), &salt_c).unwrap();
        let birth = RoomPolicy::initial(owner.actor_id(), None);
        let birth_b = birth.sign_community(&room_b, owner).unwrap();
        let chain_b = CommunityPolicyChain::anchor(&room_b, &salt_b, birth_b.clone()).unwrap();
        let chain_c = CommunityPolicyChain::anchor(
            &room_c,
            &salt_c,
            birth.sign_community(&room_c, owner).unwrap(),
        )
        .unwrap();
        (room_b, room_c, birth_b, chain_b, chain_c)
    }

    /// The splice a hostile home nest serves: a version the owner of two rooms
    /// signed in room C, offered as room B's next — which ranks a stranger to
    /// room B as its admin unless the version is bound to the room it was
    /// signed in.
    #[test]
    fn a_version_signed_in_one_room_does_not_extend_another_rooms_chain() {
        let (owner, mallory) = (kp(1), kp(8));
        let (salt_b, salt_c) = (
            binding_birth_salt(&[0x5a; 24]),
            binding_birth_salt(&[0x6b; 24]),
        );
        let (room_b, room_c, birth, mut chain_b, mut chain_c) = two_rooms(&owner, salt_b, salt_c);

        let c_v2 = next_version(&room_c, &birth.policy, &owner, |p| {
            p.set_admins([mallory.actor_id()])
        });
        chain_c.extend(c_v2.clone(), &names_only).unwrap();
        // As signed: its room signature is room C's.
        assert!(chain_b.clone().extend(c_v2.clone(), &names_only).is_err());
        // Stripped of it: every room requires one above version 1.
        let mut stripped = c_v2;
        stripped.room_signature = None;
        assert!(chain_b.clone().extend(stripped, &names_only).is_err());
        assert_eq!(chain_b.head_version(), 1);
        assert!(
            chain_b.policy_at(2).is_none(),
            "Mallory ranks nothing in room B"
        );

        // Room B's own version 2 extends it, and room C's birth record — whose
        // bytes derive room B's id under room B's salt — does not anchor it.
        let b_v2 = next_version(&room_b, &birth.policy, &owner, |p| {
            p.name = Some("b".into())
        });
        chain_b.extend(b_v2, &names_only).unwrap();
        let birth_c = RoomPolicy::initial(owner.actor_id(), None)
            .sign_community(&room_c, &owner)
            .unwrap();
        assert!(CommunityPolicyChain::anchor(&room_b, &salt_b, birth_c).is_err());
    }

    /// A birth salt without the binding mark is the lever a non-conforming
    /// founder would pull to mint a room whose id reads as one that predates
    /// the room signature: no such room exists, so none is founded — the birth
    /// record is refused whatever its signatures, and so is every chain it
    /// would anchor. And no room admits a version above 1 without a room
    /// signature, not even its own.
    #[test]
    fn a_salt_without_the_binding_mark_founds_no_room() {
        let owner = kp(1);
        let unmarked = [0x5a; 32];
        assert!(!is_binding_birth_salt(&unmarked));
        let room = derive_room_id(&owner.actor_id(), &unmarked).unwrap();
        let birth = RoomPolicy::initial(owner.actor_id(), None)
            .sign_community(&room, &owner)
            .unwrap();
        let refused = verify_community_birth(&birth, &unmarked).unwrap_err();
        assert!(
            refused.to_string().contains("binding mark"),
            "refused for the mark, not the signature: {refused}"
        );
        assert!(CommunityPolicyChain::anchor(&room, &unmarked, birth).is_err());

        let (room, birth, chain) = founded(&owner);
        let mut own_v2 = next_version(&room, &birth.policy, &owner, |p| p.name = Some("b".into()));
        own_v2.room_signature = None;
        assert!(chain.clone().extend(own_v2, &names_only).is_err());
        assert_eq!(chain.head_version(), 1);
    }

    #[test]
    fn a_binding_birth_salt_is_marked_and_keeps_its_entropy() {
        let salt = binding_birth_salt(&[0xa7; 24]);
        assert!(is_binding_birth_salt(&salt));
        assert_eq!(&salt[..8], &BINDING_SALT_MARK);
        assert_eq!(&salt[8..], &[0xa7; 24]);
        assert!(!is_binding_birth_salt(&[0u8; 32]));
    }

    #[test]
    fn a_successors_signature_extends_the_chain_only_for_a_judge_that_resolves_it() {
        let (owner, successor) = (kp(1), kp(4));
        let (room, birth, mut chain) = founded(&owner);
        // The owner's successor re-signs under its own name.
        let v2 = next_version(&room, &birth.policy, &successor, |p| {
            p.owner = successor.actor_id()
        });

        // A judge that resolves nothing fails closed …
        assert!(chain.clone().extend(v2.clone(), &names_only).is_err());
        // … and one that knows the succession admits it as the same seat.
        let (old, new) = (owner.actor_id(), successor.actor_id());
        let resolved = move |name: &ActorId| if *name == old { new } else { *name };
        chain.extend(v2, &resolved).unwrap();
        assert_eq!(chain.head_version(), 2);
    }

    /// A held line only ever grows: a re-answer for a root already held is
    /// taken only when it extends the held line — its newest holder succeeded
    /// in turn — and a shorter or forking answer changes nobody's seat.
    #[test]
    fn a_held_line_grows_only_by_extension_and_never_shrinks() {
        let (owner, second, third, rival) = (kp(1), kp(4), kp(5), kp(6));
        let mut lines = SuccessionLines::default();
        assert!(
            !lines.insert(owner.actor_id(), &[]),
            "an empty line designates nothing new"
        );
        assert!(lines.insert(owner.actor_id(), &[second.actor_id()]));
        assert!(
            !lines.insert(owner.actor_id(), &[second.actor_id()]),
            "the same line again is no news"
        );
        assert!(
            !lines.insert(owner.actor_id(), &[]),
            "a re-answer that now denies the succession leaves the line standing"
        );
        assert!(
            !lines.insert(owner.actor_id(), &[rival.actor_id()]),
            "a fork is not an extension"
        );
        assert!(!lines.designates(&owner.actor_id(), &rival.actor_id()));
        assert!(lines.designates(&owner.actor_id(), &second.actor_id()));
        assert_eq!(lines.seat(&owner.actor_id()), second.actor_id());

        // The newest holder succeeds in turn: the line extends.
        assert!(lines.insert(owner.actor_id(), &[second.actor_id(), third.actor_id()]));
        assert!(lines.designates(&owner.actor_id(), &third.actor_id()));
        assert!(lines.designates(&second.actor_id(), &third.actor_id()));
        assert!(
            !lines.designates(&third.actor_id(), &second.actor_id()),
            "never backward"
        );
        assert_eq!(lines.seat(&owner.actor_id()), third.actor_id());
        assert!(
            !lines.insert(owner.actor_id(), &[second.actor_id()]),
            "and never shrinks"
        );
        assert_eq!(lines.seat(&owner.actor_id()), third.actor_id());
    }

    /// The member side's line rule: a name designates itself and every
    /// verified successor AFTER it — so a chain re-proved long after the acts
    /// follows whichever seat on the line signed, and a version that names the
    /// successor retires the predecessor's key.
    #[test]
    fn a_name_designates_its_verified_line_forward_and_never_backward() {
        let (owner, second, third, admin, stranger) = (kp(1), kp(4), kp(5), kp(2), kp(9));
        let (room, birth, mut chain) = founded(&owner);
        let mut lines = SuccessionLines::default();
        lines.insert(owner.actor_id(), &[second.actor_id(), third.actor_id()]);

        // v2 by the INTERMEDIATE successor, since succeeded again, the policy
        // still naming the original owner: a terminal-only rule would strand
        // the room here.
        let v2 = next_version(&room, &birth.policy, &second, |p| {
            p.set_admins([admin.actor_id()])
        });
        assert!(chain.clone().extend(v2.clone(), &names_only).is_err());
        assert!(
            chain
                .clone()
                .extend(v2.clone(), &SuccessionLines::default())
                .is_err()
        );
        chain.extend(v2.clone(), &lines).unwrap();
        // v3 by the predecessor while the policy still names it: on the line
        // (the floor's newest-live-seat rule is what gates a retired key).
        let v3 = next_version(&room, &v2.policy, &owner, |p| p.name = Some("plaza".into()));
        chain.extend(v3.clone(), &lines).unwrap();
        // Nobody off the line, however the lines read.
        let by_stranger = next_version(&room, &v3.policy, &stranger, |_| {});
        assert!(chain.clone().extend(by_stranger, &lines).is_err());

        // v4: the terminal successor re-signs under its OWN name …
        let v4 = next_version(&room, &v3.policy, &third, |p| p.owner = third.actor_id());
        chain.extend(v4.clone(), &lines).unwrap();
        // … and from then on both retired keys are strangers: a name never
        // designates an identity before it.
        for retired in [&owner, &second] {
            let late = next_version(&room, &v4.policy, retired, |_| {});
            assert!(chain.clone().extend(late, &lines).is_err());
            assert_eq!(
                community_role_of(&v4.policy, &retired.actor_id(), &lines),
                RoomRole::Member
            );
        }
        assert_eq!(
            community_role_of(&v3.policy, &third.actor_id(), &lines),
            RoomRole::Owner
        );

        // An admin carrying the owner across as its successor's name changed
        // nobody's seat; naming anyone off the line did.
        let renamed = next_version(&room, &v3.policy, &admin, |p| p.owner = second.actor_id());
        let binding = RoomBinding::new(room);
        assert!(judge_community_policy_step(&v3.policy, &renamed, &binding, &lines).is_ok());
        let seized = next_version(&room, &v3.policy, &admin, |p| p.owner = stranger.actor_id());
        assert!(judge_community_policy_step(&v3.policy, &seized, &binding, &lines).is_err());
    }

    fn floor_delete(author: &ActorKeypair, room: [u8; 32]) -> RoomFloorDelete {
        RoomFloorDelete {
            room: room.to_vec(),
            target_seq: 7,
            author: author.actor_id(),
            policy_version: 3,
        }
    }

    #[test]
    fn a_floor_delete_record_round_trips_and_verifies_for_its_room_only() {
        let admin = kp(1);
        let room = [9u8; 32];
        let signed = floor_delete(&admin, room).sign(&admin).unwrap();
        let back = SignedRoomFloorDelete::from_bytes(&signed.to_bytes().unwrap()).unwrap();
        assert_eq!(back, signed);
        back.verify(&room)
            .expect("the author's own record verifies");
        assert!(
            back.verify(&[8u8; 32]).is_err(),
            "a record lifted into another room does not verify there"
        );
    }

    #[test]
    fn a_floor_delete_record_with_any_field_changed_does_not_verify() {
        let admin = kp(1);
        let room = [9u8; 32];
        let signed = floor_delete(&admin, room).sign(&admin).unwrap();
        let mut target = signed.clone();
        target.record.target_seq += 1;
        let mut version = signed.clone();
        version.record.policy_version += 1;
        let mut author = signed.clone();
        author.record.author = kp(2).actor_id();
        for (what, forged) in [("target", target), ("version", version), ("author", author)] {
            assert!(
                forged.verify(&room).is_err(),
                "a changed {what} must not verify"
            );
        }
    }

    #[test]
    fn a_floor_delete_record_is_never_signed_under_another_principals_name() {
        let (admin, other) = (kp(1), kp(2));
        assert!(floor_delete(&admin, [9u8; 32]).sign(&other).is_err());
    }

    fn room(owner: &ActorKeypair, admins: &[ActorId]) -> RoomPolicyExtension {
        let mut policy = RoomPolicy::initial(owner.actor_id(), Some("room".into()));
        policy.set_admins(admins.iter().copied());
        RoomPolicyExtension::new(policy.sign(owner).unwrap())
    }

    const CHANNEL: [u8; 32] = [9u8; 32];

    fn facts(committer: &ActorKeypair) -> StagedCommitFacts {
        StagedCommitFacts {
            channel: CHANNEL,
            committer: committer.actor_id(),
            adds: Vec::new(),
            removes: Vec::new(),
            next_extension: None,
            other_proposals: false,
        }
    }

    #[test]
    fn a_signed_policy_round_trips_and_a_tampered_one_does_not_verify() {
        let owner = kp(1);
        let ext = room(&owner, &[]);
        let bytes = ext.to_bytes().unwrap();
        let back = RoomPolicyExtension::from_bytes(&bytes).unwrap();
        assert_eq!(back, ext);

        let mut tampered = ext.clone();
        tampered.signed.policy.name = Some("stolen".into());
        assert!(
            tampered.validate().is_err(),
            "a changed field breaks the signature"
        );
        let bytes = canonical(&tampered).unwrap();
        assert!(RoomPolicyExtension::from_bytes(&bytes).is_err());
    }

    #[test]
    fn the_request_join_rule_is_refused_on_an_end_to_end_policy() {
        let owner = kp(1);
        let mut policy = RoomPolicy::initial(owner.actor_id(), None);
        policy.join_rule = JoinRule::Request;
        assert!(policy.sign(&owner).is_err());
    }

    /// The one rule that differs between the classes, from both sides: the
    /// `request` join rule is malformed in an MLS group context and well
    /// formed on a community room's floor. The end-to-end refusal above is
    /// the other half of this pair — together they pin that the split
    /// loosened nothing.
    #[test]
    fn the_request_join_rule_is_admissible_on_a_community_policy() {
        let owner = kp(1);
        let mut policy = RoomPolicy::initial(owner.actor_id(), None);
        policy.join_rule = JoinRule::Request;
        policy
            .validate_community()
            .expect("a community room has a reader who can vouch for a stranger");

        // The signature path follows the same split. `sign` runs the
        // end-to-end validation, so a community policy is signed over its
        // canonical bytes directly — the shape the nest's create ceremony
        // receives off the wire.
        let canonical_bytes = canonical(&policy).unwrap();
        let signature = owner
            .signing_key()
            .sign(&signing_input(TAG_ROOM_POLICY, &canonical_bytes))
            .to_bytes()
            .to_vec();
        let signed = SignedRoomPolicy {
            policy,
            signer: owner.actor_id(),
            signature,
            countersignature: None,
            room_signature: None,
        };
        signed
            .verify_signature_community()
            .expect("the community verifier admits it");
        assert!(
            signed.verify_signature().is_err(),
            "and the end-to-end verifier still refuses exactly these bytes"
        );
    }

    /// A structural defect is refused in **both** classes — the split moved
    /// one class rule out of `validate`, not the shape checks.
    #[test]
    fn a_structural_defect_is_refused_in_both_classes() {
        let owner = kp(1);
        for bad in [
            {
                // The owner listed in its own admin set.
                let mut p = RoomPolicy::initial(owner.actor_id(), None);
                p.admins = vec![owner.actor_id()];
                p
            },
            {
                // An unsorted admin set — the signed bytes must be canonical.
                let mut p = RoomPolicy::initial(owner.actor_id(), None);
                let (a, b) = (kp(7).actor_id(), kp(8).actor_id());
                let (hi, lo) = if a.0 > b.0 { (a, b) } else { (b, a) };
                p.admins = vec![hi, lo];
                p
            },
            {
                // Version 0 — the ratchet starts at 1.
                let mut p = RoomPolicy::initial(owner.actor_id(), None);
                p.version = 0;
                p
            },
        ] {
            assert!(bad.validate().is_err(), "end-to-end refuses {bad:?}");
            assert!(
                bad.validate_community().is_err(),
                "and so does community: {bad:?}"
            );
        }
    }

    /// The birth record's id commits to **both** halves — whose key founded
    /// the room and which salt it used — so a room id cannot be claimed by a
    /// principal that did not derive it, and one principal can found many
    /// rooms.
    #[test]
    fn a_room_id_commits_to_its_founder_and_its_salt() {
        let founder = kp(1);
        let other = kp(2);
        let id = derive_room_id(&founder.actor_id(), &[3u8; 32]).unwrap();

        assert_eq!(
            id,
            derive_room_id(&founder.actor_id(), &[3u8; 32]).unwrap(),
            "the derivation is a function of the birth record alone"
        );
        assert_ne!(
            id,
            derive_room_id(&other.actor_id(), &[3u8; 32]).unwrap(),
            "a different founder is a different room"
        );
        assert_ne!(
            id,
            derive_room_id(&founder.actor_id(), &[4u8; 32]).unwrap(),
            "and one founder's two salts are two rooms"
        );
    }

    #[test]
    fn roles_resolve_from_the_policy_and_through_successions() {
        let owner = kp(1);
        let admin = kp(2);
        let member = kp(3);
        let ext = room(&owner, &[admin.actor_id()]);
        assert_eq!(ext.role_of(&owner.actor_id()), RoomRole::Owner);
        assert_eq!(ext.role_of(&admin.actor_id()), RoomRole::Admin);
        assert_eq!(ext.role_of(&member.actor_id()), RoomRole::Member);

        let owner2 = kp(11);
        let admin2 = kp(12);
        let ext = ext
            .with_succession(RecordedSuccession {
                old: owner.actor_id(),
                new: owner2.actor_id(),
            })
            .unwrap()
            .with_succession(RecordedSuccession {
                old: admin.actor_id(),
                new: admin2.actor_id(),
            })
            .unwrap();
        assert_eq!(ext.role_of(&owner2.actor_id()), RoomRole::Owner);
        assert_eq!(ext.role_of(&admin2.actor_id()), RoomRole::Admin);
        // The succeeded identities resolve AWAY from their roles.
        assert_eq!(ext.role_of(&owner.actor_id()), RoomRole::Member);
        assert_eq!(ext.role_of(&admin.actor_id()), RoomRole::Member);
        assert_eq!(ext.effective_owner(), owner2.actor_id());
    }

    #[test]
    fn a_succession_chain_runs_forward_only() {
        let owner = kp(1);
        let a = kp(2).actor_id();
        let b = kp(3).actor_id();
        let ext = room(&owner, &[]);
        let ext = ext
            .with_succession(RecordedSuccession { old: a, new: b })
            .unwrap();
        assert!(
            ext.with_succession(RecordedSuccession {
                old: a,
                new: kp(4).actor_id()
            })
            .is_err(),
            "an identity is succeeded at most once"
        );
        assert!(
            ext.with_succession(RecordedSuccession { old: b, new: a })
                .is_err(),
            "no pointing back at a succeeded identity"
        );
        assert!(
            ext.with_succession(RecordedSuccession { old: b, new: b })
                .is_err(),
            "no self-succession"
        );
    }

    /// `resolve` answers only the terminal identity; callers deciding whether
    /// a chain is still LIVE (some real successor currently seated, not
    /// necessarily the terminal one — `ConversationsManager::apply_inbound_roster`'s
    /// drop arm) need every intermediate hop too.
    #[test]
    fn successor_chain_walks_every_hop_not_just_the_terminal() {
        let owner = kp(1);
        let a = kp(2).actor_id();
        let b = kp(3).actor_id();
        let c = kp(4).actor_id();
        let ext = room(&owner, &[])
            .with_succession(RecordedSuccession { old: a, new: b })
            .unwrap()
            .with_succession(RecordedSuccession { old: b, new: c })
            .unwrap();

        assert_eq!(ext.successor_chain(a), vec![b, c], "every hop, in order");
        assert_eq!(ext.successor_chain(b), vec![c]);
        assert_eq!(
            ext.successor_chain(c),
            Vec::<ActorId>::new(),
            "c was never succeeded"
        );
        assert_eq!(ext.resolve(a), c, "resolve still answers only the terminal");
    }

    #[test]
    fn a_plain_member_may_neither_add_under_invite_nor_remove() {
        let owner = kp(1);
        let member = kp(3);
        let other = kp(4);
        let ext = room(&owner, &[]);

        let mut f = facts(&member);
        f.adds = vec![kp(5).actor_id()];
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        let mut f = facts(&member);
        f.removes = vec![other.actor_id()];
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        // A bare self-update (no proposals) is every member's right.
        assert!(judge_commit(&ext, &facts(&member)).is_permit());
    }

    #[test]
    fn member_invite_lets_a_member_add_but_still_not_remove() {
        let owner = kp(1);
        let member = kp(3);
        let mut policy = RoomPolicy::initial(owner.actor_id(), None);
        policy.join_rule = JoinRule::MemberInvite;
        let ext = RoomPolicyExtension::new(policy.sign(&owner).unwrap());

        let mut f = facts(&member);
        f.adds = vec![kp(5).actor_id()];
        assert!(judge_commit(&ext, &f).is_permit());

        let mut f = facts(&member);
        f.removes = vec![kp(4).actor_id()];
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));
    }

    #[test]
    fn an_admin_adds_and_removes_members_but_never_the_owner() {
        let owner = kp(1);
        let admin = kp(2);
        let member = kp(3);
        let ext = room(&owner, &[admin.actor_id()]);

        let mut f = facts(&admin);
        f.adds = vec![kp(5).actor_id()];
        f.removes = vec![member.actor_id()];
        assert!(judge_commit(&ext, &f).is_permit());

        let mut f = facts(&admin);
        f.removes = vec![owner.actor_id()];
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        // Nor the owner itself, by anyone.
        let mut f = facts(&owner);
        f.removes = vec![owner.actor_id()];
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));
    }

    #[test]
    fn policy_changes_follow_the_signers_role_and_the_version_ratchet() {
        let owner = kp(1);
        let admin = kp(2);
        let member = kp(3);
        let ext = room(&owner, &[admin.actor_id()]);

        // An admin renames — the fields the table lets admins set.
        let mut renamed = ext.signed.policy.clone();
        renamed.version += 1;
        renamed.name = Some("renamed".into());
        let next = RoomPolicyExtension::new(renamed.clone().sign(&admin).unwrap());
        let mut f = facts(&admin);
        f.next_extension = Some(Ok(next));
        assert!(judge_commit(&ext, &f).is_permit());

        // …but an admin may not touch the admin set.
        let mut widened = ext.signed.policy.clone();
        widened.version += 1;
        widened.set_admins([admin.actor_id(), member.actor_id()]);
        let next = RoomPolicyExtension::new(widened.sign(&admin).unwrap());
        let mut f = facts(&admin);
        f.next_extension = Some(Ok(next));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        // The owner may.
        let mut widened = ext.signed.policy.clone();
        widened.version += 1;
        widened.set_admins([admin.actor_id(), member.actor_id()]);
        let next = RoomPolicyExtension::new(widened.sign(&owner).unwrap());
        let mut f = facts(&owner);
        f.next_extension = Some(Ok(next));
        assert!(judge_commit(&ext, &f).is_permit());

        // A member may change nothing.
        let mut renamed = ext.signed.policy.clone();
        renamed.version += 1;
        renamed.name = Some("mine now".into());
        let next = RoomPolicyExtension::new(renamed.sign(&member).unwrap());
        let mut f = facts(&member);
        f.next_extension = Some(Ok(next));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        // The version must advance by exactly one.
        let mut skipped = ext.signed.policy.clone();
        skipped.version += 2;
        let next = RoomPolicyExtension::new(skipped.sign(&owner).unwrap());
        let mut f = facts(&owner);
        f.next_extension = Some(Ok(next));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        // A policy is committed by its own signer.
        let mut renamed = ext.signed.policy.clone();
        renamed.version += 1;
        renamed.name = Some("x".into());
        let next = RoomPolicyExtension::new(renamed.sign(&owner).unwrap());
        let mut f = facts(&admin);
        f.next_extension = Some(Ok(next));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));
    }

    #[test]
    fn a_succession_is_recorded_by_its_old_leaf_and_admits_the_pair() {
        let owner = kp(1);
        let member = kp(3);
        let member2 = kp(13);
        let ext = room(&owner, &[]);

        // 1. The old leaf records its succession (a group-context change).
        let recorded = ext
            .with_succession(RecordedSuccession {
                old: member.actor_id(),
                new: member2.actor_id(),
            })
            .unwrap();
        let mut f = facts(&member);
        f.next_extension = Some(Ok(recorded.clone()));
        assert!(judge_commit(&ext, &f).is_permit());

        // A stranger to the record may not record it for them…
        let mut f = facts(&kp(4));
        f.next_extension = Some(Ok(recorded.clone()));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));
        // …but the owner may vouch for it (the member-side re-add remedy).
        let mut f = facts(&owner);
        f.next_extension = Some(Ok(recorded.clone()));
        assert!(judge_commit(&ext, &f).is_permit());

        // 2. add-successor by the old leaf, under `invite`.
        let mut f = facts(&member);
        f.adds = vec![member2.actor_id()];
        assert!(judge_commit(&recorded, &f).is_permit());
        // Only its own successor.
        let mut f = facts(&member);
        f.adds = vec![kp(5).actor_id()];
        assert!(matches!(
            judge_commit(&recorded, &f),
            CommitVerdict::Refuse(_)
        ));

        // 3. remove-old by the new leaf.
        let mut f = facts(&member2);
        f.removes = vec![member.actor_id()];
        assert!(judge_commit(&recorded, &f).is_permit());
        // And nobody else's leaf.
        let mut f = facts(&member2);
        f.removes = vec![kp(4).actor_id()];
        assert!(matches!(
            judge_commit(&recorded, &f),
            CommitVerdict::Refuse(_)
        ));
    }

    /// The vouch arm asks *whose* succession is being vouched for, not only
    /// who is vouching: a voucher records a
    /// succession only for a principal it strictly outranks. An admin that
    /// could vouch for the **owner** would plant a successor it holds and
    /// inherit the room through `role_of`'s chain resolution — crossing the
    /// owner-only lines of `conversation-rooms.md` § Roles and authorization
    /// (appointing admins, transferring ownership, the owner's unremovable
    /// membership) with a commit every honest member permits.
    ///
    /// Red-verified 2026-09-14: forcing `outranks` to
    /// always return `true`, or always `false`, or forcing `self_record` to
    /// always be `false`, each reddens this test plus
    /// `a_succession_is_recorded_by_its_old_leaf_and_admits_the_pair` — every
    /// other unit pin in this module stays green under all three. The three
    /// mutations separate cleanly on the group-level pins in
    /// `room_policy_commits.rs` instead (see the red-verify note there):
    /// forcing `outranks` either way reddens the admin-for-owner group pins,
    /// forcing `self_record` false reddens the old-leaf-succession group pins,
    /// disjointly.
    #[test]
    fn a_voucher_records_a_succession_only_for_a_principal_it_outranks() {
        let owner = kp(1);
        let admin = kp(2);
        let other_admin = kp(4);
        let member = kp(3);
        let ext = room(&owner, &[admin.actor_id(), other_admin.actor_id()]);

        // An admin planting a successor for the owner: refused.
        let planted_owner = ext
            .with_succession(RecordedSuccession {
                old: owner.actor_id(),
                new: kp(21).actor_id(),
            })
            .unwrap();
        let mut f = facts(&admin);
        f.next_extension = Some(Ok(planted_owner.clone()));
        assert!(
            matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)),
            "an admin may not record the owner's succession"
        );
        // Nor for another admin — equal rank is not outranking.
        let planted_admin = ext
            .with_succession(RecordedSuccession {
                old: other_admin.actor_id(),
                new: kp(22).actor_id(),
            })
            .unwrap();
        let mut f = facts(&admin);
        f.next_extension = Some(Ok(planted_admin.clone()));
        assert!(
            matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)),
            "an admin may not record another admin's succession"
        );
        // The owner outranks an admin and may vouch for one …
        let mut f = facts(&owner);
        f.next_extension = Some(Ok(planted_admin));
        assert!(judge_commit(&ext, &f).is_permit());
        // … an admin outranks a plain member (the re-add remedy) …
        let planted_member = ext
            .with_succession(RecordedSuccession {
                old: member.actor_id(),
                new: kp(23).actor_id(),
            })
            .unwrap();
        let mut f = facts(&admin);
        f.next_extension = Some(Ok(planted_member));
        assert!(judge_commit(&ext, &f).is_permit());
        // … and only the owner's own leaf records the owner's succession.
        let mut f = facts(&owner);
        f.next_extension = Some(Ok(planted_owner));
        assert!(judge_commit(&ext, &f).is_permit());
    }

    #[test]
    fn an_owners_successor_holds_the_owner_role_and_may_re_sign() {
        let owner = kp(1);
        let owner2 = kp(11);
        let member = kp(3);
        let ext = room(&owner, &[]);
        let recorded = ext
            .with_succession(RecordedSuccession {
                old: owner.actor_id(),
                new: owner2.actor_id(),
            })
            .unwrap();
        assert_eq!(recorded.role_of(&owner2.actor_id()), RoomRole::Owner);

        // The successor removes its predecessor (the old owner, resolved away
        // from the owner role) …
        let mut f = facts(&owner2);
        f.removes = vec![owner.actor_id()];
        assert!(judge_commit(&recorded, &f).is_permit());

        // … and re-points the owner field onto itself in a policy it signs.
        let mut rewritten = recorded.signed.policy.clone();
        rewritten.version += 1;
        rewritten.owner = owner2.actor_id();
        let mut next = recorded.clone();
        next.signed = rewritten.sign(&owner2).unwrap();
        let mut f = facts(&owner2);
        f.next_extension = Some(Ok(next.clone()));
        assert!(judge_commit(&recorded, &f).is_permit());
        assert_eq!(next.role_of(&owner2.actor_id()), RoomRole::Owner);

        // But the owner field cannot jump to an arbitrary identity.
        let mut hijack = recorded.signed.policy.clone();
        hijack.version += 1;
        hijack.owner = member.actor_id();
        let mut next = recorded.clone();
        next.signed = hijack.sign(&owner2).unwrap();
        let mut f = facts(&owner2);
        f.next_extension = Some(Ok(next));
        assert!(matches!(
            judge_commit(&recorded, &f),
            CommitVerdict::Refuse(_)
        ));
    }

    /// The hand-over needs both keys, bound to the room: the incoming owner
    /// signs and commits, the outgoing owner countersigns for this channel.
    /// Missing, mis-signed or other-room countersignatures are refused, as
    /// is a transfer the outgoing owner tries to commit itself; the landed
    /// policy resolves the new owner to `Owner` and the old one to `Member`.
    #[test]
    fn an_ownership_transfer_needs_both_signatures_bound_to_the_room() {
        let owner = kp(1);
        let admin = kp(2);
        let heir = kp(3);
        // The heir is an admin today; the transfer takes it out of the set.
        let ext = room(&owner, &[admin.actor_id(), heir.actor_id()]);

        let mut policy = ext.signed.policy.clone();
        policy.version += 1;
        policy.owner = heir.actor_id();
        let admins = policy.admins.clone();
        policy.set_admins(admins);
        assert_eq!(policy.admins, vec![admin.actor_id()]);
        let countersignature = policy.countersign_transfer(&CHANNEL, &owner).unwrap();

        let transfer = |signer: &ActorKeypair, cs: Option<OwnershipCountersignature>| {
            let mut signed = policy.sign(signer).unwrap();
            signed.countersignature = cs;
            RoomPolicyExtension {
                signed,
                successions: Vec::new(),
            }
        };
        let judged = |committer: &ActorKeypair, next: RoomPolicyExtension| {
            let mut f = facts(committer);
            f.next_extension = Some(Ok(next));
            judge_commit(&ext, &f)
        };

        // The whole ceremony: permitted, and the roles follow.
        let next = transfer(&heir, Some(countersignature.clone()));
        assert!(judged(&heir, next.clone()).is_permit());
        assert_eq!(next.role_of(&heir.actor_id()), RoomRole::Owner);
        assert_eq!(next.role_of(&owner.actor_id()), RoomRole::Member);
        assert_eq!(next.role_of(&admin.actor_id()), RoomRole::Admin);

        // No countersignature: refused.
        assert!(matches!(
            judged(&heir, transfer(&heir, None)),
            CommitVerdict::Refuse(_)
        ));
        // Countersigned by an admin rather than the owner: refused.
        let by_admin = policy.countersign_transfer(&CHANNEL, &admin).unwrap();
        assert!(matches!(
            judged(&heir, transfer(&heir, Some(by_admin))),
            CommitVerdict::Refuse(_)
        ));
        // Countersigned for another room: refused here.
        let other_room = policy.countersign_transfer(&[8u8; 32], &owner).unwrap();
        assert!(matches!(
            judged(&heir, transfer(&heir, Some(other_room))),
            CommitVerdict::Refuse(_)
        ));
        // The outgoing owner cannot commit the transfer itself, countersigned
        // or not — the incoming owner's key must consent.
        assert!(matches!(
            judged(&owner, transfer(&owner, Some(countersignature.clone()))),
            CommitVerdict::Refuse(_)
        ));
        // A third party cannot commit it either, even carrying both keys'
        // work: the signer must be the incoming owner.
        assert!(matches!(
            judged(&admin, transfer(&admin, Some(countersignature.clone()))),
            CommitVerdict::Refuse(_)
        ));

        // The offer's own check, as the incoming device asks it.
        let offer = RoomOwnershipOffer {
            policy: policy.clone(),
            countersignature,
        };
        let offer = RoomOwnershipOffer::from_bytes(&offer.to_bytes().unwrap()).unwrap();
        offer
            .verify_against(&ext, &CHANNEL, &heir.actor_id())
            .expect("addressed to the heir, next version, the owner's countersignature");
        assert!(
            offer
                .verify_against(&ext, &CHANNEL, &admin.actor_id())
                .is_err(),
            "not addressed to the admin"
        );
        assert!(
            offer
                .verify_against(&ext, &[8u8; 32], &heir.actor_id())
                .is_err(),
            "bound to the room"
        );
        assert!(
            offer
                .verify_against(&next, &CHANNEL, &heir.actor_id())
                .is_err(),
            "stale once the room moved on"
        );
    }

    #[test]
    fn dropping_or_truncating_the_record_is_refused() {
        let owner = kp(1);
        let ext = room(&owner, &[])
            .with_succession(RecordedSuccession {
                old: kp(3).actor_id(),
                new: kp(13).actor_id(),
            })
            .unwrap();
        let mut truncated = ext.clone();
        truncated.successions.clear();
        let mut f = facts(&owner);
        f.next_extension = Some(Ok(truncated));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        let mut f = facts(&owner);
        f.next_extension = Some(Err("gone".into()));
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));

        let mut f = facts(&owner);
        f.other_proposals = true;
        assert!(matches!(judge_commit(&ext, &f), CommitVerdict::Refuse(_)));
    }

    // ── the labeler set ────────────────────────────────────────────

    const ROOM: [u8; 32] = [0x44u8; 32];

    #[test]
    fn a_labeler_set_round_trips_and_a_tampered_one_does_not_verify() {
        let admin = kp(2);
        let set = RoomLabelers::new(ROOM, 1, [kp(20).actor_id(), kp(21).actor_id()]);
        let signed = set.sign(&admin).unwrap();
        signed.verify_signature().unwrap();

        let bytes = canonical(&signed).unwrap();
        let back: SignedRoomLabelers = fauna_cbor::decode_strict(&bytes).unwrap();
        assert_eq!(back, signed);
        back.verify_signature().unwrap();

        let mut tampered = signed.clone();
        tampered.labelers.labelers.pop();
        assert!(
            tampered.verify_signature().is_err(),
            "dropping a labeler is a change the signature does not cover"
        );
        let mut resigned_by_another = signed;
        resigned_by_another.signer = kp(3).actor_id();
        assert!(
            resigned_by_another.verify_signature().is_err(),
            "the signature is the named signer's or nobody's"
        );
    }

    #[test]
    fn a_labeler_set_is_bound_to_its_room() {
        // An admin of two rooms must not be able to lift the set it signed
        // for one onto the other: the room is inside the signed bytes.
        let admin = kp(2);
        let mut signed = RoomLabelers::new(ROOM, 1, [kp(20).actor_id()])
            .sign(&admin)
            .unwrap();
        signed.labelers.room_id = vec![0x45u8; 32];
        assert!(signed.verify_signature().is_err());
    }

    #[test]
    fn a_labeler_set_is_sorted_deduplicated_bounded_and_versioned_from_one() {
        let admin = kp(2);
        let a = kp(20).actor_id();
        let b = kp(21).actor_id();

        // The constructor canonicalizes, so an honest author never trips it.
        let set = RoomLabelers::new(ROOM, 1, [b, a, b]);
        let mut expected = vec![a, b];
        expected.sort_unstable_by_key(|id| id.0);
        assert_eq!(set.labelers, expected);

        let mut unsorted = set.clone();
        unsorted.labelers.reverse();
        assert!(
            unsorted.validate().is_err(),
            "unsorted bytes are not canonical"
        );
        assert!(unsorted.sign(&admin).is_err());

        let mut duplicated = set.clone();
        duplicated
            .labelers
            .push(*duplicated.labelers.last().unwrap());
        assert!(duplicated.validate().is_err());

        let over: Vec<ActorId> = (0..=MAX_ROOM_LABELERS as u8)
            .map(|i| kp(100 + i).actor_id())
            .collect();
        assert!(
            RoomLabelers::new(ROOM, 1, over).validate().is_err(),
            "every named labeler runs on every send, so the set is bounded"
        );

        assert!(
            RoomLabelers::new(ROOM, 0, [a]).validate().is_err(),
            "version 0 is the room with no set at all, never a set"
        );
        let mut short_room = RoomLabelers::new(ROOM, 1, [a]);
        short_room.room_id.truncate(31);
        assert!(short_room.validate().is_err(), "a room id is 32 bytes");

        // Empty is a real set: it is how an admin stops the nest labelling.
        RoomLabelers::new(ROOM, 2, [])
            .sign(&admin)
            .unwrap()
            .verify_signature()
            .unwrap();
    }
}
