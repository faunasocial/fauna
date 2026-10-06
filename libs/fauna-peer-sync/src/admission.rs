//! The admission seam's **verdict** — what the pull/serve core consumes
//! (`account-data-plane.md` § The peer leg → *The admission seam*: "the
//! pull/serve core's entire admission contract is a per-connection verdict:
//! `(account, admitted scope set, validity bound)`").
//!
//! Witness *evaluation* is deliberately not here: one verifier per witness
//! kind, each owned where its mechanics live — `DeviceAuthorization` in
//! `fauna_core::encoding` ([`verify_device_admission_witness`]), the custody
//! grant in `fauna_core::custody_grant` ([`verify_custody_witness`]), M2
//! membership with the cross-user share leg when it builds. A witness kind
//! the *core* could name would be the redesign the seam ruling exists to
//! prevent; this module holds only the per-kind *adaptors* from verifier
//! output to the verdict shape, and [`evaluate_witness`] — the one
//! by-name dispatch both exchange halves share.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result, bail};
use fauna_core::custody_grant::{CustodyScopeSet, verify_custody_witness};
use fauna_core::data::Timestamp;
use fauna_core::encoding::{EmbedAsBytes, verify_device_admission_witness};
use fauna_core::identity::ActorId;
use fauna_protocol::RpcError;
use fauna_protocol::peer_sync::{WITNESS_CUSTODY_GRANT, WITNESS_DEVICE_AUTHORIZATION};
use fauna_transport::EndpointKey;

use crate::quota::QuotaLedger;

/// The scope-predicate half of the verdict — lifted to
/// `fauna_protocol::scope` (W8.6 (account-data-plane.md § Workstreams)) so the nest custody door shares the one
/// vocabulary; re-exported here so every existing path keeps working.
pub use fauna_protocol::scope::AdmittedScopes;

/// A per-connection admission verdict: `(account, admitted scope set,
/// validity bound)`. Produced by a witness verifier once per connection (and
/// re-produced on re-presentation); consulted by **every** pull and serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionVerdict {
    /// The admitted account (32-byte actor key).
    pub account: [u8; 32],
    pub scopes: AdmittedScopes,
    /// The witness's expiry (epoch seconds), when it carries one. A verdict's
    /// lifetime is `min(connection lifetime, this)` — [`Self::valid_at`] is
    /// the "past it, re-present before continuing" check.
    pub expires_at: Option<u64>,
}

impl AdmissionVerdict {
    /// Is the verdict still within its validity bound at `now_secs`?
    pub fn valid_at(&self, now_secs: u64) -> bool {
        self.expires_at.is_none_or(|e| now_secs <= e)
    }

    /// The core's scope-enforcement duty: does this verdict admit `scope` for
    /// `account`? `account` is the account whose store the serve would answer
    /// from — an `AllOfAccount` verdict admits any of *that* account's scopes
    /// (the account-state scopes are account-implicit; a content scope's actor
    /// binding is the serving store's, checked at the store boundary), and
    /// only when the verdict names the same account.
    pub fn admits_scope(&self, account: &[u8; 32], scope: &str) -> bool {
        if self.account != *account {
            return false;
        }
        // The scope predicate itself has ONE owner
        // (`fauna_protocol::scope::AdmittedScopes::admits` — shape checks,
        // the A5-partition account-state pair, the co-authored carve-out),
        // shared with the nest custody door.
        self.scopes.admits(scope)
    }
}

/// What a connection's verdict slot holds, as the shared preflight needs to
/// read it. Two shapes implement it: [`AdmissionVerdict`] itself — the share
/// leg's slot, which needs nothing beside the verdict — and
/// [`AdmittedConnection`], the sync leg's, which carries the admitted fleet
/// device key alongside so the per-request severance checks bind to the
/// `DeviceAuthorization` arm and to nothing else.
///
/// The verdict stays the core's *entire* admission contract (`(account,
/// admitted scope set, validity bound)`, the seam's ruling): what a slot adds
/// is evaluator bookkeeping about the witness that produced the verdict,
/// never a fourth field of the verdict.
pub trait AdmittedEntry: Clone {
    fn verdict(&self) -> &AdmissionVerdict;
}

impl AdmittedEntry for AdmissionVerdict {
    fn verdict(&self) -> &AdmissionVerdict {
        self
    }
}

