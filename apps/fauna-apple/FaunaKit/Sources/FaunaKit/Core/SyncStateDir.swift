import FaunaFFISwift
import Foundation

/// The per-set sync state home, per **consent domain** (`on-demand-files.md`
/// § On-Demand Files → Apple File Provider binding, *state unification* —
/// re-ratified per consent domain 2026-08-25; Multi-account × File Provider
/// consequence 3 for the per-actor scoping): `<domain root>/sync/<actor-id-hex>/`
/// holds one account's `fsid-<ref>.db`s and `device.db`, and **each set's engine state lives in its HOST's domain root**.
///
/// - **iOS has one domain.** The sandboxed app and its File Provider extension
///   share the app-group container, so ``Domain/container`` is the app's own
///   root too.
/// - **macOS has two.** The **container** (`<app-group>/sync`) is the sandboxed
///   extension's root — and *only* its root: macOS 15+ puts
///   `~/Library/Group Containers` under `kTCCServiceSystemPolicyAppData`, and a
///   launchd-spawned background process is prompted there on every instance
///   with no user decision ever binding the next one (`installers/macos.md`
///   § Identifier domain, record item 5). The **user domain**
///   (`~/Library/Application Support/Fauna/sync` — shared Rust's
///   `platform_state_base()`, the root the external `fauna-sync-agent`,
///   fauna-tui and the account store already resolve) is everyone else's, the
///   app's own one-shot host included. A process never opens a root outside its
///   own domain; the one process spanning both is the unsandboxed **app**, the
///   container's *steward* — it writes `domain-owners.json` and the pin replica
///   there and reads the FP-bound sets' `file_states` there, while its own state
///   and the agent-hosted sets' reads live in the user domain
///   (`SyncStatesStore`'s two-root fold routes each set to its host's root).
///
/// The derivation is shared Rust either way (`account_state_dir` — the ONE
/// `<base>/<actor-id-hex>/` rule every account-scoped store takes; the macOS
/// user-domain base comes from `platform_state_base()` and is never re-spelled
/// here), so the app, the extension and the agent cannot diverge on a root they
/// share — and two accounts can never share one state dir (the cross-account
/// leak per-actor scoping closes).
///
/// One dir, one writer per set: the one-local-presence rule
/// (`FileProviderCoordinator.reconcile`) guarantees a set's engine runs in
/// exactly one process, and `SyncDb` (WAL + busy-timeout) makes the cross-process
/// *reads* — Media-page badges over `file_states` — safe.
///
/// Nothing is adopted: the pre-scoping flat layouts (the flat domain bases, the
/// M2 `<group>/FileProvider/state`, the pre-M4 in-app engine dir) and their
/// first-adopter copy were removed by the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2, the fourth ratified exception — no
/// pre-scoping install exists).
///
/// `FileManager`'s user-domain Application Support directory — the one door
/// ``SyncStateDir/appSupportSyncDir``, `NestTrust.perProcessDir` and
/// `AccountStateDir.base` all resolve through, so those three Swift-spelled
/// bases can never silently diverge on how the root itself is found. Distinct
/// from `platformStateBase()` (shared Rust's resolution of the user-domain sync
/// root).
func applicationSupportDirectory() -> URL {
    FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
}

public enum SyncStateDir {
    /// A consent domain's sync root.
    public enum Domain: Sendable, Equatable {
        /// `<app-group>/sync` — the sandboxed File Provider extension's root
        /// (both OSes), and on iOS the app's own root as well.
        case container
        /// macOS only: `~/Library/Application Support/Fauna/sync` — the platform
        /// state base every non-sandboxed Fauna process shares consent-free.
        case userDomain
    }

    /// The domain the **app's own** hosted state lives in: the user domain on
    /// macOS, the container on iOS (where it is the only one).
    public static var appDomain: Domain {
        #if os(macOS)
            return .userDomain
        #else
            return .container
        #endif
    }

    /// Whether an e2e launch must keep the unscoped ``appSupportSyncDir`` instead
    /// of the app's scoped domain root: only where that root is the app-group
    /// container (iOS) — machine-global state shared by every install on the
    /// device, which a test launch must never touch (testing.md § Cross-app e2e
    /// conventions point 10). On macOS the app's root is the user domain, which
    /// the driver relocates per launch (`HOME` + `CFFIXED_USER_HOME`), so e2e
    /// runs the production derivation — scoped, and shared with the launch's own
    /// child agent — exactly as linux does.
    public static var e2eKeepsFlatLayout: Bool {
        FaunaE2E.isActive && appDomain == .container
    }

    /// The active account's scoped dir in `domain`, created. `nil` when the
    /// domain's root is unreachable — for the container, a missing
    /// `application-groups` entitlement (a packaging bug); for the user domain,
    /// only on iOS, where it does not exist — or when the hex is malformed;
    /// callers fall back to ``appSupportSyncDir``.
    public static func resolve(actorIdHex: String, in domain: Domain) -> URL? {
        guard let base = base(of: domain) else { return nil }
        guard let dir = try? accountStateDir(baseDir: base.path, actorIdHex: actorIdHex) else {
            return nil
        }
        let url = URL(fileURLWithPath: dir, isDirectory: true)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    /// A domain's base — the actor subdirs live under it. No side effect
    /// (test/diagnostic seam).
    public static func base(of domain: Domain) -> URL? {
        switch domain {
        case .container:
            return containerSyncDir()
        case .userDomain:
            #if os(macOS)
                return userDomainSyncDir
            #else
                return nil
            #endif
        }
    }

    /// `<app-group>/sync` — the container domain's base. `nil` when the
    /// app-group container is unreachable (missing `application-groups`
    /// entitlement — a packaging bug).
    public static func containerSyncDir() -> URL? {
        FileProviderCredentialStore.containerURL()?.appendingPathComponent("sync", isDirectory: true)
    }

    #if os(macOS)
        /// `~/Library/Application Support/Fauna/sync` — the user domain's base,
        /// resolved by shared Rust (`platform_state_base()`) so this app can
        /// never spell the root differently from the agent whose `fsid-<ref>.db`s
        /// it reads there. Under an e2e launch the same call resolves inside the
        /// launch's relocated `HOME`.
        public static var userDomainSyncDir: URL {
            URL(fileURLWithPath: platformStateBase(), isDirectory: true)
        }

        /// `~/Library/Application Support/Fauna` — the user-domain home the
        /// app's other install-scoped files hang off (the deployment-seed
        /// `config-replica`, the pin store's `trust/`), one level above
        /// ``userDomainSyncDir``.
        public static var userDomainHome: URL {
            userDomainSyncDir.deletingLastPathComponent()
        }
    #endif

    /// `FileManager`'s `Application Support/Fauna/sync` — the unscoped state dir
    /// an iOS e2e launch keeps (``e2eKeepsFlatLayout``: inside the app's own
    /// sandbox, never the machine-global container), and the fallback when no
    /// scoped root resolves (no session has published its actor yet, or the
    /// domain root is unreachable). On macOS it names the same directory as
    /// ``userDomainSyncDir``.
    public static var appSupportSyncDir: URL {
        applicationSupportDirectory().appendingPathComponent("Fauna/sync")
    }
}

/// Minimal mutex-guarded box (avoids pulling a concurrency dependency into this
/// leaf).
final class Locked<Value>: @unchecked Sendable {
    private var value: Value
    private let lock = NSLock()
    init(_ value: Value) { self.value = value }
    func withLock<R>(_ body: (inout Value) -> R) -> R {
        lock.lock()
        defer { lock.unlock() }
        return body(&value)
    }
}
