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
 * Fork variant of the `launch_identity_changed` surface: the nest's rotation
 * chain shows two competing heads (`box-recovery.md` § Client acceptance —
 * `LaunchSnapshot.identityFork`). The warning names both heads and there is
 * NO trust button and NO Retry — the machine refuses a re-trust on a fork, and
 * an affordance that refuses is a lie. The wizard fallthrough is the only way
 * out. Same shape as tui's `LaunchSurface::IdentityForked`.
 */
@Composable
fun LaunchIdentityForkedScreen(
    pinnedHex: String,
    seenHex: String,
    onUseDifferentNest: () -> Unit,
) {
    val template = stringResource(R.string.onboarding_launch_identity_fork_warning)
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            template.replace("{pinned}", pinnedHex).replace("{seen}", seenHex),
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.NEST_IDENTITY_FORK_WARNING),
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
