import FaunaFFISwift
import Foundation

/// The unreadable-index floor's confirm ("start over on this device",
/// `account-index-reset-confirm-button`) — **asked before its erase**
/// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
/// sibling serves the account*).
///
/// The floor runs sign-out's erase, which on macOS sweeps the platform store
/// root (`AccountStateDir.storeContainerDir` is `nil` there) — the root a tui
/// or the sync agent on the same Mac serves from. So the confirm asks the
/// shared door, `fauna-ffi`'s `start_over_blocked`, first — about the
/// registry's accounts plus every actor scope under the two bases the erase
/// sweeps, since a malformed index names nobody. On a refusal nothing runs:
/// no keychain clear, no scope erase; the caller paints the returned line as
/// the launch page's `error-message` with the confirm still showing, so
/// closing the other window and pressing it again is the whole remedy.
///
/// Twin of tui's `launch::start_over_unless`, linux's
/// `account_scope::start_over_blocked` and windows'
/// `LaunchAccountIndexUnreadablePage.OnResetConfirmClick`. iOS does not route
/// through it: it admits one instance per app and shares a store with nobody.
public enum AccountIndexStartOver {
    /// Ask, then run `erase` only if nobody else serves an account it reaches.
    /// Returns the refusal line (resolved through the app's i18n), or `nil`
    /// once `erase` has run.
    ///
    /// `ownLock` is this window's raw per-account instance lock when it holds
    /// one — `nil` on the unreadable-index gate in practice, since launch
    /// never admitted an account — passed so the probe can put it down and
    /// not meet its own reflection (`SignOutSection`'s `ownLock` doc).
    @MainActor
    public static func confirm(
        ownLock: FfiAccountInstanceLock?,
        erase: () async -> Void
    ) async -> String? {
        await confirm(
            registry: FaunaAccounts.registry(),
            baseDir: AccountStateDir.base.path,
            storeContainerDir: AccountStateDir.storeContainerDir,
            ownLock: ownLock,
            erase: erase)
    }

    /// The directory-explicit half of `confirm(ownLock:erase:)` — the seam a
    /// test drives over temp bases, since the production root is the
    /// developer's own (`AccountStateDir.erase(actorIdHex:storeContainerDir:)`).
    /// The bases must be the ones `erase` sweeps: the question is about
    /// exactly the erase's reach.
    @MainActor
    static func confirm(
        registry: FfiAccountRegistry,
        baseDir: String,
        storeContainerDir: String?,
        ownLock: FfiAccountInstanceLock?,
        erase: () async -> Void
    ) async -> String? {
        if let blocked = startOverBlocked(
            registry: registry,
            baseDir: baseDir,
            storeContainerDir: storeContainerDir,
            ownLock: ownLock
        ) {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message:
                    "[start-over] refused: \(blocked.accounts.count) account(s) served by "
                    + "another instance: \(blocked.accounts.joined(separator: ", "))")
            return renderLocalizedText(blocked.line)
        }
        await erase()
        return nil
    }
}
