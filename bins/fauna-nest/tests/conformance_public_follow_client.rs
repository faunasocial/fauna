//! **The follower's CLIENT half, against two real nests** — folders re-model
//! phase 4 slice 4f-iv, the half a pytest seat cannot reach.
//!
//! Owner docs: `docs/goal/behavior/folders.md` § Publicly-synced follow
//! (follow state lives solely in the follower's own account store),
//! `docs/goal/architecture/federation.md` § The public folder read plane.
//!
//! # Why this file exists
//!
//! Slice 4f-ii landed the whole follower client stack — the follow record,
//! the chokepoint `fauna_client_config::follows`, the read ops
//! `fauna_client_folders::public_follow`, and the projection source
//! `fauna_devices_machine::StoreFollowedFoldersSource` — with **fake-nest unit
//! tests only**. That source's own doc comment says the rule it applies is put
//! in a pure function *because* "this glue cannot be unit-tested — it is built
//! over a concrete `NestClient`".
//!
//! That is true of a *unit* test and false of this one. `bins/fauna-nest` already
//! hosts client-stack conformance tests over in-process nests
//! (`conformance_cross_nest_conversations_client.rs`), so the concrete
//! `NestClient` the glue needs can simply be stood up — and the alternative,
//! waiting for slice 4f-iii's UI to give an e2e seat something to click, would
//! leave the whole client half unexercised behind a gate it does not depend on.
//!
//! # What is real
//!
//! * **Two in-process nests** on distinct loopback ports with distinct
//!   deployment identities — H (the folder's home, where the owner lives) and F
//!   (the follower's own nest) — federating over the plain-HTTP loopback
//!   affordance of `federation_channel::validate_peer_url`, exactly as
//!   `conformance_federation_channel` and `cross_nest_agent_capstone` do.
//! * **Two authenticated `NestClient`s**, one per seat, over real WS-RPC.
//! * **Not the account store.** The follow record rests in the follower's
//!   `fauna.state.follows` rows, a client-side store with no nest in the loop;
//!   here it is the shared `FakeFollowsStore` double (the production door and
//!   its cross-device convergence are proven over the real store in
//!   `libs/fauna-sync-engine/tests/peer_leg_convergence.rs`). The production
//!   source reads it back rather than the test.
//!
//! # What each test pins that nothing else does
//!
//! | test | the thing that had never run |
//! |---|---|
//! | `a_follow_pins_the_home_identity_through_the_relay` | first contact over the relay → `save_follow` → the stored record addresses the HOME nest and carries its SPKI-pin identity |
//! | `the_source_projects_a_live_follow_as_available` | `StoreFollowedFoldersSource::followed_folders` against a real nest — the availability probe is a live relayed read, not a stub verdict |
//! | `a_flip_back_turns_the_row_unavailable_without_dropping_it` | the revoke as the *user's row* experiences it (kept, marked), driven by an actual audience flip on H |
//! | `unfollow_removes_the_row_and_leaves_the_home_nest_untouched` | the local-delete half of the contract, end to end |
//! | `fetch_listing_folds_the_live_log_and_feeds_the_shared_verdict` | the Media followed-browse seam (`fauna_core::followed_media`) on the same glue: the live head fold, `Unavailable` on the refusal, and the one-cache contract with the Folders-page projection |
//! | `an_over_sealed_public_row_survives_the_boot_scrub_for_the_follower` | a public row recorded WITH a seal beside its plaintext path still reaches the follower with its path after H's boot pass (the S9 scrub) has run over the file |
//!
//! The projection from source rows to `DevicesSnapshot.followed` is deliberately
//! **not** re-tested here: it is a pure map already pinned by
//! `libs/fauna-devices-machine/tests/devices_lifecycle.rs` over a `FixedFollowed`
//! stub, and re-running it over a live source would only re-assert the same map
//! while dragging a whole `DevicesMachine` (and its `DevicesNestApi`) into this
//! file. What was missing was the *source*, and that is what this file drives.
//!
//! Bytes are out of scope here and covered next door:
//! `tests/e2e-unified/tests/api/test_public_folder_follow_cross_nest.py` fetches
//! the file off H's open by-hash plane over two real nest **binaries**.
//!
//! ## Run
//!
//! ```text
//! cargo test -p fauna-nest --test conformance_public_follow_client
//! ```

mod common;
use common::connected_client;

use std::sync::Arc;

use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::routes::AppState;
use fauna_nest::token_store::TokenStore;
use fauna_protocol::RpcRequester;
use fauna_protocol::folders::{
    FolderCreateReply, FolderCreateRequest, FolderUpdateReply, FolderUpdateRequest,
};
use fauna_protocol::sync::{
    SyncChangeRecordReply, SyncChangeRecordRequest, SyncRegisterReply, SyncRegisterRequest,
};

