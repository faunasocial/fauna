//! The pass driver: pump work — a pass, a nudge walk, the publish step — run
//! **beside the command channel**, so a local command is served at the
//! pass's next yield point and a pass-bound one parked behind it; a sign-out
//! cuts the pass after a grace; a panic is contained
//! (`account-data-plane.md` § The client-side lifecycle, the pump bullet →
//! *Commands and passes*).

use std::collections::VecDeque;
use std::task::Poll;
use std::time::Duration;

use fauna_account_store::backend::StoreBackend;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use futures_util::FutureExt;
use tokio::sync::{mpsc, watch};

use super::handle::{Cmd, LocalCtx, PumpCycles, Served, serve_local_cmd};
use super::handle_source::{NotReadyReason, ScopeNotReady};
use super::now_ms;
use super::pass::PumpReport;

/// How long a pass already running when a sign-out arrives may go on before
/// the sign-out cuts it ([`AccountStoreHandle::shutdown_for_sign_out`]).
///
/// Commands are served between passes and the retirement is a command, so a
/// sign-out that merely queued behind the pass it landed in waited as long as
/// that pass ran. A fresh sign-in's prologue recovers every generation key the
/// account ever minted from escrow — 50 to 70 of them by the middle of a
/// whole-suite e2e sweep, and five seconds and more — so the hosts' stop
/// budget lapsed, the erase took the writer key with its grant still live, and
/// nothing could ever retire the machine's grant
/// (`apps/sync-agent-credentials.md` § Implementation status today). So the
/// retirement never waits on a pass beyond this grace: a pass still running
/// when it ends is dropped where it stands. That is a crash at an await point,
/// which the store survives by construction (a process can be killed at any of
/// them), and whatever the pass would have written is erased with the account
/// anyway. The grace keeps the ordinary case whole: a sign-out landing near the
/// end of a pass still lets it drain its outbox before the retirement ends the
/// principal's sessions. Grace + [`ENROLLMENT_RETIRE_BUDGET`] fits inside the
/// hosts' stop budget with room for the store's own shutdown — checked at
/// compile time where that budget lives
/// (`fauna_client_account_runtime::ACCOUNT_RUNTIME_STOP_BUDGET`).
pub const SIGN_OUT_PASS_GRACE: Duration = Duration::from_secs(1);

/// The store loop's view of a sign-out's claim, raised by
/// [`AccountStoreHandle::shutdown_for_sign_out`] before it queues the
/// retirement. Every pass, and every nudge walk, runs through
/// [`drive_pass`], whose cut arm is [`Self::grace_lapsed`] — the loop's pump
/// work has no other way in.
pub(crate) struct SignOutWatch(pub(crate) watch::Receiver<Option<u64>>);

impl SignOutWatch {
    /// Resolves once the grace after a sign-out request has run out; never
    /// while no sign-out has been requested.
    pub(crate) async fn grace_lapsed(&self) {
        let mut rx = self.0.clone();
        let requested = match rx.wait_for(Option::is_some).await {
            Ok(requested) => *requested,
            // Every handle is gone: no sign-out can arrive any more.
            Err(_) => None,
        };
        let Some(requested) = requested else {
            return std::future::pending().await;
        };
        // Wall-clock millis on both sides (`now_ms`), and the cross-target
        // sleep: the grace is a best-effort bound, not a timing assumption.
        let cut = requested.saturating_add(SIGN_OUT_PASS_GRACE.as_millis() as u64);
        fauna_sleep::sleep(Duration::from_millis(cut.saturating_sub(now_ms()))).await;
    }
}

/// The pass driver's command side: the channel a pass is driven beside, the
/// parking lot for the pass-bound commands that arrive while it runs, and
/// what a local one may touch.
pub(crate) struct Drive<'a, B: StoreBackend, R: RpcRequester> {
    pub(crate) cmd_rx: &'a mut mpsc::Receiver<Cmd>,
    pub(crate) parked: &'a mut VecDeque<Cmd>,
    pub(crate) local: LocalCtx<'a, B, R>,
    /// Where a run that changed the store moves the change generation.
    pub(crate) cycles: &'a PumpCycles,
    /// How long a tip-sealed put parked behind the pass waits for a tip, and
    /// an awaited publish parked behind it waits for the pass's end
    /// (`DriverConfig::tip_wait`).
    pub(crate) tip_wait: Duration,
}

