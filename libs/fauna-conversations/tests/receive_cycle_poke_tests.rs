//! tier_1: the receive loop's **run-one-cycle-now poke** and its **completion
//! observable** — convention 14 mechanism 3 applied to the client receive loop
//! (`docs/goal/architecture/e2e-conventions.md` § convention 14: *"advance the
//! clock, poke the cycle, deadline-poll the cycle's positive completion
//! observable"*).
//!
//! **Why this file exists at tier_1.** Before the poke, a test that needed a
//! receive sweep could only wait out the backstop tick, and the e2e's answer was
//! to *shorten* it (`FAUNA_CONV_POLL_SECS=2`) — a shortened tick is still a
//! wall-clock dependence, it just lowers the odds of a false pass. Convention 14
//! puts the cadence logic here and leaves the e2e one wiring proof, so these are
//! the pins that make the e2e's single poke trustworthy.
//!
//! **The ticker is muted, not raced.** Every test here starts the loop with a
//! one-day backstop cadence, so `tokio::time::interval`'s immediate first tick is
//! the *only* cycle the ticker can ever produce. Any later cycle is therefore
//! provably the poke's — which is what stops these pins from passing on a tick
//! that happened to arrive inside a generous budget (the "unable to fail" shape
//! D6e/D7 keep finding).

mod common;
use common::SilentNest;

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fauna_conversations::ConversationsSession;
use fauna_conversations::backend::{
    ConversationsPush, InboundMailPage, InboundMailSource, OutboundMailSink,
};
use fauna_conversations::manager::ConversationsManager;
use fauna_conversations::session::ReceiveLoopExit;
use fauna_core::identity::ActorKeypair;
use fauna_mls::engine::MlsEngine;

/// Generous ceiling for "the loop got through a cycle". Sized far above any
/// non-pathological delay so a loaded machine never flips the verdict; a green
/// run pays none of it (the deadline poll returns the instant it holds).
const CYCLE_BUDGET: Duration = Duration::from_secs(120);

/// One day. The backstop ticker fires once (immediately) and never again inside
/// a test — see the module docstring for why that is what makes these pins able
/// to fail.
const MUTED_TICKER_SECS: &str = "86400";

/// A push seam that never delivers: these tests drive the loop through the poke
/// and the ticker only, so the push arm must simply park.
struct SilentPush {
    park: tokio::sync::Notify,
}

#[async_trait]
impl ConversationsPush for SilentPush {
    async fn next_event(&self) -> Option<fauna_conversations::backend::ConvPushEvent> {
        loop {
            self.park.notified().await;
        }
    }
}

/// Reads the live counter pair from inside a running sweep — the only place the
/// in-flight state exists.
type CycleReader = Arc<dyn Fn() -> (u64, u64) + Send + Sync>;

/// An INBOX that records what the counters read **while a sweep is in flight**,
/// and can be gated so a test can hold a cycle open.
struct WatchingMailbox {
    /// One entry per `fetch`, each `(started, completed)` as seen mid-sweep.
    seen: Mutex<Vec<(u64, u64)>>,
    cycles: Mutex<Option<CycleReader>>,
    /// While `> 0`, each fetch parks until `release` is notified — the way a test
    /// holds one cycle open to poke underneath it.
    hold: AtomicUsize,
    /// ⚠ Released with `notify_one`, never `notify_waiters`: the releasing test
    /// cannot observe the exact moment the fetch parks, and `notify_waiters`
    /// wakes only waiters already registered, so that version is lost whenever
    /// the release wins the race and the fetch then parks forever. `notify_one`
    /// stores a permit, so the order does not matter. (Measured: the
    /// `notify_waiters` form passed once and then hung its test to the full
    /// budget — a self-inflicted instance of exactly the wall-clock-shaped
    /// flakiness this file exists to remove.)
    release: tokio::sync::Notify,
    fetches: AtomicU64,
}

