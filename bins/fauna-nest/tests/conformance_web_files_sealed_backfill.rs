//! tier_3: a **sealed** folder synced FIRST and website-enabled SECOND ends
//! with its full back-catalogue in `web_files` — the client-driven half of the
//! enable-time backfill, against a nest that genuinely answers
//! `fauna.folders.update`, `fauna.folders.list` and `fauna.sync.changes.record`.
//!
//! `web-content-hosting.md` § Content model: `web_files` is a *projection* of a
//! folder's live sync heads, and a serving transition rebuilds it — but only
//! from the folder's **plaintext-resting** heads. A sealed folder's records rest
//! `path = NULL` (the S9 flip: `sync_handlers.rs`'s `rest_path` keeps the
//! plaintext column only for the ratified public/`web`/reserved classes), so
//! they are structurally invisible to `CacheDb::live_plaintext_heads_for_folder`
//! and the owner's toggle-ON reconciles *nothing*. Without a client-side pass a
//! paywalled site would serve only files recorded after the toggle — and a
//! finished static site never re-records.
//!
//! **Why this file rather than another `fauna-sync-engine` unit test.** The
//! claim under test is a statement about the NEST's projection, reached through
//! two mechanisms that only exist on the real thing: the enable-time backfill's
//! plaintext-heads fold (which must be shown NOT to cover this class — the
//! tier_2 twin cannot observe it at all) and `sync_handlers`' `if
//! fs.website_enabled` fan-out into `route_web_file_change`, which needs a
//! record that actually lands. Every engine test in that crate deliberately runs
//! an **unconnected** `NestClient`, so the recorded half is unreachable there;
//! `libs/fauna-sync-engine/src/download_file_bytes_test.rs`'s
//! `converge_corpus_to_website_*` pair pins the client-side walk and its gates.
//! Same split, for the same reason, as
//! `record_head_commit_wiring_test.rs` ↔ `conformance_sync_engine_record_commit.rs`,
//! whose `start_test_nest` shape this mirrors (via
//! `conformance_post_succession_reseal.rs`).
//!
//! **The posture is installed the production way.** The test never arms the
//! toggle by hand: it flips the folder row through the real
//! `fauna.folders.update` handler and then calls `SyncEngine::refresh_sync_mode`,
//! which is what reads `website_enabled` off the same `fauna.folders.list`
//! fetch as the sync mode (`config::SeatResolution`). `refresh_sync_mode` +
//! `converge_corpus_to_website` is exactly the adjacent pair
//! `always_resident::run_watch_loop` runs at its entry and on every rescan tick,
//! and that one-line call is source-visible there — the same coverage split
//! `conformance_sync_mode_role_flip.rs` records for the mode itself.
//!
//! Regression pins:
//!   * deleting the client pass leaves `web_files` empty forever — the
//!     mid-test assertion that the nest-side backfill reconciled *nothing* is
//!     what makes that failure legible rather than a mystery;
//!   * dropping `SeatResolution::website_enabled` (or its install) leaves the
//!     posture off, so the pass converges to `"off"` and re-records nothing.

use std::sync::Arc;

use bytes::Bytes;
use fauna_core::crypto::BackupKey;
use fauna_core::identity::ActorKeypair;
use fauna_nest::backup::service::BackupService;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::{encode_canonical, folders::FolderUpdateRequest};
use fauna_sync_engine::adaptive::AdaptiveConcurrency;
use fauna_sync_engine::db::SyncDb;
use fauna_sync_engine::engine::SyncEngine;
use fauna_sync_engine::ignore::IgnoreMatcher;
use fauna_sync_engine::transfer::TransferPool;

mod common;

const OWNER_SECRET: [u8; 32] = [0x84; 32];
/// The identity seed the site's corpus seals under. Owner-only (unbound), which
/// is the shape whose `label_seal_root` is the `BackupKey` — so every record
/// carries `path_sealed` and rests no plaintext path.
const OWNER_SEED: [u8; 32] = [0xC3; 32];
const DEVICE: [u8; 32] = [0x0C; 32];
const FOLDER: &str = "my_site";

