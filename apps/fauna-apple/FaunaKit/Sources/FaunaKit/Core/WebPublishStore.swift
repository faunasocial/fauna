import Foundation

/// App-scoped cache of the `fauna.web.*` published-post surface's read inputs —
/// shared by **both** the `web-settings` Published-posts section and the feed
/// ⋯-overflow's own-post web-publishing verbs, so the two can never disagree
/// about a creator's serving origin (`docs/goal/behavior/web-content-hosting.md`
/// § Published-post management; `ui/feed.md` § User actions). Mirrors tui's
/// `WebSettingsState` + shared `fauna_client_web::read_web_page` — the apple
/// twin of the same "one read, not two answers" rule.
///
/// Held by each shell's app state beside `ContentPolicyStore`/`FamilyStatusStore`
/// and injected into the SwiftUI environment at the app root (via the platform
/// `AppState`/`MacAppState`), then threaded explicitly into the shared
/// `WebSettingsView` and `FeedPostActionsButton` — both are platform-agnostic
/// FaunaKit views that cannot reach `AppState`/`MacAppState` directly (the two
/// types differ per platform), so the call site that instantiates them (which
/// DOES have the right `@Environment`) passes this store down.
///
/// **Hydrate semantics differ by caller, on purpose.** The settings page
/// self-hydrates on **every** visit (`hydrate` is unconditional) — the opt-in
/// and the published-posts list are per-actor nest state a sibling device may
/// have changed, and the view IS the whole render source. The feed menu
/// hydrates **lazily and once** (`isHydrated` gates it) — painting a *disabled*
/// copy verb off unread state would tell a creator with a working address that
/// they have none, so the menu reads these inputs once, through this store's
/// own `hydrate`, rather than growing a second answer.
///
/// Not actor-isolated for the same reason as `ContentPolicyStore`/
/// `FamilyStatusStore`: both `AppState` and `MacAppState` construct it in a
/// stored property, which a `@MainActor` initializer could not serve. Mutation
/// is pinned to the main actor per-method instead.
@Observable
public final class WebPublishStore {
    /// The subdomain toggle's projected view; `nil` until the first hydrate.
    public private(set) var view: SubdomainView?
    /// `fauna.web.domain.get` rows, narrowed to what `site_link_view` takes.
    public private(set) var domains: [WebDomainRow] = []
    /// `fauna.web.publish.list` — the `web-published-posts-list` rows.
    public private(set) var posts: [FfiPublishedPost] = []
    /// The nest cleared this actor's rendered pages after a failed render and
    /// is restoring them by itself — read off the SAME `publish.list` answer as
    /// `posts` (`publishedSite`, never a second RPC), so the `web-settings`
    /// status line (`web-settings-render-status`) can never disagree with the
    /// list beside it (`web-content-hosting.md` § Routing, render, serving →
    /// *A blanked site tells its author*). `false` before the first hydrate.
    public private(set) var renderedPagesDown = false
    /// The host THIS nest itself routes web content on (`fauna.nest.info`),
    /// read fresh at every hydrate so `siteLink` resolves off the same value
    /// `view` was projected from. Empty ⇒ this nest serves no web content.
    public private(set) var servingDomain: String = ""
    public private(set) var errorMessage: String?

    private var client: FfiWebClient?
    private var handle: String = ""

    /// Bumped by `reset()`, captured at the top of `hydrate` and re-checked
    /// before every write that lands after an await — the async-landing half
    /// of the account-scoping in-memory corollary (`account-scoping.md` §
    /// The scoping taxonomy). A stale landing must
    /// not reinstall the outgoing actor's client or state into the incoming
    /// actor's store. Mirrors android's `WebPublishStore.generation`
    /// (`WebPublishStore.kt:73`).
    private var generation = 0

    public init() {}

    public var isHydrated: Bool { view != nil }

