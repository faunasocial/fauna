//! In-memory bearer token store for actor authentication.

use std::collections::HashMap;

use fauna_core::identity::ActorId;
use tokio::sync::RwLock;

struct TokenEntry {
    actor_id: ActorId,
    token_id: String, // 8 random bytes as 16 hex chars, for display/revocation
    ip_address: Option<String>,
    created_at: u64,   // unix seconds
    expires_at: u64,   // unix seconds
    last_used_at: u64, // unix seconds, updated on validate()
    // The renewal device public key that minted this session over
    // `fauna.auth.device_handshake`, or None for a direct sign-in. What lets
    // `fauna.sync.devices.delete` revoke exactly the sessions its grant
    // minted, and `fauna.sessions.list` label them (sync-agent.md
    // § Credential model — device revocation).
    minted_by_device: Option<[u8; 32]>,
}

/// Public session info returned by list_sessions.
pub struct SessionInfo {
    pub token_id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub ip_address: Option<String>,
    pub last_used_at: u64,
    /// The renewal device key that minted this session, or None (direct auth).
    pub minted_by_device: Option<[u8; 32]>,
}

/// What the WS upgrade learns about a bearer it validates — everything the
/// connection keeps for its lifetime, read at the one moment the raw bearer
/// and its row are both in hand ([`TokenStore::validate_with_session`]).
pub struct ValidatedSession {
    pub actor_id: ActorId,
    /// **Which session** this bearer is — the id `fauna.sessions.revoke` names.
    pub token_id: String,
    /// The row's `minted_by_device` tag, verbatim: the renewal device key of a
    /// `fauna.auth.device_handshake` mint, the custodian's own actor id on a
    /// `custody_handshake` mint, `None` for a seed mint. The upgrade turns it
    /// into the connection's device binding through
    /// `ws::bound_device_key_for`, which is where the custody overload is
    /// excluded — never read this field as a device key directly.
    pub minted_by_device: Option<[u8; 32]>,
}

/// A freshly-minted token plus its short display id. Returned by
/// `insert_with_metadata` so callers (the auth handshake / verify mint) can
/// surface `token_id` to the client — the client names its own session when it
/// later calls `fauna.sessions.{revoke,revoke_all}`.
pub struct MintedToken {
    /// Opaque bearer token (`<actor_hex>.<random_hex>`).
    pub token: String,
    /// Short 16-hex display id (8 random bytes), for revocation.
    pub token_id: String,
}

pub struct TokenStore {
    tokens: RwLock<HashMap<String, TokenEntry>>,
}

impl Default for TokenStore {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

impl TokenStore {
    pub fn new() -> Self {
        Self {
            tokens: RwLock::new(HashMap::new()),
        }
    }

    /// Insert a new token for an actor. Returns the opaque token string.
    pub async fn insert(&self, actor_id: ActorId, ttl_secs: u64) -> String {
        self.insert_with_metadata(actor_id, ttl_secs, None, None)
            .await
            .token
    }

    /// Insert a new token with metadata. Returns the opaque token plus its
    /// short display id (`token_id`). Every mint site declares its origin:
    /// `minted_by_device` is the renewal device public key for a
    /// `fauna.auth.device_handshake` mint, `None` for direct auth — the tag
    /// device revocation and the sessions-list label both read.
    pub async fn insert_with_metadata(
        &self,
        actor_id: ActorId,
        ttl_secs: u64,
        ip_address: Option<String>,
        minted_by_device: Option<[u8; 32]>,
    ) -> MintedToken {
        let random_part = fauna_core::identity::random_hex(32);
        let token = format!("{}.{}", hex::encode(actor_id.0), random_part);

        let token_id = fauna_core::identity::random_hex(8);

        let now = now_secs();

        let entry = TokenEntry {
            actor_id,
            token_id: token_id.clone(),
            ip_address,
            created_at: now,
            expires_at: now + ttl_secs,
            last_used_at: now,
            minted_by_device,
        };

        let mut map = self.tokens.write().await;
        map.insert(token.clone(), entry);
        MintedToken { token, token_id }
    }

    /// Validate a token. Returns the ActorId if valid and not expired.
    /// Updates last_used_at on success.
    pub async fn validate(&self, token: &str) -> Option<ActorId> {
        self.validate_with_session(token).await.map(|s| s.actor_id)
    }

