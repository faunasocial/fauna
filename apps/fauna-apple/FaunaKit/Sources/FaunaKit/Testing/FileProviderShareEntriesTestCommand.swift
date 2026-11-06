import FaunaFFISwift
import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// Shared handler for the `file_provider_share_entries` TestAgent command — the
/// pytest witness of Finder's (and the Files app's) Share eligibility
/// (`docs/goal/architecture/apps/macos.md` § Sync → *Finder's Share entry*,
/// link 1; `docs/features/share-links.md` outcome 29).
///
/// The File Provider appex itself is gated off e2e (its domains are
/// machine-global state, convention 10), so this builds the appex's OWN host —
/// the same `FfiFileProviderHost.appDead` constructor `makeFileProviderHost`
/// calls — in the app process, over a throwaway state and staging directory
/// and the session's own capability (the values the domain coordinator would
/// provision into the shared Keychain, held in memory instead). It then
/// projects the listed items exactly as the appex does: each child the host's
/// `enumerate` returns through `FileProviderShareEntry.forFile`, and the
/// domain's root container through `FileProviderShareEntry.forRoot` over the
/// set's actor-scoped identifier. No domain is registered, no shared Keychain
/// item is written, and nothing is re-derived: the entry is the provider
/// face's `share_link_eligible` as the extension hands it the OS.
///
/// The answer rides `state.machine_method_result` (the
/// `KeychainItemAttributesTestCommand` shape):
/// `{"root":"folder"|null,"items":{"<rel>":{"is_dir":false,"entry":"link"|null}},"signer":true}`
/// (`signer`: whether this machine's principal was there to key custody by),
/// with a requested rel the host does not list (yet) left out of `items`, so a
/// caller polls until each is present. Lives in FaunaKit so macOS + iOS share
/// ONE implementation.
public enum FileProviderShareEntriesTestCommand {
    public enum Outcome: Equatable {
        /// The projection's JSON, for the shell to stash in `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell surfaces the reason as a loud
        /// TestAgent failure and leaves the result slot empty.
        case refused(String)
    }

    @MainActor
    public static func apply(_ command: [String: Any], session: SessionState) async -> Outcome {
        // Convention 11's bad-payload clause: a missing set or path list must not
        // read as "nothing eligible".
        // The set's `folders.id`, as the share routes name it; the domain's ref is
        // that row of this nest (`FaunaShareRoute.localFolderId`'s inverse).
        guard let rowId = (command["folder_id"] as? NSNumber)?.int64Value else {
            return .refused("file_provider_share_entries: needs the set's numeric `folder_id`")
        }
        let folderId = "local:\(rowId)"
        guard let rels = command["paths"] as? [String], !rels.isEmpty else {
            return .refused("file_provider_share_entries: needs a non-empty `paths` list")
        }
        guard let secretHex = session.secretHex, let nestUrl = session.nodeUrl,
              let deviceIdHex = session.deviceId
        else {
            return .refused("file_provider_share_entries: no signed-in session to build the host from")
        }

        let scratch = FileManager.default.temporaryDirectory
            .appendingPathComponent("fauna-fp-share-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: scratch) }
        do {
            let secret = hex_to_data(secretHex)
            let actorId = try actorIdFromSecret(secret: secret)
            guard let identity = FileProviderDomainIdentity(
                actorIdHex: data_to_hex(actorId), folderId: folderId)
            else {
                return .refused("file_provider_share_entries: `\(folderId)` is not a set ref")
            }
            let stateDir = scratch.appendingPathComponent("state", isDirectory: true)
            let rootDir = scratch.appendingPathComponent("root", isDirectory: true)
            for dir in [stateDir, rootDir] {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            }
            let bearer = try await mintBearer(nestUrl: nestUrl, secret: secret)
            // Custody for a nonce'd set is keyed by the machine principal; a
            // machine not yet enrolled with `SyncWrite` has none, and its host
            // lists nothing — reported, so a poller can tell that from a slow pull.
            let signer = try? machineChangeSignerCarriage(actorId: actorId)
            // The retained generation keys the app mirrors into the appex's
            // custody item (`FileProviderSignerWatch.mirrorRetainedKeys`), held in
            // memory here and installed as `makeFileProviderHost` installs its own.
            let held = HeldKeys()
            let custody = KeychainRetainedKeyCustody(read: { held.value }, write: { held.value = $0 })
            custody.record(try machineRetainedGenerationKeys(actorId: actorId))
            try installCapabilityHostCustody(actorId: actorId, custody: custody)
            let host = try FfiFileProviderHost.appDead(
                nestUrl: nestUrl,
                actorId: actorId,
                deviceId: hex_to_data(deviceIdHex),
                deviceLabel: FaunaClient.deviceLabel,
                backupKey: try backupKeyDerive(secret: secret),
                bearer: HeldBearer(bearer.token),
                signer: HeldSigner(signer),
                stateDir: stateDir.path,
                folderId: identity.folderId,
                rootDir: rootDir.path,
                predecessorChain: FaunaAccounts.registry().predecessorChain(
                    actorId: data_to_hex(actorId))
            )
            _ = try await host.refresh()

            // The items Finder lists: each requested rel looked up among its
            // parent's children, the enumeration the appex serves.
            var items: [String: Any] = [:]
            let parents = Set(rels.map { FileProviderPathMapping.parentRel(forRel: $0) ?? "" })
            for parent in parents {
                for ffi in try await host.enumerate(parentRel: parent) where rels.contains(ffi.rel) {
                    items[ffi.rel] = [
                        "is_dir": ffi.isDir,
                        "entry": orNull(
                            FileProviderShareEntry.forFile(shareLinkEligible: ffi.shareLinkEligible)),
                    ] as [String: Any]
                }
            }
            let body: [String: Any] = [
                "root": orNull(FileProviderShareEntry.forRoot(domainIdentifier: identity.domainId)),
                "items": items,
                "signer": signer != nil,
            ]
            guard let data = try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]),
                  let json = String(data: data, encoding: .utf8)
            else {
                return .refused("file_provider_share_entries: could not encode the report")
            }
            return .report(json)
        } catch {
            return .refused("file_provider_share_entries: the host could not serve `\(folderId)`: \(error)")
        }
    }

    /// An absent entry as JSON `null`, never a missing key.
    private static func orNull(_ entry: String?) -> Any { entry.map { $0 as Any } ?? NSNull() }

    /// The custody item's bytes, in memory where the appex reads the shared Keychain.
    private final class HeldKeys: @unchecked Sendable {
        private let lock = NSLock()
        private var data: Data?
        var value: Data? {
            get { lock.withLock { data } }
            set { lock.withLock { data = newValue } }
        }
    }

    /// The session's bearer, held in memory where the appex reads the shared Keychain.
    private final class HeldBearer: FfiBearerProvider, @unchecked Sendable {
        private let token: String
        init(_ token: String) { self.token = token }
        func currentBearer() -> String { token }
    }

    /// This machine's change signer, held in memory (reads never need it, but
    /// custody for a nonce'd set is keyed by it).
    private final class HeldSigner: FfiChangeSignerProvider, @unchecked Sendable {
        private let signer: FfiChangeSignerCarriage?
        init(_ signer: FfiChangeSignerCarriage?) { self.signer = signer }
        func currentSigner() -> FfiChangeSignerCarriage? { signer }
    }
}

#endif
