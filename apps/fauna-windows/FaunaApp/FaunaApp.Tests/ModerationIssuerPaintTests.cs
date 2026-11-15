using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// A queue row another nest delivered names where the decision was made
/// (<c>moderation.md</c> § Legal takedown → <i>The author homed on another nest</i>;
/// § Element IDs → <i>Queue row facts</i>): the action line carries the shared
/// <c>issued_by</c> suffix after the action label (<c>"{label} · {issued_by}"</c>, as tui
/// paints it — the wording is the shared fold's, never derived here from
/// <c>issuer_nest</c>), and the <c>content-label-badge</c>'s packed HelpText gains the
/// <c>issuer</c> fact beside <c>confidence</c>/<c>action</c>. This nest's own decision
/// names none. The rows go through the REAL native <c>FaunaFfiMethods.ModerationQueue</c>
/// fold, so the suffix is the one the shared Rust fills from the wire's
/// <c>issuer_nest</c>. The windows twin of android's <c>ModerationIssuerPaintTest</c>
/// and FaunaKit's <c>badgeFactsOfADeliveredRowNameTheIssuer</c>.
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ModerationIssuerPaintTests
{
    private const string TakenDownLabel = "Removed under legal obligation";
    private const byte TakenDown = 7;

    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["moderation/action/taken_down"] = TakenDownLabel,
            ["moderation/queue/issued_by"] = "decided by {nest}",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ModerationIssuerPaintTests() => Strings.Initialize(new FakeLocalizer());

    // FfiObligationAction(id, content_type, content_id, category, confidence_per_mille, action, timestamp, issuer_nest)
    private static FfiObligationAction ServerRow(string contentId, long ts, string? issuerNest) =>
        new(0L, "post", contentId, "illegal", 1000, TakenDown, ts, issuerNest);

    [Fact]
    public async Task ADeliveredRow_NamesItsIssuer_OnTheActionLineAndTheBadge_AnOwnRowNamesNone()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction>
            {
                ServerRow("aa", ts: 200, issuerNest: "issuer.example"),
                ServerRow("bb", ts: 100, issuerNest: null),
            },
        };
        var vm = new ModerationViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(2, vm.Actions.Count);

        var delivered = vm.Actions[0];
        Assert.Equal("aa", delivered.ContentId);
        // The `action` fact stays the bare label; the painted line carries the suffix.
        Assert.Equal(TakenDownLabel, delivered.ActionLabel);
        Assert.Equal($"{TakenDownLabel} · decided by issuer.example", delivered.ActionLine);
        Assert.Equal(
            AutomationAttributePack.Pack(
                ("category", "illegal"), ("confidence", "100%"),
                ("action", TakenDownLabel), ("issuer", "issuer.example")),
            delivered.BadgeAttributes);

        var own = vm.Actions[1];
        Assert.Equal("bb", own.ContentId);
        Assert.Equal(TakenDownLabel, own.ActionLine);
        Assert.DoesNotContain("issuer=", own.BadgeAttributes);
    }

    [Fact]
    public void TheActionLine_IsBlankWithoutAnAction_AndTheBareLabelWithoutAnIssuer()
    {
        Assert.Equal(string.Empty, ModerationViewModel.ActionLine(null, "decided by issuer.example"));
        Assert.Equal(TakenDownLabel, ModerationViewModel.ActionLine(TakenDownLabel, null));
        Assert.Equal($"{TakenDownLabel} · decided by issuer.example",
            ModerationViewModel.ActionLine(TakenDownLabel, "decided by issuer.example"));
    }
}
