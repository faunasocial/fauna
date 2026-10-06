import FaunaFFISwift
import Foundation
import Security

/// The least-privilege capability the **app-dead** File Provider extension needs to
/// serve one folder: a pre-derived owner `BackupKey` + a renewable nest
/// bearer + the public identity/device ids and nest URL — **never the identity
/// seed** (`file-sync.md` § On-Demand Files → Apple File Provider binding, *the
/// extension hosts the engines*). The app writes these into the shared app-group
/// Keychain at first domain creation; the extension reads them (and re-reads the
/// bearer fresh on every use, so a rotated bearer is picked up without a rebuild).
///
/// Beside them — not in this struct, because it changes on its own schedule —
/// the app provisions the machine principal's **change signer**
/// (`FileProviderCredentialStore.provisionSigner`), which the extension re-reads
/// at every write the same way (`mls-group-key-material.md` § M2 → *Writer-signed
/// change records* (1), *The capability host*).
public struct FileProviderCredentials: Sendable {
    public let nestURL: String
    /// 32-byte public actor id.
    public let actorId: Data
    /// 32-byte device id.
    public let deviceId: Data
    public let deviceLabel: String
    /// 32-byte owner `BackupKey` (the content-seal root; opens the owner's
    /// owner-only chunks — the only key the extension is handed).
    public let backupKey: Data

    public init(nestURL: String, actorId: Data, deviceId: Data, deviceLabel: String, backupKey: Data) {
        self.nestURL = nestURL
        self.actorId = actorId
        self.deviceId = deviceId
        self.deviceLabel = deviceLabel
        self.backupKey = backupKey
    }
}

/// Shared app-group Keychain accessor for the File Provider capability. Both the
/// **app** (writer — `provision` / `revoke`) and the **app-dead extension** (reader —
/// `load` / `currentBearer`) link this through FaunaKit, so the read/write shapes can
/// never drift. Rides the same `SharedKeychainItem` primitive `PushManager` uses for
/// its shared-Keychain items — keyed by `(service, account)`; `AfterFirstUnlock` so
/// `fileproviderd` can read the token while the app is dead but the machine has been
/// unlocked once.
public enum FileProviderCredentialStore {
    /// The app group whose Keychain both the app and the extension share.
    public static let accessGroup = AppleIdentifiers.appGroup
    private static let service = AppleIdentifiers.KeychainService.fileProvider

    private enum Account {
        static let nestURL = "nest_url"
        static let actorId = "actor_id"
        static let deviceId = "device_id"
        static let deviceLabel = "device_label"
        static let backupKey = "backup_key"
        static let bearer = "bearer"
        static let writerSecret = "writer_secret"
        static let deviceAuthorization = "device_authorization"
    }

    // MARK: App-group container