    /// Where this actor's published content is reachable — precedence
    /// **active custom domain > enabled subdomain** — resolved fresh on every
    /// read from the two halves this store holds, so the toggle above and the
    /// links below can never disagree (`fauna_client_web::site_link_view`,
    /// every app renders it identically).
    public var siteLink: SiteLinkView {
        webSiteLinkView(
            domains: domains,
            subdomainEnabled: view?.enabled ?? false,
            handle: handle.isEmpty ? nil : handle,
            domain: servingDomain)
    }

    /// Drop every field to the pre-hydrate state and retire any hydrate
    /// already in flight — the account-switch teardown's own seam, reached
    /// from `ActorScope.dropAppOwnedState` via `ActorScopedAppCaches` on
    /// BOTH shells (one call reaches iOS's and macOS's `webPublish` alike),
    /// so an outgoing actor's serving origin, published posts and
    /// `FfiWebClient` never survive into the incoming actor's feed menu or
    /// Settings → Web page (`account-scoping.md` § The scoping taxonomy).
    /// Mirrors android's `WebPublishStore`'s closer (`WebPublishStore.kt:76-83`).
    @MainActor
    public func reset() {
        view = nil
        domains = []
        posts = []
        renderedPagesDown = false
        servingDomain = ""
        errorMessage = nil
        client = nil
        handle = ""
        generation &+= 1
    }

    /// Read every input the section + the menu need: the subdomain opt-in, the
    /// custom-domain rows, the nest's serving domain, and the published-posts
    /// list. Sequential and fail-fast, mirroring the shared
    /// `fauna_client_web::read_web_page` this reimplements over the UniFFI
    /// seam (that helper is generic over `RpcRequester` and so cannot itself
    /// cross UniFFI) — nothing is partially applied: a failed read leaves
    /// whatever this store already held, rather than clearing to empty.
    @MainActor
    public func hydrate(api: APIClient, handle: String) async {
        self.handle = handle
        // Captured before any await and re-checked by `landHydrate`/
        // `landHydrateFailure` before every write that lands after one:
        // `reset()`'s account-switch drop may run while this call is
        // suspended, and a stale landing must not reinstall the outgoing
        // actor's client or state into the incoming actor's store (see
        // `generation`'s doc comment above).
        let generation = self.generation
        let webClient: FfiWebClient
        do {
            // ALWAYS rebuilt, never cached across calls: a module-scoped e2e app
            // process re-logs into a DIFFERENT nest/actor mid-run (`web_hosting_app`
            // over the same long-lived `app` fixture the earlier `logged_in_app`
            // tests already hydrated this store against), and a cached `client`
            // would keep querying the FIRST nest's now-swapped-out connection —
            // silently returning stale/empty state forever after (found via
            // `test_published_post_management_section` failing only when run
            // after another web-settings visit in the same module, never alone).
            // `buildWebClient` is a cheap `Arc<NestClient>` wrap, not a real
            // connection — nothing is saved by caching it.
            webClient = try await api.webClient()
        } catch {
            landHydrateFailure(generation: generation, error: error)
            return
        }
        do {
            let servingDomain = try await webClient.servingDomain()
            let enabled = try await webClient.getSubdomainEnabled()
            let domains = try await webClient.domainGet()
            let site = try await webClient.publishedSite()
            landHydrate(
                generation: generation,
                client: webClient,
                servingDomain: servingDomain,
                view: webSubdomainView(
                    enabled: enabled, handle: handle.isEmpty ? nil : handle, domain: servingDomain),
                domains: domains,
                posts: site.posts,
                renderedPagesDown: site.renderedPagesDown)
        } catch {
            landHydrateFailure(generation: generation, error: error)
        }
    }

