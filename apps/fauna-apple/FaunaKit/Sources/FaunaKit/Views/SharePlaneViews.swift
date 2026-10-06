import SwiftUI

// EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the plane's Swift leg reads FFI types the
// store-safe `FaunaFFI.xcframework` does not export; the reason is written once in
// `SharePlaneModel.swift`.
#if !FAUNA_EXCISE_P2P_SHARE

/// The page-level **peer-transfer surface** on the Folders page (`p2p.md` § Cross-user
/// shared-set transfer; the six ids user-approved 2026-08-18) — `share-serve-status`
/// plus the `share-transfer-list`'s `share-transfer-item` rows. Render-only: the plane
/// has no gestures here (severance stays the member-remove / leave buttons on the set
/// rows). Shared by macOS + iOS (one FaunaKit surface, priority #2). References:
/// linux's `build_share_transfer_section` / `render_share_transfers` and tui's
/// `share_transfer_elements`, whose placement it copies — directly under the
/// co-present offline-share panel.
///
/// **Renders NOTHING while no plane is running** (`model.view == nil`) — not the
/// "no shared folders to serve" line, which is a different fact and a reading *inside*
/// a non-nil view. Signed out, a seam absent, the feature off, or (today) iOS, which
/// does not host an account runtime yet: "this device is not running the plane" owes
/// the user no line, where the rule-5 transparency line owes them the second only when
/// it is true.
///
/// The list container is present whenever the plane is up — ui.yaml's stated presence
/// rule ("present when the share plane is up or has recorded activity") — with one
/// row per (set × peer) pull outcome.
struct SharePlaneSectionView: View {
    let model: SharePlaneModel

    var body: some View {
        if let view = model.view {
            let rows = sharePlaneTransferReadings(view)
            VStack(alignment: .leading, spacing: 4) {
                Text(L.folders.shareTransferSection)
                    .font(.headline)
                automationText(Ids.shareServeStatus, sharePlaneServeStatusText(view))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(rows.enumerated()), id: \.offset) { offset, row in
                        SharePlaneTransferRow(row: row)
                            // Scope path for the indexed `share-transfer-item`, so the
                            // three scoped child reads resolve per row (the
                            // `folder-pending-share` idiom).
                            .automationScope(Ids.shareTransferItem, index: offset)
                    }
                }
                // A bare container needs the `.contain` + an id + a registered read,
                // or the driver reads it as absent even though its children are found.
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier(Ids.shareTransferList)
                .automationValue(Ids.shareTransferList, text: { String(rows.count) })
            }
        }
    }
}

/// One `share-transfer-item` — a (set × peer) pull outcome: what it is, how far the
/// latest pass got, and where it stands (including the transfer gate's honest
/// "Limited by …" refusal, rendered nested by the readings that feed this row).
struct SharePlaneTransferRow: View {
    let row: SharePlaneTransferReading

    var body: some View {
        HStack {
            automationText(Ids.shareTransferName, row.name)
                .font(.caption)
            Spacer()
            automationText(Ids.shareTransferProgress, row.progress)
                .font(.caption)
                .foregroundStyle(.secondary)
            automationText(Ids.shareTransferState, row.state)
                .font(.caption)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.shareTransferItem)
        .automationValue(Ids.shareTransferItem, text: { row.name })
    }
}

#endif  // !FAUNA_EXCISE_P2P_SHARE
