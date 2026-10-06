//! `LeaseCoordinator` — the shared client-side lease loop that makes a heavy
//! task kind run on **exactly one** advisory holder (participants.md
//! § Coordination primitive; design tracked internally).
//!
//! Composes the three pieces every desktop runner needs (priority #2 — one
//! loop, lifted to macos/windows via FFI):
//!
//! 1. the transport ([`DelegationClient`]'s `observe`/`heartbeat`),
//! 2. the pure decision (`fauna_core::delegation::{current_candidates, decide}`),
//! 3. a **gate** ([`Arc<AtomicBool>`]) the task runner (e.g. the
//!    content-index builder, `fauna_client_index::IndexBuilder`) consults
//!    before each pass — `true` ⇒ this participant holds the lease and may
//!    run, `false` ⇒ stand by.
//!
//! The participant's **class** (plugged-in vs on-battery) is *platform-only
//! knowledge* — no AC-power signal exists in shared Rust — so it is **pushed**
//! in via [`LeaseCoordinator::set_class`] (participants.md § Class reporting),
//! mirroring the rebuild-on-destination-change idiom rather than deriving a
//! watch here.
//!
//! ## The one insight that de-risks the loop
//!
//! Each cycle builds its candidate set from **just `self` + the observed
//! holder** — never the full device roster with everyone's live class. The
//! observed lease already carries the holder's class; sticky+LWW convergence
//! (the `decide` rule) settles the rest with no cross-device ranking. So the
//! loop needs no device-directory read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fauna_core::data::{DelegationConfig, ParticipantRef};
use fauna_core::delegation::LEASE_STALE_MS;
use fauna_core::delegation::{
    LeaseAction, ObservedLease, ParticipantClass, ParticipantDescriptor, current_candidates, decide,
};
// Only the `driver`-gated `run` loop references the heartbeat cadence.
#[cfg(feature = "driver")]
use fauna_core::delegation::HEARTBEAT_PERIOD_MS;
use fauna_protocol::RpcRequester;
use fauna_protocol::delegation::{HeartbeatRequest, ObserveRequest};

use crate::DelegationClient;

/// Drives one task kind's advisory lease for one participant. Long-lived (one
/// per runner); [`run`](LeaseCoordinator::run) loops [`step`](LeaseCoordinator::step)
/// on a timer + push-wake, and the platform pushes class updates via
/// [`set_class`](LeaseCoordinator::set_class).
pub struct LeaseCoordinator<R: RpcRequester> {
    client: DelegationClient<R>,
    /// The task kind this loop governs (e.g. `"backup-upload"`).
    task_kind: String,
    /// This participant's stable identity (a device for backup-upload — its
    /// runner is always a client, participants.md § Dispatch by kind).
    self_ref: ParticipantRef,
    /// This participant's **live** class, updated by the platform power monitor
    /// via [`set_class`](Self::set_class). Behind a `Mutex` so a separate power
    /// poller can push updates while the loop reads it (both are cheap, held for
    /// microseconds and never across an `.await`).
    class: Mutex<ParticipantClass>,
    /// The user's synced pins (`fauna.state.delegation`). Empty ⇒ the pure policy
    /// order. `Mutex` so slice 4's config-refresh can update it live.
    config: Mutex<DelegationConfig>,
    /// The runner gate — `true` ⇒ hold the lease, run the task. Shared with the
    /// runner via [`gate`](Self::gate). Starts `false` (stand by until the first
    /// `step` claims).
    gate: Arc<AtomicBool>,
    /// Whether [`run`](Self::run) has completed its **first** step — decided or
    /// failed. `false` until then, `true` for ever after; the host reads it via
    /// [`settled`](Self::settled).
    ///
    /// Exists because the gate alone cannot tell "standing down" from "not yet
    /// answered": both read `false`, and a runner whose queue is offered
    /// **once** (a launch walk) must not consult the gate before its first
    /// answer or it withholds work nothing re-offers (`content-index.md` §
    /// Where the index is built → *The builder and the advisory task lease*,
    /// the launch-walk sub-bullet). Driver-only, like the loop that flips it.
    #[cfg(feature = "driver")]
    settled: tokio::sync::watch::Sender<bool>,
}

