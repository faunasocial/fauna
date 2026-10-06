using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;

namespace FaunaApp.Views;

/// <summary>
/// The Windows settings shell — the sidebar-swap shape (settings.md § Navigation
/// model, ratified 2026-06-03). A distinct section the user steps into via the
/// single <c>settings-tab</c> (MainPage); the settings sub-pages are switched by
/// this shell's own <c>NavigationView</c> and every sub-page carries the uniform
/// <c>settings-nav-back</c> "leave settings" button in the shell header. Hosts the
/// settings sub-pages (StatusPage as the default + SettingsAccountPage /
/// SettingsPrivacyPage / SettingsGeneralPage / SettingsEncryptionPage and the
/// mail / nests sub-pages) in an inner Frame; the nav-by-id map is the
/// platform-agnostic, unit-tested <see cref="SettingsNavigation"/>.
///
/// Mirrors <see cref="AdminShellPage"/> exactly; only the nav map, sub-page
/// types, and the nav-back target (MainPage.LeaveSettings) differ.
/// </summary>
public sealed partial class SettingsShellPage : Page
{
    private ServiceClients? _clients;
    private readonly Dictionary<string, NavigationViewItem> _itemsByTag;

    public SettingsShellPage()
    {
        this.InitializeComponent();
        _itemsByTag = new Dictionary<string, NavigationViewItem>
        {
            [SettingsNavigation.Status] = SubStatus,
            [SettingsNavigation.Account] = SubAccount,
            [SettingsNavigation.MemberReview] = SubMemberReview,
            [SettingsNavigation.Privacy] = SubPrivacy,
            [SettingsNavigation.MutedWords] = SubMutedWords,
            [SettingsNavigation.Personalization] = SubPersonalization,
            [SettingsNavigation.LabelerCatalog] = SubLabelerCatalog,
            [SettingsNavigation.General] = SubGeneral,
            [SettingsNavigation.Encryption] = SubEncryption,
            [SettingsNavigation.Web] = SubWeb,
            [SettingsNavigation.Subscriptions] = SubSubscriptions,
            [SettingsNavigation.Devices] = SubDevices,
            [SettingsNavigation.Folders] = SubFolders,
            [SettingsNavigation.Nostr] = SubNostr,
            [SettingsNavigation.Atproto] = SubAtproto,
            [SettingsNavigation.Mail] = SubMail,
            [SettingsNavigation.MailAliases] = SubMailAliases,
            [SettingsNavigation.MailSpam] = SubMailSpam,
            [SettingsNavigation.MailExport] = SubMailExport,
            [SettingsNavigation.MailImport] = SubMailImport,
            [SettingsNavigation.MailLists] = SubMailLists,
            [SettingsNavigation.MailListMembers] = SubMailListMembers,
            [SettingsNavigation.LinkedNests] = SubLinkedNests,
            [SettingsNavigation.TaskDelegation] = SubTaskDelegation,
            [SettingsNavigation.ConnectedApps] = SubConnectedApps,
            [SettingsNavigation.Logs] = SubLogs,
        };
    }

