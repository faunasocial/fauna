//! Store adapters for `atrium-oauth` state and session persistence.
//!
//! Implements [`StateStore`] and [`SessionStore`] on top of a generic
//! [`StorageBackend`] trait so the library crate stays independent of
//! any concrete database.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use atrium_api::types::string::Did;
use atrium_common::store::Store;
use atrium_oauth::store::{
    session::{Session, SessionStore},
    state::{InternalStateData, StateStore},
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error type for store operations.
#[derive(Debug)]
pub struct StoreError(anyhow::Error);

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl From<anyhow::Error> for StoreError {
    fn from(err: anyhow::Error) -> Self {
        Self(err)
    }
}

// ---------------------------------------------------------------------------
// StorageBackend trait
// ---------------------------------------------------------------------------

/// A generic key-value storage backend keyed by (table, key) pairs.
///
/// Implementations live outside this crate (e.g. in `fauna-nest`)
/// and bridge to the actual database.
pub trait StorageBackend: Send + Sync + 'static {
    // reason: hand-written boxed-future return so the trait stays
    // dyn-compatible (no `async fn` in object-safe traits); all four methods
    // share this shape, `get`'s just crosses the complexity threshold.
    #[allow(clippy::type_complexity)]
    fn get(
        &self,
        table: &str,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Vec<u8>>>> + Send + '_>>;

    fn set(
        &self,
        table: &str,
        key: &str,
        value: &[u8],
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    fn del(
        &self,
        table: &str,
        key: &str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;

    fn clear(&self, table: &str) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>>;
}

// ---------------------------------------------------------------------------
// SqliteStateStore
// ---------------------------------------------------------------------------

const STATE_TABLE: &str = "bluesky_oauth_states";

/// Wraps a [`StorageBackend`] to implement `atrium-oauth` [`StateStore`].
#[derive(Clone)]
pub struct SqliteStateStore {
    backend: Arc<dyn StorageBackend>,
}

impl SqliteStateStore {
    pub fn new(backend: Arc<dyn StorageBackend>) -> Self {
        Self { backend }
    }
}

impl Store<String, InternalStateData> for SqliteStateStore {
    type Error = StoreError;

    async fn get(&self, key: &String) -> Result<Option<InternalStateData>, Self::Error> {
        let bytes = self
            .backend
            .get(STATE_TABLE, key)
            .await
            .map_err(StoreError::from)?;
        match bytes {
            Some(b) => {
                let data: InternalStateData =
                    serde_json::from_slice(&b).map_err(|e| StoreError(e.into()))?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    async fn set(&self, key: String, value: InternalStateData) -> Result<(), Self::Error> {
        let bytes = serde_json::to_vec(&value).map_err(|e| StoreError(e.into()))?;
        self.backend
            .set(STATE_TABLE, &key, &bytes)
            .await
            .map_err(StoreError::from)
    }

    async fn del(&self, key: &String) -> Result<(), Self::Error> {
        self.backend
            .del(STATE_TABLE, key)
            .await
            .map_err(StoreError::from)
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        self.backend
            .clear(STATE_TABLE)
            .await
            .map_err(StoreError::from)
    }
}

impl StateStore for SqliteStateStore {}

// ---------------------------------------------------------------------------
// SqliteSessionStore
// ---------------------------------------------------------------------------

const SESSION_TABLE: &str = "bluesky_sessions";

/// Wraps a [`StorageBackend`] to implement `atrium-oauth` [`SessionStore`].
#[derive(Clone)]
pub struct SqliteSessionStore {
    backend: Arc<dyn StorageBackend>,
}

impl SqliteSessionStore {
    pub fn new(backend: Arc<dyn StorageBackend>) -> Self {
        Self { backend }
    }
}

impl Store<Did, Session> for SqliteSessionStore {
    type Error = StoreError;

    async fn get(&self, key: &Did) -> Result<Option<Session>, Self::Error> {
        let bytes = self
            .backend
            .get(SESSION_TABLE, key.as_ref())
            .await
            .map_err(StoreError::from)?;
        match bytes {
            Some(b) => {
                let data: Session = serde_json::from_slice(&b).map_err(|e| StoreError(e.into()))?;
                Ok(Some(data))
            }
            None => Ok(None),
        }
    }

    async fn set(&self, key: Did, value: Session) -> Result<(), Self::Error> {
        let bytes = serde_json::to_vec(&value).map_err(|e| StoreError(e.into()))?;
        self.backend
            .set(SESSION_TABLE, key.as_ref(), &bytes)
            .await
            .map_err(StoreError::from)
    }

    async fn del(&self, key: &Did) -> Result<(), Self::Error> {
        self.backend
            .del(SESSION_TABLE, key.as_ref())
            .await
            .map_err(StoreError::from)
    }

    async fn clear(&self) -> Result<(), Self::Error> {
        self.backend
            .clear(SESSION_TABLE)
            .await
            .map_err(StoreError::from)
    }
}

impl SessionStore for SqliteSessionStore {}

/// A [`StorageBackend`] fake shared by this crate's own tests and `oauth.rs`
/// — previously three byte-for-byte hand-copies (each
/// doc comment already called the others out as a "mirror", but the shared
/// home was never built.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::StorageBackend;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// In-memory `StorageBackend` for tests.
    pub(crate) struct MemoryBackend {
        data: Mutex<HashMap<(String, String), Vec<u8>>>,
    }

    impl MemoryBackend {
        pub(crate) fn new() -> Self {
            Self {
                data: Mutex::new(HashMap::new()),
            }
        }
    }

    impl StorageBackend for MemoryBackend {
        fn get(
            &self,
            table: &str,
            key: &str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Vec<u8>>>> + Send + '_>> {
            let result = self
                .data
                .lock()
                .unwrap()
                .get(&(table.to_string(), key.to_string()))
                .cloned();
            Box::pin(async move { Ok(result) })
        }
        fn set(
            &self,
            table: &str,
            key: &str,
            value: &[u8],
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
            self.data
                .lock()
                .unwrap()
                .insert((table.to_string(), key.to_string()), value.to_vec());
            Box::pin(async { Ok(()) })
        }
        fn del(
            &self,
            table: &str,
            key: &str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
            self.data
                .lock()
                .unwrap()
                .remove(&(table.to_string(), key.to_string()));
            Box::pin(async { Ok(()) })
        }
        fn clear(
            &self,
            table: &str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + '_>> {
            let table = table.to_string();
            self.data.lock().unwrap().retain(|(t, _), _| *t != table);
            Box::pin(async { Ok(()) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_common::store::Store;

    use super::fixtures::MemoryBackend;

    fn make_test_state() -> InternalStateData {
        // Construct a minimal InternalStateData via JSON since Key doesn't impl Default.
        serde_json::from_value(serde_json::json!({
            "iss": "https://bsky.social",
            "dpop_key": { "kty": "EC", "crv": "P-256", "x": "AAAA", "y": "BBBB" },
            "verifier": "test-verifier",
            "app_state": "actor-hex"
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn state_store_roundtrip() {
        let backend = Arc::new(MemoryBackend::new());
        let store = SqliteStateStore::new(backend);

        // get on missing key returns None
        let result = store.get(&"test-key".to_string()).await.unwrap();
        assert!(result.is_none());

        // set and get
        let state = make_test_state();
        store
            .set("test-key".to_string(), state.clone())
            .await
            .unwrap();
        let fetched = store.get(&"test-key".to_string()).await.unwrap().unwrap();
        assert_eq!(fetched.iss, "https://bsky.social");
        assert_eq!(fetched.app_state, Some("actor-hex".to_string()));

        // del removes it
        store.del(&"test-key".to_string()).await.unwrap();
        let result = store.get(&"test-key".to_string()).await.unwrap();
        assert!(result.is_none());
    }
}
