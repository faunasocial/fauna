//! UniFFI façades for the two per-user Nostr control planes that are *not*
//! part of the generic `fauna.bridges.*` surface:
//!
//! - **`fauna.nostr.bunker.*`** — the NIP-46 *Connected apps* roster
//!   (`docs/goal/ui/nostr.md` § The nest as the user's NIP-46 signer), via
//!   [`FfiNostrBunkerClient`].
//! - **`fauna.nostr.zap_signers.*`** — the NIP-57 zap trust root
//!   (`docs/goal/behavior/monetization.md` § Zap receipts — the trust model),
//!   via `FfiNostrZapSignerClient`. Behind the `zaps` registry feature
//!   (`dynamic-features.md` § Charter members): a Damus-flavor build keeps the
//!   bunker roster above and carries neither this face nor its kind strings.
//!
//! Each wraps the matching thin `fauna_client_nostr` client; the records below
//! are the FFI-visible shape of `fauna_protocol::nostr::{CreateBunkerInviteReply,
//! BunkerAppEntry, ZapSignerEntry}`. Linux and tui call those shared clients
//! directly (native Rust, no FFI hop); the web SPA reaches them through the
//! wasm twins (`libs/fauna-wasm/src/rpc.rs`). Mirrors `contacts_client.rs`
//! (priority #2).

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_nostr::NostrBunkerClient;
use fauna_client_nostr::nostr::{BunkerAppEntry, CreateBunkerInviteReply};
// The `zaps` registry feature's half (`dynamic-features.md` § Charter members —
// a subset member of `payments`).
#[cfg(feature = "zaps")]
use fauna_client_nostr::NostrZapSignerClient;
#[cfg(feature = "zaps")]
use fauna_client_nostr::nostr::ZapSignerEntry;

use crate::{FfiError, stringify};

/// UniFFI face of [`fauna_core::format::bunker_app_label`] — a roster row's
/// primary label (verbatim when set, else a status-keyed fallback). Shared so
/// the label ↔ status contract can't drift per-app (previously hand-rolled
/// identically in web/linux/tui); mirrors `bridges.rs::nostr_key_source_label`
/// (same Go-incompatibility gating rationale — a bare `fauna_core::LocalizedText`
/// crosses the boundary).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn bunker_app_label(label: String, status: String) -> fauna_core::localized::LocalizedText {
    fauna_core::format::bunker_app_label(&label, &status)
}

/// UniFFI face of [`fauna_core::format::bunker_last_used_label`] — a roster
/// row's last-used sub-label (`never_used` when absent, else the caller's
/// already-locally-formatted time as the `{time}` arg). Same gating as
/// [`bunker_app_label`] above.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn bunker_last_used_label(
    formatted_time: Option<String>,
) -> fauna_core::localized::LocalizedText {
    fauna_core::format::bunker_last_used_label(formatted_time.as_deref())
}

// ── CreateBunkerInviteReply mirror ─────────────────────────────────────

/// FFI mirror of [`fauna_protocol::nostr::CreateBunkerInviteReply`] — the
/// one-time reveal from minting a connect invite.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiCreateBunkerInviteReply {
    pub connection_id: i64,
    pub connect_string: String,
    pub signer_pubkey: String,
    pub expires_at: u64,
}

impl From<CreateBunkerInviteReply> for FfiCreateBunkerInviteReply {
    fn from(r: CreateBunkerInviteReply) -> Self {
        FfiCreateBunkerInviteReply {
            connection_id: r.connection_id,
            connect_string: r.connect_string,
            signer_pubkey: r.signer_pubkey,
            expires_at: r.expires_at,
        }
    }
}

// ── BunkerAppEntry mirror ──────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::nostr::BunkerAppEntry`] — one roster row
/// (pending or active).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiBunkerAppEntry {
    pub id: i64,
    pub app_pubkey: Option<String>,
    pub label: String,
    pub status: String,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub use_count: u64,
    pub expires_at: u64,
}

impl From<BunkerAppEntry> for FfiBunkerAppEntry {
    fn from(e: BunkerAppEntry) -> Self {
        FfiBunkerAppEntry {
            id: e.id,
            app_pubkey: e.app_pubkey,
            label: e.label,
            status: e.status,
            created_at: e.created_at,
            last_used_at: e.last_used_at,
            use_count: e.use_count,
            expires_at: e.expires_at,
        }
    }
}

// ── FfiNostrBunkerClient ────────────────────────────────────────────────

/// UniFFI handle for the `fauna.nostr.bunker.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::nostr_bunker`]; methods are exposed
/// to Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiNostrBunkerClient {
    nest: Arc<NestClient>,
}

