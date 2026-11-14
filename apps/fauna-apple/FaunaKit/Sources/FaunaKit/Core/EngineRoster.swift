import FaunaFFISwift
import Foundation

#if canImport(FileProvider)
    import FileProvider
#endif

/// iOS's engine roster — every set an engine on this device is bound to — lent
/// to the launch folder pass so the projection walk's take-over claims an
/// adoption marker only for a set no engine here holds (`succession-cut.md`
/// ruling (11)(l), *one consumer per marker*; the Rust side is `fauna-ffi`'s
/// `engine_roster`, the android sibling `EngineRoster.kt`). iOS binds an engine
/// to a set two ways:
///
/// - the File Provider extension, one domain per set, its identifier the set's
///   actor-scoped identity (`FileProviderDomainIdentity`; shared Rust ignores
///   another account's);
/// - the in-app photo ingest (`photoLibraryBoundRef`, the device-local binding
///   in the account's sync state dir — a paused backup still counts bound).
///
/// A registered identifier that scopes no set to an account (a bare ref, a set
/// name, a legacy spelling) is skipped: the extension never serves it and the
/// reconcile removes it, and passing it on would make shared Rust fail the
/// whole roster on every pass. A domain list that cannot be read throws — an
/// unread binding may be an engine's, so the pass then claims nothing new.
///
/// macOS installs none: its sync agent hosts every engine and its roster is
/// read in shared Rust (`agent_bound_sets`); an app-lent roster would replace
/// that reading. Called by Rust off the UI thread, so blocking here is fine.
///
/// `@unchecked Sendable` like FaunaKit's other UniFFI callback conformances:
/// the stored closures are set once at construction and never mutated.
public final class EngineRoster: FfiEngineRoster, @unchecked Sendable {
    private let readDomainIds: () throws -> [String]
    private let readPhotoBinding: () -> String?

    /// A roster over injected sources — the seam the unit pins drive.
    init(
        readDomainIds: @escaping () throws -> [String],
        readPhotoBinding: @escaping () -> String?
    ) {
        self.readDomainIds = readDomainIds
        self.readPhotoBinding = readPhotoBinding
    }

    // MARK: - FfiEngineRoster

    public func boundFolderRefs() throws -> [String] {
        let domains: [String]
        do {
            domains = try readDomainIds()
        } catch let error as FfiError {
            throw error
        } catch {
            throw FfiError.General(
                msg: "the File Provider domains are unreadable: \(error.localizedDescription)")
        }
        let scoped = domains.filter { FileProviderDomainIdentity.parse($0) != nil }
        return scoped + [readPhotoBinding()].compactMap { $0 }
    }

    /// The File Provider extension point — the one plug-in that hosts a
    /// domain's engine.
    static let fileProviderExtensionPoint = "com.apple.fileprovider-nonui"

    /// Whether any of the embedded plug-ins' Info dictionaries declares the
    /// File Provider extension point.
    static func embedsFileProvider(pluginInfos: [[String: Any]]) -> Bool {
        pluginInfos.contains { info in
            (info["NSExtension"] as? [String: Any])?["NSExtensionPointIdentifier"] as? String
                == fileProviderExtensionPoint
        }
    }

    /// The domains an engine of this bundle can be bound to: none when the
    /// bundle embeds no File Provider extension — no engine can be hosted, and
    /// the OS's domain list fails in such a bundle, which would read as an
    /// unread roster and stop every take-over on the device (`succession-cut.md`
    /// ruling (11)(l)) — else the OS's list, a failing read still thrown.
    static func domainIds(
        embedsFileProvider: Bool, read: () throws -> [String]
    ) throws -> [String] {
        guard embedsFileProvider else { return [] }
        return try read()
    }

    #if os(iOS)
        /// The live roster: the registered File Provider domains (none in a
        /// bundle that embeds no extension), and the photo binding of the
        /// active account's sync state dir.
        public static func live() -> EngineRoster {
            let embeds = mainBundleEmbedsFileProvider()
            return EngineRoster(
                readDomainIds: {
                    try domainIds(embedsFileProvider: embeds, read: registeredDomainIds)
                },
                readPhotoBinding: { photoLibraryBoundRef(stateDir: FaunaClient.syncStateDir) })
        }

        /// The main bundle's embedded plug-ins, read off their Info.plist files.
        private static func mainBundleEmbedsFileProvider() -> Bool {
            guard let dir = Bundle.main.builtInPlugInsURL,
                let appexes = try? FileManager.default.contentsOfDirectory(
                    at: dir, includingPropertiesForKeys: nil)
            else { return false }
            let infos = appexes.filter { $0.pathExtension == "appex" }.compactMap {
                Bundle(url: $0)?.infoDictionary
            }
            return embedsFileProvider(pluginInfos: infos)
        }

        /// Lend the live roster to the launch pass; called once at app start,
        /// before any conversations session is built.
        public static func install() {
            installEngineRoster(roster: live())
        }

        /// `NSFileProviderManager.getDomains`, blocking: the UniFFI call is
        /// synchronous and Rust makes it off its runtime's workers.
        private static func registeredDomainIds() throws -> [String] {
            let done = DispatchSemaphore(value: 0)
            var result: Result<[String], Error> = .success([])
            NSFileProviderManager.getDomainsWithCompletionHandler { domains, error in
                if let error {
                    result = .failure(error)
                } else {
                    result = .success(domains.map(\.identifier.rawValue))
                }
                done.signal()
            }
            done.wait()
            return try result.get()
        }
    #endif
}
