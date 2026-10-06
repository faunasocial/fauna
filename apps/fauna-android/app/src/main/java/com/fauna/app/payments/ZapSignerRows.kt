package com.fauna.app.payments

/**
 * App-owned mirror of the `zaps` plane's one read model, and the reason it
 * exists at all (`dynamic-features.md` § Platform-family surface excision) —
 * the same reason [ProviderItem]/[ClaimItem] exist one plane over. `zaps` is
 * a subset member of `payments` (§ Charter members) and rides the SAME
 * android source-set split: **Kotlin has no inline compile-time exclusion**,
 * so `if (BuildConfig.PAYMENTS) { … }` is an ordinary runtime branch whose
 * body must still *typecheck*, and a store-safe build's generated bindings
 * carry no `FfiZapSignerEntry` / `FfiNostrZapSignerClient` at all. The shared
 * `NostrVM` / `NostrScreen` are therefore typed on *this* record; the
 * `src/payments` glue variant maps the FFI record into it, and the
 * `src/noPayments` twin returns an empty roster without ever naming a symbol
 * its bindings lack.
 *
 * Field-for-field mirror of `FfiZapSignerEntry` — deliberately not a
 * narrower projection, so the mapping stays a rename-free transcription and
 * a widened FFI record fails loudly here rather than being silently dropped.
 */

/**
 * One designated zap signer as the payee sees it (`nostr-zap-signer-item`;
 * `monetization.md` § Zap receipts — the trust model). Mirrors `FfiZapSignerEntry`.
 *
 * ⚠ [signerPubkey] is always the STORED form, never a typed input — the nest
 * normalizes to lowercase on write, and only that form ever matches a real
 * receipt.
 */
data class ZapSignerItem(
    val id: Long,
    val signerPubkey: String,
    val label: String,
    val createdAt: ULong,
)
