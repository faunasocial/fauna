package com.fauna.app.core

import com.fauna.ffi.FfiSubscribeReply

/**
 * What the payment-link gesture's subscribe-then-checkout hands the shell —
 * the app-local mirror of `fauna-ffi`'s `FfiCheckoutOpen` (and of
 * `fauna_client_subscriptions::CheckoutOpen`): the subscribe's own [reply]
 * (the row's status reads it, as the Subscribe button's reply) and the
 * checkout [url] the shared rule composed, to open through [UrlOpener]
 * (`monetization.md` § Pillar 3 → *Buyer↔actor binding (Q4)*).
 *
 * App-local because `FfiCheckoutOpen` is a `subscriptions-author` export the
 * kids flavor does not carry, while `src/main`'s [com.fauna.app.ui.viewmodel.ProfileOffersVM]
 * names the result ([KidsExcisedApi.subscriptionSubscribeAndCheckout] maps it).
 */
data class SubscribeCheckout(
    val reply: FfiSubscribeReply,
    val url: String,
)
