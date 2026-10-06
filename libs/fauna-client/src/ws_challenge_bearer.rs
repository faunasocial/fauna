//! [`WsChallengeBearer`] — a [`BearerSource`] that mints its bearer over the
//! pre-identity **WS-RPC** silent challenge, `fauna.auth.challenge` +
//! `fauna.auth.verify`.
//!
//! It is the client-side bearer holder for the `AuthClient` path — a 1-hour TTL
//! cache with a 60 s pre-expiry buffer + double-checked-lock refresh
//! ([`TokenCache`]) and a `notify_401` clear-on-401 — minting each token over a
//! one-shot anonymous connection (`fauna_anon_client::AnonymousNestClient`,
//! `GET /api/v1/ws`, no bearer) through the shared
//! [`fauna_anon_client::mint_bearer_over_silent_challenge`]. Wired in at
//! [`crate::AuthClient::new`], so it is the mint behind `FfiNestClient` (the
//! four UniFFI apps); the `fauna-ffi` `mint_bearer`
//! export — for an app that keeps a bearer outside `FfiNestClient` — rides the
//! same shared mint through [`mint_bearer_over_silent_challenge`].
//!
//! **Why the silent challenge and not `fauna.auth.handshake`** (`login.md`
//! § When to use which, ruled 2026-09-21): the handshake signs a client
//! timestamp the nest holds to ±30 s, the wrong freshness rule for a user's
//! device — a device hours off would be refused at its first refresh. The
//! challenge signs the nest's own nonce instead, and the cache compares a
//! deadline anchored on this device's clock at receipt (`now + expires_in`,
//! § Token lifetime on the client's clock), so a wrong clock neither refuses
//! the mint nor mis-schedules the refresh. `apps/fauna-linux`'s and tui's
//! `LaunchMachineBearer` hold to the same ruling over `fauna-launch-machine`.
//!
//! **Per-request `client_nonce`:** a fresh nonce is folded into the verify
//! reply's channel-binding proof (the NT-1 hardening) and, on `https://`, the
//! binding is graduated against the captured TLS cert (security.md § Transport
//! trust, Axis 1) before the minted bearer is trusted.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_anon_client::AnonClientError;
use fauna_nest_http::{ApiError, BearerSource};
use fauna_protocol::RpcError;

use crate::auth_client::SharedNestUrl;
use crate::token_cache::TokenCache;

/// [`BearerSource`] that mints its bearer over the WS-RPC silent challenge
/// (`fauna.auth.{challenge,verify}`). Holds the actor id (the 32-byte public key
/// the nest knows the account by — the signing key's own public half) and the
/// signing key; constructs a fresh anonymous connection per token fetch (one
/// short-lived connection, no reuse).
pub struct WsChallengeBearer {
    /// Base URL of the nest (`http(s)://…`, no trailing slash); the connector
    /// swaps the scheme to `ws(s)://` internally. A **shared** cell so an
    /// SRV-reconnect serving-port swap done by the owning [`crate::AuthClient`]
    /// (`try_srv_recover`) is seen by this mint too — read fresh per
    /// [`fetch_token`](Self::fetch_token).
    nest_url: SharedNestUrl,
    actor_id: [u8; 32],
    signing_key: ed25519_dalek::SigningKey,
    token_cache: TokenCache,
    /// Set when a mint is refused because this identity has been succeeded.
    /// Read by the reconnect supervisor (to stop retrying a refusal no retry can
    /// clear) and by the app (to route the user to the import flow).
    superseded: SupersededLatch,
    /// Set when a mint is refused because the account is locked out, and
    /// standing until the lock's own `locked_until`. Read by the reconnect
    /// supervisor (to hold its dials until then) and by the app (to name the
    /// unlock time).
    locked: LockedLatch,
    /// The nest-side refusal (if any) the most recent mint attempt met. Read
    /// by a caller that already has an `Auth(String)` in hand and needs to
    /// tell "the nest refused this" from "no nest answered" — see
    /// [`LastAuthRefusal`].
    last_refusal: LastAuthRefusal,
}

impl WsChallengeBearer {
    pub fn new(
        nest_url: impl Into<String>,
        actor_id: [u8; 32],
        signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        let url = nest_url.into().trim_end_matches('/').to_string();
        Self::with_shared_url(Arc::new(std::sync::RwLock::new(url)), actor_id, signing_key)
    }

    /// Like [`new`](Self::new) but over a **shared** URL cell — used by
    /// [`crate::AuthClient::new`] so the bearer's token mint and the data-plane
    /// WS read the *same* live nest URL, letting an SRV-reconnect serving-port
    /// swap (`AuthClient::try_srv_recover`) reach the mint too. The caller owns
    /// the trailing-slash normalization (`AuthClient::new` trims before building
    /// the cell).
    pub(crate) fn with_shared_url(
        nest_url: SharedNestUrl,
        actor_id: [u8; 32],
        signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        Self {
            nest_url,
            actor_id,
            signing_key,
            token_cache: TokenCache::default(),
            superseded: SupersededLatch::default(),
            locked: LockedLatch::default(),
            last_refusal: LastAuthRefusal::default(),
        }
    }

