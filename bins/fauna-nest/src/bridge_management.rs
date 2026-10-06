//! Unified bridge management: trait + registry + shared error type.
//!
//! Wire types (`BridgeIdentity`, `BridgeSetting`, `BridgeSettingOption`,
//! `BridgeLinkField`, `BridgeLinkMode`, `BridgeFollow`, plus the
//! `LinkReply` returned by `link()`) come straight from
//! `fauna_protocol::bridges_ui` — providers compose them with `Value`
//! for the genuinely dynamic per-bridge fields and the handler returns
//! them as-is. The old `serde_json::Value`-shaped internal duplicates
//! retired with the HTTP twins in the T9+T10 sweep.

use async_trait::async_trait;

use fauna_protocol::Value;
pub use fauna_protocol::bridges_ui::{
    BridgeFollow, BridgeFollowRequest, BridgeIdentity, BridgeLinkField, BridgeLinkMode,
    BridgeSetting, BridgeSettingOption, LinkChallengeReply, LinkReply,
};

use crate::routes::AppState;

/// Deserialize a dynamic [`Value`] (DAG-CBOR `Ipld`) into a typed `T`.
///
/// Replaces the old `Value::deserialized()` inherent method, removed in the
/// CBOR rework — `fauna_protocol::Value` is now `ipld_core::Ipld`, which has no
/// serde bridge of its own. Round-trips through canonical CBOR bytes (the
/// cleanest serde path that exists today, mirroring how `fauna_client` decodes
/// a reply `Value`). The error is returned as a `String` so each provider can
/// wrap it in its own `BridgeError::invalid_params(...)` message.
pub fn deserialize_value<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, String> {
    let bytes = fauna_cbor::encode_canonical(value).map_err(|e| e.to_string())?;
    fauna_cbor::decode_strict(&bytes).map_err(|e| e.to_string())
}

// ── Internal trait-return shape ────────────────────────────────────
//
// What `BridgeProvider::status()` produces: the *dynamic* subset of the
// wire `BridgeStatus`. The handler combines this with the static
// `id()`/`name()`/`available()`/`supports_follows()` getters (plus the
// optional `error` slot when a provider's `status()` failed) to build
// the full `fauna_protocol::bridges_ui::BridgeStatus`. Keeping the
// dynamic subset separate avoids each provider having to repeat its
// own id/name/available — those are static metadata that lives next to
// the trait methods.

#[derive(Debug)]
pub struct BridgeStatus {
    pub linked: bool,
    pub identity: Option<BridgeIdentity>,
    pub mode: Option<String>,
    pub settings: Vec<BridgeSetting>,
    pub link_modes: Option<Vec<BridgeLinkMode>>,
}

/// Test-only fault injection for `provider.status()` — the two degraded
/// shapes `fauna_client_bridges::link_block` recognizes
/// (`bridge_status_test_hook.rs`, `AppState::bridge_status_override`).
#[cfg(feature = "test-hooks")]
#[derive(Debug, Clone)]
pub enum BridgeStatusOverride {
    /// `provider.status()` returns this `Err` — surfaces as
    /// `BridgeStatus.error` verbatim (the nest's own explanation).
    Error(String),
    /// `provider.status()` returns `Ok` with `linked: false, link_modes:
    /// None` and no error — declared, but nothing applies.
    NoApplicableModes,
}

// ── Error contract ────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BridgeError {
    pub error: String,
    pub code: String,
}

impl BridgeError {
    pub fn not_found(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: "not_found".to_string(),
        }
    }
    pub fn not_linked() -> Self {
        Self {
            error: "bridge not linked".to_string(),
            code: "not_linked".to_string(),
        }
    }
    pub fn already_linked() -> Self {
        Self {
            error: "bridge already linked".to_string(),
            code: "already_linked".to_string(),
        }
    }
    /// The identity a link names (e.g. a Nostr pubkey) is already linked to
    /// ANOTHER account on this nest. Distinct from `already_linked` (the
    /// caller's own bridge is linked).
    pub fn identity_in_use() -> Self {
        Self {
            error: "this identity is already linked to another account on this nest".to_string(),
            code: "identity_in_use".to_string(),
        }
    }
    pub fn invalid_mode(mode: &str) -> Self {
        Self {
            error: format!("unknown mode: {mode}"),
            code: "invalid_mode".to_string(),
        }
    }
    pub fn invalid_params(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: "invalid_params".to_string(),
        }
    }
    pub fn unavailable() -> Self {
        Self {
            error: "bridge not available on this nest".to_string(),
            code: "unavailable".to_string(),
        }
    }
    pub fn provider_error(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: "provider_error".to_string(),
        }
    }
    /// A link naming an identity held OUTSIDE the nest arrived without a
    /// valid proof that the caller holds it (`docs/goal/ui/nostr.md` § Errors
    /// & edge cases → *Proof of possession*): no `proof` at all (a non-conforming
    /// client — the typed, surfaced refusal, never a silent link), a proof
    /// over a stale or foreign challenge, a bad signature, or a remote signer
    /// that would not sign. `msg` says which.
    pub fn proof_required(msg: &str) -> Self {
        Self {
            error: msg.to_string(),
            code: "proof_required".to_string(),
        }
    }
}

// ── Trait ────────────────────────────────────────────────────────

