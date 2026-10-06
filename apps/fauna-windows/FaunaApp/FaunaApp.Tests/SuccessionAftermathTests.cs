using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// windows' post-succession aftermath driver
/// (<c>succession-aftermath.md</c> § Re-key scope's <c>BackupKey</c> corpus row)
/// — the windows twin of apple's <c>SuccessionAftermath</c>. Pins what this
/// layer actually owns: it <b>calls</b> the shared pass and never throws into
/// the post-auth hook.
///
/// <para>The pass's own ordering and barriers are shared Rust's and are pinned
/// there — re-asserting them here would be the second answer this seam exists to
/// avoid.</para>
/// </summary>
// Shares the process-wide AftermathProgress / InheritedFilterMarks stores that
// RunAsync resets, so it must not interleave with their own tests.
[Collection(AftermathProgressCollection.Name)]
public class SuccessionAftermathTests
{
    [Fact]
    public async Task RunAsync_drives_the_shared_pass()
    {
        var mock = new MockNestRpcClient();

        await SuccessionAftermath.RunAsync(mock);

        Assert.Contains("RunSuccessionAftermath", mock.Calls);
    }

    /// <summary>
    /// The overwhelmingly common answer on an ordinary sign-in, and <b>not</b> an
    /// error: the driver fires unconditionally precisely because the pass no-ops
    /// for an identity that never succeeded.
    /// </summary>
    [Fact]
    public async Task RunAsync_treats_NotASuccessor_as_a_normal_answer()
    {
        var mock = new MockNestRpcClient
        {
            NextAftermathOutcome = FfiAftermathOutcome.NotASuccessor,
        };

        await SuccessionAftermath.RunAsync(mock);

        Assert.Equal(1, mock.Calls.Count(c => c == "RunSuccessionAftermath"));
    }

    /// <summary>
    /// <b>Best-effort by contract.</b> The account is already the successor's, and
    /// refusing a sign-in over a pass that retries would be strictly worse than a
    /// plane that is briefly still owed — so a transport failure must not escape
    /// into the universal post-auth hook, where it would take the whole login
    /// down with it. Same shape as <see cref="SealBackfillSweep"/>.
    /// </summary>
    [Fact]
    public async Task RunAsync_never_throws_into_the_post_auth_hook()
    {
        var mock = new MockNestRpcClient { NextError = "rpc disconnected" };

        await SuccessionAftermath.RunAsync(mock);

        Assert.Contains("RunSuccessionAftermath", mock.Calls);
    }
}
