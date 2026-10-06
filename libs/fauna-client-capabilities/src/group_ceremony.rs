//! The group-share ceremony state machine — offer → accept → deliver →
//! admit, over the contact-plane peer channel (second
//! half's carrier-agnostic core).
//!
//! Authority: `docs/goal/behavior/p2p.md` § Offline share initiation
//! (contract point 1); mechanics: `docs/goal/architecture/account-data-plane.md`
//! § The audience ladder → *The recipient-set scheme*. The carrier-agnostic
//! payload shapes and their verification live in
//! `fauna_core::group_ceremony`; the durable state lives on
//! the account-plane `fauna.state.group-share-ceremony` record, a
//! `GroupShareConfig` (record-then-act — every consumed
//! peer-channel frame is captured before any action, and every owed action
//! re-derives from state alone). The custody sibling
//! ([`crate::custody_ceremony`]) is the shape template; the differences are
//! the ceremony's own:
//!
//! * **Scope birth happens at `begin`** — the machinery root and birth
//!   record are minted before the offer leaves, and the root rests in the
//!   ceremony record until its account-plane row
//!   (`fauna.state.group-machinery-root`) is written through, so no crash
//!   window strands a live offer whose scope nobody can read.
//! * **The mint happens at `deliver`** — only then does the initiator hold
//!   the recipient's reception key, so the first generation lists BOTH
//!   roster entries and wraps to both; the recipient's copy of everything
//!   (root + retained keys) travels as the admission bundle, and the
//!   machinery snapshot carries the plane rows verbatim.
//! * **Admission is verified end to end at the joiner** — [`admit_group_share`]
//!   re-derives the scope id from the delivered birth record, opens the
//!   bundle (root commitment checked in-door), verifies its own `Enrolled`
//!   entry against the authority chain, and resolves the mint DAG with a
//!   keyability closure that commitment-checks every retained key — the
//!   full resolver, not a trusting subset.
//!
//! Transport is deliberately not here: callers hand frames in and post the
//! bytes these functions hand back (PT-1b proves the sender's actor key —
//! the `sender` every ingest takes). Plane writes are the caller's seams
//! too, marked through the monotone progress booleans.

use ed25519_dalek::SigningKey;
use fauna_core::crypto::GroupMachineryRoot;
use fauna_core::data::Timestamp;
use fauna_core::encoding::canonical_encode;
use fauna_core::error::Error as CoreError;
use fauna_core::group_ceremony::GroupShareConfig;
use fauna_core::group_ceremony::{
    GroupCeremonyMessage, GroupPlaneRow, GroupShareAccept, GroupShareDeliver, GroupShareOffer,
    InitiatedGroupShare, InvitedGroupShare, decode_group_ceremony_message,
    encode_group_ceremony_message, group_offer_digest, sign_group_share_accept,
    sign_group_share_deliver, sign_group_share_offer, verify_group_share_accept,
    verify_group_share_deliver, verify_group_share_offer,
};
use fauna_core::group_generation::{
    GroupHeldRootRecord, GroupReceptionKeyRecord, group_generation_key_commitment,
    resolve_admissible_group_tip, sign_group_reception_published,
};
use fauna_core::group_scope::{
    GroupAuthority, GroupBirthRecord, GroupRosterRecord, RosterEntryCore, RosterMember, RosterView,
    decode_birth_for_scope, group_scope_id, parse_roster_cell_key, roster_cell_key,
    sign_roster_enrollment, verify_enrolled_entry,
};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::wrapped_blob::group_generation_wraps::{
    build_group_mint, open_group_admission_bundle, seal_group_admission_bundle,
};
use fauna_protocol::group_state::{
    GROUP_BIRTH_KEY, KIND_GROUP_AUTHORITY_REVOCATION, KIND_GROUP_BIRTH, KIND_GROUP_GENERATION_MINT,
    KIND_GROUP_ROSTER,
};

