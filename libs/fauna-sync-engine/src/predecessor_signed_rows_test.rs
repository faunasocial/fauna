//! **The engine's half of the succession-crossing reader rule** —
//! `mls-group-key-material.md` § M2 → *Writer-signed change records*, ruling
//! (8)(c) and the p2p relay clause: once the reader binding names the
//! account's predecessors, a row a retired identity signed is admitted as
//! signed by that identity — never as this device's echo, relayed with its
//! cert under it — and every open of its bytes or its label is offered only
//! that identity's root and its predecessors' roots, a failure there being a
//! noted skip, never a transient hold.
//!
//! Tier_1, over the crate's in-process harnesses: the blob plane on a mock
//! HTTP server (`download_file_bytes_test`) and the apply loop over hand-built
//! rows (`anchor_accounting_test`'s shape).

use fauna_core::crypto::{BackupKey, OwnerSealKey};
use fauna_core::data::ContentHash;
use fauna_core::file_download::PredecessorSealKey;
use fauna_core::identity::ActorKeypair;
use fauna_core::path_crypto::LabelRoot;
use fauna_protocol::sync::SyncChange;
use fauna_protocol::sync_row_verify::ReaderBinding;
use fauna_protocol::sync_writer_sig::{ChangeSigner, SignedChange};
use wiremock::MockServer;

use crate::download_file_bytes_test::{
    seed_synced_entry, store_sealed_fixture, test_sync_engine_as,
};
use crate::engine::SyncEngine;
use crate::pull_remote_changes_test::{our_device_hex, test_engine_with_keys};
use crate::test_support::MockNest;

const SET_NONCE: [u8; 32] = [7; 32];

fn successor() -> ActorKeypair {
    ActorKeypair::from_secret([0x51; 32])
}

fn predecessor() -> ActorKeypair {
    ActorKeypair::from_secret([0x52; 32])
}

fn successor_key() -> BackupKey {
    BackupKey::from_bytes([0x61; 32])
}

fn predecessor_key() -> BackupKey {
    BackupKey::from_bytes([0x62; 32])
}

/// The binding `engine_lifecycle` installs for an owned set of an account
/// that succeeded from `predecessors` (ruling (8)(b), source (ii)).
fn binding(own: &ActorKeypair, predecessors: &[&ActorKeypair]) -> ReaderBinding {
    ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(own.actor_id().0),
        account: Some(own.actor_id().0),
        account_predecessors: predecessors.iter().map(|p| p.actor_id().0).collect(),
        ..Default::default()
    }
}

/// A row `signer` signed directly under the set's nonce, as the set's home
/// nest serves it after a succession moved the corpus: `author_actor_id`
/// re-pointed to `served_as`, the signature still over a statement naming
/// the signer (ruling (8), the moved row).
fn moved_row(
    signer: &ActorKeypair,
    served_as: &ActorKeypair,
    seq: i64,
    manifest: Option<ContentHash>,
    device: &str,
) -> SyncChange {
    row_signed_under(SET_NONCE, signer, served_as, seq, manifest, device)
}

/// [`moved_row`] with the nonce its signature's statement names.
fn row_signed_under(
    nonce: [u8; 32],
    signer: &ActorKeypair,
    served_as: &ActorKeypair,
    seq: i64,
    manifest: Option<ContentHash>,
    device: &str,
) -> SyncChange {
    let mut row = SyncChange {
        seq,
        path_hash: hex::encode(fauna_core::sync::path_hash("inherited.bin")),
        manifest_hash: manifest.map(|m| hex::encode(m.digest())),
        size_bytes: 10,
        change_type: "create".into(),
        created_at: 1_000,
        device_id: Some(device.to_string()),
        author_actor_id: Some(signer.actor_id().to_hex()),
        path_sealed: Some(fauna_protocol::ByteBuf::from(vec![1, 2, 3])),
        derived_through: Some(seq - 1),
        ..Default::default()
    };
    let key = ChangeSigner::direct(signer);
    let statement = SignedChange::for_row(&row, nonce).unwrap();
    row.signature = Some(fauna_protocol::ByteBuf::from(
        key.sign_statement(&statement).to_vec(),
    ));
    row.signer_key = Some(fauna_protocol::ByteBuf::from(key.signer_key().to_vec()));
    row.author_actor_id = Some(served_as.actor_id().to_hex());
    row
}