    /// The superseded-refusal channel this mint publishes on — cloned by the
    /// reconnect supervisor and the app shell, which share the one latch.
    pub fn superseded_latch(&self) -> SupersededLatch {
        self.superseded.clone()
    }

    /// The locked-refusal channel this mint publishes on — cloned by
    /// [`crate::AuthClient`], whose reconnect supervisor holds on it.
    pub fn locked_latch(&self) -> LockedLatch {
        self.locked.clone()
    }

    /// The last-auth-refusal channel this mint publishes on — cloned by
    /// [`crate::AuthClient`] and exposed as
    /// [`AuthClient::last_auth_refusal`](crate::AuthClient::last_auth_refusal).
    pub fn last_auth_refusal_latch(&self) -> LastAuthRefusal {
        self.last_refusal.clone()
    }

    /// The held bearer's schedule without minting — [`TokenCache`]'s e2e peek
    /// (see [`crate::AuthClient::held_bearer_for_test`]).
    #[cfg(any(debug_assertions, feature = "e2e-agent"))]
    pub(crate) fn held_bearer_for_test(&self) -> Option<crate::auth_client::HeldBearerForTest> {
        self.token_cache.peek_for_test()
    }

    /// One silent-challenge mint via the shared
    /// [`fauna_anon_client::mint_bearer_over_silent_challenge`] — the
    /// [`TokenCache::bearer`] `fetch` callback.
    async fn fetch_mint(&self) -> Result<MintedBearer, ApiError> {
        // Read the *current* URL out of the shared cell (clone, don't hold the
        // sync guard across the await) — picks up any SRV-reconnect port swap.
        let url = self
            .nest_url
            .read()
            .expect("nest_url lock poisoned")
            .clone();
        // A supersession never un-happens (`succession-propagation.md`: the
        // refusal is terminal for a correct client), so once it has latched,
        // every later mint answers it again WITHOUT dialling. Before this, each
        // bearer read on a retired identity's client — a content-API put, an
        // index flush, a supervisor refresh — spent a fresh anonymous dial (and a
        // slot of the process's per-nest dial budget) to be told the same thing
        // (`transport-connection.md` § No dialer outlives its owner).
        if let Some(refusal) = self.superseded.get() {
            return Err(map_anon_err(AnonClientError::Rpc(refusal), &url));
        }
        // A lock is terminal until its own `locked_until` (`devices.md` § The
        // locked state): while it stands, every mint answers it again without
        // dialling, exactly as a supersession does — and unlike one it lapses,
        // after which the next mint dials and finds out.
        if let Some(refusal) = self.locked.standing() {
            return Err(map_anon_err(AnonClientError::Rpc(refusal), &url));
        }
        // Delegates to the same shared mint as the free
        // [`mint_bearer_over_silent_challenge`], but maps its error through the
        // latches: this is the path the reconnect loop drives, so it is the
        // one that must not drop a supersession — or an ordinary refusal's
        // cause — on the floor.
        match fauna_anon_client::mint_bearer_over_silent_challenge(&url, &self.signing_key).await {
            Ok(minted) => {
                // A fresh mint proves the last attempt was not a refusal —
                // clear it so a caller reading the latch after THIS success
                // never sees an earlier attempt's stale cause.
                self.last_refusal.set(None);
                self.locked.clear();
                Ok(minted)
            }
            Err(e) => Err(map_anon_err_latching(
                e,
                &url,
                &self.superseded,
                &self.locked,
                &self.last_refusal,
            )),
        }
    }
}

/// Re-exported from the leaf `fauna-anon-client` crate — the mint's *logic* moved
/// there (next to the anonymous connect + channel-binding graduation it
/// composes), so existing `fauna_client::ws_challenge_bearer::MintedBearer` paths
/// keep resolving.
pub use fauna_anon_client::MintedBearer;

/// Mint an app-held bearer over the silent challenge, **delegating** to the
/// shared [`fauna_anon_client::mint_bearer_over_silent_challenge`] — the one
/// copy of the mint + TLS channel-binding graduation logic (its leaf home,
/// priority #2/#4) — and mapping its error into the `BearerSource`
/// [`ApiError`] taxonomy. The returned `expires_at` is on this device's clock
/// (anchored at receipt). The `fauna-ffi` `mint_bearer` export calls it so a
/// native UniFFI app keeping a bearer outside `FfiNestClient` mints over the
/// same shared path (security.md § Transport trust; `login.md` § When to use
/// which).
pub async fn mint_bearer_over_silent_challenge(
    nest_url: &str,
    signing_key: &ed25519_dalek::SigningKey,
) -> Result<MintedBearer, ApiError> {
    fauna_anon_client::mint_bearer_over_silent_challenge(nest_url, signing_key)
        .await
        .map_err(|e| map_anon_err(e, nest_url))
}

