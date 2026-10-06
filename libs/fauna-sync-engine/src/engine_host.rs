//! Generic multi-engine host: the platform-agnostic multiplexing skeleton that
//! drives N keyed, cancellable per-engine futures on one dedicated worker thread
//! with a current-thread tokio runtime, and serves live start/stop commands.
//!
//! Lifted from the since-retired Linux in-process resident driver (A3,
//! `sync-agent.md` § Scope per platform — resident engines now live in the
//! per-user `fauna-sync-agent`; `apps/fauna-linux/src/sync.rs` keeps only the
//! state-dir layout). Today the agent's resident engine driver
//! (`bins/fauna-sync-agent/src/engine_driver.rs`) and its Windows on-demand
//! hydration host (`cfapi_host.rs`) both consume **one** implementation here
//! (priority #2 — maximize shared Rust): each supplies an [`EngineSpec`] that
//! says *how* to build one engine's future (and any long-lived side task /
//! running-count side effect), while this module owns the multiplexing — the
//! `FuturesUnordered`, the per-key cancellation tokens, the generation guard
//! against a stale restart retiring a live engine, and the `select!` loop over
//! cancel / command-channel / engine-completion / background-task.
//!
//! Why a dedicated thread with a *current-thread* runtime rather than a shared
//! multi-thread one: the engines a real spec builds (notably
//! [`engine::SyncEngine`](crate::engine::SyncEngine)) are `Send` but
//! intentionally **`!Sync`** (rusqlite `Connection`) with `&self` async methods
//! held across `.await`s, so they can't be `tokio::spawn`ed onto a multi-thread
//! runtime. Every engine future is polled inline by one current-thread runtime
//! via a [`FuturesUnordered`] — no `spawn`, no cross-thread sharing. Hence
//! [`EngineFuture`] is **not** `Send`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::thread;

use futures_util::FutureExt; // `catch_unwind` — per-engine panic containment
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
// Re-exported so an `EngineSpec` implementor (the Windows hydration host, the
// Linux sync driver) can name the `run`/`background` cancel token without taking
// its own direct `tokio-util` dependency.
pub use tokio_util::sync::CancellationToken;

/// A per-engine future: owns ALL resources for one keyed engine (the SyncEngine
/// plus its watcher or its OS provider-root registration + callback-routing
/// entry) and releases them when it completes — on cancel (its token fires) or
/// natural end. Teardown lives *inside* the future (drop guards / a select on
/// `cancel`), so the host only has to cancel the token. Not `Send`: the host
/// polls it inline on one current-thread runtime (SyncEngine is `Send` but
/// `!Sync`).
pub type EngineFuture = Pin<Box<dyn Future<Output = ()>>>;

/// Per-deployment policy: how to build one engine's future, plus an optional
/// long-lived side task. Implemented by the Linux always-resident driver and the
/// Windows on-demand hydration host (later slices).
pub trait EngineSpec: Send + 'static {
    /// Start descriptor for one engine (e.g. a folder↔folder mapping). Crosses
    /// the command channel, so `Send`.
    type Desc: Send + 'static;
    /// The key identifying a running engine — the folder name. Two `Start`s
    /// with the same key: the later cancels + replaces the earlier.
    fn key(desc: &Self::Desc) -> String;
    /// Build the cancellable per-engine future. Runs on the host's current-thread
    /// runtime; cancelling `cancel` must drop the future and its owned resources.
    fn run(&self, desc: Self::Desc, cancel: CancellationToken) -> EngineFuture;
    /// Optional long-lived side task polled alongside engines for the host's
    /// whole life (e.g. Linux's progress→notification consumer). Default none.
    fn background(&self) -> Option<EngineFuture> {
        None
    }
    /// Called whenever the running-engine count changes (e.g. Linux updates a
    /// `SYNC_RUNNING` flag the UI reads). Default no-op.
    fn on_engines_changed(&self, _running: usize) {}
}