/// `writer-signed-change-records.md` ruling (11)(b), the `signature_invalid`
/// trigger at its source: a member's engine that refuses rows of the set as
/// verifying under no nonce it holds asks its host for the envelope to be
/// re-fetched — once for the batch, not once per row — while the set's own
/// owner, whose nonce is custody's, never asks.
#[tokio::test]
async fn a_members_signature_refusal_asks_for_the_envelope_once_per_batch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let server = MockServer::start().await;
    let (owner, member) = (predecessor(), successor());
    let watch = tempfile::tempdir().unwrap();
    let counted = |engine: &SyncEngine| {
        let asked = std::sync::Arc::new(AtomicUsize::new(0));
        let count = std::sync::Arc::clone(&asked);
        engine.set_custody_refetch_request(std::sync::Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        asked
    };
    // The owner re-minted without an epoch advance: its rows are signed under
    // a nonce this member's engine was not built with.
    let rows = || -> Vec<SyncChange> {
        (5..8)
            .map(|seq| {
                row_signed_under([9; 32], &owner, &owner, seq, None, &hex::encode([0xAA; 32]))
            })
            .collect()
    };

    let engine = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor_key()),
        successor(),
    );
    engine.set_reader_binding(ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(owner.actor_id().0),
        account: Some(member.actor_id().0),
        ..Default::default()
    });
    let asked = counted(&engine);
    let (kept, _) = engine.verify_served_rows(rows(), &[]).await;
    assert!(
        kept.is_empty(),
        "refused: no nonce the member holds verifies"
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "one request for the batch, not one per refused row"
    );

    // A row that verifies asks for nothing.
    let good = row_signed_under(SET_NONCE, &owner, &owner, 9, None, &hex::encode([0xAA; 32]));
    engine.verify_served_rows(vec![good], &[]).await;
    assert_eq!(asked.load(Ordering::SeqCst), 1);

    // The owner's own engine: a row that does not verify is not a stale
    // envelope — there is none to fetch.
    let owning = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(predecessor_key()),
        predecessor(),
    );
    owning.set_reader_binding(binding(&owner, &[]));
    let asked = counted(&owning);
    let (kept, _) = owning.verify_served_rows(rows(), &[]).await;
    assert!(kept.is_empty());
    assert_eq!(asked.load(Ordering::SeqCst), 0, "an owner never asks");
}

/// Admitted as signed by the predecessor (its author read as the predecessor
/// from here on), never this device's own echo even carrying this device's
/// id, and retained for the p2p relay under the predecessor — and refused, as
/// before, by a reader whose binding names no predecessor.
#[tokio::test]
async fn a_predecessor_signed_row_is_admitted_as_signed_by_the_predecessor() {
    let server = MockServer::start().await;
    let (s, p) = (successor(), predecessor());
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor_key()),
        successor(),
    );
    engine.set_reader_binding(binding(&s, &[&p]));
    // This device's own id: a predecessor-signed row is never its echo.
    let row = moved_row(&p, &s, 5, None, &our_device_hex());

    let (kept, held) = engine.verify_served_rows(vec![row.clone()], &[]).await;
    assert_eq!(held, None);
    assert_eq!(kept.len(), 1, "admitted");
    assert_eq!(
        kept[0].author_actor_id.as_deref(),
        Some(p.actor_id().to_hex().as_str()),
        "read as signed by the predecessor from here on"
    );
    let superseding = SyncEngine::content_superseding_seq_by_path(
        &kept,
        &our_device_hex(),
        &s.actor_id().to_hex(),
    );
    assert!(
        kept[0]
            .path
            .as_deref()
            .is_none_or(|path| superseding.contains_key(path)),
        "never this device's echo"
    );
    let relayed = engine.db().relayed_changes_since(0, 10).unwrap();
    assert_eq!(relayed.len(), 1, "retained for the p2p relay");
    assert_eq!(relayed[0].author_actor_id, p.actor_id().to_hex());

    let no_predecessors = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor_key()),
        successor(),
    );
    no_predecessors.set_reader_binding(binding(&s, &[]));
    let (kept, _) = no_predecessors.verify_served_rows(vec![row], &[]).await;
    assert!(kept.is_empty(), "no own-account source: refused as before");
}

