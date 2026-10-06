using Xunit;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

// ── ProfileViewModel Tests — subscriptions Slice A (profile Tiers-tab SELF
// author management). monetization.md § Pillar 1 + profile.md. The VM mirrors
// apple SubscriptionsVM / linux tiers.rs: observer-free, a manual re-read after
// each mutation; the three mint-bearing actions (create / approve / remove) go
// through the author-orchestration seam methods. ──

public class ProfileViewModelTests
{
    private static readonly byte[] Alice = Enumerable.Repeat((byte)0xa1, 32).ToArray();
    private static readonly byte[] Bob = Enumerable.Repeat((byte)0xb2, 32).ToArray();

    private static MockNestRpcClient SeededRpc() => new()
    {
        NextTiers = new[]
        {
            MockNestRpcClient.MakeTier("gold", rank: 2, priceHint: "$5", askingPriceSats: 2500, autoApprove: true),
            MockNestRpcClient.MakeTier("silver", rank: 1),
        },
        NextRequests = new[]
        {
            MockNestRpcClient.MakePendingRequest(7, Alice, "gold", "subscribe"),
        },
        NextSubscribers = new[]
        {
            MockNestRpcClient.MakeSubscriber(Bob),
        },
    };

    [Fact]
    public async Task HydrateAsync_LoadsTiersRequestsAndRoster()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.Equal(2, vm.Tiers.Count);
        Assert.Equal("gold", vm.Tiers[0].Name);
        Assert.True(vm.Tiers[0].AutoApprove);
        Assert.Single(vm.Requests);
        Assert.Equal("subscribe", vm.Requests[0].Kind);
        // §3 picker populated; selection defaults to the first tier and its roster loads.
        Assert.Equal(new[] { "gold", "silver" }, vm.TierNames);
        Assert.Equal("gold", vm.SelectedTier);
        Assert.Single(vm.Subscribers);
        Assert.Equal("gold", rpc.LastSubscribersListTier);
        Assert.Contains("SubscriptionTiersList", rpc.Calls);
        Assert.Contains("SubscriptionRequestsList", rpc.Calls);
        Assert.Contains("SubscriptionSubscribersList", rpc.Calls);
    }

    [Fact]
    public async Task HydrateAsync_RendersSubscriberAsLowercaseHex()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.Equal(Convert.ToHexString(Bob).ToLowerInvariant(), vm.Subscribers[0].Handle);
        Assert.Equal(Convert.ToHexString(Alice).ToLowerInvariant(), vm.Requests[0].Subscriber);
    }

    [Fact]
    public async Task HydrateAsync_PreservesSelectedTierWhenStillPresent()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.SelectTierAsync("silver");
        await vm.HydrateAsync();

        Assert.Equal("silver", vm.SelectedTier);
    }

    [Fact]
    public async Task HydrateAsync_EmptyTiersClearsRoster()
    {
        var rpc = new MockNestRpcClient(); // no tiers / requests / subscribers
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.Empty(vm.Tiers);
        Assert.Empty(vm.TierNames);
        Assert.Equal("", vm.SelectedTier);
        Assert.Empty(vm.Subscribers);
        // No tier → no roster read.
        Assert.DoesNotContain("SubscriptionSubscribersList", rpc.Calls);
    }

    [Fact]
    public void OpenCreateForm_ShowsEmptyForm()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");

        vm.OpenCreateForm();

        Assert.True(vm.ShowForm);
        Assert.Null(vm.EditingTier);
        Assert.Equal("", vm.FormName);
        Assert.Equal("", vm.FormRank);
        Assert.False(vm.FormAutoApprove);
    }

    [Fact]
    public void OpenEditForm_PopulatesFormFromRow()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");
        var row = new SubscriptionTierRow("gold", 3, "desc", "$9", "https://pay", true, 5000ul);

        vm.OpenEditForm(row);

        Assert.True(vm.ShowForm);
        Assert.Equal("gold", vm.EditingTier);
        Assert.Equal("gold", vm.FormName);
        Assert.Equal("3", vm.FormRank);
        Assert.Equal("desc", vm.FormDescription);
        Assert.Equal("$9", vm.FormPriceHint);
        Assert.Equal("https://pay", vm.FormPaymentUrl);
        Assert.True(vm.FormAutoApprove);
        // Pre-fill from the row's current price (monetization.md § The asking
        // price → Editability) — leaving this UNCHANGED round-trips it.
        Assert.Equal("5000", vm.FormAskingPriceSats);
    }

    [Fact]
    public void CancelForm_HidesForm()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");
        vm.OpenCreateForm();

        vm.CancelForm();

        Assert.False(vm.ShowForm);
        Assert.Null(vm.EditingTier);
    }

    [Fact]
    public async Task SaveFormAsync_CreateDispatchesCreateWithFormValues()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        vm.OpenCreateForm();
        vm.FormName = "gold";
        vm.FormRank = "5";
        vm.FormDescription = "best tier";
        vm.FormPriceHint = "$5";
        vm.FormPaymentUrl = "https://pay";
        vm.FormAutoApprove = true;

        await vm.SaveFormAsync();

        Assert.Contains("SubscriptionTierCreate", rpc.Calls);
        Assert.DoesNotContain("SubscriptionTierUpdate", rpc.Calls);
        Assert.Equal(("gold", 5u, "best tier", "$5", "https://pay", true, (ulong?)null), rpc.LastSubscriptionTierCreate);
        Assert.False(vm.ShowForm);
    }

    [Fact]
    public async Task SaveFormAsync_ParsesAskingPriceSats()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        vm.OpenCreateForm();
        vm.FormName = "gold";
        vm.FormAskingPriceSats = "5000";

        await vm.SaveFormAsync();

        Assert.Equal(5000ul, rpc.LastSubscriptionTierCreate?.AskingPriceSats);
    }

    [Fact]
    public async Task SaveFormAsync_EmptyOrUnparseableAskingPriceMeansNone()
    {
        // Empty OR unparseable both mean "no machine price" — never an error at
        // this layer (mirrors tui's profile/mod.rs reference leg exactly), and on
        // the EDIT path this is what makes an untouched field "keep current"
        // rather than "clear" (monetization.md § The asking price → Editability).
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        vm.OpenCreateForm();
        vm.FormName = "gold";
        vm.FormAskingPriceSats = "not-a-number";

        await vm.SaveFormAsync();

        Assert.Null(rpc.LastSubscriptionTierCreate?.AskingPriceSats);
    }

    [Fact]
    public async Task SaveFormAsync_BlankOptionalFieldsBecomeNull()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        vm.OpenCreateForm();
        vm.FormName = "gold";
        vm.FormRank = "1";
        // description / price / payment-url left blank

        await vm.SaveFormAsync();

        Assert.Equal(("gold", 1u, (string?)null, (string?)null, (string?)null, false, (ulong?)null), rpc.LastSubscriptionTierCreate);
    }

    [Fact]
    public async Task SaveFormAsync_EditDispatchesUpdate()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();
        vm.OpenEditForm(vm.Tiers[0]); // gold
        vm.FormRank = "9";

        await vm.SaveFormAsync();

        Assert.Contains("SubscriptionTierUpdate", rpc.Calls);
        Assert.DoesNotContain("SubscriptionTierCreate", rpc.Calls);
        Assert.Equal("gold", rpc.LastSubscriptionTierUpdate?.Name);
        Assert.Equal(9u, rpc.LastSubscriptionTierUpdate?.Rank);
        // Pre-filled from the row (SeededRpc's "gold" carries 2500) and left
        // UNCHANGED — the value round-trips through the edit form (this row's
        // own success criterion), never silently dropped just because the page
        // edited an unrelated field (Rank).
        Assert.Equal(2500ul, rpc.LastSubscriptionTierUpdate?.AskingPriceSats);
    }

    [Fact]
    public async Task SaveFormAsync_EditWithClearedFieldKeepsCurrentPrice()
    {
        // monetization.md § The asking price → Editability: no client exposes a
        // clear verb yet, so clearing the field on an edit must send `null`
        // (KEEP current), never a signal to erase — the same computation as
        // "empty means no price" on create, but with the opposite real-world
        // effect because `tiers.update` reads `null` as keep-current.
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();
        vm.OpenEditForm(vm.Tiers[0]); // gold, asking price 2500
        vm.FormAskingPriceSats = "";

        await vm.SaveFormAsync();

        Assert.Null(rpc.LastSubscriptionTierUpdate?.AskingPriceSats);
    }

    [Fact]
    public async Task SaveFormAsync_EmptyNameIsNoop()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        vm.OpenCreateForm();
        vm.FormName = "   ";

        await vm.SaveFormAsync();

        Assert.DoesNotContain("SubscriptionTierCreate", rpc.Calls);
    }

    [Fact]
    public async Task DeleteTierAsync_DispatchesDelete()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.DeleteTierAsync("gold");

        Assert.Contains("SubscriptionTierDelete", rpc.Calls);
        Assert.Equal("gold", rpc.LastSubscriptionTierDelete);
    }

    [Fact]
    public async Task ApproveAsync_SetsBusyDispatchesApproveWithRawRequestAndRehydrates()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.ApproveAsync(7);

        Assert.Contains("SubscriptionRequestApprove", rpc.Calls);
        Assert.Equal(7, rpc.LastApprovedRequest?.requestId);
        Assert.Equal("gold", rpc.LastApprovedRequest?.tierName);
        Assert.False(vm.IsApproving); // toggled back off after the mint completes
    }

    [Fact]
    public async Task RejectAsync_DispatchesReject()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.RejectAsync(7);

        Assert.Contains("SubscriptionRequestReject", rpc.Calls);
        Assert.Equal(7, rpc.LastRejectedRequest);
    }

    [Fact]
    public async Task SelectTierAsync_RefreshesRosterForSelectedTier()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.SelectTierAsync("silver");

        Assert.Equal("silver", vm.SelectedTier);
        Assert.Equal("silver", rpc.LastSubscribersListTier);
    }

    [Fact]
    public async Task RemoveSubscriberAsync_DispatchesRemoveWithTierAndActorId()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.RemoveSubscriberAsync(Bob, "gold");

        Assert.Contains("SubscriptionSubscriberRemove", rpc.Calls);
        Assert.Equal("gold", rpc.LastSubscriberRemove?.TierName);
        Assert.Equal(Bob, rpc.LastSubscriberRemove?.SubscriberId);
    }

    [Fact]
    public async Task HydrateAsync_ErrorSurfacesToErrorMessage()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.NotNull(vm.ErrorMessage);
    }