    /// [`Self::validate`], also returning the short `token_id` the entry is
    /// filed under — i.e. **which session** this bearer is — and the row's
    /// `minted_by_device` tag, i.e. **which device** minted it.
    ///
    /// A separate accessor rather than a widened `validate` return: `validate`
    /// has nine call sites that want only the actor, and the one caller that
    /// needs the session identity is the WS upgrade
    /// (`routes.rs::ws_handler`). It hands both to
    /// [`crate::ws::WsState::subscribe_with_session`]: the id is what lets a
    /// per-token revocation find the sockets that bearer opened — without it
    /// the connection knows its actor and nothing about which of that actor's
    /// sessions it belongs to, and `fauna.sessions.revoke` can only govern the
    /// session's *next* connection (`transport-connection.md` § Connection
    /// lifecycle → *Revocation teardown*); the device key is what lets
    /// `fauna.sync.devices.list` answer `online` for the device this session
    /// belongs to (`devices.md` § Listing Devices → *The binding*). Both are
    /// captured here and kept on the connection because the row is never
    /// re-read: a socket outlives its bearer's hour by design.
    pub async fn validate_with_session(&self, token: &str) -> Option<ValidatedSession> {
        let now = now_secs();

        // Fast path: read lock to check validity
        {
            let map = self.tokens.read().await;
            let entry = map.get(token)?;
            if entry.expires_at <= now {
                return None;
            }
        }

        // Upgrade to write lock to update last_used_at
        let mut map = self.tokens.write().await;
        let entry = map.get_mut(token)?;
        if entry.expires_at <= now {
            return None;
        }
        entry.last_used_at = now;
        Some(ValidatedSession {
            actor_id: entry.actor_id,
            token_id: entry.token_id.clone(),
            minted_by_device: entry.minted_by_device,
        })
    }

    /// Whether `actor_id` still holds the session filed under `token_id`.
    ///
    /// A pure **read**, never a revocation, which is why it is outside the
    /// teardown census in `tests/conformance_revocation_teardown.rs`. Its
    /// caller is the upgrade's registration re-read,
    /// [`crate::routes::AppState::register_upgraded_connection`]: by the time a
    /// connection registers the raw bearer is gone, so the session can only be
    /// named by its id — and the question asked there is "did a revoke delete
    /// this row while the upgrade was in flight?"
    ///
    /// **Expiry is deliberately not consulted.** That question is about
    /// revocation, not about whether the bearer would open a *new* connection:
    /// a socket outliving its token's hour is the design (the bearer is
    /// validated once, at the upgrade), so an expired row `gc` has not swept
    /// yet answers the same as a live one.
    pub async fn has_session(&self, actor_id: &ActorId, token_id: &str) -> bool {
        let held = self
            .tokens
            .read()
            .await
            .values()
            .any(|entry| entry.actor_id == *actor_id && entry.token_id == token_id);
        #[cfg(test)]
        read_race::after_answer(token_id).await;
        held
    }

    /// List all active (non-expired) sessions for an actor.
    pub async fn list_sessions(&self, actor_id: &ActorId) -> Vec<SessionInfo> {
        let now = now_secs();
        let map = self.tokens.read().await;
        map.values()
            .filter(|e| e.actor_id == *actor_id && e.expires_at > now)
            .map(|e| SessionInfo {
                token_id: e.token_id.clone(),
                created_at: e.created_at,
                expires_at: e.expires_at,
                ip_address: e.ip_address.clone(),
                last_used_at: e.last_used_at,
                minted_by_device: e.minted_by_device,
            })
            .collect()
    }

    /// Revoke a single session by its short token_id.
    ///
    /// **Token half only.** Every caller must also close the sockets that
    /// bearer already opened — the shared helper that does both by
    /// construction is [`crate::routes::AppState::revoke_session_authority`].
    /// The bearer is validated once, at the upgrade, so dropping the row
    /// governs only the session's *next* connection; the one it already holds
    /// keeps dispatching (`transport-connection.md` § Connection lifecycle →
    /// *Revocation teardown*). Pinned by the census in
    /// `tests/conformance_revocation_teardown.rs`.
    pub async fn revoke_by_token_id(&self, token_id: &str) {
        let mut map = self.tokens.write().await;
        map.retain(|_, entry| entry.token_id != token_id);
    }

