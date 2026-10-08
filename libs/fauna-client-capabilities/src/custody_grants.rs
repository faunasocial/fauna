//! The custody-grant KIND on the capability plane (W8.2 (account-data-plane.md § Workstreams)) — the lifecycle
//! half of `account-data-plane.md` § Replica posture → *The custody grant +
//! ceremony*: "`grant_id` lives in the capability-grant id space; mint/renew/
//! revoke are signed `GrantEvent`s … and each verb also drives a **keyless
//! nest-side capability row**".
//!
//! This module is the ONE owner of the custody scope vocabulary's two
//! mappings, so the mint path, the succession sweep, and the lenses cannot
//! drift:
//!
//! * `CustodyScopeSet` → nest-blob [`ScopeTuple`]s — the `Account` form is
//!   one `{class: custody, kind: account}` tuple; each explicit entry is
//!   `{class: custody, kind: scope, set: Some(<scope-string>)}`.
//! * `CustodyScopeSet` → log [`GrantEventScope`]s — same shapes, except the
//!   scope string rides `tier` (the frozen event type has no `set` field —
//!   the `GRANT_SCOPE_TIER_BOUNDED` precedent for facts riding existing
//!   fields).
//!
//! The admission **witness** (`fauna_core::custody_grant::CustodyGrant`) is a
//! separate owner-signed artifact minted by the ceremony (W8.4) — nothing
//! here signs or verifies it. What this module mints is the keyless
//! `GrantBlob` for the nest row, under the same record-then-deposit
//! discipline as every other kind ([`UndepositedGrant`]).

use fauna_core::custody_grant::{CUSTODY_GRANT_ID_LEN, CustodyScopeSet};
use fauna_core::grant_event::GrantEventScope;
use fauna_mls::wrapped_blob::{GrantBlob, GrantWindow, ScopeTuple, build_grant_blob};
use fauna_protocol::scope::is_own_account_scope;

use crate::MintGrantError;
use crate::grant_log::UndepositedGrant;

/// The custody `Account` form's nest-blob tuple.
fn account_tuple() -> ScopeTuple {
    ScopeTuple {
        class: ScopeTuple::CLASS_CUSTODY.to_string(),
        kind: Some(ScopeTuple::KIND_CUSTODY_ACCOUNT.to_string()),
        tier: None,
        set: None,
        factor: None,
    }
}

/// One explicit custody scope entry's nest-blob tuple.
fn scope_tuple(scope: &str) -> ScopeTuple {
    ScopeTuple {
        class: ScopeTuple::CLASS_CUSTODY.to_string(),
        kind: Some(ScopeTuple::KIND_CUSTODY_SCOPE.to_string()),
        tier: None,
        set: Some(scope.to_string()),
        factor: None,
    }
}

/// `CustodyScopeSet` → the nest blob's declared [`ScopeTuple`]s.
pub fn custody_tuples(set: &CustodyScopeSet) -> Vec<ScopeTuple> {
    match set {
        CustodyScopeSet::Account => vec![account_tuple()],
        CustodyScopeSet::Scopes(scopes) => scopes.iter().map(|s| scope_tuple(s)).collect(),
        // A set a newer build minted declares no scope here.
        CustodyScopeSet::Unknown(_) => Vec::new(),
    }
}

/// `CustodyScopeSet` → the log's [`GrantEventScope`]s (scope strings ride
/// `tier` — see the module docs).
pub fn custody_event_scopes(set: &CustodyScopeSet) -> Vec<GrantEventScope> {
    match set {
        CustodyScopeSet::Account => vec![GrantEventScope {
            class: ScopeTuple::CLASS_CUSTODY.to_string(),
            kind: Some(ScopeTuple::KIND_CUSTODY_ACCOUNT.to_string()),
            tier: None,
        }],
        CustodyScopeSet::Scopes(scopes) => scopes
            .iter()
            .map(|s| GrantEventScope {
                class: ScopeTuple::CLASS_CUSTODY.to_string(),
                kind: Some(ScopeTuple::KIND_CUSTODY_SCOPE.to_string()),
                tier: Some(s.clone()),
            })
            .collect(),
        // A set a newer build minted names no scope here.
        CustodyScopeSet::Unknown(_) => Vec::new(),
    }
}

