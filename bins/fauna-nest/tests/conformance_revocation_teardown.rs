//! Integration test — revocation teardown (`transport-connection.md`
//! § Connection lifecycle → *Revocation teardown*).
//!
//! The nest validates a bearer **exactly once**, at the WS upgrade, then bakes
//! the actor into `RpcConnection` for the connection's lifetime; `dispatch_core`
//! never re-reads the token store. So `TokenStore::revoke_actor` — which every
//! revocation path already called — killed only the *next* connection, and left
//! the one the actor already held fully functional. Before this,
//! `rg 4401 bins/` returned nothing: every app knew how to handle the close
//! code, and the nest had no producer of it.
//!
//! Six properties, one per thing the teardown must get right:
//!
//! 1. **The close code is 4401, not 1001.** `1001` maps to
//!    `ReconnectSignal::Retry` — reconnect with the bearer you have. `4401` maps
//!    to `AuthExpired` — `clear_token()` → `ensure_auth()` → reconnect
//!    (`libs/fauna-ws-substrate/src/adapter.rs`,
//!    `libs/fauna-rpc-wasm/src/adapter.rs`). A revoked actor's bearer is
//!    worthless, so 1001 would send it back with a dead credential; 4401 makes it
//!    re-authenticate, and `auth_core` then refuses the re-mint. Getting this
//!    backwards is the same class of bug as the 1001-vs-1000 footgun that
//!    `graceful_shutdown.rs` guards.
//! 2. **Dispatch stops.** The revoked connection accepts no further requests —
//!    the guarantee `fauna.sessions.lockout` (the *emergency* control, whose
//!    whole point is to cut a compromised session off *now*) previously lacked
//!    entirely: lockout is enforced only at token mint, and
//!    `caller_class_for_actor` reads `users.suspended` but never
//!    `users.locked_until`.
//! 3. **An actor with no live connection is unaffected**, and revoking is
//!    idempotent — the background eviction ladder and the pending-action executor
//!    both call it unconditionally.
//! 4. **A handler that revokes its own caller still answers that one request.**
//!    Added 2026-09-12 for the identity-succession ceremony, the only path whose
//!    Reply is the ceremony's product rather than a courtesy: `new_actor_id` +
//!    `succeeded_at`. Everything else about the teardown is unchanged for that
//!    connection (dispatch stops now) and entirely unchanged for every other
//!    connection the actor holds (4401 now, no drain) — and any frame already
//!    queued ahead of that Reply, a Push included, still reaches the spared
//!    socket before its own 4401. The three companion tests below pin all
//!    three halves, because a grace that quietly became a reprieve, in any of
//!    those ways, would undo the eviction this whole file exists for.
//! 5. **Revoking one SESSION closes that session's sockets, and only those.**
//!    Added 2026-09-20 with the per-token teardown. Properties 1–4 are
//!    per-actor: the whole identity loses its authority. `fauna.sessions.
//!    {revoke,revoke_all}` is the per-token twin (`devices.md` § What a session
//!    is, and what revoking one does) — one bearer of one actor ends, and the
//!    actor's other sessions must carry on. It shipped doing the token half
//!    alone, so a revoked session kept dispatching as `User` on the socket it
//!    already held; the assertions come in pairs (the revoked session's socket
//!    dies, the sibling's lives) because a teardown that only got the first
//!    half right would sign every device a user owns out each time they
//!    dismissed one.
//! 6. **Removing a DEVICE closes the sockets its key minted, and only those.**
//!    Added 2026-09-23. Device removal — `fauna.sync.devices.delete`,
//!    `fauna.sync.device_grant.revoke`, and graduation's severance of a marked
//!    guardian device — revokes every session the device's renewal key minted
//!    (`sync-agent-credentials.md` § Credential model, decision 2). All three
//!    shipped doing the token half alone, so the thief a device deletion
//!    targets kept dispatching as `User` on the socket it already held. Same
//!    pairs as property 5: the device-minted socket dies, a direct sign-in
//!    socket of the same actor lives — and, because the grant revoke is
//!    routinely presented by the very device it retires, the same one-frame
//!    grace for the caller's own Reply.
//!
//! Tier: tier_3 (real `AppState`, real axum server, real WebSocket over
//! tungstenite, real `CacheDb` — no mocks). The unit of proof is the wire.

