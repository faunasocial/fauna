//! The nest-level ActivityPub **instance actor** — key custody + minting.
//!
//! Every other AP identity is per-user: an actor exists because a user enabled
//! federation, and outbound activities are signed with that user's key. One
//! outbound request has no such context — the remote-actor fetch
//! (`inbox_routes::fetch_remote_actor`). It runs on the *arrival* of an
//! activity, before we have verified anything, precisely to obtain the key that
//! verification needs; at that moment we do not yet know which local user (if
//! any) the activity concerns, and for a shared-inbox delivery there may be
//! several. So there is no per-account key to reach for.
//!
//! An `AUTHORIZED_FETCH` ("secure mode") peer refuses unsigned `GET`s of its
//! objects, including its own actor document. That turns the missing signature
//! into a federation break rather than a stylistic gap: we cannot fetch the
//! sender's key, so we cannot verify their `Follow`, so we reject it, so the
//! follow never completes (`activitypub.md` § Implementation status → gap 4).
//!
//! The fix every AP implementation converged on is a nest-level **instance
//! actor**: one `Application` actor owned by the server rather than by a user,
//! whose key signs server-context requests. Fauna's serves at
//! `https://<domain>/ap/instance` (§ Serving lives in `actor_routes`).
//!
//! **Custody is the per-account rule, unchanged** (`activitypub.md`
//! § Security posture → Key custody): the RSA private key rests encrypted under
//! the nest identity key via the same `key_crypto` helpers, and is decrypted
//! only transiently to sign. A second custody scheme for a second key class is
//! exactly the drift that makes a security posture unauditable.

use anyhow::{Context, Result};
use fauna_bridge_activitypub::identity::generate_rsa_keypair;
use fauna_bridge_activitypub::translate::{instance_actor_url, instance_key_id};
use fauna_bridge_activitypub::types::ApPerson;

use crate::routes::AppState;

/// The instance actor's identity plus its **decrypted** signing key.
///
/// Short-lived by construction: callers build one, sign with it, and drop it.
pub struct InstanceActor {
    pub actor_url: String,
    /// The `keyId` a remote dereferences to find `public_key_pem`.
    pub key_id: String,
    pub public_key_pem: String,
    pub privkey_der: Vec<u8>,
}

/// The header triple a signed outbound `GET` carries.
///
/// `Host` is included because it is one of the signed headers: the remote
/// rebuilds the signing string from the headers it received, so a `Host` that
/// differs from the one we signed over fails verification.
pub struct SignedGet {
    pub host: String,
    pub date: String,
    pub signature: String,
}

impl InstanceActor {
    /// Sign a `GET` of `url` as this nest.
    ///
    /// draft-cavage over `(request-target) host date` — no `digest`, since a
    /// GET has no body and some verifiers reject a digest of nothing.
    pub fn sign_get(&self, url: &url::Url, epoch_secs: i64) -> Result<SignedGet> {
        let host = url
            .host_str()
            .context("no host in the URL being signed")?
            .to_owned();
        let date = super::sync_worker::format_http_date(epoch_secs);
        let signature = fauna_bridge_activitypub::http_signatures::build_signature_header(
            &self.key_id,
            &self.privkey_der,
            "get",
            &super::outbound::signing_path(url),
            &host,
            &date,
            None,
        )?;
        Ok(SignedGet {
            host,
            date,
            signature,
        })
    }
}