#if PAYMENTS
// Gated with the plane under test — dynamic-features.md § The feature-matrix
// test story: both flavors build and run their test subset, and a store-safe
// build has no payments members to drive.
    // ── §4 Payment providers + §5 Manual claim codes (monetization.md § Pillar 3,
    // IDs reserved 2026-07-12). Pure FfiPaymentsClient pass-throughs; the VM's own
    // job is the §4 form state (kind/secret/tier-map + the live webhook-URL
    // preview) and the §5 claim-status resolve (redeemed wins over voided). ──

    [Fact]
    public async Task HydrateAsync_LoadsProvidersAndClaims()
    {
        var rpc = SeededRpc();
        rpc.NextProviders = new[] { MockNestRpcClient.MakeProvider("fake", "gold") };
        rpc.NextClaims = new[] { MockNestRpcClient.MakeClaim("CODE1", "gold") };
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.Single(vm.Providers);
        Assert.Equal("fake", vm.Providers[0].Kind);
        Assert.Equal("gold", vm.Providers[0].Tier);
        // The shared evidence-based decision (fauna_core::format::provider_status_label):
        // no verified/rejected stamp yet → "configured". No IStringLocalizer is
        // registered in this unit-test host, so Status resolves to the raw
        // (unsubstituted) i18n key rather than a rendered string (mirrors
        // TaskDelegationViewModelTests's RunnerText contract).
        Assert.Equal("subscriptions.provider_status_configured", vm.Providers[0].Status);
        Assert.Single(vm.Claims);
        Assert.Equal("CODE1", vm.Claims[0].Code);
        Assert.Contains("PaymentsProvidersList", rpc.Calls);
        Assert.Contains("PaymentsClaimsList", rpc.Calls);
    }

