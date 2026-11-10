import FaunaFFISwift
import Foundation
import Testing
@testable import FaunaKit

// `profile-follow-button` as the Follow ⇄ Following toggle on the shared FaunaKit
// view-model (`docs/goal/behavior/subscriptions.md` § The free followers tier —
// follow and unfollow, rule 4; `docs/goal/ui/profile.md` § Where logic lives →
// *Follow / unfollow / follow-status*). The derivation is the shared `is_following`
// over the status read; the optimistic flip and the armed confirm are the VM's.

/// A scripted nest: the status read answers `status`, and every subscribe and
/// unsubscribe is recorded (each answering `Queued`, the encrypted-mode reply).
private final class ScriptedFollowAPI: APIClient {
    var status = FfiSubscriptionStatus(tier: nil, expiresAt: nil, autoApprove: false, followPending: false)
    private(set) var subscribed: [String] = []
    private(set) var unsubscribed: [String] = []

    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func subscriptionOffersList(authorIdHex: String) async throws -> [FfiTierItem] { [] }

    override func subscriptionStatusGet(authorIdHex: String) async throws -> FfiSubscriptionStatus {
        status
    }

    override func subscriptionSubscribe(
        authorIdHex: String, tier: String, homeNestUrl: String? = nil
    ) async throws -> FfiSubscribeReply {
        subscribed.append(tier)
        return .queued(requestId: 1)
    }

    override func subscriptionUnsubscribe(
        authorId: Data, tier: String, homeNestUrl: String?
    ) async throws -> FfiUnsubscribeReply {
        unsubscribed.append(tier)
        return .queued(requestId: 2)
    }
}

@MainActor
struct ProfileFollowToggleTests {
    private let author = String(repeating: "ab", count: 32)

    @Test("the open derives Following from the status read through the shared is_following")
    func theOpenDerivesFollowingFromTheStatus() async {
        let api = ScriptedFollowAPI()
        api.status = FfiSubscriptionStatus(tier: "gold", expiresAt: nil, autoApprove: false, followPending: false)
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        #expect(vm.isFollowing, "a paid subscriber reads Following (web's tier == followers bug)")

        let pending = ScriptedFollowAPI()
        pending.status = FfiSubscriptionStatus(tier: nil, expiresAt: nil, autoApprove: false, followPending: true)
        await vm.configure(api: pending, authorIdHex: author)
        #expect(vm.isFollowing, "a pending follow reads Following")

        let none = ScriptedFollowAPI()
        await vm.configure(api: none, authorIdHex: author)
        #expect(!vm.isFollowing)
    }

    @Test("a free follower's Following press unfollows at once and reads Follow")
    func aFreeFollowerUnfollowsAtOnce() async {
        let api = ScriptedFollowAPI()
        api.status = FfiSubscriptionStatus(tier: "followers", expiresAt: nil, autoApprove: false, followPending: false)
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        await vm.toggleFollow()
        #expect(api.unsubscribed == ["followers"])
        // The leave is queued: the status still names the tier, the flip outranks it.
        #expect(!vm.isFollowing)
        #expect(vm.followButtonLabel == L.profile.follow)
    }

    @Test("Follow pressed subscribes to the followers tier and flips to Following")
    func followPressedSubscribes() async {
        let api = ScriptedFollowAPI()
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        await vm.toggleFollow()
        #expect(api.subscribed == ["followers"])
        #expect(vm.isFollowing)
        #expect(vm.followButtonLabel == L.profile.following)
    }

    @Test("a paid holder's first press only arms the confirm; the second leaves")
    func aPaidHolderConfirms() async {
        let api = ScriptedFollowAPI()
        api.status = FfiSubscriptionStatus(tier: "gold", expiresAt: nil, autoApprove: false, followPending: false)
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)

        await vm.toggleFollow()
        #expect(api.unsubscribed.isEmpty, "the arming press sends nothing")
        #expect(vm.followButtonLabel == L.profile.unfollowConfirm)

        await vm.toggleFollow()
        #expect(api.unsubscribed == ["followers"], "the leave is at the followers floor")
        #expect(!vm.unfollowArmed)
        #expect(vm.followButtonLabel == L.profile.follow)
    }

    @Test("a fresh open disarms the confirm")
    func aFreshOpenDisarms() async {
        let api = ScriptedFollowAPI()
        api.status = FfiSubscriptionStatus(tier: "gold", expiresAt: nil, autoApprove: false, followPending: false)
        let vm = ProfileOffersVM()
        await vm.configure(api: api, authorIdHex: author)
        await vm.toggleFollow()
        #expect(vm.unfollowArmed)
        await vm.configure(api: api, authorIdHex: author)
        #expect(!vm.unfollowArmed)
        #expect(vm.followButtonLabel == L.profile.following)
    }
}
