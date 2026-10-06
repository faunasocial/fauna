import SwiftUI

/// The flat admin **`admin-contacts`** page (`docs/goal/behavior/admin.md` § Contacts +
/// `docs/goal/behavior/carddav-server.md` § Independent enablement), shared by
/// macOS + iOS (one FaunaKit view, thin per-target mount points). The CardDAV-enable
/// sibling of `AdminCalendarView`: a dumb renderer of `CarddavPolicySnapshot` + dispatcher
/// of `CarddavPolicyAction` over the shared `CarddavPolicyMachine` (via `AdminContactsVM`);
/// no policy logic here. Element IDs match `tests/e2e-unified/ui.yaml` `admin-contacts`
/// exactly. Reference renderers: linux (`settings/admin_contacts.rs`), android
/// (`AdminContactsScreen.kt`).
///
/// One control: the deployment-wide **CardDAV-enable** toggle
/// (`admin-contacts-carddav-enabled-toggle`) — the admin shell is one of the two setters of
/// `fauna.bridges.set_carddav_enabled` (onboarding's `onboarding-enable-carddav-checkbox` is
/// the other). After the write the toggle re-reads persisted state
/// (`fauna.bridges.get_mail_config` → `carddav_enabled`). The MDA bridge runs iff
/// `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`, so flipping this
/// on starts the bridge and adds `/carddav` to the shared DAV listener.
///
/// **No port section** (unlike `AdminCalendarView`): CardDAV rides the shared DAV listener
/// that `admin-calendar-caldav-port-input` already governs, so there is no CardDAV-specific
/// port for an admin to pick (`admin.md` § Contacts — "No port field").
///
/// `admin-nav-back` is provided by the admin shell rail (macOS) / the navigation stack
/// (iOS), not this page.
public struct AdminContactsView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminContactsVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0 (the
    /// NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) { self.reloadToken = reloadToken }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                automationText(Ids.adminContactsHeading, L.admin.contactsPage.title)
                    .font(.title)
                Text(L.admin.contactsPage.description)
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

    private func enableGroup(_ snap: CarddavPolicySnapshot) -> some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                Toggle(L.admin.contactsPage.enabledLabel, isOn: Binding(
                    get: { snap.carddavEnabled },
                    set: { on in Task { await vm.setCarddavEnabled(on) } }
                ))
                .accessibilityIdentifier(Ids.adminContactsCarddavEnabledToggle)
                // Read the LIVE snapshot (not the captured `snap`) and flip via the SAME
                // VM dispatch the Toggle's setter runs.
                .automationActivate(
                    Ids.adminContactsCarddavEnabledToggle,
                    value: { (vm.snapshot?.carddavEnabled ?? false) ? "on" : "off" }
                ) {
                    let next = !(vm.snapshot?.carddavEnabled ?? false)
                    Task { await vm.setCarddavEnabled(next) }
                }
                .faunaGate("fauna.bridges.set_carddav_enabled")
                Text(L.admin.contactsPage.enabledSubtitle)
                    .font(.caption).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .disabled(vm.isBusy)
    }
}
