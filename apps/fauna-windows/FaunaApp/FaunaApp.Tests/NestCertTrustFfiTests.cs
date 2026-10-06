using System;
using System.Security.Cryptography;
using System.Security.Cryptography.X509Certificates;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the residual-HTTP TLS-pin accessors (the REAL
/// UniFFI exports — native <c>fauna_ffi</c> dll loads in the test host, memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>). The windows
/// <see cref="FaunaApp.Core.Services.DirectNestClient"/> cert-validation callback
/// computes the served cert's SPKI via <c>SpkiSha256OfCertDer</c> and the pin key
/// via <c>AuthorityOf</c> — both shared-Rust, so the value it compares is
/// byte-identical to what the WS handshake pinned (no .NET re-encoding divergence).
/// See <c>docs/goal/architecture/security.md</c> § Transport trust.
/// </summary>
public class NestCertTrustFfiTests
{
    private static X509Certificate2 MakeSelfSigned()
    {
        using var rsa = RSA.Create(2048);
        var req = new CertificateRequest("CN=fauna-test", rsa, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        return req.CreateSelfSigned(DateTimeOffset.UtcNow.AddDays(-1), DateTimeOffset.UtcNow.AddDays(30));
    }

    [Fact]
    public void SpkiSha256OfCertDer_RealCert_Returns32Bytes()
    {
        using var cert = MakeSelfSigned();
        var spki = FaunaFfiMethods.SpkiSha256OfCertDer(cert.RawData);
        Assert.NotNull(spki);
        Assert.Equal(32, spki!.Length);
    }

    [Fact]
    public void SpkiSha256OfCertDer_MatchesDotNetSpkiHash()
    {
        // The runtime comparison never relies on this (both sides go through the
        // Rust fn), but it documents that .NET's view of the embedded SPKI agrees.
        using var cert = MakeSelfSigned();
        byte[] dotnet = SHA256.HashData(cert.PublicKey.ExportSubjectPublicKeyInfo());
        var rust = FaunaFfiMethods.SpkiSha256OfCertDer(cert.RawData);
        Assert.Equal(dotnet, rust);
    }

    [Fact]
    public void SpkiSha256OfCertDer_DistinctCerts_DistinctHashes()
    {
        using var a = MakeSelfSigned();
        using var b = MakeSelfSigned();
        Assert.NotEqual(
            FaunaFfiMethods.SpkiSha256OfCertDer(a.RawData),
            FaunaFfiMethods.SpkiSha256OfCertDer(b.RawData));
    }

    [Fact]
    public void SpkiSha256OfCertDer_Garbage_ReturnsNull()
        => Assert.Null(FaunaFfiMethods.SpkiSha256OfCertDer(new byte[] { 1, 2, 3, 4 }));

    [Fact]
    public void PinnedSpkiForHost_NoPinGraduated_ReturnsNull()
        => Assert.Null(FaunaFfiMethods.PinnedSpkiForHost("unpinned.invalid:443"));

    [Theory]
    [InlineData("https://127.0.0.1:443", "127.0.0.1:443")] // default 443 KEPT (unlike .NET Uri.Authority)
    [InlineData("wss://127.0.0.1:443", "127.0.0.1:443")]
    [InlineData("https://nest.example.com/", "nest.example.com")]
    [InlineData("https://nest.example.com", "nest.example.com")]
    [InlineData("127.0.0.1:443", "127.0.0.1:443")]
    // PROBE-613: the authority must end at `?`/`#` too, not just `/`/`\` —
    // otherwise a query/fragment-carried `@host` keys this leg's pin/loopback
    // check under the wrong host; the Rust
    // `fauna_core::web::tests` rows are the mechanism's witness.
    [InlineData("https://nest.example.com?@[::1]", "nest.example.com")]
    [InlineData("https://nest.example.com#@[::1]", "nest.example.com")]
    [InlineData("https://nest.example.com?x=@127.0.0.1:443/", "nest.example.com")]
    [InlineData("wss://nest.example.com#@localhost", "nest.example.com")]
    public void AuthorityOf_StripsSchemeAndPath_KeepsPort(string url, string expected)
        => Assert.Equal(expected, FaunaFfiMethods.AuthorityOf(url));

    // The loopback classification the cert-validation callback applies to a
    // `host[:port]` authority is shared Rust too (no hand-rolled C# IPv6 parser).
    [Theory]
    [InlineData("127.0.0.1:443", true)]
    [InlineData("127.0.0.1", true)]
    [InlineData("localhost:443", true)]
    [InlineData("localhost", true)]
    [InlineData("[::1]:443", true)] // bracketed IPv6
    [InlineData("::1", true)] // bare IPv6
    [InlineData("192.168.1.5:443", false)] // private LAN is not loopback
    [InlineData("nest.example.com:443", false)]
    [InlineData("nest.example.com", false)]
    [InlineData("", false)]
    public void IsLoopbackAuthority_ClassifiesHost(string authority, bool expected)
        => Assert.Equal(expected, FaunaFfiMethods.IsLoopbackAuthority(authority));
}
