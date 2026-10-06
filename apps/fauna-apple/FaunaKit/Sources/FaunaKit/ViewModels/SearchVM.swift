import SwiftUI

/// Thin SwiftUI-friendly proxy over the shared, stateful `FfiSearchManager`
/// (`fauna_client_search::SearchManager` via UniFFI) — the `FeedVM` twin for
/// the Search page (`docs/goal/ui/search.md` § State & data shape). All
/// query/filter/paging/merge decisions live in the shared Rust manager; this
/// class owns the instance, translates its notifications into `@Observable`
/// invalidations, and forwards gestures. No client-side result-card state, no
/// client-side local-file substring scan, no client-side paging limit — the
/// bespoke pre-manager "On This Device" split this replaced computed all of
/// that itself (`search.md` § Implementation status today, the retired ⚠
/// macOS bullet).
///
/// Shared by the macOS and iOS apps; identical behaviour on both.
@MainActor @Observable
public final class SearchVM {
    /// The shared manager, `nil` until `configure` runs (it needs a connected
    /// `NestClient`, so there is no bare/offline constructor) and again after
    /// `reset()`.
    ///
    /// **Account-scoped in-memory state** — this manager, its snapshot, the
    /// local-index attachment and the typed `query` all belong to one account
    /// and are dropped by the ONE canonical drop, ``reset()``
    /// (`account-scoping.md` § The scoping taxonomy, the in-memory corollary).
    public private(set) var manager: FfiSearchManager?

    /// `search-query-field`'s live buffer — deliberately NOT manager state.
    /// The field is plain local state on every app and submit reads it
    /// explicitly, so a keystroke never implies network work (`search.md`
    /// § Where logic lives — submit-driven, no debounce). The manager's
    /// `snapshot().query` is the LAST FIRED query, which `search(loadMore:)`
    /// re-issues on "load more" rather than whatever is now in this buffer.
    public var query = ""
    /// `search-toggle-button` — pure view affordance, no search semantics
    /// (`search.md` § User actions).
    public var isSearchVisible = false

    /// The `APIClient` this VM is scoped to — the identity key. A fresh login
    /// mints a new `APIClient` (`FaunaClient.api` is a `let`), so a different
    /// instance is what signals an account switch and must drop and rebuild
    /// rather than keep serving the previous account's manager (the `FeedVM`
    /// cross-actor-leak lesson). Held strongly, so the comparison is on a live
    /// object rather than a recyclable `ObjectIdentifier`.
    ///
    /// Invariant: `manager` is non-nil only if it was built over this `api` —
    /// `api` moves only after ``reset()`` has dropped the manager.
    private var api: APIClient?
    private var observerBox: SearchObserverBox?
    private var _observerTick: UInt64 = 0
    /// Whether this manager's local search-index arm (backend 2) has been
    /// successfully registered — set only on a `true` return, so a `false`
    /// attempt (no conversations session yet, or this actor has no mail) can
    /// retry later rather than being treated as permanently settled.
    private var indexAttached = false

    public init() {}

    /// Drop everything this VM holds for the account it was scoped to — the ONE
    /// canonical drop (`account-scoping.md` § The scoping taxonomy, the in-memory
    /// corollary: at the identity change itself, keyed on the identity, with no
    /// hand-listed field set at each caller). ``configure(api:)`` calls it when
    /// the api changes and the apps call it on the nil-client phase of a switch
    /// or sign-out, so a field added to this VM is dropped at every site by
    /// adding it here and nowhere else.
    ///
    /// Retires the outgoing manager too — cancels a query it has in flight and
    /// detaches this VM's observer — and clears `api`, so a build or an attach
    /// still suspended for the outgoing account finds its identity gone and
    /// drops its own result instead of landing it (the in-flight clause).
    public func reset() {
        manager?.cancel()
        manager?.clearObservers()
        observerBox?.target = nil
        manager = nil
        observerBox = nil
        api = nil
        indexAttached = false
        query = ""
    }

    /// Build (or reuse) the manager for `api`. Idempotent for the *same*
    /// `APIClient` instance (a Search-page remount keeps the existing manager,
    /// and a retry after a failed first build keeps the typed `query`); a
    /// DIFFERENT `APIClient` (a re-login mints a fresh one) drops the previous
    /// account's state via ``reset()`` **before** building, so a build that then
    /// throws leaves the page empty and retryable — never the previous account's
    /// manager, results or attachment paired with the new api.
    public func configure(api: APIClient) async {
        if let current = self.api, current !== api { reset() }
        self.api = api
        guard manager == nil else { return }
        do {
            let mgr = try await api.searchManager()
            // The build suspended: a `reset()` (the switch's nil-client phase) or
            // a `configure` for another api may have run meanwhile, and its
            // result must not land on that account. A concurrent build for this
            // same api that finished first also wins — one manager, one observer.
            guard self.api === api, manager == nil else { return }
            let box = SearchObserverBox()
            mgr.addObserver(observer: box)
            box.target = self
            self.manager = mgr
            self.observerBox = box
            self.indexAttached = false
            onManagerChanged()
            await attachLocalIndexIfNeeded()
        } catch {
            // No connection yet — the VM stays empty (a first build) or was
            // already emptied by `reset()` above (a switch), and the next
            // `configure` retries.
        }
    }

