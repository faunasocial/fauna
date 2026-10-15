using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows lift of the four family-safety surfaces tui leads (family-safety.md §
/// Child-initiated contact requests → <i>App affordance</i>, § Feed-source approvals, §
/// The bridge-DM gate → <i>The un-deny surface</i>): the contacts and profile pages'
/// ask pair, the bridge card's feed-source ask, and the guardian's un-deny list. The
/// arms mirror tui's <c>contacts.rs</c> / <c>profile/mod.rs</c> / <c>bridges.rs</c> /
/// <c>family.rs</c> tests and android's <c>WardAsksTest</c> / <c>ProfileVMKnockTest</c> /
/// <c>FamilyContentTest</c>. Serialized with the other process-global caches because
/// <see cref="WardAsksCache"/> is static state.
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class WardAsksTests : IDisposable
{
    public WardAsksTests() => WardAsksCache.Reset();
    public void Dispose() => WardAsksCache.Reset();

    private static readonly byte[] PeerA = Enumerable.Repeat((byte)0xa1, 32).ToArray();
    private static readonly byte[] PeerB = Enumerable.Repeat((byte)0xb2, 32).ToArray();
    private static string Hex(byte[] b) => Convert.ToHexString(b).ToLowerInvariant();

    private static FfiFamilyContactRequest ContactAsk(byte[] peer) => new(peer, "peer", 1);

    private static FfiFamilyFeedRequest FeedAsk(
        string bridge, string op, string target, long? approvedAt = null) =>
        new(bridge, op, target, "label", 1, approvedAt);

    private static FfiFamilyGuardianInfo Guardian() => new(new byte[32], "guardian");

    private static FfiException.GuardianApprovalRequired Refusal() => new("nope");

    // ── The durable pending state (rule (c)) and the supervision gate ────────

    [Fact]
    public void ContactAskPending_ReadsTheDurableList_ForExactlyThatPeer()
    {
        WardAsksCache.SetFromStatus(true, [ContactAsk(PeerA)], []);

        Assert.True(WardAsksCache.ContactAskPending(Hex(PeerA)));
        Assert.True(WardAsksCache.ContactAskPending(Hex(PeerA).ToUpperInvariant()));
        Assert.False(WardAsksCache.ContactAskPending(Hex(PeerB)));
    }

    [Fact]
    public void AGraduatedAccount_HoldsNoAsks_EvenIfTheReplyStillCarriesThem()
    {
        // supervised_by absent ⇒ no guardian to be waiting on: a stale ask from an
        // earlier read must stop painting "asked — waiting" (rule (c)).
        WardAsksCache.SetFromStatus(true, [ContactAsk(PeerA)], [FeedAsk("b", "link", "")]);
        Assert.True(WardAsksCache.ContactAskPending(Hex(PeerA)));

        WardAsksCache.SetFromStatus(false, [ContactAsk(PeerA)], [FeedAsk("b", "link", "")]);

        Assert.False(WardAsksCache.ContactAskPending(Hex(PeerA)));
        Assert.Empty(WardAsksCache.FeedRequests);
    }

    [Fact]
    public void TheStatusFold_GatesOnSupervisedBy()
    {
        var asks = new[] { ContactAsk(PeerA) };

        FamilyStatusHandler.ApplyWardAsks(FfiFamilyStatusFixture.Make(contactRequests: asks));
        Assert.False(WardAsksCache.ContactAskPending(Hex(PeerA)), "no guardian ⇒ no asks");

        FamilyStatusHandler.ApplyWardAsks(
            FfiFamilyStatusFixture.Make(supervisedBy: Guardian(), contactRequests: asks));
        Assert.True(WardAsksCache.ContactAskPending(Hex(PeerA)));
    }

    [Fact]
    public void Reset_DropsAsksAndRefusals_OnAnIdentityChange()
    {
        WardAsksCache.SetFromStatus(true, [ContactAsk(PeerA)], [FeedAsk("b", "link", "")]);
        WardAsksCache.NoteFeedRefusal(new FeedTriple("b", "link", ""));

        WardAsksCache.Reset();

        Assert.False(WardAsksCache.ContactAskPending(Hex(PeerA)));
        Assert.Empty(WardAsksCache.FeedRequests);
        Assert.Empty(WardAsksCache.RefusedFeed);
    }

    [Fact]
    public void ChangedFires_WhenAnAskOrRefusalMoves()
    {
        var fired = 0;
        void Handler() => fired++;
        WardAsksCache.Changed += Handler;
        try
        {
            WardAsksCache.SetFromStatus(true, [], []);
            WardAsksCache.NoteFeedRefusal(new FeedTriple("b", "link", ""));
            WardAsksCache.NoteFeedRefusal(new FeedTriple("b", "link", ""));   // a set: no change
        }
        finally { WardAsksCache.Changed -= Handler; }

        Assert.Equal(2, fired);
    }

    // ── Rule (a): the ask is offered ONLY on the typed refusal ──────────────

    [Fact]
    public async Task ClassifyKnock_TellsTheTypedRefusalApartFromEveryOtherFailure()
    {
        Assert.IsType<KnockSend.Sent>(
            await WardAskFolds.ClassifyKnockAsync(() => Task.CompletedTask));
        Assert.IsType<KnockSend.RefusedByGuardian>(
            await WardAskFolds.ClassifyKnockAsync(() => throw Refusal()));
        // The same sentence on a GENERAL failure must NOT reveal the ask — it would
        // tell an unsupervised user their account is supervised.
        var general = await WardAskFolds.ClassifyKnockAsync(
            () => throw new FfiException.General("This account can only message approved contacts."));
        Assert.IsType<KnockSend.Failed>(general);
        Assert.IsType<KnockSend.Failed>(
            await WardAskFolds.ClassifyKnockAsync(() => throw new InvalidOperationException("boom")));
    }

    // ── Rule (g): a knock reply carries the peer it was sent to ─────────────

    [Fact]
    public void ARefusalForThePeerYouLeft_DoesNotPaintOnTheOneYouNowSee()
    {
        var now = KnockAskState.ForPeer("bob");

        var (next, effect) = WardAskFolds.FoldKnockReply(now, "alice", new KnockSend.RefusedByGuardian());

        Assert.Equal(now, next);
        Assert.False(next.GuardianRefused);
        // …but the failure itself is still real, so error-message still shows it.
        Assert.IsType<KnockErrorEffect.Guardian>(effect);
    }

    [Fact]
    public void ASentReplyForThePeerYouLeft_DoesNotPaintSentOnTheNextActor()
    {
        var now = KnockAskState.ForPeer("bob");

        var (next, effect) = WardAskFolds.FoldKnockReply(now, "alice", new KnockSend.Sent());

        Assert.False(next.KnockSent);
        Assert.IsType<KnockErrorEffect.Clear>(effect);
    }

    [Fact]
    public void ARefusalForThisPeer_OffersTheAsk_AndKeepsTheErrorOnScreen()
    {
        var inFlight = KnockAskState.ForPeer("alice") with { KnockInFlight = true };

        var (next, effect) = WardAskFolds.FoldKnockReply(inFlight, "alice", new KnockSend.RefusedByGuardian());

        Assert.True(next.GuardianRefused);
        Assert.False(next.KnockInFlight);
        Assert.IsType<KnockErrorEffect.Guardian>(effect);   // rule (b)
    }

    [Fact]
    public void AnOrdinaryFailure_NeverOffersTheAsk()
    {
        var (next, effect) = WardAskFolds.FoldKnockReply(
            KnockAskState.ForPeer("alice") with { KnockInFlight = true }, "alice",
            new KnockSend.Failed(new InvalidOperationException("boom")));

        Assert.False(next.GuardianRefused);
        Assert.False(next.KnockInFlight);
        Assert.IsType<KnockErrorEffect.Failed>(effect);
    }

    [Fact]
    public void TheAskedFlag_MovesOnlyOnThatPeersOwnOpen()
    {
        var onBob = KnockAskState.ForPeer("bob") with { AskInFlight = true };

        Assert.Equal(onBob, WardAskFolds.FoldContactAsked(onBob, "alice"));
        Assert.True(WardAskFolds.FoldContactAsked(onBob, "bob").AskSent);
        Assert.False(WardAskFolds.FoldContactAskFailed(onBob, "bob").AskInFlight);
        Assert.True(WardAskFolds.FoldContactAskFailed(onBob, "alice").AskInFlight);
    }

    // ── The pair's render ───────────────────────────────────────────────────

    [Fact]
    public void TheRender_ReadsDurablePendingFirst_ThenTheAskOnlyAfterATypedRefusal()
    {
        var fresh = KnockAskState.ForPeer("alice");
        Assert.Null(WardAskFolds.ContactAskRenderOf(false, fresh));   // nothing refused, nothing pending

        var refused = fresh with { GuardianRefused = true };
        Assert.Equal(ContactAskRender.Ask, WardAskFolds.ContactAskRenderOf(false, refused));

        // Durable pending wins even on a session that never saw the refusal.
        Assert.Equal(ContactAskRender.Pending, WardAskFolds.ContactAskRenderOf(true, fresh));
        Assert.Equal(ContactAskRender.Pending, WardAskFolds.ContactAskRenderOf(true, refused));
        // The just-asked flag answers before the durable re-read lands.
        Assert.Equal(ContactAskRender.Pending,
            WardAskFolds.ContactAskRenderOf(false, refused with { AskSent = true }));
    }

    // ── Feed-source rows: keyed on the ask data (rules (d), (e)) ────────────

    [Fact]
    public void SourceAskRows_PaintNothingInTheCommonCase()
    {
        Assert.True(WardAskFolds.SourceAskRowsFor([], [], "b").IsEmpty);
    }

    [Fact]
    public void ARefusedTriple_OffersTheButton_CarryingItsOwnTriple()
    {
        var link = new FeedTriple("b", "link", "");
        var follow = new FeedTriple("b", "follow", "user-1");
        var other = new FeedTriple("c", "link", "");

        var rows = WardAskFolds.SourceAskRowsFor([], [link, follow, other], "b");

        Assert.Equal([link, follow], rows.Asks);
        Assert.Empty(rows.States);
    }

    [Fact]
    public void AnAnsweredAsk_ShowsItsVerdict_InsteadOfOfferingTheButtonAgain()
    {
        var link = new FeedTriple("b", "link", "");
        var follow = new FeedTriple("b", "follow", "user-1");

        var rows = WardAskFolds.SourceAskRowsFor(
            [FeedAsk("b", "link", "")], [link, follow], "b");

        Assert.Equal([FfiFeedRequestState.Pending], rows.States);
        Assert.Equal([follow], rows.Asks);   // only the un-asked triple still gets a button
    }

    [Fact]
    public void AnApprovedAsk_IsALabelNeverAButton()
    {
        var link = new FeedTriple("b", "link", "");

        var rows = WardAskFolds.SourceAskRowsFor(
            [FeedAsk("b", "link", "", approvedAt: 5)], [link], "b");

        Assert.Equal([FfiFeedRequestState.Approved], rows.States);
        Assert.Empty(rows.Asks);   // the ward redeems it by repeating the ORIGINAL Link
    }

    [Fact]
    public void OneFollowsApproval_DoesNotLightUpAnotherFollowsRow()
    {
        var a = new FeedTriple("b", "follow", "user-a");
        var b = new FeedTriple("b", "follow", "user-b");

        var rows = WardAskFolds.SourceAskRowsFor(
            [FeedAsk("b", "follow", "user-a", approvedAt: 5)], [a, b], "b");

        Assert.Equal([b], rows.Asks);
    }

    [Fact]
    public void ALinkAsk_SatisfiesNeitherFollow()
    {
        var follow = new FeedTriple("b", "follow", "user-a");

        var rows = WardAskFolds.SourceAskRowsFor([FeedAsk("b", "link", "")], [follow], "b");

        Assert.Equal([follow], rows.Asks);
    }

    [Fact]
    public void AnAskOnAnotherBridge_DoesNotPaintOnThisCard()
    {
        var rows = WardAskFolds.SourceAskRowsFor([FeedAsk("c", "link", "")], [], "b");

        Assert.True(rows.IsEmpty);
    }
}

/// <summary>The contacts page's Find User ask pair, over a mock nest.</summary>
[Collection("ActorScopedStaticsGlobal")]
public class ContactsAskViewModelTests : IDisposable
{
    public ContactsAskViewModelTests() => WardAsksCache.Reset();
    public void Dispose() => WardAsksCache.Reset();

    private static readonly string Peer = string.Concat(Enumerable.Repeat("a1", 32));

    private static MockNestRpcClient Refusing() => new()
    {
        NextKnockException = new FfiException.GuardianApprovalRequired("only approved contacts"),
    };

    [Fact]
    public async Task ATypedRefusal_OffersTheAsk_AndStaysOnTheErrorMessage()
    {
        var rpc = Refusing();
        var vm = new ContactsViewModel(rpc);
        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);

        await vm.AddFoundContactCommand.ExecuteAsync(Peer);

        Assert.Equal(ContactAskRender.Ask, vm.ContactAskRenderNow());
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));   // rule (b)
        Assert.Equal(FaunaApp.Core.Services.Strings.Get("contacts/guardian_approval_required"), vm.ErrorMessage);
    }

    [Fact]
    public async Task AnOrdinaryFailure_DoesNotOfferTheAsk()
    {
        var rpc = new MockNestRpcClient { NextKnockException = new InvalidOperationException("boom") };
        var vm = new ContactsViewModel(rpc);
        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);

        await vm.AddFoundContactCommand.ExecuteAsync(Peer);

        Assert.Null(vm.ContactAskRenderNow());
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    [Fact]
    public async Task AskingTheGuardian_SendsTheResolvedPeer_AndFlipsToPending()
    {
        var rpc = Refusing();
        var vm = new ContactsViewModel(rpc);
        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);
        await vm.AddFoundContactCommand.ExecuteAsync(Peer);

        await vm.AskGuardianCommand.ExecuteAsync(Peer);

        Assert.Equal(Peer, rpc.LastFamilyContactRequest);
        Assert.Equal(ContactAskRender.Pending, vm.ContactAskRenderNow());
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task APendingAsk_IsNotReSent()
    {
        var rpc = Refusing();
        var vm = new ContactsViewModel(rpc);
        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);
        await vm.AddFoundContactCommand.ExecuteAsync(Peer);
        await vm.AskGuardianCommand.ExecuteAsync(Peer);

        await vm.AskGuardianCommand.ExecuteAsync(Peer);

        Assert.Equal(1, rpc.Calls.Count(c => c == "FamilyContactRequest"));
    }

    [Fact]
    public async Task ADurablePendingAsk_RendersWithoutAnyRefusalThisSession()
    {
        // A ward who asked yesterday must see "asked — waiting" on reopening, not a
        // send button that would only be refused again (rule (c)).
        WardAsksCache.SetFromStatus(
            true, [new FfiFamilyContactRequest(Convert.FromHexString(Peer), "peer", 1)], []);
        var vm = new ContactsViewModel(new MockNestRpcClient());

        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);

        Assert.Equal(ContactAskRender.Pending, vm.ContactAskRenderNow());
    }

    [Fact]
    public async Task ANewLookup_DropsThePreviousPeersRefusal()
    {
        var rpc = Refusing();
        var vm = new ContactsViewModel(rpc);
        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);
        await vm.AddFoundContactCommand.ExecuteAsync(Peer);
        Assert.Equal(ContactAskRender.Ask, vm.ContactAskRenderNow());

        await vm.LookUpRecipientCommand.ExecuteAsync(new string('b', 64));

        // The app never offers to ask about somebody the ward did not name.
        Assert.Null(vm.ContactAskRenderNow());
    }

    [Fact]
    public async Task AFailedAsk_ReEnablesTheButton_AndShowsTheError()
    {
        var rpc = Refusing();
        var vm = new ContactsViewModel(rpc);
        await vm.LookUpRecipientCommand.ExecuteAsync(Peer);
        await vm.AddFoundContactCommand.ExecuteAsync(Peer);
        rpc.NextError = "ask failed";

        await vm.AskGuardianCommand.ExecuteAsync(Peer);

        Assert.False(vm.KnockAsk.AskInFlight);
        Assert.Equal(ContactAskRender.Ask, vm.ContactAskRenderNow());
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }
}

