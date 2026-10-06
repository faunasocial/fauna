//! The content-index builder's **advisory `index` lease** — the host that makes
//! a live builder the runner-of-record and stands down redundant backlog work.
//!
//! Authority: `docs/goal/behavior/participants.md` § Coordination primitive →
//! *The `index` kind under the lease* (heartbeat, display and pin semantics) and
//! `docs/goal/behavior/content-index.md` § Where the index is built → *The
//! builder and the advisory task lease* (what the gate binds). Ruled 2026-08-03;
//! this module is the build.
//!
//! ## Why the host lives here
//!
//! The loop itself is shared already — [`LeaseCoordinator`] drives
//! observe→decide→heartbeat/gate for any kind. What was *not* shared is the
//! hosting: `backup-upload`'s three companion tasks (the loop, the
//! `lease_changed` wake pump, the one-shot pin load) live inside `fauna-ffi`'s
//! `FfiBackupCoordinator` worker, so they reach only the UniFFI apps. `index`
//! has to run on four apps across two seams — tui and linux directly, windows
//! and macos through the UniFFI factory — and duplicating the hosting per seam
//! is exactly the per-app divergence priority #1 forbids.
//!
//! So the host sits beside `NestMailIndexLauncher`, which already owns the one
//! `NestClient` the lease rendezvous rides and is already the single object app
//! glue hands that connection to. Every seat therefore gets the loop from one
//! place, and the app-facing cost is one value: [`IndexLeaseSeat`].
//!
//! ## Why a seat must be supplied, and cannot be derived
//!
//! A lease holder is a **device**, and a device id is app-owned state (a row in
//! a SQLite file under a directory the app chooses) — `NestClient` does not
//! carry one, so no amount of shared Rust can conjure it. That is the whole
//! reason this is a parameter rather than something the launcher discovers, and
//! it is why an app that supplies no seat simply does not heartbeat: an
//! unattached gate leaves the builder uncoordinated, byte-for-byte as it behaved
//! before the lease existed (`IndexBuilder::lease_gate`).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use fauna_client::NestClient;
use fauna_client_config::SharedPreferenceStore;
use fauna_client_delegation::{DelegationClient, LeaseCoordinator};
use fauna_core::data::ParticipantRef;
use fauna_core::delegation::ParticipantClass;
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

/// The wire identity of the content-index lease — the string every participant
/// contends on and the Task-delegation page keys its row by
/// (`fauna_core::delegation::LIVE_TASK_KINDS`).
pub(crate) const INDEX_TASK_KIND: &str = "index";

/// The push kind that says some participant's lease changed, so a standing-by
/// seat re-evaluates immediately instead of waiting out a heartbeat period.
const LEASE_CHANGED_KIND: &str = "fauna.delegation.lease_changed";

/// What app glue must supply for this seat to contend for the `index` lease.
///
/// Each field is something shared Rust cannot derive (see the module docs):
/// the device identity, the power class only a platform monitor knows, and
/// the seat's account store, where the user's pins live.
#[derive(Clone)]
pub struct IndexLeaseSeat {
    /// This device's stable 32-byte sync device id — the **same** id the Devices
    /// page rosters and `backup-upload` heartbeats, hex-encoded into
    /// `ParticipantRef::Device`. It must match, or the Task-delegation row would
    /// name a participant the roster cannot label.
    pub device_id: [u8; 32],
    /// This seat's power class at login.
    ///
    /// The direct-Rust desktops (tui, linux) pass
    /// [`ParticipantClass::PluggedInDesktop`] and never revise it — neither ships
    /// an AC-line monitor, and the unknown-power default is deliberately
    /// *candidate*, so a lone device still builds rather than waiting for a
    /// plugged-in peer that does not exist. That is the same default
    /// `LeaseRuntime::for_device` takes on the UniFFI seats until their platform
    /// monitor reports.
    pub class: ParticipantClass,
    /// Where the user's task-delegation pins are read — the seat's account
    /// store (`fauna.state.delegation`), waited for when the runtime has not
    /// come up yet.
    pub pins: SharedPreferenceStore,
}

impl std::fmt::Debug for IndexLeaseSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexLeaseSeat")
            .field("device_id", &fauna_core::hex32::encode(&self.device_id))
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