impl WatchingMailbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            cycles: Mutex::new(None),
            hold: AtomicUsize::new(0),
            release: tokio::sync::Notify::new(),
            fetches: AtomicU64::new(0),
        })
    }
    fn watch(&self, read: CycleReader) {
        *self.cycles.lock().unwrap() = Some(read);
    }
    fn observations(&self) -> Vec<(u64, u64)> {
        self.seen.lock().unwrap().clone()
    }
    fn fetches(&self) -> u64 {
        self.fetches.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl InboundMailSource for WatchingMailbox {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        if let Some(read) = self.cycles.lock().unwrap().as_ref() {
            self.seen.lock().unwrap().push(read());
        }
        if self.hold.load(Ordering::SeqCst) > 0 {
            self.hold.fetch_sub(1, Ordering::SeqCst);
            self.release.notified().await;
        }
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records: vec![],
            more: false,
        })
    }
}

/// A second mailbox slot the loop also sweeps (Sent) — inert here.
struct EmptyMailbox;

#[async_trait]
impl InboundMailSource for EmptyMailbox {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        Ok(InboundMailPage {
            skipped: Vec::new(),
            highest_modseq: 0,
            records: vec![],
            more: false,
        })
    }
}

struct NoSend;

#[async_trait]
impl OutboundMailSink for NoSend {
    async fn submit(&self, _recipients: Vec<String>, _raw: Vec<u8>) -> Result<(), String> {
        Err("this test never sends".into())
    }
}

/// Build a session wired to the mail rail only, with the ticker muted (module
/// docstring). The env read happens inside `start_receive_loop`, so the value is
/// set before the caller spawns it; every test in this binary sets the same
/// value, so the process-global write is not a race between them.
fn session(inbox: Arc<WatchingMailbox>) -> Arc<ConversationsSession> {
    session_over(inbox)
}

/// [`session`] over any INBOX — the panic pin needs one that is not a
/// [`WatchingMailbox`].
fn session_over(inbox: Arc<dyn InboundMailSource>) -> Arc<ConversationsSession> {
    // SAFETY: single value, written by every test in this binary before its loop
    // starts, and never read by anything but `resolve_poll_secs`.
    unsafe { std::env::set_var("FAUNA_CONV_POLL_SECS", MUTED_TICKER_SECS) };
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let self_actor = engine.identity_actor_id();
    let session = ConversationsSession::from_manager(
        ConversationsManager::new(),
        engine,
        Arc::new(SilentNest {
            label: "receive-cycle poke tests",
            permissive: true,
        }),
        "me@example.com".to_string(),
        self_actor,
        Some(Arc::new(SilentPush {
            park: tokio::sync::Notify::new(),
        }) as Arc<dyn ConversationsPush>),
    );
    session.register_smtp(Arc::new(NoSend));
    session.register_mail_receive(inbox, Arc::new(EmptyMailbox));
    session
}

