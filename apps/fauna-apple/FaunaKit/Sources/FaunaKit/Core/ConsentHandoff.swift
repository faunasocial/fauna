import Foundation
import Observation

/// The Apple apps' one door for the `fauna://` in-app routes — the consent route
/// (the same-device handoff's app half) and the two share routes (Finder's and
/// the Files app's Share action) (`docs/goal/architecture/apps/ios.md` § App
/// Entry → *In-app routes*; mechanisms `docs/goal/behavior/authorization-server.md`
/// § Consent → *How the same-device handoff is built* and
/// `docs/goal/architecture/apps/macos.md` § Sync → *Finder's Share entry*).
///
/// Both shells' `onOpenURL` hand the URL to ``receive(_:authenticated:)``, which
/// parses ONLY through the shared `parseAppRoute` (never a second parser in
/// Swift) and answers whether this URL was a route, and where it lands. Every
/// route is navigation only: the shell navigates to the ``Landing`` and the page
/// opens what was staged — the consent request through the shared machine's
/// `openHandoff` (staged here), a share route's target through ``ShareRouteOpen``.
///
/// A route that arrives signed out is *held* and applied when the session becomes
/// authenticated (``takeHeld()``). Process-wide singleton, like ``MediaDeepOpen``:
/// one pending open at a time is the deep-link semantic, and both shells plus the
/// shared page reach it without threading a binding through every navigation layer.
@Observable
@MainActor
public final class ConsentHandoff {
    public static let shared = ConsentHandoff(shares: .shared)

    /// Where an applied route lands — each shell maps it onto its own navigation.
    public enum Landing: Equatable, Sendable {
        /// Settings → Connected apps (the consent route).
        case connectedApps
        /// Media (the `share-link` route).
        case media
        /// Settings → Folders (the `folder-share` route).
        case folders
    }

    /// What the door did with a URL.
    public enum Reception: Equatable, Sendable {
        /// Not one of the shared routes — fall through to the other parsers.
        case notARoute
        /// Signed out: held for ``takeHeld()``.
        case held
        /// Staged: the caller navigates to the landing.
        case apply(Landing)
    }

    /// A route that arrived while signed out — applied by ``takeHeld()``.
    public private(set) var held: AppRoute?

    /// The staged `request_uri` the Connected apps page opens next. Cleared only
    /// once the card is painted on the newest visit of the page
    /// (``finishOpen(_:visit:)`` or ``finishReread(_:)``), so a waiter (the e2e
    /// `open_route` command) knows it is on screen.
    public private(set) var pending: String?

    /// The share routes' staged targets, taken by the Media and Folders pages.
    let shares: ShareRouteOpen

    /// Internal, not private: the unit tests build their own instance so they
    /// never share the process-wide ``shared`` state.
    init(shares: ShareRouteOpen? = nil) {
        self.shares = shares ?? ShareRouteOpen()
    }

    /// The consent route's `request_uri`, or `nil` for any URL that is not
    /// exactly that route.
    public nonisolated static func consentRequestUri(from url: URL) -> String? {
        guard case .consent(let requestUri)? = parseAppRoute(uri: url.absoluteString) else { return nil }
        return requestUri
    }

    /// Offer a URL to the door. Anything but ``Reception/notARoute`` means the
    /// caller must NOT fall through to its other deep-link parsers; on
    /// ``Reception/apply(_:)`` it navigates to the landing.
    @discardableResult
    public func receive(_ url: URL, authenticated: Bool) -> Reception {
        guard let route = parseAppRoute(uri: url.absoluteString) else { return .notARoute }
        guard authenticated else {
            held = route
            return .held
        }
        return .apply(stage(route))
    }

    /// Move a held route into its staged slot now the session is authenticated.
    /// Returns its landing when there was one (the caller then navigates).
    @discardableResult
    public func takeHeld() -> Landing? {
        guard let held else { return nil }
        self.held = nil
        return stage(held)
    }

    private func stage(_ route: AppRoute) -> Landing {
        switch route {
        case .consent(let requestUri):
            pending = requestUri
            stagedAtVisit = visitCount
            spent = nil
            return .connectedApps
        case .shareLink(let folderId, let path):
            shares.stageShareLink(folderId: folderId, path: path)
            return .media
        case .folderShare(let folderId):
            shares.stageFolderShare(folderId: folderId)
            return .folders
        }
    }

    /// The staged request the page should open, without consuming it.
    public func peekPending() -> String? { pending }

    /// The `request_uri` whose open is in flight — set by
    /// ``beginOpen(visit:)``, cleared by ``finishOpen(_:visit:)``.
    private var opening: String?

    /// Visits of the Connected apps page, numbered across every instance of it
    /// (``registerVisit()``). A page left mounted from an earlier session can
    /// still be finishing a visit over that session's client when a route
    /// lands — so only the NEWEST visit, begun after the request was staged,
    /// may open it. Every route lands through a navigation that starts such a
    /// visit (the shells' `land` bumps the page's reload token).
    private var visitCount = 0
    private var stagedAtVisit = 0

    /// A request an overtaken visit opened: the nest has spent it, so it is
    /// never opened again — the newest visit re-reads its tray, where the nest
    /// lists the card (``awaitSpent(visit:)``).
    private var spent: String?
    private var openWaiters: [CheckedContinuation<Void, Never>] = []

    /// Number a page visit as it starts.
    public func registerVisit() -> Int {
        visitCount += 1
        return visitCount
    }

    /// Claim the staged request for opening, or `nil` when none is staged, its
    /// open is already in flight or done, or `visit` is not the newest visit
    /// begun since it was staged. The nest answers a second open of one request
    /// "expired or already used", so exactly one visit ever opens it.
    public func beginOpen(visit: Int) -> String? {
        guard let pending, opening == nil, spent != pending,
              visit == visitCount, visit > stagedAtVisit
        else { return nil }
        opening = pending
        return pending
    }

    /// The open of `requestUri` returned. Answers whether `visit` is still the
    /// newest — it then paints the card and the staged request is cleared;
    /// otherwise the request is left for the newest visit to re-read. A newer
    /// route staged meanwhile stays pending either way.
    @discardableResult
    public func finishOpen(_ requestUri: String, visit: Int) -> Bool {
        if opening == requestUri { opening = nil }
        defer { wakeOpenWaiters() }
        if visit == visitCount {
            if pending == requestUri { pending = nil }
            return true
        }
        if pending == requestUri { spent = requestUri }
        return false
    }

    /// For a visit that could not claim the staged request: wait out an open
    /// in flight, then answer the request an older visit opened, which this
    /// visit — when it is the newest — must re-read its tray for and then
    /// ``finishReread(_:)``.
    public func awaitSpent(visit: Int) async -> String? {
        while opening != nil {
            await withCheckedContinuation { openWaiters.append($0) }
        }
        guard visit == visitCount, let spent, spent == pending else { return nil }
        return spent
    }

    /// The newest visit has re-read the tray for a request an older one opened.
    public func finishReread(_ requestUri: String) {
        if pending == requestUri { pending = nil }
        if spent == requestUri { spent = nil }
    }

    private func wakeOpenWaiters() {
        let waiters = openWaiters
        openWaiters = []
        for waiter in waiters { waiter.resume() }
    }

    /// Wait until nothing is staged (the page's open has finished — a share
    /// route's once its page has taken it) or `timeout`
    /// elapses. Deadline-polled (convention 14); returns whether it drained.
    public func waitUntilDrained(timeout: TimeInterval) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while pending != nil || shares.isPending {
            if Date() >= deadline { return false }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        return true
    }
}
