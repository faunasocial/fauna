import Foundation
import Testing
@testable import FaunaKit

// The Fauna Kids eligibility gate (`account-age-band.md` § The account age band,
// the kids-app bullet, item (3)): `FamilyStatusStore.kidsEligible` is the shared
// `kidsAppEligible` verdict over the last SUCCESSFUL status read — the iOS root
// (`Fauna-iOS/App/ContentView.swift`) refuses an account whose verdict is
// exactly `false`. A real call dials the nest, which a unit test must not, so
// `familyStatus()` is scripted.

private final class StatusAPI: APIClient {
    /// What `familyStatus()` answers; `nil` fails the read.
    var status: FfiFamilyStatus?

    init() {
        super.init(nodeUrl: URL(string: "https://nest.invalid/")!)
    }

    override func familyStatus() async throws -> FfiFamilyStatus {
        guard let status else { throw ScopeTestFailure(account: "status read") }
        return status
    }
}

private func status(supervised: Bool) -> FfiFamilyStatus {
    FfiFamilyStatus(
        supervisedBy: supervised
            ? FfiFamilyGuardianInfo(actorId: Data(repeating: 0xAB, count: 32), handle: "alex")
            : nil,
        policy: nil, wards: [], incomingTransfers: [], usageTodayMinutes: nil,
        contactRequests: [], feedRequests: [], ageBand: nil, supervision: nil)
}

@Suite("FamilyStatusStore — the Fauna Kids eligibility verdict")
@MainActor
struct KidsEligibilityTests {
    @Test("no read yet: no verdict, so the shell is never refused on a guess")
    func noReadNoVerdict() {
        #expect(FamilyStatusStore().kidsEligible == nil)
    }

    @Test("a supervised account is eligible, an unsupervised one is not")
    func verdictFollowsSupervision() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()

        api.status = status(supervised: true)
        await store.refresh(api: api)
        #expect(store.kidsEligible == true)

        api.status = status(supervised: false)
        await store.refresh(api: api)
        #expect(store.kidsEligible == false, "a graduated account flips on its next status read")
    }

    @Test("a failed re-read keeps the last verdict — a blip never refuses a supervised child")
    func failedReadKeepsVerdict() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()
        api.status = status(supervised: true)
        await store.refresh(api: api)

        api.status = nil
        await store.refresh(api: api)
        #expect(store.kidsEligible == true)
    }

    @Test("a teardown (no client) drops the verdict so the next account starts clean")
    func teardownClearsVerdict() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()
        api.status = status(supervised: false)
        await store.refresh(api: api)
        #expect(store.kidsEligible == false)

        await store.refresh(api: nil)
        #expect(store.kidsEligible == nil)
    }
}