/// Command into the host worker.
pub enum EngineCommand<D> {
    /// Start (or restart, replacing any engine on the same key) one engine.
    ///
    /// A restart is **serialized**: the old engine is cancelled at once, and the
    /// replacement starts only after the old engine's future has ENDED — never
    /// beside it. Two engines on one key are two loops over one state DB and one
    /// directory, and a mode flip is exactly that shape: the outgoing root still
    /// holds its OS binding (a linux FUSE mount over the directory, a cfapi
    /// registration) until its future drops, so a resident engine started beside
    /// it would scan the mounted view instead of the disk (`on-demand-files.md`
    /// § Linux FUSE binding, the flips rule). An engine's start-up passes do not
    /// all watch the token, so the wait is real; a later `Start` for the same key
    /// while one is pending replaces the pending one.
    Start(D),
    /// Stop the engine for `key` (cancel its token, and drop any restart still
    /// pending on it). No-op if none running.
    Stop { key: String },
}

/// Handle to the multi-engine host: owns the worker thread; dropping it cancels
/// every engine (child tokens) and the thread winds down (detached, not joined —
/// joining could block the caller on an in-flight transfer).
pub struct EngineHost<D> {
    /// Cancels every engine future; the worker thread then winds down on its own.
    cancel: CancellationToken,
    /// Live start/stop channel to the worker (see [`EngineCommand`]).
    cmd_tx: UnboundedSender<EngineCommand<D>>,
    /// The worker thread. Detached on drop (cancellation, not join, stops it —
    /// joining could block the caller on an in-flight transfer).
    _thread: Option<thread::JoinHandle<()>>,
}

impl<D: Send + 'static> EngineHost<D> {
    /// Spawn the worker thread with a current-thread tokio runtime and start one
    /// engine per `initial` descriptor. The thread spawns even with zero initial
    /// descriptors (so a later `Start` works with no restart). `spec` is moved
    /// onto the worker thread.
    pub fn start<S: EngineSpec<Desc = D>>(spec: S, initial: Vec<D>) -> Self {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<EngineCommand<D>>();
        let cancel = CancellationToken::new();
        let cancel_worker = cancel.clone();
        let thread = thread::Builder::new()
            .name("fauna-engine-host".into())
            .spawn(move || worker(spec, initial, cancel_worker, cmd_rx))
            .ok();
        Self {
            cancel,
            cmd_tx,
            _thread: thread,
        }
    }

    /// Clone of the live command channel (hand to the UI/IPC layer for live
    /// start/stop).
    pub fn command_sender(&self) -> UnboundedSender<EngineCommand<D>> {
        self.cmd_tx.clone()
    }
}

impl<D> Drop for EngineHost<D> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// One running engine's future, wrapped to yield `(key, generation)` when it
/// ends so a live restart of the same key isn't retired by the stale
/// generation's completion.
type KeyedEngineFuture = Pin<Box<dyn Future<Output = (String, u64)>>>;

