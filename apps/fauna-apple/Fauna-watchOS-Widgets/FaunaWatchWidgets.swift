import WidgetKit
import SwiftUI

@main
struct FaunaWatchWidgets: WidgetBundle {
    var body: some Widget {
        FaunaComplication()
    }
}

struct FaunaComplication: Widget {
    let kind = "social.fauna.watch.complication"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: ComplicationProvider()) { entry in
            FaunaComplicationEntryView(entry: entry)
        }
        .configurationDisplayName("Fauna Inbox")
        .description("Shows unread message count")
        .supportedFamilies([
            .accessoryCircular,
            .accessoryRectangular,
            .accessoryInline,
            .accessoryCorner,
        ])
    }
}

struct FaunaComplicationEntryView: View {
    @Environment(\.widgetFamily) var family
    let entry: UnreadEntry

    var body: some View {
        switch family {
        case .accessoryCircular:
            CircularComplicationView(entry: entry)
        case .accessoryRectangular:
            RectangularComplicationView(entry: entry)
        case .accessoryInline:
            InlineComplicationView(entry: entry)
        case .accessoryCorner:
            CornerComplicationView(entry: entry)
        default:
            CircularComplicationView(entry: entry)
        }
    }
}
