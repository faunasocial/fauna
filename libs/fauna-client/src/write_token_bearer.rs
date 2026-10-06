//! [`WriteTokenBearer`] — the caching [`BearerSource`] a **cross-nest writer**
//! presents on direct byte-plane POSTs to a set's or room's **home** nest.
//!
//! A cross-nest writer has no session bearer on the home nest — it holds one
//! only on its *own* nest. So its byte-plane client is pointed at the home nest
//! with THIS as its [`BearerSource`]: `bearer()` lazily runs the caller's mint
//! step (a `*.write_token.get` kind on the writer's own nest, which relays the
//! matching `fauna.federation.*.write_token.mint` to the home nest behind that
//! plane's gate) and caches the returned short-TTL token, refreshing proactively
//! within [`REFRESH_BUFFER_SECS`] of expiry and reactively on a `401`. The token
//! authorizes the byte POST via the home nest's `BulkWriteAuth` bulk-token arm.
//!
//! One cache, three planes (lifted here from `fauna-sync-engine` 2026-09-09, when
//! the conversation rail became the third writer): the shared-folder writer
//! (`fauna.folders.write_token.get` — `fauna_sync_engine::write_token_bearer`
//! owns the folder-specific mint + its park-on-refusal classification), the
//! source nest's in-process segment-backup coordinator
//! (`fauna.federation.backup.write_token.mint`, as itself), and a foreign
//! conversation member's attachment upload
//! (`fauna.conversations.blob.write_token.get` —
//! `fauna_client_conversations::NestConversationsRpc`). Sharing the cache is what
//! keeps the planes from drifting on token lifetime handling; each caller owns
//! only its mint call and its error classification.
//!
//! Native only: the byte plane is native (the web write path mints per upload
//! over its own `gloo` POST), so a plain `SystemTime` clock is correct here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use async_trait::async_trait;
use fauna_nest_http::{ApiError, BearerSource};

/// Refresh a cached token this many seconds before it expires, so an in-flight
/// upload never presents a token that lapses mid-request. Owned by
/// [`fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS`] — the same client-half
/// policy every other bearer cache reads. ⚠ The TTL this must stay below is the
/// bulk-byte plane's (600 s: `bridge_blob_handlers::BULK_BYTE_TOKEN_TTL_SECS`,
/// `federation_handlers::FOREIGN_WRITE_TOKEN_TTL_SECS`), not the 3600 s session
/// TTL — a tighter margin than the auth bearers run with.
pub use fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS as REFRESH_BUFFER_SECS;

/// The mint step, boxed so the caching wrapper is transport-agnostic (and
/// unit-testable without a live connection). Returns `(token, expires_at_secs)`.
type Minter = Box<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<(String, u64), ApiError>> + Send>> + Send + Sync,
>;

/// A [`BearerSource`] that runs a caller-supplied mint step and caches the
/// short-TTL token it returns (proactive refresh + `notify_401` invalidation).
pub struct WriteTokenBearer {
    minter: Minter,
    /// `(token, expires_at_unix_secs)`; `None` until first mint / after a 401.
    cached: Mutex<Option<(String, u64)>>,
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

impl WriteTokenBearer {
    /// Build a bearer over a **caller-supplied** mint step. The caller owns the
    /// mint call and its error classification: a grant refusal should surface
    /// as [`ApiError::Status`] `403` so the transfer path fails hard rather than
    /// retrying forever, while a genuine transport fault keeps its
    /// [`ApiError::Transport`] framing (the folder plane's
    /// `fauna_sync_engine::write_token_bearer::classify_mint_error` is the
    /// reference for why that distinction is load-bearing).
    pub fn from_minter<F, Fut>(mint: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(String, u64), ApiError>> + Send + 'static,
    {
        Self {
            minter: Box::new(move || Box::pin(mint())),
            cached: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_minter(minter: Minter) -> Self {
        Self {
            minter,
            cached: Mutex::new(None),
        }
    }
}

#[async_trait]
impl BearerSource for WriteTokenBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        if let Some((tok, exp)) = self.cached.lock().unwrap().clone()
            && exp > now_secs() + REFRESH_BUFFER_SECS
        {
            return Ok(tok);
        }
        let (tok, exp) = (self.minter)().await?;
        *self.cached.lock().unwrap() = Some((tok.clone(), exp));
        Ok(tok)
    }

    async fn notify_401(&self) {
        // The token the byte route just rejected is dead (revoked / expired past
        // our buffer / server restart) — drop it so the next bearer() re-mints.
        *self.cached.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counting_minter(expires_at: u64) -> (Minter, Arc<AtomicUsize>) {
        let mints = Arc::new(AtomicUsize::new(0));
        let m = Arc::clone(&mints);
        let minter: Minter = Box::new(move || {
            let m = Arc::clone(&m);
            Box::pin(async move {
                let n = m.fetch_add(1, Ordering::SeqCst);
                Ok((format!("tok-{n}"), expires_at))
            })
        });
        (minter, mints)
    }

    #[tokio::test]
    async fn caches_a_fresh_token_and_refreshes_a_stale_one() {
        // A token valid far in the future → the second bearer() reuses it.
        let (minter, mints) = counting_minter(now_secs() + 3600);
        let bearer = WriteTokenBearer::with_minter(minter);
        assert_eq!(bearer.bearer().await.unwrap(), "tok-0");
        assert_eq!(
            bearer.bearer().await.unwrap(),
            "tok-0",
            "a still-fresh token is reused, not re-minted"
        );
        assert_eq!(mints.load(Ordering::SeqCst), 1, "exactly one mint");

        // notify_401 invalidates → the next bearer() re-mints.
        bearer.notify_401().await;
        assert_eq!(
            bearer.bearer().await.unwrap(),
            "tok-1",
            "after a 401 the cache is dropped and re-minted"
        );
        assert_eq!(mints.load(Ordering::SeqCst), 2);

        // A token already within the refresh buffer is never cache-served.
        let (minter, mints) = counting_minter(now_secs() + REFRESH_BUFFER_SECS / 2);
        let bearer = WriteTokenBearer::with_minter(minter);
        let _ = bearer.bearer().await.unwrap();
        let _ = bearer.bearer().await.unwrap();
        assert_eq!(
            mints.load(Ordering::SeqCst),
            2,
            "a near-expiry token is re-minted every call (proactive refresh)"
        );
    }
}
