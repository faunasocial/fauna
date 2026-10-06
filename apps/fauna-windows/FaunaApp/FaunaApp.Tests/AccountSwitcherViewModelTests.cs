using System.Collections.Specialized;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the multi-account switcher VM
/// (<c>docs/goal/architecture/long-term-store.md</c> § Multi-account evolution), over a
/// <see cref="FakeAccountRegistry"/> implementing the same UniFFI
/// <see cref="IFfiAccountRegistry"/> seam the real <c>FfiAccountRegistry</c> does — the
/// established windows pattern (a VM over the generated <c>I&lt;Thing&gt;</c> interface,
/// faked; no hand-written seam, no FlaUI, which flakes on win-arm64).
///
/// <para>The row-title test deliberately asserts against the <b>shared</b>
/// <c>FaunaFfiMethods.AccountDisplayLabel</c> rather than a literal, so it pins
/// <i>delegation</i> to <c>fauna_core::format::account_display_label</c> and cannot be
/// satisfied by a windows-local re-derivation of the handle-else-short-id branch (priority
/// #1/#4 — linux and web each once re-derived it and drifted on the empty-handle case).
/// The native <c>fauna_ffi</c> dll loads in the test host (memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>), same as
/// <see cref="ShortIdFfiTests"/>.</para>
/// </summary>
public class AccountSwitcherViewModelTests
{
    private const string ActorA = "aa11111111111111111111111111111111111111111111111111111111111111";
    private const string ActorB = "bb22222222222222222222222222222222222222222222222222222222222222";
    private const string ActorC = "cc33333333333333333333333333333333333333333333333333333333333333";

    /// <summary>The remove door answering "nobody serves it" — so a remove test never
    /// probes the real install base's lock files.</summary>
    private static FfiEraseRemoveBlocked? NobodyServes(string _) => null;

    // ── Row projection ──

