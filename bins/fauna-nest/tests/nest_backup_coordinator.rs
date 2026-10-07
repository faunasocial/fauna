//! **tier_3** — the source nest's in-process segment-backup coordinator
//! (nest-side segment backup, slice 3, source side).
//!
//! Goal docs: `message-segment-store.md` § Cross-location backup protocol (the
//! source nest is the writer; it connects as itself and holds no owner secret),
//! `backup-restore.md` § Background Tasks (the ratified nest-side paragraph —
//! "driven by the SOURCE NEST itself, in-process, continuously — not by any
//! client or agent"), `key-material-hierarchy.md` § Path A-sibling-0 (the seal).
//!
//! **The property under test is that no client is involved.** Two real
//! in-process nests are wired the way an enroll flow leaves them — the owner's
//! client grants its `NestBackupKey` to its source nest, registers the
//! destination there, and registers the source nest as a writer *at the
//! destination* — and then the client is never used again. Everything after that
//! is the source nest acting alone:
//!
//! - it discovers who to back up (`list_nest_backup_key_owners`) and where to
//!   (`list_backup_destinations`), because it cannot read the client-sealed
//!   `fauna.state.backup` plane entries;
//! - it reads its own segment files off disk (no HTTP self-fetch);
//! - it seals every chunk under the granted `NestBackupKey` — asserted by
//!   recomputing the exact ciphertext store keys the owner's client would, so a
//!   seal under the wrong root (e.g. the client `BackupKey`) fails here;
//! - it mints a bulk-byte token and records custody over the **real** federation
//!   channel, authenticated as itself with **no pairing**, gated only on the
//!   grant row the destination wrote;
//! - and `fauna.backup.status` on the source then reports real per-destination
//!   rows off the same state the pass advanced.
//!
//! Nothing here is mocked: two `axum` servers, a real `fauna.federation.hello`
//! handshake through the pool, real `POST /chunks` + `/manifests`, a real
//! `DiskBlobStore`, real custody rows.

mod common;
use common::register_user;

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use fauna_core::crypto::NestBackupKey;
use fauna_core::data::ContentHash;
use fauna_nest::backup::service::BackupService;
use fauna_nest::blob_store::BlobStoreBackend;
use fauna_nest::db::CacheDb;
use fauna_nest::nest_identity::NestIdentity;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::segment_backup::{NestBackupCoordinator, NestBackupWorker};
use fauna_nest::segments::backup_source::NestLocalSegmentSource;
use fauna_nest::{backup_handlers, federation_handlers};
use fauna_protocol::backup::{
    BackupStatusReply, BackupStatusRequest, CustodyMaterializeReply, CustodyMaterializeRequest,
    DestinationRegisterReply, DestinationRegisterRequest, NestKeyGrantReply, NestKeyGrantRequest,
    WriterGrantRegisterReply, WriterGrantRegisterRequest, WriterGrantRevokeReply,
    WriterGrantRevokeRequest,
};
use fauna_protocol::{RpcError, decode_strict as decode, encode_canonical};
use fauna_sync_engine::segment_backup::{SegmentSource, segment_meta_rel_path, segment_rel_path};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// The owner's identity secret: the owner is a real ed25519 key, because the
/// covered-folder re-home is owner-signed (`writer-signed-change-records.md`
/// ruling (7)(a)).
const OWNER_SECRET: [u8; 32] = [0x11; 32];
/// `OWNER_SECRET`'s public key — the owner's actor id
/// ([`the_owner_constant_is_the_owner_keys_public_key`] pins the pair).
const OWNER: [u8; 32] = [
    208, 74, 178, 50, 116, 43, 180, 171, 58, 19, 104, 189, 70, 21, 228, 230, 208, 34, 74, 183, 26,
    1, 107, 175, 133, 32, 163, 50, 201, 119, 135, 55,
];

fn owner_key() -> fauna_core::identity::ActorKeypair {
    fauna_core::identity::ActorKeypair::from_secret(OWNER_SECRET)
}

#[test]
fn the_owner_constant_is_the_owner_keys_public_key() {
    assert_eq!(owner_key().actor_id().0, OWNER);
}
/// The 32 bytes the owner's client granted. Deliberately *not* derived from
/// `OWNER` here — the nest receives raw granted bytes and must never be able to
/// derive them itself.
const GRANTED_KEY: [u8; 32] = [0x5A; 32];
const KIND: &str = "mail";
const DEST_ID: &str = "dest-1";

// ═════════════════════════════════════════════════════════════════════════════
// Harness — two real nests
// ═════════════════════════════════════════════════════════════════════════════

/// A real in-process nest on loopback: own identity, federation router (so a
/// peer can dial it), anonymous discovery (so the pool can resolve its
/// `nest_id`), the client-facing backup kinds, and a real blob store + chunk
/// HTTP routes so the byte plane is not a mock.
///
/// `db_path` points into a fresh tempdir, which is both the coordinator's
/// per-owner state root and the segment store's parent — the same single data
/// dir a deployed nest has.
async fn start_nest() -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let data_dir = tempfile::tempdir().unwrap();
    let data_path = data_dir.path().to_path_buf();
    std::mem::forget(data_dir); // outlives the test; the OS reaps it

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    start_nest_on(data_path, db, secret).await
}

/// [`start_nest`] over a given data directory, database and identity seed —
/// what restarting a nest from a copy of its data directory needs: the same
/// identity (the destination's writer grant names it), the copy's database and
/// the copy's segment areas.
async fn start_nest_on(
    data_path: std::path::PathBuf,
    db: Arc<CacheDb>,
    secret: [u8; 32],
) -> (String, Arc<AppState>, Arc<dyn BlobStoreBackend>) {
    let backup_svc = Arc::new(
        BackupService::new(db.clone(), None, false, data_path.join("blobs"), None).unwrap(),
    );
    let blob_store = backup_svc.local_blob_store();

    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let base = AppState::for_test(db.clone());
    let state = Arc::new(AppState {
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        backup_service: Some(backup_svc),
        config: Arc::new(fauna_nest::config::NestConfig {
            nest: fauna_nest::config::NestSection {
                db_path: data_path.join("nest.db").to_string_lossy().into_owned(),
                ..Default::default()
            },
            ..(*base.config).clone()
        }),
        // The nest's own segment store, under the same data dir.
        mail_segments: Arc::new(fauna_segment_store::SegmentManager::new(
            data_path.clone(),
            KIND,
        )),
        // A test drives many coordinator passes within a second, which a
        // deployment spaces minutes apart; the per-peer federation throttle's
        // production budget (30 records a minute per kind) would refuse them.
        // Nothing in this file exercises that throttle.
        federation_rate_limit: Arc::new(fauna_nest::bridge_rate_limit::Limiter::with_config(
            fauna_nest::bridge_rate_limit::LimiterConfig {
                window: std::time::Duration::from_secs(60),
                max_events: 10_000,
            },
        )),
        // The placement journal under the same data dir, so a copy of the
        // data directory carries the journal with the content.
        mail_placement: Arc::new(fauna_nest::segments::MailPlacementSegmentManager::new(
            data_path.clone(),
        )),
        // The calendar and contacts stores and journals likewise, so the
        // lived-in recovery's DAV proofs can regress a nest by its data dir.
        cal_segments: Arc::new(fauna_segment_store::SegmentManager::new(
            data_path.clone(),
            "calendar",
        )),
        card_segments: Arc::new(fauna_segment_store::SegmentManager::new(
            data_path.clone(),
            "card",
        )),
        cal_placement: Arc::new(fauna_nest::segments::CalPlacementSegmentManager::new(
            data_path.clone(),
        )),
        card_placement: Arc::new(fauna_nest::segments::CardPlacementSegmentManager::new(
            data_path.clone(),
        )),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            backup_handlers::register_backup_handlers(&mut b);
            // The owner-authed custody door (`fauna.sync.changes.record`'s
            // reserved-set arm) — what the tests drive to stage custody on the
            // destination directly.
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            // The calendar and contacts doors a DAV client reaches — how the
            // every-kind tests write the corpora they back up.
            fauna_nest::bridge_caldav_handlers::register_bridge_caldav_handlers(&mut b);
            fauna_nest::bridge_carddav_handlers::register_bridge_carddav_handlers(&mut b);
            // `fauna.segments.list` — the recovery leg's skip reads it.
            fauna_nest::segments::register_segments_handlers(&mut b);
            // `fauna.auth.rotation_chain` — what an owner's device reads
            // before it carries a rotated box's writer seat.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            b.build()
        }),
        ..base
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), state, blob_store)
}

/// Dispatch a USER-class kind as `actor` — i.e. what the owner's own client
/// does over its own authenticated connection.
async fn client_call<Req: Serialize, Rep: DeserializeOwned>(
    state: &Arc<AppState>,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Result<Rep, RpcError> {
    let meta = state.rpc_router.kind_meta(kind).expect("kind registered");
    let payload = bytes::Bytes::from(encode_canonical(req).unwrap().to_vec());
    let out = (meta.handler)(state.clone(), actor, payload).await?;
    Ok(decode(&out).unwrap())
}

/// Append mail records for `actor` to this nest's own segment store — the very
/// bytes the coordinator will later back up.
async fn append_mail(state: &AppState, actor: &[u8; 32], stamps: &[i64]) {
    for (i, ts) in stamps.iter().enumerate() {
        // Unique body per (actor, i) — identity is the content hash, so
        // identical bytes would dedup rather than store N records.
        fauna_nest::segments::mail::append_record(
            &state.mail_segments,
            &state.db,
            actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                format!("sealed-body-{}-{i}", actor[0]).into_bytes(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed-index-hint".to_vec(),
            ),
            common::floor(*ts),
        )
        .await
        .expect("append mail record");
    }
}

/// One mail to file: `(received_at millis, mailbox, flags)`.
type Filed<'a> = (i64, &'a str, &'a str);

/// **File** mail for `actor`: append each record, then place it through the
/// relay's own placement function, which assigns the UID, writes the placement
/// row and journals it (the standard mailboxes first).
///
/// [`append_mail`] leaves its records in no mailbox, which is all the content
/// family's tests need. A test about the placement journal needs the journal
/// production writes, so this one goes through production to get it.
async fn file_mail(state: &Arc<AppState>, actor: &[u8; 32], mail: &[Filed<'_>]) {
    for (ts, mailbox, flags) in mail {
        // Unique per (actor, stamp, mailbox): identity is the content hash, so
        // a repeated body would dedup rather than file a second record.
        let outcome = fauna_nest::segments::mail::append_record(
            &state.mail_segments,
            &state.db,
            actor,
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                format!("sealed-filed-body-{}-{ts}-{mailbox}", actor[0]).into_bytes(),
            ),
            &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
                b"sealed-index-hint".to_vec(),
            ),
            common::floor(*ts),
        )
        .await
        .expect("append mail record");
        assert!(outcome.inserted, "each filed body is its own record");
        fauna_nest::nest_sync_worker::place_relayed_record(
            state,
            actor,
            &outcome.cid.digest(),
            mailbox,
            *ts,
            flags,
            "example.com",
        )
        .await;
    }
}

/// What a nest serves for `actor`'s `mailbox` through the ordinary mailbox read
/// (`bridge_imap_messages ⋈ segment_records`, the query inbox-fetch and IMAP
/// both run), as `(uid, flags, record digest)` in UID order.
///
/// The join is the point: a placement row with no live record behind it is not
/// served, and neither is a live record with no placement row.
async fn serve_mailbox(
    nest: &Arc<AppState>,
    actor: &[u8; 32],
    mailbox: &str,
) -> Vec<(u32, String, Vec<u8>)> {
    let conn = nest.db.conn().await;
    let mut stmt = conn
        .prepare(
            "SELECT m.uid, m.flags, m.message_id \
             FROM bridge_imap_messages m \
             JOIN segment_records sr \
               ON sr.kind = 'mail' AND sr.scope_id = m.actor_id \
              AND substr(sr.record_cid, 5) = m.message_id AND sr.tombstoned = 0 \
             WHERE m.actor_id = ?1 AND m.mailbox = ?2 \
             ORDER BY m.uid ASC",
        )
        .expect("prepare the mailbox read");
    stmt.query_map(rusqlite::params![actor.as_slice(), mailbox], |r| {
        Ok((r.get::<_, i64>(0)? as u32, r.get(1)?, r.get(2)?))
    })
    .expect("run the mailbox read")
    .collect::<Result<Vec<_>, _>>()
    .expect("read the mailbox rows")
}

/// A nest's mailbox tree for `actor`: `(mailbox, uid_validity, uid_next,
/// highestmodseq)`, by name.
async fn mailbox_tree(nest: &Arc<AppState>, actor: &[u8; 32]) -> Vec<(String, i64, i64, i64)> {
    let conn = nest.db.conn().await;
    let mut stmt = conn
        .prepare(
            "SELECT mailbox, uid_validity, uid_next, highestmodseq \
             FROM bridge_imap_mailbox_state WHERE actor_id = ?1 ORDER BY mailbox",
        )
        .expect("prepare the mailbox tree read");
    stmt.query_map(rusqlite::params![actor.as_slice()], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })
    .expect("run the mailbox tree read")
    .collect::<Result<Vec<_>, _>>()
    .expect("read the mailbox tree")
}

/// Every live custody path the destination holds for `OWNER`, sorted.
async fn custody_paths(dest: &Arc<AppState>) -> Vec<String> {
    let mut paths: Vec<String> = dest
        .db
        .list_backup_custody(&OWNER, None, 0)
        .await
        .expect("custody list")
        .into_iter()
        .filter_map(|r| r.path)
        .collect();
    paths.sort();
    paths
}

/// Every live segment id this nest holds for `actor`, read the same way the
/// coordinator's source arm reads them.
async fn live_segment_ids(state: &Arc<AppState>, actor: [u8; 32]) -> Vec<u32> {
    let src = NestLocalSegmentSource::new(Arc::clone(state), "test-source");
    src.list_segments(KIND, &hex::encode(actor))
        .await
        .expect("list segments")
        .segments
        .into_iter()
        .map(|r| r.segment_id)
        .collect()
}

/// The exact blob-store keys `upload_bytes` produces for `bytes` when sealing
/// under `root`: each CDC chunk is framed and sealed through the ONE seal door
/// (`fauna_core::chunk_seal::seal_chunk_body` — the `FILE_SYNC` framing is
/// that door's single decision, so a fixture can no longer silently re-frame
/// to `RESERVED_RAIL` and compute store keys the client never wrote) and keyed
/// by its **ciphertext** hash. Recomputing them here is what turns "some blobs
/// landed" into "the blobs the owner's own client will be able to open, sealed
/// under the key it granted".
fn expected_store_keys(bytes: &[u8], root: &[u8; 32]) -> Vec<ContentHash> {
    let manifest = fauna_core::chunker::chunk_file(bytes);
    fauna_core::chunker::extract_chunks(bytes, &manifest)
        .into_iter()
        .map(|(h, data)| {
            fauna_core::chunk_seal::seal_chunk_body(&h, &data, root)
                .unwrap()
                .0
        })
        .collect()
}

/// Wire the enroll end-state: the owner grants its key + registers the
/// destination on the SOURCE, and registers the source as a writer at the
/// DESTINATION. This is the last moment a client is involved.
async fn enroll(source: &Arc<AppState>, dest_state: &Arc<AppState>, dest_url: &str) {
    enroll_source(source, dest_state, dest_url).await;
    let wg = grant_writer(source, dest_state)
        .await
        .expect("owner authorizes its source nest to write custody at the destination");
    assert!(wg.ok);
}

/// [`enroll`]'s source half: the seal grant and the destination registration,
/// both on the SOURCE. Without [`grant_writer`] the box knows where to back up
/// and holds no authority to write there.
async fn enroll_source(source: &Arc<AppState>, dest_state: &Arc<AppState>, dest_url: &str) {
    let grant: NestKeyGrantReply = client_call(
        source,
        OWNER,
        "fauna.backup.nest_key.grant",
        &NestKeyGrantRequest {
            nest_backup_key: serde_bytes::ByteBuf::from(GRANTED_KEY.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("owner grants its NestBackupKey to its source nest");
    assert!(grant.ok);

    let reg: DestinationRegisterReply = client_call(
        source,
        OWNER,
        "fauna.backup.destination.register",
        &DestinationRegisterRequest {
            destination_id: DEST_ID.to_string(),
            destination_nest_url: dest_url.to_string(),
            destination_nest_id: hex::encode(dest_state.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
    .expect("owner tells its source nest where to back up");
    assert!(reg.ok);
}

/// [`enroll`]'s destination half: the owner registers `source` as its writer at
/// the DESTINATION, over its own connection there.
async fn grant_writer(
    source: &Arc<AppState>,
    dest_state: &Arc<AppState>,
) -> Result<WriterGrantRegisterReply, RpcError> {
    client_call(
        dest_state,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
}

async fn status(state: &Arc<AppState>, actor: [u8; 32]) -> BackupStatusReply {
    client_call(
        state,
        actor,
        "fauna.backup.status",
        &BackupStatusRequest {
            extra: Default::default(),
        },
    )
    .await
    .expect("status")
}

/// A [`BlobFetcher`](fauna_core::file_download::BlobFetcher) over a
/// destination's **open** content-addressed routes (`/api/v1/manifests/{hash}`
/// and `/api/v1/chunks/{hash}` carry no bearer — confidentiality is the seal's).
/// The test's stand-in for the shipping `NestPublicChunkFetcher`, so what is
/// reassembled below comes off the destination's own byte plane, not out of
/// its blob store by the back door.
struct OpenRouteFetcher {
    base: String,
    http: reqwest::Client,
}

#[async_trait::async_trait]
impl fauna_core::file_download::BlobFetcher for OpenRouteFetcher {
    async fn fetch_manifest(&self, hash: &ContentHash) -> anyhow::Result<Vec<u8>> {
        let url = format!(
            "{}/api/v1/manifests/{}",
            self.base,
            hex::encode(hash.digest())
        );
        let resp = self.http.get(&url).send().await?;
        anyhow::ensure!(
            resp.status() == reqwest::StatusCode::OK,
            "GET {url} ({})",
            resp.status()
        );
        Ok(resp.bytes().await?.to_vec())
    }

    async fn fetch_chunks(
        &self,
        store_keys: &[ContentHash],
        relative_path: &str,
    ) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(store_keys.len());
        for key in store_keys {
            let url = format!("{}/api/v1/chunks/{}", self.base, hex::encode(key.digest()));
            let resp = self.http.get(&url).send().await?;
            anyhow::ensure!(
                resp.status() == reqwest::StatusCode::OK,
                "GET {url} ({}) for {relative_path}",
                resp.status()
            );
            out.push(resp.bytes().await?.to_vec());
        }
        Ok(out)
    }
}

/// Open one custody path the destination holds for `OWNER` under the granted
/// key, off the destination's open byte routes — what a materialize verb (or
/// the owner's own client) does to get a file back out of a backup.
async fn open_custody_path(dest: &Arc<AppState>, dest_url: &str, path: &str) -> Vec<u8> {
    let rows = dest
        .db
        .list_backup_custody(&OWNER, None, 0)
        .await
        .expect("custody list");
    let row = rows
        .iter()
        .find(|r| r.path.as_deref() == Some(path))
        .unwrap_or_else(|| {
            panic!(
                "no live custody row at {path}; held: {:?}",
                rows.iter().map(|r| r.path.clone()).collect::<Vec<_>>()
            )
        });
    // `list_backup_custody` serves live rows only, so the manifest is never
    // NULL here — a tombstone is the absence of custody and is not listed.
    let manifest_hash: [u8; 32] = row
        .manifest_hash
        .as_slice()
        .try_into()
        .expect("32-byte manifest hash");
    let fetcher = OpenRouteFetcher {
        base: dest_url.to_string(),
        http: reqwest::Client::new(),
    };
    let keys = fauna_core::file_download::FileDownloadKeys::owner(
        fauna_core::crypto::OwnerSealKey::SourceNest(NestBackupKey::from_bytes(GRANTED_KEY)),
    );
    fauna_core::file_download::download_file_bytes_by_manifest(
        &fetcher,
        &keys,
        ContentHash::from_digest_raw(manifest_hash),
        None,
        path,
    )
    .await
    .unwrap_or_else(|e| panic!("open {path} from the destination's custody: {e:#}"))
}

// ═════════════════════════════════════════════════════════════════════════════
// The capstone
// ═════════════════════════════════════════════════════════════════════════════

/// **A source nest backs an owner's mail up with no client and no agent alive.**
///
/// The whole point of the 2026-07-23 redesign: backup freshness stops depending
/// on any user device being awake. After enroll, this test only ever drives the
/// nest's own hosting loop.
#[tokio::test]
async fn a_source_nest_backs_up_its_own_segments_with_no_client_alive() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000, 1_715_000_100_000]).await;
    enroll(&source, &dest, &d_url).await;

    // Before the sweep: enrolled, one destination, nothing uploaded, a real
    // backlog. A freshly enrolled destination reporting its full backlog is the
    // honest reading, and it pins that the projection is computed rather than
    // hard-coded.
    let before = status(&source, OWNER).await;
    assert!(before.enrolled);
    assert_eq!(before.destinations.len(), 1);
    assert_eq!(before.destinations[0].destination_id, DEST_ID);
    assert_eq!(before.destinations[0].last_upload_time, None);
    assert!(
        before.destinations[0].backlog_count > 0,
        "an enrolled destination with nothing uploaded has a backlog"
    );

    // ── The only actor from here on is the nest's own hosting loop. ──
    let ran = NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");
    assert_eq!(ran, 1, "exactly one enrolled owner was swept");

    // The destination now holds this owner's custody, in an owner-owned
    // a custody-copy set the source never provisioned — it was created lazily
    // behind the grant gate.
    let fs = dest
        .db
        .get_folder_for_actor("__mail", &OWNER)
        .await
        .unwrap()
        .expect("the destination created the owner's reserved backup set");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
    assert_eq!(fs.actor_id, OWNER.to_vec(), "custody is owner-owned");
    assert!(
        !dest
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "custody rows reference the uploaded manifests"
    );

    // …and the bytes are really there, sealed under the GRANTED key. Recomputing
    // the store keys is the strong form: a seal under any other root (or no
    // seal) yields different keys and this fails.
    let root = NestBackupKey::from_bytes(GRANTED_KEY).convergent_chunk_root();
    let seg_ids = live_segment_ids(&source, OWNER).await;
    assert!(
        !seg_ids.is_empty(),
        "the source really has segments to back up"
    );
    for seg_id in seg_ids {
        let seg_bytes = std::fs::read(source.mail_segments.segment_file_path(&OWNER, seg_id))
            .expect("the source's own segment file");
        let keys = expected_store_keys(&seg_bytes, &root);
        assert!(!keys.is_empty());
        for key in &keys {
            assert!(
                dest_blobs.exists(key).await.unwrap(),
                "chunk {} of segment {seg_id} is missing at the destination — \
                 the seal root must be the granted NestBackupKey",
                hex::encode(key.digest())
            );
        }
    }

    // The status projection now reads off the state the pass advanced.
    let after = status(&source, OWNER).await;
    assert_eq!(after.destinations.len(), 1);
    assert!(
        after.destinations[0].last_upload_time.is_some(),
        "the manifest mirror was uploaded, so a last-upload time exists"
    );
    assert_eq!(
        after.destinations[0].backlog_count, 0,
        "nothing is queued once the pass has uploaded every live segment"
    );

    // A second sweep is a no-op: the diff finds nothing new, so no bytes and no
    // custody records are re-sent (idempotent, which is what makes a 15-minute
    // cadence cheap).
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let report = coordinator
        .run_once(&dest_row, KIND)
        .await
        .expect("second pass");
    assert!(
        report.uploaded_segments.is_empty() && !report.manifest_uploaded,
        "an unchanged source re-uploads nothing: {report:?}"
    );
}

/// **What the destination holds for a segment is a SEGMENT — both files —
/// and it reopens.** (The 2026-08-29 sidecar widening, `message-segment-store.md`
/// § Client-device custodian (pull) → *Restore*.)
///
/// Until this landed the corpus carried `seg-NNNNNNNN.dat` alone, and
/// `FramedSegment::open` — the only production reader — refused every one of
/// them with `missing sidecar`: the record footers (`record_order` + the
/// per-record floor metadata) live nowhere but the `.meta`. So the assertion
/// here is not "two custody rows exist" but the definition of success the
/// widening was captured with: the pair is opened from the destination's own
/// byte plane under the granted key, written into a fresh segment area, and
/// `FramedSegment::open` reads a record back.
#[tokio::test]
async fn the_pair_a_destination_holds_reopens_as_a_segment_with_its_records() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000, 1_715_000_100_000]).await;
    enroll(&source, &dest, &d_url).await;

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");

    let scope_hex = hex::encode(OWNER);
    let seg_ids = live_segment_ids(&source, OWNER).await;
    assert!(!seg_ids.is_empty());
    for seg_id in seg_ids {
        // Both halves are held, and each is byte-identical to the source's.
        let dat = open_custody_path(&dest, &d_url, &segment_rel_path(&scope_hex, seg_id)).await;
        let meta =
            open_custody_path(&dest, &d_url, &segment_meta_rel_path(&scope_hex, seg_id)).await;
        assert_eq!(
            dat,
            std::fs::read(source.mail_segments.segment_file_path(&OWNER, seg_id)).unwrap()
        );
        assert_eq!(
            meta,
            std::fs::read(source.mail_segments.segment_meta_path(&OWNER, seg_id)).unwrap()
        );

        // Reconstitute into a fresh segment area and reopen — the mechanism a
        // materialize verb runs, minus the verb.
        let area = tempfile::tempdir().unwrap();
        let dat_path = area.path().join(format!("seg-{seg_id:08}.dat"));
        std::fs::write(&dat_path, &dat).unwrap();
        std::fs::write(area.path().join(format!("seg-{seg_id:08}.meta")), &meta).unwrap();
        let reopened = fauna_segment_store::FramedSegment::open(&dat_path)
            .expect("a backed-up pair must reopen as a segment");
        assert_eq!(reopened.header.actor_id, OWNER);
        assert_eq!(reopened.header.segment_id, seg_id);
        let entries: Vec<_> = reopened.iter_records().cloned().collect();
        assert!(
            !entries.is_empty(),
            "the footers came back with the sidecar"
        );
        // …and the source's own reader agrees record for record.
        let on_source = fauna_segment_store::FramedSegment::open(
            &source.mail_segments.segment_file_path(&OWNER, seg_id),
        )
        .unwrap();
        for entry in &entries {
            assert_eq!(
                reopened.read_record(&entry.cid).unwrap(),
                on_source.read_record(&entry.cid).unwrap(),
                "record {} reads back from the reconstituted segment",
                entry.cid
            );
        }
    }

    // A second pass owes nothing: the pair is whole on the destination.
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let report = coordinator.run_once(&dest_row, KIND).await.unwrap();
    assert!(report.uploaded_segments.is_empty(), "{report:?}");
}

/// **A zero-content owner's status reads "never synced", not a fresh
/// timestamp for a manifest sent on their behalf.**
///
/// An enrolled owner with no mail at all still gets a sweep: the pass writes
/// an empty manifest mirror (`manifest_changed` fires — no prior hash exists
/// yet — even though nothing was uploaded or dropped, since there is nothing
/// to upload or drop). That mirror write is real, honest bookkeeping — but
/// it must not read to the OWNER as "your content was just backed up",
/// because none of it was. `docs/goal/behavior/backup-destinations.md` § Per-destination
/// status read, superseding the rejected option (c): the manifest
/// mirror stays the destination's honest bookkeeping record either way;
/// only the UI-facing timestamp is redefined to track real content movement.
#[tokio::test]
async fn a_zero_content_owner_reads_never_synced_after_a_sweep() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    // Deliberately no `append_mail` — this owner has zero segments.
    enroll(&source, &dest, &d_url).await;

    let before = status(&source, OWNER).await;
    assert_eq!(before.destinations[0].last_upload_time, None);
    assert_eq!(
        before.destinations[0].backlog_count, 0,
        "zero content ⇒ zero backlog even before the first sweep"
    );

    let ran = NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");
    assert_eq!(
        ran, 1,
        "the enrolled owner was swept despite having nothing to back up"
    );

    // The manifest mirror WAS written (real bookkeeping — the destination now
    // holds an honest empty-manifest record) …
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let report = coordinator
        .run_once(&dest_row, KIND)
        .await
        .expect("a second, directly-observed pass");
    assert!(
        report.uploaded_segments.is_empty() && report.dropped_segments.is_empty(),
        "zero content ⇒ nothing to upload or drop: {report:?}"
    );

    // … but the OWNER-FACING status must still read "never synced": nothing
    // of theirs has ever moved, so a fresh timestamp here would be exactly
    // the false reassurance this test exists to catch.
    let after = status(&source, OWNER).await;
    assert_eq!(
        after.destinations[0].last_upload_time, None,
        "a manifest-bookkeeping-only pass must not read as a content sync"
    );
    assert_eq!(after.destinations[0].backlog_count, 0);
}

/// **The grant is the authorization, and the owner can revoke it with the source
/// nest fully hostile.** Revoking at the destination refuses the source's next
/// mint, so the pass fails and — critically — advances no local state, leaving
/// the segment queued rather than silently marked backed-up.
#[tokio::test]
async fn revoking_the_writer_grant_at_the_destination_stops_the_source_nest() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000]).await;
    enroll(&source, &dest, &d_url).await;

    // The owner revokes at the DESTINATION, over its own connection there —
    // never through the source nest, which is the point of the split plane.
    let rev: WriterGrantRevokeReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.writer_grant.revoke",
        &WriterGrantRevokeRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            extra: Default::default(),
        },
    )
    .await
    .expect("revoke");
    assert!(rev.revoked);

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let err = coordinator
        .run_once(&dest_row, KIND)
        .await
        .expect_err("a revoked source nest cannot write");

    let seg_id = live_segment_ids(&source, OWNER).await[0];
    let seg_bytes = std::fs::read(source.mail_segments.segment_file_path(&OWNER, seg_id))
        .expect("segment file");
    let root = NestBackupKey::from_bytes(GRANTED_KEY).convergent_chunk_root();
    for key in expected_store_keys(&seg_bytes, &root) {
        assert!(
            !dest_blobs.exists(&key).await.unwrap(),
            "no bytes may land at a destination that revoked the writer grant (error was: {err:#})"
        );
    }
    assert!(
        dest.db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "no custody may be recorded either"
    );

    // The pass advanced no state, so the segment is still queued — a failed
    // backup must never read as a completed one.
    let after = status(&source, OWNER).await;
    assert_eq!(after.destinations[0].last_upload_time, None);
    assert!(after.destinations[0].backlog_count > 0);
}

