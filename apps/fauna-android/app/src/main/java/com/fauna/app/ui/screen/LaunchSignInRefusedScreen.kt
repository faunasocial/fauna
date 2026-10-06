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
 * Launch-screen `launch_sign_in_refused` surface: the saved nest answered the
 * silent challenge with a refusal (`LaunchSnapshot.signInRefused`) — the
 * account was suspended or removed, which the app cannot tell apart and the
 * copy deliberately leaves unsaid. The one terminal launch surface WITH
 * Retry (the admin's restore is a button on their app, and a retry is the
 * user's way back in), plus "Use a different nest". Twin of
 * `apps/fauna-tui/src/launch.rs`'s page; docs/goal/behavior/onboarding.md
 * § App-launch routing (previously-signed-in row).
 */
@Composable
fun LaunchSignInRefusedScreen(
    onRetry: () -> Unit,
    onUseDifferentNest: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_launch_sign_in_refused_title),
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(16.dp))
        Text(
            stringResource(R.string.onboarding_launch_sign_in_refused),
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.LAUNCH_SIGN_IN_REFUSED_NOTICE),
        )
        Spacer(Modifier.height(32.dp))
        Button(
            onClick = onRetry,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_RETRY_BUTTON),
        ) { Text(stringResource(R.string.launch_retry_button)) }
        Spacer(Modifier.height(8.dp))
        OutlinedButton(
            onClick = onUseDifferentNest,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_FALLTHROUGH_BUTTON),
        ) { Text(stringResource(R.string.launch_use_different_nest)) }
    }
}
