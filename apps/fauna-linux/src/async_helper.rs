//! Helpers for running async work off the GTK main thread and delivering the
//! result back onto it. Two mechanisms, by context:
//!
//! - [`run_on_tokio`] — **pre-client / onboarding.** Spins a *fresh* current-thread
//!   tokio runtime in a worker `std::thread` per call, delivering the result via an
//!   `mpsc` channel polled on the GTK loop. Use it where no shared runtime handle
//!   exists yet (the onboarding flow runs before/around client creation).
//! - [`spawn_with_snapshot`] — **post-login.** Reuses the client's shared tokio
//!   runtime (`client.runtime_handle()`) via `async_channel` + `spawn_future_local`,
//!   so there is no per-call thread/runtime spawn. Use it on every post-login page
//!   that drives a shared-Rust state machine (settings, profile, feed, backups, …):
//!   `spawn_with_snapshot(&rt, produce, render)` replaces the hand-rolled
//!   `bounded(1)` + `rt.spawn` + `spawn_future_local` ritual.
//!
//! And one for the push direction: a shared-Rust manager's snapshot observer
//! wakes a GTK repaint through [`snapshot_wake_channel`] + [`spawn_wake_loop`] —
//! the only shape for it in this app. Its wakes coalesce and every repaint hands
//! the thread back to the main loop, so a burst of notifications can neither
//! repaint once per notification nor hold the thread for the whole burst. What
//! it cannot do is outrun a DEFAULT-priority source that never yields — see
//! [`spawn_wake_loop`] on why idle priority made that worse, not better.
//!
//! **Neither mechanism is optional, and the ritual is not ceremony: this process
//! holds no tokio enter-guard on the GTK main thread.** A future polled by
//! `glib::spawn_future_local` therefore runs with *no tokio runtime in scope*,
//! and any tokio-bound await inside it panics the task — `NestClient::request`
//! (every WS-RPC call wraps itself in `tokio::time::timeout`) and
//! [`hydrate_with_retry`]'s retry sleep (`fauna_sleep::sleep`, `tokio::time::sleep`
//! on this native target) both qualify. The panic kills
//! only that task, so the symptom is not a crash but a **render that silently
//! never happens** — a page that stays empty with no error, which reads as
//! "the server returned nothing". The leg-(d) Backups repoint lost a day to
//! exactly this (`views/backups/destinations.rs::refresh`, fixed 2026-07-24).
//! The rule: inside a `spawn_future_local`, await *only* the `async_channel`
//! receive; all real I/O belongs in the `produce` closure, which runs on the
//! tokio runtime.
//!
//! **Second rule, same thread, opposite direction: never build a tokio runtime
//! on the GTK main thread.** Not `Builder::new_current_thread().build()`, not
//! `Runtime::new()` — not even for "one quick round trip". Two costs, and the
//! second is the one that bites: `block_on` stalls the main loop for the whole
//! future, and then `Runtime::drop` stalls it *again* for as long as the
//! blocking pool takes to join (tokio resolves DNS on `spawn_blocking`
//! `getaddrinfo`, which answers to no timeout of ours). `FaunaClient` already
//! documents the drop half in its own `Drop` — it calls `shutdown_background()`
//! precisely because "the default `Runtime::drop()` would block the calling
//! thread waiting for tasks that never finish".
//!
//! The e2e agent drains its commands from a `glib::timeout_add_local` tick on
//! that same main thread, so a stall there is not merely slow — it makes the app
//! unable to answer *any* command, and the failure surfaces as
//! `agent timeout — the UI thread did not reply within 25s`, i.e. wearing a
//! product bug's clothes (e2e-conventions.md § point 13's windows case study,
//! which cost seven sessions there). Three launch paths grew this independently
//! — the post-wizard launch, the account switch, and the `set_state` relaunch —
//! and all three are now routed through the helpers here. The cold-launch path in `main.rs` never had it:
//! it has always used [`run_on_tokio`], and it is the shape to copy.
//!
//! Both helpers below therefore build **and drop** their runtime on a worker
//! thread, and send the result *before* dropping, so neither half of the stall
//! can reach the main loop; [`drop_runtime_loudly`] logs the drop when it is
//! slow enough to have mattered. A caller that genuinely needs the GTK thread to
//! wait for a result uses [`block_on_tokio`] — it parks on a channel, never on a
//! runtime it owns.
//!
//! **Both rules are enforced, not merely stated.** `glib-spawn-await-check`
//! (a dedicated dev-fleet checker) fails on any awaited expression here that
//! is not a channel receive; `gtk-thread-tokio-check` (another dedicated
//! checker) fails on a runtime built where its `block_on` and `Drop` can
//! land on the main thread. Both are parse-only and
//! on the cheap merge tier for any merge touching this app. They exist
//! because prose could not hold either one: the first rule was written the day the
//! Backups red was fixed, and the heads-up row that recorded that red still named
//! the wrong suspect three weeks later; the second was stated here on 2026-08-16
//! and *five* libsecret wrappers in `client.rs` were still building their runtime
//! on the calling thread when the gate was written, all five reached from GTK handlers. A defect whose only symptom is a
//! *missing* render — or an app that has simply stopped answering — has no witness
//! unless something structural looks for it.
//!
//! The second gate keys on *where* a construction can run, not on that it happens:
//! `#[cfg(test)]` bodies, `std::thread::spawn` / `thread::Builder…spawn` closures
//! and this file are allowed, and a site that is genuinely off the main thread but
//! whose spawn sits in its caller carries an explicit `// gtk-runtime-ok: <reason>`
//! (two in the tree today, both in `client.rs`).