/// **A destination removed from the registry has its local upload state
/// forgotten**, so re-registering the same `destination_id` re-uploads from
/// scratch rather than trusting rows that describe custody the owner has since
/// deleted at the destination.
#[tokio::test]
async fn removing_a_destination_forgets_its_local_upload_state() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000]).await;
    enroll(&source, &dest, &d_url).await;

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("first sweep");
    assert_eq!(
        status(&source, OWNER).await.destinations[0].backlog_count,
        0
    );

    // The owner removes the destination from the source-side registry.
    let removed: fauna_protocol::backup::DestinationRemoveReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.remove",
        &fauna_protocol::backup::DestinationRemoveRequest {
            destination_id: DEST_ID.to_string(),
            extra: Default::default(),
        },
    )
    .await
    .expect("remove");
    assert!(removed.removed);

    let forgotten = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("still enrolled")
        .reconcile_removed_destinations()
        .await
        .expect("reconcile");
    assert_eq!(forgotten, 1, "the departed destination's rows were dropped");

    // Re-registering the same id starts from a full backlog again.
    let reg: DestinationRegisterReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.register",
        &DestinationRegisterRequest {
            destination_id: DEST_ID.to_string(),
            destination_nest_url: d_url.clone(),
            destination_nest_id: hex::encode(dest.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
    .expect("re-register");
    assert!(reg.ok);

    let after = status(&source, OWNER).await;
    assert_eq!(after.destinations[0].last_upload_time, None);
    assert!(
        after.destinations[0].backlog_count > 0,
        "a re-registered destination re-uploads rather than trusting stale state"
    );
}

/// **Removing a destination tears the owner's custody down AT the destination**,
/// so the offsite chunks stop being GC-pinned there.
///
/// The complement of the test above: that one proves the *source*-local rows go,
/// this one proves the *destination* learns. Before the slice-5 flip the client
/// coordinator drove `fauna.folders.delete` here; the flip retired that arm and
/// left nothing in its place, so a removed destination kept every uploaded chunk
/// alive forever with no path left to reclaim it.
///
/// The teardown is a per-path `delete` over the same nest-writer grant the
/// uploads rode (`message-segment-store.md` § Cross-location backup protocol —
/// "or records a `delete` for each backed-up path"), never an owner-authenticated
/// set delete: the source nest holds no owner secret, and the custody **grace
/// window** deliberately bounds what a writer can destroy — so the chunks reclaim
/// one grace window later, not instantly. What this asserts is the property that
/// was missing: no LIVE custody remains, so nothing is pinned indefinitely.
#[tokio::test]
async fn removing_a_destination_drops_its_custody_at_the_destination() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    // FILED mail, so the corpus has both families: the teardown reads the
    // journal's upload state back by its serve tag, and a tag it mis-read would
    // leave the journal's custody live at the departed destination.
    file_mail(
        &source,
        &OWNER,
        &[
            (1_715_000_000_000, "INBOX", ""),
            (1_715_000_001_000, "INBOX", ""),
        ],
    )
    .await;
    enroll(&source, &dest, &d_url).await;

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("first sweep");
    assert!(
        !dest
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "precondition: the sweep recorded live custody at the destination"
    );
    assert!(
        custody_paths(&dest)
            .await
            .iter()
            .any(|p| p.contains("/placement/")),
        "precondition: and the journal's custody is part of it"
    );

    // The owner removes the destination from the source-side registry. This is
    // the ONLY gesture — no synchronous destination call rides the remove
    // (`backup-destinations.md` § Create / edit / remove protocol → Remove).
    let removed: fauna_protocol::backup::DestinationRemoveReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.remove",
        &fauna_protocol::backup::DestinationRemoveRequest {
            destination_id: DEST_ID.to_string(),
            extra: Default::default(),
        },
    )
    .await
    .expect("remove");
    assert!(removed.removed);

    let forgotten = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("still enrolled")
        .reconcile_removed_destinations()
        .await
        .expect("reconcile");
    assert_eq!(forgotten, 1);

    assert!(
        dest.db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "every path the source nest backed up must be tombstoned at the departed \
         destination — live custody left behind pins its chunks against GC forever"
    );
}

/// **A teardown that cannot be delivered KEEPS its rows** — forgetting them is
/// what would strand the destination's custody permanently.
///
/// This is the loop-reconcile half of the crash-safety contract
/// (`nest/common.md` § Client-state recoverability): the local rows are the only
/// record that a departed destination is still owed a teardown, so the moment a
/// failed attempt drops them, nothing will ever tell that destination again —
/// the exact indefinite pin the teardown exists to close, reintroduced through
/// the error path instead of the happy one.
///
/// The unreachable case is staged with a revoked writer grant rather than a dead
/// socket: it is the same refusal a real revoke-then-remove produces, and it
/// fails at the gate instead of a timeout.
#[tokio::test]
async fn a_teardown_that_fails_keeps_its_rows_for_the_next_pass() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000]).await;
    enroll(&source, &dest, &d_url).await;

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("first sweep");

    // The owner revokes the writer grant at the destination, then removes the
    // destination — so the teardown has authority for neither.
    let rev: WriterGrantRevokeReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.writer_grant.revoke",
        &WriterGrantRevokeRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            extra: Default::default(),
        },
    )
    .await
    .expect("revoke");
    assert!(rev.revoked);

    let removed: fauna_protocol::backup::DestinationRemoveReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.remove",
        &fauna_protocol::backup::DestinationRemoveRequest {
            destination_id: DEST_ID.to_string(),
            extra: Default::default(),
        },
    )
    .await
    .expect("remove");
    assert!(removed.removed);

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("still enrolled");

    // The pass itself succeeds — one unreachable destination must not abort the
    // reconcile for the others — but it forgets nothing, and the destination's
    // custody is untouched.
    let forgotten = coordinator
        .reconcile_removed_destinations()
        .await
        .expect("a deferred teardown is not a pass failure");
    assert_eq!(
        forgotten, 0,
        "a destination whose teardown was refused must NOT be forgotten"
    );
    assert!(
        !dest
            .db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "nothing could be delivered, so the custody must still be there"
    );

    // Now the obstacle clears — the owner re-authorizes the writer. A LATER pass
    // must still be able to finish the job, which it can only do if the previous
    // one kept the rows. This is the assertion that makes the deferral real:
    // survival of the rows is observable exactly as the retry succeeding.
    let wg: WriterGrantRegisterReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
    .expect("owner re-authorizes the writer");
    assert!(wg.ok);

    let forgotten = coordinator
        .reconcile_removed_destinations()
        .await
        .expect("the retry");
    assert_eq!(forgotten, 1, "the retried teardown completes");
    assert!(
        dest.db
            .backup_custody_manifest_hashes()
            .await
            .unwrap()
            .is_empty(),
        "and it tears the custody down — a deferred teardown is delayed, not lost"
    );
}

/// **An owner who granted no key is not swept**, and a nest where nobody has
/// enrolled does nothing at all — the out-of-the-box posture.
#[tokio::test]
async fn a_nest_with_no_enrolled_owner_sweeps_nothing() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000]).await;

    let ran = NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("sweep");
    assert_eq!(ran, 0);

    assert!(
        NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
            .await
            .unwrap()
            .is_none(),
        "no grant ⇒ no coordinator, which is what keeps status honest"
    );
    let s = status(&source, OWNER).await;
    assert!(!s.enrolled);
    assert!(s.destinations.is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// Ordinary-folder destination coverage — the mirror axis
// (`backup-destinations.md` § Ordinary-folder coverage — destination places)
// ═════════════════════════════════════════════════════════════════════════════

/// Seed one already-sealed "file" into the source's folder corpus: chunk bodies
/// in the blob store (one framed the way the chunk routes store, one verbatim
/// with a colliding frame prefix — pinning BOTH layers of the mirror's
/// integrity rule), a `ChunkManifest` keying them via `stored_hashes` (the
/// sealed shape), and the head row in `sync_changes` with a sealed path label.
/// Returns `(path_hash, manifest_hash, store_keys)`.
async fn seed_folder_file(
    state: &Arc<AppState>,
    folder_id: i64,
    rel_path: &str,
    body_tag: u8,
) -> ([u8; 32], ContentHash, Vec<ContentHash>) {
    let svc = state
        .backup_service
        .as_ref()
        .expect("blob store configured");
    let store = svc.local_blob_store();

    // "Sealed" chunk bodies — opaque to the mirror by design; any bytes stand
    // in for ciphertext. c2 deliberately begins with the store framing's 0x00
    // prefix so a verbatim (raw-layer) blob exercises the fallback branch.
    let c1 = vec![body_tag, 0x10, 0x20, 0x30];
    let c2 = vec![0x00, body_tag, 0x21, 0x31];
    let k1 = ContentHash::of_raw(&c1);
    let k2 = ContentHash::of_raw(&c2);
    let framed = fauna_nest::backup::encode_blob(&c1, None, false).unwrap();
    store.put(&k1, &framed).await.unwrap();
    store.put(&k2, &c2).await.unwrap();

    let manifest = fauna_core::chunk::ChunkManifest {
        file_hash: k1, // stands in for the plaintext identity; never opened here
        total_size: (c1.len() + c2.len()) as u64,
        chunk_hashes: vec![k1, k2],
        chunk_sizes: vec![c1.len() as u64, c2.len() as u64],
        stored_hashes: Some(vec![k1, k2]),
        sealed_hashes: None,
        min_reader: None,
    };
    let manifest_bytes = fauna_core::encoding::canonical_encode(&manifest).unwrap();
    let manifest_hash = ContentHash::of_raw(&manifest_bytes);
    let manifest_framed = fauna_nest::backup::encode_blob(&manifest_bytes, None, false).unwrap();
    store.put(&manifest_hash, &manifest_framed).await.unwrap();

    let path_hash = fauna_core::sync::path_hash(rel_path);
    state
        .db
        .record_sync_change_metered(
            &OWNER,
            &OWNER,
            None,
            &path_hash,
            Some(&manifest_hash.digest()),
            manifest.total_size as i64,
            "create",
            folder_id,
            &[0x99; 32],
            None, // private folder: no resting plaintext path
            None,
            None,
            Some(b"sealed-name-label"),
            None,
            None,
            i64::MAX,
        )
        .await
        .expect("record the folder head row");

    (path_hash, manifest_hash, vec![k1, k2])
}

/// **A source nest mirrors a covered ordinary folder as-is, and detach tears
/// it down.** After attach, the only actor is the nest's own sweep: the
/// folder's live head lands at the destination byte-identically (manifest +
/// both chunks, never re-sealed), custody rows land in
/// `__folder/<source-hex>/<folder-id>`, a supersede retains the prior
/// generation under T, and a detach tombstones every mirrored path.
#[tokio::test]
async fn a_source_nest_mirrors_a_covered_folder_and_detach_tears_it_down() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");
    let (path_hash, manifest_hash, store_keys) =
        seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x41).await;

    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner attaches the folder");
    assert!(attach.attached);
    let folder_set = attach.folder_set.clone();
    assert_eq!(
        folder_set,
        format!(
            "__folder/{}/{folder_id}",
            hex::encode(source.nest_identity.public_key_bytes())
        )
    );

    // ── From here on, only the nest's own hosting loop acts. ──
    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");

    // The destination created the mirror set lazily, owner-owned, backup-type.
    let fs = dest
        .db
        .get_folder_for_actor(&folder_set, &OWNER)
        .await
        .unwrap()
        .expect("the destination created the folder-mirror set");
    assert!(
        fs.custody_copy,
        "the provisioner marks the row a custody copy"
    );
    assert_eq!(fs.actor_id, OWNER.to_vec());

    // The bytes are there AS-IS: the exact source store keys, plus the
    // manifest under its own hash — a re-seal would key differently and fail.
    for key in &store_keys {
        assert!(
            dest_blobs.exists(key).await.unwrap(),
            "chunk {} must land byte-identical at the destination",
            hex::encode(key.digest())
        );
    }
    assert!(
        dest_blobs.exists(&manifest_hash).await.unwrap(),
        "the manifest lands under its own content hash"
    );

    // Custody: one live row in the mirror set, keyed by the SOURCE path_hash
    // hex (the synthetic machine path), naming the mirrored manifest.
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody at the destination");
    let row = custody
        .items
        .iter()
        .find(|i| i.folder_name == folder_set)
        .expect("a custody row exists in the mirror set");
    assert_eq!(row.path.as_deref(), Some(hex::encode(path_hash).as_str()));
    assert_eq!(row.manifest_hash, hex::encode(manifest_hash.digest()));

    // Idempotent second pass: an unchanged head re-mirrors nothing.
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("second folder pass");
    assert_eq!(
        (report.uploaded_paths, report.dropped_paths),
        (0, 0),
        "an unchanged folder head re-uploads nothing"
    );

    // Supersede: a new sealed generation for the same path mirrors once, and
    // the displaced generation is RETAINED under the grace window T.
    let (path_hash_2, manifest_hash_2, _) =
        seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x42).await;
    assert_eq!(path_hash_2, path_hash, "same path ⇒ same path key");
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("supersede pass");
    assert_eq!(report.uploaded_paths, 1);
    let generations: fauna_protocol::backup::GenerationListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.generation.list",
        &fauna_protocol::backup::GenerationListRequest::default(),
    )
    .await
    .expect("owner lists retained generations");
    let retained = generations
        .generations
        .iter()
        .find(|g| g.folder_name == folder_set)
        .expect("the displaced generation is retained under T");
    assert_eq!(retained.manifest_hash, hex::encode(manifest_hash.digest()));

    // Detach → a FRESH coordinator (coverage loads at open) tears the mirror
    // down per-path: the live custody row tombstones away.
    let detach: fauna_protocol::backup::DetachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.detach_folder",
        &fauna_protocol::backup::DetachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner detaches the folder");
    assert!(detach.detached);
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    coordinator
        .run_all_tuples()
        .await
        .expect("the detach-reconciling sweep runs");
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner re-lists custody");
    assert!(
        custody.items.iter().all(|i| i.folder_name != folder_set),
        "detach tombstones every mirrored path (generations rest under T)"
    );
    // The new manifest hash 2 was the live generation the detach displaced —
    // it is retained, not destroyed (a writer's delete power is T-bounded).
    let generations: fauna_protocol::backup::GenerationListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.generation.list",
        &fauna_protocol::backup::GenerationListRequest::default(),
    )
    .await
    .expect("owner lists generations after detach");
    assert!(
        generations
            .generations
            .iter()
            .any(|g| g.folder_name == folder_set
                && g.manifest_hash == hex::encode(manifest_hash_2.digest())),
        "the detached live generation rests under the grace window"
    );
}

/// **A legally withheld digest never mirrors off-box, is retracted if it
/// already did, and mirrors the moment the flag lifts.** Path 3 of the
/// owner-scoped withhold ruling (`moderation.md` § Legal takedown → *The
/// blob-serve door* → *What the withhold binds on owner- and admin-scoped
/// routes*): the destination's own `/api/v1/chunks/{hash}` door holds no flag,
/// so a mirrored chunk would serve there to anyone with the hex the source
/// answers 451 for. The pass therefore (1) skips a live path naming a
/// withheld digest and declares it in its report, (2) retracts a path an
/// earlier pass mirrored — the ordinary path delete, retained under the
/// destination's grace window — and (3) re-tries every pass, so an overturn
/// is followed by the push with no other act. The owner's local file is
/// untouched throughout.
#[tokio::test]
async fn a_withheld_digest_never_mirrors_off_box_and_mirrors_once_the_flag_lifts() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");
    // Two files: `cat.jpg` will name a withheld chunk; `dog.jpg` never does,
    // so the pass keeps mirroring everything the withhold does not bind.
    let (cat_path, cat_manifest, cat_keys) =
        seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x41).await;
    let (_dog_path, dog_manifest, dog_keys) =
        seed_folder_file(&source, folder_id, "photos/dog.jpg", 0x51).await;

    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner attaches the folder");
    assert!(attach.attached);
    let folder_set = attach.folder_set.clone();

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();

    // (1) The withhold stands BEFORE the first pass: `cat.jpg`'s second chunk
    // is a withheld digest (byte-identical to a taken-down record's
    // attachment — the only way a folder chunk ever enters the set). The pass
    // pushes `dog.jpg`, skips `cat.jpg`, and says so.
    source
        .db
        .replace_blob_legal_withhold(&[cat_keys[1].digest()])
        .await
        .unwrap();
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("first pass under the withhold");
    assert_eq!(
        (
            report.uploaded_paths,
            report.dropped_paths,
            report.withheld_paths
        ),
        (1, 0, 1),
        "one path mirrors, one is withheld and declared"
    );
    for key in cat_keys.iter().chain(std::iter::once(&cat_manifest)) {
        assert!(
            !dest_blobs.exists(key).await.unwrap(),
            "no byte of the withheld path may land at the destination: {}",
            hex::encode(key.digest())
        );
    }
    for key in dog_keys.iter().chain(std::iter::once(&dog_manifest)) {
        assert!(
            dest_blobs.exists(key).await.unwrap(),
            "the unbound path mirrors as always: {}",
            hex::encode(key.digest())
        );
    }
    let live_paths = |custody: &fauna_protocol::backup::CustodyListReply| -> Vec<String> {
        custody
            .items
            .iter()
            .filter(|i| i.folder_name == folder_set)
            .filter_map(|i| i.path.clone())
            .collect()
    };
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody at the destination");
    assert!(
        !live_paths(&custody).contains(&hex::encode(cat_path)),
        "no custody row for the withheld path: {custody:?}"
    );

    // Every later pass re-tries it, and re-declares it, while the flag stands.
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("second pass under the withhold");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (0, 1),
        "a withheld path is retried and declared on every pass"
    );

    // (3) The overturn: the set empties, and the very next pass mirrors the
    // path with no other act — the state row was never advanced.
    source.db.replace_blob_legal_withhold(&[]).await.unwrap();
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("pass after the overturn");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (1, 0),
        "the flag lifted, so the path mirrors"
    );
    for key in cat_keys.iter().chain(std::iter::once(&cat_manifest)) {
        assert!(
            dest_blobs.exists(key).await.unwrap(),
            "the previously withheld path lands byte-identical once the flag lifts: {}",
            hex::encode(key.digest())
        );
    }
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody after the overturn");
    assert!(
        live_paths(&custody).contains(&hex::encode(cat_path)),
        "the custody row lands with the bytes: {custody:?}"
    );

    // (2) A takedown AFTER the mirror: the flag now binds a path an earlier
    // pass pushed. The next pass retracts it — the custody row tombstones
    // (retained under the grace window, like any writer delete) and the
    // state row goes, so an overturn re-mirrors through the ordinary push.
    source
        .db
        .replace_blob_legal_withhold(&[cat_keys[1].digest()])
        .await
        .unwrap();
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("pass after a takedown of an already-mirrored path");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (0, 1),
        "the already-mirrored path is retracted and declared"
    );
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody after the retraction");
    assert!(
        !live_paths(&custody).contains(&hex::encode(cat_path)),
        "the retracted path has no live custody row: {custody:?}"
    );
    let generations: fauna_protocol::backup::GenerationListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.generation.list",
        &fauna_protocol::backup::GenerationListRequest::default(),
    )
    .await
    .expect("owner lists retained generations");
    assert!(
        generations
            .generations
            .iter()
            .any(|g| g.folder_name == folder_set
                && g.manifest_hash == hex::encode(cat_manifest.digest())),
        "the retracted generation rests under the grace window, not destroyed: {generations:?}"
    );

    // And the overturn after a retraction re-mirrors through the ordinary push.
    source.db.replace_blob_legal_withhold(&[]).await.unwrap();
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("pass after the second overturn");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (1, 0),
        "a retracted path re-mirrors once the flag lifts"
    );
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody after the re-mirror");
    assert!(
        live_paths(&custody).contains(&hex::encode(cat_path)),
        "the custody row is back: {custody:?}"
    );
}

/// **A standing withhold that names none of a folder's mirrored paths costs
/// nothing once it has been checked once — even across a fresh coordinator.**
/// Row 729: before the fix, every already-mirrored manifest was re-opened and
/// re-decrypted on every pass for the entire life of ANY takedown on the box,
/// whether or not it touched this folder. The fix persists a digest of the
/// withheld set's content in the per-owner `segment-backup.sqlite`
/// (`folder_withhold_checkpoint`) — not in the coordinator, which
/// `NestBackupWorker` rebuilds fresh every sweep — so the very next pass under
/// an *unchanged* set skips every already-mirrored path with no store read at
/// all.
///
/// One pass immediately after the set changes must still open every
/// already-mirrored manifest once (there is no way to know a manifest's inner
/// store keys without opening it) — that pass is not free, and this test
/// says so. The pass *after* that, with the set still standing unchanged, is
/// where the report's `manifests_opened` must read zero.
#[tokio::test]
async fn an_unchanged_withhold_naming_no_mirrored_path_opens_no_manifest_on_the_next_pass() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");
    seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x41).await;
    seed_folder_file(&source, folder_id, "photos/dog.jpg", 0x51).await;

    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner attaches the folder");
    assert!(attach.attached);

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();

    // Pass A — the initial mirror. Both files are new, so both manifests open
    // regardless of the (empty) withheld set; that cost is not what this test
    // is about.
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("initial mirror pass");
    assert_eq!((report.uploaded_paths, report.manifests_opened), (2, 2));

    // A withhold naming a digest neither file's manifest or chunks carry —
    // "an unrelated takedown elsewhere on the box".
    source
        .db
        .replace_blob_legal_withhold(&[[0xAB; 32]])
        .await
        .unwrap();

    // Pass B — the first pass since the set changed. Today's code and the
    // fixed code agree here: both already-mirrored manifests must open once
    // to confirm neither names the new digest.
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("first pass under the new withhold");
    assert_eq!(
        (
            report.uploaded_paths,
            report.dropped_paths,
            report.withheld_paths,
            report.manifests_opened
        ),
        (0, 0, 0, 2),
        "the first pass under a changed set must still confirm every already-mirrored path"
    );

    // A FRESH coordinator — exactly what `NestBackupWorker` builds every
    // sweep — must still find the memo: it lives in the per-owner sqlite
    // file, not in the coordinator's own memory.
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();

    // Pass C — the withhold still stands, unchanged. This is the row 729
    // regression: today's code re-opens both manifests again here, forever,
    // for the life of the takedown; the fix opens none.
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("second pass under the unchanged withhold");
    assert_eq!(
        (
            report.uploaded_paths,
            report.dropped_paths,
            report.withheld_paths,
            report.manifests_opened
        ),
        (0, 0, 0, 0),
        "an unchanged withhold that names neither path must open no manifest, \
         even from a freshly-opened coordinator"
    );
}

/// **A partial pass that pushes then aborts must not leave a withhold stamp
/// a later pass wrongly trusts.** Row 760: `folder_withhold_checkpoint` (row
/// 729) is meant to be trustworthy only when it describes the withheld set a
/// FULLY-completed pass just finished re-checking every already-mirrored
/// path against. Before this fix, a pass that pushed a path and then aborted
/// on a LATER path left the stamp from before an overturn untouched -- so if
/// the same takedown is reinstated before anything else changes the set, the
/// stale digest matches the reinstated one by coincidence and the memo
/// trusts it, even though a path it now binds was mirrored in between. The
/// fix deletes the stamp the moment a pass sees it does not match the
/// current digest, before any path is examined, so an aborted pass leaves
/// nothing behind to (mis)trust.
#[tokio::test]
async fn a_partial_pass_leaves_no_stamp_the_next_pass_can_wrongly_trust() {
    let (_s_url, source, s_blobs) = start_nest().await;
    let (d_url, dest, dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");

    // P and R are named so P's path_hash sorts first -- the order
    // `get_files_for_folder` actually returns and `run_folder_once` walks --
    // without guessing at the hash's shape.
    let (p_name, r_name) = {
        let a = "photos/cat.jpg";
        let b = "photos/robin.jpg";
        if fauna_core::sync::path_hash(a) < fauna_core::sync::path_hash(b) {
            (a, b)
        } else {
            (b, a)
        }
    };

    // P alone, so pass 1 sees only the path the takedown will bind.
    let (p_path, p_manifest, p_keys) = seed_folder_file(&source, folder_id, p_name, 0x41).await;

    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner attaches the folder");
    assert!(attach.attached);
    let folder_set = attach.folder_set.clone();

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();

    // Pass 1 -- the takedown stands over P's second chunk before the first
    // pass ever runs. P is skipped and declared; the checkpoint stamps
    // clean under digest({c}).
    source
        .db
        .replace_blob_legal_withhold(&[p_keys[1].digest()])
        .await
        .unwrap();
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("first pass under the withhold");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (0, 1),
        "P is withheld and never pushed"
    );

    // R is seeded only now (after pass 1 stamped its checkpoint), then its
    // manifest is deleted from the source's own store -- standing in for
    // whatever transient fault aborts a real pass mid-loop.
    let (_r_path, r_manifest, _r_keys) = seed_folder_file(&source, folder_id, r_name, 0x99).await;
    let r_manifest_blob = s_blobs
        .get(&r_manifest)
        .await
        .unwrap()
        .expect("R's manifest was just stored");
    s_blobs.delete(&r_manifest).await.unwrap();

    // The takedown is overturned.
    source.db.replace_blob_legal_withhold(&[]).await.unwrap();

    // Pass 2 -- P is no longer withheld and mirrors; R's manifest is
    // missing, so the pass aborts loud on R (the "should be unreachable"
    // arm) AFTER P already pushed but BEFORE the checkpoint could stamp.
    let err = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect_err("R's missing manifest aborts the pass");
    assert!(
        err.to_string().contains("missing manifest"),
        "aborted for the expected reason: {err}"
    );
    for key in p_keys.iter().chain(std::iter::once(&p_manifest)) {
        assert!(
            dest_blobs.exists(key).await.unwrap(),
            "P mirrored before the pass reached R and aborted: {}",
            hex::encode(key.digest())
        );
    }

    // The same takedown is reinstated -- byte-identical to pass 1's digest,
    // since the memo hashes the set's CONTENT, not a generation counter.
    source
        .db
        .replace_blob_legal_withhold(&[p_keys[1].digest()])
        .await
        .unwrap();

    // R's manifest reappears (the transient fault clears) so pass 3 can run
    // to completion and isolate what this row is actually about: whether P
    // -- now live at the destination, under a reinstated withhold -- gets
    // retracted rather than silently trusted under the stale pre-overturn
    // stamp.
    s_blobs.put(&r_manifest, &r_manifest_blob).await.unwrap();

    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("third pass, after the reinstated takedown");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (1, 1),
        "R (new) mirrors and P (reinstated withhold, mirrored in between) is \
         retracted -- never silently skipped under a stamp that merely \
         happens to match again: {report:?}"
    );
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody after the third pass");
    assert!(
        custody
            .items
            .iter()
            .filter(|i| i.folder_name == folder_set)
            .all(|i| i.path.as_deref() != Some(hex::encode(p_path).as_str())),
        "the reinstated-withheld path has no live custody row: {custody:?}"
    );
}

