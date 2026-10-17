package com.fauna.app.ui.screen.onboarding

import android.content.Context
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.formatUnixLocal
import com.fauna.ffi.sessionsLockoutConfirmWord
import com.fauna.ffi.onboarding.EmergencyLockoutOutcome
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.emergencyLockoutOutcomeMessage
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import kotlinx.coroutines.launch
import social.fauna.generated.Ids

@HiltViewModel
class EmergencyLockoutEntryVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    var submitting by mutableStateOf(false)
        private set

    /** The ceremony call — a seam only so the host tests need no live nest. */
    internal var submitLockout: suspend (String, String) -> EmergencyLockoutOutcome =
        { secret, word -> host.machine.submitEmergencyLockout(secret, word) }

    /**
     * `sessions-lockout-button`: the seed-signed lock over one anonymous
     * connection. The typed account rides on the machine's one account field,
     * ALWAYS (empty included), exactly as `recovery_entry` forwards it.
     * Persists nothing — the three inputs live in the composable only.
     */
    suspend fun submit(secret: String, account: String, word: String, context: Context) {
        host.machine.setCurrentHandle(account.trim())
        settle(submitLockout(secret, word), context)
    }

    fun launchSubmit(
        secret: String,
        account: String,
        word: String,
        context: Context,
        then: () -> Unit,
    ) {
        submitting = true
        viewModelScope.launch {
            try {
                submit(secret, account, word, context)
            } finally {
                submitting = false
            }
            then()
        }
    }

    /**
     * Say the shared table's sentence for the outcome on `error-message` — for
     * a refusal on this step, for [EmergencyLockoutOutcome.Locked] on
     * `identity_choice`, where the machine has already returned the wizard.
     * Only the unlock time is the client's: the shared `formatUnixLocal`.
     */
    fun settle(outcome: EmergencyLockoutOutcome, context: Context) {
        val unlockTime = when (outcome) {
            is EmergencyLockoutOutcome.Locked -> formatUnixLocal(outcome.lockedUntil)
            else -> ""
        }
        resolveLocalized(context, emergencyLockoutOutcomeMessage(outcome, unlockTime))?.let {
            host.machine.setErrorMessage(it)
        }
    }
}

/**
 * The signed-out lock door (`ui/sessions.md` § Layout & flow → *The signed-out
 * door*; `devices.md` § The signed-out door) — android's leg of the page tui
 * led. Reached from `identity_choice`'s `emergency-lockout-entry-button`: paste
 * the secret key, give the handle, type LOCK. Pre-identity; persists nothing.
 * A lock returns to `identity_choice` with the confirmation on its
 * `error-message`; every refusal stays here, speaking.
 */
@Composable
fun EmergencyLockoutEntryScreen(
    navController: NavController,
    vm: EmergencyLockoutEntryVM = hiltViewModel(),
) {
    val context = LocalContext.current
    val tick by vm.host.tick.collectAsState()
    val machine = vm.host.machine
    // A fresh page per visit and never saved: nothing typed here outlives it.
    var secret by remember { mutableStateOf("") }
    var account by remember { mutableStateOf("") }
    var confirm by remember { mutableStateOf("") }

    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_emergency_lockout_title),
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_emergency_lockout_desc),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            stringResource(R.string.onboarding_emergency_lockout_account_hint),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        OutlinedTextField(
            value = secret,
            onValueChange = { secret = it },
            label = { Text(stringResource(R.string.onboarding_emergency_lockout_secret_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.PASTE_SECRET_FIELD),
        )
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = account,
            onValueChange = { account = it },
            label = { Text(stringResource(R.string.common_handle)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD),
        )
        Spacer(Modifier.height(16.dp))
        // One copy of the REQUIRED warning: the signed-in Sessions page's string.
        Text(
            stringResource(R.string.sessions_lockout_warning),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.SESSIONS_LOCKOUT_WARNING),
        )
        Spacer(Modifier.height(12.dp))
        OutlinedTextField(
            value = confirm,
            onValueChange = { confirm = it },
            label = { Text(stringResource(R.string.sessions_lockout_confirm_placeholder)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag(Ids.SESSIONS_LOCKOUT_CONFIRM_FIELD),
        )

        remember(tick) { machine.errorMessage() }?.ifEmpty { null }?.let {
            Spacer(Modifier.height(8.dp))
            Text(
                it,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
            )
        }

        Spacer(Modifier.height(32.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = {
                    machine.back()
                    navController.popBackStack()
                },
                modifier = Modifier.testTag(Ids.EMERGENCY_LOCKOUT_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Button(
                onClick = {
                    vm.launchSubmit(secret, account, confirm, context) {
                        // A lock → identity_choice (the confirmation paints
                        // there); a refusal stays on this step, speaking.
                        val step = machine.step()
                        if (step != OnboardingStep.EMERGENCY_LOCKOUT_ENTRY) {
                            navController.popBackStack(routeForStep(step), inclusive = false)
                        }
                    }
                },
                // The shared word gates the button here AND in the machine.
                enabled = !vm.submitting && confirm == sessionsLockoutConfirmWord(),
                modifier = Modifier.testTag(Ids.SESSIONS_LOCKOUT_BUTTON),
            ) { Text(stringResource(R.string.sessions_lockout_button)) }
        }
    }
}
