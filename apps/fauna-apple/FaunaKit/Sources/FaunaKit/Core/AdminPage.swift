import SwiftUI

/// The pages inside the admin shell (admin.md § Navigation model). Shared by
/// both Apple targets (priority #2): macOS (`MacAppState.selectedAdminPage`,
/// rendered by the `AdminNavRail` sidebar-swap) and iOS (`AppState`, rendered by
/// the mobile `AdminShellView` `NavigationStack`). The *page set* and *IDs* are
/// uniform across all seven apps; only the platform widget that switches them
/// differs (admin.md:29). The `navId` is the cross-app admin sub-page id the
/// e2e state protocol drives (`nav.stack[1].id`), matching the linux GTK Stack
/// child names and ui.yaml `navigation.admin_pages`. The post-2026-06-04 page
/// set has **no** `admin-services` page (§ Admin IA redesign).
public enum AdminPage: String, CaseIterable, Identifiable {
    case dashboard
    case users
    case tiers      // nav id "settings" (tier *definitions*; nav label "Tiers")
    case nest       // nav id "admin-nest"
    case mail       // nav id "admin-mail"
    case calendar   // nav id "admin-calendar" (CalDAV-enable toggle; sibling of admin-mail)
    case contacts   // nav id "admin-contacts" (CardDAV-enable toggle; contacts sibling of admin-calendar)
    case files      // nav id "admin-files" (WebDAV-enable toggle; files sibling of admin-contacts)
    case aliases    // nav id "admin-aliases"
    case dns        // nav id "admin-dns" (full surface: records + CRUD + managed + cert)
    case web        // nav id "admin-web" (apex-actor designation; sibling of admin-mail)
    case bridgesPending  // nav id "admin-bridges-pending" (contextual detail page)
    case custodyHosting  // nav id "admin-custody-hosting" (contextual detail page)
    case adminLogs  // nav id "admin-logs" (nest fauna_log ring via fauna.admin.logs)

    public var id: String { rawValue }

    /// The cross-app admin sub-page id (e2e `nav.stack[1].id`, ui.yaml).
    public var navId: String {
        switch self {
        case .dashboard: "dashboard"
        case .users: "users"
        case .tiers: "settings"
        case .nest: "admin-nest"
        case .mail: "admin-mail"
        case .calendar: "admin-calendar"
        case .contacts: "admin-contacts"
        case .files: "admin-files"
        case .aliases: "admin-aliases"
        case .dns: "admin-dns"
        case .web: "admin-web"
        case .bridgesPending: "admin-bridges-pending"
        case .custodyHosting: "admin-custody-hosting"
        case .adminLogs: "admin-logs"
        }
    }

    public init?(navId: String?) {
        switch navId {
        case "dashboard", nil: self = .dashboard
        case "users": self = .users
        case "settings": self = .tiers
        case "admin-nest": self = .nest
        case "admin-mail": self = .mail
        case "admin-calendar": self = .calendar
        case "admin-contacts": self = .contacts
        case "admin-files": self = .files
        case "admin-aliases": self = .aliases
        case "admin-dns": self = .dns
        case "admin-web": self = .web
        case "admin-bridges-pending": self = .bridgesPending
        case "admin-custody-hosting": self = .custodyHosting
        case "admin-logs": self = .adminLogs
        default: return nil
        }
    }

    public var label: String {
        switch self {
        case .dashboard: L.admin.dashboard.title
        case .users: L.admin.usersPage.title
        case .tiers: L.admin.settingsPage.title
        case .nest: L.admin.nestPage.title
        case .mail: L.admin.mailPage.title
        case .calendar: L.admin.calendarPage.title
        case .contacts: L.admin.contactsPage.title
        case .files: L.admin.filesPage.title
        case .aliases: L.admin.aliases
        case .dns: L.admin.dns.title
        case .web: L.admin.webPage.title
        case .bridgesPending: L.admin.bridgesPending.title
        case .custodyHosting: L.admin.custodyHosting.title
        case .adminLogs: L.admin.logsPage.title
        }
    }

    public var systemImage: String {
        switch self {
        case .dashboard: "chart.bar"
        case .users: "person.3"
        case .tiers: "square.stack.3d.up"
        case .nest: "server.rack"
        case .mail: "envelope"
        case .calendar: "calendar"
        case .contacts: "person.crop.rectangle.stack"
        case .files: "folder"
        case .aliases: "arrowshape.turn.up.right"
        case .dns: "network"
        case .web: "globe"
        case .bridgesPending: "link"
        case .custodyHosting: "archivebox"
        case .adminLogs: "doc.text.magnifyingglass"
        }
    }

    /// The pages whose content is built today. The switcher lists only these, in
    /// ui.yaml `navigation.admin_pages` order (the primary pages first, then the
    /// contextual detail pages). All page content is the shared FaunaKit
    /// `Admin*View` set, reused by both Apple targets (priority #2):
    /// `tiers` (`admin-settings`, tier *definitions* + in-place cap editing,
    /// admin.md § 3) + `nest` (`admin-nest`, storage-mode indicator + operator
    /// pairing toggle + Factory Reset, admin.md § N) built 2026-06-13;
    /// `bridgesPending` (contextual detail page) + `aliases` + `dns` + `web` +
    /// `mail` (the flat admin-mail policy form) built earlier on macOS;
    /// `calendar` (`admin-calendar`, the deployment-wide CalDAV-enable toggle —
    /// the sibling of mail-enable, admin.md § 8) lifted 2026-06-17 from the linux
    /// lead over the shared `CaldavPolicyMachine`; `contacts` (`admin-contacts`,
    /// the deployment-wide CardDAV-enable toggle — the contacts sibling of
    /// calendar, admin.md § Contacts) lifted 2026-07-12 over the shared
    /// `CarddavPolicyMachine`; `files` (`admin-files`, the deployment-wide
    /// WebDAV-enable toggle — the files sibling of contacts, admin.md § Files)
    /// lifted 2026-07-12 over the shared `WebdavPolicyMachine`. Neither contacts
    /// nor files has a port knob: both ride the DAV listener calendar already
    /// governs.
    public static var built: [AdminPage] {
        [.dashboard, .users, .tiers, .nest, .mail, .calendar, .contacts, .files, .aliases, .bridgesPending, .custodyHosting, .dns, .web, .adminLogs]
    }
}
