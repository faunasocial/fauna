//! tier_1: **launch-walk backlog is never offered to an undecided lease gate** —
//! `NestMailIndexLauncher::launch` holds until the advisory `index` lease has
//! had its first answer, bounded by a ceiling.
//!
//! Proof obligation (`docs/goal/behavior/content-index.md` § Where the index is
//! built → *The builder and the advisory task lease*, the launch-walk
//! sub-bullet): the gate a builder consults starts **closed** and opens only
//! when the lease loop's first observe→decide lands, while the receive loop
//! runs the Conversation attach walk the instant `launch()` returns and
//! re-pages the mailbox on its first sweep — both offered **once** per launch.
//! A walk that meets the closed default is withheld, unrecorded, and never
//! re-offered until the next launch: on the seat that turns out to *win* the
//! lease, a whole session of restored history is silently unsearchable. So the
//! launcher awaits the lease's first answer before it returns — after the arm
//! resumes, so the wait overlaps the round trips already in flight — and gives
//! up at a ceiling, where erring closed stands as the accepted cost.
//!
//! Pinned as a **flow assertion** against a fake nest the test drives frame by
//! frame, because the race it closes is between two of the launcher's own
//! round trips and cannot be provoked from outside the launcher:
//!
//! ```text
//! launcher.launch()
//!   -> index_lease::start        (spawns the loop; its observe goes out)
//!   -> ensure_mail_arm / ensure_master_arm   (refused by the fake nest)
//!   -> await the first answer    <- the launch is suspended HERE while the
//!                                   fake nest holds the observe reply
//!   -> [fake nest releases it] -> decide -> heartbeat -> settled
//!   -> launch() returns          <- only now; the gate reads the decision
//! ```
//!
//! **Why the fake nest holds the observe rather than merely watching for the
//! heartbeat.** A stub that answers instantly wins the race whether or not the
//! wait exists, exactly as the mail-ordering test found for the launch-before-
//! spawn contract (`fauna-conversations`'s `index_launch_ordering_tests.rs`).
//! Holding the reply turns the race into a causal fact: with the wait in
//! place, `launch()` returning while the reply is held is not unlikely, it is
//! impossible short of the ceiling. The negative assert is therefore anchored
//! to the held reply; the hold is sized far above any non-pathological delay,
//! and a green run pays it once.
//!
//! The fake nest refuses every other request (the arm resumes, the pin load),
//! which is a supported launch: an arm that cannot resume now is retried by the
//! receive loop on its next sweep, so nothing here depends on an arm existing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::testing::{MockClientChannel, ServerSide, connect_queue, mpsc_pair};
use fauna_client::{AuthClient, NestClient, PushBroker};
use fauna_client_conversations::{
    FIRST_ANSWER_CEILING, IndexLeaseSeat, MailKeyCache, NestMailIndexLauncher,
};
use fauna_conversations::backend::IndexBuilderLauncher;
use fauna_core::delegation::ParticipantClass;
use fauna_core::identity::ActorKeypair;
use fauna_protocol::delegation::{
    HeartbeatReply, HeartbeatRequest, KIND_HEARTBEAT, KIND_OBSERVE, LeaseState, ObserveReply,
};
use fauna_protocol::{Frame, Reply, RpcError, Value, decode_frame, encode_frame};
use fauna_ws_substrate::{Supervisor, run_supervisor};
use tokio::sync::Notify;

/// Generous ceiling for a positive wait ("the observe reached the fake nest",
/// "launch returned once released"). Sized far above any non-pathological
/// delay so a busy machine never flips the verdict; a green run pays none of
/// it.
const OBSERVED_BUDGET: Duration = Duration::from_secs(120);

/// How long the fake nest holds the observe reply before the test checks that
/// `launch()` is still suspended. Not a settle-sleep for the positive path —
/// the hold itself is the causal barrier — but the time a *wrong* launcher
/// (one that does not wait) is given to reveal itself, so it is sized well
/// above the two refused round trips such a launcher performs before it
/// returns. Far below the launcher's own ceiling, which would release a
/// correct launcher too.
const HOLD: Duration = Duration::from_secs(2);

/// The single ordered log both sides write to — the ordering claim is a
/// statement about this sequence, so it is recorded rather than inferred.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Event {
    /// The lease's observe reached the fake nest, which is now holding it.
    ObserveHeld,
    /// The fake nest answered the held observe (a free lease).
    ObserveReleased,
    /// The fake nest answered the heartbeat the decision sent — the lease
    /// loop's first step is complete from here, and the gate reads `Acquire`.
    HeartbeatAnswered,
    /// `launch()` returned its observer.
    LaunchReturned,
}

#[derive(Default)]
struct Journal(Mutex<Vec<Event>>);

