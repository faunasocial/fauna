//! Generic E2E test agent for the unified state protocol.
//!
//! Polls the bridge for commands and pushes app state back.
//! Activated by the `FAUNA_E2E_BRIDGE` environment variable.
//!
//! This module is intentionally app-agnostic — it knows nothing about GTK,
//! windows, credentials, or navigation. The app provides two callbacks:
//! - `state_provider`: returns the current app state as JSON
//! - `command_handler`: processes a command (patch, reset, logout)
//!
//! This matches the iOS (TestAgent.swift) and Windows (TestAgent.cs) pattern.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Thread-safe shared state: the GTK main thread writes JSON here,
/// the background polling thread reads it for pushing to the bridge.
pub struct SharedState {
    /// Latest state JSON, written by GTK main thread.
    pub state_json: Value,
    /// ID of the last processed command.
    pub last_command_id: String,
    /// Session overrides from test patches (merged with keyring on read).
    pub session_override: Option<SessionOverride>,
    /// Warning text injected by test patches (the UI uses the error label
    /// for all message types, but state must report them separately).
    pub warning_text: Option<String>,
    /// Info text injected by test patches.
    pub info_text: Option<String>,
    /// A test-agent command that was refused, or honoured and then failed —
    /// surfaced on `messages.error` **ahead of** any page's own error
    /// (`e2e-conventions.md` § convention 11: "honour it or fail loudly on the
    /// app's own `error-message`").
    ///
    /// Its own slot, not a page's, for the reason tui's twin
    /// (`App::refused_agent_command`) already paid for: a refusal is not a
    /// property of whatever page is on screen, and the real-wire commands are
    /// driven from wherever the test happens to be. Cleared only by the agent's
    /// `reset` — a command that never happened poisons every later assertion, so
    /// it must not be wiped by an unrelated nav.
    pub agent_command_failure: Option<String>,
    /// True when the GTK thread is idle (no command in flight).
    /// Set false before sending a command to the GTK thread,
    /// set true by the GTK handler after the command is applied.
    pub ready: bool,
    /// Result of the most recent `rpc_echo` test command. `None` until
    /// the WS-RPC round trip finishes; populated by `FaunaClient::rpc_echo`
    /// on the tokio runtime so the Python e2e driver can verify
    /// `tests/e2e-unified/tests/test_sp_linux_ws_rpc_echo.py`.
    pub rpc_echo_reply: Option<RpcEchoOutcome>,
    /// Result of the most recent `enable_caldav_mailbox` test command. `None`
    /// until the async mint finishes; populated by
    /// `FaunaClient::enable_caldav_mailbox_for_test` on the tokio runtime so the
    /// Slice C e2e (`tests/e2e-unified/tests/test_caldav_autoschedule_mailbox_less.py`)
    /// can wait for the mailbox-less attendee's MSEK to be minted *before* the
    /// organizer PUTs the invite — so the attendee's `NestSchedulingSink` finds
    /// the key material it needs to materialize the event on the first drain.
    pub caldav_mailbox_reply: Option<CalDavMailboxOutcome>,
    /// Result of the most recent `serve_enable_folder` test command. `None` until
    /// the async serve-enable + blob reconcile finishes; populated by
    /// `FaunaClient::serve_enable_folder_for_test` on the tokio runtime so the
    /// WebDAV read+write tier_3 e2e
    /// (`tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py`) can wait for
    /// the served-set precondition (content-key genesis + the MSEK-sealed
    /// `WebdavKeysBlob`) to land *before* the WebDAV client PUTs/GETs the set.
    pub webdav_serve_reply: Option<WebdavServeOutcome>,
    /// JSON-encoded return value of the most recent `machine` (call_machine_method)
    /// command, for the value-returning bridge path. `None` for setter/command
    /// methods; `Some(json)` for readers (`provisioning_snapshot`,
    /// `provider_base_url`). The Python driver reads it back as
    /// `state.machine_method_result` so the fake-cloud orchestrator e2e can poll
    /// the live provisioning snapshot on linux (parity with web's
    /// value-returning `__fauna_callMachineMethod`).
    pub machine_method_result: Option<String>,
}

/// Outcome of a `rpc_echo` test command. Serialized into the test-agent
/// state as `rpc_echo_reply` for the Python driver.
#[derive(Clone)]
pub enum RpcEchoOutcome {
    Ok { data_hex: String },
    Err { error: String },
}

