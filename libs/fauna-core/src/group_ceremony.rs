//! The offline share-initiation ceremony's carrier-agnostic payloads —
//! offer / accept / deliver (first half).
//!
//! Authority: `docs/goal/behavior/p2p.md` § Offline share initiation
//! (contract point 1: the ceremony is two-party and co-present, and ONE
//! exchange delivers mutual actor-key verification, the scope offer +
//! accept, the recipient's wrap, and the initial machinery handoff);
//! mechanics owner: `docs/goal/architecture/account-data-plane.md` § The
//! audience ladder → *The recipient-set scheme*.
//!
//! The [`crate::custody_ceremony`] idiom, transplanted — same
//! signed-`EmbedAsBytes` steps, same **sender binding** (a ceremony step IS
//! its author's act: each verifier requires the signer to BE the
//! transport-proven channel sender, and the offer's addressee to BE the
//! reading actor — a forwarded or replayed payload conveys nothing), same
//! accept→offer digest binding — at one deliberate transport difference:
//! custody steps ride an established nest-carried conversation channel,
//! while these ride the **contact-plane peer channel** (PT-1b actor-key
//! dial; the whole point is that no nest is reachable). The payloads are
//! carrier-agnostic on purpose: the transport proves the sender's actor key
//! and hands these functions the result.
//!
//! Three payloads, three binding rules beyond the shared ones:
//!
//! * [`GroupShareOffer`] — signed by the **initiator** (v1: the authority
//!   account). Carries the birth record VERBATIM, and verification
//!   recomputes the scope id from it and requires the birth's authority to
//!   be the initiator — nobody can offer a scope whose name they do not
//!   hold.
//! * [`GroupShareAccept`] — signed by the **recipient**; binds to one exact
//!   offer by digest and carries the recipient's actor-signed published
//!   reception half VERBATIM
//!   ([`crate::group_generation::GroupReceptionPublished`]) — the wrap
//!   target the initiator mints the roster entry and admission bundle for.
//! * [`GroupShareDeliver`] — signed by the **initiator**; carries the
//!   joiner's roster-cell id, the admission wrap
//!   (`fauna_mls::wrapped_blob::group_generation_wraps` — machinery root +
//!   retained generation bundle, sealed to the recipient's reception key),
//!   and the initial **machinery snapshot**: verbatim group-plane rows
//!   (birth, roster, mints, top-ups) the joiner adopts through the ordinary
//!   `apply_class2` first-contact strictness — nothing here invents a
//!   second sync form. Everything after the snapshot rides the ordinary
//!   peer-plane sync the admission witness (the joiner's own `Enrolled`
//!   entry, present in the snapshot) unlocks.
//!
//! The ceremony *state machine* (record-then-act, expiry decay, the drive
//! arm) and the tui affordance are the slice's second half, deliberately not
//! here — this module owns payload shapes and verification only, exactly as
//! the custody sibling does.

use serde::{Deserialize, Serialize};

use crate::data::Timestamp;
use crate::encoding::{EmbedAsBytes, Signed, decode_signed_bytes, sign_envelope, verify_envelope};
use crate::error::{Error, Result};
use crate::group_scope::{GroupBirthRecord, decode_birth_for_scope};
use crate::identity::{ActorId, ActorKeypair};

/// The initiator's signed scope offer (ceremony step 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupShareOffer {
    /// The offered scope's content-derived id — verification recomputes it
    /// from `birth` and refuses a mismatch, so the id is never taken on
    /// trust.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The offering account — the signer, and (v1 authority seam) the
    /// birth record's authority actor.
    pub initiator: ActorId,
    /// The addressee: the account being offered membership. A reader whose
    /// own actor is not `recipient` refuses the payload.
    pub recipient: ActorId,
    /// The scope's [`GroupBirthRecord`], canonical bytes VERBATIM — the
    /// joiner's commitment-check anchor
    /// ([`crate::group_scope::verify_machinery_root_commitment`]).
    #[serde(with = "serde_bytes")]
    pub birth: Vec<u8>,
    pub offered_at: Timestamp,
}

impl Signed for GroupShareOffer {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.initiator.0
    }
}

/// The recipient's signed acceptance (ceremony step 2) — answers with the
/// wrap target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupShareAccept {
    /// The offer's scope id, echoed.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// BLAKE3 of the offer's canonical envelope bytes
    /// ([`group_offer_digest`]) — binds this accept to one exact offer.
    #[serde(with = "serde_bytes")]
    pub offer_digest: [u8; 32],
    /// The accepting account — the signer of this payload.
    pub recipient: ActorId,
    /// The recipient's actor-signed published reception half
    /// ([`crate::group_generation::GroupReceptionPublished`] in its
    /// `EmbedAsBytes` carriage), VERBATIM — verification opens it and
    /// requires its publisher to be `recipient`, so an accept can never
    /// smuggle a wrap target the recipient's own key did not sign.
    #[serde(with = "serde_bytes")]
    pub reception_published: Vec<u8>,
    pub accepted_at: Timestamp,
}

