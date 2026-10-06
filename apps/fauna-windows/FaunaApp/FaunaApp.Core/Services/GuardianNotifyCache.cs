using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Process-lifetime holder for the current login's <see
/// cref="GuardianNotifyAccumulator"/> (family-safety.md § Guardian Notify) — the
/// windows twin of web's <c>familyNotify.ts</c>. Sibling of <see
/// cref="ContentPolicyCache"/>: both the feed post-card and the conversation
/// bubble route enforcement events through this ONE static holder so the two
/// surfaces can never drift on what Guardian Notify counts.
///
/// <para>Stamps the UTC instant (<see cref="DateTimeOffset"/>.UtcNow) and the
/// device's UTC offset — the latter through <see
/// cref="DeviceOffset"/>, the app's ONE named door
/// (<c>value-formatting.md</c> § Absolute local timestamp display, the
/// app-side one-door rule), because this offset is nest-facing: it becomes the
/// ward-local day bucket <c>fauna.family.notify_report</c> persists. It is
/// therefore a platform-specific <see cref="TimeZoneInfo"/> read, exactly like
/// linux's glib <c>now_and_offset</c> — just routed through the door rather
/// than taken inline. Also converts the drained batch to the FFI wire shape,
/// so callers deal only in plain item ids/category strings.</para>
/// </summary>
public static class GuardianNotifyCache
{
    private static readonly object _gate = new();
    private static GuardianNotifyAccumulator _accumulator = new();

    /// <summary>
    /// Set from the SAME <c>fauna.family.status</c> read that sets <see
    /// cref="ContentPolicyCache.SetGuardianPolicy"/> (<c>MainPage.CheckFamilyStatusAsync</c>)
    /// — no extra RPC.
    /// </summary>
    public static void SetEnabled(bool on)
    {
        lock (_gate) _accumulator.SetEnabled(on);
    }

    /// <summary>
    /// Record guardian-floor enforcement on <paramref name="itemId"/>.
    /// <paramref name="categories"/> comes from
    /// <c>FaunaFfiMethods.GuardianEnforcedCategories</c> — never the ward's own-
    /// threshold collapse (Notify is a guardian-floor-only lens).
    /// </summary>
    public static void Record(string itemId, IReadOnlyList<string> categories)
    {
        if (categories.Count == 0) return;
        var nowSecs = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        // The app's one named UTC-offset door (value-formatting.md § Absolute
        // local timestamp display — the app-side one-door rule): this reading
        // is nest-facing (the day bucket `fauna.family.notify_report` stores),
        // so it must agree with every other offset read in the app.
        var offsetMinutes = DeviceOffset.UtcOffsetMinutes();
        lock (_gate) _accumulator.Record(itemId, categories, nowSecs, offsetMinutes);
    }

    /// <summary>
    /// Drain the pending deltas if a batch is due, converted to the FFI wire
    /// shape ready for <c>INestRpcClient.FamilyNotifyReportAsync</c>. <c>null</c>
    /// when nothing is due (nothing pending, off, or within the interval).
    /// </summary>
    internal static (FfiFamilyContentNotice[] Entries, int OffsetMinutes)? TryTakeDue()
    {
        var nowSecs = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var minIntervalSecs = FaunaFfiMethods.NotifyReportMinIntervalSecs();

        (IReadOnlyList<(string Category, uint Count)> Entries, int OffsetMinutes)? due;
        lock (_gate) due = _accumulator.TryTakeDue(nowSecs, minIntervalSecs);
        if (due is not { } batch) return null;

        var entries = batch.Entries
            .Select(e => new FfiFamilyContentNotice(e.Category, e.Count))
            .ToArray();
        return (entries, batch.OffsetMinutes);
    }

    /// <summary>
    /// Drain-if-due and send, best-effort (a modified client under-reports,
    /// never over-reports — Notify's trust bound: a send failure is swallowed,
    /// never retried, since the counts are already drained). Shared by
    /// <c>MainViewModel</c>'s flush tick and the <c>family_notify_check_now</c>
    /// e2e poke (testing.md convention 14).
    /// </summary>
    internal static async Task CheckNowAsync(INestRpcClient rpc)
    {
        if (TryTakeDue() is not { } batch) return;
        try
        {
            await rpc.FamilyNotifyReportAsync(batch.Entries, batch.OffsetMinutes);
        }
        catch
        {
            // Best-effort; coarse and non-retried by design (see summary above).
        }
    }

    /// <summary>
    /// Drop this actor's counts + dedup state — the identity-teardown boundary
    /// (sign-out, account switch) and test isolation both use this.
    /// </summary>
    public static void Reset()
    {
        lock (_gate) _accumulator = new GuardianNotifyAccumulator();
    }
}
