import SwiftUI
import FaunaKit
import Sparkle

struct GeneralSettingsView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var pushManager: PushManager?

    // Optional cast: under XCUITest the delegate adaptor timing can leave
    // NSApp.delegate as something other than AppDelegate at view-construction
    // time. A forced cast crashes the whole app (SIGABRT, swift_dynamicCastFailure).
    // When unavailable, hide the Updates section rather than crash.
    private var updater: SPUUpdater? {
        (NSApp.delegate as? AppDelegate)?.updaterController.updater
    }

    var body: some View {
        @Bindable var state = appState

        Form {
            Section(L.settings.appearance) {
                Toggle(L.settings.showInDock, isOn: $state.dockIconVisible)
                    .onChange(of: appState.dockIconVisible) { _, _ in
                        appState.updateDockVisibility()
                    }
            }

            // Push notifications (settings.md § Push notifications, root-page
            // item 10). This sub-page is macOS's `.general` slot, the same
            // `SettingsPage` case iOS routes to `NotificationSettingsView` — so
            // the control lands here rather than in a macOS-only rail entry no
            // other app has (priority #1). The rows are FaunaKit's, shared with
            // iOS.
            Section(L.settings.pushNotifications.title) {
                PushNotificationsRows(manager: pushManager)
            }

            Section(L.settings.startup) {
                Toggle(L.settings.autostartHeader,
                       isOn: Binding(
                        get: { appState.autostartShown },
                        set: { setAutostart($0) }
                       ))
                    .accessibilityIdentifier(Ids.settingsAutostartToggle)
                    // Toggle: activate flips through the same writeback a tap uses;
                    // value backs the toggle `state` read ("on"/"off"), one Entry.
                    .automationActivate(Ids.settingsAutostartToggle,
                                        value: { appState.autostartShown ? "on" : "off" }) {
                        setAutostart(!appState.autostartShown)
                    }
                    // The toggle tells the truth about a Login Items opt-out
                    // (`AutoStart.displayedChoice`) — re-read it whenever this
                    // page shows and whenever the user comes back to the app,
                    // e.g. from the System Settings page turning it on routes to.
                    .onAppear { appState.refreshAutostartOSStatus() }
                    .onReceive(NotificationCenter.default.publisher(
                        for: NSApplication.didBecomeActiveNotification)) { _ in
                        appState.refreshAutostartOSStatus()
                    }
            }

            if let updater {
                Section(L.settings.updates) {
                    Toggle(L.settings.autoCheckUpdates,
                           isOn: Binding(
                            get: { updater.automaticallyChecksForUpdates },
                            set: { updater.automaticallyChecksForUpdates = $0 }
                           ))

                    Toggle(L.settings.autoDownloadUpdates,
                           isOn: Binding(
                            get: { updater.automaticallyDownloadsUpdates },
                            set: { updater.automaticallyDownloadsUpdates = $0 }
                           ))

                    HStack {
                        Text(L.settings.lastChecked)
                            .foregroundStyle(.secondary)
                        Spacer()
                        if let lastCheck = updater.lastUpdateCheckDate {
                            Text(lastCheck, style: .relative)
                                .foregroundStyle(.secondary)
                        } else {
                            Text(L.common.never)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .font(.caption)
                }
            }
        }
        .formStyle(.grouped)
        .task {
            // The launch path already built one for the AppDelegate callbacks
            // (`FaunaMacApp.completeAuthenticatedLaunch`); read that instance —
            // the toggle's Enable completes in the token callback, which lands
            // on it — and fall back to a fresh one when the page is reached
            // before the session settles.
            guard let client else { return }
            let pm = appState.pushManager
                ?? PushManager.sessionManager
                ?? PushManager(api: client.api, deviceId: client.deviceId)
            await pm.checkPermission()
            appState.pushManager = pm
            self.pushManager = pm
        }
    }

    /// Apply an explicit start-at-login choice — shared by the autostart
    /// Toggle's binding and its `automationActivate` sibling (no drift).
    private func setAutostart(_ on: Bool) {
        appState.setLaunchAtLogin(on)
    }
}
