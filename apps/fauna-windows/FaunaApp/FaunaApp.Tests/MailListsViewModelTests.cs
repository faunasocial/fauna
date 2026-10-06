using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IMailListsMachine"/> for view-model unit tests. Records
/// dispatched actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw.
/// </summary>
internal sealed class FakeMailListsMachine : MailListsMachineFakeBase
{
    public MailListsSnapshot NextSnapshot { get; set; } = Empty();
    public List<MailListsAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(MailListsAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override MailListsSnapshot Snapshot() => NextSnapshot;

    public static MailListsSnapshot Empty() =>
        new(Array.Empty<ListView>(), Array.Empty<string>(), ListsStatus.Idle, null);

    public static MailListsSnapshot Snap(ListView[] lists, string[] domains, string? error = null) =>
        new(lists, domains, ListsStatus.Idle, error);

    public static ListView Row(string idHex, string name, string localPart, string domain) =>
        new(idHex, name, localPart, domain, $"{localPart}@{domain}", "", 0, null, 0, 0, "", "", null);
}

/// <summary>
/// Deterministic unit tests for the <c>mail-lists</c> page VM, over the
/// <see cref="FakeMailListsMachine"/> (the UniFFI <c>IMailListsMachine</c> seam — the
/// e2e flow made deterministic). Covers the snapshot projection (rows, domain options,
/// add-gating on an owned domain) and that each user action dispatches the right
/// <c>MailListsAction</c> with the draft fields.
/// </summary>
public class MailListsViewModelTests
{
    [Fact]
    public async Task Load_ProjectsListsDomainsAndManage()
    {
        var fake = new FakeMailListsMachine
        {
            NextSnapshot = FakeMailListsMachine.Snap(
                new[] { FakeMailListsMachine.Row("id-1", "Bob's Weekly", "bob-weekly", "example.com") },
                new[] { "example.com", "other.test" }),
        };
        var vm = new MailListsViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Single(vm.Lists);
        Assert.Equal("Bob's Weekly", vm.Lists[0].FriendlyName);
        Assert.Equal("bob-weekly@example.com", vm.Lists[0].Address);
        Assert.Equal(2, vm.LocalDomains.Count);
        Assert.True(vm.CanManage);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Load_NoOwnedDomain_DisablesManage()
    {
        var fake = new FakeMailListsMachine
        {
            NextSnapshot = FakeMailListsMachine.Snap(Array.Empty<ListView>(), Array.Empty<string>()),
        };
        var vm = new MailListsViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.CanManage);
        Assert.Empty(vm.Lists);
    }

    [Fact]
    public async Task Create_DispatchesWithDraftFields()
    {
        var fake = new FakeMailListsMachine
        {
            NextSnapshot = FakeMailListsMachine.Snap(Array.Empty<ListView>(), new[] { "example.com" }),
        };
        var vm = new MailListsViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.CreateAsync("Bob's Weekly", "bob-weekly", "example.com", "desc", "", "", 500);

        Assert.Contains(fake.Dispatched, a =>
            a is MailListsAction.Create { draft.friendlyName: "Bob's Weekly", draft.localPart: "bob-weekly", draft.localDomain: "example.com" });
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task UpdateThenDelete_DispatchTheRightActions()
    {
        var fake = new FakeMailListsMachine
        {
            NextSnapshot = FakeMailListsMachine.Snap(
                new[] { FakeMailListsMachine.Row("id-1", "Old", "news", "example.com") },
                new[] { "example.com" }),
        };
        var vm = new MailListsViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.UpdateAsync("id-1", "New name", "news", "example.com", "now described", "", "", null);
        await vm.DeleteAsync("id-1");

        Assert.Contains(fake.Dispatched, a => a is MailListsAction.Update { listIdHex: "id-1", draft.friendlyName: "New name" });
        Assert.Contains(fake.Dispatched, a => a is MailListsAction.Delete { listIdHex: "id-1" });
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeMailListsMachine
        {
            NextSnapshot = FakeMailListsMachine.Snap(Array.Empty<ListView>(), new[] { "example.com" }),
        };
        var vm = new MailListsViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        fake.NextError = "boom";
        await vm.CreateAsync("X", "x", "example.com", "", "", "", null);

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