/// How long a launch waits for the lease's **first answer** before offering
/// its walks to a still-undecided gate (`content-index.md` § Where the index is
/// built → *The builder and the advisory task lease*, the launch-walk
/// sub-bullet).
///
/// The wait starts only after the arm resumes — the observe went out before
/// them, on the same connection, so it has had every one of their round trips
/// to land and the ceiling is nearly never paid. When it is, the nest answered
/// the arms' rail reads but not one observe inside it, and the launch proceeds
/// with the gate erring closed: the accepted cost, a re-walk next launch. Not a
/// knob: no user or admin would choose it (product invariants § the only
/// configuration surface is the apps). `pub` only so the ceiling pin can
/// assert the wait was paid rather than skipped.
pub const FIRST_ANSWER_CEILING: Duration = Duration::from_secs(5);

/// This login's seat on the `index` lease, as the launcher holds it: the gate
/// every arm is built over, and the lease loop's first-answer watch.
///
/// Cloneable so the launcher can take a copy out from under its mutex and
/// await on it without holding the lock: the gate is an `Arc` and the watch
/// receiver is a handle, so a clone observes the same loop.
#[derive(Clone)]
pub(crate) struct IndexLease {
    /// `true` ⇒ this seat holds the lease. Attached to every builder at
    /// construction (`IndexBuilder::with_lease_gate`).
    pub(crate) gate: Arc<AtomicBool>,
    /// Flips once the loop's first step completes, decided or failed
    /// ([`LeaseCoordinator::settled`]).
    settled: watch::Receiver<bool>,
}

impl IndexLease {
    /// Hold until the lease loop's first step has completed — the gate then
    /// holds a decision rather than its closed default — or until
    /// [`FIRST_ANSWER_CEILING`] elapses. Answers whether the first answer
    /// landed; a caller past the first answer returns at once.
    ///
    /// A loop that has already ended (the session's cancel fired between the
    /// spawn and this wait) counts as answered: its gate reads closed and will
    /// stay so, and there is nothing further to wait for.
    pub(crate) async fn await_first_answer(&self) -> bool {
        let mut settled = self.settled.clone();
        match tokio::time::timeout(FIRST_ANSWER_CEILING, settled.wait_for(|s| *s)).await {
            Ok(Ok(_)) | Ok(Err(_)) => true,
            Err(_elapsed) => false,
        }
    }
}

/// Start contending for the `index` lease on this connection, and hand back the
/// seat — the gate the builder consults before staging **catch-up** work, and
/// the first-answer watch the launcher holds its walks for.
///
/// Spawns the three tasks the loop needs — the [`LeaseCoordinator::run`] loop,
/// the `lease_changed` wake pump, and the refresh of the user's pins off the
/// account store. The shape is `fauna-ffi`'s `FfiBackupCoordinator::start_inner`
/// lease block, lifted here so the two direct-Rust apps and the UniFFI factory
/// share one copy instead of three.
///
/// **The returned seat is not a handle to the loop.** Every task owns its own
/// `Arc` and ends on `cancel` (session drop / logout) — dropping the seat stops
/// nothing, and that is deliberate: the caller wires the gate into a builder and
/// has no reason to hold a second lifetime object. After cancellation the nest's
/// lease record simply ages out and a peer takes over.
///
/// The heartbeat this starts is **attachment-lifetime, not while-building** —
/// the ruled semantics, and what makes `RunnerStatus` truthful: a builder that
/// heartbeated only while actively staging would fall back to `Waiting` in steady
/// state, telling the user no builder device is live when one is
/// (participants.md § Coordination primitive).
///
/// The pin refresh runs *after* the loop starts and is best-effort, exactly as it
/// was for `backup-upload`: the loop begins on the automatic policy and adopts the
/// pin when it lands, so a slow or failed pin read costs correctness nothing —
/// and it adopts every later change to it on the loop's own cadence
/// ([`refresh_pins`]), because a pin made from a sibling device must stand this
/// seat down without waiting for the app to be restarted.
pub(crate) fn start(
    nest: Arc<NestClient>,
    seat: &IndexLeaseSeat,
    cancel: CancellationToken,
) -> IndexLease {
    let coordinator = Arc::new(LeaseCoordinator::new(
        DelegationClient::new(Arc::clone(&nest)),
        INDEX_TASK_KIND,
        ParticipantRef::Device {
            // `hex32::encode` *is* `hex::encode` over a `[u8; 32]`, so this is
            // byte-identical to what `backup-upload`'s loop and the Devices
            // roster emit — which it must be, or the Task-delegation row would
            // name a participant the roster cannot label.
            device_id: fauna_core::hex32::encode(&seat.device_id),
        },
        seat.class.clone(),
        // The automatic policy until the pin load below lands.
        fauna_core::data::DelegationConfig::default(),
    ));
    let gate = coordinator.gate();
    let settled = coordinator.settled();
    let wake = Arc::new(Notify::new());

    {
        let coordinator = Arc::clone(&coordinator);
        let wake = Arc::clone(&wake);
        let cancel = cancel.clone();
        tokio::spawn(async move { coordinator.run(wake, cancel).await });
    }
    {
        let nest = Arc::clone(&nest);
        let wake = Arc::clone(&wake);
        let cancel = cancel.clone();
        tokio::spawn(async move { pump_lease_changed(nest, wake, cancel).await });
    }
    {
        let coordinator = Arc::clone(&coordinator);
        let wake = Arc::clone(&wake);
        let cancel = cancel.clone();
        let pins = Arc::clone(&seat.pins);
        tokio::spawn(async move { refresh_pins(pins, coordinator, wake, cancel).await });
    }

    IndexLease { gate, settled }
}