/// Deadline-poll a counter predicate. Positive wait, generous budget, verdict
/// independent of how long the machine takes (convention 14).
async fn await_cycles(
    session: &Arc<ConversationsSession>,
    mut done: impl FnMut(u64, u64) -> bool,
    what: &str,
) {
    let deadline = tokio::time::Instant::now() + CYCLE_BUDGET;
    loop {
        let cycles = session.receive_cycles();
        if done(cycles.started(), cycles.completed()) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; counters were started={} completed={}",
            cycles.started(),
            cycles.completed()
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// **The poke pin.** A poke runs a *fresh full sweep* — not merely a counter
/// bump: the mailbox is fetched again, which is what makes the observable mean
/// "delivery has been attempted since you asked".
///
/// Reds under: no poke arm in the `select!` (the second cycle never arrives, and
/// the muted ticker cannot supply it); a poke arm that bumps the counters without
/// expanding `full_sweep!` (the fetch count does not move).
#[tokio::test(flavor = "multi_thread")]
async fn a_poke_runs_a_fresh_full_sweep() {
    let inbox = WatchingMailbox::new();
    let session = session(Arc::clone(&inbox));

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // The ticker's immediate first tick — the one cycle it will ever run here.
    await_cycles(&session, |_, completed| completed >= 1, "the launch cycle").await;
    let baseline = session.receive_cycles().started();
    let fetches_before = inbox.fetches();

    session.poke_receive_cycle();

    await_cycles(
        &session,
        |_, completed| completed > baseline,
        "a cycle that began after the poke",
    )
    .await;
    assert!(
        inbox.fetches() > fetches_before,
        "the poked cycle must sweep the rails, not just count itself: fetches \
         were {fetches_before} before the poke and {} after",
        inbox.fetches()
    );
}

/// **The ordering pin.** `started` is bumped *before* the cycle reads any rail
/// and `completed` only after it finishes — the property the consumer's
/// pigeonhole rests on (`ReceiveCycles`). Stated from inside a live sweep,
/// because that is the only place the in-flight state exists.
///
/// Reds under: bumping both counters at the end of the sweep (mid-sweep reads
/// then show `started == completed`), or bumping `started` after the drain.
#[tokio::test(flavor = "multi_thread")]
async fn the_counters_bracket_the_sweep_that_is_running() {
    let inbox = WatchingMailbox::new();
    let session = session(Arc::clone(&inbox));
    let watch_session = Arc::clone(&session);
    inbox.watch(Arc::new(move || {
        let cycles = watch_session.receive_cycles();
        (cycles.started(), cycles.completed())
    }));

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    await_cycles(&session, |_, completed| completed >= 1, "the launch cycle").await;

    let observations = inbox.observations();
    assert!(
        !observations.is_empty(),
        "the launch cycle should have fetched the INBOX at least once"
    );
    for (started, completed) in observations {
        assert!(
            started > completed,
            "a sweep that is RUNNING must read started>completed (it read \
             started={started} completed={completed}); if they are equal, a \
             consumer waiting for completed>baseline can be released by the very \
             cycle that was already in flight when it took the baseline"
        );
    }
}

/// **The mid-sweep poke pin.** A poke that lands while a cycle is running is not
/// lost — the signal stores one permit, so a fresh cycle runs as soon as the
/// in-flight one returns. Without that, a consumer would take its baseline,
/// poke, and then wait out the (production, 30 s) backstop it was poking to
/// avoid.
///
/// Reds under: a poke implementation that drops the signal when no waiter is
/// parked on it (the second cycle never arrives under the muted ticker).
#[tokio::test(flavor = "multi_thread")]
async fn a_poke_that_lands_mid_sweep_is_not_lost() {
    let inbox = WatchingMailbox::new();
    // Hold the FIRST fetch — the launch cycle parks inside the mail rail, so the
    // poke below provably arrives while a cycle is in flight.
    inbox.hold.store(1, Ordering::SeqCst);
    let session = session(Arc::clone(&inbox));

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // Wait until the loop is *inside* the launch cycle: started has moved,
    // completed has not.
    await_cycles(
        &session,
        |started, completed| started >= 1 && completed == 0,
        "the launch cycle to be in flight",
    )
    .await;
    let baseline = session.receive_cycles().started();

    session.poke_receive_cycle();
    inbox.release.notify_one();

    await_cycles(
        &session,
        |_, completed| completed > baseline,
        "the poke's own cycle, after the one it landed under",
    )
    .await;
}

/// **The release pin.** Dropping the session must end the loop *and release the
/// engine it drives* — promptly, not at the backstop's next tick. Two consumers
/// open that engine's store next: the post-succession sweep retry opens the
/// retired identity's `mls_state.db` on the device the ceremony ran on
/// (`settings.md` § Recovery kit → *Finishing an unfinished group sweep*), and
/// an account switch back to this identity opens it as the incoming session
/// (`account-scoping.md` § Serialized switching). Both meet the one-engine-per-
/// store role lock, which an outgoing loop parked in `select!` with strong
/// `backend`/`manager` handles kept held until its next tick — the production
/// 30 s backstop, measured 2026-08-27.
///
/// Reds under the polled `Weak<()>` liveness alone (the shape until
/// 2026-08-27): with the ticker muted the loop never wakes to notice the drop,
/// and the engine outlives the whole budget.
#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_session_releases_its_engine_without_waiting_for_a_tick() {
    let inbox = WatchingMailbox::new();
    let session = session(Arc::clone(&inbox));
    let engine = Arc::downgrade(&session.engine());
    let cycles = session.receive_cycles();

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });
    // The launch cycle: the loop is now parked in its `select!`, holding
    // whatever it holds.
    await_cycles(&session, |_, completed| completed >= 1, "the launch cycle").await;

    drop(session);

    let deadline = tokio::time::Instant::now() + CYCLE_BUDGET;
    while engine.upgrade().is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the receive loop kept the dropped session's engine alive for the whole \
             {CYCLE_BUDGET:?} budget — its strong handles outlived the session it serves"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The engine is free once the loop's own task has returned; its supervisor
    // records the reason a moment later.
    while !cycles.ended() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the released loop never reported itself ended"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        cycles.exit(),
        Some(ReceiveLoopExit::SessionClosed),
        "a dropped session is a designed exit and must be named as one"
    );
}

