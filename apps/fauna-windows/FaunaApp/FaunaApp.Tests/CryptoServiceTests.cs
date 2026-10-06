using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

public class CryptoServiceTests
{
    /// <summary>
    /// Cross-language fixture mirroring the Rust test
    /// `libs/fauna-ffi/tests/auth_tests.rs::sign_message_pinned_fixture`.
    /// Ed25519 (RFC 8032) is deterministic — if either the Rust signer or the
    /// C# routing through uniffi drifts, one of these two tests will fail.
    /// </summary>
    [Fact]
    public void Sign_MatchesRustFixture()
    {
        var crypto = new CryptoService();
        var secret = new byte[32];
        Array.Fill(secret, (byte)0x2A);
        crypto.LoadFromSecret(Convert.ToHexString(secret));

        var msg = System.Text.Encoding.ASCII.GetBytes("fauna sign-message fixture v1");
        var sig = crypto.Sign(msg);

        Assert.Equal(64, sig.Length);
        Assert.Equal(
            "2ae13626f4e09d60064c7b95e439236c4e30520bf39a3bb3d420d71c9e38e09e454b5993333044178fffa8282c2ddc761519752a15bc9aaf31c56e075452c006",
            Convert.ToHexString(sig).ToLowerInvariant());
    }

    [Fact]
    public void GenerateKeypair_Returns32ByteSecretAndDerivesActorId()
    {
        var crypto = new CryptoService();
        var secretHex = crypto.GenerateKeypair();

        Assert.Equal(64, secretHex.Length); // 32 bytes hex-encoded
        Assert.True(crypto.HasKey);
        Assert.Equal(32, crypto.ActorIdBytes.Length);
        Assert.Equal(64, crypto.ActorIdHex.Length);
        Assert.Equal(crypto.ActorIdHex, Convert.ToHexString(crypto.ActorIdBytes).ToLowerInvariant());
    }

    [Fact]
    public void LoadFromSecret_DerivesSameActorId_AsSameSecret()
    {
        var secret = new byte[32];
        Array.Fill(secret, (byte)0x2A);
        var hex = Convert.ToHexString(secret);

        var a = new CryptoService();
        a.LoadFromSecret(hex);
        var b = new CryptoService();
        b.LoadFromSecret(hex);

        Assert.Equal(a.ActorIdHex, b.ActorIdHex);
        Assert.Equal(a.ActorIdBytes, b.ActorIdBytes);
    }

    [Fact]
    public void Operations_ThrowWhenNoKeyLoaded()
    {
        var crypto = new CryptoService();

        Assert.False(crypto.HasKey);
        Assert.Throws<InvalidOperationException>(() => crypto.ActorIdHex);
        Assert.Throws<InvalidOperationException>(() => crypto.ActorIdBytes);
        Assert.Throws<InvalidOperationException>(() => crypto.SecretBytes);
        Assert.Throws<InvalidOperationException>(() => crypto.Sign(new byte[] { 0x01 }));
    }
}
