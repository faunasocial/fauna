//! The custody-ceremony state machine + driver (W8.4 (account-data-plane.md § Workstreams)) — offer → accept →
//! mint → deliver over an established conversation channel.
//!
//! Authority: `docs/goal/architecture/account-data-plane.md` § Replica
//! posture → *The custody grant + ceremony* (T13). The carrier-agnostic
//! payload shapes and their verification live in
//! `fauna_core::custody_ceremony`; the durable state is the account plane's
//! `fauna.state.custody-ceremony` kind, one row per ceremony side-record,
//! read and written through [`CeremonyRecords`] (record-then-act — every
//! consumed conversation payload is captured before any action, and every
//! owed action re-derives from state alone). This module owns:
//!
//! * the **pure transitions** — [`begin_offer`], [`ingest_payload`],
//!   [`build_accept`] — `&mut CustodyConfig` in, posted-payload bytes /
//!   outcomes out, no I/O (run them inside [`CeremonyRecords::update`]);
//! * the **scope algebra** — [`effective_scope_set`], the two-sided-consent
//!   intersection with the shared-audience carve-out
//!   (`fauna_protocol::scope::is_co_authored_scope` — the ONE owner the
//!   admission arm shares, decision 1 of the W8 contract);
//! * the **driver** — [`drive_ceremonies`], the idempotent owed-action
//!   executor an app (or the tier_3 rig) calls after conversation polls: it
//!   re-posts unposted payloads, runs the interactive mint door in exactly
//!   the `fauna-client-pair` record, publish, then deposit order, signs + posts the
//!   witness delivery, and writes both custody registry rows through its
//!   caller-supplied seams.
//!
//! # Crash safety
//!
//! Each ceremony step is (1) durably record intent, (2) act, (3) durably
//! mark the act done — with every mark a **monotone** boolean the
//! cross-device merge ORs. A crash between (2) and (3) re-drives the act,
//! and every act is idempotent at its receiver: a duplicate offer/accept/
//! deliver upserts by grant id, a re-deposit converges on the same nest
//! row, a registry re-put is LWW. Expiry decay is computed from state +
//! `now` at drive time ([`decayed_offers`]) — never a timer.

use fauna_client_config::CustodyCeremonyStore;
use fauna_client_config::SuccessionLedgerStore;
use fauna_core::custodian_endpoints::CustodianEndpoints;
use fauna_core::custodies_held::CustodyHeld;
use fauna_core::custody_ceremony::{
    CUSTODY_WITNESS_MINT_SKEW_SECS, CustodyAccept, CustodyCeremonyMessage, CustodyConfig,
    CustodyDeliver, CustodyOffer, GrantedCustody, HeldCustody, HostKnobs, decode_ceremony_message,
    encode_ceremony_message, offer_digest, sign_custody_accept, sign_custody_deliver,
    sign_custody_offer, verify_custody_accept, verify_custody_deliver, verify_custody_offer,
};
use fauna_core::custody_grant::{
    CUSTODY_GRANT_ID_LEN, CustodyGrant, CustodyScopeSet, canonical_removed_devices,
    sign_custody_grant, verify_custody_witness,
};
use fauna_core::custody_receipt::{CustodyReceipt, verify_custody_receipt};
use fauna_core::data::Timestamp;
use fauna_core::device_endpoints::DeviceEndpoints;
use fauna_core::encoding::EmbedAsBytes;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::succession_ledger::SuccessionLedger;
use fauna_mls::wrapped_blob::GrantWindow;
use fauna_protocol::scope::is_co_authored_scope;

use crate::custody_grants::{
    CustodyMintError, custody_event_scopes, custody_mint_blob, validate_custody_scope_set,
};
use crate::grant_log::{self, PublishedGrants};

/// The most unanswered custody offers one owner may hold a slot for on this
/// host at once (ceremony step 1's bound, `account-replica-posture.md`
/// § Replica posture → *The ceremony*). An offer is unanswered while it is
/// neither accepted, declined nor removed; it holds its slot until both its
/// own term and [`HELD_OFFER_SLOT_FLOOR_SECS`] have passed. Four mirrors the
/// offline-share ceremony's per-initiator invitation cap
/// ([`crate::group_ceremony_peer::GROUP_CEREMONY_MAX_PENDING_INVITATIONS_PER_INITIATOR`]):
/// an honest owner has one live offer to a host, a re-offer after decay a
/// second.
pub const MAX_PENDING_HELD_OFFERS_PER_OWNER: usize = 4;

/// How long an unanswered offer holds its cap slot on the host's own clock,
/// whatever term its owner chose — counted from the host's capture. Without
/// it an owner-chosen one-second term would expire every offer at once and
/// turn the cap into a revolving door; with it one owner adds at most
/// [`MAX_PENDING_HELD_OFFERS_PER_OWNER`] records a week.
pub const HELD_OFFER_SLOT_FLOOR_SECS: u64 = 7 * 24 * 60 * 60;

/// A ceremony transition was refused.
#[derive(Debug, thiserror::Error)]
pub enum CustodyCeremonyError {
    /// A payload failed decode/signature/sender/addressee verification.
    #[error("custody ceremony payload refused: {0}")]
    Payload(String),
    /// A payload named a grant id this side has no (matching) record for —
    /// an accept with no outstanding offer, a deliver with no accepted
    /// ceremony, a digest that binds to a different offer.
    #[error("custody ceremony payload does not match this side's record: {0}")]
    NoMatchingCeremony(String),
    /// The narrowed set is not a subset of the offered set (or is empty) —
    /// an accept can only ever narrow.
    #[error("custody accept widens or empties the offered scope set: {0}")]
    BadNarrowing(String),
    /// A second accept tried to re-bind an already-bound ceremony.
    #[error("custody ceremony already bound a serving device for this grant id")]
    AlreadyBound,
    /// The offer's owner already has [`MAX_PENDING_HELD_OFFERS_PER_OWNER`]
    /// unanswered offers holding a slot on this host — the new one is
    /// refused and never persisted.
    #[error(
        "custody offer refused: this owner already has {pending} unanswered offers \
         pending here (the cap is {MAX_PENDING_HELD_OFFERS_PER_OWNER})"
    )]
    TooManyPendingOffers { pending: usize },
    /// The offer's term (`offered_at + duration_secs`) has passed with no
    /// accept — the owner's side has decayed it and re-offers under a fresh
    /// grant id, so this side neither captures nor accepts it.
    #[error("custody offer has expired unanswered")]
    OfferExpired,
    /// A mint-side validation failed (scope strings, grant id length).
    #[error(transparent)]
    Mint(#[from] CustodyMintError),
    /// A signing/encoding step failed.
    #[error(transparent)]
    Core(#[from] fauna_core::error::Error),
    /// The ceremony state (`fauna.state.custody-ceremony`) could not be read
    /// or written — the store not up, a door refusal. Transient: the record
    /// stays as it was and the next drive retries.
    #[error(transparent)]
    Store(#[from] fauna_client_config::StoreError),
}

/// The two-sided-consent intersection (T13 ceremony step 2: "the effective
/// set is the intersection"), with the shared-audience carve-out binding
/// the `Account` form's coverage:
///
/// * no narrowing → the offered set;
/// * `Account` narrowed to `Account` → `Account`;
/// * `Account` narrowed to an explicit list → every entry must be a
///   canonical scope string **outside** the co-authored family
///   ([`is_co_authored_scope`]) — the `Account` form never covered a
///   co-authored scope, so "narrowing" to one would widen;
/// * an explicit list narrowed to an explicit list → subset by canonical
///   string equality;
/// * an explicit list "narrowed" to `Account` → refused (widening).
pub fn effective_scope_set(
    offered: &CustodyScopeSet,
    narrowed: Option<&CustodyScopeSet>,
) -> Result<CustodyScopeSet, CustodyCeremonyError> {
    let Some(narrowed) = narrowed else {
        // The offer stands as made — unless this side cannot read it.
        if let CustodyScopeSet::Unknown(_) = offered {
            return Err(CustodyCeremonyError::BadNarrowing(
                "a scope set of a form this version of the app does not know".into(),
            ));
        }
        return Ok(offered.clone());
    };
    let effective = match (offered, narrowed) {
        // A set a newer build minted is one this side cannot narrow or reason
        // about, so the ceremony refuses it rather than bind a grant blind.
        (CustodyScopeSet::Unknown(_), _) | (_, CustodyScopeSet::Unknown(_)) => {
            return Err(CustodyCeremonyError::BadNarrowing(
                "a scope set of a form this version of the app does not know".into(),
            ));
        }
        (CustodyScopeSet::Account, CustodyScopeSet::Account) => CustodyScopeSet::Account,
        (CustodyScopeSet::Account, CustodyScopeSet::Scopes(list)) => {
            if let Some(co) = list.iter().find(|s| is_co_authored_scope(s)) {
                return Err(CustodyCeremonyError::BadNarrowing(format!(
                    "{co:?} is co-authored — the Account form never covered it, so the \
                     narrowing would widen (co-authored scopes enter only as explicit \
                     OFFER entries)"
                )));
            }
            CustodyScopeSet::Scopes(list.clone())
        }
        (CustodyScopeSet::Scopes(_), CustodyScopeSet::Account) => {
            return Err(CustodyCeremonyError::BadNarrowing(
                "Account is wider than an explicit offer list".into(),
            ));
        }
        (CustodyScopeSet::Scopes(offered_list), CustodyScopeSet::Scopes(list)) => {
            if let Some(extra) = list.iter().find(|s| !offered_list.contains(s)) {
                return Err(CustodyCeremonyError::BadNarrowing(format!(
                    "{extra:?} was not offered"
                )));
            }
            CustodyScopeSet::Scopes(list.clone())
        }
    };
    validate_custody_scope_set(&effective)?;
    Ok(effective)
}

/// What the owner proposes — [`begin_offer`]'s input.
#[derive(Debug, Clone)]
pub struct OfferParams {
    /// The host account the offer addresses.
    pub host: ActorId,
    /// The established conversation channel (hex) the ceremony rides —
    /// the caller's pick (creating a DM is the shipped user act, not
    /// ceremony business; W8.4 design pin D8).
    pub channel_hex: String,
    /// Proposed coverage. Default mint = `Account` (T13).
    pub scopes: CustodyScopeSet,
    /// Proposed witness lifetime (seconds from mint) — also the offer's own
    /// shelf life. [`crate::DEFAULT_GRANT_WINDOW_SECS`] unless the caller
    /// has a reason.
    pub duration_secs: u64,
    /// The owner fleet's current dial candidates (discovery for the host).
    pub owner_devices: Vec<DeviceEndpoints>,
    /// The owner's nest base URL (the custodian's W8.6 anchor).
    pub owner_nest_url: Option<String>,
    /// The ceremony's grant id — 16 caller-random bytes in the
    /// capability-grant id space.
    pub grant_id: [u8; CUSTODY_GRANT_ID_LEN],
}

/// Owner side, step 1: validate + sign the offer, record the ceremony
/// (`offer_posted: false` — the driver posts), and return the channel-body
/// bytes. Refuses a grant id already on record (mint a fresh id per offer,
/// re-offers included).
pub fn begin_offer(
    cfg: &mut CustodyConfig,
    owner: &ActorKeypair,
    params: OfferParams,
    now: Timestamp,
) -> Result<Vec<u8>, CustodyCeremonyError> {
    validate_custody_scope_set(&params.scopes)?;
    // The counterparty-URL dial policy, applied where the string is BORN so a
    // well-meaning owner learns of a malformed anchor at offer time instead
    // of shipping one the custodian will refuse (the dial door re-checks —
    // `fauna_core::counterparty_url`'s module doc owns the rules).
    if let Some(url) = &params.owner_nest_url {
        fauna_core::counterparty_url::validate_counterparty_nest_url(url)
            .map_err(|reason| CustodyCeremonyError::Payload(format!("owner_nest_url: {reason}")))?;
    }
    if cfg
        .granted
        .iter()
        .any(|g| g.grant_id == params.grant_id.as_slice())
    {
        return Err(CustodyCeremonyError::NoMatchingCeremony(
            "grant id already on record — every (re-)offer mints a fresh id".into(),
        ));
    }
    let offer = CustodyOffer {
        grant_id: params.grant_id.to_vec(),
        owner: owner.actor_id(),
        host: params.host,
        scopes: params.scopes,
        duration_secs: params.duration_secs,
        owner_devices: params.owner_devices,
        owner_nest_url: params.owner_nest_url,
        offered_at: now,
    };
    let envelope = sign_custody_offer(owner, &offer)?;
    let envelope_bytes = fauna_core::encoding::canonical_encode(&envelope)?.to_vec();
    cfg.granted.push(GrantedCustody {
        grant_id: params.grant_id.to_vec(),
        host: params.host.0,
        channel_hex: params.channel_hex,
        offer: envelope_bytes.clone(),
        offered_at: now,
        duration_secs: params.duration_secs,
        updated_at: now,
        ..Default::default()
    });
    Ok(encode_ceremony_message(&CustodyCeremonyMessage::Offer(
        envelope,
    ))?)
}

/// What [`ingest_payload`] did — the driver's cue for which act is now owed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Host side: an offer is now pending the user's consent (T16 — the
    /// accept gesture is always explicit; nothing is owed until it).
    OfferPending { grant_id: Vec<u8> },
    /// Owner side: an accept bound the serving device — the mint + deliver
    /// leg is owed (the driver runs it).
    AcceptBound { grant_id: Vec<u8> },
    /// Host side: the witness arrived and verified — the `custodies-held`
    /// registry write is owed (the driver runs it).
    WitnessHeld { grant_id: Vec<u8> },
    /// The payload re-delivered something already on record — captured
    /// state unchanged (idempotent per grant id).
    Duplicate,
}

/// Ingest one received channel payload (the sink's core): decode, verify
/// (signer = MLS-authenticated `sender`; an offer's addressee = `own`),
/// bind to this side's record, and capture it. Pure — run inside the
/// config CAS update; the returned outcome tells the driver what became
/// owed.
pub fn ingest_payload(
    cfg: &mut CustodyConfig,
    own: &ActorId,
    sender: &ActorId,
    channel_hex: &str,
    bytes: &[u8],
    now: Timestamp,
) -> Result<IngestOutcome, CustodyCeremonyError> {
    let msg = decode_ceremony_message(bytes)
        .map_err(|e| CustodyCeremonyError::Payload(format!("undecodable: {e}")))?;
    match msg {
        CustodyCeremonyMessage::Offer(envelope) => {
            let offer = verify_custody_offer(&envelope, sender, own)
                .map_err(|e| CustodyCeremonyError::Payload(e.to_string()))?;
            let envelope_bytes = fauna_core::encoding::canonical_encode(&envelope)?.to_vec();
            if let Some(existing) = cfg.held.iter().find(|h| h.grant_id == offer.grant_id) {
                // Idempotent per grant id: the exact same offer re-delivered
                // is a no-op; different bytes under a known id are refused
                // (an id is single-use — a re-offer mints a fresh one).
                return if existing.offer == envelope_bytes {
                    Ok(IngestOutcome::Duplicate)
                } else {
                    Err(CustodyCeremonyError::NoMatchingCeremony(
                        "offer grant id already on record with different bytes".into(),
                    ))
                };
            }
            // The held-offer bound (ceremony step 1): an offer already past
            // its term is spent — a re-walk re-feeding old channel history
            // must not capture it — and an owner past the cap is refused.
            // Both refusals persist nothing.
            if offer_term_passed(&offer, now) {
                return Err(CustodyCeremonyError::OfferExpired);
            }
            let pending = cfg
                .held
                .iter()
                .filter(|h| h.owner == offer.owner.0 && holds_offer_slot(h, now))
                .count();
            if pending >= MAX_PENDING_HELD_OFFERS_PER_OWNER {
                return Err(CustodyCeremonyError::TooManyPendingOffers { pending });
            }
            cfg.held.push(HeldCustody {
                grant_id: offer.grant_id.clone(),
                owner: offer.owner.0,
                channel_hex: channel_hex.to_string(),
                offer: envelope_bytes.clone(),
                updated_at: now,
                ..Default::default()
            });
            Ok(IngestOutcome::OfferPending {
                grant_id: offer.grant_id,
            })
        }
        CustodyCeremonyMessage::Accept(envelope) => {
            let accept = verify_custody_accept(&envelope, sender)
                .map_err(|e| CustodyCeremonyError::Payload(e.to_string()))?;
            let envelope_bytes = fauna_core::encoding::canonical_encode(&envelope)?.to_vec();
            let record = cfg
                .granted
                .iter_mut()
                .find(|g| g.grant_id == accept.grant_id)
                .ok_or_else(|| {
                    CustodyCeremonyError::NoMatchingCeremony(
                        "accept names a grant id with no outstanding offer".into(),
                    )
                })?;
            if record.host != accept.host.0 {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "accept is signed by an account the offer never addressed".into(),
                ));
            }
            // Bind to ONE exact offer: the digest covers the recorded
            // envelope bytes verbatim.
            let recorded_offer: EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&record.offer)
                    .map_err(|e| CustodyCeremonyError::Payload(format!("recorded offer: {e}")))?;
            if offer_digest(&recorded_offer)? != accept.offer_digest {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "accept's offer digest binds to a different offer".into(),
                ));
            }
            if !record.accept.is_empty() {
                return if record.accept == envelope_bytes {
                    Ok(IngestOutcome::Duplicate)
                } else {
                    // One mint per grant id: a ceremony binds one serving
                    // device forever (W8.4 design pin D8, pinned below).
                    Err(CustodyCeremonyError::AlreadyBound)
                };
            }
            // The narrowing must validate NOW, before capture — a malformed
            // accept must not park the ceremony in an un-mintable state.
            let offer = decode_offer(&recorded_offer)?;
            effective_scope_set(&offer.scopes, accept.narrowed_scopes.as_ref())?;
            record.accept = envelope_bytes.clone();
            record.updated_at = now;
            Ok(IngestOutcome::AcceptBound {
                grant_id: accept.grant_id,
            })
        }
        CustodyCeremonyMessage::Deliver(envelope) => {
            let deliver = verify_custody_deliver(&envelope, sender)
                .map_err(|e| CustodyCeremonyError::Payload(e.to_string()))?;
            let envelope_bytes = fauna_core::encoding::canonical_encode(&envelope)?.to_vec();
            let record = cfg
                .held
                .iter_mut()
                .find(|h| h.grant_id == deliver.grant_id)
                .ok_or_else(|| {
                    CustodyCeremonyError::NoMatchingCeremony(
                        "deliver names a grant id this account never accepted".into(),
                    )
                })?;
            if record.owner != deliver.owner.0 {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "deliver is signed by an account other than the ceremony's owner".into(),
                ));
            }
            if record.accept.is_empty() {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "deliver arrived before this account accepted".into(),
                ));
            }
            // The host's terminal marks are final: a deliver — first or
            // fresher — on a declined or reclaimed ceremony is never captured,
            // so it can never re-open the runtime-row write that would re-arm
            // it (the `removed` mark exists exactly so a re-ingest cannot
            // resurrect a torn-down custody).
            if record.declined || record.removed {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "deliver for a custody this account declined or removed".into(),
                ));
            }
            // The witness must verify against the ACCEPTED custodian key and
            // the ceremony's owner — a deliver whose witness names another
            // key/account conveys nothing and is not captured.
            let accepted = decode_accept_record(record)?;
            let admission = verify_custody_witness(
                &deliver.witness,
                &accepted.custodian_key,
                &deliver.owner,
                now,
            )
            .map_err(|e| CustodyCeremonyError::Payload(format!("delivered witness: {e}")))?;
            // …and must stay inside what the host CONSENTED to: the accept's
            // effective scope set and the offer's term. Widening either is a
            // fresh offer→accept round, never a (re-)deliver.
            let offer_env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.offer)
                .map_err(|e| CustodyCeremonyError::Payload(format!("recorded offer: {e}")))?;
            let offer = decode_offer(&offer_env)?;
            let consented = effective_scope_set(&offer.scopes, accepted.narrowed_scopes.as_ref())?;
            witness_within_consent(&admission, &consented, offer.duration_secs, now)?;
            if !record.deliver.is_empty() {
                return if record.deliver == envelope_bytes {
                    Ok(IngestOutcome::Duplicate)
                } else {
                    // A superseding deliver may refresh what it carries (the
                    // owner's dial candidates, a re-signed witness after a
                    // lost post mark) but never lengthen the term already
                    // held: the honest owner re-signs under its RECORDED mint
                    // window, so only a lengthening witness reaches further.
                    let held = decode_deliver_record(record)?;
                    let held_expiry = fauna_core::custody_grant::custody_witness_expiry(
                        &held.witness,
                    )
                    .map_err(|e| CustodyCeremonyError::Payload(format!("recorded witness: {e}")))?;
                    if admission.expires_at.0 > held_expiry.0 {
                        return Err(CustodyCeremonyError::Payload(
                            "a superseding deliver may not extend the held witness's term".into(),
                        ));
                    }
                    // A fresher deliver (e.g. a candidate refresh) supersedes
                    // — freshest-wins on its capture stamp, which carries the
                    // re-opened row mark through the join with it. The
                    // runtime row it re-opens keeps the host's own knobs
                    // (`HeldCustody::host_knobs`, arm (6)).
                    record.deliver = envelope_bytes.clone();
                    record.deliver_at = fresher_than(record.deliver_at, now);
                    record.held_row_written = false;
                    record.updated_at = now;
                    Ok(IngestOutcome::WitnessHeld {
                        grant_id: deliver.grant_id,
                    })
                };
            }
            record.deliver = envelope_bytes.clone();
            record.deliver_at = fresher_than(record.deliver_at, now);
            record.updated_at = now;
            Ok(IngestOutcome::WitnessHeld {
                grant_id: deliver.grant_id,
            })
        }
    }
}