/// The own-echo test reads the signed actor: of two rows from this device's
/// id, the one signed as the current identity is its echo, the predecessor's
/// is not.
#[test]
fn only_a_row_signed_as_the_current_identity_is_this_devices_echo() {
    let (s, p) = (successor(), predecessor());
    let row = |author: &ActorKeypair| SyncChange {
        seq: 3,
        path: Some("inherited.bin".into()),
        path_hash: hex::encode(fauna_core::sync::path_hash("inherited.bin")),
        change_type: "create".into(),
        device_id: Some(our_device_hex()),
        author_actor_id: Some(author.actor_id().to_hex()),
        ..Default::default()
    };
    let own = &s.actor_id().to_hex();
    assert!(
        SyncEngine::content_superseding_seq_by_path(&[row(&s)], &our_device_hex(), own).is_empty(),
        "the current identity's row from this device is its echo"
    );
    assert!(
        SyncEngine::content_superseding_seq_by_path(&[row(&p)], &our_device_hex(), own)
            .contains_key("inherited.bin"),
        "a predecessor's row from this device's id is not"
    );
}

/// **The attack, on the engine's byte plane**: a predecessor's signature
/// naming a manifest the current root sealed does not open — a noted skip
/// (`SIGNER_BOUND`), never a transient hold — while the same signature opens
/// what the predecessor's own root sealed, and a row signed as the current
/// identity opens the current root's manifest.
#[tokio::test]
async fn a_predecessor_signed_record_opens_only_under_its_signers_roots() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let (s, p) = (successor(), predecessor());
    let body: Vec<u8> = (0..4_000u32).map(|i| (i % 251) as u8).collect();
    let under_current =
        store_sealed_fixture(&store, &body, &successor_key().convergent_chunk_root());
    let mut inherited = body.clone();
    inherited.reverse();
    let under_predecessor = store_sealed_fixture(
        &store,
        &inherited,
        &predecessor_key().convergent_chunk_root(),
    );

    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor_key()),
        successor(),
    );
    engine.set_predecessor_backup_keys(PredecessorSealKey::chain([(
        p.actor_id(),
        predecessor_key(),
    )]));
    engine.set_reader_binding(binding(&s, &[&p]));
    let peer = "ee".repeat(32);
    let (kept, _) = engine
        .verify_served_rows(
            vec![
                moved_row(&p, &s, 5, Some(under_current), &peer),
                moved_row(&p, &s, 6, Some(under_predecessor), &peer),
            ],
            &[],
        )
        .await;
    assert_eq!(kept.len(), 2);

    let err = engine
        .download_file_bytes_by_manifest(under_current, None, "inherited.bin")
        .await
        .expect_err("a predecessor's signature must not open a current-root manifest");
    assert_eq!(
        fauna_core::apply_failure::permanent_reason(&err),
        Some(fauna_core::apply_failure::PermanentApplyFailure::SIGNER_BOUND.reason),
        "a noted skip, never a transient hold: {err:#}"
    );
    assert_eq!(
        engine
            .download_file_bytes_by_manifest(under_predecessor, None, "inherited.bin")
            .await
            .expect("the predecessor's own root opens what it sealed"),
        inherited
    );

    // The current identity's own row naming the same manifest vouches for it.
    let (kept, _) = engine
        .verify_served_rows(vec![moved_row(&s, &s, 7, Some(under_current), &peer)], &[])
        .await;
    assert_eq!(kept.len(), 1);
    assert_eq!(
        engine
            .download_file_bytes_by_manifest(under_current, None, "inherited.bin")
            .await
            .expect("signed as the current identity: the current root opens it"),
        body
    );
}

