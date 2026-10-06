package com.fauna.app.core

import com.fauna.ffi.FfiNestClient

/**
 * The [ApiClient] members the **kids** build type excises — **the excised
 * half** (`family-safety.md` § The account age band, the kids-app bullet,
 * item (4); `dynamic-features.md` § Compile-time excision). Compiled into the
 * `kids` build type only, in place of the `src/noKids/` twin.
 *
 * It declares only the members `src/main` still names, each inert or an honest
 * refusal, and deliberately names **no** symbol the kids `fauna-ffi` flavor lacks: that
 * absence is what turns a half-excised build into a compile error rather than
 * a silent leak. Everything else the built twin carries (the feed, search,
 * every bridge, web publishing, the subscriptions author calls, connected apps,
 * the labeler catalog) is called only from `src/noKids/`, which this build type
 * never compiles. These are inert rather than silently dropped commands: no
 * caller in this flavor can reach a surface that would need them.
 */
abstract class KidsExcisedApi {
    /** Excised — this flavor holds no feed / search / atproto singleton to tear down. */
    internal fun detachKidsExcised(): () -> Unit = {}

    /** Excised — the TLS auto-renew cadence rides the DNS machine (a bridge); a
     *  kids account is never the admin who would renew a certificate. */
    internal fun startAutoRenewCadence() = Unit

    /** Excised — the subscriptions author plane (monetization) is compiled out. */
    @Suppress("UNUSED_PARAMETER")
    internal fun startSubscriptionsAuthorPump(client: FfiNestClient, secretBytes: ByteArray) = Unit

    /** Excised — the mail-relay hook is a bridge; the caller builds the plain machine. */
    @Suppress("UNUSED_PARAMETER")
    internal fun linkedNestsMachineWithMailRelay(
        client: FfiNestClient,
        secretBytes: ByteArray,
    ): uniffi.fauna_client_pair.LinkedNestsMachine? = null

    // The subscriptions author calls (monetization). Their surfaces — the
    // profile Tiers tab, the Offers tab and the Follow button — carry
    // `!BuildConfig.KIDS` and are not rendered in this flavor, so nothing
    // reaches these; a call that somehow did is refused, never faked.

    @Suppress("UNUSED_PARAMETER")
    suspend fun subscriptionCreateTier(
        name: String,
        rank: UInt,
        description: String?,
        priceHint: String?,
        paymentUrl: String?,
        autoApprove: Boolean,
        askingPriceSats: ULong? = null,
    ): Boolean = throw excised("subscriptions")

    @Suppress("UNUSED_PARAMETER")
    suspend fun subscriptionApproveRequest(
        request: com.fauna.ffi.FfiPendingRequest,
    ): com.fauna.ffi.FfiApproveReply = throw excised("subscriptions")

    @Suppress("UNUSED_PARAMETER")
    suspend fun subscriptionRemoveSubscriber(tierName: String, subscriberId: ByteArray): Unit =
        throw excised("subscriptions")

    @Suppress("UNUSED_PARAMETER")
    suspend fun subscriptionSubscribe(authorIdHex: String, tier: String): com.fauna.ffi.FfiSubscribeReply =
        throw excised("subscriptions")

    // Sealed tier-1 spam training rides the mail-settings machine, a bridge:
    // there is no sealed model here, so the callers take their non-mail path.

    internal suspend fun sealedSpamWriteAvailable(): Boolean = false

    @Suppress("UNUSED_PARAMETER")
    internal suspend fun trainSpamModelClient(text: String, isSpam: Boolean): Boolean = false

    @Suppress("UNUSED_PARAMETER")
    internal suspend fun trainSpamModelClientMail(
        text: String,
        isSpam: Boolean,
        messageId: ByteArray,
        mailbox: String,
        subject: String,
    ) = Unit

    private fun excised(plane: String) =
        ApiException("the $plane plane is not part of Fauna Kids")
}
