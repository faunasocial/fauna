import Foundation
import Testing
@testable import FaunaKit

// The CALL-SITE half of apple's account-switch landing guard
// (`account-scoping.md` § The scoping taxonomy, the in-flight landing rules,
// `:208-236`: a detached writer's guard is checked at fire time AND again
// after the await). `FeedPostActionsTests`/`WebPublishStoreTests` pin the
// guarded landers themselves (`FeedVM.landClientErrorMessage`,
// `WebPublishStore.landActionFailure`); they cannot see whether a call site
// captures its generation BEFORE its await — defeating every capture at once
// once left the whole suite green.
//
// So every call site's async body is an `internal static` function taking its
// awaited operation as a closure, and each test below executes that body with
// an operation that parks mid-await: the test runs `reset()` (the
// account-switch drop) while it is parked, then lets it throw, and expects
// the failure NOT to land. Each test first runs the same site without the
// reset and expects the failure TO land, so a site that stopped reaching its
// error slot at all cannot pass vacuously. Moving one site's capture below
// its await — or re-reading the generation at landing — reddens exactly that
// site's test.
//
// Out of unit-tier reach, on purpose: the two shell landers that wire
// `CueViewportObserver`'s `onError` into `FeedVM.landClientErrorMessage`
// (`MacFeedDetailView`, iOS `FeedListView`) live in the app targets, not
// FaunaKit. They forward the generation `onError` hands them verbatim; the
// cue tests below stand in for them with the same one-line lander.

// MARK: - Test doubles

private struct StubFailure: Error {}

/// Counts calls made from the non-isolated FFI-seam closures; the tests using it
/// drive the calls sequentially, so no lock is needed.
private final class Counter: @unchecked Sendable {
    private(set) var value = 0
    func bump() { value += 1 }
}

/// A one-shot signal an async test can wait on without any wall-clock sleep.
@MainActor
private final class Latch {
    private var fired = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func signal() {
        fired = true
        let pending = waiters
        waiters = []
        for waiter in pending { waiter.resume() }
    }

    func wait() async {
        if fired { return }
        await withCheckedContinuation { waiters.append($0) }
    }
}

/// An awaited operation that parks until the test settles it — so the test can
/// run `reset()` at exactly the moment a real FFI call would be in flight.
@MainActor
private final class ParkedCall {
    private var parked: CheckedContinuation<Void, Error>?
    private let arrived = Latch()

    /// The operation handed to the site: parks, then returns or throws.
    func park() async throws {
        try await withCheckedThrowingContinuation { continuation in
            parked = continuation
            arrived.signal()
        }
    }

    /// For sites whose awaited operation returns a value: only ever settled by
    /// `fail()`, so it never needs to produce one.
    func parkThenThrow<T>() async throws -> T {
        try await park()
        throw StubFailure()
    }

    func untilParked() async { await arrived.wait() }

    func fail() {
        parked?.resume(throwing: StubFailure())
        parked = nil
    }

    func succeed() {
        parked?.resume()
        parked = nil
    }
}

/// Run `site` against a fresh `FeedVM` with a call that parks mid-await, run
/// `reset()` while it is parked when `resetMidAwait`, let the call throw, and
/// report what landed in the feed's error slot.
@MainActor
private func feedErrorSlot(
    resetMidAwait: Bool, _ site: @escaping (FeedVM, ParkedCall) async -> Void
) async -> String? {
    let vm = FeedVM()
    let call = ParkedCall()
    let running = Task { await site(vm, call) }
    await call.untilParked()
    if resetMidAwait { vm.reset() }
    call.fail()
    await running.value
    return vm.clientErrorMessage
}

/// The twin of `feedErrorSlot` for the Web settings page's own sites, whose
/// error slot is `WebPublishStore.errorMessage`.
@MainActor
private func webErrorSlot(
    resetMidAwait: Bool, _ site: @escaping (WebPublishStore, ParkedCall) async -> Void
) async -> String? {
    let store = WebPublishStore()
    let call = ParkedCall()
    let running = Task { await site(store, call) }
    await call.untilParked()
    if resetMidAwait { store.reset() }
    call.fail()
    await running.value
    return store.errorMessage
}

