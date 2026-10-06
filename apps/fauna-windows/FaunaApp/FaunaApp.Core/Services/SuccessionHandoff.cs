using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// What an identity succession hands <b>across its own account switch</b> — the
/// ceremony's closing act (<c>docs/goal/behavior/identity-succession.md</c>
/// § The RecoveryKey → <i>At succession</i>). The windows twin of apple's
/// <c>FaunaKit/Core/SuccessionHandoff.swift</c> and of tui's
/// <c>succession_{sweep,kit_owed,predecessor,succeeded_at}</c> fields.
///
/// <para><b>Why this is not torn down with the rest of the session.</b> Every
/// other piece of actor-scoped state is dropped at a switch precisely so one
/// identity's state cannot paint under the next one's. These are the
/// <b>inverse</b>: they belong to the <i>outgoing</i> identity's ceremony, and the
/// teardown they have to survive <b>is that ceremony's own closing act</b>. The
/// sweep is its result (rendered after the switch); the owed kit is its last step,
/// which only the successor's session can perform — the mint authenticates as an
/// identity that does not exist as a session until the teardown completes. <b>Do
/// not "fix" this by clearing it from <c>SwitchAccountHandler</c>.</b></para>
///
/// <para><b>What DOES clear it:</b> the credential-namespace wipe behind sign-out
/// and factory reset (<see cref="ClearOnCredentialWipe"/>). That destroys every
/// identity on the box, so there is no successor left to owe a kit to and no
/// predecessor row left for that kit to seal.</para>
///
/// <para>Static rather than a field on any view model deliberately: the whole
/// contract is that this outlives the objects a switch rebuilds.</para>
/// </summary>
internal static class SuccessionHandoff
{
    private static readonly object Gate = new();

    private static bool _kitOwed;
    private static string? _predecessorActorIdHex;
    private static string? _successorActorIdHex;
    private static long? _succeededAtUnix;
    private static string? _sweepStateJson;
    private static FfiSweepView? _sweep;

    private static string? _sweepOwedTo;

    /// <summary>
    /// The successor a <b>relaunch adoption</b> owes the group sweep to — null when
    /// nothing is owed (<c>succession-propagation.md</c> § Propagation → <i>Own
    /// device fleet</i>, the relaunch-adoption clause). The ceremony runs its own
    /// sweep before its switch; an adoption cannot (a refused launch never opens the
    /// retired identity's engine), so it owes one, discharged by the successor's
    /// first authenticated session as an unbidden press of the retry —
    /// <c>RecoveryKitViewModel.DischargeOwedSweepAsync</c>.
    ///
    /// <para>⚠ <b>Its own binding, deliberately not <see cref="SuccessorActorIdHex"/></b>:
    /// <see cref="ClaimOwedKit"/> nils that one when the kit is discharged, and a
    /// sweep deferred past the kit's claim (a busy view model) would then be owed to
    /// nobody — the two obligations (kit, sweep) never consume each other's state.
    /// apple's <c>sweepOwedTo</c> is the twin.</para>
    /// </summary>
    internal static string? SweepOwedTo { get { lock (Gate) return _sweepOwedTo; } }

    /// <summary>
    /// The successor owes itself a fresh kit. Set the instant a succession lands,
    /// <b>including on the persist-failure path</b>: the account moved either way,
    /// so it is kitless and escrowless until this is discharged. The old kit
    /// retired with the old identity and the nest deleted its escrow row in the
    /// same transaction, so until the mint there is no route back into the account
    /// but the 30-day seed-alone window.
    /// </summary>
    internal static bool KitOwed { get { lock (Gate) return _kitOwed; } }

    /// <summary>
    /// The identity the account just moved <i>away</i> from, 64-hex. Read before
    /// the switch: afterwards the session holds the successor and nothing else
    /// names that row.
    /// </summary>
    internal static string? PredecessorActorIdHex
    {
        get { lock (Gate) return _predecessorActorIdHex; }
    }

    /// <summary>
    /// The identity that owes itself the kit — i.e. who
    /// <see cref="ClaimOwedKit"/> will hand it to, and nobody else.
    ///
    /// <para>⚠ Not bookkeeping — the guard that makes the obligation survivable.
    /// The ceremony runs from a <i>mounted</i> Recovery Kit section, and that page
    /// outlives the ceremony by the width of the teardown: the switch tears the
    /// clients down, the still-live page re-hydrates, and an unbound claim would be
    /// taken by the OUTGOING session — which then mints against a nest that has
    /// just revoked its bearers, fails, and leaves the flag spent.</para>
    /// </summary>
    internal static string? SuccessorActorIdHex
    {
        get { lock (Gate) return _successorActorIdHex; }
    }

    /// <summary>Unix seconds the nest applied the succession, when the submit
    /// reply carried it. Null on the reconcile arm is real and honest, never a
    /// placeholder to be filled in later.</summary>
    internal static long? SucceededAtUnix { get { lock (Gate) return _succeededAtUnix; } }

    /// <summary>
    /// The pre-switch group sweep's own account of what it managed, in the e2e
    /// state provider's vocabulary — <c>SweepStatus::state_json</c>, republished
    /// verbatim by <c>App.SerializeState</c> as <c>data.succession_sweep</c> rather
    /// than re-encoded here (the shape is the cross-app contract, so it lives on
    /// the shared status; e2e convention 11).
    ///
    /// <para>⚠ Not a painting surface: its vocabulary (<c>no_engine</c>)
    /// deliberately differs from <c>FfiSweepView.Kind</c>'s (<c>no-engine</c>),
    /// which is the human one.</para>
    /// </summary>
    internal static string? SweepStateJson { get { lock (Gate) return _sweepStateJson; } }