/// How [`drive_pass`] ended.
pub(crate) enum Driven<T> {
    Done(T),
    /// The pass panicked — contained, logged, the thread lives.
    Panicked,
    /// A sign-out cut the pass ([`SIGN_OUT_PASS_GRACE`]) — logged.
    Cut,
    /// A local command served inside the pass met a rotated writer: the
    /// pass was dropped where it stood and the caller reassembles.
    Reassemble,
}

/// [`contained_pump`]'s `Err`: the reassembly a local command demanded
/// mid-pass (its `Ok` is the report — a panicked or cut pass reports as
/// such). A marker type rather than a two-variant enum: `PumpReport` is
/// large, and a `Result` over it is what the loop reads anyway.
pub(crate) struct Reassemble;

/// Drive `fut` — pump work: a pass, a nudge walk, the publish step — beside
/// the command channel (`account-data-plane.md` § The client-side lifecycle,
/// the pump bullet → *Commands and passes*). At every yield point of `fut` a
/// local command that has arrived is served on this same thread and
/// connection, and a pass-bound one is parked for the loop to serve after;
/// a sign-out cuts `fut` where it stands [`SIGN_OUT_PASS_GRACE`] after it was
/// requested; a panic in `fut` is contained. Every unit of local work inside
/// pump work ends in a yield (`pass_breath`), so nothing here waits longer
/// than one unit.
///
/// A closed channel (every handle dropped) stops the polling of it and lets
/// `fut` finish: the loop finds the close on its next wait, exactly as it
/// did when a pass ran to completion before the loop looked.
///
/// **Every run of the pump passes through here** — [`contained_pump`]'s
/// passes, publish steps and seed passes, and the nudge's walk — so this is
/// where the run is measured for the change generation (`account-runtime.md`
/// § Multi-instance concurrency → *A runtime's own pump is a source of
/// the notice too*, parts 1–2): the store's entry-change count is read
/// before and after, the local commands served inside the run are measured
/// on their own and taken out ([`serve_counted`]: a gesture's own write is
/// not a source, and a background write is its own source, counted there),
/// and a run that changed anything moves the generation once, at its end,
/// however it ended (a cut or panicked run's writes landed too).
pub(crate) async fn drive_pass<B, R, F>(
    label: &str,
    sign_out: &SignOutWatch,
    drive: &mut Drive<'_, B, R>,
    fut: F,
) -> Driven<F::Output>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    F: std::future::Future,
{
    let before = drive.local.store.entry_changes();
    let mut gestures = 0u64;
    let driven = drive_measured(label, sign_out, drive, fut, &mut gestures).await;
    let run = drive
        .local
        .store
        .entry_changes()
        .wrapping_sub(before)
        .saturating_sub(gestures);
    if run > 0 {
        drive.cycles.changed();
    }
    driven
}

