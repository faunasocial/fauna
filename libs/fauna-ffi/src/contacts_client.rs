//! UniFFI façade for the user-facing connection-management WS-RPC kinds —
//! knocks (`fauna.knocks.{list,accept,block,unblock,dismiss}`), the contact
//! roster (`fauna.contacts.{list,confirm}`), and the inbox-acceptance policy
//! (`fauna.inbox.mode.{get,set}`), hit from the knocks-inbox + contacts
//! list + inbox-settings affordances (`knocks.unblock` powers the profile
//! `profile-block-button` Block⇄Unblock toggle).
//!
//! [`FfiContactsClient`] wraps `fauna_client_contacts::ContactsClient` (which
//! in turn wraps the shared `NestClient`); the records below are the
//! FFI-visible shape of `fauna_protocol::contacts::{KnockItem, ContactItem}`.
//! The Rust-native Linux app calls the same `ContactsClient` directly —
//! this seam gives Apple / Windows / Android the identical surface over
//! UniFFI (priority #2). Mirrors `email_client.rs`.
//!
//! Note: *sending* a knock is deliberately absent — per
//! `docs/goal/architecture/app-guidelines.md` a knock is a `ContactRequest`
//! POSTed to the peer's federation inbox (gated by `InboxMode`), not a
//! local-nest WS-RPC kind. See the knock-send follow-up TODO.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_contacts::ContactsClient;
use fauna_client_contacts::contacts::{ContactItem, KnockItem};

use crate::{FfiError, stringify};

// ── KnockItem mirror ───────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::contacts::KnockItem`] — one pending
/// inbound knock. `sender` is the hex-encoded `[u8; 32]` sender actor id;
/// `created_at` is millis since epoch.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiKnockItem {
    pub id: i64,
    pub sender: String,
    pub sender_node: String,
    pub summary: String,
    pub created_at: i64,
}

impl From<KnockItem> for FfiKnockItem {
    fn from(k: KnockItem) -> Self {
        FfiKnockItem {
            id: k.id,
            sender: k.sender,
            sender_node: k.sender_node,
            summary: k.summary,
            created_at: k.created_at,
        }
    }
}

// ── ContactItem mirror ─────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::contacts::ContactItem`] — one contact
/// roster row. `peer_id` is the hex-encoded peer actor id; `status` is
/// `accepted` / `confirmed` / `blocked`; `accepted_at` (secs) is null until
/// the contact is accepted. `handle`/`domain` are the peer's public handle and
/// the nest's handle domain — `Some` for a local peer with a handle, `None` for
/// a federated peer (the nest holds no cached federated Profile). They feed the
/// shared roster-filter predicate (`fauna_core::format::contact_matches_filter`);
/// see contacts.md § State & data shape.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiContactItem {
    pub peer_id: String,
    pub status: String,
    pub accepted_at: Option<i64>,
    pub created_at: i64,
    pub handle: Option<String>,
    pub domain: Option<String>,
}

impl From<ContactItem> for FfiContactItem {
    fn from(c: ContactItem) -> Self {
        FfiContactItem {
            peer_id: c.peer_id,
            status: c.status,
            accepted_at: c.accepted_at,
            created_at: c.created_at,
            handle: c.handle,
            domain: c.domain,
        }
    }
}

// ── FfiContactsClient ──────────────────────────────────────────────────

/// UniFFI handle for the `fauna.{knocks,contacts,inbox.mode}.*` kinds.
/// Construct via [`crate::nest_client::FfiNestClient::contacts`]; methods are
/// exposed to Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiContactsClient {
    nest: Arc<NestClient>,
}

