import SwiftUI

/// The feed post card's ⋯ overflow: the trained-topic-factor training verbs
/// (topic-factors.md § Authoring surface & picker), applied to posts —
/// `DmMessageBubble`'s `dm-message-actions-button`/`-menu` precedent. Inline
/// `@State`-driven (NOT a system `Menu`), so every id attaches to a real,
/// in-process-registered element. Shared by the macOS and iOS post cards
/// (identical logic; only the card layout that embeds it differs).
///
/// Mirrors windows' `PostActionsButton_Click`/`OpenTrainTargetSheet` (the
/// richest reference): tapping a verb trains **in-context** when the feed's
/// composition singles out one trained factor (`FeedManager::train_target_factor`);
/// otherwise it opens the target sheet (existing trained factors + a "New
/// trained topic…" link-out to the Personalization home — windows' richer
/// shape over linux's existing-factors-only sheet).
public struct FeedPostActionsButton: View {
    public let postId: String
    public let vm: FeedVM
    /// Whether the caller authored this post (`post.author == ` the caller's own
    /// actor id hex) — gates the destructive `feed-post-delete-button` row
    /// (feed.md § State & data shape → Post deletion, IDs user-approved
    /// 2026-07-16). Computed by the platform card view (`PostCardView` /
    /// `MacPostCardView`), which hold the platform-specific `AppState`/
    /// `MacAppState` this shared component doesn't have access to — mirrors
    /// `ProfileView`'s own `session: SessionState` init parameter.
    public let isOwn: Bool
    /// `PostSummary.webSlug` — the publish-state twin of `gatedTier` below
    /// (`None` = unpublished). Drives the own-post web-publishing verbs'
    /// presence (`ui/feed.md` § User actions); read fresh off the caller's
    /// live snapshot on every render, same as `isOwn`.
    public let webSlug: String?
    /// `PostSummary.gatedTier` — together with `webSlug` decides whether
    /// *Copy paywall link* offers (published **and** gated only).
    public let gatedTier: String?
    /// The shared `fauna.web.*` published-post cache (`web-content-hosting.md`
    /// § Published-post management) — the SAME store the `web-settings`
    /// Published-posts section reads/writes, so the two surfaces can never
    /// disagree about a creator's serving origin. Threaded in from the
    /// platform card view, which holds it via `AppState`/`MacAppState`.
    public let webPublish: WebPublishStore
    /// The caller's own handle — the one input a lazy `webPublish.hydrate` needs.
    /// Threaded in rather than read from an environment: FaunaKit has no
    /// cross-platform `SessionState` accessor of its own (`AppState`/
    /// `MacAppState` differ per platform), mirroring how `isOwn` is already
    /// computed by the platform card view and handed down as a plain value.
    public let handle: String
    /// Jump to the Personalization home's Trained-topics facet. Per-platform
    /// navigation (Settings tab/sidebar) — threaded in like `PersonalizationView`'s
    /// own nav closures, since FaunaKit has no cross-tab navigation model of
    /// its own.
    public var onNavigateToPersonalization: () -> Void
    /// The report target the platform card view built from this post
    /// (`report_post_target` — it carries the sealed rule once, so a gated post
    /// offers the include-text checkbox). `feed-post-report-button` paints only
    /// on another author's post (`!isOwn`), opening the shared ``ReportHost``
    /// sheet through ``ReportSheetStore``.
    public let reportTarget: FfiReportTarget?
    @Environment(ContentPolicyStore.self) private var contentPolicy: ContentPolicyStore?

    public init(
        postId: String, vm: FeedVM, isOwn: Bool, webSlug: String?, gatedTier: String?,
        webPublish: WebPublishStore, handle: String,
        reportTarget: FfiReportTarget? = nil,
        onNavigateToPersonalization: @escaping () -> Void
    ) {
        self.postId = postId
        self.vm = vm
        self.isOwn = isOwn
        self.webSlug = webSlug
        self.gatedTier = gatedTier
        self.webPublish = webPublish
        self.handle = handle
        self.reportTarget = reportTarget
        self.onNavigateToPersonalization = onNavigateToPersonalization
    }

