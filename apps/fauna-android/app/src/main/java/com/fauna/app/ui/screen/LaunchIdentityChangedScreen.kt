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
 * Launch-screen `launch_identity_changed` surface: the nest's pinned
 * deployment identity changed, or a pinned nest can no longer prove any
 * identity (security.md § Transport trust — the SSH
 * `known_hosts` model). Auto-entry is BLOCKED; NO Retry CTA (a retry
 * cannot change the verdict and must never silently re-pin) — only
 * "trust this nest" or "use a different nest". Same uniform surface
 * web/linux/tui render (ui.yaml widened to all apps, user-approved
 * rule A, 2026-07-13).
 */
@Composable
fun LaunchIdentityChangedScreen(
    onTrust: () -> Unit,
    onUseDifferentNest: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_launch_identity_changed_warning),
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.NEST_IDENTITY_CHANGED_WARNING),
        )
        Spacer(Modifier.height(32.dp))
        Button(
            onClick = onTrust,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.NEST_IDENTITY_CHANGED_TRUST_BUTTON),
        ) { Text(stringResource(R.string.onboarding_launch_identity_changed_trust)) }
        Spacer(Modifier.height(12.dp))
        OutlinedButton(
            onClick = onUseDifferentNest,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_FALLTHROUGH_BUTTON),
        ) { Text(stringResource(R.string.launch_use_different_nest)) }
    }
}