mod common;
use common::{Ws, open_authed, read_until_close};

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use fauna_core::identity::ActorKeypair;
use fauna_nest::db::CacheDb;
use fauna_nest::rpc_router::{RpcKindMeta, RpcRouter};
use fauna_nest::token_store::TokenStore;
use fauna_protocol::push_events::AccountUpdatedPayload;
use fauna_protocol::sessions::{RevokeAllRequest, RevokeRequest};
use fauna_protocol::{
    Frame, PushEvent, Request, Value, decode_frame, decode_strict, encode_canonical,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

struct Harness {
    url: String,
    kp: ActorKeypair,
    /// The first bearer, and the `token_id` the nest filed it under. The
    /// per-token teardown is keyed on that id, so a harness that only kept the
    /// raw token could not name the session it is revoking.
    token: String,
    token_id: String,
    state: Arc<fauna_nest::routes::AppState>,
    gate: Arc<RevokeGate>,
}

impl Harness {
    /// Mint another bearer for the same actor — a second (third, …) *session*,
    /// which is what the per-token teardown has to tell apart. Returns
    /// `(raw token, token_id)`.
    async fn mint(&self) -> (String, String) {
        let minted = self
            .state
            .auth
            .token_store
            .insert_with_metadata(self.kp.actor_id(), 3600, None, None)
            .await;
        (minted.token, minted.token_id)
    }
}

/// Lets a test stand *inside* the window a self-revoking handler opens: after
/// the authority is stripped and before its Reply is emitted.
///
/// Two one-shot signals rather than one, for the reason `auth_core::mint_race`
/// gives: the handler must both *announce* that it has revoked and *wait* to be
/// let through, and a single `Notify` cannot express both without the test
/// guessing which side it woke. `notify_one` stores a permit when nobody is
/// waiting, so neither side can miss the other by arriving first.
///
/// It replaces the obvious alternative — let the handler yield a few times and
/// hope the outbound task is polled in between — which decides the property by a
/// scheduling race rather than proving it, in *both* directions: it could leave
/// the pre-fix red intermittent, and it gives a test that wants to pipeline a
/// second request no window at all to do it in.
#[derive(Default)]
struct RevokeGate {
    /// Raised by the handler once it has stripped its own caller's authority.
    revoked: tokio::sync::Notify,
    /// Raised by the test once it is done observing that window.
    may_reply: tokio::sync::Notify,
}

/// A trivial always-ok kind, so "did dispatch happen" is a clean signal
/// uncontaminated by handler logic.
///
/// ⚠ **The names are real kinds, and that is not decoration.** A harness that
/// registers an invented `fauna.test.<x>` into its own router builds a kind that
/// is registered, not pre-identity, and absent from
/// `bridge_method_allowlist::is_permitted` — so `routes.rs` gate (1d) refuses it
/// with `permission_denied` and the handler never runs. The refusal is itself a
/// `Frame::Reply`, which is how this file's echo kind spent its life answering
/// property 2's precondition with a refusal (`read_until_close` now requires
/// `ok` so that cannot recur). `graceful_shutdown.rs` names `fauna.posts.create`
/// for the same reason; the echo here follows it, and the revoking kind is the
/// production succession kind itself, whose pre-identity status is exactly what
/// exempts it from that gate in production too.
const ECHO_KIND: &str = "fauna.posts.create";
const REVOKE_SELF_KIND: &str = "fauna.recovery.succession.submit";
/// The handle domain the ward's admission registers under (property 6).
const DOMAIN: &str = "test.fauna.social";

async fn start() -> Harness {
    let db = Arc::new(CacheDb::open_in_memory().unwrap());
    let tokens = Arc::new(TokenStore::new());
    let kp = ActorKeypair::generate();
    db.create_user(&kp.actor_id().0, "free", "alice").await.ok();
    let minted = tokens
        .insert_with_metadata(kp.actor_id(), 3600, None, None)
        .await;
    let (token, token_id) = (minted.token, minted.token_id);
    let gate = Arc::new(RevokeGate::default());
    let handler_gate = Arc::clone(&gate);

    let state = Arc::new(fauna_nest::routes::AppState {
        rpc_router: Arc::new({
            let mut b = RpcRouter::builder();
            b.add(
                ECHO_KIND,
                RpcKindMeta {
                    forbid_replay: false,
                    default_deadline: Duration::from_secs(5),
                    handler: Box::new(move |_state, _actor, _payload| {
                        Box::pin(async move {
                            Ok(Bytes::from(encode_canonical(&true).unwrap().to_vec()))
                        })
                    }),
                },
            );
            // The shape of `fauna.recovery.succession.submit`, reduced to the
            // one thing that makes it different from every other member of the
            // teardown enumeration: the handler strips its OWN caller's
            // authority and then has something to say about it.
            b.add(
                REVOKE_SELF_KIND,
                RpcKindMeta {
                    forbid_replay: false,
                    default_deadline: Duration::from_secs(5),
                    handler: Box::new(move |state, actor, _payload| {
                        let gate = Arc::clone(&handler_gate);
                        Box::pin(async move {
                            state.revoke_actor_authority_sparing_caller(&actor).await;
                            // The real ceremony has post-revoke work here — it
                            // heals segment directories and rotates the
                            // successor's inherited period keys, both `await`
                            // points. This stands in for that span, but as a
                            // barrier rather than a delay: the test holds the
                            // handler open for exactly as long as it needs to
                            // look, then lets the Reply go.
                            gate.revoked.notify_one();
                            gate.may_reply.notified().await;
                            Ok(Bytes::from(encode_canonical(&true).unwrap().to_vec()))
                        })
                    }),
                },
            );
            // The production session-management surface, registered whole. The
            // per-token teardown is a property of `fauna.sessions.{revoke,
            // revoke_all}` as the client drives them, so the harness runs the
            // real handlers rather than a stand-in: a hand-written double
            // could not have had the defect this file's per-token half exists
            // to pin, which was that the arms called `TokenStore` and stopped.
            fauna_nest::session_handlers::register_sessions_handlers(&mut b);
            // The three device-removal doors (property 6), registered whole for
            // the same reason: `fauna.sync.{devices.delete,device_grant.revoke}`
            // and `fauna.family.graduate` shipped doing the token half alone,
            // which only the production handlers could reproduce. The account
            // surface is here for the ward's admission — the one production way
            // a guardianship link comes to exist.
            fauna_nest::sync_handlers::register_sync_handlers(&mut b);
            fauna_nest::family_handlers::register_family_handlers(&mut b);
            fauna_nest::account_handlers::register_account_handlers(&mut b);
            b.build()
        }),
        auth: fauna_nest::state::AuthState {
            token_store: tokens.clone(),
            registration: fauna_nest::routes::RegistrationConfig {
                handle_domain: Some(DOMAIN.to_string()),
                reserved_handles: vec![],
            },
            ..Default::default()
        },
        registration_mode: Arc::new(tokio::sync::RwLock::new((
            fauna_protocol::node_policy::RegistrationMode::InviteRequired,
            None,
        ))),
        enforce_tier_quotas: std::sync::Arc::new(tokio::sync::RwLock::new(true)),
        ..fauna_nest::routes::AppState::for_test(db.clone())
    });

    let app = fauna_nest::build_router(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    Harness {
        url: format!("ws://{addr}"),
        kp,
        token,
        token_id,
        state,
        gate,
    }
}

fn echo_frame(corr: u64) -> Message {
    request_frame(ECHO_KIND, corr)
}

fn request_frame(kind: &str, corr: u64) -> Message {
    request_frame_with(kind, corr, Value::Null)
}

fn request_frame_with(kind: &str, corr: u64, payload: Value) -> Message {
    Message::Binary(
        fauna_protocol::encode_frame(&Frame::Request(Request {
            ty: Request::TYPE,
            correlation_id: corr,
            kind: kind.to_string(),
            idempotency_key: [corr as u8; 16],
            payload,
            replay_forbidden: Some(false),
            deadline_ms: None,
        }))
        .unwrap(),
    )
}

/// Encode a typed request body into the L3 `Value` a `Frame::Request` carries.
fn to_value<T: serde::Serialize>(t: &T) -> Value {
    decode_strict::<Value>(&encode_canonical(t).unwrap()).unwrap()
}

fn revoke_frame(corr: u64, token_id: &str) -> Message {
    request_frame_with(
        "fauna.sessions.revoke",
        corr,
        to_value(&RevokeRequest {
            token_id: token_id.to_string(),
            extra: Default::default(),
        }),
    )
}

fn revoke_all_frame(corr: u64, keep_token_id: &str) -> Message {
    request_frame_with(
        "fauna.sessions.revoke_all",
        corr,
        to_value(&RevokeAllRequest {
            keep_token_id: keep_token_id.to_string(),
            extra: Default::default(),
        }),
    )
}

/// Read until a **successful** Reply for `corr` arrives on a socket that is
/// expected to stay OPEN. The counterpart of [`read_until_close`], which needs
/// a close frame to terminate and so cannot be used on a surviving connection:
/// here a close is a *failure*, and is reported as such rather than waited for.
async fn expect_ok_reply(ws: &mut Ws, corr: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => match decode_frame(&bytes) {
                Ok(Frame::Reply(r)) if r.correlation_id == corr => return r.ok,
                _ => continue,
            },
            Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => return false,
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// Property 1 — the close code. `1001` would tell the client to come straight
/// back with the bearer it just had revoked.
#[tokio::test]
async fn revoking_an_actor_closes_its_live_socket_with_4401() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    // Let the server-side subscription land in `WsState.subs`.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let closed = h.state.ws.disconnect_actor(&h.kp.actor_id().0);
    assert_eq!(
        closed, 1,
        "the actor's one live connection should be signalled"
    );

    let (code, _) = read_until_close(&mut ws, None).await;
    assert_eq!(
        code,
        Some(4401),
        "a revoked actor's socket must close 4401 (AuthExpired → clear bearer, \
         re-authenticate), never 1001 (Retry → reconnect with the dead bearer)"
    );
}

/// Property 2 — dispatch stops. This is the whole of `fauna.sessions.lockout`'s
/// intent: the emergency control must cut off a session that is *already open*.
#[tokio::test]
async fn a_revoked_connection_dispatches_nothing_further() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Sanity: the connection works before revocation, so a later absence of
    // replies means "revoked", not "this kind never worked".
    ws.send(echo_frame(1)).await.unwrap();
    let mut saw_first_reply = false;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_millis(400), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                if matches!(decode_frame(&b), Ok(Frame::Reply(_))) {
                    saw_first_reply = true;
                    break;
                }
            }
            Ok(Some(Ok(_))) => continue,
            _ => break,
        }
    }
    assert!(
        saw_first_reply,
        "precondition: echo must work pre-revocation"
    );

    h.state.ws.disconnect_actor(&h.kp.actor_id().0);
    // Race the close: send immediately, before the client observes the frame.
    let _ = ws.send(echo_frame(2)).await;

    let (code, reply_seen) = read_until_close(&mut ws, Some(2)).await;
    assert_eq!(code, Some(4401), "expected the 4401 close");
    assert!(
        !reply_seen,
        "a revoked connection answered an RPC issued after revocation — the \
         emergency lockout does not actually cut the session off"
    );
}