impl FfiContactsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> ContactsClient<Arc<NestClient>> {
        ContactsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiContactsClient {
    /// `fauna.knocks.list` — the calling actor's pending inbound knocks.
    pub async fn knocks_list(&self) -> Result<Vec<FfiKnockItem>, FfiError> {
        let reply = self.client().knocks_list().await.map_err(stringify)?;
        Ok(reply.knocks.into_iter().map(Into::into).collect())
    }

    /// `fauna.knocks.accept` — accept the knock from `peer_id` (hex).
    pub async fn knocks_accept(&self, peer_id: String) -> Result<(), FfiError> {
        self.client()
            .knocks_accept(peer_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.knocks.block` — block the knock sender `peer_id` (hex).
    pub async fn knocks_block(&self, peer_id: String) -> Result<(), FfiError> {
        self.client()
            .knocks_block(peer_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.knocks.unblock` — unblock `peer_id` (hex), clearing the
    /// `blocked` contact edge (returns the relationship to no-edge). The
    /// inverse of `knocks_block`; a no-op on a non-blocked edge.
    pub async fn knocks_unblock(&self, peer_id: String) -> Result<(), FfiError> {
        self.client()
            .knocks_unblock(peer_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.knocks.dismiss` — dismiss the knock from `peer_id` (hex).
    pub async fn knocks_dismiss(&self, peer_id: String) -> Result<(), FfiError> {
        self.client()
            .knocks_dismiss(peer_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.contacts.list` — the calling actor's full contact roster.
    pub async fn contacts_list(&self) -> Result<Vec<FfiContactItem>, FfiError> {
        let reply = self.client().contacts_list().await.map_err(stringify)?;
        Ok(reply.contacts.into_iter().map(Into::into).collect())
    }

    /// `fauna.contacts.confirm` — promote the `accepted` contact `peer_id`
    /// (hex) to `confirmed`.
    pub async fn contacts_confirm(&self, peer_id: String) -> Result<(), FfiError> {
        self.client()
            .contacts_confirm(peer_id)
            .await
            .map_err(stringify)?;
        Ok(())
    }

    /// `fauna.inbox.mode.get` — the calling actor's inbox-acceptance mode
    /// (`open` / `allow_knock` / `contacts_only` / `closed`).
    pub async fn inbox_mode_get(&self) -> Result<String, FfiError> {
        let reply = self.client().inbox_mode_get().await.map_err(stringify)?;
        Ok(reply.mode)
    }

    /// `fauna.inbox.mode.set` — set the calling actor's inbox-acceptance
    /// `mode`. An unrecognized mode yields an invalid-params error.
    pub async fn inbox_mode_set(&self, mode: String) -> Result<(), FfiError> {
        self.client()
            .inbox_mode_set(mode)
            .await
            .map_err(stringify)?;
        Ok(())
    }
}

// ── Inbox-mode selector catalog (`inbox-mode-selector`, `settings.md` § 7) ──
//
// Gated behind `value-format` for the same uniffi-bindgen-go reason as
// `admin.rs`'s `registration_mode_options` (a `#[uniffi::export]` returning a
// `fauna_core` `LocalizedText` makes uniffi-bindgen-go emit an uncompilable
// bare `import "fauna_core"` — see `family.rs`). The Go mail-bridge has no
// inbox-privacy surface. Deliberately independent of
// `fauna_protocol::contacts::INBOX_MODES` (the Rust-native tui/linux table,
// which carries already-resolved English text for direct paint, not i18n
// keys) — every other cross-app catalog door in this file returns
// `LocalizedText` keys for the client to resolve through its own pipeline,
// and this is the same shape, not a rework of the native-only table.

/// One `inbox-mode-selector` option — mirrors [`FfiRegistrationModeOption`]'s
/// shape (`admin.rs`). `value` is [`fauna_core::data::InboxMode::to_wire`],
/// the wire token `fauna.inbox.mode.set` accepts and the
/// `inbox-mode-<value>` test-id suffix; `label`/`desc` are i18n keys for the
/// client to resolve.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiInboxModeOption {
    pub value: String,
    pub label: fauna_core::localized::LocalizedText,
    pub desc: fauna_core::localized::LocalizedText,
}

/// i18n label/desc key pair for one [`fauna_core::data::InboxMode`] value
/// (`status.inbox_privacy.*` / `status.inbox_privacy.*_desc`).
#[cfg(feature = "value-format")]
fn inbox_mode_keys(mode: &fauna_core::data::InboxMode) -> (&'static str, &'static str) {
    match mode.effective() {
        fauna_core::data::InboxMode::Open => (
            "status.inbox_privacy.open",
            "status.inbox_privacy.open_desc",
        ),
        fauna_core::data::InboxMode::AllowKnock => (
            "status.inbox_privacy.allow_knock",
            "status.inbox_privacy.allow_knock_desc",
        ),
        fauna_core::data::InboxMode::ContactsOnly => (
            "status.inbox_privacy.contacts_only",
            "status.inbox_privacy.contacts_only_desc",
        ),
        fauna_core::data::InboxMode::Closed | fauna_core::data::InboxMode::Other(_) => (
            "status.inbox_privacy.closed",
            "status.inbox_privacy.closed_desc",
        ),
    }
}

/// The four `inbox-mode-selector` options, in the canonical order
/// (`settings.md` § Privacy sub-page item 7 — the same order
/// `fauna_protocol::contacts::INBOX_MODES` fixes for tui/linux). Lets the
/// Apple / Windows / Android forms drop their hand-rolled inbox-mode maps.
///
/// Deliberately carries **no default** — `settings.md`'s ratified shape is
/// that the selector shows the account's stored mode, never a guessed one
/// (a guessed value is a false statement about who can reach the user); a
/// caller with no fetched mode yet renders none of these selected.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn inbox_mode_options() -> Vec<FfiInboxModeOption> {
    [
        fauna_core::data::InboxMode::Open,
        fauna_core::data::InboxMode::AllowKnock,
        fauna_core::data::InboxMode::ContactsOnly,
        fauna_core::data::InboxMode::Closed,
    ]
    .into_iter()
    .map(|mode| {
        let (label_key, desc_key) = inbox_mode_keys(&mode);
        FfiInboxModeOption {
            value: mode
                .to_wire()
                .expect("every selector mode has a token")
                .to_string(),
            label: fauna_core::localized::LocalizedText::key(label_key),
            desc: fauna_core::localized::LocalizedText::key(desc_key),
        }
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knock_item_maps() {
        let proto = KnockItem {
            id: 7,
            sender: "ab".repeat(32),
            sender_node: "node-a".into(),
            summary: "alice wants to connect".into(),
            created_at: 1000,
            extra: std::collections::BTreeMap::new(),
        };
        let ffi: FfiKnockItem = proto.into();
        assert_eq!(ffi.id, 7);
        assert_eq!(ffi.sender, "ab".repeat(32));
        assert_eq!(ffi.sender_node, "node-a");
        assert_eq!(ffi.summary, "alice wants to connect");
        assert_eq!(ffi.created_at, 1000);
    }

    #[test]
    fn contact_item_maps_with_accepted_at() {
        let proto = ContactItem {
            peer_id: "11".repeat(32),
            status: "accepted".into(),
            accepted_at: Some(1234),
            created_at: 1200,
            handle: None,
            domain: None,
            extra: std::collections::BTreeMap::new(),
        };
        let ffi: FfiContactItem = proto.into();
        assert_eq!(ffi.peer_id, "11".repeat(32));
        assert_eq!(ffi.status, "accepted");
        assert_eq!(ffi.accepted_at, Some(1234));
        assert_eq!(ffi.created_at, 1200);
    }

    #[test]
    fn contact_item_preserves_none_accepted_at() {
        let proto = ContactItem {
            peer_id: "22".repeat(32),
            status: "blocked".into(),
            accepted_at: None,
            created_at: 1100,
            handle: None,
            domain: None,
            extra: std::collections::BTreeMap::new(),
        };
        let ffi: FfiContactItem = proto.into();
        assert!(ffi.accepted_at.is_none());
        assert_eq!(ffi.status, "blocked");
    }

    #[cfg(feature = "value-format")]
    #[test]
    fn inbox_mode_options_are_wire_values_in_the_ratified_order() {
        let opts = inbox_mode_options();
        let values: Vec<&str> = opts.iter().map(|o| o.value.as_str()).collect();
        assert_eq!(values, ["open", "allow_knock", "contacts_only", "closed"]);
        for o in &opts {
            assert!(o.label.key.starts_with("status.inbox_privacy."));
            assert!(o.desc.key.ends_with("_desc"));
        }
    }
}
