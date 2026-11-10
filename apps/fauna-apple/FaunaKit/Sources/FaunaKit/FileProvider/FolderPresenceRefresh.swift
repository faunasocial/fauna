import FaunaFFISwift
import Foundation
import SwiftUI

#if canImport(FileProvider)
    /// The Folders page's File Provider re-read trigger, shared by the OS
    /// enable notices (``FolderOSEnableRefresh``) and the domain parks
    /// (``FolderParkRefresh``): runs `run` when `key` changes (and on first
    /// appearance) and whenever the app returns to the foreground, handing it
    /// the active actor and the presence-set reader — both reads need the
    /// same signed-in guard and the same `folderPresenceSets` fetch.
    struct FolderPresenceRefresh: ViewModifier {
        @Environment(FaunaClient.self) private var client: FaunaClient?
        let key: String
        let run: (String, @Sendable () async throws -> [PresenceSet]) async -> Void

        func body(content: Content) -> some View {
            content
                .task(id: key) { await refresh() }
                .onReceive(NotificationCenter.default.publisher(for: Self.becameActive)) { _ in
                    Task { await refresh() }
                }
        }

        private static var becameActive: Notification.Name {
            #if os(macOS)
                NSApplication.didBecomeActiveNotification
            #else
                UIApplication.didBecomeActiveNotification
            #endif
        }

        private func refresh() async {
            guard let client, !client.deviceId.isEmpty,
                let actorHex = FaunaClient.activeActorIdHex
            else { return }
            let (api, deviceId) = (client.api, client.deviceId)
            await run(actorHex) {
                try await api.folderPresenceSets(deviceIdHex: deviceId)
            }
        }
    }
#endif
