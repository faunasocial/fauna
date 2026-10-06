using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows offline-affordance gate's mechanism — W4 (account-data-plane.md § Workstreams) phase 4
/// (<c>docs/goal/architecture/account-data-plane.md</c> § The offline-mutation
/// contract → <i>How a surface asks</i>).
///
/// <para>These are the twins of linux's 9 in-crate tests. They exist in
/// <c>FaunaApp.Tests</c> rather than beside a page because the registry's whole
/// decision lives in <c>FaunaApp.Core</c> over <see cref="IGatedControl"/> — this
/// project targets plain <c>net10.0</c> and cannot instantiate a WinUI
/// <c>Control</c>, so a gate written against <c>Control</c> directly would be
/// provable only through e2e.</para>
///
/// <para>The verdict is the REAL shared rule via
/// <c>FaunaFfiMethods.OfflineAffordance</c> (so the native <c>fauna_ffi</c> dll
/// loads here, memory <c>reference_windows_dotnet_test_loads_native_ffi</c>) and
/// the kinds are read out of the shared table rather than hard-coded, so a later
/// reclassification cannot turn these red for the wrong reason.</para>
/// </summary>
public class OfflineGateTests
{
    /// <summary>An online-only kind — the class that desensitizes (ruling 1).</summary>
    private const string OnlineOnlyKind = "fauna.pair.add";

    /// <summary>An offline-safe kind — must never be greyed.</summary>
    private const string OfflineSafeKind = "fauna.account.state.put";

    private const string Connected = "connected";
    private const string Disconnected = "disconnected";

    /// <summary>
    /// A fake control, standing in for a WinUI one.
    ///
    /// <para><paramref name="echoSynchronously"/> is the axis trap 3 turns on:
    /// <c>true</c> raises <c>EnabledChanged</c> from inside the setter (WinUI's
    /// real shape — a dependency-property changed callback), <c>false</c> defers
    /// it until <see cref="FlushEcho"/> (GTK's shape, where the measured 2026-08-14
    /// failure lived). The gate must behave identically under both, which is
    /// exactly why it matches on the VALUE rather than holding a flag across the
    /// write.</para>
    /// </summary>
    private sealed class FakeControl : IGatedControl
    {
        private readonly bool _echoSynchronously;
        private bool _isEnabled;
        private bool? _pendingEcho;

        /// <summary>
        /// The stand-in for the underlying WinUI control. Two wrappers sharing one
        /// of these are two adapters over the SAME control — the shape every real
        /// re-declaration takes, since `FaunaGate()` mints a fresh wrapper per call.
        /// </summary>
        private sealed class Underlying { }

        private readonly Underlying _underlying;

        internal FakeControl(bool isEnabled = true, bool echoSynchronously = true, FakeControl? sameControlAs = null)
        {
            _isEnabled = isEnabled;
            _echoSynchronously = echoSynchronously;
            _underlying = sameControlAs?._underlying ?? new Underlying();
        }

        public object Identity => _underlying;

        internal int DetachCount { get; private set; }

        public void Detach() => DetachCount++;

        public bool IsEnabled
        {
            get => _isEnabled;
            set
            {
                if (_isEnabled == value) return;
                _isEnabled = value;
                if (_echoSynchronously) EnabledChanged?.Invoke(value);
                else _pendingEcho = value;
            }
        }

        public string? GateReason { get; set; }

        public bool IsAlive { get; internal set; } = true;

        public event Action<bool>? EnabledChanged;

        /// <summary>Deliver a deferred notification, the way GTK does after the write returns.</summary>
        internal void FlushEcho()
        {
            if (_pendingEcho is not { } value) return;
            _pendingEcho = null;
            EnabledChanged?.Invoke(value);
        }

        /// <summary>The PAGE changing its mind — always notifies, like a real property write.</summary>
        internal void PageSets(bool value)
        {
            if (_isEnabled == value) return;
            _isEnabled = value;
            EnabledChanged?.Invoke(value);
            _pendingEcho = null;
        }
    }

    private static OfflineGate GateAt(string state)
    {
        var gate = new OfflineGate();
        gate.ResetForTest(state);
        return gate;
    }

    /// <summary>
    /// Guards the whole file against a silently-reclassified fixture: if
    /// <c>fauna.pair.add</c> stopped being online-only, every "desensitizes" case
    /// below would pass vacuously.
    /// </summary>
    [Fact]
    public void TheFixtureKindsStillCarryTheClassesTheseTestsAssume()
    {
        Assert.False(
            FaunaFfiMethods.OfflineAffordance(OnlineOnlyKind, Disconnected).available,
            $"{OnlineOnlyKind} is no longer OnlineOnly — every desensitize case here is now vacuous");
        Assert.True(
            FaunaFfiMethods.OfflineAffordance(OfflineSafeKind, Disconnected).available,
            $"{OfflineSafeKind} is no longer offline-capable — the stays-live cases are now vacuous");
    }