    @State private var showActions = false
    @State private var showTargetSheet = false
    @State private var targetVerb: TrainVerb = .moreLikeThis
    @State private var targetRows: [FfiTrainedTopicRow] = []
    @State private var selectedTargetFactor: String?
    /// Own-post delete confirm step (`dm-message-delete-button`/
    /// `-delete-confirm-button` precedent, `DmMessageBubble.swift`).
    @State private var confirmDelete = false
    /// What the currently-open menu's web-copy verb actually put on the
    /// clipboard — the `copied` attr's backing store. Cleared on every fresh
    /// open (closing and reopening presents an un-copied menu again), the
    /// same lifecycle as `confirmDelete`.
    @State private var copiedWebLink: String?

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                showActions.toggle()
                if showActions { openMenu() }
            } label: {
                Label(L.feed.trainTargetTitle, systemImage: "ellipsis")
                    .font(.caption2)
                    .labelStyle(.iconOnly)
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.feedPostActionsButton)
            .automationActivate(Ids.feedPostActionsButton) {
                showActions.toggle()
                if showActions { openMenu() }
            }

            if showActions {
                actionsMenu
            }

            if showTargetSheet {
                targetSheet
            }
        }
    }

    /// A fresh open is never pre-armed: `confirmDelete`/`copiedWebLink` are
    /// properties of THIS opening of the menu. Also lazily hydrates the
    /// shared `WebPublishStore` for an own post — `isHydrated` gates a second
    /// caller from re-fetching, so this fires at most once per ACTOR session
    /// unless the settings page already did it first (tui's `view.is_none()`
    /// gate: painting a *disabled* copy verb off unread state would tell a
    /// creator with a working address that they have none). An account
    /// switch resets `isHydrated` back to `false` (`WebPublishStore.reset()`)
    /// , so this re-fires for the incoming actor.
    private func openMenu() {
        confirmDelete = false
        copiedWebLink = nil
        guard isOwn, !webPublish.isHydrated, let api = vm.api else { return }
        Task {
            await webPublish.hydrate(api: api, handle: handle)
        }
    }

    /// The in-context target factor, read fresh on every body evaluation
    /// (SwiftUI re-renders on `@Observable` change, so unlike windows'
    /// imperative click handler there is no separate "read once at open"
    /// step needed).
    private var targetFactor: String? { vm.manager?.trainTargetFactor() }

    @ViewBuilder private var actionsMenu: some View {
        VStack(alignment: .leading, spacing: 6) {
            verbRow(.moreLikeThis, label: L.feed.moreLikeThis, id: "feed-post-more-like-this")
            verbRow(.lessLikeThis, label: L.feed.lessLikeThis, id: "feed-post-less-like-this")
            // Deliberately OUTSIDE the training verbs above (which have no
            // early return here to share, unlike tui — but kept as its own
            // block for the same reason tui states: the web verbs have
            // nothing to do with training).
            if isOwn {
                Divider()
                webPublishVerbs
            }
            if isOwn {
                Divider()
                deleteRow
            }
            if !isOwn, reportTarget != nil {
                Divider()
                reportRow
            }
        }
        .padding(8)
        .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.feedPostActionsMenu)
        // Presence anchor so the in-process driver can confirm the menu
        // opened — a bare `.accessibilityIdentifier` is invisible to the
        // registry (DmMessageBubble precedent).
        .automationValue(Ids.feedPostActionsMenu, text: { "" })
    }

    private func verbRow(_ verb: TrainVerb, label: String, id: String) -> some View {
        let factor = targetFactor
        let marked = factor.map { vm.manager?.exampleLabelFor(postId: postId, factor: $0) == verb } ?? false
        let action: () -> Void = {
            if let factor {
                Task { await dispatchTrain(factor: factor, verb: verb, alreadyMarked: marked) }
            } else {
                targetVerb = verb
                showActions = false
                showTargetSheet = true
                Task { targetRows = await vm.trainedFactorRows() }
            }
        }
        return Button(action: action) {
            Text(label).font(.caption)
        }
        .buttonStyle(.borderless)
        .accessibilityIdentifier(id)
        .automationActivate(id, value: { marked ? "on" : "off" }, perform: action)
    }

    /// The report verb (moderation.md § User-initiated reporting → *App surface*):
    /// another author's post only. Dismisses the overflow and opens the shared
    /// sheet; the ``ReportHost`` the shell mounts paints it.
    private var reportRow: some View {
        let open: () -> Void = {
            guard let reportTarget, let contentPolicy else { return }
            showActions = false
            contentPolicy.report.open(reportTarget)
        }
        return Button(action: open) {
            Label(L.feed.reportPost, systemImage: "flag").font(.caption)
        }
        .buttonStyle(.borderless)
        .accessibilityIdentifier(Ids.feedPostReportButton)
        .automationActivate(Ids.feedPostReportButton, perform: open)
    }

    /// Own-post delete row (feed.md § State & data shape → Post deletion) —
    /// two-step destructive confirm inside the same overflow, mirroring
    /// `DmMessageBubble`'s `dm-message-delete-button`/`-delete-confirm-button`
    /// shape verbatim.
    @ViewBuilder private var deleteRow: some View {
        if confirmDelete {
            HStack(spacing: 8) {
                Text(L.feed.deletePostConfirmTitle).font(.caption)
                Button(L.feed.deletePostConfirm, role: .destructive) {
                    performDelete()
                }
                .buttonStyle(.borderless)
                .accessibilityIdentifier(Ids.feedPostDeleteConfirmButton)
                .automationActivate(Ids.feedPostDeleteConfirmButton) { performDelete() }
                Button(L.common.cancel, role: .cancel) { confirmDelete = false }
                    .buttonStyle(.borderless)
            }
        } else {
            Button(role: .destructive) {
                confirmDelete = true
            } label: {
                Label(L.feed.deletePost, systemImage: "trash").font(.caption)
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.feedPostDeleteButton)
            .automationActivate(Ids.feedPostDeleteButton) { confirmDelete = true }
        }
    }

    /// Own-post web-publishing verbs (`ui/feed.md` § User actions;
    /// `web-content-hosting.md` § Published-post management) — presence is
    /// state-derived off `webSlug`/`gatedTier` alone, never a per-row query.
    /// Both copy affordances disable when the actor has no serving origin,
    /// with the reason painted beside them: publishing with no origin is
    /// legal but unreachable, and the UI must say so rather than hand out a
    /// link that cannot load. The takedown stays live — it needs no origin,
    /// and it is the one thing a user with an unreachable site may well want.
    @ViewBuilder private var webPublishVerbs: some View {
        let origin = webPublish.siteLink.origin
        if webSlug == nil {
            // Unpublished: one verb, and no link affordances for a page that
            // does not exist yet. A default slug is the nest's to mint, so
            // this needs no origin and no input.
            Button(L.webPublish.publishToWeb) { Task { await publishToWeb() } }
                .buttonStyle(.borderless)
                .font(.caption)
                .accessibilityIdentifier(Ids.feedPostPublishWebButton)
                .automationActivate(Ids.feedPostPublishWebButton) { Task { await publishToWeb() } }
        } else {
            if gatedTier != nil && origin != nil {
                Text(L.webPublish.paywallLinkNote).font(.caption2).foregroundStyle(.secondary)
            }
            // Why the copy verbs below are dead, in the user's own terms. The
            // ⋯ menu cannot say "the toggle above" — that control is on
            // another page — so this names where to go instead.
            if origin == nil {
                Text(L.webPublish.menuNoLinkReason).font(.caption2).foregroundStyle(.secondary)
            }
            Button(L.webPublish.copyWebLink) { copyWebLink(origin: origin) }
                .buttonStyle(.borderless)
                .font(.caption)
                .disabled(origin == nil)
                .accessibilityIdentifier(Ids.feedPostCopyWebLinkButton)
                .automationActivate(
                    Ids.feedPostCopyWebLinkButton,
                    isEnabled: { origin != nil },
                    value: { copiedWebLink }
                ) { copyWebLink(origin: origin) }

            // Gated rows only: an ungated post has no paywalled body, so the
            // mint would hand out a token for nothing.
            if gatedTier != nil {
                Button(L.webPublish.copyPaywallLink) { Task { await copyPaywallLink() } }
                    .buttonStyle(.borderless)
                    .font(.caption)
                    .disabled(origin == nil)
                    .accessibilityIdentifier(Ids.feedPostCopyPaywallLinkButton)
                    .automationActivate(
                        Ids.feedPostCopyPaywallLinkButton,
                        isEnabled: { origin != nil },
                        value: { copiedWebLink }
                    ) { Task { await copyPaywallLink() } }
                    // The feed page's one desensitizing verb. Its sibling
                    // `feed-post-copy-web-link-button` copies a URL already
                    // resolved on screen and stays live; publish/unpublish and
                    // the delete confirm are all `OfflineSafe`, so they stay
                    // live too — the MINT is what needs the nest's holder
                    // identity. Same split tui's `CopyPaywallLink` makes.
                    .faunaGate("fauna.web.paywall.mint_token")
            }

            Button(L.webPublish.unpublish) { Task { await unpublishFromWeb() } }
                .buttonStyle(.borderless)
                .font(.caption)
                .accessibilityIdentifier(Ids.feedPostUnpublishWebButton)
                .automationActivate(Ids.feedPostUnpublishWebButton) { Task { await unpublishFromWeb() } }
        }
    }

    // The three web-publishing verbs below are thin callers: each hands its
    // awaited store call, as a closure, to an `internal static` body that owns
    // the generation capture and the guarded landing. That split is the
    // unit-tier seam — `CallSiteCaptureOrderingTests` drives each body with an
    // operation that suspends, runs `reset()` mid-await, then throws, so each
    // site's capture-BEFORE-the-await ordering is executed, not read.

    private func publishToWeb() async {
        guard let bytes = Data(hexString: postId) else { return }
        await Self.publishToWeb(vm: vm) {
            _ = try await webPublish.publish(postId: bytes)
            await vm.manager?.refreshCurrentFeed()
        }
    }

    /// `publishToWeb`'s guarded body. The generation is captured before the
    /// await — same account-switch guard as `WebPublishStore.landActionFailure`,
    /// but landing into the FEED's own error slot (`vm.clientErrorMessage`),
    /// since that's where this menu displays a failure, not
    /// `webPublish.errorMessage` (`account-scoping.md` § The scoping
    /// taxonomy).
    @MainActor
    static func publishToWeb(vm: FeedVM, publish: () async throws -> Void) async {
        let generation = vm.managerGeneration
        do {
            try await publish()
        } catch {
            guard let text = DisplayError.message(error) else { return }
            vm.landClientErrorMessage(
                generation: generation, message: L.webPublish.errorPublish(message: text))
        }
    }

    private func unpublishFromWeb() async {
        guard let bytes = Data(hexString: postId) else { return }
        await Self.unpublishFromWeb(vm: vm) {
            try await webPublish.unpublish(postId: bytes)
            await vm.manager?.refreshCurrentFeed()
        }
    }

    /// `unpublishFromWeb`'s guarded body — same generation guard as
    /// `publishToWeb(vm:publish:)` above.
    @MainActor
    static func unpublishFromWeb(vm: FeedVM, unpublish: () async throws -> Void) async {
        let generation = vm.managerGeneration
        do {
            try await unpublish()
        } catch {
            guard let text = DisplayError.message(error) else { return }
            vm.landClientErrorMessage(
                generation: generation, message: L.webPublish.errorUnpublish(message: text))
        }
    }

    /// Purely local — the origin and the slug are both already resolved on
    /// screen, so the public link costs no round trip. Nothing is copied when
    /// there is no serving origin: the button paints disabled in that case,
    /// and this refuses independently rather than trusting the paint (a dead
    /// link on the clipboard is worse than no copy).
    private func copyWebLink(origin: String?) {
        guard let origin, let slug = webSlug else { return }
        let url = webPostPageUrl(origin: origin, slug: slug)
        Pasteboard.copy(url)
        copiedWebLink = url
    }

    /// A fresh mint per click: the token is short-lived by ratified design and
    /// re-minting is free, so re-copying always yields a link that works from
    /// now, never a cached one that already expired.
    private func copyPaywallLink() async {
        guard let origin = webPublish.siteLink.origin, let slug = webSlug else { return }
        let minted = await Self.mintPaywallLink(vm: vm, origin: origin) {
            try await webPublish.mintPaywallLink(slug: slug)
        }
        guard let url = minted else { return }
        Pasteboard.copy(url)
        copiedWebLink = url
    }

    /// `copyPaywallLink`'s guarded body: the tokened URL on success, `nil`
    /// once the failure has landed — same generation guard as
    /// `publishToWeb(vm:publish:)` above.
    @MainActor
    static func mintPaywallLink(
        vm: FeedVM, origin: String, mint: () async throws -> MintedPaywallLink
    ) async -> String? {
        let generation = vm.managerGeneration
        do {
            let minted = try await mint()
            return webTokenedUrl(origin: origin, path: minted.path, token: minted.token)
        } catch {
            guard let text = DisplayError.message(error) else { return nil }
            vm.landClientErrorMessage(
                generation: generation, message: L.webPublish.errorPaywallLink(message: text))
            return nil
        }
    }

    /// Dispatch the own-post delete and dismiss the overflow. Silent swallow on
    /// failure — matches `dispatchTrain`'s existing shape (no dedicated per-card
    /// error slot exists); the manager only mutates/re-emits on success, so a
    /// failed delete just leaves the card as-is for a retry.
    private func performDelete() {
        Task { await vm.deletePost(postId) }
        confirmDelete = false
        showActions = false
    }

    /// Run a training gesture: the marked verb again ⇒ un-train (the inverse
    /// delta); anything else ⇒ train (the manager applies forward/flip
    /// semantics — no client-side duplicate guard, `TrainResult.duplicateSignal`
    /// writes nothing). A successful call mutates the manager's snapshot +
    /// notifies, so the card rebuilds via the normal observer path.
    private func dispatchTrain(factor: String, verb: TrainVerb, alreadyMarked: Bool) async {
        do {
            if alreadyMarked {
                try await vm.manager?.untrainPost(postId: postId, factor: factor)
            } else {
                _ = try await vm.manager?.trainPost(postId: postId, factor: factor, verb: verb)
            }
        } catch {
            // Reason already surfaced by the manager's re-emitted snapshot
            // where applicable; mirrors `FeedVM.interact`'s existing
            // swallow-and-return shape for other per-post gestures
            // (like/repost/quote) — no dedicated per-card error slot exists.
        }
        showActions = false
        showTargetSheet = false
    }

    /// The factor-target sheet (`feed-post-train-target-sheet`): shown when
    /// the current feed's composition does not single out one trained topic.
    /// Populated async-after-open (mirrors windows/linux — the sheet appears
    /// immediately, options land when the sealed-registry read resolves).
    @ViewBuilder private var targetSheet: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.feed.trainTargetTitle).font(.subheadline.weight(.semibold))

            if targetRows.isEmpty {
                Text(L.personalization.trainedTopicsEmpty)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                Picker("", selection: $selectedTargetFactor) {
                    ForEach(targetRows, id: \.id) { row in
                        if let key = row.factorKey {
                            Text(row.name).tag(Optional(key))
                        }
                    }
                }
                .pickerStyle(.menu)
                .labelsHidden()
            }

            Button(L.common.save) {
                guard let factor = selectedTargetFactor else { return }
                Task { await dispatchTrain(factor: factor, verb: targetVerb, alreadyMarked: false) }
            }
            .disabled(selectedTargetFactor == nil)

            Button(L.personalization.trainedFactorCreate) {
                showTargetSheet = false
                onNavigateToPersonalization()
            }
            .buttonStyle(.borderless)
            .font(.caption)
        }
        .padding(8)
        .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 8))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.feedPostTrainTargetSheet)
        .automationValue(Ids.feedPostTrainTargetSheet, text: { "" })
    }
}

/// The feed post card's collapse placeholder for a muted-keyword match
/// (topic-factors.md § Scoring — a mute collapses everywhere, chronological
/// feeds included; moderation.md § Muted keywords). The feed twin of
/// `DmMessageBubble`'s `dm-message-muted` placeholder: a per-post, session-local
/// reveal that un-collapses just this card for the rest of the session — the
/// mute itself is unaffected.
public struct FeedPostMutedPlaceholder: View {
    public var onReveal: () -> Void
    public init(onReveal: @escaping () -> Void) {
        self.onReveal = onReveal
    }

    public var body: some View {
        HStack(spacing: 8) {
            automationText(Ids.feedPostMuted, L.feed.postMutedPlaceholder)
                .font(.caption)
                .italic()
                .foregroundStyle(.secondary)
            Spacer(minLength: 4)
            Button(L.feed.postMutedReveal) {
                onReveal()
            }
            .buttonStyle(.borderless)
            .font(.caption2)
            .accessibilityIdentifier(Ids.feedPostMutedRevealButton)
            .automationActivate(Ids.feedPostMutedRevealButton) {
                onReveal()
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