/// [`drive_pass`]'s body; `gestures` accumulates what the local commands
/// served inside the run changed.
///
/// **The tip wait** (`account-client-lifecycle.md` § The client-side
/// lifecycle, the pump bullet → *Commands and passes*, ruled 2026-10-27): a
/// tip-sealed door put parked here ([`Served::Park`]) waits for a TIP, not
/// for the pass — the park's one reason is the mint. While one is parked the
/// pass's breaths ([`crate::pass_breath`]) surface as a select arm
/// ([`pass_step`]), and at each the parked puts are re-asked in arrival
/// order: one whose door now admits it is served there, as the local command
/// it then is. The wait is bounded by [`Drive::tip_wait`]
/// (`FIRST_PASS_WAIT`): a put still parked at the bound answers
/// [`NotReadyReason::TipPending`] — never minted inside the pass, never
/// written without a tip. A put still parked when the run ends joins the
/// loop's parking lot in arrival order and is served there, minting as
/// before.
///
/// **The awaited publish's bound** (same section, the awaited publish's
/// bound, ruled 2026-11-02): an awaited publish (`Cmd::PublishOwn`) parked
/// here stays pass-bound — it is the pass's own publish step, one publisher
/// of each plane's ordered log per process, and a network leg, so it is
/// never served at a breath — but its park takes the same bound: still
/// parked at [`Drive::tip_wait`] it answers [`NotReadyReason::PublishPending`]
/// unserved, nothing sent, its rows owed to the runtime's next publish. A
/// publish still parked when the run ends is served after the pass as
/// before. Breaths are not surfaced for it (there is nothing to re-ask).
async fn drive_measured<B, R, F>(
    label: &str,
    sign_out: &SignOutWatch,
    drive: &mut Drive<'_, B, R>,
    fut: F,
    gestures: &mut u64,
) -> Driven<F::Output>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    F: std::future::Future,
{
    let mut fut = std::pin::pin!(std::panic::AssertUnwindSafe(fut).catch_unwind());
    let mut closed = false;
    let mut parked_writes = ParkedWrites::new();
    // Everything this run parked, in arrival order: a tip-sealed put stays
    // re-askable until the run ends, when the lot joins `drive.parked`.
    let mut lot: VecDeque<Parked> = VecDeque::new();
    // Re-armed whenever the oldest bounded park changes; polled only while a
    // put or a publish is parked.
    let mut bound = std::pin::pin!(fauna_sleep::sleep(drive.tip_wait));
    let breath_owed = std::cell::Cell::new(false);
    let ended = loop {
        // Breaths surface only for a parked put (the one park re-asked at
        // them); the bound arms for a parked put or a parked publish.
        let surface = lot.iter().any(Parked::is_put);
        let bounded = lot.iter().any(Parked::is_bounded);
        tokio::select! {
            biased;

            step = pass_step(fut.as_mut(), surface, &breath_owed) => match step {
                Step::Out(Ok(out)) => {
                    parked_writes.pass_ended(label, "done");
                    break Driven::Done(out);
                }
                Step::Out(Err(_)) => {
                    tracing::error!("account pump ({label}): panicked — contained, thread lives");
                    parked_writes.pass_ended(label, "panicked");
                    break Driven::Panicked;
                }
                Step::Breath => {
                    let oldest = oldest_bounded(&lot);
                    if reask_parked(label, drive, &mut lot, gestures).await {
                        parked_writes.pass_ended(label, "dropped for a reassembly");
                        break Driven::Reassemble;
                    }
                    if oldest_bounded(&lot) != oldest
                        && let Some(left) = time_left(&lot, drive.tip_wait)
                    {
                        bound.set(fauna_sleep::sleep(left));
                    }
                }
            },
            () = sign_out.grace_lapsed() => {
                tracing::info!(
                    "account pump ({label}): cut by a sign-out — its enrollment retirement \
                     never waits on a pass"
                );
                parked_writes.pass_ended(label, "cut by a sign-out");
                break Driven::Cut;
            }
            () = &mut bound, if bounded => {
                refuse_at_bound(label, drive, &mut lot);
                if let Some(left) = time_left(&lot, drive.tip_wait) {
                    bound.set(fauna_sleep::sleep(left));
                }
            }
            cmd = drive.cmd_rx.recv(), if !closed => match cmd {
                None => closed = true,
                Some(cmd) if cmd.is_local() => {
                    let served = serve_counted(cmd, &mut drive.local, drive.cycles, gestures).await;
                    match served {
                        Served::Done => {}
                        Served::Reassemble => {
                            parked_writes.pass_ended(label, "dropped for a reassembly");
                            break Driven::Reassemble;
                        }
                        Served::Park(cmd) => {
                            parked_writes.park(label, &cmd, drive.tip_wait);
                            if !bounded {
                                bound.set(fauna_sleep::sleep(drive.tip_wait));
                            }
                            lot.push_back(Parked::Put {
                                cmd,
                                at: now_ms(),
                                asks: 0,
                            });
                        }
                    }
                }
                // The awaited publish: pass-bound, served after the pass as
                // every pass-bound command is, but parked under the bound.
                Some(cmd @ Cmd::PublishOwn { .. }) => {
                    parked_writes.park_publish(label, drive.tip_wait);
                    if !bounded {
                        bound.set(fauna_sleep::sleep(drive.tip_wait));
                    }
                    lot.push_back(Parked::Publish { cmd, at: now_ms() });
                }
                Some(cmd) => lot.push_back(Parked::PassBound(cmd)),
            },
        }
    };
    drive.parked.extend(lot.into_iter().map(Parked::into_cmd));
    ended
}