/// Property 3 — the callers (`eviction.rs`'s ladder, the pending-action executor,
/// `admin.users.suspend`) revoke unconditionally, so an actor with no live socket
/// and a repeated revoke must both be no-ops rather than panics.
#[tokio::test]
async fn disconnecting_an_actor_with_no_connections_is_a_noop_and_is_idempotent() {
    let h = start().await;

    let stranger = ActorKeypair::generate().actor_id().0;
    assert_eq!(
        h.state.ws.disconnect_actor(&stranger),
        0,
        "an actor with no live connection has nothing to close"
    );

    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.state.ws.disconnect_actor(&h.kp.actor_id().0), 1);
    // Second call: the connection is already flagged; `watch::send` is idempotent.
    h.state.ws.disconnect_actor(&h.kp.actor_id().0);

    let (code, _) = read_until_close(&mut ws, None).await;
    assert_eq!(
        code,
        Some(4401),
        "a double revoke still yields exactly one 4401"
    );
}

/// Property 4 — a handler that revokes its **own caller** still answers the
/// request that caused the revocation.
///
/// Every other member of the teardown enumeration strips authority from someone
/// who is not asking: an admin suspends a user, the ladder evicts an account,
/// the executor deletes one. For those the 4401 really is the whole message, and
/// dropping whatever was queued behind it costs nothing.
///
/// The identity-succession ceremony is not that shape. `fauna.recovery.
/// succession.submit` retires the very identity whose socket carries it, and its
/// Reply — `new_actor_id` + `succeeded_at` — is the ceremony's entire product.
/// Drop it and the client cannot distinguish "the account moved" from "nothing
/// happened": `succeed_with_held_kit` takes its `Unconfirmed` arm, logs
/// *succession submit failed after the successor was minted — outcome unknown*,
/// and the app is left reconciling a ceremony that in fact committed
/// (`identity-succession.md` § Enforcement on the home nest).
///
/// So the spared connection is exactly one — the caller's — and its grace runs
/// only through that one Reply. Dispatch stops for it at the same instant as
/// for every sibling connection (property 2 still holds here, and
/// `a_spared_connection_dispatches_nothing_further` below pins it); all the
/// sparing buys is the answer to the question already in flight.
#[tokio::test]
async fn a_handler_that_revokes_its_own_caller_still_delivers_that_reply() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    ws.send(request_frame(REVOKE_SELF_KIND, 7)).await.unwrap();

    // Stand inside the window: the authority is gone, the answer is not out yet.
    h.gate.revoked.notified().await;
    h.gate.may_reply.notify_one();

    let (code, reply_seen) = read_until_close(&mut ws, Some(7)).await;
    assert!(
        reply_seen,
        "the Reply to the request that caused the revocation was dropped — a \
         succession ceremony cannot tell its own client that it committed"
    );
    assert_eq!(
        code,
        Some(4401),
        "the spared connection must still close 4401 once its answer is out — \
         the grace runs through that Reply, not a reprieve"
    );
}

/// The other half of property 4: sparing the caller's **reply** must not spare
/// its **authority**. A connection whose handler revoked it answers nothing it
/// did not already have in flight.
#[tokio::test]
async fn a_spared_connection_dispatches_nothing_further() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    ws.send(request_frame(REVOKE_SELF_KIND, 7)).await.unwrap();
    // Sent from *inside* the grace window, not pipelined behind the first frame:
    // pipelining would race the handler, and a request that merely arrived
    // before the revoke proves nothing about what the grace admits. Here the
    // authority is provably already gone when this leaves the client.
    h.gate.revoked.notified().await;
    let _ = ws.send(echo_frame(8)).await;
    h.gate.may_reply.notify_one();

    let (code, echo_answered) = read_until_close(&mut ws, Some(8)).await;
    assert_eq!(code, Some(4401), "expected the 4401 close");
    assert!(
        !echo_answered,
        "a connection whose caller lost its authority answered a LATER request — \
         the one-frame grace has become a reprieve"
    );
    // ⚠ What this pins is the **`4401` above**, not the guard below it, and that
    // is measured rather than assumed: deleting the inbound
    // `spared_until().is_some()` arm from `routes.rs` leaves this whole file
    // green (2026-09-12). The reason is structural and worth keeping — the
    // spared connection closes the moment its one Reply is out, which beats the
    // inbound loop's read of anything pipelined behind it, so the guard's
    // absence has no wire observable at all. It is belt-and-braces for the
    // window where the peer is slower than that, and the property this test
    // really defends is the one a *reprieve* would break: that the grace ENDS.
    // (`a_revoked_connection_dispatches_nothing_further` above has the same
    // shape and the same limit; it is stated here because it was measured
    // here.)
}

/// The sibling connections of the same actor keep the unsparing teardown: no
/// drain, no grace, 4401 now. A thief holding a second socket is what the
/// succession ceremony exists to evict, so the sparing must reach exactly the
/// connection that asked and no other.
#[tokio::test]
async fn sparing_the_caller_does_not_spare_the_actors_other_sockets() {
    let h = start().await;
    let mut caller = open_authed(&h.url, &h.kp, &h.token).await;
    let mut sibling = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    caller
        .send(request_frame(REVOKE_SELF_KIND, 7))
        .await
        .unwrap();

    // The sibling's fate is settled the moment the handler revokes — it is not
    // waiting on the caller's Reply, and asserting that from inside the window
    // is what proves the unsparing teardown still reaches it promptly rather
    // than merely eventually.
    h.gate.revoked.notified().await;
    let (sibling_code, _) = read_until_close(&mut sibling, None).await;
    h.gate.may_reply.notify_one();
    assert_eq!(
        sibling_code,
        Some(4401),
        "the actor's other live socket must be closed by the same teardown"
    );
    let (caller_code, reply_seen) = read_until_close(&mut caller, Some(7)).await;
    assert!(reply_seen, "the caller's own reply still goes out");
    assert_eq!(caller_code, Some(4401));
}

