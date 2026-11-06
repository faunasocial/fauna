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

/// The floor of the start-up re-probe's first wait, so a zero spawn grace
/// cannot turn [`run`]'s doubling backoff into a busy loop.
const MIN_STARTUP_REPROBE: Duration = Duration::from_millis(100);

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
    /// Returns whether the agent **took** the capability. On the
    /// [`NoCapability`](RefreshBearerOutcome::NoCapability) path that answer is
    /// the tick's reachability: an agent holding no capability serves no
    /// account's binding store, so the rising edge
    /// ([`agent_became_reachable`](Self::agent_became_reachable)) waits for a
    /// provision it took (`sync-agent.md` § Control plane split, *the binding
    /// store is an account's*).
    ///
    /// [`Accepted`]: RefreshBearerOutcome::Accepted
    fn full_provision(&self, bearer: &BearerToken) -> impl Future<Output = bool> + Send;

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
    /// coming up (`sync-agent.md` § Control plane split). **Reachable here means
    /// the agent holds a capability** — `RefreshBearer` accepted, or a
    /// provision it took — because a binding write needs the account's store,
    /// which only a provision opens: an agent with no account refuses it
    /// (`fauna_ipc::sync::BINDINGS_NEED_AN_ACCOUNT_PREFIX`), so an edge fired
    /// on mere reachability would push the pending binds into a refusal and
    /// leave them there until the next gesture. Default: no-op (clients that
    /// don't reconcile at attach — e.g. fauna-tui — need not implement it).
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
    tick_once(delegate, spawn_grace, true).await.reachable
}

/// What one pass did: whether the agent answered (see [`tick`]) and whether
/// the pass spawned it.
struct TickOutcome {
    reachable: bool,
    spawned: bool,
}

