//! `SealedStorage` — the nest's one and only `Storage` impl
//! (`docs/goal/architecture/nest/storage-modes.md`).
//!
//! `search` serves the floor-derived corpus on every nest (no more
//! `SearchNotServerSide`); there is no `process_on_ingest`/`index_on_ingest`/
//! `sign_dkim`/`.mode()` — those retired with the storage-mode axis. ACME/TLS
//! sealing to bridge x25519 pubkeys is the surviving behavior this file
//! exercises alongside search.

use std::sync::Arc;

use fauna_mls::wrapped_blob::{TlsCertBlob, generate_x25519_keypair, unseal_tls_cert};
use fauna_nest::db::CacheDb;
use fauna_nest::db::bridge_service_users::BridgeRole;
use fauna_nest::storage::{AcmeMaterial, SealedStorage, SearchSpec, Storage};

// ── tests ─────────────────────────────────────────────────────────────────────

/// `search` serves the floor-derived corpus (public post bodies) on every
/// nest — the replacement for the retired `SearchNotServerSide` distinction.
#[tokio::test]
async fn sealed_storage_search_serves_the_floor_derived_corpus() {
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::identity::ActorId;

    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));
    let tmpdir = tempfile::tempdir().expect("tempdir");
    let storage = SealedStorage::new(db.clone(), tmpdir.path().to_path_buf());

    let author = ActorId([7u8; 32]);
    let post = Post {
        author,
        created_at: Timestamp(1_700_000_000_000_000),
        body: PostBody::Text {
            content: "hello searchable post body via sealed storage".to_string(),
            facets: vec![],
        },
        references: vec![],
        expires_at: None,
        gated: None,
        content_warning: None,
        origin: None,
    };
    let data = fauna_core::encoding::canonical_encode(&post).unwrap();
    let hash = blake3::hash(&data);
    let post_id: [u8; 32] = *hash.as_bytes();

    // Seed via the same public projection every nest writes on ingest
    // (`CacheDb::put_post` → `insert_and_index`, body = the public preview).
    db.put_post(&post_id, &data, None).await.expect("put_post");

    let hits = storage
        .search(
            &[0u8; 32], // any actor — this is public floor-derived content
            &SearchSpec {
                query: "searchable".into(),
                content_type: None,
                before: None,
                after: None,
                limit: 20,
                offset: 0,
            },
        )
        .await
        .expect("search should succeed on every nest");

    assert!(
        !hits.is_empty(),
        "expected at least one hit for 'searchable'"
    );
    assert!(
        hits.iter().any(|h| h.content_type == "post/text"),
        "expected a hit with content_type 'post/text', got: {:?}",
        hits
    );
}

