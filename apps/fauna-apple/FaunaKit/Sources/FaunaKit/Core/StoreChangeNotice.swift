import SwiftUI

/// The apple app's consumer of the **store-change notice**
/// (`docs/goal/architecture/account-runtime.md` § Multi-instance concurrency
/// → *A runtime's own pump is a source of the notice too*, part 4): the
/// account store changed — this app's pump applied another device's row, a
/// sibling instance or the sync agent committed — so the OPEN surface that
/// reads through the store re-runs its own load.
///
/// The UniFFI face (`libs/fauna-ffi/src/store_change.rs`) crosses one
/// payload-free call, ``FfiStoreChangeListener/storeChanged()``, on a runtime
/// thread. It is a **level, not an event**: what a surface does with it is
/// its own `resync` — re-read, paint only what differs, never discard an edit
/// in progress — and a gesture's own write never fires it, so the gesture's
/// surface repaints from its own answer. This type owns the two halves that
/// are the same for every surface and so live here, once, for macOS and iOS:
///
/// - **Registration, once per process.** The listener slot outlives sign-out
///   and account switch (the relay follows the runtime on its own), so the
///   listener is set before the first runtime can start and is never cleared.
/// - **The hop to the main actor**, as a counter (``generation``) a view
///   observes, rather than a callback list that sign-out would have to unwind.
///
/// Mirrors windows' `NestRpcClient.StoreChanged` / `ViewModelBase.WatchStoreChange`.
@MainActor @Observable
public final class StoreChangeNotice {
    public static let shared = StoreChangeNotice()

    /// Bumped once per notice delivered to this process. A view that is on
    /// screen watches it through ``SwiftUI/View/onStoreChange(_:)``.
    public private(set) var generation = 0

    init() {}

    /// One notice arrived. `internal` so the tests can drive the main-actor
    /// half without a live runtime.
    func deliver() {
        generation += 1
    }

    /// Run `resync` (coalesced, one pass at a time — ``StoreResyncDriver``) on
    /// every notice until the returned watch is cancelled. For the app-wide
    /// store-backed render sources that have no open surface to hang a view
    /// modifier on (the member-review roster, the inherited-filter marks).
    public func watch(_ resync: @escaping @MainActor () async -> Void) -> StoreChangeWatch {
        StoreChangeWatch(notice: self, driver: StoreResyncDriver(resync: resync))
    }

    /// Set the process's one FFI listener — idempotent, callable from any
    /// thread (``APIClient/init(nodeUrl:)`` calls it, before any runtime can
    /// exist). Never cleared at sign-out or account switch.
    public nonisolated static func registerOnce() {
        _ = registration
    }

    /// Lazy-static: Swift runs this initializer exactly once, thread-safely.
    private nonisolated static let registration: Void = {
        setStoreChangeListener(listener: StoreChangeRelay())
    }()
}

/// The FFI listener: hops from the runtime thread to the main actor.
private final class StoreChangeRelay: FfiStoreChangeListener, @unchecked Sendable {
    func storeChanged() {
        Task { @MainActor in StoreChangeNotice.shared.deliver() }
    }
}

/// Runs a surface's `resync` for the notices that arrive while the surface is
/// open, **one pass at a time**: a notice that lands mid-pass is not dropped
/// (the pass may have read before the change) and not stacked — it folds into
/// exactly one more pass after the running one. Mirrors windows'
/// `WatchStoreChange` coalescing.
@MainActor
public final class StoreResyncDriver {
    private let resync: @MainActor () async -> Void
    private var running = false
    private var again = false

    public init(resync: @escaping @MainActor () async -> Void) {
        self.resync = resync
    }

    /// A notice arrived. Returns the running pass's task (`nil` when this
    /// notice only folded into it) so a test can await the drain.
    @discardableResult
    public func notify() -> Task<Void, Never>? {
        if running {
            again = true
            return nil
        }
        running = true
        return Task { @MainActor in
            repeat {
                again = false
                await resync()
            } while again
            running = false
        }
    }
}

/// A live subscription from ``StoreChangeNotice/watch(_:)``.
@MainActor
public final class StoreChangeWatch {
    private let notice: StoreChangeNotice
    private let driver: StoreResyncDriver
    private var cancelled = false

    fileprivate init(notice: StoreChangeNotice, driver: StoreResyncDriver) {
        self.notice = notice
        self.driver = driver
        arm()
    }

    public func cancel() { cancelled = true }

    private func arm() {
        withObservationTracking {
            _ = notice.generation
        } onChange: { [weak self] in
            // `onChange` fires before the new value is stored, from the
            // mutating context; run the pass and re-arm on the next turn.
            Task { @MainActor [weak self] in
                guard let self, !self.cancelled else { return }
                self.driver.notify()
                self.arm()
            }
        }
    }
}

private struct StoreChangeResync: ViewModifier {
    @State private var driver: StoreResyncDriver
    private let notice = StoreChangeNotice.shared

    init(resync: @escaping @MainActor () async -> Void) {
        _driver = State(initialValue: StoreResyncDriver(resync: resync))
    }

    func body(content: Content) -> some View {
        content.onChange(of: notice.generation) { _, _ in driver.notify() }
    }
}

extension View {
    /// Re-drive this view's own store-backed load when the account store
    /// changes while the view is on screen. Attach it to the surface root:
    /// SwiftUI stops delivering once the view leaves the tree, which is the
    /// "open surface only" rule. `resync` must paint only what differs and
    /// keep any edit in progress (the VMs' `resync()` methods do).
    public func onStoreChange(_ resync: @escaping @MainActor () async -> Void) -> some View {
        modifier(StoreChangeResync(resync: resync))
    }
}
