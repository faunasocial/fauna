//! User-custodied senior PLC rotation key — the client half of the ratified
//! did:plc key-custody split (`atproto-pds-bridge.md` § State & data shape).
//!
//! The key is generated HERE, on the user's client, and persisted only into
//! the account plane's `fauna.state.atproto-identity` rows (fleet-only,
//! sealed under the generation tip, nest-opaque, synced across the user's
//! device fleet — `config-dissolution.md`, the kind's row), through the
//! injected [`AtprotoIdentityStore`] door. Only the `did:key` pubkey ever
//! leaves the client — the enable flow (S4) sends it nest-side, where it lands
//! at `rotationKeys[0]` of the PLC genesis op, SENIOR to every bridge-held
//! key. Nothing in this module may grow a path that transmits the scalar.
//!
//! This module is the **sole writer** of the custody (the one-store-one-writer
//! discipline of `dns-management.md` § Where the credential lives). Every
//! write is ONE join through the door: the rotation key is user-irrecoverable
//! (losing it forfeits the recovery seniority the custody split exists for),
//! and the door's join removes nothing, so no race between two devices can
//! lose a key — a concurrent device's key simply joins the ring beside ours.

use fauna_core::data::{AtprotoContestIntent, AtprotoIdentityConfig, AtprotoRotationKey};
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint as _;

use crate::identity_store::AtprotoIdentityStore;
use fauna_protocol::atproto::{DidKeyCurve, encode_did_key};

/// Generate a fresh P-256 rotation key. Pure keygen — no persistence.
pub fn generate_rotation_key(created_at: u64) -> AtprotoRotationKey {
    let secret = p256::SecretKey::random(&mut OsRng);
    let compressed = secret.public_key().to_encoded_point(true);
    let pubkey_did_key = encode_did_key(DidKeyCurve::P256, compressed.as_bytes())
        .expect("compressed SEC1 point from p256 is always 33 bytes");
    let scalar: [u8; 32] = secret.to_bytes().into();
    AtprotoRotationKey {
        secret_scalar: scalar.into(),
        pubkey_did_key,
        created_at,
        published_for_dids: Vec::new(),
    }
}

/// Pick the rotation key a mint publishes as the new DID's senior key —
/// **never a key already published for another DID**. Reusing one would put
/// the same `rotationKeys[0]` in two DIDs' public PLC logs, permanently and
/// covertly linking them (the exact leak a user retiring an identity for a
/// clean break did not choose); seniority is per-identity, so nothing is
/// gained by reuse either.
///
/// Reuses a held key whose `published_for_dids` is empty (an aborted or
/// still-pending enable's key — retrying a failed mint must not churn keys),
/// else generates + persists a fresh one. Deterministic under the join:
/// returns the first unpublished key of the custody **as it now stands** — if
/// a concurrent device's key joined in ahead of ours, its key is returned and
/// this device's freshly generated one stays in the ring as a spare, never
/// silently double-minted into a PLC op.
pub async fn mint_rotation_key(
    store: &dyn AtprotoIdentityStore,
    now_unix_secs: u64,
) -> Result<String, String> {
    let unpublished = |custody: &AtprotoIdentityConfig| -> Option<String> {
        custody
            .rotation_keys
            .iter()
            .find(|k| k.published_for_dids.is_empty())
            .map(|k| k.pubkey_did_key.clone())
    };
    if let Some(pubkey) = unpublished(&store.atproto_identity().await?) {
        return Ok(pubkey);
    }
    let stored = store
        .merge_atproto_identity(AtprotoIdentityConfig {
            rotation_keys: vec![generate_rotation_key(now_unix_secs)],
            ..Default::default()
        })
        .await?;
    Ok(unpublished(&stored).expect("the join retains the unpublished rotation key it was handed"))
}