#[async_trait]
impl BearerSource for WsChallengeBearer {
    async fn bearer(&self) -> Result<String, ApiError> {
        self.token_cache.bearer(|| self.fetch_mint()).await
    }

    async fn bearer_with_expiry(&self) -> Result<(String, Option<u64>), ApiError> {
        let (token, expires_at) = self
            .token_cache
            .bearer_with_expiry(|| self.fetch_mint())
            .await?;
        Ok((token, Some(expires_at)))
    }

    async fn notify_401(&self) {
        // Server-revoked or stale bearer — drop it; the next `bearer()` re-mints.
        self.token_cache.clear().await;
    }

    /// This mint signs `actor_id ‖ timestamp` with its own key, so the actor it
    /// mints for is simply the one it was built with — no state to consult.
    fn bearer_actor_id(&self) -> Option<[u8; 32]> {
        Some(self.actor_id)
    }

    /// The cache is the holder, so its set is this source's set
    /// (`docs/goal/behavior/devices.md` § The client's own session).
    async fn own_token_ids(&self) -> Vec<String> {
        self.token_cache.own_token_ids().await
    }

    async fn current_token_id(&self) -> Option<String> {
        self.token_cache.current_token_id().await
    }
}

/// The last `fauna.auth.superseded` refusal this client's bearer mint met, held
/// verbatim so nothing is lost at the boundary.
///
/// **Why a side channel rather than a wider error type.** The mint's error
/// taxonomy is [`ApiError`] — `fauna-nest-http`'s *HTTP* shape, flattened to a
/// status number and a string. Most handshake refusals survive that flattening
/// intact, but a supersession carries a **payload the UI must render**: the
/// successor to import (`identity-succession.md` § Propagation → *Own device
/// fleet*) — as a lock carries its unlock time, on [`LockedLatch`]. Widening
/// `ApiError` to carry it would push a WS-RPC succession concept into the HTTP
/// taxonomy for the sake of one code — so instead the refusal rides its own
/// additive channel, which is the shape
/// `fauna_ws_substrate::supervisor::ConnectionState` already prescribes for
/// naming a connect failure's cause ("Naming the cause needs a separate,
/// additive channel — not this state").
///
/// Deliberately holds the [`RpcError`], **not** a projected notice: the
/// projection (`fauna_client_recovery::SupersededNotice`) lives in the ceremony
/// crate, and a transport crate must not depend on it just to name a type. The
/// app already depends on both and projects at the point of render.
#[derive(Clone, Default)]
pub struct SupersededLatch(Arc<std::sync::Mutex<Option<RpcError>>>);

impl SupersededLatch {
    /// Record `err` if it is a superseded refusal; ignore it otherwise.
    ///
    /// Keyed on [`RpcError::superseded_by`], which answers `None` for every
    /// other code — so an unrelated refusal can never latch, and the UI can
    /// never route a merely-locked account into the import flow.
    pub fn observe(&self, err: &RpcError) {
        if err.superseded_by().is_some() {
            *self.0.lock().unwrap() = Some(err.clone());
        }
    }

    /// The latched refusal, if this identity has been succeeded.
    pub fn get(&self) -> Option<RpcError> {
        self.0.lock().unwrap().clone()
    }

    /// Whether a supersession has been observed — the supervisor's terminal
    /// test, kept allocation-free for the reconnect path.
    pub fn is_set(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }
}

/// The `fauna.auth.account_locked` refusal this client's bearer mint last met,
/// held verbatim — the lock's twin of [`SupersededLatch`], on its own additive
/// channel for the same reason: the flattened [`ApiError`] keeps "423" and
/// drops the `locked_until` the refusal carries, which is the one fact both
/// readers need (`devices.md` § The locked state).
///
/// **Terminal until then, not for ever.** A supersession never un-happens, so
/// its latch stays set; a lock lapses at `locked_until`, so this one *stands*
/// only while that time is ahead on the client clock
/// ([`fauna_protocol::client_clock`], the clock every bearer schedule reads).
/// While it stands the mint answers from it without dialling and the reconnect
/// supervisor holds its dials ([`Self::remaining`]); once it has lapsed the
/// next mint dials, and the nest's answer — a bearer, or a fresh refusal with
/// its own time — replaces it.
#[derive(Clone, Default)]
pub struct LockedLatch(Arc<std::sync::Mutex<Option<RpcError>>>);

impl LockedLatch {
    /// Record `err` if it is a locked refusal carrying its unlock time; any
    /// other refusal is the nest answering something else, which means the
    /// lock no longer stands in the way, so it clears the latch.
    ///
    /// Keyed on [`RpcError::locked_until_secs`], which answers `None` for
    /// every other code and for a malformed payload — a lock with no readable
    /// time cannot be held until anything, so it latches nothing and keeps the
    /// ordinary backoff.
    pub fn observe(&self, err: &RpcError) {
        *self.0.lock().unwrap() = err.locked_until_secs().map(|_| err.clone());
    }

