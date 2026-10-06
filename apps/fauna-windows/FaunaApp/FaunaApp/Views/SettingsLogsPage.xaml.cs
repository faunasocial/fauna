using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Logs;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;

namespace FaunaApp.Views;

/// <summary>
/// The Settings → Logs sub-page (observability.md § Surfaces) — a dumb renderer of
/// the shared <see cref="LogsViewModel"/> (FaunaApp.Core), which reads the
/// process-global <c>fauna_log</c> ring over the <see cref="ILogRing"/> seam
/// (<see cref="FfiLogRing"/> → <c>FaunaFfiMethods.LogSnapshot</c>). No nest handle —
/// the ring is a process global the page self-wires; the shell still passes
/// <c>ServiceClients</c> uniformly, which this page ignores. The severity filter,
/// copy, and clear delegate to the VM; the row shape lives in <see cref="LogsFormat"/>
/// (shared with the admin Logs page). Mirrors linux <c>settings/logs.rs</c>.
/// </summary>
public sealed partial class SettingsLogsPage : Page
{
    private LogsViewModel? _vm;

    public SettingsLogsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        // Self-wiring off the process-global ring; the FfiLogRing wraps the UniFFI
        // global log fns the linux install_logging fills.
        _vm ??= new LogsViewModel(new FfiLogRing());
    }

    private void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        LogList.ItemsSource = _vm.Entries;
        _vm.Load();
        RenderError(_vm.Error);
        UpdateEmpty();
    }

    private void FilterCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        // Fires during XAML init (SelectedIndex=0) before the VM exists — guard it.
        if (_vm is null) return;
        _vm.SetFilter(FilterCombo.SelectedIndex);
        UpdateEmpty();
    }

    private void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        ClipboardHelper.CopyText(_vm.CopyText());
    }

    private void ClearButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.Clear();
        UpdateEmpty();
    }

    private void UpdateEmpty()
    {
        if (_vm is null) return;
        EmptyPlaceholder.Visibility = _vm.Entries.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void RenderError(string? msg)
    {
        if (string.IsNullOrEmpty(msg))
        {
            ErrorBar.IsOpen = false;
        }
        else
        {
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
        }
    }
}
