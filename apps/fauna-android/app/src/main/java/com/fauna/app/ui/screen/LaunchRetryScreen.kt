package com.fauna.app.ui.screen

import androidx.compose.foundation.layout.*
import androidx.compose.material3.Button
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
 * Launch-screen retry surface per docs/goal/behavior/onboarding.md
 * §"App-launch routing": shown when the silent-challenge fast path
 * couldn't reach the saved nest (IOException, HTTP 5xx, DNS failure,
 * etc.). Out of the seven-page wizard count — rendered by the launch
 * flow before the wizard mounts.
 *
 * Element IDs match the cross-app convention established by Web
 * and adopted by Windows — `launch-transient-error`,
 * `launch-retry-button`, `launch-fallthrough-button`, plus the optional
 * `launch-recover-button` (surviving-device recovery entry, shown only when
 * the device knows of ≥1 custodied box; per the new
 * `launch_retry` page block in `tests/e2e-unified/ui.yaml`).
 */
@Composable
fun LaunchRetryScreen(
    onRetry: () -> Unit,
    onUseDifferentNest: () -> Unit,
    /** The custodied boxes the device knows of; `launch-recover-button` shows iff non-empty. */
    recoverableBoxes: List<String> = emptyList(),
    onRecover: () -> Unit = {},
    /** `launch-retire-button`: always shown — it needs only a cloud token, no
     *  custodied seed (`nest-retirement.md` § Layout & flow). */
    onRetire: () -> Unit = {},
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.launch_retry_title),
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.LAUNCH_TRANSIENT_ERROR),
        )
        Spacer(Modifier.height(32.dp))
        Button(
            onClick = onRetry,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_RETRY_BUTTON),
        ) { Text(stringResource(R.string.launch_retry_button)) }
        if (recoverableBoxes.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            OutlinedButton(
                onClick = onRecover,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.LAUNCH_RECOVER_BUTTON),
            ) { Text(stringResource(R.string.onboarding_launch_recover_lost_box)) }
        }
        Spacer(Modifier.height(8.dp))
        OutlinedButton(
            onClick = onRetire,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_RETIRE_BUTTON),
        ) { Text(stringResource(R.string.onboarding_launch_retire_server)) }
        Spacer(Modifier.height(8.dp))
        OutlinedButton(
            onClick = onUseDifferentNest,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_FALLTHROUGH_BUTTON),
        ) { Text(stringResource(R.string.launch_use_different_nest)) }
    }
}
