package com.fauna.app.core

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when`
import java.io.ByteArrayInputStream
import java.io.File
import java.net.Socket
import java.security.cert.CertificateException
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.SSLEngine
import javax.net.ssl.SSLSession
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509ExtendedTrustManager
import javax.net.ssl.X509TrustManager

/**
 * The trust decision android's residual-HTTP leg ([ApiClient]'s OkHttp client)
 * makes for a self-signed nest — accept iff the chain is WebPKI-valid, OR the
 * host is loopback, OR the served cert's SPKI matches the pin the WS handshake
 * graduated; fail-closed otherwise. Mirrors windows' `NestCertTrustTests.cs`
 * (the pure decision), `NestCertTrustFfiTests.cs` (the shared-Rust classifiers,
 * called for real: the host-built `libfauna_ffi.so` loads over JNA) and
 * `DirectNestClientCertCallbackTests.cs` (what a peer that is not the nest is
 * granted), and apple's `NestCertTrustTests.swift` (the adapter's verdict over
 * a REAL self-signed certificate). security.md § Transport trust.
 */
class NestCertTrustTest {

    // ── The pure decision (NestCertTrustTests.cs) ──

    @Test
    fun shouldTrustWebPkiValidAccepts() =
        assertTrue(NestCertTrust.shouldTrust(certSpki = null, pinnedSpki = null, webPkiValid = true, isLoopback = false))

    @Test
    fun shouldTrustLoopbackAcceptsEvenWithoutPin() =
        assertTrue(NestCertTrust.shouldTrust(certSpki = SPKI_A, pinnedSpki = null, webPkiValid = false, isLoopback = true))

    @Test
    fun shouldTrustPinMatchAccepts() =
        assertTrue(
            NestCertTrust.shouldTrust(
                certSpki = SPKI_B, pinnedSpki = SPKI_B.copyOf(), webPkiValid = false, isLoopback = false,
            ),
        )

    @Test
    fun shouldTrustPinMismatchRejectsFailClosed() =
        assertFalse(NestCertTrust.shouldTrust(certSpki = SPKI_A, pinnedSpki = SPKI_B, webPkiValid = false, isLoopback = false))

    @Test
    fun shouldTrustNoPinRejectsFailClosed() =
        assertFalse(NestCertTrust.shouldTrust(certSpki = SPKI_A, pinnedSpki = null, webPkiValid = false, isLoopback = false))

    @Test
    fun shouldTrustNullCertSpkiRejectsFailClosed() =
        assertFalse(NestCertTrust.shouldTrust(certSpki = null, pinnedSpki = SPKI_B, webPkiValid = false, isLoopback = false))

    // ── The shared-Rust classifiers, for real (NestCertTrustFfiTests.cs) ──

    @Test
    fun isLoopbackAuthorityClassifiesHost() {
        val rows = listOf(
            "127.0.0.1:443" to true,
            "127.0.0.1" to true,
            "localhost:443" to true,
            "localhost" to true,
            "[::1]:443" to true, // bracketed IPv6
            "::1" to true, // bare IPv6
            "192.168.1.5:443" to false, // private LAN is not loopback
            "nest.example.com:443" to false,
            "nest.example.com" to false,
            "" to false,
        )
        for ((authority, expected) in rows) {
            assertEquals("isLoopbackAuthority($authority)", expected, com.fauna.ffi.isLoopbackAuthority(authority))
        }
    }

    @Test
    fun authorityOfStripsSchemeAndPathKeepsPort() {
        val rows = listOf(
            "https://127.0.0.1:443" to "127.0.0.1:443", // a written default :443 is KEPT
            "wss://127.0.0.1:443" to "127.0.0.1:443",
            "https://nest.example.com/" to "nest.example.com",
            "https://nest.example.com" to "nest.example.com",
            "127.0.0.1:443" to "127.0.0.1:443",
            // The authority ends at `?`/`#` too, so a query/fragment-carried `@host`
            // cannot key this leg's pin/loopback check under the wrong host.
            "https://nest.example.com?@[::1]" to "nest.example.com",
            "https://nest.example.com#@[::1]" to "nest.example.com",
            "https://nest.example.com?x=@127.0.0.1:443/" to "nest.example.com",
            "wss://nest.example.com#@localhost" to "nest.example.com",
        )
        for ((url, expected) in rows) {
            assertEquals("authorityOf($url)", expected, com.fauna.ffi.authorityOf(url))
        }
    }

    @Test
    fun spkiSha256OfCertDerRealCertReturns32Bytes() =
        assertEquals(32, com.fauna.ffi.spkiSha256OfCertDer(TEST_CERT_DER)?.size)

    @Test
    fun spkiSha256OfCertDerGarbageReturnsNull() =
        assertNull(com.fauna.ffi.spkiSha256OfCertDer(byteArrayOf(1, 2, 3, 4)))

    @Test
    fun pinnedSpkiForHostNoPinGraduatedReturnsNull() =
        assertNull(com.fauna.ffi.pinnedSpkiForHost("unpinned.invalid:443"))

    // ── The peer-is-the-nest guard ──

    // Member for member the eight rows `NestCertTrustTests.IsForNest_ComparesHostAndPort`
    // (C#) and `challengeIsForNestComparesHostAndPort` (Swift) pin — two legs of
    // one policy must not differ.
    @Test
    fun isForNestComparesHostAndPort() {
        data class Row(val nestUrl: String, val host: String, val port: Int, val expected: Boolean)
        val rows = listOf(
            Row("https://127.0.0.1:443", "127.0.0.1", 443, true),
            Row("https://nest.example.com", "nest.example.com", 443, true), // scheme default port
            Row("https://nest.example.com", "NEST.example.com", 443, true), // hosts are case-blind
            Row("https://[::1]:8443", "::1", 8443, true), // bracket-blind either way
            Row("https://[::1]:8443", "[::1]", 8443, true),
            Row("https://nest.example.com", "nest.example.com", 8443, false), // other port
            Row("https://nest.example.com", "elsewhere.example.com", 443, false), // a redirect elsewhere
            Row("https://127.0.0.1:443", "127.0.0.2", 443, false), // per host, not per /8
        )
        for (row in rows) {
            assertEquals("$row", row.expected, NestCertTrust.isForNest(row.host, row.port, row.nestUrl))
        }
    }

    @Test
    fun isForNestWithNoParseableNestUrlNamesNoNest() {
        // Signed out (`nodeUrl = ""`), or a string no URL parser would dial.
        assertFalse(NestCertTrust.isForNest("127.0.0.1", 443, ""))
        assertFalse(NestCertTrust.isForNest("127.0.0.1", 443, "127.0.0.1:443"))
    }

    // ── The trust manager over a REAL self-signed certificate ──

    @Test
    fun selfSignedCertIsNotWebPkiValid() {
        // The premise every arm below rests on: the OS trust store alone refuses
        // this cert.
        assertThrows(CertificateException::class.java) {
            NestCertTrust.platformTrustManager().checkServerTrusted(CHAIN, AUTH_TYPE)
        }
    }

    @Test
    fun trustManagerAcceptsTheSelfSignedFloorCertOnLoopback() {
        // The same-box-install case the e2e test walks (`self_signed_nest` binds
        // 127.0.0.1): WebPKI refuses, no pin is graduated, loopback carries it.
        // Everything real: the OS trust manager and the shared-Rust classifiers.
        val verdict = NestCertTrust.verdict(CHAIN[0], "127.0.0.1", 443, webPkiValid = false, nestUrl = LOOPBACK_NEST)
        assertTrue(verdict.forNest)
        assertTrue(verdict.isLoopback)
        assertEquals(32, verdict.certSpki?.size)
        assertNull(verdict.pinnedSpki)
        assertTrue(verdict.trusted)

        realTrustManager(LOOPBACK_NEST).checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("127.0.0.1", 443))
    }

    @Test
    fun trustManagerRefusesTheSelfSignedCertOffLoopbackWithNoPin() {
        // The refusal the row pins: a remote self-signed nest with no graduated
        // pin gets neither carve-out — never a blanket accept.
        val nest = "https://192.168.1.5:443"
        val verdict = NestCertTrust.verdict(CHAIN[0], "192.168.1.5", 443, webPkiValid = false, nestUrl = nest)
        assertTrue(verdict.forNest)
        assertFalse(verdict.isLoopback)
        assertEquals(32, verdict.certSpki?.size)
        assertNull(verdict.pinnedSpki)
        assertFalse(verdict.trusted)

        assertThrows(CertificateException::class.java) {
            realTrustManager(nest).checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("192.168.1.5", 443))
        }
    }

    @Test
    fun trustManagerRefusalIsThePlatformsOwn() {
        // A refused handshake fails as the OS would have failed it.
        val platformRefusal = CertificateException("the platform's own refusal")
        val manager = NestCertTrustManager(StubPlatform(platformRefusal), { "https://192.168.1.5:443" })
        val thrown = assertThrows(CertificateException::class.java) {
            manager.checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("192.168.1.5", 443))
        }
        assertSame(platformRefusal, thrown)
    }

    @Test
    fun trustManagerAcceptsAPinnedSelfSignedCertOffLoopback() {
        // The remote `test@<ip>` case: the leaf's SPKI (hashed by the real
        // shared-Rust hasher) equals the pin graduated for the nest's authority.
        val nest = "https://192.168.1.5:443"
        val pin = com.fauna.ffi.spkiSha256OfCertDer(TEST_CERT_DER)!!
        val pinned = PinnedClassifiers(authority = "192.168.1.5:443", pin = pin)
        val verdict = NestCertTrust.verdict(CHAIN[0], "192.168.1.5", 443, webPkiValid = false, nestUrl = nest, classifiers = pinned)
        assertFalse(verdict.isLoopback)
        assertArrayEquals(pin, verdict.certSpki)
        assertArrayEquals(pin, verdict.pinnedSpki)
        assertTrue(verdict.trusted)

        NestCertTrustManager(StubPlatform(REFUSAL), { nest }, pinned)
            .checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("192.168.1.5", 443))
    }

    @Test
    fun trustManagerRefusesACertThePinDisagreesWith() {
        val nest = "https://192.168.1.5:443"
        val pinned = PinnedClassifiers(authority = "192.168.1.5:443", pin = SPKI_B)
        assertThrows(CertificateException::class.java) {
            NestCertTrustManager(StubPlatform(REFUSAL), { nest }, pinned)
                .checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("192.168.1.5", 443))
        }
    }

    @Test
    fun trustManagerGrantsAPinOnlyUnderTheNestsOwnAuthority() {
        // A pin graduated for another authority is not this nest's.
        val pin = com.fauna.ffi.spkiSha256OfCertDer(TEST_CERT_DER)!!
        val pinned = PinnedClassifiers(authority = "192.168.1.6:443", pin = pin)
        assertThrows(CertificateException::class.java) {
            NestCertTrustManager(StubPlatform(REFUSAL), { "https://192.168.1.5:443" }, pinned)
                .checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("192.168.1.5", 443))
        }
    }

    // ── What a peer that is not the nest is granted (DirectNestClientCertCallbackTests.cs) ──

    @Test
    fun peerForTheNestKeepsTheLoopbackCarveOut() {
        // The control: the guard must not over-refuse.
        untrusting(LOOPBACK_NEST).checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("127.0.0.1", 443))
    }

    @Test
    fun peerForAnotherHostGetsWebPkiAlone() {
        // A loopback nest's carve-out must not leak to a redirect target: that
        // host's untrusted cert is judged as strict WebPKI, never accepted unverified.
        val verdict = NestCertTrust.verdict(
            CHAIN[0], "elsewhere.example.com", 443, webPkiValid = false, nestUrl = LOOPBACK_NEST,
        )
        assertFalse(verdict.forNest)
        assertFalse(verdict.trusted)
        assertThrows(CertificateException::class.java) {
            untrusting(LOOPBACK_NEST).checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("elsewhere.example.com", 443))
        }
    }

    @Test
    fun peerForAnotherHostStillAcceptsAWebPkiValidChain() {
        // "WebPKI alone" is the whole verdict — a genuinely valid chain passes.
        NestCertTrustManager(StubPlatform(refusal = null), { LOOPBACK_NEST })
            .checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("elsewhere.example.com", 443))
    }

    @Test
    fun peerForAnotherPortOnTheNestHostGetsWebPkiAlone() {
        // The nest's carve-out is its host AND port, not its host.
        assertThrows(CertificateException::class.java) {
            untrusting(LOOPBACK_NEST).checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("127.0.0.1", 8443))
        }
    }

    @Test
    fun handshakeNamingNoPeerFailsClosedToWebPkiAlone() {
        // Nothing names the host the cert was served for, so no carve-out
        // applies: the two-argument variant, a socket that is not a TLS socket,
        // and a TLS socket whose session names no host.
        val manager = untrusting(LOOPBACK_NEST)
        assertThrows(CertificateException::class.java) { manager.checkServerTrusted(CHAIN, AUTH_TYPE) }
        assertThrows(CertificateException::class.java) {
            manager.checkServerTrusted(CHAIN, AUTH_TYPE, mock(Socket::class.java))
        }
        assertThrows(CertificateException::class.java) {
            manager.checkServerTrusted(CHAIN, AUTH_TYPE, null as Socket?)
        }
        assertThrows(CertificateException::class.java) {
            manager.checkServerTrusted(CHAIN, AUTH_TYPE, null as SSLEngine?)
        }
        val namelessSession = sessionFor(host = null, port = 443)
        val nameless = mock(SSLSocket::class.java)
        `when`(nameless.handshakeSession).thenReturn(namelessSession)
        assertThrows(CertificateException::class.java) { manager.checkServerTrusted(CHAIN, AUTH_TYPE, nameless) }
    }

    @Test
    fun engineHandshakeIsDecidedLikeASockets() {
        val manager = untrusting(LOOPBACK_NEST)
        manager.checkServerTrusted(CHAIN, AUTH_TYPE, engineTo("127.0.0.1", 443))
        assertThrows(CertificateException::class.java) {
            manager.checkServerTrusted(CHAIN, AUTH_TYPE, engineTo("elsewhere.example.com", 443))
        }
    }

    @Test
    fun noNestConfiguredGrantsNoCarveOut() {
        // Signed out: `ApiClient.clearAuth` blanks the nest URL.
        assertThrows(CertificateException::class.java) {
            untrusting("").checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("127.0.0.1", 443))
        }
    }

    @Test
    fun theNestIsReadAtHandshakeTime() {
        // `ApiClient` is one instance across sign-ins: the carve-out follows the
        // nest it names NOW, and leaves the one it named before.
        var nest = LOOPBACK_NEST
        val manager = NestCertTrustManager(StubPlatform(REFUSAL), { nest })
        manager.checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("127.0.0.1", 443))

        nest = "https://nest.example.com"
        assertThrows(CertificateException::class.java) {
            manager.checkServerTrusted(CHAIN, AUTH_TYPE, socketTo("127.0.0.1", 443))
        }
    }

    @Test
    fun anExtendedPlatformIsAskedWithThePeer() {
        // Android's platform trust manager resolves per-domain network security
        // config from the peer; the two-argument variant would hide it.
        val platform = RecordingExtendedPlatform()
        val manager = NestCertTrustManager(platform, { LOOPBACK_NEST })
        val socket = socketTo("127.0.0.1", 443)
        val engine = engineTo("127.0.0.1", 443)

        manager.checkServerTrusted(CHAIN, AUTH_TYPE, socket)
        manager.checkServerTrusted(CHAIN, AUTH_TYPE, engine)
        manager.checkServerTrusted(CHAIN, AUTH_TYPE)

        assertEquals(listOf<Any?>(socket, engine, null), platform.askedWith)
    }

    // ── The hostname verifier ──

    @Test
    fun verifierAcceptsANameTheDefaultAccepts() {
        assertTrue(verifier(LOOPBACK_NEST, defaultSays = true).verify("elsewhere.example.com", sessionFor("elsewhere.example.com", 443)))
    }

    @Test
    fun verifierGrantsTheNestItsCarveOutOverANameMismatch() {
        // Loopback, for real classifiers; and a pinned LAN nest, whose floor
        // cert names `localhost` / `127.0.0.1` and never the address dialed.
        assertTrue(verifier(LOOPBACK_NEST, defaultSays = false).verify("127.0.0.1", sessionFor("127.0.0.1", 443)))

        val pin = com.fauna.ffi.spkiSha256OfCertDer(TEST_CERT_DER)!!
        val pinned = PinnedClassifiers(authority = "192.168.1.5:443", pin = pin)
        val lan = NestCertHostnameVerifier(HostnameVerifier { _, _ -> false }, { "https://192.168.1.5:443" }, pinned)
        assertTrue(lan.verify("192.168.1.5", sessionFor("192.168.1.5", 443)))
    }

    @Test
    fun verifierRefusesAMismatchedNameOffTheNest() {
        val verifier = verifier(LOOPBACK_NEST, defaultSays = false)
        assertFalse(verifier.verify("elsewhere.example.com", sessionFor("elsewhere.example.com", 443)))
        assertFalse(verifier.verify("127.0.0.1", sessionFor("127.0.0.1", 8443)))
        assertFalse(verifier.verify(null, sessionFor(null, 443)))
        assertFalse(verifier.verify("127.0.0.1", null))
    }

    @Test
    fun verifierRefusesAnUnpinnedRemoteNestOverANameMismatch() {
        val verifier = verifier("https://192.168.1.5:443", defaultSays = false)
        assertFalse(verifier.verify("192.168.1.5", sessionFor("192.168.1.5", 443)))
    }

    // ── Structural: the leg goes through the trust-deciding client ──

    @Test
    fun apiClientSendsThroughTheTrustDecidingClient() {
        val source = mainSource("core/ApiClient.kt").readText()
        assertTrue(source.contains("NestCertTrust.makeClient(okHttpClient) { nodeUrl }"))
        // Comments may name the base client to explain why it is NOT used; no
        // code line may send through it.
        val codeLines = source.lines().filterNot { it.trim().startsWith("//") || it.trim().startsWith("*") }
        assertFalse(codeLines.any { it.contains("okHttpClient.newCall") })
        assertFalse(codeLines.any { it.contains("private val okHttpClient") })
    }

    @Test
    fun noShippedSourceBlanketAcceptsACertOrAName() {
        // The classic mistake this policy exists to avoid: an accept-everything
        // trust manager or hostname verifier. The two adapters in
        // `NestCertTrust.kt` are the only JSSE trust code the app ships.
        val offenders = mainSource("").walkTopDown()
            .filter { it.isFile && it.extension == "kt" && it.name != "NestCertTrust.kt" }
            .filter { file ->
                val text = file.readText()
                listOf("X509TrustManager", "HostnameVerifier", "sslSocketFactory", "hostnameVerifier(").any(text::contains)
            }
            .map { it.name }
            .toList()
        assertEquals(emptyList<String>(), offenders)
    }

    // ── Fixtures ──

    /** The platform refusing (`refusal`) or accepting (`null`) every chain. */
    private class StubPlatform(private val refusal: CertificateException?) : X509TrustManager {
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            refusal?.let { throw it }
        }
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) = Unit
        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    /** An extended platform that records the peer handle each ask carried. */
    private class RecordingExtendedPlatform : X509ExtendedTrustManager() {
        val askedWith = mutableListOf<Any?>()
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?, socket: Socket?) {
            askedWith += socket
        }
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?, engine: SSLEngine?) {
            askedWith += engine
        }
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            askedWith += null
        }
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?, socket: Socket?) = Unit
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?, engine: SSLEngine?) = Unit
        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) = Unit
        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    /**
     * The real shared-Rust classifiers, with ONE graduated pin stood in —
     * nothing but a real WS handshake writes the process-global pin cache.
     */
    private class PinnedClassifiers(private val authority: String, private val pin: ByteArray) :
        NestTrustClassifiers by FfiNestTrustClassifiers {
        override fun pinnedSpkiForHost(authority: String): ByteArray? = pin.takeIf { authority == this.authority }
    }

    private companion object {
        val SPKI_A = ByteArray(32) // all-zero
        val SPKI_B = ByteArray(32) { 0xAB.toByte() }

        const val AUTH_TYPE = "ECDHE_ECDSA"
        const val LOOPBACK_NEST = "https://127.0.0.1:443"
        val REFUSAL = CertificateException("not WebPKI-valid")

        /**
         * A throwaway self-signed P-256 certificate (`CN=fauna-nest-test`, valid
         * to 2126, no private key anywhere) — the nest's floor cert as the OS
         * sees it, and the same fixture apple's `NestCertTrustTests.swift` reads.
         */
        val TEST_CERT_DER: ByteArray = Base64.getDecoder().decode(
            "MIIBJTCBzAIJAMM9dMoPVFbpMAoGCCqGSM49BAMCMBoxGDAWBgNVBAMMD2ZhdW5hLW5lc3QtdGVzdDAg" +
                "Fw0yNjA5MjMyMzAzMTZaGA8yMTI2MDgzMDIzMDMxNlowGjEYMBYGA1UEAwwPZmF1bmEtbmVzdC10ZXN0" +
                "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAExU1hv71xfaltT5u06o+QytXgAMNPNCm8tavnmpvp02Ac" +
                "6eHuzi4yknT9a/UiA3/CMUxovDHlG002Dxmg3+u26TAKBggqhkjOPQQDAgNIADBFAiBch8oLyxT8w6r+" +
                "s+srRDTd1ZfIIoX/LNZOajUHFI/MGgIhAJZ6mYD4X8ud++gxBBE5q6SeN5zpFcBnV422oNmvugBi",
        )

        val CHAIN: Array<X509Certificate> = arrayOf(
            CertificateFactory.getInstance("X.509")
                .generateCertificate(ByteArrayInputStream(TEST_CERT_DER)) as X509Certificate,
        )

        /** The real OS trust manager and the real shared-Rust classifiers. */
        fun realTrustManager(nestUrl: String) =
            NestCertTrustManager(NestCertTrust.platformTrustManager(), { nestUrl })

        /** A platform that refuses every chain, over the real classifiers. */
        fun untrusting(nestUrl: String) = NestCertTrustManager(StubPlatform(REFUSAL), { nestUrl })

        fun verifier(nestUrl: String, defaultSays: Boolean) =
            NestCertHostnameVerifier(HostnameVerifier { _, _ -> defaultSays }, { nestUrl })

        /** The handshake session of a TLS connection dialed at [host]:[port], serving [CHAIN]. */
        fun sessionFor(host: String?, port: Int): SSLSession {
            val session = mock(SSLSession::class.java)
            `when`(session.peerHost).thenReturn(host)
            `when`(session.peerPort).thenReturn(port)
            `when`(session.peerCertificates).thenReturn(arrayOf(CHAIN[0]))
            return session
        }

        fun socketTo(host: String, port: Int): SSLSocket {
            val session = sessionFor(host, port)
            val socket = mock(SSLSocket::class.java)
            `when`(socket.handshakeSession).thenReturn(session)
            return socket
        }

        fun engineTo(host: String, port: Int): SSLEngine {
            val session = sessionFor(host, port)
            val engine = mock(SSLEngine::class.java)
            `when`(engine.handshakeSession).thenReturn(session)
            return engine
        }

        /** `src/main/java/com/fauna/app/<relative>`, from the module dir Gradle runs unit tests in. */
        fun mainSource(relative: String): File {
            val file = File("src/main/java/com/fauna/app/$relative")
            check(file.exists()) { "no ${file.absolutePath} — unit tests must run from the app module dir" }
            return file
        }
    }
}