/// Start a real in-process nest serving the auth + folders + sync WS-RPC kinds
/// and the chunk-store HTTP routes, owner registered and folder pre-created with
/// the website toggle OFF. Returns the state too, because the claim under test
/// is about a table (`web_files`) rather than about a reply.
async fn start_test_nest(owner: [u8; 32]) -> (String, String, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());

    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlive the test; never deleted under test
    let backup_svc =
        Arc::new(BackupService::new(db.clone(), None, false, blob_path, None).unwrap());

    db.create_user(&owner, "free", "test").await.unwrap();
    db.create_folder(FOLDER, &owner).await.unwrap();
    // The set's stored nonce — what a signed record's statement is verified
    // under (the owner's create helper sends it; this fixture creates the row
    // directly, so it stores it the way the owner's update would).
    db.update_folder_for_user(
        FOLDER,
        &owner,
        fauna_nest::db::FolderUpdate {
            set_nonce: Some(&common::SET_NONCE),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let token_store = Arc::new(TokenStore::new());
    let http_token = token_store
        .insert(ActorKeypair::from_secret(OWNER_SECRET).actor_id(), 3600)
        .await;

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            // `fauna.web.files.prune_sealed` lives here — the client's
            // complete-set declaration, which is the only rail that can retire a
            // sealed projection row.
            fauna_nest::web_handlers::register_web_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store,
            ..Default::default()
        },
        ..AppState::for_test(db)
    });

    let app = fauna_nest::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (format!("http://{addr}"), http_token, state)
}

/// An owner-only engine sealing under `OWNER_SEED`'s `BackupKey`, with a
/// genuinely connected `NestClient` — the piece `fauna-sync-engine`'s own tests
/// never build, and the whole reason the recorded half of the pass lives here.
async fn connected_engine(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    connected_engine_with_keys(dest_url, http_token, watch_path, None).await
}

/// [`connected_engine`] with an explicit M2 content-key generation.
///
/// ⚠ **The generation is what makes a `web_files` row `is_sealed()`**, and that
/// predicate — not "the names are sealed" — is the class every sealed-projection
/// rule keys on. An owner-only set seals its chunks under the `BackupKey` and
/// stamps **no** `content_key_version`, so its rows read as *plaintext* to the
/// nest even though the nest holds no names for their heads: the enable-time
/// reconcile therefore drops them all (they are not `is_sealed()`, and the
/// plaintext live-head fold cannot name them) and the client's pass re-records
/// the survivors, which self-heals a delete by accident. Only a set with a
/// generation reaches the row the reconcile must skip — so only that shape
/// exhibits.
async fn connected_engine_with_keys(
    dest_url: &str,
    http_token: &str,
    watch_path: std::path::PathBuf,
    content_keys: Option<fauna_core::folder_keys::FolderContentKeys>,
) -> (SyncEngine, Arc<fauna_client::NestClient>) {
    let (engine_client, nest_client) =
        common::sync_engine_auth_client(dest_url, http_token, OWNER_SECRET, &DEVICE);

    let engine = SyncEngine::new(
        watch_path,
        SyncDb::open_in_memory().unwrap(),
        engine_client,
        Some(FOLDER.to_string()),
        DEVICE,
        None, // mls
        None, // epoch_secret
        // ⚠ The BackupKey OUTRANKS the content-key generation at the seal site
        // (`seal_for_upload`: public → plaintext, else backup key, else
        // `content_seal_root`), so an engine holding both would seal under the
        // owner root and stamp NO version — i.e. project rows that are not
        // `is_sealed()`. A generation-sealed set is therefore exactly the one
        // without a backup key here, which is also the production shape (a
        // bound set's chunks rest under its M2 generation).
        content_keys
            .is_none()
            .then(|| BackupKey::derive(&OWNER_SEED).into()),
        None, // mls_group_id — owner-only, so paths seal under the BackupKey
        content_keys,
        fauna_core::format::ConflictPolicy::Auto,
        fauna_core::format::FormatRegistry::new(),
        IgnoreMatcher::default(),
        4,
        TransferPool::new(Arc::new(AdaptiveConcurrency::fixed(4)), None),
        Arc::clone(&nest_client),
        fauna_sync_engine::config::SyncMode::Sync,
    );
    // A writer engine signs every record it sends (the nest refuses an
    // unsigned one `signature_required`), directly under the owner's key and
    // the set's stored nonce; its reader judges served rows against the same.
    let owner_kp = ActorKeypair::from_secret(OWNER_SECRET);
    engine.set_change_signer(
        Some(common::direct_signer(&owner_kp)),
        Some(common::SET_NONCE),
    );
    engine.set_reader_binding(fauna_protocol::sync_row_verify::ReaderBinding {
        set_nonce: Some(common::SET_NONCE),
        owner: Some(owner_kp.actor_id().0),
        ..Default::default()
    });

    // Complete the WS auth handshake up front rather than relying on the lazy
    // connect a bare record would trigger — without this the reconnect
    // supervisor's first attempt does not land inside the record's own request
    // deadline, and the timeout reads identically to a genuine rejection
    // (`conformance_post_succession_reseal.rs` records the same trap).
    nest_client
        .connect()
        .await
        .expect("nest_client must reach Connected (WS auth handshake)");
    // Bind by ref, as the resident agent does: the engine's per-tick row read
    // (which carries the website toggle) resolves the set's row by its
    // `FolderRef` alone, and an engine holding none reads no row.
    let row = fauna_client_folders::FoldersClient::new(Arc::clone(&nest_client))
        .list_owned_and_shared()
        .await
        .expect("list the owner's folders")
        .folders
        .into_iter()
        .find(|fs| fs.name == FOLDER)
        .expect("the fixture's one folder is listed");
    let engine = engine.with_binding_edge(fauna_sync_engine::binding_edge::BindingEdge {
        folder_ref: fauna_core::folder_keys::FolderRef::Local(row.id),
        basis: fauna_sync_engine::binding_edge::BindingBasis::of(&row),
        on_rebuild: Arc::new(|| {}),
    });
    (engine, nest_client)
}