    /// Register the local sealed index arm if it hasn't attached yet. A
    /// `false` return is a normal state (no conversations session yet, or
    /// this actor has no mail), so `indexAttached` stays false and a later
    /// call — `configure` again, or a reconnect — can retry.
    public func attachLocalIndexIfNeeded() async {
        guard let manager, let api, !indexAttached else { return }
        let attached = await api.attachLocalSearchIndex(manager: manager)
        // The attach suspended: a `reset()` or a switch that ran meanwhile means
        // `manager` is no longer this VM's, and its success must not mark the
        // NEXT account's manager attached (which would skip that account's retry).
        if attached, self.manager === manager {
            indexAttached = true
        }
    }

    // ── Observer ────────────────────────────────────────────────────────────
    fileprivate func onManagerChanged() {
        _observerTick &+= 1
    }

    // ── Convenience getters (read freshly on every access) ──────────────────
    public var snapshot: SearchSnapshot? {
        _ = _observerTick
        return manager?.snapshot()
    }
    public var results: [SearchResultRow] { snapshot?.results ?? [] }
    /// Mirrors `fauna_client_search::kind::TYPE_FILTER_ALL` — not UniFFI-exported
    /// (a plain string literal, not worth a face), so this is the one place the
    /// sentinel is spelled out; `searchTypeFilterOptions()`'s first element is
    /// always this same token (`kind.rs`'s `TYPE_FILTER_OPTIONS` construction).
    public static let typeFilterAll = "all"
    public var typeFilter: String { snapshot?.typeFilter ?? Self.typeFilterAll }
    public var isSearching: Bool { snapshot?.inFlight ?? false }
    public var hasSearched: Bool { !(snapshot?.query.isEmpty ?? true) }
    public var noResults: Bool { snapshot?.noResults ?? false }
    public var hasMore: Bool { snapshot?.hasMore ?? false }
    public var errorMessage: String? { snapshot?.error.map(renderLocalizedText) }

    // ── User actions (`search.md` § User actions) ───────────────────────────
    /// `search-submit-button`, and `search-type-filter` when the filter
    /// changes (`newTypeFilter` keeps the live buffer, re-firing under the new
    /// token — mirrors tui/linux/web's "SetTypeFilter" semantics). A blank
    /// query is a no-op every app's search bar shares.
    ///
    /// Retries the local-index attach first (idempotent — `attachLocalIndexIfNeeded`
    /// no-ops once `indexAttached`): `configure`'s own first attempt can race the
    /// login-time `conversationsSession` build (documented at its `.onReconnect`
    /// retry call site), and a user who enables mail mid-session — never
    /// reconnecting — would otherwise stay nest-only for the rest of the
    /// process. A real search submission is the natural moment to retry, same
    /// spirit as `content-index.md`'s "the manager mints on the first query
    /// that finds no arm" for the arm itself.
    public func search(newTypeFilter: String? = nil) async {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return }
        await attachLocalIndexIfNeeded()
        await manager?.runQuery(query: q, typeFilter: newTypeFilter ?? typeFilter)
    }
    /// `search-load-more-button` — re-fire the last fired query with a grown
    /// page; a no-op when the affordance isn't offered.
    public func loadMore() async {
        await manager?.loadMore()
    }
    /// `search-clear-button` — clears the query buffer only; the loaded
    /// results are left as-is until the next fire (mirrors linux's
    /// `clear_btn`).
    public func clear() {
        query = ""
    }
    /// `search-cancel-button` — resets the page to pre-search. Synchronous:
    /// the manager bumps its query generation so a reply still in flight lands
    /// on the cancelled page and is dropped.
    public func cancel() {
        query = ""
        manager?.cancel()
    }
}

/// Trampoline conforming to UniFFI's `SearchSnapshotObserver` (distinct from
/// `FeedSnapshotObserver` — a bare `SnapshotObserver` collides in the C#
/// bindgen's flattened namespace). Late-binding via `target` mirrors
/// `FeedObserverBox`, since the manager takes the observer before `self` is
/// fully initialized.
final class SearchObserverBox: SearchSnapshotObserver, @unchecked Sendable {
    weak var target: SearchVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onManagerChanged() }
    }
}
