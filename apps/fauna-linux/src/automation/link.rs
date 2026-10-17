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

/// The current stage's link, plus every command that arrived before the FIRST
/// stage installed one.
///
/// The element server answers `/health` from `start_element_agent_once`, which
/// runs at the top of `start_test_agent_if_enabled` — before that same call
/// reaches [`install`]. The driver's launch returns on the first healthy
/// `/health` and the fixture's sign-in `set_state` follows at once, so on a
/// loaded box it can land in that window. It used to be acknowledged there and
/// dropped, with nothing logged: the app stayed on onboarding, `set_state`
/// returned cleanly, and the failure surfaced a minute later as "real FaunaMls
/// backend did not activate" (convention 11 — a test agent honours a command or
/// fails loudly). So a command with no link yet is HELD here, under the same lock
/// `install` takes, and delivered to the first stage's channel in arrival order;
/// it acks when that stage's drain applies it, as any other command does.
struct Slot {
    link: Option<AgentLink>,
    held: Vec<RawCommand>,
}

static LINK: Mutex<Slot> = Mutex::new(Slot {
    link: None,
    held: Vec::new(),
});

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

/// Undo [`mark_pending`] for a command that was NOT delivered: ready again, and
/// the id is not claimed — an ack for it would tell the driver it was applied.
fn clear_pending() {
    let mut a = ACK.lock().unwrap_or_else(|e| e.into_inner());
    a.id.clear();
    a.ready = true;
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
/// `start_test_agent_if_enabled` each time it wires a stage. Commands held
/// before the first install ([`Slot`]) go to this stage's channel first.
pub fn install(shared: Arc<Mutex<SharedState>>, cmd_tx: Sender<RawCommand>) {
    install_in(&LINK, shared, cmd_tx);
}

fn install_in(slot: &Mutex<Slot>, shared: Arc<Mutex<SharedState>>, cmd_tx: Sender<RawCommand>) {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    let held = std::mem::take(&mut slot.held);
    if !held.is_empty() {
        let ids: Vec<&str> = held.iter().map(|c| c.id.as_str()).collect();
        tracing::info!(
            "[agent-link] stage link installed: delivering {} command(s) held since \
             before it ({})",
            held.len(),
            ids.join(", ")
        );
        shared.lock().unwrap_or_else(|e| e.into_inner()).ready = false;
    }
    for cmd in held {
        // The receiver lives in this stage's drain, created just before this
        // call, so a send cannot fail here; if it ever did the command would be
        // gone, so say so rather than lose it quietly.
        let id = cmd.id.clone();
        if cmd_tx.send(cmd).is_err() {
            tracing::error!(
                "[agent-link] held command {id} could not be delivered: channel closed"
            );
        }
    }
    slot.link = Some(AgentLink { shared, cmd_tx });
}

fn with_link<R>(f: impl FnOnce(&AgentLink) -> R) -> Option<R> {
    LINK.lock()
        .unwrap_or_else(|e| e.into_inner())
        .link
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
/// sets `ready`/`last_command_id` once applied. A command that arrives before
/// any stage has installed a link is held for the first one ([`Slot`]).
pub fn inject_command(body: &Value) -> Value {
    inject_in(&LINK, body)
}

fn inject_in(slot: &Mutex<Slot>, body: &Value) -> Value {
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
    let cmd = RawCommand {
        id: id.clone(),
        action,
        state: body.get("state").cloned(),
        payload: body.clone(),
    };

    mark_pending(&id);

    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(link) = slot.link.as_ref() else {
        tracing::info!(
            "[agent-link] command {id} ({}) arrived before the stage link was installed — \
             holding it for the first install",
            cmd.action
        );
        slot.held.push(cmd);
        return json!({ "ok": true, "held": true });
    };
    {
        // The stage's own copy still drives the AT-SPI bridge's `await_ack`
        // (`test_agent.rs`); the process-wide ACK above is what `app_state`
        // reports.
        let mut s = link.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.ready = false;
    }
    if link.cmd_tx.send(cmd).is_err() {
        // Never ack a command nobody will apply — the driver would read it as
        // applied. The error body makes the agent answer non-2xx, so `set_state`
        // raises naming the drop instead of returning (convention 11).
        drop(slot);
        clear_pending();
        tracing::error!("[agent-link] command {id} dropped: the stage's command channel is closed");
        return json!({ "error": "agent command channel closed" });
    }
    json!({ "ok": true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_agent::SessionOverride;

    fn empty_slot() -> Mutex<Slot> {
        Mutex::new(Slot {
            link: None,
            held: Vec::new(),
        })
    }

    /// The launch window: `/health` already answers, no stage has installed a
    /// link yet, and the fixture's sign-in `set_state` arrives. The command must
    /// reach the first stage's drain — it used to be acked and dropped, which
    /// left the app on onboarding while the driver believed it signed in.
    #[test]
    fn a_command_before_the_first_install_reaches_that_stage() {
        let slot = empty_slot();
        let body = json!({ "id": "cmd_early", "action": "patch", "state": { "session": {} } });
        let reply = inject_in(&slot, &body);
        assert!(reply.get("error").is_none(), "held, not refused: {reply}");

        let (tx, rx) = std::sync::mpsc::channel();
        install_in(&slot, Arc::new(Mutex::new(SharedState::default())), tx);
        let cmd = rx
            .try_recv()
            .expect("the early command is delivered on install");
        assert_eq!(cmd.id, "cmd_early");
        assert_eq!(cmd.action, "patch");

        // A command after the install goes straight through, behind it.
        inject_in(&slot, &json!({ "id": "cmd_late", "action": "patch" }));
        assert_eq!(rx.try_recv().expect("delivered").id, "cmd_late");
    }

    /// A stage whose drain is gone cannot apply the command: it is refused with
    /// an error body (which the agent answers non-2xx) and its id is NOT acked.
    #[test]
    fn a_command_to_a_closed_channel_is_refused_unacked() {
        let slot = empty_slot();
        let (tx, rx) = std::sync::mpsc::channel();
        install_in(&slot, Arc::new(Mutex::new(SharedState::default())), tx);
        drop(rx);
        let reply = inject_in(&slot, &json!({ "id": "cmd_closed", "action": "patch" }));
        assert!(reply.get("error").is_some(), "refused: {reply}");
        assert_ne!(
            ack_snapshot().0,
            "cmd_closed",
            "a dropped command is never acked"
        );
    }

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
