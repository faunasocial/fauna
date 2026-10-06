import FaunaFFISwift
import Foundation

/// The state of `identity_choice`'s `sign-out-residue` view while a sign-out's
/// residue still owes work: its one line, and Remove Again
/// (`account-scoping.md` § Erasure follows scope → *the residue surface*).
/// A protocol so `OnboardingVM` is driven by a fake in unit tests, and so
/// nothing a view binds is a UniFFI type. Twin of windows'
/// `ISignOutResidueSurface`.
public protocol SignOutResidueSurfacing: AnyObject, Sendable {
    /// `sign-out-residue-message`, already localized — the shared `Rendered`
    /// copy, or the retry's own refusal while another window serves one of the
    /// residue's accounts.
    var line: String { get }

    /// `sign-out-residue-retry-button`: re-sweep exactly what this residue
    /// recorded and return what is left — `nil` when the device is now clean,
    /// which closes the view. Blocking file I/O: call it off the main actor.
    func retry() -> (any SignOutResidueSurfacing)?
}

/// Where this seat's sign-out erases, and how it reaches its credential
/// registry — what the residue face needs to ask the sign-out's own questions
/// again. `baseDir` is the install base (`AccountStateDir.base`), where the
/// record is kept and where a sibling window holds its instance lock;
/// `storeContainerDir` is what the seat passes to `accountStateEraseAllScopes`
/// and `signOutBlocked` (`nil` on macOS, the iOS app-group container on iOS).
struct ResidueSeat: Sendable {
    let baseDir: String
    let storeContainerDir: String?
    let registry: @Sendable () -> FfiAccountRegistry

    /// The production seat: this install's base, store root and keychain.
    static var live: ResidueSeat {
        ResidueSeat(
            baseDir: AccountStateDir.base.path,
            storeContainerDir: AccountStateDir.storeContainerDir,
            registry: { FaunaAccounts.registry() })
    }
}

/// Apple's credential erase — the one its sign-out runs (`StatusVM.signOut`:
/// the registry's `clearAll()`, which reads back what it deleted) — handed to
/// shared Rust as the residue retry's `FfiResidueCredentialEraser`, so the
/// retry re-runs exactly what the sign-out ran. Apple has one credential store
/// and no wholesale platform reset behind it (android's eraser has both), so
/// this is the whole sequence. Shared Rust calls it only when the recorded
/// credential half is not clean, and reads the RECORDED keys back on top of
/// its answer.
final class SignOutCredentialEraser: FfiResidueCredentialEraser, @unchecked Sendable {
    private let registry: @Sendable () -> FfiAccountRegistry

    init(registry: @escaping @Sendable () -> FfiAccountRegistry) {
        self.registry = registry
    }

    func eraseCredentials() -> FfiCredentialSweep {
        registry().clearAll()
    }
}

/// The apple seat of the sign-out residue surface, over the `fauna-ffi`
/// residue face (`libs/fauna-ffi/src/sign_out_residue.rs`) android and windows
/// use. The record, the four re-sweep rules and every word of the line are
/// shared Rust; this only carries the seat's bases across and localizes the
/// line. One FaunaKit seat serves macOS and iOS. Twin of windows'
/// `SignOutResidueSurface` and android's `AccountStores.recordResidue` /
/// `retryResidue` / `recheckResidueAtLaunch`.
///
/// `@unchecked Sendable`: every stored property is immutable, and the UniFFI
/// object it wraps is internally synchronized.
public final class SignOutResidueSurface: SignOutResidueSurfacing, @unchecked Sendable {
    private let seat: ResidueSeat
    private let residue: FfiSignOutResidue
    public let line: String

    private init(seat: ResidueSeat, residue: FfiSignOutResidue) {
        self.seat = seat
        self.residue = residue
        self.line = renderLocalizedText(residue.line())
    }

    /// The sign-out's half: record what its two erases left — the
    /// `AccountStateDir.eraseAll` sweep and the registry's `clearAll()`
    /// read-back — under the install base, so it outlives this process, and
    /// return the view to paint. `nil` is the clean outcome, and the only one;
    /// a clean erase says nothing and keeps no record.
    ///
    /// The survivor paths were logged by `AccountStateDir.eraseAll`; the
    /// credential keys are logged here. Neither reaches the line.
    public static func record(
        sweep: FfiEraseSweep, credentials: FfiCredentialSweep
    ) -> (any SignOutResidueSurfacing)? {
        record(seat: .live, sweep: sweep, credentials: credentials)
    }

    static func record(
        seat: ResidueSeat, sweep: FfiEraseSweep, credentials: FfiCredentialSweep
    ) -> (any SignOutResidueSurfacing)? {
        if !credentials.survivors.isEmpty || credentials.wipeFailed {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message:
                    "[sign-out residue] sign-in credentials SURVIVED the erase (wipe failed: "
                    + "\(credentials.wipeFailed)); still readable: "
                    + credentials.survivors.joined(separator: ", "))
        }
        return painted(
            seat,
            signOutResidueRecord(baseDir: seat.baseDir, sweep: sweep, credentials: credentials))
    }

    /// The signed-out launch's silent re-check: a record a previous sign-out
    /// left is re-swept FIRST, and the view comes back only if something is
    /// still left. Shared Rust leaves the record alone while the registry holds
    /// an account — a signed-in launch is not the user the residue was reported
    /// to. Blocking file I/O: call it off the main actor.
    public static func recheckAtLaunch() -> (any SignOutResidueSurfacing)? {
        recheckAtLaunch(seat: .live)
    }

    static func recheckAtLaunch(seat: ResidueSeat) -> (any SignOutResidueSurfacing)? {
        painted(
            seat,
            signOutResidueRecheckAtLaunch(
                registry: seat.registry(), baseDir: seat.baseDir,
                storeContainerDir: seat.storeContainerDir, ownLock: nil,
                eraser: SignOutCredentialEraser(registry: seat.registry)))
    }

    public func retry() -> (any SignOutResidueSurfacing)? {
        // `ownLock: nil` — the view is only ever up signed out, and a sign-out
        // tears the session (and its instance lock) down before the wizard
        // mounts, so this window serves nothing the probe could meet.
        Self.painted(
            seat,
            residue.retry(
                registry: seat.registry(), baseDir: seat.baseDir,
                storeContainerDir: seat.storeContainerDir, ownLock: nil,
                eraser: SignOutCredentialEraser(registry: seat.registry)))
    }

    private static func painted(
        _ seat: ResidueSeat, _ residue: FfiSignOutResidue?
    ) -> (any SignOutResidueSurfacing)? {
        residue.map { SignOutResidueSurface(seat: seat, residue: $0) }
    }
}