/// Load the instance actor, minting it on first use.
///
/// Idempotent under concurrency without a lock: a racing pair of callers both
/// generate a keypair, both `INSERT OR IGNORE` (the single-row primary key
/// makes the second a no-op), and both then read back the row that won — so
/// they agree on one key. The loser's keypair is simply dropped, never stored,
/// so a remote can never see two keys for this actor.
///
/// Also called once at boot (`lib.rs`, beside the delivery worker) so the
/// ~200 ms RSA generation lands there rather than inside the first inbox POST.
pub async fn ensure(state: &AppState) -> Result<InstanceActor> {
    let domain = state
        .handle_domain_if_set()
        .context("ActivityPub domain not configured")?;

    if let Some(actor) = load(state, &domain).await? {
        return Ok(actor);
    }

    // RSA-2048 generation is ~200 ms of pure CPU; off the async runtime so a
    // first-use mint cannot stall unrelated connections on this worker thread.
    let (privkey_der, public_key_pem) = tokio::task::spawn_blocking(generate_rsa_keypair)
        .await
        .context("instance-actor keygen task")?
        .context("instance-actor keygen")?;

    // Seal under the deployment seed `nest_keypair` holds, read on the
    // connection the row is inserted on, so the read, the seal, the insert and
    // the read-back share one hold of the database guard — never a serving
    // generation's copy (`crate::nest_kek`'s module docs). Boot cannot mint this
    // row on a domainless nest, so after a later domain claim the first mint can
    // be a request answered by a generation that a deployment-seed rotation has
    // retired but not yet torn down, which still holds the retired seed.
    //
    // Read back rather than returning what we just generated: under a race the
    // stored row may be the *other* caller's key, and the key we sign with must
    // be the one the served document publishes. It opens under the seed read in
    // the same hold, the seed it was sealed under.
    let (deployment_seed, stored) = {
        let conn = state.db.conn().await;
        let deployment_seed = crate::nest_kek::require_deployment_seed(&conn)?;
        let encrypted = super::key_crypto::encrypt_rsa_privkey(&deployment_seed, &privkey_der)
            .context("encrypting the instance-actor private key")?;
        super::db_helpers::insert_instance_actor_if_absent(&conn, &encrypted, &public_key_pem)
            .context("storing the instance actor")?;
        let stored = super::db_helpers::get_instance_actor(&conn)
            .context("reading the instance actor")?
            .context("instance actor absent immediately after insert")?;
        (deployment_seed, stored)
    };
    open(&deployment_seed, &domain, stored)
}

/// Read + decrypt the stored instance actor, if one has been minted.
async fn load(state: &AppState, domain: &str) -> Result<Option<InstanceActor>> {
    let stored = {
        let conn = state.db.conn().await;
        super::db_helpers::get_instance_actor(&conn).context("reading the instance actor")?
    };
    stored
        .map(|stored| open(&state.nest_identity.signing_key.to_bytes(), domain, stored))
        .transpose()
}

/// Open a stored `(encrypted_privkey, public_key_pem)` row under `deployment_seed`.
fn open(
    deployment_seed: &[u8; 32],
    domain: &str,
    (encrypted_privkey, public_key_pem): (Vec<u8>, String),
) -> Result<InstanceActor> {
    let privkey_der = super::key_crypto::decrypt_rsa_privkey(deployment_seed, &encrypted_privkey)
        .context("decrypting the instance-actor private key")?;
    Ok(InstanceActor {
        actor_url: instance_actor_url(domain),
        key_id: instance_key_id(domain),
        public_key_pem,
        privkey_der,
    })
}

