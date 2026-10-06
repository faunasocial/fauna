//! The share-plane host's **shared half** — everything a
//! [`fauna_sync_engine::share_glue::SharePlaneHost`] answers that is one
//! identical expression over seams every native app already holds
//! (`docs/goal/behavior/p2p.md` § Cross-user shared-set transfer → *What a leg
//! still writes*).
//!
//! **Why a crate of its own.** The plane's driver lives in
//! `fauna-sync-engine::share_glue`, and a leg implements `SharePlaneHost` for
//! it. Of that trait's nine methods, six were byte-identical in `fauna-tui`
//! and `fauna-linux` by 2026-08-23 — the two bind doors and the two nest reads
//! were lifted into `fauna-sync-engine` then, and the three left over
//! (`membership`, `key_bindings`, `send_share_endpoints`) plus the durable
//! advertisement sink beside them stayed app-side on a **crate-graph**
//! constraint, not per-app-ness: they need `fauna-client-folders/mls` and
//! `fauna-conversations`, which `fauna-sync-engine` refuses on purpose so the
//! lean deployments (the bearer-only `fauna-sync-agent`, the
//! `--no-default-features` Go mail bridge) never pay for the conversations/MLS
//! graph. A shared home therefore has to sit ABOVE the driver with both edges
//! — exactly `fauna-client-custody`'s reason for existing (`ui/devices.md`
//! § Custody facet), so this is that shape again: a native-only leaf that only
//! share-plane hosts link, and that is itself the gate (see `Cargo.toml`).
//!
//! **What a leg still writes after this** is only what genuinely differs per
//! app: its `Seat` type and `seat_node`, the two UI nudges (`seat_bound`,
//! `state_changed`), and `bind_seat`'s device label + transport factory. The
//! five trait methods below, and the sink, are one-line delegations to a
//! [`ShareHostSeams`] the leg holds.
//!
//! Lifted out of `apps/fauna-tui` and `apps/fauna-linux` 2026-08-27 with no behaviour change; both were the only
//! callers and both consume it now.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_conversations::ConversationsSession;
use fauna_conversations::backend::ShareEndpointsSink;
use fauna_core::feature_gate::EffectivePolicy;
use fauna_core::folder_keys::FolderEngineKeys;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_peer_share::admission::SetMembership;
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use fauna_sync_engine::share_glue::accept_share_advertisement;

/// The seams every native share-plane host composes over — held by the leg's
/// `SharePlaneHost` struct, answered by the methods below.
///
/// `secret_hex` is the identity secret the leg already holds for its own bind
/// door; it is parsed on each `key_bindings` call rather than kept as a
/// keypair, exactly as both legs did, so this struct owns nothing a leg did
/// not already own.
pub struct ShareHostSeams {
    /// The authenticated nest connection (the key-bindings blob, the brake read,
    /// the transfer policy).
    pub nest: Arc<NestClient>,
    /// This identity's 64-hex secret.
    pub secret_hex: String,
    /// The conversations rail — its MLS engine is the roster the admission
    /// seam consults, and its channel is where advertisements are sent.
    pub conversations: Arc<ConversationsSession>,
    /// The account's folder-key custody (`fauna.state.folder-keys`) the pump's
    /// key bindings resolve from — the seat's `PlaneFolderKeys`.
    pub folder_keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
}

impl ShareHostSeams {
    /// `SharePlaneHost::membership` — the M2 roster consult over the
    /// conversations rail's engine ([`mls_membership`]).
    pub fn membership(&self) -> Arc<dyn SetMembership + Send + Sync> {
        mls_membership(&self.conversations)
    }

    /// `SharePlaneHost::key_bindings` — the engine-key bindings the pump serves
    /// under ([`engine_key_bindings`]).
    pub async fn key_bindings(&self) -> Result<Vec<FolderEngineKeys>, String> {
        engine_key_bindings(Arc::clone(&self.nest), &self.secret_hex, &*self.folder_keys).await
    }

    /// `SharePlaneHost::send_share_endpoints` — the advertisement onto the
    /// carrying channel, the rail's error rendered as the driver's `String`.
    pub async fn send_share_endpoints(
        &self,
        channel_hex: &str,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        self.conversations
            .send_share_endpoints(channel_hex, bytes)
            .await
            .map_err(|e| e.to_string())
    }

