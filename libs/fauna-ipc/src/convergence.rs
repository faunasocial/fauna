//! The provisioning **convergence loop** — one shared-Rust implementation of the
//! probe → spawn-if-absent → `RefreshBearer` → full re-provision-on-`NoCapability`
//! cycle that every desktop control surface (linux GTK, FaunaKit, fauna-tui) runs
//! to keep the per-user sync agent provisioned (`sync-agent.md` § Control plane
//! split; plan D3). It ticks every [`DEFAULT_TICK_INTERVAL`] or immediately on a
//! poke (e.g. the app just signed in or rebound a set).
//!
//! This is the Rust twin of the C# `HydrationSessionService`
//! (`FaunaApp.Core/Services/HydrationSessionService.cs`): the tri-state
//! [`RefreshBearerOutcome`] and the pinned no-capability error prefix
//! ([`crate::sync::NO_CAPABILITY_ERROR_PREFIX`]) are the **cross-process
//! contract**, so both implementations stay interchangeable against the same
//! agent. The loop itself is platform-agnostic; each app supplies the
//! platform-specific hooks via [`ProvisioningDelegate`] (how to probe/spawn the
//! agent, where the nest bearer comes from, how to push `RefreshBearer` /
//! `ProvisionCapability` over the local IPC seam).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::sync::BearerToken;

/// Default convergence tick — matches the C# `DefaultTickInterval`.
pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(30);

/// Grace granted after a spawn before leaning on the freshly-started agent —
/// matches the C# `DefaultSpawnGrace`.
pub const DEFAULT_SPAWN_GRACE: Duration = Duration::from_millis(300);

/// The tri-state outcome of pushing a `RefreshBearer` to the agent — the Rust twin
/// of C#'s `RefreshBearerOutcome` (`SyncServicePipeClient.cs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshBearerOutcome {
    /// Agent reachable and provisioned; the bearer was updated.
    Accepted,
    /// Agent reachable but holds no capability (a fresh start, or a restart before
    /// re-provisioning) — the caller must push a full `ProvisionCapability`.
    /// Detected by matching the agent's error against
    /// [`crate::sync::NO_CAPABILITY_ERROR_PREFIX`].
    NoCapability,
    /// Agent not running / the exchange failed — a normal no-op this tick.
    Unreachable,
}

impl RefreshBearerOutcome {
    /// Classify an agent `RefreshBearer` reply. `Ok(())` → [`Accepted`]. An
    /// `Err(msg)` whose text begins with the pinned no-capability prefix →
    /// [`NoCapability`]; any other error (transport failure, a wedged agent, …) →
    /// [`Unreachable`].
    ///
    /// This is the *one* place the prefix match lives, so a client's IPC-client
    /// glue can hand its raw `Result<(), String>` here rather than re-implementing
    /// the C#-mirrored classification.
    ///
    /// [`Accepted`]: RefreshBearerOutcome::Accepted
    /// [`NoCapability`]: RefreshBearerOutcome::NoCapability
    /// [`Unreachable`]: RefreshBearerOutcome::Unreachable
    pub fn classify(reply: Result<(), &str>) -> Self {
        match reply {
            Ok(()) => Self::Accepted,
            Err(msg) if msg.starts_with(crate::sync::NO_CAPABILITY_ERROR_PREFIX) => {
                Self::NoCapability
            }
            Err(_) => Self::Unreachable,
        }
    }
}

/// The platform-specific hooks the convergence loop drives. The linux GTK client,
/// FaunaKit, and fauna-tui each implement this; the loop is shared Rust.
///
/// Every method is best-effort — an error path should resolve to the "do nothing
/// this tick" outcome rather than panic, because the loop simply retries next tick.
pub trait ProvisioningDelegate: Send + Sync {
    /// Probe the agent's IPC seam and, if it is absent, spawn it. Returns whether
    /// the agent was **already** running: `false` means a spawn was just attempted,
    /// so the loop grants a spawn grace before leaning on it.
    fn ensure_agent_running(&self) -> impl Future<Output = bool> + Send;

    /// The current nest bearer (token + optional unix-seconds expiry), or `None`
    /// when the user is not authenticated — the tick then skips (nothing to push).
    fn bearer(&self) -> impl Future<Output = Option<BearerToken>> + Send;

    /// Push a `RefreshBearer` to the agent and classify the reply.
    fn refresh_bearer(
        &self,
        bearer: &BearerToken,
    ) -> impl Future<Output = RefreshBearerOutcome> + Send;

