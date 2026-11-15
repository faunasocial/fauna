#if !FAUNA_KIDS
import Foundation
import Testing
@testable import FaunaKit

// The bridges follow composer (account-offline-mutation.md § Implementation
// status today, the bridges follow composer).
//
// With no nest `FfiBridgesClient.addFollowOrQueue` / `removeFollowOrQueue`
// answer QUEUED (the device outbox holds the change, scoped to the followed
// account, and drains it when the nest is back — the queue-then-send half is
// pinned in Rust, `libs/fauna-ffi/src/bridges.rs`). These pin the page's half,
// which this unit target reaches without a socket by scripting the door's
// answer: a queued follow/unfollow clears the page error and changes no list
// (no re-list); a sent one re-lists from the nest. Mirrors android's
// `BridgesVMFollowComposerTest` and tui's queued follow outcome.

private struct ListFailure: Error {}

private final class FollowAPI: APIClient {
    /// What the follow doors answer: `true` = queued (no nest).
    var queued = false
    private(set) var adds: [(bridge: String, id: String)] = []
    private(set) var removes: [(bridge: String, id: String)] = []
    /// Re-list reads: the Bridges page's `listBridges`, the Nostr page's status read.
    private(set) var bridgeLists = 0
    private(set) var nostrReads = 0

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func addBridgeFollowOrQueue(bridgeId: String, followId: String,
                                         petname: String?) async throws -> Bool {
        adds.append((bridgeId, followId))
        return queued
    }

    override func removeBridgeFollowOrQueue(bridgeId: String, followId: String) async throws -> Bool {
        removes.append((bridgeId, followId))
        return queued
    }

    override func listBridges() async throws -> [BridgeInfo] {
        bridgeLists += 1
        throw ListFailure()
    }

    override func getNostrStatus() async throws -> NostrStatus {
        nostrReads += 1
        throw ListFailure()
    }
}

@MainActor
private func bridgesPage(_ api: FollowAPI) -> BridgeManagerVM {
    let vm = BridgeManagerVM()
    vm.configure(api: api)
    vm.errorMessage = "a stale page error"
    return vm
}

@Suite("bridges follow composer — BridgeManagerVM")
@MainActor
struct BridgeManagerVMFollowComposerTests {
    @Test("a follow with no nest is queued, clears the page error and changes no list")
    func aQueuedFollowListsNothing() async {
        let api = FollowAPI()
        api.queued = true
        let vm = bridgesPage(api)
        vm.followId = "did:plc:queued"
        vm.followPetname = "q"

        await vm.addFollow(bridgeId: "bluesky")

        #expect(api.adds.count == 1 && api.adds[0].bridge == "bluesky"
                && api.adds[0].id == "did:plc:queued")
        #expect(api.bridgeLists == 0, "a queued follow has no nest to re-list from")
        #expect(vm.follows.isEmpty, "nothing is listed until the drain")
        #expect(vm.errorMessage == nil, "a queued follow clears the page error")
        #expect(vm.followId.isEmpty && vm.followPetname.isEmpty, "the add form resets")
    }

    @Test("a follow with the nest connected is sent and re-lists")
    func aSentFollowReLists() async {
        let api = FollowAPI()
        let vm = bridgesPage(api)
        vm.followId = "did:plc:sent"

        await vm.addFollow(bridgeId: "bluesky")

        #expect(api.adds.count == 1)
        #expect(api.bridgeLists == 1, "a sent follow re-lists from the nest, the list's one word")
        #expect(vm.followId.isEmpty)
    }

    @Test("an unfollow with no nest is queued, clears the page error and changes no list")
    func aQueuedUnfollowListsNothing() async {
        let api = FollowAPI()
        api.queued = true
        let vm = bridgesPage(api)

        await vm.removeFollow(bridgeId: "bluesky", followId: "did:plc:gone")

        #expect(api.removes.count == 1 && api.removes[0].id == "did:plc:gone")
        #expect(api.bridgeLists == 0, "a queued unfollow leaves the account listed until the drain")
        #expect(vm.errorMessage == nil)
    }

    @Test("an unfollow with the nest connected is sent and re-lists")
    func aSentUnfollowReLists() async {
        let api = FollowAPI()
        let vm = bridgesPage(api)

        await vm.removeFollow(bridgeId: "bluesky", followId: "did:plc:gone")

        #expect(api.removes.count == 1)
        #expect(api.bridgeLists == 1)
    }
}

@Suite("bridges follow composer — NostrVM")
@MainActor
struct NostrVMFollowComposerTests {
    @Test("a Nostr follow with no nest is queued on the nostr bridge and changes no list")
    func aQueuedNostrFollowListsNothing() async {
        let api = FollowAPI()
        api.queued = true
        let vm = NostrVM()
        vm.configure(api: api)
        vm.errorMessage = "a stale page error"
        vm.followPubkey = "npub1queued"

        await vm.addFollow()

        #expect(api.adds.count == 1 && api.adds[0].bridge == "nostr"
                && api.adds[0].id == "npub1queued")
        #expect(api.nostrReads == 0, "a queued follow has no nest to re-list from")
        #expect(vm.follows.isEmpty)
        #expect(vm.errorMessage == nil)
        #expect(vm.followPubkey.isEmpty, "the add form resets")
    }

    @Test("a Nostr follow with the nest connected is sent and re-lists")
    func aSentNostrFollowReLists() async {
        let api = FollowAPI()
        let vm = NostrVM()
        vm.configure(api: api)
        vm.followPubkey = "npub1sent"

        await vm.addFollow()

        #expect(api.nostrReads == 1)
    }
}
#endif // !FAUNA_KIDS
