import SwiftUI

/// The gated-feature plane's transparency read — `docs/goal/architecture/dynamic-features.md`
/// § Transparency & auditability, boundary 4: *"No silent gates. Every active
/// restriction is visible to the person it binds — which feature, what limit,
/// which tier set it."* Placed directly after Quota as its sibling "what
/// bounds me" surface (`docs/goal/ui/settings.md` § Layout & flow item 2b).
///
/// Shared FaunaKit content, consumed by both macOS (`MacStatusView`, inside a
/// `GroupBox`) and iOS (`StatusDetailView`, inside a `List` `Section`) — each
/// keeps its own container shell (Quota itself is not a shared view across
/// the two targets either), but the row/cell rendering — the two-level
/// magnitude composition, the nested-key resolves, the hidden-row filter — is
/// written once here so it cannot diverge. Every judgement (which cells
/// survived the tier meet, `remaining = limit − observed`, which tier bound
/// each cell, available/disabled/hidden) is `FeaturesClient::rows()`'s
/// output — this view paints, it decides nothing (mirrors linux's
/// `update_features` / tui's `feature_limits_elements` field-for-field).
public struct FeatureLimitsSection: View {
    let rows: [FfiFeatureRow]

    public init(rows: [FfiFeatureRow]) {
        self.rows = rows
    }

    /// A `hidden` row means this nest build does not carry the feature at all
    /// (its capability token is absent) — filtered here, not in shared Rust,
    /// for the same reason every other app filters client-side: the crate's
    /// job is to decide, not to choose which decisions a surface shows.
    private var visible: [FfiFeatureRow] {
        rows.filter { $0.affordance != "hidden" }
    }

    public var body: some View {
        if visible.isEmpty {
            automationText(Ids.featureLimitsEmpty, L.features.empty)
                .foregroundStyle(.secondary)
                .font(.caption)
        } else {
            ForEach(Array(visible.enumerated()), id: \.offset) { index, row in
                featureRow(row)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.featureLimitsRow)
                    .automationValue(Ids.featureLimitsRow, text: { renderLocalizedText(row.name) })
                    .automationScope(Ids.featureLimitsRow, index: index)
            }
        }
    }

    @ViewBuilder
    private func featureRow(_ row: FfiFeatureRow) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            automationText(Ids.featureLimitsName, renderLocalizedText(row.name))
                .font(.headline)
            automationText(Ids.featureLimitsStatus, renderLocalizedText(row.status))
                .font(.caption)
                .foregroundStyle(.secondary)

            // Only when something actually blocks — boundary 4's "no silent
            // gates" half. Nested resolve: the sentence's `{window}` is
            // itself an i18n key (`features.window_day`, …), so a plain
            // resolve would paint the raw key.
            if let restriction = row.restriction {
                automationText(Ids.featureLimitsRestriction, renderLocalizedTextNested(restriction))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            ForEach(Array(row.cells.enumerated()), id: \.offset) { cellIndex, cell in
                quotaCell(cell)
                    .accessibilityElement(children: .contain)
                    .accessibilityIdentifier(Ids.featureLimitsQuota)
                    .automationValue(Ids.featureLimitsQuota, text: { ValueFormat.cellValueText(cell) })
                    .automationScope(Ids.featureLimitsQuota, index: cellIndex)
            }
        }
        .padding(.vertical, 4)
    }

    @ViewBuilder
    private func quotaCell(_ cell: FfiLimitCell) -> some View {
        HStack(spacing: 8) {
            // Nested resolve: the label composes TWO key substitutions
            // (dimension + window) — "{dimension} per {window}".
            automationText(Ids.featureLimitsQuotaLabel, renderLocalizedTextNested(cell.label))
                .font(.caption)
            automationText(Ids.featureLimitsQuotaValue, ValueFormat.cellValueText(cell))
                .font(.caption)
            automationText(Ids.featureLimitsQuotaTier, renderLocalizedText(cell.tierLabel))
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}
