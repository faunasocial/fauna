import Foundation
import Testing
@testable import FaunaKit

// The four family surfaces tui leads and macOS + iOS lift through shared FaunaKit
// (`family-safety.md` §§ Child-initiated contact requests → *App affordance*,
// Feed-source approvals, The bridge-DM gate → *The un-deny surface*;
// `profile.md` § Where logic lives → *Request contact routing*).
//
// The rules tui found, and which of them each test pins:
//   (a) the ask is offered ONLY on the typed refusal
//   (b) the refusal stays on `error-message`
//   (c) pending is durable (the store's list, gated on `supervisedBy`); a local
//       just-asked flag only makes the render answer immediately
//   (d) rows are keyed on the ask data, never on a compose buffer
//   (e) an approved feed-source ask is a PROMPT to retry, never an auto-retry
//   (f) each allow button addresses ITS OWN row — pinned by the e2e journey, since a
//       SwiftUI button closure is not reachable from a unit test; `allowBlockedPeer`
//       takes the row's record WHOLE so there is no second lookup to get wrong
//   (g) a knock reply carries the peer it was sent to
//   (h) supervision is not re-tested at render
//
// A real call dials the nest, which a unit test must not, so the APIClient's family
// and knock methods are scripted.

// MARK: - Doubles and fixtures

private let peerA = String(repeating: "aa", count: 32)
private let peerB = String(repeating: "bb", count: 32)

/// The typed refusal exactly as `stringify` hands it across the boundary.
private let guardianRefusal = FfiError.GuardianApprovalRequired(msg: "a guardian must approve")

private final class AskAPI: APIClient {
    var knockError: Error?
    private(set) var knocks: [(peer: String, route: String?)] = []
    private(set) var contactAsks: [String] = []
    private(set) var feedAsks: [(bridge: String, op: FfiFeedSourceOperation, target: String, label: String)] = []
    /// What `familyStatus()` answers; `nil` fails the read (a re-read that fails).
    var status: FfiFamilyStatus?
    var routes: [String: String] = [:]
    var linkError: Error?
    var followError: Error?
    private(set) var followCalls = 0

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func sendKnock(actorId: String, peerId: String, recipientNestUrl: String?) async throws {
        knocks.append((peerId, recipientNestUrl))
        if let knockError { throw knockError }
    }

    override func knockRoute(actorId: String, profileBody: Data) -> String? { routes[actorId] }

    override func requestContact(peerId: String) async throws { contactAsks.append(peerId) }

    override func requestFeedSource(
        bridgeId: String, operation: FfiFeedSourceOperation, target: String, label: String
    ) async throws {
        feedAsks.append((bridgeId, operation, target, label))
    }

    override func familyStatus() async throws -> FfiFamilyStatus {
        guard let status else { throw ScopeTestFailure(account: "status read") }
        return status
    }

    override func linkBridge(bridgeId: String, mode: String, fields: [String: String]) async throws -> BridgeLinkResponse {
        throw linkError ?? ScopeTestFailure(account: "link")
    }

    override func addBridgeFollow(bridgeId: String, followId: String, petname: String?) async throws {
        followCalls += 1
        if let followError { throw followError }
    }
}

private func guardian() -> FfiFamilyGuardianInfo {
    FfiFamilyGuardianInfo(actorId: Data(repeating: 0xAB, count: 32), handle: "alex")
}

private func contactRequest(_ peerHex: String) -> FfiFamilyContactRequest {
    FfiFamilyContactRequest(peerActorId: Data(hexString: peerHex)!, peerHandle: "", createdAt: 0)
}

private func feedRequest(
    _ bridge: String, _ op: FfiFeedSourceOperation, _ target: String, approved: Bool = false
) -> FfiFamilyFeedRequest {
    FfiFamilyFeedRequest(
        bridgeId: bridge, operation: feedSourceOperationWire(operation: op), target: target,
        label: "", createdAt: 0, approvedAt: approved ? 1 : nil)
}

