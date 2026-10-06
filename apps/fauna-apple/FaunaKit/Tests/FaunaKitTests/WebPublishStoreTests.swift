import Foundation
import Testing
@testable import FaunaKit

// `WebPublishStore`'s account-switch generation guard (account-scoping.md §
// The scoping taxonomy → the in-memory corollary):
// an in-flight `hydrate` captured as outgoing actor A must not land its read
// after `reset()` has already retired that generation for incoming actor B.
//
// `hydrate(api:handle:)` itself needs a live
// `FfiWebClient` and so isn't driven directly here; `landHydrate`/
// `landHydrateFailure` are the pure guarded-landing steps it calls after each
// await, extracted so the race is testable without one — mirrors
// `ContentPolicyStoreRefreshTests.swift`'s extraction of `merged` for the
// same reason.

private func aView(domain: String = "a.example.com") -> SubdomainView {
    SubdomainView(enabled: true, url: "https://\(domain)/", disabledReason: nil)
}

/// The headline: a write captured before `reset()` must not land after it —
/// deleting the `generation == self.generation` guard in `landHydrate` turns
/// this red.
@MainActor
@Test func aLandingWriteCapturedBeforeResetIsRefusedAfterReset() {
    let store = WebPublishStore()
    let generation = store.currentGeneration

    store.reset()

    let landed = store.landHydrate(
        generation: generation, client: nil, servingDomain: "a.example.com",
        view: aView(), domains: [], posts: [])

    #expect(!landed, "an A-era hydrate must not reinstall state after the account-switch drop")
    #expect(store.servingDomain.isEmpty, "the outgoing actor's serving domain must not land")
    #expect(!store.isHydrated, "the outgoing actor's view must not land")
}

/// The mirror-image case: a write captured at the CURRENT generation (the
/// ordinary path, no switch in between) must still land.
@MainActor
@Test func aLandingWriteAtTheCurrentGenerationDoesLand() {
    let store = WebPublishStore()
    let generation = store.currentGeneration

    let landed = store.landHydrate(
        generation: generation, client: nil, servingDomain: "b.example.com",
        view: aView(domain: "b.example.com"),
        domains: [WebDomainRow(domain: "b.example.com", status: "active")],
        posts: [])

    #expect(landed)
    #expect(store.servingDomain == "b.example.com")
    #expect(store.isHydrated)
    #expect(store.domains == [WebDomainRow(domain: "b.example.com", status: "active")])
}

/// The failure twin: an A-era hydrate's error must not paint over B's fresh,
/// still-unhydrated store — `landHydrateFailure` needs the same guard as
/// `landHydrate`, not just the success path (the correction the judge made:
/// the guard covers every write that lands after an await, not only the
/// field-assignment block).
@MainActor
@Test func aFailureLandingAfterResetDoesNotWriteTheErrorMessage() {
    let store = WebPublishStore()
    let generation = store.currentGeneration
    store.reset()

    store.landHydrateFailure(generation: generation, error: WebPublishStoreError.notReady)

    #expect(store.errorMessage == nil,
            "an A-era failure must not paint an error for actor B's fresh hydrate")
}

/// `reset()` itself: every field back to its pre-hydrate default, and the
/// generation bumped so an in-flight landing (tested above) is refused.
@MainActor
@Test func resetClearsEveryFieldAndBumpsTheGeneration() {
    let store = WebPublishStore()
    let generation = store.currentGeneration
    store.landHydrate(
        generation: generation, client: nil, servingDomain: "a.example.com",
        view: aView(), domains: [WebDomainRow(domain: "a.example.com", status: "active")],
        posts: [])
    #expect(store.isHydrated, "sanity: the store is actually hydrated before the reset")

    store.reset()

    #expect(!store.isHydrated)
    #expect(store.domains.isEmpty)
    #expect(store.posts.isEmpty)
    #expect(store.servingDomain.isEmpty)
    #expect(store.errorMessage == nil)
    #expect(store.currentGeneration == generation + 1)
}

/// Safe to call when nothing was ever hydrated — several account-switch
/// teardown sites can run from a partially-built session (mirrors
/// `ActorScopeTests`'s idempotent-drop coverage).
@MainActor
@Test func resetIsSafeWhenNothingWasHydrated() {
    let store = WebPublishStore()
    store.reset()
    store.reset()

    #expect(!store.isHydrated)
    #expect(store.domains.isEmpty)
    #expect(store.servingDomain.isEmpty)
}

// `refreshPosts` and `setSubdomainEnabled` write after an await too, same as
// `hydrate` — the same account-switch generation guard, pulled
// out into `landRefreshPosts`/`landSetSubdomainEnabled` so the race is
// testable without a live `FfiWebClient`, mirroring `landHydrate` above.

/// A stale posts refresh captured before `reset()` must not land after it —
/// deleting the guard in `landRefreshPosts` turns this red.
@MainActor
@Test func aRefreshPostsLandingCapturedBeforeResetIsRefusedAfterReset() {
    let store = WebPublishStore()
    let generation = store.currentGeneration

    store.reset()

    let landed = store.landRefreshPosts(
        generation: generation,
        posts: [FfiPublishedPost(postId: Data([1]), slug: "a-post", gatedTier: nil)])

    #expect(!landed, "an A-era posts refresh must not reinstall state after the account-switch drop")
    #expect(store.posts.isEmpty, "the outgoing actor's posts must not land")
}