/// Worker-thread body: build a current-thread runtime, run every engine future
/// (plus the spec's optional background task) concurrently, and serve live
/// start/stop commands until the host is dropped (cancellation).
fn worker<S: EngineSpec>(
    spec: S,
    initial: Vec<S::Desc>,
    cancel: CancellationToken,
    mut cmd_rx: UnboundedReceiver<EngineCommand<S::Desc>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("could not build engine-host runtime: {e}");
            return;
        }
    };

    rt.block_on(async move {
        // Running engines (polled inline) + their cancel tokens keyed by engine
        // key, each tagged with the generation that started it.
        let mut engines: FuturesUnordered<KeyedEngineFuture> = FuturesUnordered::new();
        // An entry stays until its future ENDS, cancelled or not — a cancelled
        // entry is a retiring engine, and a `Start` on its key waits in `deferred`.
        let mut tokens: HashMap<String, (u64, CancellationToken)> = HashMap::new();
        // Restarts waiting for the engine they replace to end (see
        // `EngineCommand::Start`). At most one per key: the latest wins.
        let mut deferred: HashMap<String, S::Desc> = HashMap::new();
        let mut gen_counter: u64 = 0;

        // The spec's optional long-lived side task, polled alongside engines for
        // the host's whole life. `None` after it completes once (we stop polling
        // it but never break the loop).
        let mut background: Option<EngineFuture> = spec.background();

        // Start one engine for `desc` on a key with no engine. Inline closure-free
        // block so it can mutate `engines` / `tokens` / `gen_counter` without a
        // borrow-checker fight over the opaque future type.
        macro_rules! start_one {
            ($desc:expr) => {{
                let desc: S::Desc = $desc;
                let key = S::key(&desc);
                let token = cancel.child_token();
                gen_counter += 1;
                let generation = gen_counter;
                tokens.insert(key.clone(), (generation, token.clone()));
                let fut = spec.run(desc, token);
                engines.push(Box::pin(async move {
                    // Contain a panicking engine to *itself*. Every engine on this host
                    // shares one current-thread runtime, so without this an unwind out
                    // of any single engine unwinds `block_on` and drops **all** of
                    // them — every sync root torn down at once, and silently, because
                    // the panic goes to a stderr that a detached service has nobody
                    // reading. That is precisely what one overflowing timestamp did on
                    // 2026-07-13 (`fauna_cfapi::unix_to_filetime`): two healthy roots
                    // died for one bad row, and the service stayed "up", syncing
                    // nothing, with not one line in the log.
                    //
                    // Its own teardown still runs correctly — the future's drop guards
                    // (e.g. the cfapi root guard) fire during the unwind — so the
                    // panicking engine retires cleanly and its siblings keep serving.
                    // The `AssertUnwindSafe` is sound for the same reason: the future
                    // owns its resources, and we drop it rather than resume it.
                    if std::panic::AssertUnwindSafe(fut)
                        .catch_unwind()
                        .await
                        .is_err()
                    {
                        tracing::error!(
                            engine = %key,
                            "engine panicked — its root is torn down; other engines keep \
                             running. This is a bug: report it with the backtrace above."
                        );
                    }
                    (key, generation)
                }));
                spec.on_engines_changed(tokens.len());
            }};
        }

        for desc in initial {
            start_one!(desc);
        }
        tracing::info!(
            "engine host ready ({} engine(s), live start/stop enabled)",
            tokens.len()
        );

        loop {
            tokio::select! {
                biased;

                // Host dropped → cancel + wind down (child tokens auto-cancel).
                _ = cancel.cancelled() => break,

                // Live start/stop commands.
                cmd = cmd_rx.recv() => match cmd {
                    Some(EngineCommand::Start(desc)) => {
                        let key = S::key(&desc);
                        match tokens.get(&key) {
                            // An engine (live or retiring) holds the key: cancel
                            // it, and start the replacement when it has ended.
                            Some((_, old)) => {
                                old.cancel();
                                deferred.insert(key, desc);
                            }
                            None => start_one!(desc),
                        }
                    }
                    Some(EngineCommand::Stop { key }) => {
                        deferred.remove(&key);
                        if let Some((_, token)) = tokens.get(&key) {
                            token.cancel();
                        }
                    }
                    None => break, // all senders dropped (host gone)
                },

                // An engine ended (cancelled, natural end, or fatal setup error):
                // retire its map entry (the generation guard is a belt — a key
                // holds one engine at a time now), then start the restart that
                // was waiting for it, if any.
                finished = engines.next(), if !engines.is_empty() => {
                    if let Some((key, generation)) = finished
                        && tokens.get(&key).map(|(g, _)| *g) == Some(generation)
                    {
                        tokens.remove(&key);
                        match deferred.remove(&key) {
                            Some(desc) => start_one!(desc),
                            None => spec.on_engines_changed(tokens.len()),
                        }
                    }
                }

                // The spec's background task, if any. If it ever completes, just
                // stop polling it (clear the slot) — never break the loop.
                _ = poll_background(&mut background) => {
                    background = None;
                }
            }
        }
        spec.on_engines_changed(0);
    });
}

