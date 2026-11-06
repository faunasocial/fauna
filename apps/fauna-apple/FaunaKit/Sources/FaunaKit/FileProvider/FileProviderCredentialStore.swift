import FaunaFFISwift
import Foundation
import Security

/// The least-privilege capability the **app-dead** File Provider extension needs to
/// serve one folder: a pre-derived owner `BackupKey` + a renewable nest
/// bearer + the public identity/device ids and nest URL — **never the identity
/// seed** (`on-demand-files-apple.md` § On-Demand Files → Apple File Provider binding, *the
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
    /// owner-only chunks — the only owner key the extension is handed).
    public let backupKey: Data
    /// The account's attested predecessors PAIRED with their retired owner keys
    /// (`FfiAccountRegistry.predecessorChain`) — key material, so it rides the
    /// same Keychain access group and protection as `backupKey`. The extension
    /// holds no registry, so the app provisions the chain: a row a retired
    /// identity signed then opens under that identity's own root
    /// (`writer-signed-change-records.md` ruling (8)(c)). `nil` — a store written
    /// before the chain was provisioned — still verifies such a row by the
    /// statement walk and opens it under no retired root.
    public let predecessorChain: FfiPredecessorChain?

    public init(
        nestURL: String, actorId: Data, deviceId: Data, deviceLabel: String, backupKey: Data,
        predecessorChain: FfiPredecessorChain? = nil
    ) {
        self.nestURL = nestURL
        self.actorId = actorId
        self.deviceId = deviceId
        self.deviceLabel = deviceLabel
        self.backupKey = backupKey
        self.predecessorChain = predecessorChain
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
        static let predecessorActorIds = "predecessor_actor_ids"
        static let predecessorKeys = "predecessor_keys"
        static let bearer = "bearer"
        static let writerSecret = "writer_secret"
        static let deviceAuthorization = "device_authorization"
        static let generationKeys = "generation_keys"
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
            backupKey: backupKey,
            predecessorChain: predecessorChain(
                actorIds: loadData(Account.predecessorActorIds),
                keys: loadData(Account.predecessorKeys))
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

    /// The chain as its two Keychain items: each list's 32-byte entries
    /// concatenated in chain order.
    static func predecessorChainItems(_ chain: FfiPredecessorChain) -> (actorIds: Data, keys: Data) {
        (chain.actorIds.reduce(Data(), +), chain.keys.reduce(Data(), +))
    }

    /// The chain the two Keychain items make together — both, or none: a
    /// missing item (a store written before the chain was provisioned, a crash
    /// between the two writes) or a pair that does not split into equally many
    /// 32-byte entries is no chain, never one identity paired with another's
    /// key. The Rust host re-checks the pairing before it offers any root.
    static func predecessorChain(actorIds: Data?, keys: Data?) -> FfiPredecessorChain? {
        guard let actorIds, let keys,
            actorIds.count == keys.count, actorIds.count % 32 == 0
        else { return nil }
        func entries(_ data: Data) -> [Data] {
            stride(from: 0, to: data.count, by: 32).map {
                Data(data[data.startIndex + $0..<data.startIndex + $0 + 32])
            }
        }
        return FfiPredecessorChain(actorIds: entries(actorIds), keys: entries(keys))
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
        // An empty chain (an identity that never succeeded) is stored as none:
        // the host carries no retired root either way, and the Keychain is
        // never asked to hold a zero-length item.
        if let chain = creds.predecessorChain, !chain.actorIds.isEmpty {
            let items = predecessorChainItems(chain)
            save(Account.predecessorActorIds, items.actorIds)
            save(Account.predecessorKeys, items.keys)
        } else {
            delete(Account.predecessorActorIds)
            delete(Account.predecessorKeys)
        }
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
            Account.deviceLabel, Account.backupKey, Account.predecessorActorIds,
            Account.predecessorKeys, Account.bearer,
            Account.writerSecret, Account.deviceAuthorization, Account.generationKeys,
        ] {
            delete(account)
        }
    }

    // MARK: Retained generation keys (the custody item)

    static func loadGenerationKeysItem() -> Data? { loadData(Account.generationKeys) }

    /// `nil` clears the item (the Keychain is never asked to hold zero bytes).
    static func saveGenerationKeysItem(_ data: Data?) {
        if let data { save(Account.generationKeys, data) } else { delete(Account.generationKeys) }
    }

    /// The app's mirror of the machine slot's retained keys
    /// (`machineRetainedGenerationKeys`) into the extension's custody item —
    /// at provisioning and on every slot write. Additive: it never drops an
    /// entry the extension recorded itself.
    public static func mirrorGenerationKeys(_ keys: [FfiRetainedGenerationKey]) {
        KeychainRetainedKeyCustody().record(keys)
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

/// The extension's **retained generation-key custody** (decision 1′ of
/// `on-demand-files-shared-sets.md`): the generations this device holds with no
/// wrap on the plane reaching it, which the extension cannot read from the
/// machine slot — the app mirrors them into the `generation_keys` app-group
/// Keychain item (`FileProviderCredentialStore.mirrorGenerationKeys`), and the
/// extension records beside them every generation it unwraps itself. One item
/// holds the whole map; a generation key has one value (the mint's commitment
/// checks it at every use), so two writers never conflict on an entry and
/// `put` of a held generation keeps the held key. Stateless: each call is a
/// fresh read. `read`/`write` are the tests' seam (`write(nil)` clears the item).
public final class KeychainRetainedKeyCustody: FfiRetainedKeyCustody, @unchecked Sendable {
    /// Serializes the read-merge-write across the app's mirror and the
    /// extension's puts within one process; across processes the last writer
    /// wins and a lost entry is re-put at the next slot write or unwrap.
    private static let lock = NSLock()

    private let read: @Sendable () -> Data?
    private let write: @Sendable (Data?) -> Void

    public convenience init() {
        self.init(
            read: { FileProviderCredentialStore.loadGenerationKeysItem() },
            write: { FileProviderCredentialStore.saveGenerationKeysItem($0) })
    }

    init(read: @escaping @Sendable () -> Data?, write: @escaping @Sendable (Data?) -> Void) {
        self.read = read
        self.write = write
    }

    public func get(generation: Data) -> Data? {
        Self.lock.withLock { Self.decode(read())[generation] }
    }

    public func put(generation: Data, key: Data) {
        record([FfiRetainedGenerationKey(generation: generation, key: key)])
    }

    public func remove(generation: Data) {
        Self.lock.withLock {
            var map = Self.decode(read())
            guard map.removeValue(forKey: generation) != nil else { return }
            write(map.isEmpty ? nil : Self.encode(map))
        }
    }

    /// Record every entry in one read-merge-write; a generation already held
    /// keeps its key.
    func record(_ entries: [FfiRetainedGenerationKey]) {
        Self.lock.withLock {
            var map = Self.decode(read())
            var changed = false
            for entry in entries where !entry.generation.isEmpty && !entry.key.isEmpty {
                if map[entry.generation] == nil {
                    map[entry.generation] = entry.key
                    changed = true
                }
            }
            if changed { write(Self.encode(map)) }
        }
    }

    /// The item's bytes: per entry `[generation length: u8][generation][key length: u8][key]`,
    /// sorted by generation so equal maps encode equal.
    static func encode(_ map: [Data: Data]) -> Data {
        var out = Data()
        for generation in map.keys.sorted(by: { $0.lexicographicallyPrecedes($1) }) {
            guard let key = map[generation], generation.count <= 255, key.count <= 255 else {
                continue
            }
            out.append(UInt8(generation.count))
            out.append(generation)
            out.append(UInt8(key.count))
            out.append(key)
        }
        return out
    }

    /// The map the bytes hold; a missing or malformed item is the empty map
    /// (custody unreadable — the host then keys through the plane's wraps alone).
    static func decode(_ data: Data?) -> [Data: Data] {
        guard let data else { return [:] }
        let bytes = [UInt8](data)
        var map: [Data: Data] = [:]
        var i = 0
        func field() -> Data? {
            guard i < bytes.count else { return nil }
            let n = Int(bytes[i])
            guard n > 0, i + 1 + n <= bytes.count else { return nil }
            defer { i += 1 + n }
            return Data(bytes[i + 1..<i + 1 + n])
        }
        while i < bytes.count {
            guard let generation = field(), let key = field() else { return [:] }
            map[generation] = key
        }
        return map
    }
}
