using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_atproto_settings_machine;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Bluesky integration-depth page VM (docs/goal/ui/atproto.md § Layout &amp; flow,
/// § Reveal/greying rules, § Transition semantics). windows is the LAST app to build
/// this page — linux/apple landed 2026-07-23, tui 07-24, web 07-29, android 07-30 —
/// so these cases pin the rules the other five already paid for, and are the
/// deterministic gate on Windows (FlaUI flakes on win-arm64).
///
/// <para>They drive a FAKE <see cref="IAtprotoSettingsMachine"/> through the VM's test
/// constructor, so no nest and no <c>FfiNestClient</c> is needed (memory
/// <c>reference_windows_vm_over_uniffi_machine_interface</c>). What they assert is
/// exactly the VM's job — <b>projection of a machine snapshot</b>. They deliberately
/// do NOT re-assert the transition matrix itself: that is the machine's, pinned in
/// shared Rust and in <c>conformance_atproto_integration_level.rs</c>. A client-side
/// copy of it would be the priority-#2 violation this page exists to avoid.</para>
/// </summary>
/// <remarks>Joins the <c>StringsGlobal</c> collection because the identity-summary and
/// handle-preview cases install a localizer into the process-global
/// <see cref="FaunaApp.Core.Services.Strings"/>. Without a localizer every key falls
/// back to its raw self, the <c>{handle}</c> placeholder is never substituted, and an
/// assertion that the handle is displayed would pass or fail for reasons unrelated to
/// the VM — so these cases resolve for real rather than asserting on key names.</remarks>
[Collection("StringsGlobal")]
public class AtprotoViewModelTests
{
    /// <summary>The subset of `atproto_settings.*` this file's assertions depend on,
    /// with the same values `i18n/strings/en.yaml` carries (so a placeholder rename
    /// there surfaces here rather than silently dropping the handle).</summary>
    private sealed class FakeLocalizer : FaunaApp.Core.Services.IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["atproto_settings/handle_either_way"] = "Your Bluesky handle will be @{handle} either way.",
            ["atproto_settings/hosted_handle_prefix"] = "Your Bluesky handle: @{handle}",
            ["atproto_settings/hosted_method_prefix"] = "Method: {method}",
            ["atproto_settings/identity_status_active"] = "Active",
            ["atproto_settings/identity_status_pending"] = "Setting up…",
            ["atproto_settings/identity_status_deactivated"] = "Deactivated",
            ["atproto_settings/identity_status_deleted"] = "Deleted",
            ["atproto_settings/identity_status_tombstoned"] = "Permanently retired",
            ["atproto_settings/session_status_live"] = "Working",
            ["atproto_settings/session_status_suspended"] = "Paused",
            ["atproto_settings/consent_client"] = "{name} — {client_id}",
            ["atproto_settings/consent_client_unnamed"] = "{client_id}",
            ["atproto_settings/consent_code"] = "Confirmation code: {code}",
            ["atproto_settings/consent_set_heading"] = "Some of that comes from “{title}” ({nsid}):",
            ["atproto_settings/consent_set_heading_unnamed"] = "Some of that comes from {nsid}:",
            ["atproto_settings/reveal_button"] = "Reveal",
            ["atproto_settings/session_created_prefix"] = "Connected {date}",
            ["atproto_settings/session_expires_prefix"] = "Expires {date}",
            ["atproto_settings/session_scopes_prefix"] = "Approved for {scopes}",
            ["atproto_settings/session_sets_prefix"] = "Granted via {sets}",
            ["atproto_settings/session_set_named"] = "“{title}” ({nsid})",
            ["atproto_settings/session_last_used_prefix"] = "Last seen {date}",
            ["atproto_settings/session_never_used"] = "Not seen since connecting",
            // The pre-fetch gate reason, resolved through the SAME path on both sides of
            // the pre-fetch differential below (the VM's own default resolves the slashed
            // key; a rendered snapshot resolves the dotted LocalizedText) — so the
            // comparison is on displayed text, not on which spelling each side happened
            // to hold.
            ["atproto_settings/gate_reason_pending"] =
                "Checking whether this nest has a public domain — hosting an AT Protocol identity needs one.",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    private static void UseRealStrings() =>
        FaunaApp.Core.Services.Strings.Initialize(new FakeLocalizer());

    // ── The fake machine ────────────────────────────────────────────────────

    // Derives from the GENERATED fake base rather than implementing
    // IAtprotoSettingsMachine directly: every member is virtual and throws, so a
    // later widening of the Rust interface cannot break this file (the base is
    // regenerated with it). Implementing the interface by hand is what made this
    // the third CS0535 gate red of the same class — `AuthorizeExternalApps` /
    // `DeauthorizeExternalApps` were added Rust-side and nothing here followed.
    // Same shape as FakeAccountRegistry / FakeDraftsSync / FakeWebClient.
    private sealed class FakeMachine : AtprotoSettingsMachineFakeBase
    {
        public AtprotoSettingsSnapshot Snap;
        public readonly List<string> Calls = new();

        /// <summary>What <c>SelectLevel</c> should make the snapshot become — the
        /// machine's matrix lives in Rust, so the fake just plays back a scripted
        /// outcome (effect-free move → level changes with no card; staged move →
        /// card appears, level unchanged).</summary>
        public Func<string, AtprotoSettingsSnapshot>? OnSelect;

        public FakeMachine(AtprotoSettingsSnapshot snap) { Snap = snap; }

        public override AtprotoSettingsSnapshot Snapshot() => Snap;

        public override Task SelectLevel(string targetLevel)
        {
            Calls.Add($"SelectLevel:{targetLevel}");
            if (OnSelect is not null) Snap = OnSelect(targetLevel);
            return Task.CompletedTask;
        }

        public override Task ConfirmTransition() { Calls.Add("ConfirmTransition"); return Task.CompletedTask; }
        public override void CancelTransition() => Calls.Add("CancelTransition");
        public override void OpenContestConfirm() => Calls.Add("OpenContestConfirm");
        public override void CancelContest() => Calls.Add("CancelContest");
        public override void OpenDeleteConfirm() => Calls.Add("OpenDeleteConfirm");
        public override void CancelDelete() => Calls.Add("CancelDelete");

        /// <summary>Scripted like <see cref="OnRequestContest"/> — plays back whatever
        /// the machine's own sweep would have produced.</summary>
        public Func<AtprotoSettingsSnapshot>? OnConfirmDelete;
        public override Task ConfirmDelete()
        {
            Calls.Add("ConfirmDelete");
            if (OnConfirmDelete is not null) Snap = OnConfirmDelete();
            return Task.CompletedTask;
        }

        /// <summary>Scripted like <c>OnSelect</c> — the fake plays back whatever the
        /// machine's own converge would have produced (a fresh snapshot, e.g. with
        /// <c>contestConfirm.inProgress</c> flipped, or the contest cleared).</summary>
        public Func<AtprotoSettingsSnapshot>? OnRequestContest;
        public override Task RequestContest()
        {
            Calls.Add("RequestContest");
            if (OnRequestContest is not null) Snap = OnRequestContest();
            return Task.CompletedTask;
        }
        public override void SetDidMethod(string method) => Calls.Add($"SetDidMethod:{method}");
        public override void SetHistoryBackfill(bool enabled) => Calls.Add($"SetHistoryBackfill:{enabled}");
        public override Task SetExternalAppsEnabled(bool enabled) { Calls.Add($"SetExternalAppsEnabled:{enabled}"); return Task.CompletedTask; }
        public override Task Refresh() { Calls.Add("Refresh"); return Task.CompletedTask; }
        // Revoke and the external-apps authorize pair are left to
        // the base's throwing defaults — this fake's cases never reach them, and
        // not restating them is what keeps the file immune to the next interface
        // widening.

        /// <summary>Scripted like <c>OnSelect</c>: the real <c>mint()</c> refreshes
        /// the machine before returning (machine.rs), so the fake mimics that by
        /// handing back the post-mint snapshot alongside the secret.</summary>
        public Func<string, bool, (AtprotoSettingsSnapshot Snap, string Secret)>? OnMint;
        public override Task<string> Mint(string label, bool dmAllowed)
        {
            Calls.Add($"Mint:{label}:{dmAllowed}");
            if (OnMint is not null)
            {
                var (snap, secret) = OnMint(label, dmAllowed);
                Snap = snap;
                return Task.FromResult(secret);
            }
            throw new NotSupportedException("OnMint not scripted");
        }

        public Func<string, string>? OnRevealSecret;
        public override Task<string> RevealSecret(string credentialId)
        {
            Calls.Add($"RevealSecret:{credentialId}");
            if (OnRevealSecret is not null) return Task.FromResult(OnRevealSecret(credentialId));
            throw new NotSupportedException("OnRevealSecret not scripted");
        }
    }

    private static LocalizedText Text(string key) => new(key, new Dictionary<string, string>());

    /// <summary>The delete-confirm card's retire-identity opt-in in its freshly
    /// opened state (live, unticked) — the machine's `RetireIdentityOptIn` gained
    /// with the S5 tombstone slice; none of the card tests here exercise it.</summary>
    private static RetireIdentityOptIn NoRetire() => new(true, false, null);

    /// <summary>A snapshot at <paramref name="level"/> with the gate open and nothing
    /// staged — the shape every case starts from and varies one axis of.</summary>
    private static AtprotoSettingsSnapshot Snapshot(
        string level = "off",
        bool hostedAllowed = true,
        LocalizedText? gateReason = null,
        IdentitySummaryRow? identity = null,
        TransitionCardModel? pending = null,
        bool showDidMethodRadio = false,
        string didMethod = "plc",
        bool historyBackfill = false,
        bool showDeletePresence = false,
        DeleteConfirmCardModel? deleteConfirm = null,
        string handlePreview = "",
        bool externalAppsEnabled = true,
        ContestCardRow? contest = null,
        ContestConfirmCardModel? contestConfirm = null,
        AppCredentialRow[]? credentials = null) =>
        new(
            @level: level,
            @hostedAllowed: hostedAllowed,
            @hostedGateReason: gateReason,
            @handlePreview: handlePreview,
            @identity: identity,
            @link: null,
            @pendingTransition: pending,
            @didMethod: didMethod,
            @showDidMethodRadio: showDidMethodRadio,
            @historyBackfill: historyBackfill,
            @showDeletePresence: showDeletePresence,
            @deleteConfirm: deleteConfirm,
            @credentials: credentials ?? Array.Empty<AppCredentialRow>(),
            @sessions: Array.Empty<AtprotoSessionRow>(),
            @externalAppsEnabled: externalAppsEnabled,
            @delegation: null,
            @consents: Array.Empty<ConsentCardRow>(),
            @contest: contest,
            @contestConfirm: contestConfirm,
            @error: null);

    private static AppCredentialRow Credential(
        string credentialId = "cred-1",
        string label = "Ivory",
        bool revealable = true,
        bool dmAllowed = false,
        long createdAtMillis = 0,
        long? lastUsedAtMillis = null) =>
        new(
            @credentialId: credentialId,
            @label: label,
            @dmAllowed: dmAllowed,
            @createdAtMillis: createdAtMillis,
            @lastUsedAtMillis: lastUsedAtMillis,
            @revealable: revealable);

    private static async Task<(AtprotoViewModel Vm, FakeMachine M)> LoadedAsync(AtprotoSettingsSnapshot snap)
    {
        var m = new FakeMachine(snap);
        var vm = new AtprotoViewModel(m);
        await vm.LoadAsync();
        return (vm, m);
    }

    /// <summary>Look up one rendered rung by its wire level — the generic
    /// <see cref="AtprotoViewModel.DepthRungs"/> collection replaces the four
    /// named per-rung properties windows used to carry (atproto.md § Where
    /// logic lives → *The rung catalog*).</summary>
    private static AtprotoViewModel.DepthRungRowVm Rung(AtprotoViewModel vm, string level) =>
        vm.DepthRungs.Single(r => r.Level == level);

    // ── The selector ────────────────────────────────────────────────────────

    [Fact]
    public async Task Defaults_to_off_with_only_that_rung_checked()
    {
        var (vm, _) = await LoadedAsync(Snapshot(level: "off"));

        Assert.Equal("off", vm.Level);
        Assert.Equal(4, vm.DepthRungs.Count);
        Assert.True(Rung(vm, "off").Checked);
        Assert.False(Rung(vm, "linked").Checked);
        Assert.False(Rung(vm, "hosted_visible").Checked);
        Assert.False(Rung(vm, "hosted_full").Checked);
    }

    [Fact]
    public async Task The_level_the_machine_reports_is_the_level_rendered()
    {
        foreach (var level in new[] { "off", "linked", "hosted_visible", "hosted_full" })
        {
            var (vm, _) = await LoadedAsync(Snapshot(level: level));
            Assert.Equal(level, vm.Level);
        }
    }

    /// <summary>§ Reveal/greying rules: the hosted rungs are GREYED WITH A REASON,
    /// never hidden — and Off/Linked are never gated at all. The e2e reads exactly
    /// these two signals (<c>is_depth_enabled</c> + the <c>reason</c> attr).</summary>
    [Fact]
    public async Task Hosted_rungs_grey_with_a_reason_when_the_domain_gate_fails()
    {
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "off", hostedAllowed: false, gateReason: Text("atproto_settings/gate_localhost")));

        Assert.False(Rung(vm, "hosted_visible").Enabled);
        Assert.False(Rung(vm, "hosted_full").Enabled);
        Assert.Equal(AtprotoViewModel.GateMarkerGated, Rung(vm, "hosted_visible").GateMarker);
        Assert.Equal(AtprotoViewModel.GateMarkerGated, Rung(vm, "hosted_full").GateMarker);

        // Greyed, never hidden: the reason must actually be shown.
        Assert.True(vm.ShowGateReason);
        Assert.NotEqual(string.Empty, vm.GateReason);
    }

    /// <summary>The PRE-FETCH window is a real render, and it must be the shared Rust
    /// default's render. Both constructors now render
    /// <c>uniffi.fauna_ffi.FaunaFfiMethods.AtprotoSettingsPrefetchSnapshot()</c> before
    /// returning — before that fix, the VM's own field initializers painted
    /// the two hosted rungs ENABLED and <c>ok</c> with no reason on screen, the exact
    /// inverse of <c>AtprotoSettingsSnapshot::default()</c> (<c>hosted_allowed: false</c>
    /// + its reason, pinned Rust-side by
    /// <c>the_prefetch_default_closes_the_hosted_gate_and_says_why</c>).
    ///
    /// <para>Asserted DIFFERENTIALLY against a render of the SAME real seam call
    /// (<c>AtprotoSettingsPrefetchSnapshot()</c>, not a hand-typed literal standing in
    /// for it) — a hand-typed expectation is precisely what drifts (android's hand-rolled
    /// copy of this record drifted from the Rust default and had to be fixed; a
    /// hand-typed test literal is the same drift risk one level down). This still proves
    /// something real: it pins that a fresh, unloaded VM's construction-time render
    /// equals a loaded VM's render of the identical snapshot — a regression that skips
    /// the constructor's <c>Render(PrefetchSnapshot)</c> call, or seeds it from the wrong
    /// value, breaks this. The absolute half below then keeps the differential from
    /// passing vacuously if both sides ever moved together.</para></summary>
    [Fact]
    public async Task Prefetch_renders_the_shared_Rust_default_gate_not_an_open_one()
    {
        UseRealStrings();

        // Never loaded. The fake carries an OPEN-gate snapshot deliberately: nothing here
        // may come from it — the constructor already rendered PrefetchSnapshot, and
        // LoadAsync() (which alone would read the fake's Snap) is never called.
        var fresh = new AtprotoViewModel(new FakeMachine(Snapshot(hostedAllowed: true)));

        // The same VM, loaded with the REAL seam's own prefetch snapshot — not a
        // hand-typed stand-in for it, so this can never drift from the true default.
        var (rendered, _) = await LoadedAsync(uniffi.fauna_ffi.FaunaFfiMethods.AtprotoSettingsPrefetchSnapshot());

        Assert.Equal(Rung(rendered, "hosted_visible").Enabled, Rung(fresh, "hosted_visible").Enabled);
        Assert.Equal(Rung(rendered, "hosted_full").Enabled, Rung(fresh, "hosted_full").Enabled);
        Assert.Equal(Rung(rendered, "hosted_visible").GateMarker, Rung(fresh, "hosted_visible").GateMarker);
        Assert.Equal(Rung(rendered, "hosted_full").GateMarker, Rung(fresh, "hosted_full").GateMarker);
        Assert.Equal(rendered.GateReason, fresh.GateReason);
        Assert.Equal(rendered.ShowGateReason, fresh.ShowGateReason);

        // The half that actually matters, spelled out: fail CLOSED, and say why.
        Assert.False(Rung(fresh, "hosted_visible").Enabled);
        Assert.False(Rung(fresh, "hosted_full").Enabled);
        Assert.Equal(AtprotoViewModel.GateMarkerGated, Rung(fresh, "hosted_visible").GateMarker);
        Assert.Equal(AtprotoViewModel.GateMarkerGated, Rung(fresh, "hosted_full").GateMarker);
        Assert.True(fresh.ShowGateReason);
        Assert.NotEqual(string.Empty, fresh.GateReason);
    }

    [Fact]
    public async Task The_gate_never_touches_off_or_linked()
    {
        var (vm, _) = await LoadedAsync(Snapshot(hostedAllowed: false, gateReason: Text("k")));

        Assert.Equal(AtprotoViewModel.GateMarkerGated, Rung(vm, "hosted_visible").GateMarker);
        // Off/Linked are never gate-subject (the catalog's own `hosted: false`) —
        // both stay enabled and read `ok` regardless of the domain gate.
        Assert.True(Rung(vm, "off").Enabled);
        Assert.Equal(AtprotoViewModel.GateMarkerOk, Rung(vm, "off").GateMarker);
        Assert.True(Rung(vm, "linked").Enabled);
        Assert.Equal(AtprotoViewModel.GateMarkerOk, Rung(vm, "linked").GateMarker);
        Assert.True(Rung(vm, "off").Checked);
    }

    /// <summary>The gate blocks ENTERING a hosted level, never leaving one: a rung the
    /// user is already at stays enabled even after the domain stops being public, or a
    /// step-down would be unreachable (linux bluesky.rs:654).</summary>
    [Fact]
    public async Task A_hosted_rung_the_user_is_already_at_stays_selectable_when_gated()
    {
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "hosted_visible", hostedAllowed: false, gateReason: Text("k")));

        Assert.True(Rung(vm, "hosted_visible").Enabled);
        Assert.Equal(AtprotoViewModel.GateMarkerOk, Rung(vm, "hosted_visible").GateMarker);
        // The rung *above* is still barred — it would be an entry.
        Assert.False(Rung(vm, "hosted_full").Enabled);
        Assert.Equal(AtprotoViewModel.GateMarkerGated, Rung(vm, "hosted_full").GateMarker);
    }

    [Fact]
    public async Task No_gate_reason_is_shown_while_the_gate_passes()
    {
        var (vm, _) = await LoadedAsync(Snapshot(hostedAllowed: true));

        Assert.True(Rung(vm, "hosted_visible").Enabled);
        Assert.False(vm.ShowGateReason);
    }

    // ── The recovery-fork contest ceremony ──────────────────────────────────

    /// <summary>Renders off client-side evidence only — an ordinary snapshot with no
    /// standing violation carries no contest card at all.</summary>
    [Fact]
    public async Task No_contest_card_renders_in_the_ordinary_state()
    {
        var (vm, _) = await LoadedAsync(Snapshot(contest: null));

        Assert.False(vm.ShowContestCard);
        Assert.Equal(string.Empty, vm.ContestState);
        Assert.Equal(string.Empty, vm.ContestDetail);
        Assert.False(vm.ShowContestDeadline);
        Assert.False(vm.ShowContestButton);
    }

    /// <summary>`atproto-contest-detail` is rendered VERBATIM — it deliberately names
    /// what undoing does NOT restore (the nest keeps the publishing key), and a
    /// re-authored copy is exactly the failure ui/atproto.md § Element IDs warns
    /// against.</summary>
    [Fact]
    public async Task Contest_card_state_and_detail_render_verbatim()
    {
        var (vm, _) = await LoadedAsync(Snapshot(contest: new ContestCardRow(
            "contestable",
            Text("atproto_settings/contest_detail_contestable"),
            Text("atproto_settings/contest_deadline"),
            true)));

        Assert.True(vm.ShowContestCard);
        Assert.Equal("contestable", vm.ContestState);
        Assert.Equal("atproto_settings/contest_detail_contestable", vm.ContestDetail);
        Assert.True(vm.ShowContestDeadline);
        Assert.Equal("atproto_settings/contest_deadline", vm.ContestDeadline);
    }

    /// <summary>`None` deadline is offered for two distinct reasons (an unparseable
    /// directory timestamp, or a terminal state) — either way it must not render.</summary>
    [Fact]
    public async Task No_deadline_renders_when_the_machine_omits_one()
    {
        var (vm, _) = await LoadedAsync(Snapshot(contest: new ContestCardRow(
            "window-closed",
            Text("atproto_settings/contest_detail_window_closed"),
            null,
            false)));

        Assert.False(vm.ShowContestDeadline);
        Assert.Equal(string.Empty, vm.ContestDeadline);
    }

    /// <summary>Decision 2: `atproto-contest` renders ONLY when the machine says
    /// `show_contest` — never inferred from <c>state</c>, since `not-contestable`
    /// covers two distinct un-buttonable reasons (a genesis violation and an
    /// unauthenticated log) that must share no dead button.</summary>
    [Theory]
    [InlineData("contestable", true)]
    [InlineData("not-contestable", false)]
    [InlineData("window-closed", false)]
    public async Task Contest_button_follows_show_contest_not_state(string state, bool showContest)
    {
        var (vm, _) = await LoadedAsync(Snapshot(contest: new ContestCardRow(
            state, Text("atproto_settings/contest_detail_genesis"), null, showContest)));

        Assert.Equal(showContest, vm.ShowContestButton);
    }

    /// <summary>The confirm card's lines are the machine's composed copy — what is
    /// being undone, what this device signs, what the directory rules on — rendered
    /// verbatim and in order, the same rule the transition card follows below.</summary>
    [Fact]
    public async Task Contest_confirm_card_lines_render_verbatim_and_in_order()
    {
        var lines = new[]
        {
            Text("atproto_settings/contest_confirm_undo"),
            Text("atproto_settings/contest_confirm_signs"),
            Text("atproto_settings/contest_confirm_directory_rules"),
        };
        var (vm, _) = await LoadedAsync(Snapshot(contestConfirm: new ContestConfirmCardModel(lines, false)));

        Assert.True(vm.ShowContestConfirmCard);
        Assert.Equal(
            new[]
            {
                "atproto_settings/contest_confirm_undo",
                "atproto_settings/contest_confirm_signs",
                "atproto_settings/contest_confirm_directory_rules",
            },
            vm.ContestConfirmLines.ToArray());
    }

    [Fact]
    public async Task No_contest_confirm_card_renders_when_nothing_is_open()
    {
        var (vm, _) = await LoadedAsync(Snapshot(contestConfirm: null));

        Assert.False(vm.ShowContestConfirmCard);
        Assert.Empty(vm.ContestConfirmLines);
    }

    /// <summary>An in-flight submit disables the confirm control, so a second press
    /// cannot sign a second time.</summary>
    [Fact]
    public async Task Contest_confirm_actions_are_disabled_while_the_submit_is_in_flight()
    {
        var (vm, _) = await LoadedAsync(Snapshot(
            contestConfirm: new ContestConfirmCardModel(new[] { Text("k") }, true)));

        Assert.False(vm.ContestConfirmActionsEnabled);
    }

    /// <summary>Open/cancel are SYNC pure-local machine mutations — no wire kind.</summary>
    [Fact]
    public async Task OpenContestConfirm_and_CancelContest_forward_to_the_machine()
    {
        var (vm, m) = await LoadedAsync(Snapshot(contest: new ContestCardRow(
            "contestable", Text("k"), null, true)));

        vm.OpenContestConfirm();
        Assert.Contains("OpenContestConfirm", m.Calls);

        vm.CancelContest();
        Assert.Contains("CancelContest", m.Calls);
    }

    /// <summary>The converge is client-direct HTTPS to the public PLC directory —
    /// declares no wire kind, unlike every other button on this page — and re-renders
    /// from whatever the machine produced (non-optimistic, like every gesture here).</summary>
    [Fact]
    public async Task RequestContestAsync_forwards_to_the_machine_and_rerenders()
    {
        var (vm, m) = await LoadedAsync(Snapshot(
            contestConfirm: new ContestConfirmCardModel(new[] { Text("k") }, false)));
        m.OnRequestContest = () => Snapshot(contest: null, contestConfirm: null);

        await vm.RequestContestAsync();

        Assert.Contains("RequestContest", m.Calls);
        Assert.False(vm.ShowContestCard);
        Assert.False(vm.ShowContestConfirmCard);
    }

    // ── The transition card ─────────────────────────────────────────────────

    /// <summary>The card's copy is the machine's composed <c>TransitionPlan</c>,
    /// rendered VERBATIM and in order. A card promising something nest did not do is
    /// the one failure this confirm-before-anything-happens UX cannot absorb, so the
    /// VM must never re-author, reorder or drop a line.</summary>
    [Fact]
    public async Task Card_lines_render_verbatim_and_in_order()
    {
        var lines = new[] { Text("atproto_settings/card_unlink"), Text("atproto_settings/card_mint"), Text("atproto_settings/card_publish") };
        var (vm, _) = await LoadedAsync(Snapshot(
            pending: new TransitionCardModel("hosted_visible", lines, false, false)));

        Assert.True(vm.ShowCard);
        Assert.Equal(3, vm.CardLines.Count);
        // Same order the machine composed them in.
        Assert.Equal(
            new[] { "atproto_settings/card_unlink", "atproto_settings/card_mint", "atproto_settings/card_publish" },
            vm.CardLines.ToArray());
    }

    [Fact]
    public async Task No_card_renders_when_nothing_is_staged()
    {
        var (vm, _) = await LoadedAsync(Snapshot(pending: null));

        Assert.False(vm.ShowCard);
        Assert.Empty(vm.CardLines);
    }

    [Fact]
    public async Task The_backfill_optin_rides_the_card_that_declares_it()
    {
        var withBackfill = await LoadedAsync(Snapshot(
            pending: new TransitionCardModel("hosted_visible", new[] { Text("k") }, true, false),
            historyBackfill: true));
        Assert.True(withBackfill.Vm.ShowHistoryBackfill);
        Assert.True(withBackfill.Vm.HistoryBackfill);

        var without = await LoadedAsync(Snapshot(
            pending: new TransitionCardModel("linked", new[] { Text("k") }, false, false)));
        Assert.False(without.Vm.ShowHistoryBackfill);
    }

    /// <summary>An in-flight confirm disables both card actions, so a double-tap
    /// cannot stage a second transition over one already running.</summary>
    [Fact]
    public async Task Card_actions_are_disabled_while_the_confirm_is_in_flight()
    {
        var (vm, _) = await LoadedAsync(Snapshot(
            pending: new TransitionCardModel("hosted_visible", new[] { Text("k") }, false, true)));

        Assert.False(vm.CardActionsEnabled);
    }

    /// <summary>§ Transition semantics: Off→Linked is the ONE effect-free move — it
    /// applies on select with no card. The VM must not invent a card the machine did
    /// not stage (nor suppress one it did).</summary>
    [Fact]
    public async Task Off_to_linked_applies_without_a_card()
    {
        var (vm, m) = await LoadedAsync(Snapshot(level: "off"));
        m.OnSelect = target => Snapshot(level: target); // effect-free: level moves, nothing staged

        await vm.SelectLevelAsync("linked");

        Assert.Equal("linked", vm.Level);
        Assert.False(vm.ShowCard);
        Assert.Contains("SelectLevel:linked", m.Calls);
    }

    [Fact]
    public async Task A_staged_move_shows_the_card_and_leaves_the_level_alone()
    {
        var (vm, m) = await LoadedAsync(Snapshot(level: "off"));
        m.OnSelect = target => Snapshot(
            level: "off",
            pending: new TransitionCardModel(target, new[] { Text("atproto_settings/card_mint") }, true, false));

        await vm.SelectLevelAsync("hosted_visible");

        Assert.Equal("off", vm.Level);
        Assert.True(vm.ShowCard);
        Assert.Single(vm.CardLines);
    }

    // ── The per-level panels ────────────────────────────────────────────────

    [Fact]
    public async Task The_linked_panel_is_a_panel_of_level_linked()
    {
        Assert.False((await LoadedAsync(Snapshot(level: "off"))).Vm.ShowLinkedPanel);
        Assert.True((await LoadedAsync(Snapshot(level: "linked"))).Vm.ShowLinkedPanel);
        Assert.False((await LoadedAsync(Snapshot(level: "hosted_visible"))).Vm.ShowLinkedPanel);
    }

    [Fact]
    public async Task The_hosted_panel_renders_at_both_hosted_levels()
    {
        Assert.False((await LoadedAsync(Snapshot(level: "off"))).Vm.ShowHostedPanel);
        Assert.True((await LoadedAsync(Snapshot(level: "hosted_visible"))).Vm.ShowHostedPanel);
        Assert.True((await LoadedAsync(Snapshot(level: "hosted_full"))).Vm.ShowHostedPanel);
    }

    /// <summary>The hosted panel must also render while a transition is STAGED toward
    /// a hosted level: the DID-method radio it contains is what the pending mint will
    /// use, so hiding it until after the confirm would make the choice unreachable.</summary>
    [Fact]
    public async Task The_hosted_panel_renders_while_staged_toward_hosted()
    {
        UseRealStrings();
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "off",
            showDidMethodRadio: true,
            handlePreview: "alice.example.com",
            pending: new TransitionCardModel("hosted_visible", new[] { Text("k") }, true, false)));

        Assert.True(vm.ShowHostedPanel);
        Assert.True(vm.ShowDidMethodRadio);
        Assert.Contains("alice.example.com", vm.HandlePreview);
    }

    /// <summary>After the mint the method is a FACT, displayed not chosen — switching
    /// methods would be a new identity, not a setting.</summary>
    [Fact]
    public async Task The_did_method_radio_disappears_once_an_identity_exists()
    {
        UseRealStrings();
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "hosted_visible",
            showDidMethodRadio: false,
            identity: new IdentitySummaryRow("alice.example.com", "plc", "active")));

        Assert.False(vm.ShowDidMethodRadio);
        Assert.True(vm.ShowHostedHandle);
        Assert.Contains("alice.example.com", vm.HostedHandle);
    }

    /// <summary>The identity summary stays visible for a DEACTIVATED identity, so the
    /// user can see what re-enabling would restore.</summary>
    [Fact]
    public async Task A_deactivated_identity_is_still_summarised()
    {
        UseRealStrings();
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "hosted_visible",
            identity: new IdentitySummaryRow("alice.example.com", "plc", "deactivated")));

        Assert.True(vm.ShowHostedHandle);
        Assert.Contains("alice.example.com", vm.HostedHandle);
    }

    /// <summary>The identity summary is gated on the IDENTITY, not on the level
    /// (ui/atproto.md § Errors &amp; edge cases: "A deactivated identity at level
    /// Off/Linked: the identity summary renders... so the user can see what
    /// re-enabling restores"). At level Off/Linked the hosted panel itself is
    /// collapsed, so ANDing the summary with the panel's own visibility — the bug
    /// this pins — made the rule impossible to satisfy: exactly the two states it
    /// names are the ones the AND excludes.</summary>
    [Fact]
    public async Task The_identity_summary_renders_at_level_off_when_a_deactivated_identity_exists()
    {
        UseRealStrings();
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "off",
            identity: new IdentitySummaryRow("alice.example.com", "plc", "deactivated")));

        Assert.False(vm.ShowHostedPanel);
        Assert.True(vm.ShowHostedHandle);
        Assert.Contains("alice.example.com", vm.HostedHandle);
    }

    /// <summary>The two terminal statuses. <c>deleted</c> is reachable from a SECOND
    /// DEVICE the moment any one app can run the delete ceremony — without these
    /// arms the summary painted the raw wire word.</summary>
    [Fact]
    public async Task The_two_terminal_identity_statuses_are_labelled()
    {
        UseRealStrings();
        var deleted = await LoadedAsync(Snapshot(
            level: "off", identity: new IdentitySummaryRow("alice.example.com", "plc", "deleted")));
        var tombstoned = await LoadedAsync(Snapshot(
            level: "off", identity: new IdentitySummaryRow("alice.example.com", "plc", "tombstoned")));

        Assert.Contains("Deleted", deleted.Vm.HostedHandle);
        Assert.Contains("Permanently retired", tombstoned.Vm.HostedHandle);
    }

    /// <summary>The raw DID string is never shown — only the derived handle
    /// (§ Layout &amp; flow item 3).</summary>
    [Fact]
    public async Task The_summary_never_shows_a_raw_did()
    {
        UseRealStrings();
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "hosted_visible",
            identity: new IdentitySummaryRow("alice.example.com", "plc", "active")));

        Assert.DoesNotContain("did:", vm.HostedHandle, StringComparison.OrdinalIgnoreCase);
    }

    // ── Delete presence + the full-PDS panel ────────────────────────────────

    /// <summary>Renders whenever a hosted identity exists — active OR deactivated —
    /// so the stronger destructive action stays reachable after a step-down.</summary>
    [Fact]
    public async Task Delete_presence_follows_the_machines_flag_not_the_level()
    {
        Assert.False((await LoadedAsync(Snapshot(level: "off", showDeletePresence: false))).Vm.ShowDeletePresence);
        // Stepped down to Off, but the identity still exists → still offered.
        Assert.True((await LoadedAsync(Snapshot(level: "off", showDeletePresence: true))).Vm.ShowDeletePresence);
    }

    /// <summary>Its own confirm card — never the depth selector's. The copy is the
    /// machine's, rendered verbatim, the same rule the transition/contest cards
    /// follow.</summary>
    [Fact]
    public async Task Delete_confirm_card_lines_render_verbatim_and_in_order()
    {
        var lines = new[]
        {
            Text("atproto_settings/delete_confirm_destroys"),
            Text("atproto_settings/delete_confirm_identity_survives"),
            Text("atproto_settings/delete_confirm_level_lands"),
        };
        var (vm, _) = await LoadedAsync(Snapshot(deleteConfirm: new DeleteConfirmCardModel(lines, false, NoRetire())));

        Assert.True(vm.ShowDeleteConfirmCard);
        Assert.Equal(
            new[]
            {
                "atproto_settings/delete_confirm_destroys",
                "atproto_settings/delete_confirm_identity_survives",
                "atproto_settings/delete_confirm_level_lands",
            },
            vm.DeleteConfirmLines.ToArray());
    }

    [Fact]
    public async Task No_delete_confirm_card_renders_when_nothing_is_open()
    {
        var (vm, _) = await LoadedAsync(Snapshot(deleteConfirm: null));

        Assert.False(vm.ShowDeleteConfirmCard);
        Assert.Empty(vm.DeleteConfirmLines);
    }

    /// <summary>An in-flight sweep disables the confirm control, so a second press
    /// cannot send a second sweep.</summary>
    [Fact]
    public async Task Delete_confirm_actions_are_disabled_while_the_sweep_is_in_flight()
    {
        var (vm, _) = await LoadedAsync(Snapshot(
            deleteConfirm: new DeleteConfirmCardModel(new[] { Text("k") }, true, NoRetire())));

        Assert.False(vm.DeleteConfirmActionsEnabled);
    }

    /// <summary>Open/cancel are SYNC pure-local machine mutations — no wire kind.</summary>
    [Fact]
    public async Task OpenDeleteConfirm_and_CancelDelete_forward_to_the_machine()
    {
        var (vm, m) = await LoadedAsync(Snapshot(showDeletePresence: true));

        vm.OpenDeleteConfirm();
        Assert.Contains("OpenDeleteConfirm", m.Calls);

        vm.CancelDelete();
        Assert.Contains("CancelDelete", m.Calls);
    }

    /// <summary>The one network round trip in the ceremony —
    /// <c>fauna.bridges.atproto.delete_presence</c> — and re-renders from whatever
    /// the machine produced (non-optimistic, like every gesture on this page).</summary>
    [Fact]
    public async Task ConfirmDeleteAsync_forwards_to_the_machine_and_rerenders()
    {
        var (vm, m) = await LoadedAsync(Snapshot(
            showDeletePresence: true,
            deleteConfirm: new DeleteConfirmCardModel(new[] { Text("k") }, false, NoRetire())));
        m.OnConfirmDelete = () => Snapshot(showDeletePresence: false, deleteConfirm: null);

        await vm.ConfirmDeleteAsync();

        Assert.Contains("ConfirmDelete", m.Calls);
        Assert.False(vm.ShowDeleteConfirmCard);
        Assert.False(vm.ShowDeletePresence);
    }

    /// <summary>The F1 login-plane controls are the DEEPEST level's panel, not
    /// standalone settings — every other app gates them on <c>hosted_full</c>, and
    /// windows must not be the one that renders them unconditionally.</summary>
    [Fact]
    public async Task The_full_pds_panel_renders_only_at_hosted_full()
    {
        Assert.False((await LoadedAsync(Snapshot(level: "off"))).Vm.ShowFullPds);
        Assert.False((await LoadedAsync(Snapshot(level: "linked"))).Vm.ShowFullPds);
        Assert.False((await LoadedAsync(Snapshot(level: "hosted_visible"))).Vm.ShowFullPds);
        Assert.True((await LoadedAsync(Snapshot(level: "hosted_full"))).Vm.ShowFullPds);
    }

    [Fact]
    public async Task The_kill_switch_state_attr_mirrors_the_snapshot()
    {
        Assert.Equal("on", (await LoadedAsync(Snapshot(externalAppsEnabled: true))).Vm.ExternalAppsState);
        Assert.Equal("off", (await LoadedAsync(Snapshot(externalAppsEnabled: false))).Vm.ExternalAppsState);
    }

    // ── Gestures are forwarded, never simulated ─────────────────────────────

    [Fact]
    public async Task Gestures_forward_to_the_machine()
    {
        var (vm, m) = await LoadedAsync(Snapshot());

        vm.CancelTransition();
        vm.SetDidMethod("web");
        vm.SetHistoryBackfill(true);
        await vm.ConfirmTransitionAsync();
        await vm.SetExternalAppsEnabledAsync(false);

        Assert.Contains("CancelTransition", m.Calls);
        Assert.Contains("SetDidMethod:web", m.Calls);
        Assert.Contains("SetHistoryBackfill:True", m.Calls);
        Assert.Contains("ConfirmTransition", m.Calls);
        Assert.Contains("SetExternalAppsEnabled:False", m.Calls);
    }

    /// <summary>A refused gesture surfaces on the page's error banner rather than
    /// being silently dropped (testing.md rule 11) — the machine puts the reason on
    /// the snapshot and the VM must show it.</summary>
    [Fact]
    public async Task A_machine_error_reaches_the_error_banner()
    {
        var snap = Snapshot() with { @error = Text("atproto_settings/error_gated") };
        var (vm, _) = await LoadedAsync(snap);

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
    }

    /// <summary>The element-id table the e2e derives its ids from
    /// (<c>actions/atproto_settings.py::_depth_id</c> maps a level to an id by
    /// <c>_</c>→<c>-</c>). Pinned against the shared catalog directly — windows no
    /// longer carries its own copy of this table (atproto.md § Where logic lives
    /// → *The rung catalog*) — so a drift here is the catalog's, not windows'.</summary>
    [Fact]
    public void The_depth_level_table_matches_the_e2e_id_derivation()
    {
        var rungs = FaunaAtprotoSettingsMachineMethods.DepthLevelOptions();
        Assert.Equal(4, rungs.Length);
        foreach (var rung in rungs)
        {
            Assert.Equal($"atproto-depth-{rung.@level.Replace('_', '-')}", rung.@uiId);
        }
        Assert.Equal(
            new[] { "off", "linked", "hosted_visible", "hosted_full" },
            rungs.Select(r => r.@level).ToArray());
    }

    // ── F1 credential reveal (atproto-app-credential-reveal) ─────────────────

    /// <summary>Before reveal, the button shows the localized label and is
    /// enabled; the row carries no secret of its own yet.</summary>
    [Fact]
    public async Task An_unrevealed_row_shows_the_reveal_label_and_stays_enabled()
    {
        UseRealStrings();
        var (vm, _) = await LoadedAsync(Snapshot(
            level: "hosted_full",
            credentials: new[] { Credential(credentialId: "ivory", revealable: true) }));

        Assert.Single(vm.Credentials);
        Assert.Equal("Reveal", vm.Credentials[0].RevealText);
        Assert.True(vm.Credentials[0].CanReveal);
    }

    /// <summary>The F1 get_text contract: <c>atproto-app-credential-reveal</c>'s
    /// own text becomes the secret once revealed, and the row disables — there is
    /// no separate secret-display id (linux <c>bluesky.rs</c> / web
    /// <c>AtprotoSettingsSection.svelte</c> precedent).</summary>
    [Fact]
    public async Task Revealing_puts_the_secret_on_the_rows_own_text_and_disables_it()
    {
        UseRealStrings();
        var (vm, m) = await LoadedAsync(Snapshot(
            level: "hosted_full",
            credentials: new[] { Credential(credentialId: "ivory", revealable: true) }));
        m.OnRevealSecret = id => id == "ivory" ? "s3cr3t-value" : throw new Exception("wrong id");

        await vm.RevealSecretAsync("ivory");

        Assert.Contains("RevealSecret:ivory", m.Calls);
        Assert.Single(vm.Credentials);
        Assert.Equal("s3cr3t-value", vm.Credentials[0].RevealText);
        Assert.False(vm.Credentials[0].CanReveal);
    }

    /// <summary>A second reveal click on an already-revealed row must not re-ask
    /// the machine (mirrors web's <c>revealCredential</c> guard) — the row stays
    /// showing what it already showed.</summary>
    [Fact]
    public async Task Revealing_twice_only_asks_the_machine_once()
    {
        UseRealStrings();
        var (vm, m) = await LoadedAsync(Snapshot(
            level: "hosted_full",
            credentials: new[] { Credential(credentialId: "ivory", revealable: true) }));
        m.OnRevealSecret = _ => "s3cr3t-value";

        await vm.RevealSecretAsync("ivory");
        await vm.RevealSecretAsync("ivory");

        Assert.Single(m.Calls, c => c.StartsWith("RevealSecret:"));
        Assert.Equal("s3cr3t-value", vm.Credentials[0].RevealText);
    }

    /// <summary>Minting attributes the returned secret to the freshly created row
    /// (diff the credential-id set before/after mint — linux's mint handler / web's
    /// <c>mintCredential</c> pattern) and shows it there immediately, matching
    /// "the row appears with the secret revealed inline" (rule 1) with no separate
    /// one-time-reveal surface.</summary>
    [Fact]
    public async Task Minting_shows_the_new_secret_on_the_new_rows_own_text()
    {
        UseRealStrings();
        var (vm, m) = await LoadedAsync(Snapshot(level: "hosted_full", credentials: Array.Empty<AppCredentialRow>()));
        m.OnMint = (label, dmAllowed) => (
            Snapshot(level: "hosted_full", credentials: new[] { Credential(credentialId: "new-cred", label: label, revealable: true) }),
            "fresh-secret");

        await vm.MintCredentialAsync("Ivory");

        Assert.Contains("Mint:Ivory:False", m.Calls);
        Assert.Single(vm.Credentials);
        Assert.Equal("fresh-secret", vm.Credentials[0].RevealText);
        Assert.False(vm.Credentials[0].CanReveal);
    }

    /// <summary>Minting a SECOND credential must not disturb the first row's
    /// already-revealed secret — the revealed-secrets map is keyed by credential
    /// id, not by mint order.</summary>
    [Fact]
    public async Task Minting_a_second_credential_does_not_disturb_the_firsts_revealed_secret()
    {
        UseRealStrings();
        var (vm, m) = await LoadedAsync(Snapshot(
            level: "hosted_full",
            credentials: new[] { Credential(credentialId: "first", revealable: true) }));
        m.OnRevealSecret = _ => "first-secret";
        await vm.RevealSecretAsync("first");

        m.OnMint = (label, dmAllowed) => (
            Snapshot(level: "hosted_full", credentials: new[]
            {
                Credential(credentialId: "first", revealable: true),
                Credential(credentialId: "second", label: label, revealable: true),
            }),
            "second-secret");
        await vm.MintCredentialAsync("Graysky");

        Assert.Equal(2, vm.Credentials.Count);
        Assert.Equal("first-secret", vm.Credentials.Single(c => c.CredentialId == "first").RevealText);
        Assert.Equal("second-secret", vm.Credentials.Single(c => c.CredentialId == "second").RevealText);
    }
}
