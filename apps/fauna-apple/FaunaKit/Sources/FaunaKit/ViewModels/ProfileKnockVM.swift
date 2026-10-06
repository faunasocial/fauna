import Foundation

/// The OTHER-profile page's request-contact knock (`profile-request-contact-button`)
/// and its guardian ask — what `profile.md` § Layout & flow ratified and § Where logic
/// lives → *Request contact routing* routes. The profile is a CALLER of the contact
/// lifecycle (`fauna.inbox.send`), never a second implementation of it: the payload is
/// the shared `buildKnockPayload` writer behind ``APIClient/sendKnock``, and the route
/// is the shared ``APIClient/knockRoute(actorId:profileBody:)``.
///
/// **State is keyed on the peer, not on "the page's current open".** A knock's reply
/// (and its refusal) can outlive the profile open that issued it — the user navigates
/// on while `fauna.inbox.send` is in flight — so "Sent", the ask offer and the route
/// each belong to the actor they were issued for. A reply for someone else's earlier
/// open lands in its own slot and a page showing a different actor reads its own,
/// which is rule (g) by construction rather than by an epoch check (tui's
/// `Outcome::Knock { peer, .. }` guard, made structural).
///
/// The ask itself — the typed-refusal classification, the durable pending read — is
/// ``ContactAsk``, the same model the Contacts page's Find User result renders from.
@MainActor @Observable
public final class ProfileKnockVM {
    /// The page-level error (`error-message`): a knock that failed, including the
    /// guardian refusal — which stays here, because it is still a real failure, just
    /// not a dead end.
    public var errorMessage: String?

    /// The guardian ask for a refused knock.
    public let contactAsk = ContactAsk()

    /// Foreign home nests to route a knock to, by lowercased peer hex. Absent = same
    /// nest (local delivery). Only ever written from a profile body ABOUT that peer
    /// (the shared rule refuses anyone else's word), so a route cannot leak across
    /// two opens.
    private var routes: [String: String] = [:]
    /// Peers a knock to has landed, lowercased hex.
    private var sentPeers: Set<String> = []

    private var api: APIClient?
    private var actorId: String?
    private var familyStatus: FamilyStatusStore?

    public init() {}

    /// Drop everything for the account this page was scoped to — the ONE canonical
    /// drop (`account-scoping.md` § The scoping taxonomy, the in-memory corollary),
    /// called by ``configure(api:actorId:familyStatus:)`` on an api change and by the
    /// page's nil-client phase.
    public func reset() {
        api = nil
        actorId = nil
        familyStatus = nil
        errorMessage = nil
        routes = [:]
        sentPeers = []
        contactAsk.reset()
    }

    public func configure(api: APIClient, actorId: String, familyStatus: FamilyStatusStore?) {
        if let current = self.api, current !== api { reset() }
        self.api = api
        self.actorId = actorId
        self.familyStatus = familyStatus
    }

    /// Begin a fresh open of `peer`'s profile: the per-open transient state — the
    /// error and this peer's "sent"/route — starts clean, as a rebuilt page would.
    /// (Other peers' slots are left alone; they can only ever be read by their own
    /// page.)
    public func beginOpen(peer: String) {
        let key = Self.key(peer)
        errorMessage = nil
        routes[key] = nil
        sentPeers.remove(key)
    }

    /// Record the knock route from the profile `body` the page's open fetched for
    /// `peer` (`nil` route = same nest). Pure shared-Rust rule; no second fetch.
    public func noteProfile(body: Data, peer: String) {
        guard let api else { return }
        routes[Self.key(peer)] = api.knockRoute(actorId: peer, profileBody: body)
    }

    /// The route a knock to `peer` would take, for tests and the page's assertions.
    public func route(peer: String) -> String? { routes[Self.key(peer)] }

    /// Whether the knock to `peer` landed (the button flips to "Request sent").
    public func isSent(peer: String) -> Bool { sentPeers.contains(Self.key(peer)) }

    /// `profile-request-contact-button` — send the knock to `peer`
    /// (`fauna.inbox.send` via the shared writer), routed to the peer's home nest
    /// when the profile named a foreign one.
    public func knock(peer: String) async {
        guard let api, let actorId, !isSent(peer: peer) else { return }
        errorMessage = nil
        do {
            try await api.sendKnock(
                actorId: actorId, peerId: peer, recipientNestUrl: routes[Self.key(peer)])
            guard self.api === api else { return }   // the in-flight clause
            sentPeers.insert(Self.key(peer))
            errorMessage = nil
        } catch {
            guard self.api === api else { return }
            if contactAsk.noteKnockFailure(error, peer: peer) {
                // Clause (b): the refusal STAYS on `error-message` — the knock did
                // not happen — with the ask offered beside it.
                errorMessage = L.contacts.guardianApprovalRequired
            } else {
                errorMessage = DisplayError.http(error)
            }
        }
    }

    /// What the ask row shows for `peer` (``ContactAsk/state(peer:store:)``).
    public func askState(peer: String) -> ContactAsk.State {
        contactAsk.state(peer: peer, store: familyStatus)
    }

    /// The ward's ask (`contact-request-guardian-button`) for `peer`. Its own typed
    /// refusals are read verbatim, as on the Contacts page.
    public func askGuardian(peer: String) async {
        guard let api else { return }
        do {
            try await contactAsk.ask(peer: peer, api: api, store: familyStatus)
            guard self.api === api else { return }   // the in-flight clause
            errorMessage = nil
        } catch {
            guard self.api === api else { return }
            errorMessage = DisplayError.message(error)
        }
    }

    private static func key(_ peer: String) -> String { peer.lowercased() }
}
