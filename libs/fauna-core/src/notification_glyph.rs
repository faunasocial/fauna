//! The Notifications page's per-row icon — the `notif_type` wire string →
//! glyph mapping `docs/goal/behavior/notifications.md` § Where logic lives
//! declares as the shared-Rust target ("shared Rust returns the type enum;
//! app glue picks the icon"). Lifted out of linux and web, which each
//! hand-wrote the identical 8-case emoji match (linux's own doc comment
//! flagged the gap, citing the `SourceGlyph` pattern this follows).
//!
//! tui and android map a different `notif_type` vocabulary onto their own
//! native asset family (terminal glyphs / Material icons) and are
//! deliberately left alone here — only linux and web were doing the exact
//! same emoji-per-string match.
//!
//! The classification itself is typed: [`NotificationGlyph::of`] matches the
//! shared [`NotifType`] exhaustively, and the string entry points are its
//! projection for the callers whose rows still carry the wire string.

use crate::notification_type::NotifType;

/// The semantic category of a notification type ([`NotifType`]).
/// `Unknown` covers every type with no icon of its own, and every type this
/// build does not name.
///
/// Exported as a `uniffi::Enum` for the same reason [`crate::source_glyph::SourceGlyph`]
/// is: `notifications.md` § Where logic lives makes shared Rust the owner of the
/// *categorisation* and leaves the icon itself to app glue, because icons are
/// platform-native assets. A native app switches on this enum directly and picks
/// its own symbol (apple: SF Symbols; android: Material icons); linux and web take
/// the [`Self::emoji`] their shared asset family wants.
///
/// ⚠ Variants are APPENDED, never inserted: UniFFI numbers them in declaration
/// order, so reordering renumbers every existing discriminant — the same
/// constraint `SourceGlyph` records for its own trailing `Archive`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum NotificationGlyph {
    Message,
    Mention,
    Follow,
    EventInvite,
    GroupInvite,
    Knock,
    Reply,
    Like,
    Unknown,
    /// An abuse report: an admin's doorbell (`abuse_report.received`) or the
    /// reporter's outcome (`abuse_report.resolved`) — `moderation.md` § App
    /// surface. Appended after `Unknown` per the rule above.
    Report,
}

impl NotificationGlyph {
    /// The icon category of a notification type. Exhaustive on purpose: a new
    /// [`NotifType`] variant does not compile until it is given a category
    /// here, where a string match would have let it fall to the bell unseen.
    pub fn of(notif_type: &NotifType) -> Self {
        match notif_type {
            NotifType::Message => Self::Message,
            NotifType::Mention => Self::Mention,
            NotifType::Follow => Self::Follow,
            NotifType::EventInvite => Self::EventInvite,
            NotifType::GroupInvite => Self::GroupInvite,
            NotifType::Knock => Self::Knock,
            NotifType::Reply => Self::Reply,
            NotifType::Like => Self::Like,
            NotifType::AbuseReportReceived | NotifType::AbuseReportResolved => Self::Report,
            // No icon of their own (yet): the neutral bell.
            NotifType::Repost
            | NotifType::Quote
            | NotifType::Interaction
            | NotifType::SecurityNotice
            | NotifType::MailForwardQueueEvicted
            | NotifType::FamilyContentNotice
            | NotifType::FamilyContactRequest
            | NotifType::FamilyFeedSourceRequest
            | NotifType::FamilyFeedSourceApproved => Self::Unknown,
            // A type this build does not name renders neutral.
            NotifType::Other(_) => Self::Unknown,
        }
    }

    /// [`Self::of`] for a caller that holds the wire string — the UniFFI and
    /// wasm faces, whose rows carry the type as its string.
    pub fn from_notif_type(notif_type: &str) -> Self {
        Self::of(&NotifType::from(notif_type))
    }

    /// The emoji linux and web both render for this category — their shared
    /// icon asset family (unlike apple/android's native symbol sets).
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Message => "💬",
            Self::Mention => "@",
            Self::Follow => "👤",
            Self::EventInvite => "📅",
            Self::GroupInvite => "👥",
            Self::Knock => "🔔",
            Self::Reply => "↩️",
            Self::Like => "❤️",
            Self::Unknown => "🔔",
            Self::Report => "🚩",
        }
    }
}

/// `notif_type` wire string → display emoji — what linux and web actually
/// want. [`NotificationGlyph`] is exposed separately for a caller that needs
/// the category itself rather than an emoji.
pub fn notification_type_emoji(notif_type: &str) -> &'static str {
    NotificationGlyph::from_notif_type(notif_type).emoji()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_types_map_to_their_emoji() {
        assert_eq!(notification_type_emoji("message"), "💬");
        assert_eq!(notification_type_emoji("mention"), "@");
        assert_eq!(notification_type_emoji("follow"), "👤");
        assert_eq!(notification_type_emoji("event_invite"), "📅");
        assert_eq!(notification_type_emoji("group_invite"), "👥");
        assert_eq!(notification_type_emoji("knock"), "🔔");
        assert_eq!(notification_type_emoji("reply"), "↩️");
        assert_eq!(notification_type_emoji("like"), "❤️");
        assert_eq!(notification_type_emoji("abuse_report.received"), "🚩");
        assert_eq!(notification_type_emoji("abuse_report.resolved"), "🚩");
        assert_eq!(
            NotificationGlyph::from_notif_type("abuse_report.resolved"),
            NotificationGlyph::Report
        );
    }

    /// The typed classification is the definition; the string face is only
    /// its projection, so the two can never disagree.
    #[test]
    fn typed_classification_covers_every_type_and_agrees_with_the_string_face() {
        use crate::notification_type::NotifType;
        assert_eq!(
            NotificationGlyph::of(&NotifType::AbuseReportReceived),
            NotificationGlyph::Report
        );
        assert_eq!(
            NotificationGlyph::of(&NotifType::Knock),
            NotificationGlyph::Knock
        );
        // A type with no icon of its own, and a type this build does not
        // name, both paint the neutral bell.
        assert_eq!(
            NotificationGlyph::of(&NotifType::SecurityNotice),
            NotificationGlyph::Unknown
        );
        assert_eq!(
            NotificationGlyph::of(&NotifType::Other("calendar.rsvp".into())),
            NotificationGlyph::Unknown
        );
        for wire in ["message", "like", "abuse_report.resolved", "repost", ""] {
            assert_eq!(
                NotificationGlyph::from_notif_type(wire),
                NotificationGlyph::of(&NotifType::from(wire)),
                "{wire:?}"
            );
        }
    }

    #[test]
    fn unknown_type_falls_back_to_bell() {
        assert_eq!(notification_type_emoji(""), "🔔");
        assert_eq!(notification_type_emoji("security.notice"), "🔔");
        assert_eq!(notification_type_emoji("repost"), "🔔");
    }
}
