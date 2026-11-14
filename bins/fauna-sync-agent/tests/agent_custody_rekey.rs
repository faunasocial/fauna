//! **The agent resolves its own content keys — a rotation made where no app of
//! this machine runs re-keys it, and it never seals under the generation the
//! rotation left behind.**
//!
//! `on-demand-files-shared-sets.md` § Shared sets on a capability host → *One mechanism —
//! the agent converges*: the desktop sync agent reads every set's content keys
//! from its holder's folder-key custody itself — the account plane's
//! `fauna.state.folder-keys`, through the account store it mounts as the
//! machine's enrolled principal; no app pushes a key — and re-reads at its own
//! edges: every reconcile, the `state-fleet` nudge the nest fans out on every
//! custody write, the resident engine's row read before a seal, and a backstop.
//! Between a rotation and the re-key, a write is **held** — never sealed under
//! the older generation, never acknowledged — and *Keys pending* says so.
//!
//! ## What is real here
//!
//! One in-process `fauna-nest` with a real byte plane (`common::start_nest`),
//! the REAL `fauna-sync-agent` binary driven over its REAL per-user socket (the
//! `same_nest_push_nudge` shape), and the owner's REAL rotation —
//! `FoldersAuthor::share_set` then `remove_member` over a real MLS engine,
//! which re-keys custody and advances the set's content-key floor on the nest.
//! Every app here is a real account runtime on a machine of its own
//! (`common::AppSeat`), writing custody through the plane's door. No test step
//! hands the agent a key: every capability here carries only the account's own
//! `BackupKey` and a bearer.
//!
//! ## The two proofs
//!
//! * **Owner** (`an_owner_rotation_from_another_seat_re_keys_the_agent_and_never_seals_stale`):
//!   the agent serves the owner's own bound set; the owner removes a member from
//!   another seat — no app on this machine does anything — and a file written
//!   after the removal is recorded under the NEW generation, never the old one.
//!   The nest's own floor exempts an owner's records, so only the agent's own
//!   re-key and pre-seal hold stand between that write and the removed member.
//! * **Member** (`a_member_agent_holds_its_writes_behind_the_floor_until_custody_arrives`):
//!   the agent serves a writer member's binding; the owner rotates; the member's
//!   custody does not yet hold the new generation (the member's app has not run).
//!   A write is HELD — absent from the change log for a quiet window — and the
//!   agent reports *Keys pending*. The generation then reaches the member's
//!   custody by a plane write that fires NO nudge (the best-effort push lost),
//!   and the member's next write alone — the pre-seal row read, which finds the
//!   floor still ahead and has the mounted store walk — re-keys the agent: both
//!   writes land under the new generation, and *Keys pending* clears.
//! * **Cut off** (`an_agent_cut_off_from_its_control_plane_publishes_nothing_under_the_generation_it_missed`,
//!   decision 2′): the owner's agent loses its CONTROL plane — its byte plane
//!   still reaches the nest, through a loopback proxy that drops only the
//!   WebSocket — and the owner removes a member from another seat. A file the
//!   agent authors then is sealed under the generation it last read and served
//!   to peers, but nothing sealed under that generation reaches the nest, not
//!   while cut off and not after: once the control plane is back, the first
//!   row read finds the floor ahead, the agent re-keys, and the file is
//!   recorded under the new generation.
//!
//! ## Run
//!
//! ```text
//! cargo test -p fauna-sync-agent --features tier3-nest --test agent_custody_rekey
//! ```

#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::sets::{
    Actor, OwnerSeat, ingest_custody, register_agent_device, store_custody_without_a_nudge,
};
use common::{
    AgentWorld, AppSeat, await_agent, await_serving_engine, connected_client, expect_ok, request,
};
use fauna_ipc::sync::{
    BearerToken, RequestMethod, ResponsePayload, ResponseResult, ServiceStatusInfo, SyncCapability,
};
use fauna_nest::routes::AppState;

