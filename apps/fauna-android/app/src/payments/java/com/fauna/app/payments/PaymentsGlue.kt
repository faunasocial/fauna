package com.fauna.app.payments

import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil

/**
 * The `payments` plane's glue — **the built half** (`dynamic-features.md`
 * § Platform-family surface excision). Compiled into `debug` and `release`
 * only; `storeSafe` compiles the twin in `src/noPayments/` instead, and the two
 * files are the complete list of android code that may name a `payments` FFI
 * symbol.
 *
 * Everything here is pure glue over `FfiNestClient.payments()` (the shared
 * `fauna-client-payments`, priority #2), mapping the FFI records into the
 * app-owned [ProviderItem] / [ClaimItem] seam types so no shared file has to
 * name a symbol the excised bindings lack. It lifts the linux lead
 * verbatim in behavior; only the return types changed.
 *
 * **Why extensions rather than members of `ApiClient`:** these lived as
 * `ApiClient` methods until the excision leg, but a member cannot be removed
 * per build variant while the class stays shared. Extensions in a variant
 * source set can — and the call sites (`api.paymentsProvidersList()`) read
 * identically, so the seam costs an import and nothing else.
 */

/** Every registered provider kind — the §4 form's kind select enumerates these
 *  (the shared `fauna-payments` registry the nest validates against). */
fun ApiClient.paymentsKnownKinds(): List<String> = com.fauna.ffi.paymentsKnownKinds()

/** §4 — the author's configured providers (the rows never carry the webhook secret). */
suspend fun ApiClient.paymentsProvidersList(): List<ProviderItem> =
    paymentsClient().providersList().map {
        ProviderItem(
            kind = it.kind,
            tier = it.tier,
            createdAt = it.createdAt,
            lastVerifiedAt = it.lastVerifiedAt,
            lastRejectedAt = it.lastRejectedAt,
        )
    }

/** §4 add — upsert one provider config (kind + webhook-verification secret + entitled tier). */
suspend fun ApiClient.paymentsProvidersSet(kind: String, webhookSecret: String, tier: String) {
    paymentsClient().providersSet(kind, webhookSecret, tier)
}

/** §4 remove — delete the config for one provider kind (idempotent). */
suspend fun ApiClient.paymentsProvidersRemove(kind: String) {
    paymentsClient().providersRemove(kind)
}

/** §4 — the exact webhook URL to register at [kind]'s provider dashboard, live-previewed
 *  as the kind select changes. Pure local compute (no nest round-trip) — derived from the
 *  same constant the nest builds its ingress route from, so no client hand-assembles the
 *  path. Empty until the session has authenticated (no secret cached yet). */
fun ApiClient.paymentsWebhookUrl(kind: String): String {
    val secretBytes = secret?.let { HexUtil.hexToBytes(it) } ?: return ""
    val actorIdHex = com.fauna.ffi.hexFull(com.fauna.ffi.actorIdFromSecret(secretBytes))
    return com.fauna.ffi.paymentsWebhookUrl(nodeUrl, actorIdHex, kind)
}

/** Redeem a post-payment claim code — binds the entitlement to this actor;
 *  the queued grant lands through Pillar 1's queue like a queued subscribe. */
suspend fun ApiClient.paymentsClaimsRedeem(code: String) {
    paymentsClient().claimsRedeem(code)
}

/** §5 mint — the author mints a manual claim code for [tier] (no expiry; `provider` is
 *  always `"manual"` on the wire, for a no-API provider already paid out-of-band). */
suspend fun ApiClient.paymentsClaimsMint(tier: String) {
    paymentsClient().claimsMint(tier, null)
}

/** §5 — the author's own claim codes, newest first; the audit surface for BOTH
 *  manually- and webhook-minted codes. */
suspend fun ApiClient.paymentsClaimsList(): List<ClaimItem> =
    paymentsClient().claimsList().map {
        ClaimItem(
            code = it.code,
            tier = it.tier,
            provider = it.provider,
            validUntil = it.validUntil,
            createdAt = it.createdAt,
            redeemedBy = it.redeemedBy,
            redeemedAt = it.redeemedAt,
            voidedAt = it.voidedAt,
        )
    }

private fun ApiClient.paymentsClient(): com.fauna.ffi.FfiPaymentsClient = nestRpc().payments()