/// **A teardown failure after a takedown must leave no checkpoint for a
/// later pass to wrongly trust.** The test above forces its natural fault
/// into the MAIN loop, over a path still live; this one forces it into the
/// DROPPED-PATHS TEARDOWN instead, over a path no longer live at all — the
/// one branch that fault could never reach, since a dropped path's custody
/// delete (`segment_backup.rs:1198-1199`) runs in the teardown loop that
/// follows the main loop, never inside it. The stamp write moved to AFTER
/// both loops (`:1211-1215`) is what makes this safe: a teardown that fails
/// leaves `run_folder_once` returning `Err` before that write ever runs, so
/// no later pass can find a checkpoint claiming to have rechecked a path it
/// never actually re-examined.
///
/// P mirrors once, then its owner deletes it — so the next pass sees P only
/// in the dropped-paths teardown, never the live loop's own withhold check
/// (`already_mirrored && (withheld.is_empty() || withhold_unchanged)`,
/// `:1077-1079`). A takedown then names P's own chunk, and the owner's
/// writer grant is revoked at the destination: P's teardown delete is the
/// only store write this pass attempts, so it is the only thing that can
/// fail, and it does. The grant is restored and P is recreated with the
/// SAME manifest hash (`seed_folder_file` with the same tag) before the next
/// pass — reachable, not merely hypothetical, since the hash is a pure
/// function of content, uncorrelated with when the bytes were (re)written.
/// That next pass must still open P's manifest and retract it, never trust
/// a checkpoint the failed pass could not have earned. The withheld set
/// never empties before the final assertion, so a green result cannot be
/// `withheld.is_empty()`'s own short-circuit passing for the wrong reason.
#[tokio::test]
async fn a_teardown_failure_leaves_no_checkpoint_the_next_pass_can_wrongly_trust() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, dest_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");
    let (p_path, p_manifest, p_keys) =
        seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x41).await;

    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner attaches the folder");
    assert!(attach.attached);
    let folder_set = attach.folder_set.clone();

    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();

    // Pass 1 — P mirrors, no withhold stands yet. The checkpoint stamps
    // clean under digest({}).
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("first pass mirrors P");
    assert_eq!(report.uploaded_paths, 1, "P mirrors");
    for key in p_keys.iter().chain(std::iter::once(&p_manifest)) {
        assert!(
            dest_blobs.exists(key).await.unwrap(),
            "P's bytes land before anything withholds them"
        );
    }

    // The owner deletes P from the folder — the NEXT pass sees it only in
    // the dropped-paths teardown, never the live loop.
    source
        .db
        .record_sync_change_metered(
            &OWNER,
            &OWNER,
            None,
            &p_path,
            None,
            0,
            "delete",
            folder_id,
            &[0x99; 32],
            None,
            None,
            None,
            Some(b"sealed-name-label"),
            None,
            None,
            i64::MAX,
        )
        .await
        .expect("record P's deletion from the folder");

    // A takedown names P's own chunk — this changes the withheld set's
    // digest, so the fix deletes the stale (empty-set) checkpoint before any
    // path is examined this pass, whatever happens below.
    source
        .db
        .replace_blob_legal_withhold(&[p_keys[1].digest()])
        .await
        .unwrap();

    // The owner revokes the writer grant AT THE DESTINATION — the natural
    // fault. P is dropped, not live, so the main loop has nothing to do;
    // the teardown's custody-delete for P is the only store write this pass
    // attempts, and it is the only thing that can fail.
    let rev: WriterGrantRevokeReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.writer_grant.revoke",
        &WriterGrantRevokeRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            extra: Default::default(),
        },
    )
    .await
    .expect("revoke");
    assert!(rev.revoked);

    coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect_err("the revoked grant fails P's teardown delete");

    // The obstacle clears.
    let wg: WriterGrantRegisterReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.writer_grant.register",
        &WriterGrantRegisterRequest {
            writer_nest_id: hex::encode(source.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
    .expect("owner re-authorizes the writer");
    assert!(wg.ok);

    // P reappears with the SAME manifest hash.
    let (p_path_2, p_manifest_2, _) =
        seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x41).await;
    assert_eq!(p_path_2, p_path, "same path ⇒ same path key");
    assert_eq!(
        p_manifest_2, p_manifest,
        "same content ⇒ the same manifest hash, byte for byte"
    );

    // The withheld set is UNCHANGED and still non-empty here — so a green
    // result below cannot be `withheld.is_empty()`'s own short-circuit
    // passing for the wrong reason; it must come from the checkpoint being
    // untrustworthy, exactly as the failed pass left it.
    let report = coordinator
        .run_folder_once(&dest_row, folder_id)
        .await
        .expect("the retry, after the teardown's fault clears");
    assert_eq!(
        (report.uploaded_paths, report.withheld_paths),
        (0, 1),
        "P is recreated already_mirrored under an UNCHANGED digest — a \
         checkpoint wrongly trusted from the failed pass would skip it \
         outright rather than retract it: {report:?}"
    );
    let custody: fauna_protocol::backup::CustodyListReply = client_call(
        &dest,
        OWNER,
        "fauna.backup.custody.list",
        &fauna_protocol::backup::CustodyListRequest::default(),
    )
    .await
    .expect("owner lists custody after the retry");
    assert!(
        custody
            .items
            .iter()
            .filter(|i| i.folder_name == folder_set)
            .all(|i| i.path.as_deref() != Some(hex::encode(p_path).as_str())),
        "the recreated, now-withheld path has no live custody row: {custody:?}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The placement journal rides the set
//
// A backed-up message kind's corpus is its content segments AND its placement
// journal (`backup-destinations.md` § Third destination kind → *Where restored
// mail lands*). Mechanics: `segment-backup-protocol.md` § Client-device
// custodian (pull) → *Restore* → *The placement journal rides the set*.
// ═════════════════════════════════════════════════════════════════════════════

/// **The sweep carries the journal, in the kind's own set.**
///
/// Three things are pinned, each of which is a way the ruling could be built
/// wrong and still look right from the source's side:
///
///   - the journal's custody rests in `__mail`, under the placement infix, and
///     **no second set exists** — the journal is the kind's other half, not a
///     second backed-up kind;
///   - what the destination holds opens, under the granted key, to the
///     source's own journal file **byte for byte** — so a restore replays the
///     journal production wrote, not a rendering of it;
///   - the journal is diffed against its own upload state, so a second sweep
///     moves nothing and a later change moves only the journal.
#[tokio::test]
async fn the_sweep_carries_the_placement_journal_in_the_kinds_own_set() {
    use fauna_sync_engine::segment_backup::SegmentFamily;

    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    file_mail(
        &source,
        &OWNER,
        &[
            (1_715_000_000_000, "INBOX", ""),
            (1_715_000_100_000, "Archive", "\\Seen"),
        ],
    )
    .await;
    enroll(&source, &dest, &d_url).await;

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");

    let scope = hex::encode(OWNER);
    let (content, journal) = (SegmentFamily::Content, SegmentFamily::Placement);
    let held = custody_paths(&dest).await;
    for path in [
        content.dat_path(&scope, 1),
        content.meta_path(&scope, 1),
        content.mirror_path(&scope, KIND),
        journal.dat_path(&scope, 1),
        journal.meta_path(&scope, 1),
        journal.mirror_path(&scope, KIND),
    ] {
        assert!(
            held.contains(&path),
            "the destination holds no custody at {path}; held: {held:?}"
        );
    }
    assert!(
        dest.db
            .get_folder_for_actor("__mail-placement", &OWNER)
            .await
            .unwrap()
            .is_none(),
        "the journal rides the kind's own set; a second set would make it a second kind"
    );

    // Byte for byte the source's own journal pair, opened under the granted key.
    for (custody, on_disk) in [
        (
            journal.dat_path(&scope, 1),
            source.mail_placement.segment_file_path(&OWNER, 1),
        ),
        (
            journal.meta_path(&scope, 1),
            source.mail_placement.segment_meta_path(&OWNER, 1),
        ),
    ] {
        assert_eq!(
            open_custody_path(&dest, &d_url, &custody).await,
            std::fs::read(&on_disk).expect("the source's own journal file"),
            "{custody} is not the source's journal file"
        );
    }
    // …and it is the journal, not the content under another name.
    assert_ne!(
        open_custody_path(&dest, &d_url, &journal.dat_path(&scope, 1)).await,
        open_custody_path(&dest, &d_url, &content.dat_path(&scope, 1)).await,
    );

    // A second sweep moves nothing in either family.
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    let again = coordinator
        .run_once(&dest_row, KIND)
        .await
        .expect("the second pass");
    let again_journal = again.placement.as_deref().expect("mail has a journal");
    assert!(
        again.uploaded_segments.is_empty() && !again.manifest_uploaded,
        "content: nothing new, nothing re-sent"
    );
    assert!(
        again_journal.uploaded_segments.is_empty() && !again_journal.manifest_uploaded,
        "journal: nothing new, nothing re-sent"
    );

    // One more mail filed: the content family gains a record in a new segment
    // and so does the journal, each diffed against its OWN state.
    file_mail(&source, &OWNER, &[(1_715_000_200_000, "INBOX", "")]).await;
    let third = coordinator
        .run_once(&dest_row, KIND)
        .await
        .expect("the third pass");
    let third_journal = third.placement.as_deref().expect("mail has a journal");
    assert_eq!(third_journal.uploaded_segments, vec![2]);
    assert!(third_journal.manifest_uploaded);
    assert!(
        custody_paths(&dest)
            .await
            .contains(&journal.dat_path(&scope, 2)),
        "the journal's second segment reached the destination"
    );
    assert_eq!(
        status(&source, OWNER).await.destinations[0].backlog_count,
        0,
        "the status row's backlog is a statement about content, and content is level"
    );
}

/// The mail every journal test restores: two mailboxes, distinct flags, so a
/// restore that files everything into one mailbox, or drops the flags, or
/// renumbers the UIDs, cannot pass.
const FILED: &[Filed<'static>] = &[
    (1_715_000_000_000, "INBOX", ""),
    (1_715_000_100_000, "INBOX", "\\Seen"),
    (1_715_000_200_000, "Archive", "\\Seen \\Flagged"),
];

/// A source nest holding [`FILED`] mail, and a destination holding its backup
/// (both families) plus the owner's key grant — a target standing exactly where
/// phase 2 leaves it. Returns `(source, dest, dest_url)`.
async fn delivered_filed() -> (Arc<AppState>, Arc<AppState>, String) {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    file_mail(&source, &OWNER, FILED).await;
    enroll(&source, &dest, &d_url).await;
    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");
    grant_key_to(&dest).await;
    (source, dest, d_url)
}

/// Every `seg-*` file in a nest's placement journal area for `OWNER`, with its
/// bytes — the on-disk truth behind the journal's manifest.
fn journal_area_contents(nest: &Arc<AppState>) -> Vec<(String, Vec<u8>)> {
    let root = fauna_mail::segments::placement::mail_placement_segments_root(
        nest.mail_placement.data_dir(),
        &OWNER,
    );
    let Ok(dir) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<u8>)> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("seg-"))
        .map(|n| {
            let bytes = std::fs::read(root.join(&n)).expect("read a journal file");
            (n, bytes)
        })
        .collect();
    out.sort();
    out
}

/// Tombstone one custody path at the destination through the production
/// owner-authed door — which is what an absence of custody IS.
async fn owner_tombstones_custody(dest: &Arc<AppState>, path: &str) {
    let device: [u8; 32] = [0x0B; 32];
    dest.db
        .register_sync_device(&OWNER, &device, "staging-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&dest.db, &OWNER).await;
    let _: fauna_protocol::sync::SyncChangeRecordReply = client_call(
        dest,
        OWNER,
        "fauna.sync.changes.record",
        &fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__mail".to_string(),
            device_id: hex::encode(device),
            path: path.to_string(),
            manifest_hash: None,
            size_bytes: 0,
            change_type: "delete".to_string(),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("tombstone custody at {path}: {e:?}"));
}

/// **Restored mail lands where it was.**
///
/// The ruling in one test (`backup-destinations.md` § Third destination kind →
/// *Where restored mail lands*): after the one `materialize` call the rebuilt
/// nest serves every mailbox exactly as the source did — the same messages, in
/// the same mailboxes, under the same UIDs, with the same flags, in a mailbox
/// tree carrying the source's UIDVALIDITY, UIDNEXT and HIGHESTMODSEQ.
///
/// Read through the join the inbox fetch and IMAP both run, so a placement row
/// with no live record behind it does not count, and neither does a live record
/// in no mailbox — which is the state this whole track exists to end.
#[tokio::test]
async fn materialize_files_the_restored_mail_where_it_was() {
    let (source, dest, _d_url) = delivered_filed().await;
    for mailbox in ["INBOX", "Archive"] {
        assert!(
            serve_mailbox(&dest, &OWNER, mailbox).await.is_empty(),
            "pre-state: the destination serves nothing from {mailbox}"
        );
    }

    let reply = materialize(&dest, "__mail")
        .await
        .expect("the owner materializes their own custody");

    assert_eq!(reply.records, FILED.len() as u64);
    assert_eq!(
        reply.placements,
        Some(FILED.len() as u64),
        "the reply counts what was filed, so a caller can tell filed from unfiled"
    );
    for mailbox in ["INBOX", "Archive"] {
        let was = serve_mailbox(&source, &OWNER, mailbox).await;
        assert!(!was.is_empty(), "the source really filed mail in {mailbox}");
        assert_eq!(
            serve_mailbox(&dest, &OWNER, mailbox).await,
            was,
            "{mailbox}: same messages, same UIDs, same flags"
        );
    }
    assert_eq!(
        serve_mailbox(&dest, &OWNER, "INBOX")
            .await
            .iter()
            .map(|(_, flags, _)| flags.as_str())
            .collect::<Vec<_>>(),
        vec!["", "\\Seen"],
        "the flags are the ones the owner set, message by message"
    );
    assert_eq!(
        mailbox_tree(&dest, &OWNER).await,
        mailbox_tree(&source, &OWNER).await,
        "the mailbox tree carries the source's UIDVALIDITY, UIDNEXT and HIGHESTMODSEQ"
    );

    // The journal is the durable truth, so it is what must have arrived: the
    // rebuilt nest's journal IS the source's, byte for byte.
    assert_eq!(journal_area_contents(&dest), journal_area_contents(&source));
    let restored = dest.mail_placement.current_manifest(&OWNER).await.unwrap();
    let original = source
        .mail_placement
        .current_manifest(&OWNER)
        .await
        .unwrap();
    assert_eq!(restored.placements, original.placements);
    assert_eq!(restored.mailboxes, original.mailboxes);

    // And the account is live in the ordinary sense: the next mail to arrive is
    // filed after the restored ones, never over them.
    file_mail(&dest, &OWNER, &[(1_715_000_900_000, "INBOX", "")]).await;
    let inbox = serve_mailbox(&dest, &OWNER, "INBOX").await;
    assert_eq!(
        inbox.iter().map(|(uid, _, _)| *uid).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the new mail takes the next UID; the restored UIDs are untouched"
    );
}

/// **The empty-target rule's journal half is freshness, not emptiness.**
///
/// This target holds no live record and no placement row: by every count the
/// content arm takes, it is empty. But its mailboxes have held mail — one
/// message was filed and expunged — so it carries a tombstone and a spent UID.
/// Folding a backup over that would hand a mail client a vanished notice for a
/// UID the restore had just made live.
///
/// Refused with `target_not_empty`, and with nothing written: not the content
/// segments, not the journal, not a row.
#[tokio::test]
async fn materialize_refuses_an_account_whose_mailboxes_have_held_mail() {
    use fauna_mail::segments::placement::MailPlacementRecord;

    let (_source, dest, _d_url) = delivered_filed().await;
    for record in [
        MailPlacementRecord::Create {
            mailbox: "INBOX".to_string(),
            uid_validity: 1,
            attrs: Vec::new(),
        },
        MailPlacementRecord::Append {
            mailbox: "INBOX".to_string(),
            uid: 1,
            modseq: 2,
            flags: Vec::new(),
            content_record_id: vec![0xEE; 32],
            internal_date: 1_714_000_000_000,
        },
        MailPlacementRecord::Expunge {
            mailbox: "INBOX".to_string(),
            uid_set: vec![1],
            modseq: 3,
            deleted_at: 1_714_000_100,
        },
    ] {
        dest.mail_placement
            .append_event(&OWNER, &record)
            .await
            .expect("journal the target's own history");
    }
    dest.mail_placement.finalize_open(&OWNER).await.unwrap();
    let journal_before = journal_area_contents(&dest);
    assert!(
        !journal_before.is_empty(),
        "the target has a journal of its own"
    );

    let err = materialize(&dest, "__mail")
        .await
        .unwrap_err_or_panic("a box that has held mail is not a fresh target");

    assert_eq!(err.code, "fauna.backup.target_not_empty");
    assert!(
        segment_area_files(&dest).is_empty(),
        "refused before any content segment was written"
    );
    assert_eq!(
        journal_area_contents(&dest),
        journal_before,
        "and the target's own journal is exactly as it was"
    );
    assert!(serve_mail(&dest).await.is_empty());
}

/// **A never-used target's mailbox scaffolding is superseded.**
///
/// The realistic rebuilt nest: before restoring, the owner's mail client
/// connected. That seeds the six standard mailboxes, and here it also created
/// a folder of its own and one that shares a name with a backed-up mailbox.
/// None of them ever held a message.
///
/// The corpus's mailbox tree replaces all of it — the ceremony's one
/// replacement. What must NOT happen is the refusal a naive "the journal area
/// must be empty" rule would give: the owner of an account holding no mail at
/// all, told to point at another nest.
#[tokio::test]
async fn materialize_supersedes_a_never_used_targets_mailbox_scaffolding() {
    use fauna_mail::segments::placement::MailPlacementRecord;

    let (source, dest, _d_url) = delivered_filed().await;

    // What a connecting mail client leaves behind, in production order: the
    // row, then its journal record.
    for seeded in dest
        .db
        .ensure_bridge_imap_mailboxes(&OWNER)
        .await
        .expect("seed the standard mailboxes")
    {
        dest.mail_placement
            .append_event(
                &OWNER,
                &MailPlacementRecord::Create {
                    mailbox: seeded.name,
                    uid_validity: seeded.uid_validity,
                    attrs: seeded.attrs,
                },
            )
            .await
            .unwrap();
        // One segment per record, so the scaffolding spans more segment ids
        // than the corpus has. Renaming the corpus into place overwrites only
        // the ids it shares; a scaffolding segment past them is still there
        // unless the adoption removes it, and a manifest rebuild would replay
        // its `Create` over the restored tree.
        dest.mail_placement.finalize_open(&OWNER).await.unwrap();
    }
    dest.db
        .create_bridge_imap_mailbox(&OWNER, "Templates", 777)
        .await
        .expect("the client creates a folder of its own");
    dest.mail_placement
        .append_event(
            &OWNER,
            &MailPlacementRecord::Create {
                mailbox: "Templates".to_string(),
                uid_validity: 777,
                attrs: Vec::new(),
            },
        )
        .await
        .unwrap();
    dest.mail_placement.finalize_open(&OWNER).await.unwrap();
    assert!(
        mailbox_tree(&dest, &OWNER)
            .await
            .iter()
            .any(|(name, ..)| name == "Templates"),
        "pre-state: the target has a mailbox tree of its own"
    );
    source.mail_placement.finalize_open(&OWNER).await.unwrap();
    assert!(
        journal_area_contents(&dest).len() > journal_area_contents(&source).len(),
        "pre-state: the scaffolding spans segment ids the corpus does not have"
    );

    let reply = materialize(&dest, "__mail")
        .await
        .expect("an account that never held mail is a fresh target");

    assert_eq!(reply.placements, Some(FILED.len() as u64));
    assert_eq!(
        mailbox_tree(&dest, &OWNER).await,
        mailbox_tree(&source, &OWNER).await,
        "the mailbox tree is the backed-up one"
    );
    for mailbox in ["INBOX", "Archive"] {
        assert_eq!(
            serve_mailbox(&dest, &OWNER, mailbox).await,
            serve_mailbox(&source, &OWNER, mailbox).await,
        );
    }
    assert_eq!(
        journal_area_contents(&dest),
        journal_area_contents(&source),
        "and the journal is the corpus's, so a rebuild of the manifest from it \
         can never bring the scaffolding back over the restored tree"
    );
}

/// **A crash between the journal adopt and the commit resumes on retry.**
///
/// Staged as the real half-state: both families are on disk and adopted, and
/// the one transaction that makes them live never committed — so the mirror
/// rows and the placement rows are both absent. The retry must recognise the
/// journal it finds as this ceremony's own and finish, not read an adopted
/// corpus as a lived-in account and refuse for ever.
#[tokio::test]
async fn a_crash_between_the_journal_adopt_and_the_commit_resumes_on_retry() {
    let (source, dest, _d_url) = delivered_filed().await;
    materialize(&dest, "__mail").await.expect("the first flip");
    let journal_on_disk = journal_area_contents(&dest);
    let content_on_disk = segment_area_contents(&dest);

    // The crash: the transaction never committed.
    {
        let conn = dest.db.conn().await;
        for sql in [
            "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'mail'",
            "DELETE FROM bridge_imap_messages WHERE actor_id = ?1",
            "DELETE FROM bridge_imap_mailbox_state WHERE actor_id = ?1",
            "DELETE FROM bridge_imap_expunged WHERE actor_id = ?1",
            "DELETE FROM bridge_imap_subscriptions WHERE actor_id = ?1",
        ] {
            conn.execute(sql, rusqlite::params![OWNER.as_slice()])
                .expect("drop the rows the interrupted run never committed");
        }
    }
    assert!(
        serve_mailbox(&dest, &OWNER, "INBOX").await.is_empty(),
        "the staged state is the real one: adopted, and serving nothing"
    );

    let reply = materialize(&dest, "__mail")
        .await
        .expect("a half-finished run of this ceremony resumes rather than refusing");

    assert_eq!(reply.placements, Some(FILED.len() as u64));
    for mailbox in ["INBOX", "Archive"] {
        assert_eq!(
            serve_mailbox(&dest, &OWNER, mailbox).await,
            serve_mailbox(&source, &OWNER, mailbox).await,
        );
    }
    assert_eq!(
        journal_area_contents(&dest),
        journal_on_disk,
        "the retry did not rewrite the adopted journal"
    );
    assert_eq!(segment_area_contents(&dest), content_on_disk);

    // Once it HAS committed, the account is live and the rule is the ordinary
    // one again.
    let err = materialize(&dest, "__mail")
        .await
        .unwrap_err_or_panic("a live account refuses");
    assert_eq!(err.code, "fauna.backup.target_not_empty");
}

/// A destination holding [`FILED`] mail's **content only** — no custody at the
/// journal's paths: a delivery still under way. Returns `(source, dest)`.
async fn delivered_journal_less() -> (Arc<AppState>, Arc<AppState>) {
    use fauna_sync_engine::segment_backup::SegmentFamily;

    let (source, dest, _d_url) = delivered_filed().await;
    let scope = hex::encode(OWNER);
    let journal = SegmentFamily::Placement;
    for path in [
        journal.mirror_path(&scope, KIND),
        journal.dat_path(&scope, 1),
        journal.meta_path(&scope, 1),
    ] {
        owner_tombstones_custody(&dest, &path).await;
    }
    (source, dest)
}

/// **A copy that holds content and no journal refuses as an unfinished
/// delivery** (`backup-destinations.md` § *Where restored mail lands*,
/// consequence 3). Every mail corpus carries its journal, so a copy without one
/// is a pass still under way; restoring it would leave mail live and in no
/// mailbox. Refused before anything is written, like calendar and contacts.
#[tokio::test]
async fn a_copy_that_holds_no_journal_refuses_as_an_unfinished_delivery() {
    let (_source, dest) = delivered_journal_less().await;

    let err = materialize(&dest, "__mail")
        .await
        .unwrap_err_or_panic("a journal-less copy is not whole");

    assert_eq!(err.code, "fauna.backup.custody_incomplete");
    assert!(
        segment_area_files(&dest).is_empty(),
        "refused before any content segment was written"
    );
    assert!(serve_mail(&dest).await.is_empty());
}

/// **The journal is anchored exactly as the content is.** A journal segment
/// whose custody does not open to the bytes the journal's own mirror advertised
/// is refused, and nothing is written — in EITHER family. The verify-everything-
/// before-writing-anything rule spans both: a journal that fails its check must
/// not leave the content it rides with half-restored.
#[tokio::test]
async fn materialize_refuses_a_journal_segment_its_mirror_does_not_anchor() {
    use fauna_sync_engine::segment_backup::SegmentFamily;

    let (_source, dest, _d_url) = delivered_filed().await;
    let scope = hex::encode(OWNER);
    // Re-record the journal's `.dat` custody to point at the CONTENT's `.dat`
    // manifest: real bytes, sealed under the right key, opening perfectly —
    // and not the bytes the journal's mirror names.
    let rows = dest.db.list_backup_custody(&OWNER, None, 0).await.unwrap();
    let content_dat = SegmentFamily::Content.dat_path(&scope, 1);
    let stolen = rows
        .iter()
        .find(|r| r.path.as_deref() == Some(content_dat.as_str()))
        .expect("the content's custody row");
    let device: [u8; 32] = [0x0B; 32];
    dest.db
        .register_sync_device(&OWNER, &device, "staging-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&dest.db, &OWNER).await;
    let _: fauna_protocol::sync::SyncChangeRecordReply = client_call(
        &dest,
        OWNER,
        "fauna.sync.changes.record",
        &fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__mail".to_string(),
            device_id: hex::encode(device),
            path: SegmentFamily::Placement.dat_path(&scope, 1),
            manifest_hash: Some(hex::encode(&stolen.manifest_hash)),
            size_bytes: stolen.size_bytes,
            change_type: "upsert".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("re-record the journal's custody at another file's manifest");

    materialize(&dest, "__mail")
        .await
        .unwrap_err_or_panic("a journal segment that is not the one its mirror names");

    assert!(
        segment_area_files(&dest).is_empty(),
        "the content was not written either"
    );
    assert!(journal_area_contents(&dest).is_empty());
    assert!(serve_mail(&dest).await.is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// Phase 3 — `fauna.backup.custody.materialize`
//
// Delivery (everything above) leaves the destination in DESTINATION posture:
// it holds the owner's corpus as opaque sealed chunks plus custody rows, and
// serves the owner nothing. Materialize is the one gesture that flips a set to
// LIVE SOURCE posture, and these are its contract:
//
//   - it reconstitutes what the destination already holds, under the key the
//     owner already granted, and the result reads back through the ORDINARY
//     mail read path — not a bespoke restore reader;
//   - it refuses a scope that already holds live records, typed, with no force
//     arm (the empty-target rule);
//   - it refuses a destination that has not caught up, rather than writing a
//     `.dat` with no sidecar that nothing could ever open;
//   - it refuses bytes that do not match what the source's own mirror
//     advertised;
//   - and every refusal leaves the target and the custody exactly as they were.
//
// Goal docs: `backup-destinations.md` § Third destination kind → *Re-seed*
// (phase 3, the empty-target rule) and `message-segment-store.md` § Client-device
// custodian (pull) → *Restore* (the wire mechanics).
//
// These run against a NEST-kind destination because delivered custody is
// byte-identical whichever leg wrote it — the ceremony's uniformity payoff is
// exactly that the verb cannot tell a custodian's push from a source nest's, and
// this file already owns a real, green delivery leg to prove it over.
// ═════════════════════════════════════════════════════════════════════════════

/// The owner grants the same `NestBackupKey` to the nest they are seeding — the
/// enrollment-time grant phase 3 materializes under. Same 32 bytes as the source
/// holds, because it is one seed-derived root of the owner's, not a per-nest
/// secret.
async fn grant_key_to(nest: &Arc<AppState>) {
    let grant: NestKeyGrantReply = client_call(
        nest,
        OWNER,
        "fauna.backup.nest_key.grant",
        &NestKeyGrantRequest {
            nest_backup_key: serde_bytes::ByteBuf::from(GRANTED_KEY.to_vec()),
            extra: Default::default(),
        },
    )
    .await
    .expect("the owner grants its NestBackupKey to the nest it is seeding");
    assert!(grant.ok);
}

async fn materialize(
    nest: &Arc<AppState>,
    set_name: &str,
) -> Result<CustodyMaterializeReply, RpcError> {
    client_call(
        nest,
        OWNER,
        "fauna.backup.custody.materialize",
        &CustodyMaterializeRequest {
            set_name: set_name.to_string(),
            folder_display_name: None,
            ..Default::default()
        },
    )
    .await
}

/// Every mail record a nest serves for `OWNER` through the ordinary read path
/// (mirror rows + segment files), as `(seq, body)` pairs.
async fn serve_mail(nest: &Arc<AppState>) -> Vec<(i64, Vec<u8>)> {
    // LIMIT is a literal SQL LIMIT here (0 would select nothing) — take the
    // whole corpus, which is two records.
    fauna_nest::segments::mail::read_after_seq(&nest.mail_segments, &nest.db, &OWNER, 0, i64::MAX)
        .await
        .expect("the ordinary mail read path")
        .into_iter()
        .map(|(seq, _rid, body, _floor)| (seq, body))
        .collect()
}

/// The segment files a nest holds for `OWNER` — the on-disk truth behind
/// [`serve_mail`], which a missing mirror would hide.
///
/// Load-bearing for the "verify everything before writing anything" claim: a
/// refused materialize must leave this EMPTY, and only reading the area can say
/// so. `serve_mail` cannot — an implementation that wrote segment files and then
/// refused before the mirror rebuild serves nothing either.
fn segment_area_files(nest: &Arc<AppState>) -> Vec<String> {
    let root = fauna_mail::segments::mail_segments_root(nest.mail_segments.data_dir(), &OWNER);
    let Ok(dir) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<String> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("seg-"))
        .collect();
    out.sort();
    out
}

/// Every `seg-*` file a nest holds for `OWNER`, **with its bytes** — the
/// stronger sibling of [`segment_area_files`].
///
/// Needed because the store's orphan reclaim unlinks a segment and opens a new
/// one at the same id immediately after, so a by-name comparison across an
/// append is vacuous: every name survives a reclaim that destroyed the records
/// under it. Anything asserting that a committed segment was left alone has to
/// compare content.
fn segment_area_contents(nest: &Arc<AppState>) -> Vec<(String, Vec<u8>)> {
    let root = fauna_mail::segments::mail_segments_root(nest.mail_segments.data_dir(), &OWNER);
    let Ok(dir) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<u8>)> = dir
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("seg-"))
        .map(|n| {
            let bytes = std::fs::read(root.join(&n)).expect("read a segment file");
            (n, bytes)
        })
        .collect();
    out.sort();
    out
}

/// A source nest + a destination holding its backup, with the destination also
/// holding the owner's key grant — i.e. a target standing exactly where phase 2
/// leaves it. Returns `(source, dest, dest_url)`.
async fn delivered() -> (Arc<AppState>, Arc<AppState>, String) {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000, 1_715_000_100_000]).await;
    enroll(&source, &dest, &d_url).await;

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep runs");

    grant_key_to(&dest).await;
    (source, dest, d_url)
}

/// **A nest holding nothing but custody becomes a live source for that corpus.**
///
/// The whole point of the ceremony in one assertion: before the verb the
/// destination serves the owner nothing at all — it holds sealed chunks it
/// cannot read — and after it, the owner's mail comes back through the same
/// `read_after_seq` the relay and the bridges use, record for record, with the
/// floor columns the mirror rebuild re-emits from the sidecars' own footers.
#[tokio::test]
async fn materialize_turns_a_destination_into_a_live_source_for_the_corpus() {
    let (source, dest, _d_url) = delivered().await;

    let on_source = serve_mail(&source).await;
    assert_eq!(on_source.len(), 2, "the source holds the corpus");
    assert!(
        serve_mail(&dest).await.is_empty(),
        "a destination serves nothing — it holds opaque chunks, not an account"
    );

    let reply = materialize(&dest, "__mail")
        .await
        .expect("the owner materializes their own custody");
    assert_eq!(
        reply.segments,
        live_segment_ids(&source, OWNER).await,
        "every live segment the mirror named is now in the target's segment area"
    );
    assert_eq!(reply.records, on_source.len() as u64);
    assert!(
        reply.custody_redundant,
        "the custody copy is now redundant — reclaiming it stays the owner's own gesture"
    );

    // The corpus reads back through the ORDINARY path, not a restore reader.
    assert_eq!(
        serve_mail(&dest).await,
        on_source,
        "the seeded nest serves the owner's mail as if it had always hosted it"
    );

    // Nothing was consumed: the custody the target reconstituted from is still
    // there, exactly as many rows as before.
    let custody = dest
        .db
        .list_backup_custody(&OWNER, None, 0)
        .await
        .unwrap()
        .len();
    assert!(
        custody > 0,
        "the ceremony deletes nothing — the custody set survives its own materialize"
    );
}

