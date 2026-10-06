//! Test doubles for driving a real [`NestClient`](crate::client::NestClient)
//! without a live WebSocket — the client-side companion to
//! [`fauna_ws_substrate::testing`], which owns the in-memory transport itself
//! (`mpsc_pair` / `MpscAdapter` / `ServerSide`) and is re-exported below.
//!
//! # Why this lives here rather than in `fauna-client-testkit`
//!
//! `fauna-client-testkit` doubles the *pure* [`fauna_protocol::RpcRequester`]
//! seam and holds a deliberate line: runtime-free and wasm-clean, so the ~30
//! feature crates that consume it stay testable on wasm32 (see that crate's
//! docs — "do not add a runtime here"). [`MockClientChannel`] is the opposite
//! shape: it needs tokio and the real supervisor, because the property it
//! exists to exercise *is* the connect/reconnect lifecycle. So it belongs
//! beside the client whose lifecycle it drives, behind the `test-util`
//! feature, exactly as the substrate publishes its own `testing` module.
//!
//! # What it is for
//!
//! Any test that must observe what a **not-yet-connected** client does. The
//! post-login connect race is the canonical case: [`NestClient::connect`]
//! returns once the WS handshake has been *initiated*, so a page that mounts
//! immediately afterwards issues its first read while the dispatcher slot is
//! still `None`. `request_inner` parks that read until the supervisor lands
//! the connection rather than failing it — the property pinned by
//! `fauna-client`'s own `request_issued_while_disconnected_waits_for_reconnect`
//! and, at the machine layer, by `fauna-client-mail-settings`'s
//! `hydrate_waits_for_socket`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fauna_protocol::RpcDispatcher;
use fauna_ws_substrate::{ConnectedAdapter, SupervisedChannel};

use crate::auth_client::AuthClient;
use crate::error::NestClientError;
use crate::push::PushBroker;

pub use fauna_ws_substrate::testing::{
    MpscAdapter, ServerSide, mpsc_pair, mpsc_pair_with_capacity,
};

/// A queue of connection attempts for [`MockClientChannel`]. Each `connect()`
/// pops one; an exhausted queue answers `WebSocket("test queue exhausted")`,
/// so a test stages a precise sequence of successes and failures.
pub type ConnectQueue = Arc<Mutex<VecDeque<Result<MpscAdapter, NestClientError>>>>;

/// Build a [`ConnectQueue`] that yields `adapters` in order.
pub fn connect_queue<I>(adapters: I) -> ConnectQueue
where
    I: IntoIterator<Item = Result<MpscAdapter, NestClientError>>,
{
    Arc::new(Mutex::new(adapters.into_iter().collect()))
}

/// The client's mock [`SupervisedChannel`]: a queued (mocked) connect plus the
/// *real* push bridge and bearer refresh. `on_connect` bridges pushes exactly
/// as production does, and `refresh_auth` drives the real [`AuthClient`] (so a
/// 4401 close exercises the actual clear+re-mint path) — only the socket is
/// fake.
pub struct MockClientChannel {
    queue: ConnectQueue,
    auth: Arc<AuthClient>,
    pushes: Arc<PushBroker>,
}

impl MockClientChannel {
    pub fn new(queue: ConnectQueue, auth: Arc<AuthClient>, pushes: Arc<PushBroker>) -> Self {
        Self {
            queue,
            auth,
            pushes,
        }
    }
}

#[async_trait]
impl SupervisedChannel for MockClientChannel {
    type Session = tokio::task::JoinHandle<()>;
    type Error = NestClientError;

    async fn connect(&self) -> Result<Box<dyn ConnectedAdapter>, NestClientError> {
        let next = {
            let mut q = self.queue.lock().unwrap();
            q.pop_front()
        };
        match next {
            Some(Ok(adapter)) => Ok(Box::new(adapter) as Box<dyn ConnectedAdapter>),
            Some(Err(e)) => Err(e),
            None => Err(NestClientError::WebSocket("test queue exhausted".into())),
        }
    }

    async fn on_connect(
        &self,
        dispatcher: &Arc<RpcDispatcher>,
    ) -> Result<Self::Session, NestClientError> {
        Ok(self.pushes.bridge_from(dispatcher.push_subscriber()))
    }

    async fn on_disconnect(&self, session: Self::Session) {
        let _ = session.await;
    }

    async fn refresh_auth(&self) -> Result<(), NestClientError> {
        self.auth.clear_token().await;
        self.auth.ensure_auth().await.map(|_| ())
    }
}