impl Journal {
    fn push(&self, e: Event) {
        self.0.lock().unwrap().push(e);
    }
    fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }
}

fn value_of<T: serde::Serialize>(v: &T) -> Value {
    let bytes = fauna_protocol::encode_canonical(v).expect("encode");
    fauna_protocol::decode_strict(&bytes).expect("decode as Value")
}

/// The fake nest: holds the lease's observe until `release` is pulsed, answers
/// the heartbeat a decision sends, and refuses everything else. Runs until the
/// client side goes away.
async fn fake_nest(mut server: ServerSide, journal: Arc<Journal>, release: Arc<Notify>) {
    let mut held: Option<u64> = None;
    loop {
        tokio::select! {
            frame = server.rx_from_client.recv() => {
                let Some(bytes) = frame else { break };
                let Ok(Frame::Request(req)) = decode_frame(&bytes) else { continue };
                let reply = match req.kind.as_str() {
                    KIND_OBSERVE => {
                        assert!(held.is_none(), "one observe in flight at a time");
                        held = Some(req.correlation_id);
                        journal.push(Event::ObserveHeld);
                        continue;
                    }
                    KIND_HEARTBEAT => {
                        let bytes = fauna_protocol::encode_canonical(&req.payload).unwrap();
                        let hb: HeartbeatRequest = fauna_protocol::decode_strict(&bytes).unwrap();
                        journal.push(Event::HeartbeatAnswered);
                        Reply {
                            ty: Reply::TYPE,
                            correlation_id: req.correlation_id,
                            payload: value_of(&HeartbeatReply {
                                lease: LeaseState {
                                    task_kind: hb.task_kind,
                                    holder: hb.holder,
                                    holder_class: hb.holder_class,
                                    age_ms: 0,
                                    extra: Default::default(),
                                },
                                extra: Default::default(),
                            }),
                            ok: true,
                        }
                    }
                    // The arm resumes and the pin load: refused, which is a
                    // supported launch (retried on the next sweep).
                    _ => Reply {
                        ty: Reply::TYPE,
                        correlation_id: req.correlation_id,
                        payload: value_of(&RpcError::new("unavailable", "error.unavailable")),
                        ok: false,
                    },
                };
                if server.tx_to_client.send(encode_frame(&Frame::Reply(reply)).unwrap()).await.is_err() {
                    break;
                }
            }
            _ = release.notified(), if held.is_some() => {
                let correlation_id = held.take().unwrap();
                journal.push(Event::ObserveReleased);
                let reply = Reply {
                    ty: Reply::TYPE,
                    correlation_id,
                    payload: value_of(&ObserveReply {
                        leases: vec![],
                        extra: Default::default(),
                    }),
                    ok: true,
                };
                if server.tx_to_client.send(encode_frame(&Frame::Reply(reply)).unwrap()).await.is_err() {
                    break;
                }
            }
        }
    }
}

/// A connected `NestClient` over the in-memory transport, the fake nest on the
/// other end. The keypair is what lets the master arm *try* to resume (its key
/// derives from the identity seed), so the refused rail read is a real one.
fn connected_client(
    journal: Arc<Journal>,
    release: Arc<Notify>,
) -> (Arc<NestClient>, tokio::task::JoinHandle<()>) {
    let auth = Arc::new(AuthClient::new(
        "http://127.0.0.1:0".into(),
        ActorKeypair::from_secret([7u8; 32]),
    ));
    let client = NestClient::with_auth(Arc::clone(&auth));
    let (slot, state_tx) = client.supervisor_channels_for_test();
    let (adapter, server) = mpsc_pair();
    let nest = tokio::spawn(fake_nest(server, journal, release));
    let channel = Arc::new(MockClientChannel::new(
        connect_queue([Ok(adapter)]),
        auth,
        PushBroker::new(16),
    ));
    tokio::spawn(run_supervisor(Supervisor {
        channel,
        dispatcher_slot: slot,
        connection_state_tx: state_tx,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
    }));
    (client, nest)
}

/// A seat with no pins: the account store answers the default (automatic)
/// delegation record.
struct NoPins;

#[async_trait::async_trait]
impl fauna_client_config::PreferenceStore for NoPins {
    async fn moderation(
        &self,
    ) -> Result<fauna_core::data::ModerationConfig, fauna_client_config::StoreError> {
        Ok(Default::default())
    }
    async fn personalization(
        &self,
    ) -> Result<fauna_core::data::PersonalizationConfig, fauna_client_config::StoreError> {
        Ok(Default::default())
    }
    async fn delegation(
        &self,
    ) -> Result<fauna_core::data::DelegationConfig, fauna_client_config::StoreError> {
        Ok(Default::default())
    }
}

