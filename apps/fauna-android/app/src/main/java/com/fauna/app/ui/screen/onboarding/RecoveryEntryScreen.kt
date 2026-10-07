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
import com.fauna.app.core.ShellLog
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.RecoveryEntryOutcome
import com.fauna.ffi.onboarding.recoveryEntryOutcomeMessage
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import kotlinx.coroutines.launch
import social.fauna.generated.Ids

@HiltViewModel
class RecoveryEntryVM @Inject constructor(
    val host: OnboardingHost,
    private val registry: FfiAccountRegistry,
) : ViewModel() {
    var submitting by mutableStateOf(false)
        private set

    /** The ceremony call — a seam only so the host tests need no live nest. */
    internal var submitEntry: suspend (String) -> RecoveryEntryOutcome =
        { phrase -> host.machine.submitRecoveryEntry(phrase) }

    /**
     * `recovery-entry-submit-button`: the shared pre-identity escrow restore.
     * The typed account rides on the machine's one account field — the one
     * `handle_entry` asks for next — ALWAYS, empty included: what the field
     * shows is what is sent (forwarding only a non-empty value let a handle an
     * earlier flow left on the machine target an account the user never typed).
     */
    suspend fun submit(phrase: String, account: String, context: Context) {
        host.machine.setCurrentHandle(account.trim())
        settle(submitEntry(phrase), context)
    }

    fun launchSubmit(phrase: String, account: String, context: Context, then: () -> Unit) {
        submitting = true
        viewModelScope.launch {
            try {
                submit(phrase, account, context)
            } finally {
                submitting = false
            }
            then()
        }
    }

    /**
     * Fold a restore's outcome back into the wizard. `Superseded` routes
     * instead of speaking — the import page, carrying why, the same route the
     * launch flow's superseded refusal takes. A restored seed is committed
     * exactly as an import commits it (the shared confirm-identity moment, both
     * modes), so a crash before complete-login resumes at `handle_entry`. What
     * every other outcome SAYS is the shared table.
     */
    fun settle(outcome: RecoveryEntryOutcome, context: Context) {
        when (outcome) {
            is RecoveryEntryOutcome.Superseded -> {
                host.machine.beginImportIdentityWithReason(
                    context.getString(R.string.onboarding_recovery_entry_superseded),
                )
                return
            }
            is RecoveryEntryOutcome.Restored,
            is RecoveryEntryOutcome.RestoredPredecessorsLost -> {
                host.machine.effectiveSecret()?.let { secret ->
                    try {
                        registry.confirmIdentity(secret, host.appendMode)
                    } catch (e: Exception) {
                        ShellLog.w(
                            "RecoveryEntryVM",
                            "persist_confirmed_identity (registry commit + read-back) failed: ${e.message}",
                        )
                    }
                }
            }
            else -> {}
        }
        resolveLocalized(context, recoveryEntryOutcomeMessage(outcome))?.let {
            host.machine.setErrorMessage(it)
        }
    }
}

/**
 * The phrase-only identity restore (`onboarding.md` § 1 Identity) — android's
 * leg of the page tui led (linux prior art:
 * `apps/fauna-linux/src/views/onboarding/recovery_entry.rs`). Reached from
 * `identity_choice`'s `restore-from-recovery-kit-button`; on success it lands
 * on `handle_entry` holding the recovered seed — exactly where an import lands.
 *
 * The account field asks for a handle (the ceremony has no session to ask
 * where the account lives). `qr-camera-view` is optional here and android's
 * import page has no camera capture yet either, so a paste is the path.
 * Submit is enabled unconditionally: every way the input can be wrong is an
 * answer the ceremony gives on `error-message`.
 */
@Composable
fun RecoveryEntryScreen(
    navController: NavController,
    vm: RecoveryEntryVM = hiltViewModel(),
) {
    val context = LocalContext.current
    val tick by vm.host.tick.collectAsState()
    val machine = vm.host.machine
    // A fresh page per visit: the composable leaves the back stack between
    // visits, so nothing typed on an earlier one rides along — the account
    // field is forwarded as shown.
    var phrase by remember { mutableStateOf("") }
    var account by remember { mutableStateOf("") }

    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_recovery_entry_title),
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_recovery_entry_desc),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            stringResource(R.string.onboarding_recovery_entry_account_hint),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        OutlinedTextField(
            value = phrase,
            onValueChange = { phrase = it },
            label = { Text(stringResource(R.string.onboarding_recovery_entry_phrase_label)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_ENTRY_PHRASE_FIELD),
        )
        Spacer(Modifier.height(12.dp))
        // The shared handle label: the same field concept as handle_entry's,
        // which a successful restore pre-fills.
        OutlinedTextField(
            value = account,
            onValueChange = { account = it },
            label = { Text(stringResource(R.string.common_handle)) },
            singleLine = true,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_ENTRY_ACCOUNT_FIELD),
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
                modifier = Modifier.testTag(Ids.RECOVERY_ENTRY_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Button(
                onClick = {
                    vm.launchSubmit(phrase, account, context) {
                        // Restored → handle_entry; Superseded → identity_import;
                        // a refusal stays here, speaking.
                        val step = machine.step()
                        if (step != OnboardingStep.RECOVERY_ENTRY) {
                            navController.navigate(routeForStep(step))
                        }
                    }
                },
                enabled = !vm.submitting,
                modifier = Modifier.testTag(Ids.RECOVERY_ENTRY_SUBMIT_BUTTON),
            ) { Text(stringResource(R.string.onboarding_recovery_entry_submit)) }
        }
    }
}