/// The sync leg's slot contents: the verdict plus what the serve side's
/// per-request withdrawal re-checks key on — the fleet device key a
/// `DeviceAuthorization` witness proved, or the id of the custody grant that
/// admitted the connection. At most one is `Some`, by witness kind.
///
/// Both are recorded at admission rather than derived at the point of use,
/// and the device key deliberately not from the channel-proven key: a
/// custody-grant connection's proven key is the *custodian's* device, of
/// another account entirely, and testing it against the owner account's fleet
/// exclusion set would be a category error that happens to answer "no".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedConnection {
    pub verdict: AdmissionVerdict,
    /// The admitted fleet device key — `Some` exactly when the witness was a
    /// `DeviceAuthorization` (whose verifier binds it to the channel-proven
    /// key), `None` for a custody grant.
    pub device_key: Option<[u8; 32]>,
    /// The admitting custody grant's id — `Some` exactly when the witness
    /// was a custody grant ([`EvaluatedWitness::custody_grant_id`]), `None`
    /// for a `DeviceAuthorization`.
    pub custody_grant_id: Option<Vec<u8>>,
}

impl AdmittedEntry for AdmittedConnection {
    fn verdict(&self) -> &AdmissionVerdict {
        &self.verdict
    }
}

/// Meter one request and read the connection's slot entry, refusing when
/// over-quota, absent, or expired. The preflight both peer-plane servers run
/// before their own plane-specific admission check —
/// `fauna_peer_sync::server::PeerSyncServer::admitted_connection`'s served-
/// account check and `fauna-peer-share`'s `ShareServer::admitted_for_set`'s
/// scope-plus-roster check both continue from what this returns. Error
/// construction is a caller closure since each plane's `RpcError` carries its
/// own wire-facing error kind (`error.peer_sync.*` vs `error.peer_share.*`).
pub fn admitted_verdict<T: AdmittedEntry>(
    peer: &EndpointKey,
    slot: &Mutex<Option<T>>,
    ledger: &QuotaLedger,
    now_secs: u64,
    over_quota: impl FnOnce() -> RpcError,
    no_verdict: impl FnOnce() -> RpcError,
    expired: impl FnOnce() -> RpcError,
) -> Result<T, RpcError> {
    if !ledger.try_request(peer.as_bytes(), now_secs) {
        return Err(over_quota());
    }
    let entry = slot.lock().unwrap().clone();
    let Some(entry) = entry else {
        return Err(no_verdict());
    };
    if !entry.verdict().valid_at(now_secs) {
        return Err(expired());
    }
    Ok(entry)
}

/// The same-account adaptor: verify a `DeviceAuthorization` witness against
/// the channel-proven `proven_key` and this store's `expected_account`, and
/// shape the result as the verdict the core consumes. The seam's rules
/// (proven-key binding, same-account, expiry, capabilities-not-consulted)
/// live in the verifier — `fauna_core::encoding`'s
/// [`verify_device_admission_witness`] — beside the cert's own mechanics.
/// Returns the verdict **and the admitted device key** — the verifier binds
/// it to the channel-proven key, and the evaluating side keys its own
/// removal view on it (the device twin of the custody adaptor's grant id:
/// removal, like revocation, is deliberately not the verifier's question,
/// because the witness is self-contained and never expires).
pub fn verdict_for_device_authorization(
    witness: &EmbedAsBytes,
    proven_key: &[u8; 32],
    expected_account: &[u8; 32],
    now_secs: u64,
) -> Result<(AdmissionVerdict, [u8; 32])> {
    let admission = verify_device_admission_witness(
        witness,
        proven_key,
        &ActorId(*expected_account),
        Timestamp(now_secs),
    )
    .context("device-authorization admission witness")?;
    Ok((
        AdmissionVerdict {
            account: admission.actor_id.0,
            scopes: AdmittedScopes::AllOfAccount,
            expires_at: admission.expires_at.map(|t| t.0),
        },
        admission.device_key,
    ))
}

