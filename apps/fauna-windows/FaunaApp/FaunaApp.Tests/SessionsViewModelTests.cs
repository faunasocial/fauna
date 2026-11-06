using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Settings → Sessions (<c>docs/goal/ui/sessions.md</c>) over
/// <see cref="MockNestRpcClient"/>. The VM holds no fold, no sort and no
/// keep-the-painted-rows rule (§ Where logic lives — "app glue is the render and
/// the two confirm states"): these pin that it paints exactly what the face
/// answers, hands the face the page it painted last, owns the two confirm
/// states, and leaves the shell on a landed lock and on nothing else.
///
/// <para>Serialized via <c>StringsGlobal</c>: it registers a localizer, and the
/// painted token ids it publishes for the e2e state are process-global.</para>
/// </summary>
[Collection("StringsGlobal")]
public class SessionsViewModelTests : IDisposable
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    public SessionsViewModelTests()
    {
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["sessions/kind_app"] = "App sign-in",
            ["sessions/kind_device"] = "Device: {name}",
            ["sessions/mark_this_app"] = "This app",
            ["sessions/act_failed"] = "That did not work: {error}",
        }));
        SessionsViewModel.PaintedTokenIdsForTest = null;
    }

    public void Dispose() => SessionsViewModel.PaintedTokenIdsForTest = null;

    private static LocalizedText Text(string key, params (string, string)[] args) =>
        new(key, args.ToDictionary(a => a.Item1, a => a.Item2));

    private static FfiSessionRow Row(string id, bool own = false) => new(
        id,
        own ? Text("sessions.kind_app") : Text("sessions.kind_device", ("name", "laptop")),
        own ? Text("sessions.mark_this_app") : null,
        Text("sessions.detail_literal_" + id),
        1, 2, 3, null,
        !own);

    private static FfiSessionsPage Page(LocalizedText? error, bool locked, params FfiSessionRow[] rows) =>
        new(new FfiSessionsSnapshot(rows, error), locked);

    private static SessionsViewModel Vm(MockNestRpcClient rpc) =>
        new(rpc, devices: () => Task.FromResult<uniffi.fauna_devices_machine.DevicesMachine?>(null),
            ownDeviceId: () => "own-device", confirmWord: "LOCK");

    [Fact]
    public async Task Load_paints_the_faces_rows_in_its_order_and_publishes_their_ids()
    {
        var rpc = new MockNestRpcClient
        {
            NextSessionsPage = Page(null, false, Row("mine", own: true), Row("b"), Row("a")),
        };
        var vm = Vm(rpc);
        Assert.Null(SessionsViewModel.PaintedTokenIdsForTest);

        await vm.LoadAsync();

        Assert.Equal(new[] { "mine", "b", "a" }, vm.Cards.Select(c => c.TokenId));
        Assert.Equal("This app", vm.Cards[0].Mark);
        Assert.False(vm.Cards[0].CanRevoke);
        Assert.Equal("App sign-in", vm.Cards[0].Kind);
        Assert.Equal("Device: laptop", vm.Cards[1].Kind);
        Assert.Null(vm.Cards[1].Mark);
        Assert.True(vm.Cards[1].CanRevoke);
        Assert.Equal(new[] { 0, 1, 2 }, vm.Cards.Select(c => c.Index));
        Assert.Null(vm.PageError);
        Assert.False(vm.ShowEmpty);
        Assert.Equal(new[] { "mine", "b", "a" }, SessionsViewModel.PaintedTokenIdsForTest);
        // The first hydrate has no painted page to hand over; the own device id
        // reaches the face (the This-device rule's fallback).
        Assert.Null(rpc.LastSessionsPrevious);
        Assert.Equal("own-device", rpc.LastSessionsOwnDeviceId);
    }

    [Fact]
    public async Task Every_act_hands_the_face_the_page_painted_last()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false, Row("mine", true), Row("x")) };
        var vm = Vm(rpc);
        await vm.LoadAsync();
        var painted = rpc.NextSessionsPage.snapshot;

        rpc.NextSessionsPage = Page(null, false, Row("mine", true));
        await vm.RevokeAsync("x");

        Assert.Equal("x", rpc.LastSessionsRevokeTokenId);
        Assert.Equal(painted, rpc.LastSessionsPrevious);
        Assert.Equal(new[] { "mine" }, vm.Cards.Select(c => c.TokenId));
    }

    [Fact]
    public async Task A_failed_act_paints_the_faces_error_over_the_rows_it_kept()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false, Row("mine", true), Row("x")) };
        var vm = Vm(rpc);
        await vm.LoadAsync();

        // The face keeps the painted rows on a failed re-read; the VM paints them.
        rpc.NextSessionsPage = Page(Text("sessions.act_failed", ("error", "gone")), false,
            Row("mine", true), Row("x"));
        await vm.RevokeAsync("x");

        Assert.Equal("That did not work: gone", vm.PageError);
        Assert.Equal(new[] { "mine", "x" }, vm.Cards.Select(c => c.TokenId));

        // The next successful act clears it.
        rpc.NextSessionsPage = Page(null, false, Row("mine", true));
        await vm.RevokeAsync("x");
        Assert.Null(vm.PageError);
    }

    [Fact]
    public async Task A_pass_that_cannot_start_says_so_and_keeps_the_rows()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false, Row("mine", true), Row("x")) };
        var vm = Vm(rpc);
        await vm.LoadAsync();

        rpc.NextError = "not connected";
        await vm.RevokeAsync("x");

        Assert.False(string.IsNullOrEmpty(vm.PageError));
        Assert.Equal(new[] { "mine", "x" }, vm.Cards.Select(c => c.TokenId));
        Assert.False(vm.Busy);
    }

    [Fact]
    public async Task Sign_out_everywhere_else_is_two_presses_and_cancel_sends_nothing()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false, Row("mine", true), Row("x")) };
        var vm = Vm(rpc);
        await vm.LoadAsync();

        vm.ArmRevokeOthers();
        Assert.True(vm.RevokeOthersArmed);
        vm.CancelRevokeOthers();
        Assert.False(vm.RevokeOthersArmed);
        Assert.DoesNotContain("SessionsRevokeOthers", rpc.Calls);

        vm.ArmRevokeOthers();
        rpc.NextSessionsPage = Page(null, false, Row("mine", true));
        await vm.ConfirmRevokeOthersAsync();

        Assert.False(vm.RevokeOthersArmed);
        Assert.Single(rpc.Calls, c => c == "SessionsRevokeOthers");
        Assert.Equal(new[] { "mine" }, vm.Cards.Select(c => c.TokenId));
    }

    [Fact]
    public async Task The_lock_arms_on_the_literal_word_only_and_the_typed_word_reaches_the_face()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false, Row("mine", true)) };
        var vm = Vm(rpc);
        await vm.LoadAsync();

        Assert.False(vm.LockEnabled);
        vm.LockWord = "lock";
        Assert.False(vm.LockEnabled);
        vm.LockWord = "LOCK";
        Assert.True(vm.LockEnabled);

        // The face is the action arm's check: a wrong word still goes to it
        // verbatim, and it answers the refusal on the page.
        var locked = 0;
        vm.Locked = () => { locked++; return Task.CompletedTask; };
        vm.LockWord = "lock";
        rpc.NextSessionsPage = Page(Text("sessions.lockout_wrong_word"), false, Row("mine", true));
        await vm.LockAsync();
        Assert.Equal("lock", rpc.LastSessionsLockoutWord);
        Assert.Equal(0, locked);
        Assert.Equal("lock", vm.LockWord);
    }

    [Fact]
    public async Task A_landed_lock_leaves_the_shell_once_and_drops_the_word()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false, Row("mine", true)) };
        var vm = Vm(rpc);
        await vm.LoadAsync();
        var locked = 0;
        vm.Locked = () => { locked++; return Task.CompletedTask; };

        vm.LockWord = "LOCK";
        rpc.NextSessionsPage = Page(null, true, Row("mine", true));
        await vm.LockAsync();

        Assert.Equal("LOCK", rpc.LastSessionsLockoutWord);
        Assert.Equal(1, locked);
        Assert.Equal("", vm.LockWord);
        Assert.False(vm.LockEnabled);
    }

    [Fact]
    public async Task An_empty_reply_shows_the_fail_safe_and_leaving_unpublishes_the_ids()
    {
        var rpc = new MockNestRpcClient { NextSessionsPage = Page(null, false) };
        var vm = Vm(rpc);
        Assert.False(vm.ShowEmpty); // never before the first hydrate

        await vm.LoadAsync();
        Assert.True(vm.ShowEmpty);
        Assert.Empty(SessionsViewModel.PaintedTokenIdsForTest!);

        vm.Leave();
        Assert.Null(SessionsViewModel.PaintedTokenIdsForTest);
    }
}
