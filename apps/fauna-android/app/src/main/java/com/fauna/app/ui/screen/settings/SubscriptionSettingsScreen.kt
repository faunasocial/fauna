package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.SubscriptionSettingsVM
import com.fauna.ffi.FfiMineSubscription
import social.fauna.generated.Ids

/**
 * The consumer-side **`subscription-settings`** page (subscriptions Slice B —
 * `monetization.md` § Pillar 1). Reached from the Settings list as
 * "Subscriptions" (sibling of mail-settings / web-settings); shows **this user's
 * own subscriptions across every creator** with per-row unsubscribe. Distinct
 * from the profile Tiers-tab SELF author management (`ProfileTiersTab.kt`): that
 * is what the user *offers*, this is what the user *consumes*.
 *
 * All data comes from one shared-Rust read — `SubscriptionsClient.mine_list` — so
 * the shell holds no logic (priority #2); lifts the linux lead
 * (`apps/fauna-linux/src/settings/subscriptions.rs`), same ui.yaml IDs
 * (priority #1). Observer-free: a re-read on mount + after unsubscribe.
 *
 * FFI-free [SubscriptionSettingsContent] is split out for the Robolectric
 * harness; the VM-bound [SubscriptionSettingsScreen] is what the NavHost mounts.
 * `page-heading` is the TopAppBar title; `error-message` is the global
 * MessageBanner (the same idiom WebSettings / the profile Tiers tab use).
 */
@Composable
fun SubscriptionSettingsScreen(
    navController: NavController,
    vm: SubscriptionSettingsVM = hiltViewModel(),
) {
    val subscriptions by vm.subscriptions.collectAsState()
    val working by vm.working.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val claimRedeemed by vm.claimRedeemed.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }
    LaunchedEffect(Unit) { vm.refresh() }

    // The creator's label is the shared projection's: the viewer's nickname for
    // them, else the handle, else the hex actor id.
    val overlaysVm: com.fauna.app.ui.viewmodel.ContactOverlaysVM = hiltViewModel()
    val overlayEpoch by overlaysVm.epoch.collectAsState()
    val authorLabel: (FfiMineSubscription) -> String = remember(overlayEpoch) {
        val overlays = overlaysVm.overlays()
        return@remember { sub -> overlays.subscriptionAuthorLabel(sub.handle, sub.authorId) }
    }

    SubscriptionSettingsContent(
        subscriptions = subscriptions,
        authorLabel = authorLabel,
        working = working,
        claimRedeemed = claimRedeemed,
        onClaimRedeemedConsumed = { vm.claimRedeemed.value = false },
        onBack = { navController.popBackStack() },
        onUnsubscribe = vm::unsubscribe,
        onRedeemClaim = vm::redeemClaim,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SubscriptionSettingsContent(
    subscriptions: List<FfiMineSubscription>,
    working: Boolean,
    claimRedeemed: Boolean = false,
    onClaimRedeemedConsumed: () -> Unit = {},
    onBack: () -> Unit,
    onUnsubscribe: (ByteArray) -> Unit,
    onRedeemClaim: (String) -> Unit = {},
    // The `subscription-mine-author` text; the stateful caller resolves it
    // through the overlay projection, so this Content stays FFI-free.
    authorLabel: (FfiMineSubscription) -> String = { it.authorDisplay },
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.subscriptions_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .testTag(Ids.SUBSCRIPTION_MINE_SECTION),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.subscriptions_my_subscriptions),
                style = MaterialTheme.typography.titleMedium,
            )
            if (subscriptions.isEmpty()) {
                Text(
                    stringResource(R.string.subscriptions_no_subscriptions),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                subscriptions.forEach { sub ->
                    MineRow(
                        sub = sub,
                        authorText = authorLabel(sub),
                        working = working,
                        onUnsubscribe = onUnsubscribe,
                    )
                }
            }

            // Claim redemption (monetization.md § Pillar 3 Q4 — the universal
            // fallback binding): paste a post-payment claim code → the queued
            // entitlement renders as a "pending" row above. Lifts the linux
            // lead; same reserved ui.yaml IDs.
            //
            // The buy-side half of the `payments` registry feature, so it
            // carries android's family compile condition (`dynamic-features.md`
            // § Platform-family surface excision) — `false` in the `storeSafe`
            // build type, where R8 folds the branch and strips both
            // `subscription-claim-redeem-*` ids out of the artifact. The
            // subscription rows above are NOT gated: Pillar 1 entitlements are
            // not a registry member, and an excised build still shows what the
            // reader already has.
            if (BuildConfig.PAYMENTS) {
                var claimCode by remember { mutableStateOf("") }
                if (claimRedeemed) {
                    claimCode = ""
                    onClaimRedeemedConsumed()
                }
                Text(
                    stringResource(R.string.subscriptions_redeem_claim_title),
                    style = MaterialTheme.typography.titleMedium,
                )
                // The commit gates, not the buffer: the claim-code input beside
                // it stays live so a code can be pasted with no nest. Hoisted
                // above the Row so the reason can render under both.
                val redeemGate = faunaGate(
                    "fauna.payments.claims.redeem",
                    enabled = !working,
                )
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    OutlinedTextField(
                        value = claimCode,
                        onValueChange = { claimCode = it },
                        singleLine = true,
                        label = { Text(stringResource(R.string.subscriptions_claim_code)) },
                        modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_CLAIM_REDEEM_INPUT),
                    )
                    Button(
                        onClick = { onRedeemClaim(claimCode) },
                        enabled = redeemGate.enabled,
                        modifier = Modifier.testTag(Ids.SUBSCRIPTION_CLAIM_REDEEM_BUTTON),
                    ) { Text(stringResource(R.string.subscriptions_redeem)) }
                }
                DisabledControlReasonText(redeemGate.reason)
            }
        }
    }
}

/**
 * One `subscription-mine-row` — creator / tier / status + unsubscribe.
 * [authorText] is the shared resolver's answer: the viewer's nickname for the
 * creator, else the handle, else the full hex actor id
 * (`value-formatting.md` § Subscription author label).
 */
@Composable
private fun MineRow(
    sub: FfiMineSubscription,
    authorText: String,
    working: Boolean,
    onUnsubscribe: (ByteArray) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SUBSCRIPTION_MINE_ROW)) {
        Row(
            modifier = Modifier.padding(12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                authorText,
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_MINE_AUTHOR),
            )
            Text(
                sub.tier,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_MINE_TIER),
            )
            // The raw wire status ("active" | "pending"), rendered verbatim
            // (uniform with §2's subscription-request-kind).
            Text(
                sub.status,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_MINE_STATUS),
            )
            // `fauna.subscriptions.unsubscribe` is OnlineOnly; the row's author,
            // tier and status beside it are pure reads and stay rendered.
            val unsubscribeGate = faunaGate(
                "fauna.subscriptions.unsubscribe",
                enabled = !working,
            )
            Column {
                OutlinedButton(
                    onClick = { onUnsubscribe(sub.authorId) },
                    enabled = unsubscribeGate.enabled,
                    modifier = Modifier.testTag(Ids.SUBSCRIPTION_MINE_UNSUBSCRIBE_BUTTON),
                ) { Text(stringResource(R.string.subscriptions_unsubscribe)) }
                DisabledControlReasonText(unsubscribeGate.reason)
            }
        }
    }
}
