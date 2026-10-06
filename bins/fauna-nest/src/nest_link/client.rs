//! Worker-side client: connects to the proxy via WebSocket and handles commands.

use std::sync::Arc;

use anyhow::Result;
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::protocol::{PayloadKind, ProxyCommand, WorkerMessage};
use crate::db::CacheDb;
use crate::routes::parse_32_bytes;

pub struct WorkerClient {
    proxy_url: String,
    signing_key: SigningKey,
    db: Arc<CacheDb>,
}

impl WorkerClient {
    pub fn new(proxy_url: String, signing_key: SigningKey, db: Arc<CacheDb>) -> Self {
        Self {
            proxy_url,
            signing_key,
            db,
        }
    }

    /// Run the worker with automatic reconnection.
    pub async fn run(&self) -> ! {
        loop {
            match self.connect_and_serve().await {
                Ok(()) => {
                    tracing::info!("worker connection closed, reconnecting...");
                }
                Err(e) => {
                    tracing::warn!("worker connection error: {e}, reconnecting...");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }

    async fn connect_and_serve(&self) -> Result<()> {
        // The WireGuard `ConnectionRouter` that used to pick a tunnel URL here
        // died with the WireGuard stack (2026-08-23); it had no caller —
        // `with_router` was never invoked. The public proxy URL is the only one.
        let base_url = self.proxy_url.clone();

        // Build WebSocket URL
        let ws_url = fauna_core::web::http_to_ws(&base_url);
        let ws_url = format!("{ws_url}/internal/worker/ws");

        tracing::info!("connecting to proxy at {ws_url}");

        let (ws_stream, _) = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok(stream) => stream,
            Err(e) => {
                return Err(e.into());
            }
        };
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        tracing::info!("connected to proxy");

        // Wait for AuthChallenge
        let challenge_msg = match ws_rx.next().await {
            Some(Ok(Message::Text(text))) => serde_json::from_str::<ProxyCommand>(&text)?,
            Some(Ok(_)) => anyhow::bail!("expected text message for auth challenge"),
            Some(Err(e)) => return Err(e.into()),
            None => anyhow::bail!("connection closed before auth challenge"),
        };

        let challenge_bytes = match challenge_msg {
            ProxyCommand::AuthChallenge { challenge } => hex::decode(&challenge)?,
            _ => anyhow::bail!("expected AuthChallenge"),
        };

        // Sign the challenge
        let signature = self.signing_key.sign(&challenge_bytes);
        let pub_key = self.signing_key.verifying_key();

        let auth_response = WorkerMessage::AuthResponse {
            public_key: hex::encode(pub_key.as_bytes()),
            signature: hex::encode(signature.to_bytes()),
        };
        let json = serde_json::to_string(&auth_response)?;
        ws_tx.send(Message::Text(json.into())).await?;

        // Send Hello
        let stats = self.get_storage_stats().await;
        let hello = WorkerMessage::Hello {
            max_storage_bytes: 0, // not tracking limits in phase 1
            current_usage_bytes: stats.0,
            payload_count: stats.1,
        };
        let json = serde_json::to_string(&hello)?;
        ws_tx.send(Message::Text(json.into())).await?;

        tracing::info!("authenticated and sent hello");

        // Main loop: handle commands from proxy
        while let Some(msg) = ws_rx.next().await {
            let text = match msg? {
                Message::Text(t) => t,
                Message::Close(_) => break,
                Message::Ping(data) => {
                    ws_tx.send(Message::Pong(data)).await?;
                    continue;
                }
                _ => continue,
            };

            let cmd: ProxyCommand = match serde_json::from_str(&text) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("failed to parse proxy command: {e}");
                    continue;
                }
            };

            let response = match cmd {
                ProxyCommand::Store {
                    request_id,
                    kind,
                    key,
                    payload,
                    inbox_row_id,
                } => {
                    self.handle_store(request_id, kind, &key, &payload, inbox_row_id)
                        .await
                }
                ProxyCommand::Fetch {
                    request_id,
                    kind,
                    key,
                } => self.handle_fetch(request_id, kind, &key).await,
                ProxyCommand::Delete {
                    request_id,
                    kind,
                    key,
                } => self.handle_delete(request_id, kind, &key).await,
                ProxyCommand::Ping { ts } => WorkerMessage::Pong { ts },
                ProxyCommand::AuthChallenge { .. } => continue,
            };

            let json = serde_json::to_string(&response)?;
            ws_tx.send(Message::Text(json.into())).await?;
        }

        Ok(())
    }

