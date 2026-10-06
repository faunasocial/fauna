using System.Net.Http;
using System.Net.Security;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// What <see cref="DirectNestClient"/>'s <c>ServerCertificateCustomValidationCallback</c>
/// grants a request for a host OTHER than the configured nest's. The loopback and
/// SPKI-pin carve-outs are the configured nest's (keyed on its authority), so they
/// must not follow a redirect elsewhere — <c>HttpClientHandler</c> follows a 30x by
/// default, and the callback sees the redirected request. Apple's
/// <c>NestCertTrust.challengeIsForNest</c> is the same guard on the Swift leg (two
/// legs of one policy must not differ); the pure host+port comparison is pinned in
/// <see cref="NestCertTrustTests"/>, this file pins that the callback CONSULTS it.
/// The authority + loopback classification behind the callback are the real shared-Rust
/// FFI (the native <c>fauna_ffi</c> dll loads in the test host). See
/// <c>docs/goal/architecture/security.md</c> § Transport trust.
/// </summary>
public class DirectNestClientCertCallbackTests
{
    // A chain error stands in for "the OS trust store refuses this cert" — the
    // self-signed floor cert every carve-out exists for.
    private const SslPolicyErrors Untrusted = SslPolicyErrors.RemoteCertificateChainErrors;

    private const string LoopbackNest = "https://127.0.0.1:443";

    private static bool Validate(string nestUrl, string? requestUrl, SslPolicyErrors errors)
    {
        var client = new DirectNestClient(nestUrl, new MockCryptoService());
        using var request = requestUrl is null
            ? new HttpRequestMessage()
            : new HttpRequestMessage(HttpMethod.Get, requestUrl);
        return client.ValidateServerCertificate(request, cert: null, chain: null, errors);
    }

    [Fact]
    public void RequestForTheNest_KeepsTheLoopbackCarveOut()
        // The control: the guard must not over-refuse. The scheme's default port
        // (443) is the nest's written `:443`.
        => Assert.True(Validate(LoopbackNest, "https://127.0.0.1/health", Untrusted));

    [Fact]
    public void RequestForAnotherHost_GetsWebPkiAlone()
        // A loopback nest's carve-out must not leak to a redirect target: that
        // host's untrusted cert is judged as strict WebPKI, never accepted unverified.
        => Assert.False(Validate(LoopbackNest, "https://elsewhere.example.com/x", Untrusted));

    [Fact]
    public void RequestForAnotherHost_StillAcceptsAWebPkiValidChain()
        // "WebPKI alone" is the whole verdict — a genuinely valid chain passes.
        => Assert.True(Validate(LoopbackNest, "https://elsewhere.example.com/x", SslPolicyErrors.None));

    [Fact]
    public void RequestForAnotherPortOnTheNestHost_GetsWebPkiAlone()
        // The nest's carve-out is its host AND port, not its host.
        => Assert.False(Validate(LoopbackNest, "https://127.0.0.1:8443/x", Untrusted));

    [Fact]
    public void RequestWithNoUri_FailsClosedToWebPkiAlone()
        // Nothing names the host the cert was served for, so no carve-out applies.
        => Assert.False(Validate(LoopbackNest, requestUrl: null, Untrusted));
}
