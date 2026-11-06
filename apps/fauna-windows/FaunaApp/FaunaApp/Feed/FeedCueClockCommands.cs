// testing.md convention 15 — automation surface, compiled out of release
// artifacts. Reached ONLY from Testing/TestAgent's command table, and it calls
// `FfiCueTracker`'s test-clock faces, which the production FFI flavor does not
// export. Gated with the agent.
#if DEBUG || FAUNA_E2E_AGENT
using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Threading.Tasks;
using Microsoft.UI.Xaml.Media;
using uniffi.fauna_ffi;

namespace FaunaApp.Feed;

/// <summary>
/// The engagement-cue test clock's windows doors (convention 14,
/// <c>e2e-latency-independent-assertions.md</c>): <c>feed_cue_clock_freeze</c>,
/// <c>feed_cue_clock_thaw</c>, <c>feed_cue_clock_advance {ms}</c>,
/// <c>feed_cue_probe</c> and <c>feed_cue_scroll_by {px}</c> — the same names and
/// reply shapes as tui's, linux's (<c>feed::viewport::test_clock</c>), web's
/// (<c>#lib/feed-cues-e2e</c>), android's (<c>TestAgent.feedCueCommand</c>) and
/// apple's (<c>FeedCueClockTestCommand</c>). The frozen clock itself is the shared
/// tracker's (<c>fauna_feed::CueTracker</c> § The test clock), reached over
/// <see cref="FfiCueTracker"/>'s test-helpers faces.
///
/// <para>The clock lives on the auth's ONE tracker
/// (<c>Core.FeedManagerHost.CueTrackerFor</c>), so freeze, thaw and advance work off
/// the Feed page too; the probe's measuring sample and the pixel step need the
/// mounted list (<see cref="CueViewportObserver.LiveForTest"/>).</para>
///
/// <para>Every sample taken here resolves only once the exposures it ended — and any
/// the sampling tick ended before it — are recorded, the engine's train and any put
/// included (<see cref="CueViewportObserver.SampleNowForTest"/>), so an ack means
/// nothing more is on its way. windows also has a quit door a test can drive and wait
/// out (<c>TrayIconService.QuitApplication</c>'s bounded flush), which the journeys
/// keep using.</para>
///
/// <para>Runs on the UI thread (the agent's post-action): the list's geometry and its
/// scroll offset are UI-thread state. A refusal throws; the agent answers it as
/// <c>{error}</c> and stamps it loudly (convention 11).</para>
/// </summary>
internal static class FeedCueClockCommands
{
    /// <summary>How many frames a scroll step waits for its offset to land before it
    /// samples anyway (a frame count, never a duration).</summary>
    private const int MaxFramesForLayout = 60;

