using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The consolidated <c>admin-users</c> hub (admin.md § Users) — one mental model
/// "who's on / who wants on / let someone on", all framed around assigning a TIER.
/// Three sections over the shared admin cluster via the WS-RPC seam
/// (<see cref="INestRpcClient"/> → <c>FfiAdminClient</c>; no <c>/admin/api/*</c> HTTP):
/// <list type="bullet">
///   <item>Users — list (paged, 50/page) + change a user's tier
///         (<c>fauna.admin.users.update</c>) + one-click eviction / cancel
///         (<c>fauna.admin.users.{evict,cancel_eviction}</c>).</item>
///   <item>Invite — mint a code (mint-on-empty) + copy + delete.</item>
///   <item>Pending requests — approve at a chosen tier / deny.</item>
/// </list>
/// Tier-*definition* editing remains out of scope (no ui.yaml edit IDs yet —
/// approval-gated). Eviction + pagination landed in a follow-on slice.
/// All three sections' action failures route to <see cref="ActionError"/> (the
/// <c>admin-users-action-error</c> surface), not the app-wide error banner.
/// </summary>
public partial class AdminUsersViewModel : ObservableObject
{
    private readonly INestRpcClient _rpc;

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _actionError;

    /// <summary>The user-list page size — matches the nest default + linux (admin.md
    /// § Users: pagination over <c>users.list</c> <c>limit</c>/<c>offset</c>).</summary>
    public const long PageSize = 50;

    /// <summary>Current <c>users.list</c> offset; prev/next move it by <see cref="PageSize"/>.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(CurrentPage), nameof(HasPrevPage), nameof(HasNextPage))]
    private long _offset;

