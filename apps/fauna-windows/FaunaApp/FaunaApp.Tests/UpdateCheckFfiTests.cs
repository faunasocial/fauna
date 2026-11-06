using System.Collections.Generic;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows newer-version check over the REAL shared update look
/// (<c>installers/README.md</c> § Knowing a newer version is out). The asked check
/// drives <c>FaunaFfiMethods.CheckForNewerRelease</c> — the native dll loads in the
/// test host — against a loopback feed serving one answer, the same shape as
/// <c>libs/fauna-ffi/src/version.rs</c>'s own tests: a newer release paints the notice,
/// the same or an older one paints none, and a failure reads as failed, never as "up
/// to date". The sign-in look reads the signed-in nest
/// (<c>INestRpcClient.LookForNewerReleaseAtSignInAsync</c>, over
/// <c>FfiNestClient.LookForNewerReleaseAtSignIn</c>), so its tests hand it a
/// <see cref="MockNestRpcClient"/> and point the feed at a closed origin.
///
/// <para>[Collection("StringsGlobal")]: the label and notice resolve through the
/// process-global <see cref="Strings"/>; this class installs its own
/// <see cref="FakeLocalizer"/> carrying the shared en strings.</para>
/// </summary>
[Collection("StringsGlobal")]
public class UpdateCheckFfiTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["settings/check_for_updates"] = "Check for Updates",
            ["common/checking"] = "Checking...",
            ["settings/up_to_date"] = "Up to date",
            ["settings/check_failed"] = "Check failed",
            ["settings/general_page/update_available"] = "{version} available",
            ["settings/update_available_notice"] = "Version {version} is available. Get it at {url}",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public UpdateCheckFfiTests() => Strings.Initialize(new FakeLocalizer());

    private const string ReleasePage = "https://github.com/faunasocial/fauna/releases/tag/v9.9.9";

    private static string Running => FaunaFfiMethods.FaunaFfiBuildVersion();

    /// <summary>A check whose answer the label holds for good, so the test reads it.</summary>
    private static UpdateCheck Over(string feedOrigin) => new(feedOrigin, Timeout.InfiniteTimeSpan);

    /// <summary>Serve one HTTP answer from a loopback listener; return its origin.</summary>
    private static string ServeOnce(string body)
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        _ = Task.Run(async () =>
        {
            try
            {
                using var client = await listener.AcceptTcpClientAsync();
                using var stream = client.GetStream();
                var request = new byte[4096];
                _ = await stream.ReadAsync(request);
                var payload = Encoding.UTF8.GetBytes(body);
                var head = Encoding.ASCII.GetBytes(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n"
                    + $"content-length: {payload.Length}\r\nconnection: close\r\n\r\n");
                await stream.WriteAsync(head);
                await stream.WriteAsync(payload);
            }
            finally
            {
                listener.Stop();
            }
        });
        return $"http://127.0.0.1:{port}";
    }

    /// <summary>An origin nothing listens on: bound, then released.</summary>
    private static string ClosedOrigin()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        return $"http://127.0.0.1:{port}";
    }

    private static string Advertising(string tag) => $"{{\"tag_name\":\"{tag}\"}}";

    [Fact]
    public async Task AskedCheck_ANewerRelease_PaintsTheNoticeAndTheBareVersionLabel()
    {
        var check = Over(ServeOnce(Advertising("v9.9.9")));
        await check.CheckAsync();

        Assert.Equal(UpdateCheck.CheckPhase.Newer, check.Phase);
        Assert.Equal("9.9.9 available", check.ButtonLabel);
        Assert.Equal($"Version 9.9.9 is available. Get it at {ReleasePage}", check.Notice);
    }

    [Fact]
    public async Task AskedCheck_TheRunningVersion_IsUpToDateAndPaintsNoNotice()
    {
        var check = Over(ServeOnce(Advertising($"v{Running}")));
        await check.CheckAsync();

        Assert.Equal(UpdateCheck.CheckPhase.UpToDate, check.Phase);
        Assert.Equal("Up to date", check.ButtonLabel);
        Assert.Null(check.Notice);
    }

    [Fact]
    public async Task AskedCheck_AnOlderRelease_IsUpToDateAndPaintsNoNotice()
    {
        var check = Over(ServeOnce(Advertising("v0.0.1")));
        await check.CheckAsync();

        Assert.Equal(UpdateCheck.CheckPhase.UpToDate, check.Phase);
        Assert.Null(check.Notice);
    }

    [Fact]
    public async Task AskedCheck_AFailedRoundTrip_ReadsAsFailedNeverUpToDate()
    {
        var check = Over(ClosedOrigin());
        await check.CheckAsync();

        Assert.Equal(UpdateCheck.CheckPhase.Failed, check.Phase);
        Assert.Equal("Check failed", check.ButtonLabel);
        Assert.Null(check.Notice);
    }

    // ── The sign-in look: the signed-in nest's newest_release, never the feed ──
    //
    // installers/README.md § Knowing a newer version is out → *The nest's
    // newest-release field*: the look reads the nest the app just signed in to,
    // through FfiNestClient.LookForNewerReleaseAtSignIn (shared Rust decides
    // "newer"). Every check below sits over a CLOSED feed origin, so a notice can
    // only have come from the nest.

    private static NewerRelease Release(string version) =>
        new($"v{version}", version, $"https://github.com/faunasocial/fauna/releases/tag/v{version}");

    private static string NoticeFor(string version) =>
        $"Version {version} is available. Get it at https://github.com/faunasocial/fauna/releases/tag/v{version}";

    [Fact]
    public async Task SignInLook_TheNestReportsANewerRelease_PaintsTheSameNoticeAndLeavesTheButtonAlone()
    {
        var check = Over(ClosedOrigin());
        var nest = new MockNestRpcClient { NextNewerReleaseAtSignIn = Release("9.9.9") };
        await check.LookOnceAtSignInAsync(nest);

        Assert.Equal($"Version 9.9.9 is available. Get it at {ReleasePage}", check.Notice);
        Assert.Equal(UpdateCheck.CheckPhase.Idle, check.Phase);
        Assert.Equal("Check for Updates", check.ButtonLabel);
        Assert.Contains("LookForNewerReleaseAtSignIn", nest.Calls);
    }

    [Fact]
    public async Task SignInLook_TheNestSaysNothingNewer_PaintsNothing()
    {
        var check = Over(ClosedOrigin());
        await check.LookOnceAtSignInAsync(new MockNestRpcClient());

        Assert.Null(check.Notice);
    }

    [Fact]
    public async Task SignInLook_AFailedLook_IsSilent()
    {
        var check = Over(ClosedOrigin());
        await check.LookOnceAtSignInAsync(new MockNestRpcClient { NextError = "disconnected" });

        Assert.Null(check.Notice);
        Assert.Equal(UpdateCheck.CheckPhase.Idle, check.Phase);
    }

    [Fact]
    public async Task SignInLook_SeveralNests_TheNoticeOnlyMovesUp()
    {
        var check = Over(ClosedOrigin());
        await check.LookOnceAtSignInAsync(new MockNestRpcClient { NextNewerReleaseAtSignIn = Release("9.9.9") });
        Assert.Equal(NoticeFor("9.9.9"), check.Notice);

        // A later nest that knows of an older release, or of none, never moves the
        // notice down or away (fauna_client::update_look::keep_the_newer's rule).
        await check.LookOnceAtSignInAsync(new MockNestRpcClient { NextNewerReleaseAtSignIn = Release("9.9.8") });
        Assert.Equal(NoticeFor("9.9.9"), check.Notice);
        await check.LookOnceAtSignInAsync(new MockNestRpcClient());
        Assert.Equal(NoticeFor("9.9.9"), check.Notice);

        // One that knows of a newer one moves it up.
        await check.LookOnceAtSignInAsync(new MockNestRpcClient { NextNewerReleaseAtSignIn = Release("10.0.0") });
        Assert.Equal(NoticeFor("10.0.0"), check.Notice);
    }

    [Fact]
    public void RunningVersion_IsTheOneWorkspaceVersion()
    {
        Assert.Equal(Running, Over(ClosedOrigin()).RunningVersion);
    }
}
