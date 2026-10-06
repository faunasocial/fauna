import SwiftUI

/// The user-facing **Encryption** settings sub-page (`docs/goal/ui/settings.md`
/// § Encryption), shared by macOS + iOS — one FaunaKit view, bare-called from each
/// platform's settings shell, exactly like `MailSettingsView`
/// / `WebSettingsView`. Renders the MLS key-package count over the shared
/// `EncryptionSettingsVM` and republishes key packages on demand.
///
/// Session bits are derived from the shared `FaunaClient` (its own secret
/// + `actor_id_from_secret`) — the same seam `FaunaClient.start()` and
/// `MailSettingsView` use — so no platform-specific app-state (`MacAppState` /
/// `AppState`) is needed. The low-key warning renders as a prominent card (the
/// richer of the two prior per-platform shapes; this was
/// iOS-only before, now shown on macOS too).
public struct EncryptionSettingsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    // The app-held ConversationsVM owns the ONE durable ConversationsManager
    // (its session MLS engine is what "Refresh Keys" must mint on — a mint
    // anywhere else strands the private init keys; see EncryptionSettingsVM).
    @Environment(ConversationsVM.self) private var conversationsVM: ConversationsVM?
    @State private var vm = EncryptionSettingsVM()

    public init() {}

    public var body: some View {
        Form {
            if vm.isKeyPackageLow {
                Section {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: "exclamationmark.triangle.fill")
                            .foregroundStyle(.orange)
                            .font(.title2)
                        VStack(alignment: .leading, spacing: 4) {
                            Text(L.settings.encryptionPage.lowKeyWarningTitle)
                                .font(.headline)
                            Text(L.settings.encryptionPage.lowKeyWarningBody(count: "\(vm.keyPackageCount ?? 0)"))
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(.vertical, 4)
                    Button(L.settings.encryptionPage.refreshKeys) {
                        Task { await vm.refreshKeys() }
                    }
                    .disabled(vm.isPublishing)
                }
            }

            Section(L.settings.encryptionPage.mlsKeyPackages) {
                if let count = vm.keyPackageCount {
                    LabeledContent(L.common.available) {
                        Text("\(count)")
                    }
                } else {
                    Text(L.status.encryption.checking)
                        .foregroundStyle(.secondary)
                }

                Button(L.settings.encryptionPage.refreshKeys) {
                    Task { await vm.refreshKeys() }
                }
                .disabled(vm.isPublishing)
                .buttonStyle(.borderedProminent)

                if vm.isPublishing {
                    ProgressView(L.settings.publishingKeyPackages)
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
        }
        .formStyle(.grouped)
        #if os(iOS)
        .pageTitle(L.settings.encryptionPage.title)
        #endif
        .task {
            // The client's OWN actor, never the registry's active account: that
            // is, on a bound instance, possibly
            // another account (`FaunaClient.ownSecretHex`).
            if let client, let actorId = client.ownActorIdHex {
                vm.configure(
                    api: client.api, actorId: actorId,
                    manager: conversationsVM?.manager
                )
            }
            await vm.loadKeyCount()
        }
    }
}
