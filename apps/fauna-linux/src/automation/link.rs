//! Bridge between the automation HTTP server and the in-app test-agent state.
//!
//! The `/element/*` ops are ref-free (they walk the live widget tree), but
//! `/app/{state,commands}` need the test agent's `SharedState` + command
//! channel, which `start_test_agent_if_enabled` (re)creates per onboarding→main
//! stage. This module holds the *latest* stage's link in a global the server
//! thread reads, so the agent can serve the state protocol without the AT-SPI
//! bridge relaying it. `install` is called on every stage; the server always
//! sees the current wiring.

use crate::test_agent::{RawCommand, SharedState};
use serde_json::{Value, json};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

pub struct AgentLink {
    shared: Arc<Mutex<SharedState>>,
    cmd_tx: Sender<RawCommand>,
}

static LINK: Mutex<Option<AgentLink>> = Mutex::new(None);

/// The command ack, held PROCESS-wide rather than in the stage's `SharedState`.
///
/// The ack protocol is process-scoped — the driver mints one command-id sequence
/// per app process and polls `/app/state` until its id comes back ready. But
/// `SharedState` is *stage*-scoped: `start_test_agent_if_enabled` mints a fresh
/// one per stage and `install` re-points the link at it. A stage swap racing an
/// in-flight command therefore stranded the ack on the OUTGOING `SharedState`
/// while the driver polled the INCOMING one, which reports `last_command_id: ""`
/// forever — so `set_state` hung to its timeout even though the command had been
/// applied. Both stages run on the GTK thread, so the two are serialized, never
/// interleaved; either order stranded the ack.
///
/// A credentialed relaunch is the reproducer: the app wires a launch-screen stage,
/// then the silent challenge completes and wires the authenticated stage. Any
/// `set_state` landing in that window hung, which read as "the GTK thread is
/// wedged" — it never was; the app was fine and the ack was simply written to a
/// `SharedState` nobody was reading any more.
///
/// Keeping the ack here makes it survive the swap: whichever stage's drain applies
/// the command records it, and `app_state` always reports it.
struct Ack {
    id: String,
    ready: bool,
}

static ACK: Mutex<Ack> = Mutex::new(Ack {
    id: String::new(),
    ready: true,
});

/// Mark a command in flight (called by `inject_command` on the server thread).
fn mark_pending(id: &str) {
    let mut a = ACK.lock().unwrap_or_else(|e| e.into_inner());
    a.id = id.to_string();
    a.ready = false;
}

/// Record a command as applied. Called by the GTK drain's ack site
/// (`handle_test_command`) for whichever stage actually handled it.
pub fn record_ack(id: &str) {
    let mut a = ACK.lock().unwrap_or_else(|e| e.into_inner());
    a.id = id.to_string();
    a.ready = true;
}

fn ack_snapshot() -> (String, bool) {
    let a = ACK.lock().unwrap_or_else(|e| e.into_inner());
    (a.id.clone(), a.ready)
}

/// The `barrier` self-test probe's applied token
/// ([`fauna_e2e_agent::BARRIER_PROBE`]), `None` until one runs.
///
/// Process-wide for the same reason [`Ack`] is: the probe is queued as a glib
/// idle callback, which can outlive the stage that queued it, and a stage swap
/// would otherwise strand the token on an orphaned `SharedState`. `reset`/
/// `logout` clear it, matching tui's `App::barrier_probe` one-test lifetime.
static BARRIER_PROBE: Mutex<Option<String>> = Mutex::new(None);

/// Record an applied probe token — called from the idle callback, i.e. from the
/// queue position the barrier's own ack idle must land *after*.
pub fn record_barrier_probe(token: &str) {
    *BARRIER_PROBE.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.to_string());
}