/// Wire witness for `transport-connection.md` § Connection lifecycle →
/// *Revocation teardown* → *The one-frame grace*: the spared socket's grace
/// runs *through its Reply*, not "one frame" — a Push already queued ahead of
/// that Reply still reaches the client before the 4401.
///
/// Queues the Push from *inside* the `RevokeGate.revoked` window, before
/// `may_reply` is raised, so it is provably enqueued on `RpcConnection::ws_tx`
/// ahead of the Reply — a `Notify` barrier, not a race against the handler's
/// own scheduling. Mutating the outbound task (`routes.rs`'s `closes_after`)
/// to close on the first frame it forwards after the revoke, rather than on
/// the frame `frame_is_reply_for` matches, reddens exactly this test: the
/// close beats the handler back from its `may_reply` wakeup, so the Reply
/// this test waits for is dropped along with the channel that would have
/// carried it. The three property-4 tests above stay green under that same
/// mutation — none of them has anything else queued ahead of its own Reply,
/// so for them the first frame after the revoke already *is* the Reply.
#[tokio::test]
async fn a_push_queued_behind_the_revoke_still_reaches_the_spared_socket_before_its_4401() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    ws.send(request_frame(REVOKE_SELF_KIND, 7)).await.unwrap();

    // Stand inside the window: the authority is gone, the Reply is not out
    // yet. Queue a Push into the same outbound channel the Reply will use,
    // then let the handler proceed — the Push is enqueued strictly first.
    h.gate.revoked.notified().await;
    h.state.ws.notify_push(
        &h.kp.actor_id().0,
        PushEvent::AccountUpdated(AccountUpdatedPayload {
            changes: vec!["handle".into()],
            timestamp: 1_710_000_000,
            extra: BTreeMap::new(),
        }),
    );
    h.gate.may_reply.notify_one();

    let mut push_seen = false;
    let mut reply_seen = false;
    let mut close_code = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => match decode_frame(&bytes) {
                Ok(Frame::Push(_)) => {
                    assert!(
                        !reply_seen,
                        "the Push must reach the client before the Reply it was \
                         queued ahead of"
                    );
                    push_seen = true;
                }
                Ok(Frame::Reply(r)) if r.correlation_id == 7 && r.ok => {
                    assert!(
                        push_seen,
                        "the Reply arrived before the Push that was queued ahead \
                         of it"
                    );
                    reply_seen = true;
                }
                _ => {}
            },
            Ok(Some(Ok(Message::Close(frame)))) => {
                close_code = frame.map(|f| u16::from(f.code));
                break;
            }
            _ => break,
        }
    }

    assert!(
        push_seen,
        "the Push queued ahead of the Reply never arrived"
    );
    assert!(
        reply_seen,
        "the Reply never arrived — a mutant that closes on the first frame \
         after the revoke, rather than on the Reply frame, drops it exactly here"
    );
    assert_eq!(
        close_code,
        Some(4401),
        "the spared connection must still close 4401 once both queued frames \
         are out"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The per-token twin — revoking one SESSION closes that session's sockets
// ═════════════════════════════════════════════════════════════════════════════
//
// Everything above is per-**actor**: the whole identity loses its authority and
// every socket it holds dies. `fauna.sessions.{revoke,revoke_all}` is the
// per-**token** twin (`devices.md` § What a session is, and what revoking one
// does): one bearer of one actor ends, and the actor's *other* sessions must
// carry on untouched. The same "validated once, at the upgrade" fact that made
// the per-actor teardown necessary applies unchanged, which is why the arms
// calling `TokenStore` and stopping governed only the revoked session's NEXT
// connection — the session's open socket kept dispatching as `User`.
//
// So the assertions come in pairs: the revoked session's socket closes 4401 and
// stops dispatching, AND the sibling session's socket stays open and keeps
// dispatching. A teardown that got only the first half right would be
// `disconnect_actor` under another name, and would cut every device the user
// owns off every time they dismissed one unrecognized session.

/// Revoking one session closes exactly that session's socket — and no other.
#[tokio::test]
async fn revoking_one_session_closes_only_that_sessions_socket() {
    let h = start().await;
    let (other_token, other_token_id) = h.mint().await;

    let mut keeper = open_authed(&h.url, &h.kp, &h.token).await;
    let mut doomed = open_authed(&h.url, &h.kp, &other_token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Sanity on the socket that must SURVIVE, so its later replies mean "still
    // authorized" rather than "this kind happened to work once".
    keeper.send(echo_frame(1)).await.unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 1).await,
        "precondition: the keeper session dispatches before the revoke"
    );

    keeper
        .send(revoke_frame(11, &other_token_id))
        .await
        .unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 11).await,
        "the revoke itself must succeed (ownership check: both bearers are this \
         actor's)"
    );

    // Race the close exactly as `a_revoked_connection_dispatches_nothing_further`
    // does: send before the client can have observed the close frame.
    let _ = doomed.send(echo_frame(12)).await;
    let (code, doomed_replied) = read_until_close(&mut doomed, Some(12)).await;
    assert_eq!(
        code,
        Some(4401),
        "the revoked session's live socket must close 4401 — a revoke that only \
         drops the token governs the session's NEXT connection, and the one it \
         already holds keeps dispatching as `User`"
    );
    assert!(
        !doomed_replied,
        "the revoked session answered an RPC issued after its revocation"
    );

    // The other half, and the one a `disconnect_actor` misuse would break: the
    // sibling session is untouched.
    keeper.send(echo_frame(13)).await.unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 13).await,
        "revoking one session killed another session of the same actor — \
         dismissing one unrecognized session must not sign out every device the \
         user owns"
    );
}

/// `revoke_all` closes every socket of the actor **except** the kept session's.
#[tokio::test]
async fn revoke_all_closes_every_socket_but_the_kept_sessions() {
    let h = start().await;
    let (second_token, _second_id) = h.mint().await;
    let (third_token, _third_id) = h.mint().await;

    let mut keeper = open_authed(&h.url, &h.kp, &h.token).await;
    let mut second = open_authed(&h.url, &h.kp, &second_token).await;
    let mut third = open_authed(&h.url, &h.kp, &third_token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    keeper
        .send(revoke_all_frame(21, &h.token_id))
        .await
        .unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 21).await,
        "the caller keeps its own session, so its reply arrives on a socket that \
         is never closed"
    );

    for (label, ws) in [("second", &mut second), ("third", &mut third)] {
        let (code, _) = read_until_close(ws, None).await;
        assert_eq!(
            code,
            Some(4401),
            "the {label} session's live socket must close 4401 on revoke_all"
        );
    }

    keeper.send(echo_frame(22)).await.unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 22).await,
        "`revoke all OTHER sessions` closed the caller's own socket — the one \
         session the control exists to spare"
    );
}

/// The one-frame grace, per-token arm: revoking **your own** session still
/// delivers the answer to the request that did it.
///
/// This is the per-token member of the question `transport-connection.md`
/// § Connection lifecycle → *Revocation teardown* → *The one-frame grace* says
/// every new authority-stripping kind must ask — *is this path's own Reply
/// something the client cannot recover without?* Here it is, for a plainer
/// reason than the succession ceremony's: the caller is being told whether the
/// thing it just asked for happened. Without the grace the socket closes under
/// the handler's own Reply and the caller sees a bare 4401, which it must read
/// as "the bearer died" — indistinguishable from an unrelated eviction, and no
/// evidence at all that the revoke committed.
///
/// ⚠ **Not an unreachable case, though `ui/sessions.md` hides the button.**
/// That page omits `session-revoke-button` on the app's own row, which removes
/// the *deliberate* self-revoke and nothing else. The app recognizes its own
/// rows by the id set it has kept since launch, and `devices.md` § The client's
/// own session states the bound in so many words: **a relaunched app has
/// forgotten its previous run's ids**, so a still-live token of its own paints
/// as an ordinary stranger's row, revoke button and all. A third-party
/// protocol client naming its own id is the other door. The wire contract has
/// to hold on both.
#[tokio::test]
async fn revoking_the_callers_own_session_still_delivers_that_reply() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    ws.send(revoke_frame(31, &h.token_id)).await.unwrap();

    let (code, reply_seen) = read_until_close(&mut ws, Some(31)).await;
    assert!(
        reply_seen,
        "the caller revoked its own session and the Reply was dropped under the \
         close — the app cannot tell the revoke committed from a stray eviction"
    );
    assert_eq!(
        code,
        Some(4401),
        "the caller's socket must still close once its answer is out: the \
         session it was upgraded with no longer exists"
    );
}

