using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;

namespace FaunaApp.Views;

/// <summary>
/// The Windows admin shell — Model B (admin.md § Navigation model, ratified
/// 2026-06-01). A distinct section the admin steps into via the single gated
/// <c>admin-tab</c> (MainPage); the admin pages are switched by this shell's own
/// <c>NavigationView</c> and every page carries the uniform <c>admin-nav-back</c>
/// "leave admin" button in the shell header. Hosts the existing admin pages
/// (AdminDashboardPage / AdminUsersPage / AdminSettingsPage / AdminNestPage /
/// AdminMailPage / AdminCalendarPage / AdminContactsPage / AdminFilesPage /
/// AdminAliasesPage / AdminDnsPage / AdminBridgesPendingPage /
/// AdminCustodyHostingPage) in
/// an inner Frame; the nav-by-id map is the platform-agnostic, unit-tested
/// <see cref="AdminNavigation"/>. (admin-services was removed + admin-nest added
/// 2026-06-04 — admin.md § Admin IA redesign; admin-contacts / admin-files added
/// 2026-07-10 — admin.md § Contacts / § Files.)
/// </summary>
public sealed partial class AdminShellPage : Page
{
    private ServiceClients? _clients;
    private readonly Dictionary<string, NavigationViewItem> _itemsByTag;

    public AdminShellPage()
    {
        this.InitializeComponent();
        _itemsByTag = new Dictionary<string, NavigationViewItem>
        {
            [AdminNavigation.Dashboard] = SubDashboard,
            [AdminNavigation.Users] = SubUsers,
            [AdminNavigation.Settings] = SubSettings,
            [AdminNavigation.Nest] = SubNest,
            [AdminNavigation.Mail] = SubMail,
            [AdminNavigation.Calendar] = SubCalendar,
            [AdminNavigation.Contacts] = SubContacts,
            [AdminNavigation.Files] = SubFiles,
            [AdminNavigation.Web] = SubWeb,
            [AdminNavigation.Aliases] = SubAliases,
            [AdminNavigation.Dns] = SubDns,
            [AdminNavigation.BridgesPending] = SubBridgesPending,
            [AdminNavigation.CustodyHosting] = SubCustodyHosting,
            [AdminNavigation.Logs] = SubLogs,
        };
    }