impl Signed for GroupShareAccept {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.recipient.0
    }
}

/// One verbatim group-plane row in the deliver's machinery snapshot. The
/// joiner adopts each through the ordinary
/// `fauna_protocol::merge_policy::apply_class2` first-contact path (kind
/// registry: `fauna_protocol::group_state`), so a forged snapshot row is
/// refused by exactly the strictness every synced row faces — this carriage
/// adds no trust of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPlaneRow {
    /// The class-2 kind string (`fauna.group.*`).
    pub kind: String,
    /// The row's logical key.
    pub key: String,
    /// The canonical value bytes, verbatim.
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
}

/// The initiator's signed admission delivery (ceremony step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupShareDeliver {
    /// The ceremony's scope id, echoed.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The delivering account — the signer of this payload.
    pub initiator: ActorId,
    /// The joiner's roster cell — the content-derived entry id its
    /// `Enrolled` row (in the snapshot) sits at, and the slot its admission
    /// wrap is bound to. The joiner recomputes it from the snapshot row's
    /// own core rather than trusting this copy; carrying it names the slot
    /// to look at.
    #[serde(with = "serde_bytes")]
    pub roster_entry_id: [u8; 32],
    /// The admission wrap: machinery root + retained generation bundle,
    /// X-Wing-sealed to the recipient's reception key at
    /// `(scope_id, roster_entry_id)`
    /// (`fauna_mls::wrapped_blob::group_generation_wraps::seal_group_admission_bundle`).
    #[serde(with = "serde_bytes")]
    pub admission_wrap: Vec<u8>,
    /// The initial machinery handoff: the scope's birth, roster, mint, and
    /// top-up rows, verbatim — enough for the joiner to stand the scope up
    /// with no follow-up round (the co-present exchange may be the only
    /// connectivity there is).
    pub machinery_snapshot: Vec<GroupPlaneRow>,
    pub delivered_at: Timestamp,
}

impl Signed for GroupShareDeliver {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.initiator.0
    }
}

/// What one peer-channel ceremony frame carries: which step, as the step's
/// signed envelope — the custody sibling's carriage idiom (the signatures
/// cover exactly the embedded encoding; no carriage layer ever re-encodes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupCeremonyMessage {
    Offer(EmbedAsBytes),
    Accept(EmbedAsBytes),
    Deliver(EmbedAsBytes),
}

/// Encode a ceremony message to the verbatim bytes the channel frame
/// carries.
pub fn encode_group_ceremony_message(msg: &GroupCeremonyMessage) -> Result<Vec<u8>> {
    crate::encoding::canonical_encode(msg).map(|b| b.to_vec())
}

/// Decode a channel frame's verbatim bytes back to the ceremony message.
/// Strict: unknown bytes are a refusal, never a guess.
pub fn decode_group_ceremony_message(bytes: &[u8]) -> Result<GroupCeremonyMessage> {
    crate::encoding::canonical_decode(bytes)
}

/// BLAKE3 of an offer envelope's canonical encoding — what an accept's
/// `offer_digest` binds to. One spelling for both sides.
pub fn group_offer_digest(offer_envelope: &EmbedAsBytes) -> Result<[u8; 32]> {
    let bytes = crate::encoding::canonical_encode(offer_envelope)?;
    Ok(blake3::hash(&bytes).into())
}

/// The offer's birth↔id↔authority consistency — shared by sign and verify
/// so neither side can produce what the other would refuse.
fn check_offer_birth(offer: &GroupShareOffer) -> Result<GroupBirthRecord> {
    let birth = decode_birth_for_scope(&offer.birth, &offer.scope_id)
        .map_err(|e| Error::Encoding(format!("group offer: {e}")))?;
    if birth.authority_actor != offer.initiator {
        return Err(Error::Encoding(
            "group offer initiator is not the birth record's authority actor".into(),
        ));
    }
    Ok(birth)
}

/// Sign a [`GroupShareOffer`] as the initiating account. Refuses an offer
/// whose `initiator` is not the signing keypair's actor, a self-addressed
/// offer (a share with yourself is your own fleet — R14 (account-data-plane.md § The ratified decisions)'s job, never this
/// ceremony's), and any birth↔id↔authority inconsistency.
pub fn sign_group_share_offer(
    initiator: &ActorKeypair,
    offer: &GroupShareOffer,
) -> Result<EmbedAsBytes> {
    if offer.initiator != initiator.actor_id() {
        return Err(Error::Encoding(
            "group offer names an initiator other than the signing keypair".into(),
        ));
    }
    if offer.recipient == offer.initiator {
        return Err(Error::Encoding(
            "group offer addresses its own initiator — a share with yourself is the fleet's job"
                .into(),
        ));
    }
    check_offer_birth(offer)?;
    let (bytes, env) = sign_envelope(initiator, offer)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Verify a received offer envelope. `sender` is the transport-proven
/// channel actor (PT-1b); `own_actor` is the reading account.
pub fn verify_group_share_offer(
    envelope: &EmbedAsBytes,
    sender: &ActorId,
    own_actor: &ActorId,
) -> Result<GroupShareOffer> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let offer: GroupShareOffer = decode_signed_bytes(&bytes)?;
    verify_envelope(&offer, &bytes, &env)
        .map_err(|_| Error::Encoding("group offer signature invalid".into()))?;
    if offer.initiator != *sender {
        return Err(Error::Encoding(
            "group offer is signed by someone other than the channel sender".into(),
        ));
    }
    if offer.recipient != *own_actor {
        return Err(Error::Encoding(
            "group offer addresses a different account".into(),
        ));
    }
    check_offer_birth(&offer)?;
    Ok(offer)
}