/// The custody adaptor: verify a `CustodyGrant` witness against the
/// channel-proven `proven_key` and the custodied `expected_account`, and shape
/// the result as the verdict the core consumes. The seam's rules (owner
/// signature, proven-key binding, owner-is-the-account, expiry) live in the
/// verifier — `fauna_core::custody_grant`'s [`verify_custody_witness`] —
/// beside the grant's own mechanics.
///
/// Returns the verdict **and the grant id**: revocation is deliberately not
/// the verifier's question (the witness is self-contained), so the evaluating
/// side consults its own revocation store — the synced grant-event log on a
/// fleet replica, the capability row on the nest — keyed by this id, beside
/// this call.
pub fn verdict_for_custody_grant(
    witness: &EmbedAsBytes,
    proven_key: &[u8; 32],
    expected_account: &[u8; 32],
    now_secs: u64,
) -> Result<(AdmissionVerdict, Vec<u8>)> {
    let admission = verify_custody_witness(
        witness,
        proven_key,
        &ActorId(*expected_account),
        Timestamp(now_secs),
    )
    .context("custody-grant admission witness")?;
    let scopes = match admission.scopes {
        CustodyScopeSet::Account => AdmittedScopes::AllOfAccountSinglePrincipal,
        CustodyScopeSet::Scopes(named) => AdmittedScopes::Named(named),
        // A set a newer build minted covers no scope here; the witness still
        // decoded, so its removed-device exclusions still bind.
        CustodyScopeSet::Unknown(_) => AdmittedScopes::Named(Vec::new()),
    };
    Ok((
        AdmissionVerdict {
            account: admission.owner.0,
            scopes,
            expires_at: Some(admission.expires_at.0),
        },
        admission.grant_id,
    ))
}

/// Which account a witness CLAIMS to admit for — the multi-account serve
/// side's routing peek (W8.5 P1): both witness kinds name their account
/// (`DeviceAuthorization.actor_id`, `CustodyGrant.owner`), so the listener
/// reads the claim first, checks it against its served set, and then runs
/// [`evaluate_witness`] against **exactly the claim** — the peek is never
/// believed on its own (the verifier's signature/key-binding/expiry rules
/// bind it), it only chooses which served account the verification targets.
///
/// A decode failure is a refusal, never a guess — the seam's strict-decode
/// rule; this reads the same canonical bytes the verifier will re-read.
pub fn claimed_account(witness_kind: &str, witness: &EmbedAsBytes) -> Result<[u8; 32]> {
    let (bytes, _env) = witness
        .clone()
        .into_signed()
        .context("admission witness envelope")?;
    if witness_kind == WITNESS_DEVICE_AUTHORIZATION {
        let cert: fauna_core::data::DeviceAuthorization =
            fauna_core::encoding::decode_signed_bytes(&bytes)
                .context("device-authorization claim")?;
        Ok(cert.actor_id.0)
    } else if witness_kind == WITNESS_CUSTODY_GRANT {
        let grant: fauna_core::custody_grant::CustodyGrant =
            fauna_core::encoding::decode_signed_bytes(&bytes).context("custody-grant claim")?;
        Ok(grant.owner.0)
    } else {
        bail!(
            "unsupported witness kind {witness_kind:?} (this leg verifies \
             {WITNESS_DEVICE_AUTHORIZATION:?} and {WITNESS_CUSTODY_GRANT:?})"
        );
    }
}

/// One evaluated admission witness — the verdict plus what the evaluating
/// side's revocation store needs to key on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedWitness {
    pub verdict: AdmissionVerdict,
    /// The custody grant's id when the witness was a custody grant — what the
    /// evaluator's revocation store answers about. `None` for every other
    /// witness kind.
    pub custody_grant_id: Option<Vec<u8>>,
    /// The fleet device key when the witness was a `DeviceAuthorization` —
    /// what the evaluator's **removed-device view** answers about. `None` for
    /// every other witness kind.
    ///
    /// Both fields exist for the same reason: a witness is self-contained, so
    /// *whether the account has since withdrawn it* is the evaluating side's
    /// own question, answered from its own synced state beside this call. The
    /// fleet cert additionally never expires (the account runtime mints it
    /// with `expires_at: None` — removal is the control), so this view is the
    /// only thing that ever severs one.
    pub device_key: Option<[u8; 32]>,
}

/// The one witness-kind dispatch, shared by the serve side and the pull side
/// (both directions of "sides admit independently, and their witness kinds may
/// differ"). A kind this build does not implement is refused **by name**,
/// never guessed at from shape — the seam's verifier rule.
pub fn evaluate_witness(
    witness_kind: &str,
    witness: &EmbedAsBytes,
    proven_key: &[u8; 32],
    expected_account: &[u8; 32],
    now_secs: u64,
) -> Result<EvaluatedWitness> {
    if witness_kind == WITNESS_DEVICE_AUTHORIZATION {
        let (verdict, device_key) =
            verdict_for_device_authorization(witness, proven_key, expected_account, now_secs)?;
        Ok(EvaluatedWitness {
            verdict,
            custody_grant_id: None,
            device_key: Some(device_key),
        })
    } else if witness_kind == WITNESS_CUSTODY_GRANT {
        let (verdict, grant_id) =
            verdict_for_custody_grant(witness, proven_key, expected_account, now_secs)?;
        Ok(EvaluatedWitness {
            verdict,
            custody_grant_id: Some(grant_id),
            device_key: None,
        })
    } else {
        bail!(
            "unsupported witness kind {witness_kind:?} (this leg verifies \
             {WITNESS_DEVICE_AUTHORIZATION:?} and {WITNESS_CUSTODY_GRANT:?})"
        );
    }
}

