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
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Stage 3b-ter: the one-tap "trust this box" offer (`onboarding.md` §
 * 3b-ter) — the one ratified survivor of the retired claim-time trust
 * question (`storage-modes.md` § What replaced each piece of the axis).
 *
 * ui.yaml `onboarding.trust_prompt` elements: `trust-box-summary`,
 * `trust-box-grant-button`, `trust-box-skip-button` (+ `error-message`).
 * Deliberately no `page-heading` id — the approved set is exactly those
 * three, same as tui/linux.
 *
 * No Back button — like the NAT page before it, the admin is already
 * server-committed by the time this shows; grant and skip are its only
 * exits and both conclude the wizard identically.
 *
 * The screen **asks only.** Minting the default set needs an authenticated
 * session and the nest's content-processor roster, neither of which the
 * wizard holds, so the answer is latched (`grantDefaultTrust` →
 * `takeTrustPromptGranted`) and the mint runs at the signed-in handoff
 * ([com.fauna.app.ui.viewmodel.MailEnableGlueVM.mintDefaultTrustSet]) —
 * the same deferral the deployment seed uses. android only reaches this
 * page because `OnboardingHost` declares `setRendersTrustPrompt(true)`.
 */
@HiltViewModel
class TrustPromptVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun grant(): OnboardingStep = host.machine.grantDefaultTrust()
    fun skip(): OnboardingStep = host.machine.skipTrustPrompt()
}

@Composable
fun TrustPromptScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: TrustPromptVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val errorMessage = remember(tick) { vm.host.machine.errorMessage() }

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
            stringResource(R.string.onboarding_trust_prompt_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_trust_prompt_summary),
            modifier = Modifier.testTag(Ids.TRUST_BOX_SUMMARY),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.weight(1f))

        // Present for the page contract (e2e convention 2) even though
        // neither exit can fail here: both are pure local latches.
        if (!errorMessage.isNullOrEmpty()) {
            Text(
                errorMessage,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
            Spacer(Modifier.height(8.dp))
        }

        Button(
            onClick = { routeStep(vm.grant()) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.TRUST_BOX_GRANT_BUTTON),
        ) { Text(stringResource(R.string.onboarding_trust_prompt_grant_button)) }

        Spacer(Modifier.height(8.dp))
        TextButton(
            onClick = { routeStep(vm.skip()) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.TRUST_BOX_SKIP_BUTTON),
        ) { Text(stringResource(R.string.onboarding_trust_prompt_skip_button)) }
    }
}
