using System;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;

namespace FaunaApp.Views;

/// <summary>
/// The admin <c>admin-custody-hosting</c> page — a dumb renderer of the
/// testable <see cref="AdminCustodyHostingViewModel"/>. Mirrors linux's
/// <c>build_custody_hosting_page</c>, tui's lead
/// (<c>apps/fauna-tui/src/admin/custody_hosting.rs</c>), and apple's
/// <c>AdminCustodyHostingView.swift</c>.
///
/// <para>The <c>(host, grant)</c> key of the row whose remove confirm is
/// armed is page-local UI state (<see cref="_armedKey"/>), never per-row VM
/// state — mirrors apple's single page-local <c>armedKey</c> (not per-row
/// indexed state): arming a new row silently retargets it, and only the
/// currently-armed row's own remove button disappears in favor of the
/// confirm box.</para>
/// </summary>
public sealed partial class AdminCustodyHostingPage : Page
{
    private ServiceClients? _clients;
    private AdminCustodyHostingViewModel? _vm;
    private string? _armedKey;

    public AdminCustodyHostingPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
            _vm ??= new AdminCustodyHostingViewModel(clients.Rpc!);
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _vm.LoadAsync();
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
        RenderState();
    }

    // ── Render ──

    /// <summary>x:Bind converter — WinUI has no built-in bool→Visibility
    /// binding. Qualified as <c>local:AdminCustodyHostingPage.VisibleWhenArmed</c>
    /// at the call site: an unqualified static x:Bind method fails the XAML
    /// compiler with CS0176.</summary>
    public static Visibility VisibleWhenArmed(bool armed) =>
        armed ? Visibility.Visible : Visibility.Collapsed;

    public static Visibility VisibleWhenNotArmed(bool armed) =>
        armed ? Visibility.Collapsed : Visibility.Visible;

    private void RenderState()
    {
        if (_vm is null) return;

        // Nothing paints here until the first RenderState() call, which only
        // ever runs after LoadAsync completes -- the pre-hydrate honesty
        // property (neither count nor empty state before the read answers)
        // holds by construction, not by an explicit flag check.
        var hasRows = _vm.Rows.Count > 0;
        CountText.Visibility = Visibility.Visible;
        CountText.Text = Strings.Format("admin/custody_hosting/count", _vm.Rows.Count);
        EmptyText.Visibility = hasRows ? Visibility.Collapsed : Visibility.Visible;

        HostingList.ItemsSource = _vm.Rows
            .Select(r => new AdminHostingRowPresentation(r, r.Key == _armedKey))
            .ToList();

        if (string.IsNullOrEmpty(_vm.Status))
        {
            StatusText.Visibility = Visibility.Collapsed;
            StatusText.Text = string.Empty;
        }
        else
        {
            StatusText.Text = _vm.Status;
            StatusText.Visibility = Visibility.Visible;
        }

        ShowError(_vm.ErrorMessage);
    }

    private void Remove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not AdminHostingRowVm row) return;
        // A row whose confirm is already armed cannot re-arm from a DIFFERENT
        // gesture path here -- this handler only fires from the (now-hidden)
        // remove button of an unarmed row, so a plain retarget is always safe.
        _armedKey = row.Key;
        RenderState();
    }

    private void RemoveCancel_Click(object sender, RoutedEventArgs e)
    {
        _armedKey = null;
        RenderState();
    }

    private async void RemoveConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not AdminHostingRowVm row) return;
        // Disarm BEFORE dispatch, synchronous, before the confirm's own first
        // await -- mirrors AdminNestPage's seed-rotate confirm.
        _armedKey = null;
        await _vm.RemoveAsync(row.HostActorId, row.GrantId);
        RenderState();
    }

    /// <summary>Declares the confirm button's offline gate the first time each
    /// per-row instance is realized (<c>fauna.admin.custody_hosting.remove</c>,
    /// OnlineOnly per <c>offline_class.rs</c>) — a DataTemplate button has no
    /// single construction site to declare it at, unlike a page's named
    /// controls (mirrors AdminNestPage's <c>SeedRotateConfirmButton.FaunaGate</c>,
    /// deferred to per-instance <c>Loaded</c> since this button is realized
    /// once per armed row, not once per page).</summary>
    private void RemoveConfirmButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.FaunaGate("fauna.admin.custody_hosting.remove");
    }

    private void ShowError(string? message)
    {
        if (string.IsNullOrEmpty(message))
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        else
        {
            ErrorBar.Message = message;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = message;
        }
    }
}

/// <summary>One <see cref="HostingList"/> item — the shared
/// <see cref="AdminHostingRowVm"/> plus this page's page-local armed-confirm
/// state, rebuilt fresh on every <see cref="AdminCustodyHostingPage.RenderState"/>
/// call (mirrors <c>SettingsMemberReviewPage</c>'s <c>ReviewList.ItemsSource</c>
/// rebuild).</summary>
public sealed record AdminHostingRowPresentation(AdminHostingRowVm Row, bool Armed);
