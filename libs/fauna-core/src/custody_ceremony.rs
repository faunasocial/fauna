//! The custody ceremony's carrier-agnostic payloads — offer / accept /
//! deliver (W8.4 (account-data-plane.md § Workstreams)).
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Replica
//! posture → *The custody grant + ceremony* (T13), "The ceremony — offer /
//! accept / mint, and it is the discovery seam". These are the signed
//! structures that ride an **established conversation channel** between the
//! owner and the host account (`ChannelMessageBody::Custody` carries the
//! canonical bytes of a [`CustodyCeremonyMessage`] verbatim — the
//! `GroupMetaMessage::Succession` carriage idiom: the signatures cover
//! exactly this encoding, so no carriage layer ever re-encodes).
//!
//! Three payloads, three signers, one binding rule each:
//!
//! * [`CustodyOffer`] — signed by the **owner** actor; names its addressee
//!   (`host`), the proposed scopes + duration, and the owner's current dial
//!   candidates + nest URL (the ceremony is also the *discovery* seam — a
//!   non-fleet host can never read the fleet-only `device-endpoints` kind).
//! * [`CustodyAccept`] — signed by the **host** actor; binds the serving
//!   principal — the accepting device (`custodian_key` = its peer-plane
//!   NodeId), or the host's NEST when `custodian_nest_url` is present
//!   (`custodian_key` = the host's pinned nest actor identity; the
//!   nest-custodian identity fact, ruled 2026-08-17) — to one exact offer
//!   (`offer_digest`), and carries the host-side byte budget + an optional
//!   narrowed scope set (two-sided consent: the effective set is the
//!   intersection — computed by the ceremony machine, which owns the scope
//!   vocabulary).
//! * [`CustodyDeliver`] — signed by the **owner** actor; carries the minted
//!   admission witness ([`crate::custody_grant::CustodyGrant`]'s envelope)
//!   **verbatim**, plus a fresh owner-fleet candidate snapshot — everything
//!   the host needs to write its `fauna.state.custodies-held` row.
//!
//! **Sender binding is deliberate and differs from the Succession idiom:**
//! a succession statement is a claim any member may carry (the signatures
//! alone are the authority), but a ceremony step IS its author's act — so
//! each verifier here also requires the signer to BE the transport-proven
//! channel sender, and the offer's addressee to BE the reading actor. A
//! forwarded or replayed ceremony payload conveys nothing.
//!
//! The ceremony *state machine* (record-then-act, expiry decay, the mint
//! door) lives with the capability plane
//! (`fauna_client_capabilities::custody_ceremony`), not here — this module
//! owns only the payload shapes and their verification, exactly as
//! [`crate::custody_grant`] owns only the witness.

use serde::{Deserialize, Serialize};

use crate::custody_grant::{CustodyScopeSet, check_grant_id};
use crate::data::Timestamp;
use crate::device_endpoints::DeviceEndpoints;
use crate::encoding::{EmbedAsBytes, Signed, decode_signed_bytes, sign_envelope, verify_envelope};
use crate::error::{Error, Result};
use crate::identity::{ActorId, ActorKeypair};

/// The accept-time byte-budget default (T15: "a `retained_bytes_cap` the
/// host chooses at accept — default a hard-coded Rust constant"). 8 GiB.
/// The later policy build (metering + eviction) may re-pin the value; the
/// mechanism — a host-chosen cap carried from the accept onward — is T13/T15
/// shape and does not move.
pub const DEFAULT_RETAINED_BYTES_CAP: u64 = 8 * 1024 * 1024 * 1024;

/// The **ceiling** on a host-chosen `retained_bytes_cap` — equal to
/// [`DEFAULT_RETAINED_BYTES_CAP`], because the accept-time default IS the
/// maximum: a host narrows the budget, never widens it.
///
/// a custody-hosting row's budget was written through
/// verbatim, so any account holder could deposit `u64::MAX` and make their nest
/// hold without bound. This is deliberately **not** a larger, distinct number.
/// "How much may one host be allowed to hold" is a *human* choice — it belongs
/// to the tier/quota surface an admin already configures, not to a constant a
/// future session would be tempted to bump. Bucket 1 under
/// `docs/goal/principles.md` § *One configuration surface*: no human would
/// choose a per-row ceiling, so it is a Rust constant and never config.
pub const MAX_RETAINED_BYTES_CAP: u64 = DEFAULT_RETAINED_BYTES_CAP;

/// How many custody-hosting rows one host may register on its own nest.
///
/// This bounds more than disk: the nest's hosting pump dials **every**
/// registered row's owner nest once per pass, so the row count is also the
/// per-host outbound dial fan-out and the number of on-disk custodied stores.
/// Eight is generous for the real shape of the feature (a household's worth of
/// friends' custodies) while keeping the worst case a hostile account can reach
/// at `MAX_CUSTODY_HOSTING_ROWS_PER_HOST × MAX_RETAINED_BYTES_CAP`. Bucket 1,
/// same reasoning as [`MAX_RETAINED_BYTES_CAP`].
pub const MAX_CUSTODY_HOSTING_ROWS_PER_HOST: usize = 8;

/// The row cap read from the **pump's** side: may a custodian nest pump the row
/// it has just reached, given how many of that host's rows this pass has already
/// seen (`seen_for_host`, 0-based)?
///
/// The byte ceiling had a backstop in the pump from the start
/// ([`MAX_RETAINED_BYTES_CAP`] is re-applied per pass) while the row cap was
/// enforced at the register door **only** — so a row set exceeding the cap, one
/// planted by any second writer that bypasses the door, kept
/// its full 15-minute outbound dial fan-out. That asymmetry is found while verifying; three artifacts had already
/// recorded the pump as enforcing *both*, which is what makes a missing backstop
/// expensive: the next reader inherits it as present.
///
/// The pump counts rows the way the door does — **stopped rows included**, since
/// a stopped row still occupies a registry slot the door would have refused — so
/// the two sides cannot disagree about which rows are the admitted set. The
/// caller tallies per host (never a run-length counter over its enumeration): the
/// registry query does order by `(host, updated_at, grant_id)`, which is what
/// makes the admitted set deterministically the host's oldest-updated
/// [`MAX_CUSTODY_HOSTING_ROWS_PER_HOST`] — but the *bound* must hold under any
/// order, because a security floor that depends on an `ORDER BY` in another
/// module is one refactor away from silently fail-opening.
#[must_use]
pub fn hosting_pump_admits_row(seen_for_host: usize) -> bool {
    seen_for_host < MAX_CUSTODY_HOSTING_ROWS_PER_HOST
}

/// The two bounds above as one decision: `Ok` with the cap to store, `Err` with
/// the refusal reason (human-readable, stable enough to surface, never parsed).
///
/// It lives beside the constants rather than in the nest's register door on
/// purpose: the door enforces it, but a **host's app** needs the same rule to
/// tell the user "you are at the hosting cap" or "your budget was clamped"
/// before the deposit round-trips — and two copies of a rule is exactly the
/// divergence the shared-Rust priority exists to prevent.
///
/// The two bounds behave differently, deliberately:
///
/// * **rows refuse** — a row-cap hit is a caller error worth surfacing, and the
///   count bounds the custodian nest's per-host outbound dial fan-out and
///   on-disk store count, not merely its disk;
/// * **bytes clamp** — a too-large budget is honoured at the ceiling, so a
///   legitimate host is never locked out by a number their own app suggested.
///
/// A **rewrite** (`is_rewrite`: this host already holds a row under this grant
/// id) is admitted even at the row cap. Stop and budget-adjust are that same
/// re-register verb, so refusing it would leave a host at the cap unable to
/// *stop* its own rows — turning the cap into the unrecoverable state it exists
/// to prevent.
///
/// `0` means "no cap recorded" all the way down to the custodian's budget pass,
/// which substitutes [`DEFAULT_RETAINED_BYTES_CAP`]; the clamp preserves it
/// rather than reading it as a real budget of zero (which would evict a
/// custody's whole payload on the strength of a missing field).
///
/// ⚠ The error half is spelled `core::result::Result` on purpose: this module
/// imports `crate::error::Result`, a one-parameter alias, so the bare name would
/// not accept a second type argument. The refusal is a plain message rather than
/// a `crate::error::Error` variant because it is a *policy* verdict a caller
/// surfaces verbatim, like `counterparty_url`'s.
pub fn bound_hosting_deposit(
    held_rows: usize,
    is_rewrite: bool,
    requested_cap: u64,
) -> core::result::Result<u64, String> {
    if !is_rewrite && held_rows >= MAX_CUSTODY_HOSTING_ROWS_PER_HOST {
        return Err(format!(
            "custody-hosting row cap reached ({held_rows} of \
             {MAX_CUSTODY_HOSTING_ROWS_PER_HOST}) — stop or remove a hosting row before \
             registering another"
        ));
    }
    Ok(requested_cap.min(MAX_RETAINED_BYTES_CAP))
}

