import Foundation

/// The OTHER-profile page's request-contact knock (`profile-request-contact-button`)
/// and its guardian ask — what `profile.md` § Layout & flow ratified and § Where logic
/// lives → *Request contact routing* routes. The profile is a CALLER of the contact
/// lifecycle (`fauna.inbox.send`), never a second implementation of it: the payload is
/// the shared `buildKnockPayload` writer behind ``APIClient/sendKnock``, and the route
/// is the page's actor-home slot, the shared ``APIClient/actorHomeSlot(carried:actorId:profileBody:)``.
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

    /// The page's ACTOR-HOME slot by lowercased peer hex (`profile.md` § Where logic
    /// lives → *The actor's home*): foreign home nests the follow, the knock and the
    /// remote header read go to. Absent = this nest (local delivery). Only ever
    /// written through the shared `actor_home` rule, from the opener's carried home
    /// and a profile body ABOUT that peer (the rule refuses anyone else's word), so
    /// a home cannot leak across two opens.
    private var homes: [String: String] = [:]
    /// The home each peer's open was handed by its opener, kept so the slot's
    /// replacement after the header read goes through the rule with it.
    private var carrieds: [String: String] = [:]
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
        homes = [:]
        carrieds = [:]
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
    ///
    /// `carried` is the home the opener carried (a Find User result, an author line,
    /// a contact row, or the nav patch): the slot is SEEDED with it before any read,
    /// so the follow and the remote header read already have it.
    public func beginOpen(peer: String, carried: String? = nil) {
        let key = Self.key(peer)
        errorMessage = nil
        carrieds[key] = carried
        homes[key] = api?.actorHomeSlot(carried: carried, actorId: peer, profileBody: nil)
        sentPeers.remove(key)
    }

    /// Replace the actor-home slot from the profile `body` the page's open fetched
    /// for `peer` (`nil` = this nest). Pure shared-Rust rule (`actor_home`); no
    /// second fetch.
    public func noteProfile(body: Data, peer: String) {
        guard let api else { return }
        let key = Self.key(peer)
        homes[key] = api.actorHomeSlot(carried: carrieds[key], actorId: peer, profileBody: body)
    }

    /// The page's actor-home slot for `peer` — the nest the follow, the knock and
    /// the remote header read all go to (`nil` = this nest).
    public func home(peer: String) -> String? { homes[Self.key(peer)] }

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
                actorId: actorId, peerId: peer, recipientNestUrl: homes[Self.key(peer)])
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