use gtk::glib;
use std::sync::mpsc;

/// A per-call runtime is cheap to build and usually cheap to drop — but
/// `Runtime::drop` is **not** cancel-and-return: it joins the blocking pool, so a
/// still-running blocking task (tokio resolves DNS via `spawn_blocking`
/// `getaddrinfo`, which has no timeout of its own) holds the dropping thread for
/// as long as that task takes. That is the hazard `FaunaClient` already names in
/// its own `Drop` (`client.rs` — it uses `shutdown_background()` precisely
/// because "the default `Runtime::drop()` would block the calling thread waiting
/// for tasks that never finish").
///
/// Both helpers here drop their runtime on the *worker* thread, after the result
/// has been sent, so the stall can never reach the GTK main loop. It is still
/// worth a line when it happens: a multi-second drop means a blocking task ran
/// long past the work we were waiting for, and that is the signature to look for
/// the next time a launch path feels slow. Below the threshold this is silent.
fn drop_runtime_loudly(
    rt: tokio::runtime::Runtime,
    caller: &'static std::panic::Location<'static>,
) {
    let t = std::time::Instant::now();
    drop(rt);
    let ms = t.elapsed().as_millis() as u64;
    if ms >= 1_000 {
        tracing::warn!(
            drop_ms = ms,
            caller = %caller,
            "[async-helper] tokio runtime drop blocked its worker thread — a blocking \
             task (usually a DNS `getaddrinfo`) outlived the work; harmless here, but it \
             would have frozen the GTK main loop had this runtime been built on it"
        );
    }
}

/// Run an async block on a fresh current-thread tokio runtime in a worker
/// thread, then invoke `on_done` on the GTK main loop with the result.
///
/// Pre-client / onboarding mechanism — see the module docs. Post-login callers
/// that already hold a `tokio::runtime::Handle` should use [`spawn_with_snapshot`]
/// instead (it reuses the shared runtime rather than spawning a new one per call).
#[track_caller]
pub fn run_on_tokio<T, F, D>(work: F, on_done: D)
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
    D: FnOnce(T) + 'static,
{
    let caller = std::panic::Location::caller();
    let (tx, rx) = mpsc::channel::<T>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let t_work = std::time::Instant::now();
        let out = rt.block_on(work);
        tracing::debug!(
            work_ms = t_work.elapsed().as_millis() as u64,
            caller = %caller,
            "[async-helper] run_on_tokio work completed"
        );
        // Send BEFORE dropping the runtime: the GTK continuation must not wait
        // on a drop that may join a long-running blocking task.
        let _ = tx.send(out);
        drop_runtime_loudly(rt, caller);
    });
    let on_done = std::cell::RefCell::new(Some(on_done));
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        match rx.try_recv() {
            Ok(v) => {
                if let Some(cb) = on_done.borrow_mut().take() {
                    let source = crate::main_loop_meter::site_source("run-on-tokio-done", caller);
                    crate::main_loop_meter::dispatch(source, String::new, || cb(v));
                }
                glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(_) => glib::ControlFlow::Break,
        }
    });
}

