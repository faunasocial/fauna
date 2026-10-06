//! UniFFI façade for the fauna-native inbox delivery-queue WS-RPC kinds —
//! the caller-scoped drain `fauna.inbox.fetch` + `fauna.inbox.ack` (the
//! per-actor store-and-forward queue: contact-requests, knocks, group
//! invites/messages, MLS Welcomes, security notices, cross-nest DMs) plus the
//! producing leg `fauna.inbox.send` (the client→home-nest delivery of a signed
//! `(ContactRequest, Post)` tuple), the seam Apple / Windows / Android adopt
//! to retire their direct `POST /api/v1/inbox/{actor}`.
//!
//! [`FfiInboxClient`] wraps `fauna_client_inbox::InboxClient` (which in turn
//! wraps the shared `NestClient`); the records below are the FFI-visible shape
//! of `fauna_protocol::inbox::{InboxFetchReply, InboxItem}`. The Rust-native
//! Linux app calls the same `InboxClient` directly — this seam gives Apple
//! / Windows / Android the identical surface over UniFFI (priority #2).
//! Mirrors `contacts_client.rs`.
//!
//! This is the faithful transport migration of the HTTP `GET
//! /api/v1/inbox/{actor_id}` drain (`api-layers.md` § Inbox & Messaging). The
//! split into `fetch` (peek) + `ack` (consume) fixes the HTTP twin's
//! data-loss bug, which marked items delivered on *read*. Consumers run the
//! drain loop: `fetch(limit)` → durably apply the returned items → `ack`
//! their ids → repeat while `reply.more`.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_inbox::InboxClient;
use fauna_client_inbox::inbox::{InboxFetchReply, InboxItem};

use crate::{FfiError, stringify};

// ── InboxItem mirror ───────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::inbox::InboxItem`] — one undelivered inbox
/// item. `id` is the server-assigned delivery-link id the client returns in
/// [`FfiInboxClient::ack`] once it has durably applied the item; `payload` is
/// the opaque delivered bytes (a canonical fauna-native social/federation
/// payload the client decodes — the nest holds no opening key in encrypted
/// mode).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiInboxItem {
    pub id: i64,
    pub payload: Vec<u8>,
}

impl From<InboxItem> for FfiInboxItem {
    fn from(i: InboxItem) -> Self {
        FfiInboxItem {
            id: i.id,
            payload: i.payload,
        }
    }
}

// ── InboxFetchReply mirror ─────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::inbox::InboxFetchReply`] — one page of
/// undelivered items plus the `more` flag.
///
/// `more` reports that the nest still holds undelivered rows. It is **not** a
/// loop condition for this face, which is cursor-less
/// ([`FfiInboxClient::fetch`] carries the full rule). This doc used to say
/// *"when `more` is true, `ack` the applied ids and re-call fetch"*; that
/// advice is withdrawn with the same finding that withdrew it on
/// the method. Ack-and-re-fetch advances only while every row on the page is
/// ackable, and the rows that block a peek are exactly the ones that never
/// are — unknown kinds a newer nest introduced (left un-acked *by design* for
/// additive-everywhere forward-compat), undecodable rows, and un-answered
/// stranger knocks. Once a page of those sits at the head the loop re-reads it
/// forever. A consumer that must step past residue uses the shared walkers
/// (`fauna_client_inbox::{drain, list_folder_pending_shares}`), never a
/// re-fetch loop here.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiInboxFetchReply {
    pub items: Vec<FfiInboxItem>,
    pub more: bool,
}

impl From<InboxFetchReply> for FfiInboxFetchReply {
    fn from(r: InboxFetchReply) -> Self {
        FfiInboxFetchReply {
            items: r.items.into_iter().map(Into::into).collect(),
            more: r.more,
        }
    }
}

// ── FfiInboxClient ─────────────────────────────────────────────────────

/// UniFFI handle for the `fauna.inbox.{fetch,ack}` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::inbox`]; methods are exposed to Swift
/// as `async throws` and Kotlin as `suspend fun`. Caller-scoped by
/// construction (no `actor_id` param — the connection knows its caller).
#[derive(uniffi::Object)]
pub struct FfiInboxClient {
    nest: Arc<NestClient>,
}