/// Record that `did`'s published PLC log lists the held key `pubkey_did_key`
/// senior — the burn that keeps [`mint_rotation_key`] from ever reusing it.
///
/// A **converge** writer: the fact comes from the public directory's own log
/// (or from having just signed for that log), never from nest testimony, and
/// recording it twice is a no-op (no write on every status convergence).
/// A pubkey this ring does not hold records nothing — there is no entry to
/// burn, and nothing downstream depends on the write.
pub async fn record_published_binding(
    store: &dyn AtprotoIdentityStore,
    did: &str,
    pubkey_did_key: &str,
) -> Result<(), String> {
    let custody = store.atproto_identity().await?;
    let Some(key) = custody
        .rotation_keys
        .into_iter()
        .find(|k| k.pubkey_did_key == pubkey_did_key)
    else {
        return Ok(());
    };
    if key.published_for_dids.iter().any(|d| d == did) {
        return Ok(());
    }
    // The key's own row, naming the DID: the join unions its bindings.
    let mut burned = key;
    burned.published_for_dids.push(did.to_string());
    burned.published_for_dids.sort();
    store
        .merge_atproto_identity(AtprotoIdentityConfig {
            rotation_keys: vec![burned],
            ..Default::default()
        })
        .await?;
    Ok(())
}

/// Freeze the fact that the nest named `did` as this account's hosted identity
/// — the second source of feeder #1's audit
/// floor.
///
/// A **converge** writer, called from every place that reads a fresh
/// `get_integration_status` and finds an auditable DID: the settings machine's
/// status convergence and the session-start sweep's own plan step. Idempotent —
/// re-recording is a no-op, so the steady state costs no write.
///
/// ⚠ **This deliberately records nest testimony**, unlike every other writer in
/// this module. It is not evidence of custody and must never be read as such;
/// its only job is to make the claim *irrevocable*, so a box cannot mint under
/// a hostile key and then answer `identity: null` to dodge the audit it would
/// fail. Because a floor only ever adds audit targets, a lie here can raise an
/// alarm the box itself will have to explain — never mute one.
///
/// Only auditable DIDs are recorded ([`crate::genesis_verify::is_auditable_did`]):
/// a did:web has no directory log, so freezing it would floor an audit that can
/// never run.
pub async fn record_nest_named_did(
    store: &dyn AtprotoIdentityStore,
    did: &str,
) -> Result<(), String> {
    if !crate::genesis_verify::is_auditable_did(did) {
        return Ok(());
    }
    let custody = store.atproto_identity().await?;
    if custody.nest_named_dids.iter().any(|d| d == did) {
        return Ok(());
    }
    store
        .merge_atproto_identity(AtprotoIdentityConfig {
            nest_named_dids: vec![did.to_string()],
            ..Default::default()
        })
        .await?;
    Ok(())
}

/// Record the USER's explicit consent to terminally retire `did` — the
/// client-authored half of the tombstone opt-in (`atproto-pds-bridge.md`
/// § Disable & revocation, the tombstone's shape).
///
/// Written by the opt-in gesture **before** the nest RPC that records the
/// durable intent (the mint's store-before-publish ordering): a crash between
/// the two leaves consent-without-intent, which is inert, never
/// intent-without-consent, which would wedge the retirement. The converge
/// pass publishes only when the nest's intent and this record AGREE — nest
/// testimony alone must never make a client perform a terminal, irreversible
/// act with the user's own key. Idempotent, and
/// synced across the fleet like the ring, so any device can finish what one
/// device's tick started.
pub async fn record_tombstone_consent(
    store: &dyn AtprotoIdentityStore,
    did: &str,
) -> Result<(), String> {
    let custody = store.atproto_identity().await?;
    if custody.tombstone_consents.iter().any(|d| d == did) {
        return Ok(());
    }
    store
        .merge_atproto_identity(AtprotoIdentityConfig {
            tombstone_consents: vec![did.to_string()],
            ..Default::default()
        })
        .await?;
    Ok(())
}