/// **The corpus survives the first mail that arrives after the flip.**
///
/// Materialize writes the segment halves itself and rebuilds the SQLite mirror,
/// but the segment store tracks its next id in the per-scope KIND MANIFEST, and
/// on a target that has never appended that manifest still reads
/// `next_seg_id: 1` (`KindManifest::empty`). The store's rotation reclaims
/// whatever `.dat`/`.meta` already sits at the id it is about to open, because
/// a *committed* segment can never legitimately be there — the crash-recovery
/// reclaim a 0-byte `seg-00000040.dat` bought after it permanently 451'd all
/// mail for an actor. So the first ordinary mail to arrive rotates onto
/// `seg-00000001`, finds the materialized corpus's first segment, and unlinks
/// both halves, silently doing exactly the recovery it was built to do.
///
/// That is the ceremony's ratified property broken one delivery later
/// (`message-segment-store.md` § Client-device custodian (pull) → *Restore*:
/// the ceremony "deletes nothing anywhere (*No user-data loss* structurally)"),
/// and it fires on the FIRST mail the re-seeded nest receives — which is the
/// whole reason to have re-seeded it. The fix belongs in the writer that
/// commits segments behind the manifest's back, never in the store's reclaim.
///
/// The file assertion comes before the read assertion deliberately: a deleted
/// segment leaves mirror rows pointing at bytes that are gone, so the ordinary
/// read path may fail outright, and a panic there would say far less than the
/// name of the file that vanished.
#[tokio::test]
async fn a_materialized_corpus_survives_the_first_mail_that_arrives_after_it() {
    let (source, dest, _d_url) = delivered().await;
    let corpus = serve_mail(&source).await;
    assert_eq!(corpus.len(), 2, "the source holds the corpus");

    let reply = materialize(&dest, "__mail").await.expect("the flip");
    assert!(
        reply.segments.contains(&1),
        "the corpus occupies the id a fresh manifest hands out first, which is \
         what makes this the default case rather than an unlucky one: {:?}",
        reply.segments
    );
    let after_flip = segment_area_contents(&dest);
    assert!(
        !after_flip.is_empty(),
        "the ceremony left segment files on disk — without this guard every \
         later comparison over `after_flip` passes vacuously"
    );
    assert_eq!(
        serve_mail(&dest).await,
        corpus,
        "the flip serves the corpus"
    );

    // One ordinary mail arrives — the whole point of having re-seeded the nest.
    //
    // NOT via `append_mail`: records are content-addressed, and that helper
    // derives its body from (actor, index), so the first record it writes for
    // OWNER is byte-identical to the corpus's own first record. It dedups into
    // the corpus instead of appending — no rotation, no new segment, and the
    // hazard silently unexercised while the test still reads as if mail had
    // arrived. The arrival has to be mail this scope has genuinely never seen.
    fauna_nest::segments::mail::append_record(
        &dest.mail_segments,
        &dest.db,
        &OWNER,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-body-arrived-after-the-flip".to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-index-hint".to_vec(),
        ),
        common::floor(1_715_000_200_000),
    )
    .await
    .expect("the first ordinary mail after the flip");

    // BY CONTENT, not by name. The reclaim unlinks `seg-00000001` and opens a
    // fresh segment at the same id in the same breath, so a name that is still
    // in the directory afterwards says nothing at all — the corpus can be gone
    // while every filename survives. Only the bytes distinguish "still there"
    // from "replaced by the arrival that destroyed it".
    let now_on_disk = segment_area_contents(&dest);
    for (name, bytes) in &after_flip {
        match now_on_disk.iter().find(|(n, _)| n == name) {
            None => panic!(
                "the append deleted {name}, a committed segment the ceremony \
                 wrote — on disk now: {:?}",
                now_on_disk.iter().map(|(n, _)| n).collect::<Vec<_>>()
            ),
            Some((_, now)) => assert!(
                now == bytes,
                "the append rewrote {name} ({} bytes -> {} bytes) — the \
                 ceremony's segment was reclaimed as an orphan and a new one \
                 opened over it",
                bytes.len(),
                now.len()
            ),
        }
    }

    let served = serve_mail(&dest).await;
    assert_eq!(
        served.len(),
        corpus.len() + 1,
        "the corpus plus the new arrival, not the corpus minus a segment"
    );
    assert_eq!(
        served[..corpus.len()],
        corpus[..],
        "the corpus reads back unchanged once mail has landed on top of it"
    );
}

/// **The empty-target rule.** A second materialize is refused, typed, and the
/// target is left byte-for-byte as the first one left it.
///
/// This is also what makes the kind safe to auto-retry on a reconnect: the
/// refusal fires before a single byte is read, so a replay cannot double-write a
/// segment area — idempotence by refusal rather than by convergence.
#[tokio::test]
async fn materialize_refuses_a_scope_that_already_holds_live_records() {
    let (_source, dest, _d_url) = delivered().await;

    materialize(&dest, "__mail").await.expect("the first flip");
    let served = serve_mail(&dest).await;
    assert!(!served.is_empty());

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("a lived-in scope is not a re-seed target");
    assert_eq!(err.code, "fauna.backup.target_not_empty");
    assert!(
        err.detail_or_code().contains("never merges or deletes"),
        "the refusal says why there is no force arm: {err:?}"
    );
    assert_eq!(
        serve_mail(&dest).await,
        served,
        "a refusal changes nothing — this is the no-user-data-loss half of the rule"
    );
}

/// **A `seg-*` file already on disk is not something to write through** —
/// `message-segment-store.md` § Restore → Re-seed: *"the ceremony deletes
/// nothing anywhere (No user-data loss structurally)"*.
///
/// The empty-target rule reads the SQLite mirror, and DB rows are not disk
/// files. Two ways they diverge, and this test stands in for both:
///
/// 1. **Tombstoned, not gone.** Mail purge is SQL-only — nothing under the
///    nest's segment paths unlinks a `.dat`; that happens later, in the store's
///    own GC. So a scope whose records are all tombstoned counts zero live
///    while its segment files remain.
/// 2. **The window.** The mirror check runs at the top of the verb, a whole
///    corpus fetch-decrypt-verify before the write, with no per-scope lock
///    held. Mail delivered into the scope in that window creates a `seg-*.dat`
///    through `FramedSegment::create`, and on a scope that started empty its id
///    begins at the same base the corpus's own ids do — so a collision is the
///    default case, not an unlucky one.
///
/// The write was `atomic_write` — write-tmp + **rename**, which replaces — so
/// either arm ended in silently destroying records the owner actually had, and
/// left the mirror pointing at bytes that were gone (the rebuild only inserts;
/// the wholesale DELETE lives in `restore_mail`, not here). Every other writer
/// of this directory refuses instead: `FramedSegment::create` uses `create_new`
/// and the store *removes* an orphan rather than writing through it, because a
/// 0-byte `seg-00000040.dat` once permanently 451'd all mail for an actor.
#[tokio::test]
async fn materialize_refuses_to_write_through_a_segment_file_already_on_disk() {
    let (source, dest, _d_url) = delivered().await;

    // Squat on a path the corpus will actually want — asked of the source
    // rather than hard-coded, so this keeps testing a collision even if segment
    // numbering changes.
    let wanted = live_segment_ids(&source, OWNER).await;
    let squatted_id = *wanted
        .first()
        .expect("the corpus holds at least one segment");
    let root = fauna_mail::segments::mail_segments_root(dest.mail_segments.data_dir(), &OWNER);
    std::fs::create_dir_all(&root).unwrap();
    let squatter = root.join(format!("seg-{squatted_id:08}.dat"));
    // Contents that could never pass as a segment: if the verb writes through
    // this, the refusal is the only thing that could have saved it — no later
    // `FramedSegment::open` check can take the credit.
    let bytes_on_disk: &[u8] = b"not a segment, and not the owner's to lose";
    std::fs::write(&squatter, bytes_on_disk).unwrap();

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("a segment area with a file already in it is not a fresh target");
    assert_eq!(err.code, "fauna.backup.segment_path_occupied");

    assert_eq!(
        std::fs::read(&squatter).unwrap(),
        bytes_on_disk,
        "the file that was already there is the whole point — it must be byte-for-byte \
         untouched, because on the real path those bytes are the owner's mail"
    );
    assert_eq!(
        segment_area_files(&dest),
        vec![format!("seg-{squatted_id:08}.dat")],
        "a refusal writes NOTHING — not even the halves whose own paths were free, since \
         those would be orphans the next attempt then refuses on"
    );
    assert!(
        serve_mail(&dest).await.is_empty(),
        "and the mirror is untouched, so nothing points at bytes that are not there"
    );
}

/// : guard 3's delete arm is
/// licensed by a structural belt it does not itself own — a genuinely
/// in-progress (unfinalized) CARv2 segment's header byte 11 is `0x00`, never
/// `0x80` like a finalized one (`fauna_carv2::Writer::new` zero-fills the
/// header placeholder; `finalize()` backfills it), so it can never satisfy
/// guard 3's byte-prefix test against a FINALIZED `intended` segment. That
/// property is true today and lives in `fauna-carv2`, not in this file.
///
/// No existing pin stages the one occupant shape whose deletion would
/// actually destroy real mail: a LIVE OPEN segment for the wanted id. The
/// other pins in this class use garbage bytes (fails the prefix test at byte
/// 0) or a 0-byte orphan (an empty file is trivially a prefix of anything,
/// which is what makes it OUR write, not a foreign one). This test stages the
/// open-segment shape through the REAL `FramedSegment::create` API — so the
/// bytes are exactly what an in-progress delivery leaves, not hand-crafted —
/// and asserts guard 3 refuses rather than deletes it. If `fauna-carv2` ever
/// changes so a fresh segment's header already looks final (or otherwise
/// self-describing) at the compared prefix length, this is the pin that
/// catches the widening — red-verified by temporarily narrowing
/// `classify_occupant`'s comparison to the first 11 (pragma-only) bytes: this
/// test alone reds under that mutation, while the other three pins in this
/// class (garbage content, a 0-byte orphan, a fully-adopted pair) stay green.
#[tokio::test]
async fn materialize_refuses_a_genuinely_open_segment_for_the_same_id() {
    let (source, dest, _d_url) = delivered().await;

    let wanted = live_segment_ids(&source, OWNER).await;
    let collision_id = *wanted
        .first()
        .expect("the corpus holds at least one segment");
    let root = fauna_mail::segments::mail_segments_root(dest.mail_segments.data_dir(), &OWNER);
    std::fs::create_dir_all(&root).unwrap();
    let dat_path = root.join(format!("seg-{collision_id:08}.dat"));

    // A raw `fauna_segment_store::FramedSegment::create`, never through
    // `dest.mail_segments` — that manager instance's `finalize_open` (guard
    // 1) only knows about segments IT itself opened, so a file created this
    // way is invisible to it and stays genuinely open when guard 3 runs,
    // exactly as a concurrent delivery from a different process would leave
    // it. Never finalized: dropped as soon as it is created.
    let header = fauna_segment_store::SegmentHeader {
        kind: KIND.to_string(),
        actor_id: OWNER,
        segment_id: collision_id,
        bucket: "2026-08".to_string(),
        created_at_secs: 1_700_000_000,
        record_count: 0,
    };
    drop(
        fauna_segment_store::FramedSegment::create(&dat_path, header)
            .expect("create a genuinely open segment at the collision path"),
    );
    let staged_bytes = std::fs::read(&dat_path).unwrap();
    assert!(
        !staged_bytes.is_empty(),
        "beside-control: FramedSegment::create must have written the pragma + \
         placeholder header before this test's own assertion means anything"
    );

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("a live open segment for the wanted id is not ours to delete");
    assert_eq!(err.code, "fauna.backup.segment_path_occupied");

    assert_eq!(
        std::fs::read(&dat_path).unwrap(),
        staged_bytes,
        "the open segment must be byte-for-byte untouched — deleting it would \
         destroy an in-progress delivery's bytes, the one occupant shape this \
         row exists to protect"
    );
    assert_eq!(
        segment_area_files(&dest),
        vec![format!("seg-{collision_id:08}.dat")],
        "a refusal writes nothing else — no `.meta` orphan for the halves \
         whose own paths were free"
    );
    assert!(
        serve_mail(&dest).await.is_empty(),
        "and the mirror is untouched"
    );
}

/// **An interrupted half is reclaimed, not a life sentence for the scope.**
///
/// `create_new` stopped materialize writing through what is already on disk,
/// but on its own it traded a rare silent loss for a PERMANENT HARD FAILURE: a
/// crash between a pair's two `write_half` calls leaves a durable half that
/// every later materialize refuses forever. Nothing in the tree removed it. The
/// store's own reclaim does not reach it twice over — keyed on the `.dat`, so a
/// lone `.meta` (written and fsynced FIRST, hence the likeliest residue) is
/// invisible to it; and it fires only inside an append's rotation, which would
/// make the scope non-empty and trip the empty-target rule instead. With no
/// operator role to clear it by hand, that is a client-reachable,
/// client-unrecoverable state — `nest/common.md` § Client-state recoverability
/// calls that a bug, not a deferred feature.
///
/// The residue here is a **0-byte** `.meta` with no `.dat`, which is both the
/// canonical crash shape and the exact example.com 2026-06-13 precedent (a 0-byte
/// `seg-00000040.dat` permanently 451'd all mail for an actor). An empty file
/// is a prefix of any half this run intends to write, which is precisely what
/// makes it provably OUR interrupted write rather than someone's data.
#[tokio::test]
async fn an_interrupted_half_is_reclaimed_rather_than_stranding_the_scope() {
    let (source, dest, _d_url) = delivered().await;

    let wanted = live_segment_ids(&source, OWNER).await;
    let stranded = *wanted
        .first()
        .expect("the corpus holds at least one segment");
    let root = fauna_mail::segments::mail_segments_root(dest.mail_segments.data_dir(), &OWNER);
    std::fs::create_dir_all(&root).unwrap();
    // Meta first, dat second, each fsync'd — so a lone `.meta` is what a crash
    // between the two `write_half` calls actually leaves behind.
    let orphan = root.join(format!("seg-{stranded:08}.meta"));
    std::fs::write(&orphan, b"").unwrap();

    let reply = materialize(&dest, "__mail")
        .await
        .expect("an interrupted half is this ceremony's own residue to clear");
    assert_eq!(reply.segments, wanted, "the whole corpus still lands");
    assert_eq!(
        serve_mail(&dest).await,
        serve_mail(&source).await,
        "and the owner gets their mail back — the point of the ceremony"
    );
}

/// **A crash between adopting the ids and committing the mirror RESUMES.**
///
/// The second window in the same refusal class, opened by the adopt step
/// itself: the halves are on disk AND adopted into the kind manifest, but the
/// `segment_records` rebuild never committed. A retry passes the empty-target
/// rule (the mirror holds zero live records, truthfully) and then meets its own
/// fully-written pairs. Deleting them is not an option — an adopted segment is
/// committed, and this ceremony deletes nothing — so the only correct move is
/// to resume: keep the bytes, skip the rewrite, and finish the mirror.
///
/// Staged by dropping the mirror rows a successful run wrote, which leaves
/// exactly the on-disk and manifest state that crash produces.
#[tokio::test]
async fn a_crash_between_adopting_and_the_mirror_rebuild_resumes_on_retry() {
    let (source, dest, _d_url) = delivered().await;
    let corpus = serve_mail(&source).await;

    materialize(&dest, "__mail")
        .await
        .expect("the first flip lands the halves and adopts their ids");
    let on_disk = segment_area_contents(&dest);
    assert!(!on_disk.is_empty(), "the flip left segment files behind");

    // The crash: the rebuild transaction never committed.
    {
        let conn = dest.db.conn().await;
        conn.execute(
            "DELETE FROM segment_records WHERE scope_id = ?1 AND kind = 'mail'",
            rusqlite::params![OWNER.as_slice()],
        )
        .expect("drop the mirror rows the interrupted rebuild never committed");
    }
    assert!(
        serve_mail(&dest).await.is_empty(),
        "the staged state is the real one: bytes on disk, adopted, serving nothing"
    );

    materialize(&dest, "__mail")
        .await
        .expect("a half-finished run of this ceremony resumes rather than refusing forever");

    assert_eq!(
        serve_mail(&dest).await,
        corpus,
        "the retry finishes the job the crash interrupted"
    );
    assert_eq!(
        segment_area_contents(&dest),
        on_disk,
        "and it did it WITHOUT rewriting the adopted halves — an adopted segment is \
         committed, and this ceremony deletes nothing"
    );
}

