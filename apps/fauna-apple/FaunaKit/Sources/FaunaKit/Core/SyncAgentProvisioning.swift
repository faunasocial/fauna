import Foundation

/// Bridges the app's live minted bearer to the Rust sync-agent provisioning loop
/// (`FfiProvisioningBearerSource`) — read fresh on every convergence tick and
/// pushed to the agent as `RefreshBearer`. Stateless, like the File Provider's
/// `KeychainBearerProvider`: each `currentBearer()` is a fresh read off the
/// `APIClient`, so a bearer `authenticate()` rotated in is picked up without
/// rebuilding the provisioner. An empty token ⇒ not authenticated (the tick then
/// skips); a stale token is harmless — the agent self-renews via its `RenewBearer`
/// grant, the app-pushed bearer is bootstrap/fallback.
public final class APIClientProvisioningBearerSource: FfiProvisioningBearerSource, @unchecked Sendable {
    private let api: APIClient

    public init(api: APIClient) {
        self.api = api
    }

    public func currentBearer() -> FfiProvisioningBearer {
        let (token, expiresAt) = api.currentBearerForProvisioning()
        return FfiProvisioningBearer(token: token, expiresAt: expiresAt)
    }
}
