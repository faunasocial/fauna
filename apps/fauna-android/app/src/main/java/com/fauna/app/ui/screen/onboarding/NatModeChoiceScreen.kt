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
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.ffi.onboarding.NatModeState
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import uniffi.fauna_core.NodeMode
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Snapshot-driven view of `OnboardingMachine.natModeSnapshot()` per
 * docs/goal/behavior/onboarding.md § 3b-bis "NAT mode choice".
 *
 * The **single, terminal admin-path setup step**, reached directly on a
 * successful claim-code submission (there is no storage-mode question — the
 * axis is retired; see docs/goal/architecture/nest/storage-modes.md). The admin
 * confirms the nest's network-reachability axis (public / private) and the
 * wizard exits to the logged-in app on either confirm or defer.
 *
 * No Back button — the admin is already server-committed by the claim.
 */
@HiltViewModel
class NatModeChoiceVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun select(mode: NodeMode) { host.machine.selectNatMode(mode) }
    suspend fun submit(): OnboardingStep = host.machine.submitNatModeChoice()
    fun defer(): OnboardingStep = host.machine.deferNatModeChoice()
}

@Composable
fun NatModeChoiceScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: NatModeChoiceVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.host.machine.natModeSnapshot() }
    val coroutineScope = rememberCoroutineScope()

    val submitting = snap.state is NatModeState.Submitting

    fun routeStep(step: OnboardingStep) {
        if (step == OnboardingStep.DONE) {
            vm.host.handleWizardExit(step)?.let(onWizardExit)
        } else {
            navController.navigate(routeForStep(step))
        }
    }

    Column(modifier = Modifier.fillMaxSize().padding(24.dp)) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_nat_mode_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_nat_mode_description),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        // The pre-selection comes from the snapshot: the nest's resolved seed,
        // refined private-ward when the handle targets a private network
        // (`reset_nat_mode_snapshot`). The common case is therefore confirm-only.
        Row(verticalAlignment = Alignment.CenterVertically) {
            RadioButton(
                selected = snap.selectedMode == NodeMode.PUBLIC,
                onClick = { vm.select(NodeMode.PUBLIC) },
                enabled = !submitting,
                modifier = Modifier.testTag(Ids.PUBLIC_NAT_MODE_RADIO),
            )
            Column(Modifier.padding(start = 8.dp)) {
                Text(
                    stringResource(R.string.onboarding_nat_mode_public_label),
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    stringResource(R.string.onboarding_nat_mode_public_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        Spacer(Modifier.height(12.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            RadioButton(
                selected = snap.selectedMode == NodeMode.PRIVATE,
                onClick = { vm.select(NodeMode.PRIVATE) },
                enabled = !submitting,
                modifier = Modifier.testTag(Ids.PRIVATE_NAT_MODE_RADIO),
            )
            Column(Modifier.padding(start = 8.dp)) {
                Text(
                    stringResource(R.string.onboarding_nat_mode_private_label),
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    stringResource(R.string.onboarding_nat_mode_private_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }

        Spacer(Modifier.height(16.dp))
        // Renders the snapshot's localized message: Choosing / the private-ward
        // hint / Submitting / Done / the error cause.
        val statusText = localizedOnboardingText(snap.message)
        if (statusText != null) {
            Text(
                statusText,
                modifier = Modifier.fillMaxWidth().testTag(Ids.NAT_MODE_STATUS),
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        val errorState = snap.state as? NatModeState.Error
        if (errorState != null) {
            Spacer(Modifier.height(8.dp))
            Text(
                errorState.cause,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ERROR_MESSAGE),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }

        Spacer(Modifier.weight(1f))

        // ONE confirm button, always visible (unlike the retired encryption
        // page's two). `submitEnabled` drives enabled/disabled only, never
        // visibility — and the NAT set is a mutable upsert, so submit stays
        // enabled after an error and a resubmit is always allowed.
        Button(
            onClick = { coroutineScope.launch { routeStep(vm.submit()) } },
            enabled = snap.submitEnabled,
            modifier = Modifier.fillMaxWidth().testTag(Ids.NAT_MODE_CONFIRM_BUTTON),
        ) { Text(stringResource(R.string.onboarding_nat_mode_confirm_button)) }

        Spacer(Modifier.height(8.dp))
        // Defer sends nothing and persists no resume slot: the seeded mode is
        // already a working default server-side, and the admin can change it
        // later from Admin → Nest. Exits the wizard exactly as confirm does.
        TextButton(
            onClick = { routeStep(vm.defer()) },
            enabled = !submitting,
            modifier = Modifier.fillMaxWidth().testTag(Ids.NAT_MODE_DEFER_BUTTON),
        ) { Text(stringResource(R.string.onboarding_nat_mode_defer_button)) }
    }
}
