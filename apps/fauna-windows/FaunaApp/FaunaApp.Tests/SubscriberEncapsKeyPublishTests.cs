using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="SubscriberEncapsKeyPublish.PublishAsync"/>: it calls the shared
/// publish once per session start and swallows any failure (best-effort, log-only;
/// <c>post-quantum.md</c> § surface B).
/// </summary>
public class SubscriberEncapsKeyPublishTests
{
    [Fact]
    public async Task Publishes_once_through_the_rpc_client()
    {
        var rpc = new MockNestRpcClient();
        await SubscriberEncapsKeyPublish.PublishAsync(rpc);
        Assert.Equal(new[] { "SubscriptionPublishEncapsKey" }, rpc.Calls);
    }

    [Fact]
    public async Task A_failed_publish_never_throws()
    {
        var rpc = new MockNestRpcClient { NextError = "nest unreachable" };
        await SubscriberEncapsKeyPublish.PublishAsync(rpc);
        Assert.Contains("SubscriptionPublishEncapsKey", rpc.Calls);
    }
}