/// Flip the folder's website toggle through the REAL `fauna.folders.update`
/// handler — the same door the app's web-settings screen knocks on, and the one
/// that fires the nest-side enable-time projection rebuild.
async fn enable_website(state: &Arc<AppState>, owner: [u8; 32]) {
    let req = FolderUpdateRequest {
        name: FOLDER.to_string(),
        website_enabled: Some(true),
        ..Default::default()
    };
    let meta = state
        .rpc_router
        .kind_meta("fauna.folders.update")
        .expect("fauna.folders.update registered");
    (meta.handler)(
        Arc::clone(state),
        owner,
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("the owner's website toggle must be accepted");
}

/// [`enable_website`]'s general form — the toggle also has to go OFF, which is
/// the window the journey needs (a delete recorded while the site is
/// switched off never reaches `delete_web_file`).
async fn set_website(state: &Arc<AppState>, owner: [u8; 32], on: bool) {
    let req = FolderUpdateRequest {
        name: FOLDER.to_string(),
        website_enabled: Some(on),
        ..Default::default()
    };
    let meta = state
        .rpc_router
        .kind_meta("fauna.folders.update")
        .expect("fauna.folders.update registered");
    (meta.handler)(
        Arc::clone(state),
        owner,
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("the owner's website toggle must be accepted");
}

/// Put a paywall tier on the (website-enabled, non-public) folder — what turns
/// a sealed row into something the serve door answers **402** for rather than
/// 404, which is the whole reason this journey is observable at the serve layer
/// without minting a grant.
async fn set_paywall_tier(state: &Arc<AppState>, owner: [u8; 32], tier: &str) {
    state
        .db
        .create_subscription_tier(
            &owner,
            tier,
            1,
            None,
            Some("$3/mo"),
            None,
            true,
            None,
            None,
            false,
        )
        .await
        .expect("the tier the paywall names must exist");
    let req = fauna_protocol::folders::FolderSetWebPaywallRequest {
        name: FOLDER.to_string(),
        tier: Some(tier.to_string()),
        ..Default::default()
    };
    let meta = state
        .rpc_router
        .kind_meta("fauna.folders.set_web_paywall")
        .expect("fauna.folders.set_web_paywall registered");
    (meta.handler)(
        Arc::clone(state),
        owner,
        Bytes::from(encode_canonical(&req).unwrap().to_vec()),
    )
    .await
    .expect("the owner's paywall tier must be accepted");
}

/// Ask the REAL serve door for a path, anonymously, and return its status.
///
/// Three answers matter here and they are all distinct, which is what makes
/// this a *serving* assertion rather than a table one:
/// **402** the path is on the site and paywalled (a sealed row in a paywalled
/// set, no token → the teaser); **404** the site does not have this path at
/// all; **200** free bytes.
async fn serve_status(state: &Arc<AppState>, owner: [u8; 32], path: &str) -> u16 {
    let store = state
        .backup_service
        .as_ref()
        .expect("backup service")
        .local_blob_store();
    fauna_nest::web_content::serve::serve_web_content(&state.db, &store, None, &owner, path)
        .await
        .status()
        .as_u16()
}

/// The paths the nest's `web_files` projection currently holds for `owner`.
async fn projected_paths(state: &Arc<AppState>, owner: &[u8; 32]) -> Vec<String> {
    let mut paths: Vec<String> = state
        .db
        .list_web_files(owner)
        .await
        .expect("list_web_files")
        .into_iter()
        .map(|r| r.path)
        .collect();
    paths.sort();
    paths
}

/// A sealed site's back-catalogue reaches `web_files` when the owner turns the
/// website on — and the nest-side backfill demonstrably could not have done it.
///
/// The journey end to end: an owner-only sealed engine syncs two files while the
/// toggle is OFF, the owner then flips it through the real handler (which runs
/// the enable-time projection rebuild), and only the client's own convergence
/// pass — driven off the folder-list posture, not a hand-set flag — lands the
/// rows.
///
/// ⚠ The middle assertion is the load-bearing one. It is what distinguishes
/// "the client pass works" from "the nest would have done it anyway": without
/// it, a future change that made the nest fold sealed heads would leave this
/// test green while the mechanism it names had become dead code.
#[tokio::test]
async fn a_sealed_site_enabled_after_sync_reaches_web_files_via_the_client_pass() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token, state) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    let (engine, _nest_client) = connected_engine(&url, &token, watch.path().to_path_buf()).await;

    // ── The pre-toggle life: the site syncs while the toggle is OFF ──
    let assets = [
        (
            "index.html",
            b"<!doctype html><title>chapter one</title>".to_vec(),
        ),
        (
            "assets/chapter-one.bin",
            (0..70_000u32)
                .map(|i| (i.wrapping_mul(31) % 251) as u8)
                .collect::<Vec<u8>>(),
        ),
    ];
    for (rel, bytes) in &assets {
        let full = watch.path().join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, bytes).unwrap();
        engine
            .upload_file(rel)
            .await
            .unwrap_or_else(|e| panic!("pre-toggle sync of {rel}: {e}"));
    }
    assert!(
        projected_paths(&state, &owner).await.is_empty(),
        "a toggle-off folder's records must not be routed into web_files"
    );

    // ── The owner turns the website on. The nest reconciles what it can ──
    enable_website(&state, owner).await;
    assert!(
        projected_paths(&state, &owner).await.is_empty(),
        "THE GAP: the enable-time backfill folds only plaintext-resting heads, \
         and a sealed folder's records rest `path = NULL` (S9) — so the nest \
         reconciled nothing and the site would serve nothing forever"
    );

    // ── The client half: the posture arrives off the folder list, and the
    //    convergence re-records the back-catalogue ──
    engine.refresh_sync_mode().await;
    assert!(
        engine.is_website_enabled(),
        "the toggle must reach a running seat off the same folders.list read as the mode"
    );
    assert_eq!(
        engine
            .converge_corpus_to_website()
            .await
            .expect("the website corpus convergence"),
        assets.len(),
        "every live head must be re-recorded"
    );

    // ── The whole back-catalogue is now projected, sealed ──
    assert_eq!(
        projected_paths(&state, &owner).await,
        vec![
            "assets/chapter-one.bin".to_string(),
            "index.html".to_string()
        ],
        "the sealed back-catalogue must be serving-ready without any file being touched"
    );

    // The nest holds no name for these heads, so it also cannot have invented
    // the rows from its own change log: the paths above came from the records
    // the client just published, which is the only rail that carries them.
    for (rel, _) in &assets {
        let row = state
            .db
            .get_web_file(&owner, rel)
            .await
            .expect("get_web_file")
            .unwrap_or_else(|| panic!("{rel} must be projected"));
        assert_eq!(
            row.folder_id,
            state
                .db
                .get_folder_for_actor(FOLDER, &owner)
                .await
                .unwrap()
                .map(|f| f.id),
            "the row must name the folder it was synced from — the paywall gate's qualifier"
        );
    }

    // Steady state: the marker settled, so the next catch-up tick costs one
    // meta-row read and re-records nothing.
    assert_eq!(engine.converge_corpus_to_website().await.unwrap(), 0);
}

