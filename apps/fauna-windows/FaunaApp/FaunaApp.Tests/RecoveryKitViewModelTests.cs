using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Settings → Account <b>Recovery Kit</b> section's VM
/// (<c>docs/goal/ui/settings.md</c> § Recovery kit) over
/// <see cref="MockNestRpcClient"/>. The windows leg of the succession ceremony
/// (<c>identity-succession.md</c> § Implementation status today,
/// the correction paragraph).
///
/// <para>These pin what the VM must NOT do as much as what it does: it must not
/// re-derive enablement from the status kind, must not run the irreversible
/// ceremony without both gates, and must not tear the session down on the
/// persist-failure arm.</para>
///
/// <para><b>Serialized</b> via <c>StringsGlobal</c> on two counts: the handoff is
/// process-global static state by design, and this class registers a localizer so
/// the persist-failure sentence is exercised with its real <c>{secret}</c>
/// substitution rather than the key-fallback.</para>
/// </summary>
[Collection("StringsGlobal")]
public class RecoveryKitViewModelTests : IDisposable
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly System.Collections.Generic.Dictionary<string, string> _map;
        public FakeLocalizer(System.Collections.Generic.Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    public RecoveryKitViewModelTests()
    {
        SuccessionHandoff.ClearOnCredentialWipe();
        // The real templates for the two lines whose CONTENT is load-bearing: the
        // persist-failure sentence must carry the secret (it is the only copy in
        // existence), and the pending line must carry the day count.
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["settings/recovery_kit/stolen_persist_failed"] =
                "saving it on this device failed. Write this secret key down NOW: {secret}",
            ["settings/recovery_kit/status_replacement_pending"] =
                "takes effect in {days} days",
            ["settings/recovery_kit/status_never_created"] = "No recovery kit.",
            ["settings/recovery_kit/kit_phrase_required"] = "Paste the recovery phrase first.",
            // The sweep's own lines (settings.md § Recovery kit → The sweep's own
            // lines) — real templates so the {groups}/{removed}/{count} substitution
            // is exercised, not just the key fallback.
            ["settings/recovery_kit/sweep_all_removed"] =
                "Your old identity was removed from all {groups} of your group conversations.",
            ["settings/recovery_kit/sweep_partial"] =
                "Your old identity was removed from {removed} of your {groups} group conversations.",
            ["settings/recovery_kit/sweep_unattested"] =
                "There are {count} other members this cannot confirm you added yourself.",
            ["settings/recovery_kit/sweep_retry_no_old_state"] =
                "This device does not have the conversation history from your previous identity.",
        }));
    }

    public void Dispose() => SuccessionHandoff.ClearOnCredentialWipe();

    private const string Predecessor = "11111111111111111111111111111111"
        + "11111111111111111111111111111111";
    private const string Successor = "22222222222222222222222222222222"
        + "22222222222222222222222222222222";

    // ── The status read ──────────────────────────────────────────────────

    [Fact]
    public async Task LoadStatus_ReadsTheChain_AndPaintsTheState()
    {
        var rpc = new MockNestRpcClient
        {
            NextRecoveryKitStatus = MockNestRpcClient.MakeRecoveryKitStatus(
                "registered", allowsReplace: true, allowsLost: true),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.LoadStatusAsync();

        Assert.Contains("RecoveryKitStatus", rpc.Calls);
        Assert.Equal("registered", vm.Status?.kind);
        Assert.False(vm.Loading);
        Assert.Null(vm.ErrorMessage);
    }

    /// <summary>
    /// A section that cannot read its chain paints NO state and says why. It must
    /// not fall back to a state it did not observe, because every action's
    /// enablement hangs off that read.
    /// </summary>
    [Fact]
    public async Task AFailedChainRead_LeavesNoStatusAtAll()
    {
        var rpc = new MockNestRpcClient { NextError = "nest unreachable" };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.LoadStatusAsync();

        Assert.Null(vm.Status);
        Assert.NotNull(vm.ErrorMessage);
        // ⚠ The kit-in-hand field and the stolen trigger STAY — see
        // TheStolenTriggerRendersEvenWhenTheChainReadFails. Everything the failed
        // read gates (create/replace/lost enablement) is off; what the KIT authorizes
        // is not.
        Assert.True(vm.PhraseFieldVisible);
        Assert.True(vm.StolenVisible);
    }

    /// <summary>
    /// ⚠ The whole reason the shared predicates are carried on the status record:
    /// <c>allows_stolen</c> is unconditionally true (theft is exactly the no-kit
    /// case) and <c>allows_replace</c> stays true DURING a pending window. A VM
    /// that re-derived enablement from <c>kind</c> would get both wrong.
    /// </summary>
    [Fact]
    public async Task EnablementComesFromThePredicates_NotTheKind()
    {
        var rpc = new MockNestRpcClient
        {
            // never-created, yet stolen is offered and replace is not.
            NextRecoveryKitStatus = MockNestRpcClient.MakeRecoveryKitStatus(
                "never-created", allowsCreate: true, allowsReplace: false, allowsStolen: true),
        };
        var vm = new RecoveryKitViewModel(rpc);
        await vm.LoadStatusAsync();

        Assert.True(vm.Status!.allowsStolen);
        Assert.False(vm.Status!.allowsReplace);
        // The phrase field still renders: the stolen ceremony consumes a kit.
        Assert.True(vm.PhraseFieldVisible);

        // …and during a pending window replace stays live.
        rpc.NextRecoveryKitStatus = MockNestRpcClient.MakeRecoveryKitStatus(
            "replacement-pending", allowsReplace: true, pendingLandsAt: 1_700_000_000L);
        await vm.LoadStatusAsync();

        Assert.True(vm.Status!.allowsReplace);
        Assert.True(vm.PhraseFieldVisible);
    }

    /// <summary>
    /// ⚠ The succession trigger renders while the status is still UNREAD, and that is
    /// deliberate rather than incidental: this ceremony exists for an owner a thief
    /// has locked out, the authenticated chain read is exactly what fails for such an
    /// owner, and the KIT is the authorization — the shared export does no status
    /// re-read of its own. Hiding the trigger behind a read that may never succeed
    /// would withhold the affordance precisely from the person it is for. (Windows is
    /// wider than apple here; the lift is captured for the other apps.)
    /// </summary>
    [Fact]
    public async Task TheStolenTriggerRendersEvenWhenTheChainReadFails()
    {
        var rpc = new MockNestRpcClient { NextError = "nest unreachable" };
        var vm = new RecoveryKitViewModel(rpc);

        // Before any read at all…
        Assert.True(vm.StolenVisible);
        Assert.True(vm.PhraseFieldVisible);

        await vm.LoadStatusAsync();

        // …and after one that failed outright.
        Assert.Null(vm.Status);
        Assert.True(vm.StolenVisible);
        Assert.True(vm.PhraseFieldVisible);
    }

    /// <summary>Only a status that POSITIVELY says stolen is disallowed hides it —
    /// the shared predicate never does today, but the render honours it if it ever
    /// starts.</summary>
    [Fact]
    public async Task OnlyAPositiveRefusalHidesTheStolenTrigger()
    {
        var rpc = new MockNestRpcClient
        {
            NextRecoveryKitStatus = MockNestRpcClient.MakeRecoveryKitStatus(
                "registered", allowsStolen: false),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.LoadStatusAsync();

        Assert.False(vm.StolenVisible);
    }

    // ── The kit ceremonies ───────────────────────────────────────────────

    [Fact]
    public async Task Create_MintsWithNoHeldPhrase_AndShowsTheSecretOnce()
    {
        var rpc = new MockNestRpcClient
        {
            NextMintedKit = new uniffi.fauna_ffi.FfiMintedKit(new string('d', 64), true, null),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: false);

        Assert.Contains("RecoveryCreateKit", rpc.Calls);
        Assert.Null(rpc.LastRecoveryCreateHeldKitInput);
        Assert.Equal(new string('d', 64), vm.MintedSecretHex);
        Assert.True(vm.MintedEscrowStored);
        // The mint re-reads the chain so the section ends on its NEW state.
        Assert.Contains("RecoveryKitStatus", rpc.Calls);

        // …and there is deliberately no path that shows it again.
        vm.ClearHeldSecrets();
        Assert.Null(vm.MintedSecretHex);
    }

    [Fact]
    public async Task Replace_SendsTheHeldPhrase_AsTheAuthorizingArm()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc) { PhraseInput = "fauna://recovery/abc" };

        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: true);

        Assert.Equal("fauna://recovery/abc", rpc.LastRecoveryCreateHeldKitInput);
        // The buffer is dropped once spent — it is a recovery root.
        Assert.Equal(string.Empty, vm.PhraseInput);
    }

    /// <summary>An empty phrase is an honest refusal on <c>error-message</c>, never
    /// a silently dropped gesture (e2e convention 11).</summary>
    [Fact]
    public async Task AKitInHandCeremonyWithNoPhrase_RefusesOutLoud()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc);

        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: true);
        await vm.VetoPendingReplacementAsync();
        await vm.ResealEscrowWithHeldKitAsync();

        Assert.DoesNotContain("RecoveryCreateKit", rpc.Calls);
        Assert.DoesNotContain("RecoveryVetoPendingReplacement", rpc.Calls);
        Assert.DoesNotContain("RecoveryResealEscrowWithHeldKit", rpc.Calls);
        Assert.NotNull(vm.ErrorMessage);
    }

    /// <summary>
    /// ⚠ A failed escrow put is NOT an error: the registration has already landed,
    /// so the returned secret is live and is the only copy in existence. The VM
    /// must surface the kit, not swallow it behind an error.
    /// </summary>
    [Fact]
    public async Task AFailedEscrowPut_StillShowsTheKit()
    {
        var rpc = new MockNestRpcClient
        {
            NextMintedKit = new uniffi.fauna_ffi.FfiMintedKit(new string('e', 64), false, null),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: false);

        Assert.Equal(new string('e', 64), vm.MintedSecretHex);
        Assert.False(vm.MintedEscrowStored);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task Lost_OpensTheWindow_AndStillShowsASecretNow()
    {
        var rpc = new MockNestRpcClient
        {
            NextMintedKit = new uniffi.fauna_ffi.FfiMintedKit(
                new string('f', 64), false, 1_700_000_000L),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.RequestSeedAloneReplacementAsync();

        Assert.Contains("RecoveryRequestSeedAloneReplacement", rpc.Calls);
        Assert.Equal(new string('f', 64), vm.MintedSecretHex);
        Assert.Equal(1_700_000_000L, vm.MintedLandsAt);
    }

    // ── The succession ───────────────────────────────────────────────────

    /// <summary>
    /// Both gates are re-checked in the handler, not only in the render: a disabled
    /// control emits no gesture, but a test agent driving the id reaches here, and
    /// an irreversible ceremony must refuse out loud rather than run
    /// (<c>settings.md</c> § Recovery kit).
    /// </summary>
    [Fact]
    public async Task TheSuccessionRefusesWithoutTheTypeToConfirmToken()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc) { PhraseInput = "kit" };
        var switched = false;

        // No token at all.
        await vm.SucceedWithHeldKitAsync(Predecessor, _ => { switched = true; return Task.CompletedTask; });
        // A near-miss, and a localized-looking one: the token is never localized.
        vm.StolenConfirmInput = "Succeed";
        await vm.SucceedWithHeldKitAsync(Predecessor, _ => { switched = true; return Task.CompletedTask; });

        Assert.DoesNotContain("SuccessionSucceedWithHeldKit", rpc.Calls);
        Assert.False(switched);
        Assert.False(SuccessionHandoff.KitOwed);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task TheSuccessionRefusesWithoutTheKitInHand()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc) { StolenConfirmInput = "SUCCEED" };

        await vm.SucceedWithHeldKitAsync(Predecessor, _ => Task.CompletedTask);

        Assert.DoesNotContain("SuccessionSucceedWithHeldKit", rpc.Calls);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public void TheStolenButtonArmsOnlyOnTheExactLiteral()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient());

        Assert.False(vm.StolenArmed);
        vm.StolenConfirmInput = "succeed";
        Assert.False(vm.StolenArmed);
        vm.StolenConfirmInput = " SUCCEED ";
        Assert.False(vm.StolenArmed);
        vm.StolenConfirmInput = "SUCCEED";
        Assert.True(vm.StolenArmed);
    }

    /// <summary>The happy arm: the account moved, the seed is on the device, so the
    /// caller switches to the successor and the handoff carries the ceremony's
    /// survivors across that switch.</summary>
    [Fact]
    public async Task APersistedSuccessionSwitchesToTheSuccessor_AndOwesItAKit()
    {
        var rpc = new MockNestRpcClient
        {
            NextLandedSuccession = MockNestRpcClient.MakeLandedSuccession(
                newActorIdHex: Successor, persisted: true, sweepStateJson: "{\"kind\":\"ran\"}"),
        };
        var vm = new RecoveryKitViewModel(rpc)
        {
            PhraseInput = "kit", StolenConfirmInput = "SUCCEED",
        };
        string? switchedTo = null;

        await vm.SucceedWithHeldKitAsync(Predecessor, actor =>
        {
            switchedTo = actor;
            return Task.CompletedTask;
        });

        Assert.Equal("kit", rpc.LastSuccessionKitInput);
        Assert.Equal(Successor, switchedTo);
        Assert.True(SuccessionHandoff.KitOwed);
        Assert.Equal(Successor, SuccessionHandoff.SuccessorActorIdHex);
        Assert.Equal(Predecessor, SuccessionHandoff.PredecessorActorIdHex);
        Assert.Equal("{\"kind\":\"ran\"}", SuccessionHandoff.SweepStateJson);
        // Both buffers are dropped: a recovery root and a confirm token.
        Assert.Equal(string.Empty, vm.PhraseInput);
        Assert.Equal(string.Empty, vm.StolenConfirmInput);
        Assert.Null(vm.ErrorMessage);
    }

    /// <summary>
    /// ⚠ The arm that must not be got wrong: the account DID move, but the device
    /// could not save the successor seed — so the secret goes on screen and the
    /// session is NOT torn down, because tearing it down takes the only copy of the
    /// key with it.
    /// </summary>
    [Fact]
    public async Task APersistFailureShowsTheSecret_AndDoesNotTearTheSessionDown()
    {
        var rpc = new MockNestRpcClient
        {
            NextLandedSuccession = MockNestRpcClient.MakeLandedSuccession(
                successorSecretHex: new string('9', 64), newActorIdHex: Successor,
                persisted: false),
        };
        var vm = new RecoveryKitViewModel(rpc)
        {
            PhraseInput = "kit", StolenConfirmInput = "SUCCEED",
        };
        var switched = false;

        await vm.SucceedWithHeldKitAsync(Predecessor, _ => { switched = true; return Task.CompletedTask; });

        Assert.False(switched);
        Assert.Equal(new string('9', 64), vm.MintedSecretHex);
        Assert.NotNull(vm.ErrorMessage);
        Assert.Contains(new string('9', 64), vm.ErrorMessage!);
        // The succession landed either way, so the kit is owed either way.
        Assert.True(SuccessionHandoff.KitOwed);
        // The park write must not be dropped by its own guard (ordering correction): the flag flips true only AFTER the message
        // lands, so this arm both shows the secret AND parks it.
        Assert.True(vm.StolenPersistFailurePending);
    }

    /// <summary>A ceremony that never landed mints nothing anyone must keep — and
    /// must not leave a phantom obligation behind.</summary>
    [Fact]
    public async Task AFailedSuccessionOwesNothing()
    {
        var rpc = new MockNestRpcClient { NextError = "nest unreachable" };
        var vm = new RecoveryKitViewModel(rpc)
        {
            PhraseInput = "kit", StolenConfirmInput = "SUCCEED",
        };
        var switched = false;

        await vm.SucceedWithHeldKitAsync(Predecessor, _ => { switched = true; return Task.CompletedTask; });

        Assert.False(switched);
        Assert.False(SuccessionHandoff.KitOwed);
        Assert.Null(vm.MintedSecretHex);
        Assert.NotNull(vm.ErrorMessage);
    }

    // ── The successor's closing act ──────────────────────────────────────

    [Fact]
    public async Task TheSuccessorMintsItsOwedKitUnbidden()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor), Predecessor);
        var rpc = new MockNestRpcClient
        {
            NextMintedKit = new uniffi.fauna_ffi.FfiMintedKit(new string('7', 64), true, null),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.DischargeOwedSuccessionKitAsync(Successor);

        Assert.Contains("RecoveryCreateKit", rpc.Calls);
        Assert.Equal(new string('7', 64), vm.MintedSecretHex);
        Assert.False(SuccessionHandoff.KitOwed);
    }

    /// <summary>An ordinary visit pays one boolean check and mints nothing.</summary>
    [Fact]
    public async Task AnOrdinaryVisitMintsNothing()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc);

        await vm.DischargeOwedSuccessionKitAsync(Successor);

        Assert.DoesNotContain("RecoveryCreateKit", rpc.Calls);
        Assert.Null(vm.MintedSecretHex);
    }

    /// <summary>The departing session re-hydrates through the teardown; it must not
    /// take an obligation it cannot perform (its bearers were just revoked).</summary>
    [Fact]
    public async Task TheDepartingSessionDoesNotMintTheSuccessorsKit()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor), Predecessor);
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc);

        await vm.DischargeOwedSuccessionKitAsync(Predecessor);

        Assert.DoesNotContain("RecoveryCreateKit", rpc.Calls);
        Assert.True(SuccessionHandoff.KitOwed);
    }

    // ── Identity change ──────────────────────────────────────────────────

    /// <summary>
    /// Carrying the previous account's status into this one does not render a
    /// WEAKER answer, it renders the WRONG one — the section would offer ceremonies
    /// the signed-in account cannot run and withhold ones it can.
    /// </summary>
    [Fact]
    public async Task AnIdentityChangeDropsTheStatusAndEveryHeldSecret()
    {
        var rpc = new MockNestRpcClient
        {
            NextRecoveryKitStatus = MockNestRpcClient.MakeRecoveryKitStatus(
                "registered", allowsReplace: true),
        };
        var vm = new RecoveryKitViewModel(rpc);
        await vm.LoadStatusAsync();
        await vm.CreateOrReplaceKitAsync(usingHeldPhrase: false);
        vm.PhraseInput = "kit";
        vm.StolenConfirmInput = "SUCCEED";

        vm.ResetForIdentityChange();

        Assert.Null(vm.Status);
        Assert.Null(vm.MintedSecretHex);
        Assert.Equal(string.Empty, vm.PhraseInput);
        Assert.Equal(string.Empty, vm.StolenConfirmInput);
        Assert.Null(vm.ErrorMessage);
    }

    // ── The persist-failure message survives the page (parity with
    //    linux / apple) ──
    //
    // Mirrors apple's RecoveryKitVMTests.swift shape (per the row's merit
    // judgment): the guard is targeted directly — `SetGuardedError` /
    // `StolenPersistFailurePending` / `AcknowledgeStolenPersistFailure` — never
    // `ErrorBar`, which FaunaApp.Tests cannot reach (FaunaApp.Tests.csproj
    // references only FaunaApp.Core).

    /// <summary>
    /// Mutation check: dropping the <c>if (StolenPersistFailurePending) return;</c>
    /// guard in <c>SetGuardedError</c> reds this.
    /// </summary>
    [Fact]
    public void APendingPersistFailureMessageWinsOverAnyOtherWriteToTheErrorSlot()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            ErrorMessage = "the persist-failure message, with the successor's seed",
            StolenPersistFailurePending = true,
        };

        // Stands in for any of this view model's ordinary writers — a
        // validation guard, a caught error, a landed status/sweep answer.
        vm.SetGuardedError("an unrelated write that must not land");

        Assert.Equal(
            "the persist-failure message, with the successor's seed", vm.ErrorMessage);
    }

    /// <summary>
    /// Mutation check: <c>AcknowledgeStolenPersistFailure</c> not actually
    /// clearing <c>StolenPersistFailurePending</c> reds this.
    /// </summary>
    [Fact]
    public void AcknowledgingThePersistFailureLetsOrdinaryWritesThroughAgain()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            ErrorMessage = "the persist-failure message",
            StolenPersistFailurePending = true,
        };

        vm.AcknowledgeStolenPersistFailure();
        vm.SetGuardedError("an ordinary write");

        Assert.Equal("an ordinary write", vm.ErrorMessage);
        Assert.False(vm.StolenPersistFailurePending);
    }

    /// <summary>
    /// The regression review flagged: <c>ResetForIdentityChange</c>
    /// (fired on an account switch mid-visit, <c>SettingsAccountPage.HydrateRecoveryKitAsync</c>)
    /// must not refuse its OWN <c>ErrorMessage = null</c> write by tripping over the
    /// very guard it is supposed to discharge — a previous identity's parked
    /// persist-failure message (its seed) must not stay on screen for the identity
    /// that just signed in. <c>ClearHeldSecrets</c> (called first) is what
    /// discharges the flag before the write, so ordering here is load-bearing —
    /// apple's <c>resetForIdentityChange</c> has the identical shape.
    ///
    /// <para>Mutation check: discharging AFTER (or never) rather than before the
    /// <c>ErrorMessage</c> write reds this.</para>
    /// </summary>
    [Fact]
    public void AnIdentityChangeDischargesRatherThanRefusingItsOwnErrorClear()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            ErrorMessage = "the PREVIOUS identity's persist-failure message",
            StolenPersistFailurePending = true,
        };

        vm.ResetForIdentityChange();

        Assert.Null(vm.ErrorMessage);
        Assert.False(vm.StolenPersistFailurePending);
    }

    /// <summary><c>ClearHeldSecrets</c> alone — the page's <c>OnNavigatedFrom</c>
    /// nav-away edge, distinct from an identity change — discharges too, so a
    /// later ordinary write on the SAME identity's next visit isn't silently
    /// dropped forever.</summary>
    [Fact]
    public void LeavingTheAccountPageDischargesThePendingMessage()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            ErrorMessage = "the persist-failure message",
            StolenPersistFailurePending = true,
        };

        vm.ClearHeldSecrets();

        Assert.False(vm.StolenPersistFailurePending);
    }

    // ── The post-succession sweep's own lines ──────────────────

    private static uniffi.fauna_ffi.FfiSweepView MakeSweepView(
        string kind = "ran", string? detail = null, uint groups = 2,
        uint groupsOldLeafRemoved = 2, uint unattestedMembers = 0, bool owesWork = false) =>
        new(kind, detail, groups, groupsOldLeafRemoved, unattestedMembers, owesWork);

    /// <summary>Mirrored, not consumed: an ordinary hydrate re-reads the handoff's
    /// carried view every time, which must be idempotent.</summary>
    [Fact]
    public void HydrateSweep_MirrorsTheHandoffsCarriedView()
    {
        var view = MakeSweepView();
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor)
                with { sweep = view },
            Predecessor);
        var vm = new RecoveryKitViewModel(new MockNestRpcClient());

        vm.HydrateSweep();
        Assert.Same(view, vm.SweepView);

        // Idempotent: hydrating again changes nothing.
        vm.HydrateSweep();
        Assert.Same(view, vm.SweepView);
    }

    /// <summary>An ordinary sign-in that never ran a ceremony carries no sweep —
    /// the panel stays fully collapsed, never a silence dressed as an answer.</summary>
    [Fact]
    public void NoSweep_RendersNoLines()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient());
        Assert.Null(vm.SweepView);
        Assert.Null(vm.SweepOutcomeLine);
        Assert.Null(vm.SweepUnattestedLine);
        Assert.False(vm.SweepOwesWork);
    }

    // FFI contract — localizer-independent (asserts the LocalizedText key
    // directly), mirroring ThreadLabelDisplayTests' convention: the shared
    // `SweepView::copy` projection selects the line, this app only localizes.
    [Fact]
    public void SweepCopy_AllRemoved_SelectsTheAllRemovedKey_NoUnattestedLine()
    {
        var copy = uniffi.fauna_ffi.FaunaFfiMethods.SweepCopy(
            MakeSweepView(groups: 3, groupsOldLeafRemoved: 3), rendersRetry: true);
        Assert.Equal("settings.recovery_kit.sweep_all_removed", copy.outcome?.key);
        Assert.Equal("3", copy.outcome?.args["groups"]);
        Assert.Null(copy.unattested);
    }

    [Fact]
    public void SweepCopy_Partial_SelectsThePartialKey_PlusItsOwnUnattestedLine()
    {
        var copy = uniffi.fauna_ffi.FaunaFfiMethods.SweepCopy(
            MakeSweepView(groups: 3, groupsOldLeafRemoved: 1, unattestedMembers: 2, owesWork: true),
            rendersRetry: true);
        Assert.Equal("settings.recovery_kit.sweep_partial", copy.outcome?.key);
        Assert.Equal("1", copy.outcome?.args["removed"]);
        Assert.Equal("3", copy.outcome?.args["groups"]);
        Assert.Equal("settings.recovery_kit.sweep_unattested", copy.unattested?.key);
        Assert.Equal("2", copy.unattested?.args["count"]);
    }

    /// <summary>A succession over an account with NO groups at all is silent —
    /// "removed from all 0 of your groups" would be noise dressed as
    /// reassurance, the shared projection's own rule.</summary>
    [Fact]
    public void SweepCopy_ZeroGroups_IsSilent()
    {
        var copy = uniffi.fauna_ffi.FaunaFfiMethods.SweepCopy(
            MakeSweepView(groups: 0, groupsOldLeafRemoved: 0), rendersRetry: true);
        Assert.Null(copy.outcome);
        Assert.Null(copy.unattested);
    }

    // Windows render path — the exact expression the section paints, through the
    // real Strings.Resolve pipeline with the fake's real templates registered above.
    [Fact]
    public void SweepOutcomeLine_ResolvesThroughWindowsPipeline_WithSubstitution()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            SweepView = MakeSweepView(groups: 4, groupsOldLeafRemoved: 4),
        };
        Assert.Equal(
            "Your old identity was removed from all 4 of your group conversations.",
            vm.SweepOutcomeLine);
        Assert.Null(vm.SweepUnattestedLine);
    }

    [Fact]
    public void SweepUnattestedLine_ResolvesAsItsOwnLine_NeverFoldedIntoTheOutcome()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            SweepView = MakeSweepView(
                groups: 3, groupsOldLeafRemoved: 1, unattestedMembers: 2, owesWork: true),
        };
        Assert.Equal(
            "Your old identity was removed from 1 of your 3 group conversations.",
            vm.SweepOutcomeLine);
        Assert.Equal(
            "There are 2 other members this cannot confirm you added yourself.",
            vm.SweepUnattestedLine);
        Assert.True(vm.SweepOwesWork);
    }

    /// <summary>The render gate is unfinished work, never "this device can
    /// retry" — a fully-swept view owes nothing even though the button's
    /// resw/id both exist on this platform.</summary>
    [Fact]
    public void SweepOwesWork_IsFalseOnceEveryGroupIsSwept()
    {
        var vm = new RecoveryKitViewModel(new MockNestRpcClient())
        {
            SweepView = MakeSweepView(groups: 2, groupsOldLeafRemoved: 2, owesWork: false),
        };
        Assert.False(vm.SweepOwesWork);
    }

    /// <summary>
    /// The happy arm: the retry's fresh report REPLACES the carried view (never
    /// re-painting the old one, which would show the user the state their press
    /// just fixed), the handoff's both halves move together, and the swept arm
    /// says nothing on the error surface — its outcome renders through the two
    /// lines instead.
    /// </summary>
    [Fact]
    public async Task RetrySweep_OnSwept_ReplacesTheViewAndTheHandoff_NoError()
    {
        var stale = MakeSweepView(groups: 3, groupsOldLeafRemoved: 1, owesWork: true);
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor) with { sweep = stale },
            Predecessor);
        var fresh = MakeSweepView(groups: 3, groupsOldLeafRemoved: 3, owesWork: false);
        var rpc = new MockNestRpcClient
        {
            NextSweepRetryAnswer = new uniffi.fauna_ffi.FfiSweepRetryAnswer(
                "swept", null, fresh, "{\"kind\":\"ran\"}", Array.Empty<byte[]>()),
        };
        var vm = new RecoveryKitViewModel(rpc);
        vm.HydrateSweep();
        Assert.Same(stale, vm.SweepView);

        await vm.RetrySweepAsync();

        Assert.Contains("SuccessionRetryGroupSweep", rpc.Calls);
        Assert.Same(fresh, vm.SweepView);
        Assert.Same(fresh, SuccessionHandoff.Sweep);
        Assert.Equal("{\"kind\":\"ran\"}", SuccessionHandoff.SweepStateJson);
        Assert.Null(vm.ErrorMessage);
        Assert.False(vm.Busy);
    }

    /// <summary>
    /// The other four arms: nothing was swept, so nothing about the sweep
    /// changes — but the press must still say something, since the button
    /// renders on unfinished work regardless of whether THIS device can
    /// finish it.
    /// </summary>
    [Theory]
    [InlineData("no-old-state")]
    [InlineData("not-landed")]
    [InlineData("landed-for-another")]
    [InlineData("failed")]
    public async Task RetrySweep_OnANonSweptArm_SetsTheErrorAndLeavesTheViewUntouched(string kind)
    {
        var carried = MakeSweepView(groups: 3, groupsOldLeafRemoved: 1, owesWork: true);
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor) with { sweep = carried },
            Predecessor);
        var rpc = new MockNestRpcClient
        {
            NextSweepRetryAnswer = new uniffi.fauna_ffi.FfiSweepRetryAnswer(
                kind,
                new uniffi.fauna_core.LocalizedText("settings.recovery_kit.sweep_retry_no_old_state", new()),
                null, null, Array.Empty<byte[]>()),
        };
        var vm = new RecoveryKitViewModel(rpc);
        vm.HydrateSweep();

        await vm.RetrySweepAsync();

        Assert.Same(carried, vm.SweepView);
        Assert.Same(carried, SuccessionHandoff.Sweep);
        Assert.NotNull(vm.ErrorMessage);
        Assert.Equal(
            "This device does not have the conversation history from your previous identity.",
            vm.ErrorMessage);
    }

    // ── A relaunch adoption's owed sweep ─────────────────────────────────

    /// <summary>
    /// The unbidden press (<c>succession-propagation.md</c> § Propagation → <i>Own
    /// device fleet</i>, the relaunch-adoption clause): the successor's session runs
    /// the owed sweep once, parks the report shared Rust chose, and folds the press's
    /// sentence onto the error surface exactly as a press would.
    /// </summary>
    [Fact]
    public async Task OwedSweep_TheSuccessorPressesItUnbidden_AndParksTheAnswer()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);
        var parked = MakeSweepView(groups: 2, groupsOldLeafRemoved: 0, owesWork: true);
        var rpc = new MockNestRpcClient
        {
            NextOwedSweepAnswer = new uniffi.fauna_ffi.FfiOwedSweepAnswer(
                new uniffi.fauna_ffi.FfiSweepRetryAnswer(
                    "no-old-state",
                    new uniffi.fauna_core.LocalizedText("settings.recovery_kit.sweep_retry_no_old_state", new()),
                    null, null, Array.Empty<byte[]>()),
                parked, "{\"kind\":\"no_engine\"}"),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.DischargeOwedSweepAsync(Successor);

        Assert.Contains("SuccessionDischargeOwedSweep", rpc.Calls);
        Assert.Same(parked, vm.SweepView);
        Assert.Same(parked, SuccessionHandoff.Sweep);
        Assert.Equal("{\"kind\":\"no_engine\"}", SuccessionHandoff.SweepStateJson);
        Assert.Equal(
            "This device does not have the conversation history from your previous identity.",
            vm.ErrorMessage);
        Assert.Null(SuccessionHandoff.SweepOwedTo);
        Assert.False(vm.Busy);
    }

    [Fact]
    public async Task OwedSweep_OnSwept_SaysNothingOnTheErrorSurface()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);
        var fresh = MakeSweepView(groups: 2, groupsOldLeafRemoved: 2, owesWork: false);
        var rpc = new MockNestRpcClient
        {
            NextOwedSweepAnswer = new uniffi.fauna_ffi.FfiOwedSweepAnswer(
                new uniffi.fauna_ffi.FfiSweepRetryAnswer(
                    "swept", null, fresh, "{\"kind\":\"ran\"}", Array.Empty<byte[]>()),
                fresh, "{\"kind\":\"ran\"}"),
        };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.DischargeOwedSweepAsync(Successor);

        Assert.Same(fresh, vm.SweepView);
        Assert.Null(vm.ErrorMessage);
    }

    /// <summary>An ordinary visit, and the departing session, run nothing.</summary>
    [Fact]
    public async Task OwedSweep_NotOwedHere_RunsNothing()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc);
        await vm.DischargeOwedSweepAsync(Successor);
        Assert.DoesNotContain("SuccessionDischargeOwedSweep", rpc.Calls);

        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);
        await vm.DischargeOwedSweepAsync(Predecessor);
        Assert.DoesNotContain("SuccessionDischargeOwedSweep", rpc.Calls);
        Assert.Equal(Successor, SuccessionHandoff.SweepOwedTo);
    }

    /// <summary>A busy view model defers rather than spends the obligation.</summary>
    [Fact]
    public async Task OwedSweep_WhileBusy_KeepsTheObligation()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc) { Busy = true };

        await vm.DischargeOwedSweepAsync(Successor);

        Assert.DoesNotContain("SuccessionDischargeOwedSweep", rpc.Calls);
        Assert.Equal(Successor, SuccessionHandoff.SweepOwedTo);
    }

    /// <summary>A press that could not run at all puts the obligation back.</summary>
    [Fact]
    public async Task OwedSweep_WhenThePressThrows_RearmsIt()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);
        var rpc = new MockNestRpcClient { NextError = "no secret for the owed sweep" };
        var vm = new RecoveryKitViewModel(rpc);

        await vm.DischargeOwedSweepAsync(Successor);

        Assert.Equal(Successor, SuccessionHandoff.SweepOwedTo);
        Assert.NotNull(vm.ErrorMessage);
        Assert.False(vm.Busy);
    }

    /// <summary>Busy guards the retry exactly like every other ceremony — a
    /// second click while one is in flight must not start a second.</summary>
    [Fact]
    public async Task RetrySweep_RefusesWhileBusy()
    {
        var rpc = new MockNestRpcClient();
        var vm = new RecoveryKitViewModel(rpc) { Busy = true };

        await vm.RetrySweepAsync();

        Assert.DoesNotContain("SuccessionRetryGroupSweep", rpc.Calls);
    }
}
