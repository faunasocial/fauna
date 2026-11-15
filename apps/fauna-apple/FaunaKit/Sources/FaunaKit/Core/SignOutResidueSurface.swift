import FaunaFFISwift
import Foundation

/// The state of `identity_choice`'s `sign-out-residue` view while a sign-out's
/// residue still owes work: its one line (`account-scoping.md` § Erasure
/// follows scope → *the residue surface*). Remove Again is not the residue's
/// own act any more: it re-runs the one sign-out routine
/// (`OnboardingVM.retrySignOutResidue` → `SignOutResidueSurface.finish`). A
/// protocol so `OnboardingVM` is driven by a fake in unit tests, and so nothing
/// a view binds is a UniFFI type. Twin of windows' `ISignOutResidueSurface`.
public protocol SignOutResidueSurfacing: AnyObject, Sendable {
    /// `sign-out-residue-message`, already localized — the shared `Rendered`
    /// copy, or the finish's own refusal while another window serves one of
    /// the accounts it would reach.
    var line: String { get }
}

/// What one run of the sign-out routine answers the seat
/// (`SignOutResidueSurface.finish`, the FFI `FfiSignOutFinish`).
public struct SignOutFinishOutcome: Sendable {
    /// The `sign-out-residue` view to paint — `nil` when the run left nothing.
    public let residue: (any SignOutResidueSurfacing)?
    /// The launch must not route over the registry: another instance serves an
    /// account the run would reach and the sign-out record still owes its wipe
    /// (`account-scoping.md` § Erasure follows scope → *The sign-out record is
    /// on all 7 apps*, decision 4). The wizard lands on `identity_choice`
    /// painting `residue` (the refusal line), whose Remove Again re-runs the
    /// routine.
    public let failsClosed: Bool

    public init(residue: (any SignOutResidueSurfacing)?, failsClosed: Bool) {
        self.residue = residue
        self.failsClosed = failsClosed
    }

    /// A run that found nothing to do.
    public static let nothing = SignOutFinishOutcome(residue: nil, failsClosed: false)
}

/// Where this seat's sign-out erases, and how it reaches its credential
/// registry. `baseDir` is the install base (`AccountStateDir.base`), home of
/// both the sign-out record and the residue record, and where a sibling window
/// holds its instance lock; `storeContainerDir` is what the seat passes to
/// `signOutBlocked` and the account runtime (`nil` on macOS, the iOS app-group
/// container on iOS).
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

/// Apple's own credential acts inside the sign-out routine — handed to shared
/// Rust as its `FfiSignOutHost`.
///
/// - `wipeOwnStore` — nothing more to do: shared Rust has already run the
///   registry's own wipe (`wipe_own`: the index and every per-actor slot of
///   this app's service), and apple has no wholesale platform reset behind it
///   that a sign-out may take — `KeychainStore.deleteAll()` is the e2e
///   factory reset's, and would take install-scoped rows a sign-out keeps.
///   The read-back shared Rust runs after this is the witness.
/// - `eraseCredentials` — the residue retry's credential erase: the registry's
///   `clearAll()`, which reads back what it deleted. Shared Rust reads the
///   RECORDED keys back on top of this answer.
final class SignOutHost: FfiSignOutHost, @unchecked Sendable {
    private let registry: @Sendable () -> FfiAccountRegistry

    init(registry: @escaping @Sendable () -> FfiAccountRegistry) {
        self.registry = registry
    }

    func wipeOwnStore() -> Bool { true }

    func eraseCredentials() -> FfiCredentialSweep {
        registry().clearAll()
    }
}

/// The apple seat of **the sign-out record** and the residue surface, over the
/// `fauna-ffi` face (`libs/fauna-ffi/src/sign_out_residue.rs`) android and
/// windows use (`account-scoping.md` § Erasure follows scope → *The sign-out
/// record is on all 7 apps*). The record, the finish and every word of the
/// line are shared Rust; this only carries the seat's bases across and
/// localizes the line. One FaunaKit seat serves macOS and iOS. Twin of
/// android's `AccountStores.recordSignOut` / `finishSignOut`.
///
/// `@unchecked Sendable`: its one stored property is immutable.
public final class SignOutResidueSurface: SignOutResidueSurfacing, @unchecked Sendable {
    public let line: String

    private init(residue: FfiSignOutResidue) {
        self.line = renderLocalizedText(residue.line())
    }

    /// **The sign-out's decision**, written before anything else — after the
    /// serving question passed, before the runtime's stop is awaited
    /// (decision 1): `sign-out-record.json` under the install base names every
    /// account the registry lists plus `servedActor` and owes the credential
    /// wipe, so a process killed anywhere after this is finished by the next
    /// launch. Returns the accounts recorded, which the gesture hands to its
    /// own `finish` as `written`. Synchronous and quick (one small file write):
    /// it runs on the gesture's own turn, before its first await.
    public static func recordSignOut(servedActor: String?) -> [String] {
        recordSignOut(seat: .live, servedActor: servedActor)
    }

    static func recordSignOut(seat: ResidueSeat, servedActor: String?) -> [String] {
        signOutRecordBegin(
            registry: seat.registry(), baseDir: seat.baseDir, servedActor: servedActor)
    }

    /// **The one routine** the sign-out's tail, the launch and Remove Again
    /// run — shared Rust's finish over this seat's install base and store
    /// container: the serving question, the sync agent dismissed
    /// (for every `caller` but `.gesture`, whose own stop already did it), the wipe, each
    /// recorded account's scopes and auxiliary slots, the residue re-sweep, the
    /// residue record written before the sign-out record clears (decisions
    /// 2–6). `ownLock` is a lock this process still holds (the macOS window's
    /// instance lock during its own sign-out), put down around the run;
    /// `written` is the gesture's `recordSignOut` answer, `nil` elsewhere.
    ///
    /// `caller` is which of the three runs this is: while the registry names
    /// somebody only `.removeAgain` re-sweeps the residue record, its recorded
    /// paths only, the credential half held as recorded (decision 3).
    ///
    /// Blocking file I/O and, but for the gesture, up to the agent budget per
    /// recorded account: call it off the main actor.
    public static func finish(
        ownLock: FfiAccountInstanceLock? = nil, caller: FfiFinishCaller, written: [String]?
    ) -> SignOutFinishOutcome {
        finish(seat: .live, ownLock: ownLock, caller: caller, written: written)
    }

    static func finish(
        seat: ResidueSeat, ownLock: FfiAccountInstanceLock? = nil, caller: FfiFinishCaller,
        written: [String]?
    ) -> SignOutFinishOutcome {
        let outcome = signOutFinish(
            registry: seat.registry(), baseDirs: [seat.baseDir],
            storeContainerDir: seat.storeContainerDir, ownLock: ownLock,
            host: SignOutHost(registry: seat.registry), caller: caller,
            written: written)
        if outcome.refused {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message:
                    "[sign-out] finish REFUSED — another instance serves an account it "
                    + "reaches (fails closed: \(outcome.failsClosed))")
        } else if outcome.residue != nil {
            logMessage(
                level: .warn, target: "fauna.accounts",
                message: "[sign-out] finish left this user's data on the device")
        }
        return SignOutFinishOutcome(
            residue: outcome.residue.map { SignOutResidueSurface(residue: $0) },
            failsClosed: outcome.failsClosed)
    }
}