/// One hosting row's **effective** byte budget: the host's own number, squeezed
/// by the per-row ceiling and then by whatever headroom the host's tier leaves
/// after its *other* rows.
///
/// The accounting half (`account-data-plane.md` § Replica posture →
/// *The custody grant + ceremony* → **Two-sided bounds**, the held-bytes
/// bullet). Held custody bytes are counted in their own **derived** figure —
/// `SUM(custody_hosting.held_bytes)` per host, which the custodian's pump
/// already meters every pass — and bounded by that host's tier
/// `max_storage_bytes`, so "how much may a host hold" stays an admin choice on a
/// surface that already exists. They deliberately do **not** join the shared
/// per-actor sync counter: that one is enforced against the host's own file
/// writes, so charging custody there would make a friend's custody refuse the
/// host's own uploads.
///
/// `tier_bound` is `None` for "no cap" (the actor has no tier row, or
/// enforcement is off) — fail **open**, matching how the sync plane's metering
/// callers read the same absent lookup.
///
/// **`others_held` is the sum over the host's OTHER rows, not including this
/// one.** The pump enumerates rows in a stable order, which makes the policy
/// stated rather than incidental: **first-registered keeps its space**, and
/// later rows absorb the squeeze. A row squeezed to a smaller budget is not a
/// loss event — the custodian's eviction is payload-only and its `AtFloor` state
/// is the honest report.
pub fn effective_hosting_cap(requested_cap: u64, tier_bound: Option<u64>, others_held: u64) -> u64 {
    let capped = if requested_cap == 0 {
        DEFAULT_RETAINED_BYTES_CAP
    } else {
        requested_cap.min(MAX_RETAINED_BYTES_CAP)
    };
    match tier_bound {
        None => capped,
        Some(bound) => capped.min(bound.saturating_sub(others_held)),
    }
}

/// How long past a hosting witness's expiry the custodied store's bytes are
/// retained before the pump reclaims them (the expired-row GC).
///
/// The **T-window vocabulary** applied to hosting (the destination-side
/// custody grace window `T = 30 days`, `message-segment-store.md` § Custody
/// grace window): a lapsed custody keeps its bytes one grace window so an
/// owner who simply let the ~90-day witness lapse can re-mint and find the
/// store warm, and reclaims after it so an abandoned custody is bounded. A
/// Rust constant, not a knob — nobody would want to choose this
/// (`principles.md` § One configuration surface).
pub const HOSTING_EXPIRED_STORE_GC_GRACE_SECS: u64 = 30 * 24 * 3600;

/// How far past `now + offer.duration_secs` a delivered witness's `expires_at`
/// may reach before the host refuses it at ingest — the allowance for the
/// owner's clock running ahead of the host's (the owner mints at ITS now,
/// always before the host ingests).
///
/// The term the host accepted is the offer's duration from mint; a witness
/// reaching further is a term the host never consented to ("renew widens
/// nothing — widening scope or budget is a fresh offer→accept round",
/// `account-replica-posture.md` § Scope subsets). A day on a ~90-day term: wide
/// enough that an honest skewed clock never strands a ceremony, narrow
/// enough to bound the widening. A Rust constant, not a knob
/// (`principles.md` § One configuration surface).
pub const CUSTODY_WITNESS_MINT_SKEW_SECS: u64 = 24 * 3600;

/// May a hosting row's share of its custodied store be reclaimed? — true
/// once the row's witness has been expired for more than
/// [`HOSTING_EXPIRED_STORE_GC_GRACE_SECS`]. Timestamps in the custody
/// ceremony's microseconds. The pump applies this per `(host, owner)` PAIR:
/// the store dir is shared by every grant of the pair, so it falls only when
/// every row answers true (a live, stopped, or merely-expired-within-grace
/// row keeps the pair's bytes).
#[must_use]
pub fn hosting_store_reclaimable(expires_at: Timestamp, now: Timestamp) -> bool {
    // An unexpired witness saturates to 0 lapsed — never reclaimable.
    now.0.saturating_sub(expires_at.0) / 1_000_000 > HOSTING_EXPIRED_STORE_GC_GRACE_SECS
}

/// The owner's signed custody proposal (ceremony step 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyOffer {
    /// 16 opaque bytes in the capability-grant id space — chosen by the
    /// owner at offer time, threading the whole ceremony (the accept binds
    /// to it, the mint uses it, both registry rows key on it). A CBOR byte
    /// string (`serialization.md` § Canonical IPLD dag-cbor), as on every
    /// type carrying the id.
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// The granting account — the signer of this payload.
    pub owner: ActorId,
    /// The addressee: the account being asked to hold custody. A reader
    /// whose own actor is not `host` refuses the payload.
    pub host: ActorId,
    /// The proposed coverage (the mint may end narrower, never wider).
    pub scopes: CustodyScopeSet,
    /// Proposed witness lifetime in seconds from mint (the capability
    /// plane's ~90-day default when the offering app has no reason to pick
    /// another).
    pub duration_secs: u64,
    /// The owner fleet's current dial identities + candidates — discovery
    /// for a host that can never read the owner's fleet-only
    /// `device-endpoints` kind.
    pub owner_devices: Vec<DeviceEndpoints>,
    /// The owner's nest base URL — the always-on anchor the custodian's
    /// nest-pull leg (W8.6) dials.
    pub owner_nest_url: Option<String>,
    pub offered_at: Timestamp,
}

impl Signed for CustodyOffer {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.owner.0
    }
}

impl Default for CustodyOffer {
    fn default() -> Self {
        Self {
            grant_id: Vec::new(),
            owner: ActorId([0u8; 32]),
            host: ActorId([0u8; 32]),
            scopes: CustodyScopeSet::Account,
            duration_secs: 0,
            owner_devices: Vec::new(),
            owner_nest_url: None,
            offered_at: Timestamp(0),
        }
    }
}

/// The host's signed acceptance (ceremony step 2) — binds the serving
/// device and answers the budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyAccept {
    /// The offer's grant id, echoed — a CBOR byte string.
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// BLAKE3 of the offer's canonical envelope bytes — binds this accept
    /// to one exact offer (a replayed accept against a re-offer with the
    /// same grant id fails this digest).
    #[serde(with = "serde_bytes")]
    pub offer_digest: [u8; 32],
    /// The accepting account — the signer of this payload.
    pub host: ActorId,
    /// The SERVING device's principal Ed25519 key (= its peer-plane
    /// NodeId) — what the minted witness will name. The accept binds the
    /// device, not just the account.
    #[serde(with = "serde_bytes")]
    pub custodian_key: [u8; 32],
    /// The serving device's dial identity + candidates (its
    /// `endpoints.node_id` equals [`Self::custodian_key`] — cross-checked
    /// at verification).
    pub custodian_endpoints: DeviceEndpoints,
    /// The host-side byte budget (T15) — [`DEFAULT_RETAINED_BYTES_CAP`]
    /// unless the host chose otherwise.
    pub retained_bytes_cap: u64,
    /// `Some` = the host consents to a subset only; the effective set is
    /// the intersection, computed and validated by the ceremony machine.
    pub narrowed_scopes: Option<CustodyScopeSet>,
    pub accepted_at: Timestamp,
    /// `Some` = the bound custodian is the host's NEST (the nest-custodian
    /// identity fact, ruled 2026-08-17): [`Self::custodian_key`] then names
    /// the host's **pinned** nest actor identity (TOFU state, never the
    /// nest's self-claim) and this URL is the owner fleet's dial anchor;
    /// the endpoints carry no candidates (nest-form validation). `None` =
    /// the accepting device. Absent stays off the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian_nest_url: Option<String>,
}

impl Signed for CustodyAccept {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.host.0
    }
}

impl Default for CustodyAccept {
    fn default() -> Self {
        Self {
            grant_id: Vec::new(),
            offer_digest: [0u8; 32],
            host: ActorId([0u8; 32]),
            custodian_key: [0u8; 32],
            custodian_endpoints: DeviceEndpoints::default(),
            retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
            narrowed_scopes: None,
            accepted_at: Timestamp(0),
            custodian_nest_url: None,
        }
    }
}

/// The nest-form invariants (the nest-custodian identity fact): a
/// nest-anchored accept names a non-empty dial anchor and carries NO dial
/// candidates — the URL is the only anchor, and the
/// `endpoints.node_id == custodian_key` invariant is unchanged. Checked at
/// sign AND verify, like the endpoints cross-check.
fn check_nest_form(accept: &CustodyAccept) -> Result<()> {
    let Some(url) = &accept.custodian_nest_url else {
        return Ok(());
    };
    if url.is_empty() {
        return Err(Error::Encoding(
            "nest-anchored custody accept carries an empty nest URL".into(),
        ));
    }
    let e = &accept.custodian_endpoints;
    if !e.lan_addrs.is_empty() || !e.public_addrs.is_empty() || e.relay_url.is_some() {
        return Err(Error::Encoding(
            "nest-anchored custody accept carries dial candidates — the nest URL is the only anchor"
                .into(),
        ));
    }
    Ok(())
}

