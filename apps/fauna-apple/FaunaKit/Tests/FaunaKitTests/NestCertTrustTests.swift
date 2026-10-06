import Foundation
import Security
import Testing
@testable import FaunaKit

// The trust decision apple's residual-HTTP leg (`APIClient`'s `URLSession`)
// makes for a self-signed nest — accept iff the chain is WebPKI-valid, OR the
// host is loopback, OR the served cert's SPKI matches the pin the WS handshake
// graduated; fail-closed otherwise. Mirrors windows' `NestCertTrustTests.cs`
// (the pure decision) + `NestCertTrustFfiTests.cs` (the shared-Rust
// classifiers, called for real: the FaunaFFI dylib loads in the swift-test
// host), and adds the `URLSession` adapter's verdict over a REAL self-signed
// certificate — the refusal the row's definition of success asks a unit test
// to pin. security.md § Transport trust.

private let spkiA = Data(repeating: 0x00, count: 32)
private let spkiB = Data(repeating: 0xAB, count: 32)

/// A throwaway self-signed P-256 certificate (`CN=fauna-nest-test`, valid to
/// 2126, no private key anywhere) — the nest's floor cert as the OS sees it:
/// no public CA signed it, so `SecTrustEvaluateWithError` refuses it under the
/// SSL policy. What the delegate does with that refusal is the whole point.
private let testCertDer = Data(base64Encoded: """
    MIIBJTCBzAIJAMM9dMoPVFbpMAoGCCqGSM49BAMCMBoxGDAWBgNVBAMMD2ZhdW5hLW5lc3QtdGVzdDAg\
    Fw0yNjA5MjMyMzAzMTZaGA8yMTI2MDgzMDIzMDMxNlowGjEYMBYGA1UEAwwPZmF1bmEtbmVzdC10ZXN0\
    MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAExU1hv71xfaltT5u06o+QytXgAMNPNCm8tavnmpvp02Ac\
    6eHuzi4yknT9a/UiA3/CMUxovDHlG002Dxmg3+u26TAKBggqhkjOPQQDAgNIADBFAiBch8oLyxT8w6r+\
    s+srRDTd1ZfIIoX/LNZOajUHFI/MGgIhAJZ6mYD4X8ud++gxBBE5q6SeN5zpFcBnV422oNmvugBi
    """)!

/// `trust` as `URLSession` would hand it to the delegate for a TLS connection
/// to `host`: the served chain under the SSL server policy for that name.
private func selfSignedTrust(servedTo host: String) throws -> SecTrust {
    let cert = try #require(SecCertificateCreateWithData(nil, testCertDer as CFData))
    var trust: SecTrust?
    let status = SecTrustCreateWithCertificates(cert, SecPolicyCreateSSL(true, host as CFString), &trust)
    #expect(status == errSecSuccess)
    return try #require(trust)
}

/// `APIClient.swift`'s own source, `#filePath`-relative like
/// `APIClientActorAdoptionTests`: the one HTTP session the client sends
/// through must be the trust-deciding one — a `URLSession.shared` creeping
/// back in would silently reopen the -1202 gap.
private let apiClientSourceURL = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // FaunaKitTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // FaunaKit
    .appendingPathComponent("Sources/FaunaKit/Core/APIClient.swift")
    .standardizedFileURL

@Suite("NestCertTrust — the apple residual-HTTP leg's trust decision")
struct NestCertTrustTests {
    // MARK: - The pure decision (NestCertTrustTests.cs)

    @Test func shouldTrustWebPkiValidAccepts() {
        #expect(NestCertTrust.shouldTrust(certSpki: nil, pinnedSpki: nil, webPkiValid: true, isLoopback: false))
    }

    @Test func shouldTrustLoopbackAcceptsEvenWithoutPin() {
        #expect(NestCertTrust.shouldTrust(certSpki: spkiA, pinnedSpki: nil, webPkiValid: false, isLoopback: true))
    }

