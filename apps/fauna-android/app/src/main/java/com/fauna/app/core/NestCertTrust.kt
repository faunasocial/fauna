package com.fauna.app.core

import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.OkHttpClient
import java.net.Socket
import java.security.KeyStore
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLEngine
import javax.net.ssl.SSLPeerUnverifiedException
import javax.net.ssl.SSLSession
import javax.net.ssl.SSLSocket
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509ExtendedTrustManager
import javax.net.ssl.X509TrustManager

/**
 * The pure trust decision android's residual-HTTP leg — [ApiClient]'s OkHttp
 * client (blob upload / download / `HEAD`) — applies to the nest's TLS cert, so
 * a same-box or remote self-signed nest is trusted the SAME way the rest of the
 * Rust stack trusts it: via the shared pinned SPKI. Member for member the twin
 * of windows' `NestCertTrust.cs` and apple's `NestCertTrust.swift`.
 *
 * Without it the leg made no trust decision at all — a plain `OkHttpClient`
 * validates against the OS trust store and refuses the nest's self-signed floor
 * cert (`SSLHandshakeException`), so media broke against a same-box
 * `https://127.0.0.1` nest while every Rust (rustls) leg accepted it.
 *
 * The SPKI computation, the pin lookup and the loopback classification are
 * shared Rust (`spkiSha256OfCertDer` / `pinnedSpkiForHost` /
 * `isLoopbackAuthority` / `authorityOf` — `fauna_ffi::trust`), so the value
 * compared here is byte-identical to the pin the WS handshake graduated: no JCA
 * re-encoding, no hand-rolled IPv6 parse. This file is only the platform-side
 * policy ([shouldTrust], [isForNest]) and its two JSSE adapters
 * ([NestCertTrustManager], [NestCertHostnameVerifier]).
 * `docs/goal/architecture/security.md` § Transport trust, the residual-HTTP legs.
 */
object NestCertTrust {
    /**
     * Decide whether to trust a served cert: accept iff the chain is
     * WebPKI-valid (public-CA / ACME nest), OR the host is loopback (same-box
     * install — the sanctioned `danger_accept_invalid_certs` prior art; also
     * covers an unauthenticated call that can run before any handshake
     * graduates a pin), OR the served cert's SPKI matches the pin the WS
     * handshake graduated for this host. Fail-closed otherwise: no pin, no
     * parseable cert, or a pin the cert disagrees with all refuse.
     */
    fun shouldTrust(
        certSpki: ByteArray?,
        pinnedSpki: ByteArray?,
        webPkiValid: Boolean,
        isLoopback: Boolean,
    ): Boolean {
        if (webPkiValid) return true
        if (isLoopback) return true
        return certSpki != null && pinnedSpki != null && certSpki.contentEquals(pinnedSpki)
    }

    /**
     * Whether a TLS peer is the nest the client was built for. The loopback
     * carve-out and the pin are the *configured nest's* (keyed on
     * `authorityOf(nestUrl)` — the port-as-written pin key the handshake used,
     * which a peer's host/port pair cannot rebuild), so they are granted only
     * to that host and port: OkHttp follows a 30x by default and handshakes
     * with the redirect target, and a redirect anywhere else falls to strict
     * WebPKI. Host compare is case-insensitive and bracket-blind; the port
     * defaults per scheme the way a URL parser dials — the nest URL is read by
     * OkHttp's own parser, the one that dials it. Member for member the twin
     * of `NestCertTrust.IsForNest` (C#) and `challengeIsForNest` (Swift) — two
     * legs of one policy must not differ. Fail-closed: a nest URL that does
     * not parse names no nest.
     */
    fun isForNest(requestHost: String, requestPort: Int, nestUrl: String): Boolean {
        val nest = nestUrl.toHttpUrlOrNull() ?: return false
        return bareHost(requestHost) == bareHost(nest.host) && requestPort == nest.port
    }

    /**
     * One evaluation of a served cert, with every input the decision read —
     * the unit tests assert these, not just [trusted].
     */
    class Verdict(
        val trusted: Boolean,
        val forNest: Boolean,
        val webPkiValid: Boolean,
        val isLoopback: Boolean,
        val certSpki: ByteArray?,
        val pinnedSpki: ByteArray?,
    )

