package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
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
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.provisioningStepLabel
import com.fauna.ffi.provisioningSubstepLabel
import com.fauna.ffi.onboarding.BillOfMaterialsItem
import com.fauna.ffi.onboarding.OnboardingStep
import com.fauna.ffi.onboarding.WizardOutcome
import com.fauna.ffi.onboarding.formatPrice
import com.fauna.ffi.provisioning.OverallStatus
import com.fauna.ffi.provisioning.ProvisionStep
import com.fauna.ffi.provisioning.StepSnapshot
import com.fauna.ffi.provisioning.StepStatus
import com.fauna.ffi.provisioning.SubstepKey
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import javax.inject.Inject
import social.fauna.generated.Ids

/**
 * Snapshot-driven view of OnboardingMachine.provisioning_snapshot()
 * per docs/goal/behavior/onboarding.md §6 "Nest provisioning". Four-step
 * (Domain/Server/Dns/Online) progress + cancel/retry + Continue when done.
 *
 * The orchestrator is idempotent so retry just re-runs from the top
 * (skipping completed steps); cancel sets a soft flag the running step
 * observes at the next boundary.
 */
@HiltViewModel
class NestProvisioningVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    // `startProvisioning()`/`retryProvisioning()` (sync) do a raw `tokio::spawn`
    // inside machine.rs with no ambient runtime guaranteed on the calling thread —
    // the same hazard linux's GTK button hit (onboarding.md §"E2E bridge contract").
    // Drive the async `runProvisioning()` suspend export instead, which UniFFI
    // dispatches onto its own managed tokio runtime; `run_provisioning_inner`
    // resets the snapshot + cancel flag at entry, so it doubles as the retry/resume
    // entry point too (mirrors apps/fauna-linux/src/views/onboarding/nest_provisioning.rs).
    fun start() {
        viewModelScope.launch { host.machine.runProvisioning() }
    }
    fun cancel() = host.machine.cancelProvisioning()
    fun retry() {
        viewModelScope.launch { host.machine.runProvisioning() }
    }

    fun continueAction(): OnboardingStep = host.machine.continueFromProvisioning()
}

@Composable
fun NestProvisioningScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: NestProvisioningVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.host.machine.provisioningSnapshot() }
    // Why the wizard-exit Continue is dead when it is — `ui/README.md` rule 5.
    // `OnboardingMachine.provisioningContinueBlockedReason()` is already
    // UniFFI-visible (same exported impl block as `vpsContinueBlockedReason`,
    // libs/fauna-onboarding-machine/src/machine.rs) — no FFI wrapper needed.
    val continueBlockedReason = remember(tick) { vm.host.machine.provisioningContinueBlockedReason() }
    val coroutineScope = rememberCoroutineScope()

    // Update elapsed-time display every second while running.
    var nowMs by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(snap.overall) {
        if (snap.overall == OverallStatus.RUNNING) {
            while (true) {
                nowMs = System.currentTimeMillis()
                delay(1000)
            }
        }
    }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_nest_provisioning_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))

        ProvisioningPriceBom(remember(tick) { vm.host.machine.billOfMaterials() })
        Spacer(Modifier.height(8.dp))

        // Top region: start CTA before running, elapsed timer once started.
        when (snap.overall) {
            OverallStatus.IDLE -> {
                Button(
                    onClick = { vm.start() },
                    modifier = Modifier.testTag(Ids.PROVISIONING_START_BUTTON),
                ) { Text(stringResource(R.string.onboarding_nest_provisioning_start_button)) }
            }
            OverallStatus.RUNNING, OverallStatus.SUCCEEDED, OverallStatus.FAILED, OverallStatus.CANCELLED -> {
                // Elapsed compute + format is single-sourced in shared Rust
                // (fauna_provisioning::progress::elapsed_display, via the FFI
                // provisioningElapsed): null → run not started → hide the row;
                // freezes at finishedAtMs once the run stops. nowMs is the live tick.
                localized(
                    com.fauna.ffi.provisioningElapsed(snap.startedAtMs, snap.finishedAtMs, nowMs.toULong()),
                )?.let { elapsed ->
                    Text(
                        elapsed,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.testTag(Ids.PROVISIONING_ELAPSED),
                    )
                }
            }
        }

        Spacer(Modifier.height(16.dp))

        // Per-step rows.
        snap.steps.forEachIndexed { index, step ->
            StepRow(index, step)
        }

        snap.finalError?.let {
            Spacer(Modifier.height(8.dp))
            Text(
                it,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
            )
        }

        Spacer(Modifier.weight(1f))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = { navController.popBackStack() },
                modifier = Modifier.testTag(Ids.PROVISIONING_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (snap.overall == OverallStatus.RUNNING) {
                    OutlinedButton(
                        onClick = { vm.cancel() },
                        modifier = Modifier.testTag(Ids.PROVISIONING_CANCEL_BUTTON),
                    ) { Text(stringResource(R.string.onboarding_nest_provisioning_cancel_button)) }
                }
                // Retry resumes a stopped run from either terminal state —
                // Failed or Cancelled (idempotency skips done steps). Without
                // the Cancelled case a soft-cancel strands the user with only
                // Back. Per docs/goal/behavior/onboarding.md §6.
                if (snap.overall == OverallStatus.FAILED ||
                    snap.overall == OverallStatus.CANCELLED) {
                    OutlinedButton(
                        onClick = { vm.retry() },
                        modifier = Modifier.testTag(Ids.PROVISIONING_RETRY_BUTTON),
                    ) { Text(stringResource(R.string.onboarding_nest_provisioning_retry_button)) }
                }
                Column(horizontalAlignment = Alignment.End) {
                    Button(
                        onClick = {
                            coroutineScope.launch {
                                val step = vm.continueAction()
                                if (step == OnboardingStep.DONE) {
                                    vm.host.handleWizardExit(step)?.let(onWizardExit)
                                } else {
                                    navController.navigate(routeForStep(step))
                                }
                            }
                        },
                        enabled = snap.overall == OverallStatus.SUCCEEDED,
                        modifier = Modifier.testTag(Ids.PROVISIONING_CONTINUE_BUTTON),
                    ) { Text(stringResource(R.string.common_continue)) }
                    DisabledControlReasonText(localized(continueBlockedReason))
                }
            }
        }
    }
}