/// <summary>The profile page's knock + the same ask pair.</summary>
[Collection("ActorScopedStaticsGlobal")]
public class ProfileKnockViewModelTests : IDisposable
{
    public ProfileKnockViewModelTests() => WardAsksCache.Reset();
    public void Dispose() => WardAsksCache.Reset();

    private static readonly string Peer = new('a', 64);

    [Fact]
    public async Task RequestContact_SendsTheKnock_AndReadsRequestSent()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, Peer, isSelf: false);

        await vm.RequestContactAsync();

        Assert.Equal(Peer, rpc.LastKnockSent);
        Assert.True(vm.KnockAsk.KnockSent);
        Assert.Null(vm.ErrorMessage);
        Assert.Null(vm.ContactAskRenderNow());
    }

    [Fact]
    public async Task ASentKnock_IsNotSentTwice()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, Peer, isSelf: false);
        await vm.RequestContactAsync();

        await vm.RequestContactAsync();

        Assert.Equal(1, rpc.Calls.Count(c => c == "SendKnock"));
    }

    [Fact]
    public async Task NoRouteFromTheProfile_MeansThisNest()
    {
        // No profile read landed (an unpublished profile): the shared rule has nothing
        // to route on, so the knock goes to the home nest (recipient_nest_url null).
        var rpc = new MockNestRpcClient { NextProfile = null };
        var vm = new ProfileViewModel(rpc, Peer, isSelf: false);
        await vm.RefreshHeaderAsync();

        await vm.RequestContactAsync();

        Assert.Null(rpc.LastKnockRoute);
    }

    [Fact]
    public async Task ATypedRefusal_ReEnablesTheKnock_OffersTheAsk_AndStaysOnTheError()
    {
        var rpc = new MockNestRpcClient
        {
            NextKnockException = new FfiException.GuardianApprovalRequired("approved contacts only"),
        };
        var vm = new ProfileViewModel(rpc, Peer, isSelf: false);

        await vm.RequestContactAsync();

        Assert.False(vm.KnockAsk.KnockSent);
        Assert.False(vm.KnockAsk.KnockInFlight);   // the button is live again
        Assert.Equal(ContactAskRender.Ask, vm.ContactAskRenderNow());
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));   // rule (b)
    }

    [Fact]
    public async Task AnOrdinaryFailure_NeverOffersTheAsk()
    {
        var rpc = new MockNestRpcClient { NextKnockException = new InvalidOperationException("boom") };
        var vm = new ProfileViewModel(rpc, Peer, isSelf: false);

        await vm.RequestContactAsync();

        Assert.Null(vm.ContactAskRenderNow());
        Assert.False(vm.KnockAsk.KnockSent);
    }

    [Fact]
    public async Task AskingTheGuardian_FlipsToPending_AndANewOpenOfAnotherActorStartsClean()
    {
        var rpc = new MockNestRpcClient
        {
            NextKnockException = new FfiException.GuardianApprovalRequired("approved contacts only"),
        };
        var vm = new ProfileViewModel(rpc, Peer, isSelf: false);
        await vm.RequestContactAsync();

        await vm.AskGuardianAsync();

        Assert.Equal(Peer, rpc.LastFamilyContactRequest);
        Assert.Equal(ContactAskRender.Pending, vm.ContactAskRenderNow());

        // A different actor's open is a different view model: it reads its own state.
        var other = new ProfileViewModel(rpc, new string('b', 64), isSelf: false);
        Assert.Null(other.ContactAskRenderNow());
    }

    [Fact]
    public async Task OnSelf_NeitherTheKnockNorTheAskExists()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, Peer, isSelf: true);

        await vm.RequestContactAsync();
        await vm.AskGuardianAsync();

        Assert.DoesNotContain("SendKnock", rpc.Calls);
        Assert.DoesNotContain("FamilyContactRequest", rpc.Calls);
        Assert.Null(vm.ContactAskRenderNow());
    }
}