/// A ceremony transition was refused.
#[derive(Debug, thiserror::Error)]
pub enum GroupCeremonyError {
    /// A frame failed decode/signature/sender/addressee verification.
    #[error("group ceremony frame refused: {0}")]
    Payload(String),
    /// A frame named a ceremony this side has no (matching) record for.
    #[error("group ceremony frame does not match this side's record: {0}")]
    NoMatchingCeremony(String),
    /// A step ran out of order (accept before offer, deliver before accept,
    /// admit before deliver).
    #[error("group ceremony step out of order: {0}")]
    OutOfOrder(String),
    /// A wrap/seal step failed.
    #[error("group ceremony wrap step failed: {0}")]
    Wrap(String),
    /// A signing/encoding step failed.
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// What [`begin_group_share`] yields: the offer frame to post, and the
/// held-root row the driver writes through (marking `root_row_written`).
pub struct BegunGroupShare {
    /// The scope this ceremony mints.
    pub scope_id: [u8; 32],
    /// The encoded [`GroupCeremonyMessage::Offer`] frame.
    pub frame: Vec<u8>,
    /// The initiator's own `fauna.state.group-machinery-root` row.
    pub held_root_row: GroupHeldRootRecord,
}

/// Begin a share: mint the scope (machinery root + birth record), record
/// the ceremony (record-then-act: the root and the signed offer are durable
/// before the frame exists to post), and hand back the frame.
///
/// One ceremony per `(scope, recipient)`; every call mints a NEW scope —
/// adding a second member to an existing scope is a later affordance riding
/// the same accept/deliver steps, deliberately not built until the
/// two-party journey is proven.
pub fn begin_group_share(
    cfg: &mut GroupShareConfig,
    initiator: &ActorKeypair,
    recipient: ActorId,
    now: Timestamp,
) -> Result<BegunGroupShare, GroupCeremonyError> {
    let root = GroupMachineryRoot::mint();
    let birth = GroupBirthRecord {
        authority_actor: initiator.actor_id(),
        salt: fauna_core::crypto::random_salt_32(),
        machinery_root_commit: root.commitment(),
        created_at_ms: now_ms(now),
    };
    let scope_id = group_scope_id(&birth)?;
    let offer = GroupShareOffer {
        scope_id,
        initiator: initiator.actor_id(),
        recipient,
        birth: canonical_encode(&birth)?.to_vec(),
        offered_at: now,
    };
    let envelope = sign_group_share_offer(initiator, &offer)?;
    let offer_bytes = canonical_encode(&envelope)?.to_vec();
    cfg.initiated.push(InitiatedGroupShare {
        scope_id,
        recipient,
        root: fauna_core::secret::SecretByteBuf::from(root.as_bytes().to_vec()),
        offer: offer_bytes,
        offered_at: now,
        updated_at: now,
        ..Default::default()
    });
    let frame = encode_group_ceremony_message(&GroupCeremonyMessage::Offer(envelope))?;
    Ok(BegunGroupShare {
        scope_id,
        frame,
        held_root_row: GroupHeldRootRecord {
            scope_id,
            root: fauna_core::secret::SecretByteBuf::from(root.as_bytes().to_vec()),
            held_since_ms: now_ms(now),
        },
    })
}

/// The outcome of ingesting one peer-channel ceremony frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupIngestOutcome {
    /// An offer was recorded (or re-recorded idempotently); surface the
    /// consent card.
    OfferRecorded {
        scope_id: [u8; 32],
        initiator: ActorId,
    },
    /// An accept was recorded on an outstanding offer; the deliver step is
    /// now owed.
    AcceptRecorded {
        scope_id: [u8; 32],
        recipient: ActorId,
    },
    /// A deliver was recorded on an accepted invitation; the admit step is
    /// now owed.
    DeliverRecorded { scope_id: [u8; 32] },
}

/// Ingest one frame from the transport-proven `sender` (record-then-act:
/// the frame is captured verbatim before this returns; actions re-derive
/// from state).
pub fn ingest_group_frame(
    cfg: &mut GroupShareConfig,
    own_actor: &ActorId,
    sender: &ActorId,
    frame: &[u8],
    now: Timestamp,
) -> Result<GroupIngestOutcome, GroupCeremonyError> {
    let message = decode_group_ceremony_message(frame)
        .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;
    match message {
        GroupCeremonyMessage::Offer(envelope) => {
            let offer = verify_group_share_offer(&envelope, sender, own_actor)
                .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;
            let bytes = canonical_encode(&envelope)?.to_vec();
            match cfg
                .invited
                .iter_mut()
                .find(|r| r.scope_id == offer.scope_id)
            {
                Some(existing) => {
                    if existing.offer.is_empty() {
                        existing.offer = bytes;
                        existing.updated_at = now;
                    }
                }
                None => cfg.invited.push(InvitedGroupShare {
                    scope_id: offer.scope_id,
                    initiator: offer.initiator,
                    offer: bytes,
                    updated_at: now,
                    ..Default::default()
                }),
            }
            Ok(GroupIngestOutcome::OfferRecorded {
                scope_id: offer.scope_id,
                initiator: offer.initiator,
            })
        }
        GroupCeremonyMessage::Accept(envelope) => {
            let (accept, _published) = verify_group_share_accept(&envelope, sender)
                .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;
            let bytes = canonical_encode(&envelope)?.to_vec();
            let record = cfg
                .initiated
                .iter_mut()
                .find(|r| r.scope_id == accept.scope_id && r.recipient == accept.recipient)
                .ok_or_else(|| {
                    GroupCeremonyError::NoMatchingCeremony(
                        "accept names a scope/recipient this side never offered".into(),
                    )
                })?;
            // The digest must bind to the offer THIS record holds — a
            // replayed accept against a re-offer fails here.
            let recorded_offer: fauna_core::encoding::EmbedAsBytes =
                fauna_core::encoding::canonical_decode(&record.offer)?;
            if group_offer_digest(&recorded_offer)? != accept.offer_digest {
                return Err(GroupCeremonyError::NoMatchingCeremony(
                    "accept's digest binds a different offer".into(),
                ));
            }
            if record.accept.is_empty() {
                record.accept = bytes;
                record.updated_at = now;
            }
            Ok(GroupIngestOutcome::AcceptRecorded {
                scope_id: accept.scope_id,
                recipient: accept.recipient,
            })
        }
        GroupCeremonyMessage::Deliver(envelope) => {
            let deliver = verify_group_share_deliver(&envelope, sender)
                .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;
            let bytes = canonical_encode(&envelope)?.to_vec();
            let record = cfg
                .invited
                .iter_mut()
                .find(|r| r.scope_id == deliver.scope_id && r.initiator == *sender)
                .ok_or_else(|| {
                    GroupCeremonyError::NoMatchingCeremony(
                        "deliver names a scope this side was never offered by this sender".into(),
                    )
                })?;
            if record.accept.is_empty() {
                return Err(GroupCeremonyError::OutOfOrder(
                    "deliver arrived before this side accepted".into(),
                ));
            }
            if record.deliver.is_empty() {
                record.deliver = bytes;
                record.updated_at = now;
            }
            Ok(GroupIngestOutcome::DeliverRecorded {
                scope_id: deliver.scope_id,
            })
        }
    }
}

