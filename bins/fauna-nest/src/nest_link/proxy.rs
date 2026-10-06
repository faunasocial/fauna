//! Proxy-side worker management: WebSocket handler, auth, request-response dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{FromRequest, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::sync::{Mutex, RwLock, mpsc, oneshot};

use super::protocol::{PayloadKind, ProxyCommand, WorkerMessage};
use crate::routes::AppState;
use crate::ws::{Beat, ServerHeartbeat};

/// Information reported by the worker on connect.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerInfo {
    pub max_storage_bytes: u64,
    pub current_usage_bytes: u64,
    pub payload_count: u64,
}

/// A handle to an active worker connection.
pub struct WorkerHandle {
    cmd_tx: mpsc::Sender<WorkerRequest>,
    pub info: WorkerInfo,
    pub connected_at: u64,
    next_request_id: AtomicU64,
}

/// A request sent to the worker, paired with a channel for the response.
struct WorkerRequest {
    command: ProxyCommand,
    response_tx: oneshot::Sender<WorkerMessage>,
}

impl WorkerHandle {
    /// Send a Store command and wait for StoreAck. Returns true if stored successfully.
    pub async fn store(
        &self,
        kind: PayloadKind,
        key: &str,
        payload: &str,
        inbox_row_id: Option<i64>,
    ) -> Result<bool> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let cmd = ProxyCommand::Store {
            request_id,
            kind,
            key: key.to_string(),
            payload: payload.to_string(),
            inbox_row_id,
        };
        let resp = self.send_and_wait(cmd).await?;
        match resp {
            WorkerMessage::StoreAck { ok, error, .. } => {
                if !ok {
                    tracing::warn!("worker store failed: {:?}", error);
                }
                Ok(ok)
            }
            _ => anyhow::bail!("unexpected response to Store"),
        }
    }

    /// Send a Delete command and wait for DeleteAck. Returns true when the
    /// worker replica removed the payload (or it was already gone — idempotent).
    /// The delete twin of [`Self::store`].
    pub async fn delete(&self, kind: PayloadKind, key: &str) -> Result<bool> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let cmd = ProxyCommand::Delete {
            request_id,
            kind,
            key: key.to_string(),
        };
        let resp = self.send_and_wait(cmd).await?;
        match resp {
            WorkerMessage::DeleteAck { ok, error, .. } => {
                if !ok {
                    tracing::warn!("worker delete failed: {:?}", error);
                }
                Ok(ok)
            }
            _ => anyhow::bail!("unexpected response to Delete"),
        }
    }

    /// Send a Fetch command and wait for FetchResult. Returns hex-encoded payload if found.
    pub async fn fetch(&self, kind: PayloadKind, key: &str) -> Result<Option<Vec<u8>>> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let cmd = ProxyCommand::Fetch {
            request_id,
            kind,
            key: key.to_string(),
        };
        let resp = self.send_and_wait(cmd).await?;
        match resp {
            WorkerMessage::FetchResult { found, payload, .. } => {
                if found {
                    if let Some(hex_payload) = payload {
                        let bytes = hex::decode(&hex_payload)?;
                        Ok(Some(bytes))
                    } else {
                        Ok(None)
                    }
                } else {
                    Ok(None)
                }
            }
            _ => anyhow::bail!("unexpected response to Fetch"),
        }
    }

    async fn send_and_wait(&self, cmd: ProxyCommand) -> Result<WorkerMessage> {
        let (tx, rx) = oneshot::channel();
        let req = WorkerRequest {
            command: cmd,
            response_tx: tx,
        };
        self.cmd_tx
            .send(req)
            .await
            .map_err(|_| anyhow::anyhow!("worker disconnected"))?;
        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), rx).await??;
        Ok(resp)
    }
}

#[cfg(test)]
impl WorkerHandle {
    /// A handle wired to an in-process fake worker that acks every `Delete`
    /// and records its key — the seam the post-delete re-drive tests observe
    /// the replica leg through. Any other command is dropped unanswered.
    pub(crate) fn fake_acking_deletes() -> (Arc<Self>, Arc<std::sync::Mutex<Vec<String>>>) {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<WorkerRequest>(64);
        let deleted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&deleted);
        // spawn-ok(test)
        tokio::spawn(async move {
            while let Some(req) = cmd_rx.recv().await {
                if let ProxyCommand::Delete {
                    request_id, key, ..
                } = req.command
                {
                    seen.lock().unwrap().push(key);
                    let _ = req.response_tx.send(WorkerMessage::DeleteAck {
                        request_id,
                        ok: true,
                        error: None,
                    });
                }
            }
        });
        let handle = Arc::new(Self {
            cmd_tx,
            info: WorkerInfo {
                max_storage_bytes: 0,
                current_usage_bytes: 0,
                payload_count: 0,
            },
            connected_at: 0,
            next_request_id: AtomicU64::new(1),
        });
        (handle, deleted)
    }
}