/// The owner's signed witness delivery (ceremony step 3's last leg).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyDeliver {
    /// The ceremony's grant id, echoed (must match the witness inside) — a
    /// CBOR byte string.
    #[serde(with = "serde_bytes")]
    pub grant_id: Vec<u8>,
    /// The delivering account — the signer of this payload.
    pub owner: ActorId,
    /// The minted admission witness, **verbatim** — the exact
    /// [`EmbedAsBytes`] the host will present at every admission exchange
    /// and store in its `custodies-held` row. Never re-encoded here.
    pub witness: EmbedAsBytes,
    /// A fresh owner-fleet candidate snapshot (may supersede the offer's).
    pub owner_devices: Vec<DeviceEndpoints>,
    /// The owner's nest base URL (may supersede the offer's).
    pub owner_nest_url: Option<String>,
}

impl Signed for CustodyDeliver {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.owner.0
    }
}

/// What one `ChannelMessageBody::Custody` record carries: which ceremony
/// step, as the step's signed envelope. Canonical-dag-cbor encoded to the
/// body's verbatim bytes via [`encode_ceremony_message`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodyCeremonyMessage {
    Offer(EmbedAsBytes),
    Accept(EmbedAsBytes),
    Deliver(EmbedAsBytes),
}

/// Encode a ceremony message to the verbatim bytes the channel body
/// carries.
pub fn encode_ceremony_message(msg: &CustodyCeremonyMessage) -> Result<Vec<u8>> {
    crate::encoding::canonical_encode(msg).map(|b| b.to_vec())
}

/// Decode a channel body's verbatim bytes back to the ceremony message.
/// Strict: unknown bytes are a refusal, never a guess.
pub fn decode_ceremony_message(bytes: &[u8]) -> Result<CustodyCeremonyMessage> {
    crate::encoding::canonical_decode(bytes)
}

/// Sign a [`CustodyOffer`] with the owner's actor identity key. Refuses an
/// offer whose `owner` is not the signing keypair's actor, and a
/// self-addressed offer (custody is a cross-account contract — same-account
/// custodians admit by `DeviceAuthorization` and never run this ceremony).
pub fn sign_custody_offer(owner: &ActorKeypair, offer: &CustodyOffer) -> Result<EmbedAsBytes> {
    if offer.owner != owner.actor_id() {
        return Err(Error::Encoding(
            "custody offer names an owner other than the signing keypair".into(),
        ));
    }
    if offer.host == offer.owner {
        return Err(Error::Encoding(
            "custody offer addresses its own owner — same-account custodians need no grant".into(),
        ));
    }
    check_grant_id(&offer.grant_id, "ceremony")?;
    let (bytes, env) = sign_envelope(owner, offer)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Sign a [`CustodyAccept`] with the host's actor identity key. Refuses an
/// accept whose `host` is not the signing keypair's actor, and one whose
/// `custodian_endpoints` names a different device than `custodian_key`.
pub fn sign_custody_accept(host: &ActorKeypair, accept: &CustodyAccept) -> Result<EmbedAsBytes> {
    if accept.host != host.actor_id() {
        return Err(Error::Encoding(
            "custody accept names a host other than the signing keypair".into(),
        ));
    }
    if accept.custodian_endpoints.node_id != accept.custodian_key {
        return Err(Error::Encoding(
            "custody accept's endpoints name a different device than its custodian key".into(),
        ));
    }
    check_nest_form(accept)?;
    check_grant_id(&accept.grant_id, "ceremony")?;
    let (bytes, env) = sign_envelope(host, accept)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Sign a [`CustodyDeliver`] with the owner's actor identity key.
pub fn sign_custody_deliver(
    owner: &ActorKeypair,
    deliver: &CustodyDeliver,
) -> Result<EmbedAsBytes> {
    if deliver.owner != owner.actor_id() {
        return Err(Error::Encoding(
            "custody deliver names an owner other than the signing keypair".into(),
        ));
    }
    check_grant_id(&deliver.grant_id, "ceremony")?;
    let (bytes, env) = sign_envelope(owner, deliver)?;
    Ok(EmbedAsBytes::from_signed(bytes, env))
}

/// Verify a received offer envelope. `sender` is the transport-proven
/// channel sender (MLS-authenticated); `own_actor` is the reading account.
///
/// Rules: the envelope verifies under `offer.owner`; the signer IS the
/// sender (a ceremony step is its author's act — a forwarded offer conveys
/// nothing); the addressee IS the reader; the grant id is well-formed.
pub fn verify_custody_offer(
    envelope: &EmbedAsBytes,
    sender: &ActorId,
    own_actor: &ActorId,
) -> Result<CustodyOffer> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let offer: CustodyOffer = decode_signed_bytes(&bytes)?;
    verify_envelope(&offer, &bytes, &env)
        .map_err(|_| Error::Encoding("custody offer signature invalid".into()))?;
    if offer.owner != *sender {
        return Err(Error::Encoding(
            "custody offer is signed by someone other than the channel sender".into(),
        ));
    }
    if offer.host != *own_actor {
        return Err(Error::Encoding(
            "custody offer addresses a different account".into(),
        ));
    }
    check_grant_id(&offer.grant_id, "ceremony")?;
    Ok(offer)
}

/// Verify a received accept envelope. Binding to one exact offer
/// (`offer_digest` + `grant_id` against the outstanding offer) is the
/// ceremony machine's job — it holds the outstanding state; this verifies
/// what is checkable from the payload alone.
pub fn verify_custody_accept(envelope: &EmbedAsBytes, sender: &ActorId) -> Result<CustodyAccept> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let accept: CustodyAccept = decode_signed_bytes(&bytes)?;
    verify_envelope(&accept, &bytes, &env)
        .map_err(|_| Error::Encoding("custody accept signature invalid".into()))?;
    if accept.host != *sender {
        return Err(Error::Encoding(
            "custody accept is signed by someone other than the channel sender".into(),
        ));
    }
    if accept.custodian_endpoints.node_id != accept.custodian_key {
        return Err(Error::Encoding(
            "custody accept's endpoints name a different device than its custodian key".into(),
        ));
    }
    check_nest_form(&accept)?;
    check_grant_id(&accept.grant_id, "ceremony")?;
    Ok(accept)
}

/// Verify a received deliver envelope. The inner witness is verified
/// separately by the host against its own custodian key
/// ([`crate::custody_grant::verify_custody_witness`]) — this verifies the
/// delivery wrapper: owner-signed, sender-bound, ids consistent.
pub fn verify_custody_deliver(envelope: &EmbedAsBytes, sender: &ActorId) -> Result<CustodyDeliver> {
    let (bytes, env) = envelope.clone().into_signed()?;
    let deliver: CustodyDeliver = decode_signed_bytes(&bytes)?;
    verify_envelope(&deliver, &bytes, &env)
        .map_err(|_| Error::Encoding("custody deliver signature invalid".into()))?;
    if deliver.owner != *sender {
        return Err(Error::Encoding(
            "custody deliver is signed by someone other than the channel sender".into(),
        ));
    }
    check_grant_id(&deliver.grant_id, "ceremony")?;
    Ok(deliver)
}

/// BLAKE3 of an offer envelope's canonical encoding — what an accept's
/// `offer_digest` binds to. One spelling for both sides.
pub fn offer_digest(offer_envelope: &EmbedAsBytes) -> Result<[u8; 32]> {
    let bytes = crate::encoding::canonical_encode(offer_envelope)?;
    Ok(blake3::hash(&bytes).into())
}

// ── Durable ceremony state (`fauna.state.custody-ceremony`) ───────────────────────────
//
// Every ceremony step is record-then-act: a consumed MLS application message
// cannot be re-decrypted (the succession-parking lesson), so each payload is
// durably captured here before any action, and every action a step owes
// (post a reply, run the mint door, put a registry row) is re-derivable from
// this state alone — the driver re-drives owed actions idempotently after a
// crash. Expiry decay is computed from these records + `now` at drive time,
// never a timer.
//
// The verbatim envelope bytes are the source of truth (they carry the
// signatures); the booleans are monotone progress markers (false → true
// only), which is what makes the cross-device merge
// (`CustodyConfig::merge`) a commutative, associative,
// idempotent OR + non-empty-payload union per grant id.

/// The custody-ceremony record (`fauna.state.custody-ceremony`) — this
/// account's in-flight and completed ceremonies, both sides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CustodyConfig {
    /// Ceremonies where this account is the OWNER (granting custody),
    /// keyed by grant id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub granted: Vec<GrantedCustody>,
    /// Ceremonies where this account is the HOST (holding custody),
    /// keyed by grant id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<HeldCustody>,
}

