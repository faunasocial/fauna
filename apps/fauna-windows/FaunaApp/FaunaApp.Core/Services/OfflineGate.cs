using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// One gateable control, as the offline gate needs to see it — the whole seam
/// between this registry and WinUI.
///
/// <para>It exists because <c>FaunaApp.Tests</c> targets plain <c>net10.0</c> and
/// references only <c>FaunaApp.Core</c>: it cannot instantiate a
/// <c>FrameworkElement</c>, so a registry written against <c>Control</c> directly
/// would have no unit tests at all and the four traps below would be provable
/// only through e2e. Everything toolkit-specific (finding a parent
/// <c>Panel</c>, inserting the caption <c>TextBlock</c>, mapping
/// <c>IsEnabledChanged</c>) lives in the WinUI adapter behind this interface;
/// everything *decided* lives here.</para>
/// </summary>
internal interface IGatedControl
{
    /// <summary>
    /// The control's OWN enablement — never an ancestor-folded value.
    ///
    /// <para>⚠ Trap 1 (<c>account-data-plane.md</c> § Built — the linux leg): a
    /// control inside a momentarily-disabled container has not had its own
    /// intent changed, and recording that as the page's intent leaves it dead
    /// after a reconnect. The adapter is responsible for answering with the
    /// control's own property, and <see cref="EnabledChanged"/> must report on
    /// that same property.</para>
    /// </summary>
    bool IsEnabled { get; set; }

    /// <summary>
    /// The gate's inline reason caption — <c>null</c> for none.
    ///
    /// <para>Owned entirely by this gate: the page never writes it, which is why
    /// there is no "only clear what we wrote" question on the text itself (the
    /// <c>ReasonIsOurs</c> bookkeeping below still tracks whether the gate is
    /// currently the one showing a caption, so a reconnect withdraws exactly
    /// what the gate put up).</para>
    /// </summary>
    string? GateReason { get; set; }

    /// <summary>
    /// Whether the underlying control is still alive. A navigated-away page's
    /// controls are pruned on the next pass, so a page rebuild needs no
    /// teardown — the same posture as linux's <c>WeakRef</c> entries.
    /// </summary>
    bool IsAlive { get; }

    /// <summary>
    /// Raised when <see cref="IsEnabled"/> changes — by the page <b>or</b> by
    /// this gate. Separating the two is <see cref="OfflineGate"/>'s job, and it
    /// is the whole subject of trap 3.
    /// </summary>
    event Action<bool>? EnabledChanged;

    /// <summary>
    /// The underlying control, as an identity to compare declarations by.
    ///
    /// <para>⚠ This exists because trap 4 is otherwise defeated at the adapter
    /// boundary, silently. Each <c>FaunaGate()</c> call wraps its control in a
    /// FRESH adapter, so a registry comparing the <i>wrappers</i> by reference
    /// would never recognise a re-declaration of the same control: it would keep
    /// both declarations, leave the retired kind re-deciding the control, and
    /// leak a change subscription per declaration — exactly the four failures
    /// <c>Superseded</c> exists to prevent, reintroduced one layer down. Compare
    /// this instead, which is stable across wrappers.</para>
    /// </summary>
    object Identity { get; }

    /// <summary>
    /// Stop listening to the control. Called on the declaration this gate
    /// retires, so a re-declared control ends up with exactly one live
    /// subscription rather than one per declaration.
    /// </summary>
    void Detach();
}

/// <summary>
/// The offline-affordance gate — W4 (account-data-plane.md § Workstreams) phase 4 on windows.
///
/// <para>The charter's class-3 sentence ("UI desensitizes these offline",
/// <c>docs/goal/architecture/account-data-plane.md</c> § The offline-mutation
/// contract) as one seam. The <b>decision</b> is not ours: it is the shared
/// <c>fauna_protocol::offline_class::affordance</c>, reached here through the
/// UniFFI export <c>FaunaFfiMethods.OfflineAffordance</c>, so windows keeps no
/// per-app list of widgets-to-grey and no per-app copy of the three rulings
/// (priority #2). What is windows' own is only <b>how a persistent widget tree
/// obeys it</b>.</para>
///
/// <para>WinUI is the same SHAPE as GTK — controls outlive the state that gated
/// them — so this is a port of <c>apps/fauna-linux/src/offline_gate.rs</c>, not a
/// design: a <b>registry</b> rather than a pass. A page declares what a control
/// issues once at construction (<see cref="Declare"/>), <see
/// cref="SetConnectionState"/> re-decides every live declaration from the ONE
/// place the app learns the state word (<c>MainViewModel.OnConnectionStateChanged</c>,
/// so the <c>connection-status</c> indicator and this gate cannot disagree), and
/// each declaration watches its own control's enablement so a page enabling a
/// control while offline is re-gated at once rather than at a repaint that may
/// never come.</para>
///
/// <para><b>What it never does:</b> enable what the page disabled. The page's own
/// reason is stronger and more specific than "no nest", so effective enablement
/// is <i>the page's own intent AND the verdict</i>, and a reconnect restores
/// exactly the page's intent, never more.</para>
/// </summary>
internal sealed class OfflineGate
{
    /// <summary>
    /// The app-wide gate. One per process, like the single connection-state seam
    /// that drives it.
    /// </summary>
    internal static OfflineGate Shared { get; } = new();