/// Proxy-side state for tracking the connected worker.
pub struct WorkerState {
    authorized_key: Option<[u8; 32]>,
    connected: Arc<RwLock<Option<Arc<WorkerHandle>>>>,
}

impl WorkerState {
    pub fn new(authorized_key: Option<[u8; 32]>) -> Self {
        Self {
            authorized_key,
            connected: Arc::new(RwLock::new(None)),
        }
    }

    /// Register `handle` as the connected worker without a socket — the test
    /// seam that lets a leg's `get_handle()` reach a fake worker.
    #[cfg(test)]
    pub(crate) async fn connect_for_test(&self, handle: Arc<WorkerHandle>) {
        *self.connected.write().await = Some(handle);
    }

    pub async fn is_connected(&self) -> bool {
        self.connected.read().await.is_some()
    }

    pub async fn get_handle(&self) -> Option<Arc<WorkerHandle>> {
        self.connected.read().await.clone()
    }

    pub fn authorized_key_hex(&self) -> Option<String> {
        self.authorized_key.map(hex::encode)
    }
}

fn now_epoch() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

/// Axum handler for `GET /internal/worker/ws`.
pub async fn worker_ws_handler(
    State(state): State<Arc<AppState>>,
    req: axum::extract::Request,
) -> Response {
    // Shares its allowlist gate with the sibling `sidecar_channel::sidecar_ws_upgrade`
    // — see [`crate::sidecar_channel::internal_ws_allowlist_gate`]'s own doc comment.
    // The prior `if let Some(...)` with no `else` skipped the check outright on a
    // missing ConnectInfo, on the retired belief that TLS connections lack it.
    if let Err(resp) = crate::sidecar_channel::internal_ws_allowlist_gate(&req, "worker") {
        return resp;
    }
    // Extract WebSocketUpgrade from the request
    let ws = match axum::extract::WebSocketUpgrade::from_request(req, &state).await {
        Ok(ws) => ws,
        Err(e) => return e.into_response(),
    };
    // (clv, row 89) Generation-scope the whole connection future. The paired
    // worker is its own channel with its own registry (`state.bridge.worker`),
    // not a `state.ws` client and not plain HTTP, so neither the 1001-drain nor
    // an idle timeout reaches it — and this one *serves the nest's own
    // replication traffic*, so an un-torn-down worker channel would keep the
    // superseded generation writing to the paired public box after a rotation.
    ws.on_upgrade(move |socket| {
        let scope = Arc::clone(&state);
        async move {
            let _ = scope.spawn_scoped(handle_worker_ws(state, socket)).await;
        }
    })
}

