import Testing
import Foundation
@testable import FaunaKit

// The own-tier and mail gates on apple (`docs/goal/ui/folders.md` § Errors & edge
// cases, *Own-tier and mail gates*; `docs/goal/ui/profile.md` § Errors & edge cases,
// *A tier before what sells it*): `folder-webdav-toggle`, `folder-paywall-tier-select`,
// `wizard-next-button` / `wizard-create-button`, `subscription-claim-mint-button` and
// `subscription-provider-add-button` are painted from the shared Possibility answers,
// which keep a pending or failed read apart from "no mail key" / "no tier"; each press
// asks the same answer and refuses with its reason on `error-message`. The answers
// themselves are pinned in shared Rust (`libs/fauna-ffi/src/forms.rs`,
// `libs/fauna-folders-machine/src/possibility.rs`); these pin the Swift seam: which
// read state the VM hands over, and that the press refuses with the painted reason.

private struct StubBuildError: Error {}
private struct StubReadError: Error {}

/// A folders page whose machine never builds (no nest), with scripted custody and
/// own-tier reads — the two page-level reads the gates sit on.
private final class ScriptedGateReadsAPI: APIClient {
    var holdsMsek: Result<Bool, Error> = .success(false)
    var tierNames: Result<[String], Error> = .success([])

    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func devicesMachine(observer: DevicesObserver) async throws -> DevicesMachine {
        throw StubBuildError()
    }
    override func canServeWebdav() async throws -> Bool { try holdsMsek.get() }
    override func listSubscriptionTiers() async throws -> [FfiTierItem] {
        try tierNames.get().map(tier)
    }
}

private func tier(_ name: String) -> FfiTierItem {
    FfiTierItem(
        name: name, rank: 1, description: nil, priceHint: nil, askingPriceSats: nil,
        paymentUrl: nil, autoApprove: false, createdAt: 0, unlocksPost: nil, hidden: false)
}

@MainActor
@Suite("Own-tier and mail gates — folders page")
struct FolderOwnTierAndMailGateTests {
    @Test("before the reads answer, the toggle and the select say still loading, never 'set up mail' / 'create a tier'")
    func pendingReadsAreStillLoading() async {
        let vm = DevicesMachineVM()
        #expect(vm.webdavRead == .pending)
        #expect(vm.ownTiersRead == .pending)
        #expect(vm.webdavAnswer.reasonText == L.common.stillLoading)
        #expect(vm.paywallAnswer.reasonText == L.common.stillLoading)
        #expect(serveWebdavHintText(vm.webdavAnswer) == L.common.stillLoading)

        // The press asks the same answer and refuses with its reason.
        await vm.serveFolderWebdav(name: "photos", mlsGroupIdHex: nil, enable: true)
        #expect(vm.sharingError == L.common.stillLoading)
        await vm.paywallFolder(name: "photos", mlsGroupIdHex: nil, tier: "gold")
        #expect(vm.sharingError == L.common.stillLoading)
    }

    @Test("an answered read with no mail key says still loading; no tier names the step")
    func answeredNoneNamesTheStep() async {
        let api = ScriptedGateReadsAPI()
        let vm = DevicesMachineVM()
        await vm.configure(api: api)
        await vm.loadCanServeWebdav()
        await vm.loadOwnTierNames()

        #expect(vm.webdavRead == .resolved)
        #expect(vm.ownTiersRead == .resolved)
        #expect(vm.webdavAnswer.reasonText == L.common.stillLoading)
        #expect(serveWebdavHintText(vm.webdavAnswer) == L.common.stillLoading)
        #expect(vm.paywallAnswer.reasonText == L.devices.paywallTierNeedsTier)

        await vm.paywallFolder(name: "photos", mlsGroupIdHex: nil, tier: "gold")
        #expect(vm.sharingError == L.devices.paywallTierNeedsTier)
    }

    @Test("a failed read says it failed, never 'you have none'")
    func failedReadsSayFailed() async {
        let api = ScriptedGateReadsAPI()
        api.holdsMsek = .failure(StubReadError())
        api.tierNames = .failure(StubReadError())
        let vm = DevicesMachineVM()
        await vm.configure(api: api)
        await vm.loadCanServeWebdav()
        await vm.loadOwnTierNames()

        #expect(vm.webdavRead == .failed)
        #expect(vm.ownTiersRead == .failed)
        #expect(vm.webdavAnswer.reasonText == L.common.loadFailed)
        #expect(vm.paywallAnswer.reasonText == L.common.loadFailed)
        #expect(vm.canServeWebdav == false, "a failed read never opens the toggle")
    }

