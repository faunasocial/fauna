package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
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
import com.fauna.app.ui.util.localized
import com.fauna.ffi.onboarding.CredentialForm
import com.fauna.ffi.onboarding.FieldTypePlain
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import com.fauna.ffi.onboarding.serverTypeAllowedForMail
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import social.fauna.generated.Capability
import social.fauna.generated.PROVIDERS
import javax.inject.Inject
import social.fauna.generated.Ids

@HiltViewModel
class VpsConfigVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun selectProvider(id: String) = host.machine.selectVpsProvider(id)
    fun setCred(field: String, value: String) = host.machine.setVpsCred(field, value)
    fun selectLocation(id: String) = host.machine.selectVpsLocation(id)
    fun selectServerType(id: String) = host.machine.selectVpsServerType(id)
    fun setProvisionMailMode(enabled: Boolean) = host.machine.setProvisionMailMode(enabled)
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
    val canVerify = remember(tick) { vm.host.machine.canVerifyVps() }
    val canContinue = remember(tick) { vm.host.machine.canContinueVps() }
    // Why Continue is dead when it is — `ui/README.md` rule 5; paired with
    // canContinue over one private shortfall so verdict and explanation can't
    // disagree (onboarding.md § 5).
    val continueBlockedReason = remember(tick) { vm.host.machine.vpsContinueBlockedReason() }
    val visibleFields = remember(tick) { vm.host.machine.visibleVpsFields() }
    val mailOn = remember(tick) { vm.host.machine.provisionMailModeEnabled() }
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
    val hostedAuthEnabled = remember(tick, hostedAuthVpsFields) {
        hostedAuthVpsFields.associate { it.id to vm.host.machine.hostedAuthCanBegin(CredentialForm.VPS, it.id) }
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
                hostedAuthEnabled = hostedAuthEnabled,
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
                enabled = canVerify && !isLoading,
                modifier = Modifier.testTag(Ids.VPS_VERIFY_BUTTON),
            ) {
                if (isLoading) {
                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                } else Text(stringResource(R.string.common_verify))
            }

            if (state.locations.isNotEmpty()) {
                Spacer(Modifier.height(16.dp))
                var locationPickerExpanded by remember { mutableStateOf(false) }
                val selectedLocation = state.locations.firstOrNull { it.id == state.selectedLocationId }
                ExposedDropdownMenuBox(
                    expanded = locationPickerExpanded,
                    onExpandedChange = { locationPickerExpanded = !locationPickerExpanded },
                ) {
                    OutlinedTextField(
                        value = selectedLocation?.name ?: "",
                        onValueChange = {},
                        readOnly = true,
                        label = { Text(stringResource(R.string.onboarding_vps_config_location_heading)) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = locationPickerExpanded) },
                        modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.VPS_LOCATION_PICKER),
                    )
                    ExposedDropdownMenu(
                        expanded = locationPickerExpanded,
                        onDismissRequest = { locationPickerExpanded = false },
                    ) {
                        state.locations.forEach { loc ->
                            DropdownMenuItem(
                                text = { Text(loc.name) },
                                onClick = {
                                    locationPickerExpanded = false
                                    vm.selectLocation(loc.id)
                                },
                                modifier = Modifier.testTag("vps-location-picker[${loc.id}]"),
                            )
                        }
                    }
                }
            }

            // ── Mail-vs-social mode toggle ──────────────────────────────
            // Decided here (before the box boots) so cloud-init knows whether
            // to provision the scanner sidecars + mail ports. Drives the
            // server-type RAM gate just below (mail ON => only >=2 GB plans).
            // Default reads provisionMailModeEnabled() (handle real-domain
            // default); user-driven thereafter. See onboarding.md §5.
            Spacer(Modifier.height(16.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = mailOn,
                    onCheckedChange = { vm.setProvisionMailMode(it) },
                    modifier = Modifier.testTag(Ids.VPS_CONFIG_MAIL_MODE_TOGGLE),
                )
                Column(Modifier.padding(start = 8.dp)) {
                    Text(
                        stringResource(R.string.onboarding_vps_config_mail_mode_label),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(
                        stringResource(R.string.onboarding_vps_config_mail_mode_desc),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            // serverTypes are already curated to <= 5 by the machine; the
            // mail-mode RAM gate filters them before indexing so the radio
            // test-ids stay 0-based over the shown set (shared gate helper,
            // identical across all seven apps).
            if (state.serverTypes.isNotEmpty()) {
                Spacer(Modifier.height(16.dp))
                Text(stringResource(R.string.onboarding_vps_config_server_type_radio_legend), style = MaterialTheme.typography.titleSmall)
                state.serverTypes.filter { serverTypeAllowedForMail(it, mailOn) }.forEachIndexed { idx, server ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = state.selectedServerTypeId == server.id,
                                onClick = { vm.selectServerType(server.id) },
                            )
                            .testTag("vps-server-type-radio[$idx]"),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = state.selectedServerTypeId == server.id,
                            onClick = { vm.selectServerType(server.id) },
                        )
                        Text(server.id)
                    }
                }
            }
        }

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
                    enabled = canContinue && !isLoading,
                    modifier = Modifier.testTag(Ids.VPS_CONFIG_CONTINUE_BUTTON),
                ) { Text(stringResource(R.string.common_continue)) }
                DisabledControlReasonText(localized(continueBlockedReason))
            }
        }
    }
}
