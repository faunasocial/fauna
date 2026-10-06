package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.components.CopyButton
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Per docs/goal/behavior/onboarding.md §7: rendered DKIM + other manual records
 * the user must add at their registrar after the deferred-DNS path. Single
 * Continue button → continueFromDnsPostInstructions() → AwaitingManualDns
 * outcome.
 */
@HiltViewModel
class DnsPostInstructionsVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    val records get() = host.machine.dnsRecords()

    fun continueAction(): OnboardingStep = host.machine.continueFromDnsPostInstructions()
}

@Composable
fun DnsPostInstructionsScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: DnsPostInstructionsVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val records = remember(tick) { vm.records }
    val recordsText = remember(records) {
        records.joinToString("\n") { rec ->
            "${rec.recordType}\t${rec.name}\t${rec.value}"
        }
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_dns_post_instructions_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_dns_post_instructions_description),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(16.dp))

        Card(
            modifier = Modifier
                .fillMaxWidth()
                .weight(1f, fill = false),
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
        ) {
            Text(
                recordsText.ifBlank { stringResource(R.string.onboarding_dns_post_instructions_records_pending) },
                modifier = Modifier
                    .padding(16.dp)
                    .verticalScroll(rememberScrollState())
                    .testTag(Ids.DNS_POST_INSTRUCTIONS_TEXT),
                style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace),
            )
        }

        Spacer(Modifier.height(8.dp))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CopyButton(
                testTag = Ids.DNS_POST_INSTRUCTIONS_COPY_BUTTON,
                text = recordsText,
                label = stringResource(R.string.onboarding_dns_post_instructions_copy_button),
            )

            Button(
                onClick = {
                    val step = vm.continueAction()
                    if (step == OnboardingStep.DONE) {
                        val outcome = vm.host.handleWizardExit(step)
                        if (outcome is WizardOutcome.AwaitingManualDns) {
                            // NOT an exit from onboarding, even though the step is
                            // Done: the nest is provisioned but unclaimed, so the
                            // wizard stays open on the "Almost ready" surface and
                            // polls until DNS resolves. Calling onWizardExit here
                            // would drop the user into the app with no nest.
                            // handleWizardExit above has already persisted the
                            // resume slot, so a force-quit now still comes back here.
                            navController.navigate("onboarding/almost-ready")
                        } else {
                            outcome?.let(onWizardExit)
                        }
                    } else {
                        navController.navigate(routeForStep(step))
                    }
                },
                modifier = Modifier.testTag(Ids.DNS_POST_INSTRUCTIONS_CONTINUE_BUTTON),
            ) { Text(stringResource(R.string.common_continue)) }
        }
    }
}
