package com.fauna.app.age

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import com.fauna.ffi.onboarding.AgeAttestationPlain
import com.fauna.ffi.onboarding.AgeClaimPlain
import com.fauna.ffi.onboarding.AgeNoncePlain
import java.nio.charset.StandardCharsets
import java.util.Base64

/**
 * The android **store-age arm** of the account age band
 * (`family-safety.md` § The account age band, D3 store signals + D5 attested
 * age-at-admission): before an admission call, ask the Play Store for the
 * user's age range, fold it to the band (shared Rust —
 * `ageBandFromAgeRange`), harden it with a Play Integrity *classic* verdict
 * over the machine-minted nonce, and hand the claim to the onboarding machine,
 * which carries it on the wire. Every step is **best-effort by design**: no
 * Play, no shared signal, or no attestation means the admission simply carries
 * less — a declared-only claim, or none — and the nest's provenance records
 * exactly that (the claim corroborates the admitting adult; it never decides).
 * The round also degrades when the nest cannot verify: the minted nonce lists
 * the attestation platforms the nest checks, and when `android` is not among
 * them the Play Integrity request is skipped (an economy — the machine strips
 * an unlisted attestation regardless).
 *
 * Two seams keep the round unit-testable on a plain JVM:
 * [StoreAgeSignals] is the Play half (the per-build-type `StoreAgeGlue` twins
 * — real under the Play-distributed build types, inert in `foss`), and
 * [AgeClaimMachine] is the three shared-Rust calls it makes. Nothing here
 * assembles a wire struct: the machine mints the nonce, the digest, and the
 * `age_claim` carry (priority #2).
 */

/** The store's age range as the platform shares it — either bound may be absent. */
data class StoreAgeRange(val lower: UInt?, val upper: UInt?)

/** The two Play calls the arm needs. */
interface StoreAgeSignals {
    /**
     * The user's shared age range, or null when the store shares none (the
     * user or parent declined, the region is not eligible, or a verified adult
     * shares no range). Needs the Activity: the access request may show Play's
     * own consent prompt.
     */
    suspend fun ageRange(activity: Activity): StoreAgeRange?

    /**
     * A Play Integrity classic-request verdict token bound to `nonceB64Url`
     * (the base64url-unpadded SHA-256 of the claim message). Throws when Play
     * cannot mint one — the caller degrades to a declared-only claim.
     */
    suspend fun integrityToken(activity: Activity, nonceB64Url: String): String
}

/** The three shared-Rust calls the round makes (a seam over `OnboardingMachine` + `ageBandFromAgeRange`). */
interface AgeClaimMachine {
    suspend fun requestAgeNonce(): AgeNoncePlain
    fun ageClaimDigest(nonceHex: String, band: String, applicationId: String): ByteArray
    fun bandFromAgeRange(lower: UInt?, upper: UInt?): String?
}

object StoreAgeClaim {
    /** `AgeAttestation.platform` for this arm — the nest's `"android"` seam. */
    const val PLATFORM = "android"

    /**
     * Build the claim for the admission ahead, or null when the store shared no
     * usable signal. `onWarn` receives the reason each time a step degrades.
     */
    suspend fun build(
        signals: StoreAgeSignals,
        machine: AgeClaimMachine,
        activity: Activity,
        applicationId: String,
        onWarn: (String) -> Unit = {},
    ): AgeClaimPlain? {
        val range = runCatching { signals.ageRange(activity) }
            .getOrElse { e ->
                onWarn("store age range unavailable, no claim: $e")
                null
            } ?: return null
        val band = machine.bandFromAgeRange(range.lower, range.upper) ?: return null
        val attestation = runCatching {
            val nonce = machine.requestAgeNonce()
            // An economy, never the guarantee (the machine strips an unlisted
            // attestation itself): a nest that does not list android cannot
            // check a Play verdict, so spend no classic request (the quota is
            // fleet-wide per app) — and no client-side "arming" constant, the
            // nonce's list is the only signal. An absent list reads as empty.
            if (PLATFORM !in nonce.attestationPlatforms) {
                onWarn("this nest cannot verify android attestations, claim is declared-only")
                return@runCatching null
            }
            val digest = machine.ageClaimDigest(nonce.nonceHex, band, applicationId)
            val nonceB64Url = Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
            val token = signals.integrityToken(activity, nonceB64Url)
            AgeAttestationPlain(
                platform = PLATFORM,
                nonceHex = nonce.nonceHex,
                keyIdHex = "",
                attestationObject = token.toByteArray(StandardCharsets.US_ASCII),
            )
        }.getOrElse { e ->
            onWarn("attestation unavailable, claim is declared-only: $e")
            null
        }
        return AgeClaimPlain(band = band, attestation = attestation)
    }
}

/** The Activity behind a Compose `LocalContext`, unwrapping context wrappers; null off-screen. */
fun Context.findActivity(): Activity? {
    var c: Context = this
    while (c is ContextWrapper) {
        if (c is Activity) return c
        c = c.baseContext
    }
    return null
}
