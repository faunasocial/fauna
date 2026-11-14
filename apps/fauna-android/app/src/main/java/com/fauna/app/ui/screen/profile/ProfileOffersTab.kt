package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.components.ClaimRedeemPair
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.ProfileOffersVM
import com.fauna.ffi.FfiTierItem
import uniffi.fauna_core.OfferStatus
import uniffi.fauna_core.isSafePaymentUrl
import social.fauna.generated.Ids

/**
 * The profile Tiers-tab **OTHER** (subscriber-browse) section — subscriptions
 * Slice B item 7 (`monetization.md` § Pillar 1; `profile.md` § Layout & flow).
 * When viewing another actor's profile, shows the creator's offered tiers
 * (`subscription-offers-section` / `subscription-offer-list`): per-row name /
 * price / description / external payment link / Subscribe + the viewer's status
 * (none / pending / active). Lifts the linux lead
 * (`apps/fauna-linux/src/views/profile/offers.rs`) onto Compose (priority #1
 * uniform; same ui.yaml IDs). The free "followers" tier is excluded (followed via
 * the header `profile-follow-button`).
 *
 * FFI-free [ProfileOffersContent] is split out for the Robolectric harness; the
 * VM-bound [ProfileOffersSection] is what [ProfileScreen] mounts for an OTHER
 * profile.
 */
@Composable
fun ProfileOffersSection(
    targetActorIdHex: String,
    /** The page's actor-home slot (`ProfileVM.actorHomeSlot`; `null` = this
     * nest): where the offers are read and where a per-row Subscribe and its
     * payment link subscribe (the home the header follow passes). The header
     * read may replace it after the first load; the effect below re-reads then. */
    home: String? = null,
    /** Bumped by every `profile-tiers-tab` activation — see [reloadToken] in the
     * `LaunchedEffect` key below. */
    reloadToken: Int = 0,
    vm: ProfileOffersVM = hiltViewModel(),
) {
    val offers by vm.offers.collectAsState()
    val statusTier by vm.statusTier.collectAsState()
    val pendingTiers by vm.pendingTiers.collectAsState()
    val working by vm.working.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val redeemInput by vm.redeemInput.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }
    // Keyed on the reload token as well as the actor: every activation of
    // `profile-tiers-tab` re-reads, the ruled uniform door (`monetization.md`
    // § Pillar 1 → *The Tiers-tab re-read door*, lifting tui's `Action::ShowTiers`
    // shape). Keying on the actor alone re-read only when this branch re-composed
    // — i.e. after a detour through Posts — so a re-click while already on Tiers,
    // which is exactly how a subscriber asks "did the author approve me yet?",
    // read nothing. There is no push kind for a subscribe grant to answer it.
    // Keyed on the home slot too: the header read replacing it re-reads the
    // offers from the new home (`subscriptions.md` § Pillar 1 → *The offers
    // browse across nests*), tui's `offers_reload_op`.
    LaunchedEffect(targetActorIdHex, home, reloadToken) { vm.load(targetActorIdHex, home) }

    ProfileOffersContent(
        offers = offers,
        // Per-tier viewer status + badge label from the shared `offer_status` /
        // `offer_status_label` (precedence Active>Pending>None single-sourced); resolved
        // by this stateful caller and injected so the Content stays FFI-free for Robolectric.
        statusFor = { tierName -> com.fauna.ffi.offerStatus(tierName, statusTier, tierName in pendingTiers) },
        statusLabel = { status -> resolveLocalized(context, com.fauna.ffi.offerStatusLabel(status)).orEmpty() },
        working = working,
        onSubscribe = { tier -> vm.subscribe(targetActorIdHex, tier, home) },
        redeemCode = redeemInput,
        onRedeemCodeChange = vm::setRedeemInput,
        // The follow's home, as the Subscribe above carries it.
        onRedeem = { vm.redeemClaim(targetActorIdHex, home) },
        // The payment link: the shared https-only guard refuses an unsafe link up
        // front (F-CL2 anti-phishing-redirect; the error shows, nothing is sent),
        // else the subscribe goes first and the composed checkout URL opens
        // through the app's one opener (`monetization.md` § Pillar 3 →
        // *Buyer↔actor binding (Q4)*).
        onPaymentLink = { tier, url ->
            if (isSafePaymentUrl(url)) {
                vm.payTo(targetActorIdHex, tier, url, home) { checkout ->
                    UrlOpener.open(context, checkout)
                }
            } else {
                appMessages.showError(context.getString(R.string.subscriptions_unsafe_payment_url))
            }
        },
    )
}