/// A capture stamp strictly after `prior` — `now`, unless a clock behind the
/// last capture's would make the fresher deliver lose the join to the one it
/// replaces.
fn fresher_than(prior: Timestamp, now: Timestamp) -> Timestamp {
    Timestamp(now.0.max(prior.0.saturating_add(1)))
}

/// Host side, the explicit consent gesture (T16): sign the accept binding
/// the serving device, record it (`accept_posted: false` — the driver
/// posts), and return the channel-body bytes.
#[allow(clippy::too_many_arguments)]
pub fn build_accept(
    cfg: &mut CustodyConfig,
    host: &ActorKeypair,
    grant_id: &[u8],
    custodian_key: [u8; 32],
    custodian_endpoints: DeviceEndpoints,
    retained_bytes_cap: u64,
    narrowed_scopes: Option<CustodyScopeSet>,
    now: Timestamp,
) -> Result<Vec<u8>, CustodyCeremonyError> {
    build_accept_inner(
        cfg,
        host,
        grant_id,
        custodian_key,
        custodian_endpoints,
        None,
        retained_bytes_cap,
        narrowed_scopes,
        now,
    )
}

/// [`build_accept`]'s NEST form — the host-side choice of the nest-custodian
/// identity fact (the device-or-nest bullet): the accept names the host's
/// PINNED nest actor identity as the bound principal and its URL as the dial
/// anchor, with zero device candidates. Two rules beyond the shared core:
///
/// * the offer must carry `owner_nest_url` — the nest pump's ONLY route is
///   the owner's nest, so a nest custody for an unreachable-by-nest owner
///   could never pull and is refused at consent, not discovered dead;
/// * the host's own `nest_url` must pass the counterparty dial policy — a
///   malformed pin fails at the gesture, where the user can see it.
#[allow(clippy::too_many_arguments)]
pub fn build_accept_nest(
    cfg: &mut CustodyConfig,
    host: &ActorKeypair,
    grant_id: &[u8],
    nest_identity: [u8; 32],
    nest_url: String,
    retained_bytes_cap: u64,
    narrowed_scopes: Option<CustodyScopeSet>,
    now: Timestamp,
) -> Result<Vec<u8>, CustodyCeremonyError> {
    fauna_core::counterparty_url::validate_counterparty_nest_url(&nest_url)
        .map_err(|reason| CustodyCeremonyError::Payload(format!("custodian_nest_url: {reason}")))?;
    let endpoints = DeviceEndpoints {
        node_id: nest_identity,
        ..Default::default()
    };
    build_accept_inner(
        cfg,
        host,
        grant_id,
        nest_identity,
        endpoints,
        Some(nest_url),
        retained_bytes_cap,
        narrowed_scopes,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_accept_inner(
    cfg: &mut CustodyConfig,
    host: &ActorKeypair,
    grant_id: &[u8],
    custodian_key: [u8; 32],
    custodian_endpoints: DeviceEndpoints,
    custodian_nest_url: Option<String>,
    retained_bytes_cap: u64,
    narrowed_scopes: Option<CustodyScopeSet>,
    now: Timestamp,
) -> Result<Vec<u8>, CustodyCeremonyError> {
    let record = cfg
        .held
        .iter_mut()
        .find(|h| h.grant_id == grant_id)
        .ok_or_else(|| {
            CustodyCeremonyError::NoMatchingCeremony("no pending offer for this grant id".into())
        })?;
    if !record.accept.is_empty() {
        return Err(CustodyCeremonyError::AlreadyBound);
    }
    let offer_env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.offer)
        .map_err(|e| CustodyCeremonyError::Payload(format!("recorded offer: {e}")))?;
    let offer = decode_offer(&offer_env)?;
    // The owner's side has decayed an offer past its term (`decayed_offers`):
    // an accept now would bind a ceremony the owner already re-offered.
    if offer_term_passed(&offer, now) {
        return Err(CustodyCeremonyError::OfferExpired);
    }
    effective_scope_set(&offer.scopes, narrowed_scopes.as_ref())?;
    // The custodian's own ingest door for the counterparty-URL dial
    // policy: an offer whose anchor this device would refuse to
    // dial is refused at acceptance, not held as a dead anchor. The dial
    // door re-checks every pass regardless (rows can be rewritten later).
    if let Some(url) = &offer.owner_nest_url {
        fauna_core::counterparty_url::validate_counterparty_nest_url(url)
            .map_err(|reason| CustodyCeremonyError::Payload(format!("owner_nest_url: {reason}")))?;
    }
    if custodian_nest_url.is_some() {
        // The nest form's reachability floor (the bullet's item 6): never
        // for an owner the nest pump could not reach.
        if offer.owner_nest_url.is_none() {
            return Err(CustodyCeremonyError::Payload(
                "the offer names no owner nest URL — a nest custodian's only pull \
                 route is the owner's nest"
                    .into(),
            ));
        }
    }
    let accept = CustodyAccept {
        grant_id: grant_id.to_vec(),
        offer_digest: offer_digest(&offer_env)?,
        host: host.actor_id(),
        custodian_key,
        custodian_endpoints,
        retained_bytes_cap,
        narrowed_scopes,
        accepted_at: now,
        custodian_nest_url,
    };
    let envelope = sign_custody_accept(host, &accept)?;
    record.accept = fauna_core::encoding::canonical_encode(&envelope)?.to_vec();
    record.updated_at = now;
    Ok(encode_ceremony_message(&CustodyCeremonyMessage::Accept(
        envelope,
    ))?)
}

/// Owner-side offers past their shelf life with no accept — T13's
/// decay-to-re-offer, computed from state + `now` (never a timer). The
/// caller re-offers with a fresh grant id; the decayed record stays as the
/// audit trail and is never reclaimed (ruled 2026-10-08,
/// `account-replica-posture.md` § Replica posture → *The custody grant +
/// ceremony*, step 1: the bounded capture rate stands on both sides).
pub fn decayed_offers(custody: &CustodyConfig, now: Timestamp) -> Vec<Vec<u8>> {
    custody
        .granted
        .iter()
        .filter(|g| {
            g.accept.is_empty()
                && now.0.saturating_sub(g.offered_at.0) > g.duration_secs.saturating_mul(1_000_000)
        })
        .map(|g| g.grant_id.clone())
        .collect()
}

/// Has the offer's term passed? The host-side mirror of [`decayed_offers`]'
/// owner-side test, to the microsecond, so both sides spend an unanswered
/// offer at the same instant.
fn offer_term_passed(offer: &CustodyOffer, now: Timestamp) -> bool {
    now.0.saturating_sub(offer.offered_at.0) > offer.duration_secs.saturating_mul(1_000_000)
}

/// Is this held record an offer still awaiting the host's answer — neither
/// accepted, declined nor removed?
fn is_unanswered(h: &HeldCustody) -> bool {
    h.accept.is_empty() && !h.declined && !h.removed
}

/// Host side: has this unanswered offer's term passed? A recorded offer this
/// build cannot decode counts as spent — it could never render or accept.
/// Answered records are never "expired" here: their lifetime is the
/// witness's, not the offer's.
pub fn held_offer_expired(h: &HeldCustody, now: Timestamp) -> bool {
    if !is_unanswered(h) {
        return false;
    }
    let Ok(env) = fauna_core::encoding::canonical_decode::<EmbedAsBytes>(&h.offer) else {
        return true;
    };
    decode_offer(&env).map_or(true, |offer| offer_term_passed(&offer, now))
}

/// Does this record hold one of its owner's
/// [`MAX_PENDING_HELD_OFFERS_PER_OWNER`] slots? While unanswered, until both
/// its term and [`HELD_OFFER_SLOT_FLOOR_SECS`] from its capture (`updated_at`,
/// the host's own clock — no answer has moved it) have passed.
fn holds_offer_slot(h: &HeldCustody, now: Timestamp) -> bool {
    is_unanswered(h)
        && (!held_offer_expired(h, now)
            || now.0.saturating_sub(h.updated_at.0)
                <= HELD_OFFER_SLOT_FLOOR_SECS.saturating_mul(1_000_000))
}

/// The consent surface's DECLINE gesture (T16): mark a pending offer
/// dismissed. A local, monotone mark — no wire message (the owner side
/// renders no-answer honestly via offer decay), and the record stays so an
/// idempotent re-ingest of the same offer stays `Duplicate` with the card
/// still dismissed. Returns `false` when the grant id names no pending
/// (un-accepted) offer — declining an accepted custody is the STOP control's
/// business, not this one's.
pub fn decline_offer(cfg: &mut CustodyConfig, grant_id: &[u8], now: Timestamp) -> bool {
    match cfg
        .held
        .iter_mut()
        .find(|h| h.grant_id == grant_id && h.accept.is_empty())
    {
        Some(h) => {
            h.declined = true;
            h.updated_at = now;
            true
        }
        None => false,
    }
}

/// Mark a held custody RECLAIMED — the terminal host-side
/// state, set once the hosting row is actually gone.
///
/// The record is kept, not deleted: an idempotent re-ingest of the same
/// ceremony must not resurrect a custody the host tore down, and
/// `held_row_written` alone would not stop the card rendering it. Only an
/// ACCEPTED custody can be removed; an un-accepted offer is `decline_offer`'s.
pub fn mark_custody_removed(cfg: &mut CustodyConfig, grant_id: &[u8], now: Timestamp) -> bool {
    match cfg
        .held
        .iter_mut()
        .find(|h| h.grant_id == grant_id && !h.accept.is_empty())
    {
        Some(h) => {
            h.removed = true;
            h.updated_at = now;
            true
        }
        None => false,
    }
}

/// Is a delivered witness inside what the host consented to? Its scopes must
/// sit within the accept's effective set (the same subset algebra an accept's
/// narrowing obeys — [`effective_scope_set`]), and its `expires_at` must not
/// reach past the offer's term measured from `now`, the host's ingest time
/// (the owner mints before the host ingests; [`CUSTODY_WITNESS_MINT_SKEW_SECS`]
/// absorbs an owner clock running ahead).
fn witness_within_consent(
    admission: &fauna_core::custody_grant::CustodyAdmission,
    consented: &CustodyScopeSet,
    duration_secs: u64,
    now: Timestamp,
) -> Result<(), CustodyCeremonyError> {
    effective_scope_set(consented, Some(&admission.scopes)).map_err(|e| {
        CustodyCeremonyError::Payload(format!(
            "delivered witness covers scopes the host never accepted: {e}"
        ))
    })?;
    let term_end = now.0.saturating_add(
        duration_secs
            .saturating_add(CUSTODY_WITNESS_MINT_SKEW_SECS)
            .saturating_mul(1_000_000),
    );
    if admission.expires_at.0 > term_end {
        return Err(CustodyCeremonyError::Payload(
            "delivered witness outlives the offered term".into(),
        ));
    }
    Ok(())
}

/// The runtime knobs a held custody's row must carry — the host's recorded
/// Stop/budget ([`HeldCustody::host_knobs`]) when it has set them, else the
/// accept's cap, running. Every runtime-row write reads this, never the
/// accept alone.
pub fn runtime_knobs(record: &HeldCustody, accept: &CustodyAccept) -> (u64, bool) {
    match record.host_knobs {
        Some(k) => (k.retained_bytes_cap, k.stopped),
        None => (accept.retained_bytes_cap, false),
    }
}

/// Record the host's Stop/budget on the ceremony record — the "record" half
/// of the knob write, run BEFORE the runtime row is rewritten so the record
/// stays the authority a later re-derivation reads. Returns `false` when no
/// held record names `grant_id`.
pub fn record_host_knobs(
    cfg: &mut CustodyConfig,
    grant_id: &[u8],
    retained_bytes_cap: u64,
    stopped: bool,
    now: Timestamp,
) -> bool {
    match cfg.held.iter_mut().find(|h| h.grant_id == grant_id) {
        Some(h) => {
            h.host_knobs = Some(HostKnobs {
                retained_bytes_cap,
                stopped,
                set_at: now,
            });
            h.updated_at = now;
            true
        }
        None => false,
    }
}

pub(crate) fn decode_offer(envelope: &EmbedAsBytes) -> Result<CustodyOffer, CustodyCeremonyError> {
    let (bytes, _env) = envelope.clone().into_signed()?;
    Ok(fauna_core::encoding::decode_signed_bytes(&bytes)?)
}

fn decode_accept(envelope: &EmbedAsBytes) -> Result<CustodyAccept, CustodyCeremonyError> {
    let (bytes, _env) = envelope.clone().into_signed()?;
    Ok(fauna_core::encoding::decode_signed_bytes(&bytes)?)
}

/// Decode a held ceremony record's ACCEPT — pub because the act layer's
/// runtime-row router (`fauna-client-custody`) branches on its form
/// (device-or-nest) exactly as the drive's arm (6) does.
pub fn decode_accept_record(record: &HeldCustody) -> Result<CustodyAccept, CustodyCeremonyError> {
    let env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.accept)
        .map_err(|e| CustodyCeremonyError::Payload(format!("recorded accept: {e}")))?;
    decode_accept(&env)
}

/// Decode a held ceremony record's DELIVER — the witness + owner-fleet
/// snapshot + owner nest URL a nest-form rewrite rebuilds its deposit from.
pub fn decode_deliver_record(record: &HeldCustody) -> Result<CustodyDeliver, CustodyCeremonyError> {
    let env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.deliver)
        .map_err(|e| CustodyCeremonyError::Payload(format!("recorded deliver: {e}")))?;
    let (bytes, _env) = env.into_signed()?;
    Ok(fauna_core::encoding::decode_signed_bytes(&bytes)?)
}

// ── Custody receipts, owner side (W8.7 leg 2) ────────────────────────────────

/// What [`ingest_receipt`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptOutcome {
    /// A newer verified receipt is now on record; the registry-row write is
    /// owed and `drive_ceremonies` runs it.
    Recorded { grant_id: Vec<u8> },
    /// The receipt verified but is not newer than the one already recorded —
    /// a re-delivery or an out-of-order arrival. Captured state unchanged.
    NotNewer,
}

/// Verify one arriving custody receipt against **this owner's own record** of
/// the custody, and record it.
///
/// The owner is the authority on who may attest for a custody it granted: the
/// custodian key lives in the accept it recorded at the ceremony, so that — not
/// anything the receipt asserts about itself — is what the signature is checked
/// against. Refused: a receipt naming a grant this account never granted, one
/// arriving on a channel other than that ceremony's, one for a ceremony no
/// device has accepted (there is no bound key to check), and one signed by any
/// key but the accept-bound device.
///
/// **Monotone in `attested_at`.** An older receipt never displaces a newer one,
/// so a re-delivery, a duplicated channel walk, or a custodian replaying an old
/// attestation cannot make coverage look fresher or thinner than it is — the
/// same replay reasoning the ceremony's per-grant-id idempotence uses.
///
/// The envelope is stored **verbatim**: a re-encode would invalidate the very
/// signature a later re-check depends on, and re-checkability is what keeps a
/// receipt a claim rather than something the owner has to take on trust.
///
/// Pure — run it inside the ceremony-record update, exactly like
/// [`ingest_payload`]. The row write is the driver's, per record-then-act.
pub fn ingest_receipt(
    cfg: &mut CustodyConfig,
    channel_hex: &str,
    bytes: &[u8],
) -> Result<ReceiptOutcome, CustodyCeremonyError> {
    ingest_receipt_carried(cfg, ReceiptCarriage::Channel(channel_hex), bytes)
}