const FOLDER: &str = "follower-facing";
const DEVICE_ID: [u8; 32] = [0xa9; 32];

// ── the two nests ──────────────────────────────────────────────────────────

/// One in-process nest serving every kind this journey touches, with its own
/// deployment identity so H's and F's stamps are distinguishable.
///
/// Plain HTTP on loopback: the federation relay's documented in-process peer
/// affordance (`federation_channel::validate_peer_url`).
async fn start_nest(db: CacheDb) -> (String, Arc<AppState>) {
    use ed25519_dalek::SigningKey;
    use fauna_nest::nest_identity::NestIdentity;

    let db = Arc::new(db);

    // A real blob-store-backed `BackupService`, as every nest fixture in this
    // suite has: the record rail's own metering reads through it. No bytes are
    // fetched here (see the module docs), so it only has to exist.
    let blob_dir = tempfile::tempdir().unwrap();
    let blob_path = blob_dir.path().to_path_buf();
    std::mem::forget(blob_dir); // outlives the test process; the OS reaps the temp root
    let backup_svc = Arc::new(
        fauna_nest::backup::service::BackupService::new(db.clone(), None, false, blob_path, None)
            .unwrap(),
    );

    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).unwrap();
    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let state = Arc::new(AppState {
        backup_service: Some(backup_svc),
        nest_identity: Arc::new(NestIdentity {
            signing_key,
            verifying_key,
        }),
        federation_router: Arc::new({
            let mut b = fauna_nest::federation_router::FederationRouter::builder();
            fauna_nest::federation_handlers::register_federation_handlers(&mut b);
            b.build()
        }),
        rpc_router: Arc::new({
            let mut b = fauna_nest::rpc_router::RpcRouter::builder();
            // The authenticated `NestClient` mints its bearer over the
            // pre-identity `fauna.auth.handshake`, so the auth-bootstrap kinds
            // must be served before anything else works.
            fauna_nest::auth_handlers::register_auth_handlers(&mut b);
            fauna_nest::discovery_handlers::register_discovery_handlers(&mut b);
            fauna_nest::folder_handlers::register_folders_handlers(&mut b);
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: Arc::new(TokenStore::new()),
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
    (format!("http://{addr}"), state)
}

// ── the world ──────────────────────────────────────────────────────────────

/// H + F, an owner with a **public** folder holding one recorded row, and a
/// follower seat on F that has never touched H.
struct World {
    home_base: String,
    owner: Arc<fauna_client::NestClient>,
    owner_hex: String,
    /// The owner's identity secret — the key its records are signed with.
    owner_secret: [u8; 32],
    follower: Arc<fauna_client::NestClient>,
    /// The follower's account store's follows (`fauna.state.follows`).
    follows: Arc<fauna_client_config::test_helpers::FakeFollowsStore>,
    /// H's database FILE — re-opened by a test to run H's boot pass.
    home_db_path: std::path::PathBuf,
}

impl World {
    async fn build() -> Self {
        // H rests on a file so a test can run its boot pass over it; F never
        // needs one.
        let home_dir = tempfile::tempdir().unwrap();
        let home_db_path = home_dir.path().join("nest.db");
        std::mem::forget(home_dir); // outlives the test process; the OS reaps the temp root
        let (home_base, h_state) = start_nest(CacheDb::open(&home_db_path).unwrap()).await;
        let (follower_base, f_state) = start_nest(CacheDb::open_in_memory().unwrap()).await;

        let mut owner_secret = [0u8; 32];
        getrandom::fill(&mut owner_secret).unwrap();
        let owner_id = ActorKeypair::from_secret(owner_secret).actor_id();
        h_state
            .db
            .create_user(&owner_id.0, "free", "owner")
            .await
            .unwrap();

        // The follower holds no account, roster row, grant or key on H. Its only
        // relationship to the home nest is that its own nest can reach it.
        let mut follower_secret = [0u8; 32];
        getrandom::fill(&mut follower_secret).unwrap();
        let follower_id = ActorKeypair::from_secret(follower_secret).actor_id();
        f_state
            .db
            .create_user(&follower_id.0, "free", "follower")
            .await
            .unwrap();

        let owner = connected_client(&home_base, ActorKeypair::from_secret(owner_secret)).await;
        let follower =
            connected_client(&follower_base, ActorKeypair::from_secret(follower_secret)).await;

        let _: FolderCreateReply = owner
            .request(
                "fauna.folders.create",
                FolderCreateRequest {
                    name: FOLDER.into(),
                    // The client-minted set nonce every signed record binds to.
                    set_nonce: common::set_nonce_field(),
                    ..Default::default()
                },
            )
            .await
            .expect("create folder");
        let _: SyncRegisterReply = owner
            .request(
                "fauna.sync.register",
                SyncRegisterRequest {
                    device_id: hex::encode(DEVICE_ID),
                    label: "seed".into(),
                    // `capabilities` defaults to the wire default "read,write"
                    // (`SyncRegisterRequest`'s hand-written `Default`).
                    ..Default::default()
                },
            )
            .await
            .expect("register device");

        let world = Self {
            home_base,
            owner,
            owner_hex: hex::encode(owner_id.0),
            owner_secret,
            follower,
            follows: Arc::new(fauna_client_config::test_helpers::FakeFollowsStore::empty()),
            home_db_path,
        };
        world.set_audience("public").await;
        world.record("published.txt").await;
        world
    }

    async fn set_audience(&self, audience: &str) {
        let reply: FolderUpdateReply = self
            .owner
            .request(
                "fauna.folders.update",
                FolderUpdateRequest {
                    name: FOLDER.into(),
                    audience: Some(audience.into()),
                    ..Default::default()
                },
            )
            .await
            .expect("set audience");
        assert!(reply.ok, "audience flip to {audience} refused");
    }

    /// Record one plaintext row. Only legal while the folder is public — a
    /// plaintext `path` with no `path_sealed` is refused outside a public folder
    /// (the S9 `path_seal_required` gate, whose plaintext arm *is* the
    /// public-audience exemption).
    async fn record(&self, path: &str) -> i64 {
        self.record_with_seal(path, None).await
    }

    /// Record one row, optionally carrying a `path_sealed` envelope beside its
    /// plaintext path — the shape an owner engine still sealing across a flip
    /// to public writes (the nest stores the envelope as sent). Signed by the
    /// owner's identity key under the set's nonce, as the owner's engine signs
    /// every record (the nest refuses an unsigned one `signature_required`).
    async fn record_with_seal(&self, path: &str, path_sealed: Option<Vec<u8>>) -> i64 {
        let req = common::signed_record(
            SyncChangeRecordRequest {
                folder: FOLDER.into(),
                device_id: hex::encode(DEVICE_ID),
                path: path.into(),
                // No bytes are fetched here (see the module docs), so the
                // manifest ref only has to be a well-formed pointer.
                manifest_hash: Some(hex::encode([0x11u8; 32])),
                size_bytes: 11,
                change_type: "create".into(),
                path_sealed: path_sealed.map(serde_bytes::ByteBuf::from),
                ..Default::default()
            },
            &ActorKeypair::from_secret(self.owner_secret),
        );
        let reply: SyncChangeRecordReply = self
            .owner
            .request("fauna.sync.changes.record", req)
            .await
            .expect("record change");
        reply.seq
    }

    /// First contact over the relay, as an app makes it.
    async fn resolve(
        &self,
    ) -> Result<
        fauna_client_folders::public_follow::PublicFolderPage,
        fauna_client_folders::public_follow::FollowError<fauna_client::NestClientError>,
    > {
        fauna_client_folders::public_follow::resolve_public_folder(
            &self.follower,
            &self.home_base,
            &self.owner_hex,
            FOLDER,
            0,
        )
        .await
    }

    /// The production projection source, built exactly as an app would build it.
    fn source(&self) -> fauna_devices_machine::StoreFollowedFoldersSource {
        fauna_devices_machine::StoreFollowedFoldersSource::new(
            Arc::clone(&self.follower),
            Arc::clone(&self.follows) as Arc<dyn fauna_client_config::FollowsStore>,
        )
    }
}

// ── the tests ──────────────────────────────────────────────────────────────

/// First contact over the relay, persisted, and read back — the record the
/// relay pins addresses the HOME nest and carries its identity, on live wire
/// rather than a fake nest.
#[tokio::test]
async fn a_follow_pins_the_home_identity_through_the_relay() {
    let world = World::build().await;

    let page = world
        .resolve()
        .await
        .expect("the relay serves a public folder");
    let record = page.record.clone();
    assert!(record.folder_id > 0, "the pinned id the follow addresses");
    assert_eq!(record.display_name, FOLDER);
    assert_eq!(
        record.home_nest_url, world.home_base,
        "the record must address the HOME nest — it is the byte-plane dial"
    );
    assert!(
        record.home_nest_actor_id.is_some(),
        "the SPKI-pin trust root must survive the relay into the stored record"
    );

    let stored = fauna_client_config::save_follow(&*world.follows, record.clone())
        .await
        .expect("persist the follow");
    assert_eq!(stored, vec![record.clone()]);

    let reloaded = fauna_client_config::load_followed_folders(&*world.follows)
        .await
        .expect("reload");
    assert_eq!(reloaded, vec![record], "the stored follow reads back whole");
}

/// `StoreFollowedFoldersSource` against a real nest: the row it emits is the
/// row a followed-folders list renders, and its `available` verdict comes from
/// an actual relayed read of H rather than a stub.
///
/// ⚠ **`available == true` is not on its own evidence that the probe reached H**
/// — a *transport* fault also keeps a row available, deliberately (a dropped
/// connection must not read as a revoke), and the fallback name it then shows is
/// the stored one, which here is the same string. So this test alone cannot tell
/// a served reply from a silent failure, and the assertions below are about the
/// row's **shape**. What supplies the missing half is
/// `a_flip_back_turns_the_row_unavailable_without_dropping_it`: it drives this
/// same source to `false` and back across real audience flips on H, and only the
/// plane's own refusal can move that verdict. Do not delete that test believing
/// this one covers it.
#[tokio::test]
async fn the_source_projects_a_live_follow_as_available() {
    let world = World::build().await;
    let record = world.resolve().await.expect("resolve").record;
    fauna_client_config::save_follow(&*world.follows, record.clone())
        .await
        .expect("persist");

    let rows = {
        use fauna_devices_machine::FollowedFoldersSource as _;
        world.source().followed_folders().await
    };

    assert_eq!(rows.len(), 1, "one follow, one row: {rows:?}");
    let row = &rows[0];
    assert_eq!(row.folder_id, record.folder_id);
    assert_eq!(row.home_nest_url, world.home_base);
    assert_eq!(row.owner_actor_id, world.owner_hex);
    assert_eq!(
        row.display_name, FOLDER,
        "a live probe refreshes the name from the reply, so a rename shows up"
    );
    assert!(
        row.available,
        "the folder is public and serving — the probe must say so"
    );
}

/// The revoke, as the follower's own row experiences it. The owner re-seals on
/// H; the follower's next refresh keeps the row (it is the user's, and a re-flip
/// resumes it) but marks it unavailable.
#[tokio::test]
async fn a_flip_back_turns_the_row_unavailable_without_dropping_it() {
    use fauna_devices_machine::FollowedFoldersSource as _;

    let world = World::build().await;
    let record = world.resolve().await.expect("resolve").record;
    fauna_client_config::save_follow(&*world.follows, record.clone())
        .await
        .expect("persist");
    assert!(
        world.source().followed_folders().await[0].available,
        "precondition: the follow starts available"
    );

    world.set_audience("private").await;

    let rows = world.source().followed_folders().await;
    assert_eq!(
        rows.len(),
        1,
        "a revoke must NOT drop the user's row — it is theirs to remove"
    );
    assert!(
        !rows[0].available,
        "the plane's own not_found is the revoke; the row must show it"
    );
    assert_eq!(
        rows[0].display_name, FOLDER,
        "an unavailable row keeps its last known name so it stays recognisable"
    );

    // And a re-flip resumes the very same follow — the pinned id is stable, so
    // nothing the follower stored has to change.
    world.set_audience("public").await;
    let rows = world.source().followed_folders().await;
    assert!(
        rows[0].available,
        "a re-flip resumes the follow under the same pinned id"
    );
    assert_eq!(rows[0].folder_id, record.folder_id);
}

/// Unfollow is a purely local delete: nothing on H changes, and the follower's
/// own store no longer carries the record.
#[tokio::test]
async fn unfollow_removes_the_row_and_leaves_the_home_nest_untouched() {
    use fauna_devices_machine::FollowedFoldersSource as _;

    let world = World::build().await;
    let record = world.resolve().await.expect("resolve").record;
    fauna_client_config::save_follow(&*world.follows, record.clone())
        .await
        .expect("persist");

    let after = fauna_client_config::save_unfollow(
        &*world.follows,
        &record.home_nest_url,
        record.folder_id,
    )
    .await
    .expect("unfollow");
    assert!(after.is_empty(), "unfollow removes the record");

    assert!(
        world.source().followed_folders().await.is_empty(),
        "and the list source stops emitting it"
    );

    // The folder is untouched on H: it still serves anyone who asks, which is
    // what "unfollow writes nothing on the home nest" means in practice.
    assert!(
        world.resolve().await.is_ok(),
        "unfollowing must not have disturbed the home nest's folder"
    );
}

/// The Media machine's followed browse seam (`fauna_core::followed_media`) on
/// the same production glue, against real nests: `fetch_listing` serves the
/// **head-folded listing** of the followed folder over the relay, the plane's
/// refusal maps to `Unavailable` — and, the shared-verdict contract, that
/// refusal feeds the very cache the Folders-page projection reads, so the two
/// surfaces cannot race contradicting answers (`media.md` § Followed public
/// folders: "a browse fetch is availability evidence").
#[tokio::test]
async fn fetch_listing_folds_the_live_log_and_feeds_the_shared_verdict() {
    use fauna_core::followed_media::{FollowedFetchError, FollowedMediaSource as _};
    use fauna_devices_machine::FollowedFoldersSource as _;

    let world = World::build().await;
    let record = world.resolve().await.expect("resolve").record;
    fauna_client_config::save_follow(&*world.follows, record.clone())
        .await
        .expect("persist");
    world.record("second.txt").await;

    // ONE instance for the whole test — the shared cache is the point.
    let source = world.source();
    let scopes = source.followed_scopes().await;
    assert_eq!(scopes.len(), 1, "one follow, one browse scope");
    assert_eq!(scopes[0].folder_id, record.folder_id);
    assert!(scopes[0].available);

    let listing = source
        .fetch_listing(scopes[0].folder_id, &scopes[0].home_nest_url)
        .await
        .expect("a public folder's listing serves over the relay");
    let paths: Vec<&str> = listing.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        ["published.txt", "second.txt"],
        "the head fold of the live change log, in path order"
    );
    assert!(
        listing.iter().all(|e| !e.manifest_hash.is_empty()),
        "every head carries the manifest the keyless download addresses"
    );

    world.set_audience("private").await;
    let err = source
        .fetch_listing(scopes[0].folder_id, &scopes[0].home_nest_url)
        .await
        .expect_err("the re-sealed folder refuses the browse");
    assert!(
        matches!(err, FollowedFetchError::Unavailable),
        "the plane's own refusal is the revoke, never a transport error: {err:?}"
    );

    // The refusal fed the shared cache: the Folders-page projection on the
    // SAME source answers unavailable from that verdict — fresh within the
    // staleness budget, so no second probe is needed to agree.
    let rows = source.followed_folders().await;
    assert!(
        !rows[0].available,
        "one verdict, two consumers — the browse fetch IS the probe"
    );
}

/// An over-sealing owner writer on a PUBLIC folder — its engine still sealing
/// in the tick between the flip to public and its next mode refresh — rests a
/// `path_sealed` envelope beside the row's plaintext path. H's boot pass (the
/// S9 plaintext scrub) must leave that plaintext resting: the public
/// projection withholds the seal from the follower, so a scrubbed row would
/// reach it with neither label and be refused as `NoSeal`, the file lost to
/// every follower (`path-sealing.md` § The S9 scrub).
///
/// "Across a restart" is H's database file re-opened while H serves: the open
/// IS the boot pass (migrations, reconcilers, the scrub and its VACUUM), and
/// H's own serving handle then reads whatever it left.
#[tokio::test]
async fn an_over_sealed_public_row_survives_the_boot_scrub_for_the_follower() {
    let world = World::build().await;
    let seq = world
        .record_with_seal("over-sealed.txt", Some(vec![0xAA; 64]))
        .await;

    // The precondition, read off H's file: the seal rests BESIDE the plaintext
    // — the exact shape the scrub would clear.
    let rests_both: bool = rusqlite::Connection::open(&world.home_db_path)
        .unwrap()
        .query_row(
            "SELECT path IS NOT NULL AND path_sealed IS NOT NULL FROM sync_changes WHERE seq = ?1",
            [seq],
            |r| r.get(0),
        )
        .unwrap();
    assert!(rests_both, "the over-sealed row rests plaintext and seal");

    // H's boot pass over the same file.
    drop(CacheDb::open(&world.home_db_path).unwrap());

    let record = world.resolve().await.expect("resolve").record;
    let page =
        fauna_client_folders::public_follow::fetch_followed_changes(&world.follower, &record, 0)
            .await
            .expect("the public folder's log serves over the relay");
    let row = page
        .changes
        .iter()
        .find(|c| c.seq == seq)
        .expect("the over-sealed row is in the follower's page");
    assert_eq!(
        row.path.as_deref(),
        Some("over-sealed.txt"),
        "a public folder's plaintext path is the only label a follower ever \
         receives — the boot scrub must not destroy it beside a seal"
    );
    assert!(
        row.path_sealed.is_none(),
        "the public projection still withholds the seal"
    );
}
