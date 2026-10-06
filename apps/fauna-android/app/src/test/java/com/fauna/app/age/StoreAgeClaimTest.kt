package com.fauna.app.age

import android.app.Activity
import com.fauna.ffi.onboarding.AgeNoncePlain
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.mockito.Mockito.mock
import java.util.Base64

/**
 * The store-age round's branching (`family-safety.md` § The account age band,
 * D3/D5) on a plain JVM: the Play half and the shared-Rust half are the two
 * seams `StoreAgeClaim` is written against, faked here. The fold itself
 * (`AgeBand::from_age_range`) and the digest are Rust-tested; what this pins is
 * the DEGRADATION contract — every step short of a verdict still yields the
 * most the store gave us, never an error the admission would trip on.
 */
class StoreAgeClaimTest {
    private val activity: Activity = mock(Activity::class.java)
    private val nonceHex = "ab".repeat(32)
    private val digest = ByteArray(32) { 7 }

    private inner class FakeMachine : AgeClaimMachine {
        var nonceFails = false
        // What the nest's nonce reply lists as verifiable; android by default.
        var platforms: List<String> = listOf("android")
        val digestCalls = mutableListOf<Triple<String, String, String>>()
        override suspend fun requestAgeNonce(): AgeNoncePlain {
            if (nonceFails) throw IllegalStateException("nest too old to mint a nonce")
            return AgeNoncePlain(nonceHex = nonceHex, expiresInSecs = 300uL, attestationPlatforms = platforms)
        }
        override fun ageClaimDigest(nonceHex: String, band: String, applicationId: String): ByteArray {
            digestCalls += Triple(nonceHex, band, applicationId)
            return digest
        }
        // The Rust fold's shape, minimally: the lower bound names the band.
        override fun bandFromAgeRange(lower: UInt?, upper: UInt?): String? =
            when (val anchor = lower ?: upper) {
                null -> null
                in 0u..12u -> "u13"
                in 13u..15u -> "13-15"
                in 16u..17u -> "16-17"
                else -> "18+"
            }
    }

    private class FakeSignals(
        private val range: StoreAgeRange?,
        private val token: String? = "eyJhbGciOiJBMjU2S1cifQ.x.y.z.w",
        private val rangeThrows: Boolean = false,
    ) : StoreAgeSignals {
        val nonces = mutableListOf<String>()
        override suspend fun ageRange(activity: Activity): StoreAgeRange? {
            if (rangeThrows) throw IllegalStateException("no Play Store on this device")
            return range
        }
        override suspend fun integrityToken(activity: Activity, nonceB64Url: String): String {
            nonces += nonceB64Url
            return token ?: throw IllegalStateException("Play Integrity unavailable")
        }
    }

    @Test
    fun a_shared_range_with_a_verdict_yields_an_attested_claim_over_the_digest() = runBlocking {
        val machine = FakeMachine()
        val signals = FakeSignals(StoreAgeRange(13u, 15u))
        val claim = StoreAgeClaim.build(signals, machine, activity, "social.fauna.fauna")!!

        assertEquals("13-15", claim.band)
        val att = claim.attestation!!
        assertEquals("android", att.platform)
        assertEquals(nonceHex, att.nonceHex)
        assertEquals("", att.keyIdHex)
        assertEquals("eyJhbGciOiJBMjU2S1cifQ.x.y.z.w", String(att.attestationObject, Charsets.US_ASCII))
        // The digest was minted for exactly this nonce/band/app, and Play saw
        // it base64url-unpadded — the nest's nonce contract.
        assertEquals(listOf(Triple(nonceHex, "13-15", "social.fauna.fauna")), machine.digestCalls)
        assertEquals(listOf(Base64.getUrlEncoder().withoutPadding().encodeToString(digest)), signals.nonces)
    }

    @Test
    fun a_nest_that_does_not_list_android_gets_no_play_request_and_a_declared_only_claim() = runBlocking {
        // Only ios listed, and nothing listed (no store keys armed):
        // the classic request is never spent and the digest is never minted.
        for (listed in listOf(listOf("ios"), emptyList())) {
            val machine = FakeMachine().apply { platforms = listed }
            val signals = FakeSignals(StoreAgeRange(13u, 15u))
            val warnings = mutableListOf<String>()
            val claim = StoreAgeClaim.build(signals, machine, activity, "social.fauna.fauna") { warnings += it }!!

            assertEquals("13-15", claim.band)
            assertNull(claim.attestation)
            assertEquals(emptyList<String>(), signals.nonces)
            assertEquals(emptyList<Triple<String, String, String>>(), machine.digestCalls)
            assertEquals(1, warnings.size)
        }
    }

    @Test
    fun no_shared_range_means_no_claim_at_all() = runBlocking {
        assertNull(StoreAgeClaim.build(FakeSignals(null), FakeMachine(), activity, "social.fauna.fauna"))
        assertNull(StoreAgeClaim.build(FakeSignals(StoreAgeRange(null, null)), FakeMachine(), activity, "social.fauna.fauna"))
    }

    @Test
    fun a_device_without_play_degrades_to_no_claim_not_an_error() = runBlocking {
        val warnings = mutableListOf<String>()
        val claim = StoreAgeClaim.build(
            FakeSignals(StoreAgeRange(13u, 15u), rangeThrows = true), FakeMachine(), activity, "social.fauna.fauna",
        ) { warnings += it }
        assertNull(claim)
        assertEquals(1, warnings.size)
    }

    @Test
    fun a_failed_attestation_degrades_to_a_declared_only_claim() = runBlocking {
        val declared = StoreAgeClaim.build(FakeSignals(StoreAgeRange(18u, null), token = null), FakeMachine(), activity, "social.fauna.fauna")!!
        assertEquals("18+", declared.band)
        assertNull(declared.attestation)

        val noNonce = FakeMachine().apply { nonceFails = true }
        val stillDeclared = StoreAgeClaim.build(FakeSignals(StoreAgeRange(null, 12u)), noNonce, activity, "social.fauna.fauna")!!
        assertEquals("u13", stillDeclared.band)
        assertNull(stillDeclared.attestation)
    }
}
