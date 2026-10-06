//! The share leg's witness verifiers — the admission seam's **third and
//! fourth** witness kinds, owned here exactly as the seam ruling places them
//! ("one verifier per witness kind, each owned where its mechanics live";
//! `fauna_peer_sync::admission`'s module doc names this crate as that home).
//!
//! The two are deliberately NOT unified. M2 membership is a **claim** the
//! evaluator resolves entirely from local state; group-scope membership is a
//! **carried certificate** plus a local supersession check. Collapsing them
//! into one signature is how a bearer check sneaks in — see the group section
//! below.
//!
//! **M2 membership** is a claim, not a certificate: the requester names set
//! ids, and verification is the evaluating side's own local roster consult —
//! the channel-proven actor key (PT-1b) checked against each claimed set's
//! MLS roster through the [`SetMembership`] seam. No envelope, no signature,
//! no registry lookup: the roster is the evaluator's own store,
//! offline-available, and the proven key IS the roster entry, which
//! satisfies "a witness admits a proven key, never a bearer" directly. Its
//! seam trait keeps this crate `fauna-mls`-free: the production impl adapts
//! `FolderGroupCrypto::contains_member` (fauna-client-folders) where the
//! engine actually lives.
//!
//! **Group-scope membership** carries the member's `Enrolled` roster entry
//! inline and verifies it against the evaluator's own authority root and
//! roster frontier — [`GroupRosterState`] is its seam, and
//! [`verdict_for_group_membership`] the verifier. The same "proven key, never
//! a bearer" rule holds there by an explicit PT-1b check rather than by
//! construction, which is why that check runs before any signature work.

use anyhow::{Result, bail};
use fauna_core::encoding::canonical_decode;
use fauna_core::group_scope::{GroupAuthority, GroupRosterRecord, verify_enrolled_entry};
use fauna_core::identity::ActorId;
use fauna_peer_sync::admission::{AdmissionVerdict, AdmittedScopes};
use fauna_protocol::peer_share::{WITNESS_GROUP_MEMBERSHIP, WITNESS_M2_MEMBERSHIP};
use fauna_protocol::scope::{FolderScope, GroupScope};

/// The roster-consult seam: "is `actor` a member of the set named by
/// `channel_id`, per this evaluator's OWN local group state?" Answering
/// `false` for a set this evaluator does not hold is the correct refusal —
/// an evaluator can only vouch for sets it is itself a member of.
///
/// `Send + Sync` are supertraits: the consult is held across awaits by the
/// serve handlers and the pump alike, and an engine-backed roster is
/// thread-shared by construction — without the bound, a `&dyn
/// SetMembership` coerced from a `Send + Sync` trait object silently loses
/// the markers and makes every future up the call chain unspawnable.
pub trait SetMembership: Send + Sync {
    fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool;
}

/// The canonical folder-scope string for a set — the vocabulary admission
/// verdicts enumerate (`fauna_protocol::scope`, the ruled folder family).
pub fn folder_scope_string(channel_id: [u8; 32]) -> String {
    FolderScope::new(channel_id).to_string()
}