    async fn handle_store(
        &self,
        request_id: u64,
        kind: PayloadKind,
        key: &str,
        payload_hex: &str,
        inbox_row_id: Option<i64>,
    ) -> WorkerMessage {
        let payload = match hex::decode(payload_hex) {
            Ok(p) => p,
            Err(e) => {
                return WorkerMessage::StoreAck {
                    request_id,
                    ok: false,
                    error: Some(format!("invalid hex payload: {e}")),
                };
            }
        };

        let result = match kind {
            PayloadKind::Inbox => {
                let actor_id = match parse_32_bytes(key) {
                    Some(id) => id,
                    None => {
                        return WorkerMessage::StoreAck {
                            request_id,
                            ok: false,
                            error: Some("invalid actor_id".into()),
                        };
                    }
                };
                self.db
                    .push_inbox(&actor_id, &payload, None)
                    .await
                    .map(|_| ())
            }
            PayloadKind::Post => {
                let post_id = match parse_32_bytes(key) {
                    Some(id) => id,
                    None => {
                        return WorkerMessage::StoreAck {
                            request_id,
                            ok: false,
                            error: Some("invalid post_id".into()),
                        };
                    }
                };
                self.db.put_post(&post_id, &payload, None).await
            }
            // No worker holds a blob store — see `handle_fetch`'s twin arm.
            PayloadKind::Blob | PayloadKind::Chunk | PayloadKind::Manifest => {
                Err(anyhow::anyhow!("store unsupported for {kind:?}"))
            }
        };

        match result {
            Ok(()) => {
                tracing::debug!("stored {kind:?} {key} (inbox_row_id={inbox_row_id:?})");
                WorkerMessage::StoreAck {
                    request_id,
                    ok: true,
                    error: None,
                }
            }
            Err(e) => WorkerMessage::StoreAck {
                request_id,
                ok: false,
                error: Some(e.to_string()),
            },
        }
    }

    async fn handle_fetch(&self, request_id: u64, kind: PayloadKind, key: &str) -> WorkerMessage {
        let result = match kind {
            PayloadKind::Post => {
                let post_id = match parse_32_bytes(key) {
                    Some(id) => id,
                    None => {
                        return WorkerMessage::FetchResult {
                            request_id,
                            found: false,
                            payload: None,
                        };
                    }
                };
                self.db
                    .get_post(&post_id)
                    .await
                    .map(|opt| opt.map(|(p, _)| p))
            }
            PayloadKind::Inbox => {
                // Inbox fetch not supported in Phase 1
                Ok(None)
            }
            // No worker holds a blob store. The arm that once read one served a
            // digest the PROXY named, with no legal-takedown gate — a door onto
            // a store the nest's withhold never reaches — and nothing ever
            // constructed a worker with a store, so it was removed rather than
            // left for blob replication to wire in (`nest/worker.md` § Payload
            // Types; `moderation.md` § Legal takedown → *The blob-serve door*).
            // Not-found is what that dormant arm already answered.
            PayloadKind::Blob | PayloadKind::Chunk | PayloadKind::Manifest => Ok(None),
        };

        match result {
            Ok(Some(data)) => WorkerMessage::FetchResult {
                request_id,
                found: true,
                payload: Some(hex::encode(&data)),
            },
            Ok(None) => WorkerMessage::FetchResult {
                request_id,
                found: false,
                payload: None,
            },
            Err(e) => {
                tracing::warn!("fetch error for {kind:?} {key}: {e}");
                WorkerMessage::FetchResult {
                    request_id,
                    found: false,
                    payload: None,
                }
            }
        }
    }

    /// Remove a replicated payload — the delete twin of [`Self::handle_store`].
    /// The proxy has already authorized the removal (`delete_post_core`'s three
    /// author checks); the worker just executes, trusting the proxy exactly as
    /// `handle_store` does. Idempotent: deleting an already-gone post still acks
    /// `ok: true` (the `delete_post_core` AlreadyGone-is-success contract), so a
    /// crash-retry never errors. An unsupported kind fails loudly (`ok: false`)
    /// rather than silently dropping the command.
    async fn handle_delete(&self, request_id: u64, kind: PayloadKind, key: &str) -> WorkerMessage {
        let result = match kind {
            PayloadKind::Post => {
                let post_id = match parse_32_bytes(key) {
                    Some(id) => id,
                    None => {
                        return WorkerMessage::DeleteAck {
                            request_id,
                            ok: false,
                            error: Some("invalid post_id".into()),
                        };
                    }
                };
                // `existed` is discarded on purpose: a delete of a post the
                // replica never held is still a success (idempotent), matching
                // the origin nest's AlreadyGone outcome.
                self.db.delete_post_projection(&post_id).await.map(|_| ())
            }
            PayloadKind::Inbox | PayloadKind::Blob | PayloadKind::Chunk | PayloadKind::Manifest => {
                Err(anyhow::anyhow!("delete unsupported for {kind:?}"))
            }
        };

        match result {
            Ok(()) => {
                tracing::debug!("deleted {kind:?} {key}");
                WorkerMessage::DeleteAck {
                    request_id,
                    ok: true,
                    error: None,
                }
            }
            Err(e) => {
                tracing::warn!("delete error for {kind:?} {key}: {e}");
                WorkerMessage::DeleteAck {
                    request_id,
                    ok: false,
                    error: Some(e.to_string()),
                }
            }
        }
    }