/// **A destination that has not caught up is refused, not half-restored.**
///
/// Staged as the crash window between the two custody records leaves it: the
/// `.dat` custody is live and the `.meta` custody is absent (tombstoned through the production owner-authed
/// door, which is what an absence of custody is). Reconstituting the `.dat`
/// alone would write a file `FramedSegment::open` refuses — the record footers
/// live nowhere but the sidecar — so the verb must say so and stop.
#[tokio::test]
async fn materialize_refuses_a_segment_whose_sidecar_custody_is_missing() {
    let (source, dest, _d_url) = delivered().await;

    let scope_hex = hex::encode(OWNER);
    let device: [u8; 32] = [0x0B; 32];
    dest.db
        .register_sync_device(&OWNER, &device, "staging-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&dest.db, &OWNER).await;
    for seg_id in live_segment_ids(&source, OWNER).await {
        let _: fauna_protocol::sync::SyncChangeRecordReply = client_call(
            &dest,
            OWNER,
            "fauna.sync.changes.record",
            &fauna_protocol::sync::SyncChangeRecordRequest {
                folder: "__mail".to_string(),
                device_id: hex::encode(device),
                path: segment_meta_rel_path(&scope_hex, seg_id),
                manifest_hash: None,
                size_bytes: 0,
                change_type: "delete".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("tombstone the sidecar custody to stage a not-yet-caught-up destination");
    }

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("half a segment is not a segment");
    assert_eq!(err.code, "fauna.backup.custody_incomplete");
    assert!(
        err.detail_or_code().contains(".meta"),
        "the refusal names the half that is missing: {err:?}"
    );
    assert!(
        serve_mail(&dest).await.is_empty(),
        "a refused materialize writes nothing into the scope"
    );
}

/// **The blake3 anchor.** Custody that does not open to the bytes the source's
/// own mirror advertised is refused, and the target stays untouched.
///
/// Staged as a *substitution*, not a truncation: one segment's `.dat` custody is
/// re-recorded — through the production owner-authed door — to point at the
/// OTHER segment's manifest. Every byte is real, sealed under the right key, and
/// opens perfectly; only the mirror's per-segment hash disagrees. That is
/// precisely the check `message-segment-store.md` § … → *Restore* pins the
/// reconstitution on, and nothing else in the path would catch it.
#[tokio::test]
async fn materialize_refuses_a_segment_substituted_for_another() {
    let (source, dest, _d_url) = delivered().await;

    let scope_hex = hex::encode(OWNER);
    let seg_ids = live_segment_ids(&source, OWNER).await;
    assert!(
        seg_ids.len() >= 2 || seg_ids.len() == 1,
        "need at least one segment to substitute against"
    );

    // The manifest hash currently held at the mirror path — real custody, real
    // chunks, sealed under the granted key, and emphatically not segment 1's.
    let rows = dest.db.list_backup_custody(&OWNER, None, 0).await.unwrap();
    let mirror_path = fauna_sync_engine::segment_backup::manifest_rel_path(&scope_hex, KIND);
    let other_manifest = rows
        .iter()
        .find(|r| r.path.as_deref() == Some(mirror_path.as_str()))
        .map(|r| hex::encode(&r.manifest_hash))
        .expect("the mirror is in custody");

    let device: [u8; 32] = [0x0C; 32];
    dest.db
        .register_sync_device(&OWNER, &device, "substituting-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&dest.db, &OWNER).await;
    let victim = seg_ids[0];
    let _: fauna_protocol::sync::SyncChangeRecordReply = client_call(
        &dest,
        OWNER,
        "fauna.sync.changes.record",
        &fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__mail".to_string(),
            device_id: hex::encode(device),
            path: segment_rel_path(&scope_hex, victim),
            manifest_hash: Some(other_manifest),
            size_bytes: 1,
            change_type: "upsert".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("re-record the .dat path at another file's manifest");

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("bytes that are not the segment the mirror named");
    assert_eq!(err.code, "fauna.backup.custody_unreadable");
    assert!(
        serve_mail(&dest).await.is_empty(),
        "a refused materialize writes nothing into the scope"
    );
    assert!(
        !dest
            .db
            .list_backup_custody(&OWNER, None, 0)
            .await
            .unwrap()
            .is_empty(),
        "and it deletes no custody either — the owner can re-push and retry"
    );
}

/// **The blake3 anchor, on its own.** One segment's `.dat` custody is swapped
/// for ANOTHER REAL SEGMENT'S, and only the mirror's per-segment hash can tell.
///
/// [`materialize_refuses_a_segment_substituted_for_another`] above swaps in a
/// non-segment (the mirror blob), which `FramedSegment::open` would reject on its
/// own — so that test does not actually prove the anchor. A mutation round said
/// so out loud: deleting the `.dat` hash check left every test green. This is the
/// case that has no other witness. Both files here are genuine segments of this
/// owner's, sealed under the granted key, and each opens perfectly; the target
/// would end up serving segment B's records filed under segment A's id, from a
/// segment area that reads as entirely healthy.
///
/// Two segments exist because the store buckets by calendar month
/// (`bucket_for`), so mail two months apart lands in two files — the same reason
/// a real corpus has more than one.
#[tokio::test]
async fn materialize_refuses_a_segment_swapped_for_a_different_valid_segment() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    // Two segment FILES, not two records in one file: the writer buckets by the
    // record's own `received_at` (`bucket_for`), so stamps two calendar months
    // apart — 2024-05 and 2024-07 — roll the open segment between them. Both
    // stamps go through ONE `append_mail` call: it derives each body from the
    // call-local index, so a second call would re-emit body 0 verbatim and the
    // scoped pre-append dedup would drop it (which is exactly what left this
    // test with one segment the first time).
    append_mail(&source, &OWNER, &[1_715_000_000_000, 1_720_200_000_000]).await;
    enroll(&source, &dest, &d_url).await;
    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .unwrap();
    grant_key_to(&dest).await;

    let scope_hex = hex::encode(OWNER);
    let seg_ids = live_segment_ids(&source, OWNER).await;
    assert_eq!(
        seg_ids.len(),
        2,
        "this test needs two real segments to swap: {seg_ids:?}"
    );

    let rows = dest.db.list_backup_custody(&OWNER, None, 0).await.unwrap();
    let manifest_at = |path: &str| {
        rows.iter()
            .find(|r| r.path.as_deref() == Some(path))
            .map(|r| hex::encode(&r.manifest_hash))
            .unwrap_or_else(|| panic!("no custody at {path}"))
    };
    // The victim is the LAST segment on purpose. Under a "write each pair as it
    // verifies" implementation the earlier segment would already be on disk when
    // this one fails — which is exactly the state `segment_area_files` below
    // asserts against, and the reason the verification loop is separate from the
    // write loop at all.
    let a = segment_rel_path(&scope_hex, seg_ids[0]);
    let b = segment_rel_path(&scope_hex, seg_ids[1]);
    let a_manifest = manifest_at(&a);

    // Point A's `.dat` custody at B's bytes. Real segment, real seal, real
    // chunks — only the mirror disagrees about which one belongs here.
    let device: [u8; 32] = [0x0D; 32];
    dest.db
        .register_sync_device(&OWNER, &device, "swapping-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&dest.db, &OWNER).await;
    let _: fauna_protocol::sync::SyncChangeRecordReply = client_call(
        &dest,
        OWNER,
        "fauna.sync.changes.record",
        &fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__mail".to_string(),
            device_id: hex::encode(device),
            path: b,
            manifest_hash: Some(a_manifest),
            size_bytes: 1,
            change_type: "upsert".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("swap the last segment's custody for the first segment's");

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("a real segment in the wrong slot is still the wrong segment");
    assert_eq!(err.code, "fauna.backup.custody_unreadable");
    assert!(
        err.detail_or_code().contains("mirror advertised"),
        "the refusal names the anchor that caught it: {err:?}"
    );
    assert!(
        serve_mail(&dest).await.is_empty(),
        "and the target is untouched — the verification runs before any write"
    );
    assert!(
        segment_area_files(&dest).is_empty(),
        "not one segment file landed: every pair is verified before ANY is \
         written, so a failure on the last leaves the target as untouched as one \
         on the first — got {:?}",
        segment_area_files(&dest)
    );
}

/// **The sidecar's own anchor.** The `.meta` half is verified against
/// `SegmentRef.meta_blake3_hex` too — swap in another segment's real sidecar and
/// nothing else notices.
///
/// The sibling refusal above ([`materialize_refuses_a_segment_whose_sidecar_custody_is_missing`])
/// covers an ABSENT sidecar, which is caught by the custody lookup long before
/// any hash is compared. This is the present-but-wrong case, and it is the one
/// with real consequences: the `.dat` decides which bytes a record holds, but the
/// sidecar decides which records exist, what order they are in, and what each
/// one's floor says. A mismatched sidecar of the same record count opens without
/// complaint and yields a segment whose every mirror row is a lie.
#[tokio::test]
async fn materialize_refuses_a_sidecar_swapped_for_a_different_valid_one() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000, 1_720_200_000_000]).await;
    enroll(&source, &dest, &d_url).await;
    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .unwrap();
    grant_key_to(&dest).await;

    let scope_hex = hex::encode(OWNER);
    let seg_ids = live_segment_ids(&source, OWNER).await;
    assert_eq!(seg_ids.len(), 2, "need two sidecars to swap: {seg_ids:?}");

    let rows = dest.db.list_backup_custody(&OWNER, None, 0).await.unwrap();
    let manifest_at = |path: &str| {
        rows.iter()
            .find(|r| r.path.as_deref() == Some(path))
            .map(|r| hex::encode(&r.manifest_hash))
            .unwrap_or_else(|| panic!("no custody at {path}"))
    };
    let first_meta = segment_meta_rel_path(&scope_hex, seg_ids[0]);
    let last_meta = segment_meta_rel_path(&scope_hex, seg_ids[1]);
    let first_meta_manifest = manifest_at(&first_meta);

    let device: [u8; 32] = [0x0E; 32];
    dest.db
        .register_sync_device(&OWNER, &device, "sidecar-swapping-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&dest.db, &OWNER).await;
    let _: fauna_protocol::sync::SyncChangeRecordReply = client_call(
        &dest,
        OWNER,
        "fauna.sync.changes.record",
        &fauna_protocol::sync::SyncChangeRecordRequest {
            folder: "__mail".to_string(),
            device_id: hex::encode(device),
            path: last_meta,
            manifest_hash: Some(first_meta_manifest),
            size_bytes: 1,
            change_type: "upsert".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("swap the last segment's sidecar for the first segment's");

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("the wrong sidecar describes the wrong records");
    assert_eq!(err.code, "fauna.backup.custody_unreadable");
    assert!(
        segment_area_files(&dest).is_empty(),
        "and nothing was written: {:?}",
        segment_area_files(&dest)
    );
}

/// **No grant, no unseal — and the refusal says so.**
///
/// The ratified ceremony grants the key at enrollment, so this is the honest
/// answer for a caller who reached phase 3 without phase 1, not a state the flow
/// produces. It matters because the alternative — an opaque decrypt failure —
/// reads to the owner as a corrupt backup rather than a missing step.
#[tokio::test]
async fn materialize_refuses_without_the_owners_key_grant() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_mail(&source, &OWNER, &[1_715_000_000_000]).await;
    enroll(&source, &dest, &d_url).await;
    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .unwrap();
    // Deliberately no `grant_key_to(&dest)`.

    let err = materialize(&dest, "__mail")
        .await
        .expect_err("nothing to open the custody with");
    assert_eq!(err.code, "fauna.backup.not_enrolled");
    assert!(serve_mail(&dest).await.is_empty());
}

/// `expect_err` with a caller-supplied message — `Result`'s own `expect_err`
/// needs `T: Debug`, which `CustodyMaterializeReply` has, but the message it
/// prints does not say *which* set name was the one that wrongly succeeded.
trait UnwrapErrOrPanic<E> {
    fn unwrap_err_or_panic(self, what: &str) -> E;
}

impl<T: std::fmt::Debug, E> UnwrapErrOrPanic<E> for Result<T, E> {
    fn unwrap_err_or_panic(self, what: &str) -> E {
        match self {
            Err(e) => e,
            Ok(v) => panic!("{what}, but it succeeded with {v:?}"),
        }
    }
}

/// **An arm that is not built refuses; it does not half-run.**
///
/// The covered-folder axis (`__folder/<source-nest>/<id>`) is a real custody
/// shape this nest really holds — its materialize arm is simply not built yet.
/// The failure mode worth pinning is the verb quietly treating it as a segment
/// set and reconstituting a segment area for a folder mirror.
#[tokio::test]
async fn materialize_refuses_a_set_name_it_cannot_reconstitute() {
    let (_source, dest, _d_url) = delivered().await;

    // A folder-axis mirror and an unrecognised name are not segment sets at all;
    // `__conv/<hex>` IS one, of a kind whose reconstitution is unbuilt. Both
    // refuse — the distinction they carry is for the client's message, not for
    // whether anything runs.
    // Each of these must refuse as INVALID PARAMS specifically — not merely
    // refuse. `__conv/<hex>` is the one that makes the assertion load-bearing:
    // it IS a well-formed segment set, of a kind with no reconstitution path, so
    // a nest that dropped the kind gate would sail past it and refuse later for
    // an unrelated reason (its custody set is empty, so the mirror is missing).
    // Same red light, wrong bulb — and the next kind added to the sweep would
    // then be materialized into the MAIL segment area.
    for name in [
        "__folder/aa/7".to_string(),
        format!("__conv/{}", "cd".repeat(32)),
        "not-a-reserved-set".to_string(),
        "__mail-ish".to_string(),
    ] {
        let err = materialize(&dest, &name)
            .await
            .unwrap_err_or_panic(&format!("`{name}` must refuse"));
        assert_eq!(
            err.code, "fauna.backup.invalid_params",
            "`{name}` refused, but for the wrong reason: {err:?}"
        );
    }
    assert!(
        serve_mail(&dest).await.is_empty(),
        "no refused name wrote anything into the scope"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Every message kind comes back — posts, calendar and contacts beside mail
// (`nest/box-recovery.md` § Goal item 2; `recover-a-lost-nest` outcome 4).
//
// The materialize verb reconstitutes each kind through the snapshot restore's
// own row rebuild for that kind. Calendar and contacts carry their placement
// journals in the set, as mail does; posts have no placement layer. The three
// are in the `BACKED_UP_KINDS` sweep beside mail; each test drives the
// coordinator's own per-kind pass (`NestBackupCoordinator::run_once`, the mover
// the sweep runs for every kind it lists) from a source nest that wrote the
// kind through its own doors, then materializes the set on the destination and
// reads the rows and bodies back off the rebuilt nest.
// ═════════════════════════════════════════════════════════════════════════════

const CALENDAR: [u8; 32] = [0xCA; 32];
const ADDRESSBOOK: [u8; 32] = [0xAB; 32];

/// Seal `plaintext` as every production calendar/card writer does — the only
/// currency the DAV put doors accept (S6.12's structural seal).
fn sealed(plaintext: &[u8]) -> Vec<u8> {
    use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, seal_to_recipient};
    let (_secret, pubkey) = derive_recipient_hpke_keypair(&[0x5E; 32]);
    seal_to_recipient(plaintext, &pubkey)
        .expect("seal a fixture")
        .to_canonical_bytes()
        .expect("canonical sealed bytes")
}

/// Provision `OWNER`'s calendar and put one event per uid, through the
/// production `fauna.bridges.*` doors a calendar app reaches.
async fn put_events(nest: &Arc<AppState>, uids: &[u8]) {
    use fauna_protocol::bridge_routing::{
        ProvisionCalendarReply, ProvisionCalendarRequest, PutEventCiphertextReply,
        PutEventCiphertextRequest,
    };
    let _: ProvisionCalendarReply = client_call(
        nest,
        OWNER,
        "fauna.bridges.provision_calendar",
        &ProvisionCalendarRequest {
            actor_id: OWNER.to_vec(),
            calendar_id: CALENDAR.to_vec(),
            encrypted_metadata: sealed(b"Personal"),
            ..Default::default()
        },
    )
    .await
    .expect("provision the calendar");
    for uid in uids {
        let body = sealed(format!("BEGIN:VEVENT uid-{uid}").as_bytes());
        let _: PutEventCiphertextReply = client_call(
            nest,
            OWNER,
            "fauna.bridges.put_event_ciphertext",
            &PutEventCiphertextRequest {
                actor_id: OWNER.to_vec(),
                calendar_id: CALENDAR.to_vec(),
                uid_hash: vec![*uid; 32],
                ciphertext_size: body.len() as u32,
                encrypted_body: body,
                encrypted_index_hint: sealed(format!("hint-{uid}").as_bytes()),
                timestamp: 1_752_000_000 + i64::from(*uid),
                ..Default::default()
            },
        )
        .await
        .expect("put an event");
    }
}

/// Delete `OWNER`'s event `uid` through the production door.
async fn delete_event(nest: &Arc<AppState>, uid: u8) {
    use fauna_protocol::bridge_routing::{DeleteEventReply, DeleteEventRequest};
    let _: DeleteEventReply = client_call(
        nest,
        OWNER,
        "fauna.bridges.delete_event",
        &DeleteEventRequest {
            actor_id: OWNER.to_vec(),
            calendar_id: CALENDAR.to_vec(),
            uid_hash: vec![uid; 32],
            if_match: None,
        },
    )
    .await
    .expect("delete an event");
}

/// Provision `OWNER`'s addressbook and put one card per uid.
async fn put_cards(nest: &Arc<AppState>, uids: &[u8]) {
    use fauna_protocol::bridge_routing::{
        ProvisionAddressbookReply, ProvisionAddressbookRequest, PutCardCiphertextReply,
        PutCardCiphertextRequest,
    };
    let _: ProvisionAddressbookReply = client_call(
        nest,
        OWNER,
        "fauna.bridges.provision_addressbook",
        &ProvisionAddressbookRequest {
            actor_id: OWNER.to_vec(),
            addressbook_id: ADDRESSBOOK.to_vec(),
            encrypted_metadata: sealed(b"Contacts"),
            ..Default::default()
        },
    )
    .await
    .expect("provision the addressbook");
    for uid in uids {
        let body = sealed(format!("BEGIN:VCARD uid-{uid}").as_bytes());
        let _: PutCardCiphertextReply = client_call(
            nest,
            OWNER,
            "fauna.bridges.put_card_ciphertext",
            &PutCardCiphertextRequest {
                actor_id: OWNER.to_vec(),
                addressbook_id: ADDRESSBOOK.to_vec(),
                uid_hash: vec![*uid; 32],
                ciphertext_size: body.len() as u32,
                encrypted_body: body,
                encrypted_index_hint: sealed(format!("hint-{uid}").as_bytes()),
                timestamp: 1_752_000_000 + i64::from(*uid),
                ..Default::default()
            },
        )
        .await
        .expect("put a card");
    }
}

/// One served DAV item: `(item id, uid_hash, etag, modseq, sealed body)`.
type DavItem = (Vec<u8>, Vec<u8>, String, i64, Vec<u8>);

/// Every event a nest serves for `OWNER`: the `bridge_caldav_events` row joined
/// to its body in the segment store, by the row's stored `record_cid` — the
/// join every calendar read runs, so a row with no body behind it fails here.
/// Plus the calendar's `highestmodseq`, which a CalDAV client's sync token is
/// measured against.
async fn served_events(nest: &Arc<AppState>) -> (Vec<DavItem>, Option<i64>) {
    let rows: Vec<DavItem> = {
        let conn = nest.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT event_id, uid_hash, etag, modseq, record_cid FROM bridge_caldav_events \
                 WHERE actor_id = ?1 ORDER BY event_id",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![OWNER.as_slice()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    let hms: Option<i64> = {
        let conn = nest.db.conn().await;
        conn.query_row(
            "SELECT highestmodseq FROM bridge_caldav_calendars WHERE actor_id = ?1 AND calendar_id = ?2",
            rusqlite::params![OWNER.as_slice(), CALENDAR.as_slice()],
            |r| r.get(0),
        )
        .ok()
    };
    let mut out = Vec::with_capacity(rows.len());
    for (id, uid, etag, modseq, cid) in rows {
        let cid = fauna_cbor::Cid::from_bytes(cid.as_slice().try_into().unwrap()).unwrap();
        let (envelope, _floor) =
            fauna_nest::segments::cal::read_record(&nest.cal_segments, &nest.db, &OWNER, &cid)
                .await
                .expect("read the event's record")
                .expect("the row's body is live in the segment store");
        out.push((id, uid, etag, modseq, envelope.encrypted_body));
    }
    (out, hms)
}

/// [`served_events`]' card twin.
async fn served_cards(nest: &Arc<AppState>) -> (Vec<DavItem>, Option<i64>) {
    let rows: Vec<DavItem> = {
        let conn = nest.db.conn().await;
        let mut stmt = conn
            .prepare(
                "SELECT card_id, uid_hash, etag, modseq, record_cid FROM bridge_carddav_cards \
                 WHERE actor_id = ?1 ORDER BY card_id",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![OWNER.as_slice()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    };
    let hms: Option<i64> = {
        let conn = nest.db.conn().await;
        conn.query_row(
            "SELECT highestmodseq FROM bridge_carddav_addressbooks WHERE actor_id = ?1 AND addressbook_id = ?2",
            rusqlite::params![OWNER.as_slice(), ADDRESSBOOK.as_slice()],
            |r| r.get(0),
        )
        .ok()
    };
    let mut out = Vec::with_capacity(rows.len());
    for (id, uid, etag, modseq, cid) in rows {
        let cid = fauna_cbor::Cid::from_bytes(cid.as_slice().try_into().unwrap()).unwrap();
        let (envelope, _floor) =
            fauna_nest::segments::card::read_record(&nest.card_segments, &nest.db, &OWNER, &cid)
                .await
                .expect("read the card's record")
                .expect("the row's body is live in the segment store");
        out.push((id, uid, etag, modseq, envelope.encrypted_body));
    }
    (out, hms)
}

/// Run the coordinator's own pass for `kind` — both families, content then
/// journal — from `source` to its one enrolled destination.
async fn back_up_kind(source: &Arc<AppState>, kind: &str) {
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(source), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    coordinator
        .run_once(&dest_row, kind)
        .await
        .unwrap_or_else(|e| panic!("the {kind} pass runs: {e:#}"));
}

/// A source that wrote `seed`, with `kind` backed up to a fresh destination
/// holding the owner's key grant — phase 2's end state, for any kind. Returns
/// `(source, dest)`.
async fn delivered_with<F, Fut>(kind: &str, seed: F) -> (Arc<AppState>, Arc<AppState>)
where
    F: FnOnce(Arc<AppState>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    seed(Arc::clone(&source)).await;
    enroll(&source, &dest, &d_url).await;
    back_up_kind(&source, kind).await;
    grant_key_to(&dest).await;
    (source, dest)
}

/// **The owner's calendar comes back after a box loss** — every event, its
/// ETag, its change number and its sealed body, in a calendar carrying the
/// source's `highestmodseq`, with the deletion the source recorded still a
/// deletion.
///
/// The journal rode the set and was adopted: the rebuilt nest's calendar
/// placement fold IS the source's, which is what the next write's change
/// numbers continue from.
#[tokio::test]
async fn the_calendar_is_backed_up_and_comes_back_where_it_was() {
    let (source, dest) = delivered_with("calendar", |s| async move {
        put_events(&s, &[1, 2, 3]).await;
        delete_event(&s, 2).await;
    })
    .await;

    let custody = custody_paths(&dest).await;
    assert!(
        custody.iter().any(|p| p.contains("/placement/")),
        "the calendar's journal rode its set: {custody:?}"
    );
    assert_eq!(
        served_events(&dest).await,
        (Vec::new(), None),
        "pre-state: the destination serves no calendar"
    );

    let reply = materialize(&dest, "__calendar")
        .await
        .expect("the owner materializes their calendar");
    assert_eq!(reply.placements, Some(2), "two events are placed");

    let was = served_events(&source).await;
    assert_eq!(was.0.len(), 2, "the source really holds two events");
    assert_eq!(
        served_events(&dest).await,
        was,
        "same events, same ETags, same change numbers, same bodies, same highestmodseq"
    );
    let restored = dest.cal_placement.current_manifest(&OWNER).await.unwrap();
    let original = source.cal_placement.current_manifest(&OWNER).await.unwrap();
    assert_eq!(restored.events, original.events);
    assert_eq!(restored.calendars, original.calendars);
    assert_eq!(
        restored.tombstones, original.tombstones,
        "the deletion is still a deletion"
    );
    let expunged: i64 = {
        let conn = dest.db.conn().await;
        conn.query_row(
            "SELECT COUNT(*) FROM bridge_caldav_expunged WHERE actor_id = ?1",
            rusqlite::params![OWNER.as_slice()],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(expunged, 1, "WebDAV-Sync still reports the deletion");
}

/// **The owner's contacts come back after a box loss** — the calendar test's
/// twin over the addressbook.
#[tokio::test]
async fn the_contacts_are_backed_up_and_come_back_where_they_were() {
    let (source, dest) = delivered_with("card", |s| async move {
        put_cards(&s, &[7, 8]).await;
    })
    .await;

    let reply = materialize(&dest, "__card")
        .await
        .expect("the owner materializes their contacts");
    assert_eq!(reply.placements, Some(2));

    let was = served_cards(&source).await;
    assert_eq!(was.0.len(), 2);
    assert_eq!(
        served_cards(&dest).await,
        was,
        "same cards, same ETags, same change numbers, same bodies, same highestmodseq"
    );
    let restored = dest.card_placement.current_manifest(&OWNER).await.unwrap();
    let original = source
        .card_placement
        .current_manifest(&OWNER)
        .await
        .unwrap();
    assert_eq!(restored.cards, original.cards);
    assert_eq!(restored.addressbooks, original.addressbooks);
}

/// **The owner's posts come back after a box loss** — each body readable by
/// its post id through the ordinary read, and listed again in the feed
/// projection. Posts have no placement layer, so the reply counts none.
#[tokio::test]
async fn the_posts_are_backed_up_and_come_back() {
    let author = fauna_core::identity::ActorKeypair::generate();
    let posts: Vec<(Vec<u8>, [u8; 32])> = ["first post", "second post"]
        .iter()
        .map(|t| common::signed_post_wire(&author, t))
        .collect();
    let seeded = posts.clone();
    let (source, dest) = delivered_with("post", |s| async move {
        // The owner's own post scope: the body appended to `__post` and the
        // feed projection written beside it — what `segments::post::store_post`
        // does for a post whose author is this scope.
        for (i, (body, post_id)) in seeded.iter().enumerate() {
            fauna_nest::segments::post::append_body(
                &s.post_segments,
                &s.db,
                &OWNER,
                body,
                1_715_000_000_000 + i as i64,
            )
            .await
            .expect("append a post body");
            s.db.put_post_index_only(post_id, body)
                .await
                .expect("write the post's feed projection");
        }
    })
    .await;

    for (_, post_id) in &posts {
        assert!(
            fauna_nest::segments::post::read_body_by_post_id(
                &dest.post_segments,
                &dest.db,
                post_id
            )
            .await
            .unwrap()
            .is_none(),
            "pre-state: the destination serves no post"
        );
    }

    let reply = materialize(&dest, "__post")
        .await
        .expect("the owner materializes their posts");
    assert_eq!(reply.records, posts.len() as u64);
    assert_eq!(reply.placements, None, "posts have no placement layer");

    for (body, post_id) in &posts {
        assert_eq!(
            fauna_nest::segments::post::read_body_by_post_id(
                &dest.post_segments,
                &dest.db,
                post_id
            )
            .await
            .unwrap()
            .as_ref(),
            Some(body),
            "the post body reads back by its id"
        );
        assert!(
            dest.db.content_schema(post_id).await.unwrap().is_some(),
            "and the feed projection lists it again"
        );
    }
    let _ = source;
}

/// **The journal half of the empty-target rule binds calendars too.**
///
/// This target holds no live calendar record and no event row, but its
/// calendar has held an event — put, then deleted — so its journal carries a
/// tombstone and spent change numbers. A CalDAV client that synced it holds a
/// token a restore reusing those numbers would silently satisfy. Refused as
/// `target_not_empty`, with nothing of the corpus written.
#[tokio::test]
async fn materialize_refuses_a_calendar_that_has_held_events() {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    put_events(&source, &[1]).await;
    // The target lived before it became a destination.
    put_events(&dest, &[9]).await;
    delete_event(&dest, 9).await;
    enroll(&source, &dest, &d_url).await;
    back_up_kind(&source, "calendar").await;
    grant_key_to(&dest).await;

    let before = dest.cal_placement.current_manifest(&OWNER).await.unwrap();
    let err = materialize(&dest, "__calendar")
        .await
        .unwrap_err_or_panic("a lived-in calendar must refuse");
    assert_eq!(err.code, "fauna.backup.target_not_empty", "{err:?}");
    assert!(served_events(&dest).await.0.is_empty(), "no event written");
    assert_eq!(
        dest.cal_placement.current_manifest(&OWNER).await.unwrap(),
        before,
        "the target's own journal is untouched"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Phase 3, the COVERED-FOLDER arm — `fauna.backup.custody.materialize` over a
// `__folder/<source-hex>/<id>` mirror.
//
// The segment arm above holds a key and moves bytes. This one holds no key and
// moves none: a covered folder's mirror IS the source's at-rest ciphertext, so
// materializing it is **pure row re-homing** from the triple custody carries —
// `(path_hash, path_sealed, manifest_hash)` — with the folder's display name
// riding the verb payload, because custody deliberately never carried a label
// (`message-segment-store.md` § Client-device custodian (pull) → *Restore*).
//
// The contract these pin:
//
//   - a delivered covered folder becomes a live folder whose per-path rows carry
//     the SOURCE's path hash and the SOURCE's sealed name, pointing at the
//     mirrored manifest — the owner's client then lists and opens them as it
//     would any folder it had always had;
//   - the empty-target rule binds this axis identically: a folder already
//     holding live records this ceremony did not write refuses, typed, with no
//     force arm, and nothing is deleted;
//   - a custody row with no sealed name refuses by its OWN code, rather than
//     failing deeper at `record_change_core`'s `path_seal_required`;
//   - a torn run resumes instead of locking the owner out forever.
//
// They run over the same real delivery leg the mirror-axis tests above use — the
// verb cannot tell a source nest's coordinator from a custodian's push, which is
// the ceremony's whole uniformity payoff.
// ═════════════════════════════════════════════════════════════════════════════

/// Deliver covered-folder files from a fresh source nest to a fresh destination,
/// and hand back `(dest, folder_set, [(path_hash, manifest_hash)])` — the
/// destination in DESTINATION posture, exactly as phase 2 leaves it.
async fn deliver_covered_folder_files(
    files: u8,
) -> (Arc<AppState>, String, Vec<([u8; 32], ContentHash)>) {
    let (_s_url, source, _s_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");
    let mut seeded = Vec::new();
    for i in 0..files {
        let (path_hash, manifest_hash, _keys) =
            seed_folder_file(&source, folder_id, &format!("photos/{i}.jpg"), 0x41 + i).await;
        seeded.push((path_hash, manifest_hash));
    }

    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &source,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id,
            extra: Default::default(),
        },
    )
    .await
    .expect("owner attaches the folder");

    NestBackupWorker::new(Arc::clone(&source), std::time::Duration::MAX)
        .run_once()
        .await
        .expect("the hosting sweep delivers the covered folder");

    (dest, attach.folder_set, seeded)
}

/// [`deliver_covered_folder_files`] with one file: `(dest, folder_set,
/// path_hash, manifest_hash)`.
async fn deliver_a_covered_folder() -> (Arc<AppState>, String, [u8; 32], ContentHash) {
    let (dest, set, seeded) = deliver_covered_folder_files(1).await;
    (dest, set, seeded[0].0, seeded[0].1)
}

/// The target set's nonce in these tests.
const TARGET_NONCE: [u8; 32] = [0x6E; 32];

/// What the ceremony's seed-holding process does before materializing
/// (`writer-signed-change-records.md` ruling (7)(a)(i)): the owner's live set
/// under the display name, with its nonce stored — the nest never mints it.
async fn prepare_target(nest: &Arc<AppState>, name: &str) -> i64 {
    nest.db
        .create_folder_with_options(
            name,
            &OWNER,
            fauna_nest::db::FolderOptions {
                set_nonce: Some(TARGET_NONCE.to_vec()),
                ..Default::default()
            },
        )
        .await
        .expect("prepare the target set")
}

/// The manifest `nest` holds under `manifest_hash`, decoded as the arm reads it.
async fn held_manifest(
    nest: &Arc<AppState>,
    manifest_hash: &[u8],
) -> fauna_core::chunk::ChunkManifest {
    let svc = nest.backup_service.as_ref().expect("blob store configured");
    let digest: [u8; 32] = manifest_hash.try_into().unwrap();
    let raw = svc
        .local_blob_store()
        .get(&ContentHash::from_digest_raw(digest))
        .await
        .unwrap()
        .expect("the manifest is held");
    let bytes = fauna_nest::backup::decode_blob(&raw, svc.encryption_key()).unwrap();
    fauna_core::encoding::canonical_decode(&bytes).unwrap()
}

/// The owner's re-home signature over every custody row of `set`, under
/// `nonce` — what the delivery leg signs, rebuilt here from custody as the
/// destination stores it.
async fn sign_custody(
    nest: &Arc<AppState>,
    set: &str,
    nonce: [u8; 32],
) -> Vec<fauna_protocol::backup::RehomeSignature> {
    let key = owner_key();
    let mut out = Vec::new();
    for row in nest
        .db
        .list_backup_custody_in_set(&OWNER, set)
        .await
        .unwrap()
    {
        let path_hash = fauna_core::hex32::decode(row.path.as_deref().unwrap()).unwrap();
        // The statement's size is the held manifest's `total_size`, never the
        // custody row's derived charge (ruling (7)(a)(ii)).
        let manifest = held_manifest(nest, &row.manifest_hash).await;
        let statement = fauna_protocol::sync_writer_sig::SignedChange::for_rehome(
            nonce,
            OWNER,
            path_hash,
            row.manifest_hash.as_slice().try_into().unwrap(),
            manifest.total_size as i64,
            row.path_sealed.as_deref().unwrap(),
        );
        out.push(fauna_protocol::backup::RehomeSignature {
            path_hash: serde_bytes::ByteBuf::from(path_hash.to_vec()),
            signature: serde_bytes::ByteBuf::from(statement.sign(key.signing_key()).to_vec()),
            ..Default::default()
        });
    }
    out.sort_by(|a, b| a.path_hash.cmp(&b.path_hash));
    out
}

/// An unsigned folder materialize — what the arm refuses `signature_required`.
async fn materialize_folder(
    nest: &Arc<AppState>,
    set_name: &str,
    display_name: Option<&str>,
) -> Result<CustodyMaterializeReply, RpcError> {
    materialize_signed(nest, set_name, display_name, Vec::new()).await
}

/// One page of the owner-signed folder materialize.
async fn materialize_signed(
    nest: &Arc<AppState>,
    set_name: &str,
    display_name: Option<&str>,
    signatures: Vec<fauna_protocol::backup::RehomeSignature>,
) -> Result<CustodyMaterializeReply, RpcError> {
    client_call(
        nest,
        OWNER,
        "fauna.backup.custody.materialize",
        &CustodyMaterializeRequest {
            set_name: set_name.to_string(),
            folder_display_name: display_name.map(str::to_string),
            signer_key: (!signatures.is_empty())
                .then(|| serde_bytes::ByteBuf::from(OWNER.to_vec())),
            signatures,
            ..Default::default()
        },
    )
    .await
}

/// One whole owner-signed page naming its target by `name_hash` beside the
/// display name (`CustodyMaterializeRequest::folder_name_hash`).
async fn materialize_addressed(
    nest: &Arc<AppState>,
    set_name: &str,
    display_name: Option<&str>,
    name_hash: [u8; 32],
) -> Result<CustodyMaterializeReply, RpcError> {
    let signatures = sign_custody(nest, set_name, TARGET_NONCE).await;
    client_call(
        nest,
        OWNER,
        "fauna.backup.custody.materialize",
        &CustodyMaterializeRequest {
            set_name: set_name.to_string(),
            folder_display_name: display_name.map(str::to_string),
            folder_name_hash: Some(serde_bytes::ByteBuf::from(name_hash.to_vec())),
            signer_key: Some(serde_bytes::ByteBuf::from(OWNER.to_vec())),
            signatures,
            ..Default::default()
        },
    )
    .await
}

/// Prepare the target and materialize every custody row signed, in one page.
async fn materialize_folder_whole(
    nest: &Arc<AppState>,
    set_name: &str,
    display_name: &str,
) -> Result<CustodyMaterializeReply, RpcError> {
    let sigs = sign_custody(nest, set_name, TARGET_NONCE).await;
    materialize_signed(nest, set_name, Some(display_name), sigs).await
}

/// Every live row of a folder the destination serves for `OWNER`, as
/// `(path_hash, path_sealed, manifest_hash)` — the triple the ceremony re-homes,
/// read back off the ordinary device-pull feed rather than a bespoke reader.
async fn live_rows(
    nest: &Arc<AppState>,
    folder_id: i64,
) -> Vec<([u8; 32], Option<Vec<u8>>, Option<[u8; 32]>)> {
    nest.db
        .get_sync_changes_for_folder(folder_id, 0, None)
        .await
        .expect("the ordinary per-folder change feed")
        .into_iter()
        .map(|r| {
            (
                <[u8; 32]>::try_from(&r.path_hash[..]).expect("32-byte path hash"),
                r.path_sealed.clone(),
                r.manifest_hash
                    .as_ref()
                    .map(|m| <[u8; 32]>::try_from(&m[..]).expect("32-byte manifest hash")),
            )
        })
        .collect()
}

/// Every row the owner's app is served for `folder` judged by the shared
/// reader every app runs, bound to the target's nonce: each one must be
/// admitted (owner-signed, unconditionally — there is no unsigned arm), and
/// carry the re-seed pseudo-device so no device reads it as its own echo.
async fn assert_every_served_row_verifies(nest: &Arc<AppState>, folder: &str, expect: usize) {
    use fauna_protocol::sync_row_verify::{ReaderBinding, RowReader, writer_roster};
    let page: fauna_protocol::sync::SyncChangesListReply = client_call(
        nest,
        OWNER,
        "fauna.sync.changes.list",
        &fauna_protocol::sync::SyncChangesListRequest {
            folder: Some(folder.into()),
            since: 0,
            ..Default::default()
        },
    )
    .await
    .expect("the owner's ordinary pull");
    assert_eq!(page.changes.len(), expect, "{page:?}");
    let mut reader = RowReader::new();
    reader.install_binding(ReaderBinding {
        set_nonce: Some(TARGET_NONCE),
        owner: Some(OWNER),
        account: Some(OWNER),
        ..Default::default()
    });
    reader.install_roster(writer_roster(&[]));
    let pseudo = hex::encode(fauna_core::label_custody::reseed_pseudo_device_id(&OWNER));
    for row in &page.changes {
        assert!(
            reader.judge(row).admits(),
            "every re-homed row verifies on every reader: {:?} for {row:?}",
            reader.judge(row)
        );
        assert_eq!(row.device_id.as_deref(), Some(pseudo.as_str()));
    }
}

/// **The covered-folder arm refuses an unsigned re-home and writes nothing.**
/// The arm never mints an unsigned row (`writer-signed-change-records.md`
/// ruling (7)(a)(ii)), so a folder-set request carrying no signatures refuses
/// `signature_required`: the prepared target stays empty and the delivered
/// custody untouched.
#[tokio::test]
async fn a_covered_folder_materialize_refuses_signature_required_and_writes_nothing() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;
    let target = prepare_target(&dest, "Photos").await;
    let custody_before = dest
        .db
        .list_backup_custody_in_set(&OWNER, &folder_set)
        .await
        .unwrap()
        .len();
    assert!(custody_before > 0, "the delivery landed custody");

    let err = materialize_folder(&dest, &folder_set, Some("Photos"))
        .await
        .expect_err("an unsigned re-home is refused");
    assert_eq!(
        err.code,
        RpcError::CODE_BACKUP_SIGNATURE_REQUIRED,
        "{err:?}"
    );

    assert!(
        live_rows(&dest, target).await.is_empty(),
        "the refused re-home minted no row"
    );
    assert_eq!(
        dest.db
            .list_backup_custody_in_set(&OWNER, &folder_set)
            .await
            .unwrap()
            .len(),
        custody_before,
        "custody is untouched"
    );
}

/// **A hash-addressed materialize names its target by `name_hash`** — proven
/// against a target whose resting plaintext name is blanked (the row after the
/// folder-name contraction), which no by-name lookup could find — and a
/// request whose hash is not its display name's is refused before anything is
/// written, since the two would name different sets.
#[tokio::test]
async fn a_hash_addressed_materialize_finds_a_blanked_target_and_refuses_a_foreign_hash() {
    let (dest, folder_set, path_hash, _) = deliver_a_covered_folder().await;
    let target = prepare_target(&dest, "Photos").await;
    // The post-contraction row: no plaintext name rests, only its address.
    {
        let conn = dest.db.conn().await;
        conn.execute(
            "UPDATE folders SET name = '' WHERE id = ?1",
            rusqlite::params![target],
        )
        .unwrap();
    }

    // A hash that is not the display name's names two different sets.
    let err = materialize_addressed(
        &dest,
        &folder_set,
        Some("Photos"),
        fauna_core::path_crypto::set_name_hash("Pictures"),
    )
    .await
    .expect_err("a foreign hash is refused");
    assert!(err.code.ends_with("invalid_params"), "{err:?}");
    assert!(live_rows(&dest, target).await.is_empty(), "nothing written");

    // The address alone is a whole request: a blanked set's custodian may
    // hold no plaintext name to send, and the nest needs none.
    let reply = materialize_addressed(
        &dest,
        &folder_set,
        None,
        fauna_core::path_crypto::set_name_hash("Photos"),
    )
    .await
    .expect("the hash alone resolves the blanked target");
    assert_eq!(reply.records, 1);
    assert_eq!(reply.remaining, Some(0));
    let rows = live_rows(&dest, target).await;
    assert_eq!(rows.len(), 1, "re-homed into the set the hash names");
    assert_eq!(rows[0].0, path_hash);
}

/// **A delivered covered folder materializes, owner-signed, into the live
/// folder the ceremony prepared.** The destination held the folder's
/// ciphertext and served the owner nothing; after the flip it serves the
/// prepared folder rows carrying the SOURCE's path hash, the SOURCE's sealed
/// name and the mirrored manifest — every one owner-signed under the target's
/// nonce and admitted by the reader every app runs. No key was used, no byte
/// moved, and the nest created no folder.
#[tokio::test]
async fn a_delivered_covered_folder_materializes_into_a_live_folder() {
    let (dest, folder_set, path_hash, manifest_hash) = deliver_a_covered_folder().await;

    // DESTINATION posture: the mirror set exists and is backup-type, and the
    // owner has no live folder of their own by that name.
    let mirror = dest
        .db
        .get_folder_for_actor(&folder_set, &OWNER)
        .await
        .unwrap()
        .expect("the delivered mirror set");
    assert!(mirror.custody_copy, "the folder mirror is a custody copy");
    assert!(
        dest.db
            .get_folder_for_actor("Photos", &OWNER)
            .await
            .unwrap()
            .is_none(),
        "delivery alone must not mint a live folder"
    );

    let target = prepare_target(&dest, "Photos").await;
    let reply = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect("the covered-folder arm materializes a delivered mirror");
    assert!(
        reply.segments.is_empty(),
        "the folder axis writes no segment area — that is what key-free means"
    );
    assert_eq!(reply.records, 1, "one custody row, one re-homed live row");
    assert_eq!(reply.remaining, Some(0), "nothing is owed");
    assert!(
        reply.custody_redundant,
        "the custody copy is redundant now, and survives: reclaiming it is the \
         owner's own separate set-delete gesture"
    );

    let live = dest
        .db
        .get_folder_for_actor("Photos", &OWNER)
        .await
        .unwrap()
        .expect("the prepared folder");
    assert_eq!(live.id, target, "the arm re-homes into the prepared set");
    assert!(
        !live.custody_copy,
        "the live folder is never a custody copy"
    );

    // The re-homed row: the SOURCE's path hash (custody's own `path_hash` is a
    // hash OF the mirror leaf, not this), the SOURCE's sealed name and the
    // mirrored manifest.
    let rows = live_rows(&dest, live.id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].0, path_hash,
        "the live row is keyed on the SOURCE's path hash, so the owner's other \
         devices address the file exactly as they always did"
    );
    assert_eq!(
        rows[0].1.as_deref(),
        Some(&b"sealed-name-label"[..]),
        "the sealed name re-homes verbatim — the nest holds no key that opens it"
    );
    assert_eq!(rows[0].2, Some(manifest_hash.digest()));
    assert_every_served_row_verifies(&dest, "Photos", 1).await;

    // Nothing was deleted: the custody set is intact behind the live folder.
    let custody = dest
        .db
        .list_backup_custody_in_set(&OWNER, &folder_set)
        .await
        .unwrap();
    assert_eq!(custody.len(), 1, "the ceremony deletes nothing, on success");

    // The size the statement signs and the live row carries is the manifest's
    // `total_size` — the custody row's figure is the nest's derived charge,
    // which never equals it (ruling (7)(a)(ii), *Where the statement reads its
    // size*). Asserted unequal so this cannot pass by coincidence.
    let manifest = held_manifest(&dest, &custody[0].manifest_hash).await;
    assert_ne!(
        custody[0].size_bytes, manifest.total_size as i64,
        "the custody charge is the held bytes, not the logical size"
    );
    let minted = dest
        .db
        .get_sync_changes_for_folder(live.id, 0, None)
        .await
        .unwrap();
    assert_eq!(
        minted[0].size_bytes, manifest.total_size as i64,
        "the re-homed row carries the manifest's size, never the custody charge"
    );
}

/// **A custody row whose manifest is not held refuses the set** — the size its
/// statement signs is unknown, so nothing is minted (`custody_incomplete`).
#[tokio::test]
async fn a_custody_row_whose_manifest_is_not_held_refuses_and_writes_nothing() {
    let (dest, folder_set, _path_hash, manifest_hash) = deliver_a_covered_folder().await;
    let target = prepare_target(&dest, "Photos").await;
    // Signed while the manifest is held, then the manifest goes missing.
    let signatures = sign_custody(&dest, &folder_set, TARGET_NONCE).await;
    dest.backup_service
        .as_ref()
        .unwrap()
        .local_blob_store()
        .delete(&manifest_hash)
        .await
        .unwrap();

    let err = materialize_signed(&dest, &folder_set, Some("Photos"), signatures)
        .await
        .expect_err("an unreadable manifest refuses");
    assert_eq!(err.code, "fauna.backup.custody_incomplete", "{err:?}");
    assert!(
        live_rows(&dest, target).await.is_empty(),
        "nothing is minted"
    );
}

/// **The nest never creates the target** (ruling (7)(a)(i)): a materialize
/// naming a folder the owner does not list refuses `target_missing`, typed,
/// and writes nothing — the remedy is the caller preparing the set.
#[tokio::test]
async fn materializing_into_a_folder_the_owner_does_not_list_refuses_target_missing() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;
    let err = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect_err("the arm creates no folder");
    assert_eq!(err.code, RpcError::CODE_BACKUP_TARGET_MISSING, "{err:?}");
    assert!(
        dest.db
            .get_folder_for_actor("Photos", &OWNER)
            .await
            .unwrap()
            .is_none(),
        "and none was minted"
    );
}

/// **A target with no stored nonce cannot bind a signature** — refused as the
/// record door refuses one, and nothing is written.
#[tokio::test]
async fn a_target_with_no_stored_nonce_refuses_and_writes_nothing() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;
    let target = dest
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .unwrap();
    let err = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect_err("no nonce, no signed row");
    assert_eq!(err.code, "fauna.backup.signature_invalid", "{err:?}");
    assert!(live_rows(&dest, target).await.is_empty());
}

/// **One bad signature refuses the page whole.** A page whose rows verify but
/// one is signed under another set's nonce writes nothing at all — not the
/// good rows either (ruling (7)(a)(ii): one transaction per page, refused
/// whole).
#[tokio::test]
async fn a_page_with_one_bad_signature_refuses_whole_and_writes_nothing() {
    let (dest, folder_set, _seeded) = deliver_covered_folder_files(2).await;
    let target = prepare_target(&dest, "Photos").await;
    let mut sigs = sign_custody(&dest, &folder_set, TARGET_NONCE).await;
    let forged = sign_custody(&dest, &folder_set, [0xEE; 32]).await;
    sigs[1] = forged[1].clone();

    let err = materialize_signed(&dest, &folder_set, Some("Photos"), sigs)
        .await
        .expect_err("a row bound to another set's nonce does not verify");
    assert_eq!(err.code, "fauna.backup.signature_invalid", "{err:?}");
    assert!(
        live_rows(&dest, target).await.is_empty(),
        "the good row in the same page was not written either"
    );
}

/// **A signature naming no custody row of the set refuses the page.**
#[tokio::test]
async fn a_signature_naming_no_custody_row_refuses_the_page() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;
    let target = prepare_target(&dest, "Photos").await;
    let mut sigs = sign_custody(&dest, &folder_set, TARGET_NONCE).await;
    sigs.push(fauna_protocol::backup::RehomeSignature {
        path_hash: serde_bytes::ByteBuf::from(vec![0xAB; 32]),
        signature: sigs[0].signature.clone(),
        ..Default::default()
    });
    let err = materialize_signed(&dest, &folder_set, Some("Photos"), sigs)
        .await
        .expect_err("a row the set does not hold");
    assert!(err.code.ends_with("invalid_params"), "{err:?}");
    assert!(live_rows(&dest, target).await.is_empty());
}

/// **Paging resumes, landing each row once.** The driver sends the signatures
/// in pages; each page re-homes exactly what it carries and says what is still
/// owed, and a page replayed after a tear is ours and is skipped — never a
/// duplicate row, never `target_not_empty`.
#[tokio::test]
async fn a_paged_materialize_lands_each_row_once_across_pages() {
    let (dest, folder_set, seeded) = deliver_covered_folder_files(3).await;
    let target = prepare_target(&dest, "Photos").await;
    let sigs = sign_custody(&dest, &folder_set, TARGET_NONCE).await;

    let first = materialize_signed(&dest, &folder_set, Some("Photos"), sigs[..2].to_vec())
        .await
        .expect("first page");
    assert_eq!((first.records, first.remaining), (2, Some(1)));
    assert_eq!(live_rows(&dest, target).await.len(), 2);

    let second = materialize_signed(&dest, &folder_set, Some("Photos"), sigs[2..].to_vec())
        .await
        .expect("second page — the first page's rows are ours, not a lived-in folder");
    assert_eq!((second.records, second.remaining), (3, Some(0)));

    let replay = materialize_signed(&dest, &folder_set, Some("Photos"), sigs[..2].to_vec())
        .await
        .expect("a replayed page after a tear resumes");
    assert_eq!((replay.records, replay.remaining), (3, Some(0)));

    let rows = live_rows(&dest, target).await;
    assert_eq!(rows.len(), 3, "each row landed exactly once");
    let mut want: Vec<[u8; 32]> = seeded.iter().map(|(p, _)| *p).collect();
    let mut got: Vec<[u8; 32]> = rows.iter().map(|r| r.0).collect();
    want.sort();
    got.sort();
    assert_eq!(got, want);
    assert_every_served_row_verifies(&dest, "Photos", 3).await;
}

/// **The empty-target rule, folder axis.** A folder already holding live records
/// this ceremony did not write refuses — typed `fauna.backup.target_not_empty`,
/// no force arm — and changes nothing, on either side.
#[tokio::test]
async fn materializing_onto_a_lived_in_folder_refuses_and_deletes_nothing() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;

    // The owner already has a "Photos" folder on this nest, with a file in it.
    let live_id = prepare_target(&dest, "Photos").await;
    let (existing_path, existing_manifest, _) =
        seed_folder_file(&dest, live_id, "photos/dog.jpg", 0x51).await;

    let err = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect_err("a lived-in folder is not a fresh target");
    assert!(
        format!("{err:?}").contains("target_not_empty"),
        "the empty-target rule's own wire code, shared with the segment axis: {err:?}"
    );

    // Untouched on both sides — no merge, no delete, no partial write.
    let rows = live_rows(&dest, live_id).await;
    assert_eq!(rows.len(), 1, "the lived-in folder gained nothing");
    assert_eq!(rows[0].0, existing_path);
    assert_eq!(rows[0].2, Some(existing_manifest.digest()));
    assert_eq!(
        dest.db
            .list_backup_custody_in_set(&OWNER, &folder_set)
            .await
            .unwrap()
            .len(),
        1,
        "and the custody it refused to re-home is still there"
    );
}

/// **A custody row with no sealed name refuses by its own code.** The
/// legitimate, recoverable case: a custodian that pulled before the 2026-08-23
/// widening holds bytes for a path whose content has not changed since, and
/// delivers them with no sealed name. Minting a nameless live row is not an
/// option — a live folder set rests no plaintext path — so the refusal has to
/// name the cause, rather than surfacing as `path_seal_required` from one layer
/// deeper where it says nothing the owner can act on.
#[tokio::test]
async fn a_custody_row_without_a_sealed_name_refuses_by_its_own_code() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;

    // Clear the delivered row's sealed name — the shape a public-audience head
    // rests in (it carries no sealed name).
    {
        let conn = dest.db.conn().await;
        let n = conn
            .execute(
                "UPDATE backup_custody SET path_sealed = NULL
                 WHERE folder_id = (SELECT id FROM folders WHERE actor_id = ?1 AND name = ?2)",
                rusqlite::params![OWNER.as_slice(), &folder_set],
            )
            .unwrap();
        assert_eq!(n, 1, "the delivered row is the one being aged");
    }

    let err = materialize_folder(&dest, &folder_set, Some("Photos"))
        .await
        .expect_err("a nameless custody row cannot become a live row");
    assert!(
        format!("{err:?}").contains("custody_unsealed"),
        "its own code, not `custody_incomplete` — the fix is a pull pass then a \
         re-delivery, not waiting for the destination to catch up: {err:?}"
    );
    assert!(
        dest.db
            .get_folder_for_actor("Photos", &OWNER)
            .await
            .unwrap()
            .is_none(),
        "a refusal that validated every row before writing any leaves no folder \
         behind either"
    );
}

/// **A torn run resumes rather than locking the owner out.** Re-running the verb
/// over a target that already holds this ceremony's own rows is the crash-
/// recovery path, and reading those rows as "not empty" would make the set
/// permanently un-materializable under a rule whose whole purpose is protecting
/// data. Same discriminator as the segment axis's occupied-path classifier: a
/// row whose `(path_hash, manifest_hash)` is one this call would itself write is
/// ours, and is skipped rather than rewritten.
#[tokio::test]
async fn re_running_a_materialize_resumes_instead_of_refusing_its_own_rows() {
    let (dest, folder_set, path_hash, manifest_hash) = deliver_a_covered_folder().await;
    let live = prepare_target(&dest, "Photos").await;

    let first = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect("first pass");
    assert_eq!(first.records, 1);

    let second = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect("a re-run over this ceremony's own rows resumes, it does not refuse");
    assert_eq!(
        second.records, 1,
        "the row is counted as materialized, not written twice"
    );

    let rows = live_rows(&dest, live).await;
    assert_eq!(
        rows.len(),
        1,
        "and no duplicate row was appended for the same path"
    );
    assert_eq!(rows[0].0, path_hash);
    assert_eq!(rows[0].2, Some(manifest_hash.digest()));
}

/// **An empty folder is not the same thing as a FRESH one.** The empty-target
/// rule's whole promise is that materialize seeds a fresh folder, and a folder
/// can hold zero records while being *shared with an MLS roster*, declassified
/// public, served as the user's website, or exposed over WebDAV. Adopting one
/// re-homes the restored corpus straight into somebody else's audience: every
/// roster member gets the corpus's metadata (`can_read_folder` grants
/// `FolderReadGrant::Member` per folder, not per row), and a member holding an
/// explicit `writer` role can tombstone the re-homed rows — reclaiming the
/// chunks and destroying the corpus the owner just restored.
///
/// The precondition is entirely ordinary: create a folder, share it, add no
/// files. So the refusal names the property rather than a row count, and gets
/// its own wire code — `target_not_empty` sends the owner looking for records
/// that are not there, which is the wrong instruction here.
#[tokio::test]
async fn materializing_onto_a_shared_but_empty_folder_refuses_as_not_fresh() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;

    // The owner has a "Photos" folder they created and shared with a group —
    // and never put a file in.
    let live = prepare_target(&dest, "Photos").await;
    assert!(
        dest.db
            .set_folder_mls_group("Photos", &OWNER, Some(&[7u8; 32][..]))
            .await
            .unwrap(),
        "the empty folder is now bound to a roster"
    );

    let err = materialize_folder_whole(&dest, &folder_set, "Photos")
        .await
        .expect_err("an empty folder with an audience is not a fresh target");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("target_not_fresh"),
        "its own wire code: the folder IS empty, so `target_not_empty` would \
         send the owner looking for records that are not there — what they have \
         to do is pick a different name or unshare this one: {err:?}"
    );
    assert!(
        rendered.contains("mls_group_id"),
        "and it names the property that made the target unfresh, so the owner \
         knows which one to clear: {err:?}"
    );

    // Nothing was written: the folder is still empty, and the custody it
    // refused to re-home is still there to try again with.
    assert!(
        live_rows(&dest, live).await.is_empty(),
        "the shared folder gained nothing — the refusal changed nothing"
    );
    assert_eq!(
        dest.db
            .list_backup_custody_in_set(&OWNER, &folder_set)
            .await
            .unwrap()
            .len(),
        1,
        "and the custody survives the refusal"
    );
}

/// **Every publication-bearing property refuses, not just the roster binding.**
/// The family is classified by one exhaustive destructure, so a column added to
/// `folders` later cannot join it un-checked; this pins each member the
/// destructure knows today, each against its own freshly-delivered corpus.
#[tokio::test]
async fn each_publication_property_refuses_an_otherwise_empty_target() {
    // (the SQL that turns the property on, the property name the refusal names)
    for (turn_it_on, property) in [
        ("UPDATE folders SET audience = 'public'", "audience"),
        ("UPDATE folders SET website_enabled = 1", "website_enabled"),
        ("UPDATE folders SET webdav_enabled = 1", "webdav_enabled"),
        (
            "UPDATE folders SET web_paywall_tier = 'gold'",
            "web_paywall_tier",
        ),
    ] {
        let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;
        prepare_target(&dest, "Photos").await;
        {
            let conn = dest.db.conn().await;
            let n = conn
                .execute(
                    &format!("{turn_it_on} WHERE actor_id = ?1 AND name = ?2"),
                    rusqlite::params![OWNER.as_slice(), "Photos"],
                )
                .unwrap();
            assert_eq!(
                n, 1,
                "{property}: the empty target is the row being flagged"
            );
        }

        let err = materialize_folder_whole(&dest, &folder_set, "Photos")
            .await
            .expect_err("a publication-bearing empty folder is not a fresh target");
        let rendered = format!("{err:?}");
        assert!(
            rendered.contains("target_not_fresh"),
            "{property} refuses under the freshness code: {err:?}"
        );
        assert!(
            rendered.contains(property),
            "{property} is named in the refusal: {err:?}"
        );
    }
}

/// **A folder-axis set with no display name refuses, rather than guessing one.**
/// The set name carries only the source nest id and the source's folder rowid —
/// neither is a label, and neither means anything on the target — so the name
/// has to come from the custodian driving the ceremony.
#[tokio::test]
async fn a_folder_set_without_a_display_name_refuses() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;

    let err = materialize_folder(&dest, &folder_set, None)
        .await
        .expect_err("custody holds sealed paths, never the folder's label");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("folder_display_name"),
        "the refusal names the field the caller must supply: {rendered}"
    );
    assert!(
        dest.db
            .get_folder_for_actor("Photos", &OWNER)
            .await
            .unwrap()
            .is_none()
    );
}

