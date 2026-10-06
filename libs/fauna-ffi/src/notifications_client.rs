//! UniFFI façade for the unified `fauna.notifications.*` WS-RPC kinds — the
//! list / mark-read / count plane hit from the notifications inbox + unread
//! badge.
//!
//! [`FfiNotificationsClient`] wraps
//! `fauna_client_notifications::NotificationsClient` (which in turn wraps the
//! shared `NestClient`); the records below are the FFI-visible shape of
//! `fauna_protocol::notifications::{NotifItem, NotifListReply}`. The
//! Rust-native Linux app calls the same `NotificationsClient` directly —
//! this seam gives Apple / Windows / Android the identical surface over
//! UniFFI (priority #2). Mirrors `email_client.rs`.

use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_notifications::notifications::{NotifItem, NotifListReply};
use fauna_client_notifications::{NotificationDestination, NotificationText, NotificationsClient};
use fauna_core::localized::LocalizedText;

use crate::{FfiError, stringify};

// ── NotifItem mirror ───────────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::notifications::NotifItem`] — one
/// notification across all protocols. `sender_id` / `content_id` are
/// hex-encoded `[u8; 32]` when present; `created_at` is micros since epoch.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiNotifItem {
    pub id: i64,
    /// `like` / `reply` / … — the wire string of the shared
    /// `fauna_protocol::notifications::NotifType`, carried as the string
    /// across UniFFI (a type a newer nest mints reaches the app unchanged).
    /// The decisions on it are shared: `notification_glyph_for_type` and
    /// `notification_destination_for`, never an app-side match.
    pub notif_type: String,
    /// Origin protocol (`fauna` / `bluesky` / `nostr` / `activitypub`).
    pub source: String,
    pub sender_id: Option<String>,
    pub content_id: Option<String>,
    pub subject_uri: Option<String>,
    /// The English rendering of the row — the compat fallback, never what an
    /// app paints on its own say-so. Paint [`notification_text_for`]'s answer.
    pub summary: String,
    pub is_read: bool,
    pub created_at: i64,
    /// The row's sentence as a catalog key plus data args
    /// (`behavior/notifications.md` § Localized body), or `None` (the row's summary arm answers, as for a key
    /// this build's catalog lacks). Carried so the row round-trips whole into the pure
    /// readers below; an app does not decide from it —
    /// [`notification_text_for`] does, because a key this build's catalog
    /// lacks must fall back to `summary`, never paint the raw key.
    ///
    /// Last and defaulted at the FFI boundary so the Swift, Kotlin and C# sites
    /// that build a row memberwise keep compiling (`version-compatibility.md`
    /// § I4 — the FFI binding boundary rule).
    #[uniffi(default = None)]
    pub body: Option<LocalizedText>,
}

impl From<NotifItem> for FfiNotifItem {
    fn from(n: NotifItem) -> Self {
        FfiNotifItem {
            id: n.id,
            notif_type: n.notif_type.as_wire().to_string(),
            source: n.source,
            sender_id: n.sender_id,
            content_id: n.content_id,
            subject_uri: n.subject_uri,
            summary: n.summary,
            is_read: n.is_read,
            created_at: n.created_at,
            body: n.body.map(|b| LocalizedText::key_args(b.key, b.args)),
        }
    }
}

// ── What the row says ──────────────────────────────────────────────────

/// FFI mirror of [`fauna_client_notifications::NotificationText`] — the text
/// a notification row paints (`behavior/notifications.md` § Localized body).
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiNotificationText {
    /// Resolve `text` through the app's own i18n pipeline (`getString` on
    /// android, the generated `L.lookup` flat table on apple, RESW on
    /// windows). Shared Rust never resolves it: the catalog an app resolves
    /// against is its own.
    Localized { text: LocalizedText },
    /// Paint `text` as-is: the nest's English `summary`.
    Verbatim { text: String },
}

impl From<NotificationText> for FfiNotificationText {
    fn from(t: NotificationText) -> Self {
        match t {
            NotificationText::Localized(text) => FfiNotificationText::Localized { text },
            NotificationText::Verbatim(text) => FfiNotificationText::Verbatim { text },
        }
    }
}

