using System.Linq;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The pure trust decision the <see cref="DirectNestClient"/> residual-HTTP leg
/// (health / blob / snapshot) applies to a nest's TLS cert, so a same-box or
/// remote self-signed nest is trusted the SAME way the rest of the Rust stack is —
/// via the shared pinned SPKI. The SPKI computation and pin lookup themselves are
/// shared-Rust (<c>FaunaFfiMethods.SpkiSha256OfCertDer</c> /
/// <c>PinnedSpkiForHost</c>); this class is only the platform-side
/// <c>ServerCertificateCustomValidationCallback</c> policy. See
/// <c>docs/goal/architecture/security.md</c> § Transport trust.
/// </summary>
public static class NestCertTrust
{
    /// <summary>
    /// Decide whether to trust a served cert: accept iff the chain is WebPKI-valid
    /// (public-CA / ACME nest), OR the host is loopback (same-box install — the
    /// sanctioned <c>danger_accept_invalid_certs</c> / <c>InsecureSkipVerify</c> prior
    /// art, installers/windows.md; also covers the unauthenticated health check that
    /// can run before any handshake graduates a pin), OR the served cert's SPKI
    /// matches the pin the WS handshake graduated for this host. Fail-closed otherwise.
    /// </summary>
    public static bool ShouldTrust(byte[]? certSpki, byte[]? pinnedSpki, bool webPkiValid, bool isLoopback)
    {
        if (webPkiValid)
            return true;
        if (isLoopback)
            return true;
        return certSpki != null && pinnedSpki != null && certSpki.SequenceEqual(pinnedSpki);
    }

    /// <summary>
    /// Whether a request is for the nest the client was built for. The loopback
    /// carve-out and the pin are the *configured nest's* (keyed on its authority — the
    /// port-as-written pin key the handshake used, which a request's host/port pair
    /// cannot rebuild), so they are granted only to that host and port: a redirect
    /// anywhere else falls to strict WebPKI. Host compare is case-insensitive and
    /// bracket-blind (a <see cref="Uri"/> reports an IPv6 literal with its brackets,
    /// a caller may not); the port defaults per scheme the way a URL parser dials.
    /// Member for member the twin of apple's <c>NestCertTrust.challengeIsForNest</c>
    /// — two legs of one policy must not differ. Fail-closed: no absolute nest URL
    /// names no nest.
    /// </summary>
    public static bool IsForNest(string requestHost, int requestPort, Uri nestUrl)
    {
        if (!nestUrl.IsAbsoluteUri)
            return false;
        var nestPort = nestUrl.Port >= 0 ? nestUrl.Port : DefaultPort(nestUrl.Scheme);
        return string.Equals(BareHost(requestHost), BareHost(nestUrl.Host), StringComparison.OrdinalIgnoreCase)
            && requestPort == nestPort;
    }

    private static int DefaultPort(string scheme)
        => scheme.ToLowerInvariant() is "http" or "ws" ? 80 : 443;

    private static string BareHost(string host)
        => host.Length >= 2 && host[0] == '[' && host[^1] == ']' ? host[1..^1] : host;

    /// <summary>
    /// True if the <c>host[:port]</c> authority's host is a loopback address
    /// (<c>127.0.0.0/8</c> / <c>::1</c>) or <c>localhost</c>. Delegates to shared
    /// Rust (<c>FaunaFfiMethods.IsLoopbackAuthority</c>) so the classification —
    /// including the IPv6-bracket parse — is one tested impl across the stack rather
    /// than a hand-rolled per-app parser. The authority is the shared-Rust pin key
    /// (<c>FaunaFfiMethods.AuthorityOf</c>), which keeps the port.
    /// </summary>
    public static bool IsLoopbackAuthority(string authority)
        => FaunaFfiMethods.IsLoopbackAuthority(authority);
}
