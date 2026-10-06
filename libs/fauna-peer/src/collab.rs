//! Real-time collaboration session management.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CollabMessage {
    #[serde(rename = "join")]
    Join {
        session_id: String,
        participant: String,
    },
    #[serde(rename = "leave")]
    Leave {
        session_id: String,
        participant: String,
    },
    #[serde(rename = "data")]
    Data {
        sender: String,
        payload: serde_json::Value,
    },
    #[serde(rename = "info")]
    Info {
        session_id: String,
        topic: String,
        participants: Vec<String>,
    },
    #[serde(rename = "error")]
    Error { message: String },
}

struct Session {
    topic: String,
    participants: HashMap<String, mpsc::UnboundedSender<CollabMessage>>,
}

pub struct SessionInfo {
    pub topic: String,
    pub participants: Vec<String>,
}

pub struct SessionManager {
    sessions: Mutex<HashMap<String, Session>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn create_session(&self, topic: &str) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.sessions.lock().unwrap().insert(
            id.clone(),
            Session {
                topic: topic.to_string(),
                participants: HashMap::new(),
            },
        );
        id
    }

    pub fn session_exists(&self, id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(id)
    }

    pub fn session_info(&self, id: &str) -> Option<SessionInfo> {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(id).map(|s| SessionInfo {
            topic: s.topic.clone(),
            participants: s.participants.keys().cloned().collect(),
        })
    }

    pub fn join_session(
        &self,
        id: &str,
        participant: &str,
        tx: mpsc::UnboundedSender<CollabMessage>,
    ) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
        session.participants.insert(participant.to_string(), tx);
        Ok(())
    }

    pub fn leave_session(&self, id: &str, participant: &str) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get_mut(id) {
            session.participants.remove(participant);
        }
    }

    pub fn broadcast(&self, id: &str, msg: &CollabMessage, exclude: Option<&str>) {
        let sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get(id) {
            for (name, tx) in &session.participants {
                if exclude.is_some_and(|e| e == name) {
                    continue;
                }
                let _ = tx.send(msg.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[test]
    fn create_and_join_session() {
        let mgr = SessionManager::new();
        let id = mgr.create_session("test-topic");
        assert!(mgr.session_exists(&id));

        let (tx, _rx) = mpsc::unbounded_channel();
        mgr.join_session(&id, "alice", tx).unwrap();

        let info = mgr.session_info(&id).unwrap();
        assert_eq!(info.topic, "test-topic");
        assert_eq!(info.participants.len(), 1);
        assert!(info.participants.contains(&"alice".to_string()));
    }

    #[test]
    fn leave_session() {
        let mgr = SessionManager::new();
        let id = mgr.create_session("chat");

        let (tx, _rx) = mpsc::unbounded_channel();
        mgr.join_session(&id, "bob", tx).unwrap();
        assert_eq!(mgr.session_info(&id).unwrap().participants.len(), 1);

        mgr.leave_session(&id, "bob");
        assert_eq!(mgr.session_info(&id).unwrap().participants.len(), 0);
    }

    #[test]
    fn broadcast_to_participants() {
        let mgr = SessionManager::new();
        let id = mgr.create_session("collab");

        let (tx_alice, _rx_alice) = mpsc::unbounded_channel();
        let (tx_bob, mut rx_bob) = mpsc::unbounded_channel();

        mgr.join_session(&id, "alice", tx_alice).unwrap();
        mgr.join_session(&id, "bob", tx_bob).unwrap();

        let msg = CollabMessage::Data {
            sender: "alice".to_string(),
            payload: serde_json::json!({"text": "hello"}),
        };
        mgr.broadcast(&id, &msg, Some("alice"));

        // bob should have received the message; alice excluded
        let received = rx_bob
            .try_recv()
            .expect("bob should have received a message");
        match received {
            CollabMessage::Data { sender, .. } => assert_eq!(sender, "alice"),
            _ => panic!("unexpected message variant"),
        }
    }

    #[test]
    fn join_nonexistent_session_fails() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let result = mgr.join_session("no-such-id", "charlie", tx);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("session not found")
        );
    }
}
