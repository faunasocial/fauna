#if !FAUNA_KIDS
import Testing
import Foundation
@testable import FaunaKit

// The tier_1 pin for the form case of `ui/README.md` § Offer only what is
// possible, rule 4 (and the page rulings in `feed.md`, `search.md` and
// `nostr.md` § Errors & edge cases) on the apple apps: every listed submit
// paints from its shared draft-validity predicate (`libs/fauna-ffi` `forms.rs`,
// over UniFFI), its verb asks the same predicate again and refuses with the
// same reason on the page's `error-message`, and the four feed forms answer
// whether their save succeeded, so the form closes only on a saved result.
// Whether the disabled button and its caption LOOK right is the last inch.

/// A blocked answer's reason, rendered — never empty, never the bare key.
@MainActor
private func reason(_ answer: Possibility) throws -> String {
    let text = try #require(answer.reasonText, "expected a blocked answer with a reason")
    #expect(!text.isEmpty)
    return text
}

@Suite("Possibility.refusal — the submit asks what the paint asked")
struct PossibilityRefusalTests {
    @Test("possible: no refusal; blocked: its own reason; absent: refused with nothing to add")
    func threeCases() {
        #expect(Possibility.possible.refusal == nil)
        let blocked = Possibility.blocked(
            reason: LocalizedText(key: "feed.compose_empty", args: [:]))
        #expect(blocked.refusal == blocked.reasonText)
        #expect(blocked.refusal?.isEmpty == false)
        #expect(Possibility.absent.refusal == "")
    }
}

@Suite("Feed forms — painted from the shared predicates, closed only on a save")
@MainActor
struct FeedFormPossibilityTests {
    @Test("post-submit-button: a blank compose is blocked with its reason")
    func postBlankIsBlocked() throws {
        let vm = FeedVM()
        _ = try reason(vm.postSubmitPossibility)
    }

    @Test("feed-reply-submit-button: blank is blocked, and a blank reply refuses on error-message without saving")
    func replyBlankRefuses() async throws {
        let vm = FeedVM()
        #expect(vm.replySubmitPossibility("hi").isPossible)
        let why = try reason(vm.replySubmitPossibility("   "))
        let saved = await vm.replyToPost(postId: "p1", body: "   ")
        #expect(saved == false, "the dialog must stay open")
        #expect(vm.errorMessage == why)
    }

    @Test("a reply that cannot be saved answers false — the dialog stays open")
    func replyFailedSaveKeepsOpen() async {
        let vm = FeedVM()  // no manager: the save cannot happen
        let saved = await vm.replyToPost(postId: "p1", body: "a real reply")
        #expect(saved == false)
    }

    @Test("create-feed: a blank name refuses on error-message and leaves the form open")
    func createBlankRefuses() async throws {
        let vm = FeedVM()
        vm.showCreateForm = true
        var form = FeedCreateForm()
        let why = try reason(form.createPossibility)
        let saved = await vm.createFeed(name: form.name, combination: "and", rules: [])
        #expect(saved == false)
        #expect(vm.showCreateForm, "a refused create keeps the inline form open")
        #expect(vm.errorMessage == why)
        form.name = "My Feed"
        #expect(form.createPossibility.isPossible)
        form.name = "   "
        #expect(!form.createPossibility.isPossible, "a whitespace-only name is blank")
    }

    @Test("create-feed: a create that cannot be saved answers false and keeps the form open")
    func createFailedSaveKeepsOpen() async {
        let vm = FeedVM()  // no manager: the save cannot happen
        vm.showCreateForm = true
        let saved = await vm.createFeed(name: "My Feed", combination: "and", rules: [])
        #expect(saved == false)
        #expect(vm.showCreateForm)
    }

    @Test("bridge-form-subscribe-button: the URI is required, the name is not")
    func bridgeSubscribe() async throws {
        let vm = FeedVM()
        #expect(vm.bridgeSubscribePossibility(bridge: "bluesky", feedUri: "at://feed").isPossible)
        let why = try reason(vm.bridgeSubscribePossibility(bridge: "bluesky", feedUri: ""))
        _ = try reason(vm.bridgeSubscribePossibility(bridge: "", feedUri: "at://feed"))
        let saved = await vm.subscribeBridge(bridge: "bluesky", feedUri: " ", name: "")
        #expect(saved == false)
        #expect(vm.errorMessage == why)
        // A valid draft that cannot be saved also answers false (no manager).
        #expect(await vm.subscribeBridge(bridge: "bluesky", feedUri: "at://feed", name: "") == false)
    }
}

