import WatchKit
import SwiftUI
import UserNotifications

class NotificationController: WKUserNotificationHostingController<NotificationView> {
    override var body: NotificationView {
        NotificationView()
    }
}

struct NotificationView: View {
    var body: some View {
        VStack(alignment: .leading) {
            Text("Fauna")
                .font(.headline)
            Text("New message received")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
