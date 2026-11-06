import SwiftUI

/// The ward's ask row, shared by the Contacts page's Find User result and the
/// profile page's request-contact button (`family-safety.md` § Child-initiated
/// contact requests → *App affordance*): the ask button after a guardian-refused
/// knock, the "asked — waiting for your guardian" label once one is outstanding,
/// and nothing otherwise. One view for both pages and both apps (priority #1/#2) —
/// the four `contact-request-*` render sites were otherwise four copies.
///
/// The state is decided by ``ContactAsk/state(peer:store:)``, never here; this only
/// paints it.
public struct ContactAskRow: View {
    let state: ContactAsk.State
    let onAsk: () -> Void

    public init(state: ContactAsk.State, onAsk: @escaping () -> Void) {
        self.state = state
        self.onAsk = onAsk
    }

    public var body: some View {
        switch state {
        case .none:
            EmptyView()
        case .pending:
            automationText(Ids.contactRequestPending, L.contacts.contactRequestPending)
                .font(.caption)
                .foregroundStyle(.secondary)
        case .offered:
            Button(L.contacts.askGuardian, action: onAsk)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.contactRequestGuardianButton)
                .automationActivate(Ids.contactRequestGuardianButton, perform: onAsk)
        }
    }
}

/// The knock row's "waiting for your guardian's approval" mark
/// (`knock-guardian-pending`, `family-safety.md` § App surface) — painted on every
/// incoming knock while the supervised account's policy has contact approval on,
/// because the guardian decides it, not the ward. One view for the macOS and iOS
/// knock rows; the verdict is ``ContactsVM/knocksAwaitGuardian``, never decided here.
public struct KnockGuardianPendingLabel: View {
    let visible: Bool

    public init(visible: Bool) {
        self.visible = visible
    }

    public var body: some View {
        if visible {
            automationText(Ids.knockGuardianPending, L.contacts.knockGuardianPending)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}
