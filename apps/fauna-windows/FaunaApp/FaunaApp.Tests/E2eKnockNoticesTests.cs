#if DEBUG || FAUNA_E2E_AGENT
using System.Linq;
using System.Text.Json;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The knock-toast log windows publishes as <c>state.knock_notices</c>
/// (<c>fauna_e2e_agent::KNOCK_NOTICES_KEY</c>, <c>notifications</c> outcome 15):
/// <c>{"seen": N, "fired": [{"key", "text", "replace_id"}]}</c>, built by the shared
/// recorder (<c>fauna-ffi</c>'s <c>FfiKnockNoticesForTest</c> over
/// <c>fauna_e2e_contract::KnockNotices</c>) — so windows logs with the same code as
/// tui, linux, android, web and apple. The log is process-wide, so each test reads
/// what ITS calls added rather than absolute numbers.
/// </summary>
public class E2eKnockNoticesTests
{
    private static JsonElement Log() =>
        JsonDocument.Parse(E2eLoudSurfaces.KnockNoticesJson()).RootElement;

    [Fact]
    public void AnObservedKnock_CountsAsSeen_AndOnlyARecordedToastIsFired()
    {
        var seenBefore = Log().GetProperty("seen").GetInt64();
        var firedBefore = Log().GetProperty("fired").GetArrayLength();

        // Two pushes reach the arm; one is withheld (a muted word), one is shown.
        E2eLoudSurfaces.ObserveKnockNotice();
        E2eLoudSurfaces.ObserveKnockNotice();
        E2eLoudSurfaces.RecordKnockNotice("notifications.row_knock", "probe-e2e-knock-log", 1003);

        var log = Log();
        Assert.Equal(seenBefore + 2, log.GetProperty("seen").GetInt64());
        var fired = log.GetProperty("fired").EnumerateArray().ToList();
        Assert.Equal(firedBefore + 1, fired.Count);
        var mine = Assert.Single(fired, e => e.GetProperty("text").GetString() == "probe-e2e-knock-log");
        Assert.Equal("notifications.row_knock", mine.GetProperty("key").GetString());
        Assert.Equal(1003, mine.GetProperty("replace_id").GetInt64());
    }
}
#endif
