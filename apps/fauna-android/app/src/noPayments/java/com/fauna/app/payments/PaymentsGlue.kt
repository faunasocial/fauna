package com.fauna.app.payments

import com.fauna.app.core.ApiClient
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The `payments` plane's glue — **the excised half** (`dynamic-features.md`
 * § Platform-family surface excision, § The App-Store escape hatch). Compiled
 * into the `storeSafe` build type only, in place of the `src/payments/` twin.
 *
 * Signature-for-signature identical to that twin, and deliberately naming **no
 * `payments` FFI symbol at all** — a `storeSafe` build links a `fauna-ffi`
 * built `--no-default-features --features store-safe,tunnel`, whose generated
 * Kotlin bindings carry no `FfiPaymentsClient`, no `FfiProviderItem` /
 * `FfiClaimItem` and no `paymentsKnownKinds` / `paymentsWebhookUrl`. That absence is the point: it makes a
 * half-excised build a **compile error** rather than a silent leak, so this
 * file failing to compile is the escape hatch working.
 *
 * **These are inert, not silently-dropped commands.** e2e convention 11 forbids
 * an agent quietly swallowing a command, and this is the deliberate exception
 * the convention's own boundary allows: nothing can call these, because every
 * caller's UI carries the same `BuildConfig.PAYMENTS` condition and is not
 * rendered in this flavor. There is no runtime flag, hidden setting or agent
 * command that reaches them — criterion 5 ("no re-enable path") is what makes
 * an empty body the honest implementation instead of an error.
 */

/** Excised — see the file doc. No provider kinds exist in this flavor. */
fun ApiClient.paymentsKnownKinds(): List<String> = emptyList()

/** Excised — the §4 provider section is not rendered in this flavor. */
suspend fun ApiClient.paymentsProvidersList(): List<ProviderItem> = emptyList()

/** Excised — the §4 add form is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.paymentsProvidersSet(kind: String, webhookSecret: String, tier: String) = Unit

/** Excised — the §4 provider rows are not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.paymentsProvidersRemove(kind: String) = Unit

/** Excised — the §4 form's live webhook-URL preview is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
fun ApiClient.paymentsWebhookUrl(kind: String): String = ""

/** Excised — the §4 form's return-URL display is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
fun paymentsReturnUrl(siteOrigin: String, kind: String): String = ""

/** Excised — no web-site origin is read for the §4 return-URL display in this flavor. */
@Singleton
class PaymentsSiteOrigin @Inject constructor() {
    val origin: StateFlow<String?> = MutableStateFlow(null)

    fun ensure() = Unit
}

/** Excised — the claim-redemption row is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.paymentsClaimsRedeem(code: String) = Unit

/** Excised — the §5 mint control is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.paymentsClaimsMint(tier: String) = Unit

/** Excised — the §5 Void control is not rendered in this flavor. */
@Suppress("UNUSED_PARAMETER")
suspend fun ApiClient.paymentsClaimsVoid(code: String) = Unit

/** Excised — the §5 claim list is not rendered in this flavor. */
suspend fun ApiClient.paymentsClaimsList(): List<ClaimItem> = emptyList()