@Composable
fun ProfileOffersContent(
    offers: List<FfiTierItem>,
    statusFor: (String) -> OfferStatus,
    statusLabel: (OfferStatus) -> String,
    working: Boolean,
    onSubscribe: (String) -> Unit,
    redeemCode: String = "",
    onRedeemCodeChange: (String) -> Unit = {},
    onRedeem: () -> Unit = {},
    /** `subscription-offer-payment-link` pressed on the row for (tier, payment URL). */
    onPaymentLink: (String, String) -> Unit = { _, _ -> },
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp)
            .testTag(Ids.SUBSCRIPTION_OFFERS_SECTION),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.subscriptions_offers),
            style = MaterialTheme.typography.titleMedium,
        )
        Column(
            modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_OFFER_LIST),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (offers.isEmpty()) {
                Text(
                    stringResource(R.string.subscriptions_no_offers),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                offers.forEach { tier ->
                    OfferRow(
                        tier = tier,
                        status = statusFor(tier.name),
                        statusLabel = statusLabel,
                        working = working,
                        onSubscribe = { onSubscribe(tier.name) },
                        onPaymentLink = { url -> onPaymentLink(tier.name, url) },
                    )
                }
            }
        }
        // The redeem pair (`profile.md` § Layout & flow → *Another's profile*):
        // the `subscription-settings` pair, here carrying this creator's home, so
        // a code minted on another nest redeems from the creator's profile. Paints
        // with no offer rows too. Same family compile condition and the same
        // `fauna.payments.claims.redeem` gate as the settings pair (one shared
        // composable); lifts tui's OTHER Tiers tab.
        if (BuildConfig.PAYMENTS) {
            ClaimRedeemPair(
                code = redeemCode,
                onCodeChange = onRedeemCodeChange,
                working = working,
                onRedeem = onRedeem,
            )
        }
    }
}

/** One `subscription-offer-row` — name / price / description + external payment
 * link + the viewer's status + Subscribe. The viewer's per-tier [OfferStatus] and its
 * badge label come from the shared `offer_status` / `offer_status_label` (resolved by
 * the stateful section), branched on here for the badge and the Subscribe-disable. */
@Composable
private fun OfferRow(
    tier: FfiTierItem,
    status: OfferStatus,
    statusLabel: (OfferStatus) -> String,
    working: Boolean,
    onSubscribe: () -> Unit,
    onPaymentLink: (String) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_OFFER_ROW)) {
        Column(
            modifier = Modifier.padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            Text(
                tier.name,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_OFFER_NAME),
            )
            if (BuildConfig.PAYMENTS) {
                tier.priceHint?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.SUBSCRIPTION_OFFER_PRICE),
                    )
                }
            }
            tier.description?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_OFFER_DESCRIPTION),
                )
            }
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    statusLabel(status),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_OFFER_STATUS),
                )
                if (BuildConfig.PAYMENTS) {
                    tier.paymentUrl?.let { url ->
                        // The payment link subscribes before it opens the
                        // checkout, so it declares the subscribe's kind too
                        // (linux `offers.rs` declares the same on its button).
                        val payGate = faunaGate("fauna.subscriptions.subscribe", enabled = !working)
                        Column {
                            OutlinedButton(
                                onClick = { onPaymentLink(url) },
                                enabled = payGate.enabled,
                                modifier = Modifier.testTag(Ids.SUBSCRIPTION_OFFER_PAYMENT_LINK),
                            ) { Text(stringResource(R.string.subscriptions_payment_url)) }
                            DisabledControlReasonText(payGate.reason)
                        }
                    }
                }
                // `fauna.subscriptions.subscribe` is OnlineOnly. The page's own
                // predicate (not already active, nothing in flight) is handed over.
                val subscribeGate = faunaGate(
                    "fauna.subscriptions.subscribe",
                    enabled = !working && status != OfferStatus.ACTIVE,
                )
                Column {
                    Button(
                        onClick = onSubscribe,
                        enabled = subscribeGate.enabled,
                        modifier = Modifier.testTag(Ids.SUBSCRIPTION_OFFER_SUBSCRIBE_BUTTON),
                    ) { Text(stringResource(R.string.subscriptions_subscribe)) }
                    DisabledControlReasonText(subscribeGate.reason)
                }
            }
        }
    }
}