/// Sign a [`GroupShareAccept`] as the recipient. Refuses an accept whose
/// `recipient` is not the signing keypair's actor or whose embedded
/// published reception half is not the recipient's own.
pub fn sign_group_share_accept(
    recipient: &ActorKeypair,
    accept: &GroupShareAccept,
) -> Result<EmbedAsBytes> {
    if accept.recipient != recipient.actor_id() {
        return Err(Error::Encoding(
            "group accept names a recipient other than the signing keypair".into(),
        ));
    }
    crate::group_generation::verify_group_reception_published(
        &accept.reception_published,
        &accept.recipient,
    )
    .map_err(|e| Error::Encoding(format!("group accept reception half: {e}")))?;
    let (bytes, env) = sign_envelope(recipient, accept)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Verify a received accept envelope, returning the payload and its opened
/// reception half (the initiator's wrap target — already verified as the
/// recipient's own). Binding `offer_digest` to the outstanding offer is the
/// ceremony machine's job; this verifies what the payload alone can prove.
pub fn verify_group_share_accept(
    envelope: &EmbedAsBytes,
    sender: &ActorId,
) -> Result<(
    GroupShareAccept,
    crate::group_generation::GroupReceptionPublished,
)> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let accept: GroupShareAccept = decode_signed_bytes(&bytes)?;
    verify_envelope(&accept, &bytes, &env)
        .map_err(|_| Error::Encoding("group accept signature invalid".into()))?;
    if accept.recipient != *sender {
        return Err(Error::Encoding(
            "group accept is signed by someone other than the channel sender".into(),
        ));
    }
    let published = crate::group_generation::verify_group_reception_published(
        &accept.reception_published,
        &accept.recipient,
    )
    .map_err(|e| Error::Encoding(format!("group accept reception half: {e}")))?;
    Ok((accept, published))
}

/// Sign a [`GroupShareDeliver`] as the initiating account.
pub fn sign_group_share_deliver(
    initiator: &ActorKeypair,
    deliver: &GroupShareDeliver,
) -> Result<EmbedAsBytes> {
    if deliver.initiator != initiator.actor_id() {
        return Err(Error::Encoding(
            "group deliver names an initiator other than the signing keypair".into(),
        ));
    }
    let (bytes, env) = sign_envelope(initiator, deliver)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Verify a received deliver envelope: initiator-signed, sender-bound. The
/// inner artifacts carry their own verification — the snapshot rows through
/// `apply_class2` adoption strictness, the admission wrap through the
/// bundle-open door's commitment check, the joiner's own roster entry
/// through `verify_enrolled_entry` — so this verifies the delivery wrapper
/// only, exactly as the custody sibling does.
pub fn verify_group_share_deliver(
    envelope: &EmbedAsBytes,
    sender: &ActorId,
) -> Result<GroupShareDeliver> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let deliver: GroupShareDeliver = decode_signed_bytes(&bytes)?;
    verify_envelope(&deliver, &bytes, &env)
        .map_err(|_| Error::Encoding("group deliver signature invalid".into()))?;
    if deliver.initiator != *sender {
        return Err(Error::Encoding(
            "group deliver is signed by someone other than the channel sender".into(),
        ));
    }
    Ok(deliver)
}

// ── Durable ceremony state (`fauna.state.group-share-ceremony`) ─────────────
//
// The custody sibling's record-then-act law, unchanged: a consumed ceremony
// frame is captured here verbatim before any action, and every action a step
// owes (post a reply, write the held-root row, write the machinery rows,
// adopt the snapshot) is re-derivable from this state alone — the driver
// re-drives owed actions idempotently after a crash. The verbatim envelope
// bytes are the source of truth (they carry the signatures); the booleans
// are monotone progress markers (false → true only), which is what makes
// the cross-device merge ([`GroupShareConfig::merge`]) a commutative,
// associative, idempotent OR + non-empty-payload union per ceremony.
//
// Every `Vec<u8>` field — here, on the three signed payloads and on the
// snapshot row — is a CBOR byte string (`serde_bytes`), never serde's
// default integer array: that costs up to two bytes per byte, compounding
// through the deliver's three nesting levels, and put one ceremony's record
// over the plane's per-entry cap (`config-dissolution.md` § Phases and gates
// → *Bounded rows*; the custody sibling's shape). The fixed-width ids beside
// them (`[u8; 32]`, `ActorId`) are byte strings by the tree-wide rule
// (`serialization.md` § Canonical IPLD dag-cbor → *Fixed-size byte arrays*).
//
// All three types decode with `deny_unknown_fields`, deliberately: the
// record is the CrdtPerField kind `fauna.state.group-share-ceremony`, and a
// tolerant reader would silently strip a newer build's field from its
// re-encoded union (`config-dissolution.md` P4 — the kinds table's posture
// column; the seen-set's argument).