/// A predecessor-signed row whose (unstamped) label opens under none of the
/// roots its signer may open — here sealed under the current root — is a noted
/// skip: recorded, the anchor past it, the batch not deferred. Today's
/// `NoRoot` would have capped the batch and held the anchor below it for ever.
#[tokio::test]
async fn a_predecessor_signed_label_no_allowed_root_opens_is_a_noted_skip() {
    let watch = tempfile::tempdir().unwrap();
    let mut engine = test_engine_with_keys(
        watch.path().to_path_buf(),
        Some(OwnerSealKey::Client(successor_key())),
        None,
        None,
    );
    let p = predecessor();
    engine.set_predecessor_backup_keys(PredecessorSealKey::chain([(
        p.actor_id(),
        predecessor_key(),
    )]));
    let own_id = fauna_core::identity::ActorId::from_hex(&engine.owner_actor_id_hex()).unwrap();
    engine.set_reader_binding(ReaderBinding {
        set_nonce: Some(SET_NONCE),
        owner: Some(own_id.0),
        account: Some(own_id.0),
        account_predecessors: vec![p.actor_id().0],
        ..Default::default()
    });
    let path = "after-the-ceremony.txt";
    let path_hash = hex::encode(fauna_core::sync::path_hash(path));
    // The post-verify row: its author is the identity it was signed as.
    let planted = SyncChange {
        seq: 9,
        path_hash: path_hash.clone(),
        change_type: "delete".into(),
        created_at: 1_700_000_000_000,
        path_sealed: Some(
            fauna_core::label_custody::seal_path(&LabelRoot::owner_of(&successor_key()), path)
                .unwrap()
                .into(),
        ),
        device_id: Some("peerpeerpeer".into()),
        author_actor_id: Some(p.actor_id().to_hex()),
        ..Default::default()
    };
    let mut changes = vec![planted];
    engine.open_sealed_change_paths(&mut changes);
    assert_eq!(
        changes[0].path, None,
        "the current root is not offered to it"
    );
    let batch = engine.apply_remote_changes(&changes, 0).await.unwrap();
    assert!(!batch.deferred, "a noted skip, never a transient hold");
    assert_eq!(engine.db().get_anchor().unwrap(), 9, "the cursor passes it");
    let failures: Vec<_> = engine
        .db()
        .list_unresolved_conflicts()
        .unwrap()
        .into_iter()
        .filter(|(_, row_path, kind, _, _)| kind == "catchup_failed" && *row_path == path_hash)
        .collect();
    assert_eq!(failures.len(), 1, "recorded as permanently un-appliable");
}

// ── The restore re-seal (ruling (8)(d), the restore sentence) ───────────────

/// A successor's engine over the mock nest at `uri`, holding the predecessor's
/// key paired with its identity and a binding that names it.
fn successor_engine(uri: &str, watch: &std::path::Path) -> SyncEngine {
    let (s, p) = (successor(), predecessor());
    let mut engine =
        test_sync_engine_as(uri, watch.to_path_buf(), Some(successor_key()), successor());
    engine.set_predecessor_backup_keys(PredecessorSealKey::chain([(
        p.actor_id(),
        predecessor_key(),
    )]));
    engine.set_reader_binding(binding(&s, &[&p]));
    engine
}

/// A Library blob — one whole-file primary sealed under the bare `key`, as a
/// Media-page upload rests.
fn store_blob(
    store: &crate::test_support::BlobStore,
    plaintext: &[u8],
    key: &BackupKey,
) -> ContentHash {
    let sealed = fauna_core::crypto::encrypt_backup_chunk(key, plaintext).unwrap();
    let hex = blake3::hash(&sealed).to_hex().to_string();
    store.blobs.lock().unwrap().insert(hex.clone(), sealed);
    ContentHash::from_digest_raw(fauna_core::hex32::decode(&hex).unwrap())
}

/// How many sealed artifacts the store holds — a refused re-seal adds none.
fn stored(store: &crate::test_support::BlobStore) -> (usize, usize, usize) {
    (
        store.chunks.lock().unwrap().len(),
        store.manifests.lock().unwrap().len(),
        store.blob_posts(),
    )
}

/// `manifest` as a device holding the current root alone reads it — no
/// retired key, no predecessor in its binding.
async fn read_under_the_current_root_alone(
    uri: &str,
    watch: &std::path::Path,
    manifest: ContentHash,
) -> anyhow::Result<Vec<u8>> {
    test_sync_engine_as(uri, watch.to_path_buf(), Some(successor_key()), successor())
        .download_file_bytes_by_manifest(manifest, None, "inherited.bin")
        .await
}

/// An inherited unstamped version the predecessor's root sealed is re-sealed:
/// the manifest the restore records is a NEW one, and it opens under the
/// current root alone.
#[tokio::test]
async fn an_inherited_version_is_resealed_into_a_manifest_the_current_root_alone_opens() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let p = predecessor();
    let inherited: Vec<u8> = (0..4_000u32).map(|i| (i % 241) as u8).collect();
    let under_predecessor = store_sealed_fixture(
        &store,
        &inherited,
        &predecessor_key().convergent_chunk_root(),
    );
    let watch = tempfile::tempdir().unwrap();
    assert!(
        read_under_the_current_root_alone(&server.uri(), watch.path(), under_predecessor)
            .await
            .is_err(),
        "the fixture: the current root does not open the inherited manifest"
    );
    let engine = successor_engine(&server.uri(), watch.path());

    let resealed = engine
        .reseal_inherited_version("inherited.bin", under_predecessor, Some(p.actor_id().0))
        .await
        .expect("the predecessor's own root opens what it sealed");
    assert_ne!(resealed.manifest_hash, under_predecessor, "a new manifest");
    assert_eq!(resealed.size_bytes, inherited.len() as i64);
    assert_eq!(
        read_under_the_current_root_alone(&server.uri(), watch.path(), resealed.manifest_hash)
            .await
            .expect("re-sealed under the current root"),
        inherited
    );
}