/// **The failed-build strand pin.** The hand-over's own step — the factory's
/// `retire_conversations_engine`, run *before* the successor engine is built —
/// must end the predecessor's receive loop **on its own**, without waiting for
/// anyone to drop the session.
///
/// Why that is the contract and not merely tidy: the successor build can throw
/// (`MlsEngine::new` refusing `ServedElsewhere`, a bad db path, a mid-build RPC
/// failure), and every shell that reaches this seam installs the new session
/// *only on success* — apple's `ConversationsVM.activate` assigns `self.session`
/// as its first statement, windows' `App.xaml.cs` assigns `_liveConvSession`
/// inside `if (convSession is not null)`, both inside a best-effort `try` whose
/// catch arm only logs. So on the failure path the shell keeps holding the
/// **predecessor**, and a loop that exits only at the drop is a loop that never
/// exits: one leaked task per failed build, each still holding `backend` and
/// `manager` strongly, accumulating for the life of the process.
/// `account-scoping.md` § The scoping taxonomy, corollary 2 is the rule that
/// breaks — *"the background loops that WRITE that state must be retired by the
/// same drop"* — and `account-data-plane.md` § Multi-instance concurrency is why
/// the fix cannot be a shell obligation: the predecessor is held "in three
/// languages at once", so the release has to be independent of every remaining
/// reference.
///
/// This test keeps `session` alive across the whole assertion **on purpose** —
/// that live binding IS the stranded shell field. Reds against the
/// session-closed arm alone: with the ticker muted the loop never wakes, and
/// `ended` stays false for the entire budget.
#[tokio::test(flavor = "multi_thread")]
async fn retiring_the_engine_ends_the_loop_a_failed_build_left_installed() {
    let inbox = WatchingMailbox::new();
    let session = session(Arc::clone(&inbox));
    let manager = session.manager();

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });
    // The launch cycle: the loop is now parked in its `select!`.
    await_cycles(&session, |_, completed| completed >= 1, "the launch cycle").await;
    assert!(
        !session.receive_cycles().ended(),
        "the loop reported itself gone before anything retired it"
    );

    // Exactly what `FfiNestClient::conversations_session*` runs before
    // `MlsEngine::new` — and then the successor build fails, so nothing after
    // this point ever replaces the shell's session.
    manager.retire_conversations_engine();

    let deadline = tokio::time::Instant::now() + CYCLE_BUDGET;
    while !session.receive_cycles().ended() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the retired engine's receive loop was still running {CYCLE_BUDGET:?} after \
             the hand-over — a failed successor build strands it for the life of the \
             process, holding `backend` and `manager` strongly"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A hand-over is a designed exit, not a failure: the reason says so, and the
    // app is told nothing — a "receiving stopped" banner after every account
    // switch would teach users to ignore the one that means it.
    assert_eq!(
        session.receive_cycles().exit(),
        Some(ReceiveLoopExit::EngineRetired),
        "the retired loop must name the retire as its exit"
    );
    assert!(
        !manager.receive_stopped(),
        "an engine hand-over reported the receive rail stopped to the app"
    );

    // Held across the entire assertion above, and dropped only now: the loop
    // ended while the shell's reference was still installed, which is the whole
    // claim.
    drop(session);
}

/// An INBOX whose every fetch panics — a receive pass dying inside a rail, the
/// shape of the three-day web outage (a `RefCell` borrow panicking inside
/// `poll_conversations` on every tick), reproduced on the native loop.
struct PanickingMailbox;

