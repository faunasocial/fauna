import FaunaExtensionKit
import SwiftUI
import WidgetKit

/// The home-screen widget extension (`apps/common.md` § Home-screen widget),
/// shared source for the macOS `Fauna-Widget` and iOS `Fauna-iOS-Widget` appex
/// targets. A shell by design: every piece of logic is in `FaunaExtensionKit`
/// (the snapshot store, the timeline, the view), so this file only wires them
/// to WidgetKit. It links nothing else — no FaunaKit, no Rust FFI.
@main
struct FaunaWidgetBundle: WidgetBundle {
    var body: some Widget {
        UnreadWidget()
    }
}

struct UnreadWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: AppleIdentifiers.unreadWidgetKind, provider: UnreadProvider()) { entry in
            UnreadWidgetView(entry: entry)
        }
        .configurationDisplayName(Text(verbatim: "Fauna"))
        .description(Text(L.widget.description))
        .supportedFamilies([.systemSmall])
    }
}

/// Reads the snapshot the app wrote; never computes a count itself
/// (`UnreadSnapshot` says why).
struct UnreadProvider: TimelineProvider {
    func placeholder(in context: Context) -> UnreadWidgetEntry {
        .placeholder
    }

    func getSnapshot(in context: Context, completion: @escaping (UnreadWidgetEntry) -> Void) {
        completion(UnreadWidgetEntry.from(UnreadSnapshotStore.appGroup()?.load(), now: .now))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<UnreadWidgetEntry>) -> Void) {
        completion(UnreadWidgetTimeline.timeline(for: UnreadSnapshotStore.appGroup()?.load(), now: .now))
    }
}
