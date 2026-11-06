using System.Collections.Generic;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The author's own withheld conversation message on windows — <c>dm-message-withheld-badge</c>
/// (takedown-and-block.md § Legal takedown → <i>App render</i>, the fourth surface;
/// conversations.md § Element IDs → <i>The author's own withheld message</i>). The lines,
/// the ground and the matched queue row's content id are shared Rust's
/// (<c>MessageSnapshot.withheld</c>); these pin the C# glue: the lines resolve NESTED and
/// join <c>" · "</c>, and a <c>Blocked</c> ground's badge is the bubble's ONE block notice —
/// the beside-body notice then paints nothing, so the bubble never says it twice.
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class WithheldBadgeTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["moderation/report/reason/harassment"] = "Harassment",
            ["moderation/admin_block/notice"] = "Blocked by this nest's admin after a report: {reason}",
            ["moderation/action/taken_down"] = "Removed under legal obligation",
            ["moderation/issued_by"] = "decided by {nest}",
            ["moderation/appeal"] = "Appeal",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public WithheldBadgeTests() => Strings.Initialize(new FakeLocalizer());

    private static LocalizedText Key(string key, Dictionary<string, string>? args = null) =>
        new(key, args ?? new Dictionary<string, string>());

    private const string BlockNotice = "Blocked by this nest's admin after a report: Harassment";

    /// The badge a blocked copy carries: its ground line IS the block notice with the
    /// reason (shared <c>admin_block_notice</c>, whose argument is itself a key).
    private static WithheldOwnMessage Blocked(string contentId) =>
        new(contentId, WithheldGround.Blocked, null,
            new[] { FaunaFfiMethods.AdminBlockNotice("harassment"), Key("moderation.appeal") });

    private static WithheldOwnMessage TakenDown(string contentId, string? issuer = null)
    {
        var lines = new List<LocalizedText> { Key("moderation.action.taken_down") };
        if (issuer is not null)
            lines.Add(Key("moderation.issued_by", new Dictionary<string, string> { ["nest"] = issuer }));
        lines.Add(Key("moderation.appeal"));
        return new(contentId, WithheldGround.TakenDown, issuer, lines.ToArray());
    }

    [Fact]
    public void AMessageTheNestWithholdsNothingOf_HasNoBadge()
    {
        Assert.Null(WithheldBadgeView.From(null));
    }

    [Fact]
    public void TheLinesResolveNested_AndJoinSideBySide()
    {
        var badge = WithheldBadgeView.From(Blocked("ab12"))!;

        Assert.Equal("ab12", badge.ContentId);
        Assert.Equal(BlockNotice + " · Appeal", badge.Text);
    }

    [Fact]
    public void AnotherNestsDecision_NamesItBetweenTheGroundAndTheAppeal()
    {
        var badge = WithheldBadgeView.From(TakenDown("cd34", "other.example"))!;

        // The domain argument is a value, not a key: it reaches the line as written.
        Assert.Equal("Removed under legal obligation · decided by other.example · Appeal", badge.Text);
        Assert.EndsWith("Appeal", badge.Text);
    }

    [Fact]
    public void ABlockedGround_IsTheOneNotice_TheBesideBodyNoticePaintsNothing()
    {
        var badge = WithheldBadgeView.From(Blocked("ab12"));

        Assert.True(badge!.CarriesBlockNotice);
        Assert.Null(WithheldBadgeView.BesideBlockNotice(BlockNotice, badge));
        // Said once — by the badge.
        Assert.Contains(BlockNotice, badge.Text);
    }

    [Fact]
    public void ATakedownBadge_LeavesAKeptBlockNoticeBesideTheBody()
    {
        var badge = WithheldBadgeView.From(TakenDown("cd34"));

        Assert.False(badge!.CarriesBlockNotice);
        Assert.Equal(BlockNotice, WithheldBadgeView.BesideBlockNotice(BlockNotice, badge));
        Assert.DoesNotContain(BlockNotice, badge.Text);
    }

    [Fact]
    public void NoBadge_TheBlockNoticePaintsBesideTheBodyAsBefore()
    {
        Assert.Equal(BlockNotice, WithheldBadgeView.BesideBlockNotice(BlockNotice, null));
        Assert.Null(WithheldBadgeView.BesideBlockNotice(null, null));
    }
}
