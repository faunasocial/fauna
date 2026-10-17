using System;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Shared base for the page ViewModels that surface a user-facing error banner. Owns
/// the bound <c>ErrorMessage</c> (each page's <c>ErrorBar</c> InfoBar still paints from
/// it, unchanged) and funnels every error through <see cref="SetError"/> /
/// <see cref="ShowError"/> so the message is also recorded in the shared
/// <c>fauna_log</c> ring (observability.md § What must be logged, category 1).
///
/// <para>The banner is a <i>reactive</i> render (the InfoBar re-paints on every state
/// change), so per observability.md § "Log on the <i>event</i>, not the <i>paint</i>"
/// the log fires here — at the producer where the error state is set — exactly once
/// per transition, never in the binding/paint.</para>
/// </summary>
public abstract partial class ViewModelBase : ObservableObject
{
    [ObservableProperty] private string? _errorMessage;
    [ObservableProperty] private string? _noticeMessage;

    /// <summary>Set (or clear, with <c>null</c>) the page's error banner. A non-empty
    /// message is also logged to the ring at <c>error</c> level under
    /// <c>fauna_windows::{ViewModel}</c>; a clear logs nothing (don't log noise).</summary>
    protected void SetError(string? message)
    {
        ErrorMessage = message;
        if (!string.IsNullOrEmpty(message))
            ShellLog.Error(GetType().Name, message);
    }

    /// <summary>Display + log a caught exception as the localized error string
    /// (<see cref="Strings.Error"/>) — the displayed string is redaction-safe by
    /// construction (it is already shown to the user).</summary>
    protected void ShowError(Exception ex) => SetError(Strings.Error(ex));

    /// <summary>Set (or clear, with <c>null</c>) a transient success/info banner —
    /// the <see cref="SetError"/> twin for non-error confirmations (a "Copied"
    /// notice, and future notices of the same shape). A non-empty message is also
    /// logged to the ring at <c>info</c> level under <c>fauna_windows::{ViewModel}</c>;
    /// a clear logs nothing. Callers own the display timing (e.g. a delayed
    /// <c>SetNotice(null)</c> to hide the banner again) — this funnel only ever logs
    /// on the non-null transition, exactly once per shown notice, mirroring
    /// linux's <c>copy_and_confirm</c> / web's <c>copyAndConfirm</c> (log the
    /// displayed text, never the underlying value).
    ///
    /// <para>Public, unlike <see cref="SetError"/>: a copy confirmation is triggered
    /// by the page's own click handler (the copy buttons live in page code-behind,
    /// not a VM command), so the page calls this directly on its bound VM.</para>
    /// </summary>
    public void SetNotice(string? message)
    {
        NoticeMessage = message;
        if (!string.IsNullOrEmpty(message))
            ShellLog.Info(GetType().Name, message);
    }

    // ── reconnect re-hydrate ────────────────────────────────────────────

    private Action? _reconnectUnsub;

    /// <summary>
    /// Re-run <paramref name="reload"/> whenever the WS reconnects
    /// (<see cref="INestRpcClient.Reconnected"/>) — the <c>transport.md</c>
    /// § Push events "observers re-pull on reconnect" contract. Call once from the
    /// derived VM's constructor; the owning page calls <see cref="CleanupReconnect"/>
    /// on navigate-away to unsubscribe (the rpc seam is app-lifetime, so an
    /// un-cleaned VM would leak). The reload is skipped while already running, so a
    /// reconnect that races an in-flight load (e.g. the compose-triggered refresh)
    /// doesn't double-fetch. <see cref="Reconnected"/> is raised on the UI thread,
    /// so the reload's bound-state mutation is thread-safe.
    /// </summary>
    private protected void RefreshOnReconnect(INestRpcClient rpc, IAsyncRelayCommand reload)
    {
        void Handler()
        {
            if (!reload.IsRunning) reload.Execute(null);
        }
        rpc.Reconnected += Handler;
        _reconnectUnsub = () => rpc.Reconnected -= Handler;
    }

    /// <summary>Unsubscribe the reconnect handler wired by
    /// <see cref="RefreshOnReconnect"/>. Idempotent; safe to call when none was
    /// wired. The owning page calls this on <c>OnNavigatedFrom</c>.</summary>
    /// <summary>True while a <see cref="RefreshOnReconnect"/> handler is wired — the
    /// owning page is open and will re-pull on the next reconnect itself.</summary>
    public bool IsReconnectWired => _reconnectUnsub is not null;

    public void CleanupReconnect()
    {
        _reconnectUnsub?.Invoke();
        _reconnectUnsub = null;
    }

    // ── store-change re-read ────────────────────────────────────────────

    private Action? _storeChangeUnsub;
    private Task? _storeResyncTask;
    private bool _storeResyncAgain;

    /// <summary>Raised on the UI thread after a store-change resync that CHANGED
    /// what the surface shows — the owning page re-renders on it. A resync that
    /// answered what is already painted raises nothing (the level rule:
    /// <c>account-runtime.md</c> § Multi-instance concurrency → <i>A runtime's own
    /// pump is a source of the notice too</i>, part 4).</summary>
    public event Action? StoreResynced;

    /// <summary>
    /// Re-run <paramref name="resync"/> whenever the shared store-change notice
    /// arrives (<see cref="INestRpcClient.StoreChanged"/>) while this VM's page is
    /// OPEN: the derived VM exposes a public <c>WatchStore()</c> the page calls
    /// from <c>Page_Loaded</c>, and the page calls <see cref="CleanupStoreChange"/>
    /// from <c>OnNavigatedFrom</c> — so only the open surface re-reads and the
    /// app-lifetime rpc seam never holds a left page's VM. Idempotent.
    ///
    /// <para><paramref name="resync"/> is the surface's own store-backed load,
    /// written to the level rule: it answers <c>true</c> only when the read
    /// changed what the surface shows, replaces nothing when it fails, and never
    /// touches an edit in progress. A notice that arrives mid-resync coalesces
    /// into exactly one more pass. <see cref="INestRpcClient.StoreChanged"/> is
    /// raised on the UI thread, so the resync's bound-state mutation is
    /// thread-safe.</para>
    /// </summary>
    private protected void WatchStoreChange(INestRpcClient rpc, Func<Task<bool>> resync)
    {
        if (_storeChangeUnsub is not null) return;

        async Task RunAsync()
        {
            do
            {
                _storeResyncAgain = false;
                try
                {
                    if (await resync()) StoreResynced?.Invoke();
                }
                catch (Exception ex)
                {
                    // A background re-read must never surface into the page: the
                    // next visit's own load reports a real fault.
                    ShellLog.Warn(GetType().Name, $"store-change re-read failed: {ex.Message}");
                }
            }
            while (_storeResyncAgain);
        }

        void Handler()
        {
            if (_storeResyncTask is { IsCompleted: false })
            {
                _storeResyncAgain = true;
                return;
            }
            _storeResyncTask = RunAsync();
        }

        rpc.StoreChanged += Handler;
        _storeChangeUnsub = () => rpc.StoreChanged -= Handler;
    }

    /// <summary>Stop re-reading on the store-change notice (the
    /// <see cref="WatchStoreChange"/> subscription). Idempotent; the owning page
    /// calls it on <c>OnNavigatedFrom</c>.</summary>
    public void CleanupStoreChange()
    {
        _storeChangeUnsub?.Invoke();
        _storeChangeUnsub = null;
    }

    /// <summary>Completes when the store-change resync in flight (if any) has
    /// finished — the tests' barrier.</summary>
    internal Task WhenStoreResyncedAsync() => _storeResyncTask ?? Task.CompletedTask;
}
