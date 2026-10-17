using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The mail-settings Forwarding section's view-model (mail-forwarding.md § Per-account
/// "forward all", § Per-account forward rate-limit): the shared draft parsers decide
/// first, a refused draft sends nothing, and each field is the value the nest kept.
/// </summary>
public class MailForwardingViewModelTests
{
    [Fact]
    public async Task Load_ProjectsAddressLimitAndHint()
    {
        var rpc = new MockNestRpcClient { NextForwardAllTo = "me@elsewhere.example", NextForwardPerHour = new(40, 250) };
        var vm = new MailForwardingViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal("me@elsewhere.example", vm.ForwardAllToText);
        Assert.Equal("40", vm.ForwardPerHourText);
        // The test host has no resw loaded, so the hint is the key fallback; non-empty
        // proves the shared hint was resolved off the ceiling read.
        Assert.False(string.IsNullOrEmpty(vm.ForwardPerHourHint));
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task CommitForwardAllTo_SetsTheTrimmedAddress()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailForwardingViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitForwardAllToAsync("  me@elsewhere.example ");

        Assert.Equal("me@elsewhere.example", rpc.LastForwardAllToSet);
        Assert.Equal("me@elsewhere.example", vm.ForwardAllToText);
    }

    [Fact]
    public async Task CommitForwardAllTo_BlankClearsForwarding()
    {
        var rpc = new MockNestRpcClient { NextForwardAllTo = "me@elsewhere.example" };
        var vm = new MailForwardingViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitForwardAllToAsync("");

        Assert.Equal(1, rpc.ForwardAllToSetCalls);
        Assert.Null(rpc.LastForwardAllToSet);
        Assert.Equal(string.Empty, vm.ForwardAllToText);
    }

    [Fact]
    public async Task CommitForwardAllTo_MalformedIsRefusedAndNothingIsSent()
    {
        var rpc = new MockNestRpcClient { NextForwardAllTo = "me@elsewhere.example" };
        var vm = new MailForwardingViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitForwardAllToAsync("me@nodot");

        Assert.Equal(0, rpc.ForwardAllToSetCalls);
        Assert.False(string.IsNullOrEmpty(vm.Error));
        Assert.Equal("me@elsewhere.example", vm.ForwardAllToText);
    }

    [Fact]
    public async Task CommitForwardPerHour_SetsAValueUnderTheCeiling()
    {
        var rpc = new MockNestRpcClient { NextForwardPerHour = new(100, 500) };
        var vm = new MailForwardingViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitForwardPerHourAsync("25");

        Assert.Equal((uint?)25, rpc.LastForwardPerHourSet);
        Assert.Equal("25", vm.ForwardPerHourText);
    }

    [Theory]
    [InlineData("0")]
    [InlineData("501")]
    [InlineData("many")]
    public async Task CommitForwardPerHour_RefusedDraftsSendNothing(string draft)
    {
        var rpc = new MockNestRpcClient { NextForwardPerHour = new(100, 500) };
        var vm = new MailForwardingViewModel(rpc);
        await vm.LoadAsync();

        await vm.CommitForwardPerHourAsync(draft);

        Assert.Equal(0, rpc.ForwardPerHourSetCalls);
        Assert.False(string.IsNullOrEmpty(vm.Error));
        Assert.Equal("100", vm.ForwardPerHourText);
    }

    [Fact]
    public async Task CommitForwardPerHour_IsInertBeforeTheFirstReadResolves()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MailForwardingViewModel(rpc);

        await vm.CommitForwardPerHourAsync("25");

        Assert.Equal(0, rpc.ForwardPerHourSetCalls);
        Assert.Null(vm.Error);
    }
}