/// This account's in-flight and completed offline share initiations, both
/// sides — the READ fold over the account's `fauna.state.group-share-ceremony`
/// rows, one row per ceremony side-record ([`GroupShareRowKey`];
/// [`GroupShareConfig::rows`] and [`GroupShareConfig::fold_row`] are the two
/// directions).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupShareConfig {
    /// Ceremonies where this account is the INITIATOR, one record per
    /// `(scope, recipient)` pair.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initiated: Vec<InitiatedGroupShare>,
    /// Ceremonies where this account is the RECIPIENT, one record per
    /// scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invited: Vec<InvitedGroupShare>,
}

impl GroupShareConfig {
    /// Nothing on record — the serde skip predicate's question.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.initiated.is_empty() && self.invited.is_empty()
    }

    /// The cross-device join — the custody arm's union law at the group
    /// ceremony's keys, per side, per (scope, counterparty): booleans are
    /// monotone (OR), the verbatim frame envelopes are non-empty-wins
    /// (byte-smaller on a both-non-empty conflict: arbitrary but convergent;
    /// honest devices record identical bytes for one ceremony), the held root
    /// is non-empty-wins for the same reason (minted once at begin, identical
    /// on every device that recorded the ceremony), and the scalar remainder
    /// follows the higher `updated_at` — the record's OWN stamp, never an
    /// outer one — with a tie settled by the remainder itself (the later
    /// `offered_at`; the byte-greater `initiator`), so two replicas holding
    /// equal stamps still converge on one set of bytes. A ceremony record is the only durable copy of a consumed
    /// peer-channel frame — and, until the plane row lands, of the machinery
    /// root — so this unions; latest-wins would orphan one device's capture
    /// outright.
    ///
    /// The one statement of the rule: the `fauna.state.group-share-ceremony`
    /// plane arm calls it
    /// (`config-dissolution.md`, P1).
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let mut initiated = self.initiated.clone();
        for theirs in &other.initiated {
            match initiated
                .iter_mut()
                .find(|ours| ours.scope_id == theirs.scope_id && ours.recipient == theirs.recipient)
            {
                Some(ours) => *ours = ours.merge(theirs),
                None => initiated.push(theirs.clone()),
            }
        }
        initiated.sort_by_key(|r| (r.scope_id, r.recipient.0));
        let mut invited = self.invited.clone();
        for theirs in &other.invited {
            match invited
                .iter_mut()
                .find(|ours| ours.scope_id == theirs.scope_id)
            {
                Some(ours) => *ours = ours.merge(theirs),
                None => invited.push(theirs.clone()),
            }
        }
        invited.sort_by_key(|r| r.scope_id);
        Self { initiated, invited }
    }

    /// Every record as its plane row, `(key, record)`: initiated then
    /// invited, each in the canonical order.
    #[must_use]
    pub fn rows(&self) -> Vec<(String, GroupShareRecord)> {
        self.initiated
            .iter()
            .map(|r| (r.plane_key(), GroupShareRecord::Initiated(r.clone())))
            .chain(
                self.invited
                    .iter()
                    .map(|r| (r.plane_key(), GroupShareRecord::Invited(r.clone()))),
            )
            .collect()
    }

    /// Fold one plane row into the record — the read side of the
    /// per-ceremony rows.
    ///
    /// # Errors
    /// An unparseable key, an undecodable value, or a value naming a
    /// different ceremony than its key ([`decode_group_share_row`]).
    pub fn fold_row(&mut self, key: &str, value: &[u8]) -> Result<()> {
        let one = match decode_group_share_row(key, value)? {
            GroupShareRecord::Initiated(r) => Self {
                initiated: vec![r],
                invited: Vec::new(),
            },
            GroupShareRecord::Invited(r) => Self {
                initiated: Vec::new(),
                invited: vec![r],
            },
        };
        *self = self.merge(&one);
        Ok(())
    }
}

/// The verbatim-envelope clause of the join: non-empty wins, byte-smaller on
/// a both-non-empty conflict (arbitrary but convergent).
fn pick_bytes(ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    match (ours.is_empty(), theirs.is_empty()) {
        (true, _) => theirs.to_vec(),
        (_, true) => ours.to_vec(),
        _ if theirs < ours => theirs.to_vec(),
        _ => ours.to_vec(),
    }
}

// ── The plane rows' key grammar (`config-dissolution.md` § Phases and gates
// → *Bounded rows*) ──
//
// One row per ceremony side-record, never one per account: a record's size
// is fixed by the crypto suite and the NUMBER of records grows with use, so a
// per-account row would outgrow the per-entry cap.