    async fn get_storage_stats(&self) -> (u64, u64) {
        match self.db.get_stats().await {
            Ok(stats) => {
                let bytes = (stats.total_inbox_bytes + stats.total_storage_bytes) as u64;
                let count = stats.total_users as u64;
                (bytes, count)
            }
            Err(_) => (0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Headless coverage of the worker-side delete dispatch (`handle_delete`),
    //! the paired-replica tombstone twin's executing half. The proxy↔worker WS
    //! round-trip has no in-process harness (neither does the `store` create
    //! twin) — that transport is the documented boundary; the real removal
    //! mechanism (`delete_post_projection`) is exercised directly here against a
    //! real in-memory `CacheDb`.
    use super::*;

    fn test_client(db: Arc<CacheDb>) -> WorkerClient {
        WorkerClient::new(
            "http://worker.invalid".to_string(),
            SigningKey::from_bytes(&[7u8; 32]),
            db,
        )
    }

    #[tokio::test]
    async fn handle_delete_removes_the_post_projection() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let post_id = [1u8; 32];
        // Seed a projection row via the same put_post path replication uses;
        // raw non-decodable bytes take the fallback content-insert branch.
        db.put_post(&post_id, b"replicated-post-body", None)
            .await
            .unwrap();
        assert!(db.get_post(&post_id).await.unwrap().is_some());

        let client = test_client(db.clone());
        let ack = client
            .handle_delete(1, PayloadKind::Post, &hex::encode(post_id))
            .await;
        assert!(
            matches!(ack, WorkerMessage::DeleteAck { ok: true, .. }),
            "expected ok DeleteAck, got {ack:?}"
        );
        assert!(
            db.get_post(&post_id).await.unwrap().is_none(),
            "post projection must be gone on the replica"
        );
    }

    #[tokio::test]
    async fn handle_delete_of_absent_post_is_idempotent_ok() {
        // A crash-retry (or a post the replica never held) must ack success,
        // matching delete_post_core's AlreadyGone-is-success contract.
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let client = test_client(db);
        let ack = client
            .handle_delete(2, PayloadKind::Post, &hex::encode([2u8; 32]))
            .await;
        assert!(
            matches!(ack, WorkerMessage::DeleteAck { ok: true, .. }),
            "delete of an absent post is idempotent success, got {ack:?}"
        );
    }

    #[tokio::test]
    async fn handle_delete_unsupported_kind_fails_loudly() {
        // Only Post is replicated-then-deleted today; other kinds must return a
        // loud error ack, never a silent success (principle: no dropped command).
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let client = test_client(db);
        for kind in [PayloadKind::Inbox, PayloadKind::Blob] {
            let ack = client.handle_delete(3, kind, &hex::encode([3u8; 32])).await;
            match ack {
                WorkerMessage::DeleteAck {
                    ok: false,
                    error: Some(_),
                    ..
                } => {}
                other => panic!("expected a loud error ack for {kind:?}, got {other:?}"),
            }
        }
    }

    /// A worker neither stores nor serves the blob kinds: it holds no blob
    /// store, so a store fails loudly and a fetch — for a digest the proxy
    /// names — answers not-found without reading anything.
    #[tokio::test]
    async fn blob_kinds_are_neither_stored_nor_served() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let client = test_client(db);
        let key = hex::encode([5u8; 32]);
        for kind in [PayloadKind::Blob, PayloadKind::Chunk, PayloadKind::Manifest] {
            match client
                .handle_store(5, kind, &key, &hex::encode(b"bytes"), None)
                .await
            {
                WorkerMessage::StoreAck {
                    ok: false,
                    error: Some(_),
                    ..
                } => {}
                other => panic!("a {kind:?} store must fail loudly, got {other:?}"),
            }
            match client.handle_fetch(6, kind, &key).await {
                WorkerMessage::FetchResult {
                    found: false,
                    payload: None,
                    ..
                } => {}
                other => panic!("a {kind:?} fetch must answer not-found, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn handle_delete_malformed_key_fails() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let client = test_client(db);
        let ack = client.handle_delete(4, PayloadKind::Post, "not-hex").await;
        assert!(
            matches!(ack, WorkerMessage::DeleteAck { ok: false, .. }),
            "a malformed key must fail, got {ack:?}"
        );
    }
}