private func status(
    supervised: Bool = true,
    contactAsks: [FfiFamilyContactRequest] = [],
    feedAsks: [FfiFamilyFeedRequest] = []
) -> FfiFamilyStatus {
    FfiFamilyStatus(
        supervisedBy: supervised ? guardian() : nil, policy: nil, wards: [], incomingTransfers: [],
        usageTodayMinutes: nil, contactRequests: contactAsks, feedRequests: feedAsks,
        ageBand: nil, supervision: nil)
}

/// A store that has read `reply` off a scripted API.
@MainActor
private func store(reading reply: FfiFamilyStatus) async -> FamilyStatusStore {
    let api = AskAPI()
    api.status = reply
    let store = FamilyStatusStore()
    await store.refresh(api: api)
    return store
}

// MARK: - The store's durable asks (rule (c), (h))

@Suite("FamilyStatusStore — the ward's durable asks")
@MainActor
struct FamilyStatusStoreAskTests {
    @Test("a supervised read carries the contact asks, matched case-insensitively per peer")
    func contactAsksMatchPerPeer() async {
        let store = await store(reading: status(contactAsks: [contactRequest(peerA)]))
        #expect(store.contactAskPending(peerActorIdHex: peerA))
        #expect(store.contactAskPending(peerActorIdHex: peerA.uppercased()),
                "the caller's id is whatever the form resolved; the wire's is lowercase")
        #expect(!store.contactAskPending(peerActorIdHex: peerB), "an ask names ONE peer")
        #expect(!store.contactAskPending(peerActorIdHex: "not hex"), "a non-id matches nothing")
    }

    @Test("a reply naming no guardian drops both lists — a graduated account has nobody to wait on")
    func asksAreGatedOnSupervision() async {
        let store = await store(reading: status(
            supervised: false, contactAsks: [contactRequest(peerA)],
            feedAsks: [feedRequest("bluesky", .link, "")]))
        #expect(!store.contactAskPending(peerActorIdHex: peerA),
                "a stale ask must not keep saying 'waiting' on a page that now sends freely")
        #expect(store.feedRequestState(bridgeId: "bluesky", operation: "link", target: "") == nil)
    }

    @Test("a failed re-read keeps what the last live read said")
    func aFailedRereadKeepsTheAsks() async {
        let api = AskAPI()
        api.status = status(contactAsks: [contactRequest(peerA)])
        let store = FamilyStatusStore()
        await store.refresh(api: api)
        api.status = nil
        await store.refresh(api: api)
        #expect(store.contactAskPending(peerActorIdHex: peerA),
                "a failed read is not a withdrawn ask")
    }

    @Test("a nil client clears them with the rest of the account's state")
    func aNilClientClearsTheAsks() async {
        let store = await store(reading: status(contactAsks: [contactRequest(peerA)]))
        await store.refresh(api: nil)
        #expect(!store.contactAskPending(peerActorIdHex: peerA))
    }

    @Test("a feed ask reads pending until it carries an approval instant, keyed on the whole triple")
    func feedStateIsKeyedOnTheTriple() async {
        let store = await store(reading: status(feedAsks: [
            feedRequest("bluesky", .follow, "did:plc:one"),
            feedRequest("bluesky", .follow, "did:plc:two", approved: true),
        ]))
        #expect(store.feedRequestState(bridgeId: "bluesky", operation: "follow", target: "did:plc:one") == .pending)
        #expect(store.feedRequestState(bridgeId: "bluesky", operation: "follow", target: "did:plc:two") == .approved)
        #expect(store.feedRequestState(bridgeId: "bluesky", operation: "follow", target: "did:plc:three") == nil,
                "a different follow on the same bridge is a different ask")
        #expect(store.feedRequestState(bridgeId: "bluesky", operation: "link", target: "") == nil,
                "nor is a link ask a follow ask")
        #expect(store.feedRequestStates(bridgeId: "bluesky") == [.pending, .approved])
        #expect(store.feedRequestStates(bridgeId: "other").isEmpty)
    }
}

// MARK: - ContactAsk (rules (a), (c), (d), (g))

