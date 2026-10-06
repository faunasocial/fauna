import Foundation

/// What the push-notification settings control renders right now
/// (`docs/goal/ui/settings.md` § Push notifications — one opt-in toggle, an
/// inline failure line, and apple's denied pair).
///
/// Pure and platform-free on purpose: macOS and iOS render the same control
/// from one decision rather than two hand-rolled ones that drift, and the
/// decision is unit-testable without a notification centre
/// (`PushSectionStateTests`).
public struct PushSectionState: Equatable, Sendable {
    /// The toggle. **The install's own opt-in, never the OS permission** — the
    /// two differ exactly when it matters: turning push off deliberately leaves
    /// the permission granted, so a render keyed on permission would show "on"
    /// straight after a successful switch-off and never tell the user it took.
    public let isOn: Bool
    /// The OS permission was refused: paint the hint plus the shortcut to the
    /// platform's own notification settings (apple's `platform_elements` pair).
    public let showsDenied: Bool
    /// The inline failure line, when the last toggle failed. On a build that
    /// cannot register at all — no `aps-environment` entitlement yet, or the
    /// bare debug binary with no notification centre — this line is the
    /// specified behaviour and the user's only witness that the gate is shut.
    public let error: String?

    public static func resolve(
        optedIn: Bool,
        permission: PushManager.Permission?,
        lastError: String?
    ) -> PushSectionState {
        PushSectionState(
            isOn: optedIn,
            showsDenied: permission == .denied,
            error: (lastError?.isEmpty ?? true) ? nil : lastError
        )
    }
}
