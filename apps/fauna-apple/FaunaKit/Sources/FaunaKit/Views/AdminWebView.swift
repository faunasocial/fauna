import SwiftUI

/// The admin **`admin-web`** page (`docs/goal/behavior/web-content-hosting.md`
/// § Admin apex hosting), shared by macOS + iOS (one FaunaKit view, thin per-target
/// mount points). A dumb renderer of `AdminWebVM`; no
/// business logic here. Element IDs match `tests/e2e-unified/ui.yaml` `admin-web`
/// exactly. Reference renderers: linux
/// (`apps/fauna-linux/src/settings/admin_web.rs`), web
/// (`routes/admin/web/+page.svelte`), windows (`AdminWebPage`).
///
/// One control: the **apex-actor picker** (`admin-web-apex-actor-select`) — an
/// Admin-class designation of which actor's `web` content serves at
/// `https://<domain>/`, "None" clearing it to the built-in info page. The direct
/// analogue of the per-domain catch-all mail actor. Per-user subdomain hosting is
/// the user `web-settings` page, not here.
///
/// `admin-nav-back` is provided by the admin shell rail (macOS), not this page;
/// iOS reaches it via a `NavigationLink` (system back).
public struct AdminWebView: View {
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var vm = AdminWebVM()
    /// Reload trigger — macOS passes the shell's `navGeneration`; iOS leaves it 0
    /// (the NavigationLink re-mounts the view, re-running the load).
    var reloadToken: Int = 0

    public init(reloadToken: Int = 0) {
        self.reloadToken = reloadToken
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text(L.admin.webPage.title)
                    .font(.title)
                    .accessibilityIdentifier(Ids.adminWebHeading)

                VStack(alignment: .leading, spacing: 8) {
                    Text(L.admin.webPage.apexSelectLabel)
                        .font(.headline)
                    Text(L.admin.webPage.apexSelectSubtitle)
                        .font(.caption)
                        .foregroundStyle(.secondary)

                    // Index-based selection (the tag is the stable option index) so
                    // a label collision can't mis-route, like linux/web.
                    Picker(L.admin.webPage.apexSelectLabel, selection: Binding(
                        get: { vm.selectedIndex },
                        set: { idx in Task { await vm.select(index: idx) } }
                    )) {
                        ForEach(vm.options) { opt in
                            Text(opt.label).tag(opt.id)
                        }
                    }
                    .pickerStyle(.menu)
                    .labelsHidden()
                    .accessibilityIdentifier(Ids.adminWebApexActorSelect)
                    // In-process driver: `/element/select` chooses by display
                    // label (the same string the test reads back), and
                    // `/element/text` reads the selected label. `set` maps the
                    // wire label to the option's stable index and dispatches the
                    // *same* mutation choosing the menu item performs
                    // (`vm.select(index:)` → `fauna.web.set_apex_actor`). Both
                    // closures re-read live `vm.options`/`vm.selectedIndex`.
                    .automationSelect(
                        Ids.adminWebApexActorSelect,
                        value: { vm.options.first(where: { $0.id == vm.selectedIndex })?.label }
                    ) { label in
                        if let opt = vm.options.first(where: { $0.label == label }) {
                            Task { await vm.select(index: opt.id) }
                        }
                    }
                    // Commits on pick (no draft); designate and clear are the same
                    // kind — `actor: None` is the clear.
                    .faunaGate("fauna.web.set_apex_actor")

                    Text(L.admin.webPage.apexInfo(url: vm.apexUrl))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier(Ids.adminWebApexInfo)
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
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
}
