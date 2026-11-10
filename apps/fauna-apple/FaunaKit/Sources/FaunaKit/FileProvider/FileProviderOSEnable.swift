import FaunaFFISwift
import Foundation
import Observation
import SwiftUI

#if canImport(FileProvider)
    import FileProvider

    /// The rows whose set the OS holds disabled until the person enables Fauna
    /// once (`on-demand-files-apple.md` § Apple File Provider binding, *The
    /// one-time OS enable*): the scoped identifiers the shared rule answered at
    /// the last read. Derived per read, never persisted.
    @MainActor @Observable
    final class FileProviderOSEnableNotices {
        static let shared = FileProviderOSEnableNotices()

        private(set) var identifiers: Set<String> = []

        func publish(_ identifiers: Set<String>) {
            guard identifiers != self.identifiers else { return }
            self.identifiers = identifiers
            logMessage(
                level: .info, target: "fauna.fileprovider",
                message:
                    "[fp] OS enable notice on \(identifiers.count) row(s): \(identifiers.sorted().joined(separator: ","))"
            )
        }
    }

    /// What one row shows beneath `folder-on-demand-toggle` while its domain is
    /// not enabled: the steps, and the OS's door where it has one.
    struct FolderOSEnableNotice: Equatable {
        let caption: String
        let settingsURL: URL?

        /// System Settings at Login Items & Extensions, measured on macOS 27
        /// (the pane's bundle allows the scheme, and the query names the File
        /// Provider extension point). iOS has no measured door into the Files
        /// app's Edit sheet, so its caption carries the steps alone.
        static let macOSSettingsURL = URL(
            string:
                "x-apple.systempreferences:com.apple.LoginItems-Settings.extension?extensionPointIdentifier=com.apple.fileprovider-nonui"
        )

        static let onMac: Bool = {
            #if os(macOS)
                true
            #else
                false
            #endif
        }()

        /// The row's notice, or `nil` when the shared rule names no such row —
        /// including a row with no identity, which has no domain to enable.
        static func row(
            domainId: String?, noticed: Set<String>, onMac: Bool = Self.onMac
        ) -> Self? {
            guard let domainId, noticed.contains(domainId) else { return nil }
            return onMac
                ? Self(caption: L.devices.onDemandNeedsExtensionEnable, settingsURL: macOSSettingsURL)
                : Self(caption: L.devices.onDemandNeedsFilesEnable, settingsURL: nil)
        }
    }

    extension FileProviderCoordinator {
        /// The shared rule fed this platform's inputs, the same ones
        /// ``plan(actorHex:sets:boundSets:prefs:registered:ownersAt:)`` takes.
        static func osEnableNotices(
            actorHex: String, sets: [PresenceSet], boundSets: Set<String>,
            prefs: FfiOnDemandPrefsStore, registered: [RegisteredPresence],
            ownersAt url: URL? = FileProviderDomainOwners.defaultURL()
        ) throws -> Set<String> {
            Set(
                try onDemandOsEnableNotices(
                    actorIdHex: actorHex, sets: sets, stronger: boundSets.sorted(), prefs: prefs,
                    registered: registered, owners: FileProviderDomainOwners.all(at: url)))
        }

        /// Re-read which rows carry the notice and publish them. Reads only, so
        /// it stays outside the e2e gate on ``reconcile(sets:boundSets:provisioning:)``:
        /// a test launch changes no domain, keychain item or record here. With
        /// no domain registered — always so in a bare binary, whose listing
        /// fails — there is nothing to enable, and neither the owner record nor
        /// the nest is touched.
        static func refreshOSEnableNotices(
            actorHex: String, boundSets: Set<String>,
            sets: @Sendable () async throws -> [PresenceSet]
        ) async {
            let registered = (try? await FileProviderDomains.listPresences()) ?? []
            guard registered.contains(where: { !$0.userEnabled }) else {
                await FileProviderOSEnableNotices.shared.publish([])
                return
            }
            do {
                let noticed = try osEnableNotices(
                    actorHex: actorHex, sets: try await sets(), boundSets: boundSets,
                    prefs: FileProviderDomainPrefs.shared, registered: registered)
                await FileProviderOSEnableNotices.shared.publish(noticed)
            } catch {
                logMessage(
                    level: .warn, target: "fauna.fileprovider",
                    message: "[fp] OS enable read failed: \(error)")
            }
        }
    }
#endif

/// Keeps the OS enable notices current on the Folders page: read when the page
/// appears and whenever the app returns to the foreground, so the caption
/// clears when the person comes back from the Files app or System Settings.
struct FolderOSEnableRefresh: ViewModifier {
    let boundSets: () -> Set<String>

    func body(content: Content) -> some View {
        #if canImport(FileProvider)
            content.modifier(
                FolderPresenceRefresh(key: "") { actorHex, sets in
                    await FileProviderCoordinator.refreshOSEnableNotices(
                        actorHex: actorHex, boundSets: boundSets(), sets: sets)
                })
        #else
            content
        #endif
    }
}