/// The public actor document a remote dereferences from our `keyId`.
///
/// Mints the actor if it does not exist yet: a peer verifying our signature
/// fetches this document, so it must never 404 on a nest whose key was minted
/// by a path that ran before this one.
pub async fn document(state: &AppState) -> Result<ApPerson> {
    let actor = ensure(state).await?;
    let domain = state
        .handle_domain_if_set()
        .context("ActivityPub domain not configured")?;
    Ok(fauna_bridge_activitypub::translate::instance_actor_person(
        &domain,
        actor.public_key_pem,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::CacheDb;
    use fauna_bridge_activitypub::http_signatures::{parse_signature_header, verify_signature};
    use std::sync::Arc;

    async fn state_with_domain(domain: &str) -> Arc<AppState> {
        let state = crate::activitypub::ap_state_for_test().await;
        // The domain a claimed nest learns — the same `identity_domain` cache
        // `apply_primary_identity` swaps at claim, which is what every AP
        // surface now reads (`ActivityPubState` docs).
        state
            .identity_domain
            .store(Some(Arc::new(domain.to_string())));
        Arc::new(state)
    }

    /// Minting is once-per-nest. A second call must return the *same* key: a
    /// remote caches our public key against the actor URL, so silently re-minting
    /// would invalidate every peer's copy and break verification fleet-wide.
    #[tokio::test(flavor = "multi_thread")]
    async fn ensure_is_idempotent() {
        let state = state_with_domain("nest.test").await;
        let first = ensure(&state).await.expect("mints");
        let second = ensure(&state).await.expect("reads back");
        assert_eq!(first.public_key_pem, second.public_key_pem);
        assert_eq!(first.privkey_der, second.privkey_der);
        assert_eq!(first.actor_url, "https://nest.test/ap/instance");
        assert_eq!(first.key_id, "https://nest.test/ap/instance#main-key");
    }

    /// Concurrent callers converge on one key. The mint has no lock — it relies
    /// on `INSERT OR IGNORE` against a single-row primary key plus a read-back —
    /// so the racing losers must adopt the winner's key rather than sign with
    /// the one they generated and threw away.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_mints_converge_on_one_key() {
        let state = state_with_domain("nest.test").await;
        let mut handles = Vec::new();
        for _ in 0..4 {
            let state = state.clone();
            // spawn-ok(test)
            handles.push(tokio::spawn(async move { ensure(&state).await.unwrap() }));
        }
        let mut keys = Vec::new();
        for handle in handles {
            keys.push(handle.await.expect("mint task").public_key_pem);
        }
        assert!(
            keys.windows(2).all(|w| w[0] == w[1]),
            "racing mints produced different keys: {keys:#?}",
        );
    }

    /// Custody is the per-account rule: the private key is at rest **encrypted**
    /// under the nest identity key (`activitypub.md` § Security posture → Key
    /// custody). Reading the row must not yield signing capability.
    #[tokio::test(flavor = "multi_thread")]
    async fn private_key_rests_encrypted() {
        let state = state_with_domain("nest.test").await;
        let actor = ensure(&state).await.expect("mints");

        let stored = {
            let conn = state.db.conn().await;
            super::super::db_helpers::get_instance_actor(&conn)
                .expect("read")
                .expect("row exists")
        };
        assert_ne!(
            stored.0, actor.privkey_der,
            "the private key must not be stored in the clear",
        );
        // …and it is genuinely the same key once decrypted with the nest key.
        let decrypted = super::super::key_crypto::decrypt_rsa_privkey(
            &state.nest_identity.signing_key.to_bytes(),
            &stored.0,
        )
        .expect("decrypts under the nest identity key");
        assert_eq!(decrypted, actor.privkey_der);
    }

    /// The whole point of the slice: a `GET` we sign **verifies** against the
    /// public key our own document publishes.
    ///
    /// This is the assertion that would have caught a wrong method string, a
    /// dropped `Host`, or a `Date` in the wrong grammar — all of which produce a
    /// well-formed header that every peer rejects, and none of which a
    /// "the header is present" check would notice.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_signed_get_verifies_against_the_published_key() {
        let state = state_with_domain("nest.test").await;
        let actor = ensure(&state).await.expect("mints");
        let url = url::Url::parse("https://mastodon.example/users/alice").unwrap();

        let signed = actor.sign_get(&url, 1_784_419_200).expect("signs");
        let parsed = parse_signature_header(&signed.signature).expect("parses");
        assert_eq!(parsed.key_id, actor.key_id);

        verify_signature(
            &parsed,
            &actor.public_key_pem,
            "get",
            "/users/alice",
            |name| match name {
                "host" => Some(signed.host.clone()),
                "date" => Some(signed.date.clone()),
                _ => None,
            },
        )
        .expect("the signature verifies against our published key");
    }

    /// A query string is part of the request-target the remote reconstructs, so
    /// it must be part of what we signed. Signing the bare path yields a
    /// signature that fails only against peers whose URLs carry a query — the
    /// kind of gap that survives every test until production.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_signed_get_covers_the_query_string() {
        let state = state_with_domain("nest.test").await;
        let actor = ensure(&state).await.expect("mints");
        let url = url::Url::parse("https://peer.example/actor?page=2").unwrap();

        let signed = actor.sign_get(&url, 1_784_419_200).expect("signs");
        let parsed = parse_signature_header(&signed.signature).expect("parses");

        verify_signature(
            &parsed,
            &actor.public_key_pem,
            "get",
            "/actor?page=2",
            |name| match name {
                "host" => Some(signed.host.clone()),
                "date" => Some(signed.date.clone()),
                _ => None,
            },
        )
        .expect("the signature covers path AND query");
    }

    /// The signed `Date` is RFC 7231 IMF-fixdate. Mastodon parses it with Ruby's
    /// strict `Time.httpdate`; anything else fails verification remotely while
    /// looking perfectly fine locally.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_signed_date_is_imf_fixdate() {
        let state = state_with_domain("nest.test").await;
        let actor = ensure(&state).await.expect("mints");
        let url = url::Url::parse("https://peer.example/actor").unwrap();
        let signed = actor.sign_get(&url, 1_784_419_200).expect("signs");
        assert_eq!(signed.date, "Sun, 19 Jul 2026 00:00:00 GMT");
    }

    /// A nest with no AP domain has no actor URL to be, so minting refuses
    /// rather than inventing one. The fetch path treats this as "dial unsigned",
    /// which is the pre-existing behaviour.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_domain_means_no_instance_actor() {
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        {
            let conn = db.conn().await;
            conn.execute_batch(fauna_bridge_activitypub::db::CREATE_TABLES_SQL)
                .expect("AP schema");
        }
        let state = Arc::new(AppState::for_test(db));
        assert!(ensure(&state).await.is_err());
    }

    /// The deployment-seed rotation's hand-off window, driven causally: the
    /// ceremony has committed while a serving generation not yet torn down
    /// still holds the retired seed (`box-recovery.md` § Deployment-seed
    /// rotation → *The bounded hand-off window*). Boot skips the mint on a
    /// domainless nest, so after a later domain claim the first mint can be a
    /// request that generation answers. It must seal under the seed the
    /// database holds: a row sealed under the retired copy never opens again,
    /// and the satellite walk refuses it on every later rotation.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_first_mint_in_the_rotation_window_seals_under_the_successor_seed() {
        use crate::test_support::{
            every_row_opens_under, seat_deployment_seed, serving_generation,
        };
        use zeroize::Zeroizing;

        let (a, b, c) = (
            Zeroizing::new([0xa1u8; 32]),
            Zeroizing::new([0xb2u8; 32]),
            Zeroizing::new([0xc3u8; 32]),
        );
        let db = Arc::new(CacheDb::open_in_memory().expect("in-memory CacheDb"));
        crate::activitypub::init_db(&db).await.expect("AP schema");
        seat_deployment_seed(&db, &a).await;
        let outgoing = serving_generation(db.clone(), &a);
        outgoing
            .identity_domain
            .store(Some(Arc::new("nest.test".to_string())));

        db.rotate_deployment_seed(&a, &b)
            .await
            .expect("the ceremony runs")
            .expect("and commits");

        ensure(&outgoing)
            .await
            .expect("the outgoing generation mints a usable instance actor");
        assert!(
            every_row_opens_under(
                &db,
                "ap_instance_actor",
                "encrypted_privkey",
                crate::nest_kek::ACTIVITYPUB_RSA_CONTEXT,
                &b,
            )
            .await,
            "a first mint answered by the outgoing generation sealed the instance actor under \
             the retired deployment seed"
        );

        db.rotate_deployment_seed(&b, &c)
            .await
            .expect("the next rotation commits — the instance actor does not wedge the walk")
            .expect("and is no rule refusal");
    }
}