    /// <summary>
    /// The pre-switch group sweep's own account of what it managed, as a surface
    /// paints it (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The sweep's
    /// own lines</i>) — carried beside <see cref="SweepStateJson"/> rather than
    /// re-derived from it, since the two vocabularies deliberately differ (that
    /// field's own doc). Hand this to <c>FaunaFfiMethods.SweepCopy</c> at paint
    /// time; never match on <c>Kind</c> here.
    /// </summary>
    internal static FfiSweepView? Sweep { get { lock (Gate) return _sweep; } }

    /// <summary>
    /// Record a landed succession, <b>before</b> the account switch that follows
    /// it.
    ///
    /// <para>Called on both arms of <c>Persisted</c> for the reason
    /// <see cref="KitOwed"/> documents: the succession landed either way. The
    /// caller switches accounts immediately afterwards on the persisted arm —
    /// everything here is declared to survive that.</para>
    /// </summary>
    internal static void Record(FfiLandedSuccession landed, string? predecessorActorIdHex)
    {
        lock (Gate)
        {
            _kitOwed = true;
            _predecessorActorIdHex = predecessorActorIdHex;
            _successorActorIdHex = landed.newActorIdHex;
            _succeededAtUnix = landed.succeededAt;
            _sweepStateJson = landed.sweepStateJson;
            _sweep = landed.sweep;
        }
    }

    /// <summary>
    /// Record a <b>relaunch adoption</b> — a launch refused as superseded whose
    /// chain-verified successor this device held, adopted by
    /// <c>OnboardingViewModel.RouteSupersededRefusalAsync</c> — <b>before</b> the
    /// account switch that follows.
    ///
    /// <para>The ceremony's closing obligations, minus what only the ceremony had:
    /// the kit is owed (the old one retired with the old identity) and so is the
    /// group sweep, which the ceremony would have run itself. No sweep report is
    /// carried — none ran — and no stamp: the reply that carried it was the one
    /// lost. No aftermath context either: its roster is the sweep's, and none ran.
    /// tui's <c>App::adopt_held_successor</c> and apple's
    /// <c>recordRelaunchAdoption</c> are the twins.</para>
    /// </summary>
    internal static void RecordRelaunchAdoption(string predecessorActorIdHex, string successorActorIdHex)
    {
        lock (Gate)
        {
            _kitOwed = true;
            _predecessorActorIdHex = predecessorActorIdHex;
            _successorActorIdHex = successorActorIdHex;
            _succeededAtUnix = null;
            _sweepOwedTo = successorActorIdHex;
        }
    }

    /// <summary>
    /// Claim the owed sweep for the session that really is the successor — once.
    /// The actor bind and the single claim guard what <see cref="ClaimOwedKit"/>'s
    /// do, for the same reasons.
    /// </summary>
    internal static bool ClaimOwedSweep(string? asSuccessorActorIdHex)
    {
        if (string.IsNullOrEmpty(asSuccessorActorIdHex)) return false;
        lock (Gate)
        {
            if (!string.Equals(_sweepOwedTo, asSuccessorActorIdHex, System.StringComparison.Ordinal))
                return false;
            _sweepOwedTo = null;
            return true;
        }
    }

    /// <summary>Put a claimed sweep back when its press could not run at all — bound
    /// to the successor it is given.</summary>
    internal static void RearmOwedSweep(string successorActorIdHex)
    {
        lock (Gate) _sweepOwedTo = successorActorIdHex;
    }

    /// <summary>
    /// Claim the owed kit for the session that is actually the successor — once.
    ///
    /// <para>Two guards, and each prevents its own failure. <b>The actor bind</b>
    /// keeps the departing session from taking an obligation it cannot perform (see
    /// <see cref="SuccessorActorIdHex"/>). <b>The single claim</b> keeps two visits
    /// to the section from minting two kits, the second of which would register a
    /// kit nobody was shown — strictly worse than never-created.</para>
    /// </summary>
    internal static bool ClaimOwedKit(string? asSuccessorActorIdHex)
    {
        if (string.IsNullOrEmpty(asSuccessorActorIdHex)) return false;
        lock (Gate)
        {
            if (!_kitOwed) return false;
            if (!string.Equals(_successorActorIdHex, asSuccessorActorIdHex, System.StringComparison.Ordinal))
                return false;
            _kitOwed = false;
            _predecessorActorIdHex = null;
            _successorActorIdHex = null;
            return true;
        }
    }

    /// <summary>
    /// Replace the carried sweep after <c>recovery-kit-sweep-retry-button</c>
    /// finishes the job — the retry's fresh report supersedes the one carried
    /// across the account switch (<c>settings.md</c> § Recovery kit →
    /// <i>Finishing an unfinished group sweep</i>: re-painting the old view would
    /// show the user the state their press just fixed). Both halves together,
    /// same as <see cref="Record"/> sets them together, so the human-facing view
    /// and the <c>data.succession_sweep</c> state key can never disagree about
    /// which pass is current.
    /// </summary>
    internal static void ReplaceSweep(FfiSweepView fresh, string stateJson)
    {
        lock (Gate)
        {
            _sweep = fresh;
            _sweepStateJson = stateJson;
        }
    }

    /// <summary>The wipe clear point — see the type's doc for why this is the
    /// <i>only</i> one. Called from <c>App.ClearCredentialNamespace</c>, which is
    /// the erase behind both sign-out and factory reset.</summary>
    internal static void ClearOnCredentialWipe()
    {
        lock (Gate)
        {
            _kitOwed = false;
            _predecessorActorIdHex = null;
            _successorActorIdHex = null;
            _succeededAtUnix = null;
            _sweepStateJson = null;
            _sweep = null;
            _sweepOwedTo = null;
        }
    }
}