#endif   // PAYMENTS

    [Fact]
    public async Task HydrateAsync_RequestPaymentEntitledFlowsThrough()
    {
        var rpc = new MockNestRpcClient
        {
            NextRequests = new[] { MockNestRpcClient.MakePendingRequest(1, Alice, "gold", paymentEntitled: true) },
        };
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.True(vm.Requests[0].PaymentEntitled);
    }

#if PAYMENTS
// Gated with the plane under test — dynamic-features.md § The feature-matrix
// test story: both flavors build and run their test subset, and a store-safe
// build has no payments members to drive.
    // HydrateAsync_ClaimStatusResolvesRedeemedOverVoided lives in
    // ProfileViewModelClaimStatusTests.cs — it needs Strings.Initialize (the
    // [Collection("StringsGlobal")] serialization), which would otherwise force
    // this whole (large, parallel-friendly) class onto that collection too.

    [Fact]
    public async Task OpenProviderForm_DefaultsKindAndTierAndClearsSecret()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        vm.OpenProviderForm();

        Assert.True(vm.ShowProviderForm);
        Assert.Equal("", vm.ProviderFormSecret);
        Assert.Equal("gold", vm.ProviderFormTier);   // first tier (§1 order)
        Assert.NotEmpty(vm.KnownProviderKinds);
        Assert.Equal(vm.KnownProviderKinds[0], vm.ProviderFormKind);
        Assert.False(string.IsNullOrEmpty(vm.ProviderWebhookUrl));
    }

    [Fact]
    public void CancelProviderForm_HidesForm()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");
        vm.OpenProviderForm();

        vm.CancelProviderForm();

        Assert.False(vm.ShowProviderForm);
    }

    [Fact]
    public void SettingProviderFormKind_RecomputesWebhookUrlPreview()
    {
        var rpc = new MockNestRpcClient { HomeUrl = "https://nest.example" };
        var vm = new ProfileViewModel(rpc, "abcd1234");

        vm.ProviderFormKind = "fake";

        Assert.Contains("abcd1234", vm.ProviderWebhookUrl);
        Assert.Contains("fake", vm.ProviderWebhookUrl);
        Assert.StartsWith("https://nest.example", vm.ProviderWebhookUrl);
    }

    [Fact]
    public async Task SaveProviderFormAsync_DispatchesSetWithFormValuesAndHidesForm()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();
        vm.OpenProviderForm();
        vm.ProviderFormKind = "fake";
        vm.ProviderFormSecret = "whsec_test";
        vm.ProviderFormTier = "silver";

        await vm.SaveProviderFormAsync();

        Assert.Contains("PaymentsProvidersSet", rpc.Calls);
        Assert.Equal(("fake", "whsec_test", "silver"), rpc.LastProvidersSet);
        Assert.False(vm.ShowProviderForm);
        // The secret is a credential — don't leave it bound after the nest has it.
        Assert.Equal("", vm.ProviderFormSecret);
    }

    [Fact]
    public async Task SaveProviderFormAsync_BlankTierIsNoop()
    {
        var rpc = new MockNestRpcClient(); // no tiers
        var vm = new ProfileViewModel(rpc, "deadbeef");
        vm.OpenProviderForm(); // ProviderFormTier stays "" — no tiers exist

        await vm.SaveProviderFormAsync();

        Assert.DoesNotContain("PaymentsProvidersSet", rpc.Calls);
    }

    [Fact]
    public async Task RemoveProviderAsync_DispatchesRemoveWithKindAndRehydrates()
    {
        var rpc = SeededRpc();
        rpc.NextProviders = new[] { MockNestRpcClient.MakeProvider("fake", "gold") };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();

        await vm.RemoveProviderAsync("fake");

        Assert.Contains("PaymentsProvidersRemove", rpc.Calls);
        Assert.Equal("fake", rpc.LastProvidersRemove);
    }

    [Fact]
    public async Task MintClaimAsync_DispatchesMintWithSelectedTierAndRehydrates()
    {
        var rpc = SeededRpc();
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync();
        vm.ClaimTier = "silver";

        await vm.MintClaimAsync();

        Assert.Contains("PaymentsClaimsMint", rpc.Calls);
        Assert.Equal(("silver", (ulong?)null), rpc.LastClaimsMint);
    }

    [Fact]
    public async Task MintClaimAsync_BlankTierIsNoop()
    {
        var rpc = new MockNestRpcClient(); // no tiers
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.HydrateAsync(); // ClaimTier stays ""

        await vm.MintClaimAsync();

        Assert.DoesNotContain("PaymentsClaimsMint", rpc.Calls);
    }