/// Decline an offered share — monotone; the invitation stays dismissed
/// fleet-wide. Returns whether a record was found.
pub fn decline_group_offer(
    cfg: &mut GroupShareConfig,
    scope_id: &[u8; 32],
    now: Timestamp,
) -> bool {
    match cfg.invited.iter_mut().find(|r| r.scope_id == *scope_id) {
        Some(r) => {
            r.declined = true;
            r.updated_at = now;
            true
        }
        None => false,
    }
}

/// Accept an offered share: sign the published reception half for
/// `reception` (the recipient's current — or freshly minted — keypair
/// record, whose plane row the caller owns), bind the recorded offer by
/// digest, record, and hand back the frame to post.
pub fn build_group_accept(
    cfg: &mut GroupShareConfig,
    recipient: &ActorKeypair,
    scope_id: &[u8; 32],
    reception: &GroupReceptionKeyRecord,
    now: Timestamp,
) -> Result<Vec<u8>, GroupCeremonyError> {
    let record = cfg
        .invited
        .iter_mut()
        .find(|r| r.scope_id == *scope_id)
        .ok_or_else(|| {
            GroupCeremonyError::NoMatchingCeremony("no invitation for this scope".into())
        })?;
    if record.declined {
        return Err(GroupCeremonyError::OutOfOrder(
            "this invitation was declined".into(),
        ));
    }
    if record.offer.is_empty() {
        return Err(GroupCeremonyError::OutOfOrder(
            "no offer recorded for this scope".into(),
        ));
    }
    let offer_envelope: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&record.offer)?;
    let reception_published =
        sign_group_reception_published(recipient, reception.reception_pubkey()?, now_ms(now))?;
    let accept = GroupShareAccept {
        scope_id: *scope_id,
        offer_digest: group_offer_digest(&offer_envelope)?,
        recipient: recipient.actor_id(),
        reception_published,
        accepted_at: now,
    };
    let envelope = sign_group_share_accept(recipient, &accept)?;
    record.accept = canonical_encode(&envelope)?.to_vec();
    record.updated_at = now;
    encode_group_ceremony_message(&GroupCeremonyMessage::Accept(envelope))
        .map_err(GroupCeremonyError::from)
}

/// What [`build_group_deliver`] yields: the frame, plus everything the
/// initiator's own side must write (its plane rows and held-root row — the
/// same machinery the snapshot hands the joiner).
pub struct BuiltGroupShareDeliver {
    /// The encoded [`GroupCeremonyMessage::Deliver`] frame.
    pub frame: Vec<u8>,
    /// The machinery rows (birth, both roster entries, the first mint) —
    /// the initiator writes these to its OWN group plane (marking
    /// `plane_rows_written`); the identical rows ride the frame's snapshot.
    pub plane_rows: Vec<GroupPlaneRow>,
    /// The initiator's own roster cell.
    pub own_entry_id: [u8; 32],
    /// The first generation's content-derived id.
    pub generation_id: [u8; 32],
}

