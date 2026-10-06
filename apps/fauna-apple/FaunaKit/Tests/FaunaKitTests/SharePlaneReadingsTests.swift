import Foundation
import Testing
@testable import FaunaKit

// EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the plane's Swift leg reads FFI types the
// store-safe `FaunaFFI.xcframework` does not export (`SharePlaneModel.swift`).
#if !FAUNA_EXCISE_P2P_SHARE

/// The peer-transfer surface's readings (`p2p.md` § Cross-user shared-set transfer).
/// The shared Rust host hands the app `LocalizedText`s and the app only paints them, so
/// the two things an app can get wrong are pinned here: resolving the transfer gate's
/// refusal NESTED, and rendering nothing while no plane runs. tui and linux each pin the
/// first the same way against their own renderer — a plain resolve there and here both
/// paint the raw `features.tier_*` key at the user.
@Suite struct SharePlaneReadingsTests {
    private func text(_ key: String, _ args: [String: String] = [:]) -> LocalizedText {
        LocalizedText(key: key, args: args)
    }

    private func transfer(state: LocalizedText) -> FfiShareTransfer {
        FfiShareTransfer(
            name: text("folders.share_transfer_peer_row", ["folder": "Trip", "who": "ab12cd"]),
            progress: text("folders.share_transfer_progress", ["files": "2", "rows": "3"]),
            state: state)
    }

    /// The gate's honest refusal: `{source}` is itself a KEY (`features.tier_admin`), so
    /// only a nested resolve reads "Limited by Your nest admin". Goes red if the readings
    /// ever fall back to `renderLocalizedText`, which would paint "Limited by
    /// features.tier_admin".
    @Test func aRefusalNamesItsTierNotItsRawKey() {
        let limited = text(
            "folders.share_transfer_state_limited", ["source": "features.tier_admin"])
        let view = FfiSharePlaneView(
            serveStatus: text("folders.share_serve_status_no_sets"),
            transfers: [transfer(state: limited)])

        let rows = sharePlaneTransferReadings(view)

        #expect(rows.count == 1)
        #expect(rows[0].state == L.folders.shareTransferStateLimited(source: L.features.tierAdmin))
        #expect(!rows[0].state.contains("features.tier_admin"))
        // The mutation this pins: a plain substitution leaves the key in place.
        #expect(renderLocalizedText(limited).contains("features.tier_admin"))
    }

    @Test func theSixReadingsRenderTheirSharedLabels() {
        let view = FfiSharePlaneView(
            serveStatus: text("folders.share_serve_status_serving", ["count": "2"]),
            transfers: [
                transfer(state: text("folders.share_transfer_state_pulling")),
                transfer(state: text("folders.share_transfer_state_up_to_date")),
            ])

        #expect(sharePlaneServeStatusText(view) == L.folders.shareServeStatusServing(count: "2"))
        let rows = sharePlaneTransferReadings(view)
        #expect(rows.map(\.name) == Array(repeating: L.folders.shareTransferPeerRow(folder: "Trip", who: "ab12cd"), count: 2))
        #expect(rows.map(\.progress) == Array(repeating: L.folders.shareTransferProgress(files: "2", rows: "3"), count: 2))
        #expect(rows.map(\.state) == [L.folders.shareTransferStatePulling, L.folders.shareTransferStateUpToDate])
    }

    /// A device serving nothing still has a plane, and says so — the reading INSIDE a
    /// non-nil view. Distinct from the no-plane case below, which paints nothing.
    @Test func noSharedFoldersIsAReadingNotAnAbsence() {
        let view = FfiSharePlaneView(
            serveStatus: text("folders.share_serve_status_no_sets"), transfers: [])

        #expect(sharePlaneServeStatusText(view) == L.folders.shareServeStatusNoSets)
        #expect(sharePlaneTransferReadings(view).isEmpty)
    }
}

/// `nil` from the FFI cell means "this device is not running the plane" — a different
/// fact from "no shared folders to serve" — and the model must carry it as absence, and
/// return to it when the plane is forgotten (sign-out), so the surface never paints the
/// previous session's readings.
@Suite @MainActor struct SharePlaneModelTests {
    private final class Cell: @unchecked Sendable {
        var view: FfiSharePlaneView?
    }

    @Test func noPlaneIsNoViewAndForgettingItReturnsThere() {
        let cell = Cell()
        let model = SharePlaneModel(read: { cell.view })
        #expect(model.view == nil)

        cell.view = FfiSharePlaneView(
            serveStatus: LocalizedText(key: "folders.share_serve_status_no_sets", args: [:]),
            transfers: [])
        model.refresh()
        #expect(model.view != nil)

        cell.view = nil
        model.refresh()
        #expect(model.view == nil)
    }
}

#endif  // !FAUNA_EXCISE_P2P_SHARE