/// What this row says — the localized body, the English `summary`, or the
/// default — decided once in shared Rust.
///
/// A **pure** function over one row, the twin of
/// [`notification_destination_for`]: an app calls it while painting the list,
/// exactly as the Rust-native apps call
/// `fauna_client_notifications::notification_text`.
///
/// ⚠ An app must not paint `body` or `summary` on its own judgement. A body
/// whose key this build's catalog lacks (a newer nest's) must lose to
/// `summary` — resolving it would paint `notifications.some_future_key` at the
/// user — and "is the key known" is exactly the part an app cannot see.
#[uniffi::export]
pub fn notification_text_for(item: FfiNotifItem) -> FfiNotificationText {
    fauna_client_notifications::notification_text(&NotifItem::from(item)).into()
}

// ── Deep-link destination ──────────────────────────────────────────────

/// FFI mirror of [`fauna_client_notifications::NotificationDestination`] —
/// where a notification row navigates when the user opens it
/// (`behavior/notifications.md` § Deep-link destinations).
///
/// Apple / Windows / Android route this into the destination page's own
/// navigation, exactly as tui does with the Rust-native enum; nobody re-derives
/// the decision per app (priorities #1/#2, and the goal doc's § Don't do these
/// — "Don't deep-link via per-app routing tables").
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum FfiNotificationDestination {
    /// The liked post's detail view, by its 64-hex post id.
    Post { post_id: String },
    /// The pending-knock surface on the Contacts page, by the knocker's
    /// 64-hex actor id.
    Knock { sender_id: String },
    /// The Family page. Carries no id: every `family.*` row's `content_id` is
    /// a dedup token, and the page itself is the destination.
    Family,
    /// A page outside Fauna — the `bsky.app` page of the post a bridged
    /// Bluesky row is about — to be handed to the OS default browser through
    /// the app's ordinary external-link opener (apple `OpenURL.open`, windows
    /// `UrlOpener`, android `UrlOpener`), never rendered inside a Fauna page.
    /// The URL is built by shared Rust from a fixed origin and validated
    /// segments (`fauna_core::bluesky_web_url`), so the app opens it as-is.
    External { url: String },
}

impl From<NotificationDestination> for FfiNotificationDestination {
    fn from(d: NotificationDestination) -> Self {
        match d {
            NotificationDestination::Post { post_id } => {
                FfiNotificationDestination::Post { post_id }
            }
            NotificationDestination::Knock { sender_id } => {
                FfiNotificationDestination::Knock { sender_id }
            }
            NotificationDestination::Family => FfiNotificationDestination::Family,
            NotificationDestination::External { url } => {
                FfiNotificationDestination::External { url }
            }
        }
    }
}

/// Where this row goes when opened, or `None` for an honestly inert row.
///
/// A **pure** function over one row — no client, no round trip — so an app
/// calls it while painting the list, exactly as the Rust-native apps call
/// `fauna_client_notifications::notification_destination`.
///
/// ⚠ An app must not substitute its own `notif_type` match for this call. The
/// decision is keyed on `source` **then** `notif_type`, because a bridged row
/// reuses the native type vocabulary while its `content_id` is a dedup token —
/// matching the type alone deep-links every bridged like to a post that cannot
/// exist. That trap is the whole reason this is one shared function.
#[uniffi::export]
pub fn notification_destination_for(item: FfiNotifItem) -> Option<FfiNotificationDestination> {
    fauna_client_notifications::notification_destination(&NotifItem::from(item)).map(Into::into)
}

/// The mirror back to the wire type, for the pure readers above. Every field
/// a reader decides on survives the trip — the agreement tests below are what
/// would catch one stopping to. (Only `extra`, the wire's forward-compat
/// catch-all, is not on the mirror: UniFFI cannot express it, and no shared
/// decision reads it.)
impl From<FfiNotifItem> for NotifItem {
    fn from(n: FfiNotifItem) -> Self {
        NotifItem {
            id: n.id,
            notif_type: n.notif_type.into(),
            source: n.source,
            sender_id: n.sender_id,
            content_id: n.content_id,
            subject_uri: n.subject_uri,
            summary: n.summary,
            body: n.body.map(|b| {
                let mut wire = fauna_protocol::LocalizedText::new(b.key);
                wire.args = b.args.into_iter().collect();
                wire
            }),
            is_read: n.is_read,
            created_at: n.created_at,
            ..NotifItem::default()
        }
    }
}