    /// The current account-switch generation — `internal` so
    /// `WebPublishStoreTests` can capture it before simulating an in-flight
    /// hydrate's landing, the same way `hydrate` itself does above, AND so
    /// `WebSettingsView`'s own `do/catch` sites can capture it before their
    /// `try await publish/unpublish/mintPaywallLink` the same way `hydrate`
    /// captures it before its own await — those three throw a raw `Error`
    /// for the caller to localize, so the caller (not this store) is the one
    /// positioned to capture the pre-await generation `landActionFailure`
    /// below then re-checks (`account-scoping.md` § The scoping taxonomy). Not `public`: only this module's views (in
    /// FaunaKit, same as this file) write through it.
    var currentGeneration: Int { generation }

    /// Land a successful hydrate read, refusing if `generation` no longer
    /// matches this store's current one — the async-landing half of the
    /// account-switch guard (see `generation`'s doc comment above).
    /// `internal` so the race is directly testable without a live
    /// `FfiWebClient` (`WebPublishStoreTests.swift`); `hydrate`'s second
    /// `await` chain is this store's only other caller.
    @discardableResult
    @MainActor
    func landHydrate(
        generation: Int, client: FfiWebClient?, servingDomain: String,
        view: SubdomainView?, domains: [WebDomainRow], posts: [FfiPublishedPost],
        renderedPagesDown: Bool = false
    ) -> Bool {
        guard generation == self.generation else { return false }
        self.client = client
        self.servingDomain = servingDomain
        self.domains = domains
        self.posts = posts
        self.renderedPagesDown = renderedPagesDown
        self.view = view
        errorMessage = nil
        return true
    }

    /// The failure twin of `landHydrate` above — same guard, for the error
    /// message alone.
    @MainActor
    func landHydrateFailure(generation: Int, error: Error) {
        guard generation == self.generation else { return }
        errorMessage = DisplayError.message(error)
    }

    /// Re-read `fauna.web.publish.list` alone — after a publish/unpublish/mint,
    /// never a full `hydrate` (the toggle + domains did not change).
    @MainActor
    public func refreshPosts() async {
        guard let client else { return }
        // Same generation guard as `hydrate` (see `generation`'s doc comment
        // above): `reset()` may run while this await is suspended, and a
        // stale landing must not reinstall the outgoing actor's posts.
        let generation = self.generation
        do {
            // The whole site, not just the rows: a render the takedown just
            // restored must clear the status line with no re-visit.
            let site = try await client.publishedSite()
            landRefreshPosts(
                generation: generation, posts: site.posts,
                renderedPagesDown: site.renderedPagesDown)
        } catch {
            landRefreshPostsFailure(generation: generation, error: error)
        }
    }

    /// Land a successful `refreshPosts` read, refusing if `generation` no
    /// longer matches this store's current one — same guard as `landHydrate`.
    /// `internal` so the race is directly testable (`WebPublishStoreTests.swift`).
    @discardableResult
    @MainActor
    func landRefreshPosts(
        generation: Int, posts: [FfiPublishedPost], renderedPagesDown: Bool = false
    ) -> Bool {
        guard generation == self.generation else { return false }
        self.posts = posts
        self.renderedPagesDown = renderedPagesDown
        return true
    }

    /// The failure twin of `landRefreshPosts` above.
    @MainActor
    func landRefreshPostsFailure(generation: Int, error: Error) {
        guard generation == self.generation else { return }
        errorMessage = DisplayError.message(error)
    }

    /// Flip the opt-in, then re-project `view` from the nest's echoed state
    /// (non-optimistic — the toggle's own state is the round-trip proof).
    @MainActor
    public func setSubdomainEnabled(_ enabled: Bool) async {
        guard let client else { return }
        // Same generation guard as `hydrate` (see `generation`'s doc comment
        // above): `reset()` may run while this await is suspended, and a
        // stale landing must not reinstall the outgoing actor's toggle state.
        let generation = self.generation
        do {
            let confirmed = try await client.setSubdomainEnabled(enabled: enabled)
            landSetSubdomainEnabled(generation: generation, confirmed: confirmed)
        } catch {
            landSetSubdomainEnabledFailure(generation: generation, error: error)
        }
    }