#[tokio::test]
async fn store_acme_material_writes_pem_and_wrapped_tls_blobs() {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));

    // Register + approve two bridge service users with x25519 keypairs.
    let (mta_sk, mta_pk) = generate_x25519_keypair();
    let (mda_sk, mda_pk) = generate_x25519_keypair();

    let mta_ed = [0x11u8; 32];
    let mda_ed = [0x22u8; 32];

    db.create_pending_bridge_service_user(&mta_ed, BridgeRole::Mta, "mta-1")
        .await
        .expect("create mta pending");
    db.create_pending_bridge_service_user(&mda_ed, BridgeRole::Mda, "mda-1")
        .await
        .expect("create mda pending");

    db.upsert_bridge_x25519(&mta_ed, &mta_pk)
        .await
        .expect("upsert mta x25519");
    db.upsert_bridge_x25519(&mda_ed, &mda_pk)
        .await
        .expect("upsert mda x25519");

    db.approve_bridge_service_user(&mta_ed, None)
        .await
        .expect("approve mta");
    db.approve_bridge_service_user(&mda_ed, None)
        .await
        .expect("approve mda");

    let acme_dir = tempfile::tempdir().expect("acme_dir tempdir");
    let storage = SealedStorage::new(db.clone(), acme_dir.path().to_path_buf());

    let cert = b"-----BEGIN CERTIFICATE-----\naa\n-----END CERTIFICATE-----\n";
    let key = b"-----BEGIN PRIVATE KEY-----\nbb\n-----END PRIVATE KEY-----\n"; // gitleaks:allow

    storage
        .store_acme_material(&AcmeMaterial {
            domain: "example.com",
            cert_chain_pem: cert,
            priv_key_pem: key,
        })
        .await
        .expect("store_acme_material");

    // (1) Nest's own listener PEM was written atomically.
    assert_eq!(
        std::fs::read(acme_dir.path().join("fullchain.pem")).expect("fullchain.pem"),
        cert,
        "fullchain.pem mismatch"
    );
    assert_eq!(
        std::fs::read(acme_dir.path().join("privkey.pem")).expect("privkey.pem"),
        key,
        "privkey.pem mismatch"
    );

    // (2) Each bridge has a wrapped TlsCertBlob that round-trips with the
    // correct recipient secret.
    for (role, bridge_id, sk, wrong_sk) in [
        ("mta", "mta-1", &mta_sk, &mda_sk),
        ("mda", "mda-1", &mda_sk, &mta_sk),
    ] {
        let raw = db
            .get_tls_cert_blob(role, bridge_id, "example.com")
            .await
            .expect("get_tls_cert_blob")
            .unwrap_or_else(|| panic!("no blob stored for {role}/{bridge_id}"));

        let blob = TlsCertBlob::from_canonical_bytes(&raw)
            .unwrap_or_else(|e| panic!("from_canonical_bytes failed for {role}/{bridge_id}: {e}"));

        // Correct secret → round-trip succeeds.
        let bundle = unseal_tls_cert(&blob, sk).unwrap_or_else(|e| {
            panic!("unseal_tls_cert with correct sk failed for {role}/{bridge_id}: {e}")
        });
        assert_eq!(
            &bundle.cert_chain, cert,
            "cert_chain mismatch for {role}/{bridge_id}"
        );
        assert_eq!(
            &bundle.priv_key, key,
            "priv_key mismatch for {role}/{bridge_id}"
        );

        // Wrong secret → fails (HPKE open error).
        let err = unseal_tls_cert(&blob, wrong_sk).expect_err("unseal with wrong sk must fail");
        assert!(
            matches!(err, fauna_mls::wrapped_blob::UnwrapError::HpkeFailed),
            "expected HpkeFailed with wrong key for {role}/{bridge_id}, got {:?}",
            err
        );
    }
}

#[tokio::test]
async fn store_acme_material_skips_bridge_without_x25519_but_succeeds() {
    let db = Arc::new(CacheDb::open_in_memory().expect("in-memory db"));

    // Approve a bridge but do NOT set its x25519 pubkey.
    let pk_no_x25519 = [0x55u8; 32];
    db.create_pending_bridge_service_user(&pk_no_x25519, BridgeRole::Mta, "mta-no-x25519")
        .await
        .unwrap();
    db.approve_bridge_service_user(&pk_no_x25519, None)
        .await
        .unwrap();
    // Note: no upsert_bridge_x25519 call — x25519_pubkey is NULL.

    let acme_dir = tempfile::tempdir().unwrap();
    let storage = SealedStorage::new(db.clone(), acme_dir.path().to_path_buf());

    let cert = b"-----BEGIN CERTIFICATE-----\ncc\n-----END CERTIFICATE-----\n";
    let key = b"-----BEGIN PRIVATE KEY-----\ndd\n-----END PRIVATE KEY-----\n";

    // Must succeed even though the bridge has no x25519 key (warn and skip).
    storage
        .store_acme_material(&AcmeMaterial {
            domain: "example.com",
            cert_chain_pem: cert,
            priv_key_pem: key,
        })
        .await
        .expect("store_acme_material must succeed even with bridge missing x25519");

    // PEM written.
    assert_eq!(
        std::fs::read(acme_dir.path().join("fullchain.pem")).unwrap(),
        cert
    );

    // No blob stored for the x25519-less bridge.
    let blob = db
        .get_tls_cert_blob("mta", "mta-no-x25519", "example.com")
        .await
        .unwrap();
    assert!(
        blob.is_none(),
        "no blob should be stored for a bridge without x25519"
    );
}
