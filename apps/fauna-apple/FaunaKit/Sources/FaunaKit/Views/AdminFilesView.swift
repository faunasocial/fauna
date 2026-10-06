import SwiftUI

/// The flat admin **`admin-files`** page (`docs/goal/behavior/admin.md` § Files +
/// `docs/goal/behavior/webdav-server.md` § Independent enablement point 1), shared by
/// macOS + iOS (one FaunaKit view, thin per-target mount points). The WebDAV-enable
/// sibling of `AdminCalendarView`: a dumb renderer of `WebdavPolicySnapshot` + dispatcher
/// of `WebdavPolicyAction` over the shared `WebdavPolicyMachine` (via `AdminFilesVM`); no
/// policy logic here. Element IDs match `tests/e2e-unified/ui.yaml` `admin-files` exactly.
/// Reference renderers: linux (`settings/admin_files.rs`), android (`AdminFilesScreen.kt`).
///
/// One control: the deployment-wide **WebDAV-enable** toggle
/// (`admin-files-webdav-enabled-toggle`) — the admin shell is one of the two setters of
/// `fauna.bridges.set_webdav_enabled` (onboarding's `onboarding-enable-webdav-checkbox` is
/// the other). After the write the toggle re-reads persisted state
/// (`fauna.bridges.get_mail_config` → `webdav_enabled`). The MDA bridge runs iff
/// `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`, so flipping this
/// on starts the bridge and adds `/webdav` to the shared DAV listener.
///
/// **No port section** (unlike `AdminCalendarView`): WebDAV rides the shared DAV listener
/// that `admin-calendar-caldav-port-input` already governs, so there is no WebDAV-specific
/// port for an admin to pick (`admin.md` § Files — "No port field").
///
/// **Harmless-on**: enabling this exposes *nothing* on its own — the real exposure gate is
/// the user's per-set `folder-webdav-toggle` (Settings → Folders, default OFF).
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation stack
/// (iOS), not this page.
public struct AdminFilesView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminFilesVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0 (the
    /// NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.adminFilesHeading, L.admin.filesPage.title)
                    .font(.title)
                Text(L.admin.filesPage.description)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }

                if let snap = vm.snapshot {
                    enableGroup(snap)
                } else {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) {
            guard let client else { return }
            await vm.configure(api: client.api)
        }
    }

    // MARK: - Deployment-wide toggle (dispatch-on-change)

    private func enableGroup(_ snap: WebdavPolicySnapshot) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Toggle(L.admin.filesPage.enabledLabel, isOn: Binding(
                    get: { snap.webdavEnabled },
                    set: { on in Task { await vm.setWebdavEnabled(on) } }
                ))
                .accessibilityIdentifier(Ids.adminFilesWebdavEnabledToggle)
                // Read the LIVE snapshot (not the captured `snap`) and flip via the SAME
                // VM dispatch the Toggle's setter runs.
                .automationActivate(
                    Ids.adminFilesWebdavEnabledToggle,
                    value: { (vm.snapshot?.webdavEnabled ?? false) ? "on" : "off" }
                ) {
                    let next = !(vm.snapshot?.webdavEnabled ?? false)
                    Task { await vm.setWebdavEnabled(next) }
                }
                .faunaGate("fauna.bridges.set_webdav_enabled")
                Text(L.admin.filesPage.enabledSubtitle)
                    .font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
    }
}