/// <summary>The bridge card's feed-source ask, from the refusal to the request.</summary>
[Collection("ActorScopedStaticsGlobal")]
public class BridgesSourceAskViewModelTests : IDisposable
{
    public BridgesSourceAskViewModelTests() => WardAsksCache.Reset();
    public void Dispose() => WardAsksCache.Reset();

    [Fact]
    public async Task ARefusedLink_RecordsExactlyThatTriple_AndStaysOnTheError()
    {
        var rpc = new MockNestRpcClient { NextLinkException = new FfiException.GuardianApprovalRequired("no") };
        var vm = new BridgesViewModel(rpc);

        await vm.LinkBridgeAsync("activitypub", "default", new Dictionary<string, string>());

        var refused = Assert.Single(WardAsksCache.RefusedFeed);
        Assert.Equal(new FeedTriple("activitypub", "link", ""), refused);
        Assert.Equal(FaunaApp.Core.Services.Strings.Get("bridges/source_blocked"), vm.ErrorMessage);
    }

    [Fact]
    public async Task AnOrdinaryLinkFailure_RecordsNothing_SoNoAskIsOffered()
    {
        var rpc = new MockNestRpcClient { NextLinkException = new InvalidOperationException("boom") };
        var vm = new BridgesViewModel(rpc);

        await vm.LinkBridgeAsync("activitypub", "default", new Dictionary<string, string>());

        Assert.Empty(WardAsksCache.RefusedFeed);
    }