/// Reconstruct the [`CustodyScopeSet`] from a grant's logged event scopes —
/// `None` when the scope list is not a custody grant's at all. The read-side
/// twin of [`custody_event_scopes`], consumed by the succession sweep (which
/// must re-mint the same declared set) and by anything rendering a custody
/// row.
///
/// Reconstruction is tolerant in exactly one direction: an `account` entry
/// wins over any stray `scope` entries (the `Account` form is a superset,
/// so widening on a malformed mix is never a narrower re-mint), and a
/// `scope` entry with no `tier` string is dropped rather than invented.
pub fn custody_set_from_scopes(scope: &[GrantEventScope]) -> Option<CustodyScopeSet> {
    let custody: Vec<&GrantEventScope> = scope
        .iter()
        .filter(|s| s.class == ScopeTuple::CLASS_CUSTODY)
        .collect();
    if custody.is_empty() {
        return None;
    }
    if custody
        .iter()
        .any(|s| s.kind.as_deref() == Some(ScopeTuple::KIND_CUSTODY_ACCOUNT))
    {
        return Some(CustodyScopeSet::Account);
    }
    Some(CustodyScopeSet::Scopes(
        custody
            .iter()
            .filter(|s| s.kind.as_deref() == Some(ScopeTuple::KIND_CUSTODY_SCOPE))
            .filter_map(|s| s.tier.clone())
            .collect(),
    ))
}

/// Is this logged scope list a custody grant's? (Any custody-class entry —
/// mint never mixes custody with another class, pinned below.)
pub fn is_custody_grant(scope: &[GrantEventScope]) -> bool {
    scope.iter().any(|s| s.class == ScopeTuple::CLASS_CUSTODY)
}

/// A custody mint request was malformed.
#[derive(Debug, thiserror::Error)]
pub enum CustodyMintError {
    /// An explicit scope entry is not a canonical scope string
    /// (`fauna_protocol::scope` — one spelling, refused not repaired).
    #[error("custody scope entry is not a canonical scope string: {0:?}")]
    BadScopeString(String),
    /// The explicit form named no scopes at all — an empty custody grant is a
    /// mint mistake, not a narrow grant.
    #[error("custody scope list is empty")]
    EmptyScopeList,
    /// `grant_id` is not 16 bytes.
    #[error("custody grant id must be {CUSTODY_GRANT_ID_LEN} bytes")]
    BadGrantId,
    /// The keyless blob could not be assembled.
    #[error(transparent)]
    Mint(#[from] MintGrantError),
}

/// Validate a [`CustodyScopeSet`] at mint time: every explicit entry must be
/// a canonical scope string — an account-state scope (`state` /
/// `state-fleet`) or a well-formed **content** scope. Co-authored (`conv`)
/// scopes are deliberately allowed here: the explicit list is their one door
/// (the shared-audience carve-out binds the `Account` form at admission, not
/// this list). A **folder** scope (the shared-set family the W8 share twin
/// ruled, 2026-08-17) is deliberately REFUSED: a shared set's plane rides
/// cross-account custody only once group-side consent is answered (the same
/// T20-gate reasoning `fauna_protocol::scope::CO_AUTHORED_CONTENT_KINDS`
/// records), and today the custody pull serves no folder plane — a mintable
/// grant naming one would be a claim nothing can honor.
pub fn validate_custody_scope_set(set: &CustodyScopeSet) -> Result<(), CustodyMintError> {
    match set {
        CustodyScopeSet::Account => Ok(()),
        CustodyScopeSet::Scopes(scopes) => {
            if scopes.is_empty() {
                return Err(CustodyMintError::EmptyScopeList);
            }
            for s in scopes {
                if !is_own_account_scope(s) {
                    return Err(CustodyMintError::BadScopeString(s.clone()));
                }
            }
            Ok(())
        }
        // Never minted by this build: only a carried value holds one.
        CustodyScopeSet::Unknown(_) => Err(CustodyMintError::BadScopeString(
            "a scope set of a form this version of the app does not know".into(),
        )),
    }
}

/// Assemble the custody grant's **keyless** nest blob, held as an
/// [`UndepositedGrant`] so the deposit cannot precede the log's `Mint` event
/// (record-then-deposit — the type carries the rule).
///
/// `custodian_key` is the custodian's device-principal Ed25519 key (= its
/// peer-plane NodeId) — it rides `GrantBlob.holder` verbatim, and no HPKE
/// wrap ever targets it: the custody class derives no payloads
/// (`derive_scope_payload` → `None`), so `wrapped_keys` is empty by
/// construction (pinned below) — which is also why no custody is read:
/// there is no payload to derive from it.
pub fn custody_mint_blob(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8],
    custodian_key: &[u8; 32],
    window: GrantWindow,
    set: &CustodyScopeSet,
) -> Result<UndepositedGrant, CustodyMintError> {
    let grant_id: [u8; CUSTODY_GRANT_ID_LEN] = grant_id
        .try_into()
        .map_err(|_| CustodyMintError::BadGrantId)?;
    let blob = keyless_custody_blob(owner_actor_id, &grant_id, custodian_key, window, set)?;
    let bytes = blob
        .to_canonical_bytes()
        .map_err(|e| CustodyMintError::Mint(MintGrantError::Wrap(e)))?;
    Ok(UndepositedGrant::new(grant_id, bytes))
}

