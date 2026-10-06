//! The pass driver: pump work — a pass, a nudge walk, the publish step — run
//! **beside the command channel**, so a local command is served at the
//! pass's next yield point and a pass-bound one parked behind it; a sign-out
//! cuts the pass after a grace; a panic is contained
//! (`account-data-plane.md` § The client-side lifecycle, the pump bullet →
//! *Commands and passes*).

use std::collections::VecDeque;
use std::time::Duration;

use fauna_account_store::backend::StoreBackend;
use fauna_protocol::{RpcErrorClass, RpcRequester};
use futures_util::FutureExt;
use tokio::sync::{mpsc, watch};

use super::handle::{Cmd, LocalCtx, PumpCycles, Served, serve_local_cmd};
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
/// before and after, the local commands served inside the run — a gesture's
/// own write, which is not a source — are measured on their own and taken
/// out, and a run that changed anything moves the generation once, at its
/// end, however it ended (a cut or panicked run's writes landed too).
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
    loop {
        tokio::select! {
            biased;

            out = &mut fut => {
                return match out {
                    Ok(out) => Driven::Done(out),
                    Err(_) => {
                        tracing::error!("account pump ({label}): panicked — contained, thread lives");
                        Driven::Panicked
                    }
                };
            }
            () = sign_out.grace_lapsed() => {
                tracing::info!(
                    "account pump ({label}): cut by a sign-out — its enrollment retirement \
                     never waits on a pass"
                );
                return Driven::Cut;
            }
            cmd = drive.cmd_rx.recv(), if !closed => match cmd {
                None => closed = true,
                Some(cmd) if cmd.is_local() => {
                    let at = drive.local.store.entry_changes();
                    let served = serve_local_cmd(cmd, &mut drive.local).await;
                    *gestures += drive.local.store.entry_changes().wrapping_sub(at);
                    match served {
                        Served::Done => {}
                        Served::Reassemble => return Driven::Reassemble,
                        Served::Park(cmd) => drive.parked.push_back(cmd),
                    }
                }
                Some(cmd) => drive.parked.push_back(cmd),
            },
        }
    }
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