impl FfiNostrBunkerClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> NostrBunkerClient<Arc<NestClient>> {
        NostrBunkerClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiNostrBunkerClient {
    /// `fauna.nostr.bunker.create_invite` — mint a pending connection; the
    /// reply is the single one-time reveal of the connect string.
    pub async fn create_invite(&self) -> Result<FfiCreateBunkerInviteReply, FfiError> {
        let reply = self.client().create_invite().await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.nostr.bunker.list` — the caller's connection roster (pending +
    /// active rows).
    pub async fn list(&self) -> Result<Vec<FfiBunkerAppEntry>, FfiError> {
        let apps = self.client().list().await.map_err(stringify)?;
        Ok(apps.into_iter().map(Into::into).collect())
    }

    /// `fauna.nostr.bunker.revoke` — immediate disconnect of one connection.
    /// `false` when no live caller-owned row matched.
    pub async fn revoke(&self, connection_id: i64) -> Result<bool, FfiError> {
        self.client().revoke(connection_id).await.map_err(stringify)
    }

    /// `fauna.nostr.bunker.set_label` — name a connection row. `false` when
    /// no live caller-owned row matched.
    pub async fn set_label(&self, connection_id: i64, label: String) -> Result<bool, FfiError> {
        self.client()
            .set_label(connection_id, label)
            .await
            .map_err(stringify)
    }
}

// ── ZapSignerEntry mirror ──────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::nostr::ZapSignerEntry`] — one designated
/// zap signer (`nostr-zap-signer-item`).
#[cfg(feature = "zaps")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiZapSignerEntry {
    pub id: i64,
    /// 64 lowercase hex — normalized nest-side on write, so this is the form
    /// to render and the form to pass back to
    /// [`FfiNostrZapSignerClient::remove`], whatever case the user pasted.
    pub signer_pubkey: String,
    pub label: String,
    pub created_at: u64,
}

#[cfg(feature = "zaps")]
impl From<ZapSignerEntry> for FfiZapSignerEntry {
    fn from(e: ZapSignerEntry) -> Self {
        FfiZapSignerEntry {
            id: e.id,
            signer_pubkey: e.signer_pubkey,
            label: e.label,
            created_at: e.created_at,
        }
    }
}

// ── FfiNostrZapSignerClient ─────────────────────────────────────────────

/// UniFFI handle for the `fauna.nostr.zap_signers.*` kinds — the NIP-57 trust
/// root. Construct via [`crate::nest_client::FfiNestClient::nostr_zap_signers`];
/// methods are exposed to Swift as `async throws` and Kotlin as `suspend fun`.
///
/// A kind-9735 zap receipt is signed by the payee's LNURL/wallet server and is
/// plain signed JSON anyone may mint, so its own signature proves nothing about
/// payment. This roster is what makes one believable: the payee designates
/// which signer pubkey(s) may speak for their money.
///
/// **An empty roster is the meaningful out-of-the-box default, not an
/// unconfigured state** — a payee who has designated nobody believes nobody, so
/// every zap stays inert. A UI rendering this list should say so rather than
/// presenting emptiness as an error or a load failure.
///
/// All three kinds are User-class and caller-scoped: the nest keys every query
/// on the authenticated connection's actor, never a request field.
///
/// `zaps`-gated. The `#[cfg]` sits on the WHOLE `#[uniffi::export] impl` block
/// below rather than on its methods: the macro does not honour a method-level
/// `cfg`, which is the W1 (account-data-plane.md § Workstreams) gotcha this retrofit inherits.
#[cfg(feature = "zaps")]
#[derive(uniffi::Object)]
pub struct FfiNostrZapSignerClient {
    nest: Arc<NestClient>,
}

#[cfg(feature = "zaps")]
impl FfiNostrZapSignerClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> NostrZapSignerClient<Arc<NestClient>> {
        NostrZapSignerClient::new(Arc::clone(&self.nest))
    }
}

#[cfg(feature = "zaps")]
#[fauna_uniffi_async::export]
impl FfiNostrZapSignerClient {
    /// `fauna.nostr.zap_signers.list` — the caller's designated signers,
    /// newest first. An empty vec means this payee believes no zap receipt
    /// at all (the ratified default, not an error).
    pub async fn list(&self) -> Result<Vec<FfiZapSignerEntry>, FfiError> {
        let signers = self.client().list().await.map_err(stringify)?;
        Ok(signers.into_iter().map(Into::into).collect())
    }