// ── NotifListReply mirror ──────────────────────────────────────────────

/// FFI mirror of [`fauna_protocol::notifications::NotifListReply`] — a page
/// of notifications + the next-page `cursor` (the `id` of the last row,
/// `None` when the page is empty).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiNotifListReply {
    pub notifications: Vec<FfiNotifItem>,
    pub cursor: Option<i64>,
}

impl From<NotifListReply> for FfiNotifListReply {
    fn from(r: NotifListReply) -> Self {
        FfiNotifListReply {
            notifications: r.notifications.into_iter().map(Into::into).collect(),
            cursor: r.cursor,
        }
    }
}

// ── FfiNotificationsClient ─────────────────────────────────────────────

/// UniFFI handle for the `fauna.notifications.*` kinds. Construct via
/// [`crate::nest_client::FfiNestClient::notifications`]; methods are exposed
/// to Swift as `async throws` and Kotlin as `suspend fun`.
#[derive(uniffi::Object)]
pub struct FfiNotificationsClient {
    nest: Arc<NestClient>,
}

impl FfiNotificationsClient {
    pub(crate) fn from_nest(nest: Arc<NestClient>) -> Arc<Self> {
        Arc::new(Self { nest })
    }

    fn client(&self) -> NotificationsClient<Arc<NestClient>> {
        NotificationsClient::new(Arc::clone(&self.nest))
    }
}

#[fauna_uniffi_async::export]
impl FfiNotificationsClient {
    /// `fauna.notifications.list` — a page of the calling actor's unified
    /// notification history, newest first. `cursor` is the last-seen `id`
    /// (`None` ⇒ newest page); `limit` is the page size (`None` ⇒ the nest's
    /// default 25 + 1..=100 clamp).
    pub async fn list(
        &self,
        cursor: Option<i64>,
        limit: Option<i64>,
    ) -> Result<FfiNotifListReply, FfiError> {
        let reply = self
            .client()
            .notifications_list(cursor, limit)
            .await
            .map_err(stringify)?;
        Ok(reply.into())
    }

    /// `fauna.notifications.mark_read` — mark all notifications created at or
    /// before `up_to` (micros) as read (`None` ⇒ now). Returns the number of
    /// rows flipped.
    pub async fn mark_read(&self, up_to: Option<i64>) -> Result<i64, FfiError> {
        let reply = self
            .client()
            .notifications_mark_read(up_to)
            .await
            .map_err(stringify)?;
        Ok(reply.marked_read)
    }

