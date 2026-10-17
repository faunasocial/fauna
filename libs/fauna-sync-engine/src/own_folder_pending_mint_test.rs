//! The own-folder pending mint, nest down (`p2p-shared-set-build.md` § Build
//! design — the same-account row leg, decision 2): a write on an OWNER-SEALED
//! folder — no M2 set, no floor — whose nest refuses the connection still
//! seals, mints and signs its pending row, retains the manifest and indexes
//! its chunks for the serve core before the upload fails at the nest. That is
//! what a sibling pulls with no nest answering (`fauna.peer.sync.changes.list`
//! / `manifests.get` / `chunks.pull`), so this pin keeps the mint on the path:
//! the design pass found it already there for every folder engine, and the
//! `transfer_queue` — the byte plane's retry ledger — is never its source.

use std::sync::Arc;

use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::sync_writer_sig::ChangeSigner;

use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::test_engine_with_nest_client_keyed;

const REL: &str = "notes/cabin.txt";
const BODY: &[u8] = b"written while no nest answers\n";
const SET_NONCE: [u8; 32] = [0x4e; 32];

/// An owner-sealed engine (its owner key held, no M2 keys) that signs its
/// records as its own actor, against a nest that refuses every connection
/// (`127.0.0.1:9`).
fn owner_sealed_signing(dir: &std::path::Path) -> SyncEngine {
    let owner = ActorKeypair::from_secret([0x0a; 32]);
    let nest = fauna_client::NestClient::new(
        "http://127.0.0.1:9".to_string(),
        ActorKeypair::from_secret([0x0a; 32]),
    );
    test_engine_with_nest_client_keyed(
        dir.to_path_buf(),
        "__test",
        nest,
        Some(OwnerSealKey::Client(BackupKey::from_bytes([11u8; 32]))),
    )
    .with_change_signer(Arc::new(ChangeSigner::direct(&owner)), Some(SET_NONCE))
}

#[tokio::test]
async fn an_owner_sealed_write_with_the_nest_down_mints_a_signed_serveable_pending_row() {
    let dir = tempfile::tempdir().unwrap();
    let engine = owner_sealed_signing(dir.path());
    std::fs::create_dir_all(dir.path().join("notes")).unwrap();
    std::fs::write(dir.path().join(REL), BODY).unwrap();

    // The nest refuses the connection: the upload cannot complete.
    let outcome = engine.upload_file(REL).await;
    assert!(
        outcome.as_ref().map_or(true, |o| !o.recorded),
        "nothing records with no nest answering"
    );

    // The signed pending row, unsequenced, unstamped by any generation.
    let pending = engine.db().own_pending_changes(16).unwrap();
    assert_eq!(pending.len(), 1, "one own-pending row for the write");
    let row = &pending[0];
    assert_eq!(row.path, REL);
    assert!(row.seq.is_none(), "no nest coordinate");
    assert_eq!(
        row.content_key_version, None,
        "an owner-sealed set mints with no M2 generation"
    );
    assert!(
        row.signature.is_some() && row.signer_key.is_some(),
        "signed as served, so a sibling's judge verifies it"
    );

    // The manifest it names is retained — the sibling is its only source.
    let manifest_hex = row
        .manifest_hash
        .clone()
        .expect("a create names a manifest");
    let retained = engine
        .db()
        .retained_manifest(&manifest_hex)
        .unwrap()
        .expect("the manifest is retained for the manifest door");
    let manifest: fauna_core::chunk::ChunkManifest =
        fauna_core::encoding::canonical_decode(&retained.bytes).unwrap();

    // Every chunk the manifest lists is serveable through the one serve core.
    let keys = manifest.store_keys();
    assert!(!keys.is_empty());
    for key in keys {
        let body = engine
            .serve_chunk(&key.digest())
            .await
            .unwrap()
            .expect("the chunk door can serve the body with the nest down");
        assert!(!body.is_empty());
    }
}
