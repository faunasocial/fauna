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
