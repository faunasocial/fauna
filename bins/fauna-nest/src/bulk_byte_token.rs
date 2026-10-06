//! Short-TTL scoped tokens for the bulk-byte plane.
//!
//! The chunk/manifest HTTP byte routes (`/api/v1/chunks`, `/api/v1/manifests`)
//! require a client bearer a bridge — a service user — does not hold. On its authed
//! service-user WS connection a bridge calls `fauna.bridges.mint_bulk_byte_token`
//! (`webdav-server.md` § Bulk-byte plane); nest mints a token here that the byte
//! routes accept.
//!
//! Two purposes reach this store, and they differ **only** in the gate nest applies
//! before minting (see `bridge_blob_handlers::mint_bulk_byte_token_handler`):
//! WebDAV folder bytes (BridgeMda, gated on the set being served to the actor)
//! and a sealed mail body over the inline RPC budget (BridgeMta or BridgeMda, gated
//! on the actor being a mail recipient — `smtp-server.md` § Message size limits).
//!
//! **Why a store separate from [`crate::token_store::TokenStore`]:** a bulk token
//! conveys *transport authz only* for the byte routes; it must NEVER be usable as
//! a full session bearer (which `BearerAuth` grants on WS-RPC and every other
//! route). Keeping bulk tokens in a disjoint map guarantees `BearerAuth`
//! (`token_store` only) can never validate one — no privilege escalation.
//!
//! **What the scope means:** the scope is transport authz + a QUOTA/audit
//! attribution key, NOT a chunk-hash ACL. The chunk store is a single global
//! content-addressed store, so per-set byte partitioning is impossible without a
//! forbidden mirror index — confidentiality between sets is the M2 content key (the
//! MDA never holds an unserved set's key), and a mail body's confidentiality is its
//! recipient seal. This is exactly why the *purpose* is spent at the mint gate and
//! the byte routes never branch on it: both purposes yield a bearer of identical
//! power, so what matters is who nest was willing to mint one for.

use std::collections::HashMap;

use fauna_core::identity::ActorId;
use fauna_protocol::wrapped_blob::{BulkByteAccess, BulkByteMintPurpose};
use tokio::sync::RwLock;

/// The authorization a validated bulk-byte token carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkByteScope {
    pub actor_id: ActorId,
    /// The audit/attribution key — what this token was minted for:
    /// `folder:<row id>` for a set (never its name, which rests sealed —
    /// `path-sealing.md` § the set-name plane), `conv:<channel hex>` for a
    /// conversation, empty when the purpose names neither (mail, index
    /// segments, nest backup). Mint it with [`folder_attribution`].
    pub folder: String,
    pub access: BulkByteAccess,
    /// What the token was minted for. Carried for audit/attribution: the write
    /// routes do not branch on it (the chunk store is global and
    /// content-addressed — the token is "not a chunk-hash ACL"), so the purpose's
    /// force is spent at the *mint* gate, not at the route. One reader: the
    /// chunk route's relay arm, which takes a bulk token only of the
    /// foreign-folder pair and reads the cross-nest roster for it
    /// (`crate::auth::relay_reader`).
    pub purpose: BulkByteMintPurpose,
    /// Absolute expiry, Unix seconds.
    pub expires_at: u64,
}

struct TokenEntry {
    scope: BulkByteScope,
}

/// The [`BulkByteScope::folder`] attribution key of a set: its row id, never
/// its name.
pub fn folder_attribution(folder_id: i64) -> String {
    format!("folder:{folder_id}")
}

pub struct BulkByteTokenStore {
    tokens: RwLock<HashMap<String, TokenEntry>>,
}

impl Default for BulkByteTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    fauna_core::data::Timestamp::now_secs() as u64
}

impl BulkByteTokenStore {
    pub fn new() -> Self {
        Self {
            tokens: RwLock::new(HashMap::new()),
        }
    }

    /// Mint a scoped token valid for `ttl_secs`. Returns `(token, expires_at)`.
    pub async fn mint(
        &self,
        actor_id: ActorId,
        folder: String,
        access: BulkByteAccess,
        purpose: BulkByteMintPurpose,
        ttl_secs: u64,
    ) -> (String, u64) {
        let token = fauna_core::identity::random_hex(32);
        let expires_at = now_secs() + ttl_secs;

        let entry = TokenEntry {
            scope: BulkByteScope {
                actor_id,
                folder,
                access,
                purpose,
                expires_at,
            },
        };
        self.tokens.write().await.insert(token.clone(), entry);
        (token, expires_at)
    }

    /// Validate a token. Returns its scope iff present and not expired.
    pub async fn validate(&self, token: &str) -> Option<BulkByteScope> {
        let now = now_secs();
        let map = self.tokens.read().await;
        let entry = map.get(token)?;
        if entry.scope.expires_at <= now {
            return None;
        }
        Some(entry.scope.clone())
    }

    /// Remove all expired tokens. Returns the number removed.
    pub async fn gc(&self) -> usize {
        let now = now_secs();
        let mut map = self.tokens.write().await;
        let before = map.len();
        map.retain(|_, entry| entry.scope.expires_at > now);
        before - map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mint_then_validate_returns_scope() {
        let store = BulkByteTokenStore::new();
        let actor = ActorId([0x11; 32]);
        let (token, expires_at) = store
            .mint(
                actor,
                "photos".into(),
                BulkByteAccess::Write,
                BulkByteMintPurpose::Folder,
                300,
            )
            .await;
        let scope = store.validate(&token).await.expect("valid token");
        assert_eq!(scope.actor_id, actor);
        assert_eq!(scope.folder, "photos");
        assert_eq!(scope.access, BulkByteAccess::Write);
        assert_eq!(scope.expires_at, expires_at);
    }

    #[tokio::test]
    async fn validate_rejects_unknown_token() {
        let store = BulkByteTokenStore::new();
        assert!(store.validate("deadbeef").await.is_none());
    }

    #[tokio::test]
    async fn validate_rejects_expired() {
        let store = BulkByteTokenStore::new();
        let actor = ActorId([0x22; 32]);
        let (token, _) = store
            .mint(
                actor,
                "docs".into(),
                BulkByteAccess::Read,
                BulkByteMintPurpose::Folder,
                0,
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert!(store.validate(&token).await.is_none());
    }

    #[tokio::test]
    async fn distinct_mints_are_distinct_tokens() {
        let store = BulkByteTokenStore::new();
        let actor = ActorId([0x33; 32]);
        let (t1, _) = store
            .mint(
                actor,
                "a".into(),
                BulkByteAccess::Read,
                BulkByteMintPurpose::Folder,
                300,
            )
            .await;
        let (t2, _) = store
            .mint(
                actor,
                "a".into(),
                BulkByteAccess::Read,
                BulkByteMintPurpose::Folder,
                300,
            )
            .await;
        assert_ne!(t1, t2);
    }

    #[tokio::test]
    async fn gc_drops_only_expired() {
        let store = BulkByteTokenStore::new();
        let actor = ActorId([0x44; 32]);
        let (_live, _) = store
            .mint(
                actor,
                "live".into(),
                BulkByteAccess::Write,
                BulkByteMintPurpose::Folder,
                300,
            )
            .await;
        let (_dead, _) = store
            .mint(
                actor,
                "dead".into(),
                BulkByteAccess::Write,
                BulkByteMintPurpose::Folder,
                0,
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        assert_eq!(store.gc().await, 1);
    }
}