/// Build the deliver: mint the roster (both entries), the first generation
/// (wrapped to both), and the recipient's admission bundle; record; hand
/// back the frame + the initiator's own writes.
///
/// `authority_device` is the initiating account's device principal signing
/// key and `device_authorization` its `DeviceAuthorization` carriage (the
/// v1 authority seam: roster entries and mints are authority-device-signed
/// with the chain carried inline). `own_reception` is the initiator's own
/// reception keypair record — the initiator is a member too.
#[allow(clippy::too_many_arguments)]
pub fn build_group_deliver(
    cfg: &mut GroupShareConfig,
    initiator: &ActorKeypair,
    authority_device: &SigningKey,
    device_authorization: Vec<u8>,
    own_reception: &GroupReceptionKeyRecord,
    scope_id: &[u8; 32],
    recipient: &ActorId,
    now: Timestamp,
) -> Result<BuiltGroupShareDeliver, GroupCeremonyError> {
    let record = cfg
        .initiated
        .iter_mut()
        .find(|r| r.scope_id == *scope_id && r.recipient == *recipient)
        .ok_or_else(|| {
            GroupCeremonyError::NoMatchingCeremony("no offered ceremony for this pair".into())
        })?;
    if record.accept.is_empty() {
        return Err(GroupCeremonyError::OutOfOrder(
            "no accept recorded — the deliver is not owed yet".into(),
        ));
    }
    let accept_envelope: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&record.accept)?;
    let (_accept, published) = verify_group_share_accept(&accept_envelope, recipient)
        .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;
    let offer_envelope: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&record.offer)?;
    let offer = verify_group_share_offer(&offer_envelope, &initiator.actor_id(), recipient)
        .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;

    // Both roster entries, authority-device-signed with the carried chain —
    // and BOUND: the authority's binding signature ties each reception key
    // to its entry (the recipient's key arrived member-signed in the accept,
    // verified above; the initiator's is its own). The one shape a roster
    // entry has since the bound roster entry ruling (2026-09-16).
    let build_entry = |member_actor: ActorId,
                       reception_pubkey: Vec<u8>|
     -> Result<(RosterEntryCore, [u8; 32], Vec<u8>), GroupCeremonyError> {
        let core = RosterEntryCore {
            scope_id: *scope_id,
            member_actor,
            admission_salt: fauna_core::crypto::random_salt_32(),
        };
        let (entry_id, record) = sign_roster_enrollment(
            authority_device,
            core.clone(),
            reception_pubkey,
            device_authorization.clone(),
            now_ms(now),
        )?;
        let value = canonical_encode(&record)?.to_vec();
        Ok((core, entry_id, value))
    };
    let own_pubkey = own_reception.reception_pubkey()?;
    let (_own_core, own_entry_id, own_row) = build_entry(initiator.actor_id(), own_pubkey.clone())?;
    let (_their_core, their_entry_id, their_row) =
        build_entry(*recipient, published.reception_pubkey.clone())?;

    // The first generation: wrapped to both entries' reception keys.
    let members = [
        RosterMember {
            entry_id: own_entry_id,
            member_actor: initiator.actor_id(),
            reception_pubkey: own_pubkey,
            enrolled_at_ms: now_ms(now),
        },
        RosterMember {
            entry_id: their_entry_id,
            member_actor: *recipient,
            reception_pubkey: published.reception_pubkey.clone(),
            enrolled_at_ms: now_ms(now),
        },
    ];
    let built = build_group_mint(
        &members,
        Vec::new(),
        // Birth: nothing is revoked yet, so the first mint is past nothing.
        Vec::new(),
        authority_device,
        device_authorization,
        now_ms(now),
    )
    .map_err(|e| GroupCeremonyError::Wrap(e.to_string()))?;
    let authority_device_id = authority_device.verifying_key().to_bytes();

    // The recipient's admission bundle: root + the (only) retained
    // generation, sealed to their reception key at their entry slot.
    let root = held_root(record)?;
    let admission_wrap = seal_group_admission_bundle(
        &root,
        &[(built.generation_id, &built.gen_key)],
        &published.reception_pubkey,
        scope_id,
        &their_entry_id,
    )
    .map_err(|e| GroupCeremonyError::Wrap(e.to_string()))?;

    let plane_rows = vec![
        GroupPlaneRow {
            kind: KIND_GROUP_BIRTH.into(),
            key: GROUP_BIRTH_KEY.into(),
            value: offer.birth.clone(),
        },
        GroupPlaneRow {
            kind: KIND_GROUP_ROSTER.into(),
            key: roster_cell_key(&own_entry_id, &authority_device_id),
            value: own_row,
        },
        GroupPlaneRow {
            kind: KIND_GROUP_ROSTER.into(),
            key: roster_cell_key(&their_entry_id, &authority_device_id),
            value: their_row,
        },
        GroupPlaneRow {
            kind: KIND_GROUP_GENERATION_MINT.into(),
            key: fauna_core::hex32::encode(&built.generation_id),
            value: canonical_encode(&built.record)?.to_vec(),
        },
    ];

    let deliver = GroupShareDeliver {
        scope_id: *scope_id,
        initiator: initiator.actor_id(),
        roster_entry_id: their_entry_id,
        admission_wrap,
        machinery_snapshot: plane_rows.clone(),
        delivered_at: now,
    };
    let envelope = sign_group_share_deliver(initiator, &deliver)?;
    record.deliver = canonical_encode(&envelope)?.to_vec();
    record.updated_at = now;
    let frame = encode_group_ceremony_message(&GroupCeremonyMessage::Deliver(envelope))?;
    Ok(BuiltGroupShareDeliver {
        frame,
        plane_rows,
        own_entry_id,
        generation_id: built.generation_id,
    })
}

/// What [`admit_group_share`] yields: everything the joiner's driver writes
/// through (marking the two monotone booleans as each lands).
pub struct AdmittedGroupShare {
    /// The joiner's `fauna.state.group-machinery-root` row.
    pub held_root_row: GroupHeldRootRecord,
    /// The machinery snapshot to adopt into the joiner's own group plane —
    /// verbatim, through `apply_class2`'s ordinary first-contact strictness.
    pub rows: Vec<GroupPlaneRow>,
    /// The joiner's own roster cell.
    pub entry_id: [u8; 32],
    /// The resolved (and keyability-verified) tip.
    pub generation_id: [u8; 32],
}

