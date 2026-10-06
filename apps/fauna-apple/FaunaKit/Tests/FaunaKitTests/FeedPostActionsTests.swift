import Foundation
import Testing
@testable import FaunaKit

// `FeedPostActionsButton`'s own-post web-publishing verbs
// (`publishToWeb`/`unpublishFromWeb`/`copyPaywallLink`), `PostUnlockOfferTeaser.buy`,
// `ComposeAttachButton`'s post-attach catch, and `CueViewportObserver`'s
// `onError` all write a failure into `FeedVM.clientErrorMessage`, the feed
// page's own error slot — every one of them a write that lands after an
// await/Task/FFI callback, the same account-switch race
// `WebPublishStore.landActionFailure` closes for its own `errorMessage`
// (`account-scoping.md` § The scoping taxonomy). `FeedVM.landClientErrorMessage` is the
// guarded, directly-testable seam every one of those call sites lands
// through instead of writing `clientErrorMessage` unconditionally (the
// setter is `private(set)`, so no other path compiles);
// `FeedVM.reset()` bumps `managerGeneration` unconditionally (even with no
// manager ever configured — `FeedVM.swift:241`), so this needs no live
// `FfiFeedManager`, mirroring `WebPublishStoreTests`'s own extraction of the
// pure guarded landers. One guard, one test pair: every call site above
// shares this exact implementation, so pinning it here pins the GUARD for all
// of them — deleting it from `landClientErrorMessage` turns every one of their
// failure paths silently wrong, not just this test. Whether each call site
// captures its generation BEFORE its await is the other half, which this
// pair cannot see; `CallSiteCaptureOrderingTests` executes it per site.

/// The headline: a write captured before `reset()` must not land after it —
/// deleting the `generation == managerGeneration` guard in
/// `FeedVM.landClientErrorMessage` turns this red.
@MainActor
@Test func aFeedClientErrorLandingCapturedBeforeResetIsRefusedAfterReset() {
    let vm = FeedVM()
    let generation = vm.managerGeneration

    vm.reset()

    let landed = vm.landClientErrorMessage(
        generation: generation, message: "actor A's paywall-link error")

    #expect(!landed, "an A-era action failure must not land after the account-switch drop")
    #expect(vm.clientErrorMessage == nil,
            "an A-era action failure must not paint an error for actor B's fresh feed")
}

/// The mirror-image case: a write captured at the CURRENT generation (the
/// ordinary path, no switch in between) must still land.
@MainActor
@Test func aFeedClientErrorLandingAtTheCurrentGenerationDoesLand() {
    let vm = FeedVM()
    let generation = vm.managerGeneration

    let landed = vm.landClientErrorMessage(generation: generation, message: "a paywall-link error")

    #expect(landed)
    #expect(vm.clientErrorMessage == "a paywall-link error")
}
