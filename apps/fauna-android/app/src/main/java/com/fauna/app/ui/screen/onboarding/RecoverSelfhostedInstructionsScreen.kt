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
import com.fauna.app.core.AccountStores
import com.fauna.app.core.HexUtil
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.ShellLog
import com.fauna.app.ui.components.CopyButton
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.withContext
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Self-hosted install instructions for total-box-loss recovery
 * (docs/goal/architecture/nest/box-recovery.md § Recovery UI (step 4), Task E).
 * On entry the VM resolves the selected box's installer line through the shared
 * `recoverSelfhostedCommandPreLogin` FFI: this device's own store joined with a
 * cold read over the nest when it answers, never either-or (a dead nest still
 * resolves from the store). Until it resolves — or when it
 * cannot — the page shows the pending string with the copy button disabled, so
 * an unresolved command never reaches the clipboard (a command carrying the
 * WRONG box's seed would rebuild the box under a different nest_actor_id — the
 * exact trust break recovery exists to prevent).
 */
@HiltViewModel
class RecoverSelfhostedInstructionsVM @Inject constructor(
    val host: OnboardingHost,
    private val accountStores: AccountStores,
) : ViewModel() {
    private val _command = MutableStateFlow<String?>(null)

    /** The resolved `FAUNA_DEPLOYMENT_SEED=…` line, or null while unresolved. */
    val command: StateFlow<String?> = _command

    /**
     * The shared pre-login FFI call (`recoverSelfhostedCommandPreLogin`); replaced
     * in tests. The device's own store is always read; a cold read over `nestUrl`
     * joins it only when a URL is given and the nest answers, so a dead nest still
     * resolves from the store. Null when no custodied seed matches the box.
     */
    internal var fetchCommand: suspend (String?, ByteArray, String, String) -> String? =
        { nestUrl, ownerSecret, nestActorId, storeContainerDir ->
            com.fauna.ffi.recoverSelfhostedCommandPreLogin(ownerSecret, nestActorId, nestUrl, storeContainerDir)
        }

    /** Resolve the selected box's command; a miss leaves it null (pending, copy disabled). */
    suspend fun resolveCommand() {
        val secretHex = host.machine.effectiveSecret() ?: return
        val nestActorId = host.machine.recoverySelectedNestId() ?: return
        val nestUrl = host.machine.nestUrl().takeIf { it.isNotBlank() }
        _command.value = withContext(Dispatchers.IO) {
            try {
                fetchCommand(nestUrl, HexUtil.hexToBytes(secretHex), nestActorId, accountStores.accountStoreContainerDir())
            } catch (e: Exception) {
                ShellLog.w("RecoverSelfhostedVM", "self-hosted command resolve failed: ${e.message}")
                null
            }
        }
    }

    /** Both the restore CTA and continue exit the wizard entirely, mirroring linux's `m.reset()`. */
    fun reset() = host.machine.reset()
}

@Composable
fun RecoverSelfhostedInstructionsScreen(
    navController: NavController,
    vm: RecoverSelfhostedInstructionsVM = hiltViewModel(),
) {
    val resolved by vm.command.collectAsState()
    LaunchedEffect(Unit) { vm.resolveCommand() }
    val pendingCommand = stringResource(R.string.onboarding_recovery_selfhosted_command_pending)
    val shownCommand = resolved ?: pendingCommand

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
            shownCommand,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_SELFHOSTED_COMMAND),
            style = MaterialTheme.typography.bodyMedium,
        )

        Spacer(Modifier.height(8.dp))

        // Disabled while the command is unresolved — never copy a placeholder
        // (or wrong-box) command to the clipboard.
        CopyButton(
            testTag = Ids.RECOVER_SELFHOSTED_COPY_BUTTON,
            text = shownCommand,
            enabled = resolved != null,
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
