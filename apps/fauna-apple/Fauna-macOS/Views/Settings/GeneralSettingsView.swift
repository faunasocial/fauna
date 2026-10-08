import SwiftUI
import FaunaKit

struct GeneralSettingsView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var pushManager: PushManager?

    private var updates: UpdateCheck { appState.updates }

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

            // About: the version you are running and the newer-version check
            // (`docs/features/app-version-and-updates.md`; `installers/README.md`
            // § Knowing a newer version is out) — linux's General-page block, the
            // same three ids. The check only tells you: no toggle, no download.
            Section(L.settings.about) {
                LabeledContent(L.settings.generalPage.version) {
                    automationText(Ids.settingsAppVersion, updates.runningVersion)
                        .textSelection(.enabled)
                }

                Button(updates.buttonLabel) { updates.check() }
                    .disabled(updates.isChecking)
                    .accessibilityIdentifier(Ids.settingsCheckUpdatesButton)
                    .automationActivate(Ids.settingsCheckUpdatesButton,
                                        isEnabled: { !updates.isChecking },
                                        text: { updates.buttonLabel }) {
                        updates.check()
                    }

                if let notice = updates.notice {
                    automationText(Ids.updateAvailableNotice, notice)
                        .textSelection(.enabled)
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
