package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
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
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.localized
import com.fauna.ffi.onboarding.CredentialForm
import com.fauna.ffi.onboarding.FieldTypePlain
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import kotlinx.coroutines.launch
import social.fauna.generated.Capability
import social.fauna.generated.Ids
import social.fauna.generated.PROVIDERS
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.FilledTonalButton
import com.fauna.app.ui.controls.OutlinedButton

@HiltViewModel
class VpsConfigVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun selectProvider(id: String) = host.machine.selectVpsProvider(id)
    fun setCred(field: String, value: String) = host.machine.setVpsCred(field, value)
    fun verify() = viewModelScope.launch { host.machine.verifyVps() }
    fun continueAction(onStep: (OnboardingStep) -> Unit) = viewModelScope.launch {
        host.machine.continueFromVps()
        onStep(host.machine.step())
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun VpsConfigScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: VpsConfigVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val state = remember(tick) { vm.host.machine.vpsConfig() }
    val isLoading = remember(tick) { vm.host.machine.isLoading() }
    val verifyAnswer = remember(tick) { vm.host.machine.verifyVpsPossibility() }
    // Continue paints from the machine's one answer (`ui/README.md` § Offer
    // only what is possible, rule 6): enabled exactly while possible, the
    // blocked reason beside it (rule 5) — never a condition of this screen's.
    val continuePossibility = remember(tick) { vm.host.machine.vpsContinuePossibility() }
    val visibleFields = remember(tick) { vm.host.machine.visibleVpsFields() }
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    // Twin of DnsConfigScreen's hosted-auth derivation — the CredentialsForm
    // composable itself stays FFI-free (onboarding.md § 4).
    val hostedAuthVpsFields = remember(tick, visibleFields) {
        visibleFields.filter { it.fieldType == FieldTypePlain.HOSTED_AUTH }
    }
    val hostedAuthLabels = remember(tick, hostedAuthVpsFields) {
        hostedAuthVpsFields.associate {
            it.id to hostedAuthButtonLabel(ctx, vm.host.machine.hostedAuthState(CredentialForm.VPS, it.id))
        }
    }
    val hostedAuth = remember(tick, hostedAuthVpsFields) {
        hostedAuthVpsFields.associate { it.id to vm.host.machine.hostedAuthPossibility(CredentialForm.VPS, it.id) }
    }

    val vpsProviders = remember { PROVIDERS.filter { it.capabilities.contains(Capability.VPS) } }
    val selectedProvider = vpsProviders.find { it.id == state.selectedProviderId }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(stringResource(R.string.onboarding_vps_config_title), style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(16.dp))

        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            vpsProviders.forEachIndexed { idx, provider ->
                FilledTonalButton(
                    onClick = { vm.selectProvider(provider.id) },
                    modifier = Modifier.testTag("vps-provider-row[$idx]"),
                    colors = if (provider.id == state.selectedProviderId) {
                        ButtonDefaults.filledTonalButtonColors(
                            containerColor = MaterialTheme.colorScheme.primaryContainer,
                        )
                    } else ButtonDefaults.filledTonalButtonColors(),
                ) { Text(provider.id.replaceFirstChar { it.uppercase() }) }
            }
        }

        if (selectedProvider != null) {
            Spacer(Modifier.height(16.dp))
            ProviderHelpRow(selectedProvider)

            Spacer(Modifier.height(8.dp))
            CredentialsForm(
                fields = visibleFields,
                creds = state.creds,
                tag = "vps-credentials-form",
                enabled = !isLoading,
                onChange = { id, v -> vm.setCred(id, v) },
                hostedAuthLabels = hostedAuthLabels,
                hostedAuth = hostedAuth,
                onHostedAuthClick = { fieldId ->
                    scope.launch {
                        try {
                            val prompt = vm.host.machine.hostedAuthBegin(CredentialForm.VPS, fieldId)
                            UrlOpener.open(ctx, prompt.verificationUrl)
                            vm.host.machine.hostedAuthWait(CredentialForm.VPS, fieldId)
                        } catch (e: Exception) {
                            // Already in HostedAuthState.Failed (painted as the
                            // button label on the next tick) — nothing to
                            // re-derive here.
                        }
                    }
                },
            )

            Spacer(Modifier.height(8.dp))
            Button(
                onClick = { vm.verify() },
                enabled = verifyAnswer.isPossible && !isLoading,
                modifier = Modifier.testTag(Ids.VPS_VERIFY_BUTTON),
            ) {
                if (isLoading) {
                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                } else Text(stringResource(R.string.common_verify))
            }
            DisabledControlReasonText(localized(verifyAnswer.blockedReason))

            ProvisioningChoices(vm.host.machine, serverControls = true, tick = tick)
        }

        // The update channel shows before a provider is picked: it does not
        // depend on one (onboarding-provisioning.md § 5).
        if (selectedProvider == null) ProvisioningChoices(vm.host.machine, serverControls = false, tick = tick)

        Spacer(Modifier.weight(1f))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = { navController.popBackStack() },
                modifier = Modifier.testTag(Ids.VPS_CONFIG_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Column(horizontalAlignment = Alignment.End) {
                Button(
                    onClick = {
                        vm.continueAction { step ->
                            if (step == OnboardingStep.DONE) {
                                vm.host.handleWizardExit(step)?.let(onWizardExit)
                            } else {
                                navController.navigate(routeForStep(step))
                            }
                        }
                    },
                    enabled = continuePossibility.isPossible && !isLoading,
                    modifier = Modifier.testTag(Ids.VPS_CONFIG_CONTINUE_BUTTON),
                ) { Text(stringResource(R.string.common_continue)) }
                DisabledControlReasonText(localized(continuePossibility.blockedReason))
            }
        }
    }
}
