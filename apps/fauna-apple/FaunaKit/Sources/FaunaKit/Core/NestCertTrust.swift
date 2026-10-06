import Foundation
import Security

/// The pure trust decision apple's residual-HTTP leg — ``APIClient``'s
/// `URLSession` (blob upload / download / `HEAD`, chunks, manifests) — applies
/// to the nest's TLS cert, so a same-box or remote self-signed nest is trusted
/// the SAME way the rest of the Rust stack trusts it: via the shared pinned
/// SPKI. Member for member the twin of windows' `NestCertTrust.cs`, the policy
/// behind `DirectNestClient`'s `ServerCertificateCustomValidationCallback`.
///
/// Without it the leg made no trust decision at all — plain `URLSession.shared`
/// validates against the OS trust store and rejects the nest's self-signed
/// floor cert (`NSURLErrorServerCertificateUntrusted`, -1202), so media upload
/// and download silently broke against a same-box `https://127.0.0.1` nest
/// while every Rust (rustls) leg accepted it.
///
/// The SPKI computation, the pin lookup and the loopback classification are
/// shared Rust (`spkiSha256OfCertDer` / `pinnedSpkiForHost` /
/// `isLoopbackAuthority` / `authorityOf` — `fauna_ffi::trust`), so the value
/// compared here is byte-identical to the pin the WS handshake graduated: no
/// Security.framework re-encoding, no hand-rolled IPv6 parse. This file is
/// only the platform-side policy (`shouldTrust`) and its `URLSession` adapter
/// (``NestCertTrustSessionDelegate``). `docs/goal/architecture/security.md`
/// § Transport trust, the residual-HTTP legs.
public enum NestCertTrust {
    /// Decide whether to trust a served cert: accept iff the chain is
    /// WebPKI-valid (public-CA / ACME nest), OR the host is loopback (same-box
    /// install — the sanctioned `danger_accept_invalid_certs` prior art; also
    /// covers the unauthenticated health check that can run before any
    /// handshake graduates a pin), OR the served cert's SPKI matches the pin the
    /// WS handshake graduated for this host. Fail-closed otherwise: no pin, no
    /// parseable cert, or a pin the cert disagrees with all refuse.
    public static func shouldTrust(
        certSpki: Data?, pinnedSpki: Data?, webPkiValid: Bool, isLoopback: Bool
    ) -> Bool {
        if webPkiValid { return true }
        if isLoopback { return true }
        guard let certSpki, let pinnedSpki else { return false }
        return certSpki == pinnedSpki
    }

    /// The `URLSession` every ``APIClient`` sends through: `.default`
    /// configuration, the trust delegate for `nestUrl`. One per client — the
    /// session retains its delegate, and the client invalidates the session
    /// when it goes away.
    public static func makeSession(nestUrl: URL) -> URLSession {
        URLSession(
            configuration: .default,
            delegate: NestCertTrustSessionDelegate(nestUrl: nestUrl),
            delegateQueue: nil
        )
    }

    /// Whether a server-trust challenge is for the nest the session was built
    /// for. The loopback carve-out and the pin are the *configured nest's*
    /// (keyed on `authorityOf(nestUrl)` — the port-as-written pin key the
    /// handshake used, which a challenge's `host`/`port` pair cannot rebuild),
    /// so they are granted only to that host: a redirect anywhere else falls
    /// to the system default, strict WebPKI. Host compare is case-insensitive
    /// and bracket-blind (Foundation reports an IPv6 literal with or without
    /// its brackets depending on the reader); the port defaults per scheme
    /// the way a URL parser dials.
    static func challengeIsForNest(host: String, port: Int, nestUrl: URL) -> Bool {
        guard let nestHost = nestUrl.host else { return false }
        let nestPort = nestUrl.port ?? defaultPort(forScheme: nestUrl.scheme)
        return bareHost(host) == bareHost(nestHost) && port == nestPort
    }

    /// SHA-256 of the served leaf cert's raw DER `SubjectPublicKeyInfo`, by the
    /// shared-Rust hasher the handshake pins with; `nil` when the trust carries
    /// no certificate or it does not parse as X.509.
    static func leafSpki(of trust: SecTrust) -> Data? {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
              let leaf = chain.first
        else { return nil }
        return FaunaFFISwift.spkiSha256OfCertDer(certDer: SecCertificateCopyData(leaf) as Data)
    }