#endif   // PAYMENTS

    // ── OTHER-profile offers browse (monetization.md § Pillar 1 surface 2;
    // profile.md § Layout & flow → Another's profile). The FFI twin of linux
    // apps/fauna-linux/src/views/profile/offers.rs: load offered tiers (excluding
    // the free "followers" tier) + status, subscribe per-row, follow. ──

    private static readonly string BobHex = Convert.ToHexString(Bob).ToLowerInvariant();

    [Fact]
    public async Task HydrateAsync_OtherProfile_LoadsOffersExcludingFollowersAndSkipsAuthorMgmt()
    {
        var rpc = new MockNestRpcClient
        {
            NextOffers = new[]
            {
                MockNestRpcClient.MakeTier("followers", rank: 0),
                MockNestRpcClient.MakeTier("gold", rank: 2, priceHint: "$5", description: "best", paymentUrl: "https://pay"),
                MockNestRpcClient.MakeTier("silver", rank: 1),
            },
            NextStatus = MockNestRpcClient.MakeStatus(tier: null),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.HydrateAsync();

        Assert.False(vm.IsSelf);
        Assert.Equal(2, vm.Offers.Count);             // "followers" excluded
        Assert.Equal("gold", vm.Offers[0].Name);
        Assert.Equal("$5", vm.Offers[0].PriceHint);
        Assert.Equal("best", vm.Offers[0].Description);
        Assert.True(vm.Offers[0].HasPaymentUrl);
        Assert.Equal(uniffi.fauna_core.OfferStatus.None, vm.Offers[0].Status);
        Assert.Equal(Bob, rpc.LastOffersAuthorId);
        Assert.Contains("SubscriptionOffersList", rpc.Calls);
        // OTHER never loads the SELF author-management sections.
        Assert.DoesNotContain("SubscriptionTiersList", rpc.Calls);
        Assert.DoesNotContain("SubscriptionRequestsList", rpc.Calls);
    }

    [Fact]
    public async Task HydrateAsync_OtherProfile_MapsHeldTierToActive()
    {
        var rpc = new MockNestRpcClient
        {
            NextOffers = new[] { MockNestRpcClient.MakeTier("gold"), MockNestRpcClient.MakeTier("silver") },
            NextStatus = MockNestRpcClient.MakeStatus(tier: "gold"),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.HydrateAsync();

        Assert.Equal(uniffi.fauna_core.OfferStatus.Active, vm.Offers.Single(o => o.Name == "gold").Status);
        Assert.Equal(uniffi.fauna_core.OfferStatus.None, vm.Offers.Single(o => o.Name == "silver").Status);
        Assert.Contains("SubscriptionStatusGet", rpc.Calls);
    }

    [Fact]
    public async Task SubscribeToOfferAsync_Approved_ReReadsOffersAndShowsActive()
    {
        var rpc = new MockNestRpcClient
        {
            NextOffers = new[] { MockNestRpcClient.MakeTier("gold") },
            NextSubscribeReply = new FfiSubscribeReply.Approved("gold", null),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);
        await vm.HydrateAsync();
        rpc.NextStatus = MockNestRpcClient.MakeStatus(tier: "gold");   // post-approval the read reports it

        await vm.SubscribeToOfferAsync("gold");

        Assert.Contains("SubscriptionSubscribe", rpc.Calls);
        Assert.Equal("gold", rpc.LastSubscribe?.Tier);
        Assert.Equal(Bob, rpc.LastSubscribe?.AuthorId);
        Assert.Equal(uniffi.fauna_core.OfferStatus.Active, vm.Offers.Single().Status);
        // Approved re-reads (offers loaded once on hydrate, once after subscribe).
        Assert.Equal(2, rpc.Calls.Count(c => c == "SubscriptionOffersList"));
    }

    [Fact]
    public async Task SubscribeToOfferAsync_Queued_FlipsRowToPendingWithoutReRead()
    {
        var rpc = new MockNestRpcClient
        {
            NextOffers = new[] { MockNestRpcClient.MakeTier("gold") },
            NextSubscribeReply = new FfiSubscribeReply.Queued(7),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);
        await vm.HydrateAsync();

        await vm.SubscribeToOfferAsync("gold");

        Assert.Equal(uniffi.fauna_core.OfferStatus.Pending, vm.Offers.Single().Status);
        // Queued does NOT re-read (status.get won't report a pending tier).
        Assert.Equal(1, rpc.Calls.Count(c => c == "SubscriptionOffersList"));
    }

    [Fact]
    public async Task FollowAsync_SubscribesToFollowersTierAndFlips()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.FollowAsync();

        Assert.Contains("SubscriptionSubscribe", rpc.Calls);
        Assert.Equal("followers", rpc.LastSubscribe?.Tier);
        Assert.Equal(Bob, rpc.LastSubscribe?.AuthorId);
        Assert.True(vm.IsFollowing);
    }

    // ── OTHER-profile secondary relationship action: Block⇄Unblock toggle.
    // profile.md § User actions + § Where logic lives → block; contacts.md § Where
    // logic lives → Unblock. The FFI twin of linux apps/fauna-linux/src/views/
    // profile/mod.rs {refresh_block_state, toggle_block}: read the edge on open
    // (fauna.contacts.list), then tap knocks_block (not blocked) / knocks_unblock
    // (blocked, the guarded clear-the-edge) and flip the label. ──

    private static ContactInfo BlockedEdge(byte[] peer) =>
        new(Convert.ToHexString(peer).ToLowerInvariant(), null, ContactStatus.Blocked, null);

    private static ContactInfo AcceptedEdge(byte[] peer) =>
        new(Convert.ToHexString(peer).ToLowerInvariant(), null, ContactStatus.Accepted, null);

    [Fact]
    public async Task RefreshBlockStateAsync_BlockedEdge_SetsIsBlocked()
    {
        var rpc = new MockNestRpcClient { NextContacts = new[] { BlockedEdge(Bob) } };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.RefreshBlockStateAsync();

        Assert.Contains("ContactsList", rpc.Calls);
        Assert.True(vm.IsBlocked);
    }

    [Fact]
    public async Task RefreshBlockStateAsync_NonBlockedEdge_LeavesNotBlocked()
    {
        var rpc = new MockNestRpcClient { NextContacts = new[] { AcceptedEdge(Bob) } };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.RefreshBlockStateAsync();

        Assert.False(vm.IsBlocked);
    }

    [Fact]
    public async Task RefreshBlockStateAsync_OtherActorBlocked_LeavesNotBlocked()
    {
        // A `blocked` edge for a DIFFERENT actor must not flip THIS profile's toggle.
        var rpc = new MockNestRpcClient { NextContacts = new[] { BlockedEdge(Alice) } };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.RefreshBlockStateAsync();

        Assert.False(vm.IsBlocked);
    }

    [Fact]
    public async Task RefreshBlockStateAsync_ReadFailure_LeavesDefaultNoThrow()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.RefreshBlockStateAsync();   // non-fatal: leaves the "Block" default

        Assert.False(vm.IsBlocked);
        Assert.Null(vm.ErrorMessage);        // a block-state read failure is silent
    }

    [Fact]
    public async Task ToggleBlockAsync_NotBlocked_BlocksPeerAndFlips()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.ToggleBlockAsync();

        Assert.Contains("KnocksBlock", rpc.Calls);
        Assert.DoesNotContain("KnocksUnblock", rpc.Calls);
        Assert.Equal(BobHex, rpc.LastKnockPeer);
        Assert.True(vm.IsBlocked);
    }

    [Fact]
    public async Task ToggleBlockAsync_Blocked_UnblocksPeerAndFlips()
    {
        var rpc = new MockNestRpcClient { NextContacts = new[] { BlockedEdge(Bob) } };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);
        await vm.RefreshBlockStateAsync();   // opens already blocked
        Assert.True(vm.IsBlocked);

        await vm.ToggleBlockAsync();

        Assert.Contains("KnocksUnblock", rpc.Calls);
        Assert.Equal(BobHex, rpc.LastKnockPeer);
        Assert.False(vm.IsBlocked);
    }

    [Fact]
    public async Task ToggleBlockAsync_ErrorSurfacesToErrorMessageAndDoesNotFlip()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.ToggleBlockAsync();

        Assert.NotNull(vm.ErrorMessage);
        Assert.False(vm.IsBlocked);
    }

    [Fact]
    public async Task LoadOffersAsync_ErrorSurfacesToErrorMessage()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false);

        await vm.HydrateAsync();

        Assert.NotNull(vm.ErrorMessage);
    }

    // ── Profile edit form (display-name / bio / links read-modify-write).
    // profile.md § State & data shape; the FFI read-modify-write twin of the linux
    // apps/fauna-linux/src/views/profile/edit.rs submit/refresh branches. ──

    private static readonly byte[] Body = { 1, 2, 3, 4 };

    [Fact]
    public async Task OpenEditFormAsync_PrefillsFromFetchedProfile()
    {
        var rpc = new MockNestRpcClient
        {
            NextProfile = MockNestRpcClient.MakeProfile(
                displayName: "Ada", bio: "Maths",
                links: new[] { new ProfileLinkRow("site", "https://ada") },
                rawBody: Body),
        };
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.OpenEditFormAsync();

        Assert.True(vm.ShowEditForm);
        Assert.Equal("Ada", vm.EditDisplayName);
        Assert.Equal("Maths", vm.EditBio);
        Assert.Single(vm.EditLinks);
        Assert.Equal("site", vm.EditLinks[0].Label);
        Assert.Equal("https://ada", vm.EditLinks[0].Uri);
        Assert.Contains("LoadProfileEditBase", rpc.Calls);
    }

    [Fact]
    public async Task OpenEditFormAsync_NotFound_OpensEmptyForm()
    {
        var rpc = new MockNestRpcClient { NextProfile = null }; // unpublished
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.OpenEditFormAsync();

        Assert.True(vm.ShowEditForm);
        Assert.Equal("", vm.EditDisplayName);
        Assert.Equal("", vm.EditBio);
        Assert.Empty(vm.EditLinks);
    }

    [Fact]
    public async Task SaveEditAsync_PublishesEditedDisplayFieldsAndClosesForm()
    {
        var rpc = new MockNestRpcClient
        {
            NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body), // opened base body
        };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.EditDisplayName = "Ada Lovelace";
        vm.EditBio = "Mathematician";
        // The header refresh re-reads → make the get now return the published name.
        rpc.NextProfile = MockNestRpcClient.MakeProfile(displayName: "Ada Lovelace", rawBody: Body);

        await vm.SaveEditAsync();

        Assert.Equal("Ada Lovelace", rpc.LastProfileSet?.Display.DisplayName);
        Assert.Equal("Mathematician", rpc.LastProfileSet?.Display.Bio);
        Assert.Equal(Body, rpc.LastProfileSet?.BaseBody);
        Assert.False(vm.ShowEditForm);
        Assert.Equal("Ada Lovelace", vm.HeaderName);
        Assert.Contains("ProfileSet", rpc.Calls);
    }

    [Fact]
    public async Task SaveEditAsync_ErrorSurfacesToErrorMessage()
    {
        var rpc = new MockNestRpcClient
        {
            NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body),
            NextProfileSetThrows = true,
        };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.EditDisplayName = "Ada";

        await vm.SaveEditAsync();

        Assert.NotNull(vm.ErrorMessage);
        Assert.True(vm.ShowEditForm); // stays open on failure
    }

    [Fact]
    public void AddLink_AppendsEmptyRow()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");

        vm.AddLink();

        Assert.Single(vm.EditLinks);
        Assert.Equal("", vm.EditLinks[0].Label);
        Assert.Equal("", vm.EditLinks[0].Uri);
    }

    [Fact]
    public void RemoveLink_RemovesRow()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");
        vm.AddLink();
        var row = vm.EditLinks[0];

        vm.RemoveLink(row);

        Assert.Empty(vm.EditLinks);
    }

    [Fact]
    public async Task RefreshHeaderAsync_FallsBackToActorIdWhenUnpublished()
    {
        var rpc = new MockNestRpcClient { NextProfile = null }; // unpublished
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.RefreshHeaderAsync();

        Assert.Equal("deadbeef", vm.HeaderName);
    }

    [Fact]
    public async Task SaveEditAsync_DropsFullyEmptyLinkRows()
    {
        var rpc = new MockNestRpcClient
        {
            NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body),
        };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.EditLinks.Add(new ProfileLinkRow("", ""));            // fully-empty → dropped
        vm.EditLinks.Add(new ProfileLinkRow("site", "https://x")); // real → kept

        await vm.SaveEditAsync();

        var links = rpc.LastProfileSet?.Display.Links;
        Assert.NotNull(links);
        Assert.Single(links!);
        Assert.Equal("site", links![0].Label);
    }

    // ── Avatar / banner (profile.md § Where logic lives → Field ownership) ──
    // The page owns the OS picker + reads bytes on pick; these tests exercise
    // the VM's staging + Save-time resolution only (StageAvatar/ClearAvatar and
    // SaveEditAsync's Keep/Clear/Set priority — mirrors linux edit.rs's
    // pending_image tests).

    private static readonly byte[] Avatar1 = { 9, 9, 9 };
    private static readonly byte[] Banner1 = { 7, 7, 7 };

    [Fact]
    public async Task SaveEditAsync_UntouchedAvatarAndBanner_ResolveToKeep()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body) };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();

        await vm.SaveEditAsync();

        Assert.IsType<FfiProfileImageEdit.Keep>(rpc.LastProfileSet?.Avatar);
        Assert.IsType<FfiProfileImageEdit.Keep>(rpc.LastProfileSet?.Banner);
    }

    [Fact]
    public async Task SaveEditAsync_StagedAvatarAndBanner_UploadsAndResolvesToSet()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body) };
        var http = new MockNestHttpClient { NextUploadedBlobHash = "ab" + new string('0', 62) };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.StageAvatar(Avatar1);
        vm.StageBanner(Banner1);

        await vm.SaveEditAsync(http);

        Assert.Equal(2, http.UploadBlobCalls.Count);
        Assert.Contains(http.UploadBlobCalls, c => c.Data.SequenceEqual(Avatar1) && c.Audience is UploadAudience.PublicPost);
        Assert.Contains(http.UploadBlobCalls, c => c.Data.SequenceEqual(Banner1) && c.Audience is UploadAudience.PublicPost);
        var avatar = Assert.IsType<FfiProfileImageEdit.Set>(rpc.LastProfileSet?.Avatar);
        Assert.Equal(http.NextUploadedBlobHash, avatar.blobHashHex);
        var banner = Assert.IsType<FfiProfileImageEdit.Set>(rpc.LastProfileSet?.Banner);
        Assert.Equal(http.NextUploadedBlobHash, banner.blobHashHex);
    }

    [Fact]
    public async Task SaveEditAsync_ClearedAvatar_ResolvesToClear()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body) };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.ClearAvatar();

        await vm.SaveEditAsync();

        Assert.IsType<FfiProfileImageEdit.Clear>(rpc.LastProfileSet?.Avatar);
        Assert.IsType<FfiProfileImageEdit.Keep>(rpc.LastProfileSet?.Banner); // untouched
    }

    [Fact]
    public void StageAvatar_OverridesAPendingClear()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), "deadbeef");

        vm.ClearAvatar();
        Assert.True(vm.EditAvatarClear);

        vm.StageAvatar(Avatar1);
        Assert.False(vm.EditAvatarClear); // a fresh pick always wins over a pending clear
    }

    [Fact]
    public async Task SaveEditAsync_StagedAvatarWithNoHttpClient_FailsAndKeepsFormOpen()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body) };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.StageAvatar(Avatar1);

        await vm.SaveEditAsync(); // no http client — never drop a picture silently

        Assert.NotNull(vm.ErrorMessage);
        Assert.True(vm.ShowEditForm);
        Assert.DoesNotContain("ProfileSet", rpc.Calls);
    }

    [Fact]
    public async Task OpenEditFormAsync_ResetsStagedAvatarAndBannerState()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(rawBody: Body) };
        var vm = new ProfileViewModel(rpc, "deadbeef");
        await vm.OpenEditFormAsync();
        vm.StageAvatar(Avatar1);
        vm.ClearBanner();

        await vm.OpenEditFormAsync(); // re-open (e.g. cancel then re-edit) resets both fields
        await vm.SaveEditAsync(); // no http client needed — everything resolved to Keep

        Assert.IsType<FfiProfileImageEdit.Keep>(rpc.LastProfileSet?.Avatar);
        Assert.IsType<FfiProfileImageEdit.Keep>(rpc.LastProfileSet?.Banner);
        Assert.False(vm.EditAvatarClear);
        Assert.False(vm.EditBannerClear);
    }

    // ── profile-block toggle viewmodel single-sourced in shared Rust
    // (fauna_core::format::{contact_row_blocks_actor, contact_toggle_block_label} via
    // the value-format FFI face; contacts.md § Where logic lives → Unblock). The
    // RefreshBlockStateAsync_* tests above now exercise the shared predicate. ──

    [Fact]
    public async Task RefreshBlockStateAsync_BlockedEdge_MatchesActorIdCaseInsensitively()
    {
        // The shared predicate matches the actor-id case-insensitively — the exact
        // divergence this lift resolves (windows was the lone client doing the
        // OrdinalIgnoreCase compare; the shared predicate adopts it for all five).
        var upperEdge = new ContactInfo(BobHex.ToUpperInvariant(), null, ContactStatus.Blocked, null);
        var rpc = new MockNestRpcClient { NextContacts = new[] { upperEdge } };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false); // lowercase target

        await vm.RefreshBlockStateAsync();

        Assert.True(vm.IsBlocked);
    }

    [Theory]
    [InlineData(true, "profile.unblock")]
    [InlineData(false, "profile.block")]
    public void ContactToggleBlockLabel_ResolvesSharedKey(bool isBlocked, string expectedKey)
    {
        // The profile-block-button label rides the shared fn (no global Strings state:
        // assert the LocalizedText key directly).
        Assert.Equal(expectedKey, FaunaFfiMethods.ContactToggleBlockLabel(isBlocked).key);
    }
}