/// The M2 adaptor: evaluate a claimed-set list against the local roster
/// state for the channel-proven `proven_key`, and shape the result as the
/// verdict the core consumes. Returns the verdict **and the admitted set
/// ids** (the admit reply's `admitted_sets` — the dialer plans from it).
///
/// - A claimed id that is not exactly 32 bytes refuses the WHOLE claim
///   (strict-decode discipline: a malformed claim is a protocol error, not
///   a set to skip).
/// - A well-formed claimed set the roster does not admit is dropped, not
///   fatal — a partially-stale claim degrades (the wire doc's rule).
/// - An empty admitted set is a refusal ([`anyhow::Error`] here; the wire
///   layer answers `ERR_WITNESS_REFUSED`): a verdict admitting nothing must
///   not exist, or its connection would read as admitted to later checks.
///
/// The verdict's `account` is the **remote counterparty's actor key** —
/// identification for accounting and re-evaluation, NOT a serving-store
/// binding: share-serve scope checks go through [`verdict_admits_set`],
/// never `AdmissionVerdict::admits_scope` (whose account-equality semantics
/// are the account plane's). `expires_at` is `None` — a roster claim has no
/// expiry; severance is re-evaluation (next admission finds the roster
/// changed) plus the M2 rotate-on-removal making retained ciphertext dark.
pub fn verdict_for_m2_membership(
    claimed_sets: &[impl AsRef<[u8]>],
    membership: &dyn SetMembership,
    proven_key: &[u8; 32],
) -> Result<(AdmissionVerdict, Vec<[u8; 32]>)> {
    let actor = ActorId(*proven_key);
    let mut admitted: Vec<[u8; 32]> = Vec::new();
    for claimed in claimed_sets {
        let id: [u8; 32] = claimed
            .as_ref()
            .try_into()
            .map_err(|_| anyhow::anyhow!("malformed claimed set id (must be 32 bytes)"))?;
        if membership.is_member(&id, &actor) && !admitted.contains(&id) {
            admitted.push(id);
        }
    }
    if admitted.is_empty() {
        bail!("no claimed set admits the channel-proven actor");
    }
    let scopes = admitted
        .iter()
        .map(|id| folder_scope_string(*id))
        .collect::<Vec<_>>();
    Ok((
        AdmissionVerdict {
            account: *proven_key,
            scopes: AdmittedScopes::Named(scopes),
            expires_at: None,
        },
        admitted,
    ))
}

/// The share serve core's enforcement door: does `verdict` (held for
/// `remote_actor`) admit the set named by `channel_id`? Checks the verdict
/// belongs to the actor on the channel AND its scope set covers the set's
/// folder scope — the share-plane analog of the account plane's
/// `admits_scope`, with the account field read as counterparty identity.
pub fn verdict_admits_set(
    verdict: &AdmissionVerdict,
    remote_actor: &[u8; 32],
    channel_id: &[u8; 32],
) -> bool {
    verdict.account == *remote_actor && verdict.scopes.admits(&folder_scope_string(*channel_id))
}

/// The share exchange's witness-kind dispatch. A kind this build does not
/// implement is refused **by name**, never guessed at from shape (the seam's
/// verifier rule). [`WITNESS_GROUP_MEMBERSHIP`] is dispatched by
/// [`verdict_for_group_membership`], which takes a carried entry rather than
/// a claim list and so does not share this signature.
pub fn evaluate_share_witness(
    witness_kind: &str,
    claimed_sets: &[impl AsRef<[u8]>],
    membership: &dyn SetMembership,
    proven_key: &[u8; 32],
) -> Result<(AdmissionVerdict, Vec<[u8; 32]>)> {
    if witness_kind == WITNESS_M2_MEMBERSHIP {
        verdict_for_m2_membership(claimed_sets, membership, proven_key)
    } else if witness_kind == WITNESS_GROUP_MEMBERSHIP {
        // Refused *here* on purpose, and by name: the group witness is a
        // carried certificate, so routing it through a claim-list signature
        // would mean admitting on the claim alone.
        bail!(
            "witness kind {WITNESS_GROUP_MEMBERSHIP:?} carries an entry — evaluate it with verdict_for_group_membership"
        );
    } else {
        bail!(
            "unsupported witness kind {witness_kind:?} (this leg verifies {WITNESS_M2_MEMBERSHIP:?})"
        );
    }
}

// ── The group-scope membership witness (the seam's FOURTH kind) ─────────────
//
// T20's form, ruled 2026-08-17 (`account-data-plane.md` § The recipient-set
// scheme, membership-witness bullet): the member's `Enrolled` roster entry
// plus its authoring chain to the authority actor root, carried inline. So it
// differs from the M2 arm above on exactly one axis — carriage. M2 is a claim
// the evaluator resolves entirely from local state; this one is a
// certificate, and the evaluator still consults local state, for the
// supersession check. The two are NOT unified into one signature: pretending a
// carried certificate is a claim is how a bearer check sneaks in.