/// Record the USER's explicit, scoped consent to contest the standing PLC
/// operation `contested_op_cid` on `did` — the 72 h recovery fork
/// (`atproto-pds-bridge.md` § State & data shape, the recovery-fork contest,
/// decisions 6 and 7).
///
/// **Scoped to the named op, never a standing flag.** The consent covers this
/// one CID, so a new hostile op after a completed contest needs a new human
/// decision — an "always contest" flag would promote the *detector* into an
/// *actor* able to nullify the user's own out-of-band operations the day
/// outbound migration or third-party PLC tooling touches the log.
///
/// Written by the ceremony's confirm, before the converge pass that acts on
/// it; synced across the fleet like the ring, so a sibling device can finish a
/// contest this device started inside the window. Idempotent on the
/// `(did, cid)` pair. Nothing here reaches the nest — the whole contest is
/// client-side (decision 9).
pub async fn record_contest_intent(
    store: &dyn AtprotoIdentityStore,
    did: &str,
    contested_op_cid: &str,
    now_unix_secs: u64,
) -> Result<(), String> {
    let custody = store.atproto_identity().await?;
    if custody
        .contest_intents
        .iter()
        .any(|i| i.did == did && i.contested_op_cid == contested_op_cid)
    {
        return Ok(());
    }
    store
        .merge_atproto_identity(AtprotoIdentityConfig {
            contest_intents: vec![AtprotoContestIntent {
                did: did.to_string(),
                contested_op_cid: contested_op_cid.to_string(),
                requested_at: now_unix_secs,
            }],
            ..Default::default()
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::identity_store::InMemoryAtprotoIdentityStore;
    use fauna_protocol::atproto::decode_did_key;

    /// The plane door's in-memory double, counting the joins that reach it —
    /// the "no write in the steady state" pins count these.
    #[derive(Default)]
    struct Counting {
        inner: InMemoryAtprotoIdentityStore,
        merges: AtomicUsize,
        /// A key another device joined in first, merged ahead of every write.
        racing: std::sync::Mutex<Option<AtprotoRotationKey>>,
    }

    impl Counting {
        fn merges(&self) -> usize {
            self.merges.load(Ordering::SeqCst)
        }
        fn current(&self) -> AtprotoIdentityConfig {
            self.inner.current()
        }
    }

    #[async_trait::async_trait]
    impl AtprotoIdentityStore for Counting {
        async fn atproto_identity(&self) -> Result<AtprotoIdentityConfig, String> {
            self.inner.atproto_identity().await
        }
        async fn merge_atproto_identity(
            &self,
            replica: AtprotoIdentityConfig,
        ) -> Result<AtprotoIdentityConfig, String> {
            self.merges.fetch_add(1, Ordering::SeqCst);
            let racing = self.racing.lock().unwrap().take();
            if let Some(key) = racing {
                self.inner
                    .merge_atproto_identity(AtprotoIdentityConfig {
                        rotation_keys: vec![key],
                        ..Default::default()
                    })
                    .await?;
            }
            self.inner.merge_atproto_identity(replica).await
        }
    }

    #[test]
    fn generated_key_is_p256_and_scalar_rederives_pubkey() {
        let key = generate_rotation_key(1_700_000_000);
        assert!(key.pubkey_did_key.starts_with("did:key:zDn"));
        let (curve, point) = decode_did_key(&key.pubkey_did_key).unwrap();
        assert_eq!(curve, DidKeyCurve::P256);

        let sk = p256::SecretKey::from_slice(&*key.secret_scalar).unwrap();
        let rederived = sk.public_key().to_encoded_point(true);
        assert_eq!(rederived.as_bytes(), point.as_slice());
    }

    #[tokio::test]
    async fn mint_reuses_an_unpublished_key() {
        // A failed-mint retry must not churn keys: while the key was never
        // published for any DID, minting again returns the same one.
        let store = Counting::default();
        let first = mint_rotation_key(&store, 1_700_000_000).await.unwrap();
        let second = mint_rotation_key(&store, 1_700_000_001).await.unwrap();
        assert_eq!(first, second, "an unpublished key is reused, not churned");
        assert_eq!(store.merges(), 1, "the reuse writes nothing");

        let custody = store.current();
        assert_eq!(custody.rotation_keys.len(), 1);
        assert_eq!(custody.rotation_keys[0].pubkey_did_key, first);
    }

    #[tokio::test]
    async fn mint_never_reuses_a_published_key() {
        // The linkability pin: a key already published for a DID (a retired
        // identity's key) must never ride a second mint — the fresh DID's
        // genesis would share `rotationKeys[0]` with the retired DID's public
        // log, permanently linking them.
        let store = Counting::default();
        let old = mint_rotation_key(&store, 1_700_000_000).await.unwrap();
        record_published_binding(&store, "did:plc:retired11111111111111111", &old)
            .await
            .unwrap();

        let fresh = mint_rotation_key(&store, 1_700_000_002).await.unwrap();
        assert_ne!(fresh, old, "a published key must never ride a second mint");

        let custody = store.current();
        assert_eq!(custody.rotation_keys.len(), 2, "old key stays held");
        assert!(
            custody.rotation_keys.iter().any(|k| k.pubkey_did_key == old
                && k.published_for_dids == vec!["did:plc:retired11111111111111111"]),
            "the burned key keeps its binding"
        );
    }

    #[tokio::test]
    async fn mint_returns_the_stored_unpublished_key_after_a_concurrent_join() {
        // A concurrent device's unpublished key joined in while this device
        // minted: the pubkey handed to the enable flow must be the STORED
        // ring's first unpublished key (in the ring's canonical order), and
        // this device's fresh key stays held as a spare — never lost.
        let store = Counting::default();
        let theirs = generate_rotation_key(1);
        *store.racing.lock().unwrap() = Some(theirs.clone());
        let minted = mint_rotation_key(&store, 2).await.unwrap();
        let custody = store.current();
        assert_eq!(custody.rotation_keys.len(), 2, "both keys held");
        let first_unpublished = custody
            .rotation_keys
            .iter()
            .find(|k| k.published_for_dids.is_empty())
            .unwrap();
        assert_eq!(
            first_unpublished.pubkey_did_key, minted,
            "the pubkey handed to the enable flow must be the stored ring's choice"
        );
        assert!(
            custody
                .rotation_keys
                .iter()
                .any(|k| k.pubkey_did_key == theirs.pubkey_did_key),
            "the concurrent device's key is never lost to the race"
        );
    }

    #[tokio::test]
    async fn record_binding_is_idempotent_and_ignores_unheld_keys() {
        let store = Counting::default();
        let key = mint_rotation_key(&store, 1_700_000_000).await.unwrap();
        assert_eq!(store.merges(), 1, "the mint's own write");

        record_published_binding(&store, "did:plc:aaaa", &key)
            .await
            .unwrap();
        assert_eq!(store.merges(), 2, "first binding persists");
        assert_eq!(
            store.current().rotation_keys[0].published_for_dids,
            vec!["did:plc:aaaa"]
        );

        // Idempotent: a converge pass re-recording the same fact writes
        // nothing (no write on every status convergence).
        record_published_binding(&store, "did:plc:aaaa", &key)
            .await
            .unwrap();
        assert_eq!(store.merges(), 2, "second record is a no-op");

        // An unheld pubkey has no entry to burn: recorded nowhere, no write.
        record_published_binding(&store, "did:plc:bbbb", "did:key:zDnaeUNHELD")
            .await
            .unwrap();
        assert_eq!(store.merges(), 2, "unheld key is a no-op");
        assert_eq!(store.current().rotation_keys.len(), 1);
    }

    /// The audit floor's second source: freezing a nest claim
    /// must survive the nest un-claiming it, cost nothing in the steady state,
    /// and refuse to floor a DID that has no directory log to audit.
    #[tokio::test]
    async fn a_frozen_nest_claim_is_idempotent_and_only_ever_did_plc() {
        let store = Counting::default();

        record_nest_named_did(&store, "did:plc:bbbb").await.unwrap();
        record_nest_named_did(&store, "did:plc:aaaa").await.unwrap();
        assert_eq!(store.merges(), 2, "each new claim persists");
        assert_eq!(
            store.current().nest_named_dids,
            vec!["did:plc:aaaa", "did:plc:bbbb"],
            "sorted, the fold's canonical order"
        );

        // Idempotent: every convergence and every sweep calls this, so the
        // steady state must not write.
        record_nest_named_did(&store, "did:plc:aaaa").await.unwrap();
        assert_eq!(store.merges(), 2, "re-freezing a known claim is a no-op");

        // A did:web has no PLC log to read, so flooring it would pin an audit
        // that can never run.
        record_nest_named_did(&store, "did:web:example.com")
            .await
            .unwrap();
        assert_eq!(store.merges(), 2, "an unauditable DID is never frozen");
        assert_eq!(
            store.current().nest_named_dids,
            vec!["did:plc:aaaa", "did:plc:bbbb"]
        );
    }

    /// The floor must reach every device: a claim frozen on one device survives
    /// the two-device union merge, or a box could shrink the audit set simply by
    /// telling a *second* device nothing.
    #[test]
    fn a_frozen_claim_survives_the_two_device_merge() {
        let a = AtprotoIdentityConfig {
            nest_named_dids: vec!["did:plc:aaaa".into()],
            ..Default::default()
        };
        let b = AtprotoIdentityConfig {
            nest_named_dids: vec!["did:plc:bbbb".into()],
            ..Default::default()
        };
        assert_eq!(
            a.merge(&b).nest_named_dids,
            vec!["did:plc:aaaa", "did:plc:bbbb"]
        );
        assert_eq!(
            a.merge(&b).nest_named_dids,
            b.merge(&a).nest_named_dids,
            "commutative, like every other union arm"
        );
    }

    #[tokio::test]
    async fn tombstone_consent_is_idempotent() {
        let store = Counting::default();
        record_tombstone_consent(&store, "did:plc:alice")
            .await
            .unwrap();
        record_tombstone_consent(&store, "did:plc:alice")
            .await
            .unwrap();
        assert_eq!(store.merges(), 1, "re-consenting writes nothing");
        assert_eq!(store.current().tombstone_consents, vec!["did:plc:alice"]);
    }

    #[tokio::test]
    async fn contest_intents_are_scoped_to_the_op_and_idempotent_per_pair() {
        let store = Counting::default();

        record_contest_intent(&store, "did:plc:alice", "bafyA", 10)
            .await
            .unwrap();
        assert_eq!(store.merges(), 1);

        // The SAME decision re-confirmed writes nothing: a converge pass or a
        // double-tap must not churn.
        record_contest_intent(&store, "did:plc:alice", "bafyA", 99)
            .await
            .unwrap();
        assert_eq!(store.merges(), 1, "same (did, cid) is a no-op");

        // A DIFFERENT op on the same DID is a DIFFERENT human decision — it
        // must be recorded separately, never collapsed into a standing
        // "contest anything for this DID" flag (decision 6's auto-actor).
        record_contest_intent(&store, "did:plc:alice", "bafyB", 20)
            .await
            .unwrap();
        assert_eq!(store.merges(), 2);

        let custody = store.current();
        assert_eq!(
            custody
                .contest_intents
                .iter()
                .map(|i| (i.did.as_str(), i.contested_op_cid.as_str(), i.requested_at))
                .collect::<Vec<_>>(),
            vec![
                ("did:plc:alice", "bafyA", 10),
                ("did:plc:alice", "bafyB", 20),
            ],
            "both decisions stand, and the first keeps its original timestamp"
        );
        // The consent is client-only: nothing about it is a nest-facing field.
        assert!(custody.rotation_keys.is_empty());
    }

    /// A refused door refuses the write — never a silent success the enable
    /// flow would publish a key for.
    #[tokio::test]
    async fn a_refused_door_refuses_the_mint() {
        let store = Arc::new(crate::identity_store::NoAccountRuntime);
        assert!(mint_rotation_key(&*store, 1).await.is_err());
    }
}