const SET_NAME: &str = "team-docs";
const AGENT_DEVICE: [u8; 32] = [0x0b; 32];

/// A causal window: the chain it bounds (watcher flush → seal → upload →
/// record; or edge → re-resolve → rebuild → catch-up upload) is seconds when
/// healthy, and far below the 300 s rescan tick and the agent's 300 s
/// content-key backstop, so arrival inside it is attributable to the edge
/// under test.
const EDGE_WINDOW: Duration = Duration::from_secs(90);
/// The held-write control: a write that is NOT held would be recorded within a
/// few seconds (the watcher flush), so absence across this window is the hold.
const HOLD_QUIET: Duration = Duration::from_secs(15);

// ─────────────────────────────────────────────────────────────────────────
// The agent
// ─────────────────────────────────────────────────────────────────────────

struct Agent {
    _world: AgentWorld,
    _process: common::KillOnDrop,
    socket: PathBuf,
    location: PathBuf,
}

impl Agent {
    /// Enrol this machine for `actor`, spawn the real agent, provision it with
    /// ONLY the account's own `BackupKey` and a bearer — no content key — and
    /// bind a folder to the set. `app` is the actor's app on another machine:
    /// its next pass keys this machine's principal for the account's custody.
    async fn serving(
        state: &AppState,
        base: &str,
        actor: &Actor,
        app: &AppSeat,
        folder_id: i64,
    ) -> Self {
        let world = AgentWorld::new("fauna-custody-rekey-");
        let socket = world.socket_path();
        let location = world.root().join("bound");
        std::fs::create_dir_all(&location).unwrap();
        // The actor's app enrols this machine first, so the agent loads a
        // `SyncWrite` writer key as its change signer — the nest refuses an
        // unsigned record. The app is gone before the agent starts.
        common::enroll_as_w5_app(
            actor.secret,
            &world,
            base,
            &hex::encode(actor.id.0),
            None,
            hex::encode(AGENT_DEVICE),
        )
        .await;
        app.sync().await;
        let process = world.spawn_agent_trusting(&[base]);
        await_agent(&socket);

        let token = state.auth.token_store.insert(actor.id, 3600).await;
        let cap = SyncCapability::new(
            actor.backup_key().to_bytes().to_vec(),
            actor.id.0.to_vec(),
            base.to_string(),
            hex::encode(AGENT_DEVICE),
            BearerToken::new(token, 4_000_000_000),
        );
        expect_ok(&socket, RequestMethod::ProvisionCapability(cap));
        expect_ok(
            &socket,
            RequestMethod::AddLocation {
                path: location.display().to_string(),
            },
        );
        expect_ok(
            &socket,
            RequestMethod::SetLocationFolder {
                path: location.display().to_string(),
                folder: SET_NAME.to_string(),
                folder_id: fauna_core::folder_keys::FolderRef::Local(folder_id).to_wire(),
            },
        );
        let agent = Self {
            _world: world,
            _process: process,
            socket,
            location,
        };
        agent.await_serving();
        agent
    }

    fn await_serving(&self) {
        await_serving_engine(&self.socket, SET_NAME, EDGE_WINDOW);
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        std::fs::write(self.location.join(rel), bytes).unwrap();
    }

    fn keys_pending(&self) -> bool {
        service_status(&self.socket).keys_pending
    }