    /// Push a full `ProvisionCapability` to the agent (resolve the capability +
    /// send it). Called after [`refresh_bearer`](Self::refresh_bearer) reported
    /// [`RefreshBearerOutcome::NoCapability`], **and** on an otherwise-[`Accepted`]
    /// tick when [`needs_reprovision`](Self::needs_reprovision) is `true` (the
    /// capability's app-controlled *content* changed while its bearer stayed
    /// valid). A delegate that tracks a last-provisioned marker for
    /// [`needs_reprovision`] must advance it here, and only on a successful push,
    /// so a failed re-provision is retried on the next tick.
    ///
    /// [`Accepted`]: RefreshBearerOutcome::Accepted
    fn full_provision(&self, bearer: &BearerToken) -> impl Future<Output = ()> + Send;

    /// Whether the agent's currently-provisioned capability is **stale in its
    /// content** — the bearer is still valid (so [`refresh_bearer`] returns
    /// [`Accepted`], never [`NoCapability`]), but the app has since pushed a newer
    /// content-key binding set that the agent has not yet received. When `true`,
    /// [`run`]/[`tick`] re-drive [`full_provision`] on the Accepted path so the
    /// agent re-keys the affected engines (its engine-stamp machinery restarts an
    /// engine whose key material changed). Default `false` — a delegate with no
    /// mutable capability content never re-provisions on a valid bearer.
    ///
    /// **Async** because the freshest inputs are the agent's own — the
    /// bound-(3) license (`sync-agent.md` § Credential model → *Bound (3)'s
    /// enforcement design*, ruling 2) is a predicate recomputed from a live
    /// `ListEngines` answer at every provision, not a latch the app can hold.
    /// A delegate that only tracks locally-known staleness simply ignores the
    /// await.
    ///
    /// [`Accepted`]: RefreshBearerOutcome::Accepted
    /// [`NoCapability`]: RefreshBearerOutcome::NoCapability
    fn needs_reprovision(&self) -> impl Future<Output = bool> + Send {
        async { false }
    }

    /// Fired by [`run`] on the **rising edge** of agent reachability — the first
    /// tick after a not-reachable→reachable transition (and again after a
    /// down→up cycle). A client that reconciles device-local folder
    /// bindings *once* at attach uses this to re-drive that reconcile the moment
    /// the agent actually answers, closing the first-launch race
    /// where attach's single reconcile beats the just-spawned agent's socket
    /// coming up (`sync-agent.md` § Control plane split). Plain reachability is
    /// enough to re-drive folder binding — `AddLocation` is a config write that
    /// needs no capability. Default: no-op (clients that don't reconcile at
    /// attach — e.g. fauna-tui — need not implement it).
    fn agent_became_reachable(&self) -> impl Future<Output = ()> + Send {
        async {}
    }
}

/// One convergence pass: probe/spawn → (grace) → bearer → refresh → re-provision
/// on `NoCapability`. Exposed for unit tests; production drives it via [`run`].
///
/// Returns whether the agent **answered** this tick — `true` iff a bearer was
/// available and the `RefreshBearer` classified as [`Accepted`] or
/// [`NoCapability`] (both mean the socket is up and talking). A missing bearer
/// (not authenticated) or an [`Unreachable`] exchange returns `false`. [`run`]
/// tracks the false→true edge of this signal to fire
/// [`ProvisioningDelegate::agent_became_reachable`].
///
/// [`Accepted`]: RefreshBearerOutcome::Accepted
/// [`NoCapability`]: RefreshBearerOutcome::NoCapability
/// [`Unreachable`]: RefreshBearerOutcome::Unreachable
pub async fn tick<D: ProvisioningDelegate>(delegate: &D, spawn_grace: Duration) -> bool {
    let was_running = delegate.ensure_agent_running().await;
    if was_running {
        tracing::debug!("convergence tick: agent already running");
    } else {
        // The agent was just spawned — give its IPC seam a moment to come up
        // before the RefreshBearer, exactly as the C# service does.
        tracing::info!("convergence tick: agent was not running, spawned it; granting spawn grace");
        tokio::time::sleep(spawn_grace).await;
    }

    let Some(bearer) = delegate.bearer().await else {
        tracing::debug!("convergence tick: no bearer (not authenticated) — skipping");
        return false; // not authenticated — nothing to push this tick, not reachable
    };
    tracing::debug!("convergence tick: bearer present");

    match delegate.refresh_bearer(&bearer).await {
        RefreshBearerOutcome::NoCapability => {
            tracing::warn!("convergence tick: RefreshBearer -> NoCapability, full-provisioning");
            delegate.full_provision(&bearer).await;
            tracing::info!("convergence tick: full_provision complete (NoCapability path)");
            true // agent answered (it just holds no capability yet)
        }
        RefreshBearerOutcome::Accepted => {
            tracing::debug!("convergence tick: RefreshBearer -> Accepted");
            // The bearer is valid and the agent holds a capability, but its
            // app-controlled content (the content-key bindings) may have gone
            // stale — a set was shared/bound/rotated since the last provision.
            // Re-push so the agent re-keys the affected engine. Self-retrying:
            // the delegate advances its last-provisioned marker only on success.
            if delegate.needs_reprovision().await {
                tracing::info!(
                    "convergence tick: content stale on a valid bearer, full-provisioning"
                );
                delegate.full_provision(&bearer).await;
                tracing::info!("convergence tick: full_provision complete (reprovision path)");
            }
            true
        }
        RefreshBearerOutcome::Unreachable => {
            tracing::debug!("convergence tick: RefreshBearer -> Unreachable");
            false
        }
    }
}