    /// <summary>
    /// The settings sub-page currently shown in the shell's inner frame, if any.
    /// MainPage's UpdateTestMessages drills through here so per-page error/info
    /// surfacing keeps working while the shell is the main content.
    /// </summary>
    internal Page? CurrentContent => SettingsContentFrame?.Content as Page;

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
        // path (MainPage.NavigateToSettingsSubPage) selects a sub-page before this
        // runs; the `settings-tab` click path leaves nothing selected → status.
        // Either way, if SelectionChanged didn't already navigate the frame
        // (timing), navigate to the selected tag (default status) here.
        if (SettingsContentFrame.Content is null)
        {
            var tag = (SettingsNav.SelectedItem as NavigationViewItem)?.Tag as string
                      ?? SettingsNavigation.Status;
            if (SettingsNav.SelectedItem is null && _itemsByTag.TryGetValue(tag, out var item))
            {
                SettingsNav.SelectedItem = item;
            }
            NavigateInner(tag);
        }
    }

    /// <summary>
    /// Show the settings sub-page for a shared state-protocol sub-page id (null /
    /// bare-settings / unknown / "status" → the Status sub-page). Selecting the
    /// inner nav item drives <see cref="SettingsNav_SelectionChanged"/>, which
    /// navigates the inner Frame; re-selecting the current item forces a re-nav.
    /// </summary>
    public void NavigateToSubPage(string? subId)
    {
        var tag = SettingsNavigation.SubPageTag(subId);
        if (!_itemsByTag.TryGetValue(tag, out var item))
        {
            return;
        }
        if (ReferenceEquals(SettingsNav.SelectedItem, item))
        {
            NavigateInner(tag);
        }
        else
        {
            SettingsNav.SelectedItem = item;
        }
    }

    private void SettingsNav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem item && item.Tag is string tag)
        {
            NavigateInner(tag);
        }
    }

    private void NavigateInner(string tag)
    {
        FaunaApp.Core.Logs.E2eTrace.Write($"[settings-shell] NavigateInner tag={tag} clientsNull={_clients is null}");
        if (_clients is null)
        {
            return;
        }
        var pageType = SubPageType(tag);
        if (pageType is not null)
        {
            SettingsContentFrame.Navigate(pageType, _clients);
            // Arm the e2e nav-readiness barrier from the just-constructed sub-page's
            // IAsyncLoadedPage.LoadComplete (no-op in production / for pages that don't
            // implement it) so the TestAgent holds ready=false until an async-loading
            // sub-page (e.g. FoldersPage) finishes Page_Loaded. See App.ArmNavLoad.
            App.ArmNavLoad(SettingsContentFrame.Content);
        }
    }

    private void NavBack_Click(object sender, RoutedEventArgs e)
        => MainPage.Current?.LeaveSettings();

    /// <summary>
    /// Map a shell tag (the inner nav item Tag = the <see cref="SettingsNavigation"/>
    /// tag) to its WinUI settings Page type — the WinUI-only half of the nav map.
    /// </summary>
    private static Type? SubPageType(string tag) => tag switch
    {
        SettingsNavigation.Status => typeof(StatusPage),
        SettingsNavigation.Account => typeof(SettingsAccountPage),
        SettingsNavigation.MemberReview => typeof(SettingsMemberReviewPage),
        SettingsNavigation.Privacy => typeof(SettingsPrivacyPage),
        SettingsNavigation.MutedWords => typeof(SettingsMutedWordsPage),
        SettingsNavigation.Personalization => typeof(PersonalizationPage),
        SettingsNavigation.LabelerCatalog => typeof(LabelerCatalogPage),
        SettingsNavigation.General => typeof(SettingsGeneralPage),
        SettingsNavigation.Encryption => typeof(SettingsEncryptionPage),
        SettingsNavigation.Web => typeof(SettingsWebPage),
        SettingsNavigation.Subscriptions => typeof(SettingsSubscriptionsPage),
        SettingsNavigation.Devices => typeof(DevicesPage),
        SettingsNavigation.Folders => typeof(FoldersPage),
        SettingsNavigation.Nostr => typeof(NostrPage),
        SettingsNavigation.Atproto => typeof(AtprotoPage),
        SettingsNavigation.Mail => typeof(SettingsMailPage),
        SettingsNavigation.MailAliases => typeof(SettingsMailAliasesPage),
        SettingsNavigation.MailSpam => typeof(SettingsMailSpamPage),
        SettingsNavigation.MailExport => typeof(SettingsMailExportPage),
        SettingsNavigation.MailImport => typeof(SettingsMailImportPage),
        SettingsNavigation.MailLists => typeof(SettingsMailListsPage),
        SettingsNavigation.MailListMembers => typeof(SettingsMailListMembersPage),
        SettingsNavigation.LinkedNests => typeof(SettingsLinkedNestsPage),
        SettingsNavigation.TaskDelegation => typeof(SettingsTaskDelegationPage),
        SettingsNavigation.ConnectedApps => typeof(ConnectedAppsPage),
        SettingsNavigation.Logs => typeof(SettingsLogsPage),
        _ => null,
    };
}