/// A sealed path deleted while the website was switched off
/// must stop **serving** once the site comes back up.
///
/// This is the delete-that-does-not-take-effect journey, end to end, and the
/// assertion is deliberately the **serve** result rather than the row: the
/// promise `principles.md` § The user always controls their data makes is about
/// what visitors can fetch, and a test that only reads `web_files` would stay
/// green if some later door learned to serve from somewhere else.
///
/// Why 402-vs-404 is the discriminator, and why it needs no grant: a sealed row
/// in a **paywalled** set answers an anonymous request with the teaser — *the
/// resource exists and costs money* — while a path the site does not have at all
/// answers 404. So the two states this row is about are directly observable at
/// the door, with no holder, no grant and no capability token in sight.
///
/// The gap being closed: the nest cannot see this class. A sealed head rests no
/// plaintext path (S9), so `reconcile_web_files_projection` deliberately skips
/// sealed rows — their absence from its live set is ignorance, not evidence —
/// and the client's re-record pass is additive by construction. Neither side
/// could say *"and nothing else"* until `fauna.web.files.prune_sealed`.
///
/// Red-verify: drop the `declare_live_web_corpus` call from
/// `converge_corpus_to_website` and the final assertion fails with 402 — the
/// deleted page still on sale.
#[tokio::test]
async fn a_sealed_path_deleted_while_the_site_was_off_stops_serving_when_it_comes_back() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token, state) = start_test_nest(owner).await;

    let watch = tempfile::tempdir().unwrap();
    // A generation-sealed set: its records stamp `content_key_version`, which is
    // exactly what makes the projection rows `is_sealed()` — the class the
    // nest's reconcile must skip, and therefore the only class that can strand a
    // deleted page. See `connected_engine_with_keys`.
    let (engine, _nest_client) = connected_engine_with_keys(
        &url,
        &token,
        watch.path().to_path_buf(),
        Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            [0x5e; 32], 1,
        )),
    )
    .await;

    // ── A sealed site with two pages, published the ordinary way ──
    for (rel, bytes) in [
        ("index.html", b"<!doctype html><title>home</title>".to_vec()),
        (
            "chapter-two.html",
            b"<!doctype html><title>chapter two</title>".to_vec(),
        ),
    ] {
        std::fs::write(watch.path().join(rel), &bytes).unwrap();
        engine
            .upload_file(rel)
            .await
            .unwrap_or_else(|e| panic!("sync of {rel}: {e}"));
    }
    set_website(&state, owner, true).await;
    engine.refresh_sync_mode().await;
    assert_eq!(engine.converge_corpus_to_website().await.unwrap(), 2);
    assert_eq!(
        projected_paths(&state, &owner).await,
        vec!["chapter-two.html".to_string(), "index.html".to_string()]
    );
    // The premise of the whole journey: these rows are `is_sealed()`, which is
    // the class the nest's reconcile must skip. Without a generation they would
    // read as plaintext and the reconcile would (on its own terms, correctly)
    // drop and re-earn them, hiding the finding entirely.
    for page in ["index.html", "chapter-two.html"] {
        let row = state.db.get_web_file(&owner, page).await.unwrap().unwrap();
        assert!(row.is_sealed(), "{page} must project SEALED: {row:?}");
    }

    // ── The owner takes the site down, deletes a page, and puts it back up ──
    set_website(&state, owner, false).await;
    engine.refresh_sync_mode().await;
    engine.converge_corpus_to_website().await.unwrap();

    std::fs::remove_file(watch.path().join("chapter-two.html")).unwrap();
    engine
        .handle_delete("chapter-two.html")
        .await
        .expect("the owner's delete records");
    assert!(
        projected_paths(&state, &owner)
            .await
            .contains(&"chapter-two.html".to_string()),
        "THE GAP: the delete arrived while the site was off, so nothing fired \
         `delete_web_file` and the projection still names the deleted page"
    );

    set_website(&state, owner, true).await;
    engine.refresh_sync_mode().await;
    assert_eq!(
        engine.converge_corpus_to_website().await.unwrap(),
        1,
        "only the surviving page is a live head to re-record"
    );

    // ── The verdict, at the serve door ──
    set_paywall_tier(&state, owner, "supporter").await;
    assert_eq!(
        serve_status(&state, owner, "index.html").await,
        402,
        "the surviving page is still on the site, behind the paywall"
    );
    assert_eq!(
        serve_status(&state, owner, "chapter-two.html").await,
        404,
        "the DELETED page must be gone from the site — 402 here is the finding: \
         the owner deleted it, and every holder of a live grant could still fetch it"
    );
}