    /// Land a successful `setSubdomainEnabled` echo, refusing if `generation`
    /// no longer matches this store's current one — same guard as
    /// `landHydrate`. `internal` so the race is directly testable
    /// (`WebPublishStoreTests.swift`).
    @discardableResult
    @MainActor
    func landSetSubdomainEnabled(generation: Int, confirmed: Bool) -> Bool {
        guard generation == self.generation else { return false }
        view = webSubdomainView(
            enabled: confirmed, handle: handle.isEmpty ? nil : handle, domain: servingDomain)
        errorMessage = nil
        return true
    }

    /// The failure twin of `landSetSubdomainEnabled` above.
    @MainActor
    func landSetSubdomainEnabledFailure(generation: Int, error: Error) {
        guard generation == self.generation else { return }
        errorMessage = DisplayError.message(error)
    }

    /// `fauna.web.publish.set` — publish one of the caller's own posts under
    /// the nest's default slug. Refreshes `posts` on success so every reader
    /// of this store (both surfaces) sees the new row without a second call.
    @MainActor
    public func publish(postId: Data) async throws -> String {
        guard let client else { throw WebPublishStoreError.notReady }
        let slug = try await client.publishSet(postId: postId, slug: nil)
        await refreshPosts()
        return slug
    }

    /// `fauna.web.publish.unset` — idempotent and reversible, hence no
    /// destructive-confirm step at either call site.
    @MainActor
    public func unpublish(postId: Data) async throws {
        guard let client else { throw WebPublishStoreError.notReady }
        _ = try await client.publishUnset(postId: postId)
        await refreshPosts()
    }

    /// `fauna.web.paywall.mint_token` for a published post's slug — a fresh
    /// mint per call (the token is short-lived by ratified design, and
    /// re-minting is free, so a caller never hands out an already-expired
    /// cached value).
    @MainActor
    public func mintPaywallLink(slug: String) async throws -> MintedPaywallLink {
        guard let client else { throw WebPublishStoreError.notReady }
        return try await client.paywallMintToken(target: .postSlug(slug: slug))
    }

    /// Land a caller-localized failure message for `publish`/`unpublish`/
    /// `mintPaywallLink`, refusing if `generation` no longer matches this
    /// store's current one — the same account-switch guard as
    /// `landHydrateFailure`/`landRefreshPostsFailure`/
    /// `landSetSubdomainEnabledFailure`, but exposed (rather than internal to
    /// one of this store's own methods) because those three throw a raw
    /// `Error` and leave the LOCALIZED message to the caller (`WebSettingsView`
    /// picks `L.webPublish.errorPaywallLink`/`errorUnpublish`; this store has
    /// no localization strings of its own to pick with). The caller captures
    /// `currentGeneration` before its `try await`, exactly as `hydrate` does
    /// internally, and lands the formatted string through this seam instead
    /// of writing `errorMessage` directly — which the private setter above
    /// now forbids (`account-scoping.md` § The scoping taxonomy). `internal`: `FeedPostActionsButton`'s own
    /// catch sites write `FeedVM.clientErrorMessage` instead (a different
    /// store, guarded by `FeedVM.managerGeneration` — see
    /// `FeedVM.landClientErrorMessage`), since an own-post action's error
    /// belongs on the feed page, not this one.
    @discardableResult
    @MainActor
    func landActionFailure(generation: Int, message: String) -> Bool {
        guard generation == self.generation else { return false }
        errorMessage = message
        return true
    }
}

public enum WebPublishStoreError: Error {
    /// A mutation was attempted before `hydrate` ever built the underlying
    /// `FfiWebClient` — cannot happen from either shipped call site (the
    /// settings page hydrates in `.task`; the feed menu hydrates on open),
    /// but named rather than force-unwrapped for callers that arrive some
    /// other way.
    case notReady
}