    [Fact]
    public async Task ARefusedFollow_IsKeyedOnTheFollowIdNotOnTheFormBuffers()
    {
        var rpc = new MockNestRpcClient { NextAddFollowException = new FfiException.GuardianApprovalRequired("no") };
        var vm = new BridgesViewModel(rpc) { SelectedBridgeId = "activitypub" };

        await vm.AddFollowCommand.ExecuteAsync("user-9|Nine");

        Assert.Equal(
            new FeedTriple("activitypub", "follow", "user-9"),
            Assert.Single(WardAsksCache.RefusedFeed));
    }

    [Fact]
    public async Task AskSource_SendsTheTriple_WithTheBridgeNameAsItsLabel()
    {
        var rpc = new MockNestRpcClient();
        var vm = new BridgesViewModel(rpc);

        await vm.AskSourceAsync(new FeedTriple("activitypub", "follow", "user-9"), "Fediverse");

        Assert.Equal(("activitypub", "follow", "user-9", "Fediverse"), rpc.LastFamilyFeedSourceRequest);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task AFailedAsk_SurfacesOnTheErrorBar()
    {
        var rpc = new MockNestRpcClient { NextError = "ask failed" };
        var vm = new BridgesViewModel(rpc);

        await vm.AskSourceAsync(new FeedTriple("b", "link", ""), "B");

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }
}

/// <summary>The guardian's un-deny list: one row, one button, that row's own peer.</summary>
public class FamilyBlockedPeerViewModelTests
{
    private static readonly byte[] Ward = Enumerable.Repeat((byte)0x77, 32).ToArray();

