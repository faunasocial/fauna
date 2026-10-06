import SwiftUI
import WidgetKit

/// One timeline entry: the count the widget shows, as of `date`.
public struct UnreadWidgetEntry: TimelineEntry, Equatable {
    public let date: Date
    public let count: Int

    public init(date: Date, count: Int) {
        self.date = date
        self.count = count
    }

    /// What the widget shows for whatever the app last wrote. No snapshot
    /// (never published, or signed out) reads as zero — android's
    /// `prefs[UNREAD_COUNT_KEY] ?: 0`, and the same "a stale number is worse
    /// than none" rule its account-switch refold states.
    public static func from(_ snapshot: UnreadSnapshot?, now: Date) -> UnreadWidgetEntry {
        UnreadWidgetEntry(date: now, count: snapshot?.count ?? 0)
    }

    /// The widget gallery's preview and the pre-first-render placeholder.
    public static let placeholder = UnreadWidgetEntry(date: .distantPast, count: 0)
}

/// The timeline a provider hands WidgetKit: one entry, reloaded only when the
/// app writes a new snapshot and calls `WidgetCenter.reloadTimelines(ofKind:)`.
/// Never `.after(…)`: the widget cannot compute the count (``UnreadSnapshot``),
/// so waking it on a schedule would only re-read an unchanged file. What moves
/// the count lives in the app, the one process that can open the messages.
public enum UnreadWidgetTimeline {
    public static func timeline(for snapshot: UnreadSnapshot?, now: Date) -> Timeline<UnreadWidgetEntry> {
        Timeline(entries: [UnreadWidgetEntry.from(snapshot, now: now)], policy: .never)
    }
}

/// The widget face — android's `FaunaWidget` content on the apple platforms:
/// the brand line, the count, the shared `widget.unread_label` string, and the
/// compose line the shared `widget.description` promises. Tapping anywhere,
/// the compose line included, opens the app (WidgetKit's default for a widget
/// with no explicit link — android's `actionStartActivity<MainActivity>()` on
/// both its body and its compose text).
public struct UnreadWidgetView: View {
    public let entry: UnreadWidgetEntry

    public init(entry: UnreadWidgetEntry) {
        self.entry = entry
    }

    public var body: some View {
        VStack(spacing: 4) {
            Text(verbatim: "Fauna")
                .font(.system(size: 14, weight: .bold))
                .foregroundStyle(.tint)
            Text(verbatim: "\(entry.count)")
                .font(.system(size: 32, weight: .bold))
                .contentTransition(.numericText())
            Text(L.widget.unreadLabel)
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
            Text(L.conversations.compose.title)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(.tint)
                .padding(.top, 4)
        }
        .containerBackground(.fill.tertiary, for: .widget)
    }
}
