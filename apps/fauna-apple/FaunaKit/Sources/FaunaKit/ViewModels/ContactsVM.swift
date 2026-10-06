import SwiftUI

@MainActor @Observable
public class ContactsVM {
    public var knocks: [Knock] = []
    public var contacts: [Contact] = []
    public var findQuery = ""
    public var findResult: (actorId: String, handle: String?, domain: String?)?
    public var findError: String?
    public var isLoading = false
    public var errorMessage: String?

    /// The ward's in-app "ask your guardian" for a knock the nest refused for a
    /// guardian (`family-safety.md` § Child-initiated contact requests). Owned here
    /// because the Find User result is where the knock is sent from; the profile
    /// page renders the same model shape (`ProfileKnockVM`). Cleared with the rest
    /// of the account-scoped state by ``reset()``.
    public let contactAsk = ContactAsk()

    private var api: APIClient?
    private var actorId: String?
    /// The app-root `fauna.family.status` projection the durable "asked — waiting"
    /// state reads from. Optional: a host that has none renders the just-asked flag
    /// only.
    private var familyStatus: FamilyStatusStore?

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary: at the identity change itself, keyed on the identity, with no field
    /// hand-listed at each caller), on `SearchVM.reset()`'s shape. Called by
    /// ``configure(api:actorId:)`` on an api-identity change **before** it re-points,
    /// and by the Contacts tab's nil-client phase — iOS never unmounts a tab, so
    /// without it account A's roster stays rendered under account B
    /// .
    ///
    /// That is the social roster and the pending knocks themselves, the find-a-user
    /// query and whatever it resolved, and **both identity keys**. Dropping `actorId`
    /// matters as much as dropping the rows: every mutation here
    /// (``acceptKnock(peerId:)``, ``blockKnock(peerId:)``, ``dismissKnock(peerId:)``,
    /// ``confirmContact(peerId:)``, ``addContact(actorId:)``) passes it as the *acting*
    /// actor, so a surviving one would make the incoming account's taps act as the
    /// outgoing account — the isolation contract broken in the write direction, not
    /// just the read one.
    public func reset() {
        api = nil
        actorId = nil
        familyStatus = nil
        knocks = []
        contacts = []
        findQuery = ""
        findResult = nil
        findError = nil
        isLoading = false
        errorMessage = nil
        contactAsk.reset()
    }

    public func configure(api: APIClient, actorId: String, familyStatus: FamilyStatusStore? = nil) {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.actorId = actorId
        self.familyStatus = familyStatus
    }