    /**
     * Evaluate [leaf], served by [host]:[port], under the three-way policy.
     * [webPkiValid] is the platform's own answer for the half the calling
     * adapter owns (the chain for the trust manager, the name for the hostname
     * verifier). A peer that is not the nest's — or that names no host — gets
     * that answer alone: no carve-out, and no classifier is even read.
     */
    internal fun verdict(
        leaf: X509Certificate?,
        host: String?,
        port: Int,
        webPkiValid: Boolean,
        nestUrl: String,
        classifiers: NestTrustClassifiers = FfiNestTrustClassifiers,
    ): Verdict {
        if (host == null || !isForNest(host, port, nestUrl)) {
            return Verdict(
                trusted = webPkiValid, forNest = false, webPkiValid = webPkiValid,
                isLoopback = false, certSpki = null, pinnedSpki = null,
            )
        }
        // The pin key is shared Rust's `authorityOf`, which keeps a written
        // default `:443` (OkHttp's parser reads it as the scheme default), so
        // the lookup key is the same string Rust pinned.
        val authority = classifiers.authorityOf(nestUrl)
        val isLoopback = classifiers.isLoopbackAuthority(authority)
        val certSpki = leaf?.let { classifiers.spkiSha256OfCertDer(it.encoded) }
        val pinnedSpki = classifiers.pinnedSpkiForHost(authority)
        return Verdict(
            trusted = shouldTrust(certSpki, pinnedSpki, webPkiValid, isLoopback),
            forNest = true, webPkiValid = webPkiValid, isLoopback = isLoopback,
            certSpki = certSpki, pinnedSpki = pinnedSpki,
        )
    }

    /**
     * The OkHttp client every [ApiClient] call sends through: [base]'s
     * timeouts and pool, plus the trust manager and hostname verifier for the
     * nest [nestUrl] names *at handshake time* — [ApiClient] is one instance
     * across sign-ins, so the adapters read the current nest rather than
     * capture one.
     */
    fun makeClient(base: OkHttpClient, nestUrl: () -> String): OkHttpClient {
        val trustManager = NestCertTrustManager(platformTrustManager(), nestUrl)
        val sslContext = SSLContext.getInstance("TLS")
        sslContext.init(null, arrayOf(trustManager), null)
        return base.newBuilder()
            .sslSocketFactory(sslContext.socketFactory, trustManager)
            .hostnameVerifier(NestCertHostnameVerifier(base.hostnameVerifier, nestUrl))
            .build()
    }

    /** The OS trust store's own trust manager — the WebPKI half of the policy. */
    internal fun platformTrustManager(): X509TrustManager {
        val factory = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm())
        factory.init(null as KeyStore?)
        return factory.trustManagers.filterIsInstance<X509TrustManager>().first()
    }

    private fun bareHost(host: String): String {
        val h = host.lowercase()
        return if (h.length >= 2 && h.startsWith("[") && h.endsWith("]")) h.substring(1, h.length - 1) else h
    }
}

/**
 * The four shared-Rust classifiers the policy reads. A seam only so a unit
 * test can stand a graduated pin in (nothing but a real WS handshake writes
 * one); production is always [FfiNestTrustClassifiers].
 */
internal interface NestTrustClassifiers {
    fun authorityOf(nestUrl: String): String
    fun isLoopbackAuthority(authority: String): Boolean
    fun pinnedSpkiForHost(authority: String): ByteArray?
    fun spkiSha256OfCertDer(certDer: ByteArray): ByteArray?
}

internal object FfiNestTrustClassifiers : NestTrustClassifiers {
    override fun authorityOf(nestUrl: String): String = com.fauna.ffi.authorityOf(nestUrl)
    override fun isLoopbackAuthority(authority: String): Boolean = com.fauna.ffi.isLoopbackAuthority(authority)
    override fun pinnedSpkiForHost(authority: String): ByteArray? = com.fauna.ffi.pinnedSpkiForHost(authority)
    override fun spkiSha256OfCertDer(certDer: ByteArray): ByteArray? = com.fauna.ffi.spkiSha256OfCertDer(certDer)
}