    /// <summary>Rows follow the registry's add order (one switcher row per entry), and the
    /// title is the cached handle when there is one.</summary>
    [Fact]
    public void Refresh_ProjectsRowsInRegistryOrder_WithHandleTitles()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA, handle: "alice"),
                FakeAccountRegistry.Entry(ActorB, handle: "bob"),
            });
        var vm = new AccountSwitcherViewModel(fake);

        vm.Refresh();

        Assert.Collection(vm.Accounts,
            r => { Assert.Equal(ActorA, r.ActorId); Assert.Equal("alice", r.DisplayLabel); },
            r => { Assert.Equal(ActorB, r.ActorId); Assert.Equal("bob", r.DisplayLabel); });
        Assert.Null(vm.ErrorMessage);
    }

    /// <summary>
    /// The empty-handle case is exactly where a hand-rolled "handle ?? short id" fallback
    /// drifts (an account whose cache holds <c>""</c> renders a blank row). Asserting the
    /// label equals what the shared FFI function returns pins the delegation itself — a
    /// windows-local re-implementation would have to reproduce shared Rust byte-for-byte to
    /// pass, and would still be the divergence this pins against.
    /// </summary>
    [Theory]
    [InlineData(null)]
    [InlineData("")]
    public void Refresh_HandleMissingOrEmpty_TitleComesFromTheSharedFfiLabel(string? handle)
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA, handle: handle) });
        var vm = new AccountSwitcherViewModel(fake);

        vm.Refresh();

        var row = Assert.Single(vm.Accounts);
        Assert.Equal(FaunaFfiMethods.AccountDisplayLabel(null, ActorA), row.DisplayLabel);
        // Sanity: the shared fallback is not the raw 64-char actor id.
        Assert.NotEqual(ActorA, row.DisplayLabel);
    }

    /// <summary>Exactly one row carries the active marker
    /// (<c>account-item-active-indicator</c>), and it is the one
    /// <c>registry.Active()</c> names.</summary>
    [Fact]
    public void Refresh_MarksExactlyTheActiveRow()
    {
        var fake = new FakeAccountRegistry(
            active: ActorB,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB),
                FakeAccountRegistry.Entry(ActorC),
            });
        var vm = new AccountSwitcherViewModel(fake);

        vm.Refresh();

        var active = Assert.Single(vm.Accounts, r => r.IsActive);
        Assert.Equal(ActorB, active.ActorId);
        Assert.Equal(ActorB, vm.ActiveActorId);
    }

    /// <summary>
    /// <b>The bound-secondary case.</b> A window launched bound to B (the launch-collision
    /// chooser, or <c>FAUNA_BOUND_ACCOUNT</c>) serves B while the registry's active pointer
    /// still names A — a secondary never moves it. The "account in use" row (indicator, no
    /// switch, no <c>account-remove-button</c>) is the one this window SERVES
    /// (<c>session_account</c>), never the registry's active one: keyed on the registry,
    /// B's row offered remove, and the remove erased the stores this process was running
    /// from (<c>account-scoping.md</c> § Concurrent instances → <i>Remove-account also refuses
    /// the account THIS instance serves</i>).
    /// </summary>
    [Fact]
    public void Refresh_MarksTheServedAccount_NotTheRegistryActiveOne()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA), FakeAccountRegistry.Entry(ActorB) })
        { Served = ActorB };
        var vm = new AccountSwitcherViewModel(fake);

        vm.Refresh();

        var inUse = Assert.Single(vm.Accounts, r => r.IsActive);
        Assert.Equal(ActorB, inUse.ActorId);
        Assert.False(inUse.CanRemove, "the account this window serves must not offer remove");
        Assert.True(vm.Accounts.Single(r => r.ActorId == ActorA).CanRemove);
        Assert.Equal(ActorB, vm.ActiveActorId);
    }

    /// <summary>Tapping the account this window serves is the no-op, even though the
    /// registry's active pointer names another account.</summary>
    [Fact]
    public async Task SwitchTo_TheServedAccount_IsANoOp_OnABoundSecondary()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA), FakeAccountRegistry.Entry(ActorB) })
        { Served = ActorB };
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        var requests = 0;
        vm.OnSwitchRequested = (_, _) => { requests++; return Task.CompletedTask; };

        await vm.SwitchToAsync(ActorB);

        Assert.Equal(0, requests);
    }

    /// <summary>The remove affordance (<c>account-remove-button</c>) renders on NON-active
    /// rows only — removing the running identity would leave the client authenticated as an
    /// account it just forgot (the shared <c>remove()</c> would promote the first remaining
    /// one, but the live session would not follow).</summary>
    [Fact]
    public void Refresh_CanRemove_IsFalseOnTheActiveRow_TrueOnTheOthers()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB),
            });
        var vm = new AccountSwitcherViewModel(fake);

        vm.Refresh();

        Assert.False(vm.Accounts[0].CanRemove, "the active row must not offer remove");
        Assert.True(vm.Accounts[1].CanRemove);
    }

    /// <summary>
    /// <b>The anti-stale-flag regression.</b> A build-once settings surface that caches
    /// <c>require_confirm_to_activate</c> renders a stale <c>false</c> over a flag the admin
    /// auto-default has since set — which makes the flag <i>impossible to turn off</i>: the
    /// user's tap on an OFF-looking toggle writes ON (long-term-store.md § Multi-account
    /// evolution → "Clients must read the flag fresh at activation and at render"; linux hit
    /// exactly this). So every <c>Refresh</c> must re-read the registry.
    /// </summary>
    [Fact]
    public void Refresh_RereadsRequireConfirm_NeverCachesTheFlag()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA, requireConfirm: false) });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        Assert.False(Assert.Single(vm.Accounts).RequireConfirmToActivate);

        // The admin auto-default flips it ON underneath the page (no VM call involved).
        fake.SetRequireConfirm(ActorA, true);
        vm.Refresh();

        Assert.True(Assert.Single(vm.Accounts).RequireConfirmToActivate,
            "the flag must be re-read on every refresh, not cached at construction");
    }

    // ── Remove ──

    /// <summary>
    /// Remove calls through to the registry and refreshes the SAME bound collection —
    /// removing a non-active account does not switch, so nothing rebuilds the page and no
    /// re-navigation happens. The e2e polls the already-navigated page, so an implementation
    /// that only refreshes on navigate-to would hang there.
    /// </summary>
    [Fact]
    public void Remove_CallsRegistry_AndShrinksTheBoundCollectionInPlace()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB),
            });
        var erased = new List<string>();
        var vm = new AccountSwitcherViewModel(
            fake, eraseAccountState: erased.Add, removeAccountBlocked: NobodyServes);
        vm.Refresh();
        var bound = vm.Accounts;           // the instance the page bound to
        var changes = 0;
        NotifyCollectionChangedEventHandler handler = (_, _) => changes++;
        bound.CollectionChanged += handler;

        vm.Remove(ActorB);

        Assert.Equal(new[] { ActorB }, fake.Removed);
        // Erasure follows scope: a successful remove also erases the account's
        // scoped content stores (account-scoping.md § Erasure follows scope).
        Assert.Equal(new[] { ActorB }, erased);
        Assert.Same(bound, vm.Accounts);   // no new collection — the page's binding survives
        Assert.Equal(ActorA, Assert.Single(bound).ActorId);
        Assert.True(changes > 0, "the bound collection must raise CollectionChanged in place");
        bound.CollectionChanged -= handler;
        Assert.Null(vm.ErrorMessage);
    }

    /// <summary>A registry rejection surfaces on the page's <c>error-message</c> banner and
    /// leaves the list intact.</summary>
    [Fact]
    public void Remove_Failure_SurfacesTheErrorAndKeepsTheList()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB),
            })
        { ThrowOnRemove = true };
        var erased = new List<string>();
        var vm = new AccountSwitcherViewModel(
            fake, eraseAccountState: erased.Add, removeAccountBlocked: NobodyServes);
        vm.Refresh();

        vm.Remove(ActorB);

        Assert.NotNull(vm.ErrorMessage);
        // A rejected removal must NOT erase the account's stores (the account is
        // still on this install).
        Assert.Empty(erased);
        Assert.Equal(2, vm.Accounts.Count);
    }

    /// <summary>
    /// A remove that reaches the path anyway (a stale row, a race with a launch) asks the
    /// shared door BEFORE the registry removal — which drops the account's secret slots, so
    /// a refusal after it would strand the scopes with nothing left to sign in to them. On
    /// <c>ServedHere</c> it paints the door's own line (<c>settings.remove_account_blocked_this_window</c>)
    /// on <c>error-message</c>, and nothing is removed or erased.
    /// </summary>
    [Fact]
    public void Remove_TheServedAccount_IsRefusedWithTheThisWindowLine_AndTouchesNothing()
    {
        var line = new uniffi.fauna_core.LocalizedText(
            "settings.remove_account_blocked_this_window", new Dictionary<string, string>());
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA), FakeAccountRegistry.Entry(ActorB) })
        { Served = ActorB };
        var erased = new List<string>();
        var asked = new List<string>();
        var vm = new AccountSwitcherViewModel(
            fake,
            eraseAccountState: erased.Add,
            removeAccountBlocked: actor =>
            {
                asked.Add(actor);
                return new FfiEraseRemoveBlocked.ServedHere(line);
            });
        vm.Refresh();

        vm.Remove(ActorB);

        Assert.Equal(new[] { ActorB }, asked);
        Assert.Empty(fake.Removed);
        Assert.Empty(erased);
        Assert.Equal(FaunaApp.Core.Services.Strings.Resolve(line), vm.ErrorMessage);
        Assert.Equal(2, vm.Accounts.Count);
    }

    /// <summary>The sibling half of the same door: an account another live window (of this
    /// app or another on the same login) serves is refused with the other-window line, and
    /// nothing is removed or erased.</summary>
    [Fact]
    public void Remove_AnAccountASiblingServes_IsRefusedWithTheOtherWindowLine()
    {
        var line = new uniffi.fauna_core.LocalizedText(
            "settings.remove_account_blocked_other_window", new Dictionary<string, string>());
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA), FakeAccountRegistry.Entry(ActorB) });
        var erased = new List<string>();
        var vm = new AccountSwitcherViewModel(
            fake,
            eraseAccountState: erased.Add,
            removeAccountBlocked: _ => new FfiEraseRemoveBlocked.ServedElsewhere(new[] { ActorB }, line));
        vm.Refresh();

        vm.Remove(ActorB);

        Assert.Empty(fake.Removed);
        Assert.Empty(erased);
        Assert.Equal(FaunaApp.Core.Services.Strings.Resolve(line), vm.ErrorMessage);
        Assert.Equal(2, vm.Accounts.Count);
    }

    // ── Require-confirm toggle ──

    /// <summary>The per-row <c>account-require-confirm-toggle</c> write path. Setting the
    /// flag never prompts (only activating a flagged account does), and the list re-reads so
    /// the row reflects what the registry actually stored.</summary>
    [Fact]
    public void SetRequireConfirm_WritesThroughAndRerenders()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA, requireConfirm: false) });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();

        vm.SetRequireConfirm(ActorA, true);

        Assert.True(Assert.Single(vm.Accounts).RequireConfirmToActivate);
    }

    // ── Switch ──

    /// <summary>
    /// Switching tears down and rebuilds the whole authenticated session (nest clients, MLS
    /// engine, backup driver, launch machine), which is app-owned state a FaunaApp.Core VM
    /// cannot reach — so the VM only <i>requests</i> the switch through a callback the app
    /// root fills in, and touches the registry not at all. (Same seam shape as apple's
    /// <c>AccountSwitcherVM.requestSwitch</c> → <c>onSwitch</c>.)
    ///
    /// <para>An UNflagged account never prompts, so it requests the switch with
    /// <c>confirmed: false</c> — the app then routes it through the plain <c>SetActive</c>.</para>
    /// </summary>
    [Fact]
    public async Task SwitchTo_RequestsTheSwitch_WithoutMutatingTheRegistry()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        string? requested = null;
        bool? requestedConfirmed = null;
        vm.OnSwitchRequested = (id, confirmed) =>
        {
            requested = id;
            requestedConfirmed = confirmed;
            return Task.CompletedTask;
        };

        await vm.SwitchToAsync(ActorB);

        Assert.Equal(ActorB, requested);
        Assert.False(requestedConfirmed);       // unflagged → the plain (non-re-auth) path
        Assert.Empty(fake.Activated);           // the VM must NOT call SetActive itself
        Assert.Equal(ActorA, fake.Active());    // …and so the registry is untouched
    }

    /// <summary>Tapping the row you are already on is a no-op — no teardown, no rebuild.</summary>
    [Fact]
    public async Task SwitchTo_TheActiveAccount_IsANoOp()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA) });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        var requests = 0;
        vm.OnSwitchRequested = (_, _) => { requests++; return Task.CompletedTask; };

        await vm.SwitchToAsync(ActorA);

        Assert.Equal(0, requests);
    }

    // ── Switch: the Stage-2 re-auth gate ──
    //
    // A require_confirm_to_activate-flagged account demands a native re-auth prompt
    // (Windows Hello) BEFORE the switch runs (long-term-store.md § Multi-account
    // evolution → Per-account re-auth). The prompt is app/platform state (WinRT
    // UserConsentVerifier is unreachable from FaunaApp.Core's plain-net10.0 TFM), so
    // it is an injected seam — the VM owns only the branch: read the flag fresh →
    // maybe gate → request the switch as confirmed/unconfirmed. This mirrors apple's
    // AccountSwitcherVM.requestSwitch → AccountReauth.confirmActivation().

    /// <summary>A flagged account whose re-auth is APPROVED requests the switch with
    /// <c>confirmed: true</c> — the app then routes it through <c>SetActiveConfirmed</c>, the
    /// only post-re-auth activation path.</summary>
    [Fact]
    public async Task SwitchTo_FlaggedAccount_ApprovedReauth_RequestsConfirmedSwitch()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB, requireConfirm: true),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        string? requested = null;
        bool? requestedConfirmed = null;
        vm.ConfirmReauth = () => Task.FromResult(true);   // Hello approved
        vm.OnSwitchRequested = (id, confirmed) =>
        {
            requested = id;
            requestedConfirmed = confirmed;
            return Task.CompletedTask;
        };

        await vm.SwitchToAsync(ActorB);

        Assert.Equal(ActorB, requested);
        Assert.True(requestedConfirmed);        // approved → the confirmed activation path
        Assert.Empty(fake.Activated);           // the VM still never activates itself
    }

    /// <summary>Declining the re-auth is a PURE no-op: no switch is requested, the registry
    /// is untouched, and nothing lands on the error banner — the user cancelled it
    /// themselves (long-term-store.md — "Declining is a pure no-op").</summary>
    [Fact]
    public async Task SwitchTo_FlaggedAccount_DeclinedReauth_IsPureNoOp()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB, requireConfirm: true),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        var requests = 0;
        vm.ConfirmReauth = () => Task.FromResult(false);  // Hello declined / cancelled
        vm.OnSwitchRequested = (_, _) => { requests++; return Task.CompletedTask; };

        await vm.SwitchToAsync(ActorB);

        Assert.Equal(0, requests);              // no switch requested
        Assert.Empty(fake.Activated);
        Assert.Equal(ActorA, fake.Active());    // still on the original account
        Assert.Null(vm.ErrorMessage);           // a self-cancel is not an error
    }

    /// <summary>
    /// Every activation gesture counts itself on completion, whatever it decided —
    /// <c>fauna_e2e_agent::ACTIVATION_GESTURES_KEY</c>, the completion observable the
    /// e2e decline arm anchors <c>assert_no_relaunch</c>'s barrier to.
    ///
    /// <para>This is the half the e2e cannot grade. A counter that only advanced on the
    /// happy path would make the negative assert <b>hang</b> rather than fail, so its red
    /// would read as a timeout — a different bug from the one it names. The three arms
    /// below are exactly the three ways this method can leave: the declined gate (an
    /// early <c>return</c> from the inner try), the already-active row (an early
    /// <c>return</c> before it), and a throwing switch handler (caught by
    /// <c>ShowError</c>). Red-verify by moving the bump out of the <c>finally</c>.</para>
    /// </summary>
    [Fact]
    public async Task EveryActivationGesture_CountsItself_WhateverItDecided()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB, requireConfirm: true),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();

        var before = FaunaApp.Core.Services.E2eSessionCounters.ActivationGestures;

        // (1) The declined gate — the arm the e2e decline journey drives.
        vm.ConfirmReauth = () => Task.FromResult(false);
        vm.OnSwitchRequested = (_, _) => Task.CompletedTask;
        await vm.SwitchToAsync(ActorB);
        Assert.Equal(before + 1, FaunaApp.Core.Services.E2eSessionCounters.ActivationGestures);

        // (2) Tapping the already-active row — returns before the gate is even reached.
        await vm.SwitchToAsync(ActorA);
        Assert.Equal(before + 2, FaunaApp.Core.Services.E2eSessionCounters.ActivationGestures);

        // (3) A switch handler that throws — the gesture still FINISHED, and a waiting
        //     test must be told so rather than left to time out.
        vm.ConfirmReauth = () => Task.FromResult(true);
        vm.OnSwitchRequested = (_, _) => throw new InvalidOperationException("boom");
        await vm.SwitchToAsync(ActorB);
        Assert.Equal(before + 3, FaunaApp.Core.Services.E2eSessionCounters.ActivationGestures);
        Assert.NotNull(vm.ErrorMessage);
    }

    /// <summary>The counter is monotonic — the property that makes a late read
    /// conservative rather than vacuous, and the reason this is a count and not an
    /// "in flight" flag (<c>fauna_e2e_agent::ACTIVATION_GESTURES_KEY</c> argues it out:
    /// a flag's <c>false</c> is also its pre-gesture value, so a <c>settled</c> reading
    /// one can pass on the world before the tap).</summary>
    [Fact]
    public async Task ActivationGestureCounter_NeverDecreases()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[] { FakeAccountRegistry.Entry(ActorA), FakeAccountRegistry.Entry(ActorB) });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        vm.OnSwitchRequested = (_, _) => Task.CompletedTask;

        var seen = FaunaApp.Core.Services.E2eSessionCounters.ActivationGestures;
        foreach (var target in new[] { ActorB, ActorA, ActorB, ActorB })
        {
            await vm.SwitchToAsync(target);
            var now = FaunaApp.Core.Services.E2eSessionCounters.ActivationGestures;
            Assert.True(now > seen, $"the counter must advance on every gesture: {seen} -> {now}");
            seen = now;
        }
    }

    /// <summary>Fail-closed: a flagged account with NO re-auth gate configured cannot be
    /// activated — the switch is not requested. (In production a null gate cannot occur, but
    /// the security property must hold structurally: the default is decline, never allow.)</summary>
    [Fact]
    public async Task SwitchTo_FlaggedAccount_NoGateConfigured_FailsClosed()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB, requireConfirm: true),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        var requests = 0;
        vm.ConfirmReauth = null;                 // no native gate wired
        vm.OnSwitchRequested = (_, _) => { requests++; return Task.CompletedTask; };

        await vm.SwitchToAsync(ActorB);

        Assert.Equal(0, requests);               // fail-closed — no switch
    }

    /// <summary>An UNflagged switch never consults the re-auth gate (no needless prompt on
    /// the common path).</summary>
    [Fact]
    public async Task SwitchTo_UnflaggedAccount_NeverConsultsTheGate()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB, requireConfirm: false),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        var gateConsulted = false;
        vm.ConfirmReauth = () => { gateConsulted = true; return Task.FromResult(true); };
        vm.OnSwitchRequested = (_, _) => Task.CompletedTask;

        await vm.SwitchToAsync(ActorB);

        Assert.False(gateConsulted, "an unflagged account must never trigger the re-auth prompt");
    }

    /// <summary>
    /// The gate reads the flag FRESH from the registry at activation, never the rendered row.
    /// The admin auto-default flips the flag ON underneath the page (no VM refresh), so a VM
    /// that decided from its cached row would skip the prompt on a stale <c>false</c>. Here
    /// the flag is flipped after <see cref="AccountSwitcherViewModel.Refresh"/> and the gate
    /// must still fire (long-term-store.md — "Clients must read the flag fresh at activation").
    /// </summary>
    [Fact]
    public async Task SwitchTo_ReadsTheFlagFresh_NotTheStaleRenderedRow()
    {
        var fake = new FakeAccountRegistry(
            active: ActorA,
            entries: new[]
            {
                FakeAccountRegistry.Entry(ActorA),
                FakeAccountRegistry.Entry(ActorB, requireConfirm: false),
            });
        var vm = new AccountSwitcherViewModel(fake);
        vm.Refresh();
        Assert.False(vm.Accounts[1].RequireConfirmToActivate);   // the rendered row is stale-OFF

        // The auto-default flips it ON in the store, with NO vm.Refresh().
        fake.SetRequireConfirm(ActorB, true);

        var gateConsulted = false;
        vm.ConfirmReauth = () => { gateConsulted = true; return Task.FromResult(false); };
        var requests = 0;
        vm.OnSwitchRequested = (_, _) => { requests++; return Task.CompletedTask; };

        await vm.SwitchToAsync(ActorB);

        Assert.True(gateConsulted, "activation must re-read the flag from the registry, not the cached row");
        Assert.Equal(0, requests);               // declined above → no switch
    }
}

