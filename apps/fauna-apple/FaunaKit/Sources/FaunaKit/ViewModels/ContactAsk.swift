import Foundation

/// The ward's in-app "ask your guardian" for a refused knock — the one model the
/// Contacts page's Find User result and the profile page's request-contact button
/// both render from (`family-safety.md` § Child-initiated contact requests → *App
/// affordance*), so the guardian gate is told apart from every other failure in
/// exactly one place. tui's `KnockSend` / `ContactsState::guardian_refused` is the
/// reference shape.
///
/// **Four rules, each the reason a member is shaped as it is:**
/// - **The ask is offered only on the TYPED refusal.** ``noteKnockFailure(_:peer:)``
///   records a peer only for ``FfiError/GuardianApprovalRequired(msg:)``, the
///   shared `RpcError::is_guardian_approval_required` verdict carried across the
///   boundary. Offering it on any other failure would tell an unsupervised user
///   their account is supervised.
/// - **Keyed on the peer, never on a compose buffer.** A knock's refusal comes
///   back after the form may already have cleared, and after the page may already
///   be showing someone else — so the refusal and the ask both belong to the peer
///   they were issued for, and a reply that outlives its lookup cannot offer the
///   ask (or paint "Asked") on the next actor.
/// - **Pending is durable first.** ``state(peer:store:)`` reads
///   `FamilyStatusStore.contactRequests` (which survives navigation and a restart,
///   and is honest on a fresh session that never saw the refusal); the just-asked
///   set only makes the render answer before the re-read lands.
/// - **No supervision test at render.** The inputs are supervised-only by
///   construction — the store gates its list on `supervisedBy`, and a refusal
///   arrives only from a supervised account — so an unsupervised account cannot
///   reach either branch, and this cannot drift out of step with what the nest
///   enforces.
@MainActor @Observable
public final class ContactAsk {
    /// What the ask row shows for one peer.
    public enum State: Equatable, Sendable {
        /// Nothing to offer and nothing outstanding — the common case.
        case none
        /// The nest refused the knock for a guardian; the ask button shows.
        case offered
        /// An ask is outstanding; the pending label shows instead of the button.
        case pending
    }

    /// Peers whose knock the nest refused for a guardian, lowercased hex.
    private var refusedPeers: Set<String> = []
    /// Peers this session just asked about — the durable half is the store's list.
    private var askedPeers: Set<String> = []

    public init() {}

    /// Drop everything — the account-scope drop for whichever page owns this.
    public func reset() {
        refusedPeers = []
        askedPeers = []
    }

    /// Classify one knock failure. Returns `true` when it was the nest's TYPED
    /// guardian-approval refusal (and records `peer` so the ask is offered for
    /// them), `false` for every other failure — which the caller renders as the
    /// ordinary error it always was.
    @discardableResult
    public func noteKnockFailure(_ error: Error, peer: String) -> Bool {
        guard let ffi = error as? FfiError, case .GuardianApprovalRequired = ffi else { return false }
        refusedPeers.insert(Self.key(peer))
        return true
    }

    /// The ask row's state for `peer`. Reads `store` inside the call, so a view
    /// that calls it from `body` re-renders when the durable list changes.
    public func state(peer: String, store: FamilyStatusStore?) -> State {
        let key = Self.key(peer)
        if askedPeers.contains(key) || store?.contactAskPending(peerActorIdHex: peer) == true {
            return .pending
        }
        return refusedPeers.contains(key) ? .offered : .none
    }

    /// Send the ask (`fauna.family.contact.request`) and re-read the ward's own asks
    /// so what paints is what the NEST holds rather than this session's memory of
    /// having asked. A failed re-read is not a failed ask — the guardian has been
    /// rung — so `store.refresh` swallows its own error and the just-asked set
    /// carries the render.
    public func ask(peer: String, api: APIClient, store: FamilyStatusStore?) async throws {
        try await api.requestContact(peerId: peer)
        askedPeers.insert(Self.key(peer))
        await store?.refresh(api: api)
    }

    private static func key(_ peer: String) -> String { peer.lowercased() }
}