/// The same act on a Media-page upload, which rests as one blob primary under
/// the bare owner key: opened under the predecessor's key, re-sealed as a
/// chunk manifest under the current root.
#[tokio::test]
async fn an_inherited_library_blob_version_is_resealed_as_a_current_root_manifest() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let p = predecessor();
    let inherited: Vec<u8> = (0..3_000u32).map(|i| (i % 239) as u8).collect();
    let blob = store_blob(&store, &inherited, &predecessor_key());
    let watch = tempfile::tempdir().unwrap();
    let engine = successor_engine(&server.uri(), watch.path());

    let resealed = engine
        .reseal_inherited_version("inherited.bin", blob, Some(p.actor_id().0))
        .await
        .expect("the predecessor's key opens its Library blob");
    assert_eq!(resealed.size_bytes, inherited.len() as i64);
    assert_eq!(
        read_under_the_current_root_alone(&server.uri(), watch.path(), resealed.manifest_hash)
            .await
            .expect("re-sealed under the current root"),
        inherited
    );
}

/// **The attack.** A version under a predecessor's signature that names bytes
/// the CURRENT root sealed — a manifest or a Library blob — opens under no
/// root that signature may reach: refused with the reason, and nothing is
/// uploaded. So is a version no retired identity of this account signed.
#[tokio::test]
async fn a_version_sealed_under_the_current_root_is_refused_and_nothing_is_uploaded() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let p = predecessor().actor_id().0;
    let body: Vec<u8> = (0..4_000u32).map(|i| (i % 251) as u8).collect();
    let under_current =
        store_sealed_fixture(&store, &body, &successor_key().convergent_chunk_root());
    let under_predecessor =
        store_sealed_fixture(&store, &body, &predecessor_key().convergent_chunk_root());
    let current_blob = store_blob(&store, &body, &successor_key());
    let watch = tempfile::tempdir().unwrap();
    let engine = successor_engine(&server.uri(), watch.path());
    let before = stored(&store);

    let stranger = ActorKeypair::from_secret([0x53; 32]).actor_id().0;
    for (manifest, signed_as, what) in [
        (under_current, Some(p), "a current-root manifest"),
        (current_blob, Some(p), "a current-root blob"),
        (
            under_predecessor,
            Some(stranger),
            "another writer's version",
        ),
        (under_predecessor, None, "a version with no signer"),
    ] {
        let err = engine
            .reseal_inherited_version("inherited.bin", manifest, signed_as)
            .await
            .expect_err(what);
        assert!(
            format!("{err:#}").contains(fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE),
            "{what}: refused with the reason, got {err:#}"
        );
    }
    assert_eq!(stored(&store), before, "nothing uploaded");
}

/// Not this owner-root move: a set whose bytes rest unsealed is refused
/// before anything is fetched.
#[tokio::test]
async fn a_public_sets_version_is_refused_as_not_an_owner_root_move() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let body: Vec<u8> = (0..4_000u32).map(|i| (i % 251) as u8).collect();
    let under_predecessor =
        store_sealed_fixture(&store, &body, &predecessor_key().convergent_chunk_root());
    let watch = tempfile::tempdir().unwrap();
    let engine = successor_engine(&server.uri(), watch.path()).with_public_audience(true);
    let before = stored(&store);

    let err = engine
        .reseal_inherited_version(
            "inherited.bin",
            under_predecessor,
            Some(predecessor().actor_id().0),
        )
        .await
        .expect_err("a public set's bytes rest unsealed");
    assert!(
        format!("{err:#}").contains(fauna_core::nest_reseal::RESTORE_INHERITED_UNOPENABLE),
        "got {err:#}"
    );
    assert_eq!(stored(&store), before, "nothing uploaded");
}