    /// Drop the latch — a mint succeeded, so the account is not locked.
    pub(crate) fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }

    /// The unlock time (Unix seconds) of the last locked refusal, lapsed or
    /// not — what an app renders.
    pub fn locked_until_secs(&self) -> Option<u64> {
        self.0.lock().unwrap().as_ref()?.locked_until_secs()
    }

    /// How long the lock still stands, read on the client clock. `None` when
    /// nothing is latched, when the lock has lapsed, and when the clock cannot
    /// be read — an unreadable clock must not hold a client for ever, so it
    /// falls back to the ordinary dial-and-back-off.
    pub fn remaining(&self) -> Option<std::time::Duration> {
        Self::remaining_at(
            self.locked_until_secs()?,
            fauna_protocol::client_clock::now_secs()?,
        )
    }

    /// [`Self::remaining`] at an explicit `now_secs`.
    fn remaining_at(locked_until_secs: u64, now_secs: u64) -> Option<std::time::Duration> {
        locked_until_secs
            .checked_sub(now_secs)
            .filter(|secs| *secs > 0)
            .map(std::time::Duration::from_secs)
    }

    /// The latched refusal, while the lock still stands — what a mint answers
    /// in place of a dial.
    pub fn standing(&self) -> Option<RpcError> {
        self.remaining()?;
        self.0.lock().unwrap().clone()
    }
}

/// The most recent nest-side refusal this client's bearer mint has met — a
/// wire-level `Rpc` error that reached the nest and was refused, as opposed to
/// a transport fault that never got an answer.
///
/// **Why a side channel, mirroring [`SupersededLatch`] right above.** By the
/// time a mint failure has crossed the `BearerSource` boundary it is a
/// flattened [`ApiError`] (a status number and a string), and
/// `auth_client.rs`'s `map_api_err` flattens it one step further into
/// [`crate::NestClientError::Auth`] — the SAME variant a transport fault
/// during the same mint also lands in. Nothing in `Auth(String)` tells the
/// two apart at that point without parsing its text, which is exactly what
/// this channel exists to avoid: a caller reads it, right after an `Auth(_)`
/// failure, to name a connect refusal by its cause instead of logging it as an
/// offline nest (`file-sync.md` § 1 Device Registration).
///
/// **Overwritten on every mint attempt** — `Some` on a refusal, `None` on a
/// transport fault or a success — so a stale refusal from an earlier attempt
/// is never read as the cause of a later, different failure. Unlike
/// [`SupersededLatch`] (which latches once and stays latched — a
/// supersession never un-happens), this one tracks only the *most recent*
/// attempt, because an ordinary refusal (wrong signature, not yet
/// registered) can clear on a later, successful mint.
#[derive(Clone, Default)]
pub struct LastAuthRefusal(Arc<std::sync::Mutex<Option<RpcError>>>);

impl LastAuthRefusal {
    pub(crate) fn set(&self, refusal: Option<RpcError>) {
        *self.0.lock().unwrap() = refusal;
    }

    /// The refusal the most recent mint attempt met, if it was one.
    pub fn get(&self) -> Option<RpcError> {
        self.0.lock().unwrap().clone()
    }
}

/// Map the anonymous connector's error onto the `BearerSource` taxonomy. A
/// wire-level `RpcError` (the nest refused the handshake — lockout, drift, bad
/// sig, private-nest reject) becomes [`ApiError::Status`] with a representative
/// HTTP-equivalent code so existing `ApiError::Status` consumers keep working;
/// a graduation failure that is the **identity-changed verdict** becomes
/// [`ApiError::NestIdentityChanged`]; every other transport/decode/disconnect
/// failure becomes [`ApiError::Transport`].
///
/// `nest_url` is the URL this mint dialled — the identity arm needs the same
/// authority the graduation pinned against, because "no binding, but a pin
/// EXISTS for this host" is itself one of the verdicts
/// (`fauna_anon_client::classify_identity_changed`).
///
/// [`map_anon_err_latching`] is the same mapping plus the one refusal that
/// cannot survive it — see [`SupersededLatch`].
pub(crate) fn map_anon_err(e: AnonClientError, nest_url: &str) -> ApiError {
    match e {
        // The nest's pinned identity changed (or a pinned nest stopped proving
        // any identity). Before this arm the verdict fell through to
        // `Transport` below, where the reconnect supervisor read a MITM signal
        // as a transient blip and retried it forever
        // (`security.md` § Post-auth surfacing — the plumbing rule).
        AnonClientError::Trust(ref trust_err) => {
            let host = fauna_anon_client::authority_of(nest_url);
            match fauna_anon_client::classify_identity_changed(trust_err, &host) {
                Some(v) => {
                    // Logged here, at the classification, exactly as the silent-
                    // challenge channel's `classify_silent_challenge` does — one
                    // record of what was pinned vs seen, at the moment the
                    // verdict is reached, including the `fork` bit the taxonomy
                    // deliberately does not carry onward.
                    tracing::error!(
                        "[identity] nest identity changed for {host} (pinned {}, seen {:?}, fork \
                         evidence: {}) — dropping the bearer; the session blocks until the user \
                         re-trusts or walks away",
                        v.pinned_hex,
                        v.seen_hex,
                        v.fork
                    );
                    ApiError::NestIdentityChanged {
                        host,
                        pinned_hex: v.pinned_hex,
                        seen_hex: v.seen_hex,
                    }
                }
                // First-contact binding trouble or a root mismatch: real trust
                // failures, but not the `known_hosts` verdict — they keep the
                // generic mapping rather than blocking the session on a
                // re-trust surface that would not be the honest answer.
                None => ApiError::Transport(e.to_string()),
            }
        }
        AnonClientError::Rpc(err) => {
            // Mirror the HTTP twin's status mapping (auth_handlers.rs
            // `auth_error_to_rpc` is the inverse of the old HTTP status map).
            let code = match err.code.as_str() {
                "fauna.auth.account_locked" => 423,
                "fauna.auth.not_registered" => 403,
                "fauna.auth.signature_failed"
                | "fauna.auth.timestamp_drift"
                | "fauna.auth.invalid_request"
                | "fauna.auth.invalid_nonce" => 401,
                _ => 400,
            };
            ApiError::Status {
                code,
                message: err.code,
            }
        }
        // WebSocket/decode/disconnect/timeout trouble: nothing was proven
        // either way — retryable. Written as an exhaustive match (not a
        // `Trust`/`Rpc`-only catch-all) so a variant added to
        // `AnonClientError` later forces a decision here at compile time
        // instead of silently landing in this bucket, same shape as
        // `classify_core_failure`'s twin table one crate over.
        e @ (AnonClientError::Decode(_)
        | AnonClientError::WebSocket(_)
        | AnonClientError::RpcDisconnected { .. }
        | AnonClientError::RpcTimeout) => ApiError::Transport(e.to_string()),
    }
}