@Suite("Search submit — painted from query_possibility")
@MainActor
struct SearchFormPossibilityTests {
    @Test("a blank submit refuses with the reason on error-message and shows the page")
    func blankSubmitRefuses() async throws {
        let vm = SearchVM()
        vm.query = "  "
        let why = try reason(vm.queryPossibility)
        await vm.search()
        #expect(vm.errorMessage == why)
        #expect(vm.showsResults, "the refusal's reason must be on screen")
        vm.clear()
        #expect(vm.errorMessage == nil)
    }

    @Test("a filter change over a blank buffer stays quiet")
    func filterChangeIsQuiet() async {
        let vm = SearchVM()
        await vm.search(newTypeFilter: SearchVM.typeFilterAll)
        #expect(vm.errorMessage == nil)
        #expect(!vm.showsResults)
    }

    @Test("a typed query is possible")
    func typedIsPossible() {
        let vm = SearchVM()
        vm.query = "cats"
        #expect(vm.queryPossibility.isPossible)
    }
}

@Suite("Nostr forms — painted from the shared predicates")
@MainActor
struct NostrFormPossibilityTests {
    @Test("nostr-link-button: import needs an nsec, remote a bunker URL; generate needs nothing")
    func link() async throws {
        let vm = NostrVM()
        #expect(vm.linkPossibility.isPossible)
        vm.linkMode = .import
        let why = try reason(vm.linkPossibility)
        await vm.link()
        #expect(vm.errorMessage == why)
        vm.importNsec = "nsec1x"
        #expect(vm.linkPossibility.isPossible)
        vm.linkMode = .remote
        _ = try reason(vm.linkPossibility)
    }

    @Test("nostr-add-relay: blank and duplicate drafts are blocked and refuse")
    func addRelay() async throws {
        let vm = NostrVM()
        _ = try reason(vm.addRelayPossibility)
        vm.relays = ["wss://relay.example"]
        vm.relayInput = "wss://relay.example"
        let why = try reason(vm.addRelayPossibility)
        await vm.addRelay()
        #expect(vm.errorMessage == why)
        #expect(vm.relays == ["wss://relay.example"])
        #expect(vm.relayInput == "wss://relay.example", "a refused draft is kept, never dropped")
    }

    @Test("nostr-add-follow: a blank pubkey refuses")
    func addFollow() async throws {
        let vm = NostrVM()
        let why = try reason(vm.addFollowPossibility)
        await vm.addFollow()
        #expect(vm.errorMessage == why)
        vm.followPubkey = "npub1someone"
        #expect(vm.addFollowPossibility.isPossible)
    }

    #if !FAUNA_EXCISE_PAYMENTS
    @Test("nostr-zap-signer-add-btn: blank and non-hex drafts are blocked and refuse")
    func zapSigner() async throws {
        let vm = NostrVM()
        // The zaps plane answered available, so the draft's own reason is the
        // one the designate refuses with (the gate's are pinned in
        // `UnreadStateAnswersTests`).
        vm.applyZapSignerFeaturesRead([unreadStateFeatureRow("zaps", affordance: "available")])
        _ = try reason(vm.zapSignerAddPossibility)
        vm.zapSignerPubkeyInput = "not-hex"
        let why = try reason(vm.zapSignerAddPossibility)
        await vm.addZapSigner()
        #expect(vm.errorMessage == why)
        vm.zapSignerPubkeyInput = String(repeating: "a", count: 64)
        #expect(vm.zapSignerAddPossibility.isPossible)
    }
    #endif

    @Test("Connected apps: offered for a key on the box, absent otherwise")
    func connectedApps() {
        let vm = NostrVM()
        #expect(!vm.showConnectedApps, "no linked account")
        #expect(nostrConnectedAppsPossibility(signingMode: "generated").isPossible)
        #expect(nostrConnectedAppsPossibility(signingMode: "imported").isPossible)
        #expect(!nostrConnectedAppsPossibility(signingMode: "remote").isShown)
        #expect(!nostrConnectedAppsPossibility(signingMode: nil).isShown)
    }
}
#endif