    /// Poll until *Keys pending* reads `want`.
    fn await_keys_pending(&self, want: bool, what: &str) {
        let deadline = Instant::now() + EDGE_WINDOW;
        while self.keys_pending() != want {
            assert!(
                Instant::now() < deadline,
                "{what}: *Keys pending* never read {want} within {EDGE_WINDOW:?}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// A loopback TCP proxy in front of a nest that can cut a client's **control
/// plane** (its WebSocket) while its **byte plane** (plain HTTP requests) keeps
/// flowing — the two are separate connections, which is why decision 2′'s
/// publication hold is a gate and not a side effect of being offline.
struct ControlPlaneCut {
    url: String,
    cut: Arc<std::sync::atomic::AtomicBool>,
    sockets: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
}

impl ControlPlaneCut {
    async fn start(nest_base: &str) -> Self {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let upstream = nest_base.trim_start_matches("http://").to_string();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let cut = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sockets = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (cut_c, sockets_c) = (Arc::clone(&cut), Arc::clone(&sockets));
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                let (cut, upstream) = (Arc::clone(&cut_c), upstream.clone());
                let (ws_tx, ws_rx) = tokio::sync::oneshot::channel::<()>();
                let task = tokio::spawn(async move {
                    // The request head says which plane this connection is.
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65_536 {
                        match client.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let is_ws = String::from_utf8_lossy(&head)
                        .to_ascii_lowercase()
                        .contains("upgrade: websocket");
                    if is_ws {
                        if cut.load(Ordering::SeqCst) {
                            return;
                        }
                        let _ = ws_tx.send(());
                    }
                    let Ok(mut server) = tokio::net::TcpStream::connect(&upstream).await else {
                        return;
                    };
                    if server.write_all(&head).await.is_err() {
                        return;
                    }
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                });
                let abort = task.abort_handle();
                let sockets = Arc::clone(&sockets_c);
                tokio::spawn(async move {
                    if ws_rx.await.is_ok() {
                        sockets.lock().unwrap().push(abort);
                    }
                });
            }
        });
        Self { url, cut, sockets }
    }

    /// Drop every live control-plane connection and refuse new ones.
    fn cut(&self) {
        self.cut.store(true, std::sync::atomic::Ordering::SeqCst);
        for socket in self.sockets.lock().unwrap().drain(..) {
            socket.abort();
        }
    }

    fn restore(&self) {
        self.cut.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Whether the nest's byte plane holds the manifest `hash` (read with `token`).
async fn manifest_on_nest(base: &str, token: &str, hash: fauna_core::data::ContentHash) -> bool {
    let status = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/manifests/{}",
            hex::encode(hash.digest())
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status();
    match status.as_u16() {
        200 => true,
        404 => false,
        other => panic!("the manifest probe answered {other}, neither found nor absent"),
    }
}

fn service_status(socket: &Path) -> ServiceStatusInfo {
    match request(socket, RequestMethod::GetServiceStatus) {
        ResponseResult::Ok(ResponsePayload::ServiceStatus(info)) => info,
        other => panic!("unexpected GetServiceStatus answer: {other:?}"),
    }
}

fn payload(seed: u32) -> Vec<u8> {
    (0..32_000u32)
        .map(|i| (i.wrapping_mul(seed) % 251) as u8)
        .collect()
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_test_writer()
        .try_init();
}

// ─────────────────────────────────────────────────────────────────────────
// The proofs
// ─────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_rotation_from_another_seat_re_keys_the_agent_and_never_seals_stale() {
    init_tracing();
    let (base, state) = common::start_nest().await;
    let alice = Actor::create(&state, "alice").await;
    let bob = Actor::create(&state, "bob").await;
    let bob_engine = fauna_mls::engine::MlsEngine::new_in_memory(bob.keypair()).unwrap();
    bob.publish_key_package(&state, &bob_engine).await;

    // Alice's other seat creates the set and binds it to a group by sharing it.
    let seat = OwnerSeat::open(&state, &base, &alice, SET_NAME).await;
    let folder_id = seat.create_set().await;
    let channel = seat.share(&bob, "reader").await;
    assert_eq!(seat.keys(channel).await.current_version(), 1);
    register_agent_device(&seat.nest, AGENT_DEVICE).await;

    // Alice's agent, handed nothing but her BackupKey and a bearer.
    let agent = Agent::serving(&state, &base, &alice, &seat.app, folder_id).await;

    // Baseline: the agent resolved generation 1 from Alice's custody itself.
    agent.write("before.bin", &payload(7));
    assert_eq!(
        seat.await_recorded("before.bin", "the pre-rotation write")
            .await,
        Some(1),
        "the agent keyed the bound set from custody (generation 1) — nobody pushed it"
    );

    // The rotation, from the other seat. Nothing on this machine is told.
    seat.remove(channel, &bob).await;
    assert_eq!(
        seat.keys(channel).await.current_version(),
        2,
        "vacuity guard: the removal rotated the set's content key"
    );

    // A write straight after the rotation. The nest exempts an owner's records
    // from its floor, so only the agent's own edges — the `state-fleet` nudge the
    // rotation's custody write fanned out, and the pre-seal row read — keep it
    // off generation 1.
    agent.write("after.bin", &payload(11));
    assert_eq!(
        seat.await_recorded("after.bin", "the post-rotation write")
            .await,
        Some(2),
        "a write made after the owner's rotation was sealed under the generation the \
         removed member still holds — the agent neither re-keyed on the `state-fleet` \
         nudge nor held the seal behind the floor"
    );
    assert!(
        !agent.keys_pending(),
        "custody holds the owner's new generation, so nothing is pending"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_agent_holds_its_writes_behind_the_floor_until_custody_arrives() {
    init_tracing();
    let (base, state) = common::start_nest().await;
    let alice = Actor::create(&state, "alice").await;
    let bob = Actor::create(&state, "bob").await;
    let carol = Actor::create(&state, "carol").await;
    let bob_engine = fauna_mls::engine::MlsEngine::new_in_memory(bob.keypair()).unwrap();
    let carol_engine = fauna_mls::engine::MlsEngine::new_in_memory(carol.keypair()).unwrap();
    bob.publish_key_package(&state, &bob_engine).await;
    carol.publish_key_package(&state, &carol_engine).await;

    // Alice creates the set and shares it with Bob (writer) and Carol; Bob's
    // app ingests generation 1.
    let seat = OwnerSeat::open(&state, &base, &alice, SET_NAME).await;
    let folder_id = seat.create_set().await;
    let channel = seat.share(&bob, "writer").await;
    seat.share(&carol, "reader").await;
    let bob_nest = connected_client(&base, bob.keypair()).await;
    let bob_app = AppSeat::sign_in(&state, &base, bob.secret).await;
    ingest_custody(&bob_app, channel, seat.envelope(channel).await).await;
    register_agent_device(&bob_nest, AGENT_DEVICE).await;

    // Bob's agent, handed nothing but his BackupKey and a bearer.
    let agent = Agent::serving(&state, &base, &bob, &bob_app, folder_id).await;
    agent.write("before.bin", &payload(3));
    assert_eq!(
        seat.await_recorded("before.bin", "the member's pre-rotation write")
            .await,
        Some(1),
        "the member's agent keyed the shared set from its own custody"
    );
    assert!(!agent.keys_pending());

    // Alice removes Carol: generation 2, floor 2. Bob's app has not run, so
    // his custody still holds generation 1 — and no nudge reaches his agent
    // (the rotation wrote Alice's custody, not his).
    seat.remove(channel, &carol).await;

    // A write now is HELD: the pre-seal row read finds the floor ahead of the
    // generation the engine holds, so nothing is sealed or recorded.
    agent.write("held.bin", &payload(5));
    agent.await_keys_pending(true, "the member's write behind the floor");
    let quiet_until = Instant::now() + HOLD_QUIET;
    while Instant::now() < quiet_until {
        assert_eq!(
            seat.recorded_version("held.bin").await,
            None,
            "a write behind the content-key floor was recorded — the agent sealed it \
             under generation 1, the generation the removed member still holds"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // The generation reaches Bob's custody by a plane write that fires NO nudge.
    // Bob's next write alone — its pre-seal row read, still behind the floor —
    // wakes the agent's re-resolve, which has the mounted store walk and now
    // finds the generation.
    store_custody_without_a_nudge(&bob_app, channel, seat.envelope(channel).await).await;
    agent.write("next.bin", &payload(9));
    assert_eq!(
        seat.await_recorded("held.bin", "the held write, once custody caught up")
            .await,
        Some(2),
        "the held write must land under the new generation once custody holds it"
    );
    assert_eq!(
        seat.await_recorded("next.bin", "the write that re-keyed the agent")
            .await,
        Some(2)
    );
    agent.await_keys_pending(false, "custody caught up with the floor");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_cut_off_from_its_control_plane_publishes_nothing_under_the_generation_it_missed()
{
    init_tracing();
    let (base, state) = common::start_nest().await;
    let proxy = ControlPlaneCut::start(&base).await;
    let alice = Actor::create(&state, "alice").await;
    let bob = Actor::create(&state, "bob").await;
    let bob_engine = fauna_mls::engine::MlsEngine::new_in_memory(bob.keypair()).unwrap();
    bob.publish_key_package(&state, &bob_engine).await;

    let seat = OwnerSeat::open(&state, &base, &alice, SET_NAME).await;
    let folder_id = seat.create_set().await;
    let channel = seat.share(&bob, "reader").await;
    register_agent_device(&seat.nest, AGENT_DEVICE).await;

    // Alice's agent reaches the nest only through the proxy.
    let agent = Agent::serving(&state, &proxy.url, &alice, &seat.app, folder_id).await;
    agent.write("before.bin", &payload(7));
    assert_eq!(
        seat.await_recorded("before.bin", "the write before the cut")
            .await,
        Some(1)
    );

    // The agent's control plane goes; its byte plane stays. Then the rotation,
    // from the other seat: no nudge and no row read reaches the agent.
    proxy.cut();
    seat.remove(channel, &bob).await;
    let keys = seat.keys(channel).await;
    assert_eq!(
        keys.current_version(),
        2,
        "vacuity guard: the removal rotated"
    );

    let bytes = payload(13);
    let seal = |version: u64| {
        let key = *keys
            .key_for(version)
            .expect("the owner holds every generation");
        fauna_core::blob_seal::seal_blob(
            &bytes,
            Some((
                key,
                Some(fauna_core::blob_seal::SealStamp::Version(version)),
            )),
        )
        .unwrap()
        .manifest_hash
    };
    let (stale, fresh) = (seal(1), seal(2));
    let token = state.auth.token_store.insert(alice.id, 3600).await;

    agent.write("offline.bin", &bytes);
    // Well past the watcher flush plus the pre-seal read's bounded wait for a
    // connecting control plane (10 s) — an ungated pass would have sent its
    // chunks and manifest by now: the byte plane is open. Without the gate the
    // record also queues behind the control plane and lands at generation 1
    // the moment it returns, which the reconnect assertion below catches.
    tokio::time::sleep(2 * HOLD_QUIET).await;
    assert!(
        !manifest_on_nest(&base, &token, stale).await,
        "decision 2′ (c): a pass whose row read failed sent its generation-1 seal to the \
         nest over the still-open byte plane"
    );
    assert_eq!(seat.recorded_version("offline.bin").await, None);

    // The control plane is back: the first row read finds the floor ahead, the
    // agent re-keys, and the file is published under generation 2 alone.
    proxy.restore();
    assert_eq!(
        seat.await_recorded("offline.bin", "the file authored while cut off")
            .await,
        Some(2),
        "the file authored while cut off lands under the generation it missed"
    );
    assert!(
        manifest_on_nest(&base, &token, fresh).await,
        "probe control: the generation-2 seal is on the nest"
    );
    assert!(
        !manifest_on_nest(&base, &token, stale).await,
        "nothing sealed under generation 1 reached the nest after the reconnect either"
    );
}