/// What one poll of the pass came to, as [`drive_measured`]'s loop sees it.
enum Step<T> {
    Out(T),
    /// The pass took a breath and is pending on it — the parked puts'
    /// re-ask point.
    Breath,
}

/// Poll the pass: its output when it is done; with `surface` (a put is
/// parked), [`Step::Breath`] for a breath it took
/// ([`crate::pass_breath::pass_breath`]). A pass pending on anything else
/// (the network) stays pending, so nothing here spins; with nothing parked
/// this is a plain pass-through.
///
/// A breath is answered on the poll AFTER the one that saw it: that poll
/// answers `Pending` and marks the breath `owed`, so the select's other arms
/// — a sign-out's cut, a new command, the bound — are looked at first, as
/// they are at any breath with nothing parked; a pass grinding breath after
/// breath would otherwise win every round of the biased select. The breath
/// woke the task itself, so the next poll comes at once.
fn pass_step<'a, F: std::future::Future>(
    mut pass: std::pin::Pin<&'a mut F>,
    surface: bool,
    owed: &'a std::cell::Cell<bool>,
) -> impl std::future::Future<Output = Step<F::Output>> + 'a {
    std::future::poll_fn(move |cx| {
        if !surface {
            owed.set(false);
        } else if owed.take() {
            return Poll::Ready(Step::Breath);
        }
        let before = crate::pass_breath::breaths_taken();
        match pass.as_mut().poll(cx) {
            Poll::Ready(out) => Poll::Ready(Step::Out(out)),
            Poll::Pending => {
                if surface && crate::pass_breath::breaths_taken() != before {
                    owed.set(true);
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
        }
    })
}

/// One command a run parked: a pass-bound one, served after the pass; a
/// tip-sealed put, re-asked at every breath until a tip resolves; or an
/// awaited publish, pass-bound but parked under the bound.
enum Parked {
    PassBound(Cmd),
    /// `at`: when it parked, wall-clock millis ([`now_ms`]); `asks`: the
    /// breaths it has been re-asked at — the wait's cost, logged at its end.
    Put {
        cmd: Cmd,
        at: u64,
        asks: u32,
    },
    /// An awaited publish (`Cmd::PublishOwn`): never re-asked (it is the
    /// pass's own publish step, served only after the pass), refused at the
    /// bound like a put. `at` as for [`Self::Put`].
    Publish {
        cmd: Cmd,
        at: u64,
    },
}

impl Parked {
    fn is_put(&self) -> bool {
        matches!(self, Self::Put { .. })
    }

    /// Parked under the bound: a put or a publish.
    fn is_bounded(&self) -> bool {
        matches!(self, Self::Put { .. } | Self::Publish { .. })
    }

    /// When a bounded park parked; `None` for a pass-bound command.
    fn bounded_at(&self) -> Option<u64> {
        match self {
            Self::Put { at, .. } | Self::Publish { at, .. } => Some(*at),
            Self::PassBound(_) => None,
        }
    }

    fn into_cmd(self) -> Cmd {
        match self {
            Self::PassBound(cmd) | Self::Put { cmd, .. } | Self::Publish { cmd, .. } => cmd,
        }
    }
}

/// When the oldest bounded park (a put or a publish) parked.
fn oldest_bounded(lot: &VecDeque<Parked>) -> Option<u64> {
    lot.iter().find_map(Parked::bounded_at)
}

/// How long the oldest bounded park has left before the bound; `None` with
/// none parked. Wall-clock millis on the elapsed side ([`now_ms`] — the web
/// host has no monotonic clock), the cross-target sleep on the other: a
/// best-effort bound, as the sign-out grace is.
fn time_left(lot: &VecDeque<Parked>, tip_wait: Duration) -> Option<Duration> {
    let at = oldest_bounded(lot)?;
    Some(tip_wait.saturating_sub(Duration::from_millis(now_ms().saturating_sub(at))))
}

/// At a breath: re-ask every parked put, in arrival order, and serve each
/// whose door now admits it (the door-put verdict is the first thing
/// [`serve_local_cmd`] takes); one still parking stays where it was. `true`
/// when a served put met a rotated writer — the caller drops the pass and
/// reassembles, the puts not yet re-asked still parked.
///
/// Every breath re-asks (one `resolve_tip` per parked put, store reads on
/// this thread): a key recovered from escrow is what makes a tip resolve, and
/// re-asking only when the store's entry count moved would rest on that
/// recovery moving it.
async fn reask_parked<B, R>(
    label: &str,
    drive: &mut Drive<'_, B, R>,
    lot: &mut VecDeque<Parked>,
    gestures: &mut u64,
) -> bool
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let round = lot.len();
    for asked in 1..=round {
        let Some(parked) = lot.pop_front() else {
            break;
        };
        let Parked::Put { cmd, at, asks } = parked else {
            lot.push_back(parked);
            continue;
        };
        let kind = cmd.tip_sealed_kind().unwrap_or("tip-sealed");
        let asks = asks + 1;
        match serve_counted(cmd, &mut drive.local, drive.cycles, gestures).await {
            Served::Done => tracing::info!(
                "account pump ({label}): a {kind} write parked behind this pass was served at a \
                 breath, {} ms after it parked — a tip resolves now (re-asked at {asks} \
                 breath(s))",
                now_ms().saturating_sub(at),
            ),
            Served::Park(cmd) => lot.push_back(Parked::Put { cmd, at, asks }),
            Served::Reassemble => {
                // Keep arrival order for what is left: the lot is the round's
                // unasked rest, then what this round already re-queued.
                lot.rotate_left(round - asked);
                return true;
            }
        }
    }
    false
}

