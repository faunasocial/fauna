using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

public partial class NotificationsViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private long _unreadCount;
    [ObservableProperty] private bool _hasMore;

    public ObservableCollection<NotificationItem> Notifications { get; } = new();

    private readonly INestRpcClient _rpc;
    private readonly Func<FfiNotificationDestination, bool> _canNavigate;
    private long? _cursor;

    /// <param name="canNavigate">Whether this app can act on a decided destination
    /// right now (the `Family` target is only reachable while the gated family tab is
    /// shown). Null = every destination is navigable.</param>
    internal NotificationsViewModel(INestRpcClient rpc,
        Func<FfiNotificationDestination, bool>? canNavigate = null)
    {
        _rpc = rpc;
        _canNavigate = canNavigate ?? (_ => true);
        // Re-fetch notifications on every WS reconnect (the linux ResyncRequired
        // sweep set; transport.md § Push events).
        RefreshOnReconnect(rpc, LoadCommand);
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        _cursor = null;
        Notifications.Clear();
        try
        {
            await FetchPageAsync();
            await LoadCountAsync();
        }
        catch (Exception ex) { ShowError(ex); }
        finally { IsLoading = false; }
    }

    [RelayCommand]
    private async Task LoadMoreAsync()
    {
        if (!HasMore) return;
        ErrorMessage = null;
        try { await FetchPageAsync(); }
        catch (Exception ex) { ShowError(ex); }
    }

    private async Task FetchPageAsync()
    {
        var reply = await _rpc.NotificationsListAsync(_cursor, 25);
        foreach (var n in reply.notifications)
        {
            // The shared decision (`notifications.md` § Localized body): the
            // catalog sentence for a `.Localized` key this build's RESW carries,
            // resolved through `Strings.Resolve` (never `ResolveNested` — a
            // notification's args are relayed data, never translatable keys),
            // else the nest's own English `.Verbatim` text as-is.
            var text = FaunaFfiMethods.NotificationTextFor(n) switch
            {
                FfiNotificationText.Localized l => Strings.Resolve(l.text),
                FfiNotificationText.Verbatim v => v.text,
                _ => n.summary,
            };
            // The shared destination router (`notifications.md` § Deep-link
            // destinations): shared Rust decides, the app only navigates. Decided
            // here, while the raw `FfiNotifItem` (source / contentId / senderId) is
            // still in hand — `NotificationItem` keeps none of them. A destination
            // this app cannot act on right now (`_canNavigate`) paints an inert row.
            var destination = FaunaFfiMethods.NotificationDestinationFor(n);
            var navigable = destination is not null && _canNavigate(destination);
            Notifications.Add(new NotificationItem(n.id, n.notifType, text, n.isRead, n.createdAt,
                navigable ? destination : null));
        }

        // A full page (== the requested limit) implies more rows behind it.
        HasMore = reply.notifications.Length >= 25;
        if (reply.cursor is { } cursor)
            _cursor = cursor;
    }

    [RelayCommand]
    private async Task MarkReadAsync()
    {
        ErrorMessage = null;
        try
        {
            await _rpc.NotificationsMarkReadAsync(null);
            UnreadCount = 0;
        }
        catch (Exception ex) { ShowError(ex); }
    }

    public async Task LoadCountAsync()
    {
        try
        {
            UnreadCount = await _rpc.NotificationsCountAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Debug("NotificationsViewModel",
                $"notifications count fetch failed: {ex.Message}");
        }
    }
}

public class NotificationItem
{
    // Internal: the generated FFI destination type is internal.
    internal NotificationItem(long id, string type, string text, bool isRead, long createdAt,
        FfiNotificationDestination? destination = null)
    {
        Id = id;
        Type = type;
        Text = text;
        IsRead = isRead;
        CreatedAt = createdAt;
        Destination = destination;
    }

    /// <summary>Where a tap goes, as decided by shared Rust and filtered by what this
    /// app can reach right now; null = the row is inert plain text.</summary>
    internal FfiNotificationDestination? Destination { get; }
    public bool IsNavigable => Destination is not null;
    public long Id { get; }
    public string Type { get; }
    /// <summary>The decided display text (`notification_text_for`'s answer,
    /// already resolved) — never the nest's raw English `summary`.</summary>
    public string Text { get; }
    public bool IsRead { get; set; }
    public long CreatedAt { get; }
    public string TypeIcon => Type switch
    {
        "message" => "\uE715",
        "knock" => "\uE77B",
        "post" => "\uE8F2",
        "event" => "\uE787",
        _ => "\uE7E7",
    };
    // created_at is epoch MICROSECONDS (÷1000 → ms); shared Rust buckets it and
    // supplies the localized strings via ValueFormat.RelativeTime.
    public string TimeAgo => ValueFormat.RelativeTime(
        DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), CreatedAt / 1000);
}
