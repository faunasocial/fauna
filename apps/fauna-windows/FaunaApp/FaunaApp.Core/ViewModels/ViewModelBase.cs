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
    public void CleanupReconnect()
    {
        _reconnectUnsub?.Invoke();
        _reconnectUnsub = null;
    }
}