async fn handle_worker_ws(state: Arc<AppState>, mut socket: WebSocket) {
    // --- Auth challenge-response ---
    let authorized_key = match state.bridge.worker.authorized_key {
        Some(k) => k,
        None => {
            tracing::warn!("worker connection rejected: no authorized key configured");
            return;
        }
    };

    // Generate 32-byte challenge
    let mut challenge_bytes = [0u8; 32];
    getrandom::fill(&mut challenge_bytes).expect("getrandom failed");
    let challenge_hex = hex::encode(challenge_bytes);

    let challenge_cmd = ProxyCommand::AuthChallenge {
        challenge: challenge_hex.clone(),
    };
    let json = serde_json::to_string(&challenge_cmd).unwrap();
    if socket.send(Message::Text(json.into())).await.is_err() {
        return;
    }

    // Wait for AuthResponse
    let auth_msg = match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
    {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<WorkerMessage>(&text) {
            Ok(msg) => msg,
            Err(_) => return,
        },
        _ => return,
    };

    let (pub_key_hex, sig_hex) = match auth_msg {
        WorkerMessage::AuthResponse {
            public_key,
            signature,
        } => (public_key, signature),
        _ => return,
    };

    // Verify public key matches authorized key
    let pub_key_bytes = match fauna_core::hex32::decode(&pub_key_hex) {
        Ok(arr) => arr,
        Err(_) => return,
    };

    if pub_key_bytes != authorized_key {
        tracing::warn!("worker auth failed: wrong public key");
        return;
    }

    // Verify signature
    let sig_bytes = match hex::decode(&sig_hex) {
        Ok(b) if b.len() == 64 => b,
        _ => return,
    };

    // The equality check above already pins the key to `authorized_key`, so this
    // is not the attacker-chosen-key case — but it verifies through the
    // one primitive anyway, so the nest has a single Ed25519 verification shape.
    if !fauna_core::identity::verify_detached(&pub_key_bytes, &challenge_bytes, &sig_bytes) {
        tracing::warn!("worker auth failed: invalid signature");
        return;
    }

    tracing::info!("worker authenticated: {}", pub_key_hex);

    // Wait for Hello
    let hello_msg = match tokio::time::timeout(std::time::Duration::from_secs(10), socket.next())
        .await
    {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<WorkerMessage>(&text) {
            Ok(msg) => msg,
            Err(_) => return,
        },
        _ => return,
    };

    let info = match hello_msg {
        WorkerMessage::Hello {
            max_storage_bytes,
            current_usage_bytes,
            payload_count,
        } => WorkerInfo {
            max_storage_bytes,
            current_usage_bytes,
            payload_count,
        },
        _ => return,
    };

    tracing::info!("worker hello: {:?}", info);

    // --- Set up request-response dispatch ---
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<WorkerRequest>(64);
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<WorkerMessage>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let handle = Arc::new(WorkerHandle {
        cmd_tx,
        info,
        connected_at: now_epoch(),
        next_request_id: AtomicU64::new(1),
    });

    // Register as connected
    {
        let mut connected = state.bridge.worker.connected.write().await;
        *connected = Some(handle.clone());
    }

    tracing::info!("worker connected");

    // Replay the replica deletes a disconnected worker missed: a post deleted
    // while no worker was connected left its `worker_replication` marker, and
    // this connect is the first moment the delete can reach the replica
    // (`nest/worker.md` § Post delete propagation).
    crate::post_delete_redrive::spawn_replica_redrive(Arc::clone(&state), Arc::clone(&handle));

    // One task owns the whole connection — commands out, worker messages in, and
    // the heartbeat — rather than the three it used to take.
    //
    // **The three-task shape was the bug, not a style choice.** Those tasks were
    // joined with `tokio::select!` over their `JoinHandle`s, and dropping a
    // `JoinHandle` does not abort its task: when the heartbeat timed out, the
    // reader went on holding `ws_rx` — and the socket, and the per-IP permit
    // under it (`libs/fauna-conn-limit`) — for the life of the process. The
    // liveness timer noticed the dead worker and then walked away from it. With
    // a single loop there is nothing to abort: leaving it drops the socket.
    //
    // The heartbeat itself is now the standard server half of
    // `transport.md` § Connection lifecycle rather than the app-level
    // `ProxyCommand::Ping` / `WorkerMessage::Pong` pair it replaces. Same cadence
    // source as every other endpoint (`WsHeartbeatPolicy` on `WsState`), and it
    // works below the application protocol, so it cannot queue behind a slow
    // command the way the old ping — which travelled through this very channel —
    // could. The worker answers a protocol Ping explicitly (`client.rs`), and
    // nothing on its side depends on *receiving* the app-level one, so no worker
    // build needs to change; the wire variants stay for exactly that reason.
    let mut hb = ServerHeartbeat::new(state.ws.heartbeat());
    loop {
        tokio::select! {
            beat = hb.next_beat() => match beat {
                Beat::Ping => {
                    if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                        break;
                    }
                }
                Beat::Dead => {
                    // `warn`, not `debug`: a connection reaped in silence is
                    // indistinguishable from a quiet night.
                    tracing::warn!(
                        timeout_ms = hb.liveness_timeout().as_millis() as u64,
                        "worker answered no heartbeat within the liveness window; \
                         closing dead link",
                    );
                    break;
                }
            },
            // Outbound: a proxy command bound for the worker.
            maybe = cmd_rx.recv() => {
                let Some(req) = maybe else { break }; // handle dropped: tear down
                // Extract request_id from command for response correlation
                let request_id = match &req.command {
                    ProxyCommand::Store { request_id, .. } => Some(*request_id),
                    ProxyCommand::Fetch { request_id, .. } => Some(*request_id),
                    ProxyCommand::Delete { request_id, .. } => Some(*request_id),
                    _ => None,
                };
                if let Some(rid) = request_id {
                    pending.lock().await.insert(rid, req.response_tx);
                }
                let json = serde_json::to_string(&req.command).unwrap();
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            // Inbound: a worker message, or the heartbeat's Pong.
            msg = socket.next() => {
                // Any inbound frame proves the worker is alive, so re-arm before
                // the frame is even inspected — the Pong counts exactly as much
                // as a StoreAck.
                hb.re_arm();
                let Some(Ok(msg)) = msg else { break };
                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    // Ping/Pong: the heartbeat, already accounted for above.
                    _ => continue,
                };
                let worker_msg: WorkerMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                match &worker_msg {
                    WorkerMessage::StoreAck { request_id, .. }
                    | WorkerMessage::FetchResult { request_id, .. }
                    | WorkerMessage::DeleteAck { request_id, .. } => {
                        let mut map = pending.lock().await;
                        if let Some(tx) = map.remove(request_id) {
                            let _ = tx.send(worker_msg);
                        }
                    }
                    // A `Pong`, or any other unmodelled frame, is ignored.
                    // Re-arming above already credited it as the liveness proof
                    // it is.
                    _ => {}
                }
            }
        }
    }

    // Clear connected state
    {
        let mut connected = state.bridge.worker.connected.write().await;
        *connected = None;
    }

    tracing::info!("worker disconnected");
}