@Suite("ContactAsk — the ask is offered only on the typed refusal")
@MainActor
struct ContactAskTests {
    @Test("the typed refusal offers the ask for THAT peer and no other")
    func theTypedRefusalOffersTheAskForThatPeerOnly() {
        let ask = ContactAsk()
        #expect(ask.noteKnockFailure(guardianRefusal, peer: peerA))
        #expect(ask.state(peer: peerA, store: nil) == .offered)
        #expect(ask.state(peer: peerB, store: nil) == .none,
                "a refusal outliving its lookup must not offer the ask on the next actor")
        #expect(ask.state(peer: peerA.uppercased(), store: nil) == .offered)
    }

    @Test("every other failure leaves the ask unoffered")
    func otherFailuresNeverOfferTheAsk() {
        let ask = ContactAsk()
        let others: [Error] = [
            FfiError.General(msg: "boom"), FfiError.NestOutdated(msg: "update"),
            URLError(.notConnectedToInternet), ScopeTestFailure(account: "x"),
        ]
        for error in others {
            #expect(!ask.noteKnockFailure(error, peer: peerA), "\(error) must not read as a guardian gate")
        }
        #expect(ask.state(peer: peerA, store: nil) == .none,
                "offering it on any other failure tells an unsupervised user their account is supervised")
    }

    @Test("a durable ask reads pending on a fresh session that never saw the refusal")
    func pendingIsDurableFirst() async {
        let ask = ContactAsk()
        let store = await store(reading: status(contactAsks: [contactRequest(peerA)]))
        #expect(ask.state(peer: peerA, store: store) == .pending)
        #expect(ask.state(peer: peerB, store: store) == .none)
    }

    @Test("asking sends the request, re-reads, and renders pending even if the re-read fails")
    func askingRendersPendingWithoutTheRereadLanding() async throws {
        let api = AskAPI()   // no status scripted: the re-read throws
        let ask = ContactAsk()
        let store = FamilyStatusStore()
        ask.noteKnockFailure(guardianRefusal, peer: peerA)
        try await ask.ask(peer: peerA, api: api, store: store)
        #expect(api.contactAsks == [peerA])
        #expect(ask.state(peer: peerA, store: store) == .pending,
                "a failed re-read is not a failed ask — the guardian has been rung")
    }

    @Test("reset drops every refusal and ask")
    func resetDropsEverything() async throws {
        let ask = ContactAsk()
        ask.noteKnockFailure(guardianRefusal, peer: peerA)
        try await ask.ask(peer: peerB, api: AskAPI(), store: nil)
        ask.reset()
        #expect(ask.state(peer: peerA, store: nil) == .none)
        #expect(ask.state(peer: peerB, store: nil) == .none)
    }
}

// MARK: - ContactsVM (the Find User result)

@Suite("ContactsVM — the guardian ask on the Find User result")
@MainActor
struct ContactsVMGuardianAskTests {
    @Test("a guardian-refused knock keeps its refusal on error-message AND offers the ask")
    func aRefusedKnockOffersTheAsk() async {
        let api = AskAPI()
        api.knockError = guardianRefusal
        let vm = ContactsVM()
        vm.configure(api: api, actorId: "me")
        await vm.addContact(actorId: peerA)
        #expect(vm.errorMessage == L.contacts.guardianApprovalRequired,
                "clause (b): still a real failure, just not a dead end")
        #expect(vm.askState(peerId: peerA) == .offered)
        #expect(vm.askState(peerId: peerB) == .none)
    }

    @Test("any other failed knock is the ordinary error and offers nothing")
    func anotherFailureOffersNothing() async {
        let api = AskAPI()
        api.knockError = FfiError.General(msg: "offline")
        let vm = ContactsVM()
        vm.configure(api: api, actorId: "me")
        await vm.addContact(actorId: peerA)
        #expect(vm.errorMessage != nil && vm.errorMessage != L.contacts.guardianApprovalRequired)
        #expect(vm.askState(peerId: peerA) == .none)
    }

    @Test("the ask rings the guardian, clears the error, and reads pending")
    func askingReadsPending() async {
        let api = AskAPI()
        api.knockError = guardianRefusal
        let vm = ContactsVM()
        vm.configure(api: api, actorId: "me", familyStatus: FamilyStatusStore())
        await vm.addContact(actorId: peerA)
        await vm.askGuardian(peerId: peerA)
        #expect(api.contactAsks == [peerA])
        #expect(vm.errorMessage == nil)
        #expect(vm.askState(peerId: peerA) == .pending)
    }

    @Test("a durable ask shows pending with no refusal ever seen this session")
    func pendingFromTheDurableList() async {
        let vm = ContactsVM()
        vm.configure(
            api: AskAPI(), actorId: "me",
            familyStatus: await store(reading: status(contactAsks: [contactRequest(peerA)])))
        #expect(vm.askState(peerId: peerA) == .pending)
    }

    @Test("a switch drops the refusals and asks with the rest of the account's state")
    func aSwitchDropsTheAsk() async {
        let api = AskAPI()
        api.knockError = guardianRefusal
        let vm = ContactsVM()
        vm.configure(api: api, actorId: "me")
        await vm.addContact(actorId: peerA)
        vm.configure(api: AskAPI(), actorId: "other")
        #expect(vm.askState(peerId: peerA) == .none)
    }
}

