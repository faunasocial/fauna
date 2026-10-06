using System;
using FaunaApp.Core.Calendar;
using Xunit;

namespace FaunaApp.Tests;

// All-day classification moved to the shared FFI (FaunaFfiMethods.EventIsAllDay,
// libs/fauna-ffi/src/caltime.rs — covered there); only TryParseEventDateTime
// stays a .NET-side concern.
public class EventTimeUtilTests
{
    // TryParseEventDateTime: accept BOTH extended ISO-8601 (what a .NET picker /
    // human types) AND the **basic/compact** ISO form that build_vevent + the
    // shared CalDAV path emit (events.md; the linux Events form already accepts
    // it). The fallback below is what unblocks test_caldav_client_seal_to_mua on
    // --client windows.
    private static readonly DateTimeOffset _Fb = new(2030, 1, 1, 0, 0, 0, TimeSpan.Zero);

    [Fact]
    public void BasicIsoUtcParsesToCorrectInstant()
    {
        // %Y%m%dT%H%M00Z — the exact format test_caldav_client_seal_to_mua._utc() emits.
        Assert.True(EventTimeUtil.TryParseEventDateTime("20260626T154300Z", _Fb, out var r));
        Assert.Equal(new DateTimeOffset(2026, 6, 26, 15, 43, 0, TimeSpan.Zero), r);
    }

    [Fact]
    public void BasicIsoUtcNoSecondsParses()
    {
        Assert.True(EventTimeUtil.TryParseEventDateTime("20260626T1543Z", _Fb, out var r));
        Assert.Equal(new DateTimeOffset(2026, 6, 26, 15, 43, 0, TimeSpan.Zero), r);
    }

    [Fact]
    public void ExtendedIsoUtcStillParses()
    {
        Assert.True(EventTimeUtil.TryParseEventDateTime("2026-06-26T15:43:00Z", _Fb, out var r));
        Assert.Equal(new DateTimeOffset(2026, 6, 26, 15, 43, 0, TimeSpan.Zero), r);
    }

    [Fact]
    public void ExtendedIsoLocalStillParses() =>
        Assert.True(EventTimeUtil.TryParseEventDateTime("2026-06-05T14:30", _Fb, out _));

    [Fact]
    public void EmptyYieldsFallback()
    {
        Assert.True(EventTimeUtil.TryParseEventDateTime("", _Fb, out var r));
        Assert.Equal(_Fb, r);
    }

    [Fact]
    public void WhitespaceYieldsFallback()
    {
        Assert.True(EventTimeUtil.TryParseEventDateTime("   ", _Fb, out var r));
        Assert.Equal(_Fb, r);
    }

    [Fact]
    public void GarbageReturnsFalse() =>
        Assert.False(EventTimeUtil.TryParseEventDateTime("not-a-date", _Fb, out _));
}