/// Outcome of an `enable_caldav_mailbox` test command. Serialized into the
/// test-agent state as `caldav_mailbox_reply` for the Python driver.
#[derive(Clone)]
pub enum CalDavMailboxOutcome {
    Ok,
    Err { error: String },
}

/// Outcome of a `serve_enable_folder` test command. Serialized into the
/// test-agent state as `webdav_serve_reply` for the Python driver
/// (`served_sets` = the number of served sets the reconciled `WebdavKeysBlob`
/// now carries — a signal the precondition landed).
#[derive(Clone)]
pub enum WebdavServeOutcome {
    Ok { served_sets: u64 },
    Err { error: String },
}

/// Session fields set by test patches. These take precedence over
/// whatever is in the keyring, allowing tests to set session state
/// without actually going through the full auth flow.
#[derive(Clone, Default)]
pub struct SessionOverride {
    pub authenticated: Option<bool>,
    pub node_url: Option<String>,
    pub secret_hex: Option<String>,
    pub actor_id: Option<String>,
    pub handle: Option<String>,
    pub device_id: Option<String>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            state_json: Value::Null,
            last_command_id: String::new(),
            session_override: None,
            warning_text: None,
            info_text: None,
            agent_command_failure: None,
            ready: true,
            rpc_echo_reply: None,
            caldav_mailbox_reply: None,
            webdav_serve_reply: None,
            machine_method_result: None,
        }
    }
}

/// A raw command from the bridge — action + optional state payload.
///
/// `state` is the legacy "patch"-shaped envelope (`{"action": "patch",
/// "state": {…}}`) — used by `handle_test_command` for the patch /
/// session-shape commands. `payload` carries the whole command JSON for
/// the newer flat-shape commands (e.g. unified-conversations bridge
/// commands send `{"action": "conversations_inject_inbound", "rail":
/// "Smtp", "sender": "…", …}` with fields at the top level), matching
/// Windows TestAgent.cs which reads `command["rail"]` directly.
#[derive(Clone)]
pub struct RawCommand {
    pub id: String,
    pub action: String,       // "patch", "reset", "logout"
    pub state: Option<Value>, // present for "patch" actions
    pub payload: Value,       // the full command JSON object
}

/// Start the test agent polling loop on a background thread, feeding the given
/// command sender. The caller owns the channel (it's shared with the
/// in-process agent server); the GTK main thread drains the matching receiver
/// in a `glib::timeout_add_local` callback.
pub fn start(
    bridge_url: String,
    shared: Arc<Mutex<SharedState>>,
    cmd_tx: std::sync::mpsc::Sender<RawCommand>,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(poll_loop(bridge_url, shared, cmd_tx));
    });
}

