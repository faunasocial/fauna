//! A cached bearer token behind a double-checked-lock refresh guard — the
//! caching ceremony hand-copied byte for byte across the WS bearer family
//! ([`crate::ws_challenge_bearer`], [`crate::ws_device_handshake_bearer`],
//! [`crate::ws_custody_handshake_bearer`]): each mints over its own ceremony,
//! but all three implemented the identical "serve a fresh cached token;
//! else take a refresh lock, re-check (a concurrent caller may have refreshed
//! while this one waited), else mint" discipline.
//!
//! **One clock.** Every deadline here — the spend rule and the own-id pruning —
//! compares [`MintedBearer::expires_at`] against this device's clock, which is
//! only sound because the shared mints anchor it on that same clock at receipt
//! (`now + expires_in`, `login.md` § Token lifetime on the client's clock).

use fauna_anon_client::MintedBearer;
use fauna_nest_http::ApiError;

/// Pre-expiry buffer — owned by [`fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS`]
/// so this cache, `fauna_nest_http`'s `LaunchMachineBearer`, `fauna-sync-engine`'s
/// `WriteTokenBearer` and the `fauna-launch-machine` TTL loop cannot drift.
use fauna_protocol::auth::BEARER_REFRESH_BUFFER_SECS as REFRESH_BUFFER_SECS;

use fauna_protocol::auth::OwnSessionIds;

