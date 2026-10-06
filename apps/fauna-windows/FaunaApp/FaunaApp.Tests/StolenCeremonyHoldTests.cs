using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The persist-failure message
/// survives the page</i>: a supersession this device's own stolen-identity ceremony
/// caused is held back while the ceremony runs or its persist-failure message is
/// pending, and performed once the user leaves Account — or at once, when the
/// ceremony ends while the user is elsewhere and nothing is parked. The windows twin
/// of apple's <c>StolenCeremonyHoldTests</c> and tui's
/// <c>the_ceremonys_own_supersession_waits_for_the_parked_key</c>.
/// </summary>
public class StolenCeremonyHoldTests
{
    /// <summary>Counts escalations performed — the teardown + relaunch stand-in.</summary>
    private sealed class Escalations
    {
        public int Count { get; private set; }
        public Task Perform()
        {
            Count++;
            return Task.CompletedTask;
        }
    }

    /// No ceremony owns Account: an ordinary mid-session supersession goes to the
    /// launch surface at once.
    [Fact]
    public async Task AnOrdinarySupersessionEscalatesAtOnce()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();

        await SessionEndingRoute.EscalateAsync(FfiSessionEndingVerdict.Superseded, hold, escalations.Perform);

        Assert.Equal(1, escalations.Count);
        Assert.False(hold.IsOwed);
    }

    /// The whole path the outcome-17 journey drives: the ceremony's own
    /// supersession arrives mid-ceremony, the seed cannot be stored, the key is
    /// parked on Account — the escalation waits through all of it and runs on the
    /// leave edge.
    ///
    /// Mutation check: dropping <c>MessagePending</c> from <c>EscalateAsync</c>'s
    /// condition, or <c>CeremonyEndedAsync</c> performing without checking
    /// <c>OnAccount</c>, reds this.
    [Fact]
    public async Task TheCeremonysOwnSupersessionWaitsForTheParkedKey()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.CeremonyStarted();

        await SessionEndingRoute.EscalateAsync(FfiSessionEndingVerdict.Superseded, hold, escalations.Perform);
        Assert.Equal(0, escalations.Count); // the ceremony's result has not been handled yet
        Assert.True(hold.IsOwed);

        await hold.CeremonyEndedAsync(adopted: false, messageParked: true);
        Assert.Equal(0, escalations.Count); // the parked key is the only copy — it stays on screen

        await hold.AccountLeftAsync();
        Assert.Equal(1, escalations.Count); // leaving Account performs the held-back escalation
        Assert.False(hold.IsOwed);
    }

    /// A parked message holds the escalation even when the user is not on Account
    /// at the moment the ceremony ends.
    [Fact]
    public async Task AParkedMessageHoldsTheEscalationWhereverTheUserIs()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.CeremonyStarted();
        await hold.EscalateAsync(escalations.Perform);

        await hold.CeremonyEndedAsync(adopted: false, messageParked: true);

        Assert.Equal(0, escalations.Count);
        Assert.True(hold.IsOwed);
    }

    /// The ceremony failed off Account with nothing parked: the owed escalation
    /// runs at once — no leave edge is coming.
    [Fact]
    public async Task ACeremonyEndingOffAccountWithNothingParkedEscalatesAtOnce()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.CeremonyStarted();
        await hold.EscalateAsync(escalations.Perform);

        await hold.CeremonyEndedAsync(adopted: false, messageParked: false);

        Assert.Equal(1, escalations.Count);
    }

    /// On Account with nothing parked, the user reads the ceremony's result first:
    /// the leave edge performs the escalation, not the ceremony's end.
    [Fact]
    public async Task OnAccountTheLeaveEdgePerformsItAfterTheResultIsRead()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.CeremonyStarted();
        await hold.EscalateAsync(escalations.Perform);

        await hold.CeremonyEndedAsync(adopted: false, messageParked: false);
        Assert.Equal(0, escalations.Count);

        await hold.AccountLeftAsync();
        Assert.Equal(1, escalations.Count);
    }

    /// A re-entered Account page can mount before the old one unmounts; the old
    /// one's leave edge is not the user leaving Account.
    ///
    /// Mutation check: collapsing the mount count back to a boolean reds this.
    [Fact]
    public async Task AnOverlappingRemountIsStillOnAccount()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.AccountAppeared();
        await hold.AccountLeftAsync();
        hold.CeremonyStarted();
        await hold.EscalateAsync(escalations.Perform);

        await hold.CeremonyEndedAsync(adopted: false, messageParked: false);
        Assert.Equal(0, escalations.Count); // still on Account reading the outcome

        await hold.AccountLeftAsync();
        Assert.Equal(1, escalations.Count);
    }

    /// The refusal can land AFTER the ceremony's result. A ceremony that adopted
    /// nothing while the user was on Account keeps owning its supersession until
    /// they leave, so its message is not replaced by an import screen for a key
    /// never shown.
    ///
    /// Mutation check: dropping <c>OutcomeOnScreen</c> from <c>EscalateAsync</c>'s
    /// condition reds this.
    [Fact]
    public async Task ARefusalLandingAfterTheResultStillWaitsForTheLeaveEdge()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.CeremonyStarted();
        await hold.CeremonyEndedAsync(adopted: false, messageParked: false);

        await SessionEndingRoute.EscalateAsync(FfiSessionEndingVerdict.Superseded, hold, escalations.Perform);
        Assert.Equal(0, escalations.Count); // the outcome message is still what Account shows

        await hold.AccountLeftAsync();
        Assert.Equal(1, escalations.Count);

        await SessionEndingRoute.EscalateAsync(FfiSessionEndingVerdict.Superseded, hold, escalations.Perform);
        Assert.Equal(2, escalations.Count); // once left, a later supersession is ordinary again
    }

    /// The ceremony adopted its successor: the account switch is itself the
    /// relaunch, so the owed escalation is spent, never performed.
    [Fact]
    public async Task AnAdoptedSuccessorSpendsTheOwedEscalation()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.CeremonyStarted();
        await hold.EscalateAsync(escalations.Perform);

        await hold.CeremonyEndedAsync(adopted: true, messageParked: false);
        await hold.AccountLeftAsync();

        Assert.Equal(0, escalations.Count);
        Assert.False(hold.IsOwed);
    }

    /// Only the supersession is the ceremony's own doing: a changed nest identity
    /// or a refused sign-in is never held back, even mid-ceremony.
    [Fact]
    public async Task OtherSessionEndingVerdictsAreNeverHeldBack()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.CeremonyStarted();

        await SessionEndingRoute.EscalateAsync(FfiSessionEndingVerdict.NestIdentityChanged, hold, escalations.Perform);
        await SessionEndingRoute.EscalateAsync(FfiSessionEndingVerdict.SignInRefused, hold, escalations.Perform);

        Assert.Equal(2, escalations.Count);
        Assert.False(hold.IsOwed);
    }

    /// While the message is pending the page's other error writers are refused
    /// (a clear too: on windows the one <c>error-message</c> InfoBar IS the message,
    /// so a clear closes it); once the user has left Account they land again.
    ///
    /// Mutation check: <c>Admits</c> ignoring <c>MessagePending</c> reds this.
    [Fact]
    public async Task APendingMessageRefusesEveryOtherAccountPageWrite()
    {
        var hold = new StolenCeremonyHold();
        hold.AccountAppeared();
        hold.CeremonyStarted();
        await hold.CeremonyEndedAsync(adopted: false, messageParked: true);

        Assert.False(hold.Admits("HTTP 401"));
        Assert.False(hold.Admits(null));

        await hold.AccountLeftAsync();
        Assert.True(hold.Admits("HTTP 401"));
    }

    /// Leaving Account while the ceremony is still in flight does not perform the
    /// escalation — the ceremony's result has not been handled yet.
    [Fact]
    public async Task LeavingAccountMidCeremonyStillWaitsForTheResult()
    {
        var hold = new StolenCeremonyHold();
        var escalations = new Escalations();
        hold.AccountAppeared();
        hold.CeremonyStarted();
        await hold.EscalateAsync(escalations.Perform);

        await hold.AccountLeftAsync();
        Assert.Equal(0, escalations.Count);

        await hold.CeremonyEndedAsync(adopted: false, messageParked: false);
        Assert.Equal(1, escalations.Count); // off Account with nothing parked, the end performs it
    }
}
