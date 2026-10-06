using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using Windows.Foundation;

namespace FaunaApp.Feed;

/// <summary>
/// Engagement-cue viewport observer for the Feed post list — the thin WinUI glue over
/// the shared <see cref="FfiCueTracker"/> bookkeeping (<c>docs/goal/behavior/
/// engagement-cues.md</c> § Cue vocabulary &amp; derivation, the capture-shell boundary
/// revised 2026-07-29). Mirrors linux's <c>apps/fauna-linux/src/feed/viewport.rs::wire</c>,
/// NOT android's virtualization-aware observer: <c>FeedPage</c>'s <c>PostsList</c> is a
/// deliberately non-virtualizing <c>StackPanel</c> (<c>FeedPage.xaml:139-151</c>'s own
/// comment), so every post-card <c>ListViewItem</c> container stays realized regardless of
/// scroll position — there is no container-disposal signal to read the way there is on
/// Compose/GTK-with-virtualization, hence <see cref="LeaveModel.HoldUnmeasured"/>.
///
/// This class holds NO bookkeeping of its own — visibility bucketing, dwell credit, the
/// hold-vs-leave decision and the noise floor all live in the shared tracker now. It only
/// samples honest geometry (raw row rects, in the <see cref="ScrollViewer"/>'s own
/// coordinate space) into <see cref="FfiCueTracker.Sample"/>, and reports whatever it
/// returns via <c>FfiFeedManager.RecordObservation</c>. <c>media_played_pm</c> is always
/// <c>null</c>: no client, windows included, has any video-playback UI today — the
/// <c>post-image</c>/<c>video-thumbnail</c> elements are stills-only.
/// </summary>
internal sealed class CueViewportObserver
{
    private readonly ListView _list;
    private readonly Func<FfiFeedManager?> _manager;
    private readonly Action<string> _onError;

    private FfiCueTracker? _tracker;
    private DispatcherTimer? _timer;
    private ScrollViewer? _scrollViewer;
    private bool _wired;

    internal CueViewportObserver(ListView list, Func<FfiFeedManager?> manager, Action<string> onError)
    {
        _list = list;
        _manager = manager;
        _onError = onError;
    }