/// Key prefix of an initiator-side record:
/// `initiated/<scope hex32>/<recipient actor hex32>`.
pub const INITIATED_KEY_PREFIX: &str = "initiated/";

/// Key prefix of a recipient-side record: `invited/<scope hex32>`.
pub const INVITED_KEY_PREFIX: &str = "invited/";

/// A parsed `fauna.state.group-share-ceremony` row key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupShareRowKey {
    /// `initiated/<scope>/<recipient>` — holds one [`InitiatedGroupShare`].
    Initiated {
        scope_id: [u8; 32],
        recipient: ActorId,
    },
    /// `invited/<scope>` — holds one [`InvitedGroupShare`].
    Invited { scope_id: [u8; 32] },
}

impl GroupShareRowKey {
    /// Parse a row key. Strict: only the canonical spelling (lowercase hex,
    /// exactly the segments above) parses, so one record has one key.
    ///
    /// # Errors
    /// Any other string.
    pub fn parse(key: &str) -> Result<Self> {
        let hex = |s: &str| -> Result<[u8; 32]> {
            if !crate::hex32::is_lowercase_hex64(s) {
                return Err(Error::Encoding(format!(
                    "group-share ceremony key segment is not lowercase hex32: {s:?}"
                )));
            }
            crate::hex32::decode(s).map_err(|e| Error::Encoding(e.to_string()))
        };
        if let Some(rest) = key.strip_prefix(INITIATED_KEY_PREFIX) {
            let (scope, recipient) = rest.split_once('/').ok_or_else(|| {
                Error::Encoding(format!(
                    "group-share ceremony key lacks a recipient: {key:?}"
                ))
            })?;
            return Ok(Self::Initiated {
                scope_id: hex(scope)?,
                recipient: ActorId(hex(recipient)?),
            });
        }
        if let Some(scope) = key.strip_prefix(INVITED_KEY_PREFIX) {
            return Ok(Self::Invited {
                scope_id: hex(scope)?,
            });
        }
        Err(Error::Encoding(format!(
            "not a group-share ceremony key: {key:?}"
        )))
    }

    /// The canonical key string.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::Initiated {
                scope_id,
                recipient,
            } => format!(
                "{INITIATED_KEY_PREFIX}{}/{}",
                crate::hex32::encode(scope_id),
                crate::hex32::encode(&recipient.0)
            ),
            Self::Invited { scope_id } => {
                format!("{INVITED_KEY_PREFIX}{}", crate::hex32::encode(scope_id))
            }
        }
    }
}

/// One plane row's value: the record its key names.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupShareRecord {
    Initiated(InitiatedGroupShare),
    Invited(InvitedGroupShare),
}

impl GroupShareRecord {
    /// The canonical value bytes.
    ///
    /// # Errors
    /// Canonical-encoding failure.
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Initiated(r) => crate::encoding::canonical_encode(r),
            Self::Invited(r) => crate::encoding::canonical_encode(r),
        }
        .map(|b| b.to_vec())
    }

    /// The per-record join — the halves [`GroupShareConfig::merge`] runs.
    ///
    /// # Errors
    /// The two sides are different record types or different ceremonies.
    pub fn merge(&self, other: &Self) -> Result<Self> {
        match (self, other) {
            (Self::Initiated(a), Self::Initiated(b))
                if a.scope_id == b.scope_id && a.recipient == b.recipient =>
            {
                Ok(Self::Initiated(a.merge(b)))
            }
            (Self::Invited(a), Self::Invited(b)) if a.scope_id == b.scope_id => {
                Ok(Self::Invited(a.merge(b)))
            }
            _ => Err(Error::Encoding(
                "group-share ceremony records name different ceremonies".into(),
            )),
        }
    }
}

/// Decode one `fauna.state.group-share-ceremony` row: the key's first
/// segment picks the record type, the value must decode as it
/// (`deny_unknown_fields`) and name the same ceremony as the key — a record
/// is never silently filed under another scope.
///
/// # Errors
/// An unparseable key, an undecodable value, or a key/value mismatch.
pub fn decode_group_share_row(key: &str, value: &[u8]) -> Result<GroupShareRecord> {
    let record = match GroupShareRowKey::parse(key)? {
        GroupShareRowKey::Initiated { .. } => {
            GroupShareRecord::Initiated(crate::encoding::canonical_decode(value)?)
        }
        GroupShareRowKey::Invited { .. } => {
            GroupShareRecord::Invited(crate::encoding::canonical_decode(value)?)
        }
    };
    let named = match &record {
        GroupShareRecord::Initiated(r) => r.plane_key(),
        GroupShareRecord::Invited(r) => r.plane_key(),
    };
    if named != key {
        return Err(Error::Encoding(format!(
            "group-share ceremony row at {key:?} holds the record for {named:?}"
        )));
    }
    Ok(record)
}

