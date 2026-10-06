//! Cross-set media aggregation round-trip — the `fauna.media.list` RPC backing
//! the Media page's default all-media view (`docs/goal/ui/media.md` § State &
//! data shape; spec O-4 settled at nest implementation).
//!
//! Proves end-to-end through the real handler + real `CacheDb`:
//! - the RPC **aggregates** media items across every folder the caller may
//!   read — their **owned** sets *and* a **group-bound shared** set they're a
//!   roster member of — in stable `(folder, path)` order;
//! - it returns **nothing** for an actor with no readable set (no own sets, no
//!   membership) — the same S2-P3 read boundary, no cross-user leak;
//! - **keyset pagination** (`limit` + `cursor` / `next_cursor`) walks the whole
//!   aggregate in bounded pages with no gaps or repeats;
//! - each item carries the backing set's `source_online` reachability verdict
//!   (`chunk_relay::folder_content_reachable`: content resident on the
//!   nest is reachable with no device online).
//!
//! Authority: `docs/goal/ui/media.md` + the readable-set boundary in
//! `docs/goal/architecture/key-material-hierarchy.md` § *Audience: an MLS group
//! at a specific epoch*.

mod common;
use common::dispatch;

use std::sync::Arc;

use bytes::Bytes;

use fauna_mls::types::ChannelId;
use fauna_nest::{db::CacheDb, media_handlers, routes::AppState, rpc_router::RpcRouter};
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    media::{MEDIA_LIST_CURSOR_V2, MediaListReply, MediaListRequest},
};

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    media_handlers::register_media_handlers(&mut b);
    (b.build(), state)
}

fn enc<T: serde::Serialize>(req: &T) -> Bytes {
    Bytes::from(encode_canonical(req).unwrap().to_vec())
}

/// Seed a non-deleted file with a manifest into `folder_id` so
/// `get_files_for_folder` returns it (mirrors a recorded `create` change).
async fn seed_file(db: &CacheDb, actor: &[u8; 32], folder_id: i64, path: &str, size: i64) {
    let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
    let manifest: [u8; 32] = *blake3::hash(format!("manifest:{path}").as_bytes()).as_bytes();
    let device: [u8; 32] = [0xd1u8; 32];
    db.record_sync_change(
        actor,
        &path_hash,
        Some(&manifest),
        size,
        "create",
        Some(folder_id),
        Some(&device),
        Some(path),
    )
    .await
    .unwrap();
}

/// Seed a file into a **Backup**-mode folder's head feed. Since the phase 3
/// head unification (2026-08-17, `file-sync.md` § Membership →
/// *Target state — head unification*) a Backup folder records into
/// `sync_changes` exactly like a Sync folder, so this drives the same metered
/// record path the client upload uses — the one that carries a thumbnail hash.
/// `thumb` is the uploader-recorded thumbnail-blob hash, or `None`.
async fn seed_backup_file(
    db: &CacheDb,
    actor: &[u8; 32],
    folder_id: i64,
    path: &str,
    size: i64,
    thumb: Option<&str>,
) {
    let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
    let manifest: [u8; 32] = *blake3::hash(format!("manifest:{path}").as_bytes()).as_bytes();
    let device: [u8; 32] = [0xd1u8; 32];
    db.record_sync_change_metered(
        actor,
        // owner path: recorder == metered owner, no member cap
        actor,
        None,
        &path_hash,
        Some(&manifest),
        size,
        "create",
        folder_id,
        &device,
        Some(path),
        None,
        thumb,
        None,
        None,
        None,
        i64::MAX,
    )
    .await
    .unwrap();
}

/// Seed a file recorded **with** a thumbnail-blob hash via the metered
/// record path (the client upload path `fauna.sync.changes.record` drives), so
/// `get_files_for_folder` surfaces a non-`None` `thumbnail_hash`. The existing
/// `seed_file` helper records via the unmetered nest-internal twin, which (like
/// the production internal-bookkeeping paths) carries no thumbnail.
async fn seed_file_with_thumb(
    db: &CacheDb,
    actor: &[u8; 32],
    folder_id: i64,
    path: &str,
    size: i64,
    thumb: &str,
) {
    let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
    let manifest: [u8; 32] = *blake3::hash(format!("manifest:{path}").as_bytes()).as_bytes();
    let device: [u8; 32] = [0xd1u8; 32];
    db.record_sync_change_metered(
        actor,
        // owner path: recorder == metered owner, no member cap
        actor,
        None,
        &path_hash,
        Some(&manifest),
        size,
        "create",
        folder_id,
        &device,
        Some(path),
        None,
        Some(thumb),
        None,
        None,
        None,
        i64::MAX,
    )
    .await
    .unwrap();
}

/// List one page as `actor`, with an optional cursor + limit.
async fn list_page(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    cursor: Option<String>,
    limit: u32,
) -> MediaListReply {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.media.list",
        enc(&MediaListRequest {
            cursor,
            limit,
            cursor_version: MEDIA_LIST_CURSOR_V2,
            ..Default::default()
        }),
    )
    .await
    .expect("media.list ok");
    decode(&bytes).unwrap()
}

