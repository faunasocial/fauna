package com.fauna.app.ui.screen

import androidx.compose.foundation.layout.*
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import social.fauna.generated.Ids
import uniffi.fauna_launch_machine.AccountIndexRefusal

/**
 * Launch-screen `launch_account_index_unreadable` surface: the saved account
 * index is present and this build cannot use it
 * (`version-compatibility.md` § 5 item 9). Never a retry or fallthrough — the
 * nest was never contacted. The `NewerBuild` verdict offers nothing else; the
 * `Malformed` verdict reveals a start-over confirm on a first press (a
 * purely local reveal — `onReset` is called only from the confirm, never the
 * reveal). Twin of `apps/fauna-tui/src/launch.rs`'s
 * `LaunchSurface::AccountIndexUnreadable`.
 */
@Composable
fun LaunchAccountIndexUnreadableScreen(
    refusal: AccountIndexRefusal,
    onReset: () -> Unit,
) {
    var confirming by remember(refusal) { mutableStateOf(false) }
    val malformed = refusal is AccountIndexRefusal.Malformed

    val message = when {
        !malformed -> stringResource(R.string.onboarding_launch_index_newer_build)
        confirming -> stringResource(R.string.onboarding_launch_index_malformed_reset_residual)
        else -> stringResource(R.string.onboarding_launch_index_malformed)
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            message,
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.ACCOUNT_INDEX_REFUSAL_WARNING),
        )
        if (malformed) {
            Spacer(Modifier.height(32.dp))
            if (confirming) {
                Button(
                    onClick = onReset,
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(Ids.ACCOUNT_INDEX_RESET_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.onboarding_launch_index_malformed_reset_confirm)) }
            } else {
                Button(
                    onClick = { confirming = true },
                    modifier = Modifier
                        .fillMaxWidth()
                        .testTag(Ids.ACCOUNT_INDEX_RESET_BUTTON),
                ) { Text(stringResource(R.string.onboarding_launch_index_malformed_reset)) }
            }
        }
    }
}
