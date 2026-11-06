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
/// returns via <c>FfiFeedManager.RecordObservation</c>.
///
/// <para><b>Playback:</b> a list card's inline player (<c>Controls.VideoThumbnailView</c>)
/// hands each progress reading and its end to the live page's tracker
/// (<see cref="NotePlayed"/> → <c>FfiCueTracker.NotePlayed</c>), which holds the post's
/// unread peak and takes it for the post's media row at the next sample — the
/// tracker-held form of the shared <c>CueRow.media_played_pm</c> contract — so the rows
/// here pass no played fraction of their own, and a video played to its end derives
/// the media <c>watch-complete</c>. apple's <c>VideoThumbnailView(onPlayed:)</c> is the
/// twin.</para>
///
/// <para><b>The tracker is the auth's, not this page's</b>
/// (<c>Core.FeedManagerHost.CueTrackerFor</c>): what it holds — the frozen test clock
/// included — outlives a trip off the Feed page. <see cref="Unwire"/> drains it, so a
/// later mount still starts with nothing tracked.</para>
///
/// <para><b>Records reach the engine one batch at a time, in the order their samples
/// were taken</b> (<see cref="Emit"/>, apple's <c>CueViewportObserver.record</c>
/// twin): every batch chains behind the one before, so a test step that awaits its own
/// sample's batch has waited out every earlier one too — the sampling tick's included.</para>
/// </summary>
internal sealed class CueViewportObserver
{
    /// <summary>The observer sampling the Feed page on screen, if any — what a player's
    /// reading is handed to. Set by <see cref="Wire"/>, cleared by <see cref="Unwire"/>;
    /// UI thread only.</summary>
    private static CueViewportObserver? _live;

    /// <summary>The last batch of exposures handed to the engine — every
    /// <see cref="Emit"/> chains behind it. Process-wide rather than per mount, so a
    /// page leave's drain and the next mount's first records stay in order too. UI
    /// thread only; never faults (<see cref="EmitAsync"/> reports each failure).</summary>
    private static Task _recording = Task.CompletedTask;

    /// <summary>An inline player's reading on <paramref name="postId"/>'s media, both in
    /// seconds, handed to the live page's tracker. With no Feed page wired (a player in
    /// the post-detail dialog over another page) there is no list to sample, so it is
    /// dropped.</summary>
    internal static void NotePlayed(string postId, double position, double duration)
    {
        if (_live?.Tracker is not { } tracker)
        {
            Core.Logs.E2eTrace.Write($"[cue] played post={Short(postId)} dropped: no live feed capture");
            return;
        }
        tracker.NotePlayed(postId, position, duration);
        Core.Logs.E2eTrace.Write($"[cue] played post={Short(postId)} position={position:0.###} duration={duration:0.###}");
    }

    /// <summary>Whether <paramref name="postId"/>'s inline player is engaged, handed to the
    /// live page's tracker (<c>FfiCueTracker.NotePlaying</c>; engagement-cues.md § Cue
    /// vocabulary &amp; derivation, <em>A play is attention</em>): an engaged media row is
    /// attended whatever its visible fraction, so the tapped card that grows with its player
    /// under the 50 % gate still counts. The twin of <see cref="NotePlayed"/>, dropped the same
    /// way with no Feed page wired.</summary>
    internal static void NotePlaying(string postId, bool playing)
    {
        if (_live?.Tracker is not { } tracker)
        {
            Core.Logs.E2eTrace.Write($"[cue] playing post={Short(postId)} dropped: no live feed capture");
            return;
        }
        tracker.NotePlaying(postId, playing);
        Core.Logs.E2eTrace.Write($"[cue] playing post={Short(postId)} {(playing ? "engaged" : "released")}");
    }

    private static string Short(string postId) => postId[..Math.Min(12, postId.Length)];
    private readonly ListView _list;
    private readonly Func<FfiFeedManager?> _manager;
    private readonly Action<string> _onError;

    private DispatcherTimer? _timer;
    private ScrollViewer? _scrollViewer;
    private bool _wired;

    internal CueViewportObserver(ListView list, Func<FfiFeedManager?> manager, Action<string> onError)
    {
        _list = list;
        _manager = manager;
        _onError = onError;
    }

    /// <summary>The auth's ONE tracker (<c>Core.FeedManagerHost.CueTrackerFor</c>) for
    /// the manager this page observes, or <c>null</c> while there is none.</summary>
    private FfiCueTracker? Tracker
        => _manager() is { } manager ? Core.FeedManagerHost.CueTrackerFor(manager) : null;

    /// <summary>
    /// Wire the observer: mark it the live capture, attach the
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