    /// Discard a token that was minted but **never issued** — the mint
    /// function's own failure path, before the bearer left the nest.
    ///
    /// Byte-identical to [`Self::revoke_by_token_id`] and deliberately not the
    /// same method, because the two are different facts and one of them has a
    /// duty the other does not. Revoking a *session* must also close the
    /// sockets that session opened; un-minting a token nobody has ever seen
    /// cannot have any, so pairing it with a socket teardown would be a no-op
    /// call written only to satisfy a guard. Giving the un-mint its own name
    /// keeps the census in `conformance_revocation_teardown.rs` exact **by
    /// construction** rather than by an exemption list — the same discipline
    /// that census already applies to comments, which is that a guard
    /// satisfiable by text near a thing is a guard on the text.
    ///
    /// Sole caller: `auth_core`'s device-grant mint, whose revocation re-check
    /// runs *after* the insert (the ordering is what makes the sweep total)
    /// and drops the token it just made when the grant turns out to be revoked.
    pub async fn drop_unissued_token(&self, token_id: &str) {
        let mut map = self.tokens.write().await;
        map.retain(|_, entry| entry.token_id != token_id);
    }

    /// Revoke every session of `actor_id` that a given renewal device key
    /// minted over `fauna.auth.device_handshake` — the token half of device
    /// revocation (`fauna.sync.devices.delete`). Sessions the device did not
    /// mint (direct sign-ins, other devices) are untouched. Returns the
    /// number revoked.
    ///
    /// **Token half only** — see [`Self::revoke_by_token_id`]; the paired
    /// helper is [`crate::routes::AppState::revoke_device_authority`].
    pub async fn revoke_minted_by(&self, actor_id: &ActorId, device_key: &[u8; 32]) -> usize {
        let mut map = self.tokens.write().await;
        let before = map.len();
        map.retain(|_, entry| {
            entry.actor_id != *actor_id || entry.minted_by_device != Some(*device_key)
        });
        before - map.len()
    }

    /// Revoke all sessions for an actor except the one with the given short
    /// `token_id`. Returns the number of sessions revoked. Named by `token_id`
    /// because the bearer connection drops the raw token, so
    /// `fauna.sessions.revoke_all` names the session to keep by the id the
    /// client learned at mint. A `keep_token_id` that
    /// matches none of the actor's sessions simply revokes them all.
    ///
    /// **Token half only** — see [`Self::revoke_by_token_id`]; the paired
    /// helper is
    /// [`crate::routes::AppState::revoke_other_sessions_authority`].
    pub async fn revoke_all_except_token_id(
        &self,
        actor_id: &ActorId,
        keep_token_id: &str,
    ) -> usize {
        let mut map = self.tokens.write().await;
        let before = map.len();
        map.retain(|_token, entry| {
            // Keep entries for other actors, or this actor's kept token_id.
            entry.actor_id != *actor_id || entry.token_id == keep_token_id
        });
        before - map.len()
    }

    /// Remove all tokens for an actor (used when admin suspends/deletes user).
    pub async fn revoke_actor(&self, actor_id: &ActorId) {
        let mut map = self.tokens.write().await;
        map.retain(|_, entry| entry.actor_id != *actor_id);
    }

    /// Remove EVERY token — all sessions, all actors. The deployment-seed
    /// rotation's bearer eviction (`box-recovery.md` § Client acceptance →
    /// *Live-session convergence*): every bearer is a credential minted under
    /// the predecessor's authority, and this store deliberately survives the
    /// in-process serving-generation restart (so a serving-port change never
    /// signs everyone out) — which is exactly why the rotation must evict the
    /// class explicitly, in the same decision that retires the key. Returns
    /// the number removed.
    pub async fn clear(&self) -> usize {
        let mut map = self.tokens.write().await;
        let removed = map.len();
        map.clear();
        removed
    }