impl FfiInboxClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> InboxClient<Arc<NestClient>> {
        InboxClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiInboxClient {
    /// `fauna.inbox.fetch` — peek the caller's undelivered inbox items without
    /// changing their status. `limit == 0` selects the handler default.
    ///
    /// ⚠ **This face is cursor-less, so it cannot page.** It used to tell
    /// callers to *"page until `reply.more` is false"*, which this signature
    /// gives them no way to do — there is no `after_id` here, so every call
    /// returns the same oldest page. That advice is withdrawn rather than
    /// implemented: no consumer needs a raw paging face, because the surfaces
    /// that must step past un-acked residue do it in shared Rust, where the
    /// walk and its termination guards are written once
    /// (`fauna_client_inbox::{drain, list_folder_pending_shares}` — both share
    /// `inbox_page_step`). Reach for those, not for a loop over this. If a
    /// consumer ever genuinely needs the cursor at the FFI boundary, add
    /// `after_id` here as an additive parameter and regenerate the bindings —
    /// do not rebuild the walk app-side (a second, subtly different
    /// walk is exactly the defect that finding was about).
    pub async fn fetch(&self, limit: u32) -> Result<FfiInboxFetchReply, FfiError> {
        let reply = self.client().fetch(limit).await.map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.inbox.ack` — mark the `ids` the caller has durably applied as
    /// delivered, so a re-fetch no longer returns them. Idempotent
    /// (already-delivered / not-owned ids are no-ops); returns the count of
    /// rows newly flipped.
    pub async fn ack(&self, ids: Vec<i64>) -> Result<u32, FfiError> {
        let reply = self.client().ack(ids).await.map_err(stringify)?;
        Ok(reply.acked)
    }

    /// `fauna.inbox.send` — hand the caller's home nest the canonical signed
    /// `(ContactRequest, Post)` tuple (`payload_bytes`) for delivery. The home
    /// nest verifies the caller is the payload's sender, then **local-delivers**
    /// when `recipient_nest_url` is `nil` (recipient on the caller's own home
    /// nest), or originates the cross-nest federation leg to that peer when set.
    /// Returns the created inbox row id, or `nil` when a knock was stored
    /// (`allow_knock` mode). The WS-RPC successor of the
    /// `POST /api/v1/inbox/{actor}` twin; the native Linux app calls the same
    /// `InboxClient::send` directly (priority #2).
    pub async fn send(
        &self,
        recipient_actor_id: String,
        recipient_nest_url: Option<String>,
        payload_bytes: Vec<u8>,
    ) -> Result<Option<i64>, FfiError> {
        let reply = self
            .client()
            .send(recipient_actor_id, recipient_nest_url, payload_bytes)
            .await
            .map_err(stringify)?;
        Ok(reply.inbox_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_item_maps() {
        let proto = InboxItem {
            id: 42,
            payload: vec![1, 2, 3, 4],
            extra: Default::default(),
        };
        let ffi: FfiInboxItem = proto.into();
        assert_eq!(ffi.id, 42);
        assert_eq!(ffi.payload, vec![1, 2, 3, 4]);
    }

    #[test]
    fn fetch_reply_maps_items_and_more() {
        let proto = InboxFetchReply {
            items: vec![
                InboxItem {
                    id: 1,
                    payload: vec![0xaa],
                    extra: Default::default(),
                },
                InboxItem {
                    id: 2,
                    payload: vec![0xbb, 0xcc],
                    extra: Default::default(),
                },
            ],
            more: true,
            extra: Default::default(),
        };
        let ffi: FfiInboxFetchReply = proto.into();
        assert!(ffi.more);
        assert_eq!(ffi.items.len(), 2);
        assert_eq!(ffi.items[0].id, 1);
        assert_eq!(ffi.items[1].payload, vec![0xbb, 0xcc]);
    }

    #[test]
    fn fetch_reply_empty_is_not_more() {
        let proto = InboxFetchReply::default();
        let ffi: FfiInboxFetchReply = proto.into();
        assert!(!ffi.more);
        assert!(ffi.items.is_empty());
    }
}