/// Poll the spec's background future if present; pend forever if absent (so the
/// `select!` branch never fires when there is no background task). Returns when
/// the background future completes once.
async fn poll_background(background: &mut Option<EngineFuture>) {
    match background {
        Some(fut) => fut.await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;
    use tokio::sync::mpsc::UnboundedSender as StdUnboundedSender;

    /// Timeout guarding every cross-thread wait so a logic bug fails the test
    /// fast rather than hanging the suite.
    const TIMEOUT: Duration = Duration::from_secs(5);

    /// What a fake engine reports back to the test, over a channel out of the
    /// worker thread.
    #[derive(Debug, PartialEq, Eq)]
    enum Report {
        /// The engine future became live for this key.
        Started(String),
        /// The engine future observed cancellation for this key.
        Cancelled(String),
    }

    /// Descriptor for a fake engine: its key, plus optional control handles so a
    /// test can hold a future open and complete it on demand.
    struct FakeDesc {
        key: String,
        /// If set, the engine future awaits this `Notify` (in addition to
        /// `cancel`) and completes when it fires — lets a test end a *specific*
        /// engine future on demand (used by the generation-guard test).
        complete_on: Option<Arc<Notify>>,
        /// If set, the engine future **panics** once live — models a real engine
        /// hitting an arithmetic overflow (the 2026-07-13 `unix_to_filetime` bug).
        panics: bool,
    }

    impl FakeDesc {
        fn new(key: &str) -> Self {
            Self {
                key: key.to_string(),
                complete_on: None,
                panics: false,
            }
        }
        fn completing_on(key: &str, n: Arc<Notify>) -> Self {
            Self {
                key: key.to_string(),
                complete_on: Some(n),
                panics: false,
            }
        }
        /// An engine that panics as soon as it is polled.
        fn panicking(key: &str) -> Self {
            Self {
                key: key.to_string(),
                complete_on: None,
                panics: true,
            }
        }
    }

    /// A fake spec with no network and no real `SyncEngine`: every engine future
    /// is a pure `select!` over `cancel` and an optional `Notify`, reporting its
    /// lifecycle on a channel the test owns.
    struct FakeSpec {
        reports: StdUnboundedSender<Report>,
        /// Mirrors the running-engine count the host pushes via
        /// `on_engines_changed`, so a test can assert the count without racing.
        running: Arc<AtomicUsize>,
    }

    impl EngineSpec for FakeSpec {
        type Desc = FakeDesc;

        fn key(desc: &Self::Desc) -> String {
            desc.key.clone()
        }

        fn run(&self, desc: Self::Desc, cancel: CancellationToken) -> EngineFuture {
            let reports = self.reports.clone();
            let key = desc.key.clone();
            let complete_on = desc.complete_on.clone();
            let panics = desc.panics;
            Box::pin(async move {
                // Signal the test that this engine future is live.
                let _ = reports.send(Report::Started(key.clone()));
                if panics {
                    panic!("simulated engine panic ({key})");
                }
                match complete_on {
                    // A "stale" engine: deliberately IGNORES its cancellation
                    // token and completes only when the test fires `n`. This
                    // models a future that outlives its own cancellation (a real
                    // engine mid-transfer that hasn't reached its cancel-aware
                    // await yet), which is exactly the case the generation guard
                    // must survive. It reports no cancellation.
                    Some(n) => {
                        n.notified().await;
                    }
                    // A normal engine: completes (and reports) on cancel.
                    None => {
                        cancel.cancelled().await;
                        let _ = reports.send(Report::Cancelled(key));
                    }
                }
            })
        }

        fn on_engines_changed(&self, running: usize) {
            self.running.store(running, Ordering::SeqCst);
        }
    }

    /// Block the current (test) thread until a `Report` arrives or `TIMEOUT`
    /// elapses. Uses the std receiver's `recv_timeout` — no tokio runtime on the
    /// test thread, so this is the deterministic way to observe the worker.
    fn recv_report(rx: &std::sync::mpsc::Receiver<Report>) -> Report {
        rx.recv_timeout(TIMEOUT)
            .expect("expected a report from a fake engine within the timeout")
    }

    /// Bridge: the host worker reports over a tokio unbounded channel (so the
    /// futures can `send` without blocking), but the test thread has no runtime,
    /// so we drain it on a helper thread into a std channel the test can
    /// `recv_timeout` on. Returns `(spec, std_rx, running)`.
    fn fake_spec() -> (
        FakeSpec,
        std::sync::mpsc::Receiver<Report>,
        Arc<AtomicUsize>,
    ) {
        let (tok_tx, mut tok_rx) = tokio::sync::mpsc::unbounded_channel::<Report>();
        let (std_tx, std_rx) = std::sync::mpsc::channel::<Report>();
        // Drain the tokio channel into the std channel on a tiny dedicated
        // runtime thread so the test thread can block on `recv_timeout`.
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("drain runtime");
            rt.block_on(async move {
                while let Some(r) = tok_rx.recv().await {
                    if std_tx.send(r).is_err() {
                        break;
                    }
                }
            });
        });
        let running = Arc::new(AtomicUsize::new(0));
        let spec = FakeSpec {
            reports: tok_tx,
            running: running.clone(),
        };
        (spec, std_rx, running)
    }

    /// Spin until the mirrored running count reaches `want` or `TIMEOUT` — the
    /// count is pushed asynchronously from the worker thread, so we can't read it
    /// synchronously right after sending a command.
    fn wait_for_running(running: &AtomicUsize, want: usize) {
        let start = std::time::Instant::now();
        while running.load(Ordering::SeqCst) != want {
            assert!(
                start.elapsed() < TIMEOUT,
                "running count never reached {want} (stuck at {})",
                running.load(Ordering::SeqCst)
            );
            std::thread::yield_now();
        }
    }

    /// 1. Starting with two initial descriptors makes BOTH engine futures live.
    #[test]
    fn starts_all_initial_engines() {
        let (spec, reports, running) = fake_spec();
        let _host = EngineHost::start(spec, vec![FakeDesc::new("a"), FakeDesc::new("b")]);

        // Two initial engines → two `Started` reports (order nondeterministic).
        let mut started = vec![recv_report(&reports), recv_report(&reports)];
        started.sort_by(|x, y| format!("{x:?}").cmp(&format!("{y:?}")));
        assert_eq!(
            started,
            vec![
                Report::Started("a".to_string()),
                Report::Started("b".to_string())
            ]
        );
        wait_for_running(&running, 2);
    }

    /// 2. `Stop{key:"a"}` cancels ONLY a's future; b stays running.
    #[test]
    fn stop_cancels_only_that_engine() {
        let (spec, reports, running) = fake_spec();
        let host = EngineHost::start(spec, vec![FakeDesc::new("a"), FakeDesc::new("b")]);
        let tx = host.command_sender();

        // Both became live.
        let _ = recv_report(&reports);
        let _ = recv_report(&reports);
        wait_for_running(&running, 2);

        tx.send(EngineCommand::Stop {
            key: "a".to_string(),
        })
        .unwrap();

        // The next report MUST be a's cancellation — b is never told to stop.
        assert_eq!(recv_report(&reports), Report::Cancelled("a".to_string()));
        // And exactly one engine (b) remains.
        wait_for_running(&running, 1);

        // No further report (b is still running, not cancelled) until we drop.
        assert!(
            reports.recv_timeout(Duration::from_millis(200)).is_err(),
            "b should still be running — no extra cancellation expected"
        );
    }

    /// 3. A restart is SERIALIZED: restarting key "a" cancels the first a at once,
    ///    but the second a starts only after the first a's future has ENDED —
    ///    never beside it (two engines on one key are two loops over one state DB
    ///    and directory; a mode flip's outgoing root still holds its mount). The
    ///    first a here ignores its cancel and ends only on `first_done`, modelling
    ///    an engine mid start-up pass.
    #[test]
    fn a_restart_waits_for_the_engine_it_replaces_to_end() {
        let (spec, reports, running) = fake_spec();
        let first_done = Arc::new(Notify::new());
        let host = EngineHost::start(spec, vec![FakeDesc::completing_on("a", first_done.clone())]);
        let tx = host.command_sender();

        assert_eq!(recv_report(&reports), Report::Started("a".to_string()));
        wait_for_running(&running, 1);

        tx.send(EngineCommand::Start(FakeDesc::new("a"))).unwrap();
        assert!(
            reports.recv_timeout(Duration::from_millis(300)).is_err(),
            "the replacement started beside the engine it replaces"
        );

        // The first a ends → the replacement starts.
        first_done.notify_one();
        assert_eq!(recv_report(&reports), Report::Started("a".to_string()));
        wait_for_running(&running, 1);

        // The live engine is the second a: it answers a stop.
        tx.send(EngineCommand::Stop {
            key: "a".to_string(),
        })
        .unwrap();
        assert_eq!(recv_report(&reports), Report::Cancelled("a".to_string()));
        wait_for_running(&running, 0);
    }

    /// 3b. A stop while a restart is pending drops the restart: the key ends
    ///     with no engine at all once the retiring one is gone.
    #[test]
    fn a_stop_drops_a_pending_restart() {
        let (spec, reports, running) = fake_spec();
        let first_done = Arc::new(Notify::new());
        let host = EngineHost::start(spec, vec![FakeDesc::completing_on("a", first_done.clone())]);
        let tx = host.command_sender();
        assert_eq!(recv_report(&reports), Report::Started("a".to_string()));

        tx.send(EngineCommand::Start(FakeDesc::new("a"))).unwrap();
        tx.send(EngineCommand::Stop {
            key: "a".to_string(),
        })
        .unwrap();
        first_done.notify_one();

        wait_for_running(&running, 0);
        assert!(
            reports.recv_timeout(Duration::from_millis(300)).is_err(),
            "a stopped key restarted"
        );
    }

    /// 3c. Two restarts while the old engine retires: only the LATEST starts,
    ///     once.
    #[test]
    fn the_latest_pending_restart_wins() {
        let (spec, reports, running) = fake_spec();
        let first_done = Arc::new(Notify::new());
        let second_done = Arc::new(Notify::new());
        let host = EngineHost::start(spec, vec![FakeDesc::completing_on("a", first_done.clone())]);
        let tx = host.command_sender();
        assert_eq!(recv_report(&reports), Report::Started("a".to_string()));

        // The superseded pending restart would end on `second_done`; the latest
        // is an ordinary cancel-aware engine.
        tx.send(EngineCommand::Start(FakeDesc::completing_on(
            "a",
            second_done.clone(),
        )))
        .unwrap();
        tx.send(EngineCommand::Start(FakeDesc::new("a"))).unwrap();
        first_done.notify_one();

        assert_eq!(recv_report(&reports), Report::Started("a".to_string()));
        wait_for_running(&running, 1);
        assert!(
            reports.recv_timeout(Duration::from_millis(300)).is_err(),
            "more than one replacement started"
        );
        // The one that started is the cancel-aware latest.
        tx.send(EngineCommand::Stop {
            key: "a".to_string(),
        })
        .unwrap();
        assert_eq!(recv_report(&reports), Report::Cancelled("a".to_string()));
        wait_for_running(&running, 0);
    }

    /// A panicking engine must not take the host down with it.
    ///
    /// Every engine shares ONE current-thread runtime, so before per-engine
    /// `catch_unwind` a single panic unwound `block_on` and dropped *all* of them —
    /// every sync root torn down at once, silently (the service is detached, so the
    /// panic message goes nowhere). One overflowing timestamp did exactly this on
    /// 2026-07-13, killing two healthy roots and leaving the service "up" but syncing
    /// nothing.
    ///
    /// So: the panicking engine retires, and every other engine must KEEP SERVING and
    /// the host must keep honouring commands. Load-bearing by construction — without
    /// the containment, the worker thread is dead and none of the assertions below can
    /// ever be reported, so this test fails on the timeout rather than passing.
    #[test]
    fn a_panicking_engine_is_contained_and_its_siblings_keep_running() {
        // The panic is deliberate; keep the default hook from spamming the suite with
        // a backtrace for it.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let (spec, reports, running) = fake_spec();
        let host = EngineHost::start(spec, vec![FakeDesc::panicking("boom"), FakeDesc::new("ok")]);
        let tx = host.command_sender();

        // Both became live (order between them is not deterministic).
        let first = recv_report(&reports);
        let second = recv_report(&reports);
        let mut started = [first, second];
        started.sort_by_key(|r| match r {
            Report::Started(k) | Report::Cancelled(k) => k.clone(),
        });
        assert_eq!(
            started,
            [
                Report::Started("boom".to_string()),
                Report::Started("ok".to_string())
            ]
        );

        // "boom" panicked and retired; "ok" must still be running. If the panic had
        // unwound the shared runtime, the count would collapse to 0 and never recover.
        wait_for_running(&running, 1);

        // The host still serves commands after the panic — start a third engine.
        tx.send(EngineCommand::Start(FakeDesc::new("later")))
            .unwrap();
        assert_eq!(recv_report(&reports), Report::Started("later".to_string()));
        wait_for_running(&running, 2);

        // And the survivors still respond to cancellation.
        tx.send(EngineCommand::Stop {
            key: "ok".to_string(),
        })
        .unwrap();
        assert_eq!(recv_report(&reports), Report::Cancelled("ok".to_string()));
        wait_for_running(&running, 1);

        std::panic::set_hook(prev);
    }
}