/// The same grace on the signed-in **lock** (`fauna.sessions.lockout`), which is
/// self-directed by construction: the actor locks its own account, over one of
/// the very sockets the lock closes. Its Reply (`ok` + `locked_until`) is the
/// app's only evidence the lock committed — `ui/sessions.md` § User actions has
/// the app leave the shell for the locked surface "on success", and a bare 4401
/// is indistinguishable from an unrelated eviction, so an app that never hears
/// the answer paints a failed lock over an account that is in fact locked.
///
/// Red until 2026-10-04: the handler closed every socket of the actor, the
/// caller's included, before its Reply was written. Measured from the app side
/// by `tests/e2e-unified/tests/test_locked_surface.py` — the nest refused the
/// actor as locked while tui still stood in its shell.
#[tokio::test]
async fn locking_the_callers_own_account_still_delivers_that_reply() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    // A second session of the same actor: the lock is per-actor, so this one
    // gets no grace at all.
    let (other_token, _) = h.mint().await;
    let mut other = open_authed(&h.url, &h.kp, &other_token).await;
    await_connections(&h, h.kp.actor_id().0, 2).await;

    ws.send(request_frame_with(
        "fauna.sessions.lockout",
        41,
        to_value(&fauna_protocol::sessions::LockoutRequest {
            extra: Default::default(),
        }),
    ))
    .await
    .unwrap();

    let (code, reply_seen) = read_until_close(&mut ws, Some(41)).await;
    assert!(
        reply_seen,
        "the caller locked its own account and the Reply was dropped under the \
         close — the app cannot tell the lock landed"
    );
    assert_eq!(
        code,
        Some(4401),
        "the caller's socket must still close once its answer is out — the lock \
         spares one Reply, never a session"
    );
    let (other_code, _) = read_until_close(&mut other, None).await;
    assert_eq!(
        other_code,
        Some(4401),
        "the actor's other sockets close at once: the grace is the caller's alone"
    );
    assert!(
        h.state
            .db
            .get_locked_until(&h.kp.actor_id().0)
            .await
            .unwrap()
            .is_some(),
        "and the account is locked"
    );
}

/// The same grace on the `revoke_all` arm, reached through the race the design
/// documents rather than through a contrived id.
///
/// `ui/sessions.md` has *revoke others* read `keep_token_id` from the holder at
/// call time, and `devices.md` § The client's own session accepts the residual
/// race: a renewal landing between that read and the nest applying the revoke
/// leaves `keep_token_id` naming a token the caller no longer holds, and
/// `revoke_all_except_token_id` then revokes everything (a `keep_token_id`
/// matching none of the actor's sessions is not an error — `token_store.rs`).
/// The design calls that harmless *because the next request re-mints*; that is
/// only true if the caller gets an answer at all. Here the caller's own socket
/// is one of the ones being closed, so this is the arm where the grace is
/// load-bearing rather than incidental.
#[tokio::test]
async fn revoke_all_that_keeps_nothing_still_answers_the_caller() {
    let h = start().await;
    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // A well-formed id that is no session of this actor's — what the renewal
    // race leaves in the client's hand.
    ws.send(revoke_all_frame(41, "00112233445566ff"))
        .await
        .unwrap();

    let (code, reply_seen) = read_until_close(&mut ws, Some(41)).await;
    assert!(
        reply_seen,
        "the renewal race revoked the caller's own newest bearer and the \
         `revoked` count never reached it"
    );
    assert_eq!(code, Some(4401), "and the socket still closes");
}