/// `old` succeeded by `new`, as the landed statement carries it; `new_sig` by
/// `new_signer` (the successor itself for a genuine link).
fn link(
    old: &ActorKeypair,
    new: &ActorKeypair,
    new_signer: &ActorKeypair,
) -> fauna_protocol::ByteBuf {
    let recovery = fauna_core::recovery::RecoveryKey::generate();
    let signed = fauna_core::recovery::IdentitySuccession {
        old_actor_id: old.actor_id(),
        new_actor_id: new.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: fauna_core::data::Timestamp(0),
    }
    .sign(&recovery, new_signer.signing_key(), None)
    .expect("sign the succession");
    fauna_protocol::ByteBuf::from(fauna_core::encoding::canonical_encode(&signed).expect("encode"))
}

const LOOKUP: &str = fauna_protocol::RpcError::SUCCESSION_LOOKUP_KIND;

fn lookup_double(
    statements: Vec<fauna_protocol::ByteBuf>,
) -> fauna_client_testkit::RejectingRequester {
    fauna_client_testkit::RejectingRequester::new().reply(
        LOOKUP,
        &fauna_protocol::recovery::SuccessionLookupReply {
            statements,
            ..Default::default()
        },
    )
}

/// Ruling (8)(b), source (ii), the half for a host handed nothing: an engine
/// whose binding names no predecessor (the in-process FFI hosts, a capability
/// host) proves the link itself — the statement walk ending at its own id,
/// over the succession lookup — and admits the retired identity's row in the
/// same pull. The proven chain is bound as the account's predecessors and, on
/// a set the account owns, as the ordered owner chain ruling (11)(c) reads; a
/// stranger's row in the same batch stays refused, and neither actor is asked
/// about twice.
#[tokio::test]
async fn an_engine_handed_no_predecessors_proves_the_link_by_the_statement_walk() {
    let server = MockServer::start().await;
    let (s, p) = (successor(), predecessor());
    let p0 = ActorKeypair::from_secret([0x53; 32]);
    let stranger = ActorKeypair::from_secret([0x59; 32]);
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor_key()),
        successor(),
    );
    engine.set_reader_binding(binding(&s, &[]));
    // An owner-only set whose roster was read: a row by anyone else is
    // refused as unattributed, no longer held for the read.
    engine.install_reader_roster(Default::default());
    let nest = lookup_double(vec![link(&p0, &p, &p), link(&p, &s, &s)]);
    let rows = || {
        vec![
            moved_row(&p0, &s, 5, None, &our_device_hex()),
            moved_row(&stranger, &s, 6, None, &our_device_hex()),
        ]
    };

    let (kept, held) = engine.verify_served_rows_over(&nest, rows(), &[]).await;
    assert_eq!(held, None);
    assert_eq!(
        kept.len(),
        1,
        "the two-hop chain admits the oldest identity's row"
    );
    assert_eq!(
        kept[0].author_actor_id.as_deref(),
        Some(p0.actor_id().to_hex().as_str()),
        "read as signed by the retired identity"
    );
    let bound = engine.reader_binding();
    let chain = vec![p.actor_id().0, p0.actor_id().0];
    assert_eq!(bound.account_predecessors, chain, "nearest hop first");
    assert_eq!(
        bound.owner_chain, chain,
        "an owned set's owner chain is the same proven walk, in order"
    );
    let lookups = || nest.kinds().iter().filter(|k| **k == LOOKUP).count();
    assert_eq!(lookups(), 2, "one per unplaced signed actor");

    // The next pull asks nothing: the link is remembered, and so is the
    // actor the walk proved nothing for.
    let (kept, _) = engine.verify_served_rows_over(&nest, rows(), &[]).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(lookups(), 2);
}

/// The nest cannot mint the link: a statement naming this identity as the
/// successor that another key signed proves nothing, so the row stays refused
/// and the binding gains nothing.
#[tokio::test]
async fn a_forged_link_binds_no_predecessor_on_the_engine() {
    let server = MockServer::start().await;
    let (s, p) = (successor(), predecessor());
    let forger = ActorKeypair::from_secret([0x59; 32]);
    let watch = tempfile::tempdir().unwrap();
    let engine = test_sync_engine_as(
        &server.uri(),
        watch.path().to_path_buf(),
        Some(successor_key()),
        successor(),
    );
    engine.set_reader_binding(binding(&s, &[]));
    // An owner-only set whose roster was read: a row by anyone else is
    // refused as unattributed, no longer held for the read.
    engine.install_reader_roster(Default::default());
    let nest = lookup_double(vec![link(&p, &s, &forger)]);

    let row = moved_row(&p, &s, 5, None, &our_device_hex());
    let (kept, _) = engine.verify_served_rows_over(&nest, vec![row], &[]).await;
    assert!(kept.is_empty(), "refused as before");
    assert!(engine.reader_binding().account_predecessors.is_empty());
}