/// The mirror-image case: a posts refresh landed at the CURRENT generation
/// must still apply.
@MainActor
@Test func aRefreshPostsLandingAtTheCurrentGenerationDoesLand() {
    let store = WebPublishStore()
    let generation = store.currentGeneration
    let posts = [FfiPublishedPost(postId: Data([1]), slug: "a-post", gatedTier: nil)]

    let landed = store.landRefreshPosts(generation: generation, posts: posts)

    #expect(landed)
    #expect(store.posts == posts)
}

/// The failure twin: a stale posts-refresh error must not paint over B's
/// fresh store.
@MainActor
@Test func aRefreshPostsFailureLandingAfterResetDoesNotWriteTheErrorMessage() {
    let store = WebPublishStore()
    let generation = store.currentGeneration
    store.reset()

    store.landRefreshPostsFailure(generation: generation, error: WebPublishStoreError.notReady)

    #expect(store.errorMessage == nil,
            "an A-era posts-refresh failure must not paint an error for actor B's fresh store")
}

/// A stale toggle echo captured before `reset()` must not land after it —
/// deleting the guard in `landSetSubdomainEnabled` turns this red.
@MainActor
@Test func aSetSubdomainEnabledLandingCapturedBeforeResetIsRefusedAfterReset() {
    let store = WebPublishStore()
    let generation = store.currentGeneration

    store.reset()

    let landed = store.landSetSubdomainEnabled(generation: generation, confirmed: true)

    #expect(!landed, "an A-era toggle echo must not reinstall state after the account-switch drop")
    #expect(!store.isHydrated, "the outgoing actor's view must not land")
}

/// The mirror-image case: a toggle echo landed at the CURRENT generation
/// must still apply.
@MainActor
@Test func aSetSubdomainEnabledLandingAtTheCurrentGenerationDoesLand() {
    let store = WebPublishStore()
    let generation = store.currentGeneration

    let landed = store.landSetSubdomainEnabled(generation: generation, confirmed: true)

    #expect(landed)
    #expect(store.isHydrated)
    #expect(store.view?.enabled == true)
}

/// The blanked-site status line reads the flag off the same landing as the
/// rows: a hydrate that lands `renderedPagesDown` raises it, a posts refresh
/// that lands the restored site clears it, and `reset()` drops it.
@MainActor
@Test func theRenderedPagesDownFlagFollowsEveryLandingAndResets() {
    let store = WebPublishStore()
    #expect(!store.renderedPagesDown, "before the first hydrate nothing says the site is down")

    store.landHydrate(
        generation: store.currentGeneration, client: nil, servingDomain: "a.example.com",
        view: aView(), domains: [], posts: [], renderedPagesDown: true)
    #expect(store.renderedPagesDown)

    store.landRefreshPosts(
        generation: store.currentGeneration, posts: [], renderedPagesDown: false)
    #expect(!store.renderedPagesDown, "a restoring render must clear the line")

    store.landRefreshPosts(
        generation: store.currentGeneration, posts: [], renderedPagesDown: true)
    store.reset()
    #expect(!store.renderedPagesDown)
}

/// The failure twin: a stale toggle-echo error must not paint over B's fresh
/// store.
@MainActor
@Test func aSetSubdomainEnabledFailureLandingAfterResetDoesNotWriteTheErrorMessage() {
    let store = WebPublishStore()
    let generation = store.currentGeneration
    store.reset()

    store.landSetSubdomainEnabledFailure(generation: generation, error: WebPublishStoreError.notReady)

    #expect(store.errorMessage == nil,
            "an A-era toggle-echo failure must not paint an error for actor B's fresh store")
}

// `publish`/`unpublish`/`mintPaywallLink` throw a raw `Error` and leave
// localization to the caller (`WebSettingsView`), so THEIR write-after-an-await
// guard is `landActionFailure` — the caller-facing twin of the internal
// landers above, pinned the same way.

/// The headline: a caller-formatted failure captured before `reset()` must
/// not land after it — deleting the `generation == self.generation` guard in
/// `landActionFailure` turns this red.
@MainActor
@Test func anActionFailureLandingAfterResetDoesNotWriteTheErrorMessage() {
    let store = WebPublishStore()
    let generation = store.currentGeneration
    store.reset()

    let landed = store.landActionFailure(generation: generation, message: "actor A's paywall-link error")

    #expect(!landed, "an A-era action failure must not land after the account-switch drop")
    #expect(store.errorMessage == nil,
            "an A-era action failure must not paint an error for actor B's fresh store")
}

/// The mirror-image case: an action failure captured at the CURRENT
/// generation (the ordinary path, no switch in between) must still land.
@MainActor
@Test func anActionFailureLandingAtTheCurrentGenerationDoesLand() {
    let store = WebPublishStore()
    let generation = store.currentGeneration

    let landed = store.landActionFailure(generation: generation, message: "a paywall-link error")

    #expect(landed)
    #expect(store.errorMessage == "a paywall-link error")
}
