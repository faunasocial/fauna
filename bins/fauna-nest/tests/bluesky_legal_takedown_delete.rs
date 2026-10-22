#![cfg(feature = "bluesky")]
//! tier_3: the Bluesky write-through delete leg of a LEGAL TAKEDOWN — a post a
//! member cross-posted through their linked Bluesky account is retracted there
//! when an admin takes it down, exactly as the author's own delete retracts it
//! (`docs/goal/behavior/takedown-and-block.md` § Legal takedown → *The linked
//! Bluesky account's cross-post is retracted too*; the derived-lifecycle rule,
//! `docs/goal/ui/feed.md` § Post deletion → *Propagation*).
//!
//! Drives the REAL production flow over router dispatch: author links a
//! Bluesky account → creates a post via `fauna.posts.create` → the
//! `bluesky_posts` mapping a cross-post writes is stored → an admin takes the
//! post down via `fauna.moderation.legal_takedown`. The harness has no PDS (no
//! OAuth client is configured), so the leg's attempt fails at the agent step —
//! the attempt itself is observed on the nest's log, the mapping is RETAINED
//! (retention-on-failure), and the hourly re-drive lists the post as a
//! stranded candidate although the post is withheld, not deleted. A repeat
//! takedown leaves that state as it was; a restore re-creates nothing and ends
//! the chase (the mapping then waits for the author's own delete).
//!
//! The success arm — `deleteRecord` reaching a PDS and the mapping cleared — is
//! the author-delete leg's, unchanged here; it needs a restorable OAuth
//! session, which no hermetic harness holds.

mod common;
use common::dispatch;

use std::io::Write;
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use fauna_nest::bluesky::{db_helpers, init_db};
use fauna_nest::db::CacheDb;
use fauna_nest::moderation_handlers;
use fauna_nest::post_delete_redrive::bluesky_redrive_candidates;
use fauna_nest::posts_handlers;
use fauna_nest::routes::AppState;
use fauna_nest::rpc_router::RpcRouter;
use fauna_protocol::{
    decode_strict as decode, encode_canonical,
    moderation::{ModerationLegalTakedownReply, ModerationLegalTakedownRequest},
    posts::PostCreateRequest,
};

const CROSSPOST_URI: &str = "at://did:plc:takedownauthor/app.bsky.feed.post/3ktakedown";

/// The nest's log, captured: the leg is fire-and-forget, and its attempt is
/// what tells "the takedown ran the leg and it failed" from "the takedown never
/// ran it" — both leave the mapping in place.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogCapture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

async fn router_and_state() -> (RpcRouter, Arc<AppState>) {
    let db = Arc::new(CacheDb::open_in_memory().expect("open in-memory db"));
    init_db(&db).await.expect("init bluesky tables");
    let state = Arc::new(AppState::for_test(db));
    let mut b = RpcRouter::builder();
    posts_handlers::register_posts_handlers(&mut b);
    moderation_handlers::register_moderation_handlers(&mut b);
    (b.build(), state)
}

fn takedown_payload(post_id_hex: &str, reference: &str, restore: bool) -> Bytes {
    Bytes::from(
        encode_canonical(&ModerationLegalTakedownRequest {
            content_id: post_id_hex.to_string(),
            content_type: "post".into(),
            legal_reference: reference.to_string(),
            restore,
            extra: Default::default(),
        })
        .unwrap()
        .to_vec(),
    )
}

