//! In-process automation HTTP server — thin wiring over the shared front-end.
//!
//! The client-agnostic half (blocking `tiny_http` server, request parsing, the
//! `/element/*` + `/app/{state,commands}` routes and JSON shapes) lives in the
//! shared `fauna-e2e-agent` crate, hosted identically by both direct-Rust
//! clients (linux + cli). This module supplies the linux hooks: ops marshal to
//! the GTK main thread over the `async_channel` drain, and the state protocol
//! is served from the test agent via [`super::link`].

use super::agent::ElementOp;
use fauna_e2e_agent::{AgentHooks, UiThreadHeartbeat};
use std::sync::Arc;

/// `heartbeat` is the GTK main loop's own liveness stamp (beaten by the tick
/// `main.rs` arms next to the op drain), so an agent timeout can say whether
/// that loop was running at all — see [`UiThreadHeartbeat`].
pub fn start(
    port: u16,
    op_tx: async_channel::Sender<ElementOp>,
    heartbeat: Arc<UiThreadHeartbeat>,
) {
    fauna_e2e_agent::start(
        port,
        AgentHooks {
            dispatch: Box::new(move |op| op_tx.send_blocking(op).map_err(|_| ())),
            app_state: Box::new(super::link::app_state),
            inject_command: Box::new(super::link::inject_command),
            heartbeat: Some(heartbeat),
        },
    );
}
