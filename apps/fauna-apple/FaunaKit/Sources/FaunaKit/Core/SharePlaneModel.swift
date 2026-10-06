import Foundation
import Observation

// EXCISED BY `FAUNA_EXCISE_P2P_SHARE` — the `p2p-share` registry member's own
// apple compile condition (one condition per member, `dynamic-features.md`
// § Platform-family surface excision; every store-safe Swift invocation passes it
// beside `FAUNA_EXCISE_PAYMENTS`). The store-safe FFI is `--no-default-features
// --features store-safe`, which drops `p2p-share` (it "rides `default` and NEVER
// `store-safe`", `libs/fauna-ffi/Cargo.toml`), so a store-safe
// `FaunaFFI.xcframework` exports no `FfiSharePlaneView`, no `FfiSharePlaneListener`
// and no `startSharePlane`: every line of the plane's Swift leg is a compile error
// in that flavor — this file, `SharePlaneViews.swift`, and the call sites in
// `APIClient`, `FaunaClient`, `FoldersContent` and `FaunaMacApp`. The same
// member's co-present CEREMONY (`OfflineShareViews.swift` and its glue) carries the
// condition too, although its FFI face still sits in `store-safe`: there the
// compiler catches nothing, so `test_payments_excision_spine.py`'s apple p2p pins
// are what keep it excised.
#if !FAUNA_EXCISE_P2P_SHARE

/// The **peer-transfer plane's** observable surface on Apple (`p2p.md` § Cross-user
/// shared-set transfer → *Implementation status today*, the FFI share-plane host
/// paragraph). Shared by macOS + iOS (one FaunaKit surface, priority #2); iOS does
/// not call [start] until it hosts an account runtime, so there the section simply
/// never renders.
///
/// **What lives here and what does not.** Everything app-agnostic — the driver, the
/// five seam answers, the durable advertisement sink, the six readings — is shared
/// Rust behind `FfiNestClient.startSharePlane`. What is genuinely per-app is the
/// three inputs the shell hands over (the account's own sync-agent provisioner, a
/// spool directory) and the repaint nudge, which is this class.
///
/// **Why a process-wide `shared`.** The plane's state cell is itself a process
/// global on the Rust side (`share_plane_view()`), one per session — this is its
/// observable mirror, so the Folders page reads one instance whichever shell built
/// it. The precedent is the `*Cadence.shared` loops next to it.
///
/// **No ordering contract with the caller.** `startSharePlane` needs the account
/// runtime's store AND the conversations session, and both land on their own tasks
/// after the calls that start them return. Shared Rust waits for them (bounded), so
/// this hop is one call from wherever the provisioner starts, not a sequence to get
/// right in each shell.
@MainActor
@Observable
public final class SharePlaneModel {
    public static let shared = SharePlaneModel()

    /// The transfer surface's readings — `nil` while the plane is not running on
    /// this device this session. **A different fact from "no shared folders to
    /// serve"** (which is a reading *inside* a non-nil view): the surface renders
    /// nothing at all for `nil`, because "this device is not running the plane" owes
    /// the user no line (the linux leg's finding, `p2p.md` § Cross-user shared-set
    /// transfer → *Three findings from the second leg*).
    public private(set) var view: FfiSharePlaneView?

    /// How the readings are fetched — the FFI's process-global cell in production;
    /// a seam so a test can hand the model any state without a live plane.
    private let read: @Sendable () -> FfiSharePlaneView?

    public init(read: @escaping @Sendable () -> FfiSharePlaneView? = { sharePlaneView() }) {
        self.read = read
    }

    /// Re-read the plane's cell. Both repaint nudges land here, and so does
    /// sign-out (the account runtime's teardown forgets the cell, so this then reads
    /// `nil` and the surface disappears with the session it belonged to).
    public func refresh() {
        view = read()
    }

    /// Start the plane for the signed-in account. Best-effort like every other
    /// post-auth hook: a failure is logged and leaves the surface absent, never
    /// failing the sign-in or the launch.
    ///
    /// - Parameters:
    ///   - ownerSecretHex: THIS instance's own secret (`FaunaClient`'s
    ///     `ownSecretHex`), never the active-account pointer.
    ///   - provisioner: the app's own sync-agent provisioner — the plane's two agent
    ///     verbs ride it. macOS runs a real external `fauna-sync-agent`; android (and
    ///     iOS) run none, which is why those legs are not this call.
    ///   - spoolDir: app-private scratch for in-flight peer downloads.
    public func start(
        api: APIClient, ownerSecretHex: String, provisioner: FfiSyncAgentProvisioner,
        spoolDir: URL
    ) async {
        do {
            try FileManager.default.createDirectory(
                at: spoolDir, withIntermediateDirectories: true)
            try await api.startSharePlane(
                ownerSecret: hex_to_data(ownerSecretHex), provisioner: provisioner,
                spoolDir: spoolDir.path,
                listener: SharePlaneRepaintListener { [weak self] in
                    Task { @MainActor in self?.refresh() }
                })
            refresh()
        } catch {
            logMessage(
                level: .warn, target: "fauna.share",
                message: "[share-plane] start failed; the peer-transfer surface stays absent "
                    + "this session: \(error)")
        }
    }
}

/// The plane's two repaint nudges. **Neither carries data** — each just tells the
/// model to re-read `sharePlaneView()`, the one authoritative read
/// (`FfiSharePlaneListener`'s own contract), so `seatBound` and `stateChanged` are
/// deliberately the same act. Called from a Rust thread, hence the closure hop.
final class SharePlaneRepaintListener: FfiSharePlaneListener {
    private let onChange: @Sendable () -> Void

    init(onChange: @escaping @Sendable () -> Void) {
        self.onChange = onChange
    }

    func seatBound() { onChange() }
    func stateChanged() { onChange() }
}

/// One `share-transfer-item`'s three readings, already rendered.
public struct SharePlaneTransferReading: Equatable, Sendable {
    public let name: String
    public let progress: String
    public let state: String
}

/// The `share-serve-status` line.
///
/// ⚠ **Every reading here is resolved NESTED, never with `renderLocalizedText`.**
/// The transfer gate's honest refusal reads "Limited by {source}" where `{source}`
/// is itself an i18n KEY (`features.tier_admin`), not data — a plain substitution
/// paints the raw key at the user. tui's and linux's own readings resolve nested
/// for the same reason and each pins it with a unit test; so does this one
/// (`SharePlaneReadingsTests`).
public func sharePlaneServeStatusText(_ view: FfiSharePlaneView) -> String {
    renderLocalizedTextNested(view.serveStatus)
}

/// The `share-transfer-list`'s rows, one per (set × peer) pull outcome, resolved
/// nested (see [sharePlaneServeStatusText]).
public func sharePlaneTransferReadings(_ view: FfiSharePlaneView) -> [SharePlaneTransferReading] {
    view.transfers.map { transfer in
        SharePlaneTransferReading(
            name: renderLocalizedTextNested(transfer.name),
            progress: renderLocalizedTextNested(transfer.progress),
            state: renderLocalizedTextNested(transfer.state))
    }
}

#endif  // !FAUNA_EXCISE_P2P_SHARE
