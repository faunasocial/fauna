package com.fauna.app.payments

import com.fauna.app.core.ApiClient

/**
 * The `zaps` plane's glue — **the built half** (`dynamic-features.md` §
 * Platform-family surface excision). `zaps` is a subset member of `payments`
 * and rides the SAME store-safe axis — excising `payments` excises `zaps`
 * with it (`libs/fauna-ffi/Cargo.toml`'s `zaps = […, "payments", …]`) — so
 * this glue lives in the same `src/payments` / `src/noPayments` split as
 * [PaymentsGlue.kt] rather than a plane of its own. Compiled into `debug` and
 * `release` only; `storeSafe` compiles the twin in `src/noPayments/` instead,
 * and the two files are the complete list of android code that may name a
 * `zaps` FFI symbol.
 *
 * Pure glue over `FfiNestClient.nostrZapSigners()` (the shared
 * `fauna-client-nostr`, priority #2), mapping the FFI record into the
 * app-owned [ZapSignerItem] seam type so no shared file has to name a symbol
 * the excised bindings lack. Lifted from `ApiClient`'s own former members
 * (the 2026-08-28 regression this file fixes — a member cannot be removed
 * per build variant while the class stays shared; an extension in a variant
 * source set can, exactly as [PaymentsGlue.kt] already established for the
 * `payments` plane).
 */

private fun ApiClient.nostrZapSignersRpc(): com.fauna.ffi.FfiNostrZapSignerClient = nestRpc().nostrZapSigners()

/** `fauna.nostr.zap_signers.list` — the caller's designated signers, newest
 *  first. An empty list is the meaningful out-of-the-box default (a payee
 *  who has designated nobody believes nobody), not a failed load. */
suspend fun ApiClient.nostrZapSignersList(): List<ZapSignerItem> =
    nostrZapSignersRpc().list().map {
        ZapSignerItem(
            id = it.id,
            signerPubkey = it.signerPubkey,
            label = it.label,
            createdAt = it.createdAt,
        )
    }

/** `fauna.nostr.zap_signers.add` — designate a signer (64-hex pubkey,
 *  normalized lowercase nest-side) with an optional label; idempotent, so
 *  re-adding refreshes the label. The stored row is discarded here, not
 *  narrowed — the caller (`NostrVM.addZapSigner`) always re-lists afterward,
 *  the same "render the stored form, never the typed input" property the
 *  re-list already buys. */
suspend fun ApiClient.nostrZapSignersAdd(signerPubkey: String, label: String) {
    nostrZapSignersRpc().add(signerPubkey, label)
}

/** `fauna.nostr.zap_signers.remove` — stop trusting a signer, keyed by the
 *  pubkey itself. Takes effect at the next receipt (the gate runs at ingest,
 *  so already-stored rows are unaffected). */
suspend fun ApiClient.nostrZapSignersRemove(signerPubkey: String) {
    nostrZapSignersRpc().remove(signerPubkey)
}