/// serde `default` for the `Timestamp` fields below (`Timestamp` itself
/// deliberately derives no `Default`).
fn ts_zero() -> Timestamp {
    Timestamp(0)
}

/// One owner-side ceremony: this account offered custody to `host`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantedCustody {
    /// The ceremony's grant id (16 bytes) — the record key.
    #[serde(with = "serde_bytes", default)]
    pub grant_id: Vec<u8>,
    /// The host account the offer addresses.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub host: [u8; 32],
    /// The conversation channel (hex `ChannelId`) the ceremony rides.
    #[serde(default)]
    pub channel_hex: String,
    /// The signed offer envelope's canonical bytes, verbatim — the accept's
    /// digest binds to exactly these; also the re-post source.
    #[serde(with = "serde_bytes", default)]
    pub offer: Vec<u8>,
    /// The offer payload reached the channel (the post succeeded). Monotone;
    /// false = the post is owed and the driver re-posts (a duplicate post is
    /// harmless — the host's ingest is idempotent per grant id).
    #[serde(default)]
    pub offer_posted: bool,
    /// The received accept envelope's canonical bytes, verbatim. Empty
    /// until the host answers. Once non-empty the ceremony has one bound
    /// serving device forever — a second, different accept is refused.
    #[serde(with = "serde_bytes", default)]
    pub accept: Vec<u8>,
    /// The interactive mint door ran to completion (Mint event recorded +
    /// blob released for deposit). Monotone.
    #[serde(default)]
    pub minted: bool,
    /// The deliver payload was posted to the channel. Monotone.
    #[serde(default)]
    pub delivered: bool,
    /// The `fauna.state.custodian-endpoints` row went through the writer
    /// door. Monotone; false = the write is still owed (e.g. no generation
    /// tip resolved yet) and the driver retries.
    #[serde(default)]
    pub endpoints_row_written: bool,
    /// The custodian's latest **verified** custody receipt, as the verbatim
    /// bytes of its signed envelope (W8.7 leg 2). Empty until the custodian
    /// first checks in — the "no receipt yet" state, which
    /// `ui/nests.md` § Trust facet renders distinctly from a stale one.
    /// Verbatim because the owner's fleet re-checks the signature; a re-encode
    /// would break it.
    #[serde(with = "serde_bytes", default)]
    pub latest_receipt: Vec<u8>,
    /// [`Self::latest_receipt`]'s `attested_at` (micros), lifted out so the
    /// monotone check and the merge tiebreak never have to open the envelope.
    /// An older receipt never displaces a newer one — a replayed attestation
    /// must not make coverage look fresher than it is.
    #[serde(default = "ts_zero")]
    pub latest_receipt_at: Timestamp,
    /// The current [`Self::latest_receipt`] reached the
    /// `fauna.state.custodian-endpoints` row. Reset by every newer receipt —
    /// unlike the monotone marks above, because the row carries the receipt,
    /// so a receipt nobody wrote through leaves the row showing the old one.
    #[serde(default)]
    pub receipt_row_written: bool,
    /// When the offer was posted (micros — the decay clock's anchor).
    #[serde(default = "ts_zero")]
    pub offered_at: Timestamp,
    /// The offer's proposed witness lifetime; also the offer's own shelf
    /// life (an unanswered offer past it decays to re-offer).
    #[serde(default)]
    pub duration_secs: u64,
    /// Last state change (micros) — the merge tiebreak.
    #[serde(default = "ts_zero")]
    pub updated_at: Timestamp,
}

impl Default for GrantedCustody {
    fn default() -> Self {
        Self {
            grant_id: Vec::new(),
            host: [0u8; 32],
            channel_hex: String::new(),
            offer: Vec::new(),
            offer_posted: false,
            accept: Vec::new(),
            minted: false,
            delivered: false,
            endpoints_row_written: false,
            latest_receipt: Vec::new(),
            latest_receipt_at: Timestamp(0),
            receipt_row_written: false,
            offered_at: Timestamp(0),
            duration_secs: 0,
            updated_at: Timestamp(0),
        }
    }
}

/// One host-side ceremony: this account was offered (and may hold) custody
/// for `owner`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldCustody {
    /// The ceremony's grant id (16 bytes) — the record key.
    #[serde(with = "serde_bytes", default)]
    pub grant_id: Vec<u8>,
    /// The owner account whose planes would be custodied.
    #[serde(default)]
    #[serde(with = "serde_bytes")]
    pub owner: [u8; 32],
    /// The conversation channel (hex `ChannelId`) the ceremony rides.
    #[serde(default)]
    pub channel_hex: String,
    /// The received offer envelope's canonical bytes, verbatim — the
    /// consent surface (T16, row 52) renders from this; the accept's
    /// digest is computed over exactly these bytes.
    #[serde(with = "serde_bytes", default)]
    pub offer: Vec<u8>,
    /// The accept envelope this account posted, verbatim. Empty until the
    /// user consents (the accept gesture is always explicit — T16).
    #[serde(with = "serde_bytes", default)]
    pub accept: Vec<u8>,
    /// The accept payload reached the channel (the post succeeded).
    /// Monotone; false with a non-empty [`Self::accept`] = the post is owed
    /// and the driver re-posts (idempotent owner-side per grant id).
    #[serde(default)]
    pub accept_posted: bool,
    /// The received deliver envelope's canonical bytes, verbatim — carries
    /// the witness + the owner-fleet snapshot; everything the
    /// `custodies-held` row needs is re-derivable from it. A superseding
    /// deliver (the owner's re-signed witness, a candidate refresh) replaces
    /// it, stamped by [`Self::deliver_at`].
    #[serde(with = "serde_bytes", default)]
    pub deliver: Vec<u8>,
    /// When this device captured [`Self::deliver`] (micros; 0 = none yet) —
    /// the deliver's own freshness clock, so the join keeps the freshest
    /// capture WITH its [`Self::held_row_written`] mark (the receipt idiom).
    /// A per-record OR of the mark could never re-open the runtime-row write
    /// a fresher deliver owes.
    #[serde(default = "ts_zero")]
    pub deliver_at: Timestamp,
    /// The runtime row for [`Self::deliver`] went through its door. False =
    /// owed, the driver retries; a fresher deliver clears it, and the join
    /// carries it with the deliver it describes.
    #[serde(default)]
    pub held_row_written: bool,
    /// The latest minted A7 receipt for this custody — the canonical bytes
    /// of the signed envelope, exactly what the channel post carries
    /// verbatim (the owner re-verifies the signature, so any re-encode on
    /// the way would present a lying custodian and an honest one
    /// identically). Empty = never minted. Overwritten at each due mint
    /// (the check-in cadence, [`crate::custody_receipt::receipt_due`]).
    #[serde(with = "serde_bytes", default)]
    pub receipt: Vec<u8>,
    /// The receipt above reached the channel (the post succeeded). Cleared
    /// at each mint; false with a non-empty [`Self::receipt`] = the post is
    /// owed and the driver (re-)posts — the [`Self::accept_posted`] idiom.
    #[serde(default)]
    pub receipt_posted: bool,
    /// When [`Self::receipt`] was minted (micros; 0 = never) — the
    /// check-in cadence anchor.
    #[serde(default = "ts_zero")]
    pub receipt_minted_at: Timestamp,
    /// Whether the minted receipt reported degraded coverage — the state
    /// whose flip prompts an off-cadence check-in.
    #[serde(default)]
    pub receipt_degraded: bool,
    /// The user dismissed this offer on the consent surface (T16). A LOCAL
    /// mark, deliberately not a wire message — the owner side already
    /// renders no-answer honestly via offer decay. Monotone: the record
    /// stays (so an idempotent re-ingest of the same offer stays Duplicate
    /// and the card stays dismissed); a fresh offer is a fresh grant id.
    #[serde(default)]
    pub declined: bool,
    /// The host RECLAIMED this custody: the hosting row —
    /// and, with the `(host, owner)` pair's last row, the custodied store — is
    /// gone. A LOCAL monotone mark on the [`Self::declined`] pattern, and for
    /// the same reason: the record must stay so an idempotent re-ingest of the
    /// same ceremony cannot resurrect a custody the host tore down, while the
    /// card stops rendering it.
    ///
    /// Distinct from `declined` deliberately — that is an offer never accepted,
    /// this is a live custody torn down — and distinct from `stopped` on the
    /// registry row, which only PAUSES the pull and keeps the bytes.
    #[serde(default)]
    pub removed: bool,
    /// The host's own runtime knobs — its Stop and its budget — once it has
    /// touched either (`None` = never: the accept's cap, running).
    ///
    /// The RECORD, not the runtime row, is their authority: the Stop/budget
    /// act writes them here BEFORE the row, and every runtime-row write reads
    /// them from here — the drive's re-derivation after a fresher deliver
    /// included. Kept only on the row, the host's pause and budget were one
    /// owner re-deliver away from being reset to the accept's values.
    #[serde(default)]
    pub host_knobs: Option<HostKnobs>,
    /// Last state change (micros) — the merge tiebreak.
    #[serde(default = "ts_zero")]
    pub updated_at: Timestamp,
}

