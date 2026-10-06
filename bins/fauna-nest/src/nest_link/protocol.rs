//! Wire protocol types exchanged over the proxy-worker WebSocket.

use serde::{Deserialize, Serialize};

/// Commands sent from proxy to worker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProxyCommand {
    AuthChallenge {
        challenge: String,
    },
    Store {
        request_id: u64,
        kind: PayloadKind,
        key: String,
        payload: String,
        inbox_row_id: Option<i64>,
    },
    Fetch {
        request_id: u64,
        kind: PayloadKind,
        key: String,
    },
    /// Remove a previously-`Store`d payload from the worker replica. The delete
    /// twin of `Store` (`spawn_replicate_delete` → `WorkerHandle::delete`), so a
    /// post the origin nest deletes does not outlive its paired-public-nest
    /// replica (`feed.md` § Post deletion → Propagation). The proxy has already
    /// run the three author checks in `delete_post_core`; the worker just
    /// executes, exactly as `Store` trusts the proxy.
    Delete {
        request_id: u64,
        kind: PayloadKind,
        key: String,
    },
    Ping {
        ts: u64,
    },
}

/// Messages sent from worker to proxy.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WorkerMessage {
    AuthResponse {
        public_key: String,
        signature: String,
    },
    Hello {
        max_storage_bytes: u64,
        current_usage_bytes: u64,
        payload_count: u64,
    },
    StoreAck {
        request_id: u64,
        ok: bool,
        error: Option<String>,
    },
    FetchResult {
        request_id: u64,
        found: bool,
        payload: Option<String>,
    },
    /// Reply to `Delete`. `ok` is true when the removal succeeded — including
    /// the idempotent "the payload was already gone" case, mirroring the
    /// `delete_post_core` AlreadyGone-is-success contract; `ok` is false only on
    /// a real failure (unsupported kind, malformed key, db error).
    DeleteAck {
        request_id: u64,
        ok: bool,
        error: Option<String>,
    },
    Pong {
        ts: u64,
    },
}

/// Discriminator for payload type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PayloadKind {
    Inbox,
    Post,
    Blob,
    Chunk,
    Manifest,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_command_roundtrip() {
        let cmds = vec![
            ProxyCommand::AuthChallenge {
                challenge: "aa".repeat(32),
            },
            ProxyCommand::Store {
                request_id: 1,
                kind: PayloadKind::Inbox,
                key: "abcd".into(),
                payload: "deadbeef".into(),
                inbox_row_id: Some(42),
            },
            ProxyCommand::Store {
                request_id: 2,
                kind: PayloadKind::Post,
                key: "1234".into(),
                payload: "cafe".into(),
                inbox_row_id: None,
            },
            ProxyCommand::Fetch {
                request_id: 3,
                kind: PayloadKind::Post,
                key: "5678".into(),
            },
            ProxyCommand::Delete {
                request_id: 4,
                kind: PayloadKind::Post,
                key: "9abc".into(),
            },
            ProxyCommand::Ping { ts: 123456 },
        ];
        for cmd in &cmds {
            let json = serde_json::to_string(cmd).unwrap();
            let back: ProxyCommand = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn worker_message_roundtrip() {
        let msgs = vec![
            WorkerMessage::AuthResponse {
                public_key: "ab".repeat(32),
                signature: "cd".repeat(64),
            },
            WorkerMessage::Hello {
                max_storage_bytes: 1_000_000,
                current_usage_bytes: 500_000,
                payload_count: 42,
            },
            WorkerMessage::StoreAck {
                request_id: 1,
                ok: true,
                error: None,
            },
            WorkerMessage::StoreAck {
                request_id: 2,
                ok: false,
                error: Some("disk full".into()),
            },
            WorkerMessage::FetchResult {
                request_id: 3,
                found: true,
                payload: Some("deadbeef".into()),
            },
            WorkerMessage::FetchResult {
                request_id: 4,
                found: false,
                payload: None,
            },
            WorkerMessage::DeleteAck {
                request_id: 5,
                ok: true,
                error: None,
            },
            WorkerMessage::DeleteAck {
                request_id: 6,
                ok: false,
                error: Some("delete unsupported for Blob".into()),
            },
            WorkerMessage::Pong { ts: 123456 },
        ];
        for msg in &msgs {
            let json = serde_json::to_string(msg).unwrap();
            let back: WorkerMessage = serde_json::from_str(&json).unwrap();
            let json2 = serde_json::to_string(&back).unwrap();
            assert_eq!(json, json2);
        }
    }

    #[test]
    fn tagged_deserialization() {
        let json = r#"{"type":"Ping","ts":999}"#;
        let cmd: ProxyCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, ProxyCommand::Ping { ts: 999 }));

        let json = r#"{"type":"Pong","ts":999}"#;
        let msg: WorkerMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, WorkerMessage::Pong { ts: 999 }));
    }
}