/// **The other direction — and the one that would have been worse.**
/// A device that has not caught up must NOT be able to declare the site empty.
///
/// The declaration says *"these are all the live paths"*, so an empty one says
/// *"this site has no pages"* — and from inside the engine that is
/// indistinguishable from *"my state DB is empty because I only just bound this
/// folder"*. A second seat, or the same seat after its state was rebuilt, walks
/// nothing on its first pass; without a guard it would take a live site down as
/// its opening move. The additive-everywhere rule reads the same way here as it
/// does on the wire: **silence is never "delete everything"**.
///
/// Red-verify: drop the `live.is_empty()` return from `declare_live_web_corpus`
/// and this fails with an empty projection — the whole site gone.
#[tokio::test]
async fn a_seat_that_has_not_caught_up_cannot_declare_the_site_empty() {
    let owner = ActorKeypair::from_secret(OWNER_SECRET).actor_id().0;
    let (url, token, state) = start_test_nest(owner).await;
    let keys = || {
        Some(fauna_core::folder_keys::FolderContentKeys::genesis(
            [0x5e; 32], 1,
        ))
    };

    // ── Seat one publishes the site ──
    let watch = tempfile::tempdir().unwrap();
    let (engine, _nest) =
        connected_engine_with_keys(&url, &token, watch.path().to_path_buf(), keys()).await;
    for rel in ["index.html", "about.html"] {
        std::fs::write(
            watch.path().join(rel),
            b"<!doctype html><title>page</title>",
        )
        .unwrap();
        engine.upload_file(rel).await.expect("sync");
    }
    set_website(&state, owner, true).await;
    engine.refresh_sync_mode().await;
    engine.converge_corpus_to_website().await.unwrap();
    let published = projected_paths(&state, &owner).await;
    assert_eq!(published.len(), 2, "the site is up: {published:?}");

    // ── Seat two arrives: same owner, same folder, EMPTY local state ──
    let fresh_watch = tempfile::tempdir().unwrap();
    let (fresh, _nest2) =
        connected_engine_with_keys(&url, &token, fresh_watch.path().to_path_buf(), keys()).await;
    // The toggle is armed directly rather than through `refresh_sync_mode`:
    // this seat's subject is the EMPTY WALK, and the folder-list plumbing that
    // delivers the posture is already the first test's subject. Arming it here
    // is the stronger setup anyway — it puts the seat in exactly the state the
    // guard has to survive, with no dependence on which read happened to land.
    let fresh = fresh.with_website_enabled(true);
    assert_eq!(
        fresh.converge_corpus_to_website().await.unwrap(),
        0,
        "an empty state DB walks nothing — which is the whole danger"
    );

    assert_eq!(
        projected_paths(&state, &owner).await,
        published,
        "a seat that knows nothing must not be able to say the site is empty"
    );
}