/// The host-side runtime knobs of one held custody ([`HeldCustody::host_knobs`]).
/// Merged freshest-wins on [`Self::set_at`] ([`merge_host_knobs`]) — the
/// receipt idiom, not a monotone OR: Stop is a pause, and a budget moves both
/// ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKnobs {
    /// The host's byte budget for this custody.
    pub retained_bytes_cap: u64,
    /// The host paused the custody (Stop): the bytes stay, the pull stops.
    pub stopped: bool,
    /// When the host last set either knob (micros) — the merge tiebreak.
    pub set_at: Timestamp,
}

/// Merge two devices' [`HeldCustody::host_knobs`]: a recorded value beats
/// none, the later `set_at` beats the earlier, and an exact `set_at` tie is
/// broken on the values themselves so the merge commutes.
pub fn merge_host_knobs(ours: Option<HostKnobs>, theirs: Option<HostKnobs>) -> Option<HostKnobs> {
    match (ours, theirs) {
        (None, t) => t,
        (o, None) => o,
        (Some(o), Some(t)) => Some(
            if (t.set_at.0, t.stopped, t.retained_bytes_cap)
                > (o.set_at.0, o.stopped, o.retained_bytes_cap)
            {
                t
            } else {
                o
            },
        ),
    }
}

impl Default for HeldCustody {
    fn default() -> Self {
        Self {
            grant_id: Vec::new(),
            owner: [0u8; 32],
            channel_hex: String::new(),
            offer: Vec::new(),
            accept: Vec::new(),
            accept_posted: false,
            deliver: Vec::new(),
            deliver_at: Timestamp(0),
            held_row_written: false,
            receipt: Vec::new(),
            receipt_posted: false,
            receipt_minted_at: Timestamp(0),
            receipt_degraded: false,
            declined: false,
            removed: false,
            host_knobs: None,
            updated_at: Timestamp(0),
        }
    }
}

// ── The cross-device join (`config-dissolution.md` P1: one statement of the
// rule, called — per record — by the `fauna.state.custody-ceremony` plane
// arm) ──

/// The verbatim-envelope clause of the join: non-empty wins, byte-smaller on
/// a both-non-empty conflict (arbitrary but convergent; honest devices record
/// identical bytes for one grant id).
fn pick_bytes(ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    match (ours.is_empty(), theirs.is_empty()) {
        (true, _) => theirs.to_vec(),
        (_, true) => ours.to_vec(),
        _ if theirs < ours => theirs.to_vec(),
        _ => ours.to_vec(),
    }
}

impl CustodyConfig {
    /// The cross-device join: per side, per grant id, a union — booleans are
    /// monotone (OR), the verbatim payload envelopes are non-empty-wins
    /// ([`pick_bytes`]) — but the freshest receipt (owner side), deliver and
    /// mint (host side) each travel WITH their own written/posted mark, and
    /// the scalar
    /// remainder follows the record's OWN higher `updated_at`. Every tie is
    /// settled by the values themselves, so the join is commutative,
    /// associative and idempotent on the encoded bytes, not just on the
    /// fields a reader looks at (the plane arm's join laws). A ceremony
    /// record is the only durable copy of a consumed MLS payload, so this
    /// unions — a latest-wins would orphan one device's captured
    /// offer/accept/deliver outright. The per-record halves are
    /// [`GrantedCustody::merge`] and [`HeldCustody::merge`].
    ///
    /// The one statement of the rule: the `fauna.state.custody-ceremony` plane
    /// arm folds through it (`config-dissolution.md`, P1).
    #[must_use]
    pub fn merge(&self, other: &Self) -> Self {
        let mut granted = self.granted.clone();
        for theirs in &other.granted {
            match granted
                .iter_mut()
                .find(|ours| ours.grant_id == theirs.grant_id)
            {
                Some(ours) => *ours = ours.merge(theirs),
                None => granted.push(theirs.clone()),
            }
        }
        granted.sort_by(|a, b| a.grant_id.cmp(&b.grant_id));
        let mut held = self.held.clone();
        for theirs in &other.held {
            match held
                .iter_mut()
                .find(|ours| ours.grant_id == theirs.grant_id)
            {
                Some(ours) => *ours = ours.merge(theirs),
                None => held.push(theirs.clone()),
            }
        }
        held.sort_by(|a, b| a.grant_id.cmp(&b.grant_id));
        Self { granted, held }
    }
}

impl GrantedCustody {
    /// The per-record half of [`CustodyConfig::merge`], over two records of
    /// ONE ceremony (same grant id); the identity is `self`'s.
    #[must_use]
    pub fn merge(&self, theirs: &Self) -> Self {
        let mut ours = self.clone();
        ours.accept = pick_bytes(&ours.accept, &theirs.accept);
        ours.offer = pick_bytes(&ours.offer, &theirs.offer);
        ours.offer_posted |= theirs.offer_posted;
        ours.minted |= theirs.minted;
        ours.delivered |= theirs.delivered;
        ours.endpoints_row_written |= theirs.endpoints_row_written;
        // The receipt is NOT a monotone boolean OR: it is a freshest-wins
        // triple, tiebroken on the attestation's own timestamp rather than
        // the record's `updated_at` (two devices can capture different
        // receipts in either order). Its written-mark travels WITH the
        // receipt it describes — OR-ing it would mark a newer receipt as
        // already written through on the strength of an older one's write.
        match theirs.latest_receipt_at.cmp(&ours.latest_receipt_at) {
            std::cmp::Ordering::Greater => {
                ours.latest_receipt = theirs.latest_receipt.clone();
                ours.latest_receipt_at = theirs.latest_receipt_at;
                ours.receipt_row_written = theirs.receipt_row_written;
            }
            // Same attestation on both sides: either device having written
            // it through is enough.
            std::cmp::Ordering::Equal if theirs.latest_receipt == ours.latest_receipt => {
                ours.receipt_row_written |= theirs.receipt_row_written;
            }
            // Two different receipts at one instant (no honest custodian
            // signs two): the verbatim-envelope clause picks one, and the
            // picked receipt keeps its OWN mark — a keep-mine here would
            // leave two replicas on different bytes (the join laws).
            std::cmp::Ordering::Equal => {
                if pick_bytes(&ours.latest_receipt, &theirs.latest_receipt) == theirs.latest_receipt
                {
                    ours.latest_receipt = theirs.latest_receipt.clone();
                    ours.receipt_row_written = theirs.receipt_row_written;
                }
            }
            std::cmp::Ordering::Less => {}
        }
        // The scalar remainder: the higher `updated_at`, a tie settled by the
        // remainder itself so two replicas holding equal stamps still
        // converge on one set of bytes (the group-share record's rule).
        if (
            theirs.updated_at,
            theirs.host,
            &theirs.channel_hex,
            theirs.offered_at,
            theirs.duration_secs,
        ) > (
            ours.updated_at,
            ours.host,
            &ours.channel_hex,
            ours.offered_at,
            ours.duration_secs,
        ) {
            ours.host = theirs.host;
            ours.channel_hex = theirs.channel_hex.clone();
            ours.offered_at = theirs.offered_at;
            ours.duration_secs = theirs.duration_secs;
            ours.updated_at = theirs.updated_at;
        }
        ours
    }
}

