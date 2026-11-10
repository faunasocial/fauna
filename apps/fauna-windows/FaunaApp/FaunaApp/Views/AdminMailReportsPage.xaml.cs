using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using uniffi.fauna_client_mail_settings;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Admin page: DMARC aggregate-report summary (<c>admin-mail-reports</c>,
/// dmarc-reporting.md § Admin pane summary surface). A dumb renderer of the shared
/// <c>DmarcReportsMachine</c> (libs/fauna-client-mail-settings, exposed over UniFFI
/// by libs/fauna-ffi/src/mail_admin.rs <c>build_dmarc_reports_machine</c>) through
/// the testable <see cref="AdminMailReportsViewModel"/> (FaunaApp.Core). The page
/// builds the machine over the shared, already-connected WS-RPC client (the
/// <see cref="INestRpcClient"/> seam), hands it to the VM, and binds the VM's
/// state: the VM owns the wording, the period format and the gesture → action map.
/// No <c>ConfigureAwait(false)</c> here or in the VM (off-thread bound-state
/// mutation throws a silent COMException). Prior art: <see cref="AdminAliasesPage"/>.
/// </summary>
public sealed partial class AdminMailReportsPage : Page
{
    private ServiceClients? _clients;
    private AdminMailReportsViewModel? _vm;

    // True while the VM's options/selection are being painted into the two
    // ComboBoxes, so the programmatic SelectionChanged is not read as a gesture.
    private bool _rendering;

    public AdminMailReportsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_clients is null) return;
        try
        {
            if (_vm is null)
            {
                await EnsureViewModelAsync();
            }
            await _vm!.LoadCommand.ExecuteAsync(null);
        }
        catch (Exception ex)
        {
            RenderError(S.Error(ex));
        }
    }

    /// <summary>Build the machine over the shared client and wrap it in the VM
    /// (a transient WS blip recovers instead of pinning the page in an error).</summary>
    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        var machine = await _clients.Rpc.BuildDmarcReportsMachineAsync();
        _vm = new AdminMailReportsViewModel(machine, FaunaClientMailSettingsMethods.DmarcReportWindowOptions());
        _vm.PropertyChanged += ViewModel_PropertyChanged;
        _vm.WindowOptions.CollectionChanged += (_, _) => RenderPickers();
        _vm.DomainOptions.CollectionChanged += (_, _) => RenderPickers();
        SourcesList.ItemsSource = _vm.Sources;
        SendersList.ItemsSource = _vm.Senders;
        ReportsList.ItemsSource = _vm.Reports;
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminMailReportsViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminMailReportsViewModel.Error):
                RenderError(_vm.Error);
                break;
            case nameof(AdminMailReportsViewModel.SelectedWindow):
            case nameof(AdminMailReportsViewModel.SelectedDomain):
                RenderPickers();
                break;
            default:
                RenderSummary();
                break;
        }
    }

    /// <summary>Paint the totals, the truncated / empty notes, the lists and the raw view.
    /// All of it waits on <c>Loaded</c>, so an unanswered page and an empty one stay apart.</summary>
    private void RenderSummary()
    {
        if (_vm is null) return;
        SummaryPanel.Visibility = _vm.Loaded ? Visibility.Visible : Visibility.Collapsed;
        ListsPanel.Visibility = _vm.Loaded ? Visibility.Visible : Visibility.Collapsed;
        MessagesText.Text = _vm.MessagesText;
        AlignmentText.Text = _vm.AlignmentText;
        DispositionsText.Text = _vm.DispositionsText;
        TruncatedText.Visibility = _vm.Truncated ? Visibility.Visible : Visibility.Collapsed;
        EmptyText.Visibility = _vm.Empty ? Visibility.Visible : Visibility.Collapsed;
        var open = !string.IsNullOrEmpty(_vm.RawXml);
        RawPanel.Visibility = open ? Visibility.Visible : Visibility.Collapsed;
        RawText.Text = _vm.RawXml ?? string.Empty;
    }

    /// <summary>Paint both pickers from the VM. Each option paints its label and is
    /// keyed by its RAW value, the day count or the domain (<see cref="PickerOption"/>):
    /// the FlaUI bridge matches a value by AutomationId first and reads the selected
    /// option's key back — the every-domain option (raw value "") carries no id, so
    /// <c>select("")</c> lands on it as the only item with an empty id. This runs on
    /// every option-collection change and after every window or domain pick, so it
    /// reconciles, never rebuilds by default: a pick's own repaint tearing down the
    /// items under the automation client killed the app (<see cref="PickerOption.Reconcile"/>).</summary>
    private void RenderPickers()
    {
        if (_vm is null) return;
        _rendering = true;
        try
        {
            PickerOption.Reconcile(WindowSelect, Options(_vm.WindowOptions), _vm.SelectedWindow);
            PickerOption.Reconcile(DomainSelect, Options(_vm.DomainOptions), _vm.SelectedDomain);
        }
        finally
        {
            _rendering = false;
        }
    }

    private static List<(string Label, string Key)> Options(IEnumerable<MailReportsOption> options) =>
        options.Select(o => (o.Label, o.Value)).ToList();

    private async void WindowSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_rendering || _vm is null) return;
        if (WindowSelect.SelectedItem is ComboBoxItem { Tag: string tag } && uint.TryParse(tag, out var days)
            && tag != _vm.SelectedWindow)
        {
            await _vm.SetWindowAsync(days);
        }
    }

    private async void DomainSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_rendering || _vm is null) return;
        if (DomainSelect.SelectedItem is ComboBoxItem { Tag: string tag } && tag != _vm.SelectedDomain)
        {
            await _vm.SetDomainAsync(tag);
        }
    }

    private async void Refresh_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.RefreshAsync();
    }

    private void ViewReportButton_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is Button btn) btn.Content = S.Get("admin/mail_reports_page/view");
    }

    private async void ViewReport_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button { DataContext: MailReportRowVm row }) return;
        await _vm.OpenReportAsync(row);
    }

    /// <summary>Render the VM's error onto the page's error line and the app-wide
    /// error surface (the state-protocol <c>error-message</c> read).</summary>
    private void RenderError(string? msg)
    {
        var empty = string.IsNullOrEmpty(msg);
        ActionError.Text = msg ?? string.Empty;
        ActionError.Visibility = empty ? Visibility.Collapsed : Visibility.Visible;
        App.CurrentErrorMessage = empty ? null : msg;
    }
}