    /// `fauna.nostr.zap_signers.add` — designate a signer (64-hex pubkey,
    /// normalized lowercase nest-side) with an optional provider label.
    /// Idempotent: re-adding a designated signer refreshes its label. Returns
    /// the stored row, so render *that* pubkey rather than the input — a
    /// pubkey pasted in uppercase from a provider dashboard comes back
    /// lowercased, and only the stored form will ever match a real receipt.
    pub async fn add(
        &self,
        signer_pubkey: String,
        label: String,
    ) -> Result<FfiZapSignerEntry, FfiError> {
        let entry = self
            .client()
            .add(signer_pubkey, label)
            .await
            .map_err(stringify)?;
        Ok(entry.into())
    }

    /// `fauna.nostr.zap_signers.remove` — stop trusting a signer, keyed by the
    /// pubkey itself (no roster round-trip needed first). `false` when the
    /// caller had not designated it. Takes effect at the next receipt: the
    /// gate runs at ingest, so undesignating stops future belief and does not
    /// retract past acceptances.
    pub async fn remove(&self, signer_pubkey: String) -> Result<bool, FfiError> {
        self.client().remove(signer_pubkey).await.map_err(stringify)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_invite_reply_maps() {
        let proto = CreateBunkerInviteReply {
            connection_id: 7,
            connect_string: "bunker://abcd?relay=wss://x/nostr&secret=s".into(),
            signer_pubkey: "abcd".into(),
            expires_at: 1000,
            extra: Default::default(),
        };
        let ffi: FfiCreateBunkerInviteReply = proto.into();
        assert_eq!(ffi.connection_id, 7);
        assert_eq!(
            ffi.connect_string,
            "bunker://abcd?relay=wss://x/nostr&secret=s"
        );
        assert_eq!(ffi.signer_pubkey, "abcd");
        assert_eq!(ffi.expires_at, 1000);
    }

    #[test]
    fn bunker_app_entry_maps_pending() {
        let proto = BunkerAppEntry {
            id: 3,
            app_pubkey: None,
            label: String::new(),
            status: "pending".into(),
            created_at: 100,
            last_used_at: None,
            use_count: 0,
            expires_at: 200,
            extra: Default::default(),
        };
        let ffi: FfiBunkerAppEntry = proto.into();
        assert_eq!(ffi.id, 3);
        assert!(ffi.app_pubkey.is_none());
        assert_eq!(ffi.status, "pending");
        assert!(ffi.last_used_at.is_none());
    }

    #[test]
    fn bunker_app_entry_maps_active() {
        let proto = BunkerAppEntry {
            id: 4,
            app_pubkey: Some("beef".into()),
            label: "My phone".into(),
            status: "active".into(),
            created_at: 100,
            last_used_at: Some(150),
            use_count: 5,
            expires_at: 999,
            extra: Default::default(),
        };
        let ffi: FfiBunkerAppEntry = proto.into();
        assert_eq!(ffi.app_pubkey, Some("beef".into()));
        assert_eq!(ffi.label, "My phone");
        assert_eq!(ffi.use_count, 5);
        assert_eq!(ffi.last_used_at, Some(150));
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signer_entry_maps_every_field() {
        let proto = ZapSignerEntry {
            id: 11,
            signer_pubkey: "a".repeat(64),
            label: "Alby".into(),
            created_at: 1700,
            extra: Default::default(),
        };
        let ffi: FfiZapSignerEntry = proto.into();
        assert_eq!(ffi.id, 11);
        assert_eq!(ffi.signer_pubkey, "a".repeat(64));
        assert_eq!(ffi.label, "Alby");
        assert_eq!(ffi.created_at, 1700);
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signer_entry_carries_the_pubkey_verbatim_not_re_normalized() {
        // Normalization is the nest's job (`zap_signer_handlers.rs` lowercases
        // on write) and the reply already carries the stored form. If this
        // mirror ever "helpfully" re-cased the value it would mask a nest-side
        // normalization regression from every UniFFI app at once — the
        // roster would look right while believing nothing.
        let proto = ZapSignerEntry {
            id: 1,
            signer_pubkey: "AbCd".into(),
            label: String::new(),
            created_at: 0,
            extra: Default::default(),
        };
        let ffi: FfiZapSignerEntry = proto.into();
        assert_eq!(ffi.signer_pubkey, "AbCd");
    }

    #[test]
    #[cfg(feature = "zaps")]
    fn zap_signer_entry_maps_the_unlabeled_row() {
        // The empty label is a real, common state (a user may designate a bare
        // pubkey), distinct from any placeholder — the render-side fallback is
        // the app's, so the mirror must not substitute one here.
        let proto = ZapSignerEntry {
            id: 2,
            signer_pubkey: "b".repeat(64),
            label: String::new(),
            created_at: 5,
            extra: Default::default(),
        };
        let ffi: FfiZapSignerEntry = proto.into();
        assert_eq!(ffi.label, "");
    }
}
