package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.faunaFormGate
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.formRefusal
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.isShown
import com.fauna.app.ui.util.localizedNested
import com.fauna.ffi.subscriptionRedeemCodePossibility
import uniffi.fauna_core.Possibility
import social.fauna.generated.Ids

/**
 * The claim-code redeem pair — `subscription-claim-redeem-input` +
 * `subscription-claim-redeem-button` under the "Redeem a claim code" title
 * (`monetization.md` § Pillar 3 → *Which nest an in-app redeem binds on* →
 * *The doors*). One composable for both doors: the `subscription-settings`
 * page (home-less, the code redeems on this nest) and the OTHER profile Tiers
 * tab (the code redeems at the viewed creator's home). Stateless: the caller
 * owns the buffer and the redeem, so each door keeps its own lifetime rule.
 *
 * The commit gates, not the buffer: the input stays live so a code can be
 * pasted with no nest, while the button carries the
 * `fauna.payments.claims.redeem` offline gate and the [answer] — the
 * `payments` plane's Dim-3 courtesy answer the caller's VM holds
 * (`featuresGatePossibility`; blocked still loading before the features read
 * answers). An `Absent` answer — a nest without the payments plane — leaves
 * the whole block out, title included (`docs/goal/ui/README.md` § Offer only
 * what is possible, rules 2 and 5).
 * The pair also carries the family compile condition itself, so a store-safe
 * APK paints neither id whatever the caller does (`dynamic-features.md`
 * § Platform-family surface excision).
 *
 * The draft half is the shared `redeem_code_possibility` (profile.md § Errors &
 * edge cases; README § Offer only what is possible, rule 4), folded with that
 * payments gate: a blank code disables the button with the shared reason beside
 * it, and a press asks the same predicate first and refuses with that reason
 * through [onRefuse] — the caller's page `error-message` — never a silent no-op.
 */
@Composable
fun ClaimRedeemPair(
    code: String,
    onCodeChange: (String) -> Unit,
    working: Boolean,
    onRedeem: () -> Unit,
    answer: Possibility,
    onRefuse: (String) -> Unit = {},
) {
    if (!BuildConfig.PAYMENTS) return
    if (!answer.isShown) return
    val context = LocalContext.current
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            stringResource(R.string.subscriptions_redeem_claim_title),
            style = MaterialTheme.typography.titleMedium,
        )
        val codeDraft = askForm { subscriptionRedeemCodePossibility(code) }
        val redeemGate = faunaGate(
            "fauna.payments.claims.redeem",
            enabled = answer.isPossible && !working && codeDraft.isPossible,
        )
        val redeemSubmit = faunaFormGate(codeDraft, redeemGate)
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = code,
                onValueChange = onCodeChange,
                singleLine = true,
                label = { Text(stringResource(R.string.subscriptions_claim_code)) },
                modifier = Modifier.weight(1f).testTag(Ids.SUBSCRIPTION_CLAIM_REDEEM_INPUT),
            )
            Button(
                onClick = {
                    formRefusal(context, askForm { subscriptionRedeemCodePossibility(code) })?.let { refusal ->
                        refusal.reason?.let(onRefuse)
                        return@Button
                    }
                    onRedeem()
                },
                enabled = redeemSubmit.enabled,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_CLAIM_REDEEM_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_redeem)) }
        }
        // The plane's reason is the more specific one and wins (nested: a
        // restriction carries its `{window}` word as a key); else the draft's.
        DisabledControlReasonText(localizedNested(answer.blockedReason) ?: redeemSubmit.reason)
    }
}