/// **A folder set holding no live custody refuses, rather than re-homing
/// nothing.** Custody rows ARE this plane's corpus — there is no mirror file to
/// be missing — so an empty set means either delivery has not happened or a
/// detach at the source tombstoned every mirrored path. Reporting success and
/// leaving the owner an empty folder answers neither question.
#[tokio::test]
async fn an_empty_covered_folder_set_refuses_rather_than_minting_an_empty_folder() {
    let (dest, folder_set, _path_hash, _manifest_hash) = deliver_a_covered_folder().await;

    // Tombstone every mirrored path, exactly as a detach at the source does.
    {
        let conn = dest.db.conn().await;
        let n = conn
            .execute(
                "UPDATE backup_custody SET manifest_hash = NULL
                 WHERE folder_id = (SELECT id FROM folders WHERE actor_id = ?1 AND name = ?2)",
                rusqlite::params![OWNER.as_slice(), &folder_set],
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    let err = materialize_folder(&dest, &folder_set, Some("Photos"))
        .await
        .expect_err("an empty custody set has nothing to re-home");
    assert!(
        format!("{err:?}").contains("custody_incomplete"),
        "the same code the segment axis uses for a corpus that is not whole: {err:?}"
    );
    assert!(
        dest.db
            .get_folder_for_actor("Photos", &OWNER)
            .await
            .unwrap()
            .is_none(),
        "and no empty folder is left behind for the owner to clean up"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Phase 3b — recovery into the lived-in nest that regressed
// ═════════════════════════════════════════════════════════════════════════════
//
// `segment-backup-protocol.md` § Client-device custodian (pull) → *Restore* →
// *Recovery into the lived-in nest that regressed*, the proof obligation.

/// Five months, one segment each: the source's append rotates on the bucket.
const MONTH: [i64; 5] = [
    1_715_000_000_000, // 2024-05
    1_717_700_000_000, // 2024-06
    1_720_300_000_000, // 2024-07
    1_723_000_000_000, // 2024-08
    1_725_700_000_000, // 2024-09
];

/// The mail the source holds when the copy of its data directory is taken:
/// segments 1 and 2.
const KEPT: &[Filed<'static>] = &[
    (MONTH[0], "INBOX", ""),
    (MONTH[0] + 1_000, "INBOX", "\\Seen"),
    (MONTH[1], "Archive", "\\Seen \\Flagged"),
];
/// The mail the source takes after the copy — one month, but four segments
/// (the coordinator's push after each finalizes the open one), so the lost
/// copy numbers ids 3 to 6 — which the restore loses and the destination
/// still holds.
///
/// Four, because the verb's closing floor is load-bearing only when the lost
/// copy numbered more segments than the restored source's own post-rollback
/// life plus the recovery's own rotations (every `fauna.segments.list` and pair
/// read finalizes the open segment, so the recovered appends rotate once per
/// run): here those reach id 5, and the lost copy's counter is 7.
const LOST: &[Filed<'static>] = &[
    (MONTH[2], "Projects", "\\Seen"),
    (MONTH[2] + 1_000, "INBOX", ""),
    (MONTH[2] + 2_000, "Archive", "\\Flagged"),
    (MONTH[2] + 3_000, "INBOX", "\\Seen"),
];
/// The mail the restored source takes before anyone notices: its counter reuses
/// segment id 3, superseding the lost segment 3 at the destination.
const AFTER: &[Filed<'static>] = &[(MONTH[4], "INBOX", "")];

/// The record digest [`file_mail`] files one `(stamp, mailbox)` under.
fn filed_digest(ts: i64, mailbox: &str) -> [u8; 32] {
    let envelope = fauna_mail::segments::MailRecordEnvelope::new(
        format!("sealed-filed-body-{}-{ts}-{mailbox}", OWNER[0]).into_bytes(),
        b"sealed-index-hint".to_vec(),
    );
    fauna_mail::segments::ops::encode_record(&envelope)
        .unwrap()
        .0
        .digest()
}

/// A tempdir that outlives the test, as [`start_nest`]'s does; the OS reaps it.
fn kept_tempdir() -> std::path::PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    std::mem::forget(dir);
    path
}

/// Copy a data directory tree — everything but the live database, which
/// [`copy_data_dir`] takes consistently with `VACUUM INTO`.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("nest.db") {
            continue;
        }
        let (src, dst) = (entry.path(), to.join(&name));
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// An older copy of a running nest's data directory, as an owner's backup
/// tool (or a VM snapshot) would take it: the segment areas, the blob store
/// and a consistent image of the database.
async fn copy_data_dir(nest: &Arc<AppState>, from: &std::path::Path, to: &std::path::Path) {
    // Seal the open segments first, as a stop would — the copy is of a nest
    // at rest, not of a half-written file.
    nest.mail_segments.finalize_open(&OWNER).await.unwrap();
    nest.mail_placement.finalize_open(&OWNER).await.unwrap();
    nest.cal_segments.finalize_open(&OWNER).await.unwrap();
    nest.cal_placement.finalize_open(&OWNER).await.unwrap();
    nest.card_segments.finalize_open(&OWNER).await.unwrap();
    nest.card_placement.finalize_open(&OWNER).await.unwrap();
    copy_tree(from, to);
    let conn = nest.db.conn().await;
    conn.execute(
        "VACUUM INTO ?1",
        rusqlite::params![to.join("nest.db").to_string_lossy().into_owned()],
    )
    .unwrap();
}

/// An `RpcRequester` into one nest's client-facing router as `OWNER` — the
/// owner's own authenticated connection. `tear` optionally fails one call,
/// which is how the torn runs are staged.
struct OwnerLink {
    nest: Arc<AppState>,
    tear: Tear,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tear {
    None,
    /// Fail the first custody record of a `.meta` — after its `.dat` landed.
    BeforeFirstMeta,
    /// Run the verb for two records only, then fail as a crash would.
    VerbAfterTwo,
}

impl fauna_protocol::RpcRequester for OwnerLink {
    type Error = String;

    async fn request<Req, Reply>(&self, kind: &'static str, payload: Req) -> Result<Reply, String>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        let bytes = bytes::Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        if self.tear == Tear::BeforeFirstMeta && kind == "fauna.sync.changes.record" {
            let req: fauna_protocol::sync::SyncChangeRecordRequest = decode(&bytes).unwrap();
            if req.path.ends_with(".meta") {
                return Err("torn: the connection dropped before the sidecar".into());
            }
        }
        if self.tear == Tear::VerbAfterTwo && kind == "fauna.backup.custody.recover" {
            let req: fauna_protocol::backup::CustodyRecoverRequest = decode(&bytes).unwrap();
            let cut = fauna_nest::backup::recover::recover_segment_set_for_test(
                &self.nest,
                &OWNER,
                &req.set_name,
                Some(2),
            )
            .await?;
            assert_eq!(cut.recovered, 2, "the torn verb landed exactly two records");
            return Err("torn: the nest stopped two records in".into());
        }
        let meta = self
            .nest
            .rpc_router
            .kind_meta(kind)
            .ok_or_else(|| format!("kind not registered: {kind}"))?;
        let out = (meta.handler)(self.nest.clone(), OWNER, bytes)
            .await
            .map_err(|e| format!("{}: {:?}", e.code, e.message))?;
        decode(&out).map_err(|e| e.to_string())
    }
}

fauna_client_backup::impl_backup_nest_seam!(struct DestinationSeam<OwnerLink>);

/// The source's owner-session byte plane — the production chunk and manifest
/// handlers, called in process with the owner's bulk-write authority.
struct SourceBytePlane(Arc<AppState>);

#[async_trait::async_trait]
impl fauna_sync_engine::reseed::BlobPushSink for SourceBytePlane {
    async fn missing_chunks(&self, store_keys: &[ContentHash]) -> anyhow::Result<Vec<ContentHash>> {
        Ok(store_keys.to_vec())
    }

    async fn put_chunk(&self, store_key: &ContentHash, body: &[u8]) -> anyhow::Result<()> {
        use axum::response::IntoResponse as _;
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "X-Content-Hash",
            hex::encode(store_key.digest()).parse().unwrap(),
        );
        let resp = fauna_nest::chunk_routes::upload_chunk(
            axum::extract::State(self.0.clone()),
            fauna_nest::auth::BulkWriteAuth(fauna_core::identity::ActorId(OWNER)),
            headers,
            bytes::Bytes::from(body.to_vec()),
        )
        .await
        .into_response();
        anyhow::ensure!(
            resp.status().is_success(),
            "chunk upload: {}",
            resp.status()
        );
        Ok(())
    }

    async fn put_manifest(&self, manifest_bytes: &[u8]) -> anyhow::Result<()> {
        use axum::response::IntoResponse as _;
        let resp = fauna_nest::chunk_routes::upload_manifest(
            axum::extract::State(self.0.clone()),
            fauna_nest::auth::BulkWriteAuth(fauna_core::identity::ActorId(OWNER)),
            bytes::Bytes::from(manifest_bytes.to_vec()),
        )
        .await
        .into_response();
        anyhow::ensure!(
            resp.status().is_success(),
            "manifest upload: {}",
            resp.status()
        );
        Ok(())
    }
}

const SOURCE_DEVICE: [u8; 32] = [0x0C; 32];
const DEST_DEVICE: [u8; 32] = [0x0D; 32];

async fn register_device(nest: &Arc<AppState>, device: [u8; 32]) {
    nest.db
        .register_sync_device(&OWNER, &device, "recovery-device", None, "write")
        .await
        .unwrap();
    common::seed_dispatch_actor(&nest.db, &OWNER).await;
}

/// One run of the delivery leg B → A, as the owner's app drives it.
async fn run_recovery_leg(
    source: &Arc<AppState>,
    dest: &Arc<AppState>,
    dest_url: &str,
    kind: &str,
    tear: Tear,
) -> anyhow::Result<fauna_sync_engine::reseed::RecoveryReport> {
    use fauna_sync_engine::reseed::{
        NoAcceptedRegressionRecord, RecoveryDelivery, RecoveryDestination, RecoverySource,
    };
    let dest_link = OwnerLink {
        nest: dest.clone(),
        tear: Tear::None,
    };
    let seam = DestinationSeam {
        client: fauna_client_backup::BackupClient::new(OwnerLink {
            nest: dest.clone(),
            tear: Tear::None,
        }),
    };
    let dest_bytes = OpenRouteFetcher {
        base: dest_url.to_string(),
        http: reqwest::Client::new(),
    };
    let source_link = OwnerLink {
        nest: source.clone(),
        tear,
    };
    let sink = SourceBytePlane(source.clone());
    let leg = RecoveryDelivery::new(
        RecoveryDestination {
            seam: &seam,
            bytes: &dest_bytes,
            records: &dest_link,
            device_id: hex::encode(DEST_DEVICE),
        },
        RecoverySource {
            bytes: &sink,
            nest: &source_link,
            device_id: hex::encode(SOURCE_DEVICE),
        },
        OWNER,
        &NestBackupKey::from_bytes(GRANTED_KEY),
        &NoAcceptedRegressionRecord,
    );
    leg.run_kind(kind).await
}

/// The digests a nest serves for `OWNER` through the ordinary mail read path,
/// sorted — every record exactly as often as it is live.
async fn served_digests(nest: &Arc<AppState>) -> Vec<[u8; 32]> {
    let mut out: Vec<[u8; 32]> = fauna_nest::segments::mail::read_after_seq(
        &nest.mail_segments,
        &nest.db,
        &OWNER,
        0,
        i64::MAX,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|(_seq, rid, _body, _floor)| rid)
    .collect();
    out.sort();
    out
}

/// **A source nest that came back from an older copy of its data directory
/// gets every record it lost back — as records, filed where they were, under
/// fresh UIDs — and nothing it kept, deleted or took since is disturbed.**
///
/// The owner doc's proof obligation, step by step: the real coordinator
/// delivers A's mail to B; the harness copies A's data directory at two
/// segments, A takes mail through four, the coordinator pushes; A is stopped,
/// the copy restored and A restarted — the regression. A takes post-rollback
/// mail (its counter reuses id 3, superseding B's lost segment 3) and the
/// owner deletes one kept message. The leg pulls B → A — torn once after a
/// `.dat` and before its `.meta`, and once inside the verb after two records —
/// and the third run completes. No accepted-regression floor has landed first
/// (that verb is not built yet), so the verb's own closing floor is what keeps
/// A's next segment above every id the lost copy numbered.
#[tokio::test]
async fn a_regressed_source_recovers_its_lost_records_by_record() {
    // ── A on disk, B ordinary, both enrolled ─────────────────────────────
    let a_dir = kept_tempdir();
    let mut a_secret = [0u8; 32];
    getrandom::fill(&mut a_secret).unwrap();
    let a_db = Arc::new(CacheDb::open(a_dir.join("nest.db")).unwrap());
    let (_a_url, a, _a_blobs) = start_nest_on(a_dir.clone(), a_db, a_secret).await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&a, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&a, &dest, &d_url).await;
    // The coordinator's pass, driven directly so a failed pass fails the test
    // (the hosting worker logs and carries on, which would hide it).
    let push = |nest: &Arc<AppState>| {
        let nest = Arc::clone(nest);
        async move {
            let coordinator = NestBackupCoordinator::open_for_owner(nest, OWNER)
                .await
                .unwrap()
                .expect("owner is enrolled");
            let dest_row = coordinator.destinations()[0].clone();
            coordinator
                .run_once(&dest_row, KIND)
                .await
                .expect("the coordinator's pass");
        }
    };

    file_mail(&a, &OWNER, KEPT).await;
    push(&a).await;

    // ── The copy at M = 2 segments, then A lives on to N = 4 ─────────────
    let copy_dir = kept_tempdir();
    copy_data_dir(&a, &a_dir, &copy_dir).await;
    for lost in LOST {
        file_mail(&a, &OWNER, std::slice::from_ref(lost)).await;
        push(&a).await;
    }
    let before = served_digests(&a).await;
    assert_eq!(before.len(), KEPT.len() + LOST.len());
    let counter = |nest: &Arc<AppState>| {
        let nest = Arc::clone(nest);
        async move {
            nest.mail_segments
                .load_manifest(&OWNER)
                .await
                .unwrap()
                .kind_manifest
                .next_seg_id
        }
    };
    let pin = counter(&a).await;
    assert_eq!(pin, 7, "A numbered six segments before the rollback");

    // ── The regression: stop A, restore the copy, restart ────────────────
    drop(a);
    let a_db = Arc::new(CacheDb::open(copy_dir.join("nest.db")).unwrap());
    let (_a_url, a, _a_blobs) = start_nest_on(copy_dir.clone(), a_db, a_secret).await;
    assert_eq!(
        served_digests(&a).await.len(),
        KEPT.len(),
        "the restored A holds only what the copy held"
    );

    // Post-rollback life: new mail (reusing id 3) and one deletion.
    file_mail(&a, &OWNER, AFTER).await;
    push(&a).await;
    let reused = format!("{}/seg-00000003.dat", hex::encode(OWNER));
    assert!(
        dest.db
            .list_backup_custody_generations(&OWNER, None, 0)
            .await
            .unwrap()
            .iter()
            .any(|g| g.path.as_deref() == Some(reused.as_str())),
        "the restored A's counter reused id 3, so B retains the lost segment 3 it superseded"
    );
    let deleted = filed_digest(KEPT[0].0, KEPT[0].1);
    {
        let conn = a.db.conn().await;
        let n = conn
            .execute(
                "UPDATE segment_records SET tombstoned = 1 \
                 WHERE scope_id = ?1 AND kind = 'mail' AND substr(record_cid, 5) = ?2",
                rusqlite::params![OWNER.as_slice(), deleted.as_slice()],
            )
            .unwrap();
        assert_eq!(n, 1, "the owner deletes one kept message");
        conn.execute(
            "DELETE FROM bridge_imap_messages WHERE actor_id = ?1 AND message_id = ?2",
            rusqlite::params![OWNER.as_slice(), deleted.as_slice()],
        )
        .unwrap();
    }

    // The accepted regression's own floor (`fauna.segments.counter_floor`) is
    // not built yet, so this recovery runs before any
    // floor landed: exactly the case the verb's closing floor guards, which is
    // what the "next append lands above the pin" check below proves.

    register_device(&a, SOURCE_DEVICE).await;
    register_device(&dest, DEST_DEVICE).await;
    grant_key_to(&a).await;

    // ── Tear 1: the leg, after a `.dat`, before its `.meta` ───────────────
    let torn = run_recovery_leg(&a, &dest, &d_url, KIND, Tear::BeforeFirstMeta).await;
    assert!(torn.is_err(), "the first run tears");

    // ── Tear 2: the verb, after two records ──────────────────────────────
    let torn = run_recovery_leg(&a, &dest, &d_url, KIND, Tear::VerbAfterTwo).await;
    assert!(torn.is_err(), "the second run tears inside the verb");

    // ── The run that completes ───────────────────────────────────────────
    let report = run_recovery_leg(&a, &dest, &d_url, KIND, Tear::None)
        .await
        .expect("the third run completes");
    assert_eq!(
        report.recovered.recovered + 2,
        LOST.len() as u64,
        "the run finishes what the torn verb began, and nothing twice"
    );
    // Read before anything else appends: the recovered appends alone reach
    // id 5, so only the verb's own floor puts A's counter at the lost copy's.
    assert_eq!(report.recovered.floor, Some(pin));
    let floored = counter(&a).await;
    assert!(
        floored >= pin,
        "the verb floored A's counter at the lost copy's generation: {floored} < {pin} \
         (reply {:?})",
        report.recovered
    );
    assert_eq!(
        report.recovered.inboxed, 0,
        "every lost message was journaled"
    );

    // Every message A ever held — kept, lost and post-rollback — exactly
    // once by CID, the deleted one absent.
    let mut expected: Vec<[u8; 32]> = KEPT
        .iter()
        .chain(LOST)
        .chain(AFTER)
        .map(|(ts, mailbox, _)| filed_digest(*ts, mailbox))
        .filter(|d| *d != deleted)
        .collect();
    expected.sort();
    assert_eq!(served_digests(&a).await, expected);

    // Each lost message in its original mailbox with its flags, under a UID
    // the lived-in A minted.
    for (ts, mailbox, flags) in LOST {
        let digest = filed_digest(*ts, mailbox);
        let served = serve_mailbox(&a, &OWNER, mailbox).await;
        let row = served
            .iter()
            .find(|(_, _, d)| d.as_slice() == digest.as_slice())
            .unwrap_or_else(|| panic!("the lost message at {ts} is back in {mailbox}"));
        assert_eq!(row.1, *flags, "{mailbox}: the flags the owner set");
    }
    assert!(
        !serve_mailbox(&a, &OWNER, "INBOX")
            .await
            .iter()
            .any(|(_, _, d)| d.as_slice() == deleted.as_slice()),
        "the owner's deletion stands"
    );

    // A second recover recovers nothing.
    let again =
        fauna_nest::backup::recover::recover_segment_set_for_test(&a, &OWNER, "__mail", None)
            .await
            .unwrap();
    assert_eq!(again.recovered, 0);

    // The floor held: A's next append lands above the pin — every id the lost
    // copy numbered is spent on A too.
    file_mail(&a, &OWNER, &[(MONTH[4] + 86_400_000 * 40, "INBOX", "")]).await;
    let top = *live_segment_ids(&a, OWNER).await.iter().max().unwrap();
    assert!(
        top >= pin,
        "A's next segment ({top}) is above the pinned {pin}"
    );

    // The post-recovery tombstone retired exactly B's unnamed live rows: what
    // B still holds live is what its live ledger names, plus the ledgers.
    let scope_hex = hex::encode(OWNER);
    let live_now = custody_paths(&dest).await;
    let ledger = open_custody_path(&dest, &d_url, &format!("{scope_hex}/manifest.{KIND}")).await;
    let ledger =
        fauna_sync_engine::segment_backup::LiveManifestMirror::from_bytes(&ledger).unwrap();
    let named: Vec<u32> = ledger.live.iter().map(|s| s.segment_id).collect();
    for path in &live_now {
        if let Some((id, _)) =
            fauna_sync_engine::segment_backup::SegmentFamily::Content.parse(&scope_hex, path)
        {
            assert!(named.contains(&id), "{path} is named by B's live ledger");
        }
    }
    assert!(
        !report.tombstoned_at_destination.is_empty(),
        "B held lost segments its live ledger no longer names, and retired them"
    );
}

// ── The lived-in recovery's calendar and contacts arms ──────────────────────

/// Which DAV kind a recovery proof drives — the calendar proof and its card
/// twin are one scenario over two doors.
#[derive(Clone, Copy)]
enum Dav {
    Calendar,
    Card,
}

/// A collection the DAV proofs create after the copy, and so lose.
const NEW_COLLECTION: [u8; 32] = [0x7E; 32];

/// One served DAV resource: `(collection, uid, item id, record cid, body)`.
type Served = ([u8; 32], u8, Vec<u8>, Vec<u8>, Vec<u8>);

impl Dav {
    fn kind(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::Card => "card",
        }
    }

    /// The collection the corpus starts in.
    fn home(self) -> [u8; 32] {
        match self {
            Self::Calendar => CALENDAR,
            Self::Card => ADDRESSBOOK,
        }
    }

    async fn provision(self, nest: &Arc<AppState>, collection: [u8; 32], name: &[u8]) {
        use fauna_protocol::bridge_routing::{
            ProvisionAddressbookReply, ProvisionAddressbookRequest, ProvisionCalendarReply,
            ProvisionCalendarRequest,
        };
        match self {
            Self::Calendar => {
                let _: ProvisionCalendarReply = client_call(
                    nest,
                    OWNER,
                    "fauna.bridges.provision_calendar",
                    &ProvisionCalendarRequest {
                        actor_id: OWNER.to_vec(),
                        calendar_id: collection.to_vec(),
                        encrypted_metadata: name.to_vec(),
                        ..Default::default()
                    },
                )
                .await
                .expect("provision a calendar");
            }
            Self::Card => {
                let _: ProvisionAddressbookReply = client_call(
                    nest,
                    OWNER,
                    "fauna.bridges.provision_addressbook",
                    &ProvisionAddressbookRequest {
                        actor_id: OWNER.to_vec(),
                        addressbook_id: collection.to_vec(),
                        encrypted_metadata: name.to_vec(),
                        ..Default::default()
                    },
                )
                .await
                .expect("provision an address book");
            }
        }
    }

    /// Put (or replace) resource `uid` in `collection` through the production
    /// door; returns the sealed body it stored, which is what reads back.
    async fn put(self, nest: &Arc<AppState>, collection: [u8; 32], uid: u8, tag: &str) -> Vec<u8> {
        use fauna_protocol::bridge_routing::{
            PutCardCiphertextReply, PutCardCiphertextRequest, PutEventCiphertextReply,
            PutEventCiphertextRequest,
        };
        let body = sealed(format!("{tag} uid-{uid}").as_bytes());
        let hint = sealed(format!("hint-{tag}").as_bytes());
        let timestamp = 1_752_000_000 + i64::from(uid);
        match self {
            Self::Calendar => {
                let _: PutEventCiphertextReply = client_call(
                    nest,
                    OWNER,
                    "fauna.bridges.put_event_ciphertext",
                    &PutEventCiphertextRequest {
                        actor_id: OWNER.to_vec(),
                        calendar_id: collection.to_vec(),
                        uid_hash: vec![uid; 32],
                        ciphertext_size: body.len() as u32,
                        encrypted_body: body.clone(),
                        encrypted_index_hint: hint,
                        timestamp,
                        ..Default::default()
                    },
                )
                .await
                .expect("put an event");
            }
            Self::Card => {
                let _: PutCardCiphertextReply = client_call(
                    nest,
                    OWNER,
                    "fauna.bridges.put_card_ciphertext",
                    &PutCardCiphertextRequest {
                        actor_id: OWNER.to_vec(),
                        addressbook_id: collection.to_vec(),
                        uid_hash: vec![uid; 32],
                        ciphertext_size: body.len() as u32,
                        encrypted_body: body.clone(),
                        encrypted_index_hint: hint,
                        timestamp,
                        ..Default::default()
                    },
                )
                .await
                .expect("put a card");
            }
        }
        body
    }

    async fn delete(self, nest: &Arc<AppState>, collection: [u8; 32], uid: u8) {
        use fauna_protocol::bridge_routing::{
            DeleteCardReply, DeleteCardRequest, DeleteEventReply, DeleteEventRequest,
        };
        match self {
            Self::Calendar => {
                let _: DeleteEventReply = client_call(
                    nest,
                    OWNER,
                    "fauna.bridges.delete_event",
                    &DeleteEventRequest {
                        actor_id: OWNER.to_vec(),
                        calendar_id: collection.to_vec(),
                        uid_hash: vec![uid; 32],
                        if_match: None,
                    },
                )
                .await
                .expect("delete an event");
            }
            Self::Card => {
                let _: DeleteCardReply = client_call(
                    nest,
                    OWNER,
                    "fauna.bridges.delete_card",
                    &DeleteCardRequest {
                        actor_id: OWNER.to_vec(),
                        addressbook_id: collection.to_vec(),
                        uid_hash: vec![uid; 32],
                        if_match: None,
                    },
                )
                .await
                .expect("delete a card");
            }
        }
    }

    /// `(items table, collection column, item id column, collections table,
    /// expunge table)`.
    fn tables(self) -> [&'static str; 5] {
        match self {
            Self::Calendar => [
                "bridge_caldav_events",
                "calendar_id",
                "event_id",
                "bridge_caldav_calendars",
                "bridge_caldav_expunged",
            ],
            Self::Card => [
                "bridge_carddav_cards",
                "addressbook_id",
                "card_id",
                "bridge_carddav_addressbooks",
                "bridge_carddav_expunged",
            ],
        }
    }

    /// Every resource the nest serves for `OWNER`, across its collections, the
    /// body read through the row's stored `record_cid` as every DAV read does.
    async fn served(self, nest: &Arc<AppState>) -> Vec<Served> {
        let [items, col, id, _, _] = self.tables();
        // (collection, uid_hash, item id, record_cid bytes)
        type ServedRow = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);
        let rows: Vec<ServedRow> = {
            let conn = nest.db.conn().await;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {col}, uid_hash, {id}, record_cid FROM {items} \
                     WHERE actor_id = ?1 ORDER BY {col}, uid_hash"
                ))
                .unwrap();
            stmt.query_map(rusqlite::params![OWNER.as_slice()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        };
        let mut out = Vec::with_capacity(rows.len());
        for (collection, uid, item, cid_bytes) in rows {
            let cid =
                fauna_cbor::Cid::from_bytes(cid_bytes.as_slice().try_into().unwrap()).unwrap();
            let body = match self {
                Self::Calendar => {
                    fauna_nest::segments::cal::read_record(
                        &nest.cal_segments,
                        &nest.db,
                        &OWNER,
                        &cid,
                    )
                    .await
                    .unwrap()
                    .expect("the row's body is live in the segment store")
                    .0
                    .encrypted_body
                }
                Self::Card => {
                    fauna_nest::segments::card::read_record(
                        &nest.card_segments,
                        &nest.db,
                        &OWNER,
                        &cid,
                    )
                    .await
                    .unwrap()
                    .expect("the row's body is live in the segment store")
                    .0
                    .encrypted_body
                }
            };
            out.push((
                collection.try_into().unwrap(),
                uid[0],
                item,
                cid_bytes,
                body,
            ));
        }
        out
    }

    /// `(highestmodseq, encrypted_metadata)` of every collection, by id.
    async fn collections(
        self,
        nest: &Arc<AppState>,
    ) -> std::collections::BTreeMap<[u8; 32], (i64, Vec<u8>)> {
        let [_, col, _, table, _] = self.tables();
        let conn = nest.db.conn().await;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {col}, highestmodseq, encrypted_metadata FROM {table} WHERE actor_id = ?1"
            ))
            .unwrap();
        stmt.query_map(rusqlite::params![OWNER.as_slice()], |r| {
            let id: Vec<u8> = r.get(0)?;
            Ok((id.try_into().unwrap(), (r.get(1)?, r.get(2)?)))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    /// The item ids a sync-collection from `token` returns for `collection`.
    async fn changes_since(
        self,
        nest: &Arc<AppState>,
        collection: [u8; 32],
        token: i64,
    ) -> Vec<Vec<u8>> {
        match self {
            Self::Calendar => nest
                .db
                .query_caldav_changes_since(&OWNER, &collection, token, 1_000)
                .await
                .unwrap()
                .events
                .into_iter()
                .map(|e| e.event_id.to_vec())
                .collect(),
            Self::Card => nest
                .db
                .query_carddav_changes_since(&OWNER, &collection, token, 1_000)
                .await
                .unwrap()
                .cards
                .into_iter()
                .map(|c| c.card_id.to_vec())
                .collect(),
        }
    }

    /// Whether the nest's expunge table records item `id` as deleted.
    async fn expunged(self, nest: &Arc<AppState>, id: &[u8]) -> bool {
        let [_, _, id_col, _, table] = self.tables();
        let conn = nest.db.conn().await;
        conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE actor_id = ?1 AND {id_col} = ?2)"),
            rusqlite::params![OWNER.as_slice(), id],
            |r| r.get(0),
        )
        .unwrap()
    }
}