/// [`ingest_receipt`]'s nest-door sibling — the stage-(c) carriage
/// (`account-data-plane.md` item 6: the custodian NEST deposits at the
/// owner's nest custody door; the fleet fetches the staged copy at sync).
/// Identical verification — the recorded accept's bound key is still the
/// authority, monotone, verbatim — with the channel rule replaced by its
/// carriage's own binding: the ceremony's recorded accept must be NEST-form
/// (`custodian_nest_url` present). A device-bound custody's receipts travel
/// its ceremony channel and only that channel; one arriving through the nest
/// door instead is refused exactly as a wrong-channel delivery is.
pub fn ingest_receipt_from_nest(
    cfg: &mut CustodyConfig,
    bytes: &[u8],
) -> Result<ReceiptOutcome, CustodyCeremonyError> {
    ingest_receipt_carried(cfg, ReceiptCarriage::NestDoor, bytes)
}

/// How a receipt reached this fleet — each carriage carries its own
/// ceremony-binding rule, and everything else is shared.
enum ReceiptCarriage<'a> {
    /// The ceremony's MLS conversation channel (the device-custody path).
    Channel(&'a str),
    /// Fetched from the owner's own nest's staging buffer (the nest-custody
    /// path, stage c).
    NestDoor,
}

fn ingest_receipt_carried(
    cfg: &mut CustodyConfig,
    carriage: ReceiptCarriage<'_>,
    bytes: &[u8],
) -> Result<ReceiptOutcome, CustodyCeremonyError> {
    let envelope: EmbedAsBytes = fauna_core::encoding::canonical_decode(bytes)
        .map_err(|e| CustodyCeremonyError::Payload(format!("receipt envelope: {e}")))?;
    let (signed_bytes, _sig) = envelope.clone().into_signed()?;
    let claimed: CustodyReceipt = fauna_core::encoding::decode_signed_bytes(&signed_bytes)
        .map_err(|e| CustodyCeremonyError::Payload(format!("receipt body: {e}")))?;

    let record = cfg
        .granted
        .iter_mut()
        .find(|g| g.grant_id == claimed.grant_id)
        .ok_or_else(|| {
            CustodyCeremonyError::NoMatchingCeremony(
                "receipt names a grant this account did not grant".into(),
            )
        })?;
    match carriage {
        ReceiptCarriage::Channel(channel_hex) => {
            if record.channel_hex != channel_hex {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "receipt arrived on a channel other than its ceremony's".into(),
                ));
            }
        }
        ReceiptCarriage::NestDoor => {
            let nest_form = !record.accept.is_empty()
                && fauna_core::encoding::canonical_decode::<EmbedAsBytes>(&record.accept)
                    .ok()
                    .and_then(|env| decode_accept(&env).ok())
                    .is_some_and(|a| a.custodian_nest_url.is_some());
            if !nest_form {
                return Err(CustodyCeremonyError::NoMatchingCeremony(
                    "nest-door receipt for a ceremony that did not bind a nest".into(),
                ));
            }
        }
    }
    if record.accept.is_empty() {
        return Err(CustodyCeremonyError::NoMatchingCeremony(
            "receipt for a ceremony no device has accepted — no custodian key to check".into(),
        ));
    }
    let env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.accept)
        .map_err(|e| CustodyCeremonyError::Payload(format!("recorded accept: {e}")))?;
    let accept = decode_accept(&env)?;
    // The signature check, against the OWNER's bound key.
    let receipt = verify_custody_receipt(&envelope, &accept.custodian_key)
        .map_err(|e| CustodyCeremonyError::Payload(e.to_string()))?;

    if receipt.attested_at.0 <= record.latest_receipt_at.0 && !record.latest_receipt.is_empty() {
        return Ok(ReceiptOutcome::NotNewer);
    }
    record.latest_receipt = bytes.to_vec();
    record.latest_receipt_at = receipt.attested_at;
    // A newer attestation re-opens the row write: the row carries the receipt,
    // so a receipt nobody wrote through is a row still showing the old one.
    record.receipt_row_written = false;
    Ok(ReceiptOutcome::Recorded {
        grant_id: record.grant_id.clone(),
    })
}

/// The owner-side registry row for a granted custody, receipt included — what
/// the driver writes through the R14 (account-data-plane.md § The ratified decisions) door.
fn endpoints_row_for(record: &GrantedCustody) -> Result<CustodianEndpoints, CustodyCeremonyError> {
    let accept = decode_accept_record_granted(record)?;
    Ok(CustodianEndpoints {
        grant_id: record.grant_id.clone(),
        endpoints: accept.custodian_endpoints,
        latest_receipt: (!record.latest_receipt.is_empty()).then(|| record.latest_receipt.clone()),
        custodian_nest_url: accept.custodian_nest_url,
    })
}

// ── The driver ───────────────────────────────────────────────────────────────

/// The channel post door — implemented over `ConversationsSession::
/// send_custody_payload` by the glue; `true` = the payload reached the
/// channel (the mark may be recorded). The `FolderCustodySink` bool
/// convention: failures self-log at the impl.
#[allow(async_fn_in_trait)] // static-dispatch only; per-impl Send inference
// (native Send, wasm !Send) — the RpcRequester convention.
pub trait CustodyPayloadPoster {
    async fn post(&self, channel_hex: &str, bytes: Vec<u8>) -> bool;
    /// Post one A7 receipt envelope — a separate door because the receipt is
    /// a separate channel body (`ChannelMessageBody::CustodyReceipt`, not the
    /// ceremony's), and its bytes must reach the owner **unmodified** (the
    /// owner re-verifies the signature). Implemented over
    /// `ConversationsSession::send_custody_receipt` by the glue.
    async fn post_receipt(&self, channel_hex: &str, bytes: Vec<u8>) -> bool;
}

/// The registry-row door — implemented over the account runtime's typed
/// puts (`AccountStoreHandle::{put_custodian_endpoints,put_custodies_held}`
/// in `fauna-sync-engine`, the real R14 writer door) or the tier_3 rig's
/// plane handle. `true` = the row is durably through the door; a refusal
/// (no generation tip resolves yet) answers `false` and stays owed.
#[allow(async_fn_in_trait)] // same static-dispatch / per-impl Send story
pub trait CustodyRegistryWriter {
    async fn put_custodian_endpoints(&self, value: &CustodianEndpoints) -> bool;
    async fn put_custodies_held(&self, value: &CustodyHeld) -> bool;
    /// The fleet ids this (minting) device's own **verified** fleet view
    /// excludes — `fauna_sync_engine::fleet_removal::removed_device_ids` over
    /// the same account plane these rows go to. The witness carries them as
    /// the owner's removed-device exclusion list (`CustodyGrant::
    /// removed_devices`). Never a second derivation, never a peer's say-so; a
    /// device that cannot derive the view answers empty — the list narrows a
    /// residual, it is not a precondition of the mint.
    async fn removed_device_ids(&self) -> Vec<[u8; 32]>;
}

/// The nest-deposit door — [`crate::rpc::CapabilitiesClient::mint`] behind
/// a bool (the deposit is idempotent nest-side; a refusal stays owed and
/// re-drives). Seam-shaped so the driver never names a transport error
/// type.
#[allow(async_fn_in_trait)] // same static-dispatch / per-impl Send story
pub trait CustodyDepositor {
    async fn deposit(&self, blob_bytes: Vec<u8>) -> bool;
}

/// The custody-HOSTING deposit door — the host's own nest's
/// `fauna.custody.hosting.register` behind a bool
/// ([`crate::custody_hosting::CustodyHostingClient`] in production; the
/// register is an idempotent LWW upsert nest-side, so a refusal stays owed
/// and re-drives). This is stage (b)'s runtime hand-off: a NEST-form held
/// ceremony writes its hosting row here instead of a `custodies-held` fleet
/// row — no host device serves or pulls, the nest's pump does.
#[allow(async_fn_in_trait)] // same static-dispatch / per-impl Send story
pub trait CustodyHostingDepositor {
    async fn register(&self, deposit: &crate::custody_hosting::HostingDeposit) -> bool;
}

/// One `drive_ceremonies` pass's tally — what got done, what stayed owed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DriveReport {
    /// Offer/accept/deliver payloads (re-)posted to their channels.
    pub posted: usize,
    /// Interactive mint doors run to completion (event recorded + blob
    /// released + deposit attempted).
    pub minted: usize,
    /// Registry rows written through the door (both kinds).
    pub rows_written: usize,
    /// Owed actions attempted and still owed (post/write/deposit failures —
    /// the next drive retries).
    pub still_owed: usize,
}

/// Execute every owed ceremony action, idempotently — the record-then-act
/// loop's "act" half. Call after conversation polls (each ingest outcome
/// names owed work) and at session start (crash recovery).
///
/// `config` is the caller's durable-write door: the driver computes
/// mutations against the records the caller loaded, and persists each mark
/// through it. Deliberately a plain generic over [`CeremonyRecords`] so the
/// tier_1 tests drive an in-memory store.
///
/// `ledger` is the succession-ledger seam the mint leg records its signed
/// `Mint` event through (`fauna.state.succession-ledger`'s grant log) — the
/// account-store handle in production. Before the store is up it refuses,
/// and the mint stays owed for the next drive.
#[allow(clippy::too_many_arguments)]
pub async fn drive_ceremonies<U, P, W, D, H>(
    config: &U,
    ledger: &dyn SuccessionLedgerStore,
    owner_keypair: &ActorKeypair,
    poster: &P,
    writer: &W,
    depositor: &D,
    hosting: &H,
    now: Timestamp,
) -> Result<DriveReport, CustodyCeremonyError>
where
    U: CeremonyRecords,
    P: CustodyPayloadPoster,
    W: CustodyRegistryWriter,
    D: CustodyDepositor,
    H: CustodyHostingDepositor,
{
    let mut report = DriveReport::default();
    let cfg = config.snapshot().await?;

    // ── owner side ──
    for g in cfg.granted.clone() {
        // (1) an unposted offer → post, mark.
        if !g.offer_posted {
            let offer_env: EmbedAsBytes = match fauna_core::encoding::canonical_decode(&g.offer) {
                Ok(env) => env,
                Err(_) => continue, // unreadable record — never fatal to the pass
            };
            let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Offer(offer_env))?;
            if poster.post(&g.channel_hex, bytes).await {
                config
                    .mark(CeremonySide::Granted, &g.grant_id, |r| match r {
                        CeremonyRecord::Granted(r) => r.offer_posted = true,
                        CeremonyRecord::Held(_) => {}
                    })
                    .await;
                report.posted += 1;
            } else {
                report.still_owed += 1;
            }
        }
        // (2) accept bound but not minted → the interactive mint door,
        // exactly the record, publish, then deposit order (`pair/src/lib.rs`
        // mint): the signed Mint event recorded on the ledger and
        // acknowledged by the bound nest, then the record's `minted` mark on
        // the config, then release-against-published + deposit.
        if !g.accept.is_empty() && !g.minted {
            let accept_env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&g.accept)
                .map_err(|e| CustodyCeremonyError::Payload(format!("recorded accept: {e}")))?;
            let accept = decode_accept(&accept_env)?;
            let offer_env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&g.offer)
                .map_err(|e| CustodyCeremonyError::Payload(format!("recorded offer: {e}")))?;
            let offer = decode_offer(&offer_env)?;
            let effective = effective_scope_set(&offer.scopes, accept.narrowed_scopes.as_ref())?;
            let grant_id: [u8; CUSTODY_GRANT_ID_LEN] = g
                .grant_id
                .as_slice()
                .try_into()
                .map_err(|_| CustodyMintError::BadGrantId)?;
            let now_secs = now.0 / 1_000_000;
            // Record the Mint event — or find the one an interrupted earlier
            // drive already recorded (its mark write never landed): re-signing
            // at a later `now` would log a second Mint with a different
            // window, so the recorded one is reused, window and all.
            let current = ledger.load().await.map_err(|e| {
                CustodyCeremonyError::Payload(format!("reading the grant ledger: {e}"))
            })?;
            // The recorded one still goes through the door with an empty
            // replica: the interrupted drive may have recorded it without the
            // nest ever acknowledging it.
            let mut intent = SuccessionLedger::events_replica(owner_keypair.actor_id(), Vec::new());
            let window = match recorded_window(&current, &g.grant_id) {
                Some((start, end)) => GrantWindow(start, end),
                None => {
                    let window = GrantWindow(now_secs, now_secs + offer.duration_secs);
                    grant_log::record_mint(
                        &mut intent,
                        owner_keypair.signing_key(),
                        grant_id,
                        accept.custodian_key,
                        custody_event_scopes(&effective),
                        window.0,
                        window.1,
                        now_secs,
                    )
                    .map_err(|e| {
                        CustodyCeremonyError::Payload(format!("recording the Mint event: {e}"))
                    })?;
                    window
                }
            };
            let published = match ledger.merge_published(intent).await {
                Ok(published) => published,
                // The door refused (no tip yet, the store not up, the nest
                // offline or refusing the publish): nothing the nest
                // acknowledged, so nothing may deposit — owed.
                Err(_) => {
                    report.still_owed += 1;
                    continue;
                }
            };
            config
                .mark(CeremonySide::Granted, &g.grant_id, |r| {
                    if let CeremonyRecord::Granted(r) = r {
                        r.minted = true;
                    }
                })
                .await;
            let undeposited = custody_mint_blob(
                &owner_keypair.actor_id().0,
                &grant_id,
                &accept.custodian_key,
                window,
                &effective,
            )?;
            match undeposited.release(&PublishedGrants::from_published(&published)) {
                Ok(blob_bytes) => {
                    if !depositor.deposit(blob_bytes).await {
                        // Deposit owed — the reconcile sweep and the next
                        // drive both converge on the same keyless row.
                        report.still_owed += 1;
                    }
                }
                Err(e) => {
                    return Err(CustodyCeremonyError::Payload(format!(
                        "release against the just-published mint failed: {e}"
                    )));
                }
            }
            report.minted += 1;
        }
        // Re-read the mark states this pass may have advanced.
        let g = match config
            .snapshot()
            .await?
            .granted
            .iter()
            .find(|r| r.grant_id == g.grant_id)
        {
            Some(r) => r.clone(),
            None => continue,
        };
        // (3) minted but the deliver never reached the channel → sign the
        // witness (window re-derived from the recorded Mint event — the
        // signature is fresh, the content converges) and post it.
        //
        // The removed-device exclusion list is RE-DERIVED at every sign, not
        // recorded: this is the one witness-signing site, and a re-sign after
        // a failed post reads the fleet view again. Sound because `Removed`
        // is absorbing — a re-derived list is a superset of any earlier one,
        // so a custodian holding either copy (a post that landed before its
        // mark did) unions to the same exclusions.
        if g.minted && !g.delivered {
            let accept = decode_accept_record_granted(&g)?;
            let offer_env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&g.offer)
                .map_err(|e| CustodyCeremonyError::Payload(format!("recorded offer: {e}")))?;
            let offer = decode_offer(&offer_env)?;
            let stored = ledger.load().await.map_err(|e| {
                CustodyCeremonyError::Payload(format!("reading the grant ledger: {e}"))
            })?;
            let (window_start, window_end) =
                recorded_window(&stored, &g.grant_id).ok_or_else(|| {
                    CustodyCeremonyError::NoMatchingCeremony(
                        "minted ceremony has no recorded Mint event".into(),
                    )
                })?;
            let effective = effective_scope_set(&offer.scopes, accept.narrowed_scopes.as_ref())?;
            let removed_devices = canonical_removed_devices(writer.removed_device_ids().await);
            let witness = sign_custody_grant(
                owner_keypair,
                &CustodyGrant {
                    grant_id: g.grant_id.clone(),
                    owner: owner_keypair.actor_id(),
                    custodian_key: accept.custodian_key,
                    scopes: effective,
                    minted_at: Timestamp(window_start * 1_000_000),
                    expires_at: Timestamp(window_end * 1_000_000),
                    removed_devices,
                },
            )?;
            let deliver = CustodyDeliver {
                grant_id: g.grant_id.clone(),
                owner: owner_keypair.actor_id(),
                witness,
                owner_devices: offer.owner_devices.clone(),
                owner_nest_url: offer.owner_nest_url.clone(),
            };
            let deliver_env = sign_custody_deliver(owner_keypair, &deliver)?;
            let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Deliver(deliver_env))?;
            if poster.post(&g.channel_hex, bytes).await {
                config
                    .mark(CeremonySide::Granted, &g.grant_id, |r| {
                        if let CeremonyRecord::Granted(r) = r {
                            r.delivered = true;
                        }
                    })
                    .await;
                report.posted += 1;
            } else {
                report.still_owed += 1;
            }
        }
        // (4) the owner-side registry row (`custodian-endpoints`) — owed
        // from the moment the accept bound the device; independent of the
        // deliver post.
        //
        // A newly-verified receipt re-opens this same write (W8.7 leg 2): the
        // row CARRIES the receipt, so "row written" is only true of the receipt
        // it was written with. One door, not two — the row is whole-record LWW,
        // and a second writer for the same row would race itself.
        let receipt_owed = !g.latest_receipt.is_empty() && !g.receipt_row_written;
        if !g.accept.is_empty() && (!g.endpoints_row_written || receipt_owed) {
            let value = endpoints_row_for(&g)?;
            if writer.put_custodian_endpoints(&value).await {
                config
                    .mark(CeremonySide::Granted, &g.grant_id, |r| {
                        if let CeremonyRecord::Granted(r) = r {
                            r.endpoints_row_written = true;
                            // Marks the receipt that was ON the record when this
                            // pass read it; a newer one arriving mid-pass clears
                            // the flag again on its own ingest, so the next pass
                            // re-writes rather than losing it.
                            r.receipt_row_written = !r.latest_receipt.is_empty();
                        }
                    })
                    .await;
                report.rows_written += 1;
            } else {
                report.still_owed += 1;
            }
        }
    }

    // ── host side ──
    for h in config.snapshot().await?.held.clone() {
        // (5) an accepted-but-unposted accept → post, mark.
        if !h.accept.is_empty() && !h.accept_posted {
            let accept_env: EmbedAsBytes = match fauna_core::encoding::canonical_decode(&h.accept) {
                Ok(env) => env,
                Err(_) => continue,
            };
            let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Accept(accept_env))?;
            if poster.post(&h.channel_hex, bytes).await {
                config
                    .mark(CeremonySide::Held, &h.grant_id, |r| {
                        if let CeremonyRecord::Held(r) = r {
                            r.accept_posted = true;
                        }
                    })
                    .await;
                report.posted += 1;
            } else {
                report.still_owed += 1;
            }
        }
        // (6) a held witness whose runtime row is owed → write it from the
        // recorded deliver + accept (everything re-derivable). The accept's
        // form picks the row's HOME (the device-or-nest bullet, item 6): a
        // device-form accept writes the `custodies-held` fleet row that arms
        // THIS machine's serve/pull legs; a NEST-form accept instead deposits
        // the custody-hosting row on the host's OWN nest — no host device
        // serves or pulls, the nest's pump does — and the same
        // `held_row_written` mark means "the runtime row is through its
        // door", whichever door that is. This deposit doubles as the
        // reconcile the hosting table's Burn-on-succession ruling relies on:
        // the mark lives in the client-sealed `fauna.state.custody-ceremony` row, so a successor (or a
        // nest that lost the row) re-earns it from the ceremony record.
        //
        // The row's knobs come from the RECORD (`runtime_knobs`): the host's
        // own Stop/budget once set, the accept's cap otherwise — so a fresher
        // deliver that re-opens this write never un-stops or re-budgets the
        // custody. A declined or reclaimed record is never re-armed, whatever
        // a merge left in its row mark.
        if !h.deliver.is_empty() && !h.held_row_written && !h.declined && !h.removed {
            let deliver_env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&h.deliver)
                .map_err(|e| CustodyCeremonyError::Payload(format!("recorded deliver: {e}")))?;
            let (deliver_bytes, _env) = deliver_env.clone().into_signed()?;
            let deliver: CustodyDeliver =
                fauna_core::encoding::decode_signed_bytes(&deliver_bytes)?;
            let accept = decode_accept_record(&h)?;
            let (retained_bytes_cap, stopped) = runtime_knobs(&h, &accept);
            let through_the_door = if accept.custodian_nest_url.is_some() {
                match &deliver.owner_nest_url {
                    Some(owner_url) => {
                        let deposit = crate::custody_hosting::HostingDeposit {
                            grant_id: h.grant_id.clone(),
                            owner: h.owner,
                            witness: fauna_core::encoding::canonical_encode(&deliver.witness)?
                                .to_vec(),
                            owner_nest_url: owner_url.clone(),
                            owner_devices: fauna_core::encoding::canonical_encode(
                                &deliver.owner_devices,
                            )?
                            .to_vec(),
                            retained_bytes_cap,
                            stopped,
                        };
                        hosting.register(&deposit).await
                    }
                    // `build_accept_nest` refuses an offer with no owner
                    // URL, so a deliver without one is a dropped field —
                    // stays owed; a re-deliver re-opens the write.
                    None => false,
                }
            } else {
                let value = CustodyHeld {
                    grant_id: h.grant_id.clone(),
                    owner: h.owner,
                    witness: fauna_core::encoding::canonical_encode(&deliver.witness)?.to_vec(),
                    owner_devices: deliver.owner_devices.clone(),
                    owner_nest_url: deliver.owner_nest_url.clone(),
                    retained_bytes_cap,
                    stopped,
                };
                writer.put_custodies_held(&value).await
            };
            if through_the_door {
                config
                    .mark(CeremonySide::Held, &h.grant_id, |r| {
                        if let CeremonyRecord::Held(r) = r {
                            r.held_row_written = true;
                        }
                    })
                    .await;
                report.rows_written += 1;
            } else {
                report.still_owed += 1;
            }
        }
        // (7) a minted-but-unposted A7 receipt → post, mark (W8.7's check-in
        // cadence: the pump mints on the budget pass — `receipt_due` — and
        // this arm is the "act" half; the `accept_posted` idiom exactly).
        // The recorded bytes ARE the wire bytes: the owner re-verifies the
        // signature, so they travel verbatim through their own body door.
        if !h.receipt.is_empty() && !h.receipt_posted {
            if poster.post_receipt(&h.channel_hex, h.receipt.clone()).await {
                config
                    .mark(CeremonySide::Held, &h.grant_id, |r| {
                        if let CeremonyRecord::Held(r) = r {
                            r.receipt_posted = true;
                        }
                    })
                    .await;
                report.posted += 1;
            } else {
                report.still_owed += 1;
            }
        }
    }
    Ok(report)
}

