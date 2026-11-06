import Foundation
import Testing
@testable import FaunaKit

// The iOS store-consent round (`account-age-band.md` § The account age band →
// the store-law developer duties bullet, items (3) and (4)): the glue reads its
// three seams, hands them to the ONE shared call (`storeConsentRound`), writes
// the returned state back, and paints the lock from the round's own message.
// The decisions are the shared round's (Rust-pinned at tier_1); these pin the
// glue — what it feeds, what it writes, when it asks and what an answer does.

private struct Unavailable: Error {}

private final class FakeSignals: StoreConsentSignals {
    var features: StoreRegulatoryFeatures?
    var featuresThrow = false
    var rating: Int64?

    init(features: StoreRegulatoryFeatures?, rating: Int64?) {
        self.features = features
        self.rating = rating
    }

    func regulatoryFeatures() async throws -> StoreRegulatoryFeatures? {
        if featuresThrow { throw Unavailable() }
        return features
    }

    func ageRatingCode() async -> Int64? { rating }
}

private final class FakeAsker: StoreConsentAsker {
    var asked: [String] = []
    var acknowledged: [String] = []
    var askThrows = false

    func ask(updateDescription: String) async throws {
        if askThrows { throw Unavailable() }
        asked.append(updateDescription)
    }

    func acknowledge(updateDescription: String) async {
        acknowledged.append(updateDescription)
    }
}

private final class MemoryStateStore: StoreConsentStateStore {
    var state = FfiStoreConsentState(consentedThrough: nil, lastRating: nil)
    var saves = 0
    func load() -> FfiStoreConsentState { state }
    func save(_ state: FfiStoreConsentState) {
        self.state = state
        saves += 1
    }
}

private let parental = StoreRegulatoryFeatures(parentalConsentRequired: true, adultNotificationRequired: false)
private let adult = StoreRegulatoryFeatures(parentalConsentRequired: false, adultNotificationRequired: true)
private let ratingChanged = L.lookup("family.store_consent_lock.rating_changed")

@MainActor
private func store(_ signals: FakeSignals, _ asker: FakeAsker, _ state: MemoryStateStore) -> StoreConsentStore {
    StoreConsentStore(signals: signals, asker: asker, stateStore: state)
}

@MainActor
@Test func noRegulatoryFeatureNeverGatesButStillWritesTheBaseline() async {
    let state = MemoryStateStore()
    let s = store(FakeSignals(features: nil, rating: 4), FakeAsker(), state)
    await s.refresh()
    #expect(s.lockLines == nil)
    #expect(!s.canAsk)
    // The first-run baseline is written whatever the signal says.
    #expect(state.state.consentedThrough == 0)
}

@MainActor
@Test func aThrowingFeatureReadIsNoSignal() async {
    let signals = FakeSignals(features: parental, rating: 4)
    signals.featuresThrow = true
    let state = MemoryStateStore()
    state.state = FfiStoreConsentState(consentedThrough: 0, lastRating: 3)
    let s = store(signals, FakeAsker(), state)
    await s.refresh()
    #expect(s.lockLines == nil)
}

@MainActor
@Test func aRatingChangeLocksAndAnApprovedAskLiftsIt() async {
    let signals = FakeSignals(features: parental, rating: 4)
    let asker = FakeAsker()
    let state = MemoryStateStore()
    let s = store(signals, asker, state)

    await s.refresh()
    #expect(s.lockLines == nil, "the first rating read is a silent baseline")
    #expect(state.state.lastRating == 4)

    signals.rating = 9
    await s.refresh()
    let lines = try! #require(s.lockLines)
    #expect(lines.contains(ratingChanged))
    #expect(lines.contains(L.lookup("family.store_consent_lock.state_pending")))
    #expect(lines.contains(L.lookup("family.store_consent_lock.ask_hint")))
    #expect(s.canAsk)
    #expect(state.state.lastRating == 4, "a pending change keeps the old rating")

    await s.askParent()
    #expect(asker.asked == [ratingChanged], "the ask carries the entry's own description")
    #expect(s.asksSent == 1)
    #expect(s.lockLines != nil, "sending the ask lifts nothing")

    await s.receive(answer: .approved)
    #expect(s.lockLines == nil)
    #expect(!s.canAsk)
    #expect(state.state.lastRating == 9, "an approval consents through the new rating")

    // The consumed answer never carries over: a later change locks again.
    signals.rating = 12
    await s.refresh()
    #expect(s.lockLines != nil)
}