    private static func defaultPort(forScheme scheme: String?) -> Int {
        switch scheme?.lowercased() {
        case "http", "ws": return 80
        default: return 443
        }
    }

    private static func bareHost(_ host: String) -> String {
        var h = host.lowercased()
        if h.hasPrefix("["), h.hasSuffix("]") {
            h = String(h.dropFirst().dropLast())
        }
        return h
    }
}

/// The `URLSession` adapter: answers the server-trust challenge for the ONE
/// nest the session serves with ``NestCertTrust/shouldTrust`` over the
/// shared-Rust classifiers. Holds only the nest URL and its pin key, never the
/// client — a `URLSession` retains its delegate for its whole life.
public final class NestCertTrustSessionDelegate: NSObject, URLSessionDelegate {
    /// The nest this session was built for.
    public let nestUrl: URL
    /// The `host[:port]` authority the nest's TLS identity is pinned under —
    /// shared-Rust `authorityOf`, which keeps a written default `:443` (unlike
    /// `URL.port`, which reads it as absent), so the lookup key is the same
    /// string Rust pinned.
    public let authority: String

    public init(nestUrl: URL) {
        self.nestUrl = nestUrl
        self.authority = FaunaFFISwift.authorityOf(nestUrl: nestUrl.absoluteString)
    }

    /// One evaluation of a served trust, with every input the decision read —
    /// the unit tests assert these, not just the disposition.
    public struct Verdict: Equatable {
        public let trusted: Bool
        public let forNest: Bool
        public let webPkiValid: Bool
        public let isLoopback: Bool
        public let certSpki: Data?
        public let pinnedSpki: Data?
    }

    /// Evaluate `trust`, served by `host:port`, under the three-way policy.
    /// A challenge for any host but the nest's is not this delegate's to
    /// decide (`forNest == false` → `trusted == false`).
    public func verdict(for trust: SecTrust, host: String, port: Int) -> Verdict {
        guard NestCertTrust.challengeIsForNest(host: host, port: port, nestUrl: nestUrl) else {
            return Verdict(trusted: false, forNest: false, webPkiValid: false, isLoopback: false,
                           certSpki: nil, pinnedSpki: nil)
        }
        let webPkiValid = SecTrustEvaluateWithError(trust, nil)
        let certSpki = NestCertTrust.leafSpki(of: trust)
        let pinnedSpki = FaunaFFISwift.pinnedSpkiForHost(host: authority)
        let isLoopback = FaunaFFISwift.isLoopbackAuthority(authority: authority)
        let trusted = NestCertTrust.shouldTrust(
            certSpki: certSpki, pinnedSpki: pinnedSpki, webPkiValid: webPkiValid, isLoopback: isLoopback)
        return Verdict(trusted: trusted, forNest: true, webPkiValid: webPkiValid, isLoopback: isLoopback,
                       certSpki: certSpki, pinnedSpki: pinnedSpki)
    }

    /// The disposition a verdict maps to. A refusal is `.performDefaultHandling`,
    /// NOT `.cancelAuthenticationChallenge`: the system then re-evaluates the
    /// same trust under the same SSL policy — which just failed, since
    /// `webPkiValid` is false on every refusing path — so the request still
    /// fails closed, but with `NSURLErrorServerCertificateUntrusted` (-1202)
    /// rather than `.cancelled` (-999), which `DisplayError` reads as the user's
    /// own cancellation. The default is exactly what the leg did before it had
    /// a delegate; nothing here can accept more than the OS would.
    public static func disposition(for verdict: Verdict, trust: SecTrust)
        -> (URLSession.AuthChallengeDisposition, URLCredential?)
    {
        verdict.trusted ? (.useCredential, URLCredential(trust: trust)) : (.performDefaultHandling, nil)
    }

    public func urlSession(
        _ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        let space = challenge.protectionSpace
        guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = space.serverTrust
        else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        let (disposition, credential) = Self.disposition(
            for: verdict(for: trust, host: space.host, port: space.port), trust: trust)
        completionHandler(disposition, credential)
    }
}
