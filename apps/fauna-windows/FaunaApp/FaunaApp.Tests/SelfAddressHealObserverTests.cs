using System.Collections.Generic;
using FaunaApp.Core.Services;
using uniffi.fauna_conversations;
using uniffi.fauna_launch_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// conversations.md § State & data shape → *Self-address: live, never baked* — the windows
/// heal leg. Windows had no live channel at all for a mid-session identity change
/// (server-side handle rename, or a resolve that lands after the session was already built)
/// to reach the already-built <c>ConversationsSession</c>
/// — every <c>LaunchMachine</c> construction site wired a <see cref="NullLaunchObserver"/>.
/// <see cref="SelfAddressHealObserver"/> is the real observer: mirrors linux's
/// <c>IdentityRefreshed</c> handler / tui's <c>SelfAddressRefreshed</c> arm / android's
/// <c>AppLaunchVM.applyIdentity</c> (the reference native consumer of the same
/// <c>LaunchSnapshot.identity</c> channel).
///
/// <para>These pin the observer's pure decision logic (apply / skip-while-unresolved /
/// dedup / apply-on-late-attach) against fakes — the surrounding <c>App.xaml.cs</c> wiring
/// (which construction site owns the long-lived authenticated session) isn't unit-testable
/// without a live nest.</para>
/// </summary>
public class SelfAddressHealObserverTests
{
    private sealed class FakeMachine : LaunchMachineFakeBase
    {
        public LaunchSnapshot SnapshotToReturn = new(
            new LaunchPhase.Online(), new TokenStatus.None(), null, null, false, null, null);

        public override LaunchSnapshot Snapshot() => SnapshotToReturn;
    }

    private sealed class FakeSession : ConversationsSessionFakeBase
    {
        public readonly List<string> SetSelfAddressCalls = new();

        public override void SetSelfAddress(string selfAddress) => SetSelfAddressCalls.Add(selfAddress);
    }

    private static LaunchSnapshot SnapshotWithIdentity(string handle, string domain) =>
        new(new LaunchPhase.Online(), new TokenStatus.None(), null, null, false,
            new LaunchIdentity(handle, domain, "member"), null);

    [Fact]
    public void AttachingASessionAppliesAnAlreadyResolvedIdentity()
    {
        var machine = new FakeMachine { SnapshotToReturn = SnapshotWithIdentity("alice", "example.com") };
        var session = new FakeSession();
        var observer = new SelfAddressHealObserver { Machine = machine };

        observer.AttachSession(session);

        Assert.Equal(new[] { "alice@example.com" }, session.SetSelfAddressCalls);
    }

    [Fact]
    public void AnUnresolvedIdentityNeverHealsTheSession()
    {
        var machine = new FakeMachine(); // identity: null — never resolved
        var session = new FakeSession();
        var observer = new SelfAddressHealObserver { Machine = machine };

        observer.AttachSession(session);
        observer.OnChanged();

        Assert.Empty(session.SetSelfAddressCalls);
    }

    [Fact]
    public void OnChangedHealsAgainAfterALateHandleRename()
    {
        var machine = new FakeMachine { SnapshotToReturn = SnapshotWithIdentity("alice", "example.com") };
        var session = new FakeSession();
        var observer = new SelfAddressHealObserver { Machine = machine };
        observer.AttachSession(session);

        machine.SnapshotToReturn = SnapshotWithIdentity("alice2", "example.com");
        observer.OnChanged();

        Assert.Equal(new[] { "alice@example.com", "alice2@example.com" }, session.SetSelfAddressCalls);
    }

    [Fact]
    public void OnChangedSkipsARedundantIdenticalAddress()
    {
        // A token refresh (or any other unrelated snapshot field) also fires OnChanged —
        // the observer must not re-push the same address every tick.
        var machine = new FakeMachine { SnapshotToReturn = SnapshotWithIdentity("alice", "example.com") };
        var session = new FakeSession();
        var observer = new SelfAddressHealObserver { Machine = machine };
        observer.AttachSession(session);

        observer.OnChanged();

        Assert.Equal(new[] { "alice@example.com" }, session.SetSelfAddressCalls);
    }

    [Fact]
    public void OnChangedBeforeASessionExistsIsANoOpAndNeverThrows()
    {
        // The observer is constructed (and the machine started) before StartMainAppAsync
        // builds the ConversationsSession — a resolve racing that gap must not crash.
        var machine = new FakeMachine { SnapshotToReturn = SnapshotWithIdentity("alice", "example.com") };
        var observer = new SelfAddressHealObserver { Machine = machine };

        observer.OnChanged();

        var session = new FakeSession();
        observer.AttachSession(session);
        Assert.Equal(new[] { "alice@example.com" }, session.SetSelfAddressCalls);
    }
}
