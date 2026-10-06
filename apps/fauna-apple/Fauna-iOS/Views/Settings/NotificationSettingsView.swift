import SwiftUI
import FaunaKit

/// iOS's host for the shared push-notification control (`settings.md` § Push
/// notifications). The rows themselves are `PushNotificationsRows` in FaunaKit,
/// so macOS's General sub-page renders the same opt-in toggle, the same denied
/// pair and the same inline error — priority #2, the `IdentityChoiceView`
/// precedent. This file keeps only what is iOS's: the page chrome and the
/// `List` container.
struct NotificationSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var pushManager: PushManager?

    var body: some View {
        List {
            Section {
                PushNotificationsRows(manager: pushManager)
            }

            Section {
                Text(L.status.notifications.contentEncrypted)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .pageTitle(L.common.notifications)
        .task {
            guard let client else { return }
            // Read the launch path's instance when there is one: the toggle's
            // Enable completes in the AppDelegate's token callback, which
            // lands on that instance — a private one here would never see it.
            let pm = PushManager.sessionManager
                ?? PushManager(api: client.api, deviceId: client.deviceId)
            await pm.checkPermission()
            self.pushManager = pm
        }
    }
}
