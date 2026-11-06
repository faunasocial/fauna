import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// Shared handler for the cross-app `open_route` TestAgent command — the e2e seam
/// of the `fauna://` in-app routes (`docs/goal/architecture/apps/ios.md` § App
/// Entry → *In-app routes*; tui's `open_route` arm is the reference).
///
/// It feeds a URI to the SAME door the app's `onOpenURL` takes
/// (``ConsentHandoff/receive(_:authenticated:)``), so a journey needs no relaunch
/// inside PAR's 90-second window. Lives in FaunaKit so the macOS + iOS shells
/// share ONE implementation, covered by `swift-test` unlike the app targets.
///
/// **Awaited.** The route's page work lands before this acks: the shell
/// navigates to the route's landing and the page opens what the door staged —
/// the consent request through the shared machine (cleared once its open
/// returned), a share route's target once the Media or Folders page has taken it
/// — and this waits for that, so the driver's next read cannot see the pre-open
/// frame. Signed out, the door holds the route (it applies after sign-in) and the
/// command acks at once, as tui's does.
public enum OpenRouteTestCommand {
    /// The longest a route's page work may take before the command refuses.
    /// Generous: the open is one nest round trip behind a fresh visit's read.
    static let openDeadline: TimeInterval = 30

    /// Apply the command. Returns `nil` on success, or a human-readable reason the
    /// caller must surface as a **loud** TestAgent failure — never a silent no-op
    /// (`testing.md` convention 11: honour the command or refuse audibly; an
    /// unparseable URI the launch intake would drop silently is a refusal here,
    /// because a driver that sent one must hear about it).
    @MainActor
    public static func apply(
        _ command: [String: Any],
        authenticated: Bool,
        navigate: @MainActor (ConsentHandoff.Landing) -> Void
    ) async -> String? {
        guard let uri = command["uri"] as? String, let url = URL(string: uri) else {
            return "open_route: no parseable `uri` in the command"
        }
        let landing: ConsentHandoff.Landing
        switch ConsentHandoff.shared.receive(url, authenticated: authenticated) {
        case .notARoute:
            return "open_route: `\(uri)` is not a route this app takes"
        case .held:
            return nil
        case .apply(let applied):
            landing = applied
        }
        navigate(landing)
        guard await ConsentHandoff.shared.waitUntilDrained(timeout: openDeadline) else {
            return "open_route: the \(landing) page did not open the route within "
                + "\(Int(openDeadline)) s (no live page, or no machine in this session)"
        }
        return nil
    }
}

#endif
