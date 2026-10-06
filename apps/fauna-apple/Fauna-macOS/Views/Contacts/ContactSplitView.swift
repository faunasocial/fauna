import SwiftUI
import FaunaKit

struct ContactSplitView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = ContactsVM()
    @State private var selectedPeerId: String?
    /// The `contacts-view-segment` selection — the CardDAV Address Book is a
    /// separate store from the social contact graph below (contacts.md §
    /// Address Book segment).
    @State private var showAddressBook = false

    var body: some View {
        VStack(spacing: 0) {
            ContactsViewSegment(showAddressBook: $showAddressBook)
                .padding([.horizontal, .top], 12)

            if showAddressBook {
                AddressBookView(
                    pendingUidHash: appState.pendingContactUidHash,
                    onLocated: { appState.pendingContactUidHash = nil }
                )
            } else {
                NavigationSplitView {
                    MacContactListView(vm: vm, selectedPeerId: $selectedPeerId)
                } detail: {
                    Group {
                        if let peerId = selectedPeerId {
                            MacContactDetailView(vm: vm, peerId: peerId)
                        } else {
                            ContentUnavailableView(L.common.contacts,
                                systemImage: "person.crop.circle",
                                description: Text(L.contacts.noContactSelected))
                        }
                    }
                    .accessibilityElement(children: .contain)
                }
            }
        }
        .pageTitle(L.contacts.title)
        .accessibilityIdentifier(Ids.contactsView)
        // Presence anchor for `is_visible("contacts-view")`. Env-gated no-op.
        .automationValue(Ids.contactsView, text: { "" })
        // The drop below is REDUNDANT on macOS and carried for uniformity, as
        // `SearchVM`'s is: `tearDownSessionForSwitch()` sets `isOnboarded = false`,
        // which unmounts `MainWindowView` wholesale, so this view dies with the window
        // and its view model with it. That unmount IS the guarantee here
        // (`account-scoping.md` § The scoping taxonomy, the in-memory corollary: an app
        // whose drop rides a shell teardown must say where the guarantee comes from) —
        // which is exactly what iOS does not have, and why the seam lives on the view
        // model rather than at either site .
        // The key also moves from `client != nil` to the client INSTANCE: the boolean
        // cannot see a swap that keeps it true, which is the shape a re-login has.
        .task(id: SessionKey(client)) {
            guard let client, let actorId = appState.session.actorId else {
                vm.reset()
                return
            }
            vm.configure(api: client.api, actorId: actorId, familyStatus: appState.familyStatus)
            await vm.refresh()
            appState.lastContacts = vm.contacts
            appState.lastKnocks = vm.knocks
        }
        // Re-pull knocks + contacts on WS-RPC reconnect (mirrors linux).
        .onReconnect {
            await vm.refresh()
            appState.lastContacts = vm.contacts
            appState.lastKnocks = vm.knocks
        }
        // Re-pull on an inbound `fauna.knock` push, so a contact request arriving while
        // this screen is mounted appears live with no navigation (mirrors windows/android).
        .onKnockReceived {
            await vm.refresh()
            appState.lastContacts = vm.contacts
            appState.lastKnocks = vm.knocks
        }
        // A `search-result-item` Contact-arm deep link switches to the
        // Address Book segment (`AddressBookView` above does the actual
        // locate, once mounted) — `showAddressBook` is this view's own local
        // state, unreachable from Search directly.
        .task(id: appState.pendingContactUidHash) {
            if appState.pendingContactUidHash != nil { showAddressBook = true }
        }
        // A `notification-item` Knock-arm deep link goes the other way: the
        // pending knocks render on the PEOPLE segment, and the segment is
        // sticky, so a user last left on the Address Book would land on a page
        // with no `knock-request-item` on it (`behavior/notifications.md`
        // § Deep-link destinations). Mirrors iOS `ContactsView`.
        .task(id: appState.pendingKnockSenderId) {
            guard appState.pendingKnockSenderId != nil else { return }
            showAddressBook = false
            appState.pendingKnockSenderId = nil
        }
    }
}