@MainActor
@Test func aDeclinedAskReadsSoAndAReAskReturnsToPending() async {
    let signals = FakeSignals(features: parental, rating: 4)
    let asker = FakeAsker()
    let state = MemoryStateStore()
    state.state = FfiStoreConsentState(consentedThrough: 0, lastRating: 4)
    let s = store(signals, asker, state)
    signals.rating = 5

    await s.refresh()
    await s.receive(answer: .declined)
    let declined = try! #require(s.lockLines)
    #expect(declined.contains(L.lookup("family.store_consent_lock.state_declined")))
    #expect(!declined.contains(L.lookup("family.store_consent_lock.state_pending")))
    #expect(s.canAsk, "a declined lock can ask again")

    await s.askParent()
    let pending = try! #require(s.lockLines)
    #expect(pending.contains(L.lookup("family.store_consent_lock.state_pending")))
}

@MainActor
@Test func aFailedAskSendsNothingAndKeepsTheLock() async {
    let signals = FakeSignals(features: parental, rating: 5)
    let asker = FakeAsker()
    asker.askThrows = true
    let state = MemoryStateStore()
    state.state = FfiStoreConsentState(consentedThrough: 0, lastRating: 4)
    let s = store(signals, asker, state)
    await s.refresh()
    await s.askParent()
    #expect(s.asksSent == 0)
    #expect(s.lockLines != nil)
}

@MainActor
@Test func anAskWithNothingPendingSendsNothing() async {
    let asker = FakeAsker()
    let s = store(FakeSignals(features: parental, rating: 4), asker, MemoryStateStore())
    await s.refresh()
    await s.askParent()
    #expect(asker.asked.isEmpty)
}

@MainActor
@Test func anAdultSeesTheSystemSheetAndNoLock() async {
    let signals = FakeSignals(features: adult, rating: 5)
    let asker = FakeAsker()
    let state = MemoryStateStore()
    state.state = FfiStoreConsentState(consentedThrough: 0, lastRating: 4)
    let s = store(signals, asker, state)
    await s.refresh()
    #expect(s.lockLines == nil)
    #expect(asker.acknowledged == [ratingChanged])
    #expect(state.state.lastRating == 5, "acknowledged once the sheet was shown")

    await s.refresh()
    #expect(asker.acknowledged.count == 1, "an acknowledged change is not shown again")
}

/// Store reads that never return — what a device with no App Store account
/// gives (the iOS simulator's `requiredRegulatoryFeatures` /
/// `AppStore.ageRatingCode`). The continuations are kept, never resumed.
private final class HangingSignals: StoreConsentSignals {
    var hangFeatures: Bool
    var hangRating: Bool
    private var parked: [Any] = []

    init(hangFeatures: Bool, hangRating: Bool) {
        self.hangFeatures = hangFeatures
        self.hangRating = hangRating
    }

    func regulatoryFeatures() async throws -> StoreRegulatoryFeatures? {
        guard hangFeatures else { return parental }
        return await withCheckedContinuation { (c: CheckedContinuation<StoreRegulatoryFeatures?, Never>) in
            parked.append(c)
        }
    }

    func ageRatingCode() async -> Int64? {
        guard hangRating else { return 4 }
        return await withCheckedContinuation { (c: CheckedContinuation<Int64?, Never>) in parked.append(c) }
    }
}

@MainActor
@Test func aFeatureReadThatNeverReturnsIsNoSignal() async {
    let state = MemoryStateStore()
    let s = StoreConsentStore(
        signals: HangingSignals(hangFeatures: true, hangRating: false), asker: FakeAsker(),
        stateStore: state, readTimeout: .milliseconds(50))
    await s.refresh()
    #expect(s.lockLines == nil)
    #expect(state.saves == 1, "the round still ran and wrote its state back")
}

@MainActor
@Test func aRatingReadThatNeverReturnsKeepsTheStoredRating() async {
    let state = MemoryStateStore()
    state.state = FfiStoreConsentState(consentedThrough: 0, lastRating: 3)
    let s = StoreConsentStore(
        signals: HangingSignals(hangFeatures: false, hangRating: true), asker: FakeAsker(),
        stateStore: state, readTimeout: .milliseconds(50))
    await s.refresh()
    #expect(s.lockLines == nil)
    #expect(state.state.lastRating == 3)
}

@MainActor
@Test func aHungLaunchReadDoesNotStallTheNextRound() async {
    // The launch round's real reads hang; an injected round queued behind it
    // still runs (the e2e agent's `store_consent_inject`, or a PermissionKit
    // answer arriving).
    let state = MemoryStateStore()
    state.state = FfiStoreConsentState(consentedThrough: 0, lastRating: 3)
    let s = StoreConsentStore(
        signals: HangingSignals(hangFeatures: true, hangRating: true), asker: FakeAsker(),
        stateStore: state, readTimeout: .milliseconds(50))
    await s.refresh()
    await s.injectTestSignals(FixedStoreConsentSignals(features: parental, rating: 4))
    #expect(s.lockLines?.contains(ratingChanged) == true)
    #expect(s.canAsk)
}
