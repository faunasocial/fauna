import Foundation
import Testing
@testable import FaunaKit

// The Fauna Kids eligibility gate (`account-age-band.md` § The account age band
// → *Kids-app eligibility while it is unknown*): `FamilyStatusStore.kidsEligibility`
// is the shared three-way `kidsAppEligibility` over the last SUCCESSFUL status
// read and the persisted supervision snapshot — the iOS root
// (`Fauna-iOS/App/ContentView.swift`) mounts the shell only on `.eligible`. A real call dials the nest, which a unit test must not, so
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

private func snapshot() -> FfiSupervisionSnapshot {
    FfiSupervisionSnapshot(
        supervisedBy: FfiFamilyGuardianInfo(actorId: Data(repeating: 0xAB, count: 32), handle: "alex"),
        contentPolicy: nil, contentNotify: false, screenTime: nil)
}

@Suite("FamilyStatusStore — the Fauna Kids eligibility verdict")
@MainActor
struct KidsEligibilityTests {
    @Test("no snapshot and no read: unknown, so neither shell nor refusal on a guess")
    func noReadUnknown() {
        let store = FamilyStatusStore()
        #expect(store.kidsEligibility == .unknown)
        #expect(!store.kidsReadFailedWhileUnknown)
    }

    @Test("a supervised snapshot answers eligible before any read")
    func snapshotSeedsEligible() {
        let store = FamilyStatusStore()
        store.seed(from: snapshot())
        #expect(store.kidsEligibility == .eligible)
        store.seed(from: nil)
        #expect(store.kidsEligibility == .unknown, "an account switch starts over for the incoming account")
    }

    @Test("a supervised account is eligible, an unsupervised one is not")
    func verdictFollowsSupervision() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()

        api.status = status(supervised: true)
        await store.refresh(api: api)
        #expect(store.kidsEligibility == .eligible)

        api.status = status(supervised: false)
        await store.refresh(api: api)
        #expect(store.kidsEligibility == .ineligible, "a graduated account flips on its next status read")
    }

    @Test("a failed read while unknown stays unknown and sets the error; the next good read clears it")
    func failedReadWhileUnknown() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()
        api.status = nil
        await store.refresh(api: api)
        #expect(store.kidsEligibility == .unknown)
        #expect(store.kidsReadFailedWhileUnknown)

        api.status = status(supervised: true)
        await store.refresh(api: api)
        #expect(store.kidsEligibility == .eligible)
        #expect(!store.kidsReadFailedWhileUnknown)
    }

    @Test("a failed re-read after eligible stays eligible — a blip never refuses a supervised child")
    func failedReadKeepsVerdict() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()
        api.status = status(supervised: true)
        await store.refresh(api: api)

        api.status = nil
        await store.refresh(api: api)
        #expect(store.kidsEligibility == .eligible)
        #expect(!store.kidsReadFailedWhileUnknown)
    }

    @Test("a teardown (no client) drops the verdict so the next account starts clean")
    func teardownClearsVerdict() async {
        let api = StatusAPI()
        let store = FamilyStatusStore()
        api.status = status(supervised: false)
        await store.refresh(api: api)
        #expect(store.kidsEligibility == .ineligible)

        await store.refresh(api: nil)
        #expect(store.kidsEligibility == .unknown)
        #expect(!store.kidsReadFailedWhileUnknown)
    }
}
