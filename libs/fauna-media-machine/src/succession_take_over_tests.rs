//! Pins of the projection walk's take-over (`succession-cut.md` ruling
//! (11)(l), with (11)(d)): the walk re-signs exactly the bound history heads
//! it may open, as the current identity over the same manifest, and nothing
//! else.

use super::*;
use std::collections::HashMap;
use std::sync::Mutex;

use crate::nest_api::MediaApiError;
use anyhow::Context;
use fauna_client_testkit::{ClassifiedError, block_on};
use fauna_core::crypto::BackupKey;
use fauna_core::folder_keys::{
    FolderContentKeys, ResolvedCustody, ResolvedFolderKeys, RetiredSetNonce, SetNonceLineage,
};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::Value;
use fauna_protocol::folders::{FolderSummary, FoldersListReply, KIND_FOLDERS_LIST};
use fauna_protocol::sync::{SyncChangeRecordReply, SyncChangeRecordRequest, SyncChangesListReply};
use fauna_protocol::sync_writer_sig::ChangeSigner;

/// The nonce the successor's re-mint made live.
const LIVE: [u8; 32] = [7; 32];
/// The nonce the cut retired — the predecessor's.
const RETIRED: [u8; 32] = [8; 32];
const SET: &str = "photos";
/// The recording device id: 32 bytes of 0x04, hex (built, so no hex literal sits in the file).
fn device() -> String {
    "04".repeat(32)
}

fn predecessor() -> ActorKeypair {
    ActorKeypair::from_secret([1; 32])
}

fn successor() -> ActorKeypair {
    ActorKeypair::from_secret([2; 32])
}

fn member() -> ActorKeypair {
    ActorKeypair::from_secret([3; 32])
}

/// A nest answering a per-kind table and keeping every request's payload.
#[derive(Default)]
struct TableNest {
    replies: HashMap<&'static str, Value>,
    calls: Mutex<Vec<(&'static str, Vec<u8>)>>,
}

impl TableNest {
    fn reply<T: serde::Serialize>(mut self, kind: &'static str, value: &T) -> Self {
        let bytes = fauna_protocol::encode_canonical(value).expect("encode");
        self.replies
            .insert(kind, fauna_protocol::decode_strict(&bytes).expect("value"));
        self
    }

    fn records(&self) -> Vec<SyncChangeRecordRequest> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == "fauna.sync.changes.record")
            .map(|(_, p)| fauna_protocol::decode_strict(p).expect("record request"))
            .collect()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().iter().map(|(k, _)| *k).collect()
    }
}

impl RpcRequester for TableNest {
    type Error = ClassifiedError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, Self::Error>
    where
        Req: serde::Serialize,
        Reply: serde::de::DeserializeOwned,
    {
        let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
        self.calls.lock().unwrap().push((kind, bytes.to_vec()));
        let Some(v) = self.replies.get(kind) else {
            return Err(ClassifiedError::Transport(format!("no mock for {kind}")));
        };
        let bytes = fauna_protocol::encode_canonical(v).expect("encode reply");
        Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
    }
}

#[derive(Default)]
struct Store {
    manifests: Mutex<HashMap<ContentHash, Vec<u8>>>,
    chunks: Mutex<HashMap<ContentHash, Vec<u8>>>,
}

#[async_trait::async_trait]
impl BlobFetcher for Store {
    async fn fetch_manifest(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.manifests
            .lock()
            .unwrap()
            .get(hash)
            .cloned()
            .context("no such manifest")
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        _relative_path: &str,
    ) -> Result<Vec<Vec<u8>>> {
        let chunks = self.chunks.lock().unwrap();
        store_keys
            .iter()
            .map(|k| chunks.get(k).cloned().context("no such chunk"))
            .collect()
    }
}

#[async_trait::async_trait]
impl MediaBlobUploader for Store {
    async fn post_blob(
        &self,
        _sidecar_cbor: Vec<u8>,
        _sealed_bytes: Vec<u8>,
    ) -> Result<String, MediaApiError> {
        unreachable!("a re-seal stores chunks behind a manifest, never a blob")
    }

    async fn post_chunk(&self, store_key: [u8; 32], body: Vec<u8>) -> Result<(), MediaApiError> {
        self.chunks
            .lock()
            .unwrap()
            .insert(ContentHash::from_digest_raw(store_key), body);
        Ok(())
    }