    /// <summary>Run one command; returns its reply for the agent's result slot.</summary>
    public static async Task<JsonElement> RunAsync(string action, Dictionary<string, object?> command)
    {
        var manager = Core.FeedManagerHost.Current
            ?? throw new InvalidOperationException(
                $"{action}: no feed manager (pre-auth, or the Feed page never built one)");
        var tracker = Core.FeedManagerHost.CueTrackerFor(manager);
        var observer = CueViewportObserver.LiveForTest;

        switch (action)
        {
            case "feed_cue_clock_freeze":
                tracker.FreezeClockForTest(CueViewportObserver.MonoNowMs(), CueViewportObserver.WallNowMs());
                return Ok();
            case "feed_cue_clock_thaw":
                tracker.ThawClockForTest();
                return Ok();
            case "feed_cue_clock_advance":
                {
                    var ms = RequireUlong(command, "ms", action);
                    // Refused (FfiException) when nothing is frozen or `ms` exceeds one
                    // sample's credit cap — the agent answers it as {error}.
                    tracker.AdvanceClockForTest(ms);
                    // Sample at once, so each step lands as exactly one sample's
                    // credit (the tick could fold two advances into one capped sample).
                    if (observer is not null) await observer.SampleNowForTest().Recorded;
                    return Ok();
                }
            case "feed_cue_probe":
                // Frozen, a sample credits nothing — it only measures — so the probe
                // reads the list as laid out after the next frame (a `scroll_to` just
                // before lands only then), not as the last tick saw it.
                if (observer is not null && IsFrozen(tracker))
                {
                    Task recorded;
                    using (observer.HoldCaptureForTest())
                    {
                        await NextFrameAsync();
                        recorded = observer.SampleNowForTest().Recorded;
                    }
                    await recorded;
                }
                return Parse(tracker.ProbeForTest());
            case "feed_cue_scroll_by":
                {
                    // Scroll the post list's real offset by `px`, then answer once the
                    // moved layout has been sampled, with the probe as THAT sample left
                    // it — a later read could already show the feed's re-rank in answer
                    // to an exposure the step ended. `moved: false` at either end.
                    var px = RequireDouble(command, "px", action);
                    if (observer?.ScrollViewerForTest is not { } scroll)
                    {
                        throw new InvalidOperationException($"{action}: the feed page is not mounted");
                    }
                    // The capture's own samples are held until the step has sampled:
                    // the scroll's own sample would otherwise end the exposure first,
                    // and its train could re-rank the list before this step reads it.
                    bool moved;
                    string? probe;
                    Task recorded;
                    using (observer.HoldCaptureForTest())
                    {
                        var before = scroll.VerticalOffset;
                        var target = Math.Clamp(before + px, 0, scroll.ScrollableHeight);
                        if (target != before)
                        {
                            scroll.ChangeView(null, target, null, disableAnimation: true);
                            for (var frames = 0;
                                 Math.Abs(scroll.VerticalOffset - target) > 0.5 && frames < MaxFramesForLayout;
                                 frames++)
                            {
                                await NextFrameAsync();
                            }
                        }
                        await NextFrameAsync();
                        moved = Math.Abs(scroll.VerticalOffset - before) > 0.5;
                        (probe, recorded) = observer.SampleNowForTest();
                    }
                    await recorded;
                    return JsonSerializer.SerializeToElement(new Dictionary<string, object?>
                    {
                        ["ok"] = true,
                        ["moved"] = moved,
                        ["probe"] = probe is null ? null : Parse(probe),
                    });
                }
            default:
                throw new InvalidOperationException($"{action}: unhandled feed cue command");
        }
    }

    private static JsonElement Ok()
        => JsonSerializer.SerializeToElement(new Dictionary<string, object?> { ["ok"] = true });

    private static JsonElement Parse(string json)
    {
        using var doc = JsonDocument.Parse(json);
        return doc.RootElement.Clone();
    }

    private static bool IsFrozen(FfiCueTracker tracker)
    {
        using var doc = JsonDocument.Parse(tracker.ProbeForTest());
        return doc.RootElement.TryGetProperty("frozen", out var frozen)
            && frozen.ValueKind == JsonValueKind.True;
    }

    /// <summary>Resume once the next frame has been raised and the UI thread has run
    /// what was queued behind it — the continuation is posted, never inlined into the
    /// <c>Rendering</c> raise — so a read sees what that frame laid out (android's
    /// <c>awaitLaidOutFrame</c>, linux's <c>after_next_frame</c>, apple's
    /// <c>nextFrame</c>).</summary>
    private static Task NextFrameAsync()
    {
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        void OnRendering(object? sender, object e)
        {
            CompositionTarget.Rendering -= OnRendering;
            done.TrySetResult();
        }
        CompositionTarget.Rendering += OnRendering;
        return done.Task;
    }

    /// <summary>A present, non-negative integer field; absent or malformed is a
    /// refusal, never a silent default.</summary>
    private static ulong RequireUlong(Dictionary<string, object?> command, string key, string action)
    {
        if (command.TryGetValue(key, out var raw) && raw is JsonElement el
            && el.ValueKind == JsonValueKind.Number && el.TryGetUInt64(out var value))
        {
            return value;
        }
        throw new InvalidOperationException($"{action}: needs a non-negative integer `{key}`");
    }

    /// <summary>A present numeric field; absent or malformed is a refusal.</summary>
    private static double RequireDouble(Dictionary<string, object?> command, string key, string action)
    {
        if (command.TryGetValue(key, out var raw) && raw is JsonElement el
            && el.ValueKind == JsonValueKind.Number && el.TryGetDouble(out var value))
        {
            return value;
        }
        throw new InvalidOperationException($"{action}: needs a number `{key}`");
    }
}
#endif
