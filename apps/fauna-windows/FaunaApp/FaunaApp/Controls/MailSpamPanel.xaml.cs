using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// User-facing "Spam" surface (docs/goal/behavior/mail-spam.md § Reset / § Cold start
/// Path 2 / § Undo): a person manages their own per-account spam classifier — reset
/// the per-user Bayesian model, opt in/out of the deployment-baseline contribution,
/// and review + undo individual training events. A thin view over
/// <see cref="MailSpamViewModel"/> (FaunaApp.Core) — which projects the shared
/// <c>fauna_client_mail_settings::MailSpamMachine</c> through its UniFFI
/// <c>IMailSpamMachine</c> interface (no business logic here, priority #2). Builds the
/// machine over the session's shared, auto-reconnecting WS-RPC connection (the
/// INestRpcClient seam) and hands it to the VM. Hosted by its dedicated Settings shell
/// sub-page <c>SettingsMailSpamPage</c>, which supplies <c>ServiceClients</c> via
/// <c>OnNavigatedTo</c> and surfaces this panel's <see cref="ErrorChanged"/> on its own
/// page-level <c>error-message</c>. The reset two-click confirm is an inline relabel.
/// The VM's render-state is reflected imperatively; only the training-history list is a
/// bound ObservableCollection. Also hosts the distributed report-sharing transparency
/// pane (report-sharing.md § Client wire + transparency surface) — the
/// <c>mail-spam-share-reports-toggle</c> + <c>report-share-published-list</c>, a small
/// dedicated flow riding the same VM but not the machine. Lifts the linux reference
/// apps/fauna-linux/src/settings/mail_spam.rs.
/// </summary>
public sealed partial class MailSpamPanel : UserControl
{
    private ServiceClients? _clients;
    private MailSpamViewModel? _vm;

    // True while the reset button is armed (first click); the second click within the
    // window dispatches.
    private bool _resetArmed;
    // Set while RenderState programmatically updates the contribute toggle, so its
    // Toggled handler doesn't echo the change back as a dispatch.
    private bool _syncing;
    // Same echo-suppression flag for the report-share toggle (report-sharing.md
    // § Client wire — a small dedicated flow alongside the machine, not part of it).
    private bool _rsSyncing;

    /// <summary>Raised with the VM's error message (or null to clear) so the host page
    /// surfaces it through its own page-level <c>error-message</c> element.</summary>
    public event Action<string?>? ErrorChanged;

    public MailSpamPanel()
    {
        this.InitializeComponent();
    }

    /// <summary>Supplied by the host page (SettingsMailSpamPage.OnNavigatedTo) before Loaded
    /// fires — carries the secrets used to build the WS client.</summary>
    internal void Configure(ServiceClients clients) => _clients = clients;

    private async void Panel_Loaded(object sender, RoutedEventArgs e) => await LoadAsync();

    private async Task EnsureVmAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // The spam machine is User-class (the nest derives the owning actor from the
        // authenticated caller); built over the session's shared, auto-reconnecting
        // WS-RPC connection (the INestRpcClient seam) rather than a per-panel one-shot
        // FfiNestClient.Connect() that would surface a transient os-error-10061 as a
        // panel error while shared-client pages recover.
        _vm = new MailSpamViewModel(await _clients.Rpc.BuildMailSpamMachineAsync(), _clients.Rpc);
        HistoryList.ItemsSource = _vm.Events;
        PublishedList.ItemsSource = _vm.PublishedReports;
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;
        LoadingRing.IsActive = true;
        LoadingRing.Visibility = Visibility.Visible;
        try
        {
            await EnsureVmAsync();
            await _vm!.LoadCommand.ExecuteAsync(null);
            RenderState();
        }
        catch (Exception ex)
        {
            ErrorChanged?.Invoke(Strings.Error(ex));
        }
        finally
        {
            LoadingRing.IsActive = false;
            LoadingRing.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Reflect the VM's scalar state into the UI (the history list is a bound
    /// ObservableCollection, so it tracks the VM automatically). Called after the load +
    /// every action.</summary>
    private void RenderState()
    {
        if (_vm is null) return;

        ErrorChanged?.Invoke(string.IsNullOrEmpty(_vm.Error) ? null : _vm.Error);

        // Reflect the persisted contribution flag without echoing it back as a dispatch.
        if (ContributeToggle.IsOn != _vm.ContributeBaseline)
        {
            _syncing = true;
            ContributeToggle.IsOn = _vm.ContributeBaseline;
            _syncing = false;
        }

        // Reflect the persisted report-share opt-in without echoing it back as a dispatch.
        // Mirrored to AutomationProperties.HelpText so the e2e driver reads it via the
        // uniform get_attr(id, "state") idiom (mirrors MailSettingsPanel's serve-here toggle).
        if (ShareReportsToggle.IsOn != _vm.ShareReports)
        {
            _rsSyncing = true;
            ShareReportsToggle.IsOn = _vm.ShareReports;
            _rsSyncing = false;
        }
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            ShareReportsToggle, _vm.ShareReports ? "on" : "off");

        ListEmpty.Visibility = _vm.Events.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        PublishedEmpty.Visibility = _vm.PublishedReports.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        // Reflect the persisted override without clobbering an in-progress edit —
        // only overwrite while the box isn't focused (the report-share toggle's
        // echo-suppression shape, applied to a text field instead of a bool).
        if (ThresholdOverrideInput.FocusState == FocusState.Unfocused)
        {
            ThresholdOverrideInput.Text = _vm.ThresholdOverrideText;
        }
    }

    /// <summary>Reset is destructive + irreversible — arm-then-confirm (no modal, no
    /// separate confirm ID): the first click arms (relabels "Confirm?") and auto-disarms
    /// after 4 s; the second click within the window dispatches. Mirrors the linux
    /// wire_two_click and MailAliasesPanel's per-row confirm.</summary>
    private async void Reset_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        if (!_resetArmed)
        {
            _resetArmed = true;
            var baseLabel = ResetButton.Content;
            ResetButton.Content = "Confirm?";
            await Task.Delay(4000);
            if (_resetArmed)
            {
                _resetArmed = false;
                ResetButton.Content = baseLabel;
            }
            return;
        }

        _resetArmed = false;
        ResetButton.Content = S.Get("mail_spam/reset_button");
        await _vm.ResetModelAsync();
        RenderState();
    }

    private async void ContributeToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetContributeBaselineAsync(sw.IsOn);
        RenderState();
    }

    private async void ShareReportsToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_rsSyncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetShareReportsAsync(sw.IsOn);
        RenderState();
    }

    private async void Undo_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn || btn.Tag is not string historyIdHex || _vm is null) return;
        await _vm.UndoTrainingAsync(historyIdHex);
        RenderState();
    }

    /// <summary>Commits on blur — the e2e driver's "click an editable to commit" idiom
    /// shifts focus to the nearest neighbour rather than injecting a keystroke
    /// (FoldersPage's capInput shape).</summary>
    private async void ThresholdOverrideInput_LostFocus(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.ThresholdOverrideText = ThresholdOverrideInput.Text;
        await _vm.CommitThresholdOverrideAsync();
        RenderState();
    }

    /// <summary>Also commits on Enter, for the user who finishes typing with the
    /// keyboard: Enter doesn't move focus off a single-line TextBox, so the
    /// LostFocus handler above never observes it.</summary>
    private async void ThresholdOverrideInput_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Enter || _vm is null) return;
        _vm.ThresholdOverrideText = ThresholdOverrideInput.Text;
        await _vm.CommitThresholdOverrideAsync();
        RenderState();
    }
}