// MARK: - ProfileKnockVM (rules (a), (b), (g) and the route)

@Suite("ProfileKnockVM — the profile page's knock and its ask")
@MainActor
struct ProfileKnockVMTests {
    private func configured(_ api: AskAPI, store: FamilyStatusStore? = nil) -> ProfileKnockVM {
        let vm = ProfileKnockVM()
        vm.configure(api: api, actorId: "me", familyStatus: store)
        return vm
    }

    @Test("the knock goes to the peer's home nest when the profile named a foreign one")
    func theKnockIsRouted() async {
        let api = AskAPI()
        api.routes[peerA] = "https://peer.example:9000/"
        let vm = configured(api)
        vm.beginOpen(peer: peerA)
        vm.noteProfile(body: Data([1]), peer: peerA)
        await vm.knock(peer: peerA)
        #expect(api.knocks.map { $0.route } == ["https://peer.example:9000/"])
        #expect(vm.isSent(peer: peerA))
    }

    @Test("a peer whose profile names no foreign nest is knocked locally")
    func aLocalPeerIsKnockedLocally() async {
        let api = AskAPI()
        let vm = configured(api)
        vm.beginOpen(peer: peerA)
        vm.noteProfile(body: Data([1]), peer: peerA)
        await vm.knock(peer: peerA)
        #expect(api.knocks.count == 1 && api.knocks[0].route == nil)
    }

    @Test("a route belongs to the peer whose profile produced it")
    func aRouteIsKeyedOnThePeer() async {
        let api = AskAPI()
        api.routes[peerA] = "https://a.example"
        let vm = configured(api)
        vm.noteProfile(body: Data([1]), peer: peerA)
        vm.beginOpen(peer: peerB)
        await vm.knock(peer: peerB)
        #expect(api.knocks.map { $0.route } == [nil], "B must not inherit A's route")
    }

    @Test("a sent knock, and a refusal, belong to the peer they were issued for")
    func repliesAreKeyedOnThePeer() async {
        let api = AskAPI()
        let vm = configured(api)
        await vm.knock(peer: peerA)
        #expect(vm.isSent(peer: peerA))
        #expect(!vm.isSent(peer: peerB), "rule (g): a reply must not paint 'Sent' on the next actor")

        api.knockError = guardianRefusal
        await vm.knock(peer: peerB)
        #expect(vm.askState(peer: peerB) == .offered)
        #expect(vm.askState(peer: peerA) == .none, "…nor offer the ask for someone else")
    }

    @Test("the guardian refusal stays on error-message and only the typed one offers the ask")
    func theRefusalStaysAndOnlyTheTypedOneOffers() async {
        let api = AskAPI()
        let vm = configured(api)
        api.knockError = FfiError.General(msg: "offline")
        await vm.knock(peer: peerA)
        #expect(vm.askState(peer: peerA) == .none)
        #expect(!vm.isSent(peer: peerA))

        api.knockError = guardianRefusal
        await vm.knock(peer: peerA)
        #expect(vm.errorMessage == L.contacts.guardianApprovalRequired)
        #expect(vm.askState(peer: peerA) == .offered)
    }