/// <summary>
/// In-memory <see cref="IFfiAccountRegistry"/> for the switcher VM tests — the C# peer of
/// the Rust <c>fauna-client-accounts</c> registry tests' in-memory store. Models the
/// account list + the active pointer + the per-account require-confirm flag, records what
/// the VM called (so a test can assert the VM did <i>not</i> activate), and can be made to
/// reject a remove. Only the members the switcher touches are <c>override</c>n; the rest
/// inherit <see cref="FfiAccountRegistryFakeBase"/>'s throwing default — the boot/onboarding/
/// bind/migration surface the app owns, not the switcher VM. Notably app-owned rather than
/// switcher surface: the bind-without-activate pair (<c>BindAccount</c>/
/// <c>BindAccountConfirmed</c>/<c>BoundLaunchPersistence</c>, concurrent-instances
/// groundwork) and <c>ClearNestBinding</c> ("use a different nest") — the switcher VM only
/// lists and requests, it never binds or walks an account off its nest; the admin
/// auto-default <c>AutoEnableRequireConfirm</c>, driven by the client's own am-i-admin
/// observation, not the switcher list; the one-read <c>SessionMaterial</c>, which the app
/// builds its authenticated session from; and the onboarding-owned wizard-exit slots
/// (<c>PersistAwaitingDns</c>/<c>ClearAwaitingDns</c>/<c>PersistPendingInvite</c>) that park
/// an AwaitingDns/pending-invite resume state — the switcher VM only lists already-admitted
/// accounts. A VM that started calling any of these fails loudly here instead of silently
/// gaining a contract change.
/// </summary>
internal sealed class FakeAccountRegistry : FfiAccountRegistryFakeBase
{
    private readonly List<FfiAccountEntry> _entries = new();
    private string? _active;