    async fn post_manifest(
        &self,
        manifest_hash: [u8; 32],
        manifest_bytes: Vec<u8>,
    ) -> Result<(), MediaApiError> {
        self.manifests
            .lock()
            .unwrap()
            .insert(ContentHash::from_digest_raw(manifest_hash), manifest_bytes);
        Ok(())
    }
}

/// The bound set's custody on the successor's device after the cut: its
/// content keys, the live nonce the successor minted, the retired one the
/// predecessor minted.
struct CutCustody(FolderContentKeys);

#[async_trait::async_trait]
impl FolderKeyResolver for CutCustody {
    async fn resolve(&self, _name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
            mls_group_id: Some(vec![0xAB; 16]),
            content_keys: Some(self.0.clone()),
            home_nest_url: None,
            home_nest_actor_id: None,
        }))
    }

    async fn resolve_ref(
        &self,
        _folder_ref: &fauna_core::folder_keys::FolderRef,
    ) -> anyhow::Result<ResolvedCustody> {
        anyhow::bail!("no set with that identity")
    }

    async fn set_nonce(&self, _folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        Ok(Some(LIVE))
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        _name_hash: &[u8; 32],
    ) -> anyhow::Result<SetNonceLineage> {
        Ok(SetNonceLineage {
            live: Some(LIVE),
            live_minted_by: Some(successor().actor_id()),
            retired: vec![RetiredSetNonce {
                nonce: RETIRED,
                minted_by: Some(predecessor().actor_id()),
            }],
            ..Default::default()
        })
    }
}

fn keys() -> FolderContentKeys {
    FolderContentKeys::genesis([42; 32], 1_000)
}

/// Put a content-keyed chunk manifest of `bytes` sealed under `key` at
/// generation `generation` into the store; its hash.
fn content_manifest(store: &Store, key: [u8; 32], generation: u64, bytes: &[u8]) -> String {
    let sealed = fauna_core::blob_seal::seal_blob(
        bytes,
        Some((
            key,
            Some(fauna_core::blob_seal::SealStamp::Version(generation)),
        )),
    )
    .expect("seal");
    let mut chunks = store.chunks.lock().unwrap();
    for (k, body) in sealed.chunks {
        chunks.insert(k, body);
    }
    store
        .manifests
        .lock()
        .unwrap()
        .insert(sealed.manifest_hash, sealed.manifest_bytes);
    hex::encode(sealed.manifest_hash.digest())
}

/// A head of the bound set: stamped at the set's generation, its label sealed
/// under it, signed by `signer` under `nonce`.
fn head(
    seq: i64,
    path: &str,
    manifest_hex: String,
    signer: &ActorKeypair,
    nonce: [u8; 32],
) -> SyncChange {
    let k = keys();
    let generation = k.current_version();
    let label_keys = FileDownloadKeys {
        content_keys: Some(k),
        mls_group_id: Some(vec![0xAB; 16]),
        ..Default::default()
    };
    let mut row = SyncChange {
        seq,
        path_hash: hex::encode(fauna_core::sync::path_hash(path)),
        manifest_hash: Some(manifest_hex),
        size_bytes: 10,
        change_type: "create".into(),
        created_at: 1_000,
        device_id: Some(device()),
        author_actor_id: Some(signer.actor_id().to_hex()),
        content_key_version: Some(generation),
        path_sealed: Some(
            fauna_core::label_custody::seal_path_from_keys(&label_keys, path)
                .expect("seal path")
                .into(),
        ),
        ..Default::default()
    };
    ChangeSigner::direct(signer)
        .sign_row(&mut row, nonce)
        .expect("sign");
    row
}

