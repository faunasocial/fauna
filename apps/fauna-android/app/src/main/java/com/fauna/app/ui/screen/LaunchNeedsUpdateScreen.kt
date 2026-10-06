package com.fauna.app.ui.screen

import androidx.compose.foundation.layout.*
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
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
 * Launch-screen NON-retry "update required" surface, shown when the nest
 * authoritatively reports it is running an outdated version
 * (`fauna.nest.outdated` → the launch machine's `Offline { transient: false }`,
 * carrying the localized message in `last_error`).
 *
 * Distinct from [LaunchRetryScreen]: retrying the same outdated nest is futile,
 * so there is NO Retry CTA — only "Use a different nest". The localized message
 * renders in the canonical `error-message` element (matching web/linux), not a
 * static string. version-compatibility.md Dim 4 / docs/goal/behavior/onboarding.md
 * § App-launch routing (version-mismatch row).
 */
@Composable
fun LaunchNeedsUpdateScreen(
    message: String,
    onUseDifferentNest: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            message,
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
        )
        Spacer(Modifier.height(32.dp))
        OutlinedButton(
            onClick = onUseDifferentNest,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_FALLTHROUGH_BUTTON),
        ) { Text(stringResource(R.string.launch_use_different_nest)) }
    }
}
