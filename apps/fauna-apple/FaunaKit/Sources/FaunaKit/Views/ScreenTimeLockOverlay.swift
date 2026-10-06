import SwiftUI

/// The ward's full-screen **screen-time lock** (`family-safety.md` § Screen
/// time, Slice E; ui.yaml `global:` elements `screen-time-lock` /
/// `screen-time-lock-message`, user-approved 2026-07-15) — the apple twin of
/// linux `screen_lock.rs::build_lock_overlay` / android's lock composable.
///
/// **Client-enforced by construction.** The nest cannot see when a child's
/// device is in use and deliberately does not gate on it, so this overlay
/// *is* the enforcement. Presence and wording both come from
/// `ScreenTimeStore.lockMessage` — the one decision point, so they cannot
/// disagree.
///
/// Mounted as a `.overlay` over the content only (never the whole window):
/// the caller keeps its own top chrome — including the `supervised-indicator`
/// that navigates to Family — outside this view, and skips mounting it at all
/// while the Family page is showing, so a locked ward can always reach their
/// own policy (§ Screen time: "the Family page stays reachable read-only").
public struct ScreenTimeLockOverlay: View {
    public let message: String

    public init(message: String) {
        self.message = message
    }

    public var body: some View {
        VStack(spacing: 12) {
            Text(L.family.screenLockTitle)
                .font(.title)
                .bold()
            automationText(Ids.screenTimeLockMessage, message)
                .font(.body)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Text(L.family.screenLockFamilyHint)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // Opaque and hit-blocking — the point of the lock is that the page
        // behind it cannot be read or used (mirrors linux's `background` CSS
        // class + `can_target` swallow on a visible overlay child).
        .background(.background)
        .contentShape(Rectangle())
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.screenTimeLock)
        .automationValue(Ids.screenTimeLock, text: { "locked" })
    }
}
