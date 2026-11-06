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

    private var countText: String {
        vm.keyPackageCount.map { "\($0)" } ?? L.status.encryption.checking
    }

    private var lowBody: String {
        L.settings.encryptionPage.lowKeyWarningBody(count: "\(vm.keyPackageCount ?? 0)")
    }

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
                            Text(lowBody)
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                                .accessibilityIdentifier(Ids.encryptionKeypackageLowWarning)
                                .automationValue(Ids.encryptionKeypackageLowWarning, text: { lowBody })
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
                // ONE stable element for both states (the bare count, or the
                // loading copy before the first read): two branches registering
                // one id leave a stale registration behind on the swap.
                LabeledContent(L.common.available) {
                    Text(countText)
                        .accessibilityIdentifier(Ids.encryptionKeypackageCount)
                        .automationValue(Ids.encryptionKeypackageCount, text: { countText })
                }

                Button(L.settings.encryptionPage.refreshKeys) {
                    Task { await vm.refreshKeys() }
                }
                .disabled(vm.isPublishing)
                .accessibilityIdentifier(Ids.encryptionKeypackageRefreshButton)
                .automationActivate(
                    Ids.encryptionKeypackageRefreshButton,
                    isEnabled: { !vm.isPublishing }
                ) { Task { await vm.refreshKeys() } }
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
        // The low card's section leaves a table-backed iOS Form without its
        // window detach ever reaching the e2e registry, so the warning would read
        // present after a top-up: a new identity per flag rebuilds the Form.
        .id(vm.isKeyPackageLow)
        #if os(iOS)
        .pageTitle(L.settings.encryptionPage.title)
        #endif
        .task(id: client?.ownActorIdHex) {
            // The client's OWN actor, never the registry's active account: that
            // is, on a bound instance, possibly
            // another account (`FaunaClient.ownSecretHex`).
            if let client, let actorId = client.ownActorIdHex {
                vm.configure(
                    api: client.api, actorId: actorId,
                    manager: conversationsVM?.manager
                )
            }
            // Every visit re-reads the nest's count (a read only, never a mint),
            // showing the loading copy until it lands.
            vm.keyPackageCount = nil
            await vm.loadKeyCount()
        }
    }
}