async fn poll_loop(
    bridge_url: String,
    shared: Arc<Mutex<SharedState>>,
    cmd_tx: std::sync::mpsc::Sender<RawCommand>,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut push_counter = 0u32;

    loop {
        match fetch_command(&client, &bridge_url).await {
            Ok(Some(cmd)) => {
                let id = cmd
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let action = cmd
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("patch")
                    .to_string();
                let state = cmd.get("state").cloned();
                let payload = cmd.clone();

                // Mark not-ready before handing off to the GTK thread.
                // The GTK handler sets ready=true after the command is applied,
                // so the Python driver only sees the ack once state is consistent.
                {
                    let mut state_guard = shared.lock().unwrap();
                    state_guard.ready = false;
                }

                // Send to GTK main thread for processing.
                // last_command_id is set by the GTK thread AFTER it finishes
                // handling the command, so the Python driver only sees the ack
                // once the state actually reflects the command's effects.
                let ack_id = id.clone();
                let _ = cmd_tx.send(RawCommand {
                    id,
                    action,
                    state,
                    payload,
                });

                // Eager ack: push state to the bridge the *moment* the GTK
                // thread finishes this command, instead of waiting up to ~1s
                // for the idle push cadence below. This shrinks the
                // command-ack latency floor from ~1s to ~25ms, widening
                // headroom against the Python driver's 10s ack budget
                // (`http_bridge.py`) under full-suite CPU contention.
                await_ack(&client, &bridge_url, &shared, &ack_id).await;
                push_counter = 0;
            }
            Ok(None) => {
                push_counter += 1;
                if push_counter >= 5 {
                    push_state(&client, &bridge_url, &shared).await;
                    push_counter = 0;
                }
            }
            Err(e) => {
                eprintln!("[TestAgent] Poll error: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn fetch_command(
    client: &reqwest::Client,
    bridge_url: &str,
) -> Result<Option<Value>, reqwest::Error> {
    let resp = client
        .get(format!("{bridge_url}/app/commands"))
        .send()
        .await?;
    if resp.status().as_u16() == 204 || !resp.status().is_success() {
        return Ok(None);
    }
    Ok(Some(resp.json().await?))
}

/// After a command is handed to the GTK main thread, push state to the bridge
/// as soon as the GTK thread acks it (sets `ready == true` and a matching
/// `last_command_id`) — rather than letting the ack wait for the ~1s idle push
/// cadence in `poll_loop`. Polls the shared state at a tight 25ms interval,
/// bounded so a genuinely wedged GTK thread can't pin the poll loop forever
/// (the Python driver's own 10s ack budget is the real ceiling; this bound sits
/// comfortably past it so we never give up before the driver does).
async fn await_ack(
    client: &reqwest::Client,
    bridge_url: &str,
    shared: &Arc<Mutex<SharedState>>,
    cmd_id: &str,
) {
    // 480 × 25ms ≈ 12s — past the driver's 10s ack budget but still bounded.
    for _ in 0..480 {
        let acked = {
            let s = shared.lock().unwrap_or_else(|e| e.into_inner());
            s.ready && s.last_command_id == cmd_id
        };
        if acked {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    // Push whether acked or bound-exhausted: on ack the driver sees the fresh
    // state immediately; on a stall the driver's own timeout handles it.
    push_state(client, bridge_url, shared).await;
}

async fn push_state(client: &reqwest::Client, bridge_url: &str, shared: &Arc<Mutex<SharedState>>) {
    let payload = {
        let s = shared.lock().unwrap_or_else(|e| e.into_inner());
        if s.state_json.is_null() {
            return; // No state to push yet
        }
        serde_json::json!({
            "last_command_id": s.last_command_id,
            "ready": s.ready,
            "state": s.state_json,
        })
    };
    let _ = client
        .post(format!("{bridge_url}/app/state"))
        .json(&payload)
        .send()
        .await;
}

// ---------------------------------------------------------------------------
// View name mapping — canonical schema ↔ GTK stack names
// ---------------------------------------------------------------------------

/// Map canonical view names (from test schema) to GTK stack page names.
///
/// `"settings"` now maps to itself — the inline Settings sidebar-swap shell (the
/// former `settings`⇒`status` inversion is gone). Several canonical views fold
/// into that shell as sub-pages, so they also target `"settings"`; the matching
/// sub-page is then selected by a second nav-stack entry `{"id":"<sub>"}` OR, for
/// the single-element legacy nav (`{"view":"devices"}`), derived from the view
/// name itself (see [`settings_subpage_for_view`] + the nav-patch walk in
/// `main.rs`):
/// - `"status"` → Status sub-page (the shell's first/default child).
/// - `"devices"` → Devices sub-page (the roster; the 2026-06-28 unification moved
///   the former top-level "Peers" page here, so the legacy `{"view":"devices"}`
///   still lands on the roster).
/// - `"folders"` → Folders sub-page (the folder control plane).
pub fn canonical_to_gtk(view: &str) -> &str {
    match view {
        "status" | "devices" | "folders" => "settings",
        other => other,
    }
}

/// For a single-element canonical nav whose view targets the Settings shell,
/// return the settings sub-stack child to select (so `{"view":"devices"}` /
/// `{"view":"folders"}` reach their sub-page without an explicit `{"id":...}`).
/// `None` for views that are not a settings sub-page alias.
pub fn settings_subpage_for_view(view: &str) -> Option<&'static str> {
    match view {
        "status" => Some("status"),
        "devices" => Some("devices"),
        "folders" => Some("folders"),
        _ => None,
    }
}

/// Map GTK stack page names to canonical view names (for test schema). The
/// settings sub-page → canonical mapping (`devices` / `folders`) is applied
/// separately in `main.rs`'s state report by walking the settings sub-stack — the
/// outer stack only ever reads back `"settings"`.
pub fn gtk_to_canonical(name: &str) -> &str {
    name
}

/// Resolve the profile target a `{"view":"profile","actor_id":…}` nav-stack entry
/// asks for, in the shape `app.rs`'s `open_profile` takes.
///
/// This app's own hand-rolled copy (trim + ASCII-case-insensitive self
/// detection, 6 unit-pinned cases) was ported verbatim into
/// `fauna_core::format::profile_nav_target` — see that function's doc for the
/// full rationale and its own copy of the same pins. Delegates rather than
/// re-implements.
pub use fauna_core::format::profile_nav_target;
