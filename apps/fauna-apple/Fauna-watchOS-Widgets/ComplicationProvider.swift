import WidgetKit
import SwiftUI
import FaunaKit

struct UnreadEntry: TimelineEntry {
    let date: Date
    let unreadCount: Int
    let lastSender: String?
    let lastSubject: String?
}

struct ComplicationProvider: TimelineProvider {
    func placeholder(in context: Context) -> UnreadEntry {
        UnreadEntry(date: .now, unreadCount: 0, lastSender: nil, lastSubject: nil)
    }

    func getSnapshot(in context: Context, completion: @escaping (UnreadEntry) -> Void) {
        completion(UnreadEntry(date: .now, unreadCount: 3, lastSender: "alice", lastSubject: "Hello"))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<UnreadEntry>) -> Void) {
        // Read from shared UserDefaults (App Group)
        let defaults = UserDefaults(suiteName: AppleIdentifiers.watchAppGroup)
        let count = defaults?.integer(forKey: "unread_count") ?? 0
        let sender = defaults?.string(forKey: "last_sender")
        let subject = defaults?.string(forKey: "last_subject")

        let entry = UnreadEntry(date: .now, unreadCount: count,
                                lastSender: sender, lastSubject: subject)
        let timeline = Timeline(entries: [entry], policy: .after(.now.addingTimeInterval(900)))
        completion(timeline)
    }
}

struct CircularComplicationView: View {
    let entry: UnreadEntry

    var body: some View {
        ZStack {
            AccessoryWidgetBackground()
            Text("\(entry.unreadCount)")
                .font(.title2)
                .fontWeight(.bold)
        }
    }
}

struct RectangularComplicationView: View {
    let entry: UnreadEntry

    var body: some View {
        VStack(alignment: .leading) {
            if let sender = entry.lastSender, let subject = entry.lastSubject {
                Text(sender)
                    .font(.caption2)
                    .fontWeight(.semibold)
                Text(subject)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else {
                Text("Fauna")
                    .font(.caption2)
                    .fontWeight(.semibold)
                Text(entry.unreadCount > 0 ? "\(entry.unreadCount) unread" : "No new messages")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

struct InlineComplicationView: View {
    let entry: UnreadEntry

    var body: some View {
        Text(entry.unreadCount > 0 ? "\(entry.unreadCount) unread messages" : "No new messages")
    }
}

struct CornerComplicationView: View {
    let entry: UnreadEntry

    var body: some View {
        Text("\(entry.unreadCount)")
            .font(.title3)
            .fontWeight(.bold)
            .widgetLabel {
                Text("unread")
            }
    }
}
