using Xunit;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the create-feed factor-weight editor's decimal
/// multiplier → signed per-mille conversion (<c>fauna_core::format::parse_weight_permille</c>,
/// surfaced as <see cref="FaunaFfiMethods.ParseWeightPermille"/>). Calls the REAL UniFFI
/// export (the native <c>fauna_ffi</c> dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>).
///
/// <para>These lock windows onto the shared rule rather than a C# hand-roll. That matters
/// concretely: <c>Math.Round</c>'s default in .NET is <b>banker's</b> (to-even) rounding,
/// so a hand-rolled <c>(long)Math.Round(w * 1000)</c> would silently disagree with linux
/// and web on every midpoint — exactly the class of drift the shared
/// <c>probability_to_per_mille</c> lift already closed for spam thresholds. Priorities
/// #1 (minimize divergence) / #2 (shared Rust) / #4 (resolve drift).
/// See <c>docs/goal/behavior/value-formatting.md</c> § Factor weight.</para>
/// </summary>
public class FactorWeightParseTests
{
    [Fact]
    public void ParseWeightPermille_ScalesDecimalMultiplier()
    {
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille("1.0"));
        Assert.Equal(2000, FaunaFfiMethods.ParseWeightPermille("2.0"));
        Assert.Equal(1500, FaunaFfiMethods.ParseWeightPermille("1.5"));
        Assert.Equal(2500, FaunaFfiMethods.ParseWeightPermille("  2.5  "));
        Assert.Equal(0, FaunaFfiMethods.ParseWeightPermille("0"));
    }

    /// <summary>
    /// A negative weight is a designed case, not an error: a strong-negative factor sinks
    /// an item below any rendered page, which is how filtering falls out of ordering
    /// (<c>docs/goal/ui/feed.md</c> § Frame reconciliation).
    /// </summary>
    [Fact]
    public void ParseWeightPermille_RoundTripsNegativeSinkWeights()
    {
        Assert.Equal(-1000, FaunaFfiMethods.ParseWeightPermille("-1"));
        Assert.Equal(-2500, FaunaFfiMethods.ParseWeightPermille("-2.5"));
    }

    /// <summary>
    /// Half-away-from-zero, NOT .NET's default banker's rounding (which yields 2 / -2 here)
    /// and NOT JS's half-up <c>Math.round</c> (which yields 3 / -2).
    /// </summary>
    [Fact]
    public void ParseWeightPermille_RoundsHalfAwayFromZero_NotBankers()
    {
        Assert.Equal(3, FaunaFfiMethods.ParseWeightPermille("0.0025"));
        Assert.Equal(-3, FaunaFfiMethods.ParseWeightPermille("-0.0025"));
    }

    /// <summary>
    /// An unparseable entry falls back to the 1.0 baseline rather than silently dropping
    /// the caller's add. The parse is strict/whole-string — resolving web's lenient
    /// <c>parseFloat("2abc") == 2</c>.
    /// </summary>
    [Fact]
    public void ParseWeightPermille_FallsBackToBaselineOnBadInput()
    {
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille(""));
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille("   "));
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille("abc"));
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille("2abc"));
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille("inf"));
        Assert.Equal(1000, FaunaFfiMethods.ParseWeightPermille("NaN"));
    }
}
