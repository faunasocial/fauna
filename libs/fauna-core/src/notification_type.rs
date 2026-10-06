//! The notification type — what a row of the unified notifications surface
//! is about (`docs/goal/behavior/notifications.md` § The notification type).
//!
//! One vocabulary for every carrier: the nest's producers and its
//! `notifications.notif_type` column, the `fauna.notifications.list` row
//! (`fauna_protocol::notifications::NotifItem`), the `fauna.notification` push
//! frame, the deep-link router, and the icon classification
//! ([`crate::notification_glyph::NotificationGlyph::of`]). It lives here, below
//! `fauna-protocol`, so the icon mapping can match on it exhaustively;
//! `fauna_protocol::notifications` re-exports it as the wire type.

use serde::{Deserialize, Serialize};

/// What a notification row is about.
///
/// **On the wire and at rest this is a string**, byte-identical to the plain
/// `notif_type` string it replaced — the enum is how a build reads it, not a
/// new encoding.
///
/// **Open, carrying** (`docs/goal/architecture/transport.md` § Rule 3 in full,
/// answer 1): the nest reads the type back out of its own table and re-emits it
/// on every list reply and push frame, so a type this build does not name — a
/// newer release's, read by an older nest after a downgrade, or by an older app
/// — is kept verbatim in [`Self::Other`] and re-encodes unchanged. A decode
/// never fails on an unknown type. Every `match` gives [`Self::Other`] the
/// restrictive reading: it opens no page and paints the neutral icon.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotifType {
    /// `like` — someone liked the reader's post.
    Like,
    /// `reply`.
    Reply,
    /// `repost`.
    Repost,
    /// `quote`.
    Quote,
    /// `mention`.
    Mention,
    /// `follow`.
    Follow,
    /// `other` — a bridged interaction the origin protocol names and Fauna
    /// has no closer type for (the Bluesky poller's catch-all reason). A
    /// *known* type; not to be confused with the carrying arm
    /// [`Self::Other`].
    Interaction,
    /// `message`.
    Message,
    /// `event_invite`.
    EventInvite,
    /// `group_invite`.
    GroupInvite,
    /// `knock` — a contact request waiting at the door.
    Knock,
    /// `security.notice` — the account's record of what a session did
    /// (`notifications.md` § Security notices).
    SecurityNotice,
    /// `mail.forward_queue_evicted`.
    MailForwardQueueEvicted,
    /// `abuse_report.received` — an admin's doorbell (`moderation.md` §
    /// User-initiated reporting).
    AbuseReportReceived,
    /// `abuse_report.resolved` — the reporter's outcome.
    AbuseReportResolved,
    /// `family.content_notice` — a guardian's content notice
    /// (`family-safety.md`).
    FamilyContentNotice,
    /// `family.contact_request`.
    FamilyContactRequest,
    /// `family.feed_source_request`.
    FamilyFeedSourceRequest,
    /// `family.feed_source_approved`.
    FamilyFeedSourceApproved,
    /// A type this build does not name, kept verbatim so a decode never fails
    /// and a re-encode never replaces it. Never equal to a named variant:
    /// [`NotifType::from`] is the only constructor a reader uses, and it
    /// returns the named variant for every string this build knows.
    Other(String),
}

impl NotifType {
    /// The wire (and stored) string of this type.
    pub fn as_wire(&self) -> &str {
        match self {
            Self::Like => "like",
            Self::Reply => "reply",
            Self::Repost => "repost",
            Self::Quote => "quote",
            Self::Mention => "mention",
            Self::Follow => "follow",
            Self::Interaction => "other",
            Self::Message => "message",
            Self::EventInvite => "event_invite",
            Self::GroupInvite => "group_invite",
            Self::Knock => "knock",
            Self::SecurityNotice => "security.notice",
            Self::MailForwardQueueEvicted => "mail.forward_queue_evicted",
            Self::AbuseReportReceived => "abuse_report.received",
            Self::AbuseReportResolved => "abuse_report.resolved",
            Self::FamilyContentNotice => "family.content_notice",
            Self::FamilyContactRequest => "family.contact_request",
            Self::FamilyFeedSourceRequest => "family.feed_source_request",
            Self::FamilyFeedSourceApproved => "family.feed_source_approved",
            Self::Other(s) => s,
        }
    }
}

