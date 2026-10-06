import SwiftUI

/// The region placeholder painted **in place of** a region-withheld item
/// (`region-blocking.md` § The blocked render and the transparency surface):
/// the app's frame naming the region and its authority, the authority's name,
/// and its reason verbatim (never an i18n string); a `collapse` adds the
/// reveal, which runs `onReveal`. Shared by every apple render surface — the
/// feed card, both post details and the conversation bubble — the apple twin of
/// linux's `region::placeholder_box` and web's `RegionPlaceholder.svelte`.
///
/// `witnessKey` is the surface + item key the container registered with
/// `regionBlockWitness`; a painted `block` registers the same key on the
/// painted side of convention 17's `region_block_render`.
public struct RegionPlaceholderView: View {
    let placeholder: FfiRegionPlaceholder
    let witnessKey: String
    let onReveal: () -> Void

    public init(placeholder: FfiRegionPlaceholder, witnessKey: String, onReveal: @escaping () -> Void) {
        self.placeholder = placeholder
        self.witnessKey = witnessKey
        self.onReveal = onReveal
    }

    private var isBlock: Bool { placeholder.verb == "block" }

    private var noticeText: String {
        isBlock
            ? L.region.blockedNotice(region: placeholder.region, authority: placeholder.authorityName)
            : L.region.collapsedNotice(region: placeholder.region, authority: placeholder.authorityName)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(noticeText)
                .font(.caption)
                .italic()
                .accessibilityIdentifier(Ids.regionBlockedNotice)
                .automationValue(
                    Ids.regionBlockedNotice,
                    text: { noticeText },
                    attributes: { ["verdict": placeholder.verb] })
            automationText(Ids.regionBlockedAuthority, placeholder.authorityName)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.regionBlockedReason, placeholder.reason)
                .font(.caption)
                .foregroundStyle(.secondary)
            if !isBlock {
                Button(L.region.revealButton) { onReveal() }
                    .buttonStyle(.borderless)
                    .font(.caption2)
                    .accessibilityIdentifier(Ids.regionCollapsedRevealButton)
                    .automationActivate(Ids.regionCollapsedRevealButton) { onReveal() }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .modifier(PaintedBlockWitness(key: witnessKey, active: isBlock))
    }
}

/// The post detail's region arm — shared by iOS `PostDetailView` and macOS
/// `PostDetailSheet`, which wrap their normal content in it: a post the region
/// withholds paints the placeholder in place of the whole detail (a collapse
/// reveals it), exactly as the feed card does, so no render surface reaches the
/// composed call for one source while bypassing it for another
/// (`region-blocking.md` § Where it composes; linux's `region_withheld`, the
/// ONE check its list card and post detail make).
public struct RegionGatedPostContent<Content: View>: View {
    let post: PostSummary
    let content: () -> Content

    @Environment(ContentPolicyStore.self) private var contentPolicy: ContentPolicyStore?
    @Environment(RegionStore.self) private var region: RegionStore?
    @State private var revealed = false

    public init(post: PostSummary, @ViewBuilder content: @escaping () -> Content) {
        self.post = post
        self.content = content
    }

    private var witnessKey: String { "detail:\(post.postId)" }

    public var body: some View {
        let decision = (contentPolicy?.inputs ?? ContentPolicyInputs()).recordedRender(
            itemId: post.postId, labels: post.labels, region: region, subject: .post(post))
        Group {
            if let withheld = decision.withheld(revealed: revealed) {
                ScrollView {
                    RegionPlaceholderView(placeholder: withheld, witnessKey: witnessKey) {
                        revealed = true
                    }
                    .padding()
                }
            } else {
                content()
            }
        }
        .regionBlockWitness(witnessKey, blocked: decision.isRegionBlocked)
    }
}

/// The painted side of convention 17's `region_block_render` — a block
/// placeholder counts while it is on screen. No-op in production.
private struct PaintedBlockWitness: ViewModifier {
    let key: String
    let active: Bool

    func body(content: Content) -> some View {
        #if DEBUG
        content
            .onAppear { if active { RegionBlockRender.painted.insert(key) } }
            .onDisappear { RegionBlockRender.painted.remove(key) }
        #else
        content
        #endif
    }
}

/// The Settings region section's rows (`settings-region-*`) — a paint of the
/// shared `FfiRegionPlane.view()`, never an app-side fold: the declared region
/// and its source (read-only, the change path named — there is no in-app
/// override), each policy on the chain (authority, sequence, issued-at, the
/// inert/malformed notice), when it was last checked, and the staleness
/// warning. Each shell keeps its own container (macOS `GroupBox`, iOS
/// `GroupBox` in the Status list), exactly as `FeatureLimitsSection` does; the
/// apple twin of linux's `region::paint_settings` and web's
/// `RegionSettingsSection.svelte`.
public struct RegionSettingsSection: View {
    let region: RegionStore

    public init(region: RegionStore) {
        self.region = region
    }

    private func sourceText(_ source: FfiRegionSource) -> String {
        switch source {
        case .storefront: L.region.sourceStorefront
        case .systemRegion: L.region.sourceSystemRegion
        case .systemLocale: L.region.sourceSystemLocale
        case .browserLocale: L.region.sourceBrowserLocale
        }
    }

    public var body: some View {
        let view = region.view()
        VStack(alignment: .leading, spacing: 6) {
            if let declared = view.declared {
                automationText(Ids.settingsRegionDeclared, L.region.declared(region: declared.code))
                automationText(Ids.settingsRegionSource, sourceText(declared.source))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if view.policies.isEmpty {
                    Text(L.region.noPolicy)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                ForEach(Array(view.policies.enumerated()), id: \.offset) { index, policy in
                    policyItem(policy)
                        .accessibilityElement(children: .contain)
                        .accessibilityIdentifier(Ids.settingsRegionPolicyItem)
                        .automationValue(Ids.settingsRegionPolicyItem, text: { policy.authorityName })
                        .automationScope(Ids.settingsRegionPolicyItem, index: index)
                }
                if let checked = view.lastCheckedAt {
                    automationText(
                        Ids.settingsRegionLastChecked,
                        L.region.lastChecked(time: formatUnixLocal(secs: Int64(checked))))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if view.stale {
                    automationText(Ids.settingsRegionStaleWarning, L.region.staleWarning)
                        .font(.caption)
                        .foregroundStyle(.orange)
                }
            } else {
                automationText(Ids.settingsRegionDeclared, L.region.noneDeclared)
            }
        }
    }

    @ViewBuilder
    private func policyItem(_ policy: FfiRegionPolicyRow) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            automationText(
                Ids.settingsRegionPolicyAuthority,
                L.region.policyAuthority(region: policy.region, authority: policy.authorityName))
            automationText(
                Ids.settingsRegionPolicyVersion,
                L.region.policyVersion(
                    sequence: String(policy.sequence),
                    issued: formatUnixLocal(secs: Int64(policy.issuedAt))))
                .font(.caption)
                .foregroundStyle(.secondary)
            if policy.state == "inert" {
                automationText(
                    Ids.settingsRegionInertNotice,
                    L.region.inertNotice(version: policy.inertVersion.map { String($0) } ?? "?"))
                    .font(.caption)
            } else if policy.state == "malformed" {
                automationText(Ids.settingsRegionInertNotice, L.region.malformedNotice)
                    .font(.caption)
            }
        }
        .padding(.vertical, 4)
    }
}