    private sealed class Declaration
    {
        internal required IGatedControl Control { get; init; }

        /// <summary>The wire kind this control issues — the key the shared rule reads.</summary>
        internal required string Kind { get; init; }

        /// <summary>
        /// The enablement the <b>page</b> asked for, tracked separately from the
        /// control's live value so a reconnect restores the page's intent rather
        /// than blanket-enabling (trap 2).
        /// </summary>
        internal bool PageEnabled;

        /// <summary>True while the caption currently on the control is this gate's.</summary>
        internal bool ReasonIsOurs;

        /// <summary>
        /// The enablement this gate last WROTE and has not yet seen echoed back.
        ///
        /// <para>⚠ Trap 3, and it is silent when got wrong. Watching the property
        /// means the gate hears its OWN writes, and the obvious separator — a
        /// flag held across the write — does not work on a toolkit that delivers
        /// the notification after the write returns (measured on GTK 2026-08-14:
        /// the flag is already cleared, so the handler records the gate's own
        /// <c>NeedsNest</c> verdict as the page's intent and the control is dead
        /// forever). WinUI raises <c>IsEnabledChanged</c> synchronously from
        /// inside the property set, so a flag would happen to survive here — but
        /// a time-scoped guard cannot distinguish the echo in general and only
        /// the VALUE can, so windows uses the same value-match rule linux does
        /// and <c>OfflineGateTests</c> proves it under BOTH orderings rather
        /// than pinning today's toolkit behaviour.</para>
        ///
        /// <para>One write, one echo: the first notification carrying exactly
        /// what we wrote is consumed as ours; anything else is the page changing
        /// its mind.</para>
        /// </summary>
        internal bool? GateWrote;

        /// <summary>
        /// Set when a later <see cref="Declare"/> on the same control replaces
        /// this declaration (trap 4). Dropping it from the registry is not
        /// enough — its handler is still subscribed, so without this flag the
        /// RETIRED kind would keep re-deciding the control.
        /// </summary>
        internal bool Superseded;

        internal Action<bool> Handler = null!;
    }

    private readonly List<Declaration> _declared = new();

    /// <summary>
    /// The lowercase transport word, the same one
    /// <c>FaunaFfiMethods.ConnectionStateWord</c> produces.
    ///
    /// <para>Starts <c>"disconnected"</c> because that is what the app itself
    /// starts as, so the indicator and the gate agree from the first frame rather
    /// than from the first WS event.</para>
    /// </summary>
    private string _state = "disconnected";