/// The token for `state.barrier_probe`.
pub fn barrier_probe() -> Option<String> {
    BARRIER_PROBE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// What [`BARRIER_PROBE`] held when the last `barrier` acked, frozen — the only
/// value the self-test asserts. See [`fauna_e2e_agent::BARRIER_ACK_PROBE_KEY`]
/// for why the live token above cannot carry that proof (linux republishes state
/// on its own 50 ms tick, so a post-ack read sees a drained queue regardless).
static BARRIER_ACK_PROBE: Mutex<Option<String>> = Mutex::new(None);

/// Freeze the current probe token as the barrier's ack-time observable. Called
/// from inside the barrier's idle callback — i.e. after every idle queued before
/// it has run, and before the ack.
pub fn freeze_barrier_ack_probe() {
    let seen = barrier_probe();
    *BARRIER_ACK_PROBE.lock().unwrap_or_else(|e| e.into_inner()) = seen;
}

/// The value for `state.barrier_ack_probe`.
pub fn barrier_ack_probe() -> Option<String> {
    BARRIER_ACK_PROBE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// Clear both probe slots — the `reset`/`logout` clear point, so a token cannot
/// leak into the next test of a reused app process.
pub fn clear_barrier_probe() {
    *BARRIER_PROBE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *BARRIER_ACK_PROBE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// How many authenticated-session teardowns this process has **initiated** —
/// [`fauna_e2e_agent::SESSION_GENERATION_KEY`], the observable convention 14's
/// negative asserts read to prove a gesture did not relaunch the session.
///
/// Process-wide, and deliberately NOT cleared by `reset`/`logout` the way the
/// probe slots above are. Those are per-test scratch; this counts teardowns, and
/// a logout IS one — zeroing it there would erase the very event the counter
/// exists to report. Tests read a delta (before vs. after their own gesture), so
/// an accumulating value costs them nothing.
static SESSION_GENERATION: Mutex<u64> = Mutex::new(0);

/// Count one initiated teardown.
///
/// ⚠ **Call this synchronously in the handler that decides to tear down, BEFORE
/// the `glib::timeout_add_local_once` deferral** — never from inside the
/// deferred closure. Every linux teardown (switch, sign-out, factory reset)
/// defers its real work by 100 ms so the dialog that triggered it can release
/// its modal grab, and `barrier` is a glib **idle** round trip: an idle runs
/// long before a 100 ms timeout, so a counter bumped inside the deferred closure
/// is invisible to the barrier and the negative assert silently reverts to the
/// race it replaced. The synchronous bump is the initiation marker convention 14
/// asks the product for.
pub fn record_session_teardown() {
    let mut count = SESSION_GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    *count = count.saturating_add(1);
}

/// An account switch landed: the agent's `set_state` session override now
/// describes the session the switch is tearing down, so hand it over to the
/// switched-to account. The identity half — `actor_id`, `handle`, and the
/// `authenticated` pin — is dropped, so the state provider reports the live
/// `AppState`'s actor once the rebuilt shell mounts and `false` until then
/// (never the outgoing actor through the teardown); `node_url`/`secret_hex`
/// become the target's own slots; `device_id` is per-device and survives.
///
/// Without this, every switch a fixture-seeded seat makes reads back as the
/// predecessor for good — the override wins over `AppState` — and
/// `helpers/succession_ceremony.wait_for_successor_actor`'s live-session wait
/// never passes (every linux succession journey, red since that wait landed
/// 2026-09-28). Synchronous, beside [`record_session_teardown`], for the same
/// reason: a barrier read between the trigger and the deferred teardown must
/// not see the outgoing session as live.
pub fn adopt_switched_session(node_url: &str, secret_hex: &str) {
    with_link(|l| {
        let mut s = l.shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ov) = s.session_override.as_mut() {
            ov.authenticated = None;
            ov.actor_id = None;
            ov.handle = None;
            ov.node_url = Some(node_url.to_string());
            ov.secret_hex = Some(secret_hex.to_string());
        }
    });
}

/// The value for `state.session_generation`.
pub fn session_generation() -> u64 {
    *SESSION_GENERATION.lock().unwrap_or_else(|e| e.into_inner())
}

/// Install (or replace) the current stage's state link. Called by
/// `start_test_agent_if_enabled` each time it wires a stage.
pub fn install(shared: Arc<Mutex<SharedState>>, cmd_tx: Sender<RawCommand>) {
    *LINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(AgentLink { shared, cmd_tx });
}

fn with_link<R>(f: impl FnOnce(&AgentLink) -> R) -> Option<R> {
    LINK.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(f)
}

/// The `/app/state` body: `{last_command_id, ready, state}` — the shape the
/// driver's `set_state`/`get_state` poll. Defaults to ready-with-null-state
/// before any stage installs a link.
///
/// `last_command_id`/`ready` come from the process-wide `ACK` (see its docs), not
/// from the stage's `SharedState`, so an ack survives a stage swap. `state` still
/// comes from the current stage — that is the window the driver wants to read.
pub fn app_state() -> Value {
    let (id, ready) = ack_snapshot();
    let state = with_link(|l| {
        let s = l.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.state_json.clone()
    })
    .unwrap_or(Value::Null);
    json!({ "last_command_id": id, "ready": ready, "state": state })
}

/// Handle `POST /app/commands`: mark not-ready, inject the command into the GTK
/// drain, and return immediately — the driver polls `/app/state` for the ack
/// (matching `http_bridge.py::set_state`). The 50ms GTK drain picks it up and
/// sets `ready`/`last_command_id` once applied.
pub fn inject_command(body: &Value) -> Value {
    let id = body
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("patch")
        .to_string();
    let state = body.get("state").cloned();

    mark_pending(&id);

    let sent = with_link(|l| {
        {
            // The stage's own copy still drives the AT-SPI bridge's `await_ack`
            // (`test_agent.rs`); the process-wide ACK above is what `app_state`
            // reports.
            let mut s = l.shared.lock().unwrap_or_else(|e| e.into_inner());
            s.ready = false;
        }
        l.cmd_tx
            .send(RawCommand {
                id: id.clone(),
                action,
                state,
                payload: body.clone(),
            })
            .is_ok()
    })
    .unwrap_or(false);

    if !sent {
        // Never leave a dropped command looking merely slow: the driver polls
        // `/app/state` for the ack and would otherwise hang to its timeout, which
        // reads as a product bug rather than a dropped command. A test agent must
        // honour a command or fail loudly — never silently drop it.
        record_ack(&id);
        return json!({ "error": "agent link not installed" });
    }
    json!({ "ok": true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_agent::SessionOverride;

    /// A switch hands the fixture's session override to the switched-to
    /// account: the outgoing identity is forgotten (so the live `AppState`
    /// answers), the target's slots replace the URL/secret, and the device id
    /// survives. Re-pinning the outgoing actor here is exactly the red every
    /// linux succession journey carried.
    #[test]
    fn a_switch_forgets_the_outgoing_identity_and_keeps_the_device() {
        let shared = Arc::new(Mutex::new(SharedState::default()));
        shared.lock().unwrap().session_override = Some(SessionOverride {
            authenticated: Some(true),
            node_url: Some("http://old".into()),
            secret_hex: Some("aa".repeat(32)),
            actor_id: Some("bb".repeat(32)),
            handle: Some("old-handle".into()),
            device_id: Some("cc".repeat(32)),
        });
        let (tx, _rx) = std::sync::mpsc::channel();
        install(Arc::clone(&shared), tx);

        adopt_switched_session("http://new", &"dd".repeat(32));

        let s = shared.lock().unwrap();
        let ov = s.session_override.as_ref().expect("the override stays");
        assert_eq!(ov.actor_id, None);
        assert_eq!(ov.handle, None);
        assert_eq!(ov.authenticated, None);
        assert_eq!(ov.node_url.as_deref(), Some("http://new"));
        assert_eq!(ov.secret_hex, Some("dd".repeat(32)));
        assert_eq!(ov.device_id, Some("cc".repeat(32)));
    }
}