    /// Remove all expired tokens. Returns the number removed.
    pub async fn gc(&self) -> usize {
        let mut map = self.tokens.write().await;
        crate::ttl_gc::gc_before_cutoff(&mut map, |entry| entry.expires_at, now_secs())
    }
}

/// The test-only rendezvous [`TokenStore::has_session`] offers once it has its
/// answer — the store's lock already dropped — and before it returns it, so a
/// test can run a whole revoke between a registration re-read's *answer* and
/// its caller's next step. That is the interleaving the re-read's place AFTER
/// the subscribe exists for: read there, a "still held" answer means the
/// connection is already in `WsState.subs` and the revoke's sweep closes it;
/// read anywhere before the subscribe — hoisted to the top of
/// [`crate::routes::AppState::register_upgraded_connection`] or merely between
/// its upgrade park and the subscribe — and the answer is stale by the time
/// the connection registers, which the sweep has already passed. The park is
/// inside the read itself, so no edit to the caller can separate them.
///
/// Keyed by `token_id` and disarmed on first use, for
/// [`crate::routes::upgrade_race`]'s reason; every test mints its own random
/// session id. `#[cfg(test)]` alone, so no artifact and no integration test
/// can arm it (e2e-conventions.md § convention 15).
#[cfg(test)]
pub(crate) mod read_race {
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};
    use tokio::sync::Notify;

    #[derive(Default)]
    pub(crate) struct Barrier {
        /// Raised by the read once it has its answer.
        pub(crate) answered: Notify,
        /// Raised by the test once it is done after the answer.
        pub(crate) may_return: Notify,
    }

    static ARMED: LazyLock<Mutex<HashMap<String, Arc<Barrier>>>> = LazyLock::new(Default::default);

    /// Hold the next `has_session` read of `token_id` after it has answered.
    pub(crate) fn arm(token_id: &str) -> Arc<Barrier> {
        let barrier = Arc::new(Barrier::default());
        ARMED
            .lock()
            .unwrap()
            .insert(token_id.to_string(), Arc::clone(&barrier));
        barrier
    }

    pub(super) async fn after_answer(token_id: &str) {
        let armed = ARMED.lock().unwrap().remove(token_id);
        if let Some(barrier) = armed {
            barrier.answered.notify_one();
            barrier.may_return.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn token_contains_actor_id_prefix() {
        let store = TokenStore::new();
        let actor_id = ActorId([0xab; 32]);
        let token = store.insert(actor_id, 3600).await;
        let parts: Vec<&str> = token.splitn(2, '.').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 64);
        assert_eq!(parts[0], hex::encode(actor_id.0));
        assert_eq!(parts[1].len(), 64);
    }

    #[tokio::test]
    async fn validate_returns_actor_for_new_format() {
        let store = TokenStore::new();
        let actor_id = ActorId([0xcd; 32]);
        let token = store.insert(actor_id, 3600).await;
        assert_eq!(store.validate(&token).await, Some(actor_id));
    }

    #[tokio::test]
    async fn validate_rejects_expired() {
        let store = TokenStore::new();
        let actor_id = ActorId([0xef; 32]);
        let token = store.insert(actor_id, 0).await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(store.validate(&token).await, None);
    }

    #[tokio::test]
    async fn list_sessions_returns_active() {
        let store = TokenStore::new();
        let actor_id = ActorId([0x01; 32]);
        let _t1 = store
            .insert_with_metadata(actor_id, 3600, Some("1.2.3.4".to_string()), None)
            .await;
        let _t2 = store.insert_with_metadata(actor_id, 3600, None, None).await;
        let sessions = store.list_sessions(&actor_id).await;
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn revoke_all_except_token_id_keeps_named_session() {
        let store = TokenStore::new();
        let actor_id = ActorId([0x04; 32]);
        let _t1 = store.insert(actor_id, 3600).await;
        let _t2 = store.insert(actor_id, 3600).await;
        let _t3 = store.insert(actor_id, 3600).await;
        // Keep the first session by its token_id.
        let sessions = store.list_sessions(&actor_id).await;
        assert_eq!(sessions.len(), 3);
        let keep = sessions[0].token_id.clone();
        let revoked = store.revoke_all_except_token_id(&actor_id, &keep).await;
        assert_eq!(revoked, 2);
        let remaining = store.list_sessions(&actor_id).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].token_id, keep);
    }

    #[tokio::test]
    async fn revoke_all_except_unknown_token_id_revokes_all() {
        let store = TokenStore::new();
        let actor_id = ActorId([0x05; 32]);
        let _t1 = store.insert(actor_id, 3600).await;
        let _t2 = store.insert(actor_id, 3600).await;
        let revoked = store
            .revoke_all_except_token_id(&actor_id, "ffffffffffffffff")
            .await;
        assert_eq!(revoked, 2);
        assert_eq!(store.list_sessions(&actor_id).await.len(), 0);
    }