/// At the bound: every put or publish parked at least [`Drive::tip_wait`] —
/// the oldest always, the sleep that fired being its deadline — answers
/// [`NotReadyReason::TipPending`] (a put) or
/// [`NotReadyReason::PublishPending`] (a publish) unserved. The gesture has
/// landed nothing on the nest; the user retries.
fn refuse_at_bound<B, R>(label: &str, drive: &Drive<'_, B, R>, lot: &mut VecDeque<Parked>)
where
    B: StoreBackend,
    R: RpcRequester,
{
    let now = now_ms();
    let wait = drive.tip_wait.as_millis() as u64;
    let scope = drive.local.fleet_plane.scope().to_string();
    let mut oldest = true;
    let mut kept = VecDeque::with_capacity(lot.len());
    for parked in lot.drain(..) {
        match parked {
            Parked::Put { cmd, at, asks } if oldest || now.saturating_sub(at) >= wait => {
                oldest = false;
                let kind = cmd.tip_sealed_kind().unwrap_or("tip-sealed");
                tracing::warn!(
                    "account pump ({label}): a {kind} write parked behind this pass was refused \
                     not-ready {} ms after it parked — no generation tip resolved within {:?} \
                     (re-asked at {asks} breath(s)); nothing was written",
                    now.saturating_sub(at),
                    drive.tip_wait,
                );
                cmd.refuse(anyhow::Error::new(ScopeNotReady {
                    scope: scope.clone(),
                    reason: NotReadyReason::TipPending,
                }));
            }
            Parked::Publish { cmd, at } if oldest || now.saturating_sub(at) >= wait => {
                oldest = false;
                tracing::warn!(
                    "account pump ({label}): an awaited publish parked behind this pass was \
                     refused not-ready {} ms after it parked — the pass did not end within \
                     {:?}; nothing was sent, the rows stay owed to the next publish",
                    now.saturating_sub(at),
                    drive.tip_wait,
                );
                cmd.refuse(anyhow::Error::new(ScopeNotReady {
                    scope: scope.clone(),
                    reason: NotReadyReason::PublishPending,
                }));
            }
            other => kept.push_back(other),
        }
    }
    *lot = kept;
}

/// The tip-sealed door puts one run parked ([`Served::Park`]) — the one wait a
/// local command can have behind a pass, ended by a tip, by the pass's end or
/// by the bound ([`drive_measured`]'s tip wait;
/// `account-client-lifecycle.md` § The client-side lifecycle, the pump
/// bullet → *Commands and passes*). The wait's edges are logged — the
/// mid-pass serve and the bound's refusal where they happen — and these two
/// (observability.md § Log on the *event*, not the *paint*): the park, naming
/// the kind and the pass,
/// and the pass's end, with how long the first parked write waited. A
/// gesture's custody write right after a sign-in finds no tip until the
/// prologue has keyed one, so it waits out the prologue; without these lines
/// an automation timeout on that click read as a silent app, with nothing in
/// the log to place the wait.
struct ParkedWrites {
    /// When the run started, wall-clock millis ([`now_ms`]: the web host
    /// drives passes too, and has no monotonic `Instant`).
    started: u64,
    /// When the first write parked, and how many did.
    first_at: Option<u64>,
    count: usize,
}

impl ParkedWrites {
    fn new() -> Self {
        Self {
            started: now_ms(),
            first_at: None,
            count: 0,
        }
    }

    fn park(&mut self, label: &str, cmd: &Cmd, tip_wait: Duration) {
        let kind = cmd.tip_sealed_kind().unwrap_or("tip-sealed");
        tracing::info!(
            "account pump ({label}): a {kind} write is parked behind this pass — no generation \
             tip resolves for it yet, so its door would mint; it is served at the first breath \
             a tip resolves or when the pass ends, refused not-ready after {tip_wait:?}"
        );
        self.first_at.get_or_insert_with(now_ms);
        self.count += 1;
    }

    /// An awaited publish parked behind the pass — the same event line, for
    /// the same reason: without it an enable left at "Syncing" behind a
    /// long pass read as a silent app.
    fn park_publish(&mut self, label: &str, tip_wait: Duration) {
        tracing::info!(
            "account pump ({label}): an awaited publish is parked behind this pass — the \
             publish step is the pass's own, so it is served when the pass ends, refused \
             not-ready after {tip_wait:?}"
        );
        self.first_at.get_or_insert_with(now_ms);
        self.count += 1;
    }

    fn pass_ended(&self, label: &str, how: &str) {
        let Some(first_at) = self.first_at else {
            return;
        };
        let now = now_ms();
        tracing::info!(
            "account pump ({label}): the pass ended ({how}) {} ms after it started; {} \
             tip-sealed write(s) or awaited publish(es) parked behind it, the first for {} ms",
            now.saturating_sub(self.started),
            self.count,
            now.saturating_sub(first_at),
        );
    }
}

/// Serve one local command, measuring what it wrote — the one place a local
/// command's writes are weighed against the change generation, inside a run
/// and between runs alike (`account-runtime.md` § Multi-instance concurrency
/// → *A runtime's own pump is a source of the notice too*, part 2). A
/// **gesture's** change is credited to `gestures`, which [`drive_pass`]
/// takes out of the run's count (a gesture is never a source). A
/// **background write's** ([`Cmd::is_background_write`]) change moves the
/// generation at once, here, and is credited to `gestures` too, so a run it
/// was served inside does not count it a second time at its end.
pub(crate) async fn serve_counted<B, R>(
    cmd: Cmd,
    local: &mut LocalCtx<'_, B, R>,
    cycles: &PumpCycles,
    gestures: &mut u64,
) -> Served
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let background = cmd.is_background_write();
    let at = local.store.entry_changes();
    let served = serve_local_cmd(cmd, local).await;
    let wrote = local.store.entry_changes().wrapping_sub(at);
    *gestures += wrote;
    if background && wrote > 0 {
        cycles.changed();
    }
    served
}