    /// `SharePlaneHost::live_capabilities` — the shared brake read
    /// (`fauna_sync_engine::share_glue::live_capabilities`), re-served here so
    /// a leg's every app-agnostic answer reads off one struct.
    pub async fn live_capabilities(&self) -> Result<Vec<String>, String> {
        fauna_sync_engine::share_glue::live_capabilities(Arc::clone(&self.nest)).await
    }

    /// `SharePlaneHost::transfer_policy` — the shared policy read
    /// (`fauna_sync_engine::share_glue::transfer_policy`), likewise.
    pub async fn transfer_policy(&self) -> Option<EffectivePolicy> {
        fauna_sync_engine::share_glue::transfer_policy(Arc::clone(&self.nest)).await
    }
}

/// The admission seam's roster: the conversations rail's own MLS engine,
/// wrapped as `fauna-client-folders`' `MlsSetMembership`. One expression, kept
/// as a function so a leg that holds the session without a [`ShareHostSeams`]
/// (the bind door's caller) reaches the same answer.
pub fn mls_membership(
    conversations: &ConversationsSession,
) -> Arc<dyn SetMembership + Send + Sync> {
    Arc::new(fauna_client_folders::MlsSetMembership(
        conversations.engine(),
    ))
}

/// The engine-key bindings the pump serves under: parse the secret, then resolve
/// every set's keys from the account's folder-key custody (the shared
/// producer, [`fauna_client_folders::resolve_engine_keys`]).
///
/// The parse comes FIRST, before any round trip — a malformed secret is a
/// `secret:` refusal that never dials, which is what the unit test below pins:
/// the driver reports these strings to the surface, and a transport error
/// standing in for a bad secret would send the user debugging the wrong thing.
pub async fn engine_key_bindings(
    nest: Arc<NestClient>,
    secret_hex: &str,
    folder_keys: &dyn fauna_client_folders::FolderKeyReader,
) -> Result<Vec<FolderEngineKeys>, String> {
    let keypair = ActorKeypair::from_secret_hex(secret_hex).map_err(|e| format!("secret: {e}"))?;
    fauna_client_folders::resolve_engine_keys(folder_keys, keypair.actor_id(), nest)
        .await
        .map_err(|e| format!("key bindings: {e}"))
}

/// The `fauna.state.share-endpoints` sink — the conversations rail's trait
/// over the shared decision (decode → bind the claim to the MLS-authenticated
/// sender → land the row; `false` on refusal OR write failure). The decision
/// is `accept_share_advertisement`'s; this is the two lines that call it.
pub struct DurableShareEndpointsSink {
    account: AccountStoreHandle,
}

impl DurableShareEndpointsSink {
    pub fn new(account: AccountStoreHandle) -> Self {
        Self { account }
    }
}

#[async_trait::async_trait]
impl ShareEndpointsSink for DurableShareEndpointsSink {
    async fn share_endpoints(&self, channel_hex: &str, sender: ActorId, bytes: &[u8]) -> bool {
        accept_share_advertisement(&self.account, channel_hex, sender, bytes).await
    }
}

/// Register the durable sink on the rail — the sixth sink, registered up
/// front: advertisements can arrive on the conversation rail before the first
/// pump pass, and the sink's durable door (the account handle) exists from the
/// account-store-ready edge on. Lost-before-now advertisements are the seam's
/// stated best-effort posture — the next publish (≤1h floor) re-delivers.
pub fn install_durable_sink(conversations: &ConversationsSession, account: AccountStoreHandle) {
    conversations.set_share_endpoints_sink(Arc::new(DurableShareEndpointsSink::new(account)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed secret is refused as a `secret:` error before any round
    /// trip. Port 1 is never listening, so had the parse not come first the
    /// error would be the transport's — which is the mutation this asserts
    /// against: the driver surfaces this string, and a user told the nest is
    /// unreachable when their secret is bad debugs the wrong thing.
    #[tokio::test]
    async fn a_malformed_secret_is_refused_before_any_round_trip() {
        let nest = NestClient::new("http://127.0.0.1:1".to_string(), ActorKeypair::generate());
        let custody = fauna_client_folders::MemoryFolderKeyStore::default();
        let err = engine_key_bindings(nest, "not-a-secret", &custody)
            .await
            .expect_err("a malformed secret cannot yield bindings");
        assert!(
            err.starts_with("secret:"),
            "the parse refuses first, and says which half failed: {err}"
        );
    }
}