impl<R: RpcRequester> LeaseCoordinator<R> {
    /// Build a coordinator for `task_kind`. `initial_class` is the platform's
    /// current power state; `config` the user's synced pins (empty for the
    /// automatic policy). The gate starts closed.
    pub fn new(
        client: DelegationClient<R>,
        task_kind: impl Into<String>,
        self_ref: ParticipantRef,
        initial_class: ParticipantClass,
        config: DelegationConfig,
    ) -> Self {
        Self {
            client,
            task_kind: task_kind.into(),
            self_ref,
            class: Mutex::new(initial_class),
            config: Mutex::new(config),
            gate: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "driver")]
            settled: tokio::sync::watch::channel(false).0,
        }
    }

    /// The gate the task runner consults each pass (`true` ⇒ run). Hand a clone
    /// to the runner (e.g. `IndexBuilder::with_lease_gate`).
    pub fn gate(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.gate)
    }

    /// Whether the loop has had its first answer — a watch that flips to
    /// `true` once [`run`](Self::run)'s first step completes, **decided or
    /// failed**, and never flips back.
    ///
    /// A host whose work is offered once (the index launcher's attach walks)
    /// awaits this before its first offer, so the gate it then consults holds
    /// a decision rather than the closed default. A failed attempt settles it
    /// too: the gate then errs closed exactly as it did before the wait
    /// existed, and the next attempt is a whole [`HEARTBEAT_PERIOD_MS`] away —
    /// nothing a launch could reasonably hold for. `wait_for(|s| *s)` answers
    /// at once when the loop has already stepped, so a late caller pays
    /// nothing.
    #[cfg(feature = "driver")]
    pub fn settled(&self) -> tokio::sync::watch::Receiver<bool> {
        self.settled.subscribe()
    }

    /// Push this participant's current class (the platform power monitor calls
    /// this on plug/unplug — participants.md § Class reporting). An unplug that
    /// drops us to [`ParticipantClass::BatteryMobile`] makes the next `step`
    /// find us out of the candidate set and yield (stop running).
    pub fn set_class(&self, class: ParticipantClass) {
        *self.class.lock().expect("class mutex poisoned") = class;
    }

    /// This participant's current class (lets a poller detect a change and pulse
    /// the loop's wake for prompt re-evaluation).
    pub fn class(&self) -> ParticipantClass {
        self.class.lock().expect("class mutex poisoned").clone()
    }

    /// Refresh the synced pins — the loop's copy of `fauna.state.delegation`.
    ///
    /// Called by the host's pin refresher on its own cadence
    /// (`fauna_client_conversations::index_lease::refresh_pins`), so a pin the
    /// user makes on *any* of their devices reaches this running loop: the pin
    /// is what collapses the candidate set (participants.md § The assignment
    /// picker), so a seat deciding on a stale copy keeps running a kind the
    /// user has assigned elsewhere. The host pulses the loop's `wake` after a
    /// change, so the new assignment takes effect on the spot.
    pub fn set_config(&self, config: DelegationConfig) {
        *self.config.lock().expect("config mutex poisoned") = config;
    }

    /// One lease cycle (participants.md § Coordination primitive):
    ///
    /// `observe` the kind → build the candidate set from **self + the observed
    /// holder** → `current_candidates` → `decide` → on `Acquire`/`Renew`
    /// heartbeat and **open** the gate, on `Yield` **close** it. Returns the
    /// action (for the driver's logging and the loop tests).
    ///
    /// **Error semantics (fail-open).** A transport error propagates via `?`.
    /// An `observe` error returns before touching the gate — the runner keeps
    /// whatever state it had (a transient rendezvous-nest blip doesn't stop an
    /// in-flight idempotent pass, and doesn't newly start one). A `heartbeat`
    /// error on a *fresh* `Acquire` returns before opening the gate
    /// (conservative: we couldn't record the claim, so don't start); on a
    /// `Renew` the gate is already open and stays so (the pass continues through
    /// the blip; the lease self-heals on the next cycle). The driver logs the
    /// error and re-steps next cycle.
    pub async fn step(&self) -> Result<LeaseAction, R::Error> {
        let self_class = self.class();

        // Observe the current lease for our kind (absent ⇒ free).
        let reply = self
            .client
            .observe(ObserveRequest {
                task_kinds: vec![self.task_kind.clone()],
                extra: Default::default(),
            })
            .await?;
        let observed = reply
            .leases
            .into_iter()
            .find(|l| l.task_kind == self.task_kind);

        // The candidate set from just self + the observed holder (the insight:
        // no full-roster read needed). self is a client ⇒ `holds_grant = false`
        // (clients run with their own keys and are never grant-holders).
        //
        // An observed **nest** holder is tier-1 (`holds_grant: true`), because a
        // nest heartbeats a kind's lease *only* when its own sufficiency scan
        // passed for that kind (`delegation_runner::run_pass` on the nest) — so
        // `holder_class == AlwaysOnNest` already means "a nest that holds a
        // sufficient grant to run this". `holder_class` is self-reported by the
        // heartbeating participant and clients report a desktop/mobile class, so
        // this arm is reachable only for a genuine nest claim. That yields the
        // client-side half of participants.md:59's tier-1 handover *into* the
        // nest: this client stands down instead of preempting the nest that is
        // in fact doing the work (backup-restore.md § Flip status (slice 5)).
        //
        // Trusting the claim grants the rendezvous nest nothing it did not
        // already have — it also supplies `holder`, `holder_class` and `age_ms`,
        // any of which it could forge to steer `decide`. The "nest claims but
        // never works" threat is covered by the client audit loop instead
        // (backup-restore.md § audit-loop parameters), which is source-untrusted
        // by construction and therefore the right detector for it.
        //
        // **The freshness conjunct is load-bearing, not belt-and-braces.** Only
        // a *live* claim makes the nest a candidate, because `decide` gates on
        // eligibility (`!cands.contains(self)` ⇒ `Yield`) **before** it looks at
        // staleness. A stale nest left in the tier-1 set would therefore keep
        // every app out permanently — a crashed nest would strand the kind
        // forever rather than for `LEASE_STALE_MS`. Dropping it back out
        // restores the normal takeover path (candidates collapse to this
        // tier-2 desktop, which then acquires the stale lease).
        let mut participants = vec![ParticipantDescriptor {
            reference: self.self_ref.clone(),
            class: self_class.clone(),
            holds_grant: false,
        }];
        if let Some(l) = &observed
            && l.holder != self.self_ref
        {
            participants.push(ParticipantDescriptor {
                reference: l.holder.clone(),
                class: l.holder_class.clone(),
                holds_grant: matches!(l.holder_class, ParticipantClass::AlwaysOnNest)
                    && l.age_ms < LEASE_STALE_MS,
            });
        }

        let config = self.config.lock().expect("config mutex poisoned").clone();
        let cands = current_candidates(&self.task_kind, &participants, &config);
        let observed_lease = observed.as_ref().map(|l| ObservedLease {
            holder: l.holder.clone(),
            holder_class: l.holder_class.clone(),
            age_ms: l.age_ms,
        });
        let action = decide(
            &self.self_ref,
            &cands,
            observed_lease.as_ref(),
            LEASE_STALE_MS,
        );

        match action {
            LeaseAction::Acquire | LeaseAction::Renew => {
                self.client
                    .heartbeat(HeartbeatRequest {
                        task_kind: self.task_kind.clone(),
                        holder: self.self_ref.clone(),
                        holder_class: self_class,
                        extra: Default::default(),
                    })
                    .await?;
                self.gate.store(true, Ordering::Relaxed);
            }
            LeaseAction::Yield => {
                self.gate.store(false, Ordering::Relaxed);
            }
        }
        Ok(action)
    }

    /// Drive the lease loop until `cancel`. Runs one [`step`](Self::step)
    /// immediately, then re-steps every [`HEARTBEAT_PERIOD_MS`] or whenever
    /// `wake` is pulsed — the platform pulses it on a
    /// `fauna.delegation.lease_changed` push (a holder changed) or a class
    /// change (prompt unplug handover), so a standing-by peer takes over well
    /// inside [`LEASE_STALE_MS`] without waiting out a full heartbeat period.
    /// Transport errors are logged; the loop continues (fail-open, per
    /// [`step`](Self::step)). On exit it closes the gate so the runner stops.
    ///
    /// Native only (the `driver` feature). Call once per coordinator, on a
    /// runtime that outlives the authed session (the desktop backup worker).
    #[cfg(feature = "driver")]
    pub async fn run(
        &self,
        wake: Arc<tokio::sync::Notify>,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        let period = std::time::Duration::from_millis(HEARTBEAT_PERIOD_MS);
        let mut interval = tokio::time::interval(period);
        // The first `interval.tick()` is immediate; consume it so the loop body
        // owns the first run (below) and the in-loop tick fires after `period`.
        interval.tick().await;

        loop {
            if let Err(e) = self.step().await {
                tracing::warn!(
                    task_kind = %self.task_kind,
                    error = %e,
                    "delegation lease step failed; retrying next cycle",
                );
            }
            // The first attempt is over, whichever way it went — see
            // [`settled`](Self::settled) for why a failure counts.
            if !*self.settled.borrow() {
                self.settled.send_replace(true);
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {}
                _ = wake.notified() => {}
            }
        }
        // Departing (sign-out / rebuild): stop running locally. The nest lease
        // record ages out on its own; this just prevents the runner continuing
        // after the loop is gone.
        self.gate.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::delegation::{
        HeartbeatReply, KIND_HEARTBEAT, KIND_OBSERVE, LeaseState, ObserveReply,
    };

    fn dev(id: &str) -> ParticipantRef {
        ParticipantRef::Device {
            device_id: id.to_string(),
        }
    }

    fn lease(holder: ParticipantRef, class: ParticipantClass, age_ms: u64) -> LeaseState {
        LeaseState {
            task_kind: "backup-upload".into(),
            holder,
            holder_class: class,
            age_ms,
            extra: Default::default(),
        }
    }

    /// A requester whose `observe` returns a scripted lease set and whose
    /// `heartbeat` records the request (so a test can assert *whether* the loop
    /// heartbeated and *what holder* it claimed). Transport-free — the futures
    /// resolve immediately, so [`block_on`] never parks.
    struct ScriptedRequester {
        observe_leases: Vec<LeaseState>,
        heartbeats: Mutex<Vec<HeartbeatRequest>>,
    }

    impl ScriptedRequester {
        fn new(observe_leases: Vec<LeaseState>) -> Arc<Self> {
            Arc::new(Self {
                observe_leases,
                heartbeats: Mutex::new(Vec::new()),
            })
        }
        fn heartbeat_count(&self) -> usize {
            self.heartbeats.lock().unwrap().len()
        }
        fn last_heartbeat(&self) -> Option<HeartbeatRequest> {
            self.heartbeats.lock().unwrap().last().cloned()
        }
    }

    impl RpcRequester for ScriptedRequester {
        type Error = std::convert::Infallible;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            // Round-trip through the canonical codec so the mock exercises the
            // real wire shape (mirrors the lib.rs RecordingRequester).
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let reply = match kind {
                KIND_OBSERVE => fauna_protocol::encode_canonical(&ObserveReply {
                    leases: self.observe_leases.clone(),
                    extra: Default::default(),
                }),
                KIND_HEARTBEAT => {
                    let req: HeartbeatRequest =
                        fauna_protocol::decode_strict(&bytes).expect("decode heartbeat");
                    let holder = req.holder.clone();
                    let holder_class = req.holder_class.clone();
                    self.heartbeats.lock().unwrap().push(req);
                    fauna_protocol::encode_canonical(&HeartbeatReply {
                        lease: lease(holder, holder_class, 0),
                        extra: Default::default(),
                    })
                }
                other => panic!("ScriptedRequester: unhandled kind {other}"),
            }
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    fn coordinator(
        rec: Arc<ScriptedRequester>,
        self_id: &str,
        class: ParticipantClass,
    ) -> LeaseCoordinator<Arc<ScriptedRequester>> {
        LeaseCoordinator::new(
            DelegationClient::new(rec),
            "backup-upload",
            dev(self_id),
            class,
            DelegationConfig::default(),
        )
    }

    #[test]
    fn free_lease_acquires_opens_gate_and_heartbeats() {
        let rec = ScriptedRequester::new(vec![]); // no record ⇒ free
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        assert!(!lc.gate().load(Ordering::Relaxed), "gate starts closed");

        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Acquire);
        assert!(lc.gate().load(Ordering::Relaxed), "acquire opens the gate");
        assert_eq!(rec.heartbeat_count(), 1, "acquire heartbeats");
        assert_eq!(rec.last_heartbeat().unwrap().holder, dev("dev-a"));
    }

    #[test]
    fn holding_a_fresh_lease_renews_and_keeps_gate_open() {
        let rec = ScriptedRequester::new(vec![lease(
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
            1_000,
        )]);
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Renew);
        assert!(lc.gate().load(Ordering::Relaxed));
        assert_eq!(rec.heartbeat_count(), 1, "renew heartbeats");
    }

    #[test]
    fn cotier_peer_holding_fresh_lease_yields_and_stays_closed() {
        // A co-tier plugged peer holds a fresh lease ⇒ sticky yield, no churn,
        // no heartbeat, gate stays closed.
        let rec = ScriptedRequester::new(vec![lease(
            dev("dev-b"),
            ParticipantClass::PluggedInDesktop,
            1_000,
        )]);
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Yield);
        assert!(!lc.gate().load(Ordering::Relaxed));
        assert_eq!(rec.heartbeat_count(), 0, "yield does not heartbeat");
    }

    #[test]
    fn stale_holder_is_taken_over() {
        // The recorded holder went silent (age ≥ stale) ⇒ takeover: acquire,
        // heartbeat, open gate.
        let rec = ScriptedRequester::new(vec![lease(
            dev("dev-b"),
            ParticipantClass::PluggedInDesktop,
            LEASE_STALE_MS + 1,
        )]);
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Acquire);
        assert!(lc.gate().load(Ordering::Relaxed));
        assert_eq!(rec.heartbeat_count(), 1);
    }

    #[test]
    fn a_fresh_nest_holder_is_tier_1_and_the_client_stands_down() {
        // The slice-5 flip's closing move (backup-restore.md § Flip status
        // (slice 5)): the source nest runs `backup-upload` itself now. A client
        // that still ships a driver must **stand down** rather than double-write
        // — i.e. an observed `AlwaysOnNest` holder is tier-1 (participants.md:50
        // policy order), so this tier-2 desktop is not in the winning tier and
        // `decide`'s eligibility gate yields.
        //
        // Before the candidate-rule fix this asserted the opposite: the nest was
        // added with `holds_grant: false`, dropped out of `current_candidates`,
        // and `decide` took the "ineligible holder → preempt" arm.
        let rec = ScriptedRequester::new(vec![lease(
            ParticipantRef::Nest {
                actor_pubkey: [7u8; 32],
            },
            ParticipantClass::AlwaysOnNest,
            1_000,
        )]);
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(
            action,
            LeaseAction::Yield,
            "a client must not preempt the nest that is doing the backups"
        );
        assert!(
            !lc.gate().load(Ordering::Relaxed),
            "standing down closes the runner gate"
        );
        assert_eq!(rec.heartbeat_count(), 0, "yield does not heartbeat");
    }

    #[test]
    fn a_stale_nest_holder_is_still_taken_over() {
        // Trusting the nest's claim is bounded by staleness, never permanent: a
        // nest that stopped heartbeating (crashed, or its sufficiency lapsed
        // without a clean release) hands the kind back to an eligible client.
        let rec = ScriptedRequester::new(vec![lease(
            ParticipantRef::Nest {
                actor_pubkey: [7u8; 32],
            },
            ParticipantClass::AlwaysOnNest,
            LEASE_STALE_MS + 1,
        )]);
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Acquire);
        assert!(lc.gate().load(Ordering::Relaxed));
        assert_eq!(rec.heartbeat_count(), 1);
    }

    #[test]
    fn battery_self_never_runs() {
        // A battery-mobile participant is never a candidate ⇒ yield regardless of
        // the lease, gate closed, no heartbeat (participants.md § Don't do these).
        let rec = ScriptedRequester::new(vec![]);
        let lc = coordinator(rec.clone(), "phone-1", ParticipantClass::BatteryMobile);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Yield);
        assert!(!lc.gate().load(Ordering::Relaxed));
        assert_eq!(rec.heartbeat_count(), 0);
    }

    #[test]
    fn unplug_makes_a_running_holder_yield() {
        // Start plugged and holding a fresh self-lease (would renew) ...
        let rec = ScriptedRequester::new(vec![lease(
            dev("dev-a"),
            ParticipantClass::PluggedInDesktop,
            1_000,
        )]);
        let lc = coordinator(rec.clone(), "dev-a", ParticipantClass::PluggedInDesktop);
        assert_eq!(block_on(lc.step()).unwrap(), LeaseAction::Renew);
        assert!(lc.gate().load(Ordering::Relaxed));

        // ... then unplug: the class push drops us to battery, so the next step
        // finds us out of the candidate set and yields (stops running). Its lease
        // then ages out and a still-plugged peer takes over.
        lc.set_class(ParticipantClass::BatteryMobile);
        let action = block_on(lc.step()).unwrap();
        assert_eq!(action, LeaseAction::Yield);
        assert!(!lc.gate().load(Ordering::Relaxed), "unplug closes the gate");
    }

    /// [`LeaseCoordinator::run`] — the `driver`-gated production loop that
    /// drives [`step`](LeaseCoordinator::step) on a timer + push-wake. `step`
    /// itself is covered above; these prove the loop wrapper's own behavior:
    /// a transport error is fail-open (matching `step`'s documented
    /// contract), a wake pulse re-steps immediately rather than waiting out
    /// the full heartbeat period, and cancellation stops the loop and closes
    /// the gate. Real tokio runtime (paused clock) rather than [`block_on`]
    /// above, since `run` genuinely waits on a timer/notify/cancel select.
    #[cfg(feature = "driver")]
    mod run_loop_tests {
        use super::*;
        use std::sync::atomic::AtomicUsize;
        use std::time::Duration;
        use tokio::sync::Notify;
        use tokio_util::sync::CancellationToken;

        /// Like [`ScriptedRequester`], but its first `fail_first_n` calls to
        /// `request` (of any kind) return an error — proves `step`'s
        /// fail-open contract survives a real `run` loop, not just a direct
        /// `step` call.
        struct FlakyRequester {
            observe_leases: Vec<LeaseState>,
            heartbeats: Mutex<Vec<HeartbeatRequest>>,
            remaining_failures: AtomicUsize,
        }

        impl FlakyRequester {
            fn new(observe_leases: Vec<LeaseState>, fail_first_n: usize) -> Arc<Self> {
                Arc::new(Self {
                    observe_leases,
                    heartbeats: Mutex::new(Vec::new()),
                    remaining_failures: AtomicUsize::new(fail_first_n),
                })
            }
            fn heartbeat_count(&self) -> usize {
                self.heartbeats.lock().unwrap().len()
            }
        }

        impl RpcRequester for FlakyRequester {
            type Error = String;
            async fn request<Req, Reply>(
                &self,
                kind: &'static str,
                payload: Req,
            ) -> Result<Reply, Self::Error>
            where
                Req: serde::Serialize,
                Reply: serde::de::DeserializeOwned,
            {
                if self.remaining_failures.load(Ordering::SeqCst) > 0 {
                    self.remaining_failures.fetch_sub(1, Ordering::SeqCst);
                    return Err("scripted transport failure".to_string());
                }
                let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
                let reply = match kind {
                    KIND_OBSERVE => fauna_protocol::encode_canonical(&ObserveReply {
                        leases: self.observe_leases.clone(),
                        extra: Default::default(),
                    }),
                    KIND_HEARTBEAT => {
                        let req: HeartbeatRequest =
                            fauna_protocol::decode_strict(&bytes).expect("decode heartbeat");
                        let holder = req.holder.clone();
                        let holder_class = req.holder_class.clone();
                        self.heartbeats.lock().unwrap().push(req);
                        fauna_protocol::encode_canonical(&HeartbeatReply {
                            lease: lease(holder, holder_class, 0),
                            extra: Default::default(),
                        })
                    }
                    other => panic!("FlakyRequester: unhandled kind {other}"),
                }
                .expect("encode reply");
                Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
            }
        }

        /// Cooperatively yields until `cond` is true or `attempts` runs out —
        /// drives a spawned `run()` task forward without needing real time
        /// (the mock requesters resolve instantly, so a handful of yields is
        /// enough for the loop to reach its next await point).
        async fn wait_until(mut cond: impl FnMut() -> bool, attempts: usize) {
            for _ in 0..attempts {
                if cond() {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("condition did not become true within {attempts} cooperative yields");
        }

        #[tokio::test(start_paused = true)]
        async fn run_survives_a_step_error_and_recovers_on_the_next_tick() {
            // Fails only the very first transport call (the initial step's
            // observe) — proves the error is logged and the loop keeps
            // going rather than dying, exactly as `step`'s doc promises.
            let rec = FlakyRequester::new(vec![], 1);
            let lc = Arc::new(LeaseCoordinator::new(
                DelegationClient::new(rec.clone()),
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
                DelegationConfig::default(),
            ));
            let wake = Arc::new(Notify::new());
            let cancel = CancellationToken::new();

            let handle = tokio::spawn({
                let lc = lc.clone();
                let wake = wake.clone();
                let cancel = cancel.clone();
                async move { lc.run(wake, cancel).await }
            });

            // Let the (failing) initial step run to completion before
            // touching the clock — a fixed cooperative-yield budget, since
            // "nothing happened yet" can't be told apart from "already
            // failed" by polling a still-zero counter.
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                rec.heartbeat_count(),
                0,
                "the failed initial step must not heartbeat"
            );
            assert!(
                !lc.gate().load(Ordering::Relaxed),
                "a failed step must not open the gate"
            );

            // The next heartbeat tick re-steps; this one succeeds (the single
            // scripted failure is already spent).
            tokio::time::advance(Duration::from_millis(HEARTBEAT_PERIOD_MS)).await;
            wait_until(|| rec.heartbeat_count() == 1, 1000).await;
            assert!(
                lc.gate().load(Ordering::Relaxed),
                "the loop recovered on the next cycle"
            );

            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("run() must return promptly after cancellation")
                .unwrap();
            assert!(
                !lc.gate().load(Ordering::Relaxed),
                "loop exit closes the gate"
            );
        }

        #[tokio::test(start_paused = true)]
        async fn wake_pulse_triggers_an_immediate_restep_without_waiting_out_the_period() {
            // Always-free lease: every step is an Acquire, and Acquire always
            // heartbeats, so the heartbeat count is a direct step counter.
            let rec = ScriptedRequester::new(vec![]);
            let lc = Arc::new(coordinator(
                rec.clone(),
                "dev-a",
                ParticipantClass::PluggedInDesktop,
            ));
            let wake = Arc::new(Notify::new());
            let cancel = CancellationToken::new();

            let handle = tokio::spawn({
                let lc = lc.clone();
                let wake = wake.clone();
                let cancel = cancel.clone();
                async move { lc.run(wake, cancel).await }
            });

            wait_until(|| rec.heartbeat_count() == 1, 1000).await;
            let before = tokio::time::Instant::now();

            wake.notify_one();
            wait_until(|| rec.heartbeat_count() == 2, 1000).await;

            // Only cooperative yields drove this, never a timer-fired
            // auto-advance — so it landed well inside the heartbeat period.
            assert!(
                tokio::time::Instant::now() - before < Duration::from_millis(HEARTBEAT_PERIOD_MS),
                "wake should re-step immediately, not after the full heartbeat period"
            );

            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("run() must return promptly after cancellation")
                .unwrap();
        }

        #[tokio::test(start_paused = true)]
        async fn cancel_breaks_the_loop_and_closes_the_gate() {
            let rec = ScriptedRequester::new(vec![]);
            let lc = Arc::new(coordinator(
                rec.clone(),
                "dev-a",
                ParticipantClass::PluggedInDesktop,
            ));
            let wake = Arc::new(Notify::new());
            let cancel = CancellationToken::new();

            let handle = tokio::spawn({
                let lc = lc.clone();
                let wake = wake.clone();
                let cancel = cancel.clone();
                async move { lc.run(wake, cancel).await }
            });

            wait_until(|| lc.gate().load(Ordering::Relaxed), 1000).await;

            cancel.cancel();
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("cancellation must stop run() promptly, not hang until the next tick")
                .unwrap();
            assert!(
                !lc.gate().load(Ordering::Relaxed),
                "loop exit closes the gate"
            );
        }

        /// **The first answer is a fact the host can wait on.** A builder's
        /// launch-walk backlog must not be offered to a gate that has not had
        /// its first answer yet (a closed-by-default gate withholds it, and a
        /// walk that runs once is never re-offered — `content-index.md` § Where
        /// the index is built → *The builder and the advisory task lease*), so
        /// `run` flips [`LeaseCoordinator::settled`] after its first step and
        /// the host awaits that rather than guessing at round-trip timing.
        /// Both outcomes settle it: a decision (the gate now reads it) and a
        /// failed attempt (the gate stays closed and the next attempt is a
        /// whole heartbeat period away — nothing a launch could wait for).
        #[tokio::test(start_paused = true)]
        async fn settled_flips_after_the_first_step_whatever_its_outcome() {
            // A first step that DECIDES: free lease ⇒ Acquire ⇒ gate open.
            let rec = ScriptedRequester::new(vec![]);
            let lc = Arc::new(coordinator(
                rec.clone(),
                "dev-a",
                ParticipantClass::PluggedInDesktop,
            ));
            let mut settled = lc.settled();
            assert!(
                !*settled.borrow(),
                "nothing has stepped yet, so the lease is unsettled"
            );
            let cancel = CancellationToken::new();
            let handle = tokio::spawn({
                let lc = lc.clone();
                let cancel = cancel.clone();
                async move { lc.run(Arc::new(Notify::new()), cancel).await }
            });
            tokio::time::timeout(Duration::from_secs(5), settled.wait_for(|s| *s))
                .await
                .expect("the first step must settle the lease")
                .expect("the loop is still running");
            assert!(
                lc.gate().load(Ordering::Relaxed),
                "settled after a decision ⇒ the gate already reads it"
            );
            cancel.cancel();
            handle.await.unwrap();

            // A first step that FAILS: the transport errored, no decision — but
            // the attempt is over and the host must not wait out the retry.
            let rec = FlakyRequester::new(vec![], 1);
            let lc = Arc::new(LeaseCoordinator::new(
                DelegationClient::new(rec.clone()),
                "backup-upload",
                dev("dev-a"),
                ParticipantClass::PluggedInDesktop,
                DelegationConfig::default(),
            ));
            let mut settled = lc.settled();
            let cancel = CancellationToken::new();
            let handle = tokio::spawn({
                let lc = lc.clone();
                let cancel = cancel.clone();
                async move { lc.run(Arc::new(Notify::new()), cancel).await }
            });
            tokio::time::timeout(Duration::from_secs(5), settled.wait_for(|s| *s))
                .await
                .expect("a failed first step must settle the lease too")
                .expect("the loop is still running");
            assert_eq!(
                rec.heartbeat_count(),
                0,
                "the failed step never heartbeated — settled is not a decision"
            );
            assert!(
                !lc.gate().load(Ordering::Relaxed),
                "settled after a failure ⇒ the gate still errs closed"
            );
            cancel.cancel();
            handle.await.unwrap();
        }
    }
}
