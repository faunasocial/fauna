import SwiftUI

/// The Notifications page's per-row type icon (`notification-type-icon`) — ONE
/// leaf for both apple apps, the way `LinkPreviewCard` is one leaf for both.
///
/// `docs/goal/behavior/notifications.md` § Where logic lives splits this in two:
/// **shared Rust returns the type enum, app glue picks the icon**, because icons
/// are platform-native assets. So the `notif_type` wire string is classified by
/// `notificationGlyphForType` (`fauna_core::notification_glyph`, the same mapping
/// linux and web read) and only the symbol choice below is apple's. Until
/// 2026-09-20 both apple apps painted one fixed `bell` for every row and
/// registered an EMPTY automation value, which is the gap that doc named:
/// "macOS/iOS are the only two that don't yet render a `notif_type`-driven icon
/// at all".
///
/// The accessible label is the icon's meaning in words, and the automation value
/// is that same string — not a second, invented one. `ui.yaml` types
/// `notification-type-icon` as `text` and the cross-app e2e reads it with
/// `get_text`, so an icon-only view with no announced text can never satisfy the
/// contract; linux hit exactly this and swapped its `gtk::Image` for a
/// `gtk::Label`. apple keeps the native symbol and announces it instead, which is
/// what android already does with its Material icon's `contentDescription`.
public struct NotificationTypeIcon: View {
    private let glyph: NotificationGlyph

    /// Takes the raw `notif_type` wire string and classifies it through shared
    /// Rust — never a pre-mapped symbol name, so no caller can drift from the
    /// one classification.
    public init(notifType: String) {
        self.glyph = notificationGlyphForType(notifType: notifType)
    }

    /// The SF Symbol for each shared category. apple's own asset family: linux
    /// and web take `NotificationGlyph::emoji`, android its Material icons.
    private var systemImage: String {
        switch glyph {
        case .message: return "bubble.left.fill"
        case .mention: return "at"
        case .follow: return "person.crop.circle.badge.plus"
        case .eventInvite: return "calendar"
        case .groupInvite: return "person.2.fill"
        case .knock: return "hand.wave.fill"
        case .reply: return "arrowshape.turn.up.left.fill"
        case .like: return "heart.fill"
        case .unknown: return "bell.fill"
        case .report: return "flag.fill"
        }
    }

    /// What the symbol means, in words — announced to assistive technology and
    /// read by the automation registry as this element's text.
    private var label: String {
        switch glyph {
        case .message: return L.notifications.typeMessage
        case .mention: return L.notifications.typeMention
        case .follow: return L.notifications.typeFollow
        case .eventInvite: return L.notifications.typeEventInvite
        case .groupInvite: return L.notifications.typeGroupInvite
        case .knock: return L.notifications.typeKnock
        case .reply: return L.notifications.typeReply
        case .like: return L.notifications.typeLike
        case .unknown: return L.notifications.typeDefault
        case .report: return L.notifications.typeReport
        }
    }

    public var body: some View {
        Image(systemName: systemImage)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(Ids.notificationTypeIcon)
            .accessibilityLabel(label)
            // Re-reads the label per lookup, like every other `automationValue`
            // in this codebase. Env-gated no-op outside an automation build.
            .automationValue(Ids.notificationTypeIcon, text: { label })
    }
}