impl HeldCustody {
    /// The per-record half of [`CustodyConfig::merge`], over two records of
    /// ONE ceremony (same grant id); the identity is `self`'s.
    #[must_use]
    pub fn merge(&self, theirs: &Self) -> Self {
        let mut ours = self.clone();
        ours.offer = pick_bytes(&ours.offer, &theirs.offer);
        ours.accept = pick_bytes(&ours.accept, &theirs.accept);
        ours.accept_posted |= theirs.accept_posted;
        // The deliver: freshest-wins on its own capture stamp, its
        // runtime-row mark travelling WITH it (the receipt idiom below) — an
        // OR of the mark would mark a fresher deliver's row written on the
        // strength of an older one's write, so a candidate refresh would
        // never reach the row.
        match theirs.deliver_at.cmp(&ours.deliver_at) {
            std::cmp::Ordering::Greater => {
                ours.deliver = theirs.deliver.clone();
                ours.deliver_at = theirs.deliver_at;
                ours.held_row_written = theirs.held_row_written;
            }
            std::cmp::Ordering::Equal if theirs.deliver == ours.deliver => {
                ours.held_row_written |= theirs.held_row_written;
            }
            std::cmp::Ordering::Equal => {
                if pick_bytes(&ours.deliver, &theirs.deliver) == theirs.deliver {
                    ours.deliver = theirs.deliver.clone();
                    ours.held_row_written = theirs.held_row_written;
                }
            }
            std::cmp::Ordering::Less => {}
        }
        // A dismissal is monotone: once any device declined the offer, the
        // card stays dismissed fleet-wide.
        ours.declined |= theirs.declined;
        // A reclaim is monotone for the same reason: a device that never saw
        // the removal must not resurrect it.
        ours.removed |= theirs.removed;
        // The host's Stop/budget: freshest-wins on its own clock (the knobs
        // move both ways, so never an OR).
        ours.host_knobs = merge_host_knobs(ours.host_knobs, theirs.host_knobs);
        // The minted receipt: the owner-side `latest_receipt` rule's host
        // twin — a freshest-wins quadruple on the mint's own timestamp, whose
        // posted-mark travels WITH its receipt (OR-ing it would mark a newer
        // mint as already posted on the strength of an older one's post,
        // stranding the fresh attestation unposted forever).
        match theirs.receipt_minted_at.cmp(&ours.receipt_minted_at) {
            std::cmp::Ordering::Greater => {
                ours.receipt = theirs.receipt.clone();
                ours.receipt_minted_at = theirs.receipt_minted_at;
                ours.receipt_posted = theirs.receipt_posted;
                ours.receipt_degraded = theirs.receipt_degraded;
            }
            // Same mint on both sides: either device having posted it is
            // enough (and a degraded verdict on either is the verdict).
            std::cmp::Ordering::Equal if theirs.receipt == ours.receipt => {
                ours.receipt_posted |= theirs.receipt_posted;
                ours.receipt_degraded |= theirs.receipt_degraded;
            }
            // Two different mints at one instant: the owner-side rule — the
            // verbatim-envelope clause picks one, which keeps its own marks.
            std::cmp::Ordering::Equal => {
                if pick_bytes(&ours.receipt, &theirs.receipt) == theirs.receipt {
                    ours.receipt = theirs.receipt.clone();
                    ours.receipt_posted = theirs.receipt_posted;
                    ours.receipt_degraded = theirs.receipt_degraded;
                }
            }
            std::cmp::Ordering::Less => {}
        }
        // The scalar remainder, tie settled by itself (as on the owner side).
        if (theirs.updated_at, theirs.owner, &theirs.channel_hex)
            > (ours.updated_at, ours.owner, &ours.channel_hex)
        {
            ours.owner = theirs.owner;
            ours.channel_hex = theirs.channel_hex.clone();
            ours.updated_at = theirs.updated_at;
        }
        ours
    }
}

#[cfg(test)]
mod hosting_bounds_tests {
    use super::{
        DEFAULT_RETAINED_BYTES_CAP, MAX_CUSTODY_HOSTING_ROWS_PER_HOST, MAX_RETAINED_BYTES_CAP,
        bound_hosting_deposit as bound,
    };

    /// PROBE-379-A's core, headless: the attacker's own number never survives.
    #[test]
    fn a_budget_over_the_ceiling_is_clamped_never_written_through() {
        for requested in [
            u64::MAX,
            MAX_RETAINED_BYTES_CAP + 1,
            MAX_RETAINED_BYTES_CAP * 1024,
        ] {
            assert_eq!(
                bound(0, false, requested),
                Ok(MAX_RETAINED_BYTES_CAP),
                "{requested} must be clamped, not honoured"
            );
        }
    }

    #[test]
    fn a_budget_at_or_under_the_ceiling_is_honoured_verbatim() {
        for requested in [0, 1, 4096, MAX_RETAINED_BYTES_CAP] {
            assert_eq!(
                bound(0, false, requested),
                Ok(requested),
                "a host narrowing its own budget must be taken at its word"
            );
        }
    }

    /// `0` is "no cap recorded" (the budget pass substitutes the ceremony
    /// default) and must not become a real budget of zero, which would evict a
    /// custody's whole payload on the strength of a missing field.
    #[test]
    fn zero_stays_zero_through_the_clamp() {
        assert_eq!(bound(0, false, 0), Ok(0));
        assert_eq!(
            MAX_RETAINED_BYTES_CAP, DEFAULT_RETAINED_BYTES_CAP,
            "the ceiling IS the accept-time default — a host narrows, never widens. If this \
             pair ever diverges, revisit the ruling in account-data-plane.md § Two-sided \
             bounds rather than just re-pinning this test"
        );
    }

    #[test]
    fn a_new_row_is_refused_at_the_row_cap_and_admitted_below_it() {
        for held in 0..MAX_CUSTODY_HOSTING_ROWS_PER_HOST {
            assert!(
                bound(held, false, 4096).is_ok(),
                "row {held} is below the cap and must be admitted"
            );
        }
        for held in [
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST,
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST + 1,
            10_000,
        ] {
            let err =
                bound(held, false, 4096).expect_err("a new row at or past the cap is refused");
            assert!(
                err.contains("row cap reached"),
                "the refusal must name the cap so a host's app can say why: {err}"
            );
        }
    }

    /// The tier is the bound, so a host with no tier row (or with enforcement
    /// off) is uncapped — the same fail-open the sync plane's metering callers
    /// take for the identical absent lookup.
    #[test]
    fn no_tier_bound_means_the_row_keeps_its_own_ceiling() {
        use super::effective_hosting_cap as eff;
        assert_eq!(eff(4096, None, 0), 4096);
        assert_eq!(
            eff(4096, None, u64::MAX),
            4096,
            "headroom is moot with no bound"
        );
        assert_eq!(
            eff(u64::MAX, None, 0),
            MAX_RETAINED_BYTES_CAP,
            "the ceiling still holds"
        );
    }

    /// `0` on the stored row means "no cap recorded" — the effective budget
    /// resolves it to the ceremony default rather than to nothing, or a row
    /// written before the cap was carried would have its payload evicted whole.
    #[test]
    fn an_unrecorded_row_cap_resolves_to_the_ceremony_default() {
        use super::effective_hosting_cap as eff;
        assert_eq!(eff(0, None, 0), DEFAULT_RETAINED_BYTES_CAP);
        assert_eq!(
            eff(0, Some(DEFAULT_RETAINED_BYTES_CAP * 4), 0),
            DEFAULT_RETAINED_BYTES_CAP,
            "a generous tier does not widen an unrecorded cap past the default"
        );
    }

    /// The squeeze, and the stated policy behind it: **first-registered keeps
    /// its space**. The pump enumerates in a stable order, so later rows absorb
    /// the shortfall.
    #[test]
    fn a_row_is_squeezed_to_the_hosts_remaining_tier_headroom() {
        use super::effective_hosting_cap as eff;
        let bound = 10_000;
        // Nothing else held: the row's own number stands.
        assert_eq!(eff(4_000, Some(bound), 0), 4_000);
        // Others hold 7k of a 10k bound: 3k left, so a 4k request is squeezed.
        assert_eq!(eff(4_000, Some(bound), 7_000), 3_000);
        // Others hold exactly the bound: this row gets nothing.
        assert_eq!(eff(4_000, Some(bound), bound), 0);
        // Others hold MORE than the bound (a tier lowered under a live hold):
        // saturating, never a wrap into a huge budget.
        assert_eq!(eff(4_000, Some(bound), bound + 5_000), 0);
    }

    /// ⚠ The regression this signature change exists for: a squeeze to zero is a
    /// REAL budget. `meter_and_evict` used to read a bare `0` as "no cap
    /// recorded" and hand such a row the 8 GiB default, so the bound failed
    /// exactly at its limit; it now takes an `Option` and the pump passes
    /// `Some(0)`.
    #[test]
    fn a_zero_squeeze_is_a_real_budget_not_an_absent_one() {
        use super::effective_hosting_cap as eff;
        assert_eq!(eff(u64::MAX, Some(1), 1), 0);
        assert_ne!(
            eff(u64::MAX, Some(1), 1),
            DEFAULT_RETAINED_BYTES_CAP,
            "a host out of headroom must not inherit the default budget"
        );
    }

    /// the pump's side of the same cap, which did not exist while three
    /// artifacts said it did. The row cap bounds outbound dial fan-out, so the
    /// backstop matters for exactly the rows the door never saw.
    #[test]
    fn the_pump_admits_the_capped_prefix_and_refuses_the_excess() {
        for seen in 0..MAX_CUSTODY_HOSTING_ROWS_PER_HOST {
            assert!(
                super::hosting_pump_admits_row(seen),
                "row {seen} is within the cap and must still be pumped"
            );
        }
        for seen in [
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST,
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST + 1,
            10_000,
        ] {
            assert!(
                !super::hosting_pump_admits_row(seen),
                "row {seen} is beyond what the door would ever have admitted — the pump must \
                 not dial it"
            );
        }
    }

