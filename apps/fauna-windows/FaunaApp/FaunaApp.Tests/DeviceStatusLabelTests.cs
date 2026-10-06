using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Devices/Sync page device-status online/offline label is single-sourced in
/// shared Rust (<c>fauna_core::format::device_status_label</c> via the value-format
/// FFI wrapper; devices.md § Where logic lives → Device online/offline label). Calls
/// the REAL export (native dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>) and asserts the
/// <c>LocalizedText</c> key directly (no global <c>Strings</c> state). Replaces the
/// hand-rolled untranslated English "Online"/"Offline" ternary (priority #1).
/// </summary>
public class DeviceStatusLabelTests
{
    [Theory]
    [InlineData(true, "devices.online")]
    [InlineData(false, "devices.offline")]
    public void DeviceStatusLabel_ResolvesSharedKey(bool online, string expectedKey)
    {
        Assert.Equal(expectedKey, FaunaFfiMethods.DeviceStatusLabel(online).key);
    }
}