    private static FfiFamilyBlockedPeer Blocked(string bridge, string peer) => new(bridge, peer);

    private static MockNestRpcClient RpcWith(params FfiFamilyBlockedPeer[] blocked) => new()
    {
        NextFamilyStatus = FfiFamilyStatusFixture.Make(
            wards: [FfiFamilyWardInfoFixture.Make(actorId: Ward, blockedDmPeers: blocked)]),
    };

    [Fact]
    public async Task TheListShowsOneRowPerDeniedPeer_InTheNestsOrder()
    {
        var vm = new FamilyViewModel(RpcWith(Blocked("signal", "alice"), Blocked("signal", "bob")));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(["alice", "bob"], vm.BlockedPeers.Select(r => r.PeerId));
    }

    [Fact]
    public async Task EachAllowButton_UnDeniesItsOwnRowsPeer()
    {
        // Rule (f): pointing every button at row 0 would un-deny the wrong person while
        // the surface still looked correct. Mutation-proven: forcing AllowBlockedPeerAsync
        // to send BlockedPeers[0] turns the second assertion red.
        var rpc = RpcWith(Blocked("signal", "alice"), Blocked("matrix", "bob"));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AllowBlockedPeerAsync(vm.BlockedPeers[1]);

        var call = Assert.Single(rpc.FamilyAllowBlockedPeerCalls);
        Assert.Equal(Ward, call.SupervisedActorId);
        Assert.Equal(Blocked("matrix", "bob"), call.Peer);
    }