/// [`pump`] (or the publish step) with panic containment — a poisoned pass is
/// logged, the thread lives, and the report says so — cut by a sign-out
/// ([`SIGN_OUT_PASS_GRACE`]), so the enrollment retirement parked behind it
/// never waits on the pass itself, and driven beside the command channel
/// ([`drive_pass`]). `cycles` counts the pass when it is a full one — the
/// publish step is not (it is one write's network legs, not a pump cycle).
pub(crate) async fn contained_pump<B, R, F>(
    label: &str,
    cycles: Option<&PumpCycles>,
    sign_out: &SignOutWatch,
    drive: &mut Drive<'_, B, R>,
    fut: F,
) -> Result<PumpReport, Reassemble>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    F: std::future::Future<Output = PumpReport>,
{
    if let Some(cycles) = cycles {
        cycles.begin();
    }
    let outcome = match drive_pass(label, sign_out, drive, fut).await {
        Driven::Done(report) => Ok(report),
        Driven::Panicked => {
            let mut report = PumpReport::default();
            report.errors.push(format!("pump ({label}): panicked"));
            Ok(report)
        }
        Driven::Cut => Ok(PumpReport {
            cut_by_sign_out: true,
            ..PumpReport::default()
        }),
        Driven::Reassemble => Err(Reassemble),
    };
    if let Some(cycles) = cycles {
        cycles.end();
    }
    outcome
}