pub(crate) fn decode_accept_record_granted(
    record: &GrantedCustody,
) -> Result<CustodyAccept, CustodyCeremonyError> {
    let env: EmbedAsBytes = fauna_core::encoding::canonical_decode(&record.accept)
        .map_err(|e| CustodyCeremonyError::Payload(format!("recorded accept: {e}")))?;
    decode_accept(&env)
}

/// The device key this held ceremony's accept bound — the ONLY key whose
/// receipt the owner will verify, so the custodian's mint checks itself
/// against it first (a fleet sibling that pulls but is not the bound device
/// must not attest: its receipt would be refused owner-side, and its meter
/// describes its own store, not the bound custodian's). `None` = no accept
/// recorded yet, or the recorded bytes are unreadable — either way, not
/// mintable.
pub fn accept_bound_custodian_key(record: &HeldCustody) -> Option<[u8; 32]> {
    if record.accept.is_empty() {
        return None;
    }
    decode_accept_record(record).ok().map(|a| a.custodian_key)
}

/// Record a freshly minted A7 receipt on its held ceremony record — the
/// "record" half of the check-in's record-then-act split (the pump mints on
/// the budget pass; [`drive_ceremonies`]' receipt arm posts). Overwrites the
/// previous mint and re-opens the post mark; the config merge keeps the
/// freshest mint with its own posted-mark, so two devices racing here
/// converge on the newer attestation still owed its post.
pub async fn record_minted_receipt<U: CeremonyRecords>(
    config: &U,
    grant_id: &[u8],
    receipt_bytes: Vec<u8>,
    minted_at: Timestamp,
    degraded: bool,
) {
    config
        .mark(CeremonySide::Held, grant_id, |r| {
            if let CeremonyRecord::Held(r) = r {
                r.receipt = receipt_bytes;
                r.receipt_posted = false;
                r.receipt_minted_at = minted_at;
                r.receipt_degraded = degraded;
            }
        })
        .await;
}

/// The `(window_start, window_end)` of a grant's recorded, unrevoked Mint —
/// what a re-driven witness re-signs under so the content converges.
fn recorded_window(ledger: &SuccessionLedger, grant_id: &[u8]) -> Option<(u64, u64)> {
    grant_log::current_grants(ledger)
        .into_iter()
        .find(|g| g.grant_id == grant_id)
        .map(|g| (g.window_start, g.window_end))
}

/// One ceremony record, either side — [`CeremonyRecords::mark`]'s view.
pub enum CeremonyRecord<'a> {
    Granted(&'a mut GrantedCustody),
    Held(&'a mut HeldCustody),
}

/// Which side of the ceremony a mark targets.
///
/// **A grant id alone does not identify a record.** The id is chosen by
/// whoever offers, so the same 16 bytes can legitimately sit on BOTH sides of
/// one account's `CustodyConfig`: a peer we offered custody to knows the id we
/// minted, and nothing stops it echoing that id back in a counter-offer (the
/// offer ingest checks `held` for collisions, [`begin_offer`] checks
/// `granted` — neither sees across). Resolving a by-id lookup one way and
/// stopping leaves the other side's marks landing on the wrong variant, where
/// the driver's `if let CeremonyRecord::Held(..)` bodies silently do nothing —
/// so the owed act is never marked done and re-fires on every drive pass,
/// forever. The caller always knows which side it is driving; making it say so
/// removes the ambiguity instead of guessing at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeremonySide {
    /// This account is the OWNER — `custody.granted`.
    Granted,
    /// This account is the HOST — `custody.held`.
    Held,
}

/// The ceremony's durable-state seam — `fauna.state.custody-ceremony`, read
/// as the fold of its per-record rows and written as a join: a read snapshot,
/// a read-modify-join update, and a per-record mark. The mint leg's signed
/// event is the succession ledger's (the driver's `ledger` seam), recorded
/// before the mark — the record, publish, then deposit order demands the event
/// be durably stored, and acknowledged by the bound nest, before the blob
/// releases.
///
/// Every [`CustodyCeremonyStore`] is one (the account-store handle, the
/// resolving and no-store stand-ins, the account port's forwarder, the
/// shared fake); the pump implements it directly over the store it already
/// holds (`fauna_sync_engine::custody_leg`), whose futures cannot be `Send`.
/// Hence an AFIT trait, not the seam itself.
///
/// **A write is a join, never a replace** (`CustodyConfig::merge`): a mark
/// that is monotone lands, a stale replica drops nothing, and the only
/// resets the join admits are the ones that ride a freshness stamp — a newer
/// receipt, deliver or mint carrying its own cleared mark.
#[allow(async_fn_in_trait)] // same static-dispatch / per-impl Send story
pub trait CeremonyRecords {
    /// The current state (fresh enough for owed-action decisions). An `Err`
    /// (a store not up, a row that does not decode) aborts the drive pass —
    /// nothing is lost, the next drive retries from durable state.
    async fn snapshot(&self) -> Result<CustodyConfig, CustodyCeremonyError>;

    /// Read, run `f` on the state, join the result back — the home of every
    /// pure transition ([`begin_offer`], [`ingest_payload`], …). `f`'s own
    /// answer comes back beside the write; a refused write is an `Err` and
    /// the transition is not recorded.
    async fn update<T>(
        &self,
        f: impl FnOnce(&mut CustodyConfig) -> T + Send,
    ) -> Result<T, CustodyCeremonyError>;

    /// Apply a mark to the `side` record with `grant_id` and persist.
    /// Best-effort: a persist failure leaves the act owed — re-driving it is
    /// harmless by design. The side is explicit because the same id can sit
    /// on both (see [`CeremonySide`]).
    async fn mark(
        &self,
        side: CeremonySide,
        grant_id: &[u8],
        f: impl FnOnce(CeremonyRecord<'_>) + Send,
    ) {
        let _ = self.update(|c| apply_mark(c, side, grant_id, f)).await;
    }
}

impl<S: CustodyCeremonyStore + ?Sized> CeremonyRecords for S {
    async fn snapshot(&self) -> Result<CustodyConfig, CustodyCeremonyError> {
        Ok(self.custody().await?)
    }

