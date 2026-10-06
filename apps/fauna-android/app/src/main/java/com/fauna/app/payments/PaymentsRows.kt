package com.fauna.app.payments

/**
 * App-owned mirrors of the `payments` plane's read models, and the reason they
 * exist at all (`dynamic-features.md` § Platform-family surface excision).
 *
 * Every other shell renders the FFI records directly, because its compile
 * condition removes the render and the record reference together — a cargo
 * `#[cfg]` on tui/linux, a Swift `#if` on apple. **Kotlin has no inline
 * compile-time exclusion**: `if (BuildConfig.PAYMENTS) { … }` is an ordinary
 * runtime branch whose body must still *typecheck*, so a store-safe build —
 * which links a `fauna-ffi` with `payments` excised, and therefore has no
 * `FfiProviderItem` / `FfiClaimItem` in its generated bindings at all — cannot
 * compile a shared file that names one. The android shape is therefore
 * **variant source sets for the glue** (`src/payments` vs `src/noPayments`,
 * wired in `build.gradle.kts`) with these types as the seam between them: the
 * shared UI and view models are typed on *these*, the payments variant maps the
 * FFI records into them, and the excised variant returns empty lists without
 * ever naming a symbol its bindings lack.
 *
 * Field-for-field mirrors of `FfiProviderItem` / `FfiClaimItem` — deliberately
 * not a narrower projection, so the mapping stays a rename-free transcription
 * and a widened FFI record fails loudly here rather than being silently dropped.
 */

/**
 * One configured payment provider as the author sees it (`subscription-provider-row`;
 * `monetization.md` § Pillar 3). Mirrors `FfiProviderItem`.
 *
 * Carries no webhook secret — the nest's list reply deliberately omits it, and
 * changing a secret means re-entering it in the §4 form.
 */
data class ProviderItem(
    val kind: String,
    val tier: String,
    /** Config creation time in microseconds since the Unix epoch. */
    val createdAt: ULong,
    /**
     * Evidence-based provider-status stamps, epoch **seconds** (unlike
     * [createdAt]'s microseconds). Both `null` = no webhook delivery seen yet.
     * Feed both into the shared `providerStatusLabel` for the §4 status badge —
     * never re-derive the branch per-app.
     */
    val lastVerifiedAt: ULong?,
    val lastRejectedAt: ULong?,
)

/**
 * One claim code as the author sees it (`subscription-claim-row`) — the audit
 * surface for BOTH manually- and webhook-minted codes. Mirrors `FfiClaimItem`.
 */
data class ClaimItem(
    val code: String,
    val tier: String,
    /**
     * `"manual"` for an author-minted code, else the provider kind that minted
     * it (`"fake"`, `"stripe"`, …).
     */
    val provider: String,
    val validUntil: ULong?,
    val createdAt: ULong,
    /**
     * Who redeemed it, if anyone — `null` means still unredeemed (32-byte
     * actor id). The §5 status label reads only its presence.
     */
    val redeemedBy: ByteArray?,
    val redeemedAt: ULong?,
    /** Set when a refund/dispute voided an unredeemed claim. */
    val voidedAt: ULong?,
) {
    // `redeemedBy` is an array, so the generated structural equals would compare
    // it by identity. Nothing compares these rows for equality today (they are
    // rendered, never diffed), but a data class silently promises otherwise —
    // so pin the honest comparison rather than leave the trap for a later
    // `distinctBy`/`==` to find.
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is ClaimItem) return false
        return code == other.code &&
            tier == other.tier &&
            provider == other.provider &&
            validUntil == other.validUntil &&
            createdAt == other.createdAt &&
            redeemedBy.contentEqualsOrBothNull(other.redeemedBy) &&
            redeemedAt == other.redeemedAt &&
            voidedAt == other.voidedAt
    }

    override fun hashCode(): Int {
        var result = code.hashCode()
        result = 31 * result + tier.hashCode()
        result = 31 * result + provider.hashCode()
        result = 31 * result + (validUntil?.hashCode() ?: 0)
        result = 31 * result + createdAt.hashCode()
        result = 31 * result + (redeemedBy?.contentHashCode() ?: 0)
        result = 31 * result + (redeemedAt?.hashCode() ?: 0)
        result = 31 * result + (voidedAt?.hashCode() ?: 0)
        return result
    }
}

private fun ByteArray?.contentEqualsOrBothNull(other: ByteArray?): Boolean =
    if (this == null || other == null) this == null && other == null else contentEquals(other)
