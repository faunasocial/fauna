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
import com.fauna.ffi.onboarding.AwaitingDnsState
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * How often the surface re-probes the nest while it is shown.
 * `recheckManualDns()` is single-shot by contract, so the client owns the
 * cadence and native/wasm behave identically.
 *
 * Reads the shared constant rather than restating `10_000` — onboarding.md
 * § The pending-invite surface, "never seven hand-copied numbers". This was one
 * of the per-app copies that bullet names (lifted 2026-08-12).
 */
private val POLL_INTERVAL_MS = com.fauna.ffi.onboarding.awaitingDnsPollMs().toLong()

/**
 * The "Almost ready" surface — post-provisioning, DNS still pending
 * (docs/goal/behavior/onboarding.md § "Almost ready" surface).
 *
 * **Not an OnboardingStep.** It is shown whenever `wizardOutcome()` is
 * `AwaitingManualDns`, which is true on both paths that reach it: the
 * same-session exit from `dns_post_instructions`, and the relaunch hydration
 * (awaiting-DNS slot → LaunchMachine's `WizardAt{AwaitingManualDns}` row →
 * `seedAwaitingManualDnsJson`). Keying on the outcome rather than a step is what
 * makes those two paths one screen, and it avoids growing the `OnboardingStep`
 * enum that every app matches on exhaustively.
 */
@HiltViewModel
class AwaitingManualDnsVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun snapshot() = host.machine.awaitingManualDnsSnapshot()

    /** Shared formatter — the records label and the copy button cannot drift apart. */
    fun recordsText(): String = host.machine.awaitingDnsRecordsText()

    /** Whether "Copy all" has anything to copy — false for a resumed
     * standard-path run's records-less slot. */
    fun copyEnabled(): Boolean = host.machine.awaitingDnsCopyEnabled()

    suspend fun recheck(): OnboardingStep = host.machine.recheckManualDns()

    /** True while the wizard is still parked at the deferred-DNS exit. */
    fun stillAwaiting(): Boolean =
        host.machine.wizardOutcome() is WizardOutcome.AwaitingManualDns
}

@Composable
fun AwaitingManualDnsScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: AwaitingManualDnsVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.snapshot() }
    val recordsText = remember(tick) { vm.recordsText() }
    val statusText = localizedOnboardingText(snap.message)

    // A probe or claim is already in flight — a second would only race the first.
    val busy = snap.state is AwaitingDnsState.Checking || snap.state is AwaitingDnsState.Claiming

    // Records-less resumed runs have nothing to copy; disabled, never hidden —
    // ui.yaml scopes this ID to the page's required elements.
    val copyEnabled = remember(tick) { vm.copyEnabled() }

    // One probe, then route on what it produced. Note the middle branch: on this
    // surface `DONE` is *also* the still-waiting state (the wizard sits at Done
    // with an AwaitingManualDns outcome), so handing every DONE to
    // handleWizardExit would fire onWizardExit and drop the user out of
    // onboarding while the nest is still unreachable.
    val probe: suspend () -> Unit = probe@{
        val step = vm.recheck()
        when {
            step != OnboardingStep.DONE -> navController.navigate(routeForStep(step))
            vm.stillAwaiting() -> Unit
            else -> vm.host.handleWizardExit(step)?.let(onWizardExit)
        }
    }

    // Cancelled with the composable, so leaving the surface stops the probing —
    // which is why nothing below has to ask "am I still on this screen".
    LaunchedEffect(Unit) {
        while (true) {
            delay(POLL_INTERVAL_MS)
            probe()
        }
    }

    val scope = rememberCoroutineScope()

    Column(modifier = Modifier.fillMaxSize().padding(24.dp)) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_awaiting_dns_title),
            style = MaterialTheme.typography.headlineSmall,
        )

        Spacer(Modifier.height(8.dp))
        // The machine owns the wording of every state, including the terminal
        // Error{cause}, handed over as a LocalizedText — never re-derived here, so
        // all seven apps say the same thing in the same state.
        Text(
            statusText.orEmpty(),
            modifier = Modifier.fillMaxWidth().testTag(Ids.AWAITING_DNS_STATUS),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(16.dp))

        Card(
            modifier = Modifier.fillMaxWidth().weight(1f, fill = false),
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
        ) {
            Text(
                recordsText,
                modifier = Modifier
                    .padding(16.dp)
                    .verticalScroll(rememberScrollState())
                    .testTag(Ids.AWAITING_DNS_RECORDS),
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
                testTag = Ids.AWAITING_DNS_COPY_BUTTON,
                text = recordsText,
                label = stringResource(R.string.onboarding_awaiting_dns_copy_button),
                enabled = copyEnabled,
            )

            // The explicit "check now" probe: the same single-shot the timer runs,
            // it just skips the wait.
            Button(
                onClick = { scope.launch { probe() } },
                enabled = !busy,
                modifier = Modifier.testTag(Ids.AWAITING_DNS_RECHECK_BUTTON),
            ) { Text(stringResource(R.string.onboarding_awaiting_dns_recheck_button)) }
        }
    }
}
