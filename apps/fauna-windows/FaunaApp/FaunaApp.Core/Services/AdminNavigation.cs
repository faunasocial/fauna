using System.Collections.Generic;

namespace FaunaApp.Core.Services;

/// <summary>
/// Pure navigation map for the Windows admin shell (admin.md § Navigation model,
/// ratified 2026-06-01). Windows uses the canonical Model B: a single gated
/// <c>admin-tab</c> entry opens a <b>distinct admin shell</b> whose sub-pages are
/// switched by a shell-local NavigationView, and every page exposes the uniform
/// <c>admin-nav-back</c> "leave admin" affordance (not an inter-admin-page jump).
///
/// This class is the platform-agnostic half (no WinUI types) so it is unit-
/// testable in FaunaApp.Tests — FlaUI e2e flakes on win-arm64, so this map is the
/// deterministic gate. It maps the shared e2e state-protocol admin sub-page id
/// (the linux GTK Stack child name the cross-app action layer sends as
/// <c>nav.stack[last].id</c> — see tests/e2e-unified/actions/admin.py) to a stable
/// shell tag; the WinUI <see cref="object"/> AdminShellPage maps the tag to the
/// concrete Page type.
/// </summary>
public static class AdminNavigation
{
    // Shell sub-page tags (also the inner NavigationView item Tags). Kept equal
    // to the state-protocol sub-page ids so the map is identity for the explicit
    // ids; a bare {view:"admin"} nav (no id — the dashboard) falls through to
    // Dashboard, the shell's default landing page.
    public const string Dashboard = "dashboard";
    public const string Users = "users";
    public const string Settings = "settings";
    // The per-page-services redesign (2026-06-04, admin.md § Admin IA redesign)
    // removed the standalone admin-services page and added admin-nest (nest-wide
    // settings: storage-mode + admin pairing toggle + Factory Reset). The state-
    // protocol id for the new page is "admin-nest" (actions/admin.py navigate_nest).
    public const string Nest = "admin-nest";
    public const string Aliases = "admin-aliases";
    public const string Mail = "admin-mail";
    // The deployment-wide CalDAV-enable toggle (admin.md § 8 Calendar, ratified
    // 2026-06-17). A primary admin page (in navigation.admin_pages, right after
    // admin-mail), the CalDAV-enable sibling of admin-mail. State-protocol id
    // "admin-calendar" (actions/admin.py navigate_calendar).
    public const string Calendar = "admin-calendar";
    // The deployment-wide CardDAV-enable toggle (admin.md § Contacts,
    // carddav-server.md § Independent enablement, added 2026-07-10). A primary
    // admin page (in navigation.admin_pages, right after admin-calendar), the
    // contacts sibling of admin-calendar. State-protocol id "admin-contacts"
    // (actions/admin.py navigate_contacts).
    public const string Contacts = "admin-contacts";
    // The deployment-wide WebDAV-enable toggle (admin.md § Files,
    // webdav-server.md § Independent enablement, added 2026-07-10). A primary
    // admin page (in navigation.admin_pages, right after admin-contacts), the
    // files sibling of admin-contacts. State-protocol id "admin-files"
    // (actions/admin.py navigate_files).
    public const string Files = "admin-files";
    // Nest-wide apex-actor designation (web-content-hosting.md § Admin apex hosting;
    // ui.yaml admin-web). A contextual page (not in navigation.admin_pages), sibling of
    // admin-mail, reached by nav-by-id — state-protocol id "admin-web"
    // (actions/admin.py navigate_web / test_web_authoring.py).
    public const string Web = "admin-web";
    public const string Dns = "admin-dns";
    public const string BridgesPending = "admin-bridges-pending";
    // The nest-wide custody-hosting registry (account-data-plane.md § Two-sided
    // bounds) — a contextual detail
    // page (like Dns / BridgesPending), reached by nav-by-id, not in the
    // admin_pages rail. State-protocol id "admin-custody-hosting"
    // (actions/admin.py navigate_custody_hosting).
    public const string CustodyHosting = "admin-custody-hosting";
    // The admin view of the NEST's fauna-log ring over fauna.admin.logs
    // (observability.md § Surfaces) — a contextual detail page (like Dns /
    // BridgesPending), reached by nav-by-id, not in the admin_pages rail. The
    // state-protocol id is "admin-logs" (actions/admin.py navigate_logs).
    public const string Logs = "admin-logs";

    /// <summary>
    /// The admin sub-pages in shell display order (matching ui.yaml
    /// <c>navigation.admin_pages</c>: dashboard, users, settings, nest, mail,
    /// calendar, contacts, files, aliases) followed by the contextual detail
    /// pages (web, dns, bridges-pending, custody-hosting, logs). The first
    /// (<see cref="Dashboard"/>) is the default landing page when the shell is
    /// entered via <c>admin-tab</c>.
    /// </summary>
    public static readonly IReadOnlyList<string> SubPages = new[]
    {
        Dashboard, Users, Settings, Nest, Mail, Calendar, Contacts, Files, Web, Aliases, Dns, BridgesPending,
        CustodyHosting, Logs,
    };

    /// <summary>
    /// Map a state-protocol admin sub-page id to its shell tag. A null / empty /
    /// unrecognized id (incl. the bare <c>{view:"admin"}</c> dashboard nav and an
    /// explicit "dashboard") lands on the Dashboard — the shell's default page, so
    /// an admin nav never resolves to "nowhere".
    /// </summary>
    public static string SubPageTag(string? subId) => subId switch
    {
        Users => Users,
        Settings => Settings,
        Nest => Nest,
        Aliases => Aliases,
        Mail => Mail,
        Calendar => Calendar,
        Contacts => Contacts,
        Files => Files,
        Web => Web,
        Dns => Dns,
        BridgesPending => BridgesPending,
        CustodyHosting => CustodyHosting,
        Logs => Logs,
        _ => Dashboard,
    };
}
