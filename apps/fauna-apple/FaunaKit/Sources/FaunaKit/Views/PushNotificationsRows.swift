import SwiftUI

/// The push-notification control's rows, shared by both Apple targets
/// (`docs/goal/ui/settings.md` § Push notifications).
///
/// One opt-in toggle — the same control, with the same element ids, as every
/// other app — plus the inline failure line and apple's denied pair. Rows, not
/// a page: each host wraps these in its own container — iOS a `List` `Section`
/// inside `NotificationSettingsView`, macOS a `Form` `Section` inside
/// `GeneralSettingsView`. Lifting the body here rather than copying it to a
/// macOS-only file is the `IdentityChoiceView`/`RadioOptionRow` precedent
/// (priority #2).
///
/// What it renders is `PushSectionState` — pure, and unit-tested without a
/// notification centre. The toggle shows the install's stored opt-in, never
/// the OS permission.
public struct PushNotificationsRows: View {
    private let manager: PushManager?

    public init(manager: PushManager?) {
        self.manager = manager
    }

    private var state: PushSectionState {
        PushSectionState.resolve(
            optedIn: manager?.isOptedIn ?? false,
            permission: manager?.permission,
            lastError: manager?.lastError
        )
    }

    private var canToggle: Bool {
        guard let manager else { return false }
        return !manager.isWorking
    }

    private func apply(_ on: Bool) {
        guard let manager else { return }
        Task { await manager.setOptIn(on) }
    }

    public var body: some View {
        let state = state

        Toggle(L.settings.pushNotifications.optInLabel,
               isOn: Binding(get: { state.isOn }, set: { apply($0) }))
            .disabled(!canToggle)
            .accessibilityIdentifier(Ids.pushNotificationsOptInToggle)
            // Toggle: activate flips through the same call a tap makes; value
            // backs the toggle `state` read ("on"/"off") — the stored bit, so a
            // failed enable reads off again without a relaunch.
            .automationActivate(Ids.pushNotificationsOptInToggle,
                                isEnabled: { canToggle },
                                value: { state.isOn ? "on" : "off" }) {
                apply(!state.isOn)
            }
            // `fauna.push.subscribe` is `OnlineOnly`: both directions of the
            // toggle are a nest call, so it is not offered offline.
            .faunaGate("fauna.push.subscribe")

        // The section's own id rides this always-present leaf: a SwiftUI
        // `Form`/`List` `Section` drops a container identifier, and one on the
        // rows' enclosing view would overwrite every child's.
        Text(L.settings.pushNotifications.deviceDescription)
            .font(.caption)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier(Ids.pushNotificationsSection)
            .automationValue(Ids.pushNotificationsSection,
                             text: { L.settings.pushNotifications.deviceDescription })

        if state.showsDenied {
            Text(L.status.notifications.deniedHint)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.pushNotificationsDeniedHint)
                .automationValue(Ids.pushNotificationsDeniedHint,
                                 text: { L.status.notifications.deniedHint })

            // Both targets have somewhere to send the user; only the URL
            // differs (`PushManager.systemNotificationSettingsURL`).
            if let url = PushManager.systemNotificationSettingsURL {
                Button(L.status.notifications.openSettings) { OpenURL.open(url) }
                    .accessibilityIdentifier(Ids.pushNotificationsOpenSettingsButton)
                    .automationActivate(Ids.pushNotificationsOpenSettingsButton) {
                        OpenURL.open(url)
                    }
            }
        }

        // "On failure (permission denied, or the platform's push APIs
        // unavailable) the toggle settles back off and an inline error message
        // renders" (settings.md). It is also the only witness a user has that
        // the `aps-environment` entitlement is still missing.
        if let message = state.error {
            Text(message)
                .font(.caption)
                .foregroundStyle(.red)
                .accessibilityIdentifier(Ids.pushNotificationsError)
                .automationValue(Ids.pushNotificationsError, text: { message })
        }
    }
}
