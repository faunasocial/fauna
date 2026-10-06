using System.Linq;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The pure trust decision the windows C# <see cref="DirectNestClient"/>'s
/// <c>ServerCertificateCustomValidationCallback</c> makes for a self-signed nest:
/// accept iff the chain is WebPKI-valid, OR the host is loopback (same-box install —
/// sanctioned prior art, installers/windows.md), OR the served cert's SPKI matches the
/// pin the WS handshake graduated. Fail-closed otherwise. The SPKI computation + pin
/// lookup are shared-Rust (covered by <see cref="NestCertTrustFfiTests"/> against the
/// real dll); this file locks the decision logic. See security.md § Transport trust.
/// </summary>
public class NestCertTrustTests
{
    private static readonly byte[] SpkiA = new byte[32]; // all-zero
    private static readonly byte[] SpkiB = Enumerable.Repeat((byte)0xAB, 32).ToArray();

    [Theory]
    [InlineData("127.0.0.1:443", true)]
    [InlineData("127.0.0.1", true)]
    [InlineData("localhost:443", true)]
    [InlineData("localhost", true)]
    [InlineData("[::1]:443", true)]
    [InlineData("::1", true)]
    [InlineData("192.168.1.5:443", false)]
    [InlineData("nest.example.com:443", false)]
    [InlineData("nest.example.com", false)]
    [InlineData("", false)]
    public void IsLoopbackAuthority_ClassifiesHost(string authority, bool expected)
        => Assert.Equal(expected, NestCertTrust.IsLoopbackAuthority(authority));

    // The request-is-for-the-nest guard: the loopback carve-out and the pin are the
    // CONFIGURED nest's, so a request for any other host or port (a 30x the handler
    // followed elsewhere) is not this callback's to grant them. Member for member the
    // eight rows apple's `NestCertTrustTests.challengeIsForNestComparesHostAndPort`
    // pins — two legs of one policy must not differ.
    [Theory]
    [InlineData("https://127.0.0.1:443", "127.0.0.1", 443, true)]
    [InlineData("https://nest.example.com", "nest.example.com", 443, true)] // scheme default port
    [InlineData("https://nest.example.com", "NEST.example.com", 443, true)] // hosts are case-blind
    [InlineData("https://[::1]:8443", "::1", 8443, true)] // bracket-blind either way
    [InlineData("https://[::1]:8443", "[::1]", 8443, true)]
    [InlineData("https://nest.example.com", "nest.example.com", 8443, false)] // other port
    [InlineData("https://nest.example.com", "elsewhere.example.com", 443, false)] // a redirect elsewhere
    [InlineData("https://127.0.0.1:443", "127.0.0.2", 443, false)] // per host, not per /8
    public void IsForNest_ComparesHostAndPort(string nestUrl, string requestHost, int requestPort, bool expected)
        => Assert.Equal(expected, NestCertTrust.IsForNest(requestHost, requestPort, new Uri(nestUrl)));

    [Fact]
    public void ShouldTrust_WebPkiValid_Accepts()
        => Assert.True(NestCertTrust.ShouldTrust(certSpki: null, pinnedSpki: null, webPkiValid: true, isLoopback: false));

    [Fact]
    public void ShouldTrust_Loopback_AcceptsEvenWithoutPin()
        => Assert.True(NestCertTrust.ShouldTrust(certSpki: SpkiA, pinnedSpki: null, webPkiValid: false, isLoopback: true));

    [Fact]
    public void ShouldTrust_PinMatch_Accepts()
        => Assert.True(NestCertTrust.ShouldTrust(certSpki: SpkiB, pinnedSpki: (byte[])SpkiB.Clone(), webPkiValid: false, isLoopback: false));

    [Fact]
    public void ShouldTrust_PinMismatch_RejectsFailClosed()
        => Assert.False(NestCertTrust.ShouldTrust(certSpki: SpkiA, pinnedSpki: SpkiB, webPkiValid: false, isLoopback: false));

    [Fact]
    public void ShouldTrust_NoPin_RejectsFailClosed()
        => Assert.False(NestCertTrust.ShouldTrust(certSpki: SpkiA, pinnedSpki: null, webPkiValid: false, isLoopback: false));

    [Fact]
    public void ShouldTrust_NullCertSpki_RejectsFailClosed()
        => Assert.False(NestCertTrust.ShouldTrust(certSpki: null, pinnedSpki: SpkiB, webPkiValid: false, isLoopback: false));
}
