using System;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the Personalization home's Layer-B
/// signal-sharing facet VM (engagement-cues.md § Layer B), over
/// <see cref="MockNestRpcClient"/>'s <c>SignalShareStatusAsync</c>/
/// <c>SetSignalSharingAsync</c> fixtures — the peer of
/// <c>MailSpamViewModelTests</c>'s report-share section (the identical
/// <see cref="FfiReportShareStatus"/> shape, a small dedicated flow with no
/// machine underneath).
/// </summary>
public class SignalShareViewModelTests
{
    [Fact]
    public async Task Load_ProjectsToggleAndPublishedList()
    {
        var rpc = new MockNestRpcClient
        {
            NextSignalShareStatus = FfiReportShareStatusFixture.Make(
                published: new[] { FfiReportShareStatusFixture.Entry("ab12", "signal:watch-complete", 3u) }),
        };
        var vm = new SignalShareViewModel(rpc);

        await vm.LoadAsync();

        Assert.Contains("SignalShareStatus", rpc.Calls);
        Assert.True(vm.ShareSignals);
        Assert.Single(vm.PublishedSignals);
        Assert.Equal("ab12", vm.PublishedSignals[0].ContentHash);
        Assert.Equal("signal:watch-complete", vm.PublishedSignals[0].Factor);
        Assert.Equal("3", vm.PublishedSignals[0].Count);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task Load_DefaultsOffWithEmptyList()
    {
        var vm = new SignalShareViewModel(new MockNestRpcClient());

        await vm.LoadAsync();

        Assert.False(vm.ShareSignals);
        Assert.Empty(vm.PublishedSignals);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task SetShareSignals_RoundTripsAndRefreshesPublishedList()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SignalShareViewModel(rpc);
        await vm.LoadAsync();
        Assert.False(vm.ShareSignals);

        // The manager's own SetSignalSharing reply already carries the
        // re-read status — a newly-published aggregate (mirrors the toggle
        // crossing the k-anonymity floor) — in ONE round trip (no separate
        // status re-read call, unlike the report-share toggle).
        rpc.NextSignalShareStatus = FfiReportShareStatusFixture.Make(
            published: new[] { FfiReportShareStatusFixture.Entry("cd34", "signal:skip", 5u) });
        await vm.SetShareSignalsAsync(true);

        Assert.Equal(true, rpc.LastSignalSharingSet);
        Assert.True(vm.ShareSignals);
        Assert.Single(vm.PublishedSignals);
        Assert.Equal("cd34", vm.PublishedSignals[0].ContentHash);
        // One round trip: SetSignalSharing carries the re-read status itself,
        // so no separate SignalShareStatus call follows it.
        Assert.Equal(1, rpc.Calls.Count(c => c == "SignalShareStatus"));
        Assert.Equal(1, rpc.Calls.Count(c => c == "SetSignalSharing"));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task SetShareSignals_Failure_RoutesToError()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SignalShareViewModel(rpc);
        await vm.LoadAsync();

        rpc.NextError = "boom";
        await vm.SetShareSignalsAsync(true);

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    [Fact]
    public async Task Load_Failure_RoutesToError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new SignalShareViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }
}