#[tokio::test]
async fn media_list_aggregates_owned_and_member_sets_paginates_and_gates() {
    let (router, state) = router_and_state().await;
    let a = [0xa1u8; 32]; // owns alpha + beta; member of C's gamma
    let c = [0xccu8; 32]; // owns the shared gamma
    let d = [0xddu8; 32]; // outsider — no sets, no membership

    // A's two owned sets, with files.
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    let beta = state.db.create_folder("beta", &a).await.unwrap();
    seed_file(&state.db, &a, alpha, "a1.jpg", 100).await;
    seed_file(&state.db, &a, alpha, "a2.png", 200).await;
    seed_file(&state.db, &a, beta, "b1.txt", 300).await;

    // C's group-bound shared set "gamma"; A is welcomed onto the derived roster.
    let gamma = state.db.create_folder("gamma", &c).await.unwrap();
    let group_id = vec![0x5au8; 24];
    state
        .db
        .set_folder_mls_group("gamma", &c, Some(&group_id))
        .await
        .unwrap();
    let channel_id = ChannelId::from_group_id(&group_id).0;
    state
        .db
        .register_actor_channel(&c, &channel_id)
        .await
        .unwrap();
    state
        .db
        .register_actor_channel(&a, &channel_id)
        .await
        .unwrap();
    seed_file(&state.db, &c, gamma, "g1.mov", 400).await;

    // (1) A sees the union of owned (alpha, beta) + member (gamma), stably ordered.
    let a_all = list_page(&router, &state, a, None, 0).await;
    // The server order is only the pagination key (hash order, media.md O-4);
    // display order is the client's, so compare the set.
    let mut ids: Vec<(String, String)> = a_all
        .items
        .iter()
        .map(|it| (it.folder.clone(), it.path.clone()))
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            ("alpha".into(), "a1.jpg".into()),
            ("alpha".into(), "a2.png".into()),
            ("beta".into(), "b1.txt".into()),
            ("gamma".into(), "g1.mov".into()),
        ],
        "A aggregates owned + member sets"
    );
    assert!(a_all.next_cursor.is_none(), "the full list fits one page");
    assert!(
        a_all.items.iter().all(|it| it.source_online),
        "content resident on the nest is reachable with no device online \
         (folder_content_reachable)"
    );
    assert!(
        a_all.items.iter().all(|it| it.thumbnail_hash.is_none()),
        "no uploader↔manifest thumbnail association yet (field present, None)"
    );
    let g = a_all.items.iter().find(|it| it.folder == "gamma").unwrap();
    assert_eq!(g.size_bytes, 400, "the member-set file carries its size");

    // (2) C sees only its own gamma — it neither owns nor is a member of alpha/beta.
    let c_all = list_page(&router, &state, c, None, 0).await;
    assert_eq!(
        c_all
            .items
            .iter()
            .map(|it| it.folder.clone())
            .collect::<Vec<_>>(),
        vec!["gamma".to_string()],
        "C reads only the set it owns"
    );

    // (3) An outsider with no readable set sees nothing (no leak).
    let d_all = list_page(&router, &state, d, None, 0).await;
    assert!(
        d_all.items.is_empty() && d_all.next_cursor.is_none(),
        "an actor with no readable set gets an empty aggregate"
    );

    // (4) Keyset pagination: limit=2 walks the 4-item aggregate in two pages,
    //     no gaps, no repeats.
    let page1 = list_page(&router, &state, a, None, 2).await;
    assert_eq!(
        page1
            .items
            .iter()
            .map(|it| (it.folder.clone(), it.path.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("alpha".into(), "a1.jpg".into()),
            ("alpha".into(), "a2.png".into()),
        ]
    );
    let cursor = page1.next_cursor.expect("more pages remain");
    let page2 = list_page(&router, &state, a, Some(cursor), 2).await;
    assert_eq!(
        page2
            .items
            .iter()
            .map(|it| (it.folder.clone(), it.path.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("beta".into(), "b1.txt".into()),
            ("gamma".into(), "g1.mov".into()),
        ]
    );
    assert!(page2.next_cursor.is_none(), "the second page is the last");
}

#[tokio::test]
async fn media_list_rejects_a_malformed_cursor() {
    let (router, state) = router_and_state().await;
    let a = [0xa1u8; 32];
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    seed_file(&state.db, &a, alpha, "a1.jpg", 100).await;

    let err = dispatch(
        &router,
        state.clone(),
        a,
        "fauna.media.list",
        enc(&MediaListRequest {
            cursor: Some("not-hex-!!".into()),
            limit: 0,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a malformed cursor is rejected, not silently ignored");
    assert_eq!(err.code, "fauna.media.invalid_cursor");
}

/// Phase 4 (folders re-model, executing ratified open call #4): a
/// website-published folder appears in Media LIKE ANY OTHER — the website is a
/// serving toggle over the same substrate, not a separate content plane, so
/// the former web-type exclusion is retired (media.md § State & data shape
/// owns the view claim).
#[tokio::test]
async fn media_list_includes_website_folders() {
    let (router, state) = router_and_state().await;
    let a = [0xa1u8; 32];

    // A media set and a website-enabled site set, both seeded into the one
    // `sync_changes` head plane.
    let photos = state.db.create_folder("photos", &a).await.unwrap();
    seed_file(&state.db, &a, photos, "p1.jpg", 100).await;
    let site = state.db.create_folder("site", &a).await.unwrap();
    state
        .db
        .update_folder_for_user(
            "site",
            &a,
            fauna_nest::db::FolderUpdate {
                website_enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    seed_file(&state.db, &a, site, "index.html", 50).await;

    let all = list_page(&router, &state, a, None, 0).await;
    // Hash-ordered pagination (media.md O-4) — compare the set.
    let mut ids: Vec<(String, String)> = all
        .items
        .iter()
        .map(|it| (it.folder.clone(), it.path.clone()))
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            ("photos".into(), "p1.jpg".into()),
            ("site".into(), "index.html".into()),
        ],
        "every user folder's media aggregates — website folders included"
    );
}

/// A **Backup**-mode set (e.g. the Apple "Photo Library") records into the
/// same `sync_changes` head feed as every other folder (phase 3 head
/// unification, 2026-08-17). Media is the user's media library = every
/// user folder (`media.md` § State & data shape → Folder scope), so a backup
/// folder's live files aggregate alongside any other's through the one
/// `get_files_for_folder` projection; a deleted path stays excluded.
#[tokio::test]
async fn media_list_includes_backup_mode_sets() {
    let (router, state) = router_and_state().await;
    let a = [0xa1u8; 32];

    // A folder with a file…
    let docs = state.db.create_folder("docs", &a).await.unwrap();
    seed_file(&state.db, &a, docs, "notes.txt", 100).await;

    // …and a backup-type "Photo Library" set whose files record into custody.
    let photolib = state
        .db
        .create_folder_with_options(
            "photolib",
            &a,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    seed_backup_file(&state.db, &a, photolib, "IMG_0001.jpg", 4000, None).await;
    seed_backup_file(&state.db, &a, photolib, "IMG_0002.jpg", 5000, None).await;
    // A deleted path must NOT surface.
    seed_backup_file(&state.db, &a, photolib, "IMG_0003.jpg", 6000, None).await;
    let gone_hash: [u8; 32] = *blake3::hash("IMG_0003.jpg".as_bytes()).as_bytes();
    let device: [u8; 32] = [0xd1u8; 32];
    state
        .db
        .record_sync_change(
            &a,
            &gone_hash,
            None,
            0,
            "delete",
            Some(photolib),
            Some(&device),
            Some("IMG_0003.jpg"),
        )
        .await
        .unwrap();

    let all = list_page(&router, &state, a, None, 0).await;
    // Hash-ordered pagination (media.md O-4) — compare the set.
    let mut ids: Vec<(String, String)> = all
        .items
        .iter()
        .map(|it| (it.folder.clone(), it.path.clone()))
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![
            ("docs".into(), "notes.txt".into()),
            ("photolib".into(), "IMG_0001.jpg".into()),
            ("photolib".into(), "IMG_0002.jpg".into()),
        ],
        "the Backup set's live custody files aggregate with the Sync set's; the \
         tombstoned path is excluded"
    );
    let img1 = all
        .items
        .iter()
        .find(|it| it.path == "IMG_0001.jpg")
        .expect("the backup custody file surfaces in the aggregate");
    assert_eq!(img1.folder, "photolib");
    assert_eq!(img1.size_bytes, 4000, "the backup file carries its size");
    assert!(
        img1.source_online,
        "the backup set's content is resident on the nest, so it is reachable \
         with no device online (folder_content_reachable)"
    );
}

/// A file recorded **with** an uploader thumbnail-blob hash surfaces it as
/// `MediaItem.thumbnail_hash` through `fauna.media.list`, while a file recorded
/// without one stays `None`. Proves the uploader↔manifest thumbnail association
/// round-trips end-to-end — the persisted `thumbnail_hash` column → the one
/// `get_files_for_folder` projection → the media handler — for **both** Sync
/// and Backup folders, which share the `sync_changes` head feed since the
/// phase 3 head unification (`media.md` § State & data shape: the field is
/// in the contract; this closes the population gap, Remaining (a)). The
/// *producer* (what supplies a thumbnail at record time) stays out of scope —
/// here the test records one directly.
#[tokio::test]
async fn media_list_surfaces_recorded_thumbnail_hash() {
    let (router, state) = router_and_state().await;
    let a = [0xa1u8; 32];

    // A Sync set: one file recorded WITH a thumbnail, one WITHOUT.
    let photos = state.db.create_folder("photos", &a).await.unwrap();
    seed_file_with_thumb(&state.db, &a, photos, "with_thumb.jpg", 100, "abc123").await;
    seed_file(&state.db, &a, photos, "no_thumb.txt", 50).await;

    // A Backup set whose custody file carries a thumbnail.
    let photolib = state
        .db
        .create_folder_with_options(
            "photolib",
            &a,
            fauna_nest::db::FolderOptions {
                ..Default::default()
            },
        )
        .await
        .unwrap();
    seed_backup_file(&state.db, &a, photolib, "IMG_9.jpg", 4000, Some("deadbeef")).await;

    let all = list_page(&router, &state, a, None, 0).await;

    let sync_thumb = all
        .items
        .iter()
        .find(|it| it.path == "with_thumb.jpg")
        .expect("the sync file surfaces");
    assert_eq!(
        sync_thumb.thumbnail_hash.as_deref(),
        Some("abc123"),
        "a file recorded with a thumbnail surfaces its hash"
    );

    let sync_none = all
        .items
        .iter()
        .find(|it| it.path == "no_thumb.txt")
        .expect("the no-thumb sync file surfaces");
    assert_eq!(
        sync_none.thumbnail_hash, None,
        "a file recorded without a thumbnail stays None"
    );

    let backup_thumb = all
        .items
        .iter()
        .find(|it| it.path == "IMG_9.jpg")
        .expect("the backup custody file surfaces");
    assert_eq!(
        backup_thumb.thumbnail_hash.as_deref(),
        Some("deadbeef"),
        "a custody file recorded with a thumbnail surfaces its hash"
    );
}

// ── The hash-ordered v2 cursor (path-sealing S2b) ────────────────────────────
//
// `docs/goal/ui/media.md` O-4, sealed-names amendment: the order/cursor re-keys
// to `(folder_id, path_hash)` via the v2 cursor, because the plaintext
// `(folder, path)` key stops existing at the flip. The v1 order left the wire
// with the compat-remnant sweep; v2 is the only order a nest serves.

/// List one page in an explicit pagination order.
async fn list_page_v(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    cursor: Option<String>,
    limit: u32,
    cursor_version: u32,
) -> MediaListReply {
    let bytes = dispatch(
        router,
        state.clone(),
        actor,
        "fauna.media.list",
        enc(&MediaListRequest {
            cursor,
            limit,
            cursor_version,
            ..Default::default()
        }),
    )
    .await
    .expect("media.list ok");
    decode(&bytes).unwrap()
}

/// Drain every page in one order and return the `(folder, path)` pairs seen.
async fn drain_all(
    router: &RpcRouter,
    state: &Arc<AppState>,
    actor: [u8; 32],
    limit: u32,
    cursor_version: u32,
) -> Vec<(String, String)> {
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let reply = list_page_v(router, state, actor, cursor, limit, cursor_version).await;
        seen.extend(
            reply
                .items
                .iter()
                .map(|it| (it.folder.clone(), it.path.clone())),
        );
        match reply.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    seen
}

/// The load-bearing property: v2 pagination walks the aggregate exactly once —
/// no gap, no repeat — across page boundaries. Display sorting is client-side
/// over the pulled pages (media.md O-4), so only coverage matters.
#[tokio::test]
async fn v2_pagination_walks_the_whole_aggregate_exactly_once() {
    let (router, state) = router_and_state().await;
    let a = [0xa1u8; 32];
    // Two sets whose NAME order and whose row-id / path-hash order differ, so a
    // handler that sorted by the plaintext key would be caught rather than
    // coincidentally agreeing.
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    let beta = state.db.create_folder("beta", &a).await.unwrap();
    for i in 0..4 {
        seed_file(&state.db, &a, alpha, &format!("a{i}.jpg"), 10).await;
        seed_file(&state.db, &a, beta, &format!("b{i}.jpg"), 20).await;
    }

    // Page size 3 over 8 items ⇒ three page boundaries.
    let v2 = drain_all(&router, &state, a, 3, MEDIA_LIST_CURSOR_V2).await;
    assert_eq!(v2.len(), 8, "v2 saw every item once");

    // Non-vacuity guard: the hash order genuinely differs from the plaintext
    // `(folder, path)` order, so this is not a name-ordered walk in disguise.
    // Should this ever fail, change the seeded names — do not delete it.
    let mut by_name = v2.clone();
    by_name.sort();
    assert_ne!(
        v2, by_name,
        "the v2 order must genuinely differ from the plaintext order"
    );
    by_name.dedup();
    assert_eq!(by_name.len(), 8, "no item repeats across page boundaries");
}

/// The reply echoes the order it applied.
#[tokio::test]
async fn the_reply_echoes_the_pagination_order_it_applied() {
    let (router, state) = router_and_state().await;
    let a = [0xa2u8; 32];
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    seed_file(&state.db, &a, alpha, "a.jpg", 10).await;

    let v2 = list_page_v(&router, &state, a, None, 0, MEDIA_LIST_CURSOR_V2).await;
    assert_eq!(v2.cursor_version, MEDIA_LIST_CURSOR_V2);
}

/// A label-audience reader (here, the set's owner) carries the `path_hash` the
/// sealed-first render salts from. Without it a scrubbed row is unrenderable,
/// so this is the wire half of the S2b render, not just a pagination detail.
/// The non-audience arm (a Q5 admin gets neither half) is
/// `the_path_hash_pair_is_withheld_from_a_q5_admin_who_is_not_the_audience`,
/// below (path-sealing S5d).
#[tokio::test]
async fn every_item_carries_the_path_hash_the_render_salts_from() {
    let (router, state) = router_and_state().await;
    let a = [0xa3u8; 32];
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    seed_file(&state.db, &a, alpha, "a.jpg", 10).await;

    let reply = list_page_v(&router, &state, a, None, 0, MEDIA_LIST_CURSOR_V2).await;
    let item = reply.items.first().expect("one item");
    assert_eq!(
        item.path_hash.as_deref().map(|h| h.to_vec()),
        Some(blake3::hash(b"a.jpg").as_bytes().to_vec()),
        "the wire carries the row's stored path_hash verbatim"
    );
}

/// The retired plaintext order (`cursor_version: 1`, which left the wire with
/// the compat-remnant sweep) and any unknown order are refused rather than
/// silently served some other order — a future v3 client must not be served v2
/// while believing it got v3.
#[tokio::test]
async fn a_retired_or_unknown_cursor_version_is_refused() {
    let (router, state) = router_and_state().await;
    let a = [0xa5u8; 32];
    state.db.create_folder("alpha", &a).await.unwrap();

    for v in [1, 99] {
        let err = dispatch(
            &router,
            state.clone(),
            a,
            "fauna.media.list",
            enc(&MediaListRequest {
                cursor_version: v,
                ..Default::default()
            }),
        )
        .await
        .expect_err("an order this nest does not serve is refused");
        assert_eq!(err.code, "fauna.media.invalid_cursor", "cursor_version {v}");
    }
}

/// `cursor_version` is required: a request that omits it — the retired
/// "absent means v1" shape — is malformed, never quietly paged in some order.
#[tokio::test]
async fn a_request_without_a_cursor_version_is_malformed() {
    let (router, state) = router_and_state().await;
    let a = [0xa6u8; 32];
    state.db.create_folder("alpha", &a).await.unwrap();

    #[derive(serde::Serialize)]
    struct PreV2Request {
        limit: u32,
    }
    let err = dispatch(
        &router,
        state.clone(),
        a,
        "fauna.media.list",
        enc(&PreV2Request { limit: 1 }),
    )
    .await
    .expect_err("a version-less request is refused");
    assert_eq!(err.code, "fauna.protocol.malformed");
}

// ── The per-reader set-name label projection (path-sealing S5c-1) ────────────
//
// `file-sync.md` § Sealed names & paths: a set's `name_sealed` ships with its
// `name_hash` salt or not at all. This adds the other half of the rule
// — the pair goes to the seal's *audience* (owner, roster member) and is
// withheld from a nest admin reading under the Q5 discovery grant, because the
// salt is an unkeyed digest of a dictionary-shaped name and would hand that
// reader back the very name the seal exists to withhold.

/// Bind `name` to a group and put `members` on the derived roster — the
/// post-`share` + `welcome.deliver` state the audience test needs.
async fn bind_shared(
    state: &Arc<AppState>,
    name: &str,
    owner: &[u8; 32],
    group_id: &[u8],
    members: &[[u8; 32]],
) -> i64 {
    let id = state.db.create_folder(name, owner).await.unwrap();
    state
        .db
        .set_folder_mls_group(name, owner, Some(group_id))
        .await
        .unwrap();
    let channel_id = ChannelId::from_group_id(group_id).0;
    state
        .db
        .register_actor_channel(owner, &channel_id)
        .await
        .unwrap();
    for m in members {
        state
            .db
            .register_actor_channel(m, &channel_id)
            .await
            .unwrap();
    }
    id
}

/// The audience arms: a set's **owner** and a **roster member** both receive the
/// full pair, so both can still render the name once the plaintext scrubs.
#[tokio::test]
async fn the_set_name_pair_ships_to_the_owner_and_to_a_roster_member() {
    let (router, state) = router_and_state().await;
    let owner = [0xb1u8; 32];
    let member = [0xb2u8; 32];
    let group_id = vec![0x5bu8; 24];
    let shared = bind_shared(&state, "shared", &owner, &group_id, &[member]).await;
    seed_file(&state.db, &owner, shared, "s1.jpg", 10).await;

    // Only a keyed writer stamps the seal; stamp it directly, as the engine's
    // bind/serve catch-up pass would.
    let sealed = vec![0xEDu8; 48];
    state
        .db
        .update_folder_for_user(
            "shared",
            &owner,
            fauna_nest::db::FolderUpdate {
                name_sealed: Some(&sealed),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for (who, label) in [(owner, "owner"), (member, "roster member")] {
        let reply = list_page_v(&router, &state, who, None, 0, MEDIA_LIST_CURSOR_V2).await;
        let item = reply
            .items
            .first()
            .unwrap_or_else(|| panic!("{label} sees the row"));
        assert_eq!(
            item.folder_sealed.as_deref().map(|b| b.to_vec()),
            Some(sealed.clone()),
            "{label} receives the set-name seal verbatim"
        );
        assert_eq!(
            item.folder_hash.as_deref().map(|b| b.to_vec()),
            Some(fauna_core::path_crypto::set_name_hash("shared").to_vec()),
            "{label} receives the salt that opens it"
        );
    }
}

/// The non-audience arm — the fix. An admin who is neither owner
/// nor roster member still *reads* the row (the Q5 discovery grant is
/// unchanged), but receives **neither** label half: no seal it cannot open, and
/// crucially no salt to dictionary-attack the name with.
#[tokio::test]
async fn the_set_name_pair_is_withheld_from_a_q5_admin_who_is_not_the_audience() {
    let (router, state) = router_and_state().await;
    let owner = [0xb3u8; 32];
    let admin = [0xadu8; 32];
    let group_id = vec![0x5cu8; 24];
    let shared = bind_shared(&state, "shared", &owner, &group_id, &[]).await;
    seed_file(&state.db, &owner, shared, "s1.jpg", 10).await;
    state.db.add_admin_actor(&admin).await.unwrap();

    let sealed = vec![0xEDu8; 48];
    state
        .db
        .update_folder_for_user(
            "shared",
            &owner,
            fauna_nest::db::FolderUpdate {
                name_sealed: Some(&sealed),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let reply = list_page_v(&router, &state, admin, None, 0, MEDIA_LIST_CURSOR_V2).await;
    let item = reply
        .items
        .first()
        .expect("the Q5 discovery grant still lists the row — this slice changes no permission");
    assert_eq!(
        item.folder_sealed, None,
        "an admin who cannot open the seal is not sent it"
    );
    assert_eq!(
        item.folder_hash, None,
        "and is not sent the unkeyed salt that would recover the name by dictionary"
    );
}

/// The pair is strictly a pair: a row whose seal was never stamped ships
/// neither half, so no reader ever receives a bare salt with nothing to open.
#[tokio::test]
async fn an_unstamped_row_ships_neither_half_of_the_pair() {
    let (router, state) = router_and_state().await;
    let owner = [0xb4u8; 32];
    let alpha = state.db.create_folder("alpha", &owner).await.unwrap();
    seed_file(&state.db, &owner, alpha, "a.jpg", 10).await;

    let reply = list_page_v(&router, &state, owner, None, 0, MEDIA_LIST_CURSOR_V2).await;
    let item = reply.items.first().expect("one item");
    assert_eq!(item.folder_sealed, None, "nothing stamped this name yet");
    assert_eq!(
        item.folder_hash, None,
        "so the salt does not travel alone either"
    );
}

// ── The per-reader path label projection (path-sealing S5d) ─────────────────
//
// the `path_hash` floor exemption does
// not survive per-reader either — the same audience gate `folder_sealed` /
// `folder_hash` use above, applied to `path_sealed` / `path_hash`.

/// Stamp `path_sealed` on `path` directly — the shape a keyed writer's
/// `SyncEngine::seal_recorded_path` funnel produces; done via raw SQL since a
/// real seal requires client-side key material the nest never holds.
async fn seal_path(db: &CacheDb, folder_id: i64, path: &str, sealed: &[u8]) {
    let conn = db.conn().await;
    conn.execute(
        "UPDATE sync_changes SET path_sealed = ?1 WHERE folder_id = ?2 AND path = ?3",
        rusqlite::params![sealed, folder_id, path],
    )
    .unwrap();
}

/// The audience arms: a set's owner and a roster member both receive the full
/// `path_sealed` + `path_hash` pair.
#[tokio::test]
async fn the_path_hash_pair_ships_to_the_owner_and_to_a_roster_member() {
    let (router, state) = router_and_state().await;
    let owner = [0xb6u8; 32];
    let member = [0xb7u8; 32];
    let group_id = vec![0x5du8; 24];
    let shared = bind_shared(&state, "shared", &owner, &group_id, &[member]).await;
    seed_file(&state.db, &owner, shared, "s1.jpg", 10).await;
    let sealed = vec![0xEEu8; 48];
    seal_path(&state.db, shared, "s1.jpg", &sealed).await;

    for (who, label) in [(owner, "owner"), (member, "roster member")] {
        let reply = list_page_v(&router, &state, who, None, 0, MEDIA_LIST_CURSOR_V2).await;
        let item = reply
            .items
            .first()
            .unwrap_or_else(|| panic!("{label} sees the row"));
        assert_eq!(
            item.path_sealed.as_deref().map(|b| b.to_vec()),
            Some(sealed.clone()),
            "{label} receives the path seal verbatim"
        );
        assert_eq!(
            item.path_hash.as_deref().map(|h| h.to_vec()),
            Some(blake3::hash(b"s1.jpg").as_bytes().to_vec()),
            "{label} receives the salt that opens it"
        );
    }
}

/// The non-audience arm — the path-axis fix. An admin who is neither
/// owner nor roster member still *reads* the row (the Q5 discovery grant is
/// unchanged), but receives **neither** path label half: no seal it cannot
/// open, and crucially no salt to dictionary-attack the path with.
#[tokio::test]
async fn the_path_hash_pair_is_withheld_from_a_q5_admin_who_is_not_the_audience() {
    let (router, state) = router_and_state().await;
    let owner = [0xb8u8; 32];
    let admin = [0xaeu8; 32];
    let group_id = vec![0x5eu8; 24];
    let shared = bind_shared(&state, "shared", &owner, &group_id, &[]).await;
    seed_file(&state.db, &owner, shared, "s1.jpg", 10).await;
    let sealed = vec![0xEEu8; 48];
    seal_path(&state.db, shared, "s1.jpg", &sealed).await;
    state.db.add_admin_actor(&admin).await.unwrap();

    let reply = list_page_v(&router, &state, admin, None, 0, MEDIA_LIST_CURSOR_V2).await;
    let item = reply
        .items
        .first()
        .expect("the Q5 discovery grant still lists the row — this slice changes no permission");
    assert_eq!(
        item.path_sealed, None,
        "an admin who cannot open the seal is not sent it"
    );
    assert_eq!(
        item.path_hash, None,
        "and is not sent the unkeyed salt that would recover the path by dictionary"
    );
}

/// The v2 cursor pages on the set's **row id**, not on `set_name_hash`. A
/// cursor minted under the earlier v2 shape must be refused rather than paged
/// against a different key space — the same loud degrade a cross-order replay
/// gets, never a silent mis-page.
///
/// Since S5e sealed the cursor this is refused one step earlier — an unsealed
/// CBOR blob does not open at all — so this test doubles as the proof that a
/// **pre-S5e plaintext cursor** is refused rather than mis-paged. The
/// `folder_id`-absent guard inside `decode_cursor` stays as the defence for a
/// shape change that arrives *already sealed*.
#[tokio::test]
async fn a_v2_cursor_of_the_earlier_key_shape_is_refused_not_mispaged() {
    let (router, state) = router_and_state().await;
    let a = [0xb5u8; 32];
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    for i in 0..3 {
        seed_file(&state.db, &a, alpha, &format!("a{i}.jpg"), 10).await;
    }

    // A v2 cursor as S2b minted them: version 2, `folder_hash` + `path_hash`,
    // no `folder_id`. Hand-built because the nest no longer emits this shape.
    #[derive(serde::Serialize)]
    struct LegacyV2Cursor {
        version: u32,
        folder_hash: serde_bytes::ByteBuf,
        path_hash: serde_bytes::ByteBuf,
    }
    let legacy = fauna_protocol::encode_canonical(&LegacyV2Cursor {
        version: MEDIA_LIST_CURSOR_V2,
        folder_hash: serde_bytes::ByteBuf::from(
            fauna_core::path_crypto::set_name_hash("alpha").to_vec(),
        ),
        path_hash: serde_bytes::ByteBuf::from(blake3::hash(b"a0.jpg").as_bytes().to_vec()),
    })
    .unwrap();

    let err = dispatch(
        &router,
        state.clone(),
        a,
        "fauna.media.list",
        enc(&MediaListRequest {
            cursor: Some(hex::encode(&legacy)),
            limit: 1,
            cursor_version: MEDIA_LIST_CURSOR_V2,
            ..Default::default()
        }),
    )
    .await
    .expect_err("the earlier v2 key shape is refused");
    assert_eq!(err.code, "fauna.media.invalid_cursor");
}

// ── The v2 cursor is opaque to its holder (path-sealing S5e) ────────────────
//
// S5d withheld `path_hash` from a non-audience reader
// on the `MediaItem` **but the cursor minted beside it still embedded that same
// hash**, computed before any audience gate. A Q5 admin paging at `limit = 1`
// therefore received, in each reply, the hash of the very item that reply had
// just suppressed — item for item, for the whole listing. The cursor is now
// sealed under a nest-held key, so it discloses nothing to anyone who holds it.

/// The hex the raw digest of `path` would appear as inside a cursor that carried
/// it in the clear — the needle these tests hunt for.
fn path_hash_hex(path: &str) -> String {
    hex::encode(blake3::hash(path.as_bytes()).as_bytes())
}

/// The non-audience arm of the cursor. A Q5 admin
/// pages a group-bound set they hold no key for, one item at a time, and the
/// cursor they are handed never carries the `path_hash` the item itself
/// withheld. Two pages, because a single-page test cannot see this at all: the
/// leak is in `next_cursor`, which only a *continuing* listing receives.
#[tokio::test]
async fn the_v2_cursor_hands_a_non_audience_pager_no_path_hash_across_two_pages() {
    let (router, state) = router_and_state().await;
    let owner = [0xb8u8; 32];
    let admin = [0xa3u8; 32];
    let group_id = vec![0x5eu8; 24];
    let shared = bind_shared(&state, "shared", &owner, &group_id, &[]).await;
    seed_file(&state.db, &owner, shared, "s1.jpg", 10).await;
    seed_file(&state.db, &owner, shared, "s2.jpg", 20).await;
    seal_path(&state.db, shared, "s1.jpg", &[0xEEu8; 48]).await;
    seal_path(&state.db, shared, "s2.jpg", &[0xEFu8; 48]).await;
    state.db.add_admin_actor(&admin).await.unwrap();

    let v2 = MEDIA_LIST_CURSOR_V2;
    let needles = [path_hash_hex("s1.jpg"), path_hash_hex("s2.jpg")];

    // Page 1 of 2, at the smallest limit the surface accepts.
    let page1 = list_page_v(&router, &state, admin, None, 1, v2).await;
    let item1 = page1
        .items
        .first()
        .expect("the Q5 grant still lists the row");
    assert_eq!(item1.path_hash, None, "S5d: the item's salt is withheld");
    assert_eq!(item1.path_sealed, None, "and so is the seal");
    let cursor = page1
        .next_cursor
        .clone()
        .expect("a second page remains, so a cursor is minted");
    for (path, needle) in ["s1.jpg", "s2.jpg"].iter().zip(&needles) {
        assert!(
            !cursor.contains(needle),
            "the page-1 cursor must not carry {path}'s path_hash — it is the same \
             digest the item withheld, so handing it back defeats the projection"
        );
    }

    // Page 2 — and the cursor must still *work*: a fix that closes the leak by
    // breaking pagination is not a fix.
    let page2 = list_page_v(&router, &state, admin, Some(cursor), 1, v2).await;
    let item2 = page2
        .items
        .first()
        .expect("the cursor still pages — the admin listing is not broken");
    assert_eq!(item2.path_hash, None, "page 2 withholds the salt too");
    assert_ne!(
        item1.path, item2.path,
        "the two pages walked to different items"
    );
    if let Some(c2) = &page2.next_cursor {
        for needle in &needles {
            assert!(!c2.contains(needle), "nor does any later page's cursor");
        }
    }
}

/// The audience arm — the owner pages the same set at `limit = 1` and walks
/// every item in order. The cursor is opaque to them too (they need no hash
/// *from* it; they receive the real one on each item), so this pins that
/// sealing the cursor cost the hot path nothing.
#[tokio::test]
async fn the_sealed_v2_cursor_still_pages_the_audience_through_every_item() {
    let (router, state) = router_and_state().await;
    let owner = [0xb9u8; 32];
    let alpha = state.db.create_folder("alpha", &owner).await.unwrap();
    for i in 0..3 {
        seed_file(&state.db, &owner, alpha, &format!("a{i}.jpg"), 10).await;
    }
    let v2 = MEDIA_LIST_CURSOR_V2;

    let mut seen = Vec::new();
    let mut cursor = None;
    for _ in 0..4 {
        let page = list_page_v(&router, &state, owner, cursor.clone(), 1, v2).await;
        seen.extend(page.items.iter().map(|it| it.path.clone()));
        for it in &page.items {
            assert!(
                it.path_hash.is_some(),
                "the audience still receives each item's real salt"
            );
        }
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    // v2 orders by `path_hash`, not by the plaintext path, so the walk order is
    // digest order — assert the paging *property* (every item exactly once), not
    // an incidental digest ordering.
    seen.sort();
    assert_eq!(
        seen,
        vec!["a0.jpg".to_string(), "a1.jpg".into(), "a2.jpg".into()],
        "keyset paging walks every item exactly once"
    );
}

/// A tampered cursor is refused, not mis-paged — the seal's authentication tag
/// doing its job at the handler boundary. (The *different-key* half — a foreign
/// nest's cursor, or one from before a deployment-key rotation — is pinned in
/// `cursor_seal`'s unit tests; it is unreachable from here because every
/// `AppState::for_test` derives its identity from one fixed basename, so two
/// test states are the *same* nest identity, not two nests.)
#[tokio::test]
async fn a_tampered_cursor_is_refused_not_mispaged() {
    let (router, state) = router_and_state().await;
    let a = [0xbau8; 32];
    let alpha = state.db.create_folder("alpha", &a).await.unwrap();
    for i in 0..3 {
        seed_file(&state.db, &a, alpha, &format!("a{i}.jpg"), 10).await;
    }
    let v2 = MEDIA_LIST_CURSOR_V2;

    let good = list_page_v(&router, &state, a, None, 1, v2)
        .await
        .next_cursor
        .expect("a cursor is minted");
    // Flip one hex nibble of the ciphertext — the AEAD tag must catch it.
    let mut bad: Vec<char> = good.chars().collect();
    let last = bad.len() - 1;
    bad[last] = if bad[last] == '0' { '1' } else { '0' };
    let bad: String = bad.into_iter().collect();
    assert_ne!(bad, good, "the tamper actually changed the cursor");

    let err = dispatch(
        &router,
        state.clone(),
        a,
        "fauna.media.list",
        enc(&MediaListRequest {
            cursor: Some(bad),
            limit: 1,
            cursor_version: v2,
            ..Default::default()
        }),
    )
    .await
    .expect_err("a tampered cursor is refused");
    assert_eq!(err.code, "fauna.media.invalid_cursor");
}
