using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the shared onboarding re-claim helper: windows
/// must qualify a bare cached localpart to <c>localpart@domain</c> identically to
/// shared Rust (<c>fauna_onboarding_machine::qualify_reclaim_handle</c>, per
/// <c>docs/goal/behavior/mail-bridge-lifecycle.md</c> § Factory reset → re-claim
/// handle sourcing). These call the REAL UniFFI export
/// (<c>FaunaOnboardingMachineMethods.QualifyReclaimHandle</c>; the native
/// <c>fauna_ffi</c> dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>), mirroring the Rust unit
/// tests in <c>helpers.rs</c>. Locks windows to the shared qualification rule +
/// nest-URL-host parse (priority #2/#4) so the factory-reset re-claim never
/// re-derives it inline.
/// </summary>
public class OnboardingHelpersFfiTests
{
    [Fact]
    public void QualifyReclaimHandle_PassesThroughEmptyAndAlreadyQualified()
    {
        // Empty stays empty; an already-qualified handle is untouched.
        Assert.Equal("", FaunaOnboardingMachineMethods.QualifyReclaimHandle("", "example.com", "https://example.com"));
        Assert.Equal(
            "alice@example.com",
            FaunaOnboardingMachineMethods.QualifyReclaimHandle("alice@example.com", "other.example", "https://x"));
    }

    [Fact]
    public void QualifyReclaimHandle_PrefersCachedDomainThenNestHost()
    {
        // A non-empty cached domain wins over the nest-URL host.
        Assert.Equal(
            "alice@custom.example",
            FaunaOnboardingMachineMethods.QualifyReclaimHandle("alice", "custom.example", "https://example.com"));
        // An empty cached domain falls through to the nest-URL host.
        Assert.Equal(
            "alice@example.com",
            FaunaOnboardingMachineMethods.QualifyReclaimHandle("alice", "", "https://example.com"));
    }

    [Fact]
    public void QualifyReclaimHandle_NullDomainParsesNestHost()
    {
        // null domain → parse the host out of the nest URL (scheme/port/userinfo stripped).
        Assert.Equal(
            "bob@localhost",
            FaunaOnboardingMachineMethods.QualifyReclaimHandle("bob", null, "http://localhost:3000"));
        Assert.Equal(
            "bob@example.com",
            FaunaOnboardingMachineMethods.QualifyReclaimHandle("bob", null, "example.com"));
        Assert.Equal(
            "bob@example.com",
            FaunaOnboardingMachineMethods.QualifyReclaimHandle("bob", null, "https://user@example.com:8443/path"));
        // Hostless URL → can't qualify → return the bare localpart (the nest's
        // mail-domain safety net covers it).
        Assert.Equal("bob", FaunaOnboardingMachineMethods.QualifyReclaimHandle("bob", null, ""));
    }
}