/// Like [`run_on_tokio`] but **blocks the calling thread** until the async
/// `work` completes, returning its output.
///
/// Used by the e2e `"machine"` bridge to drive an *async* onboarding-machine
/// method (`verify_dns`) to completion before the command is acked: the shared
/// `OnboardingMachine::call_machine_method` dispatcher is sync, so it can't
/// `.await`, and the Python driver's `call_machine_method("verify_dns")` must
/// return only once the verified snapshot is in place (parity with web, whose
/// `__fauna_callMachineMethod` awaits the promise). The work runs on a fresh
/// current-thread runtime in a worker thread — so the machine observer fires
/// *off* the GTK main thread exactly as it does under [`run_on_tokio`] — and
/// the caller blocks on the result channel. The observer's `on_changed`
/// `try_send`s on an unbounded `async-channel`, so blocking the GTK thread here
/// cannot deadlock; the coalesced re-render runs once this returns.
///
/// Only safe where a brief GTK-thread stall is acceptable (a single local HTTP
/// round-trip to the e2e `fake_cloud`). Long-running work (provisioning) must
/// stay on the non-blocking [`run_on_tokio`].
#[track_caller]
pub fn block_on_tokio<T, F>(work: F) -> T
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let caller = std::panic::Location::caller();
    let (tx, rx) = mpsc::channel::<T>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let out = rt.block_on(work);
        // Send BEFORE dropping: the caller (the GTK thread) is parked on
        // `rx.recv()`, so a slow runtime drop here would extend its stall by
        // exactly the drop's duration for no reason.
        let _ = tx.send(out);
        drop_runtime_loudly(rt, caller);
    });
    rx.recv().expect("block_on_tokio worker thread panicked")
}

/// Run `produce` on the shared tokio runtime `rt`, then deliver its output to
/// `render` on the GTK main thread.
///
/// Post-login mechanism — see the module docs. The closure `produce` runs on the
/// runtime (so it and its future are `Send`); `render` runs on the GTK main loop
/// (so it is *not* `Send`, and may capture GTK widgets / an `Rc` context freely).
/// A typical call:
///
/// ```ignore
/// let machine = Arc::clone(&ctx.machine);
/// let ctx = Rc::clone(&ctx);
/// spawn_with_snapshot(
///     &ctx.rt,
///     move || async move {
///         let _ = machine.dispatch(action).await;
///         machine.snapshot()
///     },
///     move |snap| render(&ctx, &snap),
/// );
/// ```
#[track_caller]
pub fn spawn_with_snapshot<S, Fut, P, R>(rt: &tokio::runtime::Handle, produce: P, render: R)
where
    S: Send + 'static,
    Fut: std::future::Future<Output = S> + Send + 'static,
    P: FnOnce() -> Fut + Send + 'static,
    R: FnOnce(S) + 'static,
{
    let source =
        crate::main_loop_meter::site_source("snapshot-render", std::panic::Location::caller());
    let (tx, rx) = async_channel::bounded::<S>(1);
    rt.spawn(async move {
        let _ = tx.send(produce().await).await;
    });
    glib::spawn_future_local(async move {
        if let Ok(snap) = rx.recv().await {
            crate::main_loop_meter::dispatch(source, String::new, || render(snap));
        }
    });
}

/// The wake channel behind a shared-Rust snapshot observer
/// (`FeedSnapshotObserver`, `SnapshotObserver`, `SearchSnapshotObserver`, …).
/// The observer `try_send`s one `()` per `notify()`, from whichever thread the
/// mutation ran on; [`spawn_wake_loop`] consumes the wakes on the GTK main
/// thread.
///
/// **Capacity one, on purpose: pending wakes COALESCE.** Every consumer re-reads
/// the whole snapshot, so any number of notifications that land before it runs
/// owe it one pass, not one each. A `try_send` into a full channel is a wake
/// already owed, and dropping it loses nothing: the pending wake is received,
/// and the snapshot read, *after* the dropped one was sent. Every observer here
/// used to build an **unbounded** channel while its comment said "coalescing
/// duplicates is safe", so each `notify()` was a full repaint of its own — a feed
/// page whose cards each resolve an embed notified once per card and was rebuilt
/// once per card, every card each time.
pub fn snapshot_wake_channel() -> (async_channel::Sender<()>, async_channel::Receiver<()>) {
    async_channel::bounded(1)
}

