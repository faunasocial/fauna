import Foundation

/// Settings → **Sessions** (`docs/goal/ui/sessions.md`), macOS + iOS, one
/// FaunaKit VM. Holds no fold, sort or keep-the-painted-rows rule: every load
/// and act is one call into the UniFFI face (`libs/fauna-ffi/src/sessions.rs`),
/// which runs the shared `SessionsClient::pass` and answers the page to paint
/// and whether the lock landed. The two confirm states (``revokeOthersArmed``,
/// ``lockWord``) are the only state this app owns (`ui/sessions.md` § Where
/// logic lives). Hydrates on the nav edge and re-snapshots after each act —
/// tui's sub-page shape; the closest port is android's `SessionsVM`.
@MainActor @Observable
public final class SessionsVM {
    /// The shared fold's answer verbatim; `nil` until the first hydrate lands,
    /// so `sessions-empty` never paints before it.
    public private(set) var snapshot: FfiSessionsSnapshot?
    public private(set) var revokeOthersArmed = false
    public var lockWord = ""
    public private(set) var busy = false

    /// The lock's word, `LOCK`, from shared Rust — the render's enable check
    /// only; the face re-checks what was typed in its action arm.
    public let lockConfirmWord = sessionsLockoutConfirmWord()

    public init() {}

    /// What every call needs besides its own argument: the connection, the
    /// session's Devices machine (it names device-minted sessions and owns the
    /// sealed-label render) and this app's stored device id.
    public struct Context {
        let api: APIClient
        let devices: DevicesMachine?
        let ownDeviceId: String?
        let onLocked: () -> Void

        public init(api: APIClient, devices: DevicesMachine?, ownDeviceId: String?,
                    onLocked: @escaping () -> Void) {
            self.api = api
            self.devices = devices
            self.ownDeviceId = ownDeviceId
            self.onLocked = onLocked
        }
    }

    /// The nav-edge hydrate.
    public func hydrate(_ ctx: Context) async {
        await run(ctx) { try await ctx.api.sessionsLoad(devices: ctx.devices, ownDeviceId: ctx.ownDeviceId, previous: $0) }
    }

    /// `session-revoke-button[i]` — no confirm (recoverable by re-auth).
    public func revoke(_ tokenId: String, _ ctx: Context) async {
        await run(ctx) {
            try await ctx.api.sessionsRevoke(
                devices: ctx.devices, ownDeviceId: ctx.ownDeviceId, previous: $0, tokenId: tokenId)
        }
    }

    /// `sessions-revoke-others-button` — the first press only arms the confirm.
    public func armRevokeOthers() { revokeOthersArmed = true }

    /// `sessions-revoke-others-cancel-button`.
    public func cancelRevokeOthers() { revokeOthersArmed = false }

    /// `sessions-revoke-others-confirm-button` — the second press.
    public func confirmRevokeOthers(_ ctx: Context) async {
        revokeOthersArmed = false
        await run(ctx) { try await ctx.api.sessionsRevokeOthers(devices: ctx.devices, ownDeviceId: ctx.ownDeviceId, previous: $0) }
    }

    /// `sessions-lockout-button` — the typed word goes to the face verbatim,
    /// which refuses a wrong one on `error-message` and reaches no nest.
    public func lock(_ ctx: Context) async {
        let word = lockWord
        await run(ctx) {
            try await ctx.api.sessionsLockout(
                devices: ctx.devices, ownDeviceId: ctx.ownDeviceId, previous: $0, confirmWord: word)
        }
    }

    private func run(_ ctx: Context, _ call: (FfiSessionsSnapshot?) async throws -> FfiSessionsPage) async {
        guard !busy else { return }
        busy = true
        defer { busy = false }
        let page: FfiSessionsPage
        do {
            page = try await call(snapshot)
        } catch {
            // Only "not connected" reaches here — every pass answers a page.
            logMessage(level: .warn, target: "fauna.sessions",
                       message: "[sessions] the pass could not start: \(error.localizedDescription)")
            return
        }
        snapshot = page.snapshot
        SessionsTestState.paintedTokenIds = page.snapshot.rows.map(\.tokenId)
        if page.locked {
            // The lock landed: every bearer is revoked and the nest mints no new
            // one, so the app leaves the shell for its launch surface, whose
            // re-run challenge lands the locked notice (leg 2).
            logMessage(level: .warn, target: "fauna.sessions",
                       message: "[sessions] the account lock landed — leaving for the launch surface")
            lockWord = ""
            ctx.onLocked()
        }
    }
}

/// The painted token-id list, published at the e2e `settings.session_token_ids`
/// state key (`actions/sessions.py::painted_token_ids`), which every app
/// answers from the rows it painted. `nil` before the first hydrate.
public enum SessionsTestState {
    @MainActor public static var paintedTokenIds: [String]?
}