/**
 * The chain half of the JSSE adapter: answers "is this served chain trusted"
 * with [NestCertTrust.verdict], where `webPkiValid` is [platform]'s own
 * verdict. An `X509ExtendedTrustManager` because only the socket / engine
 * variants say WHICH peer served the chain; the two-argument variant names no
 * host, so it grants no carve-out (strict WebPKI, fail-closed). A refusal
 * rethrows the platform's own exception — nothing here can accept more than
 * the policy allows, and a refused handshake fails as the OS would have failed
 * it. Holds only the nest-URL reader, never the client.
 */
class NestCertTrustManager internal constructor(
    private val platform: X509TrustManager,
    private val nestUrl: () -> String,
    private val classifiers: NestTrustClassifiers,
) : X509ExtendedTrustManager() {
    constructor(platform: X509TrustManager, nestUrl: () -> String) :
        this(platform, nestUrl, FfiNestTrustClassifiers)

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?, socket: Socket?) =
        decide(chain, (socket as? SSLSocket)?.handshakeSession) {
            if (platform is X509ExtendedTrustManager) platform.checkServerTrusted(chain, authType, socket)
            else platform.checkServerTrusted(chain, authType)
        }

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?, engine: SSLEngine?) =
        decide(chain, engine?.handshakeSession) {
            if (platform is X509ExtendedTrustManager) platform.checkServerTrusted(chain, authType, engine)
            else platform.checkServerTrusted(chain, authType)
        }

    override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) =
        decide(chain, session = null) { platform.checkServerTrusted(chain, authType) }

    private fun decide(chain: Array<out X509Certificate>?, session: SSLSession?, platformCheck: () -> Unit) {
        val refusal = try {
            platformCheck()
            null
        } catch (e: CertificateException) {
            e
        }
        val verdict = NestCertTrust.verdict(
            leaf = chain?.firstOrNull(),
            host = session?.peerHost,
            port = session?.peerPort ?: -1,
            webPkiValid = refusal == null,
            nestUrl = nestUrl(),
            classifiers = classifiers,
        )
        if (!verdict.trusted) throw refusal ?: CertificateException("nest certificate refused")
    }

    // The leg never authenticates a client; these are the platform's own.
    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?, socket: Socket?) {
        if (platform is X509ExtendedTrustManager) platform.checkClientTrusted(chain, authType, socket)
        else platform.checkClientTrusted(chain, authType)
    }

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?, engine: SSLEngine?) {
        if (platform is X509ExtendedTrustManager) platform.checkClientTrusted(chain, authType, engine)
        else platform.checkClientTrusted(chain, authType)
    }

    override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) =
        platform.checkClientTrusted(chain, authType)

    override fun getAcceptedIssuers(): Array<X509Certificate> = platform.acceptedIssuers
}

/**
 * The name half of the JSSE adapter. JSSE splits what windows' `SslPolicyErrors`
 * and apple's `SecTrust` answer at once — chain validity ([NestCertTrustManager])
 * and name match (here) — so the same verdict runs on both halves: [default]'s
 * answer is this half's `webPkiValid`, and a name [default] refuses is accepted
 * only under the nest's own carve-outs (a self-signed floor cert names
 * `localhost` / `127.0.0.1`, never the LAN address a pinned nest is dialed at).
 * Both halves must accept, so the connection stands iff the chain AND the name
 * are valid, OR the peer is the nest and a carve-out holds. Never a blanket
 * accept: any other host gets [default] alone.
 */
class NestCertHostnameVerifier internal constructor(
    private val default: HostnameVerifier,
    private val nestUrl: () -> String,
    private val classifiers: NestTrustClassifiers,
) : HostnameVerifier {
    constructor(default: HostnameVerifier, nestUrl: () -> String) :
        this(default, nestUrl, FfiNestTrustClassifiers)

    override fun verify(hostname: String?, session: SSLSession?): Boolean {
        val leaf = try {
            session?.peerCertificates?.firstOrNull() as? X509Certificate
        } catch (_: SSLPeerUnverifiedException) {
            null
        }
        return NestCertTrust.verdict(
            leaf = leaf,
            host = hostname,
            port = session?.peerPort ?: -1,
            webPkiValid = default.verify(hostname, session),
            nestUrl = nestUrl(),
            classifiers = classifiers,
        ).trusted
    }
}