/// A session revocation must not reach **another actor's** socket, even when
/// the two happen to be the only connections on the nest.
///
/// The per-actor teardown is keyed on the actor and cannot make this mistake.
/// The per-token one is keyed on a `token_id` — a 16-hex string with no actor
/// in it — so a teardown that walked every connection looking for the id, or
/// that took the `revoke_all` arm's "everything but the kept id" literally
/// across the whole subscription map, would cross the boundary silently and
/// only under load.
#[tokio::test]
async fn revoking_a_session_never_touches_another_actors_socket() {
    let h = start().await;

    let stranger_kp = ActorKeypair::generate();
    h.state
        .db
        .create_user(&stranger_kp.actor_id().0, "free", "bob")
        .await
        .ok();
    let stranger_token = h
        .state
        .auth
        .token_store
        .insert_with_metadata(stranger_kp.actor_id(), 3600, None, None)
        .await;

    let mut caller = open_authed(&h.url, &h.kp, &h.token).await;
    let mut stranger = open_authed(&h.url, &stranger_kp, &stranger_token.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // `revoke_all` keeping nothing: the widest per-token teardown there is.
    caller
        .send(revoke_all_frame(51, "00112233445566ff"))
        .await
        .unwrap();
    let (code, _) = read_until_close(&mut caller, Some(51)).await;
    assert_eq!(code, Some(4401), "the caller's own socket closes");

    stranger.send(echo_frame(52)).await.unwrap();
    assert!(
        expect_ok_reply(&mut stranger, 52).await,
        "one actor's `revoke all sessions` closed a DIFFERENT actor's socket"
    );
    // And the stranger's bearer is untouched in the store too — the token half
    // has the same boundary as the socket half.
    assert_eq!(
        h.state
            .auth
            .token_store
            .list_sessions(&stranger_kp.actor_id())
            .await
            .len(),
        1,
        "another actor's session row was revoked"
    );
}

/// Revoking a session with no live socket is a no-op, not a panic — the
/// per-token twin of `disconnecting_an_actor_with_no_connections_is_a_noop`.
///
/// The ordinary case, in fact: `sessions.md`'s list is mostly rows the user has
/// no connection for (a relaunched app's forgotten predecessor, a device that
/// is asleep), and dismissing one must simply drop the token.
#[tokio::test]
async fn revoking_a_session_with_no_live_socket_is_a_noop() {
    let h = start().await;
    let (_idle_token, idle_token_id) = h.mint().await;

    let mut ws = open_authed(&h.url, &h.kp, &h.token).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    ws.send(revoke_frame(61, &idle_token_id)).await.unwrap();
    assert!(
        expect_ok_reply(&mut ws, 61).await,
        "revoking a session that never opened a socket must succeed"
    );
    assert_eq!(
        h.state
            .auth
            .token_store
            .list_sessions(&h.kp.actor_id())
            .await
            .len(),
        1,
        "the idle session's token row should be gone, the caller's should remain"
    );

    ws.send(echo_frame(62)).await.unwrap();
    assert!(
        expect_ok_reply(&mut ws, 62).await,
        "the caller's socket must survive revoking an unconnected session"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// Property 6 — removing a device closes the sockets its key minted
// ═════════════════════════════════════════════════════════════════════════════
//
// A device's renewal key mints bearers over `fauna.auth.device_handshake`, and
// each connection upgraded with one carries that key
// (`RpcConnection::bound_device_key`). The three device-removal doors drop the
// key's token rows; these pins hold them to closing the sockets too, while a
// direct sign-in socket of the same actor carries on.

/// Wait until `actor` holds exactly `n` registered connections — the
/// latency-independent form of "the upgrades have landed" (convention 14).
async fn await_connections(h: &Harness, actor: [u8; 32], n: usize) {
    let ws = h.state.ws.clone();
    common::poll_until(&format!("{n} registered connection(s)"), move || {
        ws.connections_for(&actor).len() == n
    })
    .await;
}

/// Invoke one production handler directly — setup only, never a door under
/// test (each door is driven over the wire, or by a caller whose own socket is
/// not what is being observed).
async fn call_handler<Req: serde::Serialize>(
    h: &Harness,
    actor: [u8; 32],
    kind: &str,
    req: &Req,
) -> Bytes {
    let meta = h.state.rpc_router.kind_meta(kind).expect("kind registered");
    (meta.handler)(
        Arc::clone(&h.state),
        actor,
        Bytes::from(encode_canonical(req).unwrap().to_vec()),
    )
    .await
    .unwrap_or_else(|e| panic!("{kind} failed during setup: {e:?}"))
}

/// Register a device row for `account` and attach a root-signed renewal grant,
/// exactly as the production provision does; then mint a bearer as that
/// device's handshake would (the token row tagged with the renewal key).
/// Returns `(device_id hex, renewal key, device-minted bearer)`.
async fn enroll_device(h: &Harness, account: &ActorKeypair) -> (String, [u8; 32], String) {
    let device_id_hex = hex::encode(ActorKeypair::generate().actor_id().0);
    call_handler(
        h,
        account.actor_id().0,
        "fauna.sync.register",
        &fauna_protocol::sync::SyncRegisterRequest {
            device_id: device_id_hex.clone(),
            label: "enrolled device".to_string(),
            ..Default::default()
        },
    )
    .await;
    let (grant, seed) = common::fresh_device_grant(account);
    call_handler(
        h,
        account.actor_id().0,
        "fauna.sync.device_grant.register",
        &fauna_protocol::sync::DeviceGrantRegisterRequest {
            device_id: device_id_hex.clone(),
            authorization: grant,
            extra: Default::default(),
        },
    )
    .await;
    let device_key = ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    let minted = h
        .state
        .auth
        .token_store
        .insert_with_metadata(account.actor_id(), 3600, None, Some(device_key))
        .await;
    (device_id_hex, device_key, minted.token)
}

fn delete_device_frame(corr: u64, device_id_hex: &str) -> Message {
    request_frame_with(
        "fauna.sync.devices.delete",
        corr,
        to_value(&fauna_protocol::sync::SyncDeviceDeleteRequest {
            device_id: device_id_hex.to_string(),
            extra: Default::default(),
        }),
    )
}

/// `fauna.sync.device_grant.revoke` on the session-authorized arm (no proof of
/// possession) — what an account session, or the device itself, presents.
fn revoke_grant_frame(corr: u64, device_key: &[u8; 32]) -> Message {
    request_frame_with(
        "fauna.sync.device_grant.revoke",
        corr,
        to_value(&fauna_protocol::sync::DeviceGrantRevokeRequest {
            device_key: hex::encode(device_key),
            timestamp_ms: None,
            nonce: None,
            signature: None,
            extra: Default::default(),
        }),
    )
}

/// The shared body of the two account-driven doors: a direct sign-in socket
/// (`keeper`) removes the device, and the device-minted socket must die while
/// the keeper lives.
async fn assert_removal_closes_only_the_device_socket(
    door: &str,
    frame: impl FnOnce(&str, &[u8; 32]) -> Message,
) {
    let h = start().await;
    let actor = h.kp.actor_id().0;
    let (device_id_hex, device_key, device_token) = enroll_device(&h, &h.kp).await;

    let mut keeper = open_authed(&h.url, &h.kp, &h.token).await;
    let mut doomed = open_authed(&h.url, &h.kp, &device_token).await;
    await_connections(&h, actor, 2).await;

    keeper
        .send(frame(&device_id_hex, &device_key))
        .await
        .unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 71).await,
        "precondition: {door} itself succeeds"
    );

    let _ = doomed.send(echo_frame(72)).await;
    let (code, doomed_replied) = read_until_close(&mut doomed, Some(72)).await;
    assert_eq!(
        code,
        Some(4401),
        "{door}: the socket the removed device's key minted must close 4401 — \
         dropping the token rows governs only its NEXT connection, and the one it \
         already holds keeps dispatching as `User`"
    );
    assert!(
        !doomed_replied,
        "{door}: the removed device's socket answered an RPC issued after the removal"
    );

    keeper.send(echo_frame(73)).await.unwrap();
    assert!(
        expect_ok_reply(&mut keeper, 73).await,
        "{door} closed a socket the device did not mint — removing one device is \
         not signing the account out"
    );
}

#[tokio::test]
async fn deleting_a_device_closes_only_the_sockets_its_key_minted() {
    assert_removal_closes_only_the_device_socket("fauna.sync.devices.delete", |id, _| {
        delete_device_frame(71, id)
    })
    .await;
}

#[tokio::test]
async fn revoking_a_device_grant_closes_only_the_sockets_its_key_minted() {
    assert_removal_closes_only_the_device_socket("fauna.sync.device_grant.revoke", |_, key| {
        revoke_grant_frame(71, key)
    })
    .await;
}

/// The one-frame grace, device arm: a device retiring **its own** grant over
/// its own device-minted session still gets the Reply.
///
/// This is the question `transport-connection.md` § *The one-frame grace* has
/// every new authority-stripping path answer, and the answer is yes for the
/// same reason as `fauna.sessions.revoke`: the arm is session-authorized, so
/// the device's own bearer is a legitimate presenter — the ordinary way an
/// agent retires itself when it holds no proof-of-possession signature to
/// hand — and `{revoked, sessions_revoked}` is its only evidence the
/// retirement committed. Dropped under the close, the device reads a bare 4401
/// as "my bearer expired", re-handshakes with the key it just retired, and is
/// refused with no way to tell success from failure.
#[tokio::test]
async fn a_device_retiring_its_own_grant_still_gets_the_reply() {
    let h = start().await;
    let (_device_id_hex, device_key, device_token) = enroll_device(&h, &h.kp).await;
    let mut ws = open_authed(&h.url, &h.kp, &device_token).await;
    await_connections(&h, h.kp.actor_id().0, 1).await;

    ws.send(revoke_grant_frame(81, &device_key)).await.unwrap();
    let (code, reply_seen) = read_until_close(&mut ws, Some(81)).await;
    assert!(
        reply_seen,
        "the device retired its own grant and the Reply was dropped under the close"
    );
    assert_eq!(
        code,
        Some(4401),
        "and its socket still closes once the answer is out — the bearer it was \
         upgraded with no longer exists"
    );
}

/// The same grace on `devices.delete`: a device removing its own row over its
/// own session (a user tidying up from the device they are about to wipe).
#[tokio::test]
async fn a_device_deleting_its_own_row_still_gets_the_reply() {
    let h = start().await;
    let (device_id_hex, _device_key, device_token) = enroll_device(&h, &h.kp).await;
    let mut ws = open_authed(&h.url, &h.kp, &device_token).await;
    await_connections(&h, h.kp.actor_id().0, 1).await;

    ws.send(delete_device_frame(82, &device_id_hex))
        .await
        .unwrap();
    let (code, reply_seen) = read_until_close(&mut ws, Some(82)).await;
    assert!(
        reply_seen,
        "the device deleted its own row and the Reply was dropped under the close"
    );
    assert_eq!(code, Some(4401), "and its socket still closes");
}

/// Graduation severs a marked guardian device (`family-safety.md`
/// § Graduation). That device authenticates **as the ward**, so its minted
/// sessions are the ward's — and graduation must close their sockets exactly
/// as the ward's own `devices.delete` would, while the ward's own sign-in
/// socket carries on. The guardian drives the kind; their connection is not
/// the one observed, so the handler is invoked directly as them.
#[tokio::test]
async fn graduation_closes_only_the_marked_guardian_devices_sockets() {
    let h = start().await;

    let guardian = ActorKeypair::generate();
    h.state
        .db
        .create_user(&guardian.actor_id().0, "free", "parent")
        .await
        .unwrap();
    h.state
        .db
        .create_invite_code_with_guardian(
            "WARD-CODE",
            "free",
            1,
            Some(&guardian.actor_id().0[..]),
            None,
        )
        .await
        .unwrap();
    let ward = ActorKeypair::generate();
    let meta = h
        .state
        .rpc_router
        .kind_meta("fauna.account.register")
        .unwrap();
    (meta.handler)(
        Arc::clone(&h.state),
        [0u8; 32],
        common::register_payload(&ward, "kid", DOMAIN, Some("WARD-CODE"), None),
    )
    .await
    .expect("the ward is admitted under the guardian");
    let ward_id = ward.actor_id().0;

    let (device_id_hex, _key, device_token) = enroll_device(&h, &ward).await;
    assert!(
        h.state
            .db
            .set_device_guardian_mark(&ward_id, &hex::decode(&device_id_hex).unwrap(), true)
            .await
            .unwrap(),
        "precondition: the guardian device is marked"
    );
    let ward_session = h
        .state
        .auth
        .token_store
        .insert_with_metadata(ward.actor_id(), 3600, None, None)
        .await;

    let mut ward_own = open_authed(&h.url, &ward, &ward_session.token).await;
    let mut guardian_device = open_authed(&h.url, &ward, &device_token).await;
    await_connections(&h, ward_id, 2).await;

    call_handler(
        &h,
        guardian.actor_id().0,
        "fauna.family.graduate",
        &fauna_protocol::family::FamilyGraduateRequest {
            supervised_actor_id: fauna_protocol::ByteBuf::from(ward_id.to_vec()),
            extra: Default::default(),
        },
    )
    .await;

    let _ = guardian_device.send(echo_frame(91)).await;
    let (code, replied) = read_until_close(&mut guardian_device, Some(91)).await;
    assert_eq!(
        code,
        Some(4401),
        "graduation severed the guardian device's sessions but left its live \
         socket dispatching as the ward"
    );
    assert!(
        !replied,
        "the severed guardian device answered after graduation"
    );

    ward_own.send(echo_frame(92)).await.unwrap();
    assert!(
        expect_ok_reply(&mut ward_own, 92).await,
        "graduation closed the ward's own sign-in socket — only the guardian \
         device's sessions end"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// The census — the class guard behind the three wire properties above
// ═════════════════════════════════════════════════════════════════════════════

/// Per line, `true` when it is **not** inside a `#[cfg(test)]` item.
///
/// Tracks brace depth from the `{` that opens the annotated item, so
/// production code *after* a test module is still walked. A brace-less
/// annotated statement (a test-only rendezvous call inside a production fn)
/// ends at its `;` — walking on to a brace would meet the enclosing fn's
/// close first and trip the give-up for the whole file. Gives up — every
/// line reported as production — if the depth ever goes negative or fails to
/// close by EOF, because a census that loses its place must over-guard rather
/// than fall silent. Braces inside string literals are the known imprecision;
/// they are balanced in practice, and an unbalanced one trips the give-up.
fn production_mask(lines: &[&str]) -> Vec<bool> {
    let mut mask = vec![true; lines.len()];
    let mut i = 0usize;
    while i < lines.len() {
        if !lines[i].trim_start().starts_with("#[cfg(test)]") {
            i += 1;
            continue;
        }
        // Walk to the item's opening brace, then to its matching close — or,
        // for a brace-less test-only statement (a `#[cfg(test)]` rendezvous
        // call such as `revoke_race::after_sweep(..).await;`), to its `;`.
        let mut depth = 0i32;
        let mut opened = false;
        let mut statement = false;
        let mut j = i;
        while j < lines.len() {
            if !opened && j > i && lines[j].trim_end().ends_with(';') && !lines[j].contains('{') {
                mask[j] = false;
                j += 1;
                statement = true;
                break;
            }
            for ch in lines[j].chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if depth < 0 {
                return vec![true; lines.len()]; // lost the place — over-guard
            }
            mask[j] = false;
            j += 1;
            if opened && depth == 0 {
                break;
            }
        }
        if !statement && (!opened || depth != 0) {
            return vec![true; lines.len()]; // never closed — over-guard
        }
        i = j;
    }
    mask
}

/// The mask's own shapes: a brace-less `#[cfg(test)]` statement inside a
/// production fn is masked alone, and neither it nor a test module after the
/// fn costs the production lines around them their census.
#[test]
fn production_mask_keeps_its_place_past_a_test_only_statement() {
    let src = "\
fn helper() {
    delete();
    #[cfg(test)]
    park(
        id,
    ).await;
    sweep();
}

#[cfg(test)]
mod tests {
    fn t() { sweep(); }
}

fn after() {}";
    let lines: Vec<&str> = src.lines().collect();
    let production: Vec<&str> = production_mask(&lines)
        .iter()
        .zip(&lines)
        .filter(|(p, _)| **p)
        .map(|(_, l)| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        production,
        [
            "fn helper() {",
            "delete();",
            "sweep();",
            "}",
            "fn after() {}"
        ],
        "the mask lost its place — a brace-less test-only statement must end at its `;`"
    );
}

/// Every `TokenStore` revocation call site closes the sockets too — the
/// per-actor method and both per-token ones.
///
/// The three tests above prove the *mechanism* works. They cannot see the
/// failure that actually shipped, which is not a broken mechanism but an
/// **un-adopted** one: a path that strips an actor's authority, calls
/// `revoke_actor`, and never closes the connections the actor already holds.
/// That is invisible to every wire test, because the path under test simply is
/// not the path that forgot.
///
/// It shipped exactly once and it was the worst possible member: the
/// identity-succession ceremony (`recovery_handlers.rs`) — the one path whose
/// entire purpose is evicting a thief who holds the seed — did the token half
/// alone from the day it landed until 2026-08-23, so the thief kept full
/// `User`-class dispatch on an open socket after the ceremony that was supposed
/// to undo them. Six of the seven sites were correct; the guard is here so the
/// eighth cannot be wrong.
///
/// **The per-token methods are in the walk for the same reason** (added
/// 2026-09-20 with the per-token teardown itself). `fauna.sessions.
/// {revoke,revoke_all}` shipped as the exact failure this census describes —
/// an authority-stripping path that called `TokenStore` and stopped — and it
/// was invisible to every wire test in this file for exactly the stated
/// reason: the path under test was not the path that forgot. Each watched
/// method has its own paired teardown, because pairing `revoke_by_token_id`
/// with `disconnect_actor` would be worse than not pairing it at all: it would
/// close every session the user owns each time they dismissed one.
///
/// **`revoke_minted_by` joined the walk 2026-09-23** (property 6), having
/// shipped as the same failure a third time: all three device-removal doors
/// dropped the device's token rows and left its sockets dispatching. It was
/// outside the walk only because nobody had added it — which is the argument
/// for adding every `TokenStore` revocation method the day it is written.
///
/// `TokenStore::drop_unissued_token` is deliberately NOT in the walk, and its
/// absence is structural rather than an exemption: it names the mint's own
/// failure path, where the token never left the nest, so there is no socket
/// for it to close and no name for it to be confused with. Had those two
/// `auth_core` sites stayed on `revoke_by_token_id`, the only ways to keep
/// this census green would have been a no-op teardown call written to satisfy
/// a guard, or a by-path exemption — and an exemption list is text near a
/// thing, which is the failure mode one paragraph down.
///
/// **What this walk can and cannot see** — stated because a census that
/// overstates its reach is worse than none (`transport.md` § Revocation
/// teardown). It sees: a revocation call with no matching teardown in the
/// same neighbourhood. It does NOT see: a path that strips authority without
/// calling one of these methods at all (nothing textual can find that), or a
/// pairing that is present but on the wrong actor or token id. Those stay the
/// reviewer's job. The neighbourhood is deliberately generous — the widest
/// real separation among the correct sites is 18 lines (`eviction.rs`,
/// notify-then-close).
#[test]
fn every_token_revocation_site_also_closes_the_live_sockets() {
    const WINDOW: usize = 40;

    /// `(revocation method, what pairs with it, how many sites to expect)`.
    /// The floor catches a rename silently turning the guard off, which for a
    /// security census is the one failure worse than a red.
    const WATCHED: &[(&str, &[&str], usize)] = &[
        // `close_actor_sockets`: the one name every actor-wide socket close
        // goes through.
        (
            ".revoke_actor(",
            &["close_actor_sockets", "revoke_actor_authority"],
            5,
        ),
        (
            ".revoke_by_token_id(",
            &["disconnect_token_id", "revoke_session_authority"],
            1,
        ),
        (
            ".revoke_all_except_token_id(",
            &[
                "disconnect_actor_except_token_id",
                "revoke_other_sessions_authority",
            ],
            1,
        ),
        // Device removal (property 6): the per-device twin. Its sites are the
        // three doors, and the helper that does both halves is the only
        // sanctioned pairing — `disconnect_device_key` is listed for a site
        // that needs its own interleaving, never `disconnect_actor`, which
        // would sign the whole account out for one lost phone.
        (
            ".revoke_minted_by(",
            &["disconnect_device_key", "revoke_device_authority"],
            1,
        ),
        // The principal twin (`transport-connection.md` § *The principal
        // session* → *Revocation — three doors*, door (a)): deleting a
        // principal row stops its sessions' next call, and the sweep closes
        // the sockets themselves. Not a `TokenStore` method — a principal holds
        // no nest bearer — but the same un-adopted-mechanism risk, so it is in
        // the walk from the day its sessions exist.
        (
            ".revoke_third_party_principal(",
            &["disconnect_principal"],
            1,
        ),
    ];

    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut stack = vec![src];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    assert!(
        files.len() > 20,
        "the walk found only {} source files — it is not reaching the tree, and a \
         census that reaches nothing passes vacuously",
        files.len()
    );

    // ⚠ CODE ONLY, never comments — and this is not a stylistic preference, it
    // is the finding one layer up. The first draft of this guard scanned raw
    // lines, so the prose in the succession fix's own comment ("`revoke_actor_
    // authority` does both halves…") satisfied the pairing check and the census
    // stayed GREEN under the very mutation it exists to catch. That is exactly
    // this class: *a guard that reads unstructured text NEAR a thing
    // is a guard on the text, not on the thing* — and a security census
    // defeated by a comment is worse than no census, because it certifies.
    let is_code = |l: &str| {
        let t = l.trim_start();
        !(t.starts_with("//") || t.starts_with("*") || t.starts_with("/*"))
    };

    // ⚠ **Production paths only.** A `#[cfg(test)]` unit test of `TokenStore`
    // revokes a token with no `WsState` anywhere in scope, and pairing it with
    // a teardown would mean building a connection registry inside a store test
    // purely to satisfy this walk. It is not a revocation *path*: nothing
    // reaches it but the test binary. The per-actor arm never noticed because
    // no `#[cfg(test)]` block happens to call `revoke_actor`; the per-token
    // arm hits one immediately, which is the useful kind of first red.
    //
    // Brace-tracked rather than "everything after the first `#[cfg(test)]`",
    // which would silently stop guarding any production code that follows a
    // test module — for a security census, silently guarding less is the one
    // failure worse than a red. If the tracking ever loses its place (an
    // unbalanced brace inside a literal), [`production_mask`] gives up and
    // returns the whole file as production: a mis-parse must over-guard.
    let sources: Vec<(&std::path::PathBuf, String)> = files
        .iter()
        .map(|p| (p, std::fs::read_to_string(p).expect("read source")))
        .collect();

    for (method, pairs_with, floor) in WATCHED {
        let mut sites = 0usize;
        let mut unpaired = Vec::new();
        for (path, text) in &sources {
            let lines: Vec<&str> = text.lines().collect();
            let production = production_mask(&lines);
            for (i, line) in lines.iter().enumerate() {
                // The `TokenStore` method itself, not the shared `AppState`
                // helper that does both halves by construction, and not its
                // own `pub async fn` definition.
                if !line.contains(method) || !is_code(line) || !production[i] {
                    continue;
                }
                sites += 1;
                let lo = i.saturating_sub(WINDOW);
                let hi = (i + WINDOW).min(lines.len() - 1);
                let paired = lines[lo..=hi]
                    .iter()
                    .filter(|l| is_code(l))
                    .any(|l| pairs_with.iter().any(|p| l.contains(p)));
                if !paired {
                    unpaired.push(format!(
                        "{}:{}",
                        path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                            .unwrap_or(path)
                            .display(),
                        i + 1
                    ));
                }
            }
        }

        assert!(
            sites >= *floor,
            "the census found only {sites} `{method}` call site(s), expected at least \
             {floor} — the method was probably renamed, which would make this guard \
             silently stop guarding"
        );
        assert!(
            unpaired.is_empty(),
            "these `{method}` call sites do not close the sockets the revoked \
             authority already holds, so they govern only its NEXT connection \
             (`transport-connection.md` § Connection lifecycle → *Revocation \
             teardown*): {unpaired:?}\n\
             Fix: call the `AppState` helper that does both halves ({}), or pair \
             the call with the matching `WsState` teardown inline where the site \
             needs a specific interleaving.",
            pairs_with.join(" / ")
        );
    }
}
