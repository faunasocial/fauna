import SwiftUI

/// The Contacts page's **Address Book** segment content (`contacts.md` §
/// Address Book segment; `carddav-server.md` § Independent enablement) — a
/// read-only master-detail over the CardDAV vCard store, a **separate store**
/// from the social contact graph the plain Contacts view renders. Shared macOS
/// + iOS FaunaKit renderer, embedded by the platform Contacts shells
/// (`ContactSplitView`/`ContactsView`) when the `contacts-segment-addressbook`
/// toggle is active. Mirrors the Events page's master-detail (`addressbook-item`
/// ~ `calendar-item`, `vcard-card` ~ `event-card`, `card_detail` ~
/// `event_detail`) and android's inline `card_detail` pane (the richer inline
/// shape vs. windows' separate-page `Frame` navigation — priority #4).
public struct AddressBookView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AddressBookVM()

    /// A `search-result-item` Contact-arm deep-link target (a CardDAV
    /// `uid_hash`) to locate and open once configured — the platform shell
    /// reads its own pending-state slot (`ui/search.md` § Where logic lives →
    /// Result navigation (deep link)) and passes it down + clears it via
    /// `onLocated`, keeping this shared view free of any platform-specific
    /// AppState type. `nil` in the ordinary (non-deep-link) case.
    private let pendingUidHash: String?
    private let onLocated: () -> Void

    public init(pendingUidHash: String? = nil, onLocated: @escaping () -> Void = {}) {
        self.pendingUidHash = pendingUidHash
        self.onLocated = onLocated
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 0) {
                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                        .padding(.horizontal, 16)
                        .padding(.top, 8)
                }
                bookPicker
                Divider()
                cardList
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

            // The `card_detail` sub-page — an inline `@State`-driven overlay,
            // never a `.sheet` (`apple-e2e-automation.md` registration rule 3;
            // mirrors `MediaItemDetailView`).
            if let card = vm.selectedCard {
                CardDetailOverlay(card: card, onClose: { vm.dismissCardDetail() })
            }
        }
        // Keyed on the session's client, not one-shot: iOS hosts this view inside the
        // Contacts TAB, which is never unmounted, so an account switch with the
        // segment open would otherwise leave the outgoing account's vCards on screen.
        // The nil-client phase drops them (`AddressBookVM.reset()`) and the incoming
        // client rebuilds (`account-scoping.md` § The scoping taxonomy, the "reused
        // shell" case). macOS unmounts the whole window shell on a switch, so there
        // the reset is redundant — carried for uniformity, as `SearchVM`'s is.
        .task(id: SessionKey(client)) {
            guard let client else {
                vm.reset()
                return
            }
            await vm.configure(api: client.api)
        }
        // The deep-link door: configure THEN locate, sequentially — a bare
        // `.task { locate }` run alongside the plain configure task above would
        // race `carddav` not being ready yet and silently no-op.
        //
        // ⚠ Sequencing here is NOT sufficient on its own, and believing it was
        // is what made this leg fail e2e for real:
        // both tasks start at mount, so this `configure` is the SECOND caller
        // and returns as soon as the first has *started*. `AddressBookVM`'s
        // `configureTask` is what makes that second call await the first run to
        // *completion*; without it, `locateCard` below runs against a nil client.
        .task(id: pendingUidHash) {
            guard let pendingUidHash, let client else { return }
            await vm.configure(api: client.api)
            await vm.locateCard(uidHash: pendingUidHash)
            onLocated()
        }
        // Re-pull on a fauna.addressbook.changed push — this view is mounted
        // only while the Address Book segment is showing, which is the segment
        // gate the row asks for; see `onAddressBookChanged`'s doc comment.
        .onAddressBookChanged { await vm.refreshFromPush() }
    }

    /// The Address Book picker (`addressbook-item`, indexed — mirror
    /// `calendar-item`): one row per book, name + card count, selection
    /// highlighted. Tapping loads that book's cards.
    @ViewBuilder
    private var bookPicker: some View {
        if vm.addressbooks.isEmpty {
            if !vm.isLoading {
                Text(L.contacts.addressBook.noAddressbooks)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .padding(16)
            }
        } else {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(vm.addressbooks, id: \.id) { book in
                        addressbookRow(book)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 8)
            }
        }
    }

    private func addressbookRow(_ book: FfiAddressbookRow) -> some View {
        let isSelected = vm.selectedAddressbookId == book.id
        let name = book.name.isEmpty ? L.contacts.addressBook.title : book.name
        return Button {
            Task { await vm.selectAddressbook(book.id) }
        } label: {
            HStack(spacing: 6) {
                Text(name)
                Text("\(book.cardCount)")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(isSelected ? Color.accentColor.opacity(0.2) : Color.secondary.opacity(0.1),
                        in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(Ids.addressbookItem)
        .automationActivate(Ids.addressbookItem, value: { name }) {
            Task { await vm.selectAddressbook(book.id) }
        }
    }

    /// The selected book's card list (`vcard-card`, indexed — mirror
    /// `event-card`). Tapping a card opens the `card_detail` overlay.
    @ViewBuilder
    private var cardList: some View {
        ScrollView {
            if vm.cards.isEmpty {
                if !vm.isLoading {
                    Text(L.contacts.addressBook.noCards)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .padding(.top, 32)
                }
            } else {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(vm.cards, id: \.id) { card in
                        Button {
                            vm.selectCard(card)
                        } label: {
                            automationText(Ids.vcardCardFn, card.formattedName)
                                .font(.body)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .buttonStyle(.plain)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 10)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.vcardCard)
                        .automationActivate(Ids.vcardCard) { vm.selectCard(card) }
                        Divider()
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

/// The `card_detail` sub-page: FN header + indexed EMAIL/TEL/ADR + ORG/NOTE.
/// Read-only (slice 4b — no vCard write UI). Mirrors `MediaItemDetailView`'s
/// inline-overlay shape (tap-outside-to-dismiss scrim, no `.sheet`).
private struct CardDetailOverlay: View {
    let card: FfiCardRow
    let onClose: () -> Void

    var body: some View {
        ZStack {
            Color.black.opacity(0.35)
                .ignoresSafeArea()
                .onTapGesture { onClose() }

            VStack(alignment: .leading, spacing: 10) {
                automationText(Ids.vcardDetailFn, card.formattedName)
                    .font(.headline)

                ForEach(card.emails, id: \.value) { email in
                    detailRow(L.contacts.addressBook.email, value: email.value, id: Ids.vcardDetailEmail)
                }
                ForEach(card.tels, id: \.value) { tel in
                    detailRow(L.contacts.addressBook.phone, value: tel.value, id: Ids.vcardDetailTel)
                }
                ForEach(card.addresses, id: \.formatted) { address in
                    detailRow(L.contacts.addressBook.address, value: address.formatted, id: Ids.vcardDetailAdr)
                }

                let org = AddressBookVM.orgLabel(card.org)
                if !org.isEmpty {
                    detailRow(L.contacts.addressBook.organization, value: org, id: Ids.vcardDetailOrg)
                }
                if !card.note.isEmpty {
                    detailRow(L.contacts.addressBook.note, value: card.note, id: Ids.vcardDetailNote)
                }
            }
            .padding(20)
            .background(.background, in: RoundedRectangle(cornerRadius: 12))
            .padding(32)
            .accessibilityElement(children: .contain)
        }
    }

    private func detailRow(_ label: String, value: String, id: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(id, value)
                .font(.body)
        }
    }
}
