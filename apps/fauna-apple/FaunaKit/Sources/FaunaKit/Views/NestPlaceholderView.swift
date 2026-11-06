import SwiftUI

/// The nest placeholder painted **in place of** an item a rule of the nest it
/// was read through withholds (`nest-rules.md` § The nest source in the engine;
/// § Element IDs): the app's frame naming the nest (`nest-blocked-notice`), its
/// domain (`nest-blocked-domain`) and the admin's reason verbatim
/// (`nest-blocked-reason`, never an i18n string); a `collapse` adds the reveal
/// (`nest-collapsed-reveal-button`), which runs `onReveal` — the surface's own
/// family reveal. A `block` has no reveal: a member never loosens a nest rule.
/// ``RegionPlaceholderView``'s twin; android's `NestPlaceholder`.
///
/// `witnessKey` is the surface + item key the container registered with
/// `nestBlockWitness`; a painted `block` registers the same key on the painted
/// side of convention 17's `nest_block_render`.
public struct NestPlaceholderView: View {
    let placeholder: FfiNestPlaceholder
    let witnessKey: String
    let onReveal: () -> Void

    public init(placeholder: FfiNestPlaceholder, witnessKey: String, onReveal: @escaping () -> Void) {
        self.placeholder = placeholder
        self.witnessKey = witnessKey
        self.onReveal = onReveal
    }

    private var isBlock: Bool { placeholder.verb == "block" }

    private var noticeText: String {
        isBlock
            ? L.nestRules.blockedNotice(nest: placeholder.domain)
            : L.nestRules.collapsedNotice(nest: placeholder.domain)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(noticeText)
                .font(.caption)
                .italic()
                .accessibilityIdentifier(Ids.nestBlockedNotice)
                .automationValue(
                    Ids.nestBlockedNotice,
                    text: { noticeText },
                    attributes: { ["verdict": placeholder.verb] })
            automationText(Ids.nestBlockedDomain, placeholder.domain)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.nestBlockedReason, placeholder.reason)
                .font(.caption)
                .foregroundStyle(.secondary)
            if !isBlock {
                Button(L.nestRules.revealButton) { onReveal() }
                    .buttonStyle(.borderless)
                    .font(.caption2)
                    .accessibilityIdentifier(Ids.nestCollapsedRevealButton)
                    .automationActivate(Ids.nestCollapsedRevealButton) { onReveal() }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .modifier(PaintedNestBlockWitness(key: witnessKey, active: isBlock))
    }
}

/// The painted side of convention 17's `nest_block_render` — a block placeholder
/// counts while it is on screen. No-op in production.
private struct PaintedNestBlockWitness: ViewModifier {
    let key: String
    let active: Bool

    func body(content: Content) -> some View {
        #if DEBUG
        content
            .onAppear { if active { NestBlockRender.painted.insert(key) } }
            .onDisappear { NestBlockRender.painted.remove(key) }
        #else
        content
        #endif
    }
}
