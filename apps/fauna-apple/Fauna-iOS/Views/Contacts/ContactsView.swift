import SwiftUI
import FaunaKit

struct ContactsView: View {
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The owner of the private-overlay projection every name on this page is
    /// read through (`contacts.md` § The private overlay).
    @Environment(ConversationsVM.self) private var conversationsVM
    @State private var vm = ContactsVM()
    @State private var findQuery = ""
    @State private var searchFilter = ""
    /// The `contacts-view-segment` selection — the CardDAV Address Book is a
    /// separate store from the social contact graph below (contacts.md §
    /// Address Book segment).
    @State private var showAddressBook = false

    var body: some View {
        let overlays = conversationsVM.contactOverlays
        NavigationStack {
            VStack(spacing: 0) {
            ContactsViewSegment(showAddressBook: $showAddressBook)
                .padding(.horizontal, 16)
                .padding(.top, 8)

            if showAddressBook {
                AddressBookView(
                    pendingUidHash: appState.pendingContactUidHash,
                    onLocated: { appState.pendingContactUidHash = nil }
                )
            } else {
            ScrollView {
                // Eager `ScrollView { VStack }`, NOT a lazy `List { Section }` (rule 6 —
                // apple-e2e-automation.md § Registration rules): an iOS `List` lazily
                // realizes AND POOLS its rows, so a row removed from `vm.knocks` (accept/
                // dismiss/block) or re-filtered out of `contactsByStatus` (the roster
                // search, `test_roster_filter_narrows_by_handle`) can linger on-screen
                // past its real removal — the same delete-zombie class rule 6 fixed for
                // iOS Events. Cost: rows lose `.insetGrouped` inset styling (accepted
                // rule-6 production-UI tradeoff).
                VStack(alignment: .leading, spacing: 16) {
                // Contact roster filter (`contacts-search-field`) — local,
                // case-insensitive substring over the already-loaded contacts via
                // the shared roster filter (`ContactsVM.rosterGroups`); `common.search`
                // placeholder matches web/macOS. Narrows the status groups below.
                Group {
                    TextField(L.common.search, text: $searchFilter)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier(Ids.contactsSearchField)
                        // Writes/reads the same `$searchFilter` binding. Env-gated no-op.
                        .automationField(Ids.contactsSearchField, text: $searchFilter)
                }

                // Find User
                VStack(alignment: .leading, spacing: 8) {
                    Text(L.contacts.findUser.title)
                        .font(.headline)
                    HStack {
                        TextField(L.contacts.findUser.placeholder, text: $findQuery)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier(Ids.contactActorIdField)
                            // Writes/reads the same `$findQuery` binding. Env-gated no-op.
                            .automationField(Ids.contactActorIdField, text: $findQuery)
                        Button(L.contacts.findUser.find) {
                            Task { await vm.findUser(query: findQuery) }
                        }
                        .disabled(findQuery.trimmingCharacters(in: .whitespaces).isEmpty)
                        .accessibilityIdentifier(Ids.contactActorIdLookup)
                        // Same lookup the Button runs; `isEnabled` live re-reads
                        // the trimmed query (don't capture). Env-gated no-op.
                        .automationActivate(Ids.contactActorIdLookup,
                            isEnabled: { !findQuery.trimmingCharacters(in: .whitespaces).isEmpty }) {
                            Task { await vm.findUser(query: findQuery) }
                        }
                    }

                    if let result = vm.findResult {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                // The FULL actor id, not shortId's truncated display — a user
                                // confirming they found the right person before knocking needs
                                // to see the whole id (every other app shows/holds the full
                                // string here: web untruncated, android's full underlying text
                                // under a visual ellipsis, windows unbound). `.lineLimit` +
                                // `.truncationMode` only affect the ON-SCREEN glyphs; the
                                // registered automation value stays the full id.
                                Text(result.actorId)
                                    .font(.caption.monospaced())
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                    .accessibilityIdentifier(Ids.contactActorIdResult)
                                    // Reads the resolved actor id (mirrors macOS). Env-gated no-op.
                                    .automationValue(Ids.contactActorIdResult, text: { result.actorId })
                                if let handle = result.handle, let domain = result.domain {
                                    Text("\(handle)@\(domain)")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            CopyButton(Ids.contactActorIdCopyBtn, text: result.actorId)
                            Spacer()
                            Button(L.contacts.knock) {
                                Task { await vm.addContact(actorId: result.actorId) }
                            }
                            .controlSize(.small)
                            .buttonStyle(.borderedProminent)
                            .accessibilityIdentifier(Ids.contactsAddButton)
                            // Same add the Button runs (mirrors macOS). Env-gated no-op.
                            .automationActivate(Ids.contactsAddButton) { Task { await vm.addContact(actorId: result.actorId) } }
                        }
                        // The ward's in-app ask (`family-safety.md` § Child-initiated
                        // contact requests): the button only after the nest's TYPED
                        // guardian refusal, the pending label once one is outstanding.
                        ContactAskRow(state: vm.askState(peerId: result.actorId)) {
                            Task { await vm.askGuardian(peerId: result.actorId) }
                        }
                    }

                    if let error = vm.findError {
                        ErrorBanner(message: error)
                            .accessibilityIdentifier(Ids.contactFindError)
                            // Read of the find-specific error (ErrorBanner itself
                            // registers `error-message`; this is the find wrapper id,
                            // mirrors macOS). Env-gated no-op in production.
                            .automationValue(Ids.contactFindError, text: { error })
                    }
                }

                // Message Requests
                if !vm.knocks.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(L.contacts.messageRequests.count(count: String(vm.knocks.count)))
                            .font(.headline)
                        ForEach(vm.knocks) { knock in
                            VStack(alignment: .leading, spacing: 4) {
                                HStack {
                                    // The viewer's nickname for the sender when one
                                    // is set, else the canonical short id (a knock
                                    // carries no public name). Mirrors macOS.
                                    automationText(Ids.knockSender, overlays.peerLabel(
                                        displayName: nil, handle: nil, actorId: knock.sender).primary)
                                        .font(.caption.monospaced())
                                    Spacer()
                                    if let node = knock.senderNode {
                                        Text(node)
                                            .font(.caption2)
                                            .foregroundStyle(.secondary)
                                    }
                                }
                                if let summary = knock.summary {
                                    Text(summary)
                                        .font(.caption)
                                        .lineLimit(2)
                                }
                                HStack(spacing: 8) {
                                    Button(L.common.accept) {
                                        Task { await vm.acceptKnock(peerId: knock.sender) }
                                    }
                                    .buttonStyle(.borderedProminent)
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.contactsAcceptButton)
                                    // Same accept the Button runs (one indexed Entry
                                    // per knock row). Env-gated no-op in production.
                                    .automationActivate(Ids.contactsAcceptButton) {
                                        Task { await vm.acceptKnock(peerId: knock.sender) }
                                    }

                                    Button(L.common.dismiss) {
                                        Task { await vm.dismissKnock(peerId: knock.sender) }
                                    }
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.knockDismiss)
                                    .automationActivate(Ids.knockDismiss) {
                                        Task { await vm.dismissKnock(peerId: knock.sender) }
                                    }

                                    Button(L.common.block) {
                                        Task { await vm.blockKnock(peerId: knock.sender) }
                                    }
                                    .controlSize(.small)
                                    .foregroundStyle(.red)
                                    .accessibilityIdentifier(Ids.contactsBlockButton)
                                    .automationActivate(Ids.contactsBlockButton) {
                                        Task { await vm.blockKnock(peerId: knock.sender) }
                                    }
                                }
                            }
                            .padding(.vertical, 2)
                            .accessibilityElement(children: .contain)
                            .accessibilityIdentifier(Ids.knockCard)
                            // Presence anchor for the indexed knock row. Env-gated no-op.
                            .automationValue(Ids.knockCard, text: { shortId(hex: knock.sender) })
                        }
                    }
                }

                // Contacts by status — each group narrowed by the roster filter
                // (`contacts-search-field`) via the shared `ContactsVM
                // .rosterGroups` (contacts.md § Contact roster filter), which also
                // carries each row's names and its flat index.
                let groupedContacts = vm.rosterGroups(searchFilter: searchFilter, overlays: overlays)
                if !vm.contacts.isEmpty && groupedContacts.isEmpty {
                    // Roster non-empty but contacts-search-field narrowed every status
                    // group to zero — a distinguishable message, not silent emptiness
                    // (contacts.md § Errors & edge cases; mirrors web's contacts-no-matches).
                    automationText(Ids.contactsNoMatches, L.contacts.noMatchingContacts)
                        .foregroundStyle(.secondary)
                }
                ForEach(groupedContacts, id: \.status) { status, rows in
                        // Heading uses the same shared label as the per-row badge
                        // so the two can't drift (contacts.md § Status badge text).
                        VStack(alignment: .leading, spacing: 8) {
                            Text(renderLocalizedText(contactStatusLabel(status: status)))
                                .font(.headline)
                            ForEach(rows) { row in
                                let contact = row.contact
                                HStack {
                                    // The nickname heads the row when one is set, the
                                    // public name it replaced stays beneath it, then
                                    // the person's labels as one line (contacts.md
                                    // § The private overlay; mirrors macOS
                                    // MacContactListView). The two secondary lines
                                    // are absent when there is nothing to show.
                                    VStack(alignment: .leading, spacing: 2) {
                                        automationText(Ids.contactName, row.name)
                                            .font(.caption.monospaced())
                                        if let publicName = row.publicName {
                                            automationText(Ids.contactPublicName, publicName)
                                                .font(.caption2)
                                                .foregroundStyle(.secondary)
                                        }
                                        if let labelsLine = row.labelsLine {
                                            automationText(Ids.contactLabels, labelsLine)
                                                .font(.caption2)
                                                .foregroundStyle(.secondary)
                                                .lineLimit(1)
                                        }
                                    }
                                    Button {
                                        Pasteboard.copy(contact.peerId)
                                    } label: {
                                        Image(systemName: "doc.on.doc")
                                            .font(.caption2)
                                    }
                                    .buttonStyle(.borderless)
                                    .accessibilityIdentifier(Ids.contactActorIdCopyBtn)
                                    // Same copy the Button runs (one indexed Entry per
                                    // contact row). Env-gated no-op in production.
                                    .automationActivate(Ids.contactActorIdCopyBtn) {
                                        Pasteboard.copy(contact.peerId)
                                    }
                                    Spacer()
                                    ContactStatusBadge(status: contact.status)
                                        .accessibilityIdentifier(Ids.contactStatus)
                                        // Read the status badge value (custom view, so
                                        // keep id + a read Entry) — the same localized
                                        // label the badge paints, never the raw wire
                                        // status (contacts.md § Status badge text). Env-gated no-op.
                                        .automationValue(Ids.contactStatus, text: { renderLocalizedText(contactStatusLabel(status: contact.status)) })
                                    // `contact-unattested-mark` — a post-succession
                                    // review flag (succession-aftermath.md
                                    // § Propagation), badge only, no Keep/Remove
                                    // pair here (those live on the permanent
                                    // Members To Review page). Reads the app-wide
                                    // CACHED roster, never a per-row fresh read.
                                    if contactIsUnderReview(
                                        personHex: contact.peerId,
                                        roster: client?.memberReviewRoster ?? []
                                    ) {
                                        automationText(Ids.contactUnattestedMark, L.contacts.unattestedMark)
                                            .font(.caption2)
                                            .foregroundStyle(.orange)
                                    }
                                    if contact.status == "accepted" {
                                        Button(L.common.confirm) {
                                            Task { await vm.confirmContact(peerId: contact.peerId) }
                                        }
                                        .controlSize(.small)
                                        .accessibilityIdentifier(Ids.contactConfirm)
                                        // Same confirm the Button runs (one indexed
                                        // Entry per accepted row). Env-gated no-op.
                                        .automationActivate(Ids.contactConfirm) {
                                            Task { await vm.confirmContact(peerId: contact.peerId) }
                                        }
                                    }
                                }
                                .contentShape(Rectangle())
                                .onTapGesture { openProfile(contact.peerId) }
                                .accessibilityElement(children: .contain)
                                .accessibilityIdentifier(Ids.contactRow)
                                // Presence anchor for the indexed contact row. Env-gated no-op.
                                .automationValue(Ids.contactRow, text: { shortId(hex: contact.peerId) })
                                // Tap-through to the peer's profile (profile.md
                                // § Layout & flow → Another's profile). The driver
                                // clicks the row by its full hex actor_id (mirrors
                                // linux's row widget-name), so register that id.
                                .automationActivate(contact.peerId) { openProfile(contact.peerId) }
                                // Outermost: every registration above — the row's
                                // own presence anchor included — records
                                // `contact-row[i]`, so the row's secondary lines
                                // resolve by containment. The index is flat across
                                // the status groups.
                                .automationScope(Ids.contactRow, index: row.index)
                            }
                        }
                    }
                }
                .padding()
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .overlay {
                if vm.isLoading {
                    ProgressView()
                }
            }
            .refreshable {
                await vm.refresh()
                appState.lastContacts = vm.contacts
                appState.lastKnocks = vm.knocks
            }
            }
            }
            .pageTitle(L.common.contacts)
            // Keyed on the session's client, not one-shot: iOS hosts this page in the
            // Contacts TAB, and a tab is never unmounted, so an account switch would
            // otherwise leave the outgoing account's roster and pending knocks on
            // screen — with every knock action still acting as that account, since
            // `ContactsVM` holds its `actorId` too. The nil-client phase drops both
            // (`ContactsVM.reset()`) and the incoming client rebuilds
            // (`account-scoping.md` § The scoping taxonomy, the "reused shell" case).
            // macOS unmounts the whole window shell on a switch, so there the reset is
            // redundant — carried for uniformity, as `SearchVM`'s is.
            //
            // The `appState.last*` mirrors below are NOT re-cleared here: they are on
            // `ActorScope`'s app-owned drop, and a second hand-list at this site is the
            // rot the corollary's first rule forbids.
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
            // Re-pull on an inbound `fauna.knock` push, so a contact request arriving
            // while this screen is mounted appears live with no navigation.
            .onKnockReceived {
                await vm.refresh()
                appState.lastContacts = vm.contacts
                appState.lastKnocks = vm.knocks
            }
            // A `search-result-item` Contact-arm deep link switches to the
            // Address Book segment (`AddressBookView` above does the actual
            // locate, once mounted) — `showAddressBook` is this view's own
            // local state, unreachable from Search directly. Mirrors macOS
            // `ContactSplitView`.
            .task(id: appState.pendingContactUidHash) {
                if appState.pendingContactUidHash != nil { showAddressBook = true }
            }
            // A `notification-item` Knock-arm deep link goes the other way: the
            // pending knocks render on the PEOPLE segment, and the segment is
            // sticky, so a user last left on the Address Book would land on a
            // page with no `knock-request-item` on it
            // (`behavior/notifications.md` § Deep-link destinations). Mirrors
            // macOS `ContactSplitView`.
            .task(id: appState.pendingKnockSenderId) {
                guard appState.pendingKnockSenderId != nil else { return }
                showAddressBook = false
                appState.pendingKnockSenderId = nil
            }
        }
        .accessibilityIdentifier(Ids.contactsView)
        // Presence anchor for `is_visible("contacts-view")` (mirrors macOS). Env-gated no-op.
        .automationValue(Ids.contactsView, text: { "" })
    }

    /// Open another actor's profile (the canonical per-user detail surface —
    /// profile.md § Relationship to Contacts): switch to the More-tab profile
    /// destination carrying the peer's actor_id. Mirrors linux `open_profile`.
    private func openProfile(_ actorId: String) {
        appState.profileActorId = actorId
        appState.selectedTab = "more"
        appState.moreSelectedView = "profile"
    }
}

// `ContactStatusBadge` is the shared apple-family capsule in FaunaKit (consumes
// `contactStatusLabel`); see FaunaKit/Sources/FaunaKit/Views/ContactStatusBadge.swift.