    /// <summary>Make <see cref="Remove"/> throw (the registry-rejection path).</summary>
    public bool ThrowOnRemove { get; set; }

    /// <summary>Actor ids passed to <see cref="Remove"/>, in order.</summary>
    public List<string> Removed { get; } = new();

    /// <summary>Actor ids passed to <see cref="SetActive"/> / <see cref="SetActiveConfirmed"/>,
    /// in order — asserted EMPTY: the VM requests a switch, the app performs it (both the
    /// plain and the post-re-auth activation path are app-owned).</summary>
    public List<string> Activated { get; } = new();

    public FakeAccountRegistry(string? active = null, IEnumerable<FfiAccountEntry>? entries = null)
    {
        if (entries is not null) _entries.AddRange(entries);
        _active = active;
    }

    public static FfiAccountEntry Entry(
        string actorId, string? handle = null, string? domain = null, string? tier = null,
        bool requireConfirm = false) =>
        new(actorId, handle, domain, tier, requireConfirm);

    /// <summary>The account this window serves (the process holder's actor) — set on a bound
    /// secondary, where it differs from <see cref="Active"/>. Null = nothing admitted yet.</summary>
    public string? Served { get; set; }

    public override string? Active() => _active;

    /// <summary>The shared <c>session_account</c> rule: the served account, else — before
    /// one is admitted — the registry's active one.</summary>
    public override string? SessionAccount() => Served ?? _active;