/// Admit from a recorded deliver: re-derive the scope id from the delivered
/// birth record, open the admission bundle (root commitment checked
/// in-door), verify our own `Enrolled` entry against the authority chain,
/// and resolve the delivered mint DAG with a keyability closure that
/// commitment-checks every retained key — refusing a delivery whose
/// machinery does not actually admit us.
pub fn admit_group_share(
    cfg: &GroupShareConfig,
    own: &ActorKeypair,
    reception: &GroupReceptionKeyRecord,
    scope_id: &[u8; 32],
    now: Timestamp,
) -> Result<AdmittedGroupShare, GroupCeremonyError> {
    let record = cfg
        .invited
        .iter()
        .find(|r| r.scope_id == *scope_id)
        .ok_or_else(|| {
            GroupCeremonyError::NoMatchingCeremony("no invitation for this scope".into())
        })?;
    if record.deliver.is_empty() {
        return Err(GroupCeremonyError::OutOfOrder(
            "no deliver recorded — nothing to admit from".into(),
        ));
    }
    let deliver_envelope: fauna_core::encoding::EmbedAsBytes =
        fauna_core::encoding::canonical_decode(&record.deliver)?;
    let deliver = verify_group_share_deliver(&deliver_envelope, &record.initiator)
        .map_err(|e| GroupCeremonyError::Payload(e.to_string()))?;

    // The birth row anchors everything: the scope id must be ITS
    // content-derived id, never the ceremony's claim.
    let birth_row = deliver
        .machinery_snapshot
        .iter()
        .find(|r| r.kind == KIND_GROUP_BIRTH && r.key == GROUP_BIRTH_KEY)
        .ok_or_else(|| {
            GroupCeremonyError::Payload("deliver snapshot carries no birth row".into())
        })?;
    let birth = decode_birth_for_scope(&birth_row.value, scope_id)
        .map_err(|e| GroupCeremonyError::Payload(format!("delivered {e}")))?;

    // Open the bundle — the root commitment check runs in-door.
    let keypair = reception.keypair()?;
    let (root, retained) = open_group_admission_bundle(
        &deliver.admission_wrap,
        &keypair.secret,
        scope_id,
        &deliver.roster_entry_id,
        &birth,
    )
    .map_err(|e| GroupCeremonyError::Wrap(format!("admission bundle: {e}")))?;

    // The authority line as this snapshot states it: the birth record's
    // authority, plus every authority-device revocation the deliver carries
    // (prior identities: none are known cross-account at admission; a
    // succession-crossing entry re-verifies at the next sync). Built once, so
    // the entry, the roster view and the mint resolver answer to ONE line.
    let revocation_rows: Vec<(&str, &[u8])> = deliver
        .machinery_snapshot
        .iter()
        .filter(|r| r.kind == KIND_GROUP_AUTHORITY_REVOCATION)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let authority = GroupAuthority::build(
        scope_id,
        &birth.authority_actor,
        &[],
        revocation_rows.iter().copied(),
    );

    // Our own Enrolled entry, verified on its own terms against that line.
    // The cell is `<entry>/<author>`; the deliver names the entry, and the
    // authoring device is whichever the initiator's row carries.
    let own_row = deliver
        .machinery_snapshot
        .iter()
        .find(|r| {
            r.kind == KIND_GROUP_ROSTER
                && parse_roster_cell_key(&r.key)
                    .is_some_and(|(entry, _)| entry == deliver.roster_entry_id)
        })
        .ok_or_else(|| {
            GroupCeremonyError::Payload("deliver snapshot carries no row at our entry".into())
        })?;
    let own_record: GroupRosterRecord = fauna_core::encoding::canonical_decode(&own_row.value)?;
    let member = verify_enrolled_entry(&own_record, scope_id, &authority)
        .map_err(GroupCeremonyError::Payload)?;
    if member.member_actor != own.actor_id() {
        return Err(GroupCeremonyError::Payload(
            "the delivered cell enrolls a different actor".into(),
        ));
    }
    if member.entry_id != deliver.roster_entry_id {
        return Err(GroupCeremonyError::Payload(
            "the delivered cell is not the entry's content-derived id".into(),
        ));
    }

    // Resolve the delivered DAG exactly as a member will at every read:
    // roster view + coverage admissibility + keyability through the
    // retained keys' commitments. A delivery that does not resolve a tip we
    // can key admits us to nothing — refused here, not discovered later.
    let roster_rows: Vec<(&str, &[u8])> = deliver
        .machinery_snapshot
        .iter()
        .filter(|r| r.kind == KIND_GROUP_ROSTER)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let view = RosterView::build(scope_id, &authority, roster_rows.iter().copied());
    let mint_rows: Vec<(&str, &[u8])> = deliver
        .machinery_snapshot
        .iter()
        .filter(|r| r.kind == KIND_GROUP_GENERATION_MINT)
        .map(|r| (r.key.as_str(), r.value.as_slice()))
        .collect();
    let resolution = resolve_admissible_group_tip(
        &view,
        &authority,
        mint_rows.iter().copied(),
        std::iter::empty(),
        |id, core, _wraps| {
            retained.iter().any(|(rid, key)| {
                rid == id && group_generation_key_commitment(key) == core.key_commitment
            })
        },
    );
    let tip = resolution.tip.ok_or_else(|| {
        GroupCeremonyError::Payload(format!(
            "delivered machinery resolves no keyable tip (invalid rows: {:?})",
            resolution.invalid
        ))
    })?;

    Ok(AdmittedGroupShare {
        held_root_row: GroupHeldRootRecord {
            scope_id: *scope_id,
            root: fauna_core::secret::SecretByteBuf::from(root.as_bytes().to_vec()),
            held_since_ms: now_ms(now),
        },
        rows: deliver.machinery_snapshot.clone(),
        entry_id: deliver.roster_entry_id,
        generation_id: tip.generation_id,
    })
}

