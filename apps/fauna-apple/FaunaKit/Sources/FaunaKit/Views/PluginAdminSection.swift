// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// State behind the admin's plugin list on the Bridges page
/// (`behavior/bridges.md` § Hosted plugins on this page). Every row's texts are
/// composed once in shared Rust (`FfiInstalledPlugin`); this holds only the
/// list, the install field and the arm-then-confirm uninstall latch.
@MainActor @Observable
public final class PluginAdminVM {
    /// `nil` until the first read returned (the three-state list: nothing, rows,
    /// the empty line).
    public var plugins: [FfiInstalledPlugin]?
    public var installUrl = ""
    public var installOpened = false
    public var uninstallArmed: String?
    public var errorMessage: String?

    private var api: APIClient?

    public init() {}

    public func configure(api: APIClient) {
        if let current = self.api, current !== api { reset() }
        self.api = api
    }

    public func reset() {
        plugins = nil
        installUrl = ""
        installOpened = false
        uninstallArmed = nil
        errorMessage = nil
        api = nil
    }

    public func refresh() async {
        guard let api else { return }
        do {
            plugins = try await api.listInstalledPlugins()
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    public func install() async {
        let url = installUrl.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let api, !url.isEmpty else { return }
        do {
            try await api.installPlugin(documentUrl: url)
            installUrl = ""
            installOpened = true
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// First press arms the inline confirm; the armed press uninstalls and re-reads.
    public func uninstall(_ plugin: FfiInstalledPlugin) async {
        guard uninstallArmed == plugin.principalIdHex else {
            uninstallArmed = plugin.principalIdHex
            return
        }
        uninstallArmed = nil
        guard let api else { return }
        do {
            try await api.uninstallPlugin(principalIdHex: plugin.principalIdHex)
            errorMessage = nil
        } catch {
            errorMessage = error.localizedDescription
        }
        await refresh()
    }

    public func cancelUninstall() {
        uninstallArmed = nil
    }
}

/// The admin's installed-plugins list with its install entry — one FaunaKit
/// view both Apple Bridges pages embed, painted only while the caller is an
/// admin (the host passes `isAdmin`). Reference painter: tui
/// `apps/fauna-tui/src/bridges.rs::plugin_elements`.
public struct PluginAdminSection: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = PluginAdminVM()
    private let isAdmin: Bool

    public init(isAdmin: Bool) {
        self.isAdmin = isAdmin
    }

    public var body: some View {
        Group {
            if isAdmin {
                content
            }
        }
        .task(id: SessionKey(client, reloadToken: isAdmin ? 1 : 0)) {
            guard isAdmin, let client else {
                vm.reset()
                return
            }
            vm.configure(api: client.api)
            await vm.refresh()
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(L.bridges.pluginsHeading).font(.headline)
            if let plugins = vm.plugins {
                if plugins.isEmpty {
                    Text(L.bridges.pluginsEmpty).foregroundStyle(.secondary)
                }
                ForEach(Array(plugins.enumerated()), id: \.offset) { index, plugin in
                    row(plugin, index: index)
                }
            }
            TextField(L.bridges.pluginInstallLabel, text: $vm.installUrl)
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier(Ids.bridgePluginInstallUrl)
                .automationField(
                    Ids.bridgePluginInstallUrl,
                    text: $vm.installUrl,
                    commit: { Task { await vm.install() } }
                )
            let canInstall = !vm.installUrl.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            Button(L.bridges.pluginInstallSubmit) {
                Task { await vm.install() }
            }
            .disabled(!canInstall)
            .accessibilityIdentifier(Ids.bridgePluginInstallSubmit)
            .automationActivate(Ids.bridgePluginInstallSubmit, isEnabled: { canInstall }) {
                Task { await vm.install() }
            }
            if vm.installOpened {
                Text(L.bridges.pluginInstallOpened)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            if let error = vm.errorMessage {
                ErrorBanner(message: error)
            }
        }
    }

    private func row(_ plugin: FfiInstalledPlugin, index: Int) -> some View {
        let publisher = renderLocalizedText(plugin.publisher)
        let status = plugin.status.map(renderLocalizedText).joined(separator: "\n")
        let armed = vm.uninstallArmed == plugin.principalIdHex
        return VStack(alignment: .leading, spacing: 4) {
            Text(plugin.name)
                .font(.subheadline.bold())
                .accessibilityIdentifier(Ids.bridgePluginName)
                .automationValue(
                    Ids.bridgePluginName,
                    text: { plugin.name },
                    attributes: { ["client_id": plugin.clientId] }
                )
            automationText(Ids.bridgePluginPublisher, publisher)
                .font(.caption)
            Text(status)
                .font(.caption)
                .accessibilityIdentifier(Ids.bridgePluginStatus)
                .automationValue(
                    Ids.bridgePluginStatus,
                    text: { status },
                    attributes: { ["state": plugin.state] }
                )
            if armed {
                Text(L.bridges.pluginUninstallPrompt(name: plugin.name)).font(.caption)
            }
            HStack {
                Button(armed ? L.bridges.pluginUninstallConfirm : L.bridges.pluginUninstall,
                       role: .destructive) {
                    Task { await vm.uninstall(plugin) }
                }
                .accessibilityIdentifier(Ids.bridgePluginUninstall)
                .automationActivate(Ids.bridgePluginUninstall) {
                    Task { await vm.uninstall(plugin) }
                }
                if armed {
                    // Untagged Keep, as tui's `chrome_button`.
                    Button(L.bridges.pluginUninstallCancel) { vm.cancelUninstall() }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.bridgePluginItem)
        .automationValue(Ids.bridgePluginItem, text: { [plugin.name, publisher, status].joined(separator: "\n") })
        .automationScope(Ids.bridgePluginItem, index: index)
    }
}
#endif // !FAUNA_KIDS
