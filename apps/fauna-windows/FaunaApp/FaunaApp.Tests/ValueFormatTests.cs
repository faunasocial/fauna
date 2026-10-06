using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for <see cref="ValueFormat"/>: the windows app
/// must render byte sizes / durations identically to shared Rust
/// (<c>fauna_core::format</c>, per <c>docs/goal/behavior/value-formatting.md</c>).
/// These call the REAL UniFFI exports (native <c>fauna_ffi</c> dll loads in the
/// test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>) and
/// resolve the returned <c>LocalizedText</c> through the same
/// <see cref="Strings"/> pipeline production uses, with a fake localizer that
/// mirrors the generated <b>named</b> resw templates. Locks windows to the shared
/// thresholds + the dropped-trailing-<c>.0</c> rounding (priority #1/#2/#4).
/// </summary>
[Collection("StringsGlobal")]
public class ValueFormatTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        // The windows resw keeps source {name} placeholders intact (Strings.Resolve
        // substitutes by name); these mirror the generated windows resw for
        // size.* / time.uptime_* (en.yaml 141-155).
        private static readonly Dictionary<string, string> Map = new()
        {
            ["size/bytes"] = "{value} B",
            ["size/kb"] = "{value} KB",
            ["size/mb"] = "{value} MB",
            ["size/gb"] = "{value} GB",
            ["size/tb"] = "{value} TB",
            ["time/uptime_dhm"] = "{days}d {hours}h {mins}m",
            ["time/uptime_hm"] = "{hours}h {mins}m",
            ["time/uptime_m"] = "{mins}m",
            ["time/countdown_dh"] = "{days}d {hours}h",
            ["time/countdown_h"] = "{hours}h",
            ["time/just_now"] = "just now",
            ["time/minutes_ago"] = "{count}m ago",
            ["time/hours_ago"] = "{count}h ago",
            ["time/days_ago"] = "{count}d ago",
            ["time/yesterday"] = "Yesterday",
            ["time/weekday_mon"] = "Mon",
            ["time/weekday_tue"] = "Tue",
            ["time/weekday_wed"] = "Wed",
            ["time/weekday_thu"] = "Thu",
            ["time/weekday_fri"] = "Fri",
            ["time/weekday_sat"] = "Sat",
            ["time/weekday_sun"] = "Sun",
            ["onboarding/nest_provisioning/elapsed_template"] = "{seconds}s elapsed",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ValueFormatTests() => Strings.Initialize(new FakeLocalizer());

    [Theory]
    [InlineData(0UL, "0 B")]
    [InlineData(512UL, "512 B")]
    [InlineData(1023UL, "1023 B")]
    [InlineData(1024UL, "1 KB")]            // trailing .0 dropped (was "1.0 KB" pre-adoption)
    [InlineData(1536UL, "1.5 KB")]
    [InlineData(5UL * 1024 * 1024, "5 MB")]
    [InlineData(3UL * 1024 * 1024 * 1024, "3 GB")]
    [InlineData(1024UL * 1024 * 1024 * 1024, "1 TB")] // TB bucket (windows had none pre-adoption)
    public void ByteSize_MatchesSharedRustFormatter(ulong bytes, string expected)
    {
        Assert.Equal(expected, ValueFormat.ByteSize(bytes));
    }

    [Theory]
    [InlineData(0UL, "0m")]               // sub-minute → "0m"
    [InlineData(59UL, "0m")]
    [InlineData(60UL, "1m")]
    [InlineData(3600UL, "1h 0m")]         // exactly 1h → uptime_hm, full chain
    [InlineData(3661UL, "1h 1m")]         // seconds dropped
    [InlineData(86400UL, "1d 0h 0m")]     // exactly 1d → uptime_dhm, full chain
    [InlineData(90061UL, "1d 1h 1m")]     // multi-arg: days+hours+mins all present
    public void DurationSecs_MatchesSharedRustFormatter(ulong secs, string expected)
    {
        Assert.Equal(expected, ValueFormat.DurationSecs(secs));
    }

    /// <summary>
    /// Relative-time buckets match shared Rust (<c>relative_time_display</c>): the
    /// four recent buckets resolve their i18n key; <c>≥ 7 d</c> returns
    /// <c>localized == null</c>, the signal to render an absolute, native
    /// locale-aware date. <c>now</c> is fixed so the buckets are deterministic.
    /// </summary>
    [Fact]
    public void RelativeTime_MatchesSharedRustFormatter()
    {
        const long now = 1_700_000_000_000L;

        Assert.Equal("just now", ValueFormat.RelativeTime(now, now - 30_000L));            // < 60 s
        Assert.Equal("5m ago", ValueFormat.RelativeTime(now, now - 5L * 60_000L));         // < 60 min
        Assert.Equal("3h ago", ValueFormat.RelativeTime(now, now - 3L * 3_600_000L));      // < 24 h
        Assert.Equal("2d ago", ValueFormat.RelativeTime(now, now - 2L * 86_400_000L));     // < 7 d

        // ≥ 7 d → native locale-aware absolute date (localized == null path).
        long then = now - 8L * 86_400_000L;
        Assert.Equal(
            DateTimeOffset.FromUnixTimeMilliseconds(then).LocalDateTime.ToString("d"),
            ValueFormat.RelativeTime(now, then));
    }

    /// <summary>
    /// Conversation-list contextual timestamp matches shared Rust
    /// (<c>conversation_timestamp_display</c>): today → the local 24 h wall-clock
    /// (zero-padded <c>HH:MM</c>); the previous calendar day → the <c>time.yesterday</c>
    /// key; 2–6 local days ago → a <c>time.weekday_*</c> key; <c>≥ 7</c> days →
    /// <c>absolute_epoch_ms</c>, the signal to render a native locale-aware date.
    /// Constants mirror the Rust <c>format::tests</c> exactly (NOW = 2023-11-14
    /// 22:13:20 UTC, a Tuesday) so the buckets are deterministic and locked to the
    /// shared calendar bucketer (priority #1/#2/#4), incl. the offset-shifts-the-day
    /// and calendar-vs-24h cases.
    /// </summary>
    [Fact]
    public void ConversationTimestamp_MatchesSharedRustFormatter()
    {
        const long now = 1_700_000_000_000L; // 2023-11-14 22:13:20 UTC (Tue)
        const long dayMs = 86_400_000L;
        const long hourMs = 3_600_000L;

        // Today (offset 0) → local 24 h wall-clock, zero-padded.
        Assert.Equal("22:13", ValueFormat.ConversationTimestamp(now, now, 0));
        // Earlier the same calendar day → still today's clock.
        Assert.Equal("20:13", ValueFormat.ConversationTimestamp(now, now - 2 * hourMs, 0));
        // UTC+2 shifts the local clock (22:13 UTC → 00:13 next local day, still "today").
        Assert.Equal("00:13", ValueFormat.ConversationTimestamp(now, now, 7200));

        // Previous calendar day → "Yesterday" (calendar-based, not a 24 h window).
        Assert.Equal("Yesterday", ValueFormat.ConversationTimestamp(now, now - dayMs, 0));
        // 2 days ago = 2023-11-12, a Sunday → weekday abbreviation.
        Assert.Equal("Sun", ValueFormat.ConversationTimestamp(now, now - 2 * dayMs, 0));
        // 6 days ago = 2023-11-08, a Wednesday → last day before "older".
        Assert.Equal("Wed", ValueFormat.ConversationTimestamp(now, now - 6 * dayMs, 0));

        // ≥ 7 d → native locale-aware absolute date (absolute_epoch_ms path).
        long old = now - 7 * dayMs;
        Assert.Equal(
            DateTimeOffset.FromUnixTimeMilliseconds(old).LocalDateTime.ToString("d"),
            ValueFormat.ConversationTimestamp(now, old, 0));
    }

    /// <summary>
    /// Nest-provisioning elapsed ticker matches shared Rust
    /// (<c>fauna_provisioning::progress::elapsed_display</c>): <c>null</c> until the
    /// run starts (empty string → row hidden), ms→secs floored against the live
    /// <c>now</c> while running, frozen at <c>finished_at_ms</c> once terminated, and
    /// a backwards clock saturates to 0 (no underflow). Mirrors the Rust
    /// <c>elapsed_display_computes_freezes_and_guards</c> cases — locks windows to the
    /// shared subtraction/freeze/guard (priority #2/#4) rather than re-deriving it.
    /// </summary>
    [Fact]
    public void ProvisioningElapsed_MatchesSharedRustFormatter()
    {
        // Not started → no row (null → empty string).
        Assert.Equal("", ValueFormat.ProvisioningElapsed(null, null, 5_000UL));
        Assert.Equal("", ValueFormat.ProvisioningElapsed(null, 9_000UL, 5_000UL));

        // Running: ticks against now_ms, ms→secs floored.
        Assert.Equal("5s elapsed", ValueFormat.ProvisioningElapsed(1_000UL, null, 6_500UL));

        // Finished: freezes at finished_at_ms, ignoring a later now_ms.
        Assert.Equal("3s elapsed", ValueFormat.ProvisioningElapsed(1_000UL, 4_000UL, 60_000UL));

        // Backwards clock (now < start): saturating → 0, no underflow.
        Assert.Equal("0s elapsed", ValueFormat.ProvisioningElapsed(10_000UL, null, 5_000UL));
    }

    /// <summary>
    /// A multi-arg <see cref="uniffi.fauna_core.LocalizedText"/> carries its args
    /// in a Rust <c>HashMap</c> with NO order guarantee. By-name resolution must
    /// place each value at its named slot regardless of iteration order. The old
    /// positional resolver (<c>string.Format</c> over dict-iteration order against
    /// a positional <c>{0}d {1}h {2}m</c> resw) misordered them — verified
    /// <c>duration_secs(86400)</c> rendering "0d 0h 1m". Args are supplied here in
    /// deliberately non-template (mins, days, hours) order to lock the fix in.
    /// </summary>
    [Fact]
    public void Resolve_MultiArgKey_IsArgOrderIndependent()
    {
        var msg = new uniffi.fauna_core.LocalizedText(
            "time.uptime_dhm",
            new Dictionary<string, string> { ["mins"] = "0", ["days"] = "1", ["hours"] = "0" });

        Assert.Equal("1d 0h 0m", Strings.Resolve(msg));
    }

    /// <summary>
    /// Calendar month/week grid headers (<c>events.md</c> § Layout &amp; flow) must
    /// resolve the weekday abbreviation through the app's own i18n catalog — the
    /// same <c>time/weekday_mon</c>…<c>_sun</c> keys the relative-time "N days ago"
    /// bucket above already exercises via <see cref="FakeLocalizer"/> — not a
    /// hand-rolled English table (the bug this replaces: a hardcoded
    /// <c>{"Sun","Mon",...}</c> array in <c>EventsPage.BuildMonthGrid</c> and
    /// <c>DayOfWeek.ToString()</c> — always English — in <c>BuildWeekGrid</c>).
    /// Pinning delegation to <see cref="Strings.Get"/> (not the OS's native locale)
    /// means a future in-app language switch changes this along with every other
    /// string, independent of whatever language the OS happens to be in.
    /// </summary>
    /// <summary>
    /// Grace-window countdown (mail primary-domain-rename <c>admin-dns-rename</c>
    /// banner) matches shared Rust (<c>grace_countdown</c>): coarse d/h, largest
    /// non-zero unit down to hours, minutes/seconds dropped; <c>null</c> once the
    /// deadline has passed (the caller renders its own already-localized "elapsed"
    /// label — <c>value-formatting.md</c> § Grace countdown). Constants mirror the
    /// Rust <c>format::tests::grace_countdown_*</c> cases exactly.
    /// </summary>
    [Theory]
    [InlineData(1_000L, 1_000L, null)]       // exactly at deadline → elapsed
    [InlineData(1_000L, 2_000L, null)]       // past deadline → elapsed
    [InlineData(1_800_000L, 0L, "0h")]       // 30 min left → sub-hour, zero hours
    [InlineData(18_000_000L, 0L, "5h")]      // 5h left, under a day
    [InlineData(183_600_000L, 0L, "2d 3h")]  // 2d 3h left → days+hours chain
    [InlineData(86_400_000L, 0L, "1d 0h")]   // exact day boundary
    public void GraceCountdown_MatchesSharedRustFormatter(long deadlineMs, long nowMs, string? expected)
    {
        Assert.Equal(expected, ValueFormat.GraceCountdown(deadlineMs, nowMs));
    }

    [Theory]
    [InlineData(DayOfWeek.Sunday, "Sun")]
    [InlineData(DayOfWeek.Monday, "Mon")]
    [InlineData(DayOfWeek.Tuesday, "Tue")]
    [InlineData(DayOfWeek.Wednesday, "Wed")]
    [InlineData(DayOfWeek.Thursday, "Thu")]
    [InlineData(DayOfWeek.Friday, "Fri")]
    [InlineData(DayOfWeek.Saturday, "Sat")]
    public void WeekdayAbbreviation_ResolvesThroughTheI18nCatalog(DayOfWeek day, string expected)
    {
        Assert.Equal(expected, ValueFormat.WeekdayAbbreviation(day));
    }
}