/**
 * Top-region price summary ("Bill of Materials", onboarding.md §6) — a
 * pre-commit recap of up to two priced line items from
 * OnboardingMachine.billOfMaterials(): the domain's one-time registration
 * price (only when buying a new domain) and the selected VPS's recurring
 * monthly price (always present once vps_config's Continue has been taken).
 * Both prices were already shown/agreed earlier in the wizard
 * (dns-tld-price-display, the vps_config server-type options) — this is a
 * recap, not a new price source. Distinguishes the two items by `recurring`
 * (not vec order), mirroring linux's nest_provisioning.rs price-bom block,
 * tui, web's +page.svelte, and Apple's ProvisioningPriceBom.
 */
@Composable
private fun ProvisioningPriceBom(items: List<BillOfMaterialsItem>) {
    Column(
        modifier = Modifier.testTag(Ids.PROVISIONING_PRICE_BOM),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        items.firstOrNull { !it.recurring }?.let { domain ->
            // The renewal price + term are disclosed here when the registrar
            // quoted one (onboarding.md § 6) — the recurring cost the user is
            // signing up for, said before the charge, not after. Mirrors
            // linux's nest_provisioning.rs / tui's wizard::nest_provisioning.
            val renewal = domain.renewalPriceCents
            Text(
                if (renewal != null) {
                    stringResourceFmt(
                        R.string.onboarding_nest_provisioning_bom_line_domain,
                        localized(domain.label) ?: "",
                        formatPrice(domain.priceCents, domain.currency),
                        formatPrice(renewal, domain.currency),
                    )
                } else {
                    stringResourceFmt(
                        R.string.onboarding_nest_provisioning_bom_line,
                        localized(domain.label) ?: "",
                        formatPrice(domain.priceCents, domain.currency),
                    )
                },
                modifier = Modifier.testTag(Ids.PROVISIONING_BOM_DOMAIN_LINE),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
        items.firstOrNull { it.recurring }?.let { vps ->
            Text(
                stringResourceFmt(
                    R.string.onboarding_nest_provisioning_bom_line_recurring,
                    localized(vps.label) ?: "",
                    formatPrice(vps.priceCents, vps.currency),
                ),
                modifier = Modifier.testTag(Ids.PROVISIONING_BOM_VPS_LINE),
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

@Composable
private fun StepRow(index: Int, step: StepSnapshot) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .testTag("provisioning-step-row[$index]"),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            modifier = Modifier
                .size(24.dp)
                .testTag(Ids.PROVISIONING_STEP_CHECKBOX),
            contentAlignment = Alignment.Center,
        ) {
            when (step.status) {
                StepStatus.PENDING -> {} // empty box
                StepStatus.RUNNING -> CircularProgressIndicator(
                    modifier = Modifier.size(18.dp),
                    strokeWidth = 2.dp,
                )
                StepStatus.SKIPPED -> Text("—")
                StepStatus.SUCCEEDED -> Icon(
                    Icons.Default.Check,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.primary,
                )
                StepStatus.FAILED -> Icon(
                    Icons.Default.Close,
                    contentDescription = null,
                    tint = MaterialTheme.colorScheme.error,
                )
            }
        }
        Spacer(Modifier.width(8.dp))
        Column(modifier = Modifier.weight(1f)) {
            Text(
                localized(provisioningStepLabel(step.kind)) ?: "",
                modifier = Modifier.testTag(Ids.PROVISIONING_STEP_LABEL),
                style = MaterialTheme.typography.bodyLarge,
            )
            // Visibility + attempt-suffix gate on the shared `shows_*` projection
            // (StepSnapshot::recompute_display in libs/fauna-provisioning), never a
            // re-derived rule. This block was the worst drift: it hid the substep
            // row on Skipped/Failed (only `status == RUNNING` showed it) and
            // surfaced a stale `lastError` on a Running step mid-retry.
            if (step.showsSubstep) {
                // cause = the step's lastError — the shared fn fills {cause} for
                // STATUS_RETRYING (the old local mapping passed an empty string).
                val base = step.substep?.let {
                    localized(provisioningSubstepLabel(it, step.lastError))
                } ?: ""
                val text = if (step.showsAttemptSuffix) {
                    (base + stringResourceFmt(
                        R.string.onboarding_provision_step_attempt_template,
                        step.attempt,
                        step.maxAttempts,
                    )).trim()
                } else base
                Text(
                    text,
                    modifier = Modifier.testTag(Ids.PROVISIONING_SUBSTEP),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (step.showsError) {
                Text(
                    step.lastError ?: "",
                    modifier = Modifier.testTag(Ids.PROVISIONING_STEP_ERROR),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
        }
    }
}

// The per-app `stepLabel` / `substepLabel` `when` mappings were lifted into
// shared `fauna_provisioning::progress` (value-formatting.md § Provisioning step
// display); android now consumes `provisioningStepLabel` / `provisioningSubstepLabel`
// (resolved through `localized`) above. The status column keeps android's Material
// icons (Check / Close / spinner / "—") rather than the shared text glyph
// `provisioning_status_glyph` — a deliberate divergence (no text glyph to consume).
