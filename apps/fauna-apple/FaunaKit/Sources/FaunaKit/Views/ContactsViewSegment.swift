import SwiftUI

/// The Contacts page's top-level **`Contacts | Address Book`** segment toggle
/// (`contacts-view-segment` — `contacts.md` § Address Book segment;
/// `carddav-server.md` § Independent enablement). Two plain automation-
/// registrable `Button`s in a capsule (mirrors windows' "two plain Buttons
/// toggling section visibility" — the established codebase idiom for a
/// driver-registrable toggle, e.g. `media-view-toggle`; a native segmented
/// `Picker` would extract its labels into a platform widget that never mounts
/// the SwiftUI view identity `automationActivate` needs). Shared macOS + iOS
/// FaunaKit component, embedded above the page body by `ContactSplitView`
/// (macOS) / `ContactsView` (iOS).
public struct ContactsViewSegment: View {
    @Binding private var showAddressBook: Bool

    public init(showAddressBook: Binding<Bool>) {
        self._showAddressBook = showAddressBook
    }

    public var body: some View {
        HStack(spacing: 0) {
            segment(title: L.common.contacts, isSelected: !showAddressBook, id: "contacts-segment-people") {
                showAddressBook = false
            }
            segment(title: L.contacts.addressBook.title, isSelected: showAddressBook, id: "contacts-segment-addressbook") {
                showAddressBook = true
            }
        }
        .padding(4)
        .background(Color.secondary.opacity(0.12), in: Capsule())
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.contactsViewSegment)
        .automationValue(Ids.contactsViewSegment, text: { "" })
    }

    private func segment(title: String, isSelected: Bool, id: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(title)
                .font(.callout.weight(isSelected ? .semibold : .regular))
                .padding(.horizontal, 14)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity)
                .background(isSelected ? Color.accentColor.opacity(0.25) : Color.clear, in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(id)
        .automationActivate(id) { action() }
    }
}