/// A [`MintedBearer`] slot plus the double-checked-lock refresh discipline,
/// plus **the set of session ids this cache minted** — the current one and
/// every earlier own id not yet expired (`docs/goal/behavior/devices.md`
/// § The client's own session). All three `Ws*Bearer`s hold one of
/// these, so keeping the set here gives every one of them its own-session
/// answer for free rather than three hand-copies of the same bookkeeping.
#[derive(Default)]
pub(crate) struct TokenCache {
    cache: tokio::sync::RwLock<Option<MintedBearer>>,
    /// Memory only, never persisted — so a relaunched process has forgotten
    /// its previous run's ids, the bound `devices.md` states rather than
    /// hides. Deliberately a **separate** lock from `cache`: it outlives the
    /// slot, since [`Self::clear`] empties the slot on a 401 while the ids it
    /// minted stay listed nest-side until they expire.
    own_ids: tokio::sync::RwLock<OwnSessionIds>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl TokenCache {
    fn fresh_enough(c: &MintedBearer) -> bool {
        Self::fresh_enough_at(c, now_secs())
    }

    /// Pure freshness comparison, `now` passed in so tests can pin the
    /// `None` (failed clock read) direction without mocking the system
    /// clock. `None` — never proved fresh, never served: a bearer whose
    /// freshness we cannot compute is treated the same as one already past
    /// its buffer.
    fn fresh_enough_at(c: &MintedBearer, now: Option<u64>) -> bool {
        match now {
            Some(now) => c.expires_at > now.saturating_add(REFRESH_BUFFER_SECS),
            None => false,
        }
    }

    /// Fast path: a still-fresh cached token, no lock contention. Slow path:
    /// take `refresh_lock` (serializing concurrent refreshes onto one mint),
    /// re-check, else call `fetch` and cache its result.
    pub(crate) async fn bearer<F, Fut>(&self, fetch: F) -> Result<String, ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<MintedBearer, ApiError>>,
    {
        self.bearer_with_expiry(fetch).await.map(|(token, _)| token)
    }

    /// [`Self::bearer`] plus the served bearer's [`MintedBearer::expires_at`]
    /// (this device's clock, anchored at receipt) — the pair the
    /// `BearerSource::bearer_with_expiry` overrides publish.
    pub(crate) async fn bearer_with_expiry<F, Fut>(
        &self,
        fetch: F,
    ) -> Result<(String, u64), ApiError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<MintedBearer, ApiError>>,
    {
        if let Some(c) = self.cache.read().await.as_ref()
            && Self::fresh_enough(c)
        {
            return Ok((c.token.clone(), c.expires_at));
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some(c) = self.cache.read().await.as_ref()
            && Self::fresh_enough(c)
        {
            return Ok((c.token.clone(), c.expires_at));
        }
        let minted = fetch().await?;
        let token = (minted.token.clone(), minted.expires_at);
        {
            // Record BEFORE the slot is published, so a reader that sees the
            // new token can never fail to see the id that names it.
            let mut own = self.own_ids.write().await;
            own.record(minted.token_id.clone(), minted.expires_at);
            own.prune(now_secs());
        }
        *self.cache.write().await = Some(minted);
        Ok(token)
    }

    /// Every own session id still live — what a sessions surface folds into
    /// its one "this app" row.
    pub(crate) async fn own_token_ids(&self) -> Vec<String> {
        self.own_token_ids_at(now_secs()).await
    }

    /// The current session id, read at call time — `keep_token_id`'s source.
    pub(crate) async fn current_token_id(&self) -> Option<String> {
        self.current_token_id_at(now_secs()).await
    }

    /// `now` passed in so the pruning direction is pinnable without mocking
    /// the system clock — the same discipline [`Self::fresh_enough_at`] uses.
    async fn own_token_ids_at(&self, now: Option<u64>) -> Vec<String> {
        self.own_ids.read().await.ids_at(now)
    }

    async fn current_token_id_at(&self, now: Option<u64>) -> Option<String> {
        self.own_ids.read().await.current_at(now)
    }

    /// The held bearer's schedule, read WITHOUT minting and without waiting —
    /// the e2e wrong-clock witness's `launch_token` observable
    /// (`fauna_e2e_agent::LAUNCH_TOKEN_KEY`), published from an app's
    /// synchronous state builder. `None` only while a mint holds a lock (the
    /// caller publishes "cannot answer yet"); otherwise the held deadline
    /// (`None` inside when no bearer is cached) and every live own id, both
    /// on the one client clock.
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub(crate) fn peek_for_test(&self) -> Option<crate::auth_client::HeldBearerForTest> {
        let expires_at_secs = self.cache.try_read().ok()?.as_ref().map(|c| c.expires_at);
        let own_session_ids = self.own_ids.try_read().ok()?.ids_at(now_secs());
        Some(crate::auth_client::HeldBearerForTest {
            expires_at_secs,
            own_session_ids,
        })
    }

    /// Drop the cached token — the server just rejected it (401), so the next
    /// [`Self::bearer`] call re-mints.
    ///
    /// **The own-id set is deliberately untouched.** A 401 says this process
    /// may not use that bearer; it does not say the session stopped existing,
    /// and the row keeps being listed nest-side until it expires. Clearing the
    /// set here would make the app's own row paint as a stranger for the rest
    /// of that hour — the precise failure `devices.md` § The client's own
    /// session introduces the set to prevent.
    pub(crate) async fn clear(&self) {
        *self.cache.write().await = None;
    }
}

/// A cache freshness check must never panic on a pre-1970 clock — but unlike
/// [`fauna_core::data::Timestamp::now_secs_or_zero`], a failed read must not
/// fold to epoch-0 either: `expires_at` is an absolute *future* deadline (on
/// this client's clock, anchored at receipt), so
/// comparing it against a folded `now = 0` reads as "maximally fresh", the
/// opposite of the "maximally stale, forcing a re-mint" this cache wants. Returning `None` on failure and letting `fresh_enough_at`
/// treat that as unconditionally stale gets the direction right instead.
///
/// It is the ONE client clock ([`fauna_protocol::client_clock`]) the shared
/// mint anchors `expires_at` on, e2e offset included — so the wrong-clock
/// witness's offset reaches this cache's refresh decision, not only the
/// launch machine's.
fn now_secs() -> Option<u64> {
    fauna_protocol::client_clock::now_secs()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn minted(token: &str, ttl_secs: u64) -> MintedBearer {
        minted_with_id(token, &format!("id-{token}"), ttl_secs)
    }

    fn minted_with_id(token: &str, token_id: &str, ttl_secs: u64) -> MintedBearer {
        MintedBearer {
            token: token.to_string(),
            token_id: token_id.to_string(),
            expires_at: now_secs().expect("system clock is before Unix epoch") + ttl_secs,
        }
    }

    /// The spend rule and the own-id pruning read the ONE client clock
    /// (`fauna_protocol::client_clock`) the mint anchors on — the refresh
    /// clock of the four UniFFI apps. Were it the real
    /// clock while the anchor read the skewed one, a case-M-shaped witness on
    /// those seats would compare two different clocks; were both real, it
    /// would pass without testing anything. The offset is process-global:
    /// held for a single read, microseconds, and reset before returning.
    #[test]
    fn the_spend_rule_reads_the_shared_client_clock() {
        use fauna_protocol::client_clock;
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                client_clock::set_clock_offset_secs(0);
            }
        }
        let _reset = Reset;
        let real = fauna_core::data::Timestamp::now_secs_or_zero() as u64;
        client_clock::set_clock_offset_secs(6 * 3600);
        let seen = now_secs().expect("six hours ahead is readable");
        client_clock::set_clock_offset_secs(0);
        let ahead_by = seen.saturating_sub(real);
        assert!(
            (6 * 3600 - 10..=6 * 3600 + 10).contains(&ahead_by),
            "the cache's spend rule must read the skewed client clock; ahead by {ahead_by} s"
        );
    }

