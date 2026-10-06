import SwiftUI
import FaunaKit

struct SidebarView: View {
    @Binding var selection: SidebarItem
    @Environment(MacAppState.self) private var appState

    var body: some View {
        // Admin and Settings are distinct shells entered from the sidebar: while
        // one is selected the normal nav is swapped in place for that shell's page
        // rail (admin.md / settings.md § Navigation model — the desktop vertical
        // sidebar-swap).
        if selection == .admin {
            AdminNavRail()
        } else if selection == .settings {
            SettingsNavRail()
        } else {
            mainList
        }
    }

    private var mainList: some View {
        // A real sidebar click is a top-level navigation, so it clears any
        // OTHER-profile target (the Profile row opens the viewer's OWN profile).
        // A contact-row tap-through sets `selectedSidebar` programmatically via
        // `openProfile`, which bypasses this binding setter, so its target survives.
        List(selection: Binding(
            get: { selection },
            set: { newValue in
                appState.profileActorId = nil
                selection = newValue
            }
        )) {
            ForEach(SidebarItem.standardCases) { item in
                Label(item.label, systemImage: item.systemImage)
                    .tag(item)
                    .accessibilityIdentifier("\(item.rawValue)-tab")
                    // Register the row for the in-process driver's `is_visible`/click
                    // (`{page}-tab`); the closure performs the same selection the
                    // List's tag selection does. Interpolated id is fine for these
                    // NON-gated standard rows (only the gated `admin-tab` below needs
                    // a bare-string literal for the `--nav` lint).
                    .automationActivate("\(item.rawValue)-tab") {
                        appState.profileActorId = nil
                        selection = item
                    }
            }
            // The gated admin entry (admin.md § Navigation model step 1): a single
            // `admin-tab` sidebar row shown only when the shared `am-i-admin` gate
            // passes (`appState.isAdmin`), adjacent to `settings-tab`. The literal
            // id (not the `\(rawValue)-tab` interpolation the standard rows use) is
            // required by the strict gated-tab lint (`lint-ui-elements.py --nav`)
            // and `.automationActivate` registers it for the in-process driver's
            // `is_visible("admin-tab")` gate check. Mirrors iOS's "Nest Admin"
            // Button + web's `{#if userIsAdmin}` admin-tab.
            if appState.isAdmin {
                Label(SidebarItem.admin.label, systemImage: SidebarItem.admin.systemImage)
                    .tag(SidebarItem.admin)
                    .accessibilityIdentifier(Ids.adminTab)
                    .automationActivate(Ids.adminTab) { selection = .admin }
            }
            // The gated family entry (family-safety.md § App surface;
            // ui.yaml navigation.gated_tabs — "shown when fauna.family.status
            // returns any relationship", guardian OR supervised). Same shape as
            // `admin-tab` above, including the bare-string literal id the strict
            // gated-tab lint (`lint-ui-elements.py --nav`) requires. Mirrors
            // linux's second gated sidebar row + web's `{#if familyStatus}`.
            if appState.familyStatus.hasRelationship {
                Label(SidebarItem.family.label, systemImage: SidebarItem.family.systemImage)
                    .tag(SidebarItem.family)
                    .accessibilityIdentifier(Ids.familyTab)
                    .automationActivate(Ids.familyTab) { selection = .family }
            }
        }
        .listStyle(.sidebar)
        .frame(minWidth: 180)
        .toolbar {
            ToolbarItem {
                Button(action: toggleSidebar) {
                    Image(systemName: "sidebar.left")
                }
            }
        }
    }

    private func toggleSidebar() {
        NSApp.keyWindow?.firstResponder?.tryToPerform(
            #selector(NSSplitViewController.toggleSidebar(_:)), with: nil
        )
    }
}

/// The admin shell's vertical nav rail — replaces the app sidebar in place while
/// in admin (admin.md § Navigation model). `admin-nav-back` sits at the top and
/// exits the shell back to the non-admin app; below it are the admin page rows.
struct AdminNavRail: View {
    @Environment(MacAppState.self) private var appState

    var body: some View {
        @Bindable var state = appState
        VStack(spacing: 0) {
            // The uniform "leave admin" affordance (present on every admin page).
            // Lands on the primary view (Conversations), not the Settings shell —
            // admin.md § Navigation model, the 2026-06-07 exit-target correction.
            Button {
                appState.selectedSidebar = .conversations
            } label: {
                Label(L.admin.exit, systemImage: "chevron.left")
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .accessibilityIdentifier(Ids.adminNavBack)
            .automationActivate(Ids.adminNavBack) {
                appState.selectedSidebar = .conversations
            }

            Divider()

            List(AdminPage.built, selection: $state.selectedAdminPage) { page in
                Label(page.label, systemImage: page.systemImage)
                    .tag(page)
                    .accessibilityIdentifier("admin-nav-\(page.navId)")
            }
            .listStyle(.sidebar)
        }
        .frame(minWidth: 180)
    }
}

/// The Settings shell's vertical nav rail — replaces the app sidebar in place
/// while in Settings (settings.md § Navigation model). `settings-nav-back` sits
/// at the top and exits the shell back to the non-settings app (Conversations,
/// the primary view — parallel to `admin-nav-back`); below it are the settings
/// page rows. Mirrors `AdminNavRail` (priority #3 — same shell concept). The
/// page set is `SettingsPage.built`; Status (the folded-in former standalone
/// status page) is the first/default sub-page.
struct SettingsNavRail: View {
    @Environment(MacAppState.self) private var appState

    var body: some View {
        @Bindable var state = appState
        VStack(spacing: 0) {
            // The uniform "leave settings" affordance (present on every settings
            // sub-page). Lands on the primary view (Conversations), matching
            // `admin-nav-back` (settings.md § Navigation model).
            Button {
                appState.selectedSidebar = .conversations
            } label: {
                Label(L.settings.exitSettings, systemImage: "chevron.left")
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .buttonStyle(.plain)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .accessibilityIdentifier(Ids.settingsNavBack)
            .automationActivate(Ids.settingsNavBack) {
                appState.selectedSidebar = .conversations
            }

            Divider()

            List(SettingsPage.built, selection: $state.selectedSettingsPage) { page in
                Label(page.label, systemImage: page.systemImage)
                    .tag(page)
                    .accessibilityIdentifier("settings-nav-\(page.navId)")
            }
            .listStyle(.sidebar)
        }
        .frame(minWidth: 180)
    }
}
