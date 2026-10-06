import Foundation
import Observation

/// The Apple apps' one door for the `fauna://consent/<request_uri>` route — the
/// same-device handoff's app half (`docs/goal/architecture/apps/ios.md` § App
/// Entry → *In-app routes*; mechanism `docs/goal/behavior/authorization-server.md`
/// § Consent → *How the same-device handoff is built*).
///
/// Both shells' `onOpenURL` hand the URL to ``receive(_:authenticated:)``, which
/// parses ONLY through the shared `parseAppRoute` (never a second parser in
/// Swift) and answers whether this URL was a consent route. The route is
/// navigation only: the shell navigates to Settings → Connected apps and the
/// page's view-model calls the shared machine's `openHandoff` for the staged
/// request, which reveals its card in the requests tray.
///
/// A route that arrives signed out is *held* and applied when the session becomes
/// authenticated (``takeHeld()``). Process-wide singleton, like ``MediaDeepOpen``:
/// one pending open at a time is the deep-link semantic, and both shells plus the
/// shared page reach it without threading a binding through every navigation layer.
@Observable
@MainActor
public final class ConsentHandoff {
    public static let shared = ConsentHandoff()

    /// A consent route that arrived while signed out — applied by ``takeHeld()``.
    public private(set) var held: String?

    /// The staged `request_uri` the Connected apps page opens next. Cleared only
    /// AFTER the machine's `openHandoff` has returned (``finishOpen()``), so a
    /// waiter (the e2e `open_route` command) knows the card is painted.
    public private(set) var pending: String?

    /// Internal, not private: the unit tests build their own instance so they
    /// never share the process-wide ``shared`` state.
    init() {}

    /// The consent route's `request_uri`, or `nil` for any URL that is not
    /// exactly that route (an unparseable URI, an unknown route, or one of the
    /// share routes, which have no producer on Apple).
    public nonisolated static func consentRequestUri(from url: URL) -> String? {
        guard case .consent(let requestUri)? = parseAppRoute(uri: url.absoluteString) else { return nil }
        return requestUri
    }

    /// Offer a URL to the door. Returns `true` when it was a consent route — the
    /// caller must then NOT fall through to its other deep-link parsers — and,
    /// when `authenticated`, the request is staged for the page (the caller
    /// navigates to Connected apps). Signed out, the route is held instead.
    @discardableResult
    public func receive(_ url: URL, authenticated: Bool) -> Bool {
        guard let requestUri = Self.consentRequestUri(from: url) else { return false }
        if authenticated {
            pending = requestUri
        } else {
            held = requestUri
        }
        return true
    }

    /// Move a held route into the staged slot now the session is authenticated.
    /// Returns `true` when there was one (the caller then navigates).
    @discardableResult
    public func takeHeld() -> Bool {
        guard let held else { return false }
        self.held = nil
        pending = held
        return true
    }

    /// The staged request the page should open, without consuming it.
    public func peekPending() -> String? { pending }

    /// The page's open finished (the machine's `openHandoff` returned): clear the
    /// staged request, but only if it is still the one that was opened — a newer
    /// route staged meanwhile stays pending.
    public func finishOpen(_ requestUri: String) {
        if pending == requestUri { pending = nil }
    }

    /// Wait until nothing is staged (the page's open has finished) or `timeout`
    /// elapses. Deadline-polled (convention 14); returns whether it drained.
    public func waitUntilDrained(timeout: TimeInterval) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while pending != nil {
            if Date() >= deadline { return false }
            try? await Task.sleep(nanoseconds: 50_000_000)
        }
        return true
    }
}