fn nest_over(rows: Vec<SyncChange>, role: &str) -> Arc<TableNest> {
    Arc::new(
        TableNest::default()
            .reply(
                "fauna.sync.changes.list",
                &SyncChangesListReply {
                    changes: rows,
                    ..Default::default()
                },
            )
            .reply(
                KIND_FOLDERS_LIST,
                &FoldersListReply {
                    folders: vec![FolderSummary {
                        name: SET.into(),
                        role: Some(role.into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            )
            .reply(
                "fauna.sync.changes.record",
                &SyncChangeRecordReply {
                    seq: 50,
                    extra: Default::default(),
                },
            ),
    )
}

fn walk(
    nest: Arc<TableNest>,
    store: Arc<Store>,
    seat: &ActorKeypair,
) -> TakeOverWalk<Arc<TableNest>> {
    TakeOverWalk::new(
        nest,
        seat,
        Arc::new(CutCustody(keys())),
        device(),
        store.clone(),
        store,
        vec![predecessor().actor_id().0],
    )
}

/// An owner-only set's custody on the successor's device after the cut: no
/// group, no content keys, the same nonce lineage.
struct OwnerOnlyCustody;

#[async_trait::async_trait]
impl FolderKeyResolver for OwnerOnlyCustody {
    async fn resolve(&self, _name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        Ok(ResolvedCustody::owner_only())
    }

    async fn resolve_ref(
        &self,
        _folder_ref: &fauna_core::folder_keys::FolderRef,
    ) -> anyhow::Result<ResolvedCustody> {
        anyhow::bail!("no set with that identity")
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        CutCustody(keys()).set_nonce(folder).await
    }

    async fn set_lineage(
        &self,
        folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<SetNonceLineage> {
        CutCustody(keys()).set_lineage(folder, name_hash).await
    }
}

fn backup(kp: &ActorKeypair) -> BackupKey {
    BackupKey::derive(kp.secret_bytes())
}

/// Put an owner-root chunk manifest of `bytes`, sealed under `owner`'s root,
/// into the store; its hash.
fn owner_manifest(store: &Store, owner: &ActorKeypair, bytes: &[u8]) -> String {
    let sealed = fauna_core::blob_seal::seal_blob(
        bytes,
        Some((backup(owner).convergent_chunk_root(), None)),
    )
    .expect("seal");
    let mut chunks = store.chunks.lock().unwrap();
    for (k, body) in sealed.chunks {
        chunks.insert(k, body);
    }
    store
        .manifests
        .lock()
        .unwrap()
        .insert(sealed.manifest_hash, sealed.manifest_bytes);
    hex::encode(sealed.manifest_hash.digest())
}

/// A head of an owner-only set: unstamped, its label sealed under `sealer`'s
/// owner root, signed by `signer` under `nonce`.
fn owner_head(
    seq: i64,
    path: &str,
    manifest_hex: String,
    sealer: &ActorKeypair,
    signer: &ActorKeypair,
    nonce: [u8; 32],
) -> SyncChange {
    let mut row = SyncChange {
        seq,
        path_hash: hex::encode(fauna_core::sync::path_hash(path)),
        manifest_hash: Some(manifest_hex),
        size_bytes: 10,
        change_type: "create".into(),
        created_at: 1_000,
        device_id: Some(device()),
        author_actor_id: Some(signer.actor_id().to_hex()),
        path_sealed: Some(
            fauna_core::label_custody::seal_path(
                &fauna_core::path_crypto::LabelRoot::owner_of(&backup(sealer)),
                path,
            )
            .expect("seal path")
            .into(),
        ),
        ..Default::default()
    };
    ChangeSigner::direct(signer)
        .sign_row(&mut row, nonce)
        .expect("sign");
    row
}

/// The take-over walk over an owner-only set, holding the predecessor's root
/// paired with its identity.
fn owner_walk(
    nest: Arc<TableNest>,
    store: Arc<Store>,
    seat: &ActorKeypair,
) -> TakeOverWalk<Arc<TableNest>> {
    let pred = predecessor();
    TakeOverWalk::new(
        nest,
        seat,
        Arc::new(OwnerOnlyCustody),
        device(),
        store.clone(),
        store,
        vec![pred.actor_id().0],
    )
    .with_predecessor_chain(vec![(pred.actor_id(), backup(&pred))])
}

fn hash_of(path: &str) -> String {
    hex::encode(fauna_core::sync::path_hash(path))
}

/// Ruling (11)(l) with (d): on the cutting device, the walk binds the
/// predecessor's history heads, then re-signs each bound one as the current
/// identity under the live nonce, over the SAME manifest, with a resolution
/// stamp anchored at the head, and no supersede. A head served after the
/// first pass is never adopted; a head already taken is skipped.
#[test]
fn the_walk_re_signs_the_bound_history_heads_as_the_current_identity() {
    let (pred, succ) = (predecessor(), successor());
    let k = keys();
    let generation = k.current_version();
    let store = Arc::new(Store::default());
    let earlier = content_manifest(
        &store,
        *k.current_key(),
        generation,
        b"the owner's earlier file\n",
    );
    let taken = content_manifest(&store, *k.current_key(), generation, b"already taken\n");
    let rows = vec![
        head(1, "earlier.jpg", earlier.clone(), &pred, RETIRED),
        head(2, "taken.jpg", taken.clone(), &pred, RETIRED),
        // Another device of the successor took this path already.
        head(3, "taken.jpg", taken, &succ, LIVE),
    ];
    let first = walk(nest_over(rows.clone(), "owner"), store.clone(), &succ);
    let bound = block_on(first.history_heads(SET)).expect("heads");
    assert_eq!(
        bound,
        vec![(hash_of("earlier.jpg"), 1)],
        "only the path whose head is history"
    );

    // The nest now serves a history head the first pass never saw.
    let late = content_manifest(&store, *k.current_key(), generation, b"served later\n");
    let mut later = rows;
    later.push(head(4, "late.jpg", late, &pred, RETIRED));
    let nest = nest_over(later, "owner");
    let pass = block_on(walk(nest.clone(), store, &succ).take_over(SET, &bound)).expect("pass");
    assert_eq!(
        pass,
        TakeOverPass {
            rerecorded: 1,
            skipped: 0,
            owed: 0,
        }
    );
    let records = nest.records();
    assert_eq!(
        records.len(),
        1,
        "the late head is outside the bound set: {records:?}"
    );
    let rec = &records[0];
    assert_eq!(rec.path, "earlier.jpg");
    assert_eq!(
        rec.manifest_hash.as_deref(),
        Some(earlier.as_str()),
        "the same manifest"
    );
    assert_eq!(rec.content_key_version, Some(generation), "no byte moves");
    assert_eq!(
        rec.is_resolution,
        Some(true),
        "a resolution stamp, never an edit"
    );
    assert_eq!(rec.derived_through, Some(1), "anchored at the head");
    assert_eq!(
        rec.signer_key.as_ref().map(|k| k.to_vec()),
        Some(succ.verifying_key().to_bytes().to_vec()),
        "signed as the current identity"
    );
    assert!(
        !nest.kinds().contains(&"fauna.sync.changes.supersede"),
        "no supersede: the history row stays the version it is"
    );
}

/// Ruling (11)(d), *how*: a head whose bytes open under none of the roots its
/// signer is allowed is never re-signed.
#[test]
fn a_head_that_opens_under_none_of_its_signers_roots_is_not_carried() {
    let (pred, succ) = (predecessor(), successor());
    let generation = keys().current_version();
    let store = Arc::new(Store::default());
    // Sealed under a key this set's custody does not hold.
    let foreign = content_manifest(
        &store,
        [0x99; 32],
        generation,
        b"steered in from elsewhere\n",
    );
    let rows = vec![head(1, "steered.jpg", foreign, &pred, RETIRED)];
    let nest = nest_over(rows, "owner");
    let w = walk(nest.clone(), store, &succ);
    let bound = block_on(w.history_heads(SET)).expect("heads");
    assert_eq!(bound.len(), 1);
    let pass = block_on(w.take_over(SET, &bound)).expect("pass");
    assert_eq!(pass.rerecorded, 0);
    assert!(nest.records().is_empty(), "nothing re-signed unopened");
}

/// Ruling (11)(d), *who*: a member holding the content keys adopts nothing —
/// its reader names no owner here, so no row of the set is history to it.
#[test]
fn a_member_adopts_nothing() {
    let pred = predecessor();
    let k = keys();
    let store = Arc::new(Store::default());
    let earlier = content_manifest(&store, *k.current_key(), k.current_version(), b"owner's\n");
    let rows = vec![head(1, "earlier.jpg", earlier, &pred, RETIRED)];
    let nest = nest_over(rows, "member");
    let w = walk(nest.clone(), store, &member());
    assert!(block_on(w.history_heads(SET)).expect("heads").is_empty());
    let pass = block_on(w.take_over(SET, &[(hash_of("earlier.jpg"), 1)])).expect("pass");
    assert_eq!(pass.rerecorded, 0);
    assert!(nest.records().is_empty());
}

/// A second pass after the first landed records nothing: the path's head is
/// now the successor's live-nonce row, so the bound head is no longer undone.
#[test]
fn a_second_pass_over_a_taken_head_records_nothing() {
    let (pred, succ) = (predecessor(), successor());
    let k = keys();
    let store = Arc::new(Store::default());
    let earlier = content_manifest(&store, *k.current_key(), k.current_version(), b"once\n");
    let rows = vec![
        head(1, "earlier.jpg", earlier.clone(), &pred, RETIRED),
        head(50, "earlier.jpg", earlier, &succ, LIVE),
    ];
    let nest = nest_over(rows, "owner");
    let pass =
        block_on(walk(nest.clone(), store, &succ).take_over(SET, &[(hash_of("earlier.jpg"), 1)]))
            .expect("pass");
    assert_eq!(pass.skipped, 1);
    assert!(nest.records().is_empty());
}

/// Ruling (11)(d), *what*: candidates are bound at the first pass as
/// `(path, head row)`. A bound path whose head moved on to a NEWER history row
/// after that pass is not adopted — the row the walk would re-sign is not the
/// one it bound, so a predecessor-signed row served later never rides in on a
/// bound path's name.
#[test]
fn a_bound_path_whose_head_moved_past_its_bound_row_is_skipped() {
    let (pred, succ) = (predecessor(), successor());
    let k = keys();
    let generation = k.current_version();
    let store = Arc::new(Store::default());
    let earlier = content_manifest(&store, *k.current_key(), generation, b"bound\n");
    let newer = content_manifest(&store, *k.current_key(), generation, b"served later\n");
    let rows = vec![
        head(1, "earlier.jpg", earlier, &pred, RETIRED),
        head(7, "earlier.jpg", newer, &pred, RETIRED),
    ];
    let nest = nest_over(rows, "owner");
    let pass =
        block_on(walk(nest.clone(), store, &succ).take_over(SET, &[(hash_of("earlier.jpg"), 1)]))
            .expect("pass");
    assert_eq!(
        pass,
        TakeOverPass {
            rerecorded: 0,
            skipped: 1,
            owed: 0,
        }
    );
    assert!(
        nest.records().is_empty(),
        "the head served after the bind is not adopted: {:?}",
        nest.records()
    );
}

/// Ruling (11)(l), the owner-root re-seal: a bound head of an owner-only set
/// resting under the predecessor's owner root is opened under that root alone
/// (its signer's, ruling (8)(c)), re-sealed under the CURRENT owner root and
/// recorded as the current identity over the new manifest, with a resolution
/// stamp anchored at the head and no supersede — so nothing is left owed and
/// the marker can be spent. The new copy and its name open under the current
/// root with no retired root offered.
#[test]
fn an_owner_root_head_is_re_sealed_under_the_current_root_and_recorded() {
    let (pred, succ) = (predecessor(), successor());
    let store = Arc::new(Store::default());
    let bytes = b"the owner's earlier file, owner-only\n";
    let original = owner_manifest(&store, &pred, bytes);
    let rows = vec![owner_head(
        1,
        "earlier.jpg",
        original.clone(),
        &pred,
        &pred,
        RETIRED,
    )];
    let nest = nest_over(rows, "owner");
    let w = owner_walk(nest.clone(), store.clone(), &succ);
    let bound = block_on(w.history_heads(SET)).expect("heads");
    assert_eq!(bound, vec![(hash_of("earlier.jpg"), 1)]);
    let pass = block_on(w.take_over(SET, &bound)).expect("pass");
    assert_eq!(
        pass,
        TakeOverPass {
            rerecorded: 1,
            skipped: 0,
            owed: 0,
        },
        "an owner-root head is carried, never left owed"
    );
    let records = nest.records();
    assert_eq!(records.len(), 1, "{records:?}");
    let rec = &records[0];
    assert_eq!(rec.path, "earlier.jpg");
    let resealed = rec.manifest_hash.clone().expect("a manifest");
    assert_ne!(resealed, original, "the bytes moved to the current root");
    assert_eq!(
        rec.content_key_version, None,
        "an owner-root seal carries no stamp"
    );
    assert_eq!(
        rec.is_resolution,
        Some(true),
        "a resolution stamp, never an edit"
    );
    assert_eq!(rec.derived_through, Some(1), "anchored at the head");
    assert_eq!(
        rec.signer_key.as_ref().map(|k| k.to_vec()),
        Some(succ.verifying_key().to_bytes().to_vec()),
        "signed as the current identity"
    );
    assert!(
        !nest.kinds().contains(&"fauna.sync.changes.supersede"),
        "no supersede: the history row stays the version it is"
    );
    // The new copy opens under the current root alone…
    let current_only = FileDownloadKeys::owner(backup(&succ));
    let digest = fauna_core::hex32::decode(&resealed).expect("hex");
    let opened = block_on(fauna_core::file_download::download_file_bytes_by_manifest(
        store.as_ref(),
        &current_only,
        ContentHash::from_digest_raw(digest),
        None,
        "earlier.jpg",
    ))
    .expect("the re-sealed copy opens under the current root");
    assert_eq!(opened, bytes.to_vec());
    // …and so does its name.
    let sealed = rec.path_sealed.clone().expect("the name is sealed");
    match fauna_core::label_custody::open_change_path(
        |generation| current_only.label_open_roots(generation),
        Some(&sealed[..]),
        &hash_of("earlier.jpg"),
    ) {
        fauna_core::label_custody::ChangePathOpen::Opened(p) => assert_eq!(p, "earlier.jpg"),
        other => panic!("the name does not open under the current root: {other:?}"),
    }
}

/// Ruling (8)(c) on the re-seal: a predecessor-signed head naming bytes
/// sealed under the CURRENT root (the planted row the bound exists for) opens
/// under none of its signer's roots, so it is skipped and noted — never
/// re-sealed, never recorded, and never left owed for ever.
#[test]
fn an_owner_root_head_its_signer_could_not_have_sealed_is_skipped() {
    let (pred, succ) = (predecessor(), successor());
    let store = Arc::new(Store::default());
    let planted = owner_manifest(&store, &succ, b"made after the ceremony\n");
    let rows = vec![owner_head(1, "planted.jpg", planted, &pred, &pred, RETIRED)];
    let nest = nest_over(rows, "owner");
    let w = owner_walk(nest.clone(), store, &succ);
    let bound = block_on(w.history_heads(SET)).expect("heads");
    assert_eq!(bound.len(), 1);
    let pass = block_on(w.take_over(SET, &bound)).expect("pass");
    assert_eq!(
        pass,
        TakeOverPass {
            rerecorded: 0,
            skipped: 1,
            owed: 0,
        }
    );
    assert!(nest.records().is_empty(), "nothing re-sealed unopened");
}

/// Ruling (11)(l), *resumable*: a pass whose record does not land leaves the
/// head owed, and the next pass re-seals it to the SAME manifest (the seal is
/// deterministic) and records that.
#[test]
fn an_owner_root_re_seal_cut_short_re_runs_to_the_same_manifest() {
    let (pred, succ) = (predecessor(), successor());
    let store = Arc::new(Store::default());
    let original = owner_manifest(&store, &pred, b"cut short\n");
    let rows = vec![owner_head(
        1,
        "earlier.jpg",
        original,
        &pred,
        &pred,
        RETIRED,
    )];
    let bound = vec![(hash_of("earlier.jpg"), 1)];

    // The nest answers no record: the pass stops after the byte move.
    let mut no_record = Arc::into_inner(nest_over(rows.clone(), "owner")).expect("sole owner");
    no_record.replies.remove("fauna.sync.changes.record");
    let first =
        block_on(owner_walk(Arc::new(no_record), store.clone(), &succ).take_over(SET, &bound))
            .expect("pass");
    assert_eq!(first.owed, 1, "a record that did not land is owed");
    let stored: std::collections::HashSet<ContentHash> =
        store.manifests.lock().unwrap().keys().copied().collect();

    let nest = nest_over(rows, "owner");
    let second = block_on(owner_walk(nest.clone(), store.clone(), &succ).take_over(SET, &bound))
        .expect("pass");
    assert_eq!(second.rerecorded, 1);
    let recorded = nest.records()[0].manifest_hash.clone().expect("manifest");
    let digest = ContentHash::from_digest_raw(fauna_core::hex32::decode(&recorded).expect("hex"));
    assert!(
        stored.contains(&digest),
        "the second pass converged on the manifest the first one stored"
    );
    assert_eq!(
        store.manifests.lock().unwrap().len(),
        stored.len(),
        "no second manifest"
    );
}
