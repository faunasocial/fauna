//! **The served-era adoption** — a set's WebDAV pseudo-device rows are signed
//! in place by the owner before the flag falls, and the flip OFF is refused
//! while one adoptable row is unsigned (`docs/goal/architecture/
//! writer-signed-change-records.md` ruling (7)(b) and its (i)).
//!
//! The chain under test is the production one: the real `fauna.folders.create`
//! / `fauna.folders.update` handlers (the flip and its refusal), the real
//! `fauna.bridges.webdav_record_change` recorder (the pseudo-device row's one
//! writer, and its three wiring-bug refusals), the real
//! `fauna.sync.changes.list` projection the composition's sweep reads, and the
//! new `fauna.folders.served_rows.adopt` kind. The signer side is the shared
//! statement every reader rebuilds (`SignedChange::for_row_as` over the served
//! row, the owner as actor), and the after-flip proof is the readers' own
//! judge (`RowReader` with `webdav_served == false`) — so these tests also pin
//! that an adopted row verifies exactly where every app reads it.
//!
//! Tier: tier_3 (real handlers + real `CacheDb`, nothing stubbed).

mod common;

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::{bridge_blob_handlers, db::CacheDb, folder_handlers, sync_handlers};
use fauna_protocol::folders::{
    FolderCreateReply, FolderCreateRequest, FolderUpdateReply, FolderUpdateRequest,
    ServedRowSignature, ServedRowsAdoptReply, ServedRowsAdoptRequest,
};
use fauna_protocol::sync::{SyncChange, SyncChangesListReply, SyncChangesListRequest};
use fauna_protocol::sync_row_verify::{ReaderBinding, RowReader, RowVerdict};
use fauna_protocol::sync_writer_sig::{ChangeVerifyError, SignedChange, served_row_adoptable};
use fauna_protocol::wrapped_blob::{WebdavRecordChangeReply, WebdavRecordChangeRequest};
use fauna_protocol::{ByteBuf, RpcError, Value, encode_canonical};

const SET: &str = "drive";
const OTHER: &str = "other";
const NONCE: [u8; 32] = [0x5a; 32];
const OTHER_NONCE: [u8; 32] = [0x5b; 32];
const GENERATION: u64 = 4;
const BRIDGE: [u8; 32] = [0x55; 32];

struct Nest {
    router: RpcRouter,
    state: Arc<AppState>,
    owner: ActorKeypair,
}

async fn nest() -> Nest {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    folder_handlers::register_folders_handlers(&mut b);
    sync_handlers::register_sync_handlers(&mut b);
    bridge_blob_handlers::register_bridge_blob_handlers(&mut b);
    let owner = ActorKeypair::generate();
    common::seed_dispatch_actor(&state.db, &owner.actor_id().0).await;
    common::seed_dispatch_actor(&state.db, &BRIDGE).await;
    common::approve_mda(&state, BRIDGE).await;
    let n = Nest {
        router: b.build(),
        state,
        owner,
    };
    for (name, nonce) in [(SET, NONCE), (OTHER, OTHER_NONCE)] {
        let _: FolderCreateReply = n
            .call(
                "fauna.folders.create",
                FolderCreateRequest {
                    name: name.into(),
                    set_nonce: Some(ByteBuf::from(nonce.to_vec())),
                    ..Default::default()
                },
            )
            .await
            .expect("set creates");
        n.serve(name, true).await.expect("serve ON");
    }
    n
}

impl Nest {
    fn owner_id(&self) -> [u8; 32] {
        self.owner.actor_id().0
    }

