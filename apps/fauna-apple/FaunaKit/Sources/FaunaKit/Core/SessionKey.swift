import Foundation

/// A `.task(id:)` key that changes exactly when the environment's `FaunaClient`
/// **instance** does — the identity a page-owned view model is scoped to
/// (`docs/goal/architecture/apps/account-scoping.md` § The scoping taxonomy, the
/// in-memory corollary).
///
/// Why not `client != nil`, the key the shell uses: that flips only across a nil
/// phase, so a switch whose incoming client lands in the same view-update pass as the
/// outgoing one's teardown never re-fires the task and the page keeps serving the
/// account it was configured for. Comparing the instance (`===`) re-fires on any
/// change of client — the nil phase and the new one are both keys — and the
/// view model's own api-identity guard (`configure` on a different `APIClient` drops
/// before it rebuilds) settles the rest, so this key is the trigger, never the
/// guarantee.
///
/// Compared by reference and held strongly, so the comparison is on a live object
/// rather than a recyclable `ObjectIdentifier`. `reloadToken` carries a page's own
/// reload signal (`appState.navGeneration`) so one `.task(id:)` serves both.
public struct SessionKey: Equatable {
    private let client: FaunaClient?
    private let reloadToken: Int

    public init(_ client: FaunaClient?, reloadToken: Int = 0) {
        self.client = client
        self.reloadToken = reloadToken
    }

    public static func == (lhs: SessionKey, rhs: SessionKey) -> Bool {
        lhs.client === rhs.client && lhs.reloadToken == rhs.reloadToken
    }
}
