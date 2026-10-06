import SwiftUI
import FaunaKit

struct MacContactListView: View {
    let vm: ContactsVM
    @Binding var selectedPeerId: String?
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    /// The owner of the private-overlay projection every name on this page is
    /// read through (`contacts.md` § The private overlay).
    @Environment(ConversationsVM.self) private var conversationsVM

    @State private var findQuery = ""
    @State private var searchFilter = ""

    var body: some View {
        let overlays = conversationsVM.contactOverlays
        List(selection: $selectedPeerId) {
            // Contact roster filter (`contacts-search-field`) — local,
            // case-insensitive substring over the already-loaded contacts; the
            // `common.search` placeholder matches web. Narrows the status groups
            // below via the shared roster filter (`ContactsVM.rosterGroups`).
            Section {
                TextField(L.common.search, text: $searchFilter)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityIdentifier(Ids.contactsSearchField)
                    // Writes/reads the same `$searchFilter` binding. Env-gated no-op.
                    .automationField(Ids.contactsSearchField, text: $searchFilter)
            }

            // Find User
            Section(L.contacts.findUser.title) {
                HStack {
                    TextField(L.contacts.findUser.placeholder, text: $findQuery)
                        .textFieldStyle(.roundedBorder)
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
                                .help(result.actorId)
                                .accessibilityIdentifier(Ids.contactActorIdResult)
                                // Reads the resolved actor id (mirrors iOS). Env-gated no-op.
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
                        // Same add the Button runs. Env-gated no-op.
                        .automationActivate(Ids.contactsAddButton) { Task { await vm.addContact(actorId: result.actorId) } }
                    }
                    .padding(.vertical, 2)
                    .tag(result.actorId)
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
                        // registers `error-message`; this is the find wrapper id).
                        // Env-gated no-op in production.
                        .automationValue(Ids.contactFindError, text: { error })
                }
            }

            // Message Requests
            if !vm.knocks.isEmpty {
                Section(L.contacts.messageRequests.count(count: String(vm.knocks.count))) {
                    ForEach(vm.knocks) { knock in
                        VStack(alignment: .leading, spacing: 4) {
                            HStack {
                                // The viewer's nickname for the sender when one is
                                // set, else the canonical short id (a knock
                                // carries no public name).
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
                        .tag(knock.sender)
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
                Section {
                    automationText(Ids.contactsNoMatches, L.contacts.noMatchingContacts)
                        .foregroundStyle(.secondary)
                }
            }
            ForEach(groupedContacts, id: \.status) { status, rows in
                    // Heading uses the same shared label as the per-row badge
                    // so the two can't drift (contacts.md § Status badge text).
                    Section(renderLocalizedText(contactStatusLabel(status: status))) {
                        ForEach(rows) { row in
                            let contact = row.contact
                            HStack {
                                // The nickname heads the row when one is set, the
                                // public name it replaced stays beneath it, then
                                // the person's labels as one line (contacts.md
                                // § The private overlay). The two secondary lines
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
                            .tag(contact.peerId)
                            .accessibilityIdentifier(Ids.contactRow)
                            // Presence anchor for the indexed contact row. Env-gated no-op.
                            .automationValue(Ids.contactRow, text: { shortId(hex: contact.peerId) })
                            // Tap-through to the peer's profile (the canonical
                            // per-user detail surface — profile.md § Relationship to
                            // Contacts). The driver clicks the row by its full hex
                            // actor_id (mirrors linux's row widget-name). (Real
                            // mouse selection still drives the split-view detail
                            // pane; converging that to the profile is a contacts
                            // follow-on.)
                            .automationActivate(contact.peerId) {
                                appState.openProfile(contact.peerId)
                            }
                            // Outermost: every registration above — the row's own
                            // presence anchor included — records `contact-row[i]`,
                            // so the row's secondary lines resolve by containment.
                            // The index is flat across the status groups.
                            .automationScope(Ids.contactRow, index: row.index)
                        }
                    }
            }
        }
        .listStyle(.sidebar)
        .frame(minWidth: 280)
        .overlay {
            if vm.isLoading {
                ProgressView()
            }
        }
        .toolbar {
            ToolbarItem {
                Button(action: { Task { await vm.refresh() } }) {
                    Image(systemName: "arrow.clockwise")
                }
            }
        }
    }
}

// `ContactStatusBadge` is the shared apple-family capsule in FaunaKit (consumes
// `contactStatusLabel`); see FaunaKit/Sources/FaunaKit/Views/ContactStatusBadge.swift.
