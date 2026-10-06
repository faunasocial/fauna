//! Background social feed sync between P2P contacts.
use std::collections::HashMap;
use std::sync::Mutex;

pub struct FeedSyncState {
    last_synced: Mutex<HashMap<[u8; 32], i64>>,
}

impl FeedSyncState {
    pub fn new() -> Self {
        Self {
            last_synced: Mutex::new(HashMap::new()),
        }
    }

    pub fn last_synced(&self, contact: &[u8; 32]) -> i64 {
        *self.last_synced.lock().unwrap().get(contact).unwrap_or(&0)
    }

    pub fn mark_synced(&self, contact: &[u8; 32], timestamp: i64) {
        self.last_synced.lock().unwrap().insert(*contact, timestamp);
    }

    pub fn contacts_needing_sync(
        &self,
        feed_contacts: &[[u8; 32]],
        interval_secs: i64,
    ) -> Vec<[u8; 32]> {
        let now = now_secs();
        let map = self.last_synced.lock().unwrap();
        feed_contacts
            .iter()
            .filter(|c| {
                let last = map.get(*c).copied().unwrap_or(0);
                now - last >= interval_secs
            })
            .copied()
            .collect()
    }
}

pub async fn pull_posts(
    base_url: &str,
    actor_id: &str,
    since: i64,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let url = format!("{base_url}/api/v1/posts?actor_id={actor_id}&since={since}");
    let resp = reqwest::Client::new().get(&url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("pull_posts failed: {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    Ok(body["posts"].as_array().cloned().unwrap_or_default())
}

fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

impl Default for FeedSyncState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_contact(byte: u8) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[0] = byte;
        key
    }

    #[test]
    fn feed_sync_state_tracks_last_sync() {
        let state = FeedSyncState::new();
        let contact = make_contact(0x01);

        // Initially zero
        assert_eq!(state.last_synced(&contact), 0);

        // After marking, returns that timestamp
        state.mark_synced(&contact, 1_700_000_000);
        assert_eq!(state.last_synced(&contact), 1_700_000_000);

        // Update to a newer timestamp
        state.mark_synced(&contact, 1_700_001_000);
        assert_eq!(state.last_synced(&contact), 1_700_001_000);
    }

    #[test]
    fn contacts_needing_sync() {
        let state = FeedSyncState::new();
        let never_synced = make_contact(0x02);
        let recently_synced = make_contact(0x03);

        // Mark one contact as just synced (use a far-future timestamp so it won't
        // need sync during the test regardless of wall-clock skew)
        let far_future: i64 = 9_999_999_999;
        state.mark_synced(&recently_synced, far_future);

        let contacts = [never_synced, recently_synced];
        let interval = 300; // 5 minutes

        let needs_sync = state.contacts_needing_sync(&contacts, interval);

        // never_synced should be included (last = 0, definitely overdue)
        assert!(
            needs_sync.contains(&never_synced),
            "never-synced contact should need sync"
        );

        // recently_synced has a far-future timestamp, so now - last is deeply negative
        // (< interval), meaning it does NOT need sync
        assert!(
            !needs_sync.contains(&recently_synced),
            "recently-synced contact should not need sync"
        );
    }

    #[tokio::test]
    async fn pull_posts_from_mock_server() {
        use axum::routing::get;
        use axum::{Json, Router};
        use tokio::net::TcpListener;

        // Build a minimal axum mock that returns two posts
        let app = Router::new().route(
            "/api/v1/posts",
            get(|| async {
                Json(serde_json::json!({
                    "posts": [
                        { "id": "post-1", "body": "hello world" },
                        { "id": "post-2", "body": "second post" }
                    ]
                }))
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let base_url = format!("http://{addr}");
        let posts = pull_posts(&base_url, "alice", 0).await.unwrap();

        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0]["id"], "post-1");
        assert_eq!(posts[1]["id"], "post-2");
        assert_eq!(posts[0]["body"], "hello world");
        assert_eq!(posts[1]["body"], "second post");
    }
}