/// What an evaluator knows about a group scope from its **own** state — the
/// [`SetMembership`] twin, and the reason a witness can never be
/// self-certifying: the carried entry proves enrolment, this seam proves it
/// has not since been revoked.
pub trait GroupRosterState {
    /// The group's authority line per this evaluator's OWN state: its copy
    /// of the birth record's authority root, that actor's prior
    /// (succeeded-from) identities, and the authority devices its own merged
    /// `fauna.group.authority-revocation` rows revoke
    /// ([`GroupAuthority::build`]). The revocations are the half a carried,
    /// self-contained witness can never supply: a device removed from the
    /// authority's account still authors chain-valid entries, and this door
    /// is a direct peer-ingest surface with no home nest behind it
    /// (`devices.md` § Device-signed authoring → *Revocation authority is the
    /// distinguished replica*, the re-visit).
    ///
    /// `None` for a group this evaluator does not hold — the correct refusal,
    /// exactly as [`SetMembership`] answers `false` for a set it does not
    /// hold. An evaluator that would accept a *carried* authority root would
    /// be admitting whoever wrote the witness.
    fn authority(&self, scope_id: &[u8; 32]) -> Option<GroupAuthority>;

    /// Does this evaluator's own merged roster frontier carry an **honored**
    /// `Removed` superseding `entry_id` — one authored under a device cert
    /// chaining to the authority root, the remover's own standing not
    /// consulted (`fauna_core::group_scope::RosterView::is_excluded_entry`;
    /// a stranger's removal excludes nobody, a since-revoked authority
    /// device's still does)? Revocation
    /// severs at the next admission evaluation (the seam's ratified bound) —
    /// a stale co-member is exploitable only until the remove reaches it, the
    /// same staleness window every frontier-merged fact carries.
    ///
    /// **`true` for a group this evaluator does not hold** — "not held"
    /// refuses here exactly as it does at [`Self::authority`]. The door reads
    /// the two separately, and a live evaluator may forget the scope between
    /// them (a failed store read clears it, a departure drops it); the serve
    /// re-consult ([`verdict_admits_group`]) reads this one alone. Because a
    /// `Removed` is absorbing within its writer's cell, any later state that
    /// still holds the scope still carries the removal, so the fail-closed
    /// answer needs no pinned snapshot.
    fn is_entry_removed(&self, scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool;
}

/// The canonical group-scope string for a group id — the vocabulary admission
/// verdicts enumerate (`fauna_protocol::scope`, the ruled group family).
pub fn group_scope_string(scope_id: [u8; 32]) -> String {
    GroupScope::new(scope_id).to_string()
}

/// Evaluate a carried group-membership witness for the channel-proven
/// `proven_key`, and shape the result as the verdict the core consumes.
/// Returns the verdict **and the verified entry id**.
///
/// The checks, in order — each one a refusal the seam names:
///
/// 1. The carriage decodes as an `Enrolled` roster record (strict dag-cbor;
///    a carried `Removed` is a protocol error, not a member).
/// 2. **PT-1b key binding** — the entry's member actor IS the channel-proven
///    key. This is the "a witness admits a proven key, never a bearer" rule,
///    and it is why a stolen witness envelope conveys nothing.
/// 3. The evaluator holds this group (the [`GroupRosterState::authority`]
///    seam), and the entry's authority chain verifies to that root — never to
///    a root the witness carried — under a device the evaluator has not
///    learned is revoked.
/// 4. The evaluator's own roster frontier carries no superseding `Removed`
///    — and still holds the group: a scope forgotten since check 3 reads as
///    removed ([`GroupRosterState::is_entry_removed`]).
///
/// `expires_at` is `None`, like the M2 arm: membership has no expiry, and the
/// authority cert's own expiry is checked against the *enrolment* instant
/// inside the chain verification — a cert that lapsed afterwards does not
/// un-enrol a member. Severance is re-evaluation (check 4), which is exactly
/// the seam's next-admission bound.
pub fn verdict_for_group_membership(
    carried_entry: &[u8],
    scope_id: &[u8; 32],
    state: &dyn GroupRosterState,
    proven_key: &[u8; 32],
) -> Result<(AdmissionVerdict, [u8; 32])> {
    let record: GroupRosterRecord = canonical_decode(carried_entry)
        .map_err(|e| anyhow::anyhow!("group membership witness does not decode: {e}"))?;
    let GroupRosterRecord::Enrolled { ref core, .. } = record else {
        bail!("group membership witness is not an Enrolled roster entry");
    };
    // PT-1b BEFORE any chain work: a witness for somebody else is refused on
    // identity, never on cryptography that might happen to check out.
    if core.member_actor.0 != *proven_key {
        bail!("group membership witness names an actor other than the channel-proven key");
    }
    let Some(authority) = state.authority(scope_id) else {
        bail!("this evaluator holds no group scope by that id");
    };
    let member = verify_enrolled_entry(&record, scope_id, &authority)
        .map_err(|reason| anyhow::anyhow!("group membership witness does not verify: {reason}"))?;
    if state.is_entry_removed(scope_id, &member.entry_id) {
        bail!("the roster frontier supersedes this entry with a removal");
    }
    Ok((
        AdmissionVerdict {
            account: *proven_key,
            scopes: AdmittedScopes::Named(vec![group_scope_string(*scope_id)]),
            expires_at: None,
        },
        member.entry_id,
    ))
}

/// The group serve door's enforcement check: does `verdict` (held for
/// `remote_actor`) admit the group named by `scope_id`? The
/// [`verdict_admits_set`] twin, one family out.
///
/// ⚠ **This check alone is not a serve-door preflight.**
/// The verdict is a cache minted at admit time (`expires_at: None` by
/// design), so a data arm consuming this MUST also re-consult the live
/// roster per request — `GroupRosterState::is_entry_removed` for the
/// verdict's admitted entry — exactly as the M2 data arms re-consult
/// [`SetMembership`] (`ShareServer::admitted_for_set`) and the peer-sync
/// twin re-consults its served registry. Without the re-consult, eviction
/// never reaches an open connection.
pub fn verdict_admits_group(
    verdict: &AdmissionVerdict,
    remote_actor: &[u8; 32],
    scope_id: &[u8; 32],
) -> bool {
    verdict.account == *remote_actor && verdict.scopes.admits(&group_scope_string(*scope_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A fake roster: (set, actor) pairs this evaluator vouches for.
    struct FakeRoster(HashSet<([u8; 32], [u8; 32])>);

    impl SetMembership for FakeRoster {
        fn is_member(&self, channel_id: &[u8; 32], actor: &ActorId) -> bool {
            self.0.contains(&(*channel_id, actor.0))
        }
    }

    const SPOUSE: [u8; 32] = [0xB2; 32];
    const STRANGER: [u8; 32] = [0xB3; 32];
    const SET_A: [u8; 32] = [0x4F; 32];
    const SET_B: [u8; 32] = [0x50; 32];

    fn roster() -> FakeRoster {
        FakeRoster(HashSet::from([(SET_A, SPOUSE)]))
    }

    #[test]
    fn the_admitted_subset_is_exactly_the_roster_intersection() {
        let (verdict, admitted) =
            verdict_for_m2_membership(&[SET_A.to_vec(), SET_B.to_vec()], &roster(), &SPOUSE)
                .expect("one set admits");
        assert_eq!(admitted, vec![SET_A], "SET_B is dropped, not fatal");
        assert_eq!(verdict.account, SPOUSE);
        assert_eq!(
            verdict.scopes,
            AdmittedScopes::Named(vec![folder_scope_string(SET_A)])
        );
        assert_eq!(verdict.expires_at, None, "a roster claim has no expiry");
    }

    #[test]
    fn a_stranger_is_refused_outright() {
        let err = verdict_for_m2_membership(&[SET_A.to_vec()], &roster(), &STRANGER)
            .expect_err("no set admits a stranger");
        assert!(err.to_string().contains("no claimed set"), "{err}");
    }

    #[test]
    fn a_malformed_set_id_refuses_the_whole_claim() {
        let err = verdict_for_m2_membership(&[SET_A.to_vec(), vec![0x4F; 31]], &roster(), &SPOUSE)
            .expect_err("31 bytes is a protocol error, not a set to skip");
        assert!(err.to_string().contains("malformed"), "{err}");
    }

    #[test]
    fn a_duplicate_claim_admits_once() {
        let (_, admitted) =
            verdict_for_m2_membership(&[SET_A.to_vec(), SET_A.to_vec()], &roster(), &SPOUSE)
                .expect("admits");
        assert_eq!(admitted, vec![SET_A]);
    }

    #[test]
    fn the_serve_door_checks_actor_and_set_together() {
        let (verdict, _) =
            verdict_for_m2_membership(&[SET_A.to_vec()], &roster(), &SPOUSE).expect("admits");
        assert!(verdict_admits_set(&verdict, &SPOUSE, &SET_A));
        assert!(
            !verdict_admits_set(&verdict, &SPOUSE, &SET_B),
            "an unadmitted set is refused"
        );
        assert!(
            !verdict_admits_set(&verdict, &STRANGER, &SET_A),
            "a verdict never covers a different channel identity"
        );
    }

    #[test]
    fn the_dispatch_refuses_an_unknown_kind_by_name() {
        let err = evaluate_share_witness(
            "witness-kind-from-the-future",
            &[SET_A.to_vec()],
            &roster(),
            &SPOUSE,
        )
        .expect_err("unknown kind");
        assert!(
            err.to_string().contains("unsupported witness kind"),
            "{err}"
        );
    }

    /// The wide own-account verdict forms must never reach a folder scope —
    /// pinned at the scope layer (`fauna_protocol::scope`), re-asserted here
    /// from the consumer's side so a scope-layer regression reds close to
    /// the plane it exposes.
    #[test]
    fn a_wide_account_verdict_never_admits_a_shared_set() {
        let wide = AdmissionVerdict {
            account: SPOUSE,
            scopes: AdmittedScopes::AllOfAccount,
            expires_at: None,
        };
        assert!(!verdict_admits_set(&wide, &SPOUSE, &SET_A));
    }
}

/// The group-witness fixtures, shared between this module's tests and the
/// server's admit-exchange tests (`crate::server`) — one authority, one
/// birth, one signed `carried` entry builder, so the two suites exercise
/// the same certificate shape.
#[cfg(test)]
pub(crate) mod group_fixtures {
    use super::*;
    use fauna_core::data::{Capability, DeviceAuthorization, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::group_scope::{GroupBirthRecord, RosterEntryCore, group_scope_id};
    use fauna_core::identity::ActorKeypair;

    pub(crate) fn authority() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    pub(crate) fn foreign() -> ActorKeypair {
        ActorKeypair::from_secret([23u8; 32])
    }

    pub(crate) fn member() -> ActorKeypair {
        ActorKeypair::from_secret([31u8; 32])
    }

    pub(crate) fn other_member() -> ActorKeypair {
        ActorKeypair::from_secret([32u8; 32])
    }

    pub(crate) fn birth() -> GroupBirthRecord {
        GroupBirthRecord {
            authority_actor: authority().actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: fauna_core::crypto::GroupMachineryRoot::from_bytes([0xD7; 32])
                .commitment(),
            created_at_ms: 1_700_000_000_000,
        }
    }

    pub(crate) fn scope_id() -> [u8; 32] {
        group_scope_id(&birth()).expect("scope id")
    }

    pub(crate) fn core_for(member: &ActorKeypair) -> RosterEntryCore {
        RosterEntryCore {
            scope_id: scope_id(),
            member_actor: member.actor_id(),
            admission_salt: [0x01; 32],
        }
    }

    /// A carried `Enrolled` entry for `member`, signed by a device the
    /// `signer` actor authorized.
    pub(crate) fn carried(signer: &ActorKeypair, member: &ActorKeypair) -> Vec<u8> {
        let device = authority_device();
        let cert = DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: device.verifying_key().to_bytes(),
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: None,
        };
        let (bytes, env) = sign_envelope(signer, &cert).expect("sign cert");
        let (_, record) = fauna_core::group_scope::sign_roster_enrollment(
            &device,
            core_for(member),
            vec![0xE0; 8],
            canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).expect("carriage"),
            2_000,
        )
        .expect("entry");
        canonical_encode(&record).expect("encode entry")
    }

    /// The authority device every [`carried`] entry is signed by.
    pub(crate) fn authority_device() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x41; 32])
    }

    /// The authority ROOT's own revocation of [`authority_device`], as the
    /// evaluator's merged `fauna.group.authority-revocation` rows carry it.
    pub(crate) fn authority_device_revocation() -> (String, Vec<u8>) {
        let (key, record) = fauna_core::group_scope::sign_authority_revocation(
            authority().signing_key(),
            Vec::new(),
            scope_id(),
            authority_device().verifying_key().to_bytes(),
            5_000,
        );
        (key, canonical_encode(&record).expect("encode revocation"))
    }

    /// An evaluator that holds the group, with a configurable removal set
    /// and its own merged authority-device revocation rows.
    pub(crate) struct Evaluator {
        pub(crate) holds: bool,
        pub(crate) removed: Vec<[u8; 32]>,
        pub(crate) revocations: Vec<(String, Vec<u8>)>,
    }

    impl Evaluator {
        pub(crate) fn holding() -> Self {
            Self {
                holds: true,
                removed: Vec::new(),
                revocations: Vec::new(),
            }
        }
    }

    impl GroupRosterState for Evaluator {
        fn authority(&self, scope_id: &[u8; 32]) -> Option<GroupAuthority> {
            self.holds.then(|| {
                GroupAuthority::build(
                    scope_id,
                    &authority().actor_id(),
                    &[],
                    self.revocations
                        .iter()
                        .map(|(k, v)| (k.as_str(), v.as_slice())),
                )
            })
        }
        fn is_entry_removed(&self, _scope_id: &[u8; 32], entry_id: &[u8; 32]) -> bool {
            !self.holds || self.removed.contains(entry_id)
        }
    }

    pub(crate) fn proven(kp: &ActorKeypair) -> [u8; 32] {
        kp.actor_id().0
    }
}