// ── Monotone progress marks (the custody `apply_mark` idiom) ────────────────

/// Mark the initiator-side offer as posted.
pub fn mark_group_offer_posted(
    cfg: &mut GroupShareConfig,
    scope_id: &[u8; 32],
    recipient: &ActorId,
) {
    if let Some(r) = initiated_mut(cfg, scope_id, recipient) {
        r.offer_posted = true;
    }
}

/// Mark the initiator-side deliver as posted.
pub fn mark_group_delivered(cfg: &mut GroupShareConfig, scope_id: &[u8; 32], recipient: &ActorId) {
    if let Some(r) = initiated_mut(cfg, scope_id, recipient) {
        r.delivered = true;
    }
}

/// Mark the initiator's held-root row as written through.
pub fn mark_group_root_row_written(
    cfg: &mut GroupShareConfig,
    scope_id: &[u8; 32],
    recipient: &ActorId,
) {
    if let Some(r) = initiated_mut(cfg, scope_id, recipient) {
        r.root_row_written = true;
    }
}

/// Mark the initiator's machinery rows as written to its own plane.
pub fn mark_group_plane_rows_written(
    cfg: &mut GroupShareConfig,
    scope_id: &[u8; 32],
    recipient: &ActorId,
) {
    if let Some(r) = initiated_mut(cfg, scope_id, recipient) {
        r.plane_rows_written = true;
    }
}

/// Mark the recipient-side accept as posted.
pub fn mark_group_accept_posted(cfg: &mut GroupShareConfig, scope_id: &[u8; 32]) {
    if let Some(r) = invited_mut(cfg, scope_id) {
        r.accept_posted = true;
    }
}

/// Mark the joiner's held-root row as written through.
pub fn mark_group_invited_root_written(cfg: &mut GroupShareConfig, scope_id: &[u8; 32]) {
    if let Some(r) = invited_mut(cfg, scope_id) {
        r.root_row_written = true;
    }
}

/// Mark the joiner's snapshot as adopted into its own plane.
pub fn mark_group_rows_adopted(cfg: &mut GroupShareConfig, scope_id: &[u8; 32]) {
    if let Some(r) = invited_mut(cfg, scope_id) {
        r.rows_adopted = true;
    }
}

fn initiated_mut<'a>(
    cfg: &'a mut GroupShareConfig,
    scope_id: &[u8; 32],
    recipient: &ActorId,
) -> Option<&'a mut InitiatedGroupShare> {
    cfg.initiated
        .iter_mut()
        .find(|r| r.scope_id == *scope_id && r.recipient == *recipient)
}

fn invited_mut<'a>(
    cfg: &'a mut GroupShareConfig,
    scope_id: &[u8; 32],
) -> Option<&'a mut InvitedGroupShare> {
    cfg.invited.iter_mut().find(|r| r.scope_id == *scope_id)
}

fn held_root(record: &InitiatedGroupShare) -> Result<GroupMachineryRoot, GroupCeremonyError> {
    let bytes: [u8; 32] = record.root.as_ref().try_into().map_err(|_| {
        GroupCeremonyError::Payload("ceremony record holds no 32-byte machinery root".into())
    })?;
    Ok(GroupMachineryRoot::from_bytes(bytes))
}