    #[tokio::test]
    async fn clear_removes_every_session_for_every_actor() {
        let store = TokenStore::new();
        let a = ActorId([0x06; 32]);
        let b = ActorId([0x07; 32]);
        let t_a = store.insert(a, 3600).await;
        let _ = store.insert(a, 3600).await;
        let t_b = store.insert(b, 3600).await;
        assert_eq!(store.clear().await, 3);
        assert_eq!(store.validate(&t_a).await, None);
        assert_eq!(store.validate(&t_b).await, None);
        assert!(store.list_sessions(&a).await.is_empty());
        assert!(store.list_sessions(&b).await.is_empty());
    }

    #[tokio::test]
    async fn revoke_by_token_id_works() {
        let store = TokenStore::new();
        let actor_id = ActorId([0x03; 32]);
        let _token = store.insert(actor_id, 3600).await;
        let sessions = store.list_sessions(&actor_id).await;
        assert_eq!(sessions.len(), 1);
        let tid = sessions[0].token_id.clone();
        store.revoke_by_token_id(&tid).await;
        assert_eq!(store.list_sessions(&actor_id).await.len(), 0);
    }

    /// `has_session` answers "has a revoke deleted this row?" — true while the
    /// row stands, false once either per-token revocation removed it, and never
    /// true for another actor's id.
    #[tokio::test]
    async fn has_session_tracks_the_row_through_both_per_token_revocations() {
        let store = TokenStore::new();
        let a = ActorId([0x0a; 32]);
        let b = ActorId([0x0b; 32]);
        let one = store.insert_with_metadata(a, 3600, None, None).await;
        let two = store.insert_with_metadata(a, 3600, None, None).await;
        let keep = store.insert_with_metadata(a, 3600, None, None).await;
        assert!(store.has_session(&a, &one.token_id).await);
        assert!(
            !store.has_session(&b, &one.token_id).await,
            "a session id answers only for the actor it was minted to"
        );
        assert!(!store.has_session(&a, "0000000000000000").await);

        store.revoke_by_token_id(&one.token_id).await;
        assert!(!store.has_session(&a, &one.token_id).await);
        assert!(store.has_session(&a, &two.token_id).await);

        store.revoke_all_except_token_id(&a, &keep.token_id).await;
        assert!(!store.has_session(&a, &two.token_id).await);
        assert!(store.has_session(&a, &keep.token_id).await);
    }

    /// Expiry is not revocation: an expired row `gc` has not swept yet still
    /// answers `true`. A socket outliving its bearer's hour is the design (the
    /// bearer is validated once, at the upgrade), so the registration re-read
    /// must not start closing connections at expiry.
    #[tokio::test]
    async fn has_session_does_not_consult_expiry() {
        let store = TokenStore::new();
        let actor_id = ActorId([0x0c; 32]);
        store.tokens.write().await.insert(
            "expired".to_string(),
            TokenEntry {
                actor_id,
                token_id: "expired-id".to_string(),
                ip_address: None,
                created_at: 0,
                expires_at: now_secs().saturating_sub(1),
                last_used_at: 0,
                minted_by_device: None,
            },
        );
        assert!(store.has_session(&actor_id, "expired-id").await);
    }

    #[tokio::test]
    async fn gc_drops_only_expired_tokens() {
        // No prior test called gc() directly — insert entries with a
        // controlled expires_at (rather than sleeping past a real TTL) so this
        // exercises the ttl_gc::gc_before_cutoff delegation without a
        // wall-clock wait.
        let store = TokenStore::new();
        let actor_id = ActorId([0x05; 32]);
        {
            let mut map = store.tokens.write().await;
            map.insert(
                "expired".to_string(),
                TokenEntry {
                    actor_id,
                    token_id: "expired".to_string(),
                    ip_address: None,
                    created_at: 0,
                    expires_at: now_secs().saturating_sub(1),
                    last_used_at: 0,
                    minted_by_device: None,
                },
            );
            map.insert(
                "live".to_string(),
                TokenEntry {
                    actor_id,
                    token_id: "live".to_string(),
                    ip_address: None,
                    created_at: 0,
                    expires_at: now_secs() + 3600,
                    last_used_at: 0,
                    minted_by_device: None,
                },
            );
        }
        assert_eq!(store.gc().await, 1);
        let map = store.tokens.read().await;
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("live"));
    }
}