/// Both halves for one feed-slot site: without a switch the failure lands;
/// with `reset()` mid-await it does not.
@MainActor
private func expectFeedSiteCapturesBeforeAwait(
    _ site: @escaping (FeedVM, ParkedCall) async -> Void,
    sourceLocation: SourceLocation = #_sourceLocation
) async {
    let control = await feedErrorSlot(resetMidAwait: false, site)
    #expect(control != nil,
            "control: with no account switch this site's failure must reach the feed's error slot",
            sourceLocation: sourceLocation)
    let stale = await feedErrorSlot(resetMidAwait: true, site)
    #expect(stale == nil,
            "a failure whose await outlived an account switch must not paint the incoming actor's feed",
            sourceLocation: sourceLocation)
}

/// Both halves for one `WebPublishStore`-slot site.
@MainActor
private func expectWebSiteCapturesBeforeAwait(
    _ site: @escaping (WebPublishStore, ParkedCall) async -> Void,
    sourceLocation: SourceLocation = #_sourceLocation
) async {
    let control = await webErrorSlot(resetMidAwait: false, site)
    #expect(control != nil,
            "control: with no account switch this site's failure must reach the store's error slot",
            sourceLocation: sourceLocation)
    let stale = await webErrorSlot(resetMidAwait: true, site)
    #expect(stale == nil,
            "a failure whose await outlived an account switch must not paint the incoming actor's store",
            sourceLocation: sourceLocation)
}

/// Cue calls whose every FFI leg is inert unless a test overrides it.
private func cueCalls(
    hydrateCues: @escaping () async throws -> Void = {},
    hydrateSignalOptin: @escaping () async throws -> Void = {},
    recordObservation: @escaping (CueObservation) async throws -> Void = { _ in },
    drainAll: @escaping (UInt64) -> [CueObservation] = { _ in [] }
) -> CueCaptureCalls {
    CueCaptureCalls(
        hydrateCues: hydrateCues,
        hydrateSignalOptin: hydrateSignalOptin,
        recordObservation: recordObservation,
        flushCues: {},
        sample: { _, _, _, _, _, _ in [] },
        drainAll: drainAll)
}

private let anObservation = CueObservation(
    contentId: "p", isMedia: false, mediaPlayedPm: nil,
    dwellMsAtSkipVisibility: 1, dwellMsAtLongVisibility: 0, observedAtMs: 0)

// MARK: - FeedPostActionsButton (feed ⋯ menu web-publishing verbs)

@MainActor
@Test func feedMenuPublishToWebCapturesItsGenerationBeforeTheAwait() async {
    await expectFeedSiteCapturesBeforeAwait { vm, call in
        await FeedPostActionsButton.publishToWeb(vm: vm) { try await call.park() }
    }
}

@MainActor
@Test func feedMenuUnpublishFromWebCapturesItsGenerationBeforeTheAwait() async {
    await expectFeedSiteCapturesBeforeAwait { vm, call in
        await FeedPostActionsButton.unpublishFromWeb(vm: vm) { try await call.park() }
    }
}

@MainActor
@Test func feedMenuMintPaywallLinkCapturesItsGenerationBeforeTheAwait() async {
    await expectFeedSiteCapturesBeforeAwait { vm, call in
        _ = await FeedPostActionsButton.mintPaywallLink(vm: vm, origin: "https://a.example.com") {
            try await call.parkThenThrow()
        }
    }
}

// MARK: - PostUnlockOfferTeaser

@MainActor
@Test func unlockTeaserBuyCapturesItsGenerationBeforeTheAwait() async {
    await expectFeedSiteCapturesBeforeAwait { vm, call in
        await PostUnlockOfferTeaser.buy(vm: vm) { try await call.park() }
    }
}

// MARK: - ComposeAttachButton

@MainActor
@Test func composeAttachUploadCapturesItsGenerationBeforeTheTaskAwaits() async {
    await expectFeedSiteCapturesBeforeAwait { vm, call in
        await ComposeAttachButton.upload(vm: vm, attach: { try await call.park() }, finally: {}).value
    }
}

// MARK: - CueViewportObserver

