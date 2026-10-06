using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the update-checker's semver compare. Calls the
/// REAL shared <c>fauna_core::version::is_newer</c> via
/// <c>FaunaFfiMethods.IsNewer</c> — the native dll loads in the test host
/// (memory <c>reference_windows_dotnet_test_loads_native_ffi</c>), so this proves
/// the windows <c>UpdateService</c> consumes the spec-correct shared compare
/// rather than the retired hand-rolled int-tuple parse. The pre-release and
/// build-metadata cases below are exactly the ones that old parser got wrong.
/// </summary>
public class VersionCompareFfiTests
{
    [Theory]
    // Ordinary newer / older / equal.
    [InlineData("1.2.3", "1.2.4", true)]
    [InlineData("1.2.3", "1.3.0", true)]
    [InlineData("1.2.3", "2.0.0", true)]
    [InlineData("1.2.4", "1.2.3", false)]
    [InlineData("1.2.3", "1.2.3", false)]
    // Numeric (not lexical) component compare — a non-widening parser gets this wrong.
    [InlineData("1.9.0", "1.10.0", true)]
    [InlineData("1.10.0", "1.9.0", false)]
    // Pre-release ordering: a pre-release is OLDER than its release. The retired
    // C# int-tuple parse treated "1.0.0-beta" as equal to "1.0.0" (→ wrong false).
    [InlineData("1.0.0-beta", "1.0.0", true)]
    [InlineData("1.0.0", "1.0.0-beta", false)]
    // Build metadata is ignored by precedence.
    [InlineData("1.2.3", "1.2.3+build.9", false)]
    // Unparseable (forgotten 'v' strip / garbage) degrades to "not newer".
    [InlineData("1.2.3", "v1.2.4", false)]
    [InlineData("1.2.3", "not-a-version", false)]
    public void IsNewer_MatchesSharedSpec(string current, string candidate, bool expected)
    {
        Assert.Equal(expected, uniffi.fauna_ffi.FaunaFfiMethods.IsNewer(current, candidate));
    }
}