/// Create a post over the wire and record the cross-post mapping the create
/// leg's success writes (e2e carve-out (b): fixture setup, not the mutation
/// under test — which is the TAKEDOWN below).
async fn create_crossposted(
    router: &RpcRouter,
    state: &Arc<AppState>,
    author_kp: &fauna_core::identity::ActorKeypair,
) -> String {
    let actor = author_kp.actor_id().0;
    let body = {
        use fauna_core::data::{Post, PostBody, Timestamp};
        use fauna_core::encoding::sign_and_pack;
        let post = Post {
            author: author_kp.actor_id(),
            created_at: Timestamp(1_000_000_000_000),
            body: PostBody::Text {
                content: "cross-posted, then taken down".into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        sign_and_pack(author_kp, &post).unwrap()
    };
    let post_id = hex::encode(blake3::hash(&body).as_bytes());
    dispatch(
        router,
        state.clone(),
        actor,
        "fauna.posts.create",
        Bytes::from(
            encode_canonical(&PostCreateRequest {
                body: serde_bytes::ByteBuf::from(body),
                extra: Default::default(),
            })
            .unwrap()
            .to_vec(),
        ),
    )
    .await
    .expect("create ok");

    let conn = state.db.conn().await;
    db_helpers::upsert_linked_account(
        &conn,
        &hex::encode(actor),
        "did:plc:takedownauthor",
        "author.example.com",
    )
    .unwrap();
    db_helpers::store_crosspost_mapping(
        &conn,
        &post_id,
        CROSSPOST_URI,
        "did:plc:takedownauthor",
        "bafy-takedown",
    )
    .unwrap();
    post_id
}

async fn mapping(state: &AppState, post_id: &str) -> Option<String> {
    let conn = state.db.conn().await;
    db_helpers::get_crosspost_uri(&conn, post_id).unwrap()
}

/// Wait up to 10 s for the leg's first attempt on `post_id` to reach the log.
async fn await_attempt(log: &LogCapture, post_id: &str) -> String {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let line = log
            .text()
            .lines()
            .find(|l| l.contains("write-through delete attempt 1/") && l.contains(post_id))
            .map(str::to_owned);
        if let Some(line) = line {
            return line;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the takedown never attempted the Bluesky delete; log:\n{}",
            log.text()
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn takedown_attempts_the_bluesky_delete_and_the_redrive_chases_a_failure() {
    let log = LogCapture::default();
    let writer = log.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );

    let (router, state) = router_and_state().await;
    let author_kp = fauna_core::identity::ActorKeypair::generate();
    let author = author_kp.actor_id().0;
    let admin = [0x2Au8; 32];
    state.db.add_admin_actor(&admin).await.unwrap();
    let post_id = create_crossposted(&router, &state, &author_kp).await;
    let post_digest: [u8; 32] = hex::decode(&post_id).unwrap().try_into().unwrap();

    assert!(
        bluesky_redrive_candidates(&state, None)
            .await
            .unwrap()
            .is_empty(),
        "a live cross-post is nothing to chase"
    );

    let reply = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_id, "EU-DSA-2026/1597", false),
    )
    .await
    .expect("takedown ok");
    let reply: ModerationLegalTakedownReply = decode(&reply).unwrap();
    assert_eq!(reply.status, "taken_down");

    // The leg ran, for the post's author, and failed at the agent step — the
    // outage a real PDS can have.
    let attempt = await_attempt(&log, &post_id).await;
    assert!(
        attempt.contains("agent error"),
        "the attempt fails at the agent step: {attempt}"
    );
    assert_eq!(
        mapping(&state, &post_id).await.as_deref(),
        Some(CROSSPOST_URI),
        "a failed delete keeps the mapping for the next attempt"
    );
    assert_eq!(
        bluesky_redrive_candidates(&state, None).await.unwrap(),
        vec![(author, post_digest)],
        "the re-drive chases a taken-down post's stranded cross-post"
    );

    // A repeat takedown leaves the state as it was.
    dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_id, "EU-DSA-2026/1597", false),
    )
    .await
    .expect("repeat takedown ok");
    assert_eq!(
        mapping(&state, &post_id).await.as_deref(),
        Some(CROSSPOST_URI)
    );
    assert_eq!(
        bluesky_redrive_candidates(&state, None).await.unwrap(),
        vec![(author, post_digest)]
    );

    // A restore re-creates nothing and ends the chase: the mapping that is
    // still there now waits for the author's own delete.
    let reply = dispatch(
        &router,
        state.clone(),
        admin,
        "fauna.moderation.legal_takedown",
        takedown_payload(&post_id, "EU-DSA-2026/1597 overturned", true),
    )
    .await
    .expect("restore ok");
    let reply: ModerationLegalTakedownReply = decode(&reply).unwrap();
    assert_eq!(reply.status, "restored");
    assert_eq!(
        mapping(&state, &post_id).await.as_deref(),
        Some(CROSSPOST_URI)
    );
    assert!(
        bluesky_redrive_candidates(&state, None)
            .await
            .unwrap()
            .is_empty(),
        "a restored post is not stranded"
    );
}