/// The owner doc's *Proof obligation* for the lived-in recovery's calendar and
/// contacts arms (`segment-backup-protocol.md` § *Recovery's calendar and
/// contacts arms — the three collisions ruled*), for either kind.
///
/// The copy is taken at M with resources 1–3 in the home collection. After it
/// A lives on: a new collection with resources 10 and 11, an edit of the
/// copy-era resource 1, resource 4 added, resource 5 added then deleted, and
/// resource 6 added. The regression. After it the owner re-uploads 6 and
/// deletes the copy-era 2. The leg pulls B → A — torn once after a `.dat` and
/// before its `.meta`, once inside the verb after two records — and the third
/// run completes. As for mail, the accepted-regression floor verb is not
/// driven first; the verb's own closing floor covers the segment counter.
async fn a_regressed_source_recovers_its_lost_dav_resources(dav: Dav) {
    let kind = dav.kind();
    let home = dav.home();
    let a_dir = kept_tempdir();
    let mut a_secret = [0u8; 32];
    getrandom::fill(&mut a_secret).unwrap();
    let a_db = Arc::new(CacheDb::open(a_dir.join("nest.db")).unwrap());
    let (_a_url, a, _a_blobs) = start_nest_on(a_dir.clone(), a_db, a_secret).await;
    let (d_url, dest, _d_blobs) = start_nest().await;
    register_user(&a, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    enroll(&a, &dest, &d_url).await;

    // ── Copy era, then the copy at M ─────────────────────────────────────
    dav.provision(&a, home, b"home-meta").await;
    let mut copy_era = std::collections::BTreeMap::new();
    for uid in [1u8, 2, 3] {
        copy_era.insert(uid, dav.put(&a, home, uid, "v1").await);
    }
    back_up_kind(&a, kind).await;
    let copy_dir = kept_tempdir();
    copy_data_dir(&a, &a_dir, &copy_dir).await;
    let copy_era_item1 = dav
        .served(&a)
        .await
        .into_iter()
        .find(|r| r.1 == 1)
        .unwrap()
        .2;

    // ── A lives on past the copy: all of this is lost ────────────────────
    dav.provision(&a, NEW_COLLECTION, b"new-meta").await;
    let mut lost: Vec<([u8; 32], u8, Vec<u8>)> = Vec::new();
    for uid in [10u8, 11] {
        lost.push((
            NEW_COLLECTION,
            uid,
            dav.put(&a, NEW_COLLECTION, uid, "new").await,
        ));
    }
    lost.push((home, 1, dav.put(&a, home, 1, "v2-edit").await));
    lost.push((home, 4, dav.put(&a, home, 4, "v1").await));
    back_up_kind(&a, kind).await;
    dav.put(&a, home, 5, "v1").await;
    back_up_kind(&a, kind).await;
    dav.delete(&a, home, 5).await;
    let lost6 = dav.put(&a, home, 6, "v1").await;
    back_up_kind(&a, kind).await;
    // A client's sync tokens, taken just before the regression.
    let tokens = dav.collections(&a).await;

    // ── The regression; post-regression life a second later ──────────────
    drop(a);
    let a_db = Arc::new(CacheDb::open(copy_dir.join("nest.db")).unwrap());
    let (_a_url, a, _a_blobs) = start_nest_on(copy_dir.clone(), a_db, a_secret).await;
    assert_eq!(
        dav.served(&a).await.len(),
        3,
        "the restored A holds only the copy"
    );
    // Receive times are seconds: the owner's post-regression acts are born
    // strictly after every lost record, as they are in life.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let reuploaded6 = dav.put(&a, home, 6, "v2-reupload").await;
    dav.delete(&a, home, 2).await;
    back_up_kind(&a, kind).await;

    register_device(&a, SOURCE_DEVICE).await;
    register_device(&dest, DEST_DEVICE).await;
    grant_key_to(&a).await;

    // ── Two tears, then the run that completes ───────────────────────────
    let torn = run_recovery_leg(&a, &dest, &d_url, kind, Tear::BeforeFirstMeta).await;
    assert!(torn.is_err(), "the first run tears");
    let torn = run_recovery_leg(&a, &dest, &d_url, kind, Tear::VerbAfterTwo).await;
    assert!(torn.is_err(), "the second run tears inside the verb");
    let report = run_recovery_leg(&a, &dest, &d_url, kind, Tear::None)
        .await
        .expect("the third run completes");
    let r = &report.recovered;
    assert_eq!(
        r.recovered + 2,
        lost.len() as u64,
        "the run finishes what the torn verb began, and nothing twice: {r:?}"
    );
    assert_eq!(r.filed, r.recovered, "{r:?}");
    assert_eq!(r.inboxed, 0, "{r:?}");
    assert_eq!(
        r.unplaceable, 1,
        "resource 5, deleted before the regression: {r:?}"
    );

    // ── Every resource A should now serve, exactly once by record ────────
    let served = dav.served(&a).await;
    let cids: std::collections::HashSet<&Vec<u8>> = served.iter().map(|s| &s.3).collect();
    assert_eq!(cids.len(), served.len(), "no record is served twice");
    let bodies: std::collections::BTreeMap<([u8; 32], u8), &Vec<u8>> =
        served.iter().map(|s| ((s.0, s.1), &s.4)).collect();
    for (collection, uid, body) in &lost {
        assert_eq!(
            bodies.get(&(*collection, *uid)),
            Some(&body),
            "lost resource {uid} is back in its collection"
        );
    }
    assert_eq!(
        bodies[&(home, 3)],
        &copy_era[&3],
        "the kept copy-era resource"
    );
    assert_eq!(
        bodies[&(home, 6)],
        &reuploaded6,
        "the post-regression re-upload stands"
    );
    assert_ne!(bodies[&(home, 6)], &lost6);
    assert!(
        !bodies.contains_key(&(home, 2)),
        "the post-regression delete stands"
    );
    assert!(
        !bodies.contains_key(&(home, 5)),
        "the pre-regression delete stands"
    );
    assert_eq!(served.len(), lost.len() + 2);
    assert!(
        dav.expunged(&a, &copy_era_item1).await,
        "the copy-era version the lost edit superseded is in the expunge table"
    );

    // ── The lost collection is back; recovered rows sit above the tokens ──
    let collections = dav.collections(&a).await;
    assert_eq!(collections[&NEW_COLLECTION].1, b"new-meta".to_vec());
    for (collection, uid, _) in &lost {
        let item = &served
            .iter()
            .find(|s| s.0 == *collection && s.1 == *uid)
            .unwrap()
            .2;
        let token = tokens[collection].0;
        assert!(
            dav.changes_since(&a, *collection, token)
                .await
                .contains(item),
            "a sync from the pre-regression token {token} lists recovered resource {uid}"
        );
    }

    // ── A second recover recovers nothing ────────────────────────────────
    let again = fauna_nest::backup::recover::recover_segment_set_for_test(
        &a,
        &OWNER,
        &format!("__{kind}"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(again.recovered, 0, "{again:?}");
    assert_eq!(dav.served(&a).await.len(), served.len());
}

/// **A regressed source gets its lost calendar events back, each as the
/// current version of its resource** — the three collisions as ruled.
#[tokio::test]
async fn a_regressed_source_recovers_its_lost_events_by_record() {
    a_regressed_source_recovers_its_lost_dav_resources(Dav::Calendar).await;
}

/// **The contacts twin, verbatim.**
#[tokio::test]
async fn a_regressed_source_recovers_its_lost_cards_by_record() {
    a_regressed_source_recovers_its_lost_dav_resources(Dav::Card).await;
}

// ═════════════════════════════════════════════════════════════════════════════
// The source box's identity on the backup plane — the writer seat
// (`segment-backup-protocol.md` § Cross-location backup protocol → *The writer
// seat*)
// ═════════════════════════════════════════════════════════════════════════════
//
// A destination keeps ONE writer seat per owner. The two tests below first
// reproduced the gaps that ruling closed — two boxes of one owner superseding
// each other's custody, and a rotated box stranded behind its predecessor's
// grant with a covered folder's mirror split across two sets — and now pin the
// ruling's destination half over a real federation handshake.

/// Append one mail record with a body of the caller's choosing. [`append_mail`]
/// derives its bodies from the actor and the index alone, so two boxes of one
/// owner would write byte-identical records — and these tests need the two
/// boxes (or the two sides of a rotation) to hold different mail.
async fn append_one_mail(state: &AppState, actor: &[u8; 32], body: &str, ts: i64) {
    let outcome = fauna_nest::segments::mail::append_record(
        &state.mail_segments,
        &state.db,
        actor,
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            body.as_bytes().to_vec(),
        ),
        &fauna_mls::wrapped_blob::SealedRecordBytes::carried_at_rest_unchecked(
            b"sealed-index-hint".to_vec(),
        ),
        common::floor(ts),
    )
    .await
    .expect("append mail record");
    assert!(outcome.inserted, "each body is its own record");
}

/// One coordinator pass for `OWNER`'s mail from `nest`, driven directly so the
/// caller sees the pass's own result (the hosting worker logs and carries on).
async fn mail_pass(
    nest: &Arc<AppState>,
) -> anyhow::Result<fauna_sync_engine::segment_backup::RunReport> {
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(nest), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    coordinator.run_once(&dest_row, KIND).await
}

/// One coordinator pass over a covered folder, likewise.
async fn folder_pass(
    nest: &Arc<AppState>,
    folder_id: i64,
) -> anyhow::Result<fauna_nest::segment_backup::FolderRunReport> {
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(nest), OWNER)
        .await
        .unwrap()
        .expect("owner is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    coordinator.run_folder_once(&dest_row, folder_id).await
}

/// The destination's live custody for `OWNER` in one set, as
/// `path → manifest hash`.
async fn live_custody_in(
    dest: &Arc<AppState>,
    set_name: &str,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    dest.db
        .list_backup_custody_in_set(&OWNER, set_name)
        .await
        .expect("custody list for the set")
        .into_iter()
        .map(|r| (r.path.expect("a custody path"), r.manifest_hash))
        .collect()
}

/// `OWNER`'s writer seat at `dest`, as its client lists it.
async fn writer_seat(dest: &Arc<AppState>) -> Vec<fauna_protocol::backup::WriterGrantItem> {
    client_call::<_, fauna_protocol::backup::WriterGrantListReply>(
        dest,
        OWNER,
        "fauna.backup.writer_grant.list",
        &fauna_protocol::backup::WriterGrantListRequest::default(),
    )
    .await
    .expect("the owner lists its writer seat")
    .grants
}

/// **Two source boxes of one owner, one destination: the second box is refused
/// at its enrollment, and the first box's copy is never touched.**
///
/// A destination keeps an owner's reserved-kind custody copy under
/// `(owner, kind)` — `reserved_backup_set_name` puts no source nest in the
/// set's name — and each box numbers its own segments from 1, so two granted
/// boxes would both write `<owner hex>/seg-00000001.dat` and supersede each
/// other's live copy. The writer seat is what prevents it:
///
/// - the second box's `writer_grant.register` is refused
///   `fauna.backup.writer_seat_held`, the reply naming the first box, because
///   the owner's custody here holds a live path;
/// - a second box that dials anyway is refused at the gate, typed, and creates
///   nothing;
/// - the destination's live segment at the shared path stays the first box's
///   bytes, with nothing retained (nothing was superseded).
#[tokio::test]
async fn a_second_source_box_of_one_owner_is_refused_at_one_destination() {
    let (_a_url, a, _a_blobs) = start_nest().await;
    let (_b_url, b, _b_blobs) = start_nest().await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    for nest in [&a, &b, &dest] {
        register_user(nest, OWNER, "alice").await;
    }
    append_one_mail(&a, &OWNER, "sealed-body-held-on-box-a", 1_715_000_000_000).await;
    append_one_mail(&b, &OWNER, "sealed-body-held-on-box-b", 1_715_000_100_000).await;
    enroll(&a, &dest, &d_url).await;
    let a_id = a.nest_identity.public_key_bytes();

    // Two unrelated numberings that collide: each box's first segment is 1.
    let a_ids = live_segment_ids(&a, OWNER).await;
    assert_eq!(a_ids.len(), 1);
    assert_eq!(
        a_ids,
        live_segment_ids(&b, OWNER).await,
        "each box numbers its own segments from the same start"
    );
    let seg_id = a_ids[0];
    let scope_hex = hex::encode(OWNER);
    let dat_path = segment_rel_path(&scope_hex, seg_id);

    // ── Box A's pass ─────────────────────────────────────────────────────
    let report = mail_pass(&a).await.expect("box A's pass");
    assert_eq!(report.uploaded_segments, vec![seg_id]);
    let a_dat = std::fs::read(a.mail_segments.segment_file_path(&OWNER, seg_id)).unwrap();
    let b_dat = std::fs::read(b.mail_segments.segment_file_path(&OWNER, seg_id)).unwrap();
    assert_ne!(a_dat, b_dat, "the two boxes hold different mail");
    assert_eq!(open_custody_path(&dest, &d_url, &dat_path).await, a_dat);
    let after_a = live_custody_in(&dest, "__mail").await;

    // ── Box B's enrollment stops at its grant step ───────────────────────
    let refused = grant_writer(&b, &dest)
        .await
        .expect_err("the second box's grant registration is refused");
    assert_eq!(refused.code, "fauna.backup.writer_seat_held");
    let Some(fauna_protocol::Value::Map(details)) = refused.details.as_deref() else {
        panic!("the refusal carries a details map: {refused:?}");
    };
    assert_eq!(
        details.get(fauna_protocol::backup::WRITER_SEAT_HELD_HOLDER),
        Some(&fauna_protocol::Value::String(hex::encode(a_id))),
        "the refusal names the box that holds the seat"
    );
    let seat = writer_seat(&dest).await;
    assert_eq!(seat.len(), 1, "an owner has one writer seat");
    assert_eq!(seat[0].writer_nest_id, hex::encode(a_id));
    assert!(!seat[0].revoked);

    // ── A second box that dials anyway is refused at the gate ────────────
    enroll_source(&b, &dest, &d_url).await;
    let err = mail_pass(&b)
        .await
        .expect_err("an unseated box's pass is refused");
    assert!(
        format!("{err:#}").contains("writer_not_seated"),
        "the refusal is the writer gate's typed one: {err:#}"
    );

    // ── Box A's copy is never touched ────────────────────────────────────
    assert_eq!(
        live_custody_in(&dest, "__mail").await,
        after_a,
        "every live path is still the one box A wrote"
    );
    assert_eq!(
        open_custody_path(&dest, &d_url, &dat_path).await,
        a_dat,
        "the destination's live segment is still box A's bytes"
    );
    assert!(
        dest.db
            .list_backup_custody_generations_in_set(&OWNER, "__mail")
            .await
            .unwrap()
            .is_empty(),
        "nothing was superseded, so nothing is retained"
    );
    let report = mail_pass(&a).await.expect("box A's second pass");
    assert!(
        report.uploaded_segments.is_empty() && !report.manifest_uploaded,
        "box A has nothing to re-send: {report:?}"
    );
}

/// **A source box that rotates its deployment seed is refused at its
/// destination until its owner carries the seat, and the carry keeps a covered
/// folder's mirror in one set.**
///
/// After `rotate_deployment_seed` the box proves a new nest id. The
/// destination's writer gate (`require_backup_writer`) keys on the
/// handshake-verified id, and a covered folder's mirror set
/// (`folder_backup_set_name`) carries that id in its name.
///
/// - the rotated box's next mail pass and folder pass are both refused the
///   typed `fauna.backup.writer_not_seated`, and create nothing: the seat names
///   the predecessor until the owner moves it;
/// - `attach_folder` answers the successor's set name — the name the
///   destination holds once the seat has moved;
/// - the owner's device carries the seat — the shared
///   `fauna_client_backup::seat_carry`, over its own connection to the
///   destination, verifying the box's rotation chain and then registering
///   `writer_grant.register { succeeds: <predecessor> }` — which moves the seat
///   in one transaction: the successor is the one writer listed, the
///   predecessor's authority is gone, and the folder's mirror set is renamed
///   under the successor; a second carry finds nothing left to do;
/// - the next passes are accepted, send only what is new, and the folder's
///   pre-rotation and post-rotation files rest in ONE set;
/// - the owner's next audit pass over the destination, its list re-keyed
///   under the successor, passes — the renamed mirror set included, against
///   the device's replica of the folder — with no inclusion alarm.
///
/// The source's status row still reads a stale upload time and a backlog while
/// refused: the row's `writer_state` is the source half's build
/// (`backup-destinations.md` § Per-destination status read).
#[tokio::test]
async fn a_rotated_source_box_is_refused_until_its_owner_carries_the_seat() {
    // ── A source box on disk under a known seed, with the keypair row the
    //    rotation swaps ───────────────────────────────────────────────────
    let dir = kept_tempdir();
    let (old_seed, new_seed) = ([0xA1u8; 32], [0xA2u8; 32]);
    let old_id = SigningKey::from_bytes(&old_seed).verifying_key().to_bytes();
    let new_id = SigningKey::from_bytes(&new_seed).verifying_key().to_bytes();
    let db = Arc::new(CacheDb::open(dir.join("nest.db")).unwrap());
    db.set_nest_keypair(&old_seed, &old_id).await.unwrap();
    let (_s_url, source, _s_blobs) = start_nest_on(dir.clone(), Arc::clone(&db), old_seed).await;
    let (d_url, dest, _d_blobs) = start_nest().await;

    register_user(&source, OWNER, "alice").await;
    register_user(&dest, OWNER, "alice").await;
    append_one_mail(
        &source,
        &OWNER,
        "sealed-body-before-rotation",
        1_715_000_000_000,
    )
    .await;
    enroll(&source, &dest, &d_url).await;

    let folder_id = source
        .db
        .create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
        .await
        .expect("create the ordinary folder");
    let (cat_hash, cat_manifest, _) =
        seed_folder_file(&source, folder_id, "photos/cat.jpg", 0x41).await;
    let attach = |nest: &Arc<AppState>| {
        let nest = Arc::clone(nest);
        async move {
            client_call::<_, fauna_protocol::backup::AttachFolderReply>(
                &nest,
                OWNER,
                "fauna.backup.destination.attach_folder",
                &fauna_protocol::backup::AttachFolderRequest {
                    destination_id: DEST_ID.to_string(),
                    folder_id,
                    extra: Default::default(),
                },
            )
            .await
            .expect("owner attaches the folder")
        }
    };
    let old_set = format!("__folder/{}/{folder_id}", hex::encode(old_id));
    let new_set = format!("__folder/{}/{folder_id}", hex::encode(new_id));
    assert_eq!(attach(&source).await.folder_set, old_set);

    // ── Before the rotation: both axes back up ───────────────────────────
    mail_pass(&source)
        .await
        .expect("the pre-rotation mail pass");
    let report = folder_pass(&source, folder_id)
        .await
        .expect("the pre-rotation folder pass");
    assert_eq!(report.uploaded_paths, 1);
    let before = status(&source, OWNER).await;
    let uploaded_at = before.destinations[0].last_upload_time;
    assert!(uploaded_at.is_some());
    assert_eq!(before.destinations[0].backlog_count, 0);
    let cat_path = hex::encode(cat_hash);
    let mirrored_before = live_custody_in(&dest, &old_set).await;
    assert_eq!(mirrored_before.keys().collect::<Vec<_>>(), vec![&cat_path]);
    let mirror_set_id = dest
        .db
        .get_folder_for_actor(&old_set, &OWNER)
        .await
        .unwrap()
        .expect("the mirror set under the predecessor")
        .id;

    // ── The rotation: the ceremony's atomic decision point, then the same
    //    database and data dir served under the successor ─────────────────
    source.mail_segments.finalize_open(&OWNER).await.unwrap();
    let outcome = db
        .rotate_deployment_seed(
            &zeroize::Zeroizing::new(old_seed),
            &zeroize::Zeroizing::new(new_seed),
        )
        .await
        .unwrap();
    assert!(outcome.is_ok(), "the rotation is refused: {outcome:?}");
    drop(source);
    let (s_url, source, _s_blobs) = start_nest_on(dir.clone(), db, new_seed).await;
    assert_eq!(source.nest_identity.public_key_bytes(), new_id);

    // New content on both axes, so the next pass has something to send.
    append_one_mail(
        &source,
        &OWNER,
        "sealed-body-after-rotation",
        1_715_000_200_000,
    )
    .await;
    let (dog_hash, dog_manifest, _) =
        seed_folder_file(&source, folder_id, "photos/dog.jpg", 0x42).await;
    let dog_path = hex::encode(dog_hash);

    // ── Until the owner carries it, the seat names the predecessor ───────
    let seat = writer_seat(&dest).await;
    assert_eq!(seat.len(), 1);
    assert_eq!(seat[0].writer_nest_id, hex::encode(old_id));
    let mail_err = mail_pass(&source)
        .await
        .expect_err("the rotated box's mail pass is refused");
    assert!(
        format!("{mail_err:#}").contains("writer_not_seated"),
        "the refusal is the writer gate's typed one: {mail_err:#}"
    );
    let folder_err = folder_pass(&source, folder_id)
        .await
        .expect_err("the rotated box's folder pass is refused");
    assert!(
        format!("{folder_err:#}").contains("writer_not_seated"),
        "the refusal is the writer gate's typed one: {folder_err:#}"
    );
    assert!(
        dest.db
            .get_folder_for_actor(&new_set, &OWNER)
            .await
            .unwrap()
            .is_none(),
        "a refused pass creates no set"
    );

    // The source's status row while refused: the stale upload time and a
    // backlog (its `writer_state` field is the source half's build).
    let refused = status(&source, OWNER).await;
    assert_eq!(
        refused.destinations,
        vec![fauna_protocol::backup::BackupDestinationStatusItem {
            destination_id: DEST_ID.to_string(),
            last_upload_time: uploaded_at,
            backlog_count: 1,
            ..Default::default()
        }],
    );

    // The box names the set the destination will hold once the seat has moved.
    let reattach = attach(&source).await;
    assert!(!reattach.attached, "the coverage row is the same one");
    assert_eq!(reattach.folder_set, new_set);

    // ── A plain registration of the successor is a second box: refused ───
    let second_box = grant_writer(&source, &dest)
        .await
        .expect_err("without `succeeds` the successor is a second box");
    assert_eq!(second_box.code, "fauna.backup.writer_seat_held");

    // ── The owner's device carries the seat, over its own connection ─────
    // The shared carry, as the audit pass and the trust facet run it: the
    // seat read at the destination, the chain read from the box the device
    // is bound to (the successor), verified, and the handover registered
    // naming the predecessor.
    let connector = OwnerConnector(vec![
        (s_url.clone(), Arc::clone(&source)),
        (d_url.clone(), Arc::clone(&dest)),
    ]);
    let dest_seam = connector.seam(&d_url);
    let source_seam = connector.seam(&s_url);
    let chain = fauna_client_backup::seat_carry::SeamChain(source_seam.as_ref());
    assert_eq!(
        fauna_client_backup::seat_carry::carry_writer_seat(dest_seam.as_ref(), &new_id, &chain)
            .await
            .expect("the carry completes"),
        fauna_client_backup::seat_carry::SeatCarry::Carried,
    );
    assert_eq!(
        fauna_client_backup::seat_carry::carry_writer_seat(dest_seam.as_ref(), &new_id, &chain)
            .await
            .expect("a repeat completes"),
        fauna_client_backup::seat_carry::SeatCarry::AlreadyHeld,
        "a second device carrying finds nothing left to do"
    );

    let seat = writer_seat(&dest).await;
    assert_eq!(seat.len(), 1, "the successor alone is listed");
    assert_eq!(seat[0].writer_nest_id, hex::encode(new_id));
    assert!(!seat[0].revoked);
    assert!(
        !dest
            .db
            .has_backup_writer_grant(&OWNER, &old_id)
            .await
            .unwrap(),
        "the predecessor's authority ended with the handover"
    );
    assert!(
        dest.db
            .get_folder_for_actor(&old_set, &OWNER)
            .await
            .unwrap()
            .is_none(),
        "no set is left under the predecessor's name"
    );
    assert_eq!(
        dest.db
            .get_folder_for_actor(&new_set, &OWNER)
            .await
            .unwrap()
            .expect("the mirror set under the successor")
            .id,
        mirror_set_id,
        "the same set, renamed: no custody row moved"
    );
    assert_eq!(live_custody_in(&dest, &new_set).await, mirrored_before);

    // ── Both axes resume, sending only what is new ───────────────────────
    let report = mail_pass(&source).await.expect("the carried mail pass");
    assert_eq!(report.uploaded_segments.len(), 1, "{report:?}");
    assert_eq!(
        status(&source, OWNER).await.destinations[0].backlog_count,
        0
    );
    let report = folder_pass(&source, folder_id)
        .await
        .expect("the carried folder pass");
    assert_eq!(
        report.uploaded_paths, 1,
        "only the new file is sent — nothing already mirrored is sent again"
    );
    let mirrored = live_custody_in(&dest, &new_set).await;
    let mut both = vec![&cat_path, &dog_path];
    both.sort();
    assert_eq!(
        mirrored.keys().collect::<Vec<_>>(),
        both,
        "the pre-rotation and post-rotation files rest in ONE set, under the successor"
    );
    assert_eq!(
        mirrored[&cat_path], mirrored_before[&cat_path],
        "the pre-rotation file is the copy mirrored before the rotation"
    );
    assert!(
        dest.db
            .get_folder_for_actor(&old_set, &OWNER)
            .await
            .unwrap()
            .is_none()
    );

    // ── The owner's next audit pass over the carried destination passes ──
    // The device's list as the re-file leaves it: the enrollment row and the
    // coverage row, its `folder_name` re-keyed under the successor — so the
    // mirror plane routes the renamed set, against this device's replica of
    // the folder (both files' heads, recorded long enough ago that each must
    // be mirrored).
    let now = fauna_core::data::Timestamp::now_secs();
    let long_ago = now - 3 * fauna_client_backup::audit::FRESHNESS_SLACK_SECS;
    let enrolled = fauna_core::data::BackupDestination {
        destination_id: DEST_ID.to_string(),
        destination_nest_url: d_url.clone(),
        destination_actor_pubkey: dest.nest_identity.public_key_bytes(),
        folder_name: "__mail".into(),
        added_at: long_ago as u64,
        ..Default::default()
    };
    let covered = fauna_core::data::BackupDestination {
        folder_name: new_set.clone(),
        ..enrolled.clone()
    };
    let replica = fauna_client_backup::audit::FolderIndex {
        consistent_at: now,
        entries: [(cat_hash, cat_manifest), (dog_hash, dog_manifest)]
            .into_iter()
            .map(
                |(path, manifest)| fauna_client_backup::audit::FolderIndexEntry {
                    path_hash_hex: hex::encode(path),
                    manifest_hash_hex: hex::encode(manifest.digest()),
                    recorded_at: long_ago,
                },
            )
            .collect(),
    };
    let inclusion = OwnerInclusion {
        dest_url: d_url.clone(),
        folder_set: new_set.clone(),
        replica,
    };
    let store = MemAuditStore::default();
    let pass = fauna_client_backup::audit::run_audit_pass(
        &connector,
        &inclusion,
        &store,
        &[enrolled, covered],
        &s_url,
        &new_id,
        None,
        now,
    )
    .await;
    assert!(pass.degradations.is_empty(), "{:?}", pass.degradations);
    assert_eq!(pass.records.len(), 1);
    assert_eq!(
        pass.records[0].verdict,
        Some(fauna_client_backup::audit::AuditVerdict::Passed),
        "the carried destination passes, the renamed set included: {:?}",
        pass.records[0]
    );
    assert_eq!(
        pass.records[0].state.seat_settled_under,
        Some(hex::encode(new_id)),
        "the pass found the seat the successor's"
    );
}

/// An owner-authed [`BackupDestinationConnector`] over in-process nests — each
/// URL's connection is [`OwnerLink`] into that nest's client router, proving
/// the nest's own identity, exactly what the owner's device's connection to it
/// proves.
///
/// [`BackupDestinationConnector`]: fauna_client_backup::trust::BackupDestinationConnector
struct OwnerConnector(Vec<(String, Arc<AppState>)>);

impl OwnerConnector {
    fn nest(&self, url: &str) -> Option<&Arc<AppState>> {
        self.0.iter().find(|(u, _)| u == url).map(|(_, n)| n)
    }

    fn seam(&self, url: &str) -> Arc<dyn fauna_client_backup::trust::BackupNestSeam> {
        let nest = self.nest(url).expect("a nest at the url");
        Arc::new(DestinationSeam {
            client: fauna_client_backup::BackupClient::new(OwnerLink {
                nest: Arc::clone(nest),
                tear: Tear::None,
            }),
        })
    }
}

#[async_trait::async_trait]
impl fauna_client_backup::trust::BackupDestinationConnector for OwnerConnector {
    async fn connect(
        &self,
        url: &str,
    ) -> Result<fauna_client_backup::trust::DestinationConnection, String> {
        let nest = self.nest(url).ok_or_else(|| format!("no route to {url}"))?;
        Ok(fauna_client_backup::trust::DestinationConnection {
            seam: self.seam(url),
            bound_nest_id: nest.nest_identity.public_key_bytes(),
        })
    }
}

/// The owner's inclusion inputs: the destination's open byte routes, the
/// granted key, and this device's replica of one covered folder.
struct OwnerInclusion {
    dest_url: String,
    folder_set: String,
    replica: fauna_client_backup::audit::FolderIndex,
}

impl fauna_client_backup::audit::BackupInclusionSource for OwnerInclusion {
    fn fetcher(&self, url: &str) -> Arc<dyn fauna_core::file_download::BlobFetcher> {
        assert_eq!(
            url, self.dest_url,
            "inclusion reads the audited destination"
        );
        Arc::new(OpenRouteFetcher {
            base: url.to_string(),
            http: reqwest::Client::new(),
        })
    }

    fn keys(&self) -> fauna_core::file_download::FileDownloadKeys {
        fauna_core::file_download::FileDownloadKeys::owner(
            fauna_core::crypto::OwnerSealKey::SourceNest(NestBackupKey::from_bytes(GRANTED_KEY)),
        )
    }

    fn folder_index(&self, folder_set: &str) -> Option<fauna_client_backup::audit::FolderIndex> {
        (folder_set == self.folder_set).then(|| self.replica.clone())
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// Phase 3c — the nest-held pull-back
// ═════════════════════════════════════════════════════════════════════════════
//
// `segment-backup-protocol.md` § Client-device custodian (pull) → *Restore* →
// *The nest-held pull-back*, the proof obligation: the real coordinator backs
// A up to B; A is lost; a rebuilt A′ (fresh, and in a second arm under A's own
// identity) is filled from B by the owner's app through the shared driver
// `run_reseed` over the pull-back leg; the mail reads back through A′'s
// ordinary read path, the covered folder comes back as a live folder, the
// delivered custody is byte-identical to B's, a torn run resumes, the
// generation the owner rolled back on B is what arrives — and the post-
// ceremony re-enrollment carries B's writer seat to A′.

/// The rebuilt nest's write device, registered there for the custody records.
const RESTORE_DEVICE: [u8; 32] = [0x0E; 32];

/// An owner connection whose error the driver can classify — what
/// `run_reseed` and the enroll sequence need of a transport. `tear` fails the
/// first `.meta` custody record, after its `.dat` landed.
struct PullLink {
    nest: Arc<AppState>,
    tear: std::sync::atomic::AtomicBool,
}

impl PullLink {
    fn to(nest: &Arc<AppState>) -> Self {
        Self {
            nest: nest.clone(),
            tear: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[derive(Debug)]
enum LinkError {
    Rejected(RpcError),
    Torn,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(e) => write!(f, "{}: {:?}", e.code, e.message),
            Self::Torn => write!(f, "torn: the connection dropped before the sidecar"),
        }
    }
}

impl fauna_protocol::RpcErrorClass for LinkError {
    fn is_rejection(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
    fn as_rpc_error(&self) -> Option<&RpcError> {
        match self {
            Self::Rejected(e) => Some(e),
            Self::Torn => None,
        }
    }
}

impl fauna_protocol::RpcRequester for PullLink {
    type Error = LinkError;

    async fn request<Req, Reply>(
        &self,
        kind: &'static str,
        payload: Req,
    ) -> Result<Reply, LinkError>
    where
        Req: Serialize,
        Reply: DeserializeOwned,
    {
        let bytes = bytes::Bytes::from(encode_canonical(&payload).unwrap().to_vec());
        if kind == "fauna.sync.changes.record" {
            let req: fauna_protocol::sync::SyncChangeRecordRequest = decode(&bytes).unwrap();
            if req.path.ends_with(".meta")
                && self.tear.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(LinkError::Torn);
            }
        }
        let meta = self
            .nest
            .rpc_router
            .kind_meta(kind)
            .unwrap_or_else(|| panic!("kind not registered: {kind}"));
        let out = (meta.handler)(self.nest.clone(), OWNER, bytes)
            .await
            .map_err(LinkError::Rejected)?;
        Ok(decode(&out).unwrap())
    }
}

fauna_client_backup::impl_backup_nest_seam!(struct PullSeam<PullLink>);

/// The driver's seam over the pull-back leg for this test's transport — the
/// production impl is for the owner's `NestClient`.
struct PullLeg<'a>(fauna_sync_engine::reseed_pull::NestPullBack<'a, SourceBytePlane, PullLink>);

#[async_trait::async_trait]
impl fauna_client_backup::reseed::ReseedDeliveryLeg for PullLeg<'_> {
    async fn deliver(&self) -> Result<fauna_client_backup::reseed::DeliveredCorpus, String> {
        self.0.deliver_corpus().await
    }

    async fn sign_rehome(
        &self,
        set: &fauna_client_backup::reseed::DeliveredSet,
    ) -> fauna_client_backup::reseed::FolderRehome {
        self.0.sign_folder_rehome(set).await
    }
}

/// Every live custody row a nest holds for `OWNER` as `(set, path, manifest
/// hash)`, sorted — the byte-identity comparison's key.
async fn custody_identity(nest: &Arc<AppState>) -> Vec<(String, Option<String>, Vec<u8>)> {
    let mut rows: Vec<_> = nest
        .db
        .list_backup_custody(&OWNER, None, 0)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.folder_name, r.path, r.manifest_hash))
        .collect();
    rows.sort();
    rows
}

/// **A backup held on another nest comes back onto a rebuilt nest, and the
/// destination's writer seat moves to it.** `same_identity` rebuilds A′ under
/// A's own identity (the deployment seed); otherwise A′ is a fresh box.
async fn a_nest_held_backup_is_pulled_back(same_identity: bool) {
    let owner_key_bytes = NestBackupKey::derive(&OWNER_SECRET).to_bytes();
    let grant = |nest: &Arc<AppState>| {
        let nest = Arc::clone(nest);
        async move {
            let reply: NestKeyGrantReply = client_call(
                &nest,
                OWNER,
                "fauna.backup.nest_key.grant",
                &NestKeyGrantRequest {
                    nest_backup_key: serde_bytes::ByteBuf::from(owner_key_bytes.to_vec()),
                    extra: Default::default(),
                },
            )
            .await
            .unwrap();
            assert!(reply.ok);
        }
    };

    // ── A backs its mail and a covered folder up to B ────────────────────
    let mut a_secret = [0u8; 32];
    getrandom::fill(&mut a_secret).unwrap();
    let (_a_url, a, _a_blobs) = start_nest_on(
        kept_tempdir(),
        Arc::new(CacheDb::open_in_memory().unwrap()),
        a_secret,
    )
    .await;
    let b_dir = kept_tempdir();
    let mut b_secret = [0u8; 32];
    getrandom::fill(&mut b_secret).unwrap();
    let b_db = Arc::new(CacheDb::open(b_dir.join("nest.db")).unwrap());
    let (b_url, b, _b_blobs) = start_nest_on(b_dir.clone(), b_db, b_secret).await;
    register_user(&a, OWNER, "alice").await;
    register_user(&b, OWNER, "alice").await;
    file_mail(&a, &OWNER, FILED).await;
    grant(&a).await;
    let reg: DestinationRegisterReply = client_call(
        &a,
        OWNER,
        "fauna.backup.destination.register",
        &DestinationRegisterRequest {
            destination_id: DEST_ID.to_string(),
            destination_nest_url: b_url.clone(),
            destination_nest_id: hex::encode(b.nest_identity.public_key_bytes()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(reg.ok);
    assert!(grant_writer(&a, &b).await.unwrap().ok);

    let photos =
        a.db.create_folder_with_options("Photos", &OWNER, fauna_nest::db::FolderOptions::default())
            .await
            .unwrap();
    let (cat, cat_v1, _) = seed_folder_file(&a, photos, "photos/cat.jpg", 0x41).await;
    let (dog, dog_v1, _) = seed_folder_file(&a, photos, "photos/dog.jpg", 0x51).await;
    let attach: fauna_protocol::backup::AttachFolderReply = client_call(
        &a,
        OWNER,
        "fauna.backup.destination.attach_folder",
        &fauna_protocol::backup::AttachFolderRequest {
            destination_id: DEST_ID.to_string(),
            folder_id: photos,
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    let folder_set = attach.folder_set;
    let sweep = |nest: &Arc<AppState>| {
        let nest = Arc::clone(nest);
        async move {
            NestBackupWorker::new(nest, std::time::Duration::MAX)
                .run_once()
                .await
                .expect("the hosting sweep runs");
        }
    };
    sweep(&a).await;

    // The owner overwrites the cat, the sweep mirrors it, and then — wanting
    // the old cat back — rolls B's copy back to it. The pull takes B's live
    // head, so the rolled-back generation is the one that must arrive.
    let (_, cat_v2, _) = seed_folder_file(&a, photos, "photos/cat.jpg", 0x42).await;
    assert_ne!(cat_v2, cat_v1);
    sweep(&a).await;
    let retained: fauna_protocol::backup::GenerationListReply = client_call(
        &b,
        OWNER,
        "fauna.backup.generation.list",
        &fauna_protocol::backup::GenerationListRequest::default(),
    )
    .await
    .unwrap();
    let old_cat = retained
        .generations
        .iter()
        .find(|g| g.manifest_hash == hex::encode(cat_v1.digest()))
        .expect("B retains the overwritten cat");
    let restored: fauna_protocol::backup::GenerationRestoreReply = client_call(
        &b,
        OWNER,
        "fauna.backup.generation.restore",
        &fauna_protocol::backup::GenerationRestoreRequest {
            folder_name: old_cat.folder_name.clone(),
            path_hash: old_cat.path_hash.clone(),
            manifest_hash: old_cat.manifest_hash.clone(),
            extra: Default::default(),
        },
    )
    .await
    .unwrap();
    assert!(restored.restored);

    let mail_on_a: Vec<_> = {
        let mut out = Vec::new();
        for mailbox in ["INBOX", "Archive"] {
            out.push(serve_mailbox(&a, &OWNER, mailbox).await);
        }
        out
    };
    let lost_box = a.nest_identity.public_key_bytes();

    // ── A is lost; B reboots (its boot scrub runs) ───────────────────────
    drop(a);
    let b_db = Arc::new(CacheDb::open(b_dir.join("nest.db")).unwrap());
    let (b_url, b, _b_blobs) = start_nest_on(b_dir.clone(), b_db, b_secret).await;
    assert!(
        custody_identity(&b)
            .await
            .iter()
            .filter(|(set, _, _)| *set == folder_set)
            .all(|(_, path, _)| path.is_some()),
        "the boot scrub keeps a mirror row's leaf: the pull-back addresses by it"
    );

    // ── A′, enrolled: the owner's sign-in and a write device ─────────────
    let a2_secret = if same_identity {
        a_secret
    } else {
        let mut s = [0u8; 32];
        getrandom::fill(&mut s).unwrap();
        s
    };
    let (_a2_url, a2, _a2_blobs) = start_nest_on(
        kept_tempdir(),
        Arc::new(CacheDb::open_in_memory().unwrap()),
        a2_secret,
    )
    .await;
    register_user(&a2, OWNER, "alice").await;
    register_device(&a2, RESTORE_DEVICE).await;
    // The seed-holding process prepares the folder's target set first
    // (`writer-signed-change-records.md` ruling (7)(a)(i)).
    let target_folder = prepare_target(&a2, "Photos").await;

    // ── The ceremony: run_reseed over the pull-back leg ─────────────────
    // The folder's name comes off the lost box's coverage row, as the
    // account plane keeps it.
    let coverage = [fauna_core::data::BackupDestination {
        destination_id: DEST_ID.to_string(),
        folder_name: folder_set.clone(),
        folder_display_name: Some("Photos".to_string()),
        ..Default::default()
    }];
    let names =
        fauna_sync_engine::reseed_pull::folder_names_from_coverage(&coverage, DEST_ID, None);
    let seam = PullSeam {
        client: fauna_client_backup::BackupClient::new(PullLink::to(&b)),
    };
    let fetcher = OpenRouteFetcher {
        base: b_url.clone(),
        http: reqwest::Client::new(),
    };
    let sink = SourceBytePlane(a2.clone());
    let target_link = PullLink::to(&a2);
    let leg = PullLeg(
        fauna_sync_engine::reseed_pull::NestPullBack::new(
            fauna_sync_engine::reseed_pull::PullBackDestination {
                seam: &seam,
                bytes: &fetcher,
            },
            fauna_sync_engine::reseed_pull::PullBackTarget {
                bytes: &sink,
                nest: &target_link,
                device_id: hex::encode(RESTORE_DEVICE),
            },
            OWNER,
        )
        .with_folder_names(names)
        .with_rehome_signing(fauna_client_sync::RecordSigning {
            signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
                &owner_key(),
            )),
            set_nonce: fauna_client_sync::SetNonceSource::Fixed(TARGET_NONCE),
        }),
    );
    let target = fauna_client_backup::BackupClient::new(PullLink::to(&a2));

    // Torn after a `.dat` and before its `.meta`: nothing is materialized.
    target_link
        .tear
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let torn =
        fauna_client_backup::reseed::run_reseed(&target, owner_key_bytes.to_vec(), &leg).await;
    assert!(
        matches!(
            torn,
            Err(fauna_client_backup::reseed::ReseedError::Delivery(_))
        ),
        "{torn:?}"
    );
    assert!(serve_mailbox(&a2, &OWNER, "INBOX").await.is_empty());

    // The re-run resumes to the whole corpus.
    let outcome = fauna_client_backup::reseed::run_reseed(&target, owner_key_bytes.to_vec(), &leg)
        .await
        .expect("the re-run completes");
    assert!(outcome.is_whole(), "{outcome:?}");

    // The mail reads back through A′'s ordinary read path, where it was.
    for (i, mailbox) in ["INBOX", "Archive"].into_iter().enumerate() {
        assert_eq!(
            serve_mailbox(&a2, &OWNER, mailbox).await,
            mail_on_a[i],
            "{mailbox}: same messages, same UIDs, same flags"
        );
    }
    // The folder is live, its rows the source's: path hashes, sealed names,
    // and the cat B was rolled back to.
    let mut rows = live_rows(&a2, target_folder).await;
    rows.sort();
    let mut expected = vec![
        (
            cat,
            Some(b"sealed-name-label".to_vec()),
            Some(cat_v1.digest()),
        ),
        (
            dog,
            Some(b"sealed-name-label".to_vec()),
            Some(dog_v1.digest()),
        ),
    ];
    expected.sort();
    assert_eq!(rows, expected);
    // What A′ holds as custody is what B holds, row for row.
    assert_eq!(custody_identity(&a2).await, custody_identity(&b).await);

    // ── The post-ceremony duty: B becomes A′'s destination ──────────────
    let restored_box = a2.nest_identity.public_key_bytes();
    assert_eq!(restored_box == lost_box, same_identity);
    let store = fauna_client_config::test_helpers::FakeBackupStateStore::empty();
    let row = fauna_core::data::BackupDestination {
        destination_id: DEST_ID.to_string(),
        destination_nest_url: b_url.clone(),
        destination_actor_pubkey: b.nest_identity.public_key_bytes(),
        display_name: Some("Off-site".to_string()),
        ..Default::default()
    };
    let list = fauna_client_config::reenroll_nest_destination_after_reseed(
        PullLink::to(&a2),
        PullLink::to(&b),
        &store,
        OWNER_SECRET,
        restored_box,
        &row,
        &outcome,
    )
    .await
    .expect("a whole outcome re-enrolls")
    .expect("the destination accepts the restored nest");
    assert_eq!(list.len(), 1);
    assert_eq!(
        writer_seat(&b)
            .await
            .iter()
            .map(|g| g.writer_nest_id.clone())
            .collect::<Vec<_>>(),
        vec![hex::encode(restored_box)],
        "the seat names the restored nest alone"
    );
    // And A′'s next backup pass to B is accepted.
    let coordinator = NestBackupCoordinator::open_for_owner(Arc::clone(&a2), OWNER)
        .await
        .unwrap()
        .expect("A′ is enrolled");
    let dest_row = coordinator.destinations()[0].clone();
    coordinator
        .run_once(&dest_row, KIND)
        .await
        .expect("B accepts the restored nest's pass");
}

#[tokio::test]
async fn a_nest_held_backup_is_pulled_back_onto_a_fresh_nest_and_its_seat_moves() {
    a_nest_held_backup_is_pulled_back(false).await;
}

#[tokio::test]
async fn a_nest_held_backup_is_pulled_back_onto_a_rebuild_under_the_lost_boxs_identity() {
    a_nest_held_backup_is_pulled_back(true).await;
}

/// The audit's client-local store, in memory.
#[derive(Default)]
struct MemAuditStore(std::sync::Mutex<fauna_client_backup::audit::AuditStateSnapshot>);

impl fauna_client_backup::audit::AuditStateStore for MemAuditStore {
    fn load(&self) -> Result<fauna_client_backup::audit::AuditStateSnapshot, String> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn save(
        &self,
        snapshot: &fauna_client_backup::audit::AuditStateSnapshot,
    ) -> Result<(), String> {
        *self.0.lock().unwrap() = snapshot.clone();
        Ok(())
    }
}