/// One initiator-side ceremony record. `Default` exists for the
/// struct-update fixture idiom (two branches independently growing this
/// struct then merge cleanly); production construction always names the
/// identity fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiatedGroupShare {
    /// The scope being shared.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The invited account.
    pub recipient: ActorId,
    /// The scope's machinery root, held HERE from begin until the
    /// account-plane held-root row is written through (`root_row_written`)
    /// — the ceremony-scoped custody the record-then-act law requires: a
    /// crash between minting the root and persisting it must not strand a
    /// live offer whose scope nobody can ever read.
    pub root: crate::secret::SecretByteBuf,
    /// The signed offer envelope's canonical bytes, verbatim.
    #[serde(with = "serde_bytes")]
    pub offer: Vec<u8>,
    /// Monotone: the offer frame reached the transport at least once.
    pub offer_posted: bool,
    /// The ingested accept envelope's canonical bytes, verbatim (empty
    /// until one arrives).
    #[serde(with = "serde_bytes")]
    pub accept: Vec<u8>,
    /// The assembled deliver envelope's canonical bytes, verbatim (empty
    /// until built).
    #[serde(with = "serde_bytes")]
    pub deliver: Vec<u8>,
    /// Monotone: the deliver frame reached the transport at least once.
    pub delivered: bool,
    /// Monotone: the `fauna.state.group-machinery-root` row is written.
    pub root_row_written: bool,
    /// Monotone: the scope's machinery rows (birth, both roster entries,
    /// the first mint) are written to this side's own group plane.
    pub plane_rows_written: bool,
    pub offered_at: Timestamp,
    /// Higher wins the record's scalar remainder at the cross-device merge.
    pub updated_at: Timestamp,
}

impl Default for InitiatedGroupShare {
    fn default() -> Self {
        Self {
            scope_id: [0u8; 32],
            recipient: ActorId([0u8; 32]),
            root: crate::secret::SecretByteBuf::default(),
            offer: Vec::new(),
            offer_posted: false,
            accept: Vec::new(),
            deliver: Vec::new(),
            delivered: false,
            root_row_written: false,
            plane_rows_written: false,
            offered_at: Timestamp(0),
            updated_at: Timestamp(0),
        }
    }
}

impl InitiatedGroupShare {
    /// This record's plane row key ([`GroupShareRowKey::Initiated`]).
    #[must_use]
    pub fn plane_key(&self) -> String {
        GroupShareRowKey::Initiated {
            scope_id: self.scope_id,
            recipient: self.recipient,
        }
        .render()
    }

    /// The per-record half of [`GroupShareConfig::merge`], over two records
    /// of ONE ceremony (same scope, same recipient); the identity is
    /// `self`'s.
    #[must_use]
    pub fn merge(&self, theirs: &Self) -> Self {
        let mut ours = self.clone();
        ours.root = crate::secret::SecretByteBuf::from(pick_bytes(
            ours.root.as_ref(),
            theirs.root.as_ref(),
        ));
        ours.offer = pick_bytes(&ours.offer, &theirs.offer);
        ours.accept = pick_bytes(&ours.accept, &theirs.accept);
        ours.deliver = pick_bytes(&ours.deliver, &theirs.deliver);
        ours.offer_posted |= theirs.offer_posted;
        ours.delivered |= theirs.delivered;
        ours.root_row_written |= theirs.root_row_written;
        ours.plane_rows_written |= theirs.plane_rows_written;
        if (theirs.updated_at, theirs.offered_at) > (ours.updated_at, ours.offered_at) {
            ours.offered_at = theirs.offered_at;
            ours.updated_at = theirs.updated_at;
        }
        ours
    }
}

/// One recipient-side ceremony record. `Default` for the fixture idiom, as
/// above.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitedGroupShare {
    /// The offered scope.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The offering account.
    pub initiator: ActorId,
    /// The ingested offer envelope's canonical bytes, verbatim.
    #[serde(with = "serde_bytes")]
    pub offer: Vec<u8>,
    /// Monotone: once any device declined, the invitation stays dismissed
    /// fleet-wide.
    pub declined: bool,
    /// The signed accept envelope's canonical bytes, verbatim (empty until
    /// the user accepts).
    #[serde(with = "serde_bytes")]
    pub accept: Vec<u8>,
    /// Monotone: the accept frame reached the transport at least once.
    pub accept_posted: bool,
    /// The ingested deliver envelope's canonical bytes, verbatim.
    #[serde(with = "serde_bytes")]
    pub deliver: Vec<u8>,
    /// Monotone: the held-root row is written (bundle opened, commitment
    /// verified).
    pub root_row_written: bool,
    /// Monotone: the deliver's machinery snapshot is adopted into this
    /// side's own group plane.
    pub rows_adopted: bool,
    /// Higher wins the record's scalar remainder at the cross-device merge.
    pub updated_at: Timestamp,
}

impl Default for InvitedGroupShare {
    fn default() -> Self {
        Self {
            scope_id: [0u8; 32],
            initiator: ActorId([0u8; 32]),
            offer: Vec::new(),
            declined: false,
            accept: Vec::new(),
            accept_posted: false,
            deliver: Vec::new(),
            root_row_written: false,
            rows_adopted: false,
            updated_at: Timestamp(0),
        }
    }
}