// ── The corpus re-seal's Library plane (ruling (8)(c), the bare-key arms) ────

fn later_predecessor() -> ActorKeypair {
    ActorKeypair::from_secret([0x53; 32])
}

fn later_predecessor_key() -> BackupKey {
    BackupKey::from_bytes([0x63; 32])
}

/// A successor's engine whose chain holds two retired identities, nearest hop
/// first: the later predecessor, then [`predecessor`].
fn twice_succeeded_engine(uri: &str, watch: &std::path::Path) -> SyncEngine {
    let (s, later, p) = (successor(), later_predecessor(), predecessor());
    let mut engine =
        test_sync_engine_as(uri, watch.to_path_buf(), Some(successor_key()), successor());
    engine.set_predecessor_backup_keys(PredecessorSealKey::chain([
        (later.actor_id(), later_predecessor_key()),
        (p.actor_id(), predecessor_key()),
    ]));
    engine.set_reader_binding(binding(&s, &[&later, &p]));
    engine
}

/// Admit a row the (earlier) predecessor signed naming `manifest`, so the
/// engine holds its signer.
async fn admit_predecessor_row(engine: &SyncEngine, manifest: ContentHash) {
    let (kept, _) = engine
        .verify_served_rows(
            vec![moved_row(
                &predecessor(),
                &successor(),
                5,
                Some(manifest),
                &"ee".repeat(32),
            )],
            &[],
        )
        .await;
    assert_eq!(kept.len(), 1, "admitted as signed by the predecessor");
}

/// **The attack, on the corpus re-seal's thumbnail move**: the recorded
/// thumbnail of a head a predecessor signed is moved onto the current root
/// only when it opens under that signer's own root (or an earlier one) — one
/// a LATER retired root sealed is not moved, the pointer dropping, while the
/// same head's thumbnail under the signer's own root moves.
#[tokio::test]
async fn a_predecessor_signed_thumbnail_under_a_later_root_is_not_moved() {
    for (thumbnail_key, moves) in [(later_predecessor_key(), false), (predecessor_key(), true)] {
        let server = MockServer::start().await;
        let store = MockNest::new().with_blob_plane().mount(&server).await;
        let watch = tempfile::tempdir().unwrap();
        let engine = twice_succeeded_engine(&server.uri(), watch.path());

        // Non-image bytes, never materialized: the re-seal sources the nest
        // and regenerates no thumbnail, so the recorded one is moved or lost.
        let body: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
        let pixels: Vec<u8> = (0..1_200u32).map(|i| (i % 97) as u8).collect();
        let manifest =
            store_sealed_fixture(&store, &body, &predecessor_key().convergent_chunk_root());
        let thumbnail = store_blob(&store, &pixels, &thumbnail_key);
        seed_synced_entry(&engine, "inherited.bin", &body, manifest);
        engine
            .db()
            .set_thumbnail_hash("inherited.bin", Some(&hex::encode(thumbnail.digest())))
            .unwrap();
        admit_predecessor_row(&engine, manifest).await;

        engine.reseal_predecessor_sealed().await.unwrap();

        assert_eq!(
            store.blob_posts(),
            usize::from(moves),
            "the thumbnail moves only off a root its record's signer may reach (moves: {moves})"
        );
        if moves {
            let moved = store.last_blob_hash.lock().unwrap().clone().unwrap();
            let sealed = store.blobs.lock().unwrap().get(&moved).cloned().unwrap();
            assert_eq!(
                fauna_core::crypto::decrypt_backup_chunk(&successor_key(), &sealed).unwrap(),
                pixels
            );
        }
    }
}