    @Test func shouldTrustPinMatchAccepts() {
        #expect(NestCertTrust.shouldTrust(
            certSpki: spkiB, pinnedSpki: Data(spkiB), webPkiValid: false, isLoopback: false))
    }

    @Test func shouldTrustPinMismatchRejectsFailClosed() {
        #expect(!NestCertTrust.shouldTrust(certSpki: spkiA, pinnedSpki: spkiB, webPkiValid: false, isLoopback: false))
    }

    @Test func shouldTrustNoPinRejectsFailClosed() {
        #expect(!NestCertTrust.shouldTrust(certSpki: spkiA, pinnedSpki: nil, webPkiValid: false, isLoopback: false))
    }

    @Test func shouldTrustNilCertSpkiRejectsFailClosed() {
        #expect(!NestCertTrust.shouldTrust(certSpki: nil, pinnedSpki: spkiB, webPkiValid: false, isLoopback: false))
    }

    // MARK: - The shared-Rust classifiers, for real (NestCertTrustFfiTests.cs)

    @Test(arguments: [
        ("127.0.0.1:443", true),
        ("127.0.0.1", true),
        ("localhost:443", true),
        ("localhost", true),
        ("[::1]:443", true),  // bracketed IPv6
        ("::1", true),  // bare IPv6
        ("192.168.1.5:443", false),  // private LAN is not loopback
        ("nest.example.com:443", false),
        ("nest.example.com", false),
        ("", false),
    ])
    func isLoopbackAuthorityClassifiesHost(authority: String, expected: Bool) {
        #expect(isLoopbackAuthority(authority: authority) == expected)
    }

    @Test(arguments: [
        ("https://127.0.0.1:443", "127.0.0.1:443"),  // a written default :443 is KEPT
        ("wss://127.0.0.1:443", "127.0.0.1:443"),
        ("https://nest.example.com/", "nest.example.com"),
        ("https://nest.example.com", "nest.example.com"),
        ("127.0.0.1:443", "127.0.0.1:443"),
        // The authority ends at `?`/`#` too, so a query/fragment-carried `@host`
        // cannot key this leg's pin/loopback check under the wrong host.
        ("https://nest.example.com?@[::1]", "nest.example.com"),
        ("https://nest.example.com#@[::1]", "nest.example.com"),
        ("https://nest.example.com?x=@127.0.0.1:443/", "nest.example.com"),
        ("wss://nest.example.com#@localhost", "nest.example.com"),
    ])
    func authorityOfStripsSchemeAndPathKeepsPort(url: String, expected: String) {
        #expect(authorityOf(nestUrl: url) == expected)
    }

    @Test func spkiSha256OfCertDerRealCertReturns32Bytes() {
        #expect(spkiSha256OfCertDer(certDer: testCertDer)?.count == 32)
    }

    @Test func spkiSha256OfCertDerGarbageReturnsNil() {
        #expect(spkiSha256OfCertDer(certDer: Data([1, 2, 3, 4])) == nil)
    }

    @Test func pinnedSpkiForHostNoPinGraduatedReturnsNil() {
        #expect(pinnedSpkiForHost(host: "unpinned.invalid:443") == nil)
    }

    // MARK: - The challenge-is-for-the-nest guard

    @Test(arguments: [
        ("https://127.0.0.1:443", "127.0.0.1", 443, true),
        ("https://nest.example.com", "nest.example.com", 443, true),  // scheme default port
        ("https://nest.example.com", "NEST.example.com", 443, true),  // hosts are case-blind
        ("https://[::1]:8443", "::1", 8443, true),  // bracket-blind either way
        ("https://[::1]:8443", "[::1]", 8443, true),
        ("https://nest.example.com", "nest.example.com", 8443, false),  // other port
        ("https://nest.example.com", "elsewhere.example.com", 443, false),  // a redirect elsewhere
        ("https://127.0.0.1:443", "127.0.0.2", 443, false),  // per host, not per /8
    ])
    func challengeIsForNestComparesHostAndPort(nestUrl: String, host: String, port: Int, expected: Bool) {
        #expect(
            NestCertTrust.challengeIsForNest(host: host, port: port, nestUrl: URL(string: nestUrl)!) == expected)
    }

    // MARK: - The URLSession adapter over a REAL self-signed certificate

    @Test func selfSignedCertIsNotWebPkiValid() throws {
        // The premise every arm below rests on: the OS alone refuses this cert.
        let trust = try selfSignedTrust(servedTo: "127.0.0.1")
        #expect(!SecTrustEvaluateWithError(trust, nil))
    }

    @Test func delegateAcceptsTheSelfSignedFloorCertOnLoopback() throws {
        // The same-box-install case the e2e test walks (`self_signed_nest` binds
        // 127.0.0.1): WebPKI refuses, no pin is graduated, loopback carries it.
        let delegate = NestCertTrustSessionDelegate(nestUrl: URL(string: "https://127.0.0.1:443")!)
        #expect(delegate.authority == "127.0.0.1:443")
        let trust = try selfSignedTrust(servedTo: "127.0.0.1")

        let verdict = delegate.verdict(for: trust, host: "127.0.0.1", port: 443)
        #expect(verdict.forNest)
        #expect(!verdict.webPkiValid)
        #expect(verdict.isLoopback)
        #expect(verdict.certSpki?.count == 32)
        #expect(verdict.pinnedSpki == nil)
        #expect(verdict.trusted)

        let (disposition, credential) = NestCertTrustSessionDelegate.disposition(for: verdict, trust: trust)
        #expect(disposition == .useCredential)
        #expect(credential != nil)
    }

    @Test func delegateRefusesTheSelfSignedCertOffLoopbackWithNoPin() throws {
        // The refusal the row pins: a remote self-signed nest with no graduated
        // pin gets neither carve-out. The disposition is the system default
        // (strict WebPKI, which the premise test shows refuses this cert) —
        // never a credential, never a blanket accept.
        let delegate = NestCertTrustSessionDelegate(nestUrl: URL(string: "https://192.168.1.5:443")!)
        let trust = try selfSignedTrust(servedTo: "192.168.1.5")

        let verdict = delegate.verdict(for: trust, host: "192.168.1.5", port: 443)
        #expect(verdict.forNest)
        #expect(!verdict.webPkiValid)
        #expect(!verdict.isLoopback)
        #expect(verdict.certSpki?.count == 32)
        #expect(verdict.pinnedSpki == nil)
        #expect(!verdict.trusted)

        let (disposition, credential) = NestCertTrustSessionDelegate.disposition(for: verdict, trust: trust)
        #expect(disposition == .performDefaultHandling)
        #expect(credential == nil)
    }

    @Test func delegateLeavesAChallengeForAnotherHostToTheSystem() throws {
        // A loopback nest's carve-out must not leak to a redirect elsewhere:
        // the challenge is not for the nest, so no input is even read.
        let delegate = NestCertTrustSessionDelegate(nestUrl: URL(string: "https://127.0.0.1:443")!)
        let trust = try selfSignedTrust(servedTo: "elsewhere.example.com")

        let verdict = delegate.verdict(for: trust, host: "elsewhere.example.com", port: 443)
        #expect(!verdict.forNest)
        #expect(!verdict.trusted)
        #expect(NestCertTrustSessionDelegate.disposition(for: verdict, trust: trust).0 == .performDefaultHandling)
    }

    // MARK: - Structural: the leg goes through the delegate

    @Test func apiClientSendsThroughTheTrustDecidingSession() throws {
        let source = try String(contentsOf: apiClientSourceURL, encoding: .utf8)
        #expect(source.contains("NestCertTrust.makeSession(nestUrl: nodeUrl)"))
        // Comments may name the shared session to explain why it is NOT used;
        // no code line may reach for it.
        let codeLines = source.split(separator: "\n").filter {
            !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//")
        }
        #expect(!codeLines.contains { $0.contains("URLSession.shared") })
    }
}
