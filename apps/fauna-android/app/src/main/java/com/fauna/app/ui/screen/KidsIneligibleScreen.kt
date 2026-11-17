package com.fauna.app.ui.screen

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import social.fauna.generated.Ids

/**
 * `kids_ineligible` — the Fauna Kids build's refusal for an account that is
 * not supervised (`account-age-band.md` § The account age band, the kids-app
 * bullet, item (3)). Replaces the whole signed-in shell whenever the shared
 * three-way `kidsAppEligibility` answer is not ELIGIBLE; the verdict keys on
 * supervision, never on the band. One explanation and a sign-out — nothing
 * else is reachable.
 *
 * [showMessage] is true for INELIGIBLE. While the answer is UNKNOWN
 * (`account-age-band.md` § *Kids-app eligibility while it is unknown*) the
 * page shows its frame WITHOUT `kids-ineligible-message` — the refusal is not
 * shown on a guess — and `sign-out-button` stays live; [error] then carries
 * the could-not-reach line once a read has failed.
 *
 * Kids-only: the only caller sits behind `BuildConfig.KIDS` in `FaunaNavHost`,
 * so the release shrinker drops the screen from the other four build types.
 */
@Composable
fun KidsIneligibleScreen(
    onSignOut: () -> Unit,
    showMessage: Boolean = true,
    error: String? = null,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        if (showMessage) {
            Text(
                stringResource(R.string.family_kids_ineligible_message),
                style = MaterialTheme.typography.bodyLarge,
                textAlign = TextAlign.Center,
                modifier = Modifier.testTag(Ids.KIDS_INELIGIBLE_MESSAGE),
            )
        }
        if (error != null) {
            Spacer(Modifier.height(16.dp))
            Text(
                error,
                color = MaterialTheme.colorScheme.error,
                textAlign = TextAlign.Center,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
            )
        }
        Spacer(Modifier.height(32.dp))
        Button(
            onClick = onSignOut,
            modifier = Modifier.fillMaxWidth().testTag(Ids.SIGN_OUT_BUTTON),
        ) { Text(stringResource(R.string.settings_sign_out)) }
    }
}