    @Test("a sent knock cannot be sent twice")
    func aSentKnockIsNotResent() async {
        let api = AskAPI()
        let vm = configured(api)
        await vm.knock(peer: peerA)
        await vm.knock(peer: peerA)
        #expect(api.knocks.count == 1)
    }

    @Test("beginOpen starts the peer's open clean; reset drops the account's state")
    func aFreshOpenStartsClean() async {
        let api = AskAPI()
        let vm = configured(api)
        await vm.knock(peer: peerA)
        vm.beginOpen(peer: peerA)
        #expect(!vm.isSent(peer: peerA))
        api.knockError = guardianRefusal
        await vm.knock(peer: peerA)
        vm.reset()
        #expect(vm.askState(peer: peerA) == .none)
        #expect(vm.errorMessage == nil)
    }

    @Test("the ask rings the guardian and reads pending, with the durable list winning")
    func theAskReadsPending() async {
        let api = AskAPI()
        api.knockError = guardianRefusal
        let vm = configured(api, store: FamilyStatusStore())
        await vm.knock(peer: peerA)
        await vm.askGuardian(peer: peerA)
        #expect(api.contactAsks == [peerA])
        #expect(vm.askState(peer: peerA) == .pending)
        #expect(vm.errorMessage == nil)
    }
}

// MARK: - BridgeManagerVM (the feed-source ask; rules (a), (b), (d), (e))

@Suite("BridgeManagerVM — the feed-source ask")
@MainActor
struct BridgeManagerVMSourceAskTests {
    private func bridge(_ id: String = "bluesky") -> BridgeInfo {
        BridgeInfo(
            id: id, name: "Bluesky", available: true, linked: false, identity: nil, mode: nil,
            settings: [], supportsFollows: true,
            linkModes: [BridgeLinkMode(mode: "password", label: "Password", clientAction: nil,
                                       platform: nil, fields: [])],
            error: nil)
    }

    private func configured(_ api: AskAPI, store: FamilyStatusStore? = nil) -> BridgeManagerVM {
        let vm = BridgeManagerVM()
        vm.configure(api: api, familyStatus: store)
        return vm
    }