    async fn call_as<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
        &self,
        actor: [u8; 32],
        kind: &str,
        req: Req,
    ) -> Result<Reply, RpcError> {
        let meta = self.router.kind_meta(kind).expect("kind registered");
        let bytes = Bytes::from(encode_canonical(&req).unwrap().to_vec());
        let reply = (meta.handler)(Arc::clone(&self.state), actor, bytes).await?;
        Ok(fauna_protocol::decode_strict(&reply).expect("reply decodes"))
    }

    async fn call<Req: serde::Serialize, Reply: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        req: Req,
    ) -> Result<Reply, RpcError> {
        self.call_as(self.owner_id(), kind, req).await
    }

    async fn serve(&self, name: &str, on: bool) -> Result<FolderUpdateReply, RpcError> {
        self.call(
            "fauna.folders.update",
            FolderUpdateRequest {
                name: name.into(),
                webdav_enabled: Some(on),
                ..Default::default()
            },
        )
        .await
    }

    /// One DAV write through the real recorder, in the honest MDA's shape
    /// unless the caller bends it.
    async fn dav(
        &self,
        set: &str,
        path: &str,
        bend: impl FnOnce(&mut WebdavRecordChangeRequest),
    ) -> Result<i64, RpcError> {
        let mut req = WebdavRecordChangeRequest {
            actor_id: self.owner_id().to_vec(),
            folder: set.into(),
            path: path.into(),
            manifest_hash: Some(hex::encode(fauna_core::sync::path_hash(path))),
            size_bytes: 10,
            change_type: "create".into(),
            content_key_version: Some(GENERATION),
            if_match: None,
            if_none_match: None,
            path_sealed: Some(ByteBuf::from(label(path, Some(GENERATION)))),
            ..Default::default()
        };
        bend(&mut req);
        let reply: WebdavRecordChangeReply = self
            .call_as(BRIDGE, "fauna.bridges.webdav_record_change", req)
            .await?;
        Ok(reply.seq)
    }

    async fn rows(&self, set: &str) -> Vec<SyncChange> {
        let reply: SyncChangesListReply = self
            .call(
                "fauna.sync.changes.list",
                SyncChangesListRequest {
                    folder: Some(set.into()),
                    since: 0,
                    ..Default::default()
                },
            )
            .await
            .expect("list");
        reply.changes
    }

    fn pseudo_hex(&self) -> String {
        hex::encode(fauna_core::label_custody::webdav_pseudo_device_id(
            &self.owner_id(),
        ))
    }

    /// The composition's signature over one served row: the shared statement
    /// over the row as served, the owner as actor, the set's nonce.
    fn sign(&self, row: &SyncChange, nonce: [u8; 32]) -> ServedRowSignature {
        let statement = SignedChange::for_row_as(row, nonce, self.owner_id()).unwrap();
        ServedRowSignature {
            seq: row.seq,
            signature: ByteBuf::from(statement.sign(self.owner.signing_key()).to_vec()),
            ..Default::default()
        }
    }

    async fn adopt(
        &self,
        set: &str,
        signatures: Vec<ServedRowSignature>,
    ) -> Result<ServedRowsAdoptReply, RpcError> {
        self.call(
            "fauna.folders.served_rows.adopt",
            ServedRowsAdoptRequest {
                name: set.into(),
                signer_key: ByteBuf::from(self.owner_id().to_vec()),
                signatures,
                ..Default::default()
            },
        )
        .await
    }

    /// The reader every app runs once the set is no longer served.
    fn reader(&self, nonce: [u8; 32]) -> RowReader {
        let mut r = RowReader::new();
        r.install_binding(ReaderBinding {
            set_nonce: Some(nonce),
            owner: Some(self.owner_id()),
            webdav_served: false,
            account: Some(self.owner_id()),
            ..Default::default()
        });
        r
    }
}

/// A sealed-label envelope under `generation` — the nest parses the header,
/// never opens it, so the ciphertext is opaque filler.
fn label(path: &str, generation: Option<u64>) -> Vec<u8> {
    fauna_core::path_crypto::SealedLabel {
        v: fauna_core::path_crypto::SEALED_LABEL_V1,
        generation,
        nonce: None,
        ct: serde_bytes::ByteBuf::from(path.as_bytes().to_vec()),
    }
    .to_bytes()
    .unwrap()
}

fn unadopted_count(err: &RpcError) -> i128 {
    let Some(Value::Map(m)) = err.details.as_deref() else {
        panic!("the refusal carries a details map: {err:?}");
    };
    match m.get("unadopted") {
        Some(Value::Integer(n)) => *n,
        other => panic!("details.unadopted is a count: {other:?}"),
    }
}