/// Re-step the loop the moment any participant's lease changes, so a takeover
/// lands well inside `LEASE_STALE_MS` rather than up to a heartbeat period
/// later. A lagged subscription wakes too — a missed change is exactly when a
/// re-evaluation matters most.
async fn pump_lease_changed(nest: Arc<NestClient>, wake: Arc<Notify>, cancel: CancellationToken) {
    let mut sub = nest.subscribe_kind(LEASE_CHANGED_KIND);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            event = sub.recv() => match event {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    wake.notify_one();
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

/// Keep the loop's copy of the user's synced pins (`fauna.state.delegation`)
/// current, re-reading on the lease loop's own cadence until `cancel`.
///
/// **Why a refresh and not a one-shot.** The pins are cross-device state:
/// `participants.md` § Coordination primitive keeps the assignment record
/// synced precisely "so every one of the user's clients sees the same
/// assignments", and § The assignment picker makes the
/// pin decide *who runs the kind* — a pin collapses the candidate set to the
/// pinned participant, and every other device must stand down. A seat that read
/// the pins once at login honours neither sentence for its own lifetime: a pin
/// made afterwards — from this device's own picker, or from a sibling device —
/// reaches the Task-delegation *page* (which re-reads the record on every
/// visit) while the lease loop keeps heartbeating on the stale automatic
/// policy. The user then reads a row naming a device their assignment says
/// should have stood down, until the app is next restarted. The re-read is
/// the mechanism.
///
/// The cadence is [`HEARTBEAT_PERIOD_MS`] — the loop's own — because the pins
/// are an *input to the decision that loop makes every cycle*: refreshing
/// slower would spend whole cycles deciding on input already known to be
/// stale. A change pulses `wake`, so the new pin takes effect on the spot
/// rather than at the end of the current period.
///
/// Best-effort by design, exactly as the one-shot was: a failed read leaves the
/// last-known pins in place (the automatic policy on the first pass), which is
/// the correct behaviour for the overwhelmingly common unpinned case.
async fn refresh_pins(
    pins: SharedPreferenceStore,
    coordinator: Arc<LeaseCoordinator<Arc<NestClient>>>,
    wake: Arc<Notify>,
    cancel: CancellationToken,
) {
    let mut applied = fauna_core::data::DelegationConfig::default();
    let period = std::time::Duration::from_millis(fauna_core::delegation::HEARTBEAT_PERIOD_MS);
    loop {
        let read = tokio::select! {
            _ = cancel.cancelled() => break,
            read = pins.delegation() => read,
        };
        match read {
            Ok(delegation) if delegation != applied => {
                applied = delegation.clone();
                coordinator.set_config(delegation);
                // Only a *change* pulses the loop. An unconditional pulse
                // every period would roughly double the step rate for no
                // new information — the loop already ticks on its own.
                wake.notify_one();
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "index lease: delegation pins not refreshed, keeping the last-known assignment")
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(period) => {}
        }
    }
}
