using System.Collections.Generic;
using System.Linq;

namespace FaunaApp.Core.Services;

/// <summary>
/// Ward-side Guardian Notify counter (family-safety.md § Guardian Notify) — the
/// windows twin of linux's <c>content_policy.rs::NotifyAccumulator</c> and web's
/// <c>familyNotifyBuffer.ts::NotifyBufferState</c>. Counts the viewer's own
/// GUARDIAN-floor render-enforcement events per category (deduped per item per
/// local day) and batches them for <c>fauna.family.notify_report</c> (category +
/// count, NEVER a content id), flushing at most once per the shared
/// <c>notify_report_min_interval_secs</c>.
///
/// <para>A plain, dependency-free state machine — no FFI, no UI — so it is
/// unit-testable without the native dll, the same split web's
/// <c>familyNotifyBuffer.ts</c> makes (linux keeps it inline since its module is
/// already thread-local-scoped and test-reachable either way). <see
/// cref="GuardianNotifyCache"/> owns the parts that need the SPA-equivalent
/// context: the wasm/FFI category decision, the RPC send and the flush
/// timer.</para>
/// </summary>
public sealed class GuardianNotifyAccumulator
{
    private bool _on;
    private readonly Dictionary<string, uint> _pending = new();
    private readonly HashSet<(string ItemId, string Category)> _seen = new();
    private long? _seenDay;
    private long? _lastFlush;
    private int _offsetMinutes;

    /// <summary>
    /// Whether the guardian's <c>content_notify</c> knob is on. Counting is a
    /// no-op while off; turning off drops any pending (not-yet-reported) counts
    /// (mirrors linux's <c>set_ward_content_notify</c>).
    /// </summary>
    public void SetEnabled(bool on)
    {
        _on = on;
        if (!on) _pending.Clear();
    }

    /// <summary>
    /// Count one guardian-floor enforcement on <paramref name="itemId"/> for
    /// each of <paramref name="categories"/>, deduped per <c>(item, category)</c>
    /// within the local day (the dedup set resets at local midnight, derived
    /// from <paramref name="nowSecs"/> + <paramref name="offsetMinutes"/>). A
    /// no-op unless <see cref="SetEnabled"/> is on and categories is non-empty.
    /// </summary>
    public void Record(string itemId, IReadOnlyList<string> categories, long nowSecs, int offsetMinutes)
    {
        if (!_on || categories.Count == 0) return;
        _offsetMinutes = offsetMinutes;
        var localDay = FloorDiv(nowSecs + (long)offsetMinutes * 60, 86_400);
        if (_seenDay != localDay)
        {
            _seen.Clear();
            _seenDay = localDay;
        }
        foreach (var category in categories)
        {
            if (_seen.Add((itemId, category)))
            {
                _pending[category] = _pending.TryGetValue(category, out var n) ? n + 1 : 1;
            }
        }
    }

    /// <summary>
    /// Drain the pending per-category deltas if a batch is due — "batched (at
    /// most hourly)": at least <paramref name="minIntervalSecs"/> between
    /// flushes, with the first report (no prior flush) eager. <c>null</c> when
    /// nothing is pending or the interval has not elapsed.
    /// </summary>
    public (IReadOnlyList<(string Category, uint Count)> Entries, int OffsetMinutes)? TryTakeDue(
        long nowSecs, uint minIntervalSecs)
    {
        if (_pending.Count == 0) return null;
        if (_lastFlush is { } last && nowSecs - last < minIntervalSecs) return null;

        var entries = _pending.Select(kv => (kv.Key, kv.Value)).ToList();
        _pending.Clear();
        _lastFlush = nowSecs;
        return (entries, _offsetMinutes);
    }

    /// <summary>
    /// Full reset — the identity-teardown boundary (sign-out, account switch)
    /// and test isolation both use this. Drops pending counts AND the dedup set
    /// (carrying the dedup set across an actor change would silently
    /// under-report the incoming ward's first enforcement on an item the
    /// outgoing ward already saw) and turns counting back off until the next
    /// <see cref="SetEnabled"/> from a fresh <c>fauna.family.status</c> read.
    /// </summary>
    public void Reset()
    {
        _on = false;
        _pending.Clear();
        _seen.Clear();
        _seenDay = null;
        _lastFlush = null;
        _offsetMinutes = 0;
    }

    private static long FloorDiv(long a, long b)
    {
        var q = a / b;
        var r = a % b;
        return r != 0 && (r < 0) != (b < 0) ? q - 1 : q;
    }
}