impl From<&str> for NotifType {
    /// Project a wire or stored string. Total: an unknown string is carried,
    /// never refused.
    fn from(s: &str) -> Self {
        match s {
            "like" => Self::Like,
            "reply" => Self::Reply,
            "repost" => Self::Repost,
            "quote" => Self::Quote,
            "mention" => Self::Mention,
            "follow" => Self::Follow,
            "other" => Self::Interaction,
            "message" => Self::Message,
            "event_invite" => Self::EventInvite,
            "group_invite" => Self::GroupInvite,
            "knock" => Self::Knock,
            "security.notice" => Self::SecurityNotice,
            "mail.forward_queue_evicted" => Self::MailForwardQueueEvicted,
            "abuse_report.received" => Self::AbuseReportReceived,
            "abuse_report.resolved" => Self::AbuseReportResolved,
            "family.content_notice" => Self::FamilyContentNotice,
            "family.contact_request" => Self::FamilyContactRequest,
            "family.feed_source_request" => Self::FamilyFeedSourceRequest,
            "family.feed_source_approved" => Self::FamilyFeedSourceApproved,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<String> for NotifType {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl Default for NotifType {
    /// The empty type — what the plain-string field defaulted to. Exists for
    /// fixtures (`..Default::default()`); no producer mints it.
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl std::fmt::Display for NotifType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl Serialize for NotifType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for NotifType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self::from(String::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every named variant, so a variant added without its two string arms
    /// fails here rather than silently riding [`NotifType::Other`].
    const NAMED: [NotifType; 19] = [
        NotifType::Like,
        NotifType::Reply,
        NotifType::Repost,
        NotifType::Quote,
        NotifType::Mention,
        NotifType::Follow,
        NotifType::Interaction,
        NotifType::Message,
        NotifType::EventInvite,
        NotifType::GroupInvite,
        NotifType::Knock,
        NotifType::SecurityNotice,
        NotifType::MailForwardQueueEvicted,
        NotifType::AbuseReportReceived,
        NotifType::AbuseReportResolved,
        NotifType::FamilyContentNotice,
        NotifType::FamilyContactRequest,
        NotifType::FamilyFeedSourceRequest,
        NotifType::FamilyFeedSourceApproved,
    ];

    #[test]
    fn every_named_variant_round_trips_through_its_wire_string() {
        for named in NAMED {
            let back = NotifType::from(named.as_wire());
            assert_eq!(back, named, "{:?} must project back to itself", named);
            assert!(
                !matches!(back, NotifType::Other(_)),
                "{:?} must not ride the carrying arm",
                named
            );
        }
    }

    #[test]
    fn wire_strings_are_the_ones_the_plain_field_carried() {
        assert_eq!(NotifType::Like.as_wire(), "like");
        assert_eq!(NotifType::Interaction.as_wire(), "other");
        assert_eq!(NotifType::SecurityNotice.as_wire(), "security.notice");
        assert_eq!(
            NotifType::FamilyFeedSourceApproved.as_wire(),
            "family.feed_source_approved"
        );
    }

    #[test]
    fn encodes_as_the_bare_string_and_carries_an_unknown_type_unchanged() {
        // Same bytes as the `String` field it replaced.
        assert_eq!(
            serde_json::to_string(&NotifType::Mention).unwrap(),
            serde_json::to_string("mention").unwrap()
        );
        // A newer release's type decodes, is not mistaken for a named one,
        // and re-encodes exactly.
        let unknown: NotifType = serde_json::from_str("\"calendar.rsvp\"").unwrap();
        assert_eq!(unknown, NotifType::Other("calendar.rsvp".into()));
        assert_eq!(
            serde_json::to_string(&unknown).unwrap(),
            "\"calendar.rsvp\""
        );
    }

    #[test]
    fn default_is_the_empty_string_the_plain_field_defaulted_to() {
        assert_eq!(NotifType::default().as_wire(), "");
    }
}
