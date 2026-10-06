using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The admin-users read-only mail-serving-status label (<c>admin-users-mail-serving-status</c>)
/// is single-sourced in shared Rust (<c>fauna_core::format::mail_serving_status_label</c> via
/// the value-format FFI wrapper; value-formatting.md § Serving-status label). Calls the REAL
/// export (native dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>) and asserts the <c>LocalizedText</c>
/// key directly (no global <c>Strings</c> state). Replaces the hand-rolled
/// <c>serving_here</c>/<c>serving_disabled</c> key ternary in <c>AdminUsersViewModel</c>
/// (priority #1 minimize divergence / #4 resolve drift).
/// </summary>
public class MailServingStatusLabelTests
{
    [Theory]
    [InlineData(true, "admin.users_page.serving_here")]
    [InlineData(false, "admin.users_page.serving_disabled")]
    public void MailServingStatusLabel_ResolvesSharedKey(bool enabled, string expectedKey)
    {
        Assert.Equal(expectedKey, FaunaFfiMethods.MailServingStatusLabel(enabled).key);
    }
}