    [Fact]
    public async Task AllowingFirstRow_SendsTheFirstRow()
    {
        var rpc = RpcWith(Blocked("signal", "alice"), Blocked("matrix", "bob"));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AllowBlockedPeerAsync(vm.BlockedPeers[0]);

        Assert.Equal(Blocked("signal", "alice"), Assert.Single(rpc.FamilyAllowBlockedPeerCalls).Peer);
    }

    [Fact]
    public async Task AfterTheAllow_TheListIsReReadFromTheNest()
    {
        var rpc = RpcWith(Blocked("signal", "alice"), Blocked("signal", "bob"));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        // The nest carries only `block` rows, so the allowed peer drops out of the read.
        rpc.NextFamilyStatus = FfiFamilyStatusFixture.Make(
            wards: [FfiFamilyWardInfoFixture.Make(actorId: Ward, blockedDmPeers: [Blocked("signal", "bob")])]);

        await vm.AllowBlockedPeerAsync(vm.BlockedPeers[0]);

        Assert.Equal(["bob"], vm.BlockedPeers.Select(r => r.PeerId));
    }

    [Fact]
    public async Task AFailedAllow_SurfacesOnTheError_AndKeepsTheRow()
    {
        var rpc = RpcWith(Blocked("signal", "alice"));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        rpc.NextError = "allow failed";

        await vm.AllowBlockedPeerAsync(vm.BlockedPeers[0]);

        Assert.False(string.IsNullOrEmpty(vm.ActionError));
        Assert.Single(vm.BlockedPeers);
    }

    [Fact]
    public async Task AWardWithNoDeniedPeers_HasAnEmptyList()
    {
        var vm = new FamilyViewModel(RpcWith());

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Empty(vm.BlockedPeers);
    }
}