/// Run `step` on the GTK main thread once per coalesced wake from a
/// [`snapshot_wake_channel`], until `step` answers [`glib::ControlFlow::Break`]
/// or every sender is dropped. The one shape for every snapshot observer's
/// repaint loop in this app.
///
/// **Each step ends by handing the thread back to the main loop**
/// ([`YieldToMainLoop`]). A receive on a channel that already holds a wake
/// completes without suspending, so a loop that awaited only the receive ran
/// step after step inside ONE main-loop dispatch for as long as wakes kept
/// landing — and they land during the step itself (a painted feed card spawns
/// the embed resolve whose completion notifies). Nothing else on the thread ran
/// meanwhile: not input, not the frame clock, not the e2e agent's op drain or
/// its heartbeat. The 2026-09-15b whole-suite sweep's two feed 504s were exactly
/// that (`agent timeout … the UI thread's main loop last ran 26.0s ago`, the
/// stall stack in `render_posts → build_post_card`); convention 11 in
/// `e2e-conventions.md` is the rule it broke.
///
/// **The loop stays at DEFAULT priority; idle priority is REFUTED by
/// measurement.** The first whole-suite sweep with the yield (2026-09-19)
/// still showed starvation
/// one level down: six `barrier` timeouts (the agent acks that from an idle
/// callback) and confirm dialogs that never became visible (the frame clock,
/// priority 120, is what maps them), with the heartbeat beating throughout.
/// Moving this loop to `DEFAULT_IDLE` so it could never outrank them made the
/// same sweep **worse — 81 failures against 48** — and traded those for
/// repaints that never happened at all (`conversation-item[0] never rendered …
/// even though list_threads() state already reports it`).
///
/// Both directions failing meant the starvation was NOT this loop's priority:
/// something else held DEFAULT continuously. Measured 2026-09-21
/// (`crate::main_loop_meter`), it was the e2e agent's 50 ms state publish —
/// the shared conversations serializer deep-cloned every message of every
/// thread on each tick, and one ~3 MiB mail made every tick cost ~230 ms, so
/// the tick was always due again — not the UI pump the 2026-09-19 reading
/// suspected. Beneath DEFAULT a repaint waits
/// behind all of that work; level with it, a repaint takes its turn.
///
/// A caller that wants a first pass before any wake (an empty state painted at
/// build time) calls its `step` once itself, then hands it over.
#[track_caller]
pub fn spawn_wake_loop(
    rx: async_channel::Receiver<()>,
    mut step: impl FnMut() -> glib::ControlFlow + 'static,
) {
    let source = crate::main_loop_meter::site_source("wake-loop", std::panic::Location::caller());
    glib::MainContext::ref_thread_default().spawn_local(async move {
        while rx.recv().await.is_ok() {
            let flow = crate::main_loop_meter::dispatch(source, String::new, &mut step);
            if flow == glib::ControlFlow::Break {
                break;
            }
            YieldToMainLoop::default().await;
        }
    });
}

/// One `Pending` that wakes itself at once: the enclosing glib task goes back
/// to the main loop, which dispatches every other source ready at the task's
/// priority or above before polling it again (for [`spawn_wake_loop`]'s
/// DEFAULT-priority task: input, the e2e agent's op drain and its tick). Needs
/// no runtime of any kind, so it is safe inside a
/// glib task — unlike a tokio yield, which would panic there (module docs).
#[derive(Default)]
struct YieldToMainLoop {
    yielded: bool,
}

impl std::future::Future for YieldToMainLoop {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.yielded {
            return std::task::Poll::Ready(());
        }
        self.yielded = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

/// Retry an async read up to 10 times (500 ms apart) until it returns `Ok`,
/// then yield its final result.
///
/// ⚠ **Do NOT reach for this to cover the post-login connect race — the
/// transport already does, for every read that goes through `NestClient`.**
/// This helper's doc used to teach exactly that ("the socket comes up shortly
/// after login, so wrap the first read"), and it was wrong from the day it was
/// written: `request_inner` has parked not-yet-connected reads since 2026-05-25, waiting out the kind's whole deadline budget rather than
/// answering a spurious `RpcDisconnected`. Wrapping such a read here does not
/// make it more tolerant, it multiplies the budget — 11 attempts × a 30 s
/// deadline each. The lead app (tui) calls `machine.hydrate()` bare, which is
/// the shape to copy; pinned by `fauna-client-mail-settings`'s
/// `hydrate_waits_for_socket` and `fauna-client`'s
/// `request_issued_while_disconnected_waits_for_reconnect`.
///
/// **What it is still for:** an op whose failure the transport genuinely cannot
/// see — a read that is not a single `NestClient` RPC (a local-store load, a
/// composite that touches disk first), or one racing nest-side *readiness*
/// rather than socket readiness, where the nest is connected and answering but
/// has not yet provisioned what the read asks for. That is a `Rejected`, not a
/// transport fault, and only a retry at this layer clears it.
///
/// Pass a closure that re-issues the read on each attempt: callers that only
/// need the side effect (the machine caches the loaded state internally) can
/// discard the returned `Result` and read `machine.snapshot()` afterward;
/// callers that need the value match on it directly. Runs on whatever tokio
/// runtime the caller is already on — typically the shared post-login runtime
/// inside a [`spawn_with_snapshot`] `produce` closure.
///
/// Delegates the loop+sleep mechanics to `fauna_sleep::retry` (the shared home
/// for this exact pattern — `fauna-sync-engine`'s `engine_lifecycle` tolerates
/// the same warm-up race with two hand-rolled copies of it). `11` preserves
/// this function's original call/sleep counts unchanged: one initial attempt
/// plus up to 10 retries, 10 sleeps, ~5s worst-case budget.
pub async fn hydrate_with_retry<T, E, Fut, F>(op: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    fauna_sleep::retry(11, std::time::Duration::from_millis(500), op).await
}

#[cfg(test)]
mod wake_loop_tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    /// Pump `ctx` until nothing is ready.
    fn drain(ctx: &glib::MainContext) {
        while ctx.iteration(false) {}
    }