    @Test("a guardian-refused link offers the ask with an EMPTY target, and the refusal stays")
    func aRefusedLinkOffersTheAsk() async {
        let api = AskAPI()
        api.linkError = guardianRefusal
        let vm = configured(api)
        await vm.link(bridge: bridge())
        #expect(vm.errorMessage == L.bridges.sourceBlocked, "clause (b)")
        #expect(vm.sourceAskOffers(bridgeId: "bluesky")
            == [BridgeManagerVM.SourceKey(bridgeId: "bluesky", operation: .link, target: "")])
        #expect(vm.sourceAskOffers(bridgeId: "other").isEmpty)
    }

    @Test("a refused follow is keyed on the follow's own id, never on the form buffer")
    func aRefusedFollowIsKeyedOnTheAskData() async {
        let api = AskAPI()
        api.followError = guardianRefusal
        let vm = configured(api)
        vm.followId = "did:plc:refused"
        await vm.addFollow(bridgeId: "bluesky")
        // The buffer moves on (a success elsewhere clears it; the user retypes) —
        // the row must survive, or a buffer-keyed row would never paint.
        vm.followId = ""
        #expect(vm.sourceAskOffers(bridgeId: "bluesky")
            == [BridgeManagerVM.SourceKey(bridgeId: "bluesky", operation: .follow, target: "did:plc:refused")])
    }

    @Test("any other failure offers nothing")
    func otherFailuresOfferNothing() async {
        let api = AskAPI()
        api.linkError = FfiError.General(msg: "offline")
        api.followError = URLError(.timedOut)
        let vm = configured(api)
        await vm.link(bridge: bridge())
        vm.followId = "did:plc:x"
        await vm.addFollow(bridgeId: "bluesky")
        #expect(vm.sourceAskOffers(bridgeId: "bluesky").isEmpty,
                "offering the ask on a transport failure tells an unsupervised user their account is supervised")
        #expect(vm.errorMessage != L.bridges.sourceBlocked)
    }

    @Test("asking names the shared wire operation, reads pending, and stops offering the button")
    func askingReadsPending() async {
        let api = AskAPI()
        api.followError = guardianRefusal
        let vm = configured(api, store: FamilyStatusStore())
        vm.followId = "did:plc:refused"
        await vm.addFollow(bridgeId: "bluesky")
        let source = vm.sourceAskOffers(bridgeId: "bluesky")[0]
        await vm.requestSource(source, label: "Bluesky")
        #expect(api.feedAsks.count == 1)
        #expect(api.feedAsks[0].bridge == "bluesky" && api.feedAsks[0].op == .follow
                && api.feedAsks[0].target == "did:plc:refused" && api.feedAsks[0].label == "Bluesky")
        #expect(vm.errorMessage == nil)
        #expect(vm.sourceAskOffers(bridgeId: "bluesky").isEmpty)
        #expect(vm.sourceAskStates(bridgeId: "bluesky") == [.pending],
                "a failed re-read is not a failed ask — the local flag carries the render")
    }

    @Test("an approved ask is a prompt to retry, never an auto-retry")
    func anApprovedAskIsNotRetried() async {
        let api = AskAPI()
        api.followError = guardianRefusal
        let vm = configured(api, store: FamilyStatusStore())
        vm.followId = "did:plc:refused"
        await vm.addFollow(bridgeId: "bluesky")
        let source = vm.sourceAskOffers(bridgeId: "bluesky")[0]

        // The guardian approves: the next status read carries the grant.
        api.status = status(feedAsks: [feedRequest("bluesky", .follow, "did:plc:refused", approved: true)])
        await vm.requestSource(source, label: "Bluesky")

        #expect(vm.sourceAskStates(bridgeId: "bluesky") == [.approved])
        #expect(vm.sourceAskOffers(bridgeId: "bluesky").isEmpty, "an answered ask shows its verdict, not the button")
        #expect(api.followCalls == 1,
                "the grant is single-use: spending it on a render the user did not ask for would burn it")
    }

    @Test("a durable ask on a fresh session shows its state with no refusal seen")
    func aDurableAskShowsItsState() async {
        let api = AskAPI()
        let vm = configured(api, store: await store(reading: status(feedAsks: [
            feedRequest("bluesky", .link, ""),
        ])))
        #expect(vm.sourceAskStates(bridgeId: "bluesky") == [.pending])
        #expect(vm.sourceAskStates(bridgeId: "other").isEmpty)
    }

    @Test("an ask for one follow does not light up a different refused follow")
    func theTripleScopesTheAsk() async {
        let api = AskAPI()
        api.followError = guardianRefusal
        let vm = configured(api, store: await store(reading: status(feedAsks: [
            feedRequest("bluesky", .follow, "did:plc:one"),
        ])))
        vm.followId = "did:plc:one"
        await vm.addFollow(bridgeId: "bluesky")
        vm.followId = "did:plc:two"
        await vm.addFollow(bridgeId: "bluesky")
        #expect(vm.sourceAskOffers(bridgeId: "bluesky")
            == [BridgeManagerVM.SourceKey(bridgeId: "bluesky", operation: .follow, target: "did:plc:two")],
                "only the follow with no ask is offered the button")
    }

    @Test("reset drops the refusals and asks with the rest of the account's state")
    func resetDropsTheAsks() async {
        let api = AskAPI()
        api.linkError = guardianRefusal
        let vm = configured(api)
        await vm.link(bridge: bridge())
        vm.reset()
        #expect(vm.sourceAskOffers(bridgeId: "bluesky").isEmpty)
    }
}

// MARK: - DisplayError

@Test func aGuardianApprovalRefusalDisplaysItsOwnSentence() {
    #expect(DisplayError.message(FfiError.GuardianApprovalRequired(msg: "ask your guardian")) == "ask your guardian")
}