#[async_trait]
impl InboundMailSource for PanickingMailbox {
    async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
        panic!("receive-cycle poke tests: this receive pass panics on purpose")
    }
}

/// **The dead-rail pin.** A panic inside a receive pass must reach the app —
/// the manager every app's `error-message` projection already reads — and the
/// loop's exit must name it as a panic, distinct from its two designed exits.
///
/// Why both halves: `ended` alone cannot tell a panic from an account switch
/// (both flip it), so an app reading `ended` would either cry wolf on every
/// hand-over or stay silent on a dead rail. Before this pin the panic path
/// flipped `ended` and nothing else read it, so a dead rail was silent on every
/// app until a restart the user was never told to do.
///
/// Reds under: a supervisor that records the exit but never tells the manager
/// (`receive_stopped` stays false); a loop that no longer distinguishes a panic
/// from a clean return (the exit reads `None` or a designed exit).
#[tokio::test(flavor = "multi_thread")]
async fn a_panicking_receive_pass_reports_the_rail_stopped_to_the_app() {
    let session = session_over(Arc::new(PanickingMailbox));
    let manager = session.manager();
    assert!(
        !manager.receive_stopped(),
        "sanity: no loop has run, so nothing can have stopped"
    );

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });

    // The ticker's immediate first tick sweeps the mail rail, which panics.
    let deadline = tokio::time::Instant::now() + CYCLE_BUDGET;
    while !session.receive_cycles().ended() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "a receive pass that panics did not end the loop within {CYCLE_BUDGET:?}"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(
        session.receive_cycles().exit(),
        Some(ReceiveLoopExit::Panicked),
        "the loop died by panic and must say so"
    );
    assert!(
        manager.receive_stopped(),
        "the loop died by panic but the manager the app renders was never told — \
         the rail is dead and every app stays silent about it"
    );
}

/// **The fresh-manager-factory pin — android's shape, offline, and a
/// narrower miss than the swap pin below.** A shell whose login path calls the
/// factory's fresh-manager arm gets no hand-over from the shared factory AT
/// ALL — not even the no-op call the swap pin measures, because that arm hands
/// the factory no manager to call `retire_conversations_engine` on in the
/// first place.
///
/// `FfiNestClient::conversations_session` — android's
/// `nestClient.conversationsSession(...)`
/// (`ConversationsManagerHost.kt:225-231`) — is exactly this arm:
/// `build_conversations_session`'s `manager` parameter is `None`, so its
/// `if let Some(m) = &manager { m.retire_conversations_engine(); }` guard
/// (`libs/fauna-ffi/src/nest_client.rs:1967-1968`) never executes, for ANY
/// manager — unlike windows' swapped-in manager, which at least *receives*
/// the call and takes its no-op arm only because it carries no
/// `Rail::FaunaMls`. Android's predecessor engine is therefore reachable only
/// through the shell's own explicit `retireConversationsEngine()` call on the
/// OUTGOING manager — `ConversationsManagerHost.stopConversationsSession`
/// (`ConversationsManagerHost.kt:258-268`) now makes it, before nulling
/// `sessionManager`, the android twin of windows' `ResetForActorChange` call
/// (`account-runtime.md` § Multi-instance concurrency).
///
/// Convention 14: the negative half is anchored on a causal barrier — a whole
/// second session built and left idle, the fresh-manager-arm shape itself
/// (`from_parts` is `from_manager` over a new manager, per
/// `build_conversations_session`'s own doc comment) — never a settle-sleep.
#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_manager_factory_build_hands_nothing_over_only_the_shells_own_retire_does() {
    let inbox = WatchingMailbox::new();
    let predecessor = session(Arc::clone(&inbox));
    let outgoing = predecessor.manager();

    let loop_session = Arc::clone(&predecessor);
    tokio::spawn(async move { loop_session.start_receive_loop().await });
    await_cycles(
        &predecessor,
        |_, completed| completed >= 1,
        "the launch cycle",
    )
    .await;

    // The fresh-manager factory arm, modeled exactly: a second session over a
    // brand-new manager, touching nothing of `outgoing` — there is no
    // substitute manager here for a factory call to even no-op against.
    let _successor = session(WatchingMailbox::new());

    assert!(
        !predecessor.receive_cycles().ended(),
        "the predecessor's loop ended after a fresh-manager successor build touched \
         nothing of its own — this pin is measuring nothing"
    );

    // The outgoing manager's own retire — what `stopConversationsSession` now
    // calls before nulling `sessionManager`.
    outgoing.retire_conversations_engine();

    let deadline = tokio::time::Instant::now() + CYCLE_BUDGET;
    while !predecessor.receive_cycles().ended() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the shell's own retire did not end the predecessor's loop within \
             {CYCLE_BUDGET:?} — a fresh-manager-factory shell has no other seam that works"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Held across the whole assertion, as the shell's stale field is.
    drop(predecessor);
}

