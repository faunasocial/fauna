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
import com.fauna.ffi.onboarding.ClaimCodeState
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Snapshot-driven view of OnboardingMachine.claim_code_snapshot() per
 * docs/goal/behavior/onboarding.md §3a "Claim code". Reached only when the
 * target nest is unclaimed (no admin yet); mutually exclusive with
 * invite_request. A successful claim auto-advances to nat_mode_choice
 * (the admin path's terminal step, §3b-bis), so submit routes on the
 * returned step.
 */
@HiltViewModel
class ClaimCodeVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    suspend fun submit(code: String): OnboardingStep = host.machine.wizardSubmitClaimCode(code)
    fun back() { host.machine.back() }
}

@Composable
fun ClaimCodeScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: ClaimCodeVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.host.machine.claimCodeSnapshot() }
    val coroutineScope = rememberCoroutineScope()

    var codeInput by remember { mutableStateOf("") }

    // Pre-fill from the machine when the input is empty (mirrors Linux
    // claim_code.rs): the factory-reset re-onboard path positions the wizard here
    // via navigate_to_claim_code_for_known_nest_with_code, and the human never
    // sees that code — it must be pre-loaded, not typed. The ordinary
    // unclaimed-nest path carries no prefill (claim_code_prefill() == null), so
    // this is a no-op there. Don't fight a user edit: only fill while empty.
    LaunchedEffect(tick) {
        if (codeInput.isEmpty()) {
            vm.host.machine.claimCodePrefill()?.takeIf { it.isNotEmpty() }?.let { codeInput = it }
        }
    }

    Column(modifier = Modifier.fillMaxSize().padding(24.dp)) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_claim_code_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_claim_code_description),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        OutlinedTextField(
            value = codeInput,
            onValueChange = { codeInput = it },
            modifier = Modifier.fillMaxWidth().testTag(Ids.CLAIM_CODE_INPUT),
            singleLine = true,
            enabled = snap.state !is ClaimCodeState.Submitting,
            placeholder = { Text(stringResource(R.string.onboarding_claim_code_placeholder)) },
        )

        Spacer(Modifier.height(8.dp))
        val statusText = localizedOnboardingText(snap.message)
        if (statusText != null) {
            Text(
                statusText,
                modifier = Modifier.fillMaxWidth().testTag(Ids.CLAIM_CODE_STATUS),
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        Spacer(Modifier.weight(1f))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = {
                    vm.back()
                    navController.popBackStack()
                },
                modifier = Modifier.testTag(Ids.CLAIM_CODE_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Button(
                onClick = {
                    coroutineScope.launch {
                        val step = vm.submit(codeInput)
                        if (step == OnboardingStep.DONE) {
                            vm.host.handleWizardExit(step)?.let(onWizardExit)
                        } else {
                            navController.navigate(routeForStep(step))
                        }
                    }
                },
                enabled = snap.submitEnabled && codeInput.isNotBlank(),
                modifier = Modifier.testTag(Ids.CLAIM_CODE_SUBMIT_BUTTON),
            ) { Text(stringResource(R.string.onboarding_claim_code_submit_button)) }
        }
    }
}