/// Run the convergence loop until `cancel` is notified. Ticks every
/// `tick_interval`, or immediately when `poke` is notified (sign-in, a new
/// binding, a nest move). Each tick is best-effort — the delegate absorbs its own
/// errors and the loop just retries.
pub async fn run<D: ProvisioningDelegate>(
    delegate: D,
    tick_interval: Duration,
    spawn_grace: Duration,
    poke: Arc<Notify>,
    cancel: Arc<Notify>,
) {
    // Track the reachability edge so a client that reconciles device-local state
    // once at attach can re-drive it the moment the agent first answers (and
    // again after a down→up bounce). Starts `false`: the first reachable tick is
    // a rising edge and fires the hook.
    let mut was_reachable = false;
    loop {
        let reachable = tick(&delegate, spawn_grace).await;
        if reachable && !was_reachable {
            tracing::info!("convergence: agent became reachable (rising edge)");
            delegate.agent_became_reachable().await;
        } else if !reachable && was_reachable {
            tracing::warn!("convergence: agent became unreachable (falling edge)");
        }
        was_reachable = reachable;
        tokio::select! {
            _ = tokio::time::sleep(tick_interval) => {}
            _ = poke.notified() => {}
            _ = cancel.notified() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Calls {
        ensure: usize,
        refresh: usize,
        provision: usize,
    }

    struct Fake {
        calls: Arc<Mutex<Calls>>,
        already_running: bool,
        bearer: Option<(String, u64)>,
        outcome: RefreshBearerOutcome,
        needs_reprovision: bool,
    }

    impl Fake {
        /// The common shape: agent up, a valid bearer, no pending content change.
        fn up(outcome: RefreshBearerOutcome) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Calls::default())),
                already_running: true,
                bearer: Some(("tok".into(), 4_000_000_000)),
                outcome,
                needs_reprovision: false,
            }
        }
    }

    impl ProvisioningDelegate for Fake {
        fn ensure_agent_running(&self) -> impl Future<Output = bool> + Send {
            let calls = self.calls.clone();
            let running = self.already_running;
            async move {
                calls.lock().unwrap().ensure += 1;
                running
            }
        }
        fn bearer(&self) -> impl Future<Output = Option<BearerToken>> + Send {
            let b = self.bearer.clone();
            async move { b.map(|(t, e)| BearerToken::new(t, e)) }
        }
        fn refresh_bearer(
            &self,
            _bearer: &BearerToken,
        ) -> impl Future<Output = RefreshBearerOutcome> + Send {
            let calls = self.calls.clone();
            let outcome = self.outcome;
            async move {
                calls.lock().unwrap().refresh += 1;
                outcome
            }
        }
        fn full_provision(&self, _bearer: &BearerToken) -> impl Future<Output = ()> + Send {
            let calls = self.calls.clone();
            async move {
                calls.lock().unwrap().provision += 1;
            }
        }
        fn needs_reprovision(&self) -> impl Future<Output = bool> + Send {
            let needs = self.needs_reprovision;
            async move { needs }
        }
    }

    #[test]
    fn classify_maps_the_pinned_prefix() {
        use crate::sync::NO_CAPABILITY_ERROR_MESSAGE;
        assert_eq!(
            RefreshBearerOutcome::classify(Ok(())),
            RefreshBearerOutcome::Accepted
        );
        // The exact producer message classifies as NoCapability...
        assert_eq!(
            RefreshBearerOutcome::classify(Err(NO_CAPABILITY_ERROR_MESSAGE)),
            RefreshBearerOutcome::NoCapability
        );
        // ...and so does anything sharing the pinned prefix.
        assert_eq!(
            RefreshBearerOutcome::classify(Err("no capability provisioned (whatever)")),
            RefreshBearerOutcome::NoCapability
        );
        // An unrelated transport error is Unreachable, not a re-provision trigger.
        assert_eq!(
            RefreshBearerOutcome::classify(Err("connection refused")),
            RefreshBearerOutcome::Unreachable
        );
    }

    #[tokio::test]
    async fn tick_skips_refresh_when_not_authenticated() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let fake = Fake {
            calls: calls.clone(),
            already_running: true,
            bearer: None,
            outcome: RefreshBearerOutcome::Accepted,
            needs_reprovision: false,
        };
        let reachable = tick(&fake, Duration::from_millis(0)).await;
        assert!(!reachable, "no bearer → not reachable this tick");
        let c = calls.lock().unwrap();
        assert_eq!(c.ensure, 1, "must always probe the agent");
        assert_eq!(c.refresh, 0, "no bearer → skip RefreshBearer");
        assert_eq!(c.provision, 0);
    }

    #[tokio::test]
    async fn tick_reprovisions_on_no_capability() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let fake = Fake {
            calls: calls.clone(),
            already_running: true,
            bearer: Some(("tok".into(), 9_999)),
            outcome: RefreshBearerOutcome::NoCapability,
            needs_reprovision: false,
        };
        let reachable = tick(&fake, Duration::from_millis(0)).await;
        assert!(
            reachable,
            "NoCapability means the agent answered ⇒ reachable"
        );
        let c = calls.lock().unwrap();
        assert_eq!(c.refresh, 1);
        assert_eq!(c.provision, 1, "NoCapability → full ProvisionCapability");
    }

    #[tokio::test]
    async fn tick_does_not_reprovision_when_accepted() {
        for outcome in [
            RefreshBearerOutcome::Accepted,
            RefreshBearerOutcome::Unreachable,
        ] {
            let calls = Arc::new(Mutex::new(Calls::default()));
            let fake = Fake {
                calls: calls.clone(),
                already_running: true,
                bearer: Some(("tok".into(), 4_000_000_000)),
                outcome,
                needs_reprovision: false,
            };
            let reachable = tick(&fake, Duration::from_millis(0)).await;
            assert_eq!(
                reachable,
                outcome == RefreshBearerOutcome::Accepted,
                "{outcome:?}: Accepted is reachable, Unreachable is not"
            );
            let c = calls.lock().unwrap();
            assert_eq!(c.refresh, 1);
            assert_eq!(c.provision, 0, "{outcome:?} must not re-provision");
        }
    }

    #[tokio::test]
    async fn tick_reprovisions_when_accepted_and_content_is_stale() {
        // A valid bearer (Accepted), but the app pushed a newer content-key blob
        // since the last provision (needs_reprovision) — the loop re-sends a full
        // ProvisionCapability so the agent re-keys the affected engine. This is
        // the path that heals the post-agent-cutover keyless-bound-set regression.
        let fake = Fake {
            needs_reprovision: true,
            ..Fake::up(RefreshBearerOutcome::Accepted)
        };
        let calls = fake.calls.clone();
        let reachable = tick(&fake, Duration::from_millis(0)).await;
        assert!(reachable, "Accepted ⇒ reachable");
        let c = calls.lock().unwrap();
        assert_eq!(c.refresh, 1);
        assert_eq!(
            c.provision, 1,
            "stale content on a valid bearer must re-provision"
        );
    }

    #[tokio::test]
    async fn tick_does_not_reprovision_when_unreachable_even_if_content_is_stale() {
        // Unreachable short-circuits before the needs_reprovision check — a push
        // to a dead agent is pointless; the loop retries on the next tick.
        let fake = Fake {
            needs_reprovision: true,
            ..Fake::up(RefreshBearerOutcome::Unreachable)
        };
        let calls = fake.calls.clone();
        let reachable = tick(&fake, Duration::from_millis(0)).await;
        assert!(!reachable, "Unreachable ⇒ not reachable");
        assert_eq!(
            calls.lock().unwrap().provision,
            0,
            "unreachable → no push, retry next tick"
        );
    }

    #[tokio::test]
    async fn run_ticks_then_stops_on_cancel() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let fake = Fake {
            calls: calls.clone(),
            already_running: true,
            bearer: Some(("tok".into(), 4_000_000_000)),
            outcome: RefreshBearerOutcome::Accepted,
            needs_reprovision: false,
        };
        let poke = Arc::new(Notify::new());
        let cancel = Arc::new(Notify::new());
        let cancel_signal = cancel.clone();

        let handle = tokio::spawn(run(
            fake,
            Duration::from_secs(3600), // long interval — only cancel ends it
            Duration::from_millis(0),
            poke,
            cancel,
        ));

        // Let the first tick land, then cancel.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_signal.notify_one();

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("run must stop promptly on cancel")
            .unwrap();

        assert!(
            calls.lock().unwrap().ensure >= 1,
            "run must tick at least once"
        );
    }

    /// A delegate that plays a scripted per-tick `RefreshBearer` outcome sequence
    /// and counts `agent_became_reachable` edges. It drives the loop's `poke` /
    /// `cancel` directly: on every tick it pokes to advance, and on the tick that
    /// exhausts the script it cancels instead — so `run` executes exactly
    /// `script.len()` ticks then stops, with no wall-clock dependence (the tick
    /// interval is set long enough to never fire).
    struct EdgeFake {
        script: Mutex<std::collections::VecDeque<RefreshBearerOutcome>>,
        edge_calls: Arc<Mutex<usize>>,
        poke: Arc<Notify>,
        cancel: Arc<Notify>,
    }

    impl ProvisioningDelegate for EdgeFake {
        async fn ensure_agent_running(&self) -> bool {
            true // always already-running: no spawn grace, one tick == one exchange
        }
        async fn bearer(&self) -> Option<BearerToken> {
            Some(BearerToken::new("tok".into(), 4_000_000_000)) // always authenticated
        }
        fn refresh_bearer(
            &self,
            _bearer: &BearerToken,
        ) -> impl Future<Output = RefreshBearerOutcome> + Send {
            // Pop this tick's scripted outcome and steer the loop synchronously
            // (this runs when `tick` *builds* the future, before the await), so the
            // poke/cancel permit is set before `run` reaches its select.
            let (outcome, exhausted) = {
                let mut s = self.script.lock().unwrap();
                let outcome = s.pop_front().unwrap_or(RefreshBearerOutcome::Unreachable);
                (outcome, s.is_empty())
            };
            if exhausted {
                self.cancel.notify_one();
            } else {
                self.poke.notify_one();
            }
            async move { outcome }
        }
        async fn full_provision(&self, _bearer: &BearerToken) {}
        fn agent_became_reachable(&self) -> impl Future<Output = ()> + Send {
            let calls = self.edge_calls.clone();
            async move {
                *calls.lock().unwrap() += 1;
            }
        }
    }

    /// Run `run` over a scripted outcome sequence and return how many times the
    /// rising-edge `agent_became_reachable` hook fired.
    async fn edge_fires_for(script: Vec<RefreshBearerOutcome>) -> usize {
        let edge_calls = Arc::new(Mutex::new(0usize));
        let poke = Arc::new(Notify::new());
        let cancel = Arc::new(Notify::new());
        let fake = EdgeFake {
            script: Mutex::new(script.into_iter().collect()),
            edge_calls: edge_calls.clone(),
            poke: poke.clone(),
            cancel: cancel.clone(),
        };
        // A 1-hour tick interval never fires; the fake's poke/cancel drive the loop.
        tokio::time::timeout(
            Duration::from_secs(5),
            run(
                fake,
                Duration::from_secs(3600),
                Duration::from_millis(0),
                poke,
                cancel,
            ),
        )
        .await
        .expect("scripted run must terminate via the fake's cancel");
        *edge_calls.lock().unwrap()
    }

    #[tokio::test]
    async fn agent_became_reachable_fires_once_on_the_rising_edge() {
        use RefreshBearerOutcome::*;
        // Reachable from the first tick and staying reachable ⇒ exactly one edge.
        assert_eq!(edge_fires_for(vec![Accepted, Accepted, Accepted]).await, 1);
        // NoCapability also counts as "the agent answered" ⇒ still an edge.
        assert_eq!(edge_fires_for(vec![NoCapability, NoCapability]).await, 1);
    }

    #[tokio::test]
    async fn agent_became_reachable_does_not_fire_while_unreachable() {
        use RefreshBearerOutcome::*;
        assert_eq!(edge_fires_for(vec![Unreachable, Unreachable]).await, 0);
    }

    #[tokio::test]
    async fn agent_became_reachable_refires_after_a_down_up_bounce() {
        use RefreshBearerOutcome::*;
        // up → down → up ⇒ two rising edges.
        assert_eq!(
            edge_fires_for(vec![Accepted, Unreachable, Accepted]).await,
            2
        );
        // A late first-reach (raced spawn) still fires once the agent answers.
        assert_eq!(
            edge_fires_for(vec![Unreachable, Unreachable, NoCapability]).await,
            1
        );
    }
}
