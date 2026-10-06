import SwiftUI

/// Thin SwiftUI-friendly proxy over the page-level `LabelerCatalogMachine`
/// (UniFFI, `libs/fauna-labeler-catalog-machine` via `libs/fauna-ffi`). Backs
/// BOTH the Personalization home's subscribed-labelers facet and the
/// standalone Community-labelers catalog page — each view configures its own
/// instance (mirrors `TaskDelegationView`'s reload-on-every-visit shape; the
/// machine is cheap to rebuild over the already-connected `FfiNestClient`).
///
/// Mirrors `DevicesMachineVM` / `MediaMachineVM`'s observer-box pattern:
///   1. builds + owns the machine instance (over the session `FfiNestClient`),
///   2. implements `LabelerCatalogObserver` to translate machine notifications
///      into `@Observable` invalidations on the main actor,
///   3. exposes the latest `LabelerCatalogSnapshot` + small gesture wrappers.
///
/// `content-moderation-and-ranking.md` § Tier-3 community models.
@MainActor @Observable
public final class LabelerCatalogVM {
    /// `nil` until `configure` succeeds.
    public private(set) var machine: LabelerCatalogMachine?

    /// One-time connect/build failure (`api.labelerCatalogMachine` threw). Page
    /// read/write failures live on the machine snapshot's `error` instead; both
    /// are surfaced through `errorMessage`.
    public private(set) var connectError: String?

    private let observerBox = LabelerCatalogObserverBox()

    public init() {}

    /// Vend the machine from `APIClient` and load the first snapshot. Idempotent
    /// — the machine is built once per VM instance; later calls only re-`refresh()`.
    public func configure(api: APIClient) async {
        guard machine == nil else {
            await refresh()
            return
        }
        observerBox.target = self
        do {
            machine = try await api.labelerCatalogMachine(observer: observerBox)
        } catch {
            connectError = DisplayError.message(error)
            return
        }
        await refresh()
    }

    fileprivate func onMachineChanged() {
        _observerTick &+= 1
    }
    private var _observerTick: UInt64 = 0

    /// The whole renderable catalog in one record (entries, inspect view, page
    /// error). `nil` until `configure`.
    public var snapshot: LabelerCatalogSnapshot? {
        _ = _observerTick
        return machine?.snapshot()
    }

    /// The page-level `error-message`: the connect failure first, else the
    /// machine snapshot's localized `error`.
    public var errorMessage: String? {
        _ = _observerTick
        return firstNonNil(connectError, machine?.snapshot().error.map(renderLocalizedText))
    }

    public func refresh() async { await machine?.refresh() }
    public func inspect(index: Int) async { await machine?.inspect(index: UInt32(index)) }
    public func closeInspect() { machine?.closeInspect() }
    public func subscribe(index: Int) async { await machine?.subscribe(index: UInt32(index)) }
    public func unsubscribe(index: Int) async { await machine?.unsubscribe(index: UInt32(index)) }
}

/// Trampoline conforming to UniFFI's `LabelerCatalogObserver`. The machine
/// takes the observer at construction time (`build_labeler_catalog_machine_with_grants`),
/// so late-binding via `target` lets the VM register itself after `configure`.
/// Mirrors `DevicesObserverBox`.
final class LabelerCatalogObserverBox: LabelerCatalogObserver, @unchecked Sendable {
    weak var target: LabelerCatalogVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onMachineChanged() }
    }
}