    @Test("a held mail key and an own tier make both possible, with the ordinary hint")
    func answeredYesIsPossible() async {
        let api = ScriptedGateReadsAPI()
        api.holdsMsek = .success(true)
        api.tierNames = .success(["gold"])
        let vm = DevicesMachineVM()
        await vm.configure(api: api)
        await vm.loadCanServeWebdav()
        await vm.loadOwnTierNames()

        #expect(vm.webdavAnswer.isPossible)
        #expect(vm.paywallAnswer.isPossible)
        #expect(serveWebdavHintText(vm.webdavAnswer) == L.devices.serveWebdavHint)
    }

    @Test("with no wizard open, Next and Create are absent and a press sends nothing")
    func noWizardIsAbsent() {
        let vm = DevicesMachineVM()
        #expect(!vm.wizardNextAnswer.isShown)
        #expect(!vm.wizardCreateAnswer.isShown)
        #expect(vm.refuseWizardCreate())
        vm.wizardNext()
    }
}

#if !FAUNA_EXCISE_PAYMENTS
/// The Tiers tab's four reads, the tier list scripted.
private final class ScriptedTiersAPI: APIClient {
    var tierNames: Result<[String], Error> = .success([])
    private(set) var minted: [String] = []

    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func paymentProviderKinds() -> [String] { ["stripe"] }
    override func listSubscriptionTiers() async throws -> [FfiTierItem] {
        try tierNames.get().map(tier)
    }
    override func listSubscriptionRequests() async throws -> [FfiPendingRequest] { [] }
    // With a tier selected the hydrate reads its roster: scripted, so it never
    // dials the stub's unreachable nest.
    override func listSubscribers(tierName: String) async throws -> [FfiSubscriberEntry] { [] }
    override func listPaymentProviders() async throws -> [FfiProviderItem] { [] }
    override func listPaymentClaims() async throws -> [FfiClaimItem] { [] }
    override func mintPaymentClaim(tier: String, validUntil: UInt64?) async throws -> FfiClaimMintReply {
        minted.append(tier)
        throw StubReadError()
    }
}

@MainActor
@Suite("Own-tier gates — profile Tiers tab")
struct TiersTabOwnTierGateTests {
    @Test("before the tab's first read answers, mint and the provider opener are still loading")
    func pendingIsStillLoading() {
        let vm = SubscriptionsVM()
        #expect(vm.tiersRead == .pending)
        #expect(vm.claimMintAnswer.reasonText == L.common.stillLoading)
        #expect(vm.providerAddAnswer.reasonText == L.common.stillLoading)
    }

    @Test("with no tier the provider form does not open and mint refuses, each with its reason")
    func noTierRefuses() async {
        let api = ScriptedTiersAPI()
        let vm = SubscriptionsVM()
        await vm.configure(api: api, authorIdHex: "actor")
        #expect(vm.tiersRead == .resolved)
        #expect(vm.claimMintAnswer.reasonText == L.subscriptions.claimNeedsTier)
        #expect(vm.providerAddAnswer.reasonText == L.subscriptions.providerNeedsTier)

        vm.openProviderForm()
        #expect(vm.showProviderForm == false, "the provider form must not open with no tier")
        #expect(vm.errorMessage == L.subscriptions.providerNeedsTier)

        await vm.mintClaim()
        #expect(api.minted.isEmpty)
        #expect(vm.errorMessage == L.subscriptions.claimNeedsTier)
    }

    @Test("a first read that failed says so, not 'create a tier'")
    func failedFirstReadSaysFailed() async {
        let api = ScriptedTiersAPI()
        api.tierNames = .failure(StubReadError())
        let vm = SubscriptionsVM()
        await vm.configure(api: api, authorIdHex: "actor")
        #expect(vm.tiersRead == .failed)
        #expect(vm.claimMintAnswer.reasonText == L.common.loadFailed)
        #expect(vm.providerAddAnswer.reasonText == L.common.loadFailed)
    }

    @Test("an answered list stands through a later failed re-read")
    func laterFailureLeavesTheListStanding() async {
        let api = ScriptedTiersAPI()
        api.tierNames = .success(["gold"])
        let vm = SubscriptionsVM()
        await vm.configure(api: api, authorIdHex: "actor")
        #expect(vm.claimMintAnswer.isPossible)

        api.tierNames = .failure(StubReadError())
        await vm.hydrate()
        #expect(vm.tiersRead == .resolved)
        #expect(vm.providerAddAnswer.isPossible)
        vm.openProviderForm()
        #expect(vm.showProviderForm)
    }
}
#endif
