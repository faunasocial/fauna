using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The <c>family</c> page (family-safety.md § App surface) — guardian
/// section (wards + ONE reach-policy editor for the currently selected ward +
/// the approvals queue across ALL wards + contact-add + graduate) and
/// supervised section (guardian handle + a read-only policy summary). Over
/// the shared <see cref="INestRpcClient"/> seam → <c>FfiFamilyClient</c>
/// (mirrors <see cref="AdminUsersViewModel"/>'s shape). ui.yaml's
/// <c>family:</c> block has exactly one (non-indexed) policy-editor element
/// set, so a guardian with multiple wards edits one at a time — selecting a
/// <c>family-ward-item</c> row loads that ward's policy into the shared
/// editor; the approvals queue is NOT ward-scoped (matches
/// <c>fauna.family.approvals.list</c>, which returns every ward's queue).
/// </summary>
public partial class FamilyViewModel : ObservableObject
{
    private readonly INestRpcClient _rpc;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _actionError;

    // ── Supervised side (family-guardian-handle / family-policy-summary) ──
    // Both null when this account isn't supervised.
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GuardianHandleLine))]
    private string? _guardianHandle;
    [ObservableProperty] private string? _policySummary;
    /// <summary>The caller's OWN band + how it was established
    /// (<c>family-age-band-summary</c>, family-safety.md § App surface → *Age-band
    /// surfaces*) — the shared <c>age_band_line</c> with <c>own = true</c>; <c>null</c>
    /// (absent, never placeholdered) when there is no band or it is unnameable.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasAgeBandSummary))]
    private string? _ageBandSummary;

    public bool HasAgeBandSummary => AgeBandSummary is not null;

    /// <summary>"Supervised by {guardian}" for <c>family-guardian-handle</c> —
    /// empty when this account isn't supervised.</summary>
    public string GuardianHandleLine
        => GuardianHandle is { } h ? Strings.Get("family/guardian_label").Replace("{guardian}", h) : "";

    // ── Guardian side ───────────────────────────────────────────────────
    /// <summary>The wards this account guards (<c>family-ward-item</c>, indexed).</summary>
    public ObservableCollection<FamilyWardRow> Wards { get; } = new();
    /// <summary>The reach-approval queue across ALL wards (<c>family-approval-item</c>, indexed).</summary>
    public ObservableCollection<FamilyApprovalRow> Approvals { get; } = new();
    /// <summary>The selected ward's registered devices (<c>family-device-mark-item</c>,
    /// indexed — family-safety.md § Full visibility for young children / Slice F).
    /// Cleared and refilled by <see cref="LoadPolicyFromWard"/> on every ward
    /// load/switch, same shape as <see cref="Wards"/>/<see cref="Approvals"/>.</summary>
    public ObservableCollection<FamilyDeviceRow> Devices { get; } = new();

    /// <summary>The ward currently loaded into the shared policy editor —
    /// <c>null</c> until a <see cref="SelectWard"/> call (auto-selects the
    /// first ward on load).</summary>
    [ObservableProperty] private byte[]? _selectedWardActorId;
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GraduateConfirmButtonText))]
    [NotifyPropertyChangedFor(nameof(WardDevicesHint))]
    private string? _selectedWardHandle;

    /// <summary>"Yes, graduate {handle}" — resolved here (named-placeholder
    /// substitution is per-app, i18n_placeholder_named_not_numeric).</summary>
    public string GraduateConfirmButtonText
        => Strings.Get("family/graduate_confirm_button").Replace("{handle}", SelectedWardHandle ?? "");

    /// <summary>"Mark the device you enrolled into this account. {handle} cannot
    /// remove a marked device…" (<c>family/ward_devices_hint</c>) — same
    /// named-placeholder resolution as <see cref="GraduateConfirmButtonText"/>.</summary>
    public string WardDevicesHint
        => Strings.Get("family/ward_devices_hint").Replace("{handle}", SelectedWardHandle ?? "");
    [ObservableProperty] private bool _policyContactApproval;
    [ObservableProperty] private string _policyUnknownSenderMail = "allow";
    [ObservableProperty] private bool _policyFederationContact = true;
    [ObservableProperty] private string _policyFeedSources = "allow";
    // ── The bridge-DM gate (family-safety.md § The bridge-DM gate) ───────
    [ObservableProperty] private string _policyUnknownPeerDm = "allow";
    /// <summary>Whether the guardian has touched <see cref="PolicyUnknownPeerDm"/>
    /// since the editor last loaded a ward — the ONE reach knob gated this way:
    /// unlike its siblings it rides <c>fauna.family.policy.update</c> as
    /// <c>Option&lt;String&gt;</c> where absent means "leave unchanged" (§
    /// Policy-update compatibility), so an unrelated save must not silently
    /// rewrite it to whatever this render happened to show. Reset on every
    /// <see cref="LoadPolicyFromWard"/> (a fresh load or a ward switch both go
    /// through it), set true only by a genuine edit (<c>FamilyPage.xaml.cs</c>'s
    /// <c>UnknownPeerDmSelect_SelectionChanged</c>) — mirrors linux's
    /// <c>unknown_peer_dm_edited</c>.</summary>
    [ObservableProperty] private bool _policyUnknownPeerDmEdited;

    // ── Content policy (family-safety.md § Content policy) ───────────────
    // The guardian's per-category render floor over the four negative canonical
    // categories, each `inherit` | `collapse` | `block` from the shared
    // ContentFloorOptions() catalog. `inherit` is the default — the ward's own
    // preferences decide, which is also what an ABSENT wire content_policy
    // means. Plus the Guardian Notify knob (§ Guardian Notify): category +
    // count only, never content.
    [ObservableProperty] private string _policyContentNsfw = "inherit";
    [ObservableProperty] private string _policyContentSpam = "inherit";
    [ObservableProperty] private string _policyContentPhishing = "inherit";
    [ObservableProperty] private string _policyContentCommercial = "inherit";
    [ObservableProperty] private bool _policyContentNotify;

    // ── Screen time (family-safety.md § Screen time) ─────────────────────
    // Typed HH:MM / whole minutes (never raw local-minutes) — parsed through
    // the shared fauna_core::screen_time parser at Save time
    // (SavePolicyAsync), never re-implemented here. An empty field is how a
    // guardian CLEARS a limit, which is why these are text, not a picker
    // (a picker has no empty position).
    [ObservableProperty] private string _policyScreenWindowStart = string.Empty;
    [ObservableProperty] private string _policyScreenWindowEnd = string.Empty;
    [ObservableProperty] private string _policyScreenDailyMinutes = string.Empty;

    [ObservableProperty] private string _contactAddInput = string.Empty;

    /// <summary>Whether the graduate confirm step is showing (reveal-then-confirm,
    /// the established windows convention — <c>family-graduate-button</c> reveals
    /// <c>family-graduate-confirm-button</c>).</summary>
    [ObservableProperty] private bool _isConfirmingGraduate;

    // ── Transfer consent handshake (family-safety.md § Graduation & transfer) ──

    [ObservableProperty] private string _transferInput = string.Empty;

    /// <summary>The selected ward's outstanding transfer proposal target, or
    /// <c>null</c> when none — swaps <c>family-transfer-input</c>/<c>-button</c>
    /// for <c>family-transfer-pending</c>/<c>-cancel-button</c>.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(HasPendingTransfer))]
    [NotifyPropertyChangedFor(nameof(TransferPendingText))]
    private string? _pendingTransferGuardianHandle;

    public bool HasPendingTransfer => PendingTransferGuardianHandle is not null;

    /// <summary>"Waiting for {handle} to accept guardianship" (<c>family-transfer-pending</c>).</summary>
    public string TransferPendingText
        => Strings.Get("family/transfer_pending").Replace("{handle}", PendingTransferGuardianHandle ?? "");

    /// <summary>Incoming transfer proposals naming THIS account as proposed guardian
    /// (<c>family-incoming-transfer-item</c>, indexed) — independent of both roles above;
    /// any user can be a proposed guardian, which is what widens the <c>family-tab</c> gate.</summary>
    public ObservableCollection<FamilyIncomingTransferRow> IncomingTransfers { get; } = new();

    internal FamilyViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    /// <summary>Load <c>fauna.family.status</c> (both sides) + the approvals
    /// queue. Re-selecting the previously-selected ward if it's still present,
    /// else auto-selecting the first ward.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ActionError = null;
        try
        {
            var status = await _rpc.FamilyStatusAsync();
            GuardianHandle = status.@supervisedBy?.@handle;
            AgeBandSummary = FamilyWardRow.AgeBandLine(status.@ageBand, own: true);
            // fauna_core::format::reach_policy_summary (family-safety.md § Where
            // logic lives) — the read-only policy-summary line set; may carry MORE
            // than the historical 4 lines (v1.x knobs like unknown_peer_dm when
            // present), which is a strict improvement, not a regression.
            if (status.@policy is { } p)
            {
                var lines = FaunaFfiMethods.ReachPolicySummary(p)
                    .Select(l => $"{Strings.Resolve(l.label)}: {Strings.Resolve(l.value)}")
                    .ToList();
                // Screen time (§ Screen time — the transparency rule): the ward's
                // own usage figure, folded into this same read-only summary rather
                // than a new ui.yaml ID ("usage is a line of the active policy,
                // read-only"). Present ONLY under a daily budget — the wire's
                // usage_today_minutes already encodes that gate (None = no budget).
                if (status.@usageTodayMinutes is { } used)
                {
                    var usageLine = FaunaFfiMethods.UsageTodayLine(used, p.@screenTime?.@dailyMinutes);
                    lines.Add($"{Strings.Resolve(usageLine.label)}: {Strings.Resolve(usageLine.value)}");
                }
                PolicySummary = string.Join("\n", lines);
            }
            else
            {
                PolicySummary = null;
            }

            Wards.Clear();
            foreach (var w in status.@wards) Wards.Add(FamilyWardRow.From(w));

            IncomingTransfers.Clear();
            foreach (var t in status.@incomingTransfers) IncomingTransfers.Add(FamilyIncomingTransferRow.From(t));

            var stillSelected = SelectedWardActorId is { } id
                ? Wards.FirstOrDefault(w => w.ActorId.AsSpan().SequenceEqual(id))
                : null;
            if (stillSelected is not null) LoadPolicyFromWard(stillSelected);
            else if (Wards.Count > 0) LoadPolicyFromWard(Wards[0]);
            else ClearSelectedWard();

            await ReloadApprovalsAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    private async Task ReloadApprovalsAsync()
    {
        var approvals = await _rpc.FamilyApprovalsListAsync();
        Approvals.Clear();
        foreach (var a in approvals) Approvals.Add(FamilyApprovalRow.From(a));
    }

    /// <summary>Select a ward row — loads its policy into the shared editor.</summary>
    public void SelectWard(FamilyWardRow ward) => LoadPolicyFromWard(ward);

    private void LoadPolicyFromWard(FamilyWardRow ward)
    {
        // SelectedWardActorId is set LAST, deliberately: its PropertyChanged
        // always fires (a fresh byte[] per ward row, so the [ObservableProperty]
        // equality guard — reference equality for arrays — never treats it as a
        // no-op change), unlike the Policy* properties below, which are
        // skipped whenever the newly-loaded value happens to equal what's
        // already set (a fresh page's compile-time default, or the
        // previously-selected ward's value). The platform shell uses
        // SelectedWardActorId's change as the reliable "this ward's policy
        // fields are now all set" signal to force a full editor resync rather
        // than trust each field's own (unreliable-on-a-no-op-value)
        // notification — see FamilyPage.xaml.cs's SelectedWardActorId case.
        PolicyContactApproval = ward.ContactApproval;
        PolicyUnknownSenderMail = NormalizeUnknownSenderWire(ward.UnknownSenderMail);
        PolicyFederationContact = ward.FederationContact;
        PolicyFeedSources = NormalizeFeedSourcesWire(ward.FeedSources);
        PolicyUnknownPeerDm = NormalizeUnknownPeerDmWire(ward.UnknownPeerDm);
        PolicyUnknownPeerDmEdited = false;
        // Same fail-closed normalization as the reach knobs above — a floor value
        // this build cannot name renders (and re-saves) as `block`, never the
        // permissive `inherit` (family-safety.md § Content policy).
        PolicyContentNsfw = NormalizeContentFloorWire(ward.ContentNsfw);
        PolicyContentSpam = NormalizeContentFloorWire(ward.ContentSpam);
        PolicyContentPhishing = NormalizeContentFloorWire(ward.ContentPhishing);
        PolicyContentCommercial = NormalizeContentFloorWire(ward.ContentCommercial);
        PolicyContentNotify = ward.ContentNotify;
        // Screen time: minutes-from-midnight -> "HH:MM" through the SAME
        // shared formatter the parser at Save time inverts — never a local
        // reading of the number (family-safety.md § Screen time). Empty
        // means "no window"/"no budget", exactly as the guardian's own empty
        // input meant when they last saved.
        PolicyScreenWindowStart = ward.ScreenWindowStart is { } ws ? FaunaFfiMethods.FormatTimeOfDay(ws) : string.Empty;
        PolicyScreenWindowEnd = ward.ScreenWindowEnd is { } we ? FaunaFfiMethods.FormatTimeOfDay(we) : string.Empty;
        PolicyScreenDailyMinutes = ward.ScreenDailyMinutes is { } dm ? dm.ToString() : string.Empty;
        PendingTransferGuardianHandle = ward.PendingTransferGuardianHandle;
        TransferInput = string.Empty;
        IsConfirmingGraduate = false;
        // Devices: a full clear+refill, same as Wards/Approvals on every LoadAsync
        // — an ObservableCollection's Clear/Add always raise real notifications
        // regardless of value equality, so (unlike the scalar Policy* properties
        // above) this needs no separate "unconditional resync" case in the page's
        // PropertyChanged handler.
        Devices.Clear();
        foreach (var d in ward.Devices) Devices.Add(d);
        SelectedWardHandle = ward.Handle;
        SelectedWardActorId = ward.ActorId;
    }

    private void ClearSelectedWard()
    {
        SelectedWardActorId = null;
        SelectedWardHandle = null;
        PendingTransferGuardianHandle = null;
        TransferInput = string.Empty;
        IsConfirmingGraduate = false;
        Devices.Clear();
    }

    /// <summary><c>fauna.family.device.mark</c> for one of the selected ward's
    /// devices — its own per-device RPC, fired immediately (never batched behind
    /// <see cref="SavePolicyAsync"/>). <paramref name="row"/> carries the device
    /// id BY VALUE (not a row index): a concurrent refetch that reorders the
    /// ward's devices must never route a flip at the wrong one — this flag is a
    /// security promise ("the child cannot remove the guardian's device").
    /// Returns whether the RPC succeeded, so the page can revert the toggle's
    /// visual state on failure (this method updates <see
    /// cref="FamilyDeviceRow.GuardianMarked"/> only on success).</summary>
    public async Task<bool> DeviceMarkAsync(FamilyDeviceRow row, bool marked)
    {
        if (SelectedWardActorId is not { } wardId) return false;
        ActionError = null;
        try
        {
            await _rpc.FamilyDeviceMarkAsync(wardId, row.DeviceId, marked);
            row.GuardianMarked = marked;
            return true;
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
            return false;
        }
    }

    /// <summary><c>fauna.family.policy.update</c> for the selected ward, then refetch.</summary>
    public async Task SavePolicyAsync()
    {
        if (SelectedWardActorId is not { } id) return;
        ActionError = null;
        // Screen time (§ Screen time): typed "HH:MM"/whole-minutes text, parsed
        // through the shared fauna_core::screen_time parser BEFORE the RPC — a
        // syntax error ("9:5", "24:00") surfaces on error-message locally
        // instead of as a generic transport failure. The nest's own
        // ScreenTimePolicy::validate then catches the semantic refusals
        // (half-set bounds, start == end, out-of-range budget) via the normal
        // catch below — this client pre-validates only the "HH:MM"/digits shape,
        // never re-implementing the nest's own rules.
        ushort? screenWindowStart, screenWindowEnd, screenDailyMinutes;
        try
        {
            screenWindowStart = FaunaFfiMethods.ParseTimeOfDay(PolicyScreenWindowStart.Trim());
            screenWindowEnd = FaunaFfiMethods.ParseTimeOfDay(PolicyScreenWindowEnd.Trim());
            screenDailyMinutes = FaunaFfiMethods.ParseDailyMinutes(PolicyScreenDailyMinutes.Trim());
        }
        catch (FfiException.General g)
        {
            ActionError = g.msg;
            return;
        }
        try
        {
            // content_policy + content_notify + screen_time are PRESENT: this
            // editor builds all three pillars, so it sends them (replace
            // semantics — an all-`inherit`/all-empty policy is a valid no-op,
            // and an all-empty screen_time really does clear every limit; "None
            // = leave unchanged" would make an all-empty save unable to clear
            // one). unknownPeerDm (the bridge-DM gate) is the one field that IS
            // "None = leave unchanged" (§ Policy-update compatibility) — sent
            // only once PolicyUnknownPeerDmEdited says the guardian actually
            // touched the select this session, never merely because this render
            // happens to show a value.
            var policy = new FfiReachPolicy(
                PolicyContactApproval, PolicyUnknownSenderMail, PolicyFederationContact, PolicyFeedSources,
                new FfiContentPolicy(PolicyContentNsfw, PolicyContentSpam, PolicyContentPhishing, PolicyContentCommercial),
                new FfiScreenTimePolicy(screenWindowStart, screenWindowEnd, screenDailyMinutes),
                PolicyContentNotify, PolicyUnknownPeerDmEdited ? PolicyUnknownPeerDm : null);
            await _rpc.FamilyPolicyUpdateAsync(id, policy);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary><c>fauna.family.approvals.decide</c> — approve/deny one queue item.</summary>
    public async Task DecideApprovalAsync(FamilyApprovalRow row, bool approve)
    {
        ActionError = null;
        try
        {
            // A `feed_source` item's key; empty for every other kind, just as
            // PeerActorId/MessageId are empty for the kinds they do not name.
            // PeerAddress is a `dm_hold`'s key (with BridgeId) — an external DM
            // peer is not an actor on this nest.
            await _rpc.FamilyApprovalsDecideAsync(row.SupervisedActorId, row.Kind, row.PeerActorId, row.MessageId, row.BridgeId, row.Operation, row.Target, row.PeerAddress, approve);
            await ReloadApprovalsAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary><c>fauna.family.contact.add</c> — pre-approve a contact for the
    /// selected ward. v1: <see cref="ContactAddInput"/> is a hex actor id (handle
    /// resolution is a future UX polish, not part of this seam).</summary>
    [RelayCommand]
    private async Task ContactAddAsync()
    {
        if (SelectedWardActorId is not { } id) return;
        var input = ContactAddInput.Trim();
        if (input.Length == 0) return;
        ActionError = null;
        try
        {
            var peerActorId = Convert.FromHexString(input);
            await _rpc.FamilyContactAddAsync(id, peerActorId);
            ContactAddInput = string.Empty;
        }
        catch (FormatException)
        {
            ActionError = Strings.Get("family/contact_add_invalid_actor_id");
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary><c>fauna.family.transfer</c> — propose a new guardian for the
    /// selected ward (pending until accepted). Same hex actor-id convention
    /// (and error) as <see cref="ContactAddAsync"/>.</summary>
    [RelayCommand]
    private async Task TransferAsync()
    {
        if (SelectedWardActorId is not { } id) return;
        var input = TransferInput.Trim();
        if (input.Length == 0) return;
        ActionError = null;
        try
        {
            var target = Convert.FromHexString(input);
            await _rpc.FamilyTransferAsync(id, target);
            TransferInput = string.Empty;
            await LoadAsync();
        }
        catch (FormatException)
        {
            ActionError = Strings.Get("family/contact_add_invalid_actor_id");
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Withdraw the selected ward's pending proposal (initiator side).</summary>
    [RelayCommand]
    private async Task TransferCancelAsync()
    {
        if (SelectedWardActorId is not { } id) return;
        ActionError = null;
        try
        {
            await _rpc.FamilyTransferCancelAsync(id);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Accept/decline an incoming proposal (proposed-guardian side).
    /// Accept re-points the link — the follow-up reload lands the ward in
    /// this account's own Wards list.</summary>
    public async Task DecideIncomingTransferAsync(FamilyIncomingTransferRow row, bool accept)
    {
        ActionError = null;
        try
        {
            if (accept) await _rpc.FamilyTransferAcceptAsync(row.SupervisedActorId);
            else await _rpc.FamilyTransferDeclineAsync(row.SupervisedActorId);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Reveal the graduate confirm step (<c>family-graduate-button</c>).</summary>
    [RelayCommand]
    private void BeginGraduate() => IsConfirmingGraduate = true;

    /// <summary><c>fauna.family.graduate</c> for the selected ward
    /// (<c>family-graduate-confirm-button</c>) — supervised → full account, in place.</summary>
    [RelayCommand]
    private async Task ConfirmGraduateAsync()
    {
        if (SelectedWardActorId is not { } id) return;
        ActionError = null;
        try
        {
            await _rpc.FamilyGraduateAsync(id);
            IsConfirmingGraduate = false;
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    // ── Fail-closed wire-value normalization ────────────────────────────
    // family-safety.md § "A knob value a client cannot parse renders
    // fail-closed": within a major version this client may be OLDER than its
    // nest, so the stored value may be one it cannot name. Rendering that as
    // "allow" would show the guardian a weaker policy than the one enforced —
    // and SavePolicyAsync writes the editor's state back, so the next save
    // would downgrade the ward's protection for real. Reverse-looked-up
    // through the shared FFI catalog/label functions rather than a local
    // fail-closed constant (family-safety.md § Where logic lives): the
    // catalog option whose label KEY matches the already-fail-closed resolved
    // label IS the fail-closed wire value, so this needs no local knowledge of
    // which option that is — a newer nest adding a value this client can't
    // parse degrades the same way a garbage string would (mirrors android's
    // normalizeUnknownSenderWire/normalizeFeedSourcesWire).

    internal static string NormalizeUnknownSenderWire(string wire)
    {
        var resolvedKey = FaunaFfiMethods.UnknownSenderLabel(wire).key;
        return FaunaFfiMethods.UnknownSenderOptions().FirstOrDefault(o => o.label.key == resolvedKey)?.value ?? wire;
    }

    internal static string NormalizeFeedSourcesWire(string wire)
    {
        var resolvedKey = FaunaFfiMethods.FeedSourcesLabel(wire).key;
        return FaunaFfiMethods.FeedSourcesOptions().FirstOrDefault(o => o.label.key == resolvedKey)?.value ?? wire;
    }

    /// <summary>The <c>Option&lt;String&gt;</c> twin of <see cref="NormalizeFeedSourcesWire"/>
    /// above — an ABSENT wire value folds to the <c>allow</c> DEFAULT first
    /// (family-safety.md § "An absent unknown_peer_dm renders its allow default,
    /// not the fail-closed value": the nest omits a knob sitting at its default,
    /// so absence is not "unparseable"), THEN a PRESENT-but-unrecognized value
    /// reverse-looks-up fail-closed exactly like every other reach knob (mirrors
    /// android's normalizeUnknownPeerDmWire).</summary>
    internal static string NormalizeUnknownPeerDmWire(string? wire)
    {
        if (wire is null) return "allow";
        var resolvedKey = FaunaFfiMethods.UnknownPeerDmLabel(wire).key;
        return FaunaFfiMethods.UnknownPeerDmOptions().FirstOrDefault(o => o.label.key == resolvedKey)?.value ?? wire;
    }

    /// <summary>The content-floor twin of the two above (family-safety.md
    /// § Content policy — <i>"a floor value the client cannot parse renders
    /// fail-closed (<c>block</c>)"</i>). Same reverse-lookup idiom, over
    /// <c>ContentFloorLabel</c>/<c>ContentFloorOptions</c>: no local knowledge of
    /// WHICH option is the fail-closed one, so a newer nest's unnameable floor
    /// degrades exactly as a garbage string does — and never as
    /// <c>inherit</c>, which would show the guardian a weaker policy than the one
    /// enforced and then persist that downgrade on the next save.</summary>
    internal static string NormalizeContentFloorWire(string wire)
    {
        var resolvedKey = FaunaFfiMethods.ContentFloorLabel(wire).key;
        return FaunaFfiMethods.ContentFloorOptions().FirstOrDefault(o => o.label.key == resolvedKey)?.value ?? wire;
    }
}

/// <summary>A ward row (<c>family-ward-item</c> / <c>family-ward-handle</c>) —
/// the supervised account + its current reach policy.</summary>
public sealed class FamilyWardRow
{
    public required byte[] ActorId { get; init; }
    public required string Handle { get; init; }
    public required bool ContactApproval { get; init; }
    public required string UnknownSenderMail { get; init; }
    public required bool FederationContact { get; init; }
    public required string FeedSources { get; init; }
    /// <summary>The bridge-DM gate's stored value (family-safety.md § The
    /// bridge-DM gate) — <c>null</c> when the wire omits it (the knob sitting at
    /// its <c>allow</c> default), NOT yet normalized (FamilyViewModel.LoadPolicyFromWard
    /// runs NormalizeUnknownPeerDmWire, the same place the other reach knobs are
    /// normalized).</summary>
    public string? UnknownPeerDm { get; init; }
    /// <summary>The proposed guardian's handle, or <c>null</c> when this ward has
    /// no outstanding transfer proposal (family-safety.md § Graduation & transfer).</summary>
    public string? PendingTransferGuardianHandle { get; init; }

    // ── Content policy (family-safety.md § Content policy) ───────────────
    // The four per-category floors as stored on the wire, NOT yet normalized —
    // FamilyViewModel.LoadPolicyFromWard runs NormalizeContentFloorWire over each
    // (the same place the reach knobs are normalized). An ABSENT content_policy
    // (a guardian who never set a floor) is the all-`inherit`
    // default, and an absent contentNotify is off — both the
    // unsupervised-equivalent no-op, so neither is `required`.
    public string ContentNsfw { get; init; } = "inherit";
    public string ContentSpam { get; init; } = "inherit";
    public string ContentPhishing { get; init; } = "inherit";
    public string ContentCommercial { get; init; } = "inherit";
    public bool ContentNotify { get; init; }

    // ── Screen time (family-safety.md § Screen time) ─────────────────────
    // Raw wire units (minutes from local midnight / whole minutes), NOT yet
    // formatted — FamilyViewModel.LoadPolicyFromWard runs FormatTimeOfDay /
    // ToString over each (the same place the reach knobs are normalized).
    public ushort? ScreenWindowStart { get; init; }
    public ushort? ScreenWindowEnd { get; init; }
    public ushort? ScreenDailyMinutes { get; init; }

    /// <summary>The guardian's per-ward screen-time readout
    /// (<c>family-ward-usage-today</c>, family-safety.md § Screen time) —
    /// rendered ONLY while this ward has a daily budget set (no accounting
    /// without a declared policy; the wire's <c>usage_today_minutes</c>
    /// already encodes that gate: <c>null</c> = no budget, <c>0</c> =
    /// budget-but-unreported-yet). <c>null</c> here means "don't render",
    /// mirroring <see cref="ContentNoticesText"/>.</summary>
    public string? UsageTodayText { get; init; }

    /// <summary>Whether <see cref="UsageTodayText"/> has anything to show —
    /// same plain-bool shape as <see cref="HasContentNotices"/>, for the same
    /// reason (this type lives in FaunaApp.Core; the XAML consumer x:Binds
    /// straight to a <c>Visibility</c> property).</summary>
    public bool HasUsageToday => !string.IsNullOrEmpty(UsageTodayText);

    /// <summary>The guardian's per-ward Guardian Notify readout
    /// (<c>family-ward-content-notices</c>, family-safety.md § Guardian Notify)
    /// — one resolved "{category}: {count}" line per <c>content_notices</c>
    /// entry, joined by newline. <c>null</c> when Notify is off or nothing
    /// reported today (never an empty string, so <see cref="HasContentNotices"/>
    /// stays a plain absence check).</summary>
    public string? ContentNoticesText { get; init; }

    /// <summary>Whether <see cref="ContentNoticesText"/> has anything to show —
    /// a plain bool, not a <c>Visibility</c>-typed property: this type lives in
    /// FaunaApp.Core (no <c>Microsoft.UI.Xaml</c> reference), so the XAML
    /// consumer x:Binds it straight to a <c>Visibility</c> property.</summary>
    public bool HasContentNotices => !string.IsNullOrEmpty(ContentNoticesText);

    /// <summary>The ward's devices for the guardian device-mark control
    /// (<c>family-device-mark-item</c>, family-safety.md § Guardian device
    /// marking).</summary>
    public IReadOnlyList<FamilyDeviceRow> Devices { get; init; } = Array.Empty<FamilyDeviceRow>();

    /// <summary>The guardian's per-ward band readout (<c>family-ward-age-band</c>,
    /// family-safety.md § App surface → *Age-band surfaces*) — <c>null</c> (absent, never
    /// placeholdered) for a band-less admission or a band this client cannot name.</summary>
    public string? AgeBandText { get; init; }

    /// <summary>Whether <see cref="AgeBandText"/> renders — the plain-bool shape of
    /// <see cref="HasContentNotices"/>, for the same x:Bind-to-Visibility reason.</summary>
    public bool HasAgeBand => AgeBandText is not null;

    /// <summary>The two readouts' shared text — <c>age_band_line</c> (nested keys), so the
    /// guardian's row (<paramref name="own"/> false) and the ward's own summary (true)
    /// cannot disagree; <c>null</c> when there is nothing nameable to show.</summary>
    internal static string? AgeBandLine(FfiFamilyAgeBand? band, bool own) =>
        band is { } b && FaunaFfiMethods.AgeBandLine(b.@band, b.@provenance, own) is { } line
            ? Strings.ResolveNested(line)
            : null;

    internal static FamilyWardRow From(FfiFamilyWardInfo w) => new()
    {
        ActorId = w.@actorId,
        Handle = w.@handle,
        ContactApproval = w.@policy.@contactApproval,
        UnknownSenderMail = w.@policy.@unknownSenderMail,
        FederationContact = w.@policy.@federationContact,
        FeedSources = w.@policy.@feedSources,
        UnknownPeerDm = w.@policy.@unknownPeerDm,
        PendingTransferGuardianHandle = w.@pendingTransfer?.@proposedGuardianHandle,
        ContentNsfw = w.@policy.@contentPolicy?.@nsfw ?? "inherit",
        ContentSpam = w.@policy.@contentPolicy?.@spam ?? "inherit",
        ContentPhishing = w.@policy.@contentPolicy?.@phishing ?? "inherit",
        ContentCommercial = w.@policy.@contentPolicy?.@commercial ?? "inherit",
        ContentNotify = w.@policy.@contentNotify ?? false,
        ScreenWindowStart = w.@policy.@screenTime?.@windowStart,
        ScreenWindowEnd = w.@policy.@screenTime?.@windowEnd,
        ScreenDailyMinutes = w.@policy.@screenTime?.@dailyMinutes,
        UsageTodayText = w.@usageTodayMinutes is { } used
            ? FormatUsageLine(used, w.@policy.@screenTime?.@dailyMinutes)
            : null,
        ContentNoticesText = w.@contentNotices.Length == 0
            ? null
            : string.Join("\n", w.@contentNotices.Select(n =>
            {
                var l = FaunaFfiMethods.ContentNoticeLine(n.@category, n.@count);
                return $"{Strings.Resolve(l.label)}: {Strings.Resolve(l.value)}";
            })),
        Devices = w.@devices.Select(FamilyDeviceRow.From).ToList(),
        AgeBandText = AgeBandLine(w.@ageBand, own: false),
    };

    private static string FormatUsageLine(uint usedMinutes, ushort? budgetMinutes)
    {
        var l = FaunaFfiMethods.UsageTodayLine(usedMinutes, budgetMinutes);
        return $"{Strings.Resolve(l.label)}: {Strings.Resolve(l.value)}";
    }
}

/// <summary>One of a ward's devices, for the guardian device-mark control
/// (<c>family-device-mark-item</c> / <c>family-device-mark-toggle</c>,
/// family-safety.md § Guardian device marking).</summary>
public sealed partial class FamilyDeviceRow : ObservableObject
{
    public required string DeviceId { get; init; }
    public required string Label { get; init; }
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(GuardianMarkedState))]
    private bool _guardianMarked;

    /// <summary>"on"/"off" mirror of <see cref="GuardianMarked"/> for
    /// AutomationProperties.HelpText — the cross-app get_attr(id, "state")
    /// read idiom every other toggle on this page follows. x:Bind OneWay
    /// (not code-behind) because <see cref="FamilyViewModel.DeviceMarkAsync"/>
    /// mutates THIS SAME row instance in place rather than recreating it.</summary>
    public string GuardianMarkedState => GuardianMarked ? "on" : "off";

    internal static FamilyDeviceRow From(FfiFamilyWardDevice d) => new()
    {
        DeviceId = d.@deviceId,
        Label = d.@label,
        GuardianMarked = d.@guardianMarked,
    };
}

/// <summary>An incoming transfer proposal naming this account as proposed guardian
/// (<c>family-incoming-transfer-item</c>) — family-safety.md § Graduation & transfer.
/// Any user can be a proposed guardian; the <c>family-tab</c> gate widens on this list
/// so a target with no other family relationship still reaches the prompt.</summary>
public sealed class FamilyIncomingTransferRow
{
    public required byte[] SupervisedActorId { get; init; }
    public required string SupervisedHandle { get; init; }
    public required string GuardianHandle { get; init; }
    public required long CreatedAt { get; init; }

    /// <summary>"{guardian} asks you to take over supervision of {ward}" — the
    /// CURRENT guardian, which is the fact the prompt renders (the initiator
    /// may have been the admin). Also the row's <c>AutomationProperties.Name</c>.</summary>
    public string DisplayText => Strings.Get("family/incoming_transfer_text")
        .Replace("{guardian}", GuardianHandle)
        .Replace("{ward}", SupervisedHandle);

    internal static FamilyIncomingTransferRow From(FfiFamilyIncomingTransfer t) => new()
    {
        SupervisedActorId = t.@supervisedActorId,
        SupervisedHandle = t.@supervisedHandle,
        GuardianHandle = t.@guardianHandle,
        CreatedAt = t.@createdAt,
    };
}

/// <summary>A reach-approval queue row (<c>family-approval-item</c>) — a pending
/// contact knock or mail hold awaiting the guardian's decision.</summary>
public sealed class FamilyApprovalRow
{
    public required byte[] SupervisedActorId { get; init; }
    public required string SupervisedHandle { get; init; }
    public required string Kind { get; init; }
    public required byte[] PeerActorId { get; init; }
    /// <summary>The <c>mail_hold</c> sender's envelope address; empty for a <c>contact</c>.</summary>
    public required string PeerAddress { get; init; }
    /// <summary>The held message's id, naming a <c>mail_hold</c> on decide; empty for a <c>contact</c>.</summary>
    public required byte[] MessageId { get; init; }
    public required string Summary { get; init; }
    /// <summary>v1.x — with <see cref="Operation"/> and <see cref="Target"/>, what names
    /// a <c>feed_source</c> on decide; empty for every other kind. Not <c>required</c>:
    /// a kind that does not use this key leaves it at empty, exactly as the wire does.</summary>
    public string BridgeId { get; init; } = "";
    /// <summary>v1.x — see <see cref="BridgeId"/>. <c>link</c> | <c>follow</c> | <c>feed</c>.</summary>
    public string Operation { get; init; } = "";
    /// <summary>v1.x — see <see cref="BridgeId"/>. Empty for a <c>link</c>.</summary>
    public string Target { get; init; } = "";
    public required long CreatedAt { get; init; }

    /// <summary>
    /// What the <c>family-approval-item</c> row renders — and what its
    /// <c>AutomationProperties.Name</c> binds. Computed once in <see cref="From"/>
    /// via the shared <c>fauna_core::format::approval_display_text</c>
    /// (family-safety.md § Reach approvals; § Where logic lives) rather than a
    /// per-kind ternary here.
    /// <para>
    /// A <c>mail_hold</c>'s <c>summary</c> is <b>deliberately empty</b>: a subject line is
    /// content, and the message is sealed to the ward, so the nest never sees it. Its peer is
    /// an <b>address</b>, not an actor, carried in <c>peer_address</c> rather than overloading
    /// <c>summary</c> (family-safety.md § Reach approvals). Rendering <c>Summary</c> for such a
    /// row would render it blank with live Approve/Deny buttons — and, since the row
    /// container's Name binds the same value, an empty Name makes UIA prune the row so FlaUI
    /// counts zero of them. This exact shape was a bug windows shipped once — never re-add a
    /// per-kind ternary here.
    /// </para>
    /// Any other kind (today <c>contact</c>; v1.x adds feed-source + child-initiated contact
    /// requests) is actor-shaped and carries a real summary, so it renders <c>Summary</c>.
    /// Deciding is still keyed on <c>MessageId</c> for a mail hold, never on this text.
    /// <para>
    /// A held null-path message (<c>MAIL FROM:&lt;&gt;</c> — family-safety.md § The mail gate)
    /// truthfully carries an empty <c>PeerAddress</c> too; the shared function returns
    /// <c>null</c> for that case, so this falls back to the localized no-sender label instead
    /// of the same blank-row defect.
    /// </para>
    /// </summary>
    public required string DisplayText { get; init; }

    internal static FamilyApprovalRow From(FfiFamilyApprovalEntry a) => new()
    {
        SupervisedActorId = a.@supervisedActorId,
        SupervisedHandle = a.@supervisedHandle,
        Kind = a.@kind,
        PeerActorId = a.@peerActorId,
        PeerAddress = a.@peerAddress,
        MessageId = a.@messageId,
        Summary = a.@summary,
        BridgeId = a.@bridgeId,
        Operation = a.@operation,
        Target = a.@target,
        CreatedAt = a.@createdAt,
        DisplayText = FaunaFfiMethods.ApprovalDisplayText(a) ?? Strings.Get("family/approval_no_sender"),
    };
}