    /// The e2e peek reads the held schedule without minting: nothing before
    /// the first mint, then exactly the minted deadline and id.
    #[tokio::test]
    async fn the_e2e_peek_reads_the_held_schedule_without_minting() {
        let tc = TokenCache::default();
        let empty = tc.peek_for_test().expect("no mint in flight");
        assert_eq!(empty.expires_at_secs, None);
        assert!(empty.own_session_ids.is_empty());

        let minted = minted_with_id("t", "id-one", 3600);
        let expires_at = minted.expires_at;
        tc.bearer(|| async { Ok(minted) }).await.unwrap();
        let held = tc.peek_for_test().expect("no mint in flight");
        assert_eq!(held.expires_at_secs, Some(expires_at));
        assert_eq!(held.own_session_ids, vec!["id-one".to_string()]);
    }

    #[tokio::test]
    async fn a_fresh_cache_hit_never_calls_fetch() {
        let tc = TokenCache::default();
        *tc.cache.write().await = Some(minted("cached", 3600));
        let calls = AtomicUsize::new(0);
        let token = tc
            .bearer(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(minted("should-not-mint", 3600))
            })
            .await
            .unwrap();
        assert_eq!(token, "cached");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_empty_cache_mints_once_and_caches_the_result() {
        let tc = TokenCache::default();
        let calls = AtomicUsize::new(0);
        let token = tc
            .bearer(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(minted("fresh", 3600))
            })
            .await
            .unwrap();
        assert_eq!(token, "fresh");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Second call is a cache hit: no second mint.
        let token2 = tc
            .bearer(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(minted("should-not-mint", 3600))
            })
            .await
            .unwrap();
        assert_eq!(token2, "fresh");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_token_inside_the_refresh_buffer_is_not_served_from_cache() {
        let tc = TokenCache::default();
        // Expires in 10s — inside BEARER_REFRESH_BUFFER_SECS, so treated as
        // stale even though it has not technically expired yet.
        *tc.cache.write().await = Some(minted("about-to-expire", 10));
        let calls = AtomicUsize::new(0);
        let token = tc
            .bearer(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(minted("refreshed", 3600))
            })
            .await
            .unwrap();
        assert_eq!(token, "refreshed");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // The following two tests fix `expires_at`/`now` as ABSOLUTE literals
    // rather than deriving them from `now_secs()` via `minted()` — a
    // relative fixture cancels out under a clock-reading mutation (both
    // sides shift together), so it cannot pin the freshness direction. A
    // fixed pair can and does: reverting `fresh_enough_at`'s `None` arm to
    // fold toward 0 (as the pre-fix code effectively did, since the huge
    // `expires_at` below would then read `> 0 + 60`) flips this red.

    #[test]
    fn a_failed_clock_read_is_never_maximally_fresh() {
        // A real nest-minted deadline, comfortably in the future under any
        // real `now` — the exact shape the bug served indefinitely.
        let c = MintedBearer {
            token: "t".to_string(),
            token_id: "id-t".to_string(),
            expires_at: 1_756_000_000,
        };
        assert!(!TokenCache::fresh_enough_at(&c, None));
    }

    #[test]
    fn fresh_enough_at_compares_an_absolute_deadline_against_an_absolute_now() {
        let c = MintedBearer {
            token: "t".to_string(),
            token_id: "id-t".to_string(),
            expires_at: 1_000_000_100,
        };
        // Comfortably past the 60s buffer.
        assert!(TokenCache::fresh_enough_at(&c, Some(1_000_000_000)));
        // Inside the buffer (50s remaining < 60s).
        assert!(!TokenCache::fresh_enough_at(
            &MintedBearer {
                token: "t".to_string(),
                token_id: "id-t".to_string(),
                expires_at: 1_000_000_050,
            },
            Some(1_000_000_000)
        ));
    }

    #[tokio::test]
    async fn clear_forces_the_next_call_to_re_mint() {
        let tc = TokenCache::default();
        *tc.cache.write().await = Some(minted("cached", 3600));
        tc.clear().await;
        let calls = AtomicUsize::new(0);
        let token = tc
            .bearer(|| async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(minted("re-minted", 3600))
            })
            .await
            .unwrap();
        assert_eq!(token, "re-minted");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    // ── The own-session-id set (`devices.md` § The client's own session) ──

    /// The renewal case the whole set exists for: an app that has renewed once
    /// holds two live nest rows, and both are its own. With only the current
    /// id the predecessor paints as an unknown second session and *sign out
    /// everywhere else* appears to find and kill a stranger.
    #[tokio::test]
    async fn two_successive_mints_keep_both_own_ids_until_the_first_expires() {
        let tc = TokenCache::default();
        // First mint lands inside the refresh buffer, so the second `bearer()`
        // really re-mints rather than serving the cache.
        let first = minted_with_id("t1", "aaaaaaaaaaaaaaaa", 10);
        let first_expiry = first.expires_at;
        tc.bearer(|| async move { Ok(first) }).await.unwrap();
        tc.bearer(|| async { Ok(minted_with_id("t2", "bbbbbbbbbbbbbbbb", 3600)) })
            .await
            .unwrap();

        assert_eq!(
            tc.own_token_ids().await,
            vec![
                "aaaaaaaaaaaaaaaa".to_string(),
                "bbbbbbbbbbbbbbbb".to_string()
            ],
            "the predecessor is still listed nest-side, so it is still ours"
        );
        // Past the predecessor's own deadline, only the successor remains.
        assert_eq!(
            tc.own_token_ids_at(Some(first_expiry + 1)).await,
            vec!["bbbbbbbbbbbbbbbb".to_string()]
        );
    }

    /// `keep_token_id` is the CURRENT id read at call time — the newest mint,
    /// never a predecessor a painted list happens to name.
    #[tokio::test]
    async fn current_token_id_is_always_the_newest_mint() {
        let tc = TokenCache::default();
        assert_eq!(tc.current_token_id().await, None, "nothing minted yet");
        tc.bearer(|| async { Ok(minted_with_id("t1", "aaaaaaaaaaaaaaaa", 10)) })
            .await
            .unwrap();
        assert_eq!(
            tc.current_token_id().await.as_deref(),
            Some("aaaaaaaaaaaaaaaa")
        );
        tc.bearer(|| async { Ok(minted_with_id("t2", "bbbbbbbbbbbbbbbb", 3600)) })
            .await
            .unwrap();
        assert_eq!(
            tc.current_token_id().await.as_deref(),
            Some("bbbbbbbbbbbbbbbb"),
            "a renewal between paint and press must name the new token"
        );
    }

    /// The 401 path drops the token, NOT the identity of the session it named.
    #[tokio::test]
    async fn clear_drops_the_token_but_keeps_the_unexpired_own_ids() {
        let tc = TokenCache::default();
        tc.bearer(|| async { Ok(minted_with_id("t1", "aaaaaaaaaaaaaaaa", 3600)) })
            .await
            .unwrap();
        tc.clear().await;
        assert!(tc.cache.read().await.is_none(), "the token itself is gone");
        assert_eq!(
            tc.own_token_ids().await,
            vec!["aaaaaaaaaaaaaaaa".to_string()],
            "a 401 does not unlist the session nest-side, so it stays ours"
        );
    }

    #[tokio::test]
    async fn a_fetch_error_propagates_and_leaves_the_cache_empty() {
        let tc = TokenCache::default();
        let err = tc
            .bearer(|| async { Err(ApiError::Transport("boom".to_string())) })
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Transport(msg) if msg == "boom"));
        assert!(tc.cache.read().await.is_none());
    }
}