/// [`map_anon_err`], but a superseded refusal and a locked one are each latched
/// on the way past, and every refusal updates [`LastAuthRefusal`].
///
/// The mapping itself is unchanged — every existing `ApiError` consumer sees
/// exactly what it saw before — so this adds channels rather than changing one.
fn map_anon_err_latching(
    e: AnonClientError,
    nest_url: &str,
    superseded: &SupersededLatch,
    locked: &LockedLatch,
    last_refusal: &LastAuthRefusal,
) -> ApiError {
    match &e {
        AnonClientError::Rpc(err) => {
            superseded.observe(err);
            locked.observe(err);
            last_refusal.set(Some(err.clone()));
        }
        // Every non-`Rpc` variant is transport trouble — nothing was proven,
        // so any earlier attempt's refusal must not linger as this attempt's
        // cause.
        _ => last_refusal.set(None),
    }
    map_anon_err(e, nest_url)
}

/// Convenience: a `WsChallengeBearer` boxed as the `BearerSource` trait object
/// the [`crate::AuthClient`] holds.
pub fn ws_challenge_bearer(
    nest_url: impl Into<String>,
    actor_id: [u8; 32],
    signing_key: ed25519_dalek::SigningKey,
) -> Arc<dyn BearerSource> {
    Arc::new(WsChallengeBearer::new(nest_url, actor_id, signing_key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::{LocalizedText, RpcError};

    /// Any URL works for the arms that don't consult the pin store; the host it
    /// resolves to has no pin, which is what the "not the verdict" arms want.
    const TEST_URL: &str = "https://nest.example.com";

    #[test]
    fn rpc_error_maps_to_status_codes() {
        let mk = |code: &str| {
            map_anon_err(
                AnonClientError::Rpc(RpcError {
                    code: code.into(),
                    message: Box::new(LocalizedText::new("error.x")),
                    details: None,
                    extra: Default::default(),
                }),
                TEST_URL,
            )
        };
        assert!(matches!(
            mk("fauna.auth.account_locked"),
            ApiError::Status { code: 423, .. }
        ));
        assert!(matches!(
            mk("fauna.auth.not_registered"),
            ApiError::Status { code: 403, .. }
        ));
        assert!(matches!(
            mk("fauna.auth.signature_failed"),
            ApiError::Status { code: 401, .. }
        ));
    }

    #[test]
    fn a_superseded_refusal_keeps_its_successor_through_the_mint_boundary() {
        // The refusal every *other* device of a succeeded fleet meets on its
        // next connect (`identity-succession.md` § Propagation → Own device
        // fleet). It is the one handshake refusal that carries a payload the UI
        // must render — the successor to import — so unlike every other code in
        // `map_anon_err`'s table it cannot be flattened to a status number:
        // `SupersededNotice::from_error` reads the successor out of `details`,
        // and a client that lost it can only show a generic sign-in failure.
        let successor = [7u8; 32];
        let latch = SupersededLatch::default();
        let last_refusal = LastAuthRefusal::default();
        let err = map_anon_err_latching(
            AnonClientError::Rpc(RpcError::superseded(&successor)),
            TEST_URL,
            &latch,
            &LockedLatch::default(),
            &last_refusal,
        );

        // Still an `ApiError` for every existing consumer of the bearer source…
        assert!(matches!(err, ApiError::Status { .. }));
        // …and the refusal survives verbatim, which is the whole point: the
        // latch holds the `RpcError`, so the successor is still readable and
        // `fauna_client_recovery::SupersededNotice::from_error` can project it
        // without this transport crate depending on the ceremony crate.
        let latched = latch.get().expect("superseded refusal must latch");
        assert_eq!(latched.superseded_by(), Some(successor));
    }

    #[test]
    fn an_unrelated_refusal_latches_nothing_on_the_superseded_channel() {
        // `superseded_by` returns `None` for every other code, so the latch must
        // stay empty — a locked account is not a succeeded one, and routing it
        // to the import flow would be a lie.
        let latch = SupersededLatch::default();
        let last_refusal = LastAuthRefusal::default();
        let _ = map_anon_err_latching(
            AnonClientError::Rpc(RpcError {
                code: "fauna.auth.account_locked".into(),
                message: Box::new(LocalizedText::new("error.x")),
                details: None,
                extra: Default::default(),
            }),
            TEST_URL,
            &latch,
            &LockedLatch::default(),
            &last_refusal,
        );
        assert!(latch.get().is_none());
    }

    fn locked_refusal(locked_until_secs: u64) -> RpcError {
        RpcError {
            code: RpcError::CODE_ACCOUNT_LOCKED.into(),
            message: Box::new(LocalizedText::new("error.x")),
            details: Some(Box::new(fauna_protocol::Value::Integer(
                locked_until_secs.into(),
            ))),
            extra: Default::default(),
        }
    }

    fn client_now_secs() -> u64 {
        fauna_protocol::client_clock::now_secs().expect("the client clock reads")
    }

    /// The locked refusal keeps its unlock time through the mint boundary —
    /// the fact "423" drops, and the one both readers need (`devices.md` § The
    /// locked state): the supervisor to hold until then, the app to name it.
    #[test]
    fn a_locked_refusal_keeps_its_unlock_time_through_the_mint_boundary() {
        let until = client_now_secs() + 3600;
        let locked = LockedLatch::default();
        let err = map_anon_err_latching(
            AnonClientError::Rpc(locked_refusal(until)),
            TEST_URL,
            &SupersededLatch::default(),
            &locked,
            &LastAuthRefusal::default(),
        );

        // Still the 423 every existing `ApiError` consumer saw…
        assert!(matches!(err, ApiError::Status { code: 423, .. }));
        // …and the time survives beside it.
        assert_eq!(locked.locked_until_secs(), Some(until));
        let remaining = locked.remaining().expect("the lock still stands");
        assert!(
            (3590..=3600).contains(&remaining.as_secs()),
            "got {remaining:?}"
        );
        assert_eq!(
            locked.standing().and_then(|e| e.locked_until_secs()),
            Some(until)
        );
    }

    /// Terminal **until then**: a lapsed lock still names its time, but it no
    /// longer stands — nothing is held, and the next mint dials.
    #[test]
    fn a_lapsed_lock_no_longer_stands() {
        let until = client_now_secs() - 60;
        let locked = LockedLatch::default();
        locked.observe(&locked_refusal(until));
        assert_eq!(locked.locked_until_secs(), Some(until));
        assert!(locked.remaining().is_none());
        assert!(locked.standing().is_none());
        // The boundary itself: at `locked_until` the nest no longer refuses.
        assert!(LockedLatch::remaining_at(1_000, 1_000).is_none());
        assert_eq!(
            LockedLatch::remaining_at(1_000, 999),
            Some(std::time::Duration::from_secs(1))
        );
    }

    /// The nest answering anything else means the lock is not what stands in
    /// the way any more — and a lock with no readable time latches nothing,
    /// because there is no "then" to hold until.
    #[test]
    fn another_refusal_clears_the_locked_latch() {
        let locked = LockedLatch::default();
        locked.observe(&locked_refusal(client_now_secs() + 3600));
        assert!(locked.standing().is_some());

        locked.observe(&RpcError::new("fauna.auth.signature_failed", "error.x"));
        assert!(locked.locked_until_secs().is_none());

        locked.observe(&RpcError::new(RpcError::CODE_ACCOUNT_LOCKED, "error.x"));
        assert!(
            locked.locked_until_secs().is_none(),
            "a locked refusal with no unlock time cannot be held until anything"
        );
    }

    /// While the lock stands a mint answers it without dialling: nothing
    /// listens at this URL, so a mint that dialled would come back a transport
    /// fault, never the latched 423. Once the lock has lapsed, it dials again.
    #[tokio::test]
    async fn a_standing_lock_is_answered_without_dialling() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let kp = fauna_core::identity::ActorKeypair::generate();
        let b = WsChallengeBearer::new(url, kp.actor_id().0, kp.signing_key().clone());

        b.locked_latch()
            .observe(&locked_refusal(client_now_secs() + 3600));
        for _ in 0..3 {
            let err = b
                .bearer()
                .await
                .expect_err("a locked account mints nothing");
            assert!(
                matches!(&err, ApiError::Status { code: 423, .. }),
                "a standing lock must be answered from the latch, not by a dial; got {err:?}"
            );
        }

        b.locked_latch()
            .observe(&locked_refusal(client_now_secs() - 60));
        let err = b.bearer().await.expect_err("nothing listens");
        assert!(
            matches!(err, ApiError::Transport(_)),
            "a lapsed lock must dial again; got {err:?}"
        );
    }

    /// Every nest-side refusal — not just a superseded one — latches on the
    /// last-auth-refusal channel, which is what lets a caller name an
    /// ordinary "wrong signature" or "not yet registered" connect refusal by
    /// its cause too.
    #[test]
    fn an_ordinary_refusal_latches_as_the_last_auth_refusal() {
        let superseded = SupersededLatch::default();
        let last_refusal = LastAuthRefusal::default();
        let _ = map_anon_err_latching(
            AnonClientError::Rpc(RpcError {
                code: "fauna.auth.signature_failed".into(),
                message: Box::new(LocalizedText::new("error.x")),
                details: None,
                extra: Default::default(),
            }),
            TEST_URL,
            &superseded,
            &LockedLatch::default(),
            &last_refusal,
        );
        assert_eq!(
            last_refusal.get().map(|e| e.code),
            Some("fauna.auth.signature_failed".into())
        );
        // Not a supersession, so that channel stays empty — the two latches
        // are independent.
        assert!(superseded.get().is_none());
    }

    /// A transport fault clears any refusal a PRIOR attempt latched — the
    /// exact staleness this channel exists to avoid: a caller reading it
    /// right after this failure must not attribute it to an earlier, now
    /// irrelevant refusal.
    #[test]
    fn a_transport_fault_clears_a_prior_latched_refusal() {
        let superseded = SupersededLatch::default();
        let last_refusal = LastAuthRefusal::default();
        last_refusal.set(Some(RpcError::new(
            "fauna.auth.signature_failed",
            "error.x",
        )));
        let _ = map_anon_err_latching(
            AnonClientError::RpcTimeout,
            TEST_URL,
            &superseded,
            &LockedLatch::default(),
            &last_refusal,
        );
        assert!(last_refusal.get().is_none());
    }

    /// Pins each of `AnonClientError`'s six variants to its current verdict —
    /// the equivalence an exhaustive-match refactor must preserve, in the manner of
    /// `classify_core_failure`'s own `all_six_variants_pin_to_their_current_verdict`
    /// one crate over. A variant added to the enum without a matching entry
    /// here fails to compile, not just to pass: `map_anon_err`'s match is
    /// exhaustive over the enum, so the compiler forces the new arm before
    /// this table can even be extended.
    #[test]
    fn all_six_variants_pin_to_their_current_verdict() {
        let transport_cases = [
            AnonClientError::Decode("bad cbor".into()),
            AnonClientError::WebSocket("connection refused".into()),
            AnonClientError::RpcDisconnected {
                was_in_flight: true,
            },
            AnonClientError::RpcTimeout,
        ];
        for err in transport_cases {
            assert!(
                matches!(map_anon_err(err, TEST_URL), ApiError::Transport(_)),
                "every non-Trust, non-Rpc variant must stay the generic transport mapping"
            );
        }
        assert!(matches!(
            map_anon_err(
                AnonClientError::Rpc(RpcError {
                    code: "fauna.auth.account_locked".into(),
                    message: Box::new(LocalizedText::new("error.x")),
                    details: None,
                    extra: Default::default(),
                }),
                TEST_URL,
            ),
            ApiError::Status { .. }
        ));
        assert!(matches!(
            map_anon_err(
                AnonClientError::Trust(fauna_anon_client::TrustError::Identity(
                    fauna_anon_client::IdentityError::PinChanged {
                        pinned: [1u8; 32],
                        seen: [2u8; 32],
                    },
                )),
                TEST_URL,
            ),
            ApiError::NestIdentityChanged { .. }
        ));
    }

    /// **The plumbing rule of `security.md` § Post-auth surfacing**: the hourly
    /// bearer re-mint's identity verdict must cross this boundary as its own
    /// variant. Flattened to `Transport` it is indistinguishable from a network
    /// blip, and the reconnect supervisor — whose whole job is to retry
    /// transport failures — then retries a MITM signal forever
    /// (`reconnect.rs::connect_error_is_terminal` reads this variant).
    #[test]
    fn a_changed_pinned_identity_survives_the_mint_boundary_as_its_own_variant() {
        let err = map_anon_err(
            AnonClientError::Trust(fauna_anon_client::TrustError::Identity(
                fauna_anon_client::IdentityError::PinChanged {
                    pinned: [1u8; 32],
                    seen: [2u8; 32],
                },
            )),
            TEST_URL,
        );
        assert!(
            matches!(err, ApiError::NestIdentityChanged { .. }),
            "the identity verdict must not be flattened into the retryable bucket; got {err:?}"
        );
        // The fingerprints ride along, because that is what the surface's
        // detail line renders (`FfiError::NestIdentityChanged`'s field set).
        let ApiError::NestIdentityChanged {
            host,
            pinned_hex,
            seen_hex,
        } = err
        else {
            unreachable!()
        };
        assert_eq!(host, "nest.example.com");
        assert_eq!(pinned_hex, "01".repeat(32));
        assert_eq!(seen_hex.as_deref(), Some("02".repeat(32).as_str()));
    }

    /// Rotation-chain **fork evidence** is the same blocking surface (it differs
    /// only in offering no re-trust, which the launch machine decides from the
    /// snapshot — not something this taxonomy carries).
    #[test]
    fn fork_evidence_takes_the_same_arm_as_a_plain_change() {
        let err = map_anon_err(
            AnonClientError::Trust(fauna_anon_client::TrustError::Identity(
                fauna_anon_client::IdentityError::PinForked {
                    pinned: [1u8; 32],
                    seen: [2u8; 32],
                },
            )),
            TEST_URL,
        );
        assert!(
            matches!(err, ApiError::NestIdentityChanged { .. }),
            "got {err:?}"
        );
    }

    /// The negative half, and it matters as much as the positive one: a trust
    /// failure that is **not** the `known_hosts` verdict must keep the generic
    /// mapping. `PinRequired` is a consumer process reaching a nest before the
    /// interactive app minted the pin — an ordinary retryable state, and
    /// blocking the session on a re-trust surface for it would be a lie.
    #[test]
    fn an_ordinary_trust_failure_is_not_the_identity_verdict() {
        let err = map_anon_err(
            AnonClientError::Trust(fauna_anon_client::TrustError::Identity(
                fauna_anon_client::IdentityError::PinRequired,
            )),
            TEST_URL,
        );
        assert!(matches!(err, ApiError::Transport(_)), "got {err:?}");

        // Likewise a root mismatch: a real trust failure, but not the pinned-
        // identity-changed one.
        let err = map_anon_err(
            AnonClientError::Trust(fauna_anon_client::TrustError::Identity(
                fauna_anon_client::IdentityError::RootMismatch,
            )),
            TEST_URL,
        );
        assert!(matches!(err, ApiError::Transport(_)), "got {err:?}");
    }

    /// First-contact binding trouble on a host with **no pin** is not the
    /// verdict either — that arm turns on the pin's existence, which is the one
    /// part of the rule that reads state rather than the error value.
    #[test]
    fn a_binding_failure_with_no_pin_stays_transport() {
        let err = map_anon_err(
            AnonClientError::Trust(fauna_anon_client::TrustError::BindingRequired),
            "https://never-pinned.example",
        );
        assert!(matches!(err, ApiError::Transport(_)), "got {err:?}");
    }

    /// Once a supersession has latched, a mint answers it again without
    /// dialling: the nest here refuses every TCP connect (nothing listens), so a
    /// mint that dialled would come back as a transport fault, never as the
    /// latched refusal.
    #[tokio::test]
    async fn a_latched_supersession_is_answered_without_dialling() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let kp = fauna_core::identity::ActorKeypair::generate();
        let b = WsChallengeBearer::new(url, kp.actor_id().0, kp.signing_key().clone());

        // Unlatched, the mint dials and meets the closed port.
        let err = b.bearer().await.expect_err("nothing listens");
        assert!(matches!(err, ApiError::Transport(_)), "got {err:?}");

        let successor = [7u8; 32];
        b.superseded_latch()
            .observe(&RpcError::superseded(&successor));
        for _ in 0..3 {
            let err = b
                .bearer()
                .await
                .expect_err("a retired identity mints nothing");
            assert!(
                matches!(&err, ApiError::Status { message, .. } if message == RpcError::CODE_SUPERSEDED),
                "a latched supersession must be answered from the latch, not by a dial; got {err:?}"
            );
        }
        assert_eq!(
            b.superseded_latch().get().and_then(|e| e.superseded_by()),
            Some(successor)
        );
    }

    #[test]
    fn new_trims_trailing_slash() {
        let kp = fauna_core::identity::ActorKeypair::generate();
        let b = WsChallengeBearer::new(
            "https://nest.example.com/",
            kp.actor_id().0,
            kp.signing_key().clone(),
        );
        assert_eq!(*b.nest_url.read().unwrap(), "https://nest.example.com");
    }

    #[test]
    fn with_shared_url_tracks_cell_swap() {
        // An SRV-reconnect port swap on the shared cell (what
        // `AuthClient::try_srv_recover` does) is visible to the bearer's mint URL.
        let kp = fauna_core::identity::ActorKeypair::generate();
        let cell: SharedNestUrl =
            Arc::new(std::sync::RwLock::new("https://nest.example".to_string()));
        let b = WsChallengeBearer::with_shared_url(
            Arc::clone(&cell),
            kp.actor_id().0,
            kp.signing_key().clone(),
        );
        *cell.write().unwrap() = "https://nest.example:8443".to_string();
        assert_eq!(*b.nest_url.read().unwrap(), "https://nest.example:8443");
    }
}
