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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.ui.util.faunaGate
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
 * `fauna.payments.claims.redeem` offline / Dim-3 courtesy gate and its reason.
 * The pair also carries the family compile condition itself, so a store-safe
 * APK paints neither id whatever the caller does (`dynamic-features.md`
 * § Platform-family surface excision).
 */
@Composable
fun ClaimRedeemPair(
    code: String,
    onCodeChange: (String) -> Unit,
    working: Boolean,
    onRedeem: () -> Unit,
) {
    if (!BuildConfig.PAYMENTS) return
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            stringResource(R.string.subscriptions_redeem_claim_title),
            style = MaterialTheme.typography.titleMedium,
        )
        val redeemGate = faunaGate(
            "fauna.payments.claims.redeem",
            enabled = !working,
        )
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
                onClick = onRedeem,
                enabled = redeemGate.enabled,
                modifier = Modifier.testTag(Ids.SUBSCRIPTION_CLAIM_REDEEM_BUTTON),
            ) { Text(stringResource(R.string.subscriptions_redeem)) }
        }
        DisabledControlReasonText(redeemGate.reason)
    }
}