// ── OfferStatus shared-derivation conformance — the windows consume of the shared
// fauna_core::format::offer_status / offer_status_label over the value-format FFI face
// (profile.md § Where logic lives → Tiers tab; monetization.md § Pillar 1). Asserts the
// precedence (Active>Pending>None) + badge-label keys the SubscriptionOfferRow branches
// on, dropping the prior local OfferStatusKind enum + OfferStatusToTextConverter. The
// shared uniffi enum can't be a public [Theory] param (CS0051) → param-less [Fact]s. ──
public class OfferStatusSharedTests
{
    [Fact]
    public void OfferStatus_ConfirmedTierWins_Active()
    {
        // status.get reports this tier → Active regardless of the transient pending flag.
        Assert.Equal(uniffi.fauna_core.OfferStatus.Active, FaunaFfiMethods.OfferStatus("gold", "gold", false));
        Assert.Equal(uniffi.fauna_core.OfferStatus.Active, FaunaFfiMethods.OfferStatus("gold", "gold", true));
    }

    [Fact]
    public void OfferStatus_PendingFlag_WhenNotHeld()
    {
        // A transient post-Queued click with no confirmed tier → Pending (the row's overlay).
        Assert.Equal(uniffi.fauna_core.OfferStatus.Pending, FaunaFfiMethods.OfferStatus("gold", "silver", true));
        Assert.Equal(uniffi.fauna_core.OfferStatus.Pending, FaunaFfiMethods.OfferStatus("gold", null, true));
    }

    [Fact]
    public void OfferStatus_None_WhenNeitherHeldNorPending()
    {
        Assert.Equal(uniffi.fauna_core.OfferStatus.None, FaunaFfiMethods.OfferStatus("gold", "silver", false));
        Assert.Equal(uniffi.fauna_core.OfferStatus.None, FaunaFfiMethods.OfferStatus("gold", null, false));
    }

    [Fact]
    public void OfferStatusLabel_MapsToSubscriptionsKeys()
    {
        Assert.Equal("subscriptions.offer_status_none", FaunaFfiMethods.OfferStatusLabel(uniffi.fauna_core.OfferStatus.None).key);
        Assert.Equal("subscriptions.offer_status_pending", FaunaFfiMethods.OfferStatusLabel(uniffi.fauna_core.OfferStatus.Pending).key);
        Assert.Equal("subscriptions.offer_status_active", FaunaFfiMethods.OfferStatusLabel(uniffi.fauna_core.OfferStatus.Active).key);
    }
}