/// `hydrateIfNeeded`'s own capture: the hydrate it guards is the await.
@MainActor
@Test func cueHydrateCapturesItsGenerationBeforeTheAwait() async {
    await expectFeedSiteCapturesBeforeAwait { vm, call in
        await CueViewportObserver.hydrateIfNeeded(
            feedVM: vm, calls: cueCalls(hydrateCues: { try await call.park() }),
            hydrated: CueHydrationLedger(),
            onError: { generation, message in
                vm.landClientErrorMessage(generation: generation, message: message)
            })
    }
}

/// The Layer-B producer contributes only once the manager's cached opt-in is
/// hydrated (`engagement-cues.md` § Layer B), so feed init must hydrate it —
/// once per manager generation, beside the cue rollup — and a failure there
/// is non-fatal: it neither paints the feed nor stops the cue hydrate.
@MainActor
@Test func cueHydrateAlsoHydratesTheSignalOptInOncePerGeneration() async {
    let vm = FeedVM()
    let ledger = CueHydrationLedger()
    let optinCalls = Counter()
    let cueCalls_ = Counter()
    let calls = cueCalls(
        hydrateCues: { cueCalls_.bump() },
        hydrateSignalOptin: { optinCalls.bump(); throw StubFailure() })
    var painted: [String] = []
    for _ in 0..<2 {
        await CueViewportObserver.hydrateIfNeeded(
            feedVM: vm, calls: calls, hydrated: ledger,
            onError: { _, message in painted.append(message) })
    }
    #expect(optinCalls.value == 1, "the opt-in hydrates once per manager generation")
    #expect(cueCalls_.value == 1, "the cue hydrate is unaffected by the opt-in hydrate")
    #expect(painted.isEmpty, "an opt-in hydrate failure is logged, never painted into the feed")
}

/// `run`'s own capture feeds every `recordObservation` failure the loop and
/// its teardown drain report. Its first await is the hydrate: park it, switch
/// accounts, let it SUCCEED (so the hydrate's own `onError` never fires), then
/// cancel the loop so its teardown drain reports one observation whose record
/// fails — the failure must carry the pre-switch generation and be refused.
@MainActor
private func cueRunTeardownErrorSlot(resetMidAwait: Bool) async -> String? {
    let vm = FeedVM()
    let hydrate = ParkedCall()
    let reported = Latch()
    let calls = cueCalls(
        hydrateCues: { try await hydrate.park() },
        recordObservation: { _ in throw StubFailure() },
        drainAll: { _ in [anObservation] })
    let observer = CueViewportObserver()
    let running = Task {
        await observer.run(
            feedVM: vm, calls: calls, hydrated: CueHydrationLedger(),
            onError: { generation, message in
                vm.landClientErrorMessage(generation: generation, message: message)
                reported.signal()
            })
    }
    await hydrate.untilParked()
    if resetMidAwait { vm.reset() }
    running.cancel()
    hydrate.succeed()
    await running.value
    await reported.wait()   // the teardown emit runs in a detached task
    return vm.clientErrorMessage
}

@MainActor
@Test func cueRunCapturesItsGenerationBeforeItsFirstAwait() async {
    let control = await cueRunTeardownErrorSlot(resetMidAwait: false)
    #expect(control != nil,
            "control: with no account switch the teardown drain's record failure must reach the feed")
    let stale = await cueRunTeardownErrorSlot(resetMidAwait: true)
    #expect(stale == nil,
            "a record failure from a run that began before the account switch must not paint the incoming actor's feed")
}

// MARK: - WebSettingsView (Settings → Web published-posts rows)

@MainActor
@Test func webSettingsMintPaywallLinkCapturesItsGenerationBeforeTheAwait() async {
    await expectWebSiteCapturesBeforeAwait { store, call in
        _ = await WebSettingsView.mintPaywallLink(store: store, origin: "https://a.example.com") {
            try await call.parkThenThrow()
        }
    }
}

@MainActor
@Test func webSettingsUnpublishCapturesItsGenerationBeforeTheAwait() async {
    await expectWebSiteCapturesBeforeAwait { store, call in
        await WebSettingsView.unpublish(store: store) { try await call.park() }
    }
}