        _live = this;

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
    /// unmap handling (off-screen is off-viewport). The tracker itself is the auth's and
    /// stays: the drain empties it, so nothing it held is credited across the gap.
    /// Call from
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
        if (ReferenceEquals(_live, this)) _live = null;
    }

    private void Timer_Tick(object? sender, object e) => SampleUnlessAStepHolds();

    private void ScrollViewer_ViewChanged(object? sender, ScrollViewerViewChangedEventArgs e) => SampleUnlessAStepHolds();

    /// <summary>The capture's own sample (the tick, a scroll) — skipped only while a
    /// test step holds the capture (<c>HoldCaptureForTest</c>), so the step's own
    /// sample is the first to read the layout it moved.</summary>
    private void SampleUnlessAStepHolds()
    {
#if DEBUG || FAUNA_E2E_AGENT
        if (_stepHolds > 0) return;
#endif
        Sample();
    }

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
        if (Tracker is not { } tracker) return;

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
            // No played fraction of our own: the tracker holds the player's (NotePlayed).
            rows.Add(new CueRow(post.PostId, top, container.ActualHeight, post.PlaysMedia, null));
        }

        Emit(tracker.Sample(rows.ToArray(), windowPostIds.ToArray(), 0, viewportHeight, MonoNowMs(), WallNowMs()));
    }

    /// <summary>The monotonic reading every sample passes (never a wall clock: an NTP
    /// step must not inflate dwell) — and the one the test clock freezes at.</summary>
    internal static ulong MonoNowMs() => (ulong)Environment.TickCount64;

    /// <summary>The wall reading every sample passes (the engine's pace gate reads it
    /// as <c>observed_at_ms</c>) — and the one the test clock freezes at.</summary>
    internal static ulong WallNowMs() => (ulong)DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    private void FlushAll()
    {
        if (Tracker is not { } tracker) return;
        Emit(tracker.DrainAll(WallNowMs()));
    }

    /// <summary>
    /// Hand the finished exposures to the shared engine (<c>FfiFeedManager.
    /// RecordObservation</c>) — already past the tracker's own single-sample noise floor,
    /// so no shell-side filtering is needed or correct here — behind every earlier batch
    /// (<see cref="_recording"/>). Not awaited by the tick: the capture never waits on
    /// the engine. A failure surfaces via the page's <c>onError</c> callback (the
    /// <c>error-message</c> element), mirroring linux/android's error-surfacing
    /// convention.
    /// </summary>
    private void Emit(CueObservation[] observations)
    {
        if (observations.Length == 0) return;
        var manager = _manager();
        if (manager is null) return;
        _recording = RecordAfterAsync(_recording, manager, observations);
    }

    private async Task RecordAfterAsync(Task previous, FfiFeedManager manager, CueObservation[] observations)
    {
        await previous;
        await EmitAsync(manager, observations);
    }

    private async Task EmitAsync(FfiFeedManager manager, CueObservation[] observations)
    {
        foreach (var o in observations)
        {
            Core.Logs.E2eTrace.Write(
                $"[cue] emit post={Short(o.contentId)} media={o.isMedia} "
                + $"played_pm={o.mediaPlayedPm?.ToString() ?? "none"} long_ms={o.dwellMsAtLongVisibility} ended={o.endedBy}");
            try
            {
                // Relayed whole: the tracker stamped how each exposure ended
                // (endedBy), and the shell has no honest way of its own to know it.
                await manager.RecordObservation(o);
            }
            catch (Exception ex)
            {
                _onError(ex.Message);
            }
        }
    }

#if DEBUG || FAUNA_E2E_AGENT
    // ── Test-step doors (the e2e cue-clock commands, Feed/FeedCueClockCommands) ──
    // Compiled out of release artifacts with the agent that calls them (testing.md
    // convention 15).

    /// <summary>The observer sampling the Feed page on screen, or <c>null</c> off it.</summary>
    internal static CueViewportObserver? LiveForTest => _live;

    /// <summary>The post list's own <see cref="ScrollViewer"/> — what a user's scroll
    /// moves — or <c>null</c> if the template never gave one.</summary>
    internal ScrollViewer? ScrollViewerForTest => _scrollViewer;

    /// <summary>How many test steps hold the capture's own samples; UI thread only.</summary>
    private int _stepHolds;

    /// <summary>Hold the capture's own samples (the tick, the scroll hook) until the
    /// returned handle is disposed. A pixel step moves the list and then samples it
    /// itself: without the hold, the scroll's own sample ends the exposure first and
    /// hands it to the engine, whose train can re-rank the feed while the step still
    /// waits for its frame — and the step's sample then reads a re-ordered list, in
    /// which a card the step never moved has left view too.</summary>
    internal IDisposable HoldCaptureForTest()
    {
        _stepHolds++;
        return new StepHold(this);
    }

    private sealed class StepHold(CueViewportObserver observer) : IDisposable
    {
        private bool _released;

        public void Dispose()
        {
            if (_released) return;
            _released = true;
            observer._stepHolds--;
        }
    }

    /// <summary>One sample of the mounted list, taken now, after a layout pass. Returns
    /// the tracker's probe as THIS sample left it and the batch it handed the engine,
    /// which completes once that batch — and every batch before it, the tick's
    /// included — is recorded, the engine's train and any put included: the step's
    /// record barrier. The caller awaits it before acking.</summary>
    internal (string? Probe, Task Recorded) SampleNowForTest()
    {
        _list.UpdateLayout();
        Sample();
        return (Tracker?.ProbeForTest(), _recording);
    }
#endif
}