    /// Fifty notifications that land before the loop runs owe it ONE pass —
    /// the step re-reads the whole snapshot. An unbounded channel owed fifty.
    #[test]
    fn a_burst_of_wakes_owes_one_step_not_one_each() {
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let (tx, rx) = snapshot_wake_channel();
            for _ in 0..50 {
                let _ = tx.try_send(());
            }
            let steps = Rc::new(Cell::new(0u32));
            let counted = Rc::clone(&steps);
            spawn_wake_loop(rx, move || {
                counted.set(counted.get() + 1);
                glib::ControlFlow::Continue
            });
            drain(&ctx);
            assert_eq!(
                steps.get(),
                1,
                "50 queued wakes must coalesce into one step"
            );

            // A wake sent after that step is still honoured — coalescing drops
            // only wakes that an already-pending one covers.
            let _ = tx.try_send(());
            drain(&ctx);
            assert_eq!(steps.get(), 2, "a wake after the step owes a second step");
        })
        .expect("thread-default context");
    }

    /// A step that is woken again while it runs — a painted feed card whose
    /// embed resolve notifies mid-render — must still hand the thread back
    /// between two steps. The witness is a DEFAULT-priority source ready on
    /// every iteration, the shape of the e2e agent's heartbeat
    /// (`main.rs`'s `timeout_add_local`): it must run between any two steps.
    /// Before the yield, all twenty steps ran inside one dispatch and the
    /// heartbeat ran only after the last.
    #[test]
    fn every_step_hands_the_thread_back_to_the_main_loop() {
        const STEPS: u32 = 20;
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let log: Arc<Mutex<Vec<char>>> = Arc::new(Mutex::new(Vec::new()));
            let beat_log = Arc::clone(&log);
            glib::timeout_source_new(
                std::time::Duration::ZERO,
                None,
                glib::Priority::DEFAULT,
                move || {
                    beat_log.lock().unwrap().push('B');
                    glib::ControlFlow::Continue
                },
            )
            .attach(Some(&ctx));

            let (tx, rx) = snapshot_wake_channel();
            let _ = tx.try_send(());
            let steps = Rc::new(Cell::new(0u32));
            let counted = Rc::clone(&steps);
            let step_log = Arc::clone(&log);
            spawn_wake_loop(rx, move || {
                counted.set(counted.get() + 1);
                step_log.lock().unwrap().push('S');
                if counted.get() < STEPS {
                    let _ = tx.try_send(());
                }
                glib::ControlFlow::Continue
            });

            // The heartbeat is always ready, so the context never drains; pump
            // until every owed step has run (bounded, never a wait on a clock).
            let mut iterations = 0;
            while steps.get() < STEPS && iterations < 10_000 {
                ctx.iteration(false);
                iterations += 1;
            }
            assert_eq!(steps.get(), STEPS, "every re-wake must be honoured");

            let log = log.lock().unwrap();
            let back_to_back = log.windows(2).filter(|w| w == &['S', 'S']).count();
            assert_eq!(
                back_to_back,
                0,
                "two steps ran with no heartbeat between them: {}",
                log.iter().collect::<String>()
            );
        })
        .expect("thread-default context");
    }

    /// `Break` ends the loop: later wakes run nothing.
    #[test]
    fn break_ends_the_loop() {
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let (tx, rx) = snapshot_wake_channel();
            let steps = Rc::new(Cell::new(0u32));
            let counted = Rc::clone(&steps);
            spawn_wake_loop(rx, move || {
                counted.set(counted.get() + 1);
                glib::ControlFlow::Break
            });
            let _ = tx.try_send(());
            drain(&ctx);
            let _ = tx.try_send(());
            drain(&ctx);
            assert_eq!(steps.get(), 1);
        })
        .expect("thread-default context");
    }
}