/// Advisory ms from the second-resolution [`Timestamp`] every transition
/// takes — payload stamps are seconds, machinery stamps are ms, and both are
/// advisory (the lattices order, stamps inform).
fn now_ms(now: Timestamp) -> i64 {
    i64::try_from(now.0)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::{Capability, DeviceAuthorization};
    use fauna_core::encoding::{EmbedAsBytes, sign_envelope};

    fn alice() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    fn bob() -> ActorKeypair {
        ActorKeypair::from_secret([31u8; 32])
    }

    fn mallory() -> ActorKeypair {
        ActorKeypair::from_secret([41u8; 32])
    }

    fn alices_device() -> SigningKey {
        SigningKey::from_bytes(&[0x41; 32])
    }

    /// Alice's device authorization in the EmbedAsBytes carriage roster
    /// entries and mints embed.
    fn alices_device_cert() -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: alice().actor_id(),
            device_key: alices_device().verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(&alice(), &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env))
            .expect("encode carriage")
            .to_vec()
    }

    /// The whole ceremony, both sides, in memory: begin → offer ingest →
    /// accept → accept ingest → deliver → deliver ingest → admit — with the
    /// joiner's admission running the full resolver and every delivered row
    /// passing the ordinary first-contact adoption strictness.
    #[test]
    fn the_full_offline_ceremony_admits_the_recipient_end_to_end() {
        let now = Timestamp(1_700_000_000);
        let mut alice_cfg = GroupShareConfig::default();
        let mut bob_cfg = GroupShareConfig::default();

        // Alice begins: scope minted, offer framed, root custody recorded.
        let begun =
            begin_group_share(&mut alice_cfg, &alice(), bob().actor_id(), now).expect("begin");
        let scope = begun.scope_id;
        assert_eq!(begun.held_root_row.scope_id, scope);
        mark_group_offer_posted(&mut alice_cfg, &scope, &bob().actor_id());

        // Bob ingests the offer.
        let outcome = ingest_group_frame(
            &mut bob_cfg,
            &bob().actor_id(),
            &alice().actor_id(),
            &begun.frame,
            now,
        )
        .expect("bob ingests offer");
        assert_eq!(
            outcome,
            GroupIngestOutcome::OfferRecorded {
                scope_id: scope,
                initiator: alice().actor_id(),
            }
        );

        // Bob accepts with a fresh reception keypair.
        let bob_reception = GroupReceptionKeyRecord::mint(now_ms(now));
        let accept_frame =
            build_group_accept(&mut bob_cfg, &bob(), &scope, &bob_reception, now).expect("accept");
        mark_group_accept_posted(&mut bob_cfg, &scope);

        // Alice ingests the accept.
        let outcome = ingest_group_frame(
            &mut alice_cfg,
            &alice().actor_id(),
            &bob().actor_id(),
            &accept_frame,
            now,
        )
        .expect("alice ingests accept");
        assert_eq!(
            outcome,
            GroupIngestOutcome::AcceptRecorded {
                scope_id: scope,
                recipient: bob().actor_id(),
            }
        );

        // Alice delivers: roster (both entries), first mint, Bob's bundle.
        let alice_reception = GroupReceptionKeyRecord::mint(now_ms(now));
        let delivered = build_group_deliver(
            &mut alice_cfg,
            &alice(),
            &alices_device(),
            alices_device_cert(),
            &alice_reception,
            &scope,
            &bob().actor_id(),
            now,
        )
        .expect("deliver");
        mark_group_delivered(&mut alice_cfg, &scope, &bob().actor_id());
        assert_eq!(delivered.plane_rows.len(), 4, "birth + 2 roster + mint");

        // Every delivered row is adoptable through the ordinary group-plane
        // first-contact strictness — the snapshot invents no second trust
        // path.
        for row in &delivered.plane_rows {
            let policy = fauna_protocol::group_state::group_merge_policy(&row.kind)
                .unwrap_or_else(|| panic!("{} is registered", row.kind));
            let entry = fauna_core::account_entry_crypto::EntryPlaintext {
                kind: row.kind.clone(),
                key: row.key.clone(),
                merge_meta: None,
                value: row.value.clone().into(),
                tombstone: false,
            };
            fauna_protocol::merge_policy::apply_class2(policy, None, &entry)
                .unwrap_or_else(|e| panic!("{} row adopts: {e}", row.kind));
        }

        // Bob ingests the deliver and admits — the full verification chain.
        let outcome = ingest_group_frame(
            &mut bob_cfg,
            &bob().actor_id(),
            &alice().actor_id(),
            &delivered.frame,
            now,
        )
        .expect("bob ingests deliver");
        assert_eq!(
            outcome,
            GroupIngestOutcome::DeliverRecorded { scope_id: scope }
        );

        let admitted =
            admit_group_share(&bob_cfg, &bob(), &bob_reception, &scope, now).expect("admit");
        assert_eq!(admitted.generation_id, delivered.generation_id);
        assert_eq!(admitted.held_root_row.scope_id, scope);
        assert_eq!(admitted.rows.len(), 4);
        assert_ne!(admitted.entry_id, delivered.own_entry_id);
        // The admitted root is the same one Alice minted at begin.
        assert_eq!(
            admitted.held_root_row.root.as_ref(),
            begun.held_root_row.root.as_ref()
        );
        mark_group_invited_root_written(&mut bob_cfg, &scope);
        mark_group_rows_adopted(&mut bob_cfg, &scope);
    }

    /// The out-of-order and wrong-party refusals: a deliver before any
    /// accept is refused at ingest; a stranger cannot admit from Bob's
    /// delivery (the bundle is sealed to Bob's reception key at Bob's cell).
    #[test]
    fn out_of_order_and_wrong_party_steps_are_refused() {
        let now = Timestamp(1_700_000_000);
        let mut alice_cfg = GroupShareConfig::default();
        let mut bob_cfg = GroupShareConfig::default();

        let begun =
            begin_group_share(&mut alice_cfg, &alice(), bob().actor_id(), now).expect("begin");
        let scope = begun.scope_id;

        // Deliver is not owed before an accept is recorded.
        assert!(matches!(
            build_group_deliver(
                &mut alice_cfg,
                &alice(),
                &alices_device(),
                alices_device_cert(),
                &GroupReceptionKeyRecord::mint(now_ms(now)),
                &scope,
                &bob().actor_id(),
                now,
            ),
            Err(GroupCeremonyError::OutOfOrder(_))
        ));

        // Run the ceremony forward to a real delivery.
        ingest_group_frame(
            &mut bob_cfg,
            &bob().actor_id(),
            &alice().actor_id(),
            &begun.frame,
            now,
        )
        .expect("offer");
        let bob_reception = GroupReceptionKeyRecord::mint(now_ms(now));
        let accept_frame =
            build_group_accept(&mut bob_cfg, &bob(), &scope, &bob_reception, now).expect("accept");
        ingest_group_frame(
            &mut alice_cfg,
            &alice().actor_id(),
            &bob().actor_id(),
            &accept_frame,
            now,
        )
        .expect("accept ingest");
        let delivered = build_group_deliver(
            &mut alice_cfg,
            &alice(),
            &alices_device(),
            alices_device_cert(),
            &GroupReceptionKeyRecord::mint(now_ms(now)),
            &scope,
            &bob().actor_id(),
            now,
        )
        .expect("deliver");
        ingest_group_frame(
            &mut bob_cfg,
            &bob().actor_id(),
            &alice().actor_id(),
            &delivered.frame,
            now,
        )
        .expect("deliver ingest");

        // Mallory cannot admit from Bob's record even with Bob's config in
        // hand: the bundle opens only under Bob's reception secret.
        let mallory_reception = GroupReceptionKeyRecord::mint(now_ms(now));
        assert!(admit_group_share(&bob_cfg, &mallory(), &mallory_reception, &scope, now).is_err());

        // A forwarded offer conveys nothing: Mallory relaying Alice's offer
        // as her own send is refused at ingest.
        let mut mallory_cfg = GroupShareConfig::default();
        assert!(matches!(
            ingest_group_frame(
                &mut mallory_cfg,
                &mallory().actor_id(),
                &mallory().actor_id(),
                &begun.frame,
                now,
            ),
            Err(GroupCeremonyError::Payload(_))
        ));
    }

    /// The ingest-side ordering guard: a deliver frame arriving before this
    /// side ever accepted is refused at ingest (found by a survived
    /// mutation — the build-side guard alone does not cover it).
    #[test]
    fn a_deliver_ingested_before_accepting_is_refused() {
        use fauna_core::group_ceremony::{GroupShareDeliver, sign_group_share_deliver};
        let now = Timestamp(1_700_000_000);
        let mut alice_cfg = GroupShareConfig::default();
        let mut bob_cfg = GroupShareConfig::default();
        let begun =
            begin_group_share(&mut alice_cfg, &alice(), bob().actor_id(), now).expect("begin");
        ingest_group_frame(
            &mut bob_cfg,
            &bob().actor_id(),
            &alice().actor_id(),
            &begun.frame,
            now,
        )
        .expect("offer");
        // Alice (mis)sends a deliver with no accept in hand — hand-crafted,
        // since the machine itself refuses to build one.
        let premature = GroupShareDeliver {
            scope_id: begun.scope_id,
            initiator: alice().actor_id(),
            roster_entry_id: [0x33; 32],
            admission_wrap: vec![0xEE; 32],
            machinery_snapshot: Vec::new(),
            delivered_at: now,
        };
        let envelope = sign_group_share_deliver(&alice(), &premature).expect("sign");
        let frame =
            encode_group_ceremony_message(&GroupCeremonyMessage::Deliver(envelope)).expect("frame");
        assert!(matches!(
            ingest_group_frame(
                &mut bob_cfg,
                &bob().actor_id(),
                &alice().actor_id(),
                &frame,
                now,
            ),
            Err(GroupCeremonyError::OutOfOrder(_))
        ));
    }

    /// A declined invitation stays declined (monotone), and accepting it is
    /// refused.
    #[test]
    fn a_declined_invitation_cannot_be_accepted() {
        let now = Timestamp(1_700_000_000);
        let mut alice_cfg = GroupShareConfig::default();
        let mut bob_cfg = GroupShareConfig::default();
        let begun =
            begin_group_share(&mut alice_cfg, &alice(), bob().actor_id(), now).expect("begin");
        ingest_group_frame(
            &mut bob_cfg,
            &bob().actor_id(),
            &alice().actor_id(),
            &begun.frame,
            now,
        )
        .expect("offer");
        assert!(decline_group_offer(&mut bob_cfg, &begun.scope_id, now));
        assert!(matches!(
            build_group_accept(
                &mut bob_cfg,
                &bob(),
                &begun.scope_id,
                &GroupReceptionKeyRecord::mint(now_ms(now)),
                now,
            ),
            Err(GroupCeremonyError::OutOfOrder(_))
        ));
    }
}