    /// The shared app-group container root, or `nil` when it's unreachable
    /// (missing `application-groups` entitlement — a packaging bug). The one
    /// door onto `FileManager.containerURL(forSecurityApplicationGroupIdentifier:)`
    /// for this access group — `SyncStateDir`, `NestTrust` and `FileProviderHost`
    /// each hand-rolled the identical lookup before consolidating here;
    /// each caller still composes its own sub-path (or throwing wrapper) on top.
    public static func containerURL() -> URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: accessGroup)
    }

    // MARK: Reader (extension)

    /// The full capability, or `nil` if the app has not provisioned it yet (the host
    /// then fails closed — `makeFileProviderHost` throws `.notProvisioned`).
    public static func load() -> FileProviderCredentials? {
        guard
            let nestURL = loadString(Account.nestURL),
            let actorId = loadData(Account.actorId),
            let deviceId = loadData(Account.deviceId),
            let deviceLabel = loadString(Account.deviceLabel),
            let backupKey = loadData(Account.backupKey)
        else { return nil }
        return FileProviderCredentials(
            nestURL: nestURL,
            actorId: actorId,
            deviceId: deviceId,
            deviceLabel: deviceLabel,
            backupKey: backupKey
        )
    }

    /// The bearer the app most recently wrote; `""` when none is provisioned (the
    /// Rust `ForeignBearer` treats an empty token as "not provisioned yet" and fails
    /// the request closed rather than presenting a blank `Authorization`).
    public static func currentBearer() -> String {
        loadString(Account.bearer) ?? ""
    }

    /// The machine principal's change signer the app most recently wrote, or
    /// `nil` when none is provisioned — the Rust host then holds every write
    /// with the reason rather than recording it unsigned.
    public static func currentSigner() -> FfiChangeSignerCarriage? {
        signerCarriage(
            writerSecret: loadData(Account.writerSecret),
            deviceAuthorization: loadData(Account.deviceAuthorization))
    }

    /// The signer the two Keychain items make together — both, or none: a
    /// half-written pair (a crash between the two writes) is no signer, never a
    /// key paired with some other key's grant. The Rust host re-checks the pair
    /// through the delegation chain before it signs anything with it.
    static func signerCarriage(writerSecret: Data?, deviceAuthorization: Data?)
        -> FfiChangeSignerCarriage?
    {
        guard let writerSecret, !writerSecret.isEmpty,
            let deviceAuthorization, !deviceAuthorization.isEmpty
        else { return nil }
        return FfiChangeSignerCarriage(
            writerSecret: writerSecret, deviceAuthorization: deviceAuthorization)
    }

    // MARK: Writer (app)

    /// Write the capability + bearer into the shared Keychain (app side, at first
    /// domain creation). Idempotent — each account is delete-then-add.
    /// `signer` is the machine principal's change signer
    /// (`machineChangeSignerCarriage`) — `nil` before this machine is enrolled
    /// with `SyncWrite`, which clears any stale one.
    public static func provision(
        _ creds: FileProviderCredentials, bearer: String, signer: FfiChangeSignerCarriage?
    ) {
        save(Account.nestURL, Data(creds.nestURL.utf8))
        save(Account.actorId, creds.actorId)
        save(Account.deviceId, creds.deviceId)
        save(Account.deviceLabel, Data(creds.deviceLabel.utf8))
        save(Account.backupKey, creds.backupKey)
        save(Account.bearer, Data(bearer.utf8))
        provisionSigner(signer)
    }

    /// Write (or, with `nil`, clear) just the change signer — at provisioning,
    /// and again whenever this machine's principal changes (a first enrollment,
    /// the `SyncWrite` re-certification, a re-minted writer key), so the
    /// extension's next write signs with the current one.
    public static func provisionSigner(_ signer: FfiChangeSignerCarriage?) {
        if let signer {
            save(Account.writerSecret, signer.writerSecret)
            save(Account.deviceAuthorization, signer.deviceAuthorization)
        } else {
            delete(Account.writerSecret)
            delete(Account.deviceAuthorization)
        }
    }

    /// Rotate just the bearer (the app's session-scoped refresh loop; the host
    /// re-reads it fresh on the next request).
    public static func refreshBearer(_ bearer: String) {
        save(Account.bearer, Data(bearer.utf8))
    }

    /// Delete the whole capability on sign-out — the extension then fails closed.
    public static func revoke() {
        for account in [
            Account.nestURL, Account.actorId, Account.deviceId,
            Account.deviceLabel, Account.backupKey, Account.bearer,
            Account.writerSecret, Account.deviceAuthorization,
        ] {
            delete(account)
        }
    }

    // MARK: Keychain primitives

    // `useDataProtectionKeychain: true` is load-bearing on macOS, not decoration:
    // an app-group access group (`kSecAttrAccessGroup = group.social.fauna.shared`)
    // only exists in the iOS-style **data-protection** keychain. Without this flag
    // a macOS process defaults to the legacy file-based keychain, where app-group
    // sharing does not work — so the non-sandboxed app's `provision` write and the
    // sandboxed extension's `load` read would land in different keychains and the
    // extension would fail closed (`.notProvisioned`) even though the app had
    // provisioned. On iOS every keychain is already the data-protection one, so
    // the flag is a harmless no-op there. Present on every query (via
    // `SharedKeychainItem`) so writer and reader always rendezvous.
    private static func save(_ account: String, _ data: Data) {
        SharedKeychainItem.save(
            service: service, account: account, accessGroup: accessGroup, data: data,
            useDataProtectionKeychain: true)
    }

    private static func loadData(_ account: String) -> Data? {
        SharedKeychainItem.load(
            service: service, account: account, accessGroup: accessGroup,
            useDataProtectionKeychain: true)
    }

    private static func loadString(_ account: String) -> String? {
        loadData(account).flatMap { String(data: $0, encoding: .utf8) }
    }

    private static func delete(_ account: String) {
        SharedKeychainItem.delete(
            service: service, account: account, accessGroup: accessGroup,
            useDataProtectionKeychain: true)
    }

    // MARK: Diagnostics

    /// Probe one write→read round-trip against the shared app-group access group
    /// and return the raw `(SecItemAdd, SecItemCopyMatching)` OSStatus pair, so a
    /// caller (the tier_3 provisioning host) can surface *why* the rendezvous
    /// failed instead of a bare `nil` — the writer path swallows the `SecItemAdd`
    /// status, and data-protection-keychain / app-group-access-group failures
    /// (`errSecMissingEntitlement`, `errSecParam`, …) otherwise vanish. Uses a
    /// throwaway account it deletes on both ends.
    public static func diagnoseAccessGroupRoundTrip() -> (add: OSStatus, read: OSStatus) {
        SharedKeychainItem.probeRoundTrip(
            service: service, probeAccount: "__diagnostic_probe__", accessGroup: accessGroup,
            useDataProtectionKeychain: true)
    }
}

/// Bridges the shared-Keychain bearer to the Rust `FfiBearerProvider` the app-dead
/// host reads on every request. Stateless — each `currentBearer()` is a fresh
/// Keychain read, so a bearer the app rotated in is picked up without rebuilding the
/// host.
public final class KeychainBearerProvider: FfiBearerProvider, @unchecked Sendable {
    public init() {}

    public func currentBearer() -> String {
        FileProviderCredentialStore.currentBearer()
    }
}

/// Bridges the shared-Keychain change signer to the Rust `FfiChangeSignerProvider`
/// the app-dead host reads at every write — the twin of `KeychainBearerProvider`.
/// Stateless: each `currentSigner()` is a fresh read, so a principal the app
/// re-minted is picked up without rebuilding the host. `read` is the tests' seam.
public final class KeychainChangeSignerProvider: FfiChangeSignerProvider, @unchecked Sendable {
    private let read: @Sendable () -> FfiChangeSignerCarriage?

    public convenience init() {
        self.init(read: { FileProviderCredentialStore.currentSigner() })
    }

    init(read: @escaping @Sendable () -> FfiChangeSignerCarriage?) {
        self.read = read
    }

    public func currentSigner() -> FfiChangeSignerCarriage? {
        read()
    }
}