/// The same bound on the re-seal's Library-blob primary: a blob a later
/// retired root sealed, named by a head the earlier predecessor signed, is
/// not re-sealed (the entry stays owed, nothing uploaded); one the signer's
/// own root sealed is.
#[tokio::test]
async fn a_predecessor_signed_library_blob_under_a_later_root_is_not_resealed() {
    for (blob_key, moves) in [(later_predecessor_key(), false), (predecessor_key(), true)] {
        let server = MockServer::start().await;
        let store = MockNest::new().with_blob_plane().mount(&server).await;
        let watch = tempfile::tempdir().unwrap();
        let engine = twice_succeeded_engine(&server.uri(), watch.path());

        let body: Vec<u8> = (0..9_000u32).map(|i| (i % 239) as u8).collect();
        let blob = store_blob(&store, &body, &blob_key);
        seed_synced_entry(&engine, "inherited.bin", &body, blob);
        admit_predecessor_row(&engine, blob).await;

        engine.reseal_predecessor_sealed().await.unwrap();

        assert_eq!(
            store.blob_posts() > 0,
            moves,
            "the blob is re-sealed only off a root its record's signer may reach (moves: {moves})"
        );
        if moves {
            let moved = store.last_blob_hash.lock().unwrap().clone().unwrap();
            let sealed = store.blobs.lock().unwrap().get(&moved).cloned().unwrap();
            assert_eq!(
                fauna_core::crypto::decrypt_backup_chunk(&successor_key(), &sealed).unwrap(),
                body
            );
        }
    }
}

// ── The persisted signer (ruling (11)(d)) ───────────────────────────────────

/// A placeholder folded from a predecessor-signed row, as the fold records it
/// for `inherited.bin`: the row admitted, its path opened, the entry written.
async fn fold_predecessor_placeholder(engine: &SyncEngine, manifest: ContentHash) {
    let (s, p) = (successor(), predecessor());
    let peer = "ee".repeat(32);
    let (mut kept, _) = engine
        .verify_served_rows(vec![moved_row(&p, &s, 5, Some(manifest), &peer)], &[])
        .await;
    assert_eq!(kept.len(), 1, "admitted");
    kept[0].path = Some("inherited.bin".into());
    engine.record_placeholders_from_changes(&kept).unwrap();
}

/// **The copied-record escape, across a restart** (`writer-signed-change-records.md`
/// ruling (11)(d)): a placeholder a predecessor's row planted over a manifest
/// the CURRENT root sealed keeps its signer on the entry, so once the process
/// that admitted the row is gone the manifest still opens under the
/// predecessor's roots alone — and a write that moves the entry's manifest
/// drops the signer recorded for the old one. Seen red with the signer in
/// memory only (after the cache was dropped the manifest read as the current
/// identity's and opened).
#[tokio::test]
async fn a_folded_heads_signer_outlives_the_process_that_admitted_it() {
    let server = MockServer::start().await;
    let store = MockNest::new().with_blob_plane().mount(&server).await;
    let p = predecessor();
    let body: Vec<u8> = (0..4_000u32).map(|i| (i % 251) as u8).collect();
    let under_current =
        store_sealed_fixture(&store, &body, &successor_key().convergent_chunk_root());
    let watch = tempfile::tempdir().unwrap();
    let engine = successor_engine(&server.uri(), watch.path());

    fold_predecessor_placeholder(&engine, under_current).await;
    assert_eq!(
        engine
            .db()
            .get_entry("inherited.bin")
            .unwrap()
            .unwrap()
            .head_signed_as,
        Some(p.actor_id().0),
        "the fold persisted who signed the head"
    );

    engine.forget_admitted_signers_for_test();
    let err = engine
        .download_file_bytes_by_manifest(under_current, None, "inherited.bin")
        .await
        .expect_err(
            "after a restart the predecessor's signature still opens no current-root manifest",
        );
    assert_eq!(
        fauna_core::apply_failure::permanent_reason(&err),
        Some(fauna_core::apply_failure::PermanentApplyFailure::SIGNER_BOUND.reason),
        "{err:#}"
    );

    // A move of the entry's manifest drops the signer it was recorded for.
    let mut other = body.clone();
    other.reverse();
    let moved = store_sealed_fixture(&store, &other, &successor_key().convergent_chunk_root());
    let other_hash = ContentHash::of_raw(&other);
    engine
        .db()
        .upsert_entry(
            "inherited.bin",
            Some(other_hash),
            Some(other_hash),
            Some(moved),
            crate::db::SyncState::Synced,
            1,
            1,
            other.len() as i64,
            1,
            None,
        )
        .unwrap();
    assert_eq!(
        engine
            .db()
            .get_entry("inherited.bin")
            .unwrap()
            .unwrap()
            .head_signed_as,
        None,
        "a signer never describes a head it was not recorded for"
    );
}
