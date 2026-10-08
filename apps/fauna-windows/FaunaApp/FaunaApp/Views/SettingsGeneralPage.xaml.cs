using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views;

/// <summary>
/// Settings → General sub-page (settings.md § Navigation model). Startup
/// (auto-start), Configure-nest, Updates, Storage, About. Split out of the former
/// single-scroll <see cref="SettingsPage"/>; constructs its own
/// <see cref="SettingsViewModel"/> from <see cref="ServiceClients"/> in
/// OnNavigatedTo, mirroring the admin sub-pages. The Storage bar reuses the
/// VM's quota-derived StoragePercent (the same fetch the Status sub-page shows
/// in the quota section). The About block paints the process-wide
/// <see cref="UpdateCheck"/> (the version, the asked check, the newer-version
/// notice), which outlives the page: the once-per-sign-in look may have painted
/// the notice before the page was ever opened.
/// </summary>
public sealed partial class SettingsGeneralPage : Page
{
    private SettingsViewModel? _viewModel;
    private readonly UpdateCheck _updates = UpdateCheck.Shared;

    public SettingsGeneralPage()
    {
        this.InitializeComponent();
        AppVersionText.Text = _updates.RunningVersion;
        Unloaded += (_, _) => _updates.PropertyChanged -= UpdateCheck_PropertyChanged;
    }

    // The sign-in look can land from the e2e agent's command thread, so every
    // repaint is marshalled onto this page's own dispatcher.
    private void UpdateCheck_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e) =>
        DispatcherQueue.TryEnqueue(PaintUpdateCheck);

    private void PaintUpdateCheck()
    {
        CheckUpdateButton.Content = _updates.ButtonLabel;
        UpdateNoticeText.Text = _updates.Notice ?? string.Empty;
        UpdateNoticeText.Visibility = _updates.Notice is null ? Visibility.Collapsed : Visibility.Visible;
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _viewModel = new SettingsViewModel(clients.Nest, clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        PaintUpdateCheck();
        _updates.PropertyChanged -= UpdateCheck_PropertyChanged;
        _updates.PropertyChanged += UpdateCheck_PropertyChanged;

        if (_viewModel is null) return;

        await _viewModel.LoadCommand.ExecuteAsync(null);

        NewNestUrlBox.Text = _viewModel.NewNestUrl;
        AutoStartToggle.IsOn = _viewModel.IsAutoStartEnabled;
        CloseToTrayToggle.IsOn = _viewModel.IsCloseToTrayEnabled;
        // HelpText carries the literal "true"/"false" get_attr(id, "state") reads
        // (reference_windows_flaui_state_attr_helptext — mirrors
        // PersonalizationPage's engagement-toggle wiring; a plain ToggleSwitch's
        // UIA peer exposes no ToggleState of its own, so without this the e2e
        // bridge's `get_attr(..., "state")` reads null for every launch,
        // regardless of the persisted value).
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            CloseToTrayToggle, _viewModel.IsCloseToTrayEnabled ? "true" : "false");
        // Same channel for start-at-login: it read null for every launch until this
        // was wired, so the persisted-choice witness could not even assert its
        // default-ON precondition.
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            AutoStartToggle, _viewModel.IsAutoStartEnabled ? "true" : "false");
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;

        switch (e.PropertyName)
        {
            case nameof(SettingsViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null)
                {
                    ErrorBar.Message = _viewModel.ErrorMessage;
                    ErrorBar.IsOpen = true;
                    App.CurrentErrorMessage = _viewModel.ErrorMessage;
                }
                else
                {
                    ErrorBar.IsOpen = false;
                    App.CurrentErrorMessage = null;
                }
                break;
            case nameof(SettingsViewModel.IsLoading):
                ConfigureProgress.IsActive = _viewModel.IsLoading;
                ConfigureProgress.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                ConfigureButton.IsEnabled = !_viewModel.IsLoading;
                break;
            case nameof(SettingsViewModel.StoragePercent):
                StorageBar.Value = _viewModel.StoragePercent;
                var usedLabel = ValueFormat.ByteSize((ulong)Math.Max(0, _viewModel.StorageUsedBytes));
                var totalLabel = ValueFormat.ByteSize((ulong)Math.Max(0, _viewModel.StorageTotalBytes));
                StorageText.Text = $"{usedLabel} / {totalLabel} ({_viewModel.StoragePercent:F0}%)";
                break;
            case nameof(SettingsViewModel.IsAutoStartEnabled):
                AutoStartToggle.IsOn = _viewModel.IsAutoStartEnabled;
                break;
        }
    }

    private async void ConfigureButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;

        _viewModel.NewNestUrl = NewNestUrlBox.Text;
        await _viewModel.ConfigureCommand.ExecuteAsync(null);
    }

    private async void CheckUpdateButton_Click(object sender, RoutedEventArgs e) =>
        await _updates.CheckAsync();

    private void AutoStartToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null)
            _viewModel.IsAutoStartEnabled = AutoStartToggle.IsOn;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            AutoStartToggle, AutoStartToggle.IsOn ? "true" : "false");
    }

    private void CloseToTrayToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null)
            _viewModel.IsCloseToTrayEnabled = CloseToTrayToggle.IsOn;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(
            CloseToTrayToggle, CloseToTrayToggle.IsOn ? "true" : "false");
    }
}