/// **The manager-swap pin — windows' shape, offline.** A shell that replaces its
/// manager *before* the successor build gets NO hand-over from the shared
/// factory, because the factory can only retire the manager it is handed.
///
/// `fauna_ffi::FfiNestClient::conversations_session*` runs
/// `retire_conversations_engine` on its caller-supplied manager before
/// `MlsEngine::new`, and `account-data-plane.md` § Multi-instance concurrency
/// says every UniFFI app therefore inherits the hand-over "with no glue of its
/// own". That is true only while the shell keeps ONE manager across the seam.
/// Windows does not: `ConversationsManagerHost.ResetForActorChange` swaps
/// `Instance` for a fresh manager at every actor change (its sanctioned
/// exception to the no-swap rule) and only then logs back in — so the manager
/// the factory retires is a brand-new one with no `Rail::FaunaMls` registered,
/// the call takes its documented no-op arm, and the predecessor engine keeps its
/// role lock and its open `mls_state.db`. Measured as two symptoms in one
/// window: the account-scope erase failing `os error 32`, and the successor
/// `MlsEngine::new` refused `ServedElsewhere`, wedging the first conversations
/// command after a relaunch.
///
/// The point of pinning the **no-op** is that it is what makes the shell's own
/// retire load-bearing rather than belt-and-braces: delete windows'
/// `RetireConversationsEngine()` call and nothing else releases the predecessor.
///
/// Convention 14: the negative half is anchored on a causal barrier, never a
/// settle-sleep — a poked cycle completing *after* the fresh manager's retire is
/// proof the loop is alive and progressing, not proof that 120 s of nothing
/// happened.
#[tokio::test(flavor = "multi_thread")]
async fn retiring_a_swapped_in_manager_hands_nothing_over_only_the_outgoing_one_does() {
    let inbox = WatchingMailbox::new();
    let session = session(Arc::clone(&inbox));
    let outgoing = session.manager();

    let loop_session = Arc::clone(&session);
    tokio::spawn(async move { loop_session.start_receive_loop().await });
    await_cycles(&session, |_, completed| completed >= 1, "the launch cycle").await;

    // The swap: windows' host now hands out THIS manager, and the predecessor's
    // rails stay where they were.
    let swapped_in = ConversationsManager::new();

    // Exactly what the shared factory runs before `MlsEngine::new` — against the
    // manager it was handed, which after the swap is the fresh one.
    swapped_in.retire_conversations_engine();

    // Causal barrier: a cycle that BEGAN after that call still completes, so the
    // predecessor's loop — and the engine it holds — is provably untouched.
    let baseline = session.receive_cycles().completed();
    session.poke_receive_cycle();
    await_cycles(
        &session,
        |_, completed| completed > baseline,
        "a cycle that began after the swapped-in manager's retire",
    )
    .await;
    assert!(
        !session.receive_cycles().ended(),
        "retiring the SWAPPED-IN manager ended the predecessor's loop — then the \
         shell's own retire would be redundant and this pin is measuring nothing"
    );

    // The outgoing manager's own retire is what actually hands the role over —
    // the call windows makes at its swap seam, before dropping the manager.
    outgoing.retire_conversations_engine();

    let deadline = tokio::time::Instant::now() + CYCLE_BUDGET;
    while !session.receive_cycles().ended() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the OUTGOING manager's retire did not end the predecessor's loop within \
             {CYCLE_BUDGET:?} — the hand-over has no seam left that works"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Held across the whole assertion, as the shell's stale field is.
    drop(session);
}
