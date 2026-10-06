package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.components.CopyButton
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Self-hosted install instructions for total-box-loss recovery
 * (docs/goal/architecture/nest/box-recovery.md § Recovery UI (step 4), Task E).
 * The resolved installer command (a reachable-nest fetch mirroring linux/tui's
 * C2 leg) is out of scope here — this screen renders the permanent pending
 * placeholder, exactly what linux/tui show before their fetch resolves, with
 * the copy button disabled so an unresolved command never reaches the
 * clipboard (a command carrying the WRONG box's seed would rebuild the box
 * under a different nest_actor_id — the exact trust break recovery exists to
 * prevent).
 */
@HiltViewModel
class RecoverSelfhostedInstructionsVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    /** Both the restore CTA and continue exit the wizard entirely, mirroring linux's `m.reset()`. */
    fun reset() = host.machine.reset()
}

@Composable
fun RecoverSelfhostedInstructionsScreen(
    navController: NavController,
    vm: RecoverSelfhostedInstructionsVM = hiltViewModel(),
) {
    val pendingCommand = stringResource(R.string.onboarding_recovery_selfhosted_command_pending)

    fun exitToIdentityChoice() {
        vm.reset()
        navController.navigate("onboarding/identity-choice")
    }

    Column(modifier = Modifier.fillMaxSize().padding(24.dp)) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_recovery_selfhosted_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_recovery_selfhosted_desc),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        Text(
            pendingCommand,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_SELFHOSTED_COMMAND),
            style = MaterialTheme.typography.bodyMedium,
        )

        Spacer(Modifier.height(8.dp))

        // Disabled while the command is a placeholder — never copy an unresolved
        // (or wrong-box) command to the clipboard.
        CopyButton(
            testTag = Ids.RECOVER_SELFHOSTED_COPY_BUTTON,
            text = pendingCommand,
            enabled = false,
            label = stringResource(R.string.common_copy),
        )

        Spacer(Modifier.weight(1f))

        OutlinedButton(
            onClick = { exitToIdentityChoice() },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_RESTORE_CTA),
        ) { Text(stringResource(R.string.onboarding_recovery_restore_cta)) }

        Spacer(Modifier.height(8.dp))

        Button(
            onClick = { exitToIdentityChoice() },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_SELFHOSTED_CONTINUE_BUTTON),
        ) { Text(stringResource(R.string.onboarding_recovery_selfhosted_continue)) }
    }
}
