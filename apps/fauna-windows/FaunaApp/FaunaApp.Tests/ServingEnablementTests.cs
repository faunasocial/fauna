using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The post-claim serving enablement's refusal reaches the page
/// (<c>docs/goal/behavior/onboarding.md</c> § 3b → <i>The step completes; it is not
/// fired once</i>): a step the nest refuses for good ends the shared step, which
/// answers the page's <c>error-message</c> text, and <see cref="ServingEnablement"/>
/// hands it back for <c>App</c> to paint on the current page — never only logged.
/// A failure that never reached the shared step answers nothing and is not thrown
/// into the handoff.
/// </summary>
public class ServingEnablementTests
{
    private const string NodeUrl = "https://nest.example.test";

    [Fact]
    public async Task ARefusalForGood_IsHandedBack_ForTheCurrentPagesErrorMessage()
    {
        const string refusal = "The nest refused to turn on CalDAV: not an admin.";
        var rpc = new MockNestRpcClient { NextServingEnablementRefusal = refusal };

        var shown = await ServingEnablement.ApplyAsync(rpc, NodeUrl, true, true, false, false);

        Assert.Equal(refusal, shown);
        Assert.Contains($"ApplyPostClaimServingEnablement({NodeUrl},True,True,False,False)", rpc.Calls);
    }

    [Fact]
    public async Task ACompletedRun_HandsBackNothing()
    {
        var rpc = new MockNestRpcClient();

        var shown = await ServingEnablement.ApplyAsync(rpc, NodeUrl, false, false, false, false);

        Assert.Null(shown);
        Assert.Single(rpc.Calls.Where(c => c.StartsWith("ApplyPostClaimServingEnablement(")));
    }

    [Fact]
    public async Task AFailureBeforeTheStep_IsNotThrownIntoTheHandoff_AndPaintsNothing()
    {
        var rpc = new MockNestRpcClient { ServingEnablementThrows = true };

        var shown = await ServingEnablement.ApplyAsync(rpc, NodeUrl, true, false, false, false);

        Assert.Null(shown);
    }
}