    /// The two sides read ONE cap. If a later session bumps the constant for the
    /// door alone, or gives the pump its own number, this fails.
    #[test]
    fn the_door_and_the_pump_agree_on_where_the_cap_falls() {
        let last_admitted = MAX_CUSTODY_HOSTING_ROWS_PER_HOST - 1;
        assert!(bound(last_admitted, false, 4096).is_ok());
        assert!(super::hosting_pump_admits_row(last_admitted));
        assert!(bound(MAX_CUSTODY_HOSTING_ROWS_PER_HOST, false, 4096).is_err());
        assert!(!super::hosting_pump_admits_row(
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST
        ));
    }

    /// The clause that keeps the cap from becoming the unrecoverable state it
    /// exists to prevent: stop and budget-adjust are re-registers, so a host at
    /// the cap must still be able to rewrite a row it already holds.
    #[test]
    fn a_rewrite_is_admitted_even_at_the_row_cap() {
        for held in [
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST,
            MAX_CUSTODY_HOSTING_ROWS_PER_HOST + 5,
        ] {
            assert_eq!(
                bound(held, true, u64::MAX),
                Ok(MAX_RETAINED_BYTES_CAP),
                "a host at the cap can still stop or re-budget a row it holds — and the \
                 clamp still applies to that rewrite"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custody_grant::CUSTODY_GRANT_ID_LEN;

    /// The GC predicate: reclaimable strictly PAST the grace, never at
    /// or before it, and never for an unexpired witness (micros in, the
    /// ceremony's clock).
    #[test]
    fn a_store_reclaims_only_past_the_expiry_grace() {
        let grace_micros = HOSTING_EXPIRED_STORE_GC_GRACE_SECS * 1_000_000;
        let now = Timestamp(grace_micros * 10);
        // Unexpired, and expired within/at the grace: kept.
        for expires in [
            Timestamp(now.0 + 1),
            Timestamp(now.0),
            Timestamp(now.0 - 1),
            Timestamp(now.0 - grace_micros),
        ] {
            assert!(!hosting_store_reclaimable(expires, now), "{expires:?}");
        }
        // Strictly past the grace (by a whole second — the comparison is in
        // whole lapsed seconds): reclaimable.
        assert!(hosting_store_reclaimable(
            Timestamp(now.0 - grace_micros - 1_000_000),
            now
        ));
    }

    fn owner() -> ActorKeypair {
        ActorKeypair::from_secret([9u8; 32])
    }

    fn host() -> ActorKeypair {
        ActorKeypair::from_secret([10u8; 32])
    }

    fn offer(owner: &ActorKeypair, host: &ActorKeypair) -> CustodyOffer {
        CustodyOffer {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: owner.actor_id(),
            host: host.actor_id(),
            scopes: CustodyScopeSet::Account,
            duration_secs: 90 * 24 * 3600,
            owner_devices: vec![DeviceEndpoints {
                node_id: [7u8; 32],
                lan_addrs: vec!["192.168.1.7:4433".into()],
                public_addrs: Vec::new(),
                relay_url: None,
            }],
            owner_nest_url: Some("https://nest.example/".into()),
            offered_at: Timestamp(1_000),
        }
    }

    fn accept(host: &ActorKeypair, digest: [u8; 32]) -> CustodyAccept {
        CustodyAccept {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            offer_digest: digest,
            host: host.actor_id(),
            custodian_key: [0xC5; 32],
            custodian_endpoints: DeviceEndpoints {
                node_id: [0xC5; 32],
                lan_addrs: Vec::new(),
                public_addrs: vec!["198.51.100.7:4433".into()],
                relay_url: None,
            },
            retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
            narrowed_scopes: None,
            accepted_at: Timestamp(2_000),
            ..Default::default()
        }
    }

    /// **Bytes are byte strings** (`serialization.md` § Canonical IPLD
    /// dag-cbor; `config-dissolution.md` § Phases and gates → *Bounded rows*):
    /// the grant id every signed ceremony form carries — offer, accept,
    /// deliver, and the witness the deliver wraps — rides as a CBOR byte
    /// string (major 2, length 16), never serde's default integer array.
    #[test]
    fn every_signed_form_carries_the_grant_id_as_a_cbor_byte_string() {
        let (o, h) = (owner(), host());
        let off = offer(&o, &h);
        let acc = accept(&h, [0xD1; 32]);
        let witness = crate::custody_grant::CustodyGrant {
            grant_id: off.grant_id.clone(),
            owner: o.actor_id(),
            custodian_key: acc.custodian_key,
            scopes: CustodyScopeSet::Account,
            minted_at: Timestamp(3_000),
            expires_at: Timestamp(9_000),
            removed_devices: Vec::new(),
        };
        let del = CustodyDeliver {
            grant_id: off.grant_id.clone(),
            owner: o.actor_id(),
            witness: crate::custody_grant::sign_custody_grant(&o, &witness).unwrap(),
            owner_devices: Vec::new(),
            owner_nest_url: None,
        };
        let mut want = vec![0x50]; // byte string, length 16
        want.extend_from_slice(&off.grant_id);
        for (what, bytes) in [
            ("offer", crate::encoding::canonical_encode(&off).unwrap()),
            ("accept", crate::encoding::canonical_encode(&acc).unwrap()),
            ("deliver", crate::encoding::canonical_encode(&del).unwrap()),
            (
                "witness",
                crate::encoding::canonical_encode(&witness).unwrap(),
            ),
        ] {
            assert!(
                bytes.windows(want.len()).any(|w| w == want.as_slice()),
                "the {what}'s grant id must encode as a CBOR byte string"
            );
        }
    }

    #[test]
    fn the_three_payloads_round_trip_and_verify_sender_bound() {
        let (o, h) = (owner(), host());
        let off = offer(&o, &h);
        let off_env = sign_custody_offer(&o, &off).expect("sign offer");
        let got = verify_custody_offer(&off_env, &o.actor_id(), &h.actor_id()).expect("offer");
        assert_eq!(got, off);

        let digest = offer_digest(&off_env).unwrap();
        let acc = accept(&h, digest);
        let acc_env = sign_custody_accept(&h, &acc).expect("sign accept");
        let got = verify_custody_accept(&acc_env, &h.actor_id()).expect("accept");
        assert_eq!(got, acc);

        let witness = crate::custody_grant::sign_custody_grant(
            &o,
            &crate::custody_grant::CustodyGrant {
                grant_id: off.grant_id.clone(),
                owner: o.actor_id(),
                custodian_key: acc.custodian_key,
                scopes: CustodyScopeSet::Account,
                minted_at: Timestamp(3_000),
                expires_at: Timestamp(9_000),
                removed_devices: Vec::new(),
            },
        )
        .unwrap();
        let del = CustodyDeliver {
            grant_id: off.grant_id.clone(),
            owner: o.actor_id(),
            witness,
            owner_devices: off.owner_devices.clone(),
            owner_nest_url: off.owner_nest_url.clone(),
        };
        let del_env = sign_custody_deliver(&o, &del).expect("sign deliver");
        let got = verify_custody_deliver(&del_env, &o.actor_id()).expect("deliver");
        assert_eq!(got, del);
    }

    /// The lift centralized `check_grant_id` into ten call sites but
    /// carried over only the three witnesses the four hand-copies had — none
    /// of these six ceremony doors (offer/accept/deliver, sign AND verify)
    /// had a malformed-grant-id test of their own.
    /// Each door mirrors `custody_grant`'s
    /// `custody_grant_id_length_is_enforced_at_sign_and_verify`: the sign
    /// door refuses outright, and a grant id forced through the generic
    /// `sign_envelope` path proves the verify door holds its own check
    /// rather than trusting the signer (exactly the exchange where, per
    /// `account-replica-posture.md` § Replica posture → The custody grant +
    /// ceremony (T13) → *The ceremony — offer / accept / mint*, the id is
    /// chosen by whoever offers, not fixed to either side).
    #[test]
    fn ceremony_grant_id_length_is_enforced_at_every_sign_and_verify_door() {
        let (o, h) = (owner(), host());

        let mut bad_offer = offer(&o, &h);
        bad_offer.grant_id = vec![0x1D; 8];
        sign_custody_offer(&o, &bad_offer).expect_err("short id must not sign an offer");
        let (bytes, env) = sign_envelope(&o, &bad_offer).expect("raw sign");
        let err = verify_custody_offer(
            &EmbedAsBytes::from_signed(bytes, env),
            &o.actor_id(),
            &h.actor_id(),
        )
        .expect_err("short id must not verify an offer");
        assert!(err.to_string().contains("grant id"), "{err}");

        let mut bad_accept = accept(&h, [9u8; 32]);
        bad_accept.grant_id = vec![0x1D; 8];
        sign_custody_accept(&h, &bad_accept).expect_err("short id must not sign an accept");
        let (bytes, env) = sign_envelope(&h, &bad_accept).expect("raw sign");
        let err = verify_custody_accept(&EmbedAsBytes::from_signed(bytes, env), &h.actor_id())
            .expect_err("short id must not verify an accept");
        assert!(err.to_string().contains("grant id"), "{err}");

        let witness = crate::custody_grant::sign_custody_grant(
            &o,
            &crate::custody_grant::CustodyGrant {
                grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
                owner: o.actor_id(),
                custodian_key: [0xC5; 32],
                scopes: CustodyScopeSet::Account,
                minted_at: Timestamp(3_000),
                expires_at: Timestamp(9_000),
                removed_devices: Vec::new(),
            },
        )
        .unwrap();
        let bad_deliver = CustodyDeliver {
            grant_id: vec![0x1D; 8],
            owner: o.actor_id(),
            witness,
            owner_devices: Vec::new(),
            owner_nest_url: None,
        };
        sign_custody_deliver(&o, &bad_deliver).expect_err("short id must not sign a deliver");
        let (bytes, env) = sign_envelope(&o, &bad_deliver).expect("raw sign");
        let err = verify_custody_deliver(&EmbedAsBytes::from_signed(bytes, env), &o.actor_id())
            .expect_err("short id must not verify a deliver");
        assert!(err.to_string().contains("grant id"), "{err}");
    }

    /// The sender-binding rule: a valid envelope forwarded by someone other
    /// than its signer conveys nothing (differs from the Succession
    /// any-member carriage on purpose — a ceremony step is its author's act).
    #[test]
    fn a_forwarded_ceremony_payload_is_refused() {
        let (o, h) = (owner(), host());
        let third = ActorKeypair::from_secret([11u8; 32]);
        let off_env = sign_custody_offer(&o, &offer(&o, &h)).unwrap();
        verify_custody_offer(&off_env, &third.actor_id(), &h.actor_id())
            .expect_err("offer from a non-signer sender must be refused");

        let digest = offer_digest(&off_env).unwrap();
        let acc_env = sign_custody_accept(&h, &accept(&h, digest)).unwrap();
        verify_custody_accept(&acc_env, &third.actor_id())
            .expect_err("accept from a non-signer sender must be refused");
    }

    /// The addressee rule: an offer for someone else is refused by the
    /// reader whose actor it does not name.
    #[test]
    fn an_offer_addressed_elsewhere_is_refused() {
        let (o, h) = (owner(), host());
        let third = ActorKeypair::from_secret([11u8; 32]);
        let off_env = sign_custody_offer(&o, &offer(&o, &h)).unwrap();
        verify_custody_offer(&off_env, &o.actor_id(), &third.actor_id())
            .expect_err("an offer addressed to another account must be refused");
    }

    /// Tampered envelope bytes fail the CID check before any field is
    /// believed.
    #[test]
    fn tampered_ceremony_bytes_are_refused() {
        let (o, h) = (owner(), host());
        let mut env = sign_custody_offer(&o, &offer(&o, &h)).unwrap();
        let last = env.bytes.len() - 1;
        env.bytes[last] ^= 0x01;
        verify_custody_offer(&env, &o.actor_id(), &h.actor_id())
            .expect_err("tampered offer must be refused");
    }

    /// The device-binding cross-check holds at both doors: an accept whose
    /// endpoints name a different NodeId than its custodian key can be
    /// neither signed nor believed.
    #[test]
    fn an_accept_with_mismatched_device_identity_is_refused() {
        let (o, h) = (owner(), host());
        let off_env = sign_custody_offer(&o, &offer(&o, &h)).unwrap();
        let mut acc = accept(&h, offer_digest(&off_env).unwrap());
        acc.custodian_endpoints.node_id = [0xC6; 32];
        sign_custody_accept(&h, &acc).expect_err("mismatched device must not sign");
        // Force-sign through the generic path to prove the verifier holds
        // its own door.
        let (bytes, env) = sign_envelope(&h, &acc).unwrap();
        verify_custody_accept(&EmbedAsBytes::from_signed(bytes, env), &h.actor_id())
            .expect_err("mismatched device must not verify");
    }

    /// A self-addressed offer is refused at mint: same-account custodians
    /// admit by `DeviceAuthorization` and never run this ceremony.
    #[test]
    fn a_self_addressed_offer_does_not_sign() {
        let o = owner();
        let mut off = offer(&o, &host());
        off.host = o.actor_id();
        sign_custody_offer(&o, &off).expect_err("self-custody offer must not sign");
    }

    #[test]
    fn the_ceremony_message_round_trips_through_its_verbatim_bytes() {
        let (o, h) = (owner(), host());
        let off_env = sign_custody_offer(&o, &offer(&o, &h)).unwrap();
        for msg in [
            CustodyCeremonyMessage::Offer(off_env.clone()),
            CustodyCeremonyMessage::Accept(off_env.clone()),
            CustodyCeremonyMessage::Deliver(off_env),
        ] {
            let bytes = encode_ceremony_message(&msg).unwrap();
            assert_eq!(decode_ceremony_message(&bytes).unwrap(), msg);
        }
        decode_ceremony_message(b"not a ceremony message")
            .expect_err("garbage bytes must refuse, never guess");
    }

    /// A nest-anchored accept fixture: the URL present, the endpoints
    /// candidate-free (the nest-form invariants), the key standing in for
    /// the host's pinned nest actor identity.
    fn nest_accept(host: &ActorKeypair, digest: [u8; 32]) -> CustodyAccept {
        CustodyAccept {
            custodian_endpoints: DeviceEndpoints {
                node_id: [0xC5; 32],
                ..Default::default()
            },
            custodian_nest_url: Some("https://friend-nest.example/".into()),
            ..accept(host, digest)
        }
    }

    // ── the nest-custodian identity fact (ruled 2026-08-17) ─────────────────

    /// Presence probe: serde leaves an absent key at its default, so an
    /// `Option` probe distinguishes "key on the wire" from "key absent".
    #[derive(serde::Deserialize)]
    struct AcceptKeyProbe {
        #[serde(default)]
        custodian_nest_url: Option<String>,
    }

    #[test]
    fn an_absent_nest_url_stays_off_the_wire_and_round_trips_when_present() {
        let h = host();
        // A device-form accept carries no URL key.
        let acc_bytes = crate::encoding::canonical_encode(&accept(&h, [9u8; 32])).unwrap();
        let probe: AcceptKeyProbe = crate::encoding::canonical_decode(&acc_bytes).unwrap();
        assert_eq!(
            probe.custodian_nest_url, None,
            "absent URL must stay absent"
        );
        // Present: it survives a wire round trip.
        let na = nest_accept(&h, [9u8; 32]);
        let bytes = crate::encoding::canonical_encode(&na).unwrap();
        let back: CustodyAccept = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(
            back.custodian_nest_url.as_deref(),
            Some("https://friend-nest.example/")
        );
    }

    #[test]
    fn a_nest_anchored_accept_signs_and_verifies_under_the_nest_form_rules() {
        let h = host();
        let env = sign_custody_accept(&h, &nest_accept(&h, [9u8; 32])).unwrap();
        let back = verify_custody_accept(&env, &h.actor_id()).unwrap();
        assert_eq!(
            back.custodian_nest_url.as_deref(),
            Some("https://friend-nest.example/")
        );
    }

    #[test]
    fn a_nest_anchored_accept_with_dial_candidates_or_an_empty_url_is_refused() {
        let h = host();
        let digest = [9u8; 32];
        // The URL is the only anchor — candidates are the device form's.
        let with_candidates = CustodyAccept {
            custodian_endpoints: DeviceEndpoints {
                node_id: [0xC5; 32],
                public_addrs: vec!["198.51.100.7:4433".into()],
                ..Default::default()
            },
            ..nest_accept(&h, digest)
        };
        sign_custody_accept(&h, &with_candidates)
            .expect_err("nest form must refuse dial candidates at sign");
        let with_relay = CustodyAccept {
            custodian_endpoints: DeviceEndpoints {
                node_id: [0xC5; 32],
                relay_url: Some("https://relay.example/".into()),
                ..Default::default()
            },
            ..nest_accept(&h, digest)
        };
        sign_custody_accept(&h, &with_relay)
            .expect_err("nest form must refuse a relay URL at sign");
        let empty_url = CustodyAccept {
            custodian_nest_url: Some(String::new()),
            ..nest_accept(&h, digest)
        };
        sign_custody_accept(&h, &empty_url).expect_err("nest form must refuse an empty URL");
        // The verify side enforces the same rules on received bytes: a
        // malformed accept signed by a non-conforming signer still refuses.
        let env = sign_envelope(&h, &with_candidates)
            .map(|(bytes, env)| EmbedAsBytes::from_signed(bytes, env))
            .unwrap();
        verify_custody_accept(&env, &h.actor_id())
            .expect_err("nest form must refuse dial candidates at verify");
    }
}
