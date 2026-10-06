import SwiftUI

// The post tip surface (`monetization.md` § Tips) — one component for BOTH
// the feed list card and the post-detail pane (the shared-view
// precedent: `GatedPostBadge.swift`, `FeedPostActions.swift`,
// `QuotedPostCard.swift`).
//
// ⚠ MUST STAY BEHIND `#if !FAUNA_EXCISE_PAYMENTS`, AND THE REASON IS NOT THE
// OBVIOUS ONE. `PostSummary.tips` is a deliberately UNGATED inert record
// (`dynamic-features.md` § Platform-family surface excision): in an excised
// build the resolver never populates it, so `tips` stays `nil` forever and
// this view simply paints nothing — while `Ids.postTip*` would still ship in
// the binary if referenced from ungated code. Dead is not absent, and
// criterion 1 is a `strings`-grep. Both shipped Rust shells (tui, linux) were
// bitten in exactly this place; web's `TipSurface.svelte` carries the same
// warning verbatim. `Ids.postTip*` themselves only EXIST inside this same
// `#if` in `UiIds.swift`, which enforces the gate at compile time too — an
// ungated reference to them simply fails to build in the store-safe flavor.
#if !FAUNA_EXCISE_PAYMENTS
public struct TipSurface: View {
    /// The post's `tips` record — `nil` covers both "not yet resolved" and
    /// "the nest answered untipped"; one empty surface for both.
    public let tips: TipView?

    /// The attribution window's open state — per instance, local, never
    /// persisted: the list is an audit affordance, not a preference (mirrors
    /// web's `listOpen`).
    @State private var listOpen = false

    public init(tips: TipView?) {
        self.tips = tips
    }

    public var body: some View {
        // Nothing renders at zero tips (untipped, unresolved, or a
        // transport error — one degraded shape for all three).
        if let tips, tips.tipCount != 0 {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 8) {
                    // The two counters are guarded INDEPENDENTLY within a
                    // tipped post: `total_msats` sums only receipts that
                    // reported an amount, so a post whose every receipt
                    // carried an unparseable invoice shows the count and no
                    // total — rendering "0 sats" there would say nobody paid
                    // (§ Tips — a missing amount is a real state, never
                    // coerced to 0).
                    if tips.totalMsats != 0 {
                        automationText(Ids.postTipTotal, ValueFormat.tipAmount(tips.totalMsats))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    automationText(Ids.postTipCount, ValueFormat.tipCount(tips.tipCount))
                        .font(.caption).foregroundStyle(.secondary)
                    Button(L.tips.listOpen) { listOpen.toggle() }
                        .buttonStyle(.borderless)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.postTipListButton)
                        .automationActivate(Ids.postTipListButton) { listOpen.toggle() }
                }
                if listOpen {
                    tipList(tips)
                }
            }
        }
    }

    /// The `post-tip-list` attribution window — inline, matching the
    /// `feed-post-actions-menu` shape (every id attaches to a real element,
    /// absent from the tree while closed), not a modal sheet.
    private func tipList(_ tips: TipView) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            // The bounded window's tail, from the nest's own `has_more` —
            // NEVER inferred by comparing the row count against a cap this
            // client hard-codes (the wire carries the flag precisely so no
            // client has to).
            Text(tips.hasMore
                 ? "\(L.tips.listTitle) — \(ValueFormat.tipMore(tips.tipCount - Int64(tips.senders.count)))"
                 : L.tips.listTitle)
                .font(.caption.weight(.semibold))
            // Every row the nest sent, unfiltered — authenticity is settled
            // at ingest and never at read (`monetization.md` § Zap receipts);
            // a client-side trust check here would re-open exactly the
            // per-reader re-checking that discipline exists to prevent.
            ForEach(Array(tips.senders.enumerated()), id: \.offset) { _, tip in
                automationText(
                    Ids.postTipItem,
                    "\(tip.sender ?? tip.senderRef ?? L.tips.senderUnknown) — "
                        + (tip.amountMsats.map { ValueFormat.tipAmount($0) } ?? L.tips.amountUnknown))
                    .font(.caption2)
            }
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
        .accessibilityIdentifier(Ids.postTipList)
        // `.contain` keeps the indexed `post-tip-item` children queryable
        // (the bare container id would otherwise clobber them — the
        // documented per-card pattern, `QuotedPostCard`'s precedent).
        .accessibilityElement(children: .contain)
        .automationValue(Ids.postTipList, text: { "" })
    }
}
#endif
