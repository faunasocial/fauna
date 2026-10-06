import Testing
import Foundation
@testable import FaunaKit

// The live-read twin of `SupervisionSnapshotSeedTests`: a SUCCESSFUL
// `fauna.family.status` read moves the client-enforced inputs — the content
// floor, `content_notify`, the screen-time policy and the lock's guardian —
// from the reply's supervision fold (`FfiFamilyStatus.supervision`), never from
// its raw `policy` (`family-client-enforcement.md` § Implementation status
// today). The shared `SupervisionSnapshot::from_status` gates every supervised
// field on a named guardian, so a reply that still carries a policy document
// but names no guardian yields no fold and must bind nothing. android's
// `ContentPolicyStoreTest` case "a successful read whose policy names no
// guardian binds no floor" is the reference leg this mirrors.
//
// Both stores' `refresh(api:)` need a live `APIClient` (a concrete class, no
// protocol seam), so these drive the one step past the read:
// `ContentPolicyStore.guardianHalf(of:)` through the `merged` fold `refresh`
// calls, and `ScreenTimeStore.land(_:)`, which `refresh` calls.
//
// Each case pins both directions: a guardian-less reply carrying a policy
// binds nothing (the defect a raw read has), and a fold with no raw policy
// beside it still binds (the store really reads the fold, not a raw read that
// happens to agree with it).

private func guardian(handle: String = "alex") -> FfiFamilyGuardianInfo {
    FfiFamilyGuardianInfo(actorId: Data(repeating: 0xAB, count: 32), handle: handle)
}

private func floor(nsfw: String = "block") -> FfiContentPolicy {
    FfiContentPolicy(nsfw: nsfw, spam: "inherit", phishing: "inherit", commercial: "inherit")
}

private func flaggedNsfw() -> [ContentLabelEntry] {
    [ContentLabelEntry(category: "nsfw", confidencePerMille: 900)]
}

private func window() -> FfiScreenTimePolicy {
    FfiScreenTimePolicy(windowStart: 21 * 60, windowEnd: 7 * 60, dailyMinutes: 120)
}

private func reachPolicy(
    contentPolicy: FfiContentPolicy? = nil, screenTime: FfiScreenTimePolicy? = nil,
    contentNotify: Bool? = nil
) -> FfiReachPolicy {
    FfiReachPolicy(
        contactApproval: false, unknownSenderMail: "allow", federationContact: true,
        feedSources: "allow", contentPolicy: contentPolicy, screenTime: screenTime,
        contentNotify: contentNotify, unknownPeerDm: nil)
}

private func status(
    supervisedBy: FfiFamilyGuardianInfo?, policy: FfiReachPolicy?,
    supervision: FfiSupervisionSnapshot?, usageTodayMinutes: UInt32? = nil
) -> FfiFamilyStatus {
    FfiFamilyStatus(
        supervisedBy: supervisedBy, policy: policy, wards: [], incomingTransfers: [],
        usageTodayMinutes: usageTodayMinutes, contactRequests: [], feedRequests: [],
        ageBand: nil, supervision: supervision)
}

/// A reply that still carries a policy document naming a floor, Notify and a
/// window, but names NO guardian. No current nest sends one, but a client must
/// stay correct against nests of other versions, and the shared fold hands
/// this reply no supervision at all.
private func guardianlessReplyWithAPolicy() -> FfiFamilyStatus {
    status(
        supervisedBy: nil,
        policy: reachPolicy(contentPolicy: floor(), screenTime: window(), contentNotify: true),
        supervision: nil)
}

// MARK: - ContentPolicyStore — the floor and content_notify

@Test("a successful read whose policy names no guardian binds no floor and no Notify")
func aGuardianlessPolicyBindsNoFloor() {
    let after = ContentPolicyStore.merged(
        previous: ContentPolicyInputs(),
        policy: .succeeded(ContentPolicyStore.guardianHalf(of: guardianlessReplyWithAPolicy())),
        own: .failed)

    #expect(after.contentPolicy == nil,
            "a policy naming no guardian binds no floor — reading the raw `policy` binds it to an unsupervised viewer")
    #expect(!after.contentNotify, "nor turns Guardian Notify counting on")
    #expect(after.verdictFor(flaggedNsfw()) != "block")
}

@Test("a successful read binds the floor and content_notify from the supervision fold")
func theFloorAndNotifyBindFromTheFold() {
    let reply = status(
        supervisedBy: guardian(),
        policy: nil,
        supervision: FfiSupervisionSnapshot(
            supervisedBy: guardian(), contentPolicy: floor(), contentNotify: true, screenTime: nil))

    let after = ContentPolicyStore.merged(
        previous: ContentPolicyInputs(),
        policy: .succeeded(ContentPolicyStore.guardianHalf(of: reply)),
        own: .failed)

    #expect(after.contentPolicy == floor(), "the floor moves from the fold")
    #expect(after.contentNotify, "content_notify moves from the fold, independently of the floor")
    #expect(after.verdictFor(flaggedNsfw()) == "block")
}

// MARK: - ScreenTimeStore — the window/budget policy and the lock's guardian

@MainActor
@Test("a successful read whose policy names no guardian binds no screen-time policy and no lock")
func aGuardianlessPolicyBindsNoLock() {
    let store = ScreenTimeStore()

    store.land(guardianlessReplyWithAPolicy())

    #expect(store.policyForTest == nil,
            "a policy naming no guardian binds no window or budget — reading the raw `policy` records it")
    #expect(store.guardianHandleForTest == nil)
    #expect(store.lockMessage == nil)
}

@MainActor
@Test("a successful read binds the screen-time policy and the lock's guardian from the supervision fold")
func theScreenTimePolicyAndGuardianBindFromTheFold() {
    let store = ScreenTimeStore()
    let reply = status(
        supervisedBy: guardian(handle: "alex"),
        policy: nil,
        supervision: FfiSupervisionSnapshot(
            supervisedBy: guardian(handle: "alex"), contentPolicy: nil, contentNotify: false,
            screenTime: window()),
        usageTodayMinutes: 30)

    store.land(reply)

    #expect(store.policyForTest == window(), "the window/budget moves from the fold")
    #expect(store.guardianHandleForTest == "alex", "the lock's guardian moves from the fold")
}

@MainActor
@Test("a successful unsupervised read clears a restored screen-time policy and guardian")
func anUnsupervisedReadClearsARestoredLock() {
    let store = ScreenTimeStore()
    store.seed(from: FfiSupervisionSnapshot(
        supervisedBy: guardian(), contentPolicy: nil, contentNotify: false, screenTime: window()))
    #expect(store.guardianHandleForTest != nil, "sanity: seeded")

    store.land(status(supervisedBy: nil, policy: nil, supervision: nil))

    #expect(store.policyForTest == nil && store.guardianHandleForTest == nil,
            "a successful read reporting unsupervised is the one outcome that legitimately clears the lock")
    #expect(store.lockMessage == nil)
}