/// The keyless custody blob both mints share: every custody tuple carries no
/// payload ([`crate::derive_scope_payload`]'s custody arm answers `None`), so
/// the blob is built straight from the tuples.
fn keyless_custody_blob(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; CUSTODY_GRANT_ID_LEN],
    custodian_key: &[u8; 32],
    window: GrantWindow,
    set: &CustodyScopeSet,
) -> Result<GrantBlob, CustodyMintError> {
    validate_custody_scope_set(set)?;
    let scopes: Vec<(ScopeTuple, Option<Vec<u8>>)> = custody_tuples(set)
        .into_iter()
        .map(|tuple| (tuple, None))
        .collect();
    let blob = build_grant_blob(
        owner_actor_id,
        grant_id,
        custodian_key,
        // Keyless: there is no HPKE wrap to make post-quantum, so no ML-KEM
        // key is ever fetched or threaded for a custody mint.
        None,
        window,
        &scopes,
    )
    .map_err(|e| CustodyMintError::Mint(MintGrantError::from(e)))?;
    debug_assert!(
        blob.wrapped_keys.is_empty(),
        "a custody blob must never carry wrapped keys"
    );
    Ok(blob)
}

/// The succession sweep's custody re-mint: the keyless blob's canonical
/// bytes **without** the [`UndepositedGrant`] door. Sweep-only: the sweep
/// deposits before it records, and its *derived* grant id is the convergence
/// argument (a crash re-derives the same id and the candidate re-fires) —
/// the same order and argument its generic arm has always used. Every
/// interactive mint goes through [`custody_mint_blob`] instead, where the
/// door is the rule.
pub fn custody_remint_blob_bytes(
    owner_actor_id: &[u8; 32],
    grant_id: &[u8; CUSTODY_GRANT_ID_LEN],
    custodian_key: &[u8; 32],
    window: GrantWindow,
    set: &CustodyScopeSet,
) -> Result<Vec<u8>, CustodyMintError> {
    let blob = keyless_custody_blob(owner_actor_id, grant_id, custodian_key, window, set)?;
    blob.to_canonical_bytes()
        .map_err(|e| CustodyMintError::Mint(MintGrantError::Wrap(e)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant_log::PublishedGrants;
    use fauna_client_config::PublishedLedger;
    use fauna_core::grant_event::GrantEventKind;
    use fauna_core::identity::ActorId;
    use fauna_core::succession_ledger::SuccessionLedger;

    fn conv_scope() -> String {
        format!("content:conv:{}", "2b".repeat(32))
    }

    #[test]
    fn the_two_mappings_round_trip_both_forms() {
        for set in [
            CustodyScopeSet::Account,
            CustodyScopeSet::Scopes(vec!["state".into(), conv_scope()]),
        ] {
            let events = custody_event_scopes(&set);
            assert!(is_custody_grant(&events));
            assert_eq!(
                custody_set_from_scopes(&events),
                Some(set.clone()),
                "event-scope mapping must reconstruct the declared set"
            );
            let tuples = custody_tuples(&set);
            assert!(
                tuples.iter().all(|t| t.class == ScopeTuple::CLASS_CUSTODY),
                "custody never mixes classes"
            );
            match &set {
                CustodyScopeSet::Account => {
                    assert_eq!(tuples.len(), 1);
                    assert_eq!(tuples[0].set, None);
                }
                CustodyScopeSet::Scopes(scopes) => {
                    assert_eq!(tuples.len(), scopes.len());
                    for (t, s) in tuples.iter().zip(scopes) {
                        assert_eq!(t.set.as_deref(), Some(s.as_str()));
                    }
                }
                CustodyScopeSet::Unknown(_) => unreachable!("not in the fixture list"),
            }
        }
    }

    /// The nest-door direction (W8.6 pin N4): the mint-side tuples and the
    /// fauna-mls reconstruction the nest consumes round-trip both forms —
    /// the two crates cannot drift on the vocabulary.
    #[test]
    fn nest_side_tuple_reconstruction_round_trips_the_mint() {
        for set in [
            CustodyScopeSet::Account,
            CustodyScopeSet::Scopes(vec!["state".into(), conv_scope()]),
        ] {
            assert_eq!(
                fauna_mls::wrapped_blob::custody_scope_set_from_tuples(&custody_tuples(&set)),
                Some(set),
            );
        }
        assert_eq!(
            fauna_mls::wrapped_blob::custody_scope_set_from_tuples(&[]),
            None
        );
    }

    #[test]
    fn a_non_custody_scope_list_reconstructs_to_none() {
        let mail = GrantEventScope {
            class: ScopeTuple::CLASS_CONTENT_READ.to_string(),
            kind: Some(ScopeTuple::KIND_MAIL.to_string()),
            tier: None,
        };
        assert_eq!(custody_set_from_scopes(std::slice::from_ref(&mail)), None);
        assert!(!is_custody_grant(&[mail]));
    }

    #[test]
    fn validation_holds_the_scope_string_door() {
        validate_custody_scope_set(&CustodyScopeSet::Account).unwrap();
        validate_custody_scope_set(&CustodyScopeSet::Scopes(vec![
            "state".into(),
            "state-fleet".into(),
            conv_scope(),
        ]))
        .unwrap();
        assert!(matches!(
            validate_custody_scope_set(&CustodyScopeSet::Scopes(vec![])),
            Err(CustodyMintError::EmptyScopeList)
        ));
        for bad in ["", "not a scope", "content:conv", "state:extra"] {
            assert!(matches!(
                validate_custody_scope_set(&CustodyScopeSet::Scopes(vec![bad.into()])),
                Err(CustodyMintError::BadScopeString(_))
            ));
        }
        // A folder scope PARSES since the W8 share twin ruled the family, but
        // stays un-mintable here until group-side consent is answered (the
        // validator's doc owns why) — this pin is what keeps the family
        // ruling from silently widening the custody mint door.
        assert!(matches!(
            validate_custody_scope_set(&CustodyScopeSet::Scopes(vec![format!(
                "folder:{}",
                "4f".repeat(32)
            )])),
            Err(CustodyMintError::BadScopeString(_))
        ));
    }

    /// The keyless pin (the spam-model precedent, `mint_grant_spam_model_
    /// conveys_no_key_material`'s shape): a custody blob declares its scope
    /// and carries ZERO wrapped keys, in both forms — and the blob releases
    /// only against a log that records its Mint (record-then-deposit).
    #[test]
    fn a_custody_blob_is_keyless_and_release_demands_the_recorded_mint() {
        use crate::grant_log::record_mint;
        use ed25519_dalek::SigningKey;

        let owner = [0xA1u8; 32];
        let custodian = [0xC5u8; 32];
        let grant_id = [0x1Du8; CUSTODY_GRANT_ID_LEN];

        for set in [
            CustodyScopeSet::Account,
            CustodyScopeSet::Scopes(vec![conv_scope()]),
        ] {
            let undeposited = custody_mint_blob(
                &owner,
                &grant_id,
                &custodian,
                GrantWindow(1_000, 9_000),
                &set,
            )
            .expect("keyless mint");

            // Decode the held bytes back and pin the keyless shape on the
            // wire artifact itself, not the in-memory value.
            // (Release first — the bytes are only reachable through the
            // record-then-deposit door.)
            let mut recorded_cfg = SuccessionLedger::empty(ActorId([9u8; 32]));
            let key = SigningKey::from_bytes(&[7u8; 32]);
            record_mint(
                &mut recorded_cfg,
                &key,
                grant_id,
                custodian,
                custody_event_scopes(&set),
                1_000,
                9_000,
                1_000,
            )
            .expect("record");
            let bytes = undeposited
                .release(&PublishedGrants::from_published(
                    &PublishedLedger::acknowledged_for_test(recorded_cfg),
                ))
                .expect("released against the published mint");
            let blob = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&bytes)
                .expect("blob decodes");
            assert!(blob.wrapped_keys.is_empty(), "keyless always");
            assert_eq!(blob.holder, custodian);
            assert_eq!(blob.scope, custody_tuples(&set));

            // The unrecorded path refuses — the type carries the ordering.
            let again = custody_mint_blob(
                &owner,
                &grant_id,
                &custodian,
                GrantWindow(1_000, 9_000),
                &set,
            )
            .unwrap();
            let empty = PublishedGrants::from_published(&PublishedLedger::acknowledged_for_test(
                SuccessionLedger::empty(ActorId([9u8; 32])),
            ));
            assert!(again.release(&empty).is_err(), "no Mint event, no deposit");
        }
        // The recorded events themselves are custody-shaped for the lenses.
        let mut cfg2 = SuccessionLedger::empty(ActorId([9u8; 32]));
        let key = SigningKey::from_bytes(&[7u8; 32]);
        record_mint(
            &mut cfg2,
            &key,
            grant_id,
            custodian,
            custody_event_scopes(&CustodyScopeSet::Account),
            1_000,
            9_000,
            1_000,
        )
        .unwrap();
        assert_eq!(cfg2.grant_events.len(), 1);
        assert_eq!(cfg2.grant_events[0].kind, GrantEventKind::Mint);
        assert!(is_custody_grant(&cfg2.grant_events[0].scope));
    }

    #[test]
    fn a_wrong_length_grant_id_is_refused() {
        let result = custody_mint_blob(
            &[0xA1u8; 32],
            &[0x1D; 8],
            &[0xC5u8; 32],
            GrantWindow(1_000, 9_000),
            &CustodyScopeSet::Account,
        );
        assert!(matches!(result, Err(CustodyMintError::BadGrantId)));
    }
}
