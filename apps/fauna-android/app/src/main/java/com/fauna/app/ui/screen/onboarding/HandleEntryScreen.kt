package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.ffi.onboarding.HandleCheckOutcome
import com.fauna.ffi.onboarding.HandleCheckPhase
import com.fauna.ffi.onboarding.OnboardingStep
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Thin Compose surface over OnboardingHost.machine. Renders
 * handle_check_snapshot() per docs/goal/behavior/onboarding.md §2 (Handle entry).
 * No per-stage VM state — the wizard owns it.
 */
@HiltViewModel
class HandleEntryVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun startCheck(handle: String) = viewModelScope.launch {
        host.machine.startHandleCheck(handle)
    }

    fun submitContinue(onStep: (OnboardingStep) -> Unit) = viewModelScope.launch {
        onStep(host.machine.submitHandleCheckContinue())
    }

    fun setControl(checked: Boolean) {
        host.machine.setControlCheckbox(checked)
    }

    fun setHandleInput(h: String) {
        host.machine.setCurrentHandle(h)
    }
}

@Composable
fun HandleEntryScreen(
    navController: NavController,
    onWizardExit: (com.fauna.ffi.onboarding.WizardOutcome) -> Unit,
    vm: HandleEntryVM = hiltViewModel(),
) {
    // Subscribe to observer ticks so reads pick up snapshot updates.
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.host.machine.handleCheckSnapshot() }
    val isLoading = remember(tick) { vm.host.machine.isLoading() }

    var input by remember { mutableStateOf(vm.host.machine.currentHandle()) }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Spacer(Modifier.height(24.dp))
        Text(
            stringResource(R.string.onboarding_handle_prompt),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_handle_examples_help),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = input,
                onValueChange = {
                    input = it
                    vm.setHandleInput(it)
                },
                modifier = Modifier.weight(1f).testTag(Ids.HANDLE_INPUT),
                singleLine = true,
                enabled = !isLoading,
                placeholder = { Text("alice@example.com") },
            )
            Button(
                onClick = { vm.startCheck(input) },
                enabled = input.isNotBlank() && !isLoading,
                modifier = Modifier.testTag(Ids.HANDLE_CHECK_BUTTON),
            ) {
                if (isLoading) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(18.dp),
                        strokeWidth = 2.dp,
                    )
                } else {
                    Text(stringResource(R.string.common_check))
                }
            }
        }

        Spacer(Modifier.height(16.dp))

        // Auto-sizing message panel — driven by snapshot.message (LocalizedText)
        // or the in-flight phase. Empty key = render nothing.
        val phaseLine = inFlightPhaseString(snap.phase)
        val messageLine = localizedOnboardingText(snap.message)
        val display = phaseLine ?: messageLine
        if (!display.isNullOrEmpty()) {
            Text(
                display,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.HANDLE_MESSAGE_AREA),
                style = MaterialTheme.typography.bodyMedium,
            )
            Spacer(Modifier.height(8.dp))
        }

        if (snap.controlCheckboxVisible) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth(),
            ) {
                Checkbox(
                    checked = snap.controlCheckboxChecked,
                    onCheckedChange = { vm.setControl(it) },
                    modifier = Modifier.testTag(Ids.HANDLE_CONTROL_CHECKBOX),
                )
                Text(stringResource(R.string.onboarding_handle_control_checkbox))
            }
        }

        Spacer(Modifier.weight(1f))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = { navController.popBackStack() },
                modifier = Modifier.testTag(Ids.HANDLE_ENTRY_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Button(
                onClick = {
                    vm.submitContinue { step ->
                        if (step == OnboardingStep.DONE) {
                            vm.host.handleWizardExit(step)?.let(onWizardExit)
                        } else {
                            navController.navigate(routeForStep(step))
                        }
                    }
                },
                enabled = snap.continueEnabled && !isLoading,
                modifier = Modifier.testTag(Ids.HANDLE_ENTRY_CONTINUE_BUTTON),
            ) { Text(stringResource(R.string.common_continue)) }
        }
    }
}

/** While a probe is in flight, render the phase string instead of the outcome. */
@Composable
private fun inFlightPhaseString(phase: HandleCheckPhase): String? = when (phase) {
    HandleCheckPhase.PARSING -> stringResource(R.string.onboarding_handle_check_phase_parsing)
    HandleCheckPhase.DNS_LOOKUP -> stringResource(R.string.onboarding_handle_check_phase_dns_lookup)
    HandleCheckPhase.NEST_PROBE -> stringResource(R.string.onboarding_handle_check_phase_nest_probe)
    HandleCheckPhase.CHALLENGE_RESPONSE -> stringResource(R.string.onboarding_handle_check_phase_challenge_response)
    HandleCheckPhase.PRICE_LOOKUP -> stringResource(R.string.onboarding_handle_check_phase_price_lookup)
    HandleCheckPhase.IDLE, HandleCheckPhase.COMPLETE -> null
}

internal fun routeForStep(step: OnboardingStep): String = when (step) {
    OnboardingStep.IDENTITY_CHOICE -> "onboarding/identity-choice"
    OnboardingStep.IDENTITY_CREATED -> "onboarding/identity-created"
    OnboardingStep.IDENTITY_IMPORT -> "onboarding/identity-import"
    OnboardingStep.HANDLE_ENTRY -> "onboarding/handle-entry"
    OnboardingStep.INVITE_REQUEST -> "onboarding/invite-request"
    OnboardingStep.DNS_CONFIG -> "onboarding/dns-config"
    OnboardingStep.VPS_CONFIG -> "onboarding/vps-config"
    OnboardingStep.NEST_PROVISIONING -> "onboarding/nest-provisioning"
    OnboardingStep.DNS_POST_INSTRUCTIONS -> "onboarding/dns-post-instructions"
    OnboardingStep.CLAIM_CODE -> "onboarding/claim-code"
    OnboardingStep.DONE -> "onboarding/handle-entry"  // unreachable; caller routes via WizardOutcome
    // Identity-recovery steps (onboarding.md § 1 Identity, 2026-08-01) —
    // unreachable on android until its trickle-down lands: the machine only
    // routes into RECOVERY_KIT for apps that declared setRendersRecoveryKit
    // (android has not), and RECOVERY_ENTRY needs a restore button android
    // doesn't render yet. tui leads.
    OnboardingStep.RECOVERY_KIT -> "onboarding/handle-entry"
    OnboardingStep.RECOVERY_ENTRY -> "onboarding/handle-entry"
    // The one-tap trust offer (onboarding.md § 3b-ter) — android declares
    // setRendersTrustPrompt (OnboardingHost), so the machine routes both
    // nat_mode_choice exits through it.
    OnboardingStep.TRUST_PROMPT -> "onboarding/trust-prompt"
    // The terminal admin-path step, reached directly on claim completion
    // (onboarding.md § 3b-bis; storage-modes.md — the storage-mode step that
    // used to precede it is retired).
    OnboardingStep.NAT_MODE_CHOICE -> "onboarding/nat-mode-choice"

    // box-recovery.md § Recovery UI (step 4) — Task E.
    OnboardingStep.NEST_RECOVERY -> "onboarding/nest-recovery"
    OnboardingStep.RECOVER_SELFHOSTED_INSTRUCTIONS -> "onboarding/recover-selfhosted-instructions"
}