#[async_trait]
pub trait BridgeProvider: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    async fn available(&self, state: &AppState) -> bool;
    /// Whether `link` may proceed for this `mode`. Default: identical to
    /// [`Self::available`] — a provider whose modes are uniformly gated (or
    /// ungated) needs no override.
    ///
    /// A provider whose `available()` becomes true only as a *consequence* of
    /// a successful link (e.g. Nostr's nsec-deposit gate, `available()` =
    /// "has anyone deposited a key yet") MUST override this for its
    /// deposit-creating mode(s) — gating that mode on `available()` is
    /// circular and permanently deadlocks the box's first deposit. Every other bridging act stays gated on `available()`
    /// as before — this is link-only, and only for the modes that bootstrap
    /// availability.
    async fn available_for_link(&self, state: &AppState, mode: &str) -> bool {
        let _ = mode;
        self.available(state).await
    }
    /// Why this provider is unavailable, in the nest's own words, or `None`
    /// when it is available (or has nothing more to say than "not configured").
    ///
    /// Surfaced as `BridgeStatus.error` on the `available: false` row, which
    /// `fauna_client_bridges::link_block` renders **verbatim** beside the
    /// disabled Link control in all 7 apps (`bridges.md` § A bridge that cannot
    /// be linked right now, rule 2). Without it an unavailable bridge falls
    /// back to the generic `bridges.no_link_method`, which tells a user nothing
    /// about a condition they could actually fix.
    ///
    /// Default `None` — a provider whose unavailability is simply "the feature
    /// is not built" adds nothing by saying so.
    async fn unavailable_reason(&self, state: &AppState) -> Option<String> {
        let _ = state;
        None
    }
    fn link_modes(&self) -> Vec<BridgeLinkMode>;
    fn supports_follows(&self) -> bool;
    async fn status(&self, state: &AppState, actor_id: &str) -> Result<BridgeStatus, BridgeError>;
    /// Start the link flow. `params` rides directly off the wire as a
    /// `Value`; each provider deserializes the per-mode shape it
    /// expects (typically a small map with handle / app_password /
    /// server_url / nsec / smtp_* fields) inside the impl.
    async fn link(
        &self,
        state: &AppState,
        actor_id: &str,
        mode: &str,
        params: Value,
    ) -> Result<LinkReply, BridgeError>;
    /// Mint a proof-of-possession challenge for `mode` — what an external
    /// signer must sign before [`Self::link`] in that mode writes a row
    /// (`fauna.bridges.link_challenge`). Default: no mode of this provider
    /// holds an identity outside the nest, so there is nothing to prove and
    /// the ask is refused as `invalid_mode`. A provider whose mode does
    /// (Nostr `nip07`) overrides this AND makes its `link` arm demand the
    /// signed challenge back.
    async fn link_challenge(
        &self,
        state: &AppState,
        actor_id: &str,
        mode: &str,
    ) -> Result<LinkChallengeReply, BridgeError> {
        let _ = (state, actor_id);
        Err(BridgeError::invalid_mode(&format!(
            "{mode} (this bridge issues no link challenge)"
        )))
    }
    async fn unlink(&self, state: &AppState, actor_id: &str) -> Result<(), BridgeError>;
    /// Overwrite the per-bridge settings blob. `settings` is the wire
    /// `Value` map straight from the kind handler; providers
    /// deserialize into a typed settings struct as needed.
    async fn update_settings(
        &self,
        state: &AppState,
        actor_id: &str,
        settings: Value,
    ) -> Result<(), BridgeError>;
    async fn list_follows(
        &self,
        state: &AppState,
        actor_id: &str,
    ) -> Result<Vec<BridgeFollow>, BridgeError>;
    async fn add_follow(
        &self,
        state: &AppState,
        actor_id: &str,
        id: &str,
        petname: Option<&str>,
        extra: Option<Value>,
    ) -> Result<(), BridgeError>;
    async fn remove_follow(
        &self,
        state: &AppState,
        actor_id: &str,
        follow_id: &str,
    ) -> Result<(), BridgeError>;
    /// Whether this bridge's network lets an account decide who follows it —
    /// `BridgeStatus.supports_follow_requests` on the wire (`bridges.md`
    /// § Follow requests). Default `false`: a provider that declares it
    /// overrides this and the two methods below; every other provider needs
    /// no code.
    fn supports_follow_requests(&self) -> bool {
        false
    }
    /// The follow requests waiting on `actor_id`'s account
    /// (`fauna.bridges.list_follow_requests`).
    async fn list_follow_requests(
        &self,
        state: &AppState,
        actor_id: &str,
    ) -> Result<Vec<BridgeFollowRequest>, BridgeError> {
        let _ = (state, actor_id);
        Err(BridgeError::invalid_params(
            "this bridge has no follow requests",
        ))
    }
    /// Approve or refuse the request `id` names
    /// (`fauna.bridges.resolve_follow_request`). Idempotent: a request already
    /// gone is `Ok`.
    async fn resolve_follow_request(
        &self,
        state: &AppState,
        actor_id: &str,
        id: &str,
        approve: bool,
    ) -> Result<(), BridgeError> {
        let _ = (state, actor_id, id, approve);
        Err(BridgeError::invalid_params(
            "this bridge has no follow requests",
        ))
    }
}

// ── Registry ────────────────────────────────────────────────────

pub struct BridgeProviderRegistry {
    providers: Vec<Box<dyn BridgeProvider>>,
}

impl Default for BridgeProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl BridgeProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn BridgeProvider>) {
        self.providers.push(provider);
    }

    pub fn get(&self, id: &str) -> Option<&dyn BridgeProvider> {
        self.providers
            .iter()
            .find(|p| p.id() == id)
            .map(|p| p.as_ref())
    }

    pub fn all(&self) -> &[Box<dyn BridgeProvider>] {
        &self.providers
    }
}