    public override FfiAccountEntry[] List() => _entries.ToArray();

    public override void Remove(string actorId)
    {
        Removed.Add(actorId);
        if (ThrowOnRemove)
        {
            throw new InvalidOperationException("remove rejected");
        }
        _entries.RemoveAll(e => e.actorId == actorId);
        if (_active == actorId)
        {
            _active = _entries.Count > 0 ? _entries[0].actorId : null;
        }
    }

    public override void SetActive(string actorId)
    {
        Activated.Add(actorId);
        _active = actorId;
    }

    /// <summary>The Stage-2 post-re-auth activation path. Also app-owned — recorded here so
    /// the "VM does not activate" assertion covers both doors.</summary>
    public override void SetActiveConfirmed(string actorId)
    {
        Activated.Add(actorId);
        _active = actorId;
    }

    public override void SetRequireConfirm(string actorId, bool require)
    {
        for (var i = 0; i < _entries.Count; i++)
        {
            if (_entries[i].actorId == actorId)
            {
                _entries[i] = _entries[i] with { requireConfirmToActivate = require };
            }
        }
    }

    /// <summary>
    /// The pending-provision slot's writer (`onboarding.md` § 6). Overridden — unlike
    /// the rest of the onboarding surface, which throws on purpose — because
    /// <see cref="OnboardingViewModel"/> now asks for it while CONSTRUCTING the
    /// machine, so the throwing default would take out every test that merely builds
    /// the view model, rather than the one that exercises provisioning. Returns an
    /// inert accepting store: no windows test provisions a box, and a fake that
    /// refused (returned null) would make the machine correctly refuse to build one,
    /// which is not what any of these tests are asserting.
    /// </summary>
    public override PendingProvisionStore PendingProvisionStore() => new InertPendingProvisionStore(this);

