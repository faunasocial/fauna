using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The profile Tiers tab §4 payment-provider status badge is single-sourced in
/// shared Rust (<c>fauna_core::format::provider_status_label</c> — evidence-based,
/// never an active probe; <c>monetization.md</c> § Pillar 3; FFI
/// <c>ProviderStatusLabel</c>). Calls the REAL export (native dll loads in the test
/// host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>) and asserts
/// the <c>LocalizedText</c> key directly (no global <c>Strings</c> state). Mirrors
/// <see cref="DeviceStatusLabelTests"/>; replaces the hand-rolled literal
/// <c>"configured"</c> (priority #1/#2).
/// </summary>
public class ProviderStatusLabelTests
{
    [Fact]
    public void NoEvidence_IsConfigured()
    {
        Assert.Equal(
            "subscriptions.provider_status_configured",
            FaunaFfiMethods.ProviderStatusLabel(lastVerifiedAt: null, lastRejectedAt: null).key);
    }

    [Fact]
    public void VerifiedOnly_IsVerified()
    {
        Assert.Equal(
            "subscriptions.provider_status_verified",
            FaunaFfiMethods.ProviderStatusLabel(lastVerifiedAt: 100, lastRejectedAt: null).key);
    }

    [Fact]
    public void RejectedOnly_IsError()
    {
        Assert.Equal(
            "subscriptions.provider_status_error",
            FaunaFfiMethods.ProviderStatusLabel(lastVerifiedAt: null, lastRejectedAt: 100).key);
    }

    [Fact]
    public void MostRecentEvidenceWins_VerifiedAfterRejected_IsVerified()
    {
        Assert.Equal(
            "subscriptions.provider_status_verified",
            FaunaFfiMethods.ProviderStatusLabel(lastVerifiedAt: 200, lastRejectedAt: 100).key);
    }

    [Fact]
    public void MostRecentEvidenceWins_RejectedAfterVerified_IsError()
    {
        Assert.Equal(
            "subscriptions.provider_status_error",
            FaunaFfiMethods.ProviderStatusLabel(lastVerifiedAt: 100, lastRejectedAt: 200).key);
    }

    [Fact]
    public void Tie_ResolvesConservativeTowardError()
    {
        Assert.Equal(
            "subscriptions.provider_status_error",
            FaunaFfiMethods.ProviderStatusLabel(lastVerifiedAt: 100, lastRejectedAt: 100).key);
    }
}