    async fn update<T>(
        &self,
        f: impl FnOnce(&mut CustodyConfig) -> T + Send,
    ) -> Result<T, CustodyCeremonyError> {
        let mut state = self.custody().await?;
        let out = f(&mut state);
        self.merge_custody(state).await?;
        Ok(out)
    }
}

/// Apply a mark to the `side` record carrying `grant_id` — the shared body
/// of [`CeremonyRecords::mark`]. The side is explicit because an id can sit
/// on both ([`CeremonySide`] carries the full reasoning).
pub fn apply_mark(
    cfg: &mut CustodyConfig,
    side: CeremonySide,
    grant_id: &[u8],
    f: impl FnOnce(CeremonyRecord<'_>),
) {
    match side {
        CeremonySide::Granted => {
            if let Some(r) = cfg.granted.iter_mut().find(|r| r.grant_id == grant_id) {
                f(CeremonyRecord::Granted(r));
            }
        }
        CeremonySide::Held => {
            if let Some(r) = cfg.held.iter_mut().find(|r| r.grant_id == grant_id) {
                f(CeremonyRecord::Held(r));
            }
        }
    }
}

// ── Production impls of the driver's seams ───────────────────────────────────

/// The nest-deposit seam over the real capabilities client: a deposit is
/// `fauna.capabilities.mint` with the released keyless blob — idempotent
/// nest-side (same owner + grant id converge on one row), so `false` simply
/// stays owed and re-drives.
impl<R> CustodyDepositor for crate::rpc::CapabilitiesClient<R>
where
    R: fauna_protocol::RpcRequester + Sync,
{
    async fn deposit(&self, blob_bytes: Vec<u8>) -> bool {
        self.mint(blob_bytes).await.is_ok()
    }
}

/// The hosting-deposit seam over the real hosting client: a register is
/// `fauna.custody.hosting.register` on the host's OWN nest — an idempotent
/// LWW upsert nest-side, so `false` simply stays owed and re-drives.
impl<R> CustodyHostingDepositor for crate::custody_hosting::CustodyHostingClient<R>
where
    R: fauna_protocol::RpcRequester + Sync,
{
    async fn register(&self, deposit: &crate::custody_hosting::HostingDeposit) -> bool {
        matches!(
            crate::custody_hosting::CustodyHostingClient::register(self, deposit).await,
            Ok(reply) if reply.ok
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custody_grants::{custody_set_from_scopes, is_custody_grant};
    use fauna_client_testkit::block_on;
    use fauna_core::custody_ceremony::DEFAULT_RETAINED_BYTES_CAP;
    use fauna_core::identity::ActorId;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

    fn owner_kp() -> ActorKeypair {
        ActorKeypair::from_secret([9u8; 32])
    }
    fn host_kp() -> ActorKeypair {
        ActorKeypair::from_secret([10u8; 32])
    }
    fn conv_scope() -> String {
        format!("content:conv:{}", "2b".repeat(32))
    }

    /// One account's in-memory ceremony state — the shared fake of the
    /// `fauna.state.custody-ceremony` door, which JOINS every write exactly as
    /// the handle does — beside its in-memory succession ledger, which the
    /// drive's mint leg records its Mint event on.
    struct MemConfig(
        fauna_client_config::test_helpers::FakeCustodyCeremonyStore,
        fauna_client_config::test_helpers::FakeSuccessionLedgerStore,
    );

    impl MemConfig {
        fn new(actor: ActorId) -> Self {
            Self(
                fauna_client_config::test_helpers::FakeCustodyCeremonyStore::empty(),
                fauna_client_config::test_helpers::FakeSuccessionLedgerStore::empty(actor),
            )
        }
        /// The account's grant log, as the drive's `ledger` seam.
        fn ledger(&self) -> &fauna_client_config::test_helpers::FakeSuccessionLedgerStore {
            &self.1
        }
        /// Run a transition the way production does: read, apply, join back.
        fn with<T>(&self, f: impl FnOnce(&mut CustodyConfig) -> T) -> T {
            let mut state = self.0.current();
            let out = f(&mut state);
            block_on(self.0.merge_custody(state)).expect("the fake door accepts");
            out
        }
        /// Overwrite the stored state, bypassing the join — a state another
        /// device's write could leave behind.
        fn raw<T>(&self, f: impl FnOnce(&mut CustodyConfig) -> T) -> T {
            self.0.mutate(f)
        }
    }

    #[async_trait::async_trait]
    impl CustodyCeremonyStore for MemConfig {
        async fn custody(&self) -> Result<CustodyConfig, fauna_client_config::StoreError> {
            self.0.custody().await
        }
        async fn merge_custody(
            &self,
            replica: CustodyConfig,
        ) -> Result<CustodyConfig, fauna_client_config::StoreError> {
            self.0.merge_custody(replica).await
        }
    }

    /// A poster that records what it was asked to post; `up` = false fails
    /// every post (the crash-window half of the re-drive pins).
    #[derive(Default)]
    struct MemPoster {
        posted: Mutex<Vec<(String, Vec<u8>)>>,
        receipts: Mutex<Vec<(String, Vec<u8>)>>,
        down: AtomicBool,
    }
    impl CustodyPayloadPoster for MemPoster {
        async fn post(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
            if self.down.load(Relaxed) {
                return false;
            }
            self.posted
                .lock()
                .unwrap()
                .push((channel_hex.to_string(), bytes));
            true
        }
        async fn post_receipt(&self, channel_hex: &str, bytes: Vec<u8>) -> bool {
            if self.down.load(Relaxed) {
                return false;
            }
            self.receipts
                .lock()
                .unwrap()
                .push((channel_hex.to_string(), bytes));
            true
        }
    }

    #[derive(Default)]
    struct MemWriter {
        endpoints: Mutex<Vec<CustodianEndpoints>>,
        held: Mutex<Vec<CustodyHeld>>,
        down: AtomicBool,
        /// What the minting device's fleet view answers — deliberately
        /// uncanonical in the tests that set it (the mint normalizes).
        removed: Vec<[u8; 32]>,
    }
    impl CustodyRegistryWriter for MemWriter {
        async fn removed_device_ids(&self) -> Vec<[u8; 32]> {
            self.removed.clone()
        }
        async fn put_custodian_endpoints(&self, value: &CustodianEndpoints) -> bool {
            if self.down.load(Relaxed) {
                return false;
            }
            self.endpoints.lock().unwrap().push(value.clone());
            true
        }
        async fn put_custodies_held(&self, value: &CustodyHeld) -> bool {
            if self.down.load(Relaxed) {
                return false;
            }
            self.held.lock().unwrap().push(value.clone());
            true
        }
    }

    #[derive(Default)]
    struct MemDepositor {
        blobs: Mutex<Vec<Vec<u8>>>,
    }
    impl CustodyDepositor for MemDepositor {
        async fn deposit(&self, blob_bytes: Vec<u8>) -> bool {
            self.blobs.lock().unwrap().push(blob_bytes);
            true
        }
    }

    #[derive(Default)]
    struct MemHosting {
        deposits: Mutex<Vec<crate::custody_hosting::HostingDeposit>>,
    }
    impl CustodyHostingDepositor for MemHosting {
        async fn register(&self, deposit: &crate::custody_hosting::HostingDeposit) -> bool {
            self.deposits.lock().unwrap().push(deposit.clone());
            true
        }
    }

    fn offer_params(host: ActorId, scopes: CustodyScopeSet) -> OfferParams {
        OfferParams {
            host,
            channel_hex: "aa".repeat(32),
            scopes,
            duration_secs: crate::DEFAULT_GRANT_WINDOW_SECS,
            owner_devices: vec![DeviceEndpoints {
                node_id: [7u8; 32],
                lan_addrs: vec!["192.168.1.7:4433".into()],
                public_addrs: Vec::new(),
                relay_url: None,
            }],
            owner_nest_url: Some("https://nest.example/".into()),
            grant_id: [0x1D; CUSTODY_GRANT_ID_LEN],
        }
    }

    /// **The record-level skip for `CustodyCeremonyMessage`, pinned**
    /// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
    /// full*: the enum is ruled skip). A ceremony step a LATER build sends
    /// fails only that one payload: refused as undecodable, the captured
    /// state untouched — so nothing records it as handled, the payload waits
    /// in the channel history (the per-launch 0-seeded re-walks re-feed it)
    /// for a build that can read it, and the ceremony's next ordinary step
    /// still ingests.
    #[test]
    fn an_unknown_ceremony_step_fails_only_that_payload() {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let now = Timestamp(1_000_000_000);
        let owner_cfg = MemConfig::new(o_kp.actor_id());
        let offer_bytes = owner_cfg
            .with(|c| {
                begin_offer(
                    c,
                    &o_kp,
                    offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
                    now,
                )
            })
            .expect("begin the offer");

        // A later build's step enum: the same externally tagged wire shape,
        // with a step this build has never heard of.
        #[derive(serde::Serialize)]
        enum LaterStep {
            Rescind(u64),
        }
        let later = fauna_core::encoding::canonical_encode(&LaterStep::Rescind(7))
            .unwrap()
            .to_vec();

        let host_cfg = MemConfig::new(h_kp.actor_id());
        let channel_hex = "aa".repeat(32);
        for _ in 0..2 {
            let before = host_cfg.0.current();
            let err = host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &channel_hex,
                        &later,
                        now,
                    )
                })
                .expect_err("an unknown step is refused, never guessed at");
            assert!(
                matches!(err, CustodyCeremonyError::Payload(ref m) if m.contains("undecodable")),
                "{err}"
            );
            assert_eq!(
                host_cfg.0.current(),
                before,
                "a refused step captures nothing — every re-feed meets it afresh"
            );
        }
        let outcome = host_cfg
            .with(|c| {
                ingest_payload(
                    c,
                    &h_kp.actor_id(),
                    &o_kp.actor_id(),
                    &channel_hex,
                    &offer_bytes,
                    now,
                )
            })
            .expect("the ceremony's ordinary next step still ingests");
        assert!(matches!(outcome, IngestOutcome::OfferPending { .. }));
    }

    /// both ceremony doors: a well-meaning owner cannot BEGIN
    /// an offer whose anchor violates the counterparty-URL dial policy, and —
    /// the adversarial path — a host cannot ACCEPT a crafted, validly-signed
    /// offer carrying one (a malicious owner never calls our `begin_offer`,
    /// so the accept door must hold alone; the dial loop re-checks besides).
    #[test]
    fn both_ceremony_doors_hold_the_counterparty_url_policy() {
        block_on(async {
            let (o_kp, h_kp) = (owner_kp(), host_kp());
            let now = Timestamp(1_000_000_000);

            // Owner door: begin_offer refuses the malformed anchor outright.
            let owner_cfg = MemConfig::new(o_kp.actor_id());
            let mut params = offer_params(h_kp.actor_id(), CustodyScopeSet::Account);
            params.owner_nest_url = Some("http://192.168.1.1".into());
            let err = owner_cfg
                .with(|c| begin_offer(c, &o_kp, params, now))
                .expect_err("a policy-violating anchor never leaves the owner");
            assert!(err.to_string().contains("owner_nest_url"), "{err}");

            // Host door: a CRAFTED offer (signed fine, hostile anchor) is
            // ingested as pending — and refused at acceptance.
            let host_cfg = MemConfig::new(h_kp.actor_id());
            let offer = CustodyOffer {
                grant_id: vec![0x2E; CUSTODY_GRANT_ID_LEN],
                owner: o_kp.actor_id(),
                host: h_kp.actor_id(),
                scopes: CustodyScopeSet::Account,
                duration_secs: crate::DEFAULT_GRANT_WINDOW_SECS,
                owner_devices: Vec::new(),
                owner_nest_url: Some("https://nest.example.com/../.well-known".into()),
                offered_at: now,
            };
            let envelope = sign_custody_offer(&o_kp, &offer).expect("sign");
            let bytes =
                encode_ceremony_message(&CustodyCeremonyMessage::Offer(envelope)).expect("encode");
            let outcome = host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &"aa".repeat(32),
                        &bytes,
                        now,
                    )
                })
                .expect("the offer itself ingests (verdict is the accept's)");
            assert!(matches!(outcome, IngestOutcome::OfferPending { .. }));
            let err = host_cfg
                .with(|c| {
                    build_accept(
                        c,
                        &h_kp,
                        &[0x2E; CUSTODY_GRANT_ID_LEN],
                        [0xC5; 32],
                        DeviceEndpoints {
                            node_id: [8u8; 32],
                            lan_addrs: Vec::new(),
                            public_addrs: Vec::new(),
                            relay_url: None,
                        },
                        1024,
                        None,
                        now,
                    )
                })
                .expect_err("a hostile anchor is refused at acceptance");
            assert!(err.to_string().contains("owner_nest_url"), "{err}");
        });
    }

    /// The whole ceremony, both sides, in memory: offer → ingest → accept →
    /// ingest → drive (owner: mint + deliver + endpoints row) → ingest
    /// deliver → drive (host: held row). Pins the intersection, the carried
    /// budget, the custody-shaped Mint event, and both registry values.
    #[test]
    fn the_full_ceremony_completes_and_carries_budget_and_intersection() {
        block_on(async {
            let (o_kp, h_kp) = (owner_kp(), host_kp());
            let owner_cfg = MemConfig::new(o_kp.actor_id());
            let host_cfg = MemConfig::new(h_kp.actor_id());
            let now = Timestamp(1_000_000_000);
            let custodian_key = [0xC5u8; 32];

            // Owner offers the Account form.
            let offer_bytes = owner_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &o_kp,
                        offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
                        now,
                    )
                })
                .expect("offer");

            // Host ingests the offer (sender = owner), then consents with a
            // narrowed set + an explicit budget.
            let outcome = host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &"aa".repeat(32),
                        &offer_bytes,
                        now,
                    )
                })
                .expect("ingest offer");
            assert!(matches!(outcome, IngestOutcome::OfferPending { .. }));
            let accept_bytes = host_cfg
                .with(|c| {
                    build_accept(
                        c,
                        &h_kp,
                        &[0x1D; 16],
                        custodian_key,
                        DeviceEndpoints {
                            node_id: custodian_key,
                            lan_addrs: Vec::new(),
                            public_addrs: vec!["198.51.100.7:4433".into()],
                            relay_url: None,
                        },
                        DEFAULT_RETAINED_BYTES_CAP / 2,
                        Some(CustodyScopeSet::Scopes(vec!["state".into()])),
                        now,
                    )
                })
                .expect("accept");

            // Owner ingests the accept — the mint leg becomes owed.
            let outcome = owner_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &o_kp.actor_id(),
                        &h_kp.actor_id(),
                        &"aa".repeat(32),
                        &accept_bytes,
                        now,
                    )
                })
                .expect("ingest accept");
            assert!(matches!(outcome, IngestOutcome::AcceptBound { .. }));

            // One owner drive completes offer-post (already-owed), mint,
            // deliver-post and the endpoints row. The minting device's fleet
            // view has removed two devices (answered unsorted, one twice).
            let (removed_a, removed_b) = ([0xA0u8; 32], [0xB0u8; 32]);
            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter {
                    removed: vec![removed_b, removed_a, removed_b],
                    ..Default::default()
                },
                MemDepositor::default(),
            );
            let report = drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .expect("owner drive");
            assert_eq!(report.minted, 1);
            assert_eq!(report.rows_written, 1);
            assert_eq!(report.still_owed, 0);
            // The Mint event is custody-shaped and carries the INTERSECTION.
            let stored = owner_cfg.ledger().current();
            assert_eq!(stored.grant_events.len(), 1);
            assert!(is_custody_grant(&stored.grant_events[0].scope));
            assert_eq!(
                custody_set_from_scopes(&stored.grant_events[0].scope),
                Some(CustodyScopeSet::Scopes(vec!["state".into()])),
                "the effective set is the narrowed intersection, never the offer"
            );
            // The keyless blob reached the depositor.
            assert_eq!(depositor.blobs.lock().unwrap().len(), 1);
            // The owner-side registry value names the accepted device.
            // Scoped so the guard drops before the awaits below (this test
            // drives both sides, and a lock held across an await is the
            // deadlock shape `clippy::await_holding_lock` exists to catch).
            {
                let eps = writer.endpoints.lock().unwrap();
                assert_eq!(eps.len(), 1);
                assert_eq!(eps[0].endpoints.node_id, custodian_key);
            }

            // The deliver payload the drive posted reaches the host.
            let deliver_bytes = {
                let posted = poster.posted.lock().unwrap();
                // offer re-post (unposted at drive time) + deliver.
                assert_eq!(posted.len(), 2, "offer post + deliver post");
                posted.last().unwrap().1.clone()
            };
            let outcome = host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &"aa".repeat(32),
                        &deliver_bytes,
                        now,
                    )
                })
                .expect("ingest deliver — the witness verifies against the accepted key");
            assert!(matches!(outcome, IngestOutcome::WitnessHeld { .. }));

            // The host drive writes the custodies-held row: witness verbatim,
            // budget carried, owner devices + nest URL from the deliver.
            let (h_poster, h_writer, h_depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );
            let report = drive_ceremonies(
                &host_cfg,
                host_cfg.ledger(),
                &h_kp,
                &h_poster,
                &h_writer,
                &h_depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .expect("host drive");
            assert_eq!(report.rows_written, 1);
            // Same scoping rule as the owner-side assertions above.
            {
                let held = h_writer.held.lock().unwrap();
                assert_eq!(held.len(), 1);
                assert_eq!(held[0].owner, o_kp.actor_id().0);
                assert_eq!(
                    held[0].retained_bytes_cap,
                    DEFAULT_RETAINED_BYTES_CAP / 2,
                    "the accept's budget rides into the row from day one"
                );
                assert_eq!(held[0].owner_devices.len(), 1);
                assert_eq!(
                    held[0].owner_nest_url.as_deref(),
                    Some("https://nest.example/")
                );
                // The stored witness verifies for the accepted key.
                let witness: EmbedAsBytes =
                    fauna_core::encoding::canonical_decode(&held[0].witness)
                        .expect("witness bytes");
                let admission =
                    verify_custody_witness(&witness, &custodian_key, &o_kp.actor_id(), now)
                        .expect("the held witness admits the accepted device");
                assert_eq!(
                    admission.removed_devices,
                    vec![removed_a, removed_b],
                    "the owner-signed exclusion list is the minting device's \
                     removed set, canonical"
                );
            }

            // Idempotence: a second drive on either side does nothing new.
            let report = drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .unwrap();
            assert_eq!(report, DriveReport::default(), "owner side settled");
            let report = drive_ceremonies(
                &host_cfg,
                host_cfg.ledger(),
                &h_kp,
                &h_poster,
                &h_writer,
                &h_depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .unwrap();
            assert_eq!(report, DriveReport::default(), "host side settled");
            assert_eq!(
                owner_cfg.ledger().current().grant_events.len(),
                1,
                "one mint, ever"
            );
        });
    }

    /// **The `fauna.state.custody-ceremony` size pin** (`config-dissolution.md`
    /// § Phases and gates → *Bounded rows*): one full ceremony run through its
    /// receipt, both sides, in memory — offer → accept → mint + deliver →
    /// held, then the host's minted receipt and the owner's verified copy of
    /// it — with a wide owner fleet (16 devices, every address list filled),
    /// 16 removed devices on the witness and 64 coverage families on the
    /// receipt. Each side's record, as the row the plane door writes, seals
    /// under HALF the per-entry cap, measured with the writer door's own
    /// `sealed_envelope_len`, generation-sealed as the kind is. A row keeps
    /// ONE latest receipt per side, so no later check-in grows it.
    #[test]
    fn a_full_ceremony_record_seals_under_half_the_entry_cap_on_both_sides() {
        use fauna_core::account_entry_crypto::{EntryPlaintext, sealed_envelope_len};
        use fauna_core::custody_receipt::{CustodyReceipt, ScopeCoverage, sign_custody_receipt};
        use fauna_protocol::account_state::MAX_STATE_ENTRY_BYTES;
        use fauna_protocol::merge_policy::KIND_CUSTODY_CEREMONY;
        block_on(async {
            let (o_kp, h_kp) = (owner_kp(), host_kp());
            let custodian = ActorKeypair::from_secret([0xC5; 32]);
            let custodian_key = custodian.actor_id().0;
            let owner_cfg = MemConfig::new(o_kp.actor_id());
            let host_cfg = MemConfig::new(h_kp.actor_id());
            let now = Timestamp(1_000_000_000);
            let channel = "aa".repeat(32);
            let device = |i: u8| DeviceEndpoints {
                node_id: [i; 32],
                lan_addrs: (0..4).map(|n| format!("192.168.{i}.{n}:44330")).collect(),
                public_addrs: (0..2).map(|n| format!("203.0.{i}.{n}:44330")).collect(),
                relay_url: Some(format!("https://relay-{i}.example.net/")),
            };

            let offer_bytes = owner_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &o_kp,
                        OfferParams {
                            owner_devices: (1..=16).map(device).collect(),
                            ..offer_params(h_kp.actor_id(), CustodyScopeSet::Account)
                        },
                        now,
                    )
                })
                .unwrap();
            host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &channel,
                        &offer_bytes,
                        now,
                    )
                })
                .unwrap();
            let accept_bytes = host_cfg
                .with(|c| {
                    build_accept(
                        c,
                        &h_kp,
                        &[0x1D; 16],
                        custodian_key,
                        DeviceEndpoints {
                            node_id: custodian_key,
                            ..device(0xC5)
                        },
                        DEFAULT_RETAINED_BYTES_CAP,
                        None,
                        now,
                    )
                })
                .unwrap();
            owner_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &o_kp.actor_id(),
                        &h_kp.actor_id(),
                        &channel,
                        &accept_bytes,
                        now,
                    )
                })
                .unwrap();
            let (poster, writer) = (
                MemPoster::default(),
                MemWriter {
                    removed: (0x40..0x50).map(|i| [i; 32]).collect(),
                    ..Default::default()
                },
            );
            drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &MemDepositor::default(),
                &MemHosting::default(),
                now,
            )
            .await
            .unwrap();
            let deliver_bytes = poster.posted.lock().unwrap().last().unwrap().1.clone();
            host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &channel,
                        &deliver_bytes,
                        now,
                    )
                })
                .expect("the witness verifies against the accepted key");

            // The check-in: the host mints, the owner verifies and records.
            let receipt = CustodyReceipt {
                grant_id: vec![0x1D; 16],
                owner: o_kp.actor_id().0,
                custodian_key,
                covered: (0..64)
                    .map(|i| ScopeCoverage {
                        scope: format!("content:blog:{i:064x}"),
                        item_class: "blob".into(),
                        rows: u64::MAX,
                        payload_bytes: u64::MAX,
                    })
                    .collect(),
                held_bytes: u64::MAX,
                retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
                evicted_bytes: u64::MAX,
                unreclaimable_bytes: u64::MAX,
                attested_at: Timestamp(now.0 + 1),
            };
            let receipt_bytes = fauna_core::encoding::canonical_encode(
                &sign_custody_receipt(&custodian, &receipt).unwrap(),
            )
            .unwrap()
            .to_vec();
            record_minted_receipt(
                &host_cfg,
                &[0x1D; 16],
                receipt_bytes.clone(),
                Timestamp(now.0 + 1),
                false,
            )
            .await;
            owner_cfg
                .with(|c| ingest_receipt(c, &channel, &receipt_bytes))
                .expect("the bound device's receipt verifies");

            let owner = owner_cfg.snapshot().await.unwrap();
            let host = host_cfg.snapshot().await.unwrap();
            assert!(!owner.granted[0].latest_receipt.is_empty());
            assert!(!host.held[0].deliver.is_empty() && !host.held[0].receipt.is_empty());
            for (side, cfg) in [("granted", owner), ("held", host)] {
                let rows = cfg.rows().unwrap();
                assert_eq!(rows.len(), 1, "one ceremony, one {side} row");
                let (key, record) = &rows[0];
                let sealed = sealed_envelope_len(
                    &EntryPlaintext {
                        kind: KIND_CUSTODY_CEREMONY.to_string(),
                        key: key.clone(),
                        merge_meta: None,
                        value: record.encode().unwrap().into(),
                        tombstone: false,
                    },
                    true,
                )
                .unwrap();
                eprintln!(
                    "size pin: the {side} row seals to {sealed} B (half cap {} B)",
                    MAX_STATE_ENTRY_BYTES / 2
                );
                assert!(
                    sealed <= MAX_STATE_ENTRY_BYTES / 2,
                    "the {side} row {key} seals to {sealed} B, over half the \
                     {MAX_STATE_ENTRY_BYTES} B cap"
                );
            }
        });
    }

    // ── Custody receipts, owner side (W8.7 leg 2) ────────────────────────────

    /// Run the ceremony to a settled owner-side state and hand back the pieces
    /// a receipt test needs: the owner's config (with the grant recorded and
    /// the endpoints row already written) and the custodian's keypair.
    async fn settled_owner_side() -> (MemConfig, ActorKeypair, ActorKeypair) {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let custodian = ActorKeypair::from_secret([0xC5; 32]);
        let custodian_key = custodian.actor_id().0;
        let owner_cfg = MemConfig::new(o_kp.actor_id());
        let host_cfg = MemConfig::new(h_kp.actor_id());
        let now = Timestamp(1_000_000_000);

        let offer_bytes = owner_cfg
            .with(|c| {
                begin_offer(
                    c,
                    &o_kp,
                    offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
                    now,
                )
            })
            .unwrap();
        host_cfg
            .with(|c| {
                ingest_payload(
                    c,
                    &h_kp.actor_id(),
                    &o_kp.actor_id(),
                    &"aa".repeat(32),
                    &offer_bytes,
                    now,
                )
            })
            .unwrap();
        let accept_bytes = host_cfg
            .with(|c| {
                build_accept(
                    c,
                    &h_kp,
                    &[0x1D; 16],
                    custodian_key,
                    DeviceEndpoints {
                        node_id: custodian_key,
                        lan_addrs: Vec::new(),
                        public_addrs: vec!["198.51.100.7:4433".into()],
                        relay_url: None,
                    },
                    DEFAULT_RETAINED_BYTES_CAP,
                    None,
                    now,
                )
            })
            .unwrap();
        owner_cfg
            .with(|c| {
                ingest_payload(
                    c,
                    &o_kp.actor_id(),
                    &h_kp.actor_id(),
                    &"aa".repeat(32),
                    &accept_bytes,
                    now,
                )
            })
            .unwrap();
        let (poster, writer, depositor) = (
            MemPoster::default(),
            MemWriter::default(),
            MemDepositor::default(),
        );
        drive_ceremonies(
            &owner_cfg,
            owner_cfg.ledger(),
            &o_kp,
            &poster,
            &writer,
            &depositor,
            &MemHosting::default(),
            now,
        )
        .await
        .unwrap();
        (owner_cfg, o_kp, custodian)
    }

    fn signed_receipt(
        custodian: &ActorKeypair,
        owner: &ActorKeypair,
        at: u64,
        held: u64,
    ) -> Vec<u8> {
        let receipt = fauna_core::custody_receipt::CustodyReceipt {
            grant_id: vec![0x1D; 16],
            owner: owner.actor_id().0,
            custodian_key: custodian.actor_id().0,
            covered: Vec::new(),
            held_bytes: held,
            retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
            evicted_bytes: 0,
            unreclaimable_bytes: 0,
            attested_at: Timestamp(at),
        };
        let env =
            fauna_core::custody_receipt::sign_custody_receipt(custodian, &receipt).expect("sign");
        fauna_core::encoding::canonical_encode(&env)
            .unwrap()
            .to_vec()
    }

    /// The receipt's whole owner-side leg: verified against the key the
    /// ACCEPT bound, recorded, and folded onto the registry row by the next
    /// drive — so `ui/nests.md`'s *last confirmed ‹time›* has a source.
    #[test]
    fn a_verified_receipt_is_recorded_and_reaches_the_registry_row() {
        block_on(async {
            let (cfg, o_kp, custodian) = settled_owner_side().await;
            let bytes = signed_receipt(&custodian, &o_kp, 2_000_000, 4096);

            let outcome = cfg
                .with(|c| ingest_receipt(c, &"aa".repeat(32), &bytes))
                .expect("a receipt from the accept-bound device verifies");
            assert!(matches!(outcome, ReceiptOutcome::Recorded { .. }));

            // The next drive folds it onto the row — the row is what the T16
            // facet reads, so a recorded-but-unwritten receipt is invisible.
            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );
            let report = drive_ceremonies(
                &cfg,
                cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                Timestamp(2),
            )
            .await
            .unwrap();
            assert_eq!(report.rows_written, 1, "the receipt re-opens the row write");
            {
                let eps = writer.endpoints.lock().unwrap();
                assert_eq!(eps[0].latest_receipt.as_deref(), Some(&bytes[..]));
            }

            // And it settles: nothing further is owed for the same receipt.
            let report = drive_ceremonies(
                &cfg,
                cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                Timestamp(3),
            )
            .await
            .unwrap();
            assert_eq!(report, DriveReport::default(), "settled after the fold");
        });
    }

    /// A receipt signed by any key but the accept-bound device is refused —
    /// the check that stops a stranger's attestation standing in as coverage.
    #[test]
    fn a_receipt_from_an_unbound_device_is_refused() {
        block_on(async {
            let (cfg, o_kp, _custodian) = settled_owner_side().await;
            let impostor = ActorKeypair::from_secret([0x77; 32]);
            let bytes = signed_receipt(&impostor, &o_kp, 2_000_000, 4096);

            let err = cfg
                .with(|c| ingest_receipt(c, &"aa".repeat(32), &bytes))
                .expect_err("an unbound device must not attest for this custody");
            assert!(
                err.to_string().contains("custodian"),
                "the refusal names why: {err}"
            );
        });
    }

    /// The same receipt arriving on a different channel is refused — a valid
    /// attestation replayed into an unrelated conversation conveys nothing.
    #[test]
    fn a_receipt_on_the_wrong_channel_is_refused() {
        block_on(async {
            let (cfg, o_kp, custodian) = settled_owner_side().await;
            let bytes = signed_receipt(&custodian, &o_kp, 2_000_000, 4096);
            assert!(
                cfg.with(|c| ingest_receipt(c, &"bb".repeat(32), &bytes))
                    .is_err()
            );
        });
    }

    /// A receipt for a grant this account never granted is refused.
    #[test]
    fn a_receipt_for_an_unknown_grant_is_refused() {
        block_on(async {
            let (cfg, o_kp, custodian) = settled_owner_side().await;
            let mut receipt = fauna_core::custody_receipt::CustodyReceipt {
                grant_id: vec![0x2E; 16], // never granted
                owner: o_kp.actor_id().0,
                custodian_key: custodian.actor_id().0,
                covered: Vec::new(),
                held_bytes: 1,
                retained_bytes_cap: 2,
                evicted_bytes: 0,
                unreclaimable_bytes: 0,
                attested_at: Timestamp(9),
            };
            receipt.grant_id = vec![0x2E; 16];
            let env =
                fauna_core::custody_receipt::sign_custody_receipt(&custodian, &receipt).unwrap();
            let bytes = fauna_core::encoding::canonical_encode(&env)
                .unwrap()
                .to_vec();
            assert!(
                cfg.with(|c| ingest_receipt(c, &"aa".repeat(32), &bytes))
                    .is_err()
            );
        });
    }

    /// Monotone in `attested_at`: a replayed older attestation never displaces
    /// a newer one, so coverage cannot be made to look fresher — or thinner —
    /// than the custodian last reported.
    #[test]
    fn an_older_receipt_never_displaces_a_newer_one() {
        block_on(async {
            let (cfg, o_kp, custodian) = settled_owner_side().await;
            let newer = signed_receipt(&custodian, &o_kp, 5_000_000, 4096);
            let older = signed_receipt(&custodian, &o_kp, 1_000_000, 999_999);

            assert!(matches!(
                cfg.with(|c| ingest_receipt(c, &"aa".repeat(32), &newer))
                    .unwrap(),
                ReceiptOutcome::Recorded { .. }
            ));
            assert_eq!(
                cfg.with(|c| ingest_receipt(c, &"aa".repeat(32), &older))
                    .unwrap(),
                ReceiptOutcome::NotNewer
            );
            let stored = cfg.snapshot().await.unwrap();
            let g = &stored.granted[0];
            assert_eq!(g.latest_receipt, newer, "the newer attestation stands");
            assert_eq!(g.latest_receipt_at, Timestamp(5_000_000));
        });
    }

    /// A re-delivery of the newest receipt is not newer either — idempotent,
    /// and it must not re-open the row write on every poll.
    #[test]
    fn a_redelivered_receipt_is_not_newer() {
        block_on(async {
            let (cfg, o_kp, custodian) = settled_owner_side().await;
            let bytes = signed_receipt(&custodian, &o_kp, 4_000_000, 10);
            cfg.with(|c| ingest_receipt(c, &"aa".repeat(32), &bytes))
                .unwrap();
            assert_eq!(
                cfg.with(|c| ingest_receipt(c, &"aa".repeat(32), &bytes))
                    .unwrap(),
                ReceiptOutcome::NotNewer
            );
        });
    }

    /// The scope algebra's refusal matrix — intersection only ever narrows,
    /// and the shared-audience carve-out holds at the accept door.
    #[test]
    fn narrowing_never_widens() {
        // No narrowing → offered.
        assert_eq!(
            effective_scope_set(&CustodyScopeSet::Account, None).unwrap(),
            CustodyScopeSet::Account
        );
        // Account → explicit non-co-authored list: fine.
        assert_eq!(
            effective_scope_set(
                &CustodyScopeSet::Account,
                Some(&CustodyScopeSet::Scopes(vec!["state".into()]))
            )
            .unwrap(),
            CustodyScopeSet::Scopes(vec!["state".into()])
        );
        // Account → a co-authored entry: the carve-out refuses (it was never
        // covered, so it would widen).
        assert!(matches!(
            effective_scope_set(
                &CustodyScopeSet::Account,
                Some(&CustodyScopeSet::Scopes(vec![conv_scope()]))
            ),
            Err(CustodyCeremonyError::BadNarrowing(_))
        ));
        // An explicitly OFFERED co-authored scope narrows to itself fine —
        // the explicit list is the one door for co-authored scopes.
        assert_eq!(
            effective_scope_set(
                &CustodyScopeSet::Scopes(vec![conv_scope(), "state".into()]),
                Some(&CustodyScopeSet::Scopes(vec![conv_scope()]))
            )
            .unwrap(),
            CustodyScopeSet::Scopes(vec![conv_scope()])
        );
        // List → superset: refused.
        assert!(matches!(
            effective_scope_set(
                &CustodyScopeSet::Scopes(vec!["state".into()]),
                Some(&CustodyScopeSet::Scopes(vec![
                    "state".into(),
                    "state-fleet".into()
                ]))
            ),
            Err(CustodyCeremonyError::BadNarrowing(_))
        ));
        // List → Account: refused.
        assert!(matches!(
            effective_scope_set(
                &CustodyScopeSet::Scopes(vec!["state".into()]),
                Some(&CustodyScopeSet::Account)
            ),
            Err(CustodyCeremonyError::BadNarrowing(_))
        ));
        // Narrowed-to-empty: refused (an empty custody grant is a mistake).
        assert!(
            effective_scope_set(
                &CustodyScopeSet::Scopes(vec!["state".into()]),
                Some(&CustodyScopeSet::Scopes(vec![]))
            )
            .is_err()
        );
    }

    /// One serving device per grant id, forever: a second, different accept
    /// is refused; the exact duplicate is idempotent.
    #[test]
    fn a_second_accept_cannot_rebind_the_ceremony() {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let mut owner_cfg = CustodyConfig::default();
        let mut host_cfg = CustodyConfig::default();
        let now = Timestamp(1_000_000_000);
        let offer_bytes = begin_offer(
            &mut owner_cfg,
            &o_kp,
            offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
            now,
        )
        .unwrap();
        ingest_payload(
            &mut host_cfg,
            &h_kp.actor_id(),
            &o_kp.actor_id(),
            &"aa".repeat(32),
            &offer_bytes,
            now,
        )
        .unwrap();
        let accept_bytes = build_accept(
            &mut host_cfg,
            &h_kp,
            &[0x1D; 16],
            [0xC5; 32],
            DeviceEndpoints {
                node_id: [0xC5; 32],
                ..Default::default()
            },
            DEFAULT_RETAINED_BYTES_CAP,
            None,
            now,
        )
        .unwrap();
        ingest_payload(
            &mut owner_cfg,
            &o_kp.actor_id(),
            &h_kp.actor_id(),
            &"aa".repeat(32),
            &accept_bytes,
            now,
        )
        .unwrap();
        // The exact duplicate: idempotent.
        assert_eq!(
            ingest_payload(
                &mut owner_cfg,
                &o_kp.actor_id(),
                &h_kp.actor_id(),
                &"aa".repeat(32),
                &accept_bytes,
                now,
            )
            .unwrap(),
            IngestOutcome::Duplicate
        );
        // A different serving device under the same grant id: refused.
        let offer_env: EmbedAsBytes =
            fauna_core::encoding::canonical_decode(&owner_cfg.granted[0].offer).unwrap();
        let rebind = CustodyAccept {
            grant_id: vec![0x1D; 16],
            offer_digest: offer_digest(&offer_env).unwrap(),
            host: h_kp.actor_id(),
            custodian_key: [0xC6; 32],
            custodian_endpoints: DeviceEndpoints {
                node_id: [0xC6; 32],
                ..Default::default()
            },
            retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
            narrowed_scopes: None,
            accepted_at: now,
            ..Default::default()
        };
        let env = sign_custody_accept(&h_kp, &rebind).unwrap();
        let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Accept(env)).unwrap();
        assert!(matches!(
            ingest_payload(
                &mut owner_cfg,
                &o_kp.actor_id(),
                &h_kp.actor_id(),
                &"aa".repeat(32),
                &bytes,
                now,
            ),
            Err(CustodyCeremonyError::AlreadyBound)
        ));
    }

    /// A settled owner side whose accept bound a NEST — the stage-(c)
    /// carriage's fixture. Returns the config, the owner, and the nest's
    /// identity keypair (the receipt signer).
    fn nest_form_owner_side() -> (CustodyConfig, ActorKeypair, ActorKeypair) {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let mut owner_cfg = CustodyConfig::default();
        let now = Timestamp(1_000_000_000);
        begin_offer(
            &mut owner_cfg,
            &o_kp,
            offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
            now,
        )
        .unwrap();
        let offer_env: EmbedAsBytes =
            fauna_core::encoding::canonical_decode(&owner_cfg.granted[0].offer).unwrap();
        let nest_kp = ActorKeypair::from_secret([0xAB; 32]);
        let nest_key = nest_kp.actor_id().0;
        let accept = CustodyAccept {
            grant_id: vec![0x1D; 16],
            offer_digest: offer_digest(&offer_env).unwrap(),
            host: h_kp.actor_id(),
            custodian_key: nest_key,
            custodian_endpoints: DeviceEndpoints {
                node_id: nest_key,
                ..Default::default()
            },
            custodian_nest_url: Some("https://friend-nest.example/".into()),
            accepted_at: now,
            ..Default::default()
        };
        let env = sign_custody_accept(&h_kp, &accept).unwrap();
        let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Accept(env)).unwrap();
        ingest_payload(
            &mut owner_cfg,
            &o_kp.actor_id(),
            &h_kp.actor_id(),
            &"aa".repeat(32),
            &bytes,
            now,
        )
        .unwrap();
        (owner_cfg, o_kp, nest_kp)
    }

    /// Record, publish, then deposit (`ui/nests.md` § Trust facet — grants →
    /// *Record-then-deposit*, the published form): the custody grant's `Mint`
    /// the bound nest did not acknowledge deposits no blob and stays owed; the
    /// next drive, the nest taking the publish, deposits under the SAME
    /// recorded `Mint` — never a second one.
    #[test]
    fn an_unpublished_custody_mint_deposits_nothing_and_stays_owed() {
        block_on(async {
            let (o_kp, h_kp) = (owner_kp(), host_kp());
            let owner_cfg = MemConfig::new(o_kp.actor_id());
            let host_cfg = MemConfig::new(h_kp.actor_id());
            let now = Timestamp(1_000_000_000);
            let nest_key = ActorKeypair::from_secret([0xAB; 32]).actor_id().0;

            let offer_bytes = owner_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &o_kp,
                        offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
                        now,
                    )
                })
                .expect("offer");
            host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &"aa".repeat(32),
                        &offer_bytes,
                        now,
                    )
                })
                .expect("ingest offer");
            let accept_bytes = host_cfg
                .with(|c| {
                    build_accept_nest(
                        c,
                        &h_kp,
                        &[0x1D; 16],
                        nest_key,
                        "https://my-own-nest.example".into(),
                        DEFAULT_RETAINED_BYTES_CAP,
                        None,
                        now,
                    )
                })
                .expect("nest-form accept");
            owner_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &o_kp.actor_id(),
                        &h_kp.actor_id(),
                        &"aa".repeat(32),
                        &accept_bytes,
                        now,
                    )
                })
                .expect("owner binds the accept");

            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );
            owner_cfg.ledger().publish_refuses(true);
            let report = drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .expect("an unpublished mint is owed, not an error");
            assert!(report.still_owed >= 1, "the mint stays owed: {report:?}");
            assert!(
                depositor.blobs.lock().unwrap().is_empty(),
                "no blob deposited behind an unpublished event"
            );
            assert_eq!(owner_cfg.ledger().current().grant_events.len(), 1);

            owner_cfg.ledger().publish_refuses(false);
            drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .expect("the next drive");
            assert_eq!(depositor.blobs.lock().unwrap().len(), 1, "deposited once");
            assert_eq!(
                owner_cfg.ledger().current().grant_events.len(),
                1,
                "the recorded Mint is reused, never re-signed"
            );
        });
    }

    /// The NEST-form ceremony end to end (the host-side choice): offer →
    /// `build_accept_nest` (pinned nest identity + URL, zero candidates) →
    /// owner binds + mints → deliver → the host drive routes arm (6) to the
    /// HOSTING deposit — the host's own nest's register door — and writes NO
    /// `custodies-held` fleet row (no host device serves or pulls).
    #[test]
    fn a_nest_form_ceremony_deposits_the_hosting_row_not_a_fleet_row() {
        block_on(async {
            let (o_kp, h_kp) = (owner_kp(), host_kp());
            let owner_cfg = MemConfig::new(o_kp.actor_id());
            let host_cfg = MemConfig::new(h_kp.actor_id());
            let now = Timestamp(1_000_000_000);
            let nest_kp = ActorKeypair::from_secret([0xAB; 32]);
            let nest_key = nest_kp.actor_id().0;

            let offer_bytes = owner_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &o_kp,
                        offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
                        now,
                    )
                })
                .expect("offer");
            host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &"aa".repeat(32),
                        &offer_bytes,
                        now,
                    )
                })
                .expect("ingest offer");

            let accept_bytes = host_cfg
                .with(|c| {
                    build_accept_nest(
                        c,
                        &h_kp,
                        &[0x1D; 16],
                        nest_key,
                        "https://my-own-nest.example".into(),
                        DEFAULT_RETAINED_BYTES_CAP,
                        None,
                        now,
                    )
                })
                .expect("nest-form accept");
            owner_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &o_kp.actor_id(),
                        &h_kp.actor_id(),
                        &"aa".repeat(32),
                        &accept_bytes,
                        now,
                    )
                })
                .expect("owner binds the nest-form accept");

            // Owner drive: mint + deliver (+ the endpoints row carrying the
            // URL — stage (a)'s fold, already pinned elsewhere).
            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );
            drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .expect("owner drive");
            let deliver_bytes = poster.posted.lock().unwrap().last().unwrap().1.clone();
            host_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &h_kp.actor_id(),
                        &o_kp.actor_id(),
                        &"aa".repeat(32),
                        &deliver_bytes,
                        now,
                    )
                })
                .expect("host holds the witness");

            // Host drive: arm (6) routes to the HOSTING deposit.
            let (h_poster, h_writer, h_depositor, h_hosting) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
                MemHosting::default(),
            );
            let report = drive_ceremonies(
                &host_cfg,
                host_cfg.ledger(),
                &h_kp,
                &h_poster,
                &h_writer,
                &h_depositor,
                &h_hosting,
                now,
            )
            .await
            .expect("host drive");
            assert_eq!(report.rows_written, 1, "the hosting row went through");
            assert!(
                h_writer.held.lock().unwrap().is_empty(),
                "a nest-form custody writes NO custodies-held fleet row — \
                 no host device serves or pulls"
            );
            // Scoped so the guard drops before the second drive's await (the
            // await_holding_lock deadlock shape).
            {
                let deposits = h_hosting.deposits.lock().unwrap();
                assert_eq!(deposits.len(), 1);
                assert_eq!(deposits[0].grant_id, vec![0x1D; 16]);
                assert_eq!(deposits[0].owner, o_kp.actor_id().0);
                assert_eq!(
                    deposits[0].owner_nest_url, "https://nest.example/",
                    "the OWNER's nest URL from the deliver — the pump's dial anchor"
                );
                assert!(!deposits[0].witness.is_empty(), "witness rides verbatim");
                assert!(!deposits[0].stopped);
            }

            // Settled: a second drive owes nothing and deposits nothing more.
            let report = drive_ceremonies(
                &host_cfg,
                host_cfg.ledger(),
                &h_kp,
                &h_poster,
                &h_writer,
                &h_depositor,
                &h_hosting,
                now,
            )
            .await
            .expect("host drive 2");
            assert_eq!(report, DriveReport::default(), "settled after the deposit");
        });
    }

    /// `build_accept_nest`'s refusal floor: an offer with no owner nest URL
    /// (the pump's only route missing) refuses at the consent gesture.
    #[test]
    fn build_accept_nest_refuses_an_unroutable_offer() {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let now = Timestamp(1_000_000_000);

        // No owner nest URL on the offer.
        let mut owner_cfg = CustodyConfig::default();
        let mut params = offer_params(h_kp.actor_id(), CustodyScopeSet::Account);
        params.owner_nest_url = None;
        let offer_bytes = begin_offer(&mut owner_cfg, &o_kp, params, now).unwrap();
        let mut host_cfg = CustodyConfig::default();
        ingest_payload(
            &mut host_cfg,
            &h_kp.actor_id(),
            &o_kp.actor_id(),
            &"aa".repeat(32),
            &offer_bytes,
            now,
        )
        .unwrap();
        let err = build_accept_nest(
            &mut host_cfg,
            &h_kp,
            &[0x1D; 16],
            [0xAB; 32],
            "https://my-own-nest.example".into(),
            DEFAULT_RETAINED_BYTES_CAP,
            None,
            now,
        )
        .expect_err("no owner nest URL → no pull route → refused at consent");
        assert!(err.to_string().contains("owner nest URL"), "{err}");

        // The DEVICE form still accepts the same offer (the floor is
        // nest-form-only).
        assert!(
            build_accept(
                &mut host_cfg,
                &h_kp,
                &[0x1D; 16],
                [0xC5; 32],
                DeviceEndpoints {
                    node_id: [0xC5; 32],
                    public_addrs: vec!["198.51.100.7:4433".into()],
                    ..Default::default()
                },
                DEFAULT_RETAINED_BYTES_CAP,
                None,
                now,
            )
            .is_ok()
        );
    }

    /// The stage-(c) fold: a receipt the custodian NEST signed, fetched from
    /// the owner's own nest's staging buffer, verifies against the recorded
    /// accept's bound key and records — and stays monotone (an older fetch
    /// never displaces).
    #[test]
    fn a_nest_door_receipt_folds_for_a_nest_form_ceremony() {
        let (mut cfg, o_kp, nest_kp) = nest_form_owner_side();
        let bytes = signed_receipt(&nest_kp, &o_kp, 2_000_000, 4096);
        let outcome = ingest_receipt_from_nest(&mut cfg, &bytes)
            .expect("a nest-signed receipt verifies against the nest-form accept");
        assert!(matches!(outcome, ReceiptOutcome::Recorded { .. }));

        let older = signed_receipt(&nest_kp, &o_kp, 1_500_000, 999);
        assert!(
            matches!(
                ingest_receipt_from_nest(&mut cfg, &older).unwrap(),
                ReceiptOutcome::NotNewer
            ),
            "an older staged fetch never displaces"
        );
        assert_eq!(cfg.granted[0].latest_receipt_at, Timestamp(2_000_000));
    }

    /// The carriage rule's other half: a DEVICE-form ceremony's receipts
    /// travel its channel and only its channel — one arriving through the
    /// nest door is refused, exactly as a wrong-channel delivery is.
    #[test]
    fn a_nest_door_receipt_for_a_device_form_ceremony_is_refused() {
        block_on(async {
            let (cfg, o_kp, custodian) = settled_owner_side().await;
            let bytes = signed_receipt(&custodian, &o_kp, 2_000_000, 4096);
            let err = cfg
                .with(|c| ingest_receipt_from_nest(c, &bytes))
                .expect_err("a device custody's receipt must not enter by the nest door");
            assert!(
                err.to_string().contains("did not bind a nest"),
                "the refusal names why: {err}"
            );
        });
    }

    /// A nest-anchored accept (the nest-custodian identity fact) is
    /// captured, and the URL reaches the fleet's registry
    /// row so every replica reads the render split + restore anchor from it.
    #[test]
    fn a_nest_anchored_accept_binds_and_its_url_reaches_the_registry_row() {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let mut owner_cfg = CustodyConfig::default();
        let now = Timestamp(1_000_000_000);
        begin_offer(
            &mut owner_cfg,
            &o_kp,
            offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
            now,
        )
        .unwrap();
        let offer_env: EmbedAsBytes =
            fauna_core::encoding::canonical_decode(&owner_cfg.granted[0].offer).unwrap();
        // The host's pinned nest actor identity stands in for a device key;
        // nest form = URL anchor, zero dial candidates.
        let nest_key = [0xAB; 32];
        let accept = CustodyAccept {
            grant_id: vec![0x1D; 16],
            offer_digest: offer_digest(&offer_env).unwrap(),
            host: h_kp.actor_id(),
            custodian_key: nest_key,
            custodian_endpoints: DeviceEndpoints {
                node_id: nest_key,
                ..Default::default()
            },
            custodian_nest_url: Some("https://friend-nest.example/".into()),
            accepted_at: now,
            ..Default::default()
        };
        let env = sign_custody_accept(&h_kp, &accept).unwrap();
        let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Accept(env)).unwrap();
        assert_eq!(
            ingest_payload(
                &mut owner_cfg,
                &o_kp.actor_id(),
                &h_kp.actor_id(),
                &"aa".repeat(32),
                &bytes,
                now,
            )
            .unwrap(),
            IngestOutcome::AcceptBound {
                grant_id: vec![0x1D; 16]
            }
        );
        let row = endpoints_row_for(&owner_cfg.granted[0]).unwrap();
        assert_eq!(
            row.custodian_nest_url.as_deref(),
            Some("https://friend-nest.example/")
        );
        assert_eq!(row.endpoints.node_id, nest_key);
    }

    /// The digest binding: an accept minted against one offer conveys
    /// nothing against a re-offer that reused nothing but the counterpart.
    #[test]
    fn an_accept_binds_to_one_exact_offer() {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let mut owner_cfg = CustodyConfig::default();
        let now = Timestamp(1_000_000_000);
        begin_offer(
            &mut owner_cfg,
            &o_kp,
            offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
            now,
        )
        .unwrap();
        // An accept whose digest names some OTHER offer's bytes.
        let accept = CustodyAccept {
            grant_id: vec![0x1D; 16],
            offer_digest: [0xEE; 32],
            host: h_kp.actor_id(),
            custodian_key: [0xC5; 32],
            custodian_endpoints: DeviceEndpoints {
                node_id: [0xC5; 32],
                ..Default::default()
            },
            retained_bytes_cap: DEFAULT_RETAINED_BYTES_CAP,
            narrowed_scopes: None,
            accepted_at: now,
            ..Default::default()
        };
        let env = sign_custody_accept(&h_kp, &accept).unwrap();
        let bytes = encode_ceremony_message(&CustodyCeremonyMessage::Accept(env)).unwrap();
        assert!(matches!(
            ingest_payload(
                &mut owner_cfg,
                &o_kp.actor_id(),
                &h_kp.actor_id(),
                &"aa".repeat(32),
                &bytes,
                now,
            ),
            Err(CustodyCeremonyError::NoMatchingCeremony(_))
        ));
    }

    /// The crash window between act and mark: a failed post stays owed and
    /// the next drive re-posts; nothing double-mints.
    #[test]
    fn a_failed_post_stays_owed_and_redrives() {
        block_on(async {
            let (o_kp, h_kp) = (owner_kp(), host_kp());
            let owner_cfg = MemConfig::new(o_kp.actor_id());
            let now = Timestamp(1_000_000_000);
            owner_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &o_kp,
                        offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
                        now,
                    )
                })
                .unwrap();
            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );
            poster.down.store(true, Relaxed);
            let report = drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .unwrap();
            assert_eq!((report.posted, report.still_owed), (0, 1));
            assert!(!owner_cfg.snapshot().await.unwrap().granted[0].offer_posted);
            poster.down.store(false, Relaxed);
            let report = drive_ceremonies(
                &owner_cfg,
                owner_cfg.ledger(),
                &o_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                now,
            )
            .await
            .unwrap();
            assert_eq!((report.posted, report.still_owed), (1, 0));
            assert!(owner_cfg.snapshot().await.unwrap().granted[0].offer_posted);
        });
    }

    /// Decay is computed from state + now — an unanswered offer past its
    /// shelf life names itself for re-offer; an answered one never does.
    #[test]
    fn decay_names_only_unanswered_stale_offers() {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let mut cfg = CustodyConfig::default();
        let t0 = Timestamp(1_000_000_000);
        begin_offer(
            &mut cfg,
            &o_kp,
            OfferParams {
                duration_secs: 60,
                ..offer_params(h_kp.actor_id(), CustodyScopeSet::Account)
            },
            t0,
        )
        .unwrap();
        assert!(decayed_offers(&cfg, t0).is_empty());
        let past_shelf = Timestamp(t0.0 + 61 * 1_000_000);
        assert_eq!(decayed_offers(&cfg, past_shelf), vec![vec![0x1D; 16]]);
        // An answered ceremony never decays.
        cfg.granted[0].accept = vec![1];
        assert!(decayed_offers(&cfg, past_shelf).is_empty());
    }

    // ── Custody receipts, host side (W8.7 arc 2 — the check-in's act half) ──

    /// The driver's receipt arm: a held record carrying a minted-but-unposted
    /// receipt posts it through the RECEIPT door — verbatim bytes, its own
    /// channel body, never the ceremony's — marks it posted, and settles. The
    /// crash-window half rides along: a down poster leaves it owed and the
    /// next drive posts it.
    #[test]
    fn drive_posts_an_owed_receipt_verbatim_and_marks_it() {
        block_on(async {
            let h_kp = host_kp();
            let channel = "cc".repeat(32);
            let cfg = MemConfig::new(h_kp.actor_id());
            cfg.with(|c| {
                c.held.push(HeldCustody {
                    grant_id: vec![0x4A; 16],
                    channel_hex: channel.clone(),
                    receipt: vec![0xA7; 40],
                    receipt_posted: false,
                    receipt_minted_at: Timestamp(5),
                    ..Default::default()
                })
            });
            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );

            // Down poster: the post stays owed, the mark stays open.
            poster.down.store(true, Relaxed);
            let report = drive_ceremonies(
                &cfg,
                cfg.ledger(),
                &h_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                Timestamp(6),
            )
            .await
            .unwrap();
            assert_eq!(report.still_owed, 1);
            assert!(!cfg.snapshot().await.unwrap().held[0].receipt_posted);

            // Up: posted through the receipt door, verbatim, and marked.
            poster.down.store(false, Relaxed);
            let report = drive_ceremonies(
                &cfg,
                cfg.ledger(),
                &h_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                Timestamp(7),
            )
            .await
            .unwrap();
            assert_eq!(report.posted, 1);
            {
                let receipts = poster.receipts.lock().unwrap();
                assert_eq!(receipts.as_slice(), &[(channel.clone(), vec![0xA7; 40])]);
                assert!(
                    poster.posted.lock().unwrap().is_empty(),
                    "a receipt must ride its own body door, never the ceremony's"
                );
            }
            assert!(cfg.snapshot().await.unwrap().held[0].receipt_posted);

            // Settled: a further drive owes nothing.
            let report = drive_ceremonies(
                &cfg,
                cfg.ledger(),
                &h_kp,
                &poster,
                &writer,
                &depositor,
                &MemHosting::default(),
                Timestamp(8),
            )
            .await
            .unwrap();
            assert_eq!(report, DriveReport::default());
        });
    }

    /// PROBE-375-A — the grant-id namespace is per-side, but `apply_mark`'s
    /// lookup is not.
    ///
    /// A peer CHOOSES the grant id it offers, and the offer ingest checks only
    /// `held` for a collision (`ingest_payload`, the Offer arm) while
    /// `begin_offer` checks only `granted`. So a peer this account has ever
    /// offered custody to knows one live `granted` id and can echo it back in
    /// a counter-offer, putting the SAME id on both sides of this account's
    /// `CustodyConfig`.
    ///
    /// `apply_mark` then resolves that id `granted`-first with an `else if`,
    /// so every host-side mark — which the driver applies as
    /// `if let CeremonyRecord::Held(r)` — is handed the `Granted` variant and
    /// silently does nothing. The owed act is never marked done, so the driver
    /// re-fires it on every pass, forever: re-posting to the channel (arm 5),
    /// re-writing the `custodies-held` registry row (arm 6), re-posting the
    /// receipt (arm 7). Nothing errors and `DriveReport` counts each repeat as
    /// productive work.
    ///
    /// The grant-id space is chosen by whoever offers, so a cross-side
    /// collision is always representable and cannot be prevented by our own
    /// id minting. The property that must hold is therefore the internal one:
    /// a host-side mark lands on the HOST-side record regardless of what the
    /// owner side happens to key on.
    #[test]
    fn a_shadowed_held_record_is_still_marked() {
        block_on(async {
            let (alice, bob) = (owner_kp(), host_kp());
            let alice_cfg = MemConfig::new(alice.actor_id());
            let bob_cfg = MemConfig::new(bob.actor_id());
            let now = Timestamp(1_000_000_000);
            let channel = "aa".repeat(32);
            const G: [u8; CUSTODY_GRANT_ID_LEN] = [0x1D; CUSTODY_GRANT_ID_LEN];

            // 1. Alice offers Bob custody under grant id G. Bob now knows G —
            //    it is in the offer she signed and sent him.
            alice_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &alice,
                        offer_params(bob.actor_id(), CustodyScopeSet::Account),
                        now,
                    )
                })
                .expect("alice's offer");

            // 2. Bob counter-offers, REUSING G. Nothing refuses it: the ingest
            //    consults `held` only.
            let bob_offer = bob_cfg
                .with(|c| {
                    begin_offer(
                        c,
                        &bob,
                        offer_params(alice.actor_id(), CustodyScopeSet::Account),
                        now,
                    )
                })
                .expect("bob's counter-offer");
            let outcome = alice_cfg
                .with(|c| {
                    ingest_payload(
                        c,
                        &alice.actor_id(),
                        &bob.actor_id(),
                        &channel,
                        &bob_offer,
                        now,
                    )
                })
                .expect("alice ingests bob's counter-offer");
            assert!(
                matches!(outcome, IngestOutcome::OfferPending { .. }),
                "PREMISE: the counter-offer reusing a live `granted` id is accepted"
            );
            {
                let cfg = alice_cfg.snapshot().await.unwrap();
                assert_eq!(cfg.granted[0].grant_id, G.to_vec());
                assert_eq!(
                    cfg.held[0].grant_id,
                    G.to_vec(),
                    "PREMISE: one id, both sides of the same config"
                );
            }

            // 3. Alice consents (T16) — an ordinary gesture; mutual custody is
            //    the expected buddy topology.
            alice_cfg
                .with(|c| {
                    build_accept(
                        c,
                        &alice,
                        &G,
                        [0xC5u8; 32],
                        DeviceEndpoints {
                            node_id: [0xC5u8; 32],
                            lan_addrs: Vec::new(),
                            public_addrs: vec!["198.51.100.7:4433".into()],
                            relay_url: None,
                        },
                        DEFAULT_RETAINED_BYTES_CAP,
                        None,
                        now,
                    )
                })
                .expect("alice accepts bob's offer");

            // 4. Two drive passes. Arm 5 posts the accept and marks it — but
            //    the mark lands on the shadowing `granted` record's variant and
            //    evaporates, so the second pass posts the very same accept
            //    again. Unbounded, silent, and counted as work.
            let (poster, writer, depositor) = (
                MemPoster::default(),
                MemWriter::default(),
                MemDepositor::default(),
            );
            for pass in 0..2 {
                drive_ceremonies(
                    &alice_cfg,
                    alice_cfg.ledger(),
                    &alice,
                    &poster,
                    &writer,
                    &depositor,
                    &MemHosting::default(),
                    now,
                )
                .await
                .unwrap_or_else(|e| panic!("drive pass {pass}: {e}"));
            }

            let accepts = poster
                .posted
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, b)| {
                    matches!(
                        decode_ceremony_message(b),
                        Ok(CustodyCeremonyMessage::Accept(_))
                    )
                })
                .count();
            assert_eq!(
                accepts, 1,
                "PROBE-375-A: the accept must be posted ONCE — a host-side mark \
                 that evaporates because `granted` shadows `held` re-posts it \
                 on every drive pass, forever"
            );
            assert!(
                alice_cfg.snapshot().await.unwrap().held[0].accept_posted,
                "PROBE-375-A: `accept_posted` must be markable on the held record \
                 whatever the owner side keys on"
            );
        });
    }

    // ── A fresher deliver stays inside the host's consent ──

    const T0: Timestamp = Timestamp(1_000_000_000);
    const GID: [u8; 16] = [0x1D; 16];

    /// A host that has ACCEPTED an `Account` offer — narrowed to `narrowed`,
    /// on its own device or (`nest_form`) its nest — with no deliver yet.
    /// Returns the host config and the custodian key the accept bound.
    fn accepted_host(nest_form: bool, narrowed: Option<CustodyScopeSet>) -> (MemConfig, [u8; 32]) {
        let (o_kp, h_kp) = (owner_kp(), host_kp());
        let mut owner_cfg = CustodyConfig::default();
        let offer_bytes = begin_offer(
            &mut owner_cfg,
            &o_kp,
            offer_params(h_kp.actor_id(), CustodyScopeSet::Account),
            T0,
        )
        .expect("offer");
        let host_cfg = MemConfig::new(h_kp.actor_id());
        host_cfg
            .with(|c| {
                ingest_payload(
                    c,
                    &h_kp.actor_id(),
                    &o_kp.actor_id(),
                    &"aa".repeat(32),
                    &offer_bytes,
                    T0,
                )
            })
            .expect("ingest offer");
        let key = [0xC5u8; 32];
        host_cfg
            .with(|c| {
                if nest_form {
                    build_accept_nest(
                        c,
                        &h_kp,
                        &GID,
                        key,
                        "https://my-own-nest.example".into(),
                        DEFAULT_RETAINED_BYTES_CAP,
                        narrowed,
                        T0,
                    )
                } else {
                    build_accept(
                        c,
                        &h_kp,
                        &GID,
                        key,
                        DeviceEndpoints {
                            node_id: key,
                            ..Default::default()
                        },
                        DEFAULT_RETAINED_BYTES_CAP,
                        narrowed,
                        T0,
                    )
                }
            })
            .expect("accept");
        (host_cfg, key)
    }

    /// A deliver the OWNER validly signed, carrying a witness it minted with
    /// exactly these scopes and window — what a modified owner app can send.
    fn crafted_deliver(
        custodian_key: [u8; 32],
        scopes: CustodyScopeSet,
        minted_at: Timestamp,
        expires_at: Timestamp,
        owner_devices: Vec<DeviceEndpoints>,
    ) -> Vec<u8> {
        let o_kp = owner_kp();
        let witness = sign_custody_grant(
            &o_kp,
            &CustodyGrant {
                grant_id: GID.to_vec(),
                owner: o_kp.actor_id(),
                custodian_key,
                scopes,
                minted_at,
                expires_at,
                removed_devices: Vec::new(),
            },
        )
        .unwrap();
        let env = sign_custody_deliver(
            &o_kp,
            &CustodyDeliver {
                grant_id: GID.to_vec(),
                owner: o_kp.actor_id(),
                witness,
                owner_devices,
                owner_nest_url: Some("https://nest.example/".into()),
            },
        )
        .unwrap();
        encode_ceremony_message(&CustodyCeremonyMessage::Deliver(env)).unwrap()
    }

    /// The honest witness window: minted at `T0`, the offer's full term.
    fn honest_window() -> (Timestamp, Timestamp) {
        (
            T0,
            Timestamp(T0.0 + crate::DEFAULT_GRANT_WINDOW_SECS * 1_000_000),
        )
    }

    fn candidates(tag: u8) -> Vec<DeviceEndpoints> {
        vec![DeviceEndpoints {
            node_id: [tag; 32],
            lan_addrs: vec![format!("192.168.1.{tag}:4433")],
            ..Default::default()
        }]
    }

    fn host_ingest(
        cfg: &MemConfig,
        bytes: &[u8],
        now: Timestamp,
    ) -> Result<IngestOutcome, CustodyCeremonyError> {
        cfg.with(|c| {
            ingest_payload(
                c,
                &host_kp().actor_id(),
                &owner_kp().actor_id(),
                &"aa".repeat(32),
                bytes,
                now,
            )
        })
    }

    /// One host drive; returns the fleet rows and the hosting deposits it wrote.
    async fn host_drive(
        cfg: &MemConfig,
        now: Timestamp,
    ) -> (
        Vec<CustodyHeld>,
        Vec<crate::custody_hosting::HostingDeposit>,
    ) {
        let (writer, hosting) = (MemWriter::default(), MemHosting::default());
        drive_ceremonies(
            cfg,
            cfg.ledger(),
            &host_kp(),
            &MemPoster::default(),
            &writer,
            &MemDepositor::default(),
            &hosting,
            now,
        )
        .await
        .expect("host drive");
        let held = writer.held.lock().unwrap().clone();
        let deposits = hosting.deposits.lock().unwrap().clone();
        (held, deposits)
    }

    /// Criterion 1: a deliver on a REMOVED or DECLINED record
    /// never re-arms a runtime row — refused at ingest, and arm (6) skips the
    /// record even when a merge left its row mark open. Both forms.
    #[test]
    fn a_deliver_on_a_removed_or_declined_custody_never_rearms_it() {
        block_on(async {
            for nest_form in [false, true] {
                let (host_cfg, key) = accepted_host(nest_form, None);
                let (m, e) = honest_window();
                host_ingest(
                    &host_cfg,
                    &crafted_deliver(key, CustodyScopeSet::Account, m, e, candidates(1)),
                    T0,
                )
                .expect("the first deliver is held");
                host_drive(&host_cfg, T0).await;
                // The host reclaims it (the act marks the record after the
                // teardown succeeded).
                assert!(host_cfg.with(|c| mark_custody_removed(c, &GID, T0)));

                let fresher = crafted_deliver(key, CustodyScopeSet::Account, m, e, candidates(2));
                host_ingest(&host_cfg, &fresher, T0)
                    .expect_err("a fresher deliver must not re-open a reclaimed custody");
                let (held, deposits) = host_drive(&host_cfg, T0).await;
                assert!(
                    held.is_empty() && deposits.is_empty(),
                    "nest_form={nest_form}: a removed custody must never be re-armed"
                );

                // The merge shape: another device's record left the row mark
                // open on a removed (or declined) record. Still nothing.
                for decline in [false, true] {
                    host_cfg.raw(|c| {
                        let h = &mut c.held[0];
                        h.held_row_written = false;
                        h.removed = !decline;
                        h.declined = decline;
                    });
                    let (held, deposits) = host_drive(&host_cfg, T0).await;
                    assert!(
                        held.is_empty() && deposits.is_empty(),
                        "nest_form={nest_form} declined={decline}: arm (6) must skip a \
                         terminal record whatever its row mark says"
                    );
                    host_ingest(&host_cfg, &fresher, T0)
                        .expect_err("nor may a deliver be captured on it");
                }
            }
        });
    }

    /// Criterion 2: a (first or superseding) deliver whose
    /// witness reaches past the accepted scope set or the offer's term is
    /// refused at ingest — while a superseding deliver that only refreshes
    /// the owner's candidates under the SAME window is still taken (the
    /// legitimate re-deliver this fix must not break).
    #[test]
    fn a_deliver_widening_the_accepted_scope_or_term_is_refused() {
        block_on(async {
            let accepted = CustodyScopeSet::Scopes(vec!["state".into()]);
            let (host_cfg, key) = accepted_host(false, Some(accepted.clone()));
            let (m, e) = honest_window();
            let term = crate::DEFAULT_GRANT_WINDOW_SECS * 1_000_000;
            let skew = CUSTODY_WITNESS_MINT_SKEW_SECS * 1_000_000;

            // Wider scope — the Account form, or a list naming an extra scope.
            for wider in [
                CustodyScopeSet::Account,
                CustodyScopeSet::Scopes(vec!["state".into(), "profile".into()]),
            ] {
                host_ingest(
                    &host_cfg,
                    &crafted_deliver(key, wider.clone(), m, e, candidates(1)),
                    T0,
                )
                .expect_err(&format!("{wider:?} is wider than the accepted set"));
            }
            // A longer term than the offer's, whatever minted_at claims.
            for (minted, expires) in [
                (m, Timestamp(T0.0 + term + skew + 1)),
                (
                    Timestamp(T0.0 + 400 * 24 * 3600 * 1_000_000),
                    Timestamp(T0.0 + 400 * 24 * 3600 * 1_000_000 + term),
                ),
            ] {
                host_ingest(
                    &host_cfg,
                    &crafted_deliver(key, accepted.clone(), minted, expires, candidates(1)),
                    T0,
                )
                .expect_err("a witness outliving the offered term is refused");
            }
            assert!(
                host_cfg.with(|c| c.held[0].deliver.is_empty()),
                "no refused deliver is captured"
            );

            // The honest deliver — a clock a little ahead is absorbed.
            let ahead = Timestamp(e.0 + skew / 2);
            host_ingest(
                &host_cfg,
                &crafted_deliver(key, accepted.clone(), m, ahead, candidates(1)),
                T0,
            )
            .expect("an honest witness within the skew allowance is held");

            // Superseding: a candidate refresh under the same window is taken…
            let later = Timestamp(T0.0 + 10 * 24 * 3600 * 1_000_000);
            let refresh = crafted_deliver(key, accepted.clone(), m, ahead, candidates(2));
            assert!(matches!(
                host_ingest(&host_cfg, &refresh, later),
                Ok(IngestOutcome::WitnessHeld { .. })
            ));
            // …a narrower one too; but a longer term never, even inside the
            // offered duration measured from the later ingest.
            host_ingest(
                &host_cfg,
                &crafted_deliver(
                    key,
                    accepted.clone(),
                    later,
                    Timestamp(later.0 + term),
                    candidates(3),
                ),
                later,
            )
            .expect_err("a superseding deliver may not extend the held term");
            host_ingest(
                &host_cfg,
                &crafted_deliver(key, CustodyScopeSet::Account, m, ahead, candidates(3)),
                later,
            )
            .expect_err("nor widen the scope");
            let held = host_cfg.with(|c| decode_deliver_record(&c.held[0]).unwrap());
            assert_eq!(
                held.owner_devices,
                candidates(2),
                "the refresh is what stands"
            );
        });
    }

    /// Criterion 3: after the host's Stop or budget change, a
    /// fresher deliver re-derives the runtime row with the host's knobs —
    /// read from the RECORD, never reset to the accept's. Both forms.
    #[test]
    fn a_fresher_deliver_keeps_the_hosts_stop_and_budget() {
        block_on(async {
            for nest_form in [false, true] {
                let (host_cfg, key) = accepted_host(nest_form, None);
                let (m, e) = honest_window();
                host_ingest(
                    &host_cfg,
                    &crafted_deliver(key, CustodyScopeSet::Account, m, e, candidates(1)),
                    T0,
                )
                .unwrap();
                host_drive(&host_cfg, T0).await;

                // The host stops the custody at a smaller budget (the act's
                // record half; the act then rewrites the row).
                let cap = DEFAULT_RETAINED_BYTES_CAP / 4;
                assert!(host_cfg.with(|c| record_host_knobs(c, &GID, cap, true, T0)));

                // The owner re-delivers (a candidate refresh).
                host_ingest(
                    &host_cfg,
                    &crafted_deliver(key, CustodyScopeSet::Account, m, e, candidates(2)),
                    T0,
                )
                .expect("a candidate refresh is still taken");
                let (held, deposits) = host_drive(&host_cfg, T0).await;
                let (row_cap, row_stopped) = if nest_form {
                    assert!(held.is_empty());
                    let d = deposits.last().expect("the hosting row is re-registered");
                    (d.retained_bytes_cap, d.stopped)
                } else {
                    assert!(deposits.is_empty());
                    let r = held.last().expect("the fleet row is re-written");
                    assert_eq!(r.owner_devices, candidates(2), "the refresh landed");
                    (r.retained_bytes_cap, r.stopped)
                };
                assert!(
                    row_stopped,
                    "nest_form={nest_form}: the host's Stop survives"
                );
                assert_eq!(
                    row_cap, cap,
                    "nest_form={nest_form}: the host's budget survives"
                );
            }
        });
    }

    // ── The held-offer bound ──

    /// One owner's signed offer under grant id `[id; 16]`, offered at `at`
    /// with a shelf life of `duration_secs` — the channel bytes the host
    /// ingests.
    fn held_bound_offer(
        owner: &ActorKeypair,
        host: ActorId,
        id: u8,
        duration_secs: u64,
        at: Timestamp,
    ) -> Vec<u8> {
        let mut scratch = CustodyConfig::default();
        let mut params = offer_params(host, CustodyScopeSet::Account);
        params.grant_id = [id; CUSTODY_GRANT_ID_LEN];
        params.duration_secs = duration_secs;
        begin_offer(&mut scratch, owner, params, at).expect("begin the offer")
    }

    fn host_takes(
        cfg: &MemConfig,
        h: &ActorKeypair,
        sender: &ActorKeypair,
        bytes: &[u8],
        now: Timestamp,
    ) -> Result<IngestOutcome, CustodyCeremonyError> {
        cfg.with(|c| {
            ingest_payload(
                c,
                &h.actor_id(),
                &sender.actor_id(),
                &"aa".repeat(32),
                bytes,
                now,
            )
        })
    }

    /// A peer sharing a channel with the host cannot grow the host's
    /// fleet-synced ceremony state and consent surface without bound: past
    /// [`MAX_PENDING_HELD_OFFERS_PER_OWNER`] unanswered offers from ONE owner
    /// the next is refused and nothing is persisted. The cap is per owner (a
    /// second owner is untouched), a re-delivered offer stays `Duplicate` at
    /// the cap, and the host's own decline frees the slot.
    #[test]
    fn unanswered_offers_past_the_per_owner_cap_are_refused_and_not_persisted() {
        let (o, h) = (owner_kp(), host_kp());
        let other = ActorKeypair::from_secret([11u8; 32]);
        let now = Timestamp(1_000_000_000);
        let host_cfg = MemConfig::new(h.actor_id());
        let day = 24 * 3600;

        let mut first = Vec::new();
        for id in 1..=MAX_PENDING_HELD_OFFERS_PER_OWNER as u8 {
            let bytes = held_bound_offer(&o, h.actor_id(), id, 30 * day, now);
            let out = host_takes(&host_cfg, &h, &o, &bytes, now).expect("under the cap");
            assert!(matches!(out, IngestOutcome::OfferPending { .. }));
            if id == 1 {
                first = bytes;
            }
        }
        let before = host_cfg.0.current();
        let over = held_bound_offer(&o, h.actor_id(), 0xEE, 30 * day, now);
        let err = host_takes(&host_cfg, &h, &o, &over, now).expect_err("one past the cap");
        assert!(
            matches!(err, CustodyCeremonyError::TooManyPendingOffers { .. }),
            "{err}"
        );
        assert_eq!(
            host_cfg.0.current(),
            before,
            "a refused offer persists nothing"
        );
        assert_eq!(
            crate::view_model::custody_offers(&host_cfg.0.current(), now.0).len(),
            MAX_PENDING_HELD_OFFERS_PER_OWNER,
            "the consent surface stays at the cap"
        );

        // A re-delivered offer at the cap is the same idempotent no-op.
        assert_eq!(
            host_takes(&host_cfg, &h, &o, &first, now).unwrap(),
            IngestOutcome::Duplicate
        );
        // The cap is per owner: another account's offer still lands.
        let theirs = held_bound_offer(&other, h.actor_id(), 0xDD, 30 * day, now);
        assert!(matches!(
            host_takes(&host_cfg, &h, &other, &theirs, now).unwrap(),
            IngestOutcome::OfferPending { .. }
        ));
        // The host's decline answers the offer and frees its slot.
        assert!(host_cfg.with(|c| decline_offer(c, &[1u8; CUSTODY_GRANT_ID_LEN], now)));
        assert!(matches!(
            host_takes(&host_cfg, &h, &o, &over, now).unwrap(),
            IngestOutcome::OfferPending { .. }
        ));
    }

    /// An offer whose term has passed with no answer is spent on the host
    /// exactly as the owner's decay treats it: off the consent surface, no
    /// longer acceptable, and its slot free — but only once the slot floor
    /// [`HELD_OFFER_SLOT_FLOOR_SECS`] has also passed on the host's own
    /// clock, so an owner-chosen tiny term cannot turn the cap into a
    /// revolving door. An offer already expired when it arrives (a re-walk
    /// re-feeding old channel history) is refused, never persisted.
    #[test]
    fn an_expired_unanswered_offer_leaves_the_consent_surface_and_frees_its_slot() {
        let (o, h) = (owner_kp(), host_kp());
        let t0 = Timestamp(1_000_000_000);
        let secs = |s: u64| Timestamp(t0.0 + s * 1_000_000);
        let host_cfg = MemConfig::new(h.actor_id());
        let short = 60;

        for id in 1..=MAX_PENDING_HELD_OFFERS_PER_OWNER as u8 {
            let bytes = held_bound_offer(&o, h.actor_id(), id, short, t0);
            host_takes(&host_cfg, &h, &o, &bytes, t0).expect("under the cap");
        }
        // Past every term: the consent surface is empty and none accepts.
        let later = secs(short + 1);
        assert!(crate::view_model::custody_offers(&host_cfg.0.current(), later.0).is_empty());
        let mut state = host_cfg.0.current();
        let err = build_accept(
            &mut state,
            &h,
            &[1u8; CUSTODY_GRANT_ID_LEN],
            [3u8; 32],
            DeviceEndpoints {
                node_id: [3u8; 32],
                lan_addrs: Vec::new(),
                public_addrs: Vec::new(),
                relay_url: None,
            },
            DEFAULT_RETAINED_BYTES_CAP,
            None,
            later,
        )
        .expect_err("an expired offer cannot be accepted");
        assert!(matches!(err, CustodyCeremonyError::OfferExpired), "{err}");

        // Inside the slot floor the expired offers still hold their slots…
        let fresh = held_bound_offer(&o, h.actor_id(), 0xEE, short, later);
        assert!(matches!(
            host_takes(&host_cfg, &h, &o, &fresh, later).expect_err("floor holds the slot"),
            CustodyCeremonyError::TooManyPendingOffers { .. }
        ));
        // …and past it they free them.
        let past_floor = secs(HELD_OFFER_SLOT_FLOOR_SECS + 1);
        let fresh = held_bound_offer(&o, h.actor_id(), 0xEE, short, past_floor);
        assert!(matches!(
            host_takes(&host_cfg, &h, &o, &fresh, past_floor).unwrap(),
            IngestOutcome::OfferPending { .. }
        ));

        // An offer already past its term on arrival is refused unpersisted.
        let before = host_cfg.0.current();
        let stale = held_bound_offer(&o, h.actor_id(), 0xEF, short, t0);
        let err = host_takes(&host_cfg, &h, &o, &stale, past_floor).expect_err("stale on arrival");
        assert!(matches!(err, CustodyCeremonyError::OfferExpired), "{err}");
        assert_eq!(host_cfg.0.current(), before);
    }
}