    /// <summary>
    /// The admin sub-page currently shown in the shell's inner frame, if any.
    /// MainPage's UpdateTestMessages drills through here so per-page error/info
    /// surfacing keeps working while the shell is the main content.
    /// </summary>
    internal Page? CurrentContent => AdminContentFrame?.Content as Page;

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
    }

    private void Page_Loaded(object sender, RoutedEventArgs e)
    {
        // Ensure the inner frame is populated once laid out. The state-protocol
        // path (MainPage.NavigateToAdminSubPage) selects a sub-page before this
        // runs; the `admin-tab` click path leaves nothing selected → dashboard.
        // Either way, if SelectionChanged didn't already navigate the frame
        // (timing), navigate to the selected tag (default dashboard) here.
        FaunaApp.Core.Logs.E2eTrace.Write(
            $"[admin-shell] Page_Loaded: content={AdminContentFrame.Content?.GetType().Name ?? "null"}, " +
            $"selected={(AdminNav.SelectedItem as NavigationViewItem)?.Tag ?? "null"}, clients-null={_clients is null}");
        if (AdminContentFrame.Content is null)
        {
            var tag = (AdminNav.SelectedItem as NavigationViewItem)?.Tag as string
                      ?? AdminNavigation.Dashboard;
            if (AdminNav.SelectedItem is null && _itemsByTag.TryGetValue(tag, out var item))
            {
                AdminNav.SelectedItem = item;
            }
            NavigateInner(tag);
            FaunaApp.Core.Logs.E2eTrace.Write(
                $"[admin-shell] Page_Loaded fallback done: tag={tag}, content={AdminContentFrame.Content?.GetType().Name ?? "null"}");
        }
    }

    /// <summary>
    /// Show the admin sub-page for a shared state-protocol sub-page id (null /
    /// bare-admin / unknown → the dashboard). Selecting the inner nav item drives
    /// <see cref="AdminNav_SelectionChanged"/>, which navigates the inner Frame;
    /// re-selecting the current item forces a re-nav.
    /// </summary>
    public void NavigateToSubPage(string? subId)
    {
        var tag = AdminNavigation.SubPageTag(subId);
        if (!_itemsByTag.TryGetValue(tag, out var item))
        {
            return;
        }
        FaunaApp.Core.Logs.E2eTrace.Write(
            $"[admin-shell] NavigateToSubPage: tag={tag}, already-selected={ReferenceEquals(AdminNav.SelectedItem, item)}, clients-null={_clients is null}");
        if (ReferenceEquals(AdminNav.SelectedItem, item))
        {
            NavigateInner(tag);
        }
        else
        {
            AdminNav.SelectedItem = item;
        }
        // Same defensive shape as MainPage.NavigateToAdminSubPage: `AdminNav.SelectedItem = item` alone is not reliable — a
        // freshly-constructed AdminShellPage's OWN NavigationView can still not be
        // ready to raise AdminNav_SelectionChanged synchronously (Page_Loaded's
        // own comment already knew this could happen "on timing" and has a
        // fallback — but that fallback only fires if IT runs at all, and only
        // catches the FIRST sub-page selection of the page's lifetime, not a
        // later NavigateToSubPage call once the page already exists). Confirm
        // the frame actually moved; drive it directly if not. Idempotent: a
        // late-firing SelectionChanged's own NavigateInner just re-navigates to
        // the same page type.
        if (AdminContentFrame.Content is null || SubPageType(tag) != AdminContentFrame.Content?.GetType())
        {
            FaunaApp.Core.Logs.E2eTrace.Write(
                $"[admin-shell] NavigateToSubPage fallback: tag={tag}, content-was={AdminContentFrame.Content?.GetType().Name ?? "null"}");
            NavigateInner(tag);
        }
    }

    private void AdminNav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem item && item.Tag is string tag)
        {
            NavigateInner(tag);
        }
    }

    private void NavigateInner(string tag)
    {
        if (_clients is null)
        {
            return;
        }
        var pageType = SubPageType(tag);
        if (pageType is not null)
        {
            AdminContentFrame.Navigate(pageType, _clients);
        }
    }

    private void NavBack_Click(object sender, RoutedEventArgs e)
        => MainPage.Current?.LeaveAdmin();

    /// <summary>
    /// Map a shell tag (the inner nav item Tag = the <see cref="AdminNavigation"/>
    /// tag) to its WinUI admin Page type — the WinUI-only half of the nav map.
    /// </summary>
    private static Type? SubPageType(string tag) => tag switch
    {
        AdminNavigation.Dashboard => typeof(AdminDashboardPage),
        AdminNavigation.Users => typeof(AdminUsersPage),
        AdminNavigation.Settings => typeof(AdminSettingsPage),
        AdminNavigation.Nest => typeof(AdminNestPage),
        AdminNavigation.Mail => typeof(AdminMailPage),
        AdminNavigation.Calendar => typeof(AdminCalendarPage),
        AdminNavigation.Contacts => typeof(AdminContactsPage),
        AdminNavigation.Files => typeof(AdminFilesPage),
        AdminNavigation.Web => typeof(AdminWebPage),
        AdminNavigation.Aliases => typeof(AdminAliasesPage),
        AdminNavigation.Dns => typeof(AdminDnsPage),
        AdminNavigation.BridgesPending => typeof(AdminBridgesPendingPage),
        AdminNavigation.CustodyHosting => typeof(AdminCustodyHostingPage),
        AdminNavigation.Logs => typeof(AdminLogsPage),
        _ => null,
    };
}