/// The ruling's main line: DAV writes (a head, its superseded version, a
/// strict delete) refuse the flip with the count; the owner's adoption signs
/// each in place — no new row, no moved seq — and the flip then lands, after
/// which every reader verifies every row as the owner's.
#[tokio::test]
async fn adopted_rows_verify_as_the_owners_once_the_flag_falls() {
    let n = nest().await;
    n.dav(SET, "a.txt", |_| {}).await.unwrap();
    n.dav(SET, "a.txt", |r| {
        r.change_type = "modify".into();
        r.manifest_hash = Some(hex::encode([0x77; 32]));
    })
    .await
    .unwrap();
    n.dav(SET, "b.txt", |_| {}).await.unwrap();
    n.dav(SET, "b.txt", |r| {
        r.change_type = "delete".into();
        r.manifest_hash = None;
        r.content_key_version = None;
        r.size_bytes = 0;
    })
    .await
    .unwrap();

    let err = n
        .serve(SET, false)
        .await
        .expect_err("unadopted rows hold the flag");
    assert_eq!(
        err.code,
        RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTED,
        "{err:?}"
    );
    assert_eq!(unadopted_count(&err), 4);

    let before = n.rows(SET).await;
    assert_eq!(before.len(), 4);
    assert!(
        before
            .iter()
            .all(|r| r.device_id.as_deref() == Some(&n.pseudo_hex()))
    );
    assert!(before.iter().all(served_row_adoptable));

    let reply = n
        .adopt(SET, before.iter().map(|r| n.sign(r, NONCE)).collect())
        .await
        .expect("adopt");
    assert_eq!((reply.adopted, reply.remaining), (4, 0));

    // In place: same rows, same seqs, now carrying the owner's pair.
    let after = n.rows(SET).await;
    assert_eq!(
        after.iter().map(|r| r.seq).collect::<Vec<_>>(),
        before.iter().map(|r| r.seq).collect::<Vec<_>>(),
        "no row minted, no seq moved"
    );
    assert!(
        after
            .iter()
            .all(|r| r.signer_key.as_ref().map(|k| k.to_vec()) == Some(n.owner_id().to_vec()))
    );

    // Idempotent: the same page again signs nothing new.
    let again = n
        .adopt(SET, before.iter().map(|r| n.sign(r, NONCE)).collect())
        .await
        .expect("re-adopt");
    assert_eq!((again.adopted, again.remaining), (0, 0));

    n.serve(SET, false)
        .await
        .expect("the flip lands once adopted");

    let r = n.reader(NONCE);
    for row in &after {
        assert!(
            matches!(
                r.judge(row),
                RowVerdict::Verified { writer, signed_as, .. }
                    if writer == n.owner_id() && signed_as == writer
            ),
            "seq {} verifies as the owner's after the flip: {:?}",
            row.seq,
            r.judge(row)
        );
    }
}

/// A page naming a row the nest must not sign is refused WHOLE — nothing in
/// it is filled: a row of another set, a row that is not the pseudo device's,
/// a signature that does not verify, and a row the predicate rejects.
#[tokio::test]
async fn a_page_with_one_bad_row_is_refused_whole() {
    let n = nest().await;
    n.dav(SET, "good.txt", |_| {}).await.unwrap();
    n.dav(OTHER, "theirs.txt", |_| {}).await.unwrap();
    let good = n.rows(SET).await.remove(0);
    let foreign = n.rows(OTHER).await.remove(0);

    // A row of another set, named by its seq.
    let err = n
        .adopt(
            SET,
            vec![n.sign(&good, NONCE), n.sign(&foreign, OTHER_NONCE)],
        )
        .await
        .expect_err("another set's row");
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // A bad signature (signed under the wrong nonce).
    let err = n
        .adopt(SET, vec![n.sign(&good, OTHER_NONCE)])
        .await
        .expect_err("a signature that does not verify");
    assert_eq!(err.code, "fauna.folders.signature_invalid", "{err:?}");

    // A row the predicate rejects — seeded through the db, since the recorder
    // refuses its shape — refuses the page with its own code.
    let planted = plant_unstamped(&n, SET, "planted.txt").await;
    let planted_row = n
        .rows(SET)
        .await
        .into_iter()
        .find(|r| r.seq == planted)
        .unwrap();
    assert!(!served_row_adoptable(&planted_row));
    let err = n
        .adopt(SET, vec![n.sign(&good, NONCE), n.sign(&planted_row, NONCE)])
        .await
        .expect_err("an unadoptable row");
    assert_eq!(
        err.code,
        RpcError::CODE_FOLDERS_SERVED_ROWS_UNADOPTABLE,
        "{err:?}"
    );

    // A row that is not the pseudo device's.
    let owners = plant_row(&n, SET, "mine.txt", [0x01; 32], Some(GENERATION)).await;
    let owners_row = n
        .rows(SET)
        .await
        .into_iter()
        .find(|r| r.seq == owners)
        .unwrap();
    let err = n
        .adopt(SET, vec![n.sign(&owners_row, NONCE)])
        .await
        .expect_err("a non-pseudo row");
    assert_eq!(err.code, "fauna.folders.invalid_request", "{err:?}");

    // Nothing was filled by any refused page.
    assert!(
        n.rows(SET).await.iter().all(|r| r.signature.is_none()),
        "a refused page writes nothing"
    );
}

