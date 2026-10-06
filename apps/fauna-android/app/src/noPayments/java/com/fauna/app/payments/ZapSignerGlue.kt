package com.fauna.app.payments

import com.fauna.app.core.ApiClient

/**
 * The `zaps` plane's glue — **the excised half** (`dynamic-features.md` §
 * Platform-family surface excision, § The App-Store escape hatch). Compiled
 * into the `storeSafe` build type only, in place of the `src/payments/`
 * twin.
 *
 * Signature-for-signature identical to that twin, and deliberately naming
 * **no `zaps` FFI symbol at all** — a `storeSafe` build links a `fauna-ffi`
 * built `--no-default-features --features store-safe`, whose generated
 * Kotlin bindings carry no `FfiNostrZapSignerClient` and no
 * `FfiZapSignerEntry`. That absence is the point: it makes a half-excised
 * build a **compile error** rather than a silent leak, so this file failing
 * to compile is the escape hatch working.
 *
 * **These are inert, not silently-dropped commands.** e2e convention 11
 * forbids an agent quietly swallowing a command, and this is the deliberate
 * exception the convention's own boundary allows: nothing can call these,
 * because [com.fauna.app.ui.screen.nostr.NostrScreen]'s *Zap signers*
 * section carries the same `BuildConfig.PAYMENTS` condition every other
 * gated render does and is not rendered in this flavor. There is no runtime
 * flag, hidden setting or agent command that reaches them — criterion 5
 * ("no re-enable path") is what makes an empty body the honest
 * implementation instead of an error.
 */

/** Excised — see the file doc. No signer is ever designated in this flavor. */
suspend fun ApiClient.nostrZapSignersList(): List<ZapSignerItem> = emptyList()

/** Excised — the *Zap signers* add control is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.nostrZapSignersAdd(signerPubkey: String, label: String) = Unit

/** Excised — the *Zap signers* per-row remove control is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.nostrZapSignersRemove(signerPubkey: String) = Unit