fn seated_launcher(client: &Arc<NestClient>) -> Arc<NestMailIndexLauncher> {
    NestMailIndexLauncher::new(
        Arc::clone(client),
        MailKeyCache::new(
            Arc::clone(client),
            Arc::new(fauna_client_config::test_helpers::FakeMailStore::empty()),
        ),
        Some(IndexLeaseSeat {
            device_id: [0x5eu8; 32],
            class: ParticipantClass::PluggedInDesktop,
            pins: Arc::new(NoPins),
        }),
    )
}

/// Poll `cond` every few milliseconds until it holds or `OBSERVED_BUDGET`
/// runs out — a deadline poll, never a settle-sleep (convention 14).
async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + OBSERVED_BUDGET;
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "gave up waiting for: {what}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// **The ordering.** `launch()` does not return while the lease's first answer
/// is outstanding, and returns once it lands — so the walk the receive loop
/// runs on return, and the first mail sweep after it, meet a gate that holds
/// the decision rather than the closed default.
#[tokio::test]
async fn launch_holds_until_the_lease_has_its_first_answer() {
    let journal = Arc::new(Journal::default());
    let release = Arc::new(Notify::new());
    let (client, nest) = connected_client(Arc::clone(&journal), Arc::clone(&release));
    let launcher = seated_launcher(&client);

    let launching = tokio::spawn({
        let launcher = Arc::clone(&launcher);
        async move { launcher.launch().await }
    });

    wait_until("the observe to reach the fake nest", || {
        journal.events().contains(&Event::ObserveHeld)
    })
    .await;

    // The causal barrier: the observe reply is held, so a launcher that waits
    // for the first answer cannot return here. One that does not wait has
    // long since returned — its only other round trips were refused at once.
    tokio::time::sleep(HOLD).await;
    assert!(
        !launching.is_finished(),
        "launch() returned while the lease's first observe was still unanswered — the \
         attach walk it hands the receive loop would meet an undecided (closed) gate and \
         be withheld for the whole session; journal: {:?}",
        journal.events()
    );

    release.notify_one();
    let observer = tokio::time::timeout(OBSERVED_BUDGET, launching)
        .await
        .expect("launch() must return once the first answer lands")
        .expect("launch task panicked");
    journal.push(Event::LaunchReturned);
    assert!(
        observer.is_some(),
        "the native launcher returns its container unconditionally"
    );

    assert_eq!(
        journal.events(),
        vec![
            Event::ObserveHeld,
            Event::ObserveReleased,
            Event::HeartbeatAnswered,
            Event::LaunchReturned,
        ],
        "launch() must return only after the decision's heartbeat was answered — that is \
         when the lease loop's first step completes and the gate reads Acquire"
    );

    drop(launcher);
    drop(client);
    nest.abort();
}

/// **The ceiling.** A lease whose first answer never comes must not hold the
/// receive loop's prologue for ever: `launch()` returns at the ceiling with the
/// gate still erring closed — the accepted cost, now confined to a nest that
/// cannot answer one observe inside the ceiling.
#[tokio::test(start_paused = true)]
async fn launch_gives_up_waiting_at_the_ceiling() {
    let journal = Arc::new(Journal::default());
    // Never pulsed: the observe stays held for the life of the test.
    let release = Arc::new(Notify::new());
    let (client, nest) = connected_client(Arc::clone(&journal), release);
    let launcher = seated_launcher(&client);

    let started = tokio::time::Instant::now();
    let launching = tokio::spawn({
        let launcher = Arc::clone(&launcher);
        async move { launcher.launch().await }
    });

    // The paused clock auto-advances whenever every task is idle, so the
    // ceiling elapses without the test naming it; a launcher that waited
    // unboundedly would leave this awaiting until the outer timeout.
    let observer = tokio::time::timeout(Duration::from_secs(600), launching)
        .await
        .expect("launch() must give up at the ceiling when the lease never answers")
        .expect("launch task panicked");
    journal.push(Event::LaunchReturned);
    assert!(observer.is_some());
    // The wait was PAID, not skipped: a launcher that never waited would
    // return with the clock barely moved. Latency-independent — the paused
    // clock moves only by what the runtime's own timers ask for.
    assert!(
        started.elapsed() >= FIRST_ANSWER_CEILING,
        "launch() returned after {:?}, before the {:?} ceiling — it did not wait for the \
         lease's first answer at all",
        started.elapsed(),
        FIRST_ANSWER_CEILING
    );

    assert_eq!(
        journal.events(),
        vec![Event::ObserveHeld, Event::LaunchReturned],
        "no decision ever landed: the observe stayed held and nothing heartbeated"
    );

    drop(launcher);
    drop(client);
    nest.abort();
}
