using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the shared backup-key derivation: windows must
/// derive byte-identically to Rust's <c>BackupKey::derive</c>. Calls the REAL UniFFI
/// export <c>FaunaFfiMethods.BackupKeyDerive</c>, so this also proves the native
/// <c>fauna_ffi</c> dll loads in the test host (memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>).
///
/// Rehomed from <c>DagCborProvisionTests</c> when the named-pipe dag-cbor codec was
/// deleted (the C# no longer speaks that wire). It never tested the codec — the rest
/// of that file pinned pipe-frame hex and died with the transport; this vector is
/// about the FFI export and outlives it.
/// </summary>
public class BackupKeyDeriveFfiTests
{
    private static string Hex(byte[] b) => Convert.ToHexString(b).ToLowerInvariant();

    // BackupKey::derive(&[0x01; 32]) — the Task-3.1 pinned vector.
    private const string BackupKeyHex =
        "f39582d247fa3bb84a45224943d9f058b8650bed6d6640e3a69165a147383a14";

    [Fact]
    public void BackupKeyDerive_MatchesPinnedVector()
    {
        var seed = new byte[32];
        Array.Fill(seed, (byte)0x01);
        var key = FaunaFfiMethods.BackupKeyDerive(seed);
        Assert.Equal(BackupKeyHex, Hex(key));
    }
}
