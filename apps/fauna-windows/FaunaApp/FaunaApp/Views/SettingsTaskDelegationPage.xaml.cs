using System.Collections.Generic;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Task delegation sub-page (participants.md § Task delegation +
/// § The assignment picker; ui.yaml <c>task-delegation</c>, placed after Nests —
/// settings.md § Navigation model). A dumb renderer of the shared
/// <see cref="TaskDelegationViewModel"/> — every row's <c>PinOptions</c> render
/// VERBATIM (never constructed/filtered here; a pin to a target that can never
/// run the kind would strand it forever). Reference render:
/// <c>apps/fauna-linux/src/settings/task_delegation.rs</c>.
///
/// Rows are built imperatively (not an x:Bind DataTemplate) so each picker's
/// <c>ComboBoxItem</c> can carry a visible localized Content distinct from its
/// <c>AutomationProperties.Name</c> — FlaUI's <c>Select()</c> matches Name to
/// the raw stable key ("automatic"/"this-device") EXACTLY, no normalization
/// (reference_windows_flaui_select_exact_name) — mirroring AdminUsersPage's
/// code-behind tier ComboBox (<c>PopulateTierCombo</c>).
/// </summary>
public sealed partial class SettingsTaskDelegationPage : Page
{
    private TaskDelegationViewModel? _vm;
    private readonly Dictionary<ComboBox, string> _pickerTaskKind = new();
    private bool _initializingPicker;

    public SettingsTaskDelegationPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients && _vm is null)
        {
            _vm = new TaskDelegationViewModel(clients.Rpc!, clients.Account.DeviceId ?? "");
            _vm.PropertyChanged += ViewModel_PropertyChanged;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        await _vm.LoadAsync();
        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        RenderRows();
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        if (e.PropertyName != nameof(TaskDelegationViewModel.ErrorMessage)) return;
        if (_vm.ErrorMessage is not null)
        {
            ErrorBar.Message = _vm.ErrorMessage;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = _vm.ErrorMessage;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    /// <summary>Rebuild every <c>task-delegation-kind-item</c> row from
    /// <see cref="TaskDelegationViewModel.Rows"/> — called after every load and
    /// after every assignment write (the VM reloads on write, so this is the
    /// single render path for both).</summary>
    private void RenderRows()
    {
        if (_vm is null) return;
        RowsContainer.Children.Clear();
        _pickerTaskKind.Clear();
        foreach (var row in _vm.Rows)
        {
            RowsContainer.Children.Add(BuildRow(row));
        }
    }

    /// <summary>Build one <c>task-delegation-kind-item</c> row: a Grid root
    /// (indexed rows are addressed positionally via <c>RowsContainer.Children</c>
    /// order, matching <c>LIVE_TASK_KINDS</c> order) carrying the kind name, the
    /// live runner line, and the assignment picker.</summary>
    private FrameworkElement BuildRow(TaskDelegationRowVm row)
    {
        var grid = new Grid
        {
            Padding = new Thickness(12),
            RowSpacing = 6,
            BorderBrush = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(6),
        };
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        AutomationProperties.SetAutomationId(grid, Ids.TaskDelegationKindItem);
        AutomationProperties.SetName(grid, row.Name);

        var name = new TextBlock { Text = row.Name, Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"] };
        AutomationProperties.SetAutomationId(name, Ids.TaskDelegationKindName);
        Grid.SetRow(name, 0);
        grid.Children.Add(name);

        var runner = new TextBlock { Text = row.RunnerText, Opacity = 0.7 };
        AutomationProperties.SetAutomationId(runner, Ids.TaskDelegationKindRunner);
        Grid.SetRow(runner, 1);
        grid.Children.Add(runner);

        var picker = BuildAssignmentPicker(row, _vm?.Labels ?? new Dictionary<string, string>());
        Grid.SetRow(picker, 2);
        grid.Children.Add(picker);

        return grid;
    }

    /// <summary>Build the assignment picker: one <c>ComboBoxItem</c> per
    /// <c>row.PinOptions</c>, rendered VERBATIM in the shared layer's order.
    /// Content is the localized label the user reads; the item's
    /// AutomationProperties.Name is the raw stable key
    /// (<see cref="TaskDelegationRowVm.OptionKey"/>) — the two must differ so
    /// FlaUI's exact-Name <c>Select()</c> can target an option a locale renders
    /// differently. Selection is set BEFORE the change handler attaches, so the
    /// initial programmatic selection doesn't fire a spurious pin write.</summary>
    private ComboBox BuildAssignmentPicker(TaskDelegationRowVm row, Dictionary<string, string> labels)
    {
        var picker = new ComboBox();
        AutomationProperties.SetAutomationId(picker, Ids.TaskDelegationAssignmentPicker);

        _initializingPicker = true;
        try
        {
            var selectedIndex = -1;
            for (var i = 0; i < row.PinOptions.Count; i++)
            {
                var option = row.PinOptions[i];
                var item = new ComboBoxItem
                {
                    Content = TaskDelegationRowVm.OptionLabel(option, labels),
                    Tag = option,
                };
                AutomationProperties.SetName(item, TaskDelegationRowVm.OptionKey(option));
                picker.Items.Add(item);
                if (Equals(option, row.Assignment)) selectedIndex = i;
            }
            picker.SelectedIndex = selectedIndex;
        }
        finally
        {
            _initializingPicker = false;
        }

        _pickerTaskKind[picker] = row.TaskKind;
        picker.SelectionChanged += AssignmentPicker_SelectionChanged;
        return picker;
    }

    private async void AssignmentPicker_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_initializingPicker || _vm is null) return;
        if (sender is not ComboBox { SelectedItem: ComboBoxItem { Tag: FfiPinOption option } } combo) return;
        if (!_pickerTaskKind.TryGetValue(combo, out var taskKind)) return;
        await _vm.SetAssignmentAsync(taskKind, option);
        RenderRows();
    }
}
