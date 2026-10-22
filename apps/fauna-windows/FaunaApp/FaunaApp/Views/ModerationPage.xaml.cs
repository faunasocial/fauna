using System.Collections.Specialized;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// The standalone Moderation page: the caller's moderation queue + per-item training
/// correction (moderation.md § Goal). The page is queue-only — matching linux/macOS/
/// iOS/Android and ui.yaml's <c>moderation</c> scope (<c>moderation-tab</c> +
/// the <c>moderation-queue</c> component). Spam-filtering <b>preferences</b> live on
/// Settings → Privacy (<see cref="SettingsPrivacyPage"/>, the canonical
/// <c>spam-moderation-controls</c> home); they are not duplicated here.
/// </summary>
public sealed partial class ModerationPage : Page
{
    private ModerationViewModel? _viewModel;

    public ModerationPage()
    {
        this.InitializeComponent();
        ModerationTitle.Text = S.Get("settings/moderation_page/title");
        EnforcementHeader.Text = S.Get("moderation/enforcement_title");
        NoActionsText.Text = S.Get("moderation/no_actions");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            // The queue is the union of the server obligation rows and the
            // conversations session's post-decrypt local detections; pass the session
            // (the same one ConversationsPage renders off) as the local-detection seam.
            // Null when no MLS session → the server rows alone (moderation.md § Layout).
            // The third seam is the sealed spam-model client-write path (1d — mail-spam.md
            // § Encrypted-mode interaction): a train correction writes the re-sealed model
            // client-side when the nest advertises spam-model-sealed-at-rest, else degrades
            // to fauna.moderation.train.
            _viewModel = new ModerationViewModel(
                clients.Rpc!,
                clients.ConvSession is { } session ? new SessionLocalDetections(session) : null,
                new MachineSpamModelClientWrite(clients.Rpc!));
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            ActionsList.ItemsSource = _viewModel.Actions;
            // The empty-state placeholder tracks the queue's row count, including
            // train-driven changes (RefreshQueueAsync mutates Actions in place).
            _viewModel.Actions.CollectionChanged += Actions_CollectionChanged;
            // The reporter's own ledger beside the queue (moderation.md §
            // User-initiated reporting → What the reporter is told); its empty line
            // tracks the rows AND the VM's loaded bit.
            ReportsHeader.Text = _viewModel.ReportsTitle;
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(ReportsSection, _viewModel.ReportsTitle);
            ReportsList.ItemsSource = _viewModel.Reports;
            _viewModel.Reports.CollectionChanged += (_, _) => UpdateReportsState();
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.LoadCommand.ExecuteAsync(null);
        UpdateEmptyState();
    }

    private void Actions_CollectionChanged(object? sender, NotifyCollectionChangedEventArgs e)
        => UpdateEmptyState();

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;
        switch (e.PropertyName)
        {
            case nameof(ModerationViewModel.IsLoading):
                LoadingRing.IsActive = _viewModel.IsLoading;
                LoadingRing.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(ModerationViewModel.AppealOpen):
            case nameof(ModerationViewModel.AppealReason):
            case nameof(ModerationViewModel.AppealCanSubmit):
            case nameof(ModerationViewModel.AppealBlockedReason):
            case nameof(ModerationViewModel.AppealSubmitLabel):
            case nameof(ModerationViewModel.AppealSummary):
            case nameof(ModerationViewModel.AppealStatus):
                RenderAppeal();
                break;
            case nameof(ModerationViewModel.ReportsLoaded):
                UpdateReportsState();
                break;
            case nameof(ModerationViewModel.ReportWithdrawStatus):
                ReportWithdrawStatusText.Text = _viewModel.ReportWithdrawStatus ?? "";
                ReportWithdrawStatusText.Visibility = string.IsNullOrEmpty(_viewModel.ReportWithdrawStatus)
                    ? Visibility.Collapsed : Visibility.Visible;
                break;
            case nameof(ModerationViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null)
                {
                    ErrorBar.Message = _viewModel.ErrorMessage;
                    ErrorBar.IsOpen = true;
                    App.CurrentErrorMessage = _viewModel.ErrorMessage;
                }
                else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }
                break;
        }
    }

    // The `moderation-queue` ListView stays visible even when empty (the e2e scope
    // anchor + empty-state contract); the NoActionsText placeholder overlays it when
    // there are no rows (moderation.md § Errors & edge cases — an empty queue is not
    // an error).
    private void UpdateEmptyState()
    {
        if (_viewModel is null) return;
        NoActionsText.Visibility = _viewModel.Actions.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    // The ledger's empty line paints only off a landed read AND zero rows — a read in
    // flight (or one that failed) never reads as "you have not reported anything".
    private void UpdateReportsState()
    {
        if (_viewModel is null) return;
        var empty = _viewModel.ReportsLoaded && _viewModel.Reports.Count == 0;
        ReportsEmptyText.Text = empty ? _viewModel.ReportsEmptyText : "";
        ReportsEmptyText.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
    }

    private async void WithdrawReport_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button { Tag: ReportLedgerRowView row }) return;
        await _viewModel.WithdrawReportCommand.ExecuteAsync(row);
    }

    // ── Appeal (moderation.md § Legal takedown) ──────────────────────────────

    /// <summary>appeal-button → open the form on this row; the VM captures the row's
    /// content id now, not at submit.</summary>
    private void Appeal_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button { Tag: ModerationRow row }) return;
        _viewModel.OpenAppeal(row);
    }

    private void AppealReasonInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.AppealReason = AppealReasonInput.Text;
    }

    private async void AppealSubmit_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.SubmitAppealAsync();
    }

    private void AppealCancel_Click(object sender, RoutedEventArgs e)
        => _viewModel?.CancelAppeal();

    /// <summary>Paint the form from the VM: present only while open; the submit
    /// control's label and enabled state are the shared fold's verbatim; the status
    /// line shows only once an attempt (or its cancel) has left one.</summary>
    private void RenderAppeal()
    {
        if (_viewModel is null) return;
        AppealForm.Visibility = _viewModel.AppealOpen ? Visibility.Visible : Visibility.Collapsed;
        AppealSummaryText.Text = _viewModel.AppealSummary;
        if (AppealReasonInput.Text != _viewModel.AppealReason)
            AppealReasonInput.Text = _viewModel.AppealReason;
        AppealBlockedText.Text = _viewModel.AppealBlockedReason ?? string.Empty;
        AppealBlockedText.Visibility = _viewModel.AppealBlockedReason is null
            ? Visibility.Collapsed : Visibility.Visible;
        AppealSubmitButton.Content = _viewModel.AppealSubmitLabel;
        AppealSubmitButton.IsEnabled = _viewModel.AppealCanSubmit;
        AppealStatusText.Text = _viewModel.AppealStatus ?? string.Empty;
        AppealStatusText.Visibility = _viewModel.AppealStatus is null
            ? Visibility.Collapsed : Visibility.Visible;
    }

    private async void TrainCorrection_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not ModerationRow action) return;
        await _viewModel.TrainCommand.ExecuteAsync(action);
    }
}
