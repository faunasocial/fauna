using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_atproto_settings_machine;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The dedicated AT Protocol settings page (<c>docs/goal/ui/atproto.md</c>, ratified
/// 2026-07-22). windows was the LAST app without it — linux/apple landed 2026-07-23,
/// tui 07-24, web 07-29, android 07-30 — so this is a fan-out lift against five
/// reference implementations, not a design.
///
/// <para><b>One page, one question: how deep is this user's Bluesky integration?</b>
/// A single ordered choice of level (Off → Linked → Hosted visible → Hosted full PDS)
/// with each level's sub-settings revealed as its panel. The two identity backings are
/// alternatives, so the selector makes the exclusion structural.</para>
///
/// <para><b>This VM renders a snapshot and forwards gestures — it computes nothing</b>
/// (§ Where logic lives: "Shared Rust: the machine above … all of it. Clients render
/// the snapshot and forward gestures"). Every reveal rule, the gate verdict, the
/// greying reason and the transition-card copy arrive pre-composed on
/// <see cref="AtprotoSettingsSnapshot"/>. Anything recomputed here would be a
/// priority-#2 violation and would drift from the other six apps.</para>
///
/// <para><b>The transition card's lines are rendered VERBATIM</b> from
/// <c>pending_transition.lines</c> — never re-authored client-side. The card promises
/// what the nest will actually do; a card promising something nest did not do is the
/// one failure a confirm-before-anything-happens UX cannot absorb.</para>
///
/// <para><b>Non-optimistic, like every sibling page VM:</b> each gesture awaits the
/// machine and then re-renders from a fresh <c>Snapshot()</c>, so the UI shows what
/// the nest persisted, never the tap. That is also what makes the e2e deterministic —
/// it is why the observer here is a no-op (the established windows pattern; see
/// <c>TaskDelegationViewModel.NoopDevicesObserver</c>), rather than a live callback
/// that would arrive on a Rust thread with no dispatcher available in FaunaApp.Core.</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> anywhere in this class — a WinUI VM that
/// leaves the UI context throws <c>COMException</c> on the next bound-property write
/// (the admin-users hub empty-load bug, 2026-06; memory
/// <c>reference_windows_vm_configureawait_comexception</c>).</para>
/// </summary>
public partial class AtprotoViewModel : ViewModelBase
{
    /// <summary>One rendered depth rung — the level/id/title/description/hosted
    /// facts all come from the shared catalog
    /// (<c>fauna_atproto_settings_machine::depth::depth_level_options</c>,
    /// atproto.md § Where logic lives → *The rung catalog*), none re-spelled
    /// here; <see cref="Checked"/>/<see cref="Enabled"/>/<see cref="GateMarker"/>
    /// are this VM's own projection of the current snapshot onto that rung.</summary>
    public sealed class DepthRungRowVm
    {
        public string Level { get; init; } = string.Empty;
        public string UiId { get; init; } = string.Empty;
        public string Title { get; init; } = string.Empty;
        public string Description { get; init; } = string.Empty;
        public bool Checked { get; init; }
        public bool Enabled { get; init; }
        public string GateMarker { get; init; } = string.Empty;
    }

    /// <summary>The four rungs, in the catalog's own ladder order. Rebuilt on every
    /// <see cref="Render(AtprotoSettingsSnapshot)"/> — the established shape every
    /// other roster on this page (<see cref="Credentials"/>, …) already uses. Off and Linked are NEVER gated (§ Reveal/greying rules — the
    /// catalog's own <c>hosted: false</c> for those two rungs); only the two hosted
    /// rungs answer to <c>hosted_allowed</c>.
    ///
    /// <para><b>Both constructors render <see cref="PrefetchSnapshot"/> before returning</b>
    /// (<see cref="Render(AtprotoSettingsSnapshot)"/>), so the pre-fetch paint is a real
    /// render of the shared Rust default, never a hand-duplicated literal —
    /// <c>AtprotoSettingsSnapshot::default()</c> sets <c>hosted_allowed: false</c> and
    /// carries its reason (pinned Rust-side by
    /// <c>the_prefetch_default_closes_the_hosted_gate_and_says_why</c>), so the gate reads
    /// CLOSED here, not open. Windows used to say enabled + <c>GateMarkerOk</c> + no reason
    /// before construction rendered anything, which is the exact inverse: the two hosted
    /// rungs came up freely clickable on a nest that may well refuse them, with nothing on
    /// screen explaining the wait, and a click in that window was silently dropped by the
    /// <c>_machine is null</c> guard on <see cref="SelectLevelAsync"/>. Fail-closed matches
    /// every sibling app — web (<c>!(snap?.hosted_allowed ?? false)</c>) and apple
    /// (<c>snapshot?.hostedAllowed != true</c>) both treat "no snapshot" as gated.</para></summary>
    public ObservableCollection<DepthRungRowVm> DepthRungs { get; } = new();

    /// <summary>The <c>reason</c> attr values the e2e asserts on a hosted rung
    /// (<c>depth_gate_marker</c>): a greyed rung must say WHY it is greyed.</summary>
    public const string GateMarkerGated = "gated";
    public const string GateMarkerOk = "ok";

    /// <summary>The shared Rust pre-fetch default (<c>AtprotoSettingsSnapshot::default()</c>,
    /// gate CLOSED and carrying its reason — <c>libs/fauna-atproto-settings-machine/src/
    /// snapshots.rs</c>), read ONCE per process through the UniFFI seam and rendered by
    /// both constructors below, so the pre-fetch paint is a real render of the ratified
    /// default rather than a per-app stand-in (<c>ui/README.md</c> § Copy comprehensibility
    /// rule 5; mirrors apple's cached <c>Self.prefetchSnapshot</c>). This VM keeps no local
    /// literal for it — the one-key gate-reason fallback it used to carry was the same
    /// class of drift-prone stand-in android's full hand-rolled record was, just smaller
    /// (`row 260`).</summary>
    private static readonly AtprotoSettingsSnapshot PrefetchSnapshot =
        uniffi.fauna_ffi.FaunaFfiMethods.AtprotoSettingsPrefetchSnapshot();

    private readonly INestRpcClient _rpc;
    private IAtprotoSettingsMachine? _machine;

    /// <summary>Secrets revealed (by mint or explicit reveal) this session, keyed by
    /// credential id. Never persisted, never part of the snapshot (D3) — the reveal
    /// button's OWN text becomes the secret once revealed (no separate secret-display id
    /// in F1's approved ui.yaml surface). Mirrors linux's <c>Ctx.revealed</c> / web's
    /// <c>revealedSecrets</c> state.</summary>
    private readonly Dictionary<string, string> _revealedSecrets = new();

    internal AtprotoViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
        Render(PrefetchSnapshot);
    }

    /// <summary>Test seam: drive the VM over a fake machine without a nest
    /// (memory <c>reference_windows_vm_over_uniffi_machine_interface</c> — machine-backed
    /// pages go over the uniffi <c>I&lt;Machine&gt;</c> interface so FaunaApp.Tests can
    /// fake it; FlaUI flakes on win-arm64, so the unit tests are the deterministic gate).
    /// Also renders <see cref="PrefetchSnapshot"/> immediately, matching the production
    /// constructor — the fake machine's own snapshot is not read until <see cref="LoadAsync"/>
    /// runs, so a test that constructs and never loads sees the real pre-fetch render.</summary>
    internal AtprotoViewModel(IAtprotoSettingsMachine machine)
    {
        _rpc = null!;
        _machine = machine;
        Render(PrefetchSnapshot);
    }

    [ObservableProperty] private bool _isLoading;

    // ── The recovery-fork contest ceremony (ui/atproto.md § Element IDs
    //    "Contest ceremony"; mechanism: behavior/atproto-identity-custody.md
    //    § The 72 h recovery-fork contest). Renders at the TOP of the page,
    //    above the depth selector — an identity under active attack outranks
    //    every settings row below it (linux/android/apple/web/tui reference
    //    shapes; windows was the last remaining arm). ──

    [ObservableProperty] private bool _showContestCard;

    /// <summary>The `state` attr the e2e reads off `atproto-contest-card`:
    /// `contestable` / `window-closed` / `not-contestable`.</summary>
    [ObservableProperty] private string _contestState = string.Empty;

    [ObservableProperty] private string _contestDetail = string.Empty;
    [ObservableProperty] private string _contestDeadline = string.Empty;
    [ObservableProperty] private bool _showContestDeadline;

    /// <summary>Render `atproto-contest` ONLY when the machine says so (decision
    /// 2) — never inferred from <see cref="ContestState"/>, since `not-contestable`
    /// covers two distinct un-buttonable reasons.</summary>
    [ObservableProperty] private bool _showContestButton;

    [ObservableProperty] private bool _showContestConfirmCard;

    /// <summary>The confirm card's lines, rendered verbatim — what is being
    /// undone, what this device signs, what the directory rules on.</summary>
    public ObservableCollection<string> ContestConfirmLines { get; } = new();

    /// <summary>False while the submit is in flight, so a second press cannot
    /// sign a second time.</summary>
    [ObservableProperty] private bool _contestConfirmActionsEnabled = true;

    // ── The depth selector (§ Layout & flow item 1) ──────────────────────────

    /// <summary>The current level's wire spelling — <c>atproto-depth-selector</c>'s
    /// <c>state</c> attr, which is how the e2e reads the level.</summary>
    [ObservableProperty] private string _level = "off";

    /// <summary>The one-line greying reason, composed by the machine. Shown only while
    /// the gate fails — the rungs are GREYED WITH A REASON, never hidden. Seeded
    /// pre-fetch by both constructors rendering <see cref="PrefetchSnapshot"/>'s own
    /// "still checking" reason, resolved through the same <see cref="Strings.Resolve"/>
    /// path a real machine snapshot's reason takes (<see cref="Strings"/> is initialized
    /// in <c>App()</c> before any VM is constructed, so this resolves for real).</summary>
    [ObservableProperty] private string _gateReason = string.Empty;
    [ObservableProperty] private bool _showGateReason;

    // ── The transition card (§ Layout & flow item 2) ─────────────────────────

    [ObservableProperty] private bool _showCard;

    /// <summary>The machine's composed effect lines, rendered verbatim.</summary>
    public ObservableCollection<string> CardLines { get; } = new();

    [ObservableProperty] private bool _showHistoryBackfill;
    [ObservableProperty] private bool _historyBackfill;

    /// <summary>False while the confirm is in flight, so a double-confirm cannot
    /// stage two transitions.</summary>
    [ObservableProperty] private bool _cardActionsEnabled = true;

    // ── The per-level panels (§ Layout & flow item 3) ────────────────────────

    /// <summary>The consume-side link surface, revealed at level = linked. It embeds
    /// the SAME shared bridge components the Bridges page renders, so it contributes
    /// zero new element ids.</summary>
    [ObservableProperty] private bool _showLinkedPanel;

    /// <summary>The hosted panel renders at a hosted level OR while a transition is
    /// staged toward one — the DID-method radio must be visible on the card that is
    /// about to mint with it.</summary>
    [ObservableProperty] private bool _showHostedPanel;

    [ObservableProperty] private bool _showDidMethodRadio;
    [ObservableProperty] private bool _didMethodPlc = true;
    [ObservableProperty] private bool _didMethodWeb;

    /// <summary>The "your handle is @you.yourdomain either way" line.</summary>
    [ObservableProperty] private string _handlePreview = string.Empty;
    [ObservableProperty] private bool _showHandlePreview;

    /// <summary>The post-mint identity summary: derived handle · method · status.
    /// The raw DID string is deliberately never shown (§ Layout &amp; flow item 3).</summary>
    [ObservableProperty] private string _hostedHandle = string.Empty;
    [ObservableProperty] private bool _showHostedHandle;

    /// <summary>Renders whenever a hosted identity exists, active OR deactivated, so
    /// the stronger destructive action stays reachable after a step-down.</summary>
    [ObservableProperty] private bool _showDeletePresence;

    /// <summary>The delete ceremony's own confirm card (distinct from
    /// <see cref="ShowCard"/>, the depth-transition card) — <c>atproto-delete-confirm-card</c>.</summary>
    [ObservableProperty] private bool _showDeleteConfirmCard;

    /// <summary>The confirm card's lines, rendered verbatim — the machine's own copy
    /// of what the sweep destroys, that the identity survives, and where the level
    /// lands. Mirrors <see cref="ContestConfirmLines"/> beside it.</summary>
    public ObservableCollection<string> DeleteConfirmLines { get; } = new();

    /// <summary>False while the confirm is in flight, so a second press cannot send
    /// a second sweep.</summary>
    [ObservableProperty] private bool _deleteConfirmActionsEnabled = true;

    // ── The full-PDS (F1 login-plane) panel — gated on level = hosted_full ────

    [ObservableProperty] private bool _showFullPds;
    [ObservableProperty] private bool _externalAppsEnabled = true;

    /// <summary>The <c>state</c> attr the e2e reads off the kill-switch.</summary>
    public string ExternalAppsState => ExternalAppsEnabled ? "on" : "off";

    // ── The D10 authoring-delegation row (atproto-pds-full.md § App surface) ────
    //
    // What authorizes an external ATProto app to *post* as this account, as
    // opposed to merely signing in (the kill-switch + credential + connected-app
    // groups above govern that). Two shapes, one always-present control:
    //
    //  - no row: no delegation, OR one whose stored cert failed the client-side
    //    verify under this account's own identity key. The row and its leaves are
    //    WITHHELD, never rendered as a grant the user cannot be shown to have made
    //    (the mismatch surfaces on `error-message`, which the machine already set).
    //    Only `-authorize` renders.
    //  - a row: the four leaves render, and `-authorize` STAYS, because
    //    re-authorizing IS the renewal gesture: provisioning overwrites the cert,
    //    so a lapsed grant recovers in one gesture with no revoke first. Hiding it
    //    once authorized would force the revoke-then-re-mint flow the ruling
    //    forbids, and would churn the signing sub-key K for an expiry refresh.
    //
    // Every leaf but `-last-used` is derived from the SIGNED cert, re-verified
    // client-side; the composition below is straight reads plus the two SHARED
    // wire->user-voice maps, never a fourth hand-rolled C# copy (priority #4).

    [ObservableProperty] private bool _showDelegationRow;

    /// <summary><c>atproto-delegation-scope</c> - the granted capabilities in user
    /// voice, resolved through the shared <c>DelegationCapabilityLabel</c> free
    /// function rather than a C# <c>switch</c>. An unrecognized capability degrades
    /// to its wire form rather than vanishing: silently dropping one would
    /// UNDERSTATE a grant, the one direction an audit surface must never err in.</summary>
    [ObservableProperty] private string _delegationScope = string.Empty;

    /// <summary><c>atproto-delegation-lasts-until</c> - authorized-on + expiry.
    /// MICROseconds on this row (cert-derived), unlike the millisecond
    /// credential/session rows above - dividing by the wrong factor dates the grant
    /// ~50 000 years out.</summary>
    [ObservableProperty] private string _delegationLastsUntil = string.Empty;

    /// <summary><c>atproto-delegation-status</c>'s user-voice text, through the shared
    /// <c>DelegationLivenessLabel</c> map.</summary>
    [ObservableProperty] private string _delegationStatus = string.Empty;

    /// <summary>The liveness WIRE spelling (<c>active</c> / <c>expiring_soon</c> /
    /// <c>expired</c> / <c>never_expires</c>), carried in the <c>state</c> attr the e2e
    /// reads - so a wording change never breaks a test and a test never pins prose.
    /// On WinUI that attr slot is <c>AutomationProperties.HelpText</c>.</summary>
    [ObservableProperty] private string _delegationStatusState = string.Empty;

    /// <summary><c>atproto-delegation-last-used</c> - ADVISORY ONLY (D10 &#167; Audit).
    /// Every leaf above derives from the signed cert; this one is a bare nest
    /// assertion with nothing signing it, so it is not proof of use and - the
    /// direction that actually matters - not proof of NON-use: a nest that
    /// under-reports is exactly what this cannot detect. Hence the hedged wording,
    /// and the hint line pointing at the feed's <c>delegated-origin-badge</c>, which
    /// IS read from signed bytes.</summary>
    [ObservableProperty] private string _delegationLastUsed = string.Empty;

    /// <summary>The <c>-authorize</c> button's own label - "authorize" with no row,
    /// "re-authorize" with one. One control either way (the ruling); only the wording
    /// follows the state.</summary>
    public string DelegationAuthorizeText => ShowDelegationRow
        ? Strings.Get("atproto_settings/delegation_reauthorize_button")
        : Strings.Get("atproto_settings/delegation_authorize_button");

    /// <summary>One <c>atproto-app-credential-item</c> row. F1 scope deliberately has
    /// no per-field ids for label/created/last-used — they are joined into the row's own
    /// text (ui.yaml <c>atproto-app-credentials-list</c>), so the display string is
    /// composed here rather than in XAML.</summary>
    public sealed class CredentialRowVm
    {
        public string CredentialId { get; init; } = string.Empty;
        public string Display { get; init; } = string.Empty;
        public bool Revealable { get; init; }

        /// <summary>This session's revealed secret for this row, if any (mint or
        /// explicit reveal) — looked up from the page's revealed-secrets map on every
        /// <see cref="Render"/>.</summary>
        public string? RevealedSecret { get; init; }

        /// <summary><c>atproto-app-credential-reveal</c>'s own text — the F1 get_text
        /// contract (linux <c>bluesky.rs</c> / web <c>AtprotoSettingsSection.svelte</c>
        /// precedent): the localized label before reveal, the raw secret after.</summary>
        public string RevealText => RevealedSecret ?? Strings.Get("atproto_settings/reveal_button");

        /// <summary>Disabled once revealed this session — matches linux's
        /// <c>set_sensitive(false)</c> / web's <c>disabled</c>; a disabled button that
        /// already shows the secret needs no re-click.</summary>
        public bool CanReveal => Revealable && RevealedSecret is null;
    }

    public ObservableCollection<CredentialRowVm> Credentials { get; } = new();

    partial void OnExternalAppsEnabledChanged(bool value) => OnPropertyChanged(nameof(ExternalAppsState));

    // ── Load + render ────────────────────────────────────────────────────────

    public async Task LoadAsync()
    {
        IsLoading = true;
        try
        {
            // Build-once-per-session, not per-page-visit (AtprotoSettingsMachineHost's
            // doc comment: a fresh machine on every navigation silently resets the
            // S4-C custody check's debounce, the per-visit-rebuild bug android hit).
            _machine ??= await AtprotoSettingsMachineHost.Instance.GetOrBuildAsync(_rpc, new NoopObserver());
            await _machine.Refresh();
            Render();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Render the live machine's current snapshot — every gesture handler's
    /// post-action repaint, and <see cref="LoadAsync"/>'s post-<c>Refresh()</c> repaint.
    /// A no-op before <see cref="LoadAsync"/> has built <c>_machine</c> (both constructors
    /// already painted <see cref="PrefetchSnapshot"/> directly, so there is nothing to
    /// bail out of — this guard just protects a stray call between construction and the
    /// first successful load).</summary>
    private void Render()
    {
        if (_machine is null) return;
        Render(_machine.Snapshot());
    }

    /// <summary>Project a snapshot onto the bound properties. This is the ONLY place
    /// state is derived, and every line of it is a straight read — the reveal rules, the
    /// gate verdict and the card copy are all machine-composed. Called with the live
    /// machine's snapshot after every gesture, and with <see cref="PrefetchSnapshot"/>
    /// directly from both constructors (before any machine exists).</summary>
    private void Render(AtprotoSettingsSnapshot snap)
    {
        // Recovery-fork contest ceremony — leads the page (ui/atproto.md § Layout
        // & flow item 1's own ordering note): rendered before the selector below.
        if (snap.@contest is { } contest)
        {
            ShowContestCard = true;
            ContestState = contest.@state;
            ContestDetail = Strings.Resolve(contest.@detail);
            ContestDeadline = contest.@deadline is { } deadline ? Strings.Resolve(deadline) : string.Empty;
            ShowContestDeadline = contest.@deadline is not null;
            ShowContestButton = contest.@showContest;
        }
        else
        {
            ShowContestCard = false;
            ContestState = string.Empty;
            ContestDetail = string.Empty;
            ContestDeadline = string.Empty;
            ShowContestDeadline = false;
            ShowContestButton = false;
        }

        ContestConfirmLines.Clear();
        if (snap.@contestConfirm is { } contestConfirm)
        {
            ShowContestConfirmCard = true;
            foreach (var line in contestConfirm.@lines) ContestConfirmLines.Add(Strings.Resolve(line));
            ContestConfirmActionsEnabled = !contestConfirm.@inProgress;
        }
        else
        {
            ShowContestConfirmCard = false;
            ContestConfirmActionsEnabled = true;
        }

        // Selector. The rung table itself — level, id, title, description, and
        // which rungs are hosted-gate-subject — is the shared catalog's, read
        // fresh every render rather than cached: it is a UniFFI call over a
        // static Rust table, not per-render state.
        Level = snap.@level;
        DepthRungs.Clear();
        foreach (var opt in FaunaAtprotoSettingsMachineMethods.DepthLevelOptions())
        {
            var isChecked = snap.@level == opt.@level;
            // A hosted rung the user is ALREADY AT stays enabled, so a step-down
            // is reachable even if the domain later stops being public: the gate
            // only ever blocks *entering* a hosted level from a lower one (linux
            // settings/atproto.rs). `opt.hosted` is the catalog's own fact — never a
            // `level.StartsWith("hosted")` sniff over a wire string this VM did
            // not define.
            var gated = opt.@hosted && !snap.@hostedAllowed;
            var enabled = !gated || isChecked;
            DepthRungs.Add(new DepthRungRowVm
            {
                Level = opt.@level,
                UiId = opt.@uiId,
                Title = Strings.Resolve(opt.@title),
                Description = Strings.Resolve(opt.@description),
                Checked = isChecked,
                Enabled = enabled,
                GateMarker = enabled ? GateMarkerOk : GateMarkerGated,
            });
        }

        GateReason = snap.@hostedGateReason is null ? string.Empty : Strings.Resolve(snap.@hostedGateReason);
        ShowGateReason = !snap.@hostedAllowed && GateReason.Length > 0;

        // Transition card — lines verbatim from the machine.
        CardLines.Clear();
        if (snap.@pendingTransition is { } card)
        {
            ShowCard = true;
            foreach (var line in card.@lines) CardLines.Add(Strings.Resolve(line));
            ShowHistoryBackfill = card.@showHistoryBackfill;
            CardActionsEnabled = !card.@inProgress;
        }
        else
        {
            ShowCard = false;
            ShowHistoryBackfill = false;
            CardActionsEnabled = true;
        }
        HistoryBackfill = snap.@historyBackfill;

        // Linked panel.
        ShowLinkedPanel = snap.@level == "linked";

        // Hosted panel — at a hosted level, or staged toward one.
        var targetingHosted = snap.@pendingTransition?.@targetLevel.StartsWith("hosted", StringComparison.Ordinal) == true;
        ShowHostedPanel = snap.@level.StartsWith("hosted", StringComparison.Ordinal) || targetingHosted;
        ShowDidMethodRadio = ShowHostedPanel && snap.@showDidMethodRadio;
        DidMethodPlc = snap.@didMethod == "plc";
        DidMethodWeb = snap.@didMethod == "web";
        HandlePreview = snap.@handlePreview.Length == 0
            ? string.Empty
            : Strings.Get("atproto_settings/handle_either_way").Replace("{handle}", snap.@handlePreview);
        ShowHandlePreview = ShowDidMethodRadio && HandlePreview.Length > 0;

        // Gated on the IDENTITY, not on the level (ui/atproto.md § Errors & edge
        // cases: "A deactivated identity at level Off/Linked: the identity summary
        // renders (marked deactivated) so the user can see what re-enabling
        // restores" — ShowHostedPanel is exactly the gate that rule can never hold
        // through, since Off/Linked collapse the hosted panel entirely). tui/linux/
        // web/android/macos/ios all key this off the identity alone; windows was
        // the last app still ANDing it with the panel.
        if (snap.@identity is { } id)
        {
            // Same three-part composition linux renders (bluesky.rs:732) so the
            // summary reads identically on every app: handle · method · status.
            HostedHandle = string.Join(
                " · ",
                Strings.Get("atproto_settings/hosted_handle_prefix").Replace("{handle}", id.@handle),
                Strings.Get("atproto_settings/hosted_method_prefix").Replace("{method}", id.@method),
                IdentityStatusLabel(id.@status));
            ShowHostedHandle = true;
        }
        else
        {
            ShowHostedHandle = false;
        }

        ShowDeletePresence = snap.@showDeletePresence;

        // Its own confirm card — never the depth selector's. The copy is the
        // machine's, rendered verbatim: the ceremony's promises about what
        // survives are not this VM's to word. Mirrors ContestConfirmCard above.
        DeleteConfirmLines.Clear();
        if (snap.@deleteConfirm is { } deleteConfirm)
        {
            ShowDeleteConfirmCard = true;
            foreach (var line in deleteConfirm.@lines) DeleteConfirmLines.Add(Strings.Resolve(line));
            DeleteConfirmActionsEnabled = !deleteConfirm.@inProgress;
        }
        else
        {
            ShowDeleteConfirmCard = false;
            DeleteConfirmActionsEnabled = true;
        }

        // Full-PDS panel — the deepest level's own panel.
        ShowFullPds = snap.@level == "hosted_full";
        ExternalAppsEnabled = snap.@externalAppsEnabled;
        Credentials.Clear();
        foreach (var c in snap.@credentials)
        {
            _revealedSecrets.TryGetValue(c.@credentialId, out var revealed);
            Credentials.Add(new CredentialRowVm
            {
                CredentialId = c.@credentialId,
                Revealable = c.@revealable,
                Display = c.@label,
                RevealedSecret = revealed,
            });
        }

        // The D10 authoring-delegation row. `snap.delegation` is None both when no
        // delegation exists and when the stored cert FAILED the client-side verify
        // under this account's own identity key - the machine collapses those two
        // deliberately, because a cert the account cannot be shown to have signed
        // must never render as a grant (the mismatch is already on `error-message`).
        var delegation = snap.@delegation;
        ShowDelegationRow = delegation is not null;
        OnPropertyChanged(nameof(DelegationAuthorizeText));
        if (delegation is { } d)
        {
            // Both maps are SHARED Rust free functions reached over UniFFI - tui and
            // linux call `DelegationRow::{capability_labels,status_label}` directly,
            // and a fourth hand-written copy is where the apps start disagreeing.
            DelegationScope = Strings.Get("atproto_settings/delegation_scope_prefix")
                .Replace("{capabilities}", string.Join(
                    ", ",
                    d.@capabilities.Select(c => Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.DelegationCapabilityLabel(c)))));
            // MICROseconds -> milliseconds before formatting. Cert-derived, unlike
            // every other timestamp on this page.
            var authorized = FormatDelegationStamp((long)(d.@authorizedAtMicros / 1_000));
            DelegationLastsUntil = d.@expiresAtMicros is { } expires
                ? Strings.Get("atproto_settings/delegation_lasts_until")
                    .Replace("{authorized}", authorized)
                    .Replace("{expires}", FormatDelegationStamp((long)(expires / 1_000)))
                : Strings.Get("atproto_settings/delegation_lasts_until_no_expiry")
                    .Replace("{authorized}", authorized);
            DelegationStatus = Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.DelegationLivenessLabel(d.@liveness));
            DelegationStatusState = d.@liveness;
            DelegationLastUsed = d.@lastUsedAtMillis is { } used
                ? Strings.Get("atproto_settings/delegation_last_used")
                    .Replace("{when}", FormatDelegationStamp(used))
                : Strings.Get("atproto_settings/delegation_last_used_never");
        }

        // The machine owns the page error (a refused gated selection surfaces here
        // rather than being silently dropped — testing.md rule 11).
        SetError(snap.@error is null ? null : Strings.Resolve(snap.@error));
    }

    // The one shared reading (`identity_status_label`, UniFFI
    // `IdentityStatusLabel`); an unrecognized status degrades to its wire word.
    private static string IdentityStatusLabel(string status) =>
        Strings.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.IdentityStatusLabel(status));

    // ── Gestures (forwarded; the machine decides what each move means) ───────

    /// <summary>Select a level. Off→Linked is the ONE effect-free transition and the
    /// machine applies it immediately with no card; every other move stages the card
    /// first, and a gated hosted selection is refused loudly on the error banner.
    /// This VM does not know which is which — that is the machine's matrix.</summary>
    [RelayCommand]
    public async Task SelectLevelAsync(string targetLevel)
    {
        if (_machine is null) return;
        try
        {
            await _machine.SelectLevel(targetLevel);
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    public async Task ConfirmTransitionAsync()
    {
        if (_machine is null) return;
        try
        {
            await _machine.ConfirmTransition();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    public void CancelTransition()
    {
        if (_machine is null) return;
        try
        {
            _machine.CancelTransition();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Open the contest confirm card (`atproto-contest`). A SYNC pure-local
    /// machine mutation — no wire kind, mirrors linux's `open_contest_confirm`.</summary>
    [RelayCommand]
    public void OpenContestConfirm()
    {
        if (_machine is null) return;
        try
        {
            _machine.OpenContestConfirm();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Close the contest confirm card (`atproto-contest-cancel`) — nothing
    /// signed, no intent recorded.</summary>
    [RelayCommand]
    public void CancelContest()
    {
        if (_machine is null) return;
        try
        {
            _machine.CancelContest();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Record the scoped contest intent and run the contest converge
    /// (`atproto-contest-confirm`). Client-direct HTTPS to the public PLC
    /// directory — declares NO wire kind, unlike every other button on this
    /// page (ui/atproto.md § User actions).</summary>
    /// <summary>Open the delete ceremony's confirm card (`atproto-delete-presence`).
    /// A SYNC pure-local machine mutation — no wire kind, mirrors linux's
    /// `open_delete_confirm` / tui's `Action::AtprotoDeletePresence`.</summary>
    [RelayCommand]
    public void OpenDeleteConfirm()
    {
        if (_machine is null) return;
        try
        {
            _machine.OpenDeleteConfirm();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Close the delete confirm card (`atproto-delete-cancel`) — nothing
    /// deleted.</summary>
    [RelayCommand]
    public void CancelDelete()
    {
        if (_machine is null) return;
        try
        {
            _machine.CancelDelete();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Run the delete-presence sweep (`atproto-delete-confirm`) —
    /// `fauna.bridges.atproto.delete_presence`, the one network round trip in the
    /// ceremony.</summary>
    [RelayCommand]
    public async Task ConfirmDeleteAsync()
    {
        if (_machine is null) return;
        try
        {
            await _machine.ConfirmDelete();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    public async Task RequestContestAsync()
    {
        if (_machine is null) return;
        try
        {
            await _machine.RequestContest();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    public void SetDidMethod(string method)
    {
        if (_machine is null) return;
        _machine.SetDidMethod(method);
        Render();
    }

    public void SetHistoryBackfill(bool enabled)
    {
        if (_machine is null) return;
        _machine.SetHistoryBackfill(enabled);
        Render();
    }

    public async Task SetExternalAppsEnabledAsync(bool enabled)
    {
        if (_machine is null) return;
        try
        {
            await _machine.SetExternalAppsEnabled(enabled);
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Mint an app credential. The secret is served exactly ONCE — the nest
    /// custodies only a verifier and can never re-serve it — so this session records it
    /// against the freshly minted row: diff the credential-id set before/after mint to
    /// find which row is new (linux <c>bluesky.rs</c> mint handler / web
    /// <c>mintCredential</c>'s pattern — <c>mint()</c> already refreshes the machine
    /// before returning, so the post-mint snapshot carries the new row). The row's own
    /// <see cref="CredentialRowVm.RevealText"/> then shows the secret immediately —
    /// there is no separate one-time-reveal surface in F1.</summary>
    public async Task MintCredentialAsync(string label)
    {
        if (_machine is null) return;
        try
        {
            var before = _machine.Snapshot().@credentials.Select(c => c.@credentialId).ToHashSet();
            var secret = await _machine.Mint(label, false);
            var newId = _machine.Snapshot().@credentials
                .Select(c => c.@credentialId)
                .FirstOrDefault(id => !before.Contains(id));
            if (!string.IsNullOrEmpty(newId)) _revealedSecrets[newId] = secret;
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Reveal row <paramref name="credentialId"/>'s secret. A no-op re-render
    /// when already revealed this session (mirrors web's <c>revealCredential</c> guard)
    /// — the row's own button is disabled once revealed, so this only runs on a fresh
    /// click.</summary>
    public async Task RevealSecretAsync(string credentialId)
    {
        if (_machine is null) return;
        if (_revealedSecrets.ContainsKey(credentialId))
        {
            Render();
            return;
        }
        try
        {
            _revealedSecrets[credentialId] = await _machine.RevealSecret(credentialId);
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    public async Task RevokeCredentialAsync(string credentialId)
    {
        if (_machine is null) return;
        try
        {
            await _machine.Revoke(credentialId);
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Authorize - or RE-authorize - external apps to post as this account
    /// (<c>atproto-delegation-authorize</c>). One control for both: provisioning
    /// overwrites the stored cert with a freshly dated one, so a lapsed delegation
    /// recovers without revoking first (atproto-pds-full.md &#167; App surface -
    /// "re-minting IS renewal"). A revoke-then-re-mint flow would destroy the signing
    /// sub-key K and churn it for what is only an expiry refresh.</summary>
    public async Task AuthorizeExternalAppsAsync()
    {
        if (_machine is null) return;
        try
        {
            await _machine.AuthorizeExternalApps();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Revoke (<c>atproto-delegation-revoke</c>) - the separate DESTRUCTIVE
    /// action, never the renewal path: it destroys the signing sub-key K. Already
    /// published posts stay verifiable forever (the cert is embedded in their bytes),
    /// so this stops future writes rather than un-writing past ones
    /// (atproto-pds-full.md &#167; Revocation).</summary>
    public async Task DeauthorizeExternalAppsAsync()
    {
        if (_machine is null) return;
        try
        {
            await _machine.DeauthorizeExternalApps();
            Render();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>Epoch-milliseconds -> the page's date wording. Medium date, no clock
    /// time: every stamp on this row is a day-scale fact (authorized-on, lapses-on,
    /// last-reported-use), and a minute-precision rendering would imply a precision
    /// the advisory leaf in particular does not have.</summary>
    private static string FormatDelegationStamp(long millis) =>
        DateTimeOffset.FromUnixTimeMilliseconds(millis).LocalDateTime.ToString("d MMM yyyy");

    /// <summary>No-op observer: this page re-reads the snapshot after every gesture
    /// (the non-optimistic rule), so it needs no push reactivity — the established
    /// windows pattern, and the one that keeps the e2e deterministic.</summary>
    private sealed class NoopObserver : AtprotoSettingsObserver
    {
        public void OnChanged()
        {
        }
    }
}