/// Per custodied account, the removed owner devices this custodian refuses.
pub type ExclusionMap = HashMap<[u8; 32], HashSet<[u8; 32]>>;

/// The custodian's **removed-device exclusion map** — for every account it
/// holds custody for, the owner devices the owner's own signed grants name as
/// removed (`CustodyGrant::removed_devices`; `account-replica-posture.md`
/// § The custody grant + ceremony, the witness bullet). A custodian cannot
/// read a custodied account's sealed device-set rows, so the grants it holds
/// are its ONLY exclusion source for that account — never an admitted
/// device's say-so, which the removed device could author first
/// (`account-sync-plane.md` § The admission seam → *Validity and severance*).
///
/// Pump-refreshed ([`Self::replace`] over [`Self::derive`]); clones share one
/// map, so a view built once reads it live — a refresh reaches a bound
/// server's admit door, its live connections' next requests and the next
/// dial with no rebind. **Absent admits**: an account with no held grant (or
/// only empty lists) excludes nothing, the plane's
/// additive-across-skew rule; failing closed would sever custody serving
/// wholesale.
#[derive(Clone, Default)]
pub struct CustodiedExclusions(Arc<RwLock<ExclusionMap>>);

impl CustodiedExclusions {
    /// Derive the map from the custody-grant witnesses this machine holds:
    /// each witness's envelope verifies under its own owner
    /// ([`fauna_core::custody_grant::verified_custody_grant`] — a forged or
    /// corrupted row names nobody), and its list lands under **the signing
    /// account**, unioned across that account's grants (`Removed` is
    /// absorbing, so a later grant's list is a superset and an older
    /// empty-listed grant can never mask it). No liveness filter: an expired
    /// or revoked grant's list is still the owner's signed statement of a
    /// removal, which never reverses. An unverifiable witness is skipped.
    pub fn derive<'a>(witnesses: impl IntoIterator<Item = &'a EmbedAsBytes>) -> ExclusionMap {
        let mut map = ExclusionMap::new();
        for witness in witnesses {
            match fauna_core::custody_grant::verified_custody_grant(witness) {
                Ok(grant) if !grant.removed_devices.is_empty() => {
                    map.entry(grant.owner.0)
                        .or_default()
                        .extend(grant.removed_devices);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    "custody exclusions: held witness does not verify ({e}) — contributes nothing"
                ),
            }
        }
        map
    }

    /// Install a freshly derived map.
    pub fn replace(&self, next: ExclusionMap) {
        *self.0.write().unwrap() = next;
    }

    /// Whether a held grant of `account`'s names `device_key` as removed.
    pub fn excludes(&self, account: &[u8; 32], device_key: &[u8; 32]) -> bool {
        self.0
            .read()
            .unwrap()
            .get(account)
            .is_some_and(|set| set.contains(device_key))
    }

    /// The custodied half of a removed-device view, in the shape both
    /// halves take (`server::DeviceRemovedFn`, `client::AdmissionViews`).
    pub fn view(&self) -> impl Fn(&[u8; 32], &[u8; 32]) -> bool + Send + Sync + 'static {
        let map = self.clone();
        move |account: &[u8; 32], device_key: &[u8; 32]| map.excludes(account, device_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};

    const ACCOUNT: [u8; 32] = [0xA1; 32];

    fn all_of_account() -> AdmissionVerdict {
        AdmissionVerdict {
            account: ACCOUNT,
            scopes: AdmittedScopes::AllOfAccount,
            expires_at: Some(1_000),
        }
    }

    /// **Both halves of the A5 partition, not just the grandfathered one.**
    /// This check listed `state` alone until 2026-08-15, so the peer leg
    /// refused `state-fleet` — and with it every device-set row, mint, escrow
    /// receipt and top-up wrap, i.e. the whole generation machinery could not
    /// propagate without a nest, which is exactly what the charter's "plane
    /// rows over any leg … no nest handshake door" forbids. Found by W5.8's
    /// end-to-end top-up proof (`fauna-sync-engine`'s `peer_leg_convergence`),
    /// which could not converge the fleet scope at all.
    #[test]
    fn all_of_account_admits_both_account_state_scopes() {
        let v = all_of_account();
        assert!(v.admits_scope(&ACCOUNT, ACCOUNT_STATE_SCOPE));
        assert!(
            v.admits_scope(&ACCOUNT, ACCOUNT_STATE_FLEET_SCOPE),
            "the fleet scope is an ordinary scope to an admission verdict — its own doc says \
             admission verdicts enumerate both strings"
        );
    }

    #[test]
    fn all_of_account_admits_state_and_wellformed_content_scopes_of_that_account() {
        let v = all_of_account();
        assert!(v.admits_scope(&ACCOUNT, ACCOUNT_STATE_SCOPE));
        assert!(v.admits_scope(&ACCOUNT, &format!("content:post:{}", "1a".repeat(32))));
        // A malformed scope string never reaches the store layer.
        assert!(!v.admits_scope(&ACCOUNT, "___definitely_not_a_scope"));
        // A different account's store is not admitted by this verdict.
        assert!(!v.admits_scope(&[0xB2; 32], ACCOUNT_STATE_SCOPE));
    }

    #[test]
    fn named_scopes_admit_exactly_the_named_set() {
        let scope = format!("content:post:{}", "1a".repeat(32));
        let v = AdmissionVerdict {
            account: ACCOUNT,
            scopes: AdmittedScopes::Named(vec![scope.clone()]),
            expires_at: None,
        };
        assert!(v.admits_scope(&ACCOUNT, &scope));
        assert!(!v.admits_scope(&ACCOUNT, ACCOUNT_STATE_SCOPE));
    }

    /// T13's shared-audience carve-out, pinned at the verdict layer: an
    /// `Account`-form custody verdict admits the account-state scopes and
    /// single-principal content, and NEVER a co-authored (`conv`) plane —
    /// removing the carve-out predicate from the new arm reds exactly this.
    #[test]
    fn the_single_principal_verdict_carves_out_co_authored_scopes() {
        let v = AdmissionVerdict {
            account: ACCOUNT,
            scopes: AdmittedScopes::AllOfAccountSinglePrincipal,
            expires_at: Some(1_000),
        };
        assert!(v.admits_scope(&ACCOUNT, ACCOUNT_STATE_SCOPE));
        assert!(v.admits_scope(&ACCOUNT, ACCOUNT_STATE_FLEET_SCOPE));
        assert!(v.admits_scope(&ACCOUNT, &format!("content:mail:{}", "1a".repeat(32))));
        assert!(
            !v.admits_scope(&ACCOUNT, &format!("content:conv:{}", "1a".repeat(32))),
            "a co-authored plane never rides an Account-form custody verdict"
        );
        // The shape check still holds on this arm.
        assert!(!v.admits_scope(&ACCOUNT, "___definitely_not_a_scope"));
        assert!(!v.admits_scope(&[0xB2; 32], ACCOUNT_STATE_SCOPE));
    }

    /// The explicit-list form is the one door for co-authored scopes — a
    /// deliberate per-scope owner action, so `Named` deliberately does NOT
    /// carve them out.
    #[test]
    fn a_named_custody_verdict_may_admit_a_co_authored_scope() {
        let conv = format!("content:conv:{}", "1a".repeat(32));
        let v = AdmissionVerdict {
            account: ACCOUNT,
            scopes: AdmittedScopes::Named(vec![conv.clone()]),
            expires_at: None,
        };
        assert!(v.admits_scope(&ACCOUNT, &conv));
    }

    #[test]
    fn validity_bound_is_at_most_the_witness_expiry() {
        let v = all_of_account();
        assert!(v.valid_at(999));
        assert!(v.valid_at(1_000), "at the bound is still valid");
        assert!(!v.valid_at(1_001), "past the bound needs re-presentation");
        let unbounded = AdmissionVerdict {
            expires_at: None,
            ..all_of_account()
        };
        assert!(unbounded.valid_at(u64::MAX));
    }

    // ── the custody adaptor + the shared dispatch ────────────────────────

    use fauna_core::custody_grant::{CUSTODY_GRANT_ID_LEN, CustodyGrant, sign_custody_grant};
    use fauna_core::identity::ActorKeypair;

    fn custody_witness(
        owner: &ActorKeypair,
        custodian: [u8; 32],
        scopes: CustodyScopeSet,
    ) -> (EmbedAsBytes, Vec<u8>) {
        let grant = CustodyGrant {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: owner.actor_id(),
            custodian_key: custodian,
            scopes,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(5_000),
            removed_devices: Vec::new(),
        };
        let id = grant.grant_id.clone();
        (sign_custody_grant(owner, &grant).expect("sign"), id)
    }

    #[test]
    fn the_custody_adaptor_maps_both_scope_forms_and_returns_the_grant_id() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];

        let (witness, id) = custody_witness(&owner, custodian, CustodyScopeSet::Account);
        let (verdict, grant_id) =
            verdict_for_custody_grant(&witness, &custodian, &owner.actor_id().0, 2_000)
                .expect("admits");
        assert_eq!(verdict.account, owner.actor_id().0);
        assert_eq!(verdict.scopes, AdmittedScopes::AllOfAccountSinglePrincipal);
        assert_eq!(
            verdict.expires_at,
            Some(5_000),
            "expiry always bounds custody"
        );
        assert_eq!(grant_id, id);

        let named = format!("content:conv:{}", "2b".repeat(32));
        let (witness, _) = custody_witness(
            &owner,
            custodian,
            CustodyScopeSet::Scopes(vec![named.clone()]),
        );
        let (verdict, _) =
            verdict_for_custody_grant(&witness, &custodian, &owner.actor_id().0, 2_000)
                .expect("admits");
        assert_eq!(verdict.scopes, AdmittedScopes::Named(vec![named]));
    }

    /// A scope set a newer build minted (`CustodyScopeSet::Unknown`) admits no
    /// scope, while the witness still decodes — so its owner-signed
    /// removed-device list still binds this custodian (`transport.md` § Schema
    /// and forward-compat discipline → *Rule 3 in full*; before the arm, an
    /// undecodable witness took its exclusion list down with it).
    #[test]
    fn an_unknown_scope_set_admits_nothing_and_keeps_its_exclusions() {
        #[derive(serde::Serialize)]
        enum NewerCustodyScopeSet {
            Group { group_id: String },
        }
        let unknown: CustodyScopeSet = fauna_core::encoding::canonical_decode(
            &fauna_core::encoding::canonical_encode(&NewerCustodyScopeSet::Group {
                group_id: "g1".into(),
            })
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(unknown, CustodyScopeSet::Unknown(_)));

        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let removed = [0xD1u8; 32];
        let grant = CustodyGrant {
            grant_id: vec![0x1D; CUSTODY_GRANT_ID_LEN],
            owner: owner.actor_id(),
            custodian_key: custodian,
            scopes: unknown,
            minted_at: Timestamp(1_000),
            expires_at: Timestamp(5_000),
            removed_devices: vec![removed],
        };
        let witness = sign_custody_grant(&owner, &grant).expect("sign");

        let (verdict, _) =
            verdict_for_custody_grant(&witness, &custodian, &owner.actor_id().0, 2_000)
                .expect("the witness still verifies");
        assert_eq!(verdict.scopes, AdmittedScopes::Named(Vec::new()));

        let map = CustodiedExclusions::derive([&witness]);
        assert!(
            map.get(&owner.actor_id().0)
                .is_some_and(|set| set.contains(&removed)),
            "the removed device is still refused"
        );
    }

    #[test]
    fn the_dispatch_refuses_an_unknown_kind_by_name_and_tags_custody_ids() {
        let owner = ActorKeypair::from_secret([9u8; 32]);
        let custodian = [0xC5u8; 32];
        let (witness, id) = custody_witness(&owner, custodian, CustodyScopeSet::Account);

        let evaluated = evaluate_witness(
            WITNESS_CUSTODY_GRANT,
            &witness,
            &custodian,
            &owner.actor_id().0,
            2_000,
        )
        .expect("custody kind dispatches");
        assert_eq!(evaluated.custody_grant_id, Some(id));

        let err = evaluate_witness(
            "witness-kind-from-the-future",
            &witness,
            &custodian,
            &owner.actor_id().0,
            2_000,
        )
        .expect_err("an unknown kind is refused by name");
        assert!(
            err.to_string().contains("unsupported witness kind"),
            "{err}"
        );
    }
}