#[cfg(test)]
mod group_tests {
    use super::group_fixtures::*;
    use super::*;
    use fauna_core::encoding::canonical_encode;
    use fauna_core::group_scope::roster_entry_id;

    #[test]
    fn a_carried_entry_admits_exactly_its_group_scope() {
        let (verdict, entry_id) = verdict_for_group_membership(
            &carried(&authority(), &member()),
            &scope_id(),
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect("admits");
        assert_eq!(verdict.account, proven(&member()));
        assert_eq!(
            verdict.scopes,
            AdmittedScopes::Named(vec![group_scope_string(scope_id())]),
            "verdict scope is exactly the group scope, never wider"
        );
        assert_eq!(verdict.expires_at, None, "membership has no expiry");
        assert_eq!(entry_id, roster_entry_id(&core_for(&member())).unwrap());
        assert!(verdict_admits_group(
            &verdict,
            &proven(&member()),
            &scope_id()
        ));
    }

    /// PT-1b: the witness admits a proven KEY, never a bearer. A perfectly
    /// valid entry for someone else, presented on my channel, admits nothing.
    #[test]
    fn a_stolen_witness_envelope_conveys_nothing() {
        let err = verdict_for_group_membership(
            &carried(&authority(), &other_member()),
            &scope_id(),
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect_err("the entry names a different actor");
        assert!(err.to_string().contains("channel-proven key"), "{err}");
    }

    /// The authority root comes from the EVALUATOR, never the witness — so an
    /// entry signed by a foreign root is refused even though its own chain is
    /// internally valid.
    #[test]
    fn an_entry_rooted_in_a_foreign_authority_is_refused() {
        let err = verdict_for_group_membership(
            &carried(&foreign(), &member()),
            &scope_id(),
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect_err("foreign root");
        assert!(err.to_string().contains("does not verify"), "{err}");
    }

    /// An evaluator can only vouch for groups it holds — the `SetMembership`
    /// refusal, one family out.
    #[test]
    fn an_evaluator_that_does_not_hold_the_group_refuses() {
        let err = verdict_for_group_membership(
            &carried(&authority(), &member()),
            &scope_id(),
            &Evaluator {
                holds: false,
                ..Evaluator::holding()
            },
            &proven(&member()),
        )
        .expect_err("not held");
        assert!(err.to_string().contains("holds no group scope"), "{err}");
    }

    /// Severance at the next admission evaluation: the carried entry still
    /// verifies forever, and the evaluator's own frontier is what refuses it.
    #[test]
    fn a_superseding_removal_severs_at_the_next_evaluation() {
        let entry_id = roster_entry_id(&core_for(&member())).unwrap();
        let err = verdict_for_group_membership(
            &carried(&authority(), &member()),
            &scope_id(),
            &Evaluator {
                removed: vec![entry_id],
                ..Evaluator::holding()
            },
            &proven(&member()),
        )
        .expect_err("removed");
        assert!(err.to_string().contains("supersedes"), "{err}");
    }

    /// **The pin at the door itself.** The authority device was
    /// removed from the authority's own account. It can still mint a FRESH
    /// entry — a cell no `Removed` row will ever name, so check 4 is blind to
    /// it — and the carried chain is perfectly valid. Only the evaluator's own
    /// learned revocation refuses it; an evaluator that has not learned still
    /// admits, which is the staleness window every frontier-merged fact has.
    #[test]
    fn an_entry_authored_by_a_revoked_authority_device_is_refused_at_the_door() {
        let witness = carried(&authority(), &member());
        verdict_for_group_membership(
            &witness,
            &scope_id(),
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect("an evaluator that has learned nothing still admits");
        let err = verdict_for_group_membership(
            &witness,
            &scope_id(),
            &Evaluator {
                revocations: vec![authority_device_revocation()],
                ..Evaluator::holding()
            },
            &proven(&member()),
        )
        .expect_err("the evaluator learned the device's revocation");
        assert!(
            err.to_string().contains("revoked authority device"),
            "{err}"
        );
    }

    /// A witness for group A does not admit group B, even from a member of A
    /// and even where the evaluator happens to hold both. The entry's core
    /// binds its scope, so the refusal is structural rather than a lookup
    /// miss — which is what makes it hold for an evaluator that holds A too.
    #[test]
    fn an_entry_from_another_group_does_not_admit_here() {
        let err = verdict_for_group_membership(
            &carried(&authority(), &member()),
            &[0x5A; 32],
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect_err("wrong group");
        assert!(err.to_string().contains("different group scope"), "{err}");
    }

    #[test]
    fn a_removed_row_is_never_a_witness() {
        let (_, removed) = fauna_core::group_scope::sign_roster_removal(
            &authority_device(),
            roster_entry_id(&core_for(&member())).unwrap(),
            Vec::new(),
            3_000,
        );
        let row = canonical_encode(&removed).unwrap();
        let err = verdict_for_group_membership(
            &row,
            &scope_id(),
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect_err("a removal enrols nobody");
        assert!(err.to_string().contains("not an Enrolled"), "{err}");
    }

    #[test]
    fn a_malformed_carriage_is_refused_not_guessed_at() {
        let err = verdict_for_group_membership(
            &[0xFF, 0xFE, 0xFD],
            &scope_id(),
            &Evaluator::holding(),
            &proven(&member()),
        )
        .expect_err("garbage");
        assert!(err.to_string().contains("does not decode"), "{err}");
    }

    /// The wide own-account verdict forms must never reach a group scope —
    /// the folder-family pin, one family out.
    #[test]
    fn a_wide_account_verdict_never_admits_a_group_scope() {
        let wide = AdmissionVerdict {
            account: proven(&member()),
            scopes: AdmittedScopes::AllOfAccount,
            expires_at: None,
        };
        assert!(!verdict_admits_group(
            &wide,
            &proven(&member()),
            &scope_id()
        ));
    }

    /// The claim-list dispatch refuses the group kind BY NAME rather than
    /// admitting on a claim list — the bearer-check trap this arm exists to
    /// avoid.
    #[test]
    fn the_claim_dispatch_refuses_the_group_kind_by_name() {
        struct NoSets;
        impl SetMembership for NoSets {
            fn is_member(&self, _c: &[u8; 32], _a: &ActorId) -> bool {
                true
            }
        }
        let err = evaluate_share_witness(
            WITNESS_GROUP_MEMBERSHIP,
            &[vec![0x4F; 32]],
            &NoSets,
            &proven(&member()),
        )
        .expect_err("must not admit a carried kind from a claim list");
        assert!(
            err.to_string().contains("verdict_for_group_membership"),
            "{err}"
        );
    }
}
