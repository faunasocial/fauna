using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the shared short-id display form: windows must
/// truncate a long hex id identically to shared Rust
/// (<c>fauna_core::format::short_id</c>, per
/// <c>docs/goal/behavior/value-formatting.md</c> § Short id). Calls the REAL UniFFI
/// export <c>FaunaFfiMethods.ShortId</c> (the native <c>fauna_ffi</c> dll loads in
/// the test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>),
/// locking windows to the canonical first-12 + <c>…</c> form so the linked-nests
/// list (`Abbreviate`) and the feed author (`ShortAuthor`) never re-derive a
/// first-8…last-8 or ASCII-<c>...</c> truncation (priority #1/#4).
/// </summary>
public class ShortIdFfiTests
{
    [Fact]
    public void ShortId_LongHex_IsFirst12PlusEllipsis()
    {
        var id = new string('a', 64);
        Assert.Equal("aaaaaaaaaaaa…", FaunaFfiMethods.ShortId(id));
    }

    [Fact]
    public void ShortId_TwelveOrFewerChars_Unchanged()
    {
        Assert.Equal("abc123", FaunaFfiMethods.ShortId("abc123"));
        Assert.Equal("abcdef012345", FaunaFfiMethods.ShortId("abcdef012345")); // exactly 12
    }
}
