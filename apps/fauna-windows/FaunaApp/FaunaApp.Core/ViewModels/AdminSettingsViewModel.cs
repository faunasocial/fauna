using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The <c>admin-settings</c> page (admin.md § 3 Settings — renamed "Tiers" per the
/// per-page-services redesign, 2026-06-04) over the WS-RPC seam
/// (<see cref="INestRpcClient"/> → <c>FfiAdminClient</c>; no <c>/admin/api/*</c>
/// HTTP). Tier *definitions* only:
/// <list type="bullet">
///   <item><b>Tier definitions</b> (<c>admin-settings-tiers-section</c> /
///         <c>admin-settings-tier-item</c>) — what each tier means (its caps) via
///         <c>fauna.admin.tiers.{list,update}</c>, with in-place cap editing.
///         Policy, distinct from admission (which lives on <c>admin-users</c>,
///         § 2).</item>
/// </list>
/// The Factory Reset danger zone moved to the new <c>admin-nest</c> page
/// (admin.md § N Nest — see <c>AdminNestViewModel</c>, which also carried the
/// storage-mode indicator until it was retired entirely with the no-modes
/// cutover, Phase-4 S8.7); invite-code minting moved to <c>admin-users</c>
/// (§ 2); email-domain management moved to <c>admin-dns</c> (§ 3). Mirrors linux
/// <c>build_settings_page</c> / <c>update_tiers</c>.
/// </summary>
public partial class AdminSettingsViewModel : ViewModelBase
{
    private readonly INestRpcClient _rpc;

    [ObservableProperty] private bool _isLoading;

    /// <summary>The read-only tier-definition rows (<c>admin-settings-tier-item</c>).</summary>
    public ObservableCollection<AdminTierRow> Tiers { get; } = new();

    /// <summary>The membership designation rows (<c>admin-settings-membership-item</c>),
    /// one per subscription tier the admin owns — designated or not. Empty is the
    /// normal out-of-the-box state, never an error.</summary>
    public ObservableCollection<MembershipTierRow> MembershipTiers { get; } = new();

    /// <summary>Quota-tier names backing both membership selects
    /// (<c>-admin-tier-select</c> / <c>-lapse-tier-select</c>) — the same
    /// <c>fauna.admin.tiers.list</c> read that fills <see cref="Tiers"/>, so the
    /// picker can only ever offer a quota tier that exists.</summary>
    public ObservableCollection<string> QuotaTierNames { get; } = new();

    /// <summary>The shared <c>fauna_protocol::admin::DEFAULT_LAPSE_TIER</c> over FFI —
    /// what an undesignated row shows as its lapse tier. Read, never hard-coded
    /// (linux reads the Rust constant; apple/android still mirror it privately).</summary>
    public static string DefaultLapseTier => FaunaFfiMethods.DefaultLapseTier();

    internal AdminSettingsViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    /// <summary>Load the tier definitions + the membership designation section.</summary>
    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            await ReloadTiersAsync();
            await ReloadMembershipAsync();
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

    /// <summary>Re-render the membership section from BOTH reads (monetization.md
    /// § Pillar 4): the admin's OWN subscription tiers are the row set, and
    /// <c>membership_tiers.list</c> says which of those rows already carry a
    /// designation. Mirrors linux <c>update_membership_tiers</c>.
    ///
    /// A failed own-tiers read degrades to an empty row set rather than an error
    /// (mirrors linux <c>fetch_own_membership_tier_names</c>) — a payee with no
    /// subscription tiers has nothing to designate, so the section shows its
    /// "create one in your Tiers tab" empty state. The designation read is NOT
    /// degraded: if the admin owns tiers but we cannot say which are designated,
    /// rendering every row as undesignated would misrepresent persisted state.</summary>
    private async Task ReloadMembershipAsync()
    {
        IReadOnlyList<FfiTierItem> owned;
        try
        {
            owned = await _rpc.SubscriptionTiersListAsync();
        }
        catch
        {
            owned = Array.Empty<FfiTierItem>();
        }

        var designations = await _rpc.AdminMembershipTiersListAsync();
        var ownNames = owned.Select(t => t.name).ToList();
        var quotaNames = QuotaTierNames.ToList();
        MembershipTiers.Clear();
        foreach (var tier in owned)
        {
            var existing = designations.FirstOrDefault(m => m.tierName == tier.name);
            MembershipTiers.Add(MembershipTierRow.From(tier.name, existing, ownNames, quotaNames));
        }
    }

    /// <summary>Refetch <c>fauna.admin.tiers.list</c> into <see cref="Tiers"/> so
    /// each row renders from persisted state (used by the initial load and after a
    /// per-row save).</summary>
    private async Task ReloadTiersAsync()
    {
        var tiers = await _rpc.AdminTiersListAsync();
        Tiers.Clear();
        QuotaTierNames.Clear();
        foreach (var t in tiers)
        {
            Tiers.Add(AdminTierRow.From(t));
            QuotaTierNames.Add(t.name);
        }
    }

    /// <summary>Persist one tier row's edited caps via <c>fauna.admin.tiers.update</c>
    /// (the tier <paramref name="row"/>'s name keys it — names aren't editable),
    /// then refetch so the row re-renders from persisted state (admin.md § 3 —
    /// in-place tier-cap editing). Each cap is the raw i64 from its input, falling
    /// back to the persisted value on an empty/unparseable edit (no silent zeroing),
    /// mirroring linux <c>update_tiers</c> / <c>parse_cap</c>.</summary>
    public async Task SaveTierAsync(AdminTierRow row)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.AdminTiersUpdateAsync(
                row.Name,
                ParseCap(row.MaxInboxBytesText, row.InboxBytes),
                ParseCap(row.MaxStorageBytesText, row.StorageBytes),
                ParseCap(row.MaxDevicesText, row.Devices),
                ParseCap(row.MaxBlobSizeText, row.BlobSize),
                ParseCap(row.MaxFeedsText, row.Feeds));
            await ReloadTiersAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Designate or re-point one membership row via
    /// <c>fauna.admin.membership_tiers.set</c> (an upsert — no delete-then-create
    /// dance), then refetch so the row re-renders from persisted state. Save sends the
    /// row's <em>current</em> tier selection, never its original identity, so an admin
    /// can fix a mis-set row without deleting it (mirrors linux's
    /// <c>dropdown_tier(&amp;tier_select)</c> at save time and apple's
    /// <c>selectedTierName</c>). The lapse tier is always sent explicitly — the row
    /// always shows a definite selection, so the wire's empty-means-default is never
    /// relied on here.</summary>
    public async Task SaveMembershipAsync(MembershipTierRow row)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.AdminMembershipTiersSetAsync(row.SelectedTierName, row.AdminTier, row.LapseTier);
            await ReloadMembershipAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Drop one membership row's designation via
    /// <c>fauna.admin.membership_tiers.clear</c>, then refetch. Keyed by the row's own
    /// tier name (the designation that exists), not the possibly-re-picked selection.
    /// The subscription tier itself survives — it reverts to an ordinary content
    /// tier.</summary>
    public async Task ClearMembershipAsync(MembershipTierRow row)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.AdminMembershipTiersClearAsync(row.TierName);
            await ReloadMembershipAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Parse a raw-i64 cap input via the shared validator
    /// (<c>fauna_core::format::parse_cap</c> over <c>FaunaFfiMethods.ParseCap</c>):
    /// trims, clamps a negative to <c>0</c>, returns <see langword="null"/> for an
    /// empty/unparseable value — consumed as <c>?? prev</c> so a stray edit never
    /// silently zeroes a cap. Single-sourced with linux/android (priority #1/#2/#4).</summary>
    public static long ParseCap(string? text, long prev) =>
        uniffi.fauna_ffi.FaunaFfiMethods.ParseCap(text ?? string.Empty) ?? prev;
}

/// <summary>An editable tier-*definition* row (<c>admin-settings-tier-item</c>):
/// the read-only tier name (it identifies the row) + one editable raw-i64 input
/// per <c>AdminTier</c> cap, pre-filled with the persisted value (the tier *is* the
/// quota — admin.md § 3, in-place tier-cap editing). The persisted longs back the
/// unparseable-edit fallback (<see cref="AdminSettingsViewModel.ParseCap"/>); the
/// inputs are raw integers (bytes for byte caps, counts otherwise) — a unit-aware
/// editor is a future shared-Rust refinement. Mirrors linux
/// <c>build_tier_definition_row</c>.</summary>
public sealed partial class AdminTierRow : ObservableObject
{
    public required string Name { get; init; }

    // Persisted raw-i64 caps — the fallback for an empty/unparseable edit.
    public required long InboxBytes { get; init; }
    public required long StorageBytes { get; init; }
    public required long Devices { get; init; }
    public required long BlobSize { get; init; }
    public required long Feeds { get; init; }

    // Editable raw-i64 inputs (admin-settings-tier-cap-*), pre-filled with the
    // persisted values; two-way bound to the page's per-row text inputs.
    [ObservableProperty] private string _maxInboxBytesText = string.Empty;
    [ObservableProperty] private string _maxStorageBytesText = string.Empty;
    [ObservableProperty] private string _maxDevicesText = string.Empty;
    [ObservableProperty] private string _maxBlobSizeText = string.Empty;
    [ObservableProperty] private string _maxFeedsText = string.Empty;

    internal static AdminTierRow From(FfiAdminTier t) => new()
    {
        Name = t.name,
        InboxBytes = t.maxInboxBytes,
        StorageBytes = t.maxStorageBytes,
        Devices = t.maxDevices,
        BlobSize = t.maxBlobSize,
        Feeds = t.maxFeeds,
        MaxInboxBytesText = t.maxInboxBytes.ToString(),
        MaxStorageBytesText = t.maxStorageBytes.ToString(),
        MaxDevicesText = t.maxDevices.ToString(),
        MaxBlobSizeText = t.maxBlobSize.ToString(),
        MaxFeedsText = t.maxFeeds.ToString(),
    };
}

/// <summary>One membership designation row (<c>admin-settings-membership-item</c>,
/// monetization.md § Pillar 4) — a <b>link</b> between one of the admin's own
/// subscription tiers and this page's quota tiers, never a third tier list. There is
/// one row per <em>owned subscription tier</em>, designated or not, so the section
/// doubles as the list of what the admin could designate.
///
/// <para><see cref="TierName"/> is the row's identity (which owned tier it stands
/// for) and keys Clear. <see cref="SelectedTierName"/> is what the
/// <c>-tier-select</c> currently shows — day to day the same value, but a real
/// select per the approved shape, so an admin can re-point a mis-set row without
/// deleting it; Save reads it. Mirrors linux <c>build_membership_row</c>,
/// apple <c>MembershipDesignationRow</c>, android <c>MembershipRow</c>.</para></summary>
public sealed partial class MembershipTierRow : ObservableObject
{
    /// <summary>The owned subscription tier this row stands for — the row's identity.
    /// Not editable: re-pointing edits <see cref="SelectedTierName"/>.</summary>
    public required string TierName { get; init; }

    /// <summary>Options for the <c>-tier-select</c>: every subscription tier the admin
    /// owns. Carried on the row (not the page) so the DataTemplate can bind it
    /// directly — a WinUI DataTemplate resolves <c>x:Bind</c> against its DataType,
    /// not the page, and routing it through page-level static state would break the
    /// moment two admin surfaces render at once.</summary>
    public required IReadOnlyList<string> OwnTierNames { get; init; }

    /// <summary>Options for both quota-tier selects: the tiers this page defines, so a
    /// row can never name a quota tier that does not exist.</summary>
    public required IReadOnlyList<string> QuotaTierNames { get; init; }

    /// <summary>Whether this row already carries a designation. Drives the Clear
    /// affordance's meaning — note the button stays <em>enabled</em> either way (a
    /// disabled WinUI button cannot be driven by FlaUI's InvokePattern), with
    /// <see cref="AdminSettingsViewModel.ClearMembershipAsync"/> as the real guard;
    /// linux desensitizes its button instead.</summary>
    public required bool IsDesignated { get; init; }

    /// <summary>The <c>-tier-select</c> value: which owned subscription tier this row
    /// designates. Seeded to <see cref="TierName"/>.</summary>
    [ObservableProperty] private string _selectedTierName = string.Empty;

    /// <summary>The <c>-admin-tier-select</c> value: the quota tier an admitted member
    /// is assigned. Empty on an undesignated row (nothing chosen yet), matching linux's
    /// empty initial selection.</summary>
    [ObservableProperty] private string _adminTier = string.Empty;

    /// <summary>The <c>-lapse-tier-select</c> value: the quota tier a lapsed member
    /// degrades to — a reversible downgrade, never a suspension. Defaults to the shared
    /// <see cref="AdminSettingsViewModel.DefaultLapseTier"/> on an undesignated row.</summary>
    [ObservableProperty] private string _lapseTier = string.Empty;

    internal static MembershipTierRow From(
        string tierName,
        FfiAdminMembershipTier? existing,
        IReadOnlyList<string> ownTierNames,
        IReadOnlyList<string> quotaTierNames) => new()
    {
        TierName = tierName,
        OwnTierNames = ownTierNames,
        QuotaTierNames = quotaTierNames,
        IsDesignated = existing is not null,
        SelectedTierName = tierName,
        AdminTier = existing?.adminTier ?? string.Empty,
        LapseTier = existing?.lapseTier ?? AdminSettingsViewModel.DefaultLapseTier,
    };
}
