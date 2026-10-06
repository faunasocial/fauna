import Foundation

/// TLS-trust startup wiring (security.md § Transport trust). Installs the
/// disk-backed nest-identity pin store so learned TOFU pins (self-signed / LAN
/// nests) survive app restarts instead of being re-learned every launch — the
/// SSH-`known_hosts` analogue. Mirrors linux's `client::install_disk_pin_store`;
/// shared by the macOS + iOS apps and the File Provider extension.
///
/// The store is **install-scoped, not process-scoped** (security.md § Transport
/// trust, pin custody): the app and the File Provider extension are separate
/// processes with separate per-process containers, so a per-process store left
/// the extension with an empty `known_hosts` — every connect to a TOFU-rooted
/// nest failed TLS forever. The app is the **sole writer** (first-trust is a
/// user decision, made where a user is present); the extension installs a
/// read-only view and never mints a pin. WHERE the store lives is per-OS,
/// because the two OSes have different consent domains:
///
/// - **iOS** — the app is sandboxed, so its consent domain IS the app-group
///   container: the store is `<group>/trust/`, shared with the extension
///   directly.
/// - **macOS (2026-08-25)** — the primary is the **user-domain** home shared
///   Rust resolves (`install_scoped_trust_home()` →
///   `~/Library/Application Support/Fauna/trust`), which the launchd sync
///   agent and fauna-tui read consent-free; the app-group container is
///   `kTCCServiceSystemPolicyAppData`-protected on macOS 15+ and a
///   launchd-spawned agent is prompted there on every instance
///   (`installers/macos.md` § Identifier domain, item 5). The sandboxed
///   extension cannot reach the user domain, so the writer keeps a **read
///   replica** at `<group>/trust/` — refreshed on every persist by the Rust
///   store itself (`install_nest_identity_pin_store_with_mirror`) — and the
///   extension reads that, exactly as before.
public enum NestTrust {
    /// The **interactive app's** install (writer). Call **once** at app launch,
    /// before the first authenticated nest connect. Process global,
    /// last-write-wins. (The adoption of pins recorded under earlier homes was
    /// retired by the compat-remnant sweep — `version-compatibility.md`
    /// § Dimension 2, program 4.)
    public static func installPinStore() {
        // E2E launches keep the per-process path: the app-group container is
        // machine-global state a test launch must never touch (testing.md
        // § point 10) — on macOS `perProcessDir` is the user-domain
        // home, i.e. the production PRIMARY inside the launch's relocated
        // `HOME`, minus the container replica no e2e extension reads.
        guard !FaunaE2E.isActive, let shared = sharedTrustDir() else {
            install_nest_identity_pin_store(perProcessDir.path)
            return
        }
        #if os(macOS)
            // Primary in the user domain (the ONE derivation the agent + tui
            // read), replica in the container for the extension.
            let primary = installScopedTrustHome()
            installNestIdentityPinStoreWithMirror(dataDir: primary, replicaDir: shared.path)
        #else
            install_nest_identity_pin_store(shared.path)
        #endif
    }

    /// The **File Provider extension's** install (read-only pin consumer). The
    /// extension reads the pins the app minted; with no pin for a TOFU-rooted
    /// nest its connect fails (`PinRequired`) and the engine's retry picks the
    /// pin up once the app has trusted the nest — it never silently trusts
    /// whatever it reached. The store re-reads the file per lookup, so the
    /// app's onboarding pin write lands within one retry, no relaunch.
    public static func installPinStoreReadOnly() {
        // No shared container (missing `application-groups` entitlement — a
        // packaging bug) leaves a read-only view of the extension's own empty
        // container: fail closed, never fail open.
        let dir = sharedTrustDir() ?? perProcessDir
        installNestIdentityPinStoreReadOnly(dataDir: dir.path)
    }

    /// `<app-group>/trust` — the install-scoped trust home on iOS, and on macOS
    /// the extension's read replica of the user-domain primary (see the type
    /// doc). `nil` when the app-group container is unreachable (missing
    /// `application-groups` entitlement — a packaging bug); callers fall back
    /// to `perProcessDir`.
    static func sharedTrustDir() -> URL? {
        FileProviderCredentialStore.containerURL()?.appendingPathComponent("trust", isDirectory: true)
    }

    /// The per-process store dir (application-support `Fauna/`, the dir the MLS
    /// store also uses) — the e2e-launch store home, and the fallback when the
    /// app-group container is unreachable.
    static var perProcessDir: URL {
        applicationSupportDirectory().appendingPathComponent("Fauna")
    }
}