    public func refresh() async {
        guard let api, let actorId else { return }
        isLoading = true
        defer { isLoading = false }
        errorMessage = nil

        do {
            async let k = api.fetchKnocks(actorId: actorId)
            async let c = api.fetchContacts(actorId: actorId)
            let (loadedKnocks, loadedContacts) = try await (k, c)
            // The in-flight clause (`account-scoping.md` § The scoping taxonomy):
            // cancelling is never sufficient — a read already suspended inside
            // `fetchKnocks`/`fetchContacts` for the outgoing account is not cancelled
            // mid-call, so it still returns and would still assign. `guard let api`
            // above shadowed `self.api` with the api this read was issued on, so this
            // compares the read's own identity against the VM's current one.
            guard self.api === api else { return }
            knocks = loadedKnocks
            contacts = loadedContacts
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func acceptKnock(peerId: String) async {
        guard let api, let actorId else { return }
        do {
            try await api.acceptKnock(actorId: actorId, peerId: peerId)
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func blockKnock(peerId: String) async {
        guard let api, let actorId else { return }
        do {
            try await api.blockKnock(actorId: actorId, peerId: peerId)
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func dismissKnock(peerId: String) async {
        guard let api, let actorId else { return }
        do {
            try await api.dismissKnock(actorId: actorId, peerId: peerId)
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func confirmContact(peerId: String) async {
        guard let api, let actorId else { return }
        do {
            try await api.confirmContact(actorId: actorId, peerId: peerId)
            guard self.api === api else { return }   // the in-flight clause
            await refresh()
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.http(error)
        }
    }

    public func findUser() async {
        guard let api else { return }
        let query = findQuery.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return }
        findResult = nil
        findError = nil

        do {
            // Parse + both network lookups run in shared Rust (classifyRecipient
            // → resolveNest → resolveHandle); no Swift `@`-split or raw-actor-id
            // fallback remains (priority #2/#4).
            let r = try await api.resolveRecipient(query)
            guard self.api === api else { return }   // the in-flight clause
            findResult = (actorId: r.actorId, handle: r.handle, domain: r.domain)
        } catch {
            guard self.api === api else { return }
            findError = DisplayError.http(error)
        }
    }

    /// The `contact-actor-id-lookup` Button/automation action (mac/iOS,
    /// priority #2 — was duplicated verbatim in both views before this lift):
    /// stage the view's `contact-actor-id-field` text as the query, then run
    /// the lookup.
    public func findUser(query: String) async {
        findQuery = query
        await findUser()
    }

    public func addContact(actorId: String) async {
        guard let api, let myId = self.actorId else { return }
        do {
            try await api.sendKnock(actorId: myId, peerId: actorId)
            guard self.api === api else { return }   // the in-flight clause
            findResult = nil
            await refresh()
        } catch {
            guard self.api === api else { return }
            if contactAsk.noteKnockFailure(error, peer: actorId) {
                // Clause (b): the refusal STAYS on `error-message`. It is still a
                // real failure — the knock did not happen — just no longer a dead
                // end: the ask is offered beside it. Silencing it because an ask is
                // now offered would make the page claim success.
                errorMessage = L.contacts.guardianApprovalRequired
            } else {
                errorMessage = DisplayError.http(error)
            }
        }
    }

    /// What the Find User result's ask row shows for `peerId`
    /// (``ContactAsk/state(peer:store:)``) — the ask button after a guardian-refused
    /// knock, the pending label once one is outstanding, nothing otherwise.
    public func askState(peerId: String) -> ContactAsk.State {
        contactAsk.state(peer: peerId, store: familyStatus)
    }

    /// The ward's ask (`contact-request-guardian-button`): `fauna.family.contact.
    /// request` for `peerId`, then a re-read of the ward's own asks. Its own typed
    /// refusals are the ward's to read verbatim (the guardian gate's error would
    /// mislead here), so they go through ``DisplayError/message(_:)``, not the
    /// knock's fixed sentence.
    public func askGuardian(peerId: String) async {
        guard let api else { return }
        do {
            try await contactAsk.ask(peer: peerId, api: api, store: familyStatus)
            guard self.api === api else { return }   // the in-flight clause
            errorMessage = nil
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    public func contactsByStatus(_ status: String) -> [Contact] {
        contacts.filter { $0.status == status }
    }

    /// One roster row as both apple targets paint it: the contact, its position
    /// in the roster, and what the viewer calls the person.
    public struct RosterRow: Identifiable {
        public let contact: Contact
        /// The row's position across **every** status group, in display order —
        /// the `contact-row[i]` automation scope index. The rows are nested in a
        /// `ForEach` over the groups, so a per-group offset would repeat.
        public let index: Int
        /// `contact-name`: the nickname when one is set, else the public name.
        public let name: String
        /// `contact-public-name`: the public name a nickname replaced; `nil`
        /// (element absent) when there is no nickname.
        public let publicName: String?
        /// `contact-labels`: the person's labels as one line; `nil` (element
        /// absent) when they carry none.
        public let labelsLine: String?

        public var id: String { contact.peerId }
    }

    /// The roster (`docs/goal/ui/contacts.md` § Contact roster filter, § The
    /// private overlay → *Labels and the roster filter*): the contacts grouped
    /// by status in `ContactStatusBadge.displayOrder`, narrowed by the
    /// `contacts-search-field` query, each row carrying its names. One door for
    /// macOS and iOS, so neither the grouping nor the flat index can diverge.
    ///
    /// The filter, the names and the label line are all the shared projection's
    /// (`overlays` — ``ConversationsVM/contactOverlays``): the query matches
    /// handle, domain and actor id plus the viewer's nickname and labels for
    /// that person. Local-only, never a nest query.
    public func rosterGroups(
        searchFilter: String, overlays: FfiContactOverlays
    ) -> [(status: String, rows: [RosterRow])] {
        var index = 0
        return ContactStatusBadge.displayOrder.compactMap { status in
            let rows = contactsByStatus(status)
                .filter {
                    overlays.matchesFilter(query: searchFilter, handle: $0.handle,
                                           domain: $0.domain, actorId: $0.peerId)
                }
                .map { contact -> RosterRow in
                    let label = overlays.peerLabel(
                        displayName: nil, handle: contact.handle, actorId: contact.peerId)
                    defer { index += 1 }
                    return RosterRow(
                        contact: contact, index: index, name: label.primary,
                        publicName: label.public,
                        labelsLine: overlays.labelsLine(actorId: contact.peerId))
                }
            return rows.isEmpty ? nil : (status, rows)
        }
    }
}
