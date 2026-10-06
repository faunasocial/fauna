using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

// internal because ServiceClients is internal (it carries the internal
// fauna-launch-machine LaunchMachine). Only used within the FaunaApp shell.
// InitialRsvpStatus carries the caller's RSVP from the agenda snapshot so the
// detail surface can show it on load — the WS-RPC `events.get` reply has no
// per-caller RSVP (Event.status is the ICS status, not a per-actor RSVP).
internal record EventDetailNavigationArgs(ServiceClients Clients, string EventId, string? InitialRsvpStatus = null);

public sealed partial class EventDetailPage : Page
{
    private EventDetailViewModel? _viewModel;
    private string? _eventId;

    public EventDetailPage()
    {
        this.InitializeComponent();
        InviteActorIdBox.PlaceholderText = S.Get("events/invite/email_placeholder");
        // Reminder presets from the shared catalog (events.md § Where logic lives)
        // instead of hard-coded English ComboBoxItems.
        foreach (var preset in FaunaFfiMethods.ReminderPresets())
        {
            var item = new ComboBoxItem { Content = S.Resolve(preset.label), Tag = preset.value };
            AutomationProperties.SetName(item, preset.value);
            ReminderSelect.Items.Add(item);
        }
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is EventDetailNavigationArgs args)
        {
            _viewModel = new EventDetailViewModel(args.Clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            _eventId = args.EventId;
            // Seed the caller's RSVP from the agenda snapshot; LoadAsync doesn't
            // overwrite it (events.get carries no per-caller RSVP).
            _viewModel.RsvpStatus = args.InitialRsvpStatus;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || _eventId is null) return;
        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;

        await _viewModel.LoadCommand.ExecuteAsync(_eventId);

        SummaryText.Text = _viewModel.Summary;
        TimeText.Text = _viewModel.TimeRange;
        DtStartText.Text = _viewModel.DtStart;
        DtEndText.Text = _viewModel.DtEnd;
        LocationText.Text = _viewModel.Location ?? string.Empty;
        DescriptionText.Text = _viewModel.Description ?? string.Empty;
        RsvpStatusText.Text = _viewModel.RsvpStatus is not null ? $"Your RSVP: {_viewModel.RsvpStatus}" : "";
        AttendeesList.ItemsSource = _viewModel.Attendees;
        // Author-only affordances: only the organizer may delete the event or
        // invite attendees. Non-organizers (invitees) still see the RSVP buttons.
        // A Collapsed control has no UIA peer, so the affordance is simply absent
        // for non-organizers — matches android (EventDetailScreen gates delete +
        // invite on organizedByMe).
        var authorOnly = _viewModel.OrganizedByMe ? Visibility.Visible : Visibility.Collapsed;
        DeleteButton.Visibility = authorOnly;
        InviteRow.Visibility = authorOnly;
        UpdateReminderUi();

        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
    }

    /// <summary>Flip the reminder section between the preset-select+Set view and
    /// the current-label+Remove view based on whether a reminder is set.</summary>
    private void UpdateReminderUi()
    {
        if (_viewModel is null) return;
        var has = _viewModel.HasReminder;
        ReminderSetPanel.Visibility = has ? Visibility.Collapsed : Visibility.Visible;
        ReminderCurrentPanel.Visibility = has ? Visibility.Visible : Visibility.Collapsed;
        ReminderCurrentText.Text = _viewModel.ReminderCurrentLabel;
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;
        switch (e.PropertyName)
        {
            case nameof(EventDetailViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null) { ErrorBar.Message = _viewModel.ErrorMessage; ErrorBar.IsOpen = true; App.CurrentErrorMessage = _viewModel.ErrorMessage; }
                else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }
                break;
            case nameof(EventDetailViewModel.RsvpStatus):
                RsvpStatusText.Text = _viewModel.RsvpStatus is not null ? $"Your RSVP: {_viewModel.RsvpStatus}" : "";
                break;
            case nameof(EventDetailViewModel.ReminderOffset):
                UpdateReminderUi();
                break;
        }
    }

    private void BackButton_Click(object sender, RoutedEventArgs e)
    {
        if (Frame.CanGoBack) Frame.GoBack();
    }

    private async void DeleteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.DeleteCommand.ExecuteAsync(null);
        // GoBack so the agenda reloads (EventsPage.Page_Loaded re-runs the
        // list query) and the deleted card disappears. Stay on the detail
        // page if the delete reported an error.
        if (_viewModel.ErrorMessage is null && Frame.CanGoBack) Frame.GoBack();
    }

    private async void RsvpGoing_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.RsvpCommand.ExecuteAsync("going");
    }

    private async void RsvpInterested_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.RsvpCommand.ExecuteAsync("interested");
    }

    private async void RsvpDecline_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is not null) await _viewModel.RsvpCommand.ExecuteAsync("declined");
    }

    private async void InviteButton_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || string.IsNullOrWhiteSpace(InviteActorIdBox.Text)) return;
        await _viewModel.InviteCommand.ExecuteAsync(InviteActorIdBox.Text.Trim());
        InviteActorIdBox.Text = string.Empty;
    }

    private async void SetReminder_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        // The selected preset carries its ICS offset in Tag (Content is the human
        // label). No selection → nothing to set.
        var offset = (ReminderSelect.SelectedItem as ComboBoxItem)?.Tag as string;
        if (string.IsNullOrEmpty(offset)) return;
        await _viewModel.SetReminderCommand.ExecuteAsync(offset);
    }

    private async void RemoveReminder_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.RemoveReminderCommand.ExecuteAsync(null);
    }
}

/// <summary>
/// Converts an attendee RSVP hex color string (from <see cref="AttendeeInfo.RsvpHexColor"/>)
/// to a <see cref="Microsoft.UI.Xaml.Media.SolidColorBrush"/> for the attendee-status
/// foreground. Mirrors the android/web canonical RSVP color map (events.md
/// § Attendee list presentation, 2026-06-23).
/// </summary>
public sealed class AttendeeRsvpColorConverter : Microsoft.UI.Xaml.Data.IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is string hex && hex.StartsWith("#") && hex.Length >= 7)
        {
            var r = System.Convert.ToByte(hex.Substring(1, 2), 16);
            var g = System.Convert.ToByte(hex.Substring(3, 2), 16);
            var b = System.Convert.ToByte(hex.Substring(5, 2), 16);
            return new Microsoft.UI.Xaml.Media.SolidColorBrush(
                Windows.UI.Color.FromArgb(255, r, g, b));
        }
        return new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Gray);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}
