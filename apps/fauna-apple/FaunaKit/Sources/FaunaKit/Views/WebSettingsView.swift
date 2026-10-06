import SwiftUI

/// The user-facing **web-settings** page (`docs/goal/behavior/web-content-hosting.md`
/// § Published-post management), shared by macOS + iOS (one FaunaKit view, thin
/// per-target mount points). A dumb renderer over the shared `WebPublishStore`
/// (`webPublish`, injected by the platform-specific call site — this view cannot
/// reach `AppState`/`MacAppState` directly, since the two types differ per
/// platform); no business logic here. Element IDs match
/// `tests/e2e-unified/ui.yaml` `web-settings` exactly.
///
/// Two surfaces (`web-content-hosting.md` § Published-post management):
///
/// - the **subdomain opt-in toggle** (`web-settings-subdomain-toggle`, default
///   OFF) that serves the user's `web` content at `https://<handle>.<domain>/`;
/// - the **Published-posts management section** below it —
///   `web-published-posts-list` of `web-published-post-item` rows from
///   `fauna.web.publish.list`, each with copy-link / copy-paywall-link (gated
///   rows only) / unpublish, plus `web-published-posts-empty`.
///
/// The admin uses this same page for their own site; the nest-wide apex
/// designation is the separate `admin-web` page. Reference renderers: linux
/// (`apps/fauna-linux/src/settings/web.rs`), web (`WebSettingsSection.svelte`),
/// windows (`SettingsWebPage`), tui (`apps/fauna-tui/src/settings/web.rs` — the
/// lead app; this view mirrors its `published_posts_elements` row shape).
public struct WebSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    let webPublish: WebPublishStore
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves
    /// it 0 (the NavigationLink re-mounts the view, re-running the load). A
    /// bare `.task {}` fires once per view IDENTITY, which a re-navigation to
    /// an ALREADY-mounted macOS page (`set_state`'s nav patch is a value
    /// write, not a real push/pop) does not create a new one of — so a
    /// second visit after publishing never re-read `publish.list`, same
    /// AdminWebView/DevicesView/FamilyView precedent.
    var reloadToken: Int = 0

    /// What each row actually put on the clipboard, keyed by post id — the
    /// `copied` attr's backing store. Per-post rather than per-index so a
    /// re-sort (a fresh publish landing at the top) can never point a stale
    /// index at the wrong row's copied value.
    @State private var copiedLinks: [Data: String] = [:]

    public init(webPublish: WebPublishStore, reloadToken: Int = 0) {
        self.webPublish = webPublish
        self.reloadToken = reloadToken
    }

    public var body: some View {
        // Eager `ScrollView { VStack { GroupBox } }`, NOT a lazy `Form` (rule 6 —
        // apple-e2e-automation.md § Registration rules): the Published-posts
        // section below the subdomain toggle is exactly the `MailSpamView`
        // shape — a control above a published-list section can push
        // it below the fold, where an iOS `Form` never registers it. Mirrors
        // the `GroupBox { VStack }` idiom every `Admin*View`/`MailSettingsView`
        // uses.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                groupedSection {
                    // Non-optimistic: `isOn` reads the nest-confirmed `vm.enabled`,
                    // so the switch's own state is the round-trip proof.
                    //
                    // A *plain-label* Toggle (not a custom-view label) is required so
                    // the `accessibilityIdentifier` lands on the underlying Switch:
                    // a `Toggle { VStack { … } }` attaches the id to a wrapper Group
                    // whose AX value is nil, leaving the Switch un-identified (verified
                    // via the AX tree). A string-label Toggle puts the id on the Switch
                    // (same as `settings-autostart-toggle`), whose native on/off value
                    // the apple-bridge maps to the cross-app "on"/"off" `state`
                    // contract (SwiftUI ignores `.accessibilityValue` on a Toggle, so
                    // the bridge reads the native switch value — see AppleBridge
                    // `Actions.getAttr`). The subtitle moves to a sibling caption row.
                    Toggle(L.webSettings.subdomainToggleLabel, isOn: Binding(
                        get: { webPublish.view?.enabled ?? false },
                        set: { on in Task { await webPublish.setSubdomainEnabled(on) } }
                    ))
                    .accessibilityIdentifier(Ids.webSettingsSubdomainToggle)
                    // In-process driver: one Entry so `/element/click` flips the
                    // opt-in and `/element/attr?attr=state` reads the cross-app
                    // "on"/"off" contract. Both re-read the nest-confirmed
                    // `webPublish.view?.enabled` live (non-optimistic — the value
                    // only flips once `setSubdomainEnabled` round-trips), so the
                    // toggle's own state is the round-trip proof the test asserts.
                    .automationActivate(
                        Ids.webSettingsSubdomainToggle,
                        value: { (webPublish.view?.enabled ?? false) ? "on" : "off" }
                    ) { Task { await webPublish.setSubdomainEnabled(!(webPublish.view?.enabled ?? false)) } }
                    // A dispatch-on-change toggle whose value is deliberately
                    // non-optimistic: it only flips once the nest round-trips, so
                    // with no nest it could never move — the gate says why.
                    .faunaGate("fauna.web.set_subdomain_enabled")

                    Text(L.webSettings.subdomainToggleSubtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    LabeledContent(L.webSettings.subdomainUrlLabel) {
                        automationText(Ids.webSettingsSubdomainUrl, urlText)
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }

                    // Information only — no button, and the copy affordances stay
                    // enabled (`web-content-hosting.md` § Routing, render, serving →
                    // *A blanked site tells its author*). Painted only while the
                    // nest's own `publish.list` says the rendered pages are down.
                    if webPublish.renderedPagesDown {
                        automationText(Ids.webSettingsRenderStatus, L.webSettings.renderStatusDown)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    Text(L.webSettings.contentInfo)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier(Ids.webSettingsContentInfo)
                }

                // The Published-posts section paints only once the page has actually
                // hydrated — `view` is the hydrate signal, mirroring tui's own gate.
                // Painting `web-published-posts-empty` before that would tell the
                // user "no published posts" about a list nobody has read.
                if webPublish.isHydrated {
                    publishedPostsSection
                }

                if let error = webPublish.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .pageTitle(L.webSettings.title)
        .task(id: reloadToken) {
            guard let client else { return }
            let handle = client.sessionMaterial?.handle ?? ""
            // Self-hydrates on EVERY visit (unconditional, not gated on
            // `isHydrated`): the opt-in and the published list are per-actor
            // nest state a sibling device may have just changed, the tui
            // `web_elements` "self-hydrates on every visit" shape.
            await webPublish.hydrate(api: client.api, handle: handle)
        }
    }

    /// The live URL (shown whether on or off — it's where the site would serve),
    /// else the disabled reason. Mirrors linux `web.rs::render`.
    private var urlText: String {
        if let url = webPublish.view?.url { return url }
        switch webPublish.view?.disabledReason {
        case .noHandle: return L.webSettings.subdomainNoHandle
        case .reservedLabel: return L.webSettings.subdomainReserved
        // The nest serves no web content at any host — say so rather than leave
        // the row blank (web-content-hosting.md § Published-post management:
        // legal but unreachable, and the UI must say so).
        case .noServingDomain: return L.webSettings.subdomainNoServingDomain
        case .none: return ""
        }
    }

    // ── The Published-posts management section ─────────────────────────────

    @ViewBuilder private var publishedPostsSection: some View {
        groupedSection {
            Text(L.webSettings.publishedPostsTitle)
                .font(.headline)

            // Wrapped in a plain VStack rather than a bare `if empty { A } else
            // { B }` group — the same shape `MutedWordsView`'s empty/list
            // toggle uses. (Historical note: this container originally guarded
            // against a `Form`'s lazy `List` backing on iOS registering the
            // empty/non-empty branches as two competing cell-reuse rows —
            // `web-published-posts-empty` and `web-published-posts-list` both
            // reading on-screen at once. Moot now that this page is an eager
            // `ScrollView { VStack }` (rule 6), which realizes no `List` at
            // all; kept because it's still the clearest shape for the
            // conditional.)
            VStack(alignment: .leading, spacing: 8) {
                if webPublish.posts.isEmpty {
                    automationText(Ids.webPublishedPostsEmpty, L.webSettings.publishedPostsEmpty)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                } else {
                    // Said once for the section rather than per row: the
                    // ratified ~10-minute validity, and the claim code as the
                    // durable alternative. Only when a gated row is actually
                    // present — otherwise it explains an affordance nothing
                    // on screen offers.
                    if webPublish.posts.contains(where: { $0.gatedTier != nil }) {
                        Text(L.webPublish.paywallLinkNote)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    // Why the copy buttons below are dead, in the user's own
                    // terms. Placed above the rows so it reads as a statement
                    // about the section, not about whichever row is last.
                    if webPublish.siteLink.origin == nil {
                        Text(Self.disabledReasonText(webPublish.siteLink.disabledReason))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    // The row container — the landmark
                    // `wait_for_published_posts_section` waits on alongside
                    // the empty state (a separate id from it, mirroring tui's
                    // `web-published-posts-list` marker, painted whenever the
                    // page has hydrated regardless of row count).
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(webPublish.posts.enumerated()), id: \.element.postId) { index, post in
                            publishedPostRow(post, index: index)
                        }
                    }
                    .accessibilityIdentifier(Ids.webPublishedPostsList)
                    .automationValue(Ids.webPublishedPostsList, text: { "" })
                }
            }
        }
    }

    @ViewBuilder
    private func publishedPostRow(_ post: FfiPublishedPost, index: Int) -> some View {
        let origin = webPublish.siteLink.origin
        VStack(alignment: .leading, spacing: 4) {
            automationText(Ids.webPublishedPostSlug, post.slug)
                .font(.callout.monospaced())

            HStack(spacing: 12) {
                Button(L.webPublish.copyWebLink) {
                    copyWebLink(post, origin: origin)
                }
                .buttonStyle(.borderless)
                .font(.caption)
                .disabled(origin == nil)
                .accessibilityIdentifier(Ids.webPublishedPostCopyLinkButton)
                .automationActivate(
                    Ids.webPublishedPostCopyLinkButton,
                    isEnabled: { origin != nil },
                    value: { copiedLinks[post.postId] }
                ) { copyWebLink(post, origin: origin) }

                // Gated rows only: an ungated post has no paywalled body to
                // hand out, so the affordance would mint a token for nothing.
                if post.gatedTier != nil {
                    Button(L.webPublish.copyPaywallLink) {
                        Task { await copyPaywallLink(post) }
                    }
                    .buttonStyle(.borderless)
                    .font(.caption)
                    .disabled(origin == nil)
                    .accessibilityIdentifier(Ids.webPublishedPostCopyPaywallLinkButton)
                    .automationActivate(
                        Ids.webPublishedPostCopyPaywallLinkButton,
                        isEnabled: { origin != nil },
                        value: { copiedLinks[post.postId] }
                    ) { Task { await copyPaywallLink(post) } }
                    // The plain link copy and the unpublish beside it stay live
                    // (a resolved URL, and an `OfflineSafe` takedown); only the
                    // token MINT needs the nest.
                    .faunaGate("fauna.web.paywall.mint_token")
                }

                // Always live: a takedown needs no serving origin, and it is
                // the one thing a user with an unreachable site may well want.
                Button(L.webPublish.unpublish) {
                    Task { await unpublish(post) }
                }
                .buttonStyle(.borderless)
                .font(.caption)
                .accessibilityIdentifier(Ids.webPublishedPostUnpublishButton)
                .automationActivate(Ids.webPublishedPostUnpublishButton) {
                    Task { await unpublish(post) }
                }
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.webPublishedPostItem)
        // The container's own registration: `AutomationRegistry` needs an
        // explicit `automation*` modifier to see a container at all (a bare
        // `.accessibilityIdentifier` + `.contain` is invisible to it), and this
        // is where the row's `gated-tier` DATA attr lives — an assertion on "is
        // this row gated" reads a field rather than matching a translated
        // badge string.
        .automationValue(Ids.webPublishedPostItem, value: { post.gatedTier ?? "" })
        // The scope container every leaf above nests under, so
        // `scope="web-published-post-item[i]"` resolves by real subtree
        // containment (the `DeviceCard`/`BackupDestinationsView` precedent).
        .automationScope(Ids.webPublishedPostItem, index: index)
    }

    /// Purely local — the origin and the slug are both already resolved on
    /// screen, so the public link costs no round trip. Nothing is copied when
    /// there is no serving origin: the button paints disabled in that case,
    /// and this refuses independently rather than trusting the paint (a dead
    /// link on the clipboard is worse than no copy).
    private func copyWebLink(_ post: FfiPublishedPost, origin: String?) {
        guard let origin else { return }
        let url = webPostPageUrl(origin: origin, slug: post.slug)
        Pasteboard.copy(url)
        copiedLinks[post.postId] = url
    }

    /// A fresh mint per click: the token is short-lived by ratified design and
    /// re-minting is free, so re-copying always yields a link that works from
    /// now, never a cached one that already expired.
    private func copyPaywallLink(_ post: FfiPublishedPost) async {
        guard let origin = webPublish.siteLink.origin else { return }
        let minted = await Self.mintPaywallLink(store: webPublish, origin: origin) {
            try await webPublish.mintPaywallLink(slug: post.slug)
        }
        guard let url = minted else { return }
        Pasteboard.copy(url)
        copiedLinks[post.postId] = url
    }

    /// `copyPaywallLink`'s guarded body, with the awaited mint passed in as a
    /// closure — the unit-tier seam `CallSiteCaptureOrderingTests` drives with
    /// a mint that suspends across a `reset()` and then throws. The generation is captured before the
    /// await, same as `hydrate`'s own generation capture
    /// (`WebPublishStore.hydrate`'s doc comment): `reset()` may run while this
    /// call is suspended, and a stale landing must not paint the outgoing
    /// actor's error over the incoming actor's fresh store
    /// (`account-scoping.md` § The scoping taxonomy). Returns the tokened URL on success, `nil` once the failure has
    /// landed.
    @MainActor
    static func mintPaywallLink(
        store: WebPublishStore, origin: String, mint: () async throws -> MintedPaywallLink
    ) async -> String? {
        let generation = store.currentGeneration
        do {
            let minted = try await mint()
            return webTokenedUrl(origin: origin, path: minted.path, token: minted.token)
        } catch {
            guard let text = DisplayError.message(error) else { return nil }
            store.landActionFailure(
                generation: generation, message: L.webPublish.errorPaywallLink(message: text))
            return nil
        }
    }

    private func unpublish(_ post: FfiPublishedPost) async {
        await Self.unpublish(store: webPublish) {
            try await webPublish.unpublish(postId: post.postId)
        }
    }

    /// `unpublish`'s guarded body — same seam and generation guard as
    /// `mintPaywallLink(store:origin:mint:)` above.
    @MainActor
    static func unpublish(store: WebPublishStore, unpublish: () async throws -> Void) async {
        let generation = store.currentGeneration
        do {
            try await unpublish()
        } catch {
            guard let text = DisplayError.message(error) else { return }
            store.landActionFailure(
                generation: generation, message: L.webPublish.errorUnpublish(message: text))
        }
    }

    /// The human-readable "your posts have no public address, and here's why"
    /// line for a `SiteLinkView` with no origin — this section's own wording
    /// (the feed ⋯-menu names a different, menu-appropriate reason instead,
    /// since it cannot point at "the toggle above" from a different page).
    /// Delegates to the shared `fauna_client_web::disabled_reason_text`
    /// decision (linux/tui's own door) via `webDisabledReasonText` rather than
    /// re-deriving the key mapping by hand.
    private static func disabledReasonText(_ reason: SiteLinkDisabledReason?) -> String {
        renderLocalizedText(webDisabledReasonText(reason: reason))
    }
}

// `groupedSection` (`GroupedSection.swift`) is the shared eager-container
// replacement for `Form`'s `Section` this file uses.