    /// <summary>Total users across all pages (<c>FfiAdminUsersListReply.total</c>).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(UserCount), nameof(CurrentPage), nameof(TotalPages),
        nameof(HasPrevPage), nameof(HasNextPage))]
    private long _totalUsers;

    // Registration section: the nest's registration posture
    // (fauna.setup.status → FfiSetupStatus.{registrationMode,maxFreeUsers}) +
    // the save-form drafts. RegistrationMode is the raw wire string (or null) —
    // never coerced (public-mode.md § Registration Modes: a posture this
    // client can't name renders read-only rather than guess, since saving a
    // guess would overwrite the nest's real posture). RegistrationModeDraft
    // empty ⇒ unknown/absent posture ⇒ the page renders the read-only
    // explainer instead of the picker+input+save (mirrors tui's
    // registration_section / AdminVM.swift's registrationModeDraft).
    [ObservableProperty] private string? _registrationMode;
    [ObservableProperty] private ulong? _maxFreeUsers;
    [ObservableProperty] private string _registrationModeDraft = string.Empty;
    [ObservableProperty] private string _maxFreeUsersInput = string.Empty;
    /// <summary>The persisted age require-knob (<c>FfiSetupStatus.ageVerificationRequired</c>
    /// — family-safety.md § The account age band D5+D6), read off the same
    /// <c>fauna.setup.status</c> as the posture.</summary>
    [ObservableProperty] private bool _ageVerificationRequired;
    /// <summary>The knob's draft (<c>admin-users-registration-age-verification-toggle</c>),
    /// re-seeded with the posture; the section's one save sends it only when it
    /// differs from <see cref="AgeVerificationRequired"/> (tui's changed-only
    /// <c>registration_mutation</c>).</summary>
    [ObservableProperty] private bool _ageVerificationDraft;

    // Admit section: direct admission drafts (admin-users-admit-*;
    // public-mode.md § Registration & Identity — the third account-creation
    // path, user-approved 2026-08-15). AdmitTierDraft empty ⇒ the save
    // defaults to the first known tier (mirrors AdminVM.swift's admitUser()).
    [ObservableProperty] private string _admitActorInput = string.Empty;
    [ObservableProperty] private string _admitHandleInput = string.Empty;
    [ObservableProperty] private string _admitTierDraft = string.Empty;

    // Invite section: mint form + the just-minted token (copyable).
    [ObservableProperty] private bool _isCreatingCode;
    [ObservableProperty] private string _newCodeTier = string.Empty;
    [ObservableProperty] private int _newCodeUses = 1;
    /// <summary>Links the redeemed account to a guardian for supervised admission
    /// (family-safety.md § Wire &amp; data shape, <c>admin-users-invite-guardian-select</c>);
    /// <c>null</c> mints an ordinary code.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(NewCodeAgeBandEnabled))]
    private byte[]? _newCodeGuardianActor;
    /// <summary>The mint's band (<c>admin-users-invite-age-band-select</c>) as a shared
    /// <c>age_band_options()</c> VALUE — only meaningful beside a guardian, and reset to
    /// the shared not-set value whenever the guardian is cleared (family-safety.md § App
    /// surface → *Age-band surfaces*: a band presupposes a guardianship link).</summary>
    [ObservableProperty] private string _newCodeAgeBand = FaunaFfiMethods.AgeBandNotSetValue();

    /// <summary>The mint band select is enabled only while a guardian is selected — the
    /// nest refuses a band without one; the gate here is UX, the nest the authority.</summary>
    public bool NewCodeAgeBandEnabled => NewCodeGuardianActor is not null;

    partial void OnNewCodeGuardianActorChanged(byte[]? value)
    {
        if (value is null) NewCodeAgeBand = FaunaFfiMethods.AgeBandNotSetValue();
    }
    [ObservableProperty] private string? _mintedCode;
    [ObservableProperty] private bool _showMintedCode;

    /// <summary>The user roster (<c>user-row</c>); the admin themselves is always present.</summary>
    public ObservableCollection<AdminUserRow> Users { get; } = new();
    /// <summary>Every account on the nest (admin.md § 2 → <i>Which accounts a picker
    /// offers</i>) — both admission guardian pickers' option source, kept separate from
    /// the paginated <see cref="Users"/> page so a guardian past page 1 of the roster is
    /// still selectable. Loaded once via <c>users_list_all</c>, not paged like
    /// <see cref="Users"/>.</summary>
    public ObservableCollection<AdminUserRow> GuardianCandidates { get; } = new();
    /// <summary>Existing closed-registration invite codes (<c>invite-code-item</c>).</summary>
    public ObservableCollection<AdminInviteCodeRow> InviteCodes { get; } = new();
    /// <summary>Pending onboarding invite requests (<c>admin-invite-requests-list</c>).</summary>
    public ObservableCollection<AdminInviteRequestRow> InviteRequests { get; } = new();
    /// <summary>Tier *names* (free/personal/community…) that populate the tier
    /// ComboBoxes (the page renders one item per name).</summary>
    public ObservableCollection<string> Tiers { get; } = new();

    /// <summary>The grand total for the "{N} users total" count text — the total
    /// across all pages (matches linux rendering <c>reply.total</c>), not just the
    /// rows on the loaded page.</summary>
    public int UserCount => (int)TotalUsers;

    /// <summary>1-based current page — shared <c>fauna_core::format::current_page</c>
    /// (value-formatting.md § Pagination).</summary>
    public long CurrentPage => FaunaFfiMethods.CurrentPage(Offset, PageSize);

    /// <summary>Total page count, at least 1 — shared
    /// <c>fauna_core::format::total_pages</c> (value-formatting.md § Pagination).</summary>
    public long TotalPages => FaunaFfiMethods.TotalPages(TotalUsers, PageSize);

    /// <summary>Whether a previous page exists (enables <c>admin-users-prev-page</c>) —
    /// derived from the same shared <c>prev_page_offset</c> the stepper below steps
    /// with, so the enabled check and the actual step never disagree.</summary>
    public bool HasPrevPage => FaunaFfiMethods.PrevPageOffset(Offset, PageSize) is not null;

    /// <summary>Whether a next page exists (enables <c>admin-users-next-page</c>) —
    /// derived from the same shared <c>next_page_offset</c> the stepper below steps
    /// with, so the enabled check and the actual step never disagree.</summary>
    public bool HasNextPage => FaunaFfiMethods.NextPageOffset(Offset, TotalUsers, PageSize) is not null;

    internal AdminUsersViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    /// <summary>Load all three sections + the tier list. Drives the hub render.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ActionError = null;
        try
        {
            await LoadTiersAsync();
            await LoadRegistrationAsync();
            await ReloadUsersAsync();
            await ReloadInviteCodesAsync();
            await ReloadInviteRequestsAsync();
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

    private async Task LoadTiersAsync()
    {
        var tiers = await _rpc.AdminTiersListAsync();
        Tiers.Clear();
        foreach (var t in tiers) Tiers.Add(t.name);
        if (string.IsNullOrEmpty(NewCodeTier) && Tiers.Count > 0) NewCodeTier = Tiers[0];
    }

    /// <summary>Refetch the current page of <see cref="Users"/> AND — alongside it,
    /// mirroring linux's paired <c>AdminUsersLoaded{reply,picker_users}</c> — every
    /// account on the nest into <see cref="GuardianCandidates"/>, so a suspend/evict/
    /// admit action's refetch keeps guardian eligibility current too, not just the
    /// initial page load.</summary>
    private async Task ReloadUsersAsync()
    {
        var reply = await _rpc.AdminUsersListAsync(PageSize, Offset);
        // Set the total before mutating Users so the count text + page math are
        // current when the collection-changed observer fires.
        TotalUsers = reply.total;
        Users.Clear();
        foreach (var u in reply.users) Users.Add(AdminUserRow.From(u));
        OnPropertyChanged(nameof(UserCount));

        var allUsers = await _rpc.AdminUsersListAllAsync();
        GuardianCandidates.Clear();
        foreach (var u in allUsers) GuardianCandidates.Add(AdminUserRow.From(u));
    }

    private async Task ReloadInviteCodesAsync()
    {
        var codes = await _rpc.AdminInviteCodesListAsync();
        InviteCodes.Clear();
        foreach (var c in codes) InviteCodes.Add(AdminInviteCodeRow.From(c));
    }

    private async Task ReloadInviteRequestsAsync()
    {
        var requests = await _rpc.AdminInviteRequestsListAsync();
        InviteRequests.Clear();
        foreach (var r in requests)
            if (r.isPending) InviteRequests.Add(AdminInviteRequestRow.From(r));
    }

    // ── Users section ──

    /// <summary>Change a user's tier and refetch so the row shows the applied tier
    /// (proves the round-trip, not an optimistic flip). The tier IS the quota.</summary>
    public async Task ChangeUserTierAsync(AdminUserRow row, string tier)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminUsersUpdateAsync(row.ActorId, tier, row.Label);
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Start a user's eviction timeline (<c>fauna.admin.users.evict</c>) —
    /// one-click with a default reason (the <c>evict_default_reason</c> i18n string)
    /// + the <c>other</c> category, the only inputs the row exposes (matches linux).
    /// The user is NOT deleted; refetch so the row flips to the cancel control
    /// (proves the round-trip, not an optimistic toggle).</summary>
    public async Task EvictUserAsync(AdminUserRow row)
    {
        ActionError = null;
        try
        {
            var reason = Strings.Get("admin/users_page/evict_default_reason");
            await _rpc.AdminUsersEvictAsync(row.ActorId, reason, "other");
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Cut a user off *now*, no delete timeline (<c>fauna.admin.users.suspend</c>)
    /// — one-click with a default reason (<c>suspend_default_reason</c>) + the
    /// <c>other</c> category, mirroring <see cref="EvictUserAsync"/>. Reachable from
    /// <c>Active</c> or a mid-eviction <c>warning</c> row (admin.md § 2 Users →
    /// *Cutting a user off*); the row's <see cref="AdminUserRow.CanSuspend"/> gate
    /// (from the shared <c>admin_user_row_controls</c> decision) already withholds
    /// this when it wouldn't apply. Refetch so the row flips to the restore control.</summary>
    public async Task SuspendUserAsync(AdminUserRow row)
    {
        ActionError = null;
        try
        {
            var reason = Strings.Get("admin/users_page/suspend_default_reason");
            await _rpc.AdminUsersSuspendAsync(row.ActorId, reason, "other");
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Cancel a pending eviction or lift a suspension
    /// (<c>fauna.admin.users.cancel_eviction</c> — suspension is the eviction
    /// machine's <c>suspended</c> state, so this one control restores either entry
    /// point) and refetch so the row flips back to the evict/suspend controls.</summary>
    public async Task CancelEvictionAsync(AdminUserRow row)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminUsersCancelEvictionAsync(row.ActorId);
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Grant the admin role (<c>fauna.admin.admins.add</c>) —
    /// <c>admin-users-make-admin-button</c> on a plain, non-admin row. Schedules
    /// an <c>AdminAdd</c> pending action (24h delay, <c>admin.md</c> § Admin
    /// continuity and succession) — the row does NOT flip to an admin row right
    /// away; a scheduled reply (no error) is success. Refetch anyway so any
    /// other roster change lands.</summary>
    public async Task MakeAdminAsync(AdminUserRow row)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminAdminsAddAsync(row.ActorId);
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Revoke the admin role (<c>fauna.admin.admins.remove</c>) —
    /// <c>admin-users-remove-admin-button</c> on an <c>is_admin</c> row.
    /// Schedules an <c>AdminRemove</c> pending action; the nest refuses
    /// (<c>fauna.admin.conflict</c>) when it would leave zero superadmins —
    /// the client makes no such judgment itself.</summary>
    public async Task RemoveAdminAsync(AdminUserRow row)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminAdminsRemoveAsync(row.ActorId);
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Advance to the next page and refetch — shared
    /// <c>fauna_core::format::next_page_offset</c> (value-formatting.md §
    /// Pagination), <c>null</c> at the last page. No-op there.</summary>
    public async Task NextPageAsync()
    {
        var next = FaunaFfiMethods.NextPageOffset(Offset, TotalUsers, PageSize);
        if (next is null) return;
        Offset = next.Value;
        await ReloadUsersPageAsync();
    }

    /// <summary>Go to the previous page and refetch — shared
    /// <c>fauna_core::format::prev_page_offset</c> (value-formatting.md §
    /// Pagination), <c>null</c> at the first page. No-op there.</summary>
    public async Task PrevPageAsync()
    {
        var prev = FaunaFfiMethods.PrevPageOffset(Offset, PageSize);
        if (prev is null) return;
        Offset = prev.Value;
        await ReloadUsersPageAsync();
    }

    /// <summary>Refetch the current page, routing any failure to the action-error
    /// surface (the page-turn handlers call this outside <see cref="LoadAsync"/>).</summary>
    private async Task ReloadUsersPageAsync()
    {
        ActionError = null;
        try
        {
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    // ── Registration section ──

    /// <summary>Read the persisted registration posture off <c>fauna.setup.status</c>
    /// — the raw wire string, never coerced (public-mode.md § Implementation
    /// status today) — and re-seed the save-form drafts from it.</summary>
    private async Task LoadRegistrationAsync()
    {
        var status = await _rpc.SetupStatusAsync();
        RegistrationMode = status.registrationMode;
        MaxFreeUsers = status.maxFreeUsers;
        AgeVerificationRequired = status.ageVerificationRequired;
        SyncRegistrationDrafts();
    }

    /// <summary>Re-seed the registration drafts from the persisted posture —
    /// unconditional, unlike the tier drafts: another admin (or this session's
    /// own prior save) may have changed the posture underneath a stale local
    /// draft (mirrors tui's <c>UsersState::reseed</c> / AdminVM.swift's
    /// <c>syncRegistrationDrafts</c>).</summary>
    private void SyncRegistrationDrafts()
    {
        RegistrationModeDraft = RegistrationMode is { } mode && FaunaFfiMethods.RegistrationModeFromWire(mode) is not null
            ? mode
            : string.Empty;
        MaxFreeUsersInput = MaxFreeUsers?.ToString() ?? string.Empty;
        AgeVerificationDraft = AgeVerificationRequired;
    }

    /// <summary>Save the registration posture (<c>admin-users-registration-save-button</c>)
    /// — one <c>fauna.admin.set_registration_mode</c> call carrying the mode and
    /// the orthogonal free-tier ceiling together. <see cref="MaxFreeUsersInput"/>
    /// blank ⇒ no cap; a non-numeric entry is a local user error, never
    /// dispatched (mirrors tui's <c>registration_mutation</c> / AdminVM.swift's
    /// <c>saveRegistration</c>). The same gesture sends the age require-knob
    /// (<c>fauna.admin.set_age_verification_required</c>) beside the mode, and only
    /// when <see cref="AgeVerificationDraft"/> changed (family-safety.md § App
    /// surface → *Age-band surfaces*: one gesture, no half-saved section).</summary>
    [RelayCommand]
    private async Task SaveRegistrationAsync()
    {
        var trimmed = MaxFreeUsersInput.Trim();
        ulong? maxFree;
        if (trimmed.Length == 0)
        {
            maxFree = null;
        }
        else if (ulong.TryParse(trimmed, out var parsed))
        {
            maxFree = parsed;
        }
        else
        {
            ActionError = Strings.Get("admin/users_page/max_free_users_hint");
            return;
        }
        // The picker only ever offers a known option, so a null mode here is
        // unreachable in practice — guard anyway rather than dispatch a guess.
        if (FaunaFfiMethods.RegistrationModeFromWire(RegistrationModeDraft) is not { } mode) return;
        ActionError = null;
        try
        {
            await _rpc.AdminSetRegistrationModeAsync(mode, maxFree);
            if (AgeVerificationDraft != AgeVerificationRequired)
                await _rpc.AdminSetAgeVerificationRequiredAsync(AgeVerificationDraft);
            await LoadRegistrationAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    // ── Admit section ──

    /// <summary>Admit a known actor id directly via <c>fauna.admin.users.create</c>
    /// — the third account-creation path (<c>admin-users-admit-*</c>;
    /// public-mode.md § Registration &amp; Identity). <see cref="AdmitActorInput"/>
    /// must be exactly 64 hex chars — a malformed id is a local user error,
    /// never dispatched (the nest would refuse it anyway; failing local keeps
    /// the message actionable, mirrors AdminVM.swift's <c>admitUser</c>).
    /// <see cref="AdmitHandleInput"/> blank ⇒ <c>null</c>, the deliberate
    /// handle-less admission (public-mode.md § A handle-less account). The
    /// form is never cleared here on success or failure — the new row in the
    /// Users-section refetch is the feedback.</summary>
    [RelayCommand]
    private async Task AdmitUserAsync()
    {
        var trimmed = AdmitActorInput.Trim();
        if (trimmed.Length != 64 || !trimmed.All(Uri.IsHexDigit))
        {
            ActionError = Strings.Get("admin/users_page/admit_actor_hint");
            return;
        }
        ActionError = null;
        try
        {
            var actorId = Convert.FromHexString(trimmed);
            var handle = AdmitHandleInput.Trim();
            var tier = AdmitTierDraft.Length == 0 ? (Tiers.Count > 0 ? Tiers[0] : "free") : AdmitTierDraft;
            await _rpc.AdminUsersCreateAsync(actorId, tier, handle.Length == 0 ? null : handle);
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    // ── Invite section ──

    /// <summary>Reveal the mint form (no code-input field — mint-on-empty).</summary>
    [RelayCommand]
    private void BeginCreateCode()
    {
        IsCreatingCode = true;
        ShowMintedCode = false;
        if (string.IsNullOrEmpty(NewCodeTier) && Tiers.Count > 0) NewCodeTier = Tiers[0];
        NewCodeUses = 1;
        NewCodeGuardianActor = null;
        NewCodeAgeBand = FaunaFfiMethods.AgeBandNotSetValue();
    }

    /// <summary>Cancel the mint form.</summary>
    [RelayCommand]
    private void CancelCreateCode()
    {
        IsCreatingCode = false;
    }

    /// <summary>Mint a code at <see cref="NewCodeTier"/> / <see cref="NewCodeUses"/>.
    /// Empty code ⇒ the nest mints and returns the token (admin.md § 3); surface it
    /// copyable and refetch the list. The band rides only beside a guardian.</summary>
    [RelayCommand]
    private async Task CreateCodeAsync()
    {
        ActionError = null;
        try
        {
            var code = await _rpc.AdminInviteCodesCreateAsync(
                "", NewCodeTier, NewCodeUses, NewCodeGuardianActor,
                NewCodeGuardianActor is null ? null : NewCodeAgeBand);
            MintedCode = code;
            ShowMintedCode = true;
            IsCreatingCode = false;
            await ReloadInviteCodesAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Delete an invite code and refetch.</summary>
    public async Task DeleteCodeAsync(string code)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminInviteCodesDeleteAsync(code);
            await ReloadInviteCodesAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    // ── Pending requests section ──

    /// <summary>Approve a request, admitting the requester at <paramref name="tier"/>
    /// (creates the account + deletes the request); refetch both sections.
    /// <paramref name="guardianActorId"/> links the admitted account to a guardian
    /// for supervised admission (family-safety.md § Wire &amp; data shape,
    /// <c>invite-request-row-guardian-select</c>); <c>null</c> admits an ordinary
    /// account. <paramref name="ageBand"/> (that row's <c>invite-request-row-age-band-select</c>
    /// VALUE, seeded from <see cref="AdminInviteRequestRow.AgeBandSeed"/>) rides only beside
    /// a guardian, as on the mint.</summary>
    public async Task ApproveRequestAsync(AdminInviteRequestRow row, string tier, byte[]? guardianActorId = null,
                                          string? ageBand = null)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminInviteRequestsApproveAsync(
                row.Id, tier, null, guardianActorId, guardianActorId is null ? null : ageBand);
            await ReloadInviteRequestsAsync();
            await ReloadUsersAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }

    /// <summary>Deny a request with an optional reason; refetch.</summary>
    public async Task DenyRequestAsync(AdminInviteRequestRow row, string? reason)
    {
        ActionError = null;
        try
        {
            await _rpc.AdminInviteRequestsDenyAsync(row.Id, reason);
            await ReloadInviteRequestsAsync();
        }
        catch (Exception ex)
        {
            ActionError = Strings.Error(ex);
        }
    }
}

/// <summary>A user row (<c>user-row</c>) — the actor id + its tier (the quota).</summary>
public sealed class AdminUserRow
{
    public required byte[] ActorId { get; init; }
    public required string ActorIdHex { get; init; }
    public required string Tier { get; init; }
    public required string Label { get; init; }
    /// <summary>The stable identity an admin picker shows for this user — handle,
    /// falling back to the full actor hex for a handle-less account (admin.md § 2
    /// Users → <i>What identifies a user in an admin picker</i>). Computed once in
    /// <see cref="From"/> via <see cref="AdminActorOptions.Label"/>; never the
    /// editable, non-unique <see cref="Label"/> above.</summary>
    public required string PickerOption { get; init; }
    public required bool Suspended { get; init; }
    /// <summary>A pending eviction OR an active suspension exists (both ride the
    /// same eviction row — suspension is its <c>suspended</c> status) → the row
    /// shows the restore control. Equivalent to the shared decision's
    /// <c>restore</c> (lifecycle != Active), since <c>Active</c> is exactly
    /// "no eviction row".</summary>
    public required bool EvictionActive { get; init; }
    /// <summary>Whether this actor holds the admin role — withholds the entry
    /// controls (an admin can be neither suspended nor evicted,
    /// <c>fauna.admin.conflict</c>) but never the restore control.</summary>
    public required bool IsAdmin { get; init; }
    /// <summary>The user's IMAP/CalDAV-serving flag (<c>AdminUser.mail_serving_enabled</c>,
    /// default on), surfaced READ-ONLY on the row — the user sets it from their own
    /// mail-settings serve-here toggle, never the admin (admin.md § Users;
    /// mail-settings.md § Local IMAP/CalDAV-serving toggle).</summary>
    public required bool MailServingEnabled { get; init; }
    /// <summary><c>admin-users-suspend-button</c> — cut off now, no delete timeline.
    /// From the shared <c>fauna_client_admin::admin_user_row_controls</c> decision
    /// (admin.md § 2 Users → *Cutting a user off*), NOT re-derived here — computed
    /// once in <see cref="From"/>, not stored as the raw (UniFFI-internal)
    /// <c>FfiAdminUserRowControls</c> record, which a <c>public</c> row type can't
    /// expose (matches <see cref="EvictionActive"/>'s plain-bool shape).</summary>
    public required bool CanSuspend { get; init; }
    /// <summary><c>admin-users-evict-button</c> — start the timed warn → suspend →
    /// delete ladder. Active-only AND admin-guarded (unlike the old
    /// <c>!EvictionActive</c> shortcut, which rendered a control the nest always
    /// refuses on an admin row).</summary>
    public required bool CanEvict { get; init; }
    /// <summary><c>admin-users-make-admin-button</c> — grant the admin role
    /// (a scheduled, 24h-delay <c>AdminAdd</c> pending action). From the shared
    /// decision, same as <see cref="CanSuspend"/> — never re-derived from
    /// <see cref="IsAdmin"/> here.</summary>
    public required bool CanMakeAdmin { get; init; }
    /// <summary><c>admin-users-remove-admin-button</c> — revoke the admin role
    /// (a scheduled <c>AdminRemove</c> pending action; the nest refuses
    /// <c>fauna.admin.conflict</c> when it would leave zero superadmins).</summary>
    public required bool CanRemoveAdmin { get; init; }
    /// <summary>The localized read-only serving-status label
    /// (<c>admin-users-mail-serving-status</c>): "Serving here" / "Not serving",
    /// single-sourced via the shared <c>fauna_core::format::mail_serving_status_label</c>
    /// (value-formatting.md § Serving-status label) — no per-app key ternary.</summary>
    public string MailServingStatus => Strings.Resolve(
        FaunaFfiMethods.MailServingStatusLabel(MailServingEnabled));

    internal static AdminUserRow From(FfiAdminUser u)
    {
        var controls = FaunaFfiMethods.AdminUserRowControls(u);
        return new()
        {
            ActorId = u.actorId,
            ActorIdHex = FaunaFfiMethods.HexFull(u.actorId),
            Tier = u.tier,
            Label = u.label,
            PickerOption = AdminActorOptions.Label(u),
            Suspended = u.suspended,
            EvictionActive = u.eviction is not null,
            IsAdmin = u.isAdmin,
            MailServingEnabled = u.mailServingEnabled,
            CanSuspend = controls.suspend,
            CanEvict = controls.evict,
            CanMakeAdmin = controls.makeAdmin,
            CanRemoveAdmin = controls.removeAdmin,
        };
    }
}

/// <summary>An invite-code row (<c>invite-code-item</c> / <c>invite-code-value</c>).</summary>
public sealed class AdminInviteCodeRow
{
    public required string Code { get; init; }
    public required string Tier { get; init; }
    public required long UsesLeft { get; init; }
    /// <summary>The minted band's label (shared <c>age_band_label</c>), <c>null</c> for an
    /// ordinary code or a token this client cannot name.</summary>
    public string? AgeBandText { get; init; }

    /// <summary>The <c>invite-code-item</c> row text — tier · uses left, plus the minted band
    /// when there is one ("free · 1 · Under 13"; tui's row shape — family-safety.md § App
    /// surface → *Age-band surfaces*: the same row, richer text, no new id).</summary>
    public string ItemText => AgeBandText is { } band
        ? $"{Tier} · {UsesLeft} · {band}"
        : $"{Tier} · {UsesLeft}";

    internal static AdminInviteCodeRow From(FfiAdminInviteCode c) => new()
    {
        Code = c.code,
        Tier = c.tier,
        UsesLeft = c.usesLeft,
        AgeBandText = c.ageBand is { } band && FaunaFfiMethods.AgeBandLabel(band) is { } label
            ? Strings.Resolve(label)
            : null,
    };
}

/// <summary>A pending invite-request row (<c>invite-request-row-*</c>).</summary>
public sealed class AdminInviteRequestRow
{
    public required long Id { get; init; }
    public required string ActorIdHex { get; init; }
    public required string Handle { get; init; }
    public required string Message { get; init; }
    /// <summary>The applicant's recorded claim (<c>invite-request-row-age-claim</c>) — the
    /// shared, TOTAL <c>age_claim_label</c>: "No app age verification" when the request
    /// carries none (family-safety.md § The account age band D6, absence as signal).</summary>
    public required string AgeClaimText { get; init; }
    /// <summary>The row's band-select seed — the applicant's claimed band when this client
    /// can name it, else the not-set value (shared <c>claimed_age_band_option</c>; the claim
    /// corroborates, the admitting adult decides).</summary>
    public required string AgeBandSeed { get; init; }

    internal static AdminInviteRequestRow From(FfiAdminInviteRequest r) => new()
    {
        Id = r.id,
        ActorIdHex = FaunaFfiMethods.HexFull(r.actorId),
        Handle = r.handle,
        Message = r.message,
        AgeClaimText = Strings.ResolveNested(FaunaFfiMethods.AgeClaimLabel(r.ageBand, r.ageBandProvenance)),
        AgeBandSeed = FaunaFfiMethods.ClaimedAgeBandOption(r.ageBand),
    };
}