/// [`tick`]'s body. `may_spawn` is `false` while [`run`] is re-probing an
/// agent it spawned that has not answered yet: the probe-and-spawn is skipped
/// and the pass goes straight to the `RefreshBearer`, whose connect is the
/// probe — a socket missing during start-up means *still starting*, and a
/// second spawn then only starts a duplicate the instance lock turns away.
async fn tick_once<D: ProvisioningDelegate>(
    delegate: &D,
    spawn_grace: Duration,
    may_spawn: bool,
) -> TickOutcome {
    let mut spawned = false;
    if !may_spawn {
        tracing::debug!("convergence tick: re-probing the agent it spawned");
    } else if delegate.ensure_agent_running().await {
        tracing::debug!("convergence tick: agent already running");
    } else {
        // The agent was just spawned — give its IPC seam a moment to come up
        // before the RefreshBearer, exactly as the C# service does.
        tracing::info!("convergence tick: agent was not running, spawned it; granting spawn grace");
        tokio::time::sleep(spawn_grace).await;
        spawned = true;
    }

    let Some(bearer) = delegate.bearer().await else {
        tracing::debug!("convergence tick: no bearer (not authenticated) — skipping");
        // Not authenticated — nothing to push this tick, not reachable.
        return TickOutcome {
            reachable: false,
            spawned,
        };
    };
    tracing::debug!("convergence tick: bearer present");

    let reachable = match delegate.refresh_bearer(&bearer).await {
        RefreshBearerOutcome::NoCapability => {
            tracing::warn!("convergence tick: RefreshBearer -> NoCapability, full-provisioning");
            // Reachable only once the agent took it: until then it serves no
            // account's binding store, and the edge's reconcile would meet a
            // refusal (`ProvisioningDelegate::agent_became_reachable`).
            let took = delegate.full_provision(&bearer).await;
            if took {
                tracing::info!("convergence tick: full_provision complete (NoCapability path)");
            } else {
                tracing::warn!(
                    "convergence tick: the agent did not take the capability; retrying next tick"
                );
            }
            took
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
    };
    TickOutcome { reachable, spawned }
}

/// The wait before the next start-up re-probe, or `None` once the start-up is
/// over — the agent answered, or the doubled wait would reach `tick_interval`,
/// after which ordinary ticks resume (and may spawn again: an agent that has
/// not come up by then is not coming).
fn next_startup_reprobe(
    current: Option<Duration>,
    outcome: &TickOutcome,
    spawn_grace: Duration,
    tick_interval: Duration,
) -> Option<Duration> {
    if outcome.reachable || !(outcome.spawned || current.is_some()) {
        return None;
    }
    let next = current.map_or(spawn_grace.max(MIN_STARTUP_REPROBE), |wait| wait * 2);
    (next < tick_interval).then_some(next)
}

/// Run the convergence loop until `cancel` is notified. Ticks every
/// `tick_interval`, or immediately when `poke` is notified (sign-in, a new
/// binding, a nest move). Each tick is best-effort — the delegate absorbs its own
/// errors and the loop just retries.
///
/// **After a spawn the loop re-probes, not re-ticks** (`sync-agent.md` § Control
/// plane split, *the binding store is an account's*): while the agent it spawned
/// has not answered, it waits the spawn grace doubled per miss rather than a
/// whole `tick_interval`, and spawns nothing more — a poke included. A
/// slow-starting agent is so provisioned within a beat of its socket coming up,
/// where the windows app used to start a duplicate 0.3 s after the first spawn
/// and then leave the agent unprovisioned for the rest of a 30 s tick.
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
    // `Some(wait)` while an agent this loop spawned has not answered yet.
    let mut startup: Option<Duration> = None;
    loop {
        let outcome = tick_once(&delegate, spawn_grace, startup.is_none()).await;
        let reachable = outcome.reachable;
        if reachable && !was_reachable {
            tracing::info!("convergence: agent became reachable (rising edge)");
            delegate.agent_became_reachable().await;
        } else if !reachable && was_reachable {
            tracing::warn!("convergence: agent became unreachable (falling edge)");
        }
        was_reachable = reachable;
        startup = next_startup_reprobe(startup, &outcome, spawn_grace, tick_interval);
        tokio::select! {
            _ = tokio::time::sleep(startup.unwrap_or(tick_interval)) => {}
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
        fn full_provision(&self, _bearer: &BearerToken) -> impl Future<Output = bool> + Send {
            let calls = self.calls.clone();
            async move {
                calls.lock().unwrap().provision += 1;
                true
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
        /// Whether each `full_provision` call is taken, in order; `true` once
        /// the script runs out.
        provisions: Mutex<std::collections::VecDeque<bool>>,
        /// The ticks run so far (one `RefreshBearer` each).
        ticks: Mutex<usize>,
        /// The tick each rising edge fired on, 1-based.
        edge_ticks: Arc<Mutex<Vec<usize>>>,
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
            *self.ticks.lock().unwrap() += 1;
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
        fn full_provision(&self, _bearer: &BearerToken) -> impl Future<Output = bool> + Send {
            let took = self.provisions.lock().unwrap().pop_front().unwrap_or(true);
            async move { took }
        }
        fn agent_became_reachable(&self) -> impl Future<Output = ()> + Send {
            let tick = *self.ticks.lock().unwrap();
            let edges = self.edge_ticks.clone();
            async move {
                edges.lock().unwrap().push(tick);
            }
        }
    }

    /// Run `run` over a scripted outcome sequence and return how many times the
    /// rising-edge `agent_became_reachable` hook fired.
    async fn edge_fires_for(script: Vec<RefreshBearerOutcome>) -> usize {
        edge_ticks_for(script, Vec::new()).await.len()
    }

    /// Run `run` over a scripted outcome sequence, each `full_provision` taken
    /// or not as `provisions` says, and return the tick each rising edge fired
    /// on.
    async fn edge_ticks_for(
        script: Vec<RefreshBearerOutcome>,
        provisions: Vec<bool>,
    ) -> Vec<usize> {
        let edge_ticks = Arc::new(Mutex::new(Vec::new()));
        let poke = Arc::new(Notify::new());
        let cancel = Arc::new(Notify::new());
        let fake = EdgeFake {
            script: Mutex::new(script.into_iter().collect()),
            provisions: Mutex::new(provisions.into_iter().collect()),
            ticks: Mutex::new(0),
            edge_ticks: edge_ticks.clone(),
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
        edge_ticks.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn agent_became_reachable_fires_once_on_the_rising_edge() {
        use RefreshBearerOutcome::*;
        // Reachable from the first tick and staying reachable ⇒ exactly one edge.
        assert_eq!(edge_fires_for(vec![Accepted, Accepted, Accepted]).await, 1);
        // NoCapability followed by a provision the agent took ⇒ still an edge.
        assert_eq!(edge_fires_for(vec![NoCapability, NoCapability]).await, 1);
    }

    /// The edge waits for a provision the agent TOOK: an agent holding no
    /// capability serves no account's binding store and refuses a binding
    /// write, so an edge fired on its mere answer would push the app's pending
    /// binds into a refusal and leave them there (`sync-agent.md` § Control
    /// plane split, *the binding store is an account's*).
    #[tokio::test]
    async fn the_rising_edge_waits_for_a_provision_the_agent_took() {
        use RefreshBearerOutcome::*;
        assert_eq!(
            edge_ticks_for(
                vec![NoCapability, NoCapability, Accepted],
                vec![false, true]
            )
            .await,
            vec![2],
            "a provision the agent did not take fires no edge; the one it took does"
        );
        // A re-provision on a valid bearer that fails leaves the agent holding
        // its capability: still reachable, the edge fired once at the start.
        assert_eq!(
            edge_ticks_for(vec![Accepted, Accepted], vec![false]).await,
            vec![1]
        );
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

    /// An agent that takes several probes to come up after its spawn: every
    /// probe-and-spawn finds no socket (as a probe during start-up does), and
    /// `RefreshBearer` answers only from its `answers_on`-th call. The first
    /// refresh pokes the loop, as the windows app's session install did 0.3 s
    /// after the first spawn; the rising edge cancels it.
    struct SlowStartFake {
        spawns: Arc<Mutex<usize>>,
        refreshes: Mutex<usize>,
        answers_on: usize,
        poke: Arc<Notify>,
        cancel: Arc<Notify>,
    }

    impl ProvisioningDelegate for SlowStartFake {
        fn ensure_agent_running(&self) -> impl Future<Output = bool> + Send {
            *self.spawns.lock().unwrap() += 1;
            async { false }
        }
        async fn bearer(&self) -> Option<BearerToken> {
            Some(BearerToken::new("tok".into(), 4_000_000_000))
        }
        fn refresh_bearer(
            &self,
            _bearer: &BearerToken,
        ) -> impl Future<Output = RefreshBearerOutcome> + Send {
            let n = {
                let mut n = self.refreshes.lock().unwrap();
                *n += 1;
                *n
            };
            if n == 1 {
                self.poke.notify_one();
            }
            let outcome = if n >= self.answers_on {
                RefreshBearerOutcome::Accepted
            } else {
                RefreshBearerOutcome::Unreachable
            };
            async move { outcome }
        }
        async fn full_provision(&self, _bearer: &BearerToken) -> bool {
            true
        }
        fn agent_became_reachable(&self) -> impl Future<Output = ()> + Send {
            self.cancel.notify_one();
            async {}
        }
    }

    /// The loop re-probes a spawned agent promptly and never spawns it twice —
    /// not on a poke, not on a miss — where it used to spawn a duplicate on
    /// the poke and then sleep a whole tick (`sync-agent.md` § Control plane
    /// split, *the binding store is an account's*). The tick interval is an
    /// hour: only the start-up re-probe can reach the answering call in time.
    #[tokio::test]
    async fn a_spawned_agent_is_reprobed_promptly_and_never_spawned_twice() {
        let spawns = Arc::new(Mutex::new(0usize));
        let poke = Arc::new(Notify::new());
        let cancel = Arc::new(Notify::new());
        let fake = SlowStartFake {
            spawns: spawns.clone(),
            refreshes: Mutex::new(0),
            answers_on: 5,
            poke: poke.clone(),
            cancel: cancel.clone(),
        };
        tokio::time::timeout(
            Duration::from_secs(20),
            run(
                fake,
                Duration::from_secs(3600),
                Duration::from_millis(1),
                poke,
                cancel,
            ),
        )
        .await
        .expect("the start-up re-probe must reach the agent long before the hour's tick");
        assert_eq!(
            *spawns.lock().unwrap(),
            1,
            "a probe during start-up must never spawn a second agent"
        );
    }

    #[test]
    fn the_startup_reprobe_doubles_from_the_grace_and_ends_at_the_tick_interval() {
        let grace = Duration::from_millis(300);
        let tick = Duration::from_secs(30);
        let missed = |spawned| TickOutcome {
            reachable: false,
            spawned,
        };
        let answered = TickOutcome {
            reachable: true,
            spawned: false,
        };
        let mut waits = Vec::new();
        let mut wait = next_startup_reprobe(None, &missed(true), grace, tick);
        while let Some(w) = wait {
            waits.push(w.as_millis());
            wait = next_startup_reprobe(wait, &missed(false), grace, tick);
        }
        assert_eq!(
            waits,
            vec![300, 600, 1_200, 2_400, 4_800, 9_600, 19_200],
            "doubling from the grace, ending before the tick interval"
        );
        assert_eq!(
            next_startup_reprobe(Some(grace), &answered, grace, tick),
            None,
            "an answer ends the start-up"
        );
        assert_eq!(
            next_startup_reprobe(None, &missed(false), grace, tick),
            None,
            "a miss with no spawn behind it is an ordinary tick"
        );
        assert_eq!(
            next_startup_reprobe(None, &missed(true), Duration::ZERO, tick),
            Some(MIN_STARTUP_REPROBE),
            "a zero grace still waits the floor"
        );
    }
}
