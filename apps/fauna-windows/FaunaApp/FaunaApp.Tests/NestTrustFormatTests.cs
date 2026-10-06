using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_client_pair;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Nests-page trust facet's shell-side label mapping (docs/goal/ui/nests.md
/// § Where logic lives — "scope + status + history labels are shell-side"). Pure
/// C# (no FFI call), mirroring the linux lead's scope_label/status_label/
/// history_line (apps/fauna-linux/src/settings/linked_nests.rs).
/// </summary>
[Collection("StringsGlobal")]
public class NestTrustFormatTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["nests/scope_mail"] = "Mail",
            ["nests/scope_calendar"] = "Calendar",
            ["nests/scope_posts"] = "Posts",
            ["nests/scope_posts_tier"] = "Posts — {tier}",
            ["nests/scope_spam_labels"] = "Write spam labels",
            ["nests/status_active"] = "Active",
            ["nests/status_expiring"] = "Expiring soon",
            ["nests/status_expired"] = "Paused — renew to resume",
            ["nests/status_auto_renewing"] = "Auto-renewing",
            ["nests/history_minted"] = "Trusted to read {scope} · {when}",
            ["nests/history_renewed"] = "Trust renewed: {scope} · {when}",
            ["nests/history_revoked"] = "Trust revoked: {scope} · {when}",
            ["nests/mint_option_mail"] = "Read and filter my mail",
            ["nests/mint_option_calendar"] = "Read my calendar",
            ["nests/mint_option_paywalled"] = "Serve paywalled posts — {tier}",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public NestTrustFormatTests() => Strings.Initialize(new FakeLocalizer());

    private static TrustScope Scope(string @class, string? kind, string? tier = null) => new(@class, kind, tier);

    [Theory]
    [InlineData("mail", "Mail")]
    [InlineData("calendar", "Calendar")]
    [InlineData("post", "Posts")]
    public void ScopeLabel_MapsKnownContentReadKinds(string kind, string expected)
    {
        Assert.Equal(expected, NestTrustFormat.ScopeLabel(Scope("content.read", kind)));
    }

    [Fact]
    public void ScopeLabel_TierScopedPostNamesTheTier()
    {
        // Two tiers must stay distinguishable in the audit view (mirrors linux's
        // scope_label — the drift the mint flow's live e2e caught: an untiered
        // "Posts" label made a paywalled grant indistinguishable from another tier's).
        Assert.Equal("Posts — gold", NestTrustFormat.ScopeLabel(Scope("content.read", "post", "gold")));
    }

    [Fact]
    public void ScopeLabel_ContentLabelWriteRendersWriteSpamLabels_NeverTheRawClass()
    {
        Assert.Equal("Write spam labels", NestTrustFormat.ScopeLabel(Scope("content.label-write", null)));
    }

    [Fact]
    public void ScopeLabel_FallsBackToRawKindOrClass_ForUnknownScope()
    {
        // An as-yet-unlabeled scope renders honestly (the raw kind/class) rather
        // than blank — mirrors the linux fallback.
        Assert.Equal("event", NestTrustFormat.ScopeLabel(Scope("content.read", "event")));
        Assert.Equal("some.other.class", NestTrustFormat.ScopeLabel(Scope("some.other.class", null)));
    }

    [Fact]
    public void ScopeLine_JoinsMultipleScopesWithCommaSpace()
    {
        var scopes = new[] { Scope("content.read", "mail"), Scope("content.read", "calendar") };
        Assert.Equal("Mail, Calendar", NestTrustFormat.ScopeLine(scopes));
    }

    // TrustLiveness is a UniFFI-internal enum — it can't back a public [Theory]
    // param (CS0051; reference_windows_bindgen... / the FeedPostItemTests
    // precedent). Param-less [Fact]s construct the enum internally instead.
    [Fact]
    public void StatusLabel_MapsEveryLiveness()
    {
        Assert.Equal("Active", NestTrustFormat.StatusLabel(TrustLiveness.Active));
        Assert.Equal("Expiring soon", NestTrustFormat.StatusLabel(TrustLiveness.ExpiringSoon));
        Assert.Equal("Paused — renew to resume", NestTrustFormat.StatusLabel(TrustLiveness.Expired));
        Assert.Equal("Auto-renewing", NestTrustFormat.StatusLabel(TrustLiveness.AutoRenewing));
    }

    [Fact]
    public void FormatUnixLocal_RendersUnixSecondsAsLocalDateTime()
    {
        // 2026-01-02T03:04:00Z — compare against the same conversion the
        // shared formatter uses so the assertion is timezone-independent.
        // NestTrustFormat.HistoryLine now delegates timestamp formatting to
        // the shared FaunaFfiMethods.FormatUnixLocal (value-formatting.md §
        // Absolute local timestamp display) instead of a local duplicate.
        var unix = new DateTimeOffset(2026, 1, 2, 3, 4, 0, TimeSpan.Zero).ToUnixTimeSeconds();
        var expected = DateTimeOffset.FromUnixTimeSeconds(unix).ToLocalTime().ToString("yyyy-MM-dd HH:mm");
        Assert.Equal(expected, FaunaFfiMethods.FormatUnixLocal(unix));
    }

    [Fact]
    public void HistoryLine_MintRendersTrustedToReadWithScopeAndWhen()
    {
        var unix = new DateTimeOffset(2026, 1, 2, 3, 4, 0, TimeSpan.Zero).ToUnixTimeSeconds();
        var when = DateTimeOffset.FromUnixTimeSeconds(unix).ToLocalTime().ToString("yyyy-MM-dd HH:mm");
        var row = new TrustHistoryRow(
            grantId: new byte[] { 1 },
            holder: new byte[] { 2 },
            kind: TrustEventKind.Mint,
            scope: new[] { Scope("content.read", "mail") },
            windowStart: 0,
            windowEnd: 0,
            at: unix);
        Assert.Equal($"Trusted to read Mail · {when}", NestTrustFormat.HistoryLine(row));
    }

    [Fact]
    public void HistoryLine_RenewAndRevokeUseTheirOwnTemplates()
    {
        var unix = new DateTimeOffset(2026, 1, 2, 3, 4, 0, TimeSpan.Zero).ToUnixTimeSeconds();
        var when = DateTimeOffset.FromUnixTimeSeconds(unix).ToLocalTime().ToString("yyyy-MM-dd HH:mm");
        TrustHistoryRow Row(TrustEventKind kind) => new(
            grantId: new byte[] { 1 },
            holder: new byte[] { 2 },
            kind: kind,
            scope: new[] { Scope("content.read", "calendar") },
            windowStart: 0,
            windowEnd: 0,
            at: unix);

        Assert.Equal($"Trust renewed: Calendar · {when}", NestTrustFormat.HistoryLine(Row(TrustEventKind.Renew)));
        Assert.Equal($"Trust revoked: Calendar · {when}", NestTrustFormat.HistoryLine(Row(TrustEventKind.Revoke)));
    }

    // TrustMintUseCase is a UniFFI-internal enum — it can't back a public
    // [Theory] param (CS0051, same rule as TrustLiveness above). Param-less
    // [Fact]s construct each option internally instead.
    [Fact]
    public void MintOptionLabel_MapsMailAndCalendar()
    {
        Assert.Equal("Read and filter my mail",
            NestTrustFormat.MintOptionLabel(new TrustMintOption(TrustMintUseCase.Mail, null, Array.Empty<TrustScope>(), new[] { "web-serve" })));
        Assert.Equal("Read my calendar",
            NestTrustFormat.MintOptionLabel(new TrustMintOption(TrustMintUseCase.Calendar, null, Array.Empty<TrustScope>(), new[] { "web-serve" })));
    }

    [Fact]
    public void MintOptionLabel_PaywalledPostsSubstitutesTheTier()
    {
        var option = new TrustMintOption(TrustMintUseCase.PaywalledPosts, "gold", Array.Empty<TrustScope>(), new[] { "web-serve" });
        Assert.Equal("Serve paywalled posts — gold", NestTrustFormat.MintOptionLabel(option));
    }
}