    /// <summary>
    /// Wire the observer: build the shared tracker with windows' leave model, attach the
    /// scroll-position extra-sample hook, and start the tick at the shared
    /// <c>FaunaFfiMethods.CueSampleIntervalMs()</c> cadence (never a re-declared literal).
    /// Call once from <c>FeedPage.Page_Loaded</c>, BEFORE <c>FfiFeedManager.HydrateCues()</c>
    /// (puts are suppressed pre-hydrate anyway, so the ordering between wiring and
    /// hydrate-completing is not itself a race).
    /// </summary>
    internal void Wire()
    {
        if (_wired) return;
        _wired = true;

        _tracker = new FfiCueTracker(LeaveModel.HoldUnmeasured);

        // WinUI's ListView template always contains exactly one internal
        // ScrollViewer, but there is no public API to reach it directly.
        // Fall back to the ListView itself if the template somehow hasn't
        // applied yet (defensive only — it always has by Page_Loaded).
        _scrollViewer = VisualTreeHelperExtensions.FindDescendant<ScrollViewer>(_list);
        if (_scrollViewer is not null)
            _scrollViewer.ViewChanged += ScrollViewer_ViewChanged;

        _timer = new DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(FaunaFfiMethods.CueSampleIntervalMs()),
        };
        _timer.Tick += Timer_Tick;
        _timer.Start();
    }

    /// <summary>
    /// Stop the tick + scroll hook and flush every still-tracked card — mirrors linux's
    /// unmap handling (off-screen is off-viewport). Call from
    /// <c>FeedPage.OnNavigatedFrom</c>: <c>FeedPage</c> isn't cached
    /// (<c>NavigationCacheMode.Disabled</c>, the default), so a live
    /// <see cref="DispatcherTimer"/> left running would otherwise keep sampling a
    /// detached list forever and pin the whole page in memory.
    /// </summary>
    internal void Unwire()
    {
        if (!_wired) return;
        _wired = false;

        _timer?.Stop();
        if (_timer is not null) _timer.Tick -= Timer_Tick;
        _timer = null;

        if (_scrollViewer is not null)
            _scrollViewer.ViewChanged -= ScrollViewer_ViewChanged;
        _scrollViewer = null;

        FlushAll();
        _tracker?.Dispose();
        _tracker = null;
    }

    private void Timer_Tick(object? sender, object e) => Sample();

    private void ScrollViewer_ViewChanged(object? sender, ScrollViewerViewChangedEventArgs e) => Sample();

    /// <summary>
    /// One honest sample: walk every row in the non-virtualizing <c>PostsList</c>
    /// (index-aligned 1:1 with <c>FeedPage.Posts</c> — the SAME <c>ListView</c> read
    /// twice back-to-back within this one call, so there is no window for the two reads
    /// to observe different collection states), hand the tracker each row's raw geometry
    /// plus the two clocks, and emit whatever it returns.
    ///
    /// Identity is read fresh from <c>_list.Items[i]</c> each sample — never cached
    /// across ticks: a <c>post_id</c> captured once and reused across samples could
    /// silently re-attribute one post's dwell to another the moment
    /// <c>FeedPage.SyncPosts</c>'s <c>ObservableCollectionReconcile</c> reorders the
    /// bound collection between ticks.
    ///
    /// A row this pass could not read at all (not realized, or not yet connected to the
    /// visual tree) is simply omitted — the tracker holds it, uncredited, as long as its
    /// post stays in <c>windowPostIds</c>.
    /// </summary>
    private void Sample()
    {
        if (_tracker is null) return;

        var viewportHeight = _scrollViewer?.ViewportHeight ?? _list.ActualHeight;
        var referenceFrame = (UIElement?)_scrollViewer ?? _list;

        var windowPostIds = new List<string>(_list.Items.Count);
        var rows = new List<CueRow>();

        for (int i = 0; i < _list.Items.Count; i++)
        {
            if (_list.Items[i] is not FeedPostItem post) continue; // never happens in practice
            windowPostIds.Add(post.PostId);

            if (_list.ContainerFromIndex(i) is not ListViewItem container) continue; // not realized
            double top;
            try
            {
                var transform = container.TransformToVisual(referenceFrame);
                top = transform.TransformPoint(new Point(0, 0)).Y;
            }
            catch (Exception)
            {
                continue; // not yet connected to the visual tree
            }
            // media_played_pm is always null — no client has any video-playback UI yet.
            rows.Add(new CueRow(post.PostId, top, container.ActualHeight, post.HasMedia, null));
        }

        var monoNowMs = (ulong)Environment.TickCount64;
        var wallNowMs = (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        Emit(_tracker.Sample(rows.ToArray(), windowPostIds.ToArray(), 0, viewportHeight, monoNowMs, wallNowMs));
    }

    private void FlushAll()
    {
        if (_tracker is null) return;
        var wallNowMs = (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();
        Emit(_tracker.DrainAll(wallNowMs));
    }

    /// <summary>
    /// Hand the finished exposures to the shared engine (<c>FfiFeedManager.
    /// RecordObservation</c>) — already past the tracker's own single-sample noise floor,
    /// so no shell-side filtering is needed or correct here. A failure surfaces via the
    /// page's <c>onError</c> callback (the <c>error-message</c> element), mirroring
    /// linux/android's error-surfacing convention.
    /// </summary>
    private void Emit(CueObservation[] observations)
    {
        if (observations.Length == 0) return;
        var manager = _manager();
        if (manager is null) return;
        _ = EmitAsync(manager, observations);
    }

    private async Task EmitAsync(FfiFeedManager manager, CueObservation[] observations)
    {
        foreach (var o in observations)
        {
            try
            {
                await manager.RecordObservation(
                    o.contentId,
                    o.isMedia,
                    o.mediaPlayedPm,
                    o.dwellMsAtSkipVisibility,
                    o.dwellMsAtLongVisibility,
                    o.observedAtMs);
            }
            catch (Exception ex)
            {
                _onError(ex.Message);
            }
        }
    }
}