    /// <summary>Secrets the machine's store was told to clear the awaiting-DNS slot
    /// of, in order — what the Almost-ready exit's shared door writes.</summary>
    public List<string> ClearedAwaitingDns { get; } = new();

    /// <summary>Successor actor ids whose key this fake "device" holds — what
    /// <see cref="AdoptHeldSuccessor"/> answers from.</summary>
    public HashSet<string> HeldSuccessors { get; } = new();

    /// <summary>(predecessor, verified successor) pairs <see cref="AdoptHeldSuccessor"/>
    /// accepted, in order.</summary>
    public List<(string Predecessor, string Successor)> Adoptions { get; } = new();

    /// <summary>The shared relaunch-adoption decision (<c>AccountRegistry::adopt_held_successor</c>):
    /// true — and the link recorded — only for a successor this device holds. Overridden
    /// because <see cref="OnboardingViewModel"/>'s superseded launch route asks it on every
    /// verified walk.</summary>
    public override bool AdoptHeldSuccessor(string predecessor, string verifiedSuccessor)
    {
        if (!HeldSuccessors.Contains(verifiedSuccessor)) return false;
        Adoptions.Add((predecessor, verifiedSuccessor));
        return true;
    }

    /// <summary>Accepts the write and echoes the record back, which is the
    /// store contract's "read it back" success answer.</summary>
    private sealed class InertPendingProvisionStore(FakeAccountRegistry owner) : PendingProvisionStore
    {
        public AwaitingDnsRecord? SaveAwaitingDns(string secretHex, AwaitingDnsRecord record) => record;
        // The Almost-ready exit's shared door (`abandon_awaiting_manual_dns`) clears
        // the slot through the store too; recorded so the exit's test can see it.
        public void ClearAwaitingDns(string secretHex) => owner.ClearedAwaitingDns.Add(secretHex);
    }

    // Every other IFfiAccountRegistry member (boot/onboarding/bind/migration surface —
    // AddAccount, AutoEnableRequireConfirm, the bind-without-activate trio, ClearAll,
    // ClearNestBinding, ConfirmIdentity, LaunchPersistence, PersistLoggedIn,
    // SetNestUrl, SessionMaterial, UpdateCache, the wizard-exit slot trio) inherits
    // FfiAccountRegistryFakeBase's throwing default — see the class doc-comment above
    // for why each category is app/onboarding-owned rather than switcher surface.
}