/// Ruling (7)(b)(i)(2): the flip counts exactly the ADOPTABLE unsigned rows —
/// a planted row the sweep must not sign never holds the flag — and after the
/// flip that row judges unsigned, so what it names opens nowhere.
#[tokio::test]
async fn an_unadoptable_row_never_holds_the_flag_and_is_refused_after_it() {
    let n = nest().await;
    n.dav(SET, "honest.txt", |_| {}).await.unwrap();
    let planted = plant_unstamped(&n, SET, "planted.txt").await;

    let err = n.serve(SET, false).await.expect_err("one adoptable row");
    assert_eq!(unadopted_count(&err), 1, "the planted row is not counted");

    let rows = n.rows(SET).await;
    let honest: Vec<_> = rows
        .iter()
        .filter(|r| served_row_adoptable(r))
        .map(|r| n.sign(r, NONCE))
        .collect();
    assert_eq!(honest.len(), 1);
    let reply = n.adopt(SET, honest).await.expect("adopt the honest row");
    assert_eq!(reply.remaining, 0);
    n.serve(SET, false)
        .await
        .expect("the flip lands with only an unadoptable row unsigned");

    let planted_row = n
        .rows(SET)
        .await
        .into_iter()
        .find(|r| r.seq == planted)
        .unwrap();
    assert_eq!(
        n.reader(NONCE).judge(&planted_row),
        RowVerdict::Refused(ChangeVerifyError::Unsigned)
    );
}

/// Serve-OFF on a set that was never served — no pseudo-device rows — is
/// unaffected; and a no-op OFF on an already unserved set is never refused.
#[tokio::test]
async fn a_never_served_set_flips_off_as_before() {
    let n = nest().await;
    let _: FolderCreateReply = n
        .call(
            "fauna.folders.create",
            FolderCreateRequest {
                name: "plain".into(),
                set_nonce: Some(ByteBuf::from([0x11; 32].to_vec())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    n.serve("plain", false).await.expect("never-served OFF");
    n.serve(SET, false)
        .await
        .expect("an empty served set flips OFF");
    n.serve(SET, false).await.expect("a repeated OFF");
}

/// Ruling (7)(b)(i)(3): the recorder refuses the three wiring-bug shapes, so
/// the honest DAV population is adoptable by construction.
#[tokio::test]
async fn the_recorder_refuses_the_three_unadoptable_shapes() {
    let n = nest().await;
    let err = n
        .dav(SET, "unstamped.txt", |r| r.content_key_version = None)
        .await
        .expect_err("an unstamped content row");
    assert_eq!(
        err.code,
        RpcError::CODE_BRIDGES_CONTENT_KEY_VERSION_REQUIRED,
        "{err:?}"
    );

    let err = n
        .dav(SET, "x.txt", |r| {
            r.change_type = "delete".into();
            r.content_key_version = None;
        })
        .await
        .expect_err("a delete carrying a manifest");
    assert_eq!(err.code, "fauna.protocol.malformed", "{err:?}");

    let err = n
        .dav(SET, "genless.txt", |r| {
            r.path_sealed = Some(ByteBuf::from(label("genless.txt", None)));
        })
        .await
        .expect_err("a generation-less label");
    assert_eq!(err.code, "fauna.protocol.malformed", "{err:?}");

    assert!(n.rows(SET).await.is_empty(), "no refused record landed");
}

/// Plant a pseudo-device content row with no stamp — what a lying nest could
/// hold, and what the recorder now refuses — straight into the store.
async fn plant_unstamped(n: &Nest, set: &str, path: &str) -> i64 {
    let pseudo = fauna_core::label_custody::webdav_pseudo_device_id(&n.owner_id());
    plant_row(n, set, path, pseudo, None).await
}

async fn plant_row(
    n: &Nest,
    set: &str,
    path: &str,
    device: [u8; 32],
    content_key_version: Option<u64>,
) -> i64 {
    let fs = n
        .state
        .db
        .get_folder_for_actor(set, &n.owner_id())
        .await
        .unwrap()
        .unwrap();
    let sealed = label(path, Some(GENERATION));
    n.state
        .db
        .record_sync_change_metered(
            &n.owner_id(),
            &n.owner_id(),
            None,
            &fauna_core::sync::path_hash(path),
            Some(&[0x33; 32]),
            10,
            "create",
            fs.id,
            &device,
            None,
            content_key_version.map(|v| v as i64),
            None,
            Some(&sealed[..]),
            None,
            None,
            i64::MAX,
        )
        .await
        .unwrap()
}
