using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailListMembersMachine"/> for view-model unit tests. Records
/// dispatched actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw.
/// </summary>
internal sealed class FakeMailListMembersMachine : MailListMembersMachineFakeBase
{
    public MailListMembersSnapshot NextSnapshot { get; set; } = Empty();
    public List<MailListMembersAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(MailListMembersAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override MailListMembersSnapshot Snapshot() => NextSnapshot;

    public static MailListMembersSnapshot Empty() =>
        new("00", "Bob's Weekly", Array.Empty<MemberView>(), 0, 0, ListsStatus.Idle, null, null);

    public static MailListMembersSnapshot Snap(
        MemberView[] members, uint subscribed, uint unsubscribed, string? error = null) =>
        new("00", "Bob's Weekly", members, subscribed, unsubscribed, ListsStatus.Idle, error, null);

    public static MemberView Member(string address, MemberStatus status) =>
        new(address, 1_700_000_000_000, status);
}

/// <summary>
/// Deterministic unit tests for the <c>mail-list-members</c> page VM, over the
/// <see cref="FakeMailListMembersMachine"/> (the UniFFI <c>IMailListMembersMachine</c>
/// seam). Covers the snapshot projection (rows, summary, subscribe-status gating) and
/// that each user action dispatches the right <c>MailListMembersAction</c>.
/// </summary>
public class MailListMembersViewModelTests
{
    [Fact]
    public async Task Load_ProjectsMembersAndSummary()
    {
        var fake = new FakeMailListMembersMachine
        {
            NextSnapshot = FakeMailListMembersMachine.Snap(
                new[]
                {
                    FakeMailListMembersMachine.Member("a@x.example", MemberStatus.Subscribed),
                    FakeMailListMembersMachine.Member("b@y.example", MemberStatus.Unsubscribed),
                },
                subscribed: 1, unsubscribed: 1),
        };
        var vm = new MailListMembersViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Equal(2, vm.Members.Count);
        Assert.Equal("a@x.example", vm.Members[0].Address);
        Assert.True(vm.Members[0].IsSubscribed);
        Assert.False(vm.Members[1].IsSubscribed);
        // Summary reads the i18n summary_fmt key (no localizer in the test host → the key
        // surfaces; the {subscribed}/{unsubscribed} substitution runs against the real resw
        // value at runtime). Confirms the VM populated the summary from the right key.
        Assert.Equal("mail_lists/summary_fmt", vm.Summary);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task AddMember_And_BatchImport_Dispatch()
    {
        var fake = new FakeMailListMembersMachine();
        var vm = new MailListMembersViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AddMemberAsync("a@x.example");
        await vm.BatchImportAsync("b@y.example\nc@z.example");

        Assert.Contains(fake.Dispatched, a => a is MailListMembersAction.AddMember { address: "a@x.example" });
        Assert.Contains(fake.Dispatched, a => a is MailListMembersAction.BatchImport { addresses: "b@y.example\nc@z.example" });
    }

    [Fact]
    public async Task UnsubscribeThenResubscribe_Dispatch()
    {
        var fake = new FakeMailListMembersMachine
        {
            NextSnapshot = FakeMailListMembersMachine.Snap(
                new[] { FakeMailListMembersMachine.Member("a@x.example", MemberStatus.Subscribed) },
                subscribed: 1, unsubscribed: 0),
        };
        var vm = new MailListMembersViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.UnsubscribeAsync("a@x.example");
        await vm.ResubscribeAsync("a@x.example");

        Assert.Contains(fake.Dispatched, a => a is MailListMembersAction.Unsubscribe { address: "a@x.example" });
        Assert.Contains(fake.Dispatched, a => a is MailListMembersAction.Resubscribe { address: "a@x.example" });
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeMailListMembersMachine();
        var vm = new MailListMembersViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        fake.NextError = "boom";
        await vm.AddMemberAsync("a@x.example");

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
