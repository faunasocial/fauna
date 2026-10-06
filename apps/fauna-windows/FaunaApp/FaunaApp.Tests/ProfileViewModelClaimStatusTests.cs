#if PAYMENTS
// Gated with the plane under test — dynamic-features.md § The feature-matrix
// test story: both flavors build and run their test subset, and a store-safe
// build has no payments members to drive.
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The §5 claim-status label resolve (<c>monetization.md</c> § Pillar 3):
/// <c>ProfileViewModel.HydrateAsync</c> resolves the shared 3-state decision
/// (<c>fauna_core::format::claim_status_label</c> — redeemed wins over voided)
/// through the real <see cref="Strings"/> pipeline production uses, with a fake
/// localizer mirroring the generated windows resw (<c>subscriptions/claim_status_*</c>
/// — en.yaml). Split out of <see cref="ProfileViewModelTests"/> (a large,
/// parallel-friendly class) because <c>Strings.Initialize</c> mutates
/// process-global state — <see cref="ValueFormatTests"/> is the established
/// precedent for this shape.
/// </summary>
[Collection("StringsGlobal")]
public class ProfileViewModelClaimStatusTests
{
    private static readonly byte[] Alice = Enumerable.Repeat((byte)0xa1, 32).ToArray();

    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["subscriptions/claim_status_unredeemed"] = "Unredeemed",
            ["subscriptions/claim_status_redeemed"] = "Redeemed",
            ["subscriptions/claim_status_voided"] = "Voided",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ProfileViewModelClaimStatusTests() => Strings.Initialize(new FakeLocalizer());

    [Fact]
    public async Task HydrateAsync_ClaimStatusResolvesRedeemedOverVoided()
    {
        // The two wire booleans are independent — redeemed wins is a real decision
        // (fauna_core::format::claim_status_label), not an implementation detail.
        var rpc = new MockNestRpcClient
        {
            NextClaims = new[]
            {
                MockNestRpcClient.MakeClaim("A", "gold"),
                MockNestRpcClient.MakeClaim("B", "gold", redeemedBy: Alice, voidedAt: 1),
                MockNestRpcClient.MakeClaim("C", "gold", voidedAt: 1),
            },
        };
        var vm = new ProfileViewModel(rpc, "deadbeef");

        await vm.HydrateAsync();

        Assert.Equal("Unredeemed", vm.Claims[0].StatusLabel);
        Assert.Equal("Redeemed", vm.Claims[1].StatusLabel);
        Assert.Equal("Voided", vm.Claims[2].StatusLabel);
    }
}
#endif   // PAYMENTS
