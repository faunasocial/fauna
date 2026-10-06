//! 4401 close → supervisor calls the client channel's `refresh_auth`
//! (clear_token + ensure_auth) → reconnects.
//!
//! Without a live HTTP backend the refresh fails. The supervisor exits with
//! `SupervisorError::AuthRefresh(NestClientError::Auth/Http/WebSocket)` — that
//! error shape is the assertion that we took the refresh path rather than a
//! plain backoff retry.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fauna_client::{AuthClient, NestClientError, PushBroker};
use fauna_core::identity::ActorKeypair;
use fauna_protocol::RpcDispatcher;
use fauna_ws_substrate::{
    ConnectionState, ReconnectSignal, Supervisor, SupervisorError, run_supervisor,
};
use tokio::sync::{RwLock, watch};

fn kp() -> ActorKeypair {
    ActorKeypair::from_secret([9u8; 32])
}

#[tokio::test]
async fn auth_expired_close_triggers_reconnect_attempt() {
    // First adapter: closes with 4401.
    let (adapter1, server1) = common::mpsc_pair();
    let cell1 = Arc::clone(&adapter1.closed_with);

    // Second adapter: would be the "after refresh" attempt. We never actually
    // reach it because ensure_auth fails (no live HTTP).
    let (adapter2, _server2) = common::mpsc_pair();

    let queue: Arc<Mutex<VecDeque<Result<common::MpscAdapter, NestClientError>>>> =
        Arc::new(Mutex::new(VecDeque::from([Ok(adapter1), Ok(adapter2)])));

    let auth = Arc::new(AuthClient::new("http://127.0.0.1:1".into(), kp()));
    let channel = Arc::new(common::MockClientChannel::new(
        Arc::clone(&queue),
        Arc::clone(&auth),
        PushBroker::new(16),
    ));
    let slot: Arc<RwLock<Option<Arc<RpcDispatcher>>>> = Arc::new(RwLock::new(None));
    let (tx, _rx) = watch::channel(ConnectionState::Disconnected);

    let cfg = Supervisor {
        channel,
        dispatcher_slot: slot,
        connection_state_tx: tx,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
    };

    let sup = tokio::spawn(run_supervisor(cfg));

    // Trigger 4401 close on adapter1.
    tokio::time::sleep(Duration::from_millis(50)).await;
    *cell1.lock().unwrap() = Some(ReconnectSignal::AuthExpired);
    drop(server1);

    let result = sup.await.unwrap();
    // Supervisor should exit with an auth-refresh failure wrapping the
    // Auth/Http/WebSocket error from the failed ensure_auth call (no live HTTP
    // server backing the refresh).
    match result {
        Err(SupervisorError::AuthRefresh(
            NestClientError::Auth(_) | NestClientError::Http(_) | NestClientError::WebSocket(_),
        )) => {}
        other => {
            panic!("expected AuthRefresh(Auth/Http/WebSocket) from failed refresh, got {other:?}")
        }
    }
}