impl InvitedGroupShare {
    /// This record's plane row key ([`GroupShareRowKey::Invited`]).
    #[must_use]
    pub fn plane_key(&self) -> String {
        GroupShareRowKey::Invited {
            scope_id: self.scope_id,
        }
        .render()
    }

    /// The per-record half of [`GroupShareConfig::merge`], over two records
    /// of ONE ceremony (same scope); the scope is `self`'s.
    #[must_use]
    pub fn merge(&self, theirs: &Self) -> Self {
        let mut ours = self.clone();
        ours.offer = pick_bytes(&ours.offer, &theirs.offer);
        ours.accept = pick_bytes(&ours.accept, &theirs.accept);
        ours.deliver = pick_bytes(&ours.deliver, &theirs.deliver);
        ours.declined |= theirs.declined;
        ours.accept_posted |= theirs.accept_posted;
        ours.root_row_written |= theirs.root_row_written;
        ours.rows_adopted |= theirs.rows_adopted;
        if (theirs.updated_at, theirs.initiator.0) > (ours.updated_at, ours.initiator.0) {
            ours.initiator = theirs.initiator;
            ours.updated_at = theirs.updated_at;
        }
        ours
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group_generation::{GroupReceptionKeyRecord, sign_group_reception_published};
    use crate::group_scope::group_scope_id;

    fn initiator() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    fn recipient() -> ActorKeypair {
        ActorKeypair::from_secret([31u8; 32])
    }

    fn outsider() -> ActorKeypair {
        ActorKeypair::from_secret([41u8; 32])
    }

    fn birth() -> GroupBirthRecord {
        GroupBirthRecord {
            authority_actor: initiator().actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: crate::crypto::GroupMachineryRoot::from_bytes([0xD7; 32])
                .commitment(),
            created_at_ms: 1_700_000_000_000,
        }
    }

    fn offer() -> GroupShareOffer {
        let b = birth();
        GroupShareOffer {
            scope_id: group_scope_id(&b).unwrap(),
            initiator: initiator().actor_id(),
            recipient: recipient().actor_id(),
            birth: crate::encoding::canonical_encode(&b).unwrap(),
            offered_at: Timestamp(1_000),
        }
    }

    fn reception_published_for(member: &ActorKeypair) -> Vec<u8> {
        let record = GroupReceptionKeyRecord::mint(1_000);
        sign_group_reception_published(member, record.reception_pubkey().unwrap(), 2_000).unwrap()
    }

    #[test]
    fn the_offer_round_trips_and_binds_sender_and_addressee() {
        let env = sign_group_share_offer(&initiator(), &offer()).unwrap();
        let opened =
            verify_group_share_offer(&env, &initiator().actor_id(), &recipient().actor_id())
                .expect("verifies");
        assert_eq!(opened, offer());
        // A forwarded offer (transport sender ≠ signer) conveys nothing.
        assert!(
            verify_group_share_offer(&env, &outsider().actor_id(), &recipient().actor_id())
                .is_err()
        );
        // A reader who is not the addressee refuses it.
        assert!(
            verify_group_share_offer(&env, &initiator().actor_id(), &outsider().actor_id())
                .is_err()
        );
    }

    /// Nobody can offer a scope whose name they do not hold: the id is
    /// recomputed from the carried birth record, and the birth's authority
    /// must be the offering actor.
    #[test]
    fn an_offer_cannot_name_a_scope_it_did_not_earn() {
        // Scope id not the birth's content-derived id.
        let mut squatted = offer();
        squatted.scope_id = [0xAA; 32];
        assert!(sign_group_share_offer(&initiator(), &squatted).is_err());

        // A birth naming a different authority than the initiator: signable
        // by nobody — the initiator refuses it at sign, and an outsider
        // cannot sign as the initiator at all.
        let foreign_birth = GroupBirthRecord {
            authority_actor: outsider().actor_id(),
            ..birth()
        };
        let mut stolen = offer();
        stolen.scope_id = group_scope_id(&foreign_birth).unwrap();
        stolen.birth = crate::encoding::canonical_encode(&foreign_birth).unwrap();
        assert!(sign_group_share_offer(&initiator(), &stolen).is_err());
    }

    #[test]
    fn a_self_addressed_offer_is_refused() {
        let mut own = offer();
        own.recipient = initiator().actor_id();
        assert!(sign_group_share_offer(&initiator(), &own).is_err());
    }

    #[test]
    fn the_accept_binds_the_recipients_own_reception_half() {
        let offer_env = sign_group_share_offer(&initiator(), &offer()).unwrap();
        let accept = GroupShareAccept {
            scope_id: offer().scope_id,
            offer_digest: group_offer_digest(&offer_env).unwrap(),
            recipient: recipient().actor_id(),
            reception_published: reception_published_for(&recipient()),
            accepted_at: Timestamp(2_000),
        };
        let env = sign_group_share_accept(&recipient(), &accept).unwrap();
        let (opened, published) =
            verify_group_share_accept(&env, &recipient().actor_id()).expect("verifies");
        assert_eq!(opened.offer_digest, group_offer_digest(&offer_env).unwrap());
        assert_eq!(published.member_actor, recipient().actor_id());

        // An accept smuggling SOMEONE ELSE'S published half is refused at
        // sign and at verify alike.
        let smuggled = GroupShareAccept {
            reception_published: reception_published_for(&outsider()),
            ..accept
        };
        assert!(sign_group_share_accept(&recipient(), &smuggled).is_err());

        // A forwarded accept conveys nothing.
        assert!(verify_group_share_accept(&env, &outsider().actor_id()).is_err());
    }

    #[test]
    fn the_deliver_is_sender_bound_and_the_message_carriage_round_trips() {
        let deliver = GroupShareDeliver {
            scope_id: offer().scope_id,
            initiator: initiator().actor_id(),
            roster_entry_id: [0x33; 32],
            admission_wrap: vec![0xEE; 32],
            machinery_snapshot: vec![GroupPlaneRow {
                kind: "fauna.group.birth".into(),
                key: "self".into(),
                value: crate::encoding::canonical_encode(&birth()).unwrap(),
            }],
            delivered_at: Timestamp(3_000),
        };
        let env = sign_group_share_deliver(&initiator(), &deliver).unwrap();
        let opened = verify_group_share_deliver(&env, &initiator().actor_id()).expect("verifies");
        assert_eq!(opened, deliver);
        assert!(verify_group_share_deliver(&env, &outsider().actor_id()).is_err());

        // The frame carriage: encode → decode is verbatim, and the embedded
        // envelope still verifies (no carriage layer re-encodes).
        let frame =
            encode_group_ceremony_message(&GroupCeremonyMessage::Deliver(env.clone())).unwrap();
        let GroupCeremonyMessage::Deliver(back) = decode_group_ceremony_message(&frame).unwrap()
        else {
            panic!("carriage changed the step");
        };
        assert!(verify_group_share_deliver(&back, &initiator().actor_id()).is_ok());
    }

    /// The row key grammar round-trips, and only its canonical spelling
    /// parses — one record, one key.
    #[test]
    fn the_row_key_grammar_round_trips_and_is_strict() {
        let initiated = InitiatedGroupShare {
            scope_id: [0xAB; 32],
            recipient: ActorId([0x01; 32]),
            ..Default::default()
        };
        let invited = InvitedGroupShare {
            scope_id: [0xCD; 32],
            ..Default::default()
        };
        let ik = initiated.plane_key();
        let vk = invited.plane_key();
        assert_eq!(
            ik,
            format!("initiated/{}/{}", "ab".repeat(32), "01".repeat(32))
        );
        assert_eq!(vk, format!("invited/{}", "cd".repeat(32)));
        assert_eq!(GroupShareRowKey::parse(&ik).unwrap().render(), ik);
        assert_eq!(GroupShareRowKey::parse(&vk).unwrap().render(), vk);
        for bad in [
            "self".to_string(),
            ik.to_uppercase(),
            format!("initiated/{}", "ab".repeat(32)),
            format!("invited/{}/", "cd".repeat(32)),
            format!("invited/{}", "cd".repeat(31)),
            format!("other/{}", "cd".repeat(32)),
        ] {
            assert!(
                GroupShareRowKey::parse(&bad).is_err(),
                "{bad} must not parse"
            );
        }
    }

    /// Rows and the fold are inverses, and a row filed under another
    /// ceremony's key — or holding the other side's record — is refused.
    #[test]
    fn the_rows_fold_back_and_a_misfiled_row_is_refused() {
        let cfg = GroupShareConfig {
            initiated: vec![
                InitiatedGroupShare {
                    scope_id: [1; 32],
                    recipient: ActorId([9; 32]),
                    offer: vec![1],
                    ..Default::default()
                },
                InitiatedGroupShare {
                    scope_id: [2; 32],
                    recipient: ActorId([9; 32]),
                    offer: vec![2],
                    ..Default::default()
                },
            ],
            invited: vec![InvitedGroupShare {
                scope_id: [3; 32],
                offer: vec![3],
                ..Default::default()
            }],
        };
        let rows = cfg.rows();
        assert_eq!(rows.len(), 3);
        let mut folded = GroupShareConfig::default();
        for (key, record) in rows.iter().rev() {
            folded.fold_row(key, &record.encode().unwrap()).unwrap();
        }
        assert_eq!(folded, cfg);

        let (first_key, first) = &rows[0];
        let (second_key, _) = &rows[1];
        let (invited_key, invited) = &rows[2];
        assert!(decode_group_share_row(second_key, &first.encode().unwrap()).is_err());
        assert!(decode_group_share_row(first_key, &invited.encode().unwrap()).is_err());
        assert!(decode_group_share_row(invited_key, &invited.encode().unwrap()).is_ok());
    }
}