    /// `fauna.notifications.count` — the calling actor's unread notification
    /// count (for the unread badge).
    pub async fn count(&self) -> Result<i64, FfiError> {
        let reply = self
            .client()
            .notifications_count()
            .await
            .map_err(stringify)?;
        Ok(reply.count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notif_item_maps_all_fields() {
        let proto = NotifItem {
            id: 7,
            notif_type: "like".into(),
            source: "fauna".into(),
            sender_id: Some("ab".repeat(32)),
            content_id: Some("cd".repeat(32)),
            subject_uri: None,
            summary: "alice liked your post".into(),
            body: None,
            is_read: false,
            created_at: 1000,
            extra: std::collections::BTreeMap::new(),
        };
        let ffi: FfiNotifItem = proto.into();
        assert_eq!(ffi.id, 7);
        assert_eq!(ffi.notif_type, "like");
        assert_eq!(ffi.source, "fauna");
        assert_eq!(ffi.sender_id, Some("ab".repeat(32)));
        assert_eq!(ffi.content_id, Some("cd".repeat(32)));
        assert!(ffi.subject_uri.is_none());
        assert_eq!(ffi.summary, "alice liked your post");
        assert!(!ffi.is_read);
        assert_eq!(ffi.created_at, 1000);
        assert_eq!(ffi.body, None);
    }

    /// `body` is the wire's `LocalizedText` on one side and `fauna_core`'s on
    /// the other; the key and every arg must cross both ways, or the pure
    /// readers decide on a row the nest never sent.
    #[test]
    fn the_body_crosses_the_boundary_both_ways() {
        let proto = NotifItem {
            summary: "alice wants to connect: hi".into(),
            body: Some(
                fauna_protocol::LocalizedText::new("notifications.row_knock")
                    .with_arg("sender", "alice")
                    .with_arg("message", "hi"),
            ),
            ..NotifItem::default()
        };
        let ffi = FfiNotifItem::from(proto.clone());
        let body = ffi.body.clone().expect("the body must reach the mirror");
        assert_eq!(body.key, "notifications.row_knock");
        assert_eq!(body.args.get("sender").map(String::as_str), Some("alice"));
        assert_eq!(body.args.get("message").map(String::as_str), Some("hi"));
        assert_eq!(NotifItem::from(ffi).body, proto.body);
    }

    /// The façade answers exactly what the shared decision answers, for every
    /// arm of the compat table (`behavior/notifications.md` § Localized body)
    /// — an agreement check, like the destination one above, so the two can
    /// never be updated apart.
    #[test]
    fn the_ffi_text_agrees_with_the_shared_decision_on_every_compat_arm() {
        let known = fauna_protocol::LocalizedText::new("notifications.row_like")
            .with_arg("sender", "alice");
        let unknown = fauna_protocol::LocalizedText::new("notifications.row_not_minted_yet")
            .with_arg("sender", "alice");
        let rows = [
            ("alice liked your post", Some(known.clone())),
            ("alice did a future thing", Some(unknown.clone())),
            ("alice liked your post", None),
            ("", None),
            ("", Some(unknown)),
        ];
        for (summary, body) in rows {
            let proto = NotifItem {
                notif_type: "like".into(),
                source: "fauna".into(),
                summary: summary.into(),
                body,
                ..NotifItem::default()
            };
            let shared =
                FfiNotificationText::from(fauna_client_notifications::notification_text(&proto));
            assert_eq!(
                notification_text_for(FfiNotifItem::from(proto.clone())),
                shared,
                "the FFI façade must mirror the shared decision for {proto:?}"
            );
        }
    }

    /// The two arms the non-Rust apps most need to get right, pinned by value:
    /// a known key localizes (the body wins over a DIFFERENT summary), and a
    /// key this build lacks paints the summary, never the raw key.
    #[test]
    fn a_known_key_localizes_and_an_unknown_one_paints_the_summary() {
        let row = |body: fauna_protocol::LocalizedText| FfiNotifItem {
            id: 1,
            notif_type: "like".into(),
            source: "fauna".into(),
            sender_id: None,
            content_id: None,
            subject_uri: None,
            summary: "the English fallback".into(),
            is_read: false,
            created_at: 1,
            body: Some(LocalizedText::key_args(body.key, body.args)),
        };
        let FfiNotificationText::Localized { text } = notification_text_for(row(
            fauna_protocol::LocalizedText::new("notifications.row_like")
                .with_arg("sender", "alice"),
        )) else {
            panic!("a catalog key must localize, not fall back to the summary");
        };
        assert_eq!(text.key, "notifications.row_like");
        assert_eq!(text.args.get("sender").map(String::as_str), Some("alice"));

        assert_eq!(
            notification_text_for(row(fauna_protocol::LocalizedText::new(
                "notifications.row_not_minted_yet"
            ))),
            FfiNotificationText::Verbatim {
                text: "the English fallback".into()
            }
        );
    }

    /// The façade must answer **exactly** what the shared router answers, for
    /// every row shape the nest actually produces — it is a mirror, not a
    /// second opinion. Written as an agreement check rather than a table of
    /// expected destinations so the two can never be updated apart: a new
    /// destination arm added to the router but forgotten here fails on the
    /// row it was added for.
    ///
    /// It also covers this module's `FfiNotifItem → NotifItem` round trip,
    /// which is the one place the façade could lose a field the router reads.
    #[test]
    fn the_ffi_destination_agrees_with_the_shared_router_on_every_producer() {
        let rows = [
            // (source, notif_type, sender_id, content_id) — the nest's real
            // inventory, native and bridged.
            (
                "fauna",
                "like",
                Some("bb".repeat(32)),
                Some("aa".repeat(32)),
            ),
            ("fauna", "knock", Some("cc".repeat(32)), None),
            (
                "fauna",
                "family.content_notice",
                Some("dd".repeat(32)),
                Some("ee".into()),
            ),
            (
                "fauna",
                "family.contact_request",
                Some("dd".repeat(32)),
                Some("ee".into()),
            ),
            (
                "fauna",
                "family.feed_source_request",
                Some("dd".repeat(32)),
                Some("ee".into()),
            ),
            (
                "fauna",
                "family.feed_source_approved",
                None,
                Some("ee".into()),
            ),
            ("fauna", "security.notice", None, Some("2a00".into())),
            ("fauna", "mail.forward_queue_evicted", None, None),
            ("bluesky", "like", None, Some("6174".into())),
            ("bluesky", "reply", None, Some("6174".into())),
            ("bluesky", "follow", None, Some("6174".into())),
            ("nostr", "like", None, Some("6174".into())),
            (
                "fauna",
                "some.future.type",
                Some("bb".repeat(32)),
                Some("aa".repeat(32)),
            ),
        ];
        for (source, notif_type, sender_id, content_id) in rows {
            // The AppView sends a subject for a like / reply / repost / quote
            // and none for a follow — the two bridged shapes the router
            // distinguishes, so both cross the seam here.
            let subject_uri = (source != "fauna" && notif_type != "follow")
                .then(|| "at://did:plc:xyz/app.bsky.feed.post/3kpost".to_string());
            let ffi = FfiNotifItem {
                id: 1,
                notif_type: notif_type.into(),
                source: source.into(),
                sender_id,
                content_id,
                subject_uri,
                summary: "something happened".into(),
                is_read: false,
                created_at: 1,
                body: None,
            };
            let shared =
                fauna_client_notifications::notification_destination(&NotifItem::from(ffi.clone()))
                    .map(FfiNotificationDestination::from);
            assert_eq!(
                notification_destination_for(ffi),
                shared,
                "the FFI façade must mirror the shared router for {source}/{notif_type}"
            );
        }
    }

    /// The trap, restated at the boundary the five non-Rust apps cross: a
    /// bridged like carries a `content_id`, and it is not a post id — the
    /// destination is the SUBJECT post, off-app on bsky.app, and it is the
    /// `subject_uri` field (which the apple mapping once dropped) that carries
    /// it across this boundary.
    #[test]
    fn the_ffi_facade_opens_a_bridged_like_off_app_from_its_subject() {
        let mut ffi = FfiNotifItem {
            id: 1,
            notif_type: "like".into(),
            source: "bluesky".into(),
            sender_id: None,
            content_id: Some("6174".into()),
            subject_uri: Some("at://did:plc:xyz/app.bsky.feed.post/3kpost".into()),
            summary: "alice liked your post".into(),
            is_read: false,
            created_at: 1,
            body: None,
        };
        assert_eq!(
            notification_destination_for(ffi.clone()),
            Some(FfiNotificationDestination::External {
                url: "https://bsky.app/profile/did:plc:xyz/post/3kpost".into()
            })
        );
        ffi.subject_uri = None;
        assert_eq!(
            notification_destination_for(ffi),
            None,
            "without a subject the content_id must not become a destination"
        );
    }

    #[test]
    fn list_reply_maps_and_preserves_cursor() {
        let proto = NotifListReply {
            notifications: vec![NotifItem {
                id: 3,
                notif_type: "mention".into(),
                source: "bluesky".into(),
                sender_id: None,
                content_id: None,
                subject_uri: Some("at://did:plc:xyz/app.bsky.feed.post/1".into()),
                summary: "you were mentioned".into(),
                body: None,
                is_read: true,
                created_at: 900,
                extra: std::collections::BTreeMap::new(),
            }],
            cursor: Some(3),
            extra: std::collections::BTreeMap::new(),
        };
        let ffi: FfiNotifListReply = proto.into();
        assert_eq!(ffi.notifications.len(), 1);
        assert_eq!(ffi.cursor, Some(3));
        assert_eq!(
            ffi.notifications[0].subject_uri.as_deref(),
            Some("at://did:plc:xyz/app.bsky.feed.post/1")
        );
    }

    #[test]
    fn list_reply_empty_has_no_cursor() {
        let proto = NotifListReply {
            notifications: vec![],
            cursor: None,
            extra: std::collections::BTreeMap::new(),
        };
        let ffi: FfiNotifListReply = proto.into();
        assert!(ffi.notifications.is_empty());
        assert!(ffi.cursor.is_none());
    }
}