    /// <summary>
    /// Declare that <paramref name="control"/> actuates <paramref name="kind"/>,
    /// and gate it from this moment on.
    ///
    /// <para><paramref name="kind"/> is the wire kind the gesture issues — the
    /// same string the nest's <c>KindRegistry</c> and <c>offline_class</c> table
    /// key on. A control that issues nothing over the wire (a local navigation,
    /// dismissing a modal) simply does not call this; an unregistered kind stays
    /// available by the shared rule's own ruling 2, so a typo shows up as a
    /// failing <c>check-offline-gate-kinds.py</c> run, never as a dead button.</para>
    ///
    /// <para>Where one control paints several ceremonies whose kinds differ,
    /// declare it again after the paint decides — the later declaration wins,
    /// and inherits the recorded page intent rather than reading the control
    /// (whose current value may be the retired declaration's verdict).</para>
    /// </summary>
    internal void Declare(IGatedControl control, string kind)
    {
        Prune();

        // Re-declaring the same control replaces its kind rather than stacking a
        // second verdict on it. Retiring the old entry also has to silence it and
        // hand over what it knows (trap 4).
        bool? inheritedPageEnabled = null;
        var inheritedReasonIsOurs = false;
        for (var i = _declared.Count - 1; i >= 0; i--)
        {
            var old = _declared[i];
            // By IDENTITY, never by wrapper reference — see IGatedControl.Identity.
            if (!ReferenceEquals(old.Control.Identity, control.Identity)) continue;
            old.Superseded = true;
            old.Control.EnabledChanged -= old.Handler;
            old.Control.Detach();
            inheritedPageEnabled = old.PageEnabled;
            inheritedReasonIsOurs = old.ReasonIsOurs;
            _declared.RemoveAt(i);
        }

        var declaration = new Declaration
        {
            Control = control,
            Kind = kind,
            PageEnabled = inheritedPageEnabled ?? control.IsEnabled,
            ReasonIsOurs = inheritedReasonIsOurs,
        };
        declaration.Handler = now =>
        {
            if (declaration.Superseded) return;
            // Our own write, coming back. One write, one echo — see GateWrote for
            // why this cannot be a flag held across the write.
            if (declaration.GateWrote == now)
            {
                declaration.GateWrote = null;
                return;
            }
            // The page changed its mind. Record the new intent, then re-decide:
            // enabling a control while offline must not escape the gate.
            declaration.PageEnabled = now;
            Apply(declaration);
        };
        control.EnabledChanged += declaration.Handler;
        _declared.Add(declaration);
        Apply(declaration);
    }

    /// <summary>
    /// The link's state changed — re-decide every live declaration.
    ///
    /// <para>Called from <c>MainViewModel.OnConnectionStateChanged</c>, the one
    /// place windows turns an <c>FfiConnectionState</c> into the lowercase state
    /// word, so the indicator a user reads and the gate that greys their controls
    /// are driven by the same value.</para>
    /// </summary>
    internal void SetConnectionState(string state)
    {
        if (_state == state) return;
        _state = state;
        Prune();
        foreach (var declaration in _declared.ToArray()) Apply(declaration);
    }

    /// <summary>The state word the gate is currently deciding against. Tests only.</summary>
    internal string ConnectionState => _state;

    /// <summary>Every live declaration as (kind, enabled). Tests only.</summary>
    internal IReadOnlyList<(string Kind, bool Enabled)> Declarations
    {
        get
        {
            Prune();
            return _declared.Select(d => (d.Kind, d.Control.IsEnabled)).ToList();
        }
    }

    /// <summary>
    /// Forget every declaration and start from <paramref name="state"/>. Tests
    /// only — without it a test would inherit whatever its predecessor left on
    /// <see cref="Shared"/>.
    /// </summary>
    internal void ResetForTest(string state)
    {
        foreach (var declaration in _declared)
        {
            declaration.Superseded = true;
            declaration.Control.EnabledChanged -= declaration.Handler;
            declaration.Control.Detach();
        }
        _declared.Clear();
        _state = state;
    }

    private void Prune()
    {
        for (var i = _declared.Count - 1; i >= 0; i--)
        {
            if (_declared[i].Control.IsAlive) continue;
            _declared[i].Superseded = true;
            _declared.RemoveAt(i);
        }
    }

    private void Apply(Declaration declaration)
    {
        if (declaration.Superseded || !declaration.Control.IsAlive) return;

        // The shared rule, never a C# switch over the class or the state word.
        var verdict = FaunaFfiMethods.OfflineAffordance(declaration.Kind, _state);
        var allowed = declaration.PageEnabled && verdict.available;

        if (declaration.Control.IsEnabled != allowed)
        {
            // Set BEFORE the write, so a toolkit that echoes synchronously from
            // inside the setter finds the value to match against.
            declaration.GateWrote = allowed;
            declaration.Control.IsEnabled = allowed;
        }

        // The reason belongs beside the affordance the gate itself withheld. A
        // control the PAGE disabled keeps the page's own reason, which is more
        // specific than "no nest".
        if (!verdict.available && declaration.PageEnabled)
        {
            if (declaration.Control.GateReason is null && verdict.reason is not null)
            {
                declaration.Control.GateReason = Strings.Resolve(verdict.reason);
                declaration.ReasonIsOurs = true;
            }
        }
        else if (declaration.ReasonIsOurs)
        {
            declaration.ReasonIsOurs = false;
            declaration.Control.GateReason = null;
        }
    }
}
