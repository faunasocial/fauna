using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the <c>admin-settings</c> page VM, over the
/// <see cref="MockNestRpcClient"/> WS-RPC seam (admin.md § 3 Settings — the
/// tier-*definition* in-place cap editor; no live nest / FlaUI). The
/// storage-mode indicator this page once carried moved to <c>admin-nest</c>
/// (see <c>AdminNestViewModelTests</c>) in the per-page-services redesign, and
/// was itself retired entirely with the no-modes cutover (Phase-4 S8.7).
/// </summary>
public class AdminSettingsViewModelTests
{
    [Fact]
    public async Task Load_PopulatesTierDefinitions()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminTiers = new[]
            {
                MockNestRpcClient.MakeAdminTier("free"),
                MockNestRpcClient.MakeAdminTier("personal"),
            },
        };
        var vm = new AdminSettingsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(new[] { "free", "personal" }, vm.Tiers.Select(t => t.Name));
        // Each editable cap input is pre-filled with the persisted raw integer
        // (MakeAdminTier seeds all caps 0 → "0"); admin.md § 3 in-place editing.
        Assert.All(vm.Tiers, t => Assert.Equal("0", t.MaxInboxBytesText));
        Assert.All(vm.Tiers, t => Assert.Equal("0", t.MaxFeedsText));
        Assert.Null(vm.ErrorMessage);
        Assert.False(vm.IsLoading);
        // The tier read went over the WS-RPC seam (no HTTP twin). Storage-mode
        // moved to AdminNestViewModel (admin-nest) — this page no longer reads
        // fauna.setup.status.
        Assert.Contains("AdminTiersList", rpc.Calls);
        Assert.DoesNotContain("SetupStatus", rpc.Calls);
    }

    [Fact]
    public async Task SaveTier_PersistsEditedCapsViaUpdate_ThenRefetches()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminTiers = new[] { MockNestRpcClient.MakeAdminTier("free") },
        };
        var vm = new AdminSettingsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.Tiers);
        row.MaxInboxBytesText = "2048";
        row.MaxStorageBytesText = "4096";
        row.MaxDevicesText = "5";
        row.MaxBlobSizeText = "1024";
        row.MaxFeedsText = "9";

        await vm.SaveTierAsync(row);

        // Persisted the row's caps over the WS-RPC seam (the name keys the row).
        Assert.Contains("AdminTiersUpdate", rpc.Calls);
        Assert.Equal(("free", 2048L, 4096L, 5L, 1024L, 9L), rpc.LastTierUpdate);
        // Refetched fauna.admin.tiers.list so the row re-renders from persisted
        // state (admin.md § 3 — load + post-save reload = 2 list calls).
        Assert.Equal(2, rpc.Calls.Count(c => c == "AdminTiersList"));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task SaveTier_SurfacesErrorToErrorMessage()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminTiers = new[] { MockNestRpcClient.MakeAdminTier("free") },
        };
        var vm = new AdminSettingsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        var row = Assert.Single(vm.Tiers);

        rpc.NextError = "boom";
        await vm.SaveTierAsync(row);

        Assert.NotNull(vm.ErrorMessage);
    }

    [Theory]
    [InlineData("42", 7L, 42L)]    // parses (trims), clamps non-negative
    [InlineData("  42  ", 7L, 42L)]
    [InlineData("0", 7L, 0L)]      // 0 is a valid cap ("no allowance") — unlike a port
    [InlineData("+9", 7L, 9L)]     // leading '+' is a valid signed-int literal
    [InlineData("", 7L, 7L)]       // empty → fall back to prev
    [InlineData("abc", 7L, 7L)]    // unparseable → fall back to prev (no silent zero)
    [InlineData("1.5", 7L, 7L)]    // fractional → fall back to prev (shared parse_cap)
    [InlineData("9223372036854775808", 7L, 7L)] // i64::MAX + 1 overflows → prev
    [InlineData("8 443", 7L, 7L)]  // interior whitespace → prev (shared parse_cap)
    [InlineData("-3", 7L, 0L)]     // negative → clamp to 0
    public void ParseCap_FallsBackToPrev_AndClampsNonNegative(string text, long prev, long expected)
    {
        Assert.Equal(expected, AdminSettingsViewModel.ParseCap(text, prev));
    }

    [Fact]
    public async Task Load_SurfacesErrorsToErrorMessage()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new AdminSettingsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.NotNull(vm.ErrorMessage);
        Assert.Empty(vm.Tiers);
        Assert.False(vm.IsLoading);
    }

    // ── Membership designations (monetization.md § Pillar 4) ────────────
    //
    // A LINK editor between the admin's own subscription tiers and this page's
    // quota tiers — never a third tier list (monetization.md:115). The row set is
    // every OWNED subscription tier, designated or not, which is what makes the
    // section double as "what you could designate".

    private static MockNestRpcClient MembershipRpc(
        string[] ownedTiers, params FfiAdminMembershipTier[] designations) =>
        new()
        {
            NextAdminTiers = new[]
            {
                MockNestRpcClient.MakeAdminTier("free"),
                MockNestRpcClient.MakeAdminTier("personal"),
                MockNestRpcClient.MakeAdminTier("community"),
            },
            NextTiers = ownedTiers.Select(n => MockNestRpcClient.MakeTier(n)).ToList(),
            NextMembershipTiers = designations,
        };

    [Fact]
    public async Task Load_RendersOneMembershipRowPerOwnedSubscriptionTier_DesignatedOrNot()
    {
        var rpc = MembershipRpc(
            new[] { "bronze", "gold" },
            MockNestRpcClient.MakeMembershipTier("gold", adminTier: "personal", lapseTier: "community"));
        var vm = new AdminSettingsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        // One row per OWNED tier — the undesignated "bronze" gets a row too.
        Assert.Equal(new[] { "bronze", "gold" }, vm.MembershipTiers.Select(m => m.TierName));

        var bronze = vm.MembershipTiers[0];
        Assert.False(bronze.IsDesignated);
        Assert.Equal("bronze", bronze.SelectedTierName);
        Assert.Equal("", bronze.AdminTier);          // nothing admitted-at chosen yet
        Assert.Equal("free", bronze.LapseTier);      // the shared DEFAULT_LAPSE_TIER

        var gold = vm.MembershipTiers[1];
        Assert.True(gold.IsDesignated);
        Assert.Equal("personal", gold.AdminTier);
        Assert.Equal("community", gold.LapseTier);

        // Both selects offer exactly the quota tiers this page defines, so a row can
        // never name a quota tier that does not exist.
        Assert.Equal(new[] { "free", "personal", "community" }, vm.QuotaTierNames);
    }

    [Fact]
    public void DefaultLapseTier_ComesFromSharedRust_NotAHardCodedLiteral()
    {
        // Reads fauna_protocol::admin::DEFAULT_LAPSE_TIER over FFI (priority #2) —
        // linux reads the constant directly; apple/android still mirror it privately.
        Assert.Equal("free", AdminSettingsViewModel.DefaultLapseTier);
    }

    [Fact]
    public async Task Load_NoOwnedSubscriptionTiers_IsEmptySectionNotError()
    {
        // The normal out-of-the-box state: nothing about the nest is monetized.
        var rpc = MembershipRpc(Array.Empty<string>());
        var vm = new AdminSettingsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Empty(vm.MembershipTiers);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task Load_OwnTiersReadFails_DegradesToEmptySection_KeepingTierDefinitions()
    {
        // Mirrors linux fetch_own_membership_tier_names: a payee whose own-tier read
        // fails has nothing to designate, so the section shows its empty state rather
        // than blowing up the whole page (the tier-definition editor still renders).
        var rpc = new MockNestRpcClient
        {
            NextAdminTiers = new[] { MockNestRpcClient.MakeAdminTier("free") },
            FailSubscriptionTiersList = true,
        };
        var vm = new AdminSettingsViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Empty(vm.MembershipTiers);
        Assert.Null(vm.ErrorMessage);
        Assert.Single(vm.Tiers);
    }

    [Fact]
    public async Task SaveMembership_UpsertsCurrentSelection_ThenRefetches()
    {
        var rpc = MembershipRpc(new[] { "bronze" });
        var vm = new AdminSettingsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.MembershipTiers);
        row.AdminTier = "personal";
        row.LapseTier = "community";

        await vm.SaveMembershipAsync(row);

        // The upsert carries the row's CURRENT selection + both linked quota tiers.
        Assert.Equal(("bronze", "personal", "community"), rpc.LastMembershipSet);
        // Refetched so the row re-renders from persisted state (load + post-save).
        Assert.Equal(2, rpc.Calls.Count(c => c == "AdminMembershipTiersList"));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task SaveMembership_SendsRepointedTierName_NotTheRowsOriginalIdentity()
    {
        // The -tier-select is a real select, so an admin can fix a mis-set row.
        // Save must read the current selection (linux dropdown_tier at save time).
        var rpc = MembershipRpc(new[] { "bronze", "gold" });
        var vm = new AdminSettingsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        var row = vm.MembershipTiers[0];
        row.SelectedTierName = "gold";
        row.AdminTier = "personal";

        await vm.SaveMembershipAsync(row);

        Assert.Equal("gold", rpc.LastMembershipSet!.Value.TierName);
    }

    [Fact]
    public async Task ClearMembership_KeysOffTheRowsOwnTier_ThenRefetches()
    {
        var rpc = MembershipRpc(
            new[] { "gold" }, MockNestRpcClient.MakeMembershipTier("gold"));
        var vm = new AdminSettingsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.MembershipTiers);
        // Even mid-re-point, Clear drops the designation that actually exists.
        row.SelectedTierName = "something-else";

        await vm.ClearMembershipAsync(row);

        Assert.Equal("gold", rpc.LastMembershipClear);
        Assert.Equal(2, rpc.Calls.Count(c => c == "AdminMembershipTiersList"));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task SaveMembership_SurfacesErrorToErrorMessage()
    {
        var rpc = MembershipRpc(new[] { "bronze" });
        var vm = new AdminSettingsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        var row = Assert.Single(vm.MembershipTiers);

        // A tier_name the admin doesn't own is fauna.admin.not_found; an unknown
        // quota tier is fauna.admin.invalid_params — both must reach error-message.
        rpc.NextError = "fauna.admin.not_found";
        await vm.SaveMembershipAsync(row);

        Assert.NotNull(vm.ErrorMessage);
    }
}