    [Fact]
    public void AnOnlineOnlyControlDesensitizesWithNoNestAndSaysWhy()
    {
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);
        Assert.True(control.IsEnabled);
        Assert.Null(control.GateReason);

        gate.SetConnectionState(Disconnected);
        Assert.False(control.IsEnabled);
        Assert.False(string.IsNullOrEmpty(control.GateReason));
    }

    [Fact]
    public void AnOfflineCapableControlStaysLiveWithNoNest()
    {
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OfflineSafeKind);
        gate.SetConnectionState(Disconnected);

        Assert.True(control.IsEnabled);
        Assert.Null(control.GateReason);
    }

    [Fact]
    public void DeclaringWhileAlreadyOfflineGatesImmediately()
    {
        // The gate starts "disconnected" — a control built during an outage must
        // be gated at construction, not at the next state change.
        var gate = GateAt(Disconnected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);

        Assert.False(control.IsEnabled);
        Assert.False(string.IsNullOrEmpty(control.GateReason));
    }

    [Fact]
    public void AReconnectRestoresThePagesIntentAndWithdrawsOnlyOurCaption()
    {
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);
        gate.SetConnectionState(Disconnected);
        Assert.False(control.IsEnabled);

        gate.SetConnectionState(Connected);
        Assert.True(control.IsEnabled);
        Assert.Null(control.GateReason);
    }

    [Fact]
    public void AControlThePageDisabledIsNeverEnabledByAReconnect()
    {
        // Trap 2: effective enablement is the page's intent AND the verdict. A
        // reconnect restores exactly the page's intent, never more.
        var gate = GateAt(Connected);
        var control = new FakeControl(isEnabled: false);

        gate.Declare(control, OnlineOnlyKind);
        gate.SetConnectionState(Disconnected);
        gate.SetConnectionState(Connected);

        Assert.False(control.IsEnabled);
    }

    [Fact]
    public void ThePagesOwnReasonSurvivesTheGate()
    {
        // A control the PAGE disabled keeps the page's reason — it is more
        // specific than "no nest" — and the gate never overwrites it.
        var gate = GateAt(Connected);
        var control = new FakeControl(isEnabled: false) { GateReason = "the page's own reason" };

        gate.Declare(control, OnlineOnlyKind);
        gate.SetConnectionState(Disconnected);

        Assert.Equal("the page's own reason", control.GateReason);

        gate.SetConnectionState(Connected);
        Assert.Equal("the page's own reason", control.GateReason);
    }

    /// <summary>
    /// Trap 3, under BOTH echo orderings.
    ///
    /// <para>This is the case that killed the linux control forever: the gate's own
    /// write comes back as a notification, and if it is recorded as the page's
    /// intent then the reconnect "restores" the gate's own <c>NeedsNest</c>
    /// verdict. WinUI echoes synchronously and GTK echoes after the write returns
    /// — the value-match rule must survive both, so both are parameterized here
    /// rather than one being assumed.</para>
    /// </summary>
    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void TheGatesOwnWriteIsNotMistakenForThePagesIntent(bool echoSynchronously)
    {
        var gate = GateAt(Connected);
        var control = new FakeControl(echoSynchronously: echoSynchronously);

        gate.Declare(control, OnlineOnlyKind);
        gate.SetConnectionState(Disconnected);
        control.FlushEcho(); // no-op when the echo already arrived synchronously
        Assert.False(control.IsEnabled);

        gate.SetConnectionState(Connected);
        control.FlushEcho();

        Assert.True(control.IsEnabled,
            "the gate recorded its own write as the page's intent — the control is " +
            "now dead forever, which is the measured linux failure this rule exists for");
    }

    [Fact]
    public void APageEnablingAControlWhileOfflineIsRegatedAtOnce()
    {
        var gate = GateAt(Disconnected);
        var control = new FakeControl(isEnabled: false);

        gate.Declare(control, OnlineOnlyKind);
        Assert.False(control.IsEnabled);

        // The page changes its mind mid-outage. The new intent is recorded, but
        // the verdict still applies — enabling must not escape the gate.
        control.PageSets(true);
        Assert.False(control.IsEnabled);

        // ...and the recorded intent is the page's `true`, so a reconnect releases it.
        gate.SetConnectionState(Connected);
        Assert.True(control.IsEnabled);
    }

    [Fact]
    public void RedeclaringInheritsThePageIntentAndSilencesTheOldDeclaration()
    {
        // Trap 4: reading the control fresh would capture the OLD gate's verdict
        // as the page's intent, permanently.
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);
        gate.SetConnectionState(Disconnected);
        Assert.False(control.IsEnabled);

        // Re-declared while gated — the control currently reads `false`, but that
        // is the retired declaration's verdict, not the page's intent.
        gate.Declare(control, OnlineOnlyKind);
        Assert.Single(gate.Declarations);

        gate.SetConnectionState(Connected);
        Assert.True(control.IsEnabled,
            "the re-declaration read the gate's own verdict as the page's intent");
    }

    /// <summary>
    /// Trap 4 across the ADAPTER boundary — the case the same-instance tests above
    /// cannot see, and the one the windows leg actually got wrong first.
    ///
    /// <para>`Control.FaunaGate(kind)` wraps its control in a fresh adapter on every
    /// call, so a real re-declaration hands the registry two DIFFERENT
    /// <see cref="IGatedControl"/> instances over one control. A registry matching
    /// wrappers by reference sees no re-declaration at all: it keeps both entries,
    /// so the retired kind goes on re-deciding the control and the change
    /// subscription is leaked once per declaration. Matching on
    /// <see cref="IGatedControl.Identity"/> is what makes the wrapper layer
    /// invisible to trap 4.</para>
    /// </summary>
    [Fact]
    public void RedeclaringThroughAFreshWrapperStillSupersedesTheOldDeclaration()
    {
        var gate = GateAt(Connected);
        var first = new FakeControl();

        gate.Declare(first, OnlineOnlyKind);
        // A second adapter over the SAME underlying control, as FaunaGate() makes.
        var second = new FakeControl(sameControlAs: first);
        gate.Declare(second, OfflineSafeKind);

        Assert.Single(gate.Declarations);
        Assert.Equal(1, first.DetachCount);

        gate.SetConnectionState(Disconnected);
        Assert.True(second.IsEnabled,
            "the retired OnlineOnly declaration was still re-deciding the control — " +
            "the registry matched wrappers by reference instead of by identity");
    }

    [Fact]
    public void RedeclaringWithANewKindRetiresTheOldVerdict()
    {
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);
        gate.Declare(control, OfflineSafeKind);

        gate.SetConnectionState(Disconnected);

        Assert.True(control.IsEnabled,
            "the retired OnlineOnly declaration was still re-deciding the control");
        Assert.Single(gate.Declarations);
    }

    [Fact]
    public void ADeadControlIsPrunedRatherThanDecided()
    {
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);
        Assert.Single(gate.Declarations);

        control.IsAlive = false;
        gate.SetConnectionState(Disconnected);

        Assert.Empty(gate.Declarations);
        Assert.True(control.IsEnabled, "a pruned declaration must not still be written to");
    }

    [Fact]
    public void AnUnregisteredKindStaysAvailable()
    {
        // Ruling 2: a typo must surface as a failing check, never as a dead
        // button in a user's hands.
        var gate = GateAt(Disconnected);
        var control = new FakeControl();

        gate.Declare(control, "fauna.pair.addd");

        Assert.True(control.IsEnabled);
        Assert.Null(control.GateReason);
    }

    [Fact]
    public void AnUnknownStateWordKeepsControlsLive()
    {
        // Ruling 3: only the KNOWN offline words count as offline, so an older
        // app meeting a future state word keeps its controls live.
        var gate = GateAt(Connected);
        var control = new FakeControl();

        gate.Declare(control, OnlineOnlyKind);
        gate.SetConnectionState("some-future-state-word");

        Assert.True(control.IsEnabled);
    }

    [Fact]
    public void TheStateWordComesFromTheSharedExport()
    {
        // Correction 2: never a C# switch over FfiConnectionState. This pins that
        // the word the gate is driven with is the shared one, and that the gate
        // actually reacts to it.
        var gate = GateAt(Connected);
        var control = new FakeControl();
        gate.Declare(control, OnlineOnlyKind);

        var word = FaunaFfiMethods.ConnectionStateWord(FfiConnectionState.Disconnected);
        gate.SetConnectionState(word);

        Assert.Equal(word, gate.ConnectionState);
        Assert.False(control.IsEnabled,
            $"the shared disconnected word ({word}) did not gate an OnlineOnly control");
    }
}
