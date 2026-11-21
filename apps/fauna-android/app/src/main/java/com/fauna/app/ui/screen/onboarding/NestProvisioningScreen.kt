package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
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
import com.fauna.app.core.downloadsDir
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.shareFileBytes
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.ffi.provisioningStepLabel
import com.fauna.ffi.provisioningSubstepLabel
import com.fauna.ffi.onboarding.BillOfMaterialsItem
import com.fauna.ffi.onboarding.DnsPlanSnapshot
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
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.OutlinedButton

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

    /** `dns-price-confirm-checkbox` on a run entered from the choose-providers
     *  page (onboarding-provisioning.md § 3d): the price is agreed here, at the
     *  purchase. */
    fun confirmPrice() = host.machine.confirmPrice()

    /** `provisioning-back-button`: the machine's Back — to the choose-providers
     *  page on its path, to `vps_config` on cloud recovery and the old path. */
    fun back() = host.machine.back()

    /** `provisioning-ssh-door-save-key-button`: the door's private key, handed
     * over ONCE (a phone has no `~/.ssh`) — the machine drops its copy. */
    fun takeSshDoorPrivateKey(): String? = host.machine.takeSshDoorPrivateKey()
}

@Composable
fun NestProvisioningScreen(
    navController: NavController,
    onWizardExit: (WizardOutcome) -> Unit,
    vm: NestProvisioningVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val snap = remember(tick) { vm.host.machine.provisioningSnapshot() }
    // The wizard-exit Continue paints from the machine's one answer
    // (`ui/README.md` § Offer only what is possible, rule 6) — enabled while
    // possible, the blocked reason beside it (rule 5), never this screen's own
    // `overall == SUCCEEDED` test.
    val continuePossibility = remember(tick) { vm.host.machine.provisioningContinuePossibility() }
    val coroutineScope = rememberCoroutineScope()
    // The choose-providers path (onboarding-provisioning.md § 3d *The choices
    // on the setting-things-up page*): the choices region and the price
    // confirm show while the run has not started, and *Buy and set up* is dead
    // while the machine names a reason — every one of them the machine's.
    val choicesVisible = remember(tick) { vm.host.machine.provisioningChoicesVisible() }
    val priceConfirmVisible = remember(tick) { vm.host.machine.provisioningPriceConfirmVisible() }
    val dnsCfg = remember(tick) { vm.host.machine.dnsConfig() }
    // *Buy and set up*'s answer; the choose-providers routing guard is the
    // machine's, folded into it (`ui/README.md` § Offer only what is
    // possible, rule 6).
    val startAnswer = remember(tick) { vm.host.machine.provisioningStartPossibility() }
    // The SSH door after a door run (onboarding-provisioning.md § 5): the
    // command, and the key file handed over once through the share sheet.
    val sshDoor = remember(tick) { vm.host.machine.sshDoorReadout() }
    var sshDoorKeySaved by remember { mutableStateOf(false) }
    var sshDoorSaveError by remember { mutableStateOf<String?>(null) }
    val context = LocalContext.current
    // The share sheet reads the key from the app's download cache, so the file
    // goes when the page does: the app keeps no copy of the door key
    // (`installers/vps.md` § The SSH door → *The key*).
    DisposableEffect(sshDoor?.domain) {
        val domain = sshDoor?.domain
        onDispose {
            if (domain != null) java.io.File(context.downloadsDir(), "fauna-$domain").delete()
        }
    }

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
        modifier = Modifier.fillMaxSize().padding(24.dp).verticalScroll(rememberScrollState()),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_nest_provisioning_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(8.dp))

        ProvisioningPriceBom(remember(tick) { vm.host.machine.billOfMaterials() }) {
            // Directly under the domain line, § 4's label unchanged: it says
            // registration begins on *Buy and set up*, and this is that page.
            if (priceConfirmVisible) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(
                        checked = dnsCfg.priceAgreed,
                        onCheckedChange = { if (it && !dnsCfg.priceAgreed) vm.confirmPrice() },
                        enabled = !dnsCfg.priceAgreed,
                        modifier = Modifier.testTag(Ids.DNS_PRICE_CONFIRM_CHECKBOX),
                    )
                    Text(stringResource(R.string.registrar_price_confirm))
                }
            }
        }
        // The DNS plan — every record the run will keep, add, change or
        // remove at the domain (onboarding-provisioning.md § 6 *The DNS plan*),
        // rows and sentences from the shared machine; nothing here decides an
        // action. Mirrors linux's nest_provisioning.rs dns_plan_box.
        ProvisioningDnsPlan(remember(tick) { vm.host.machine.dnsPlan() })
        // The choices region — location, server type, mail mode, update
        // channel, SSH door — between the price summary and the start, every
        // one with a default; the same composable `vps_config` paints.
        if (choicesVisible) {
            ProvisioningChoices(vm.host.machine, serverControls = true, tick = tick)
        }
        Spacer(Modifier.height(8.dp))

        // By-continuing notice: below the price recap, before the charge
        // (Buy and set up is the acceptance gesture; onboarding-provisioning.md §6).
        TermsNotice()

        // Top region: start CTA before running, elapsed timer once started.
        when (snap.overall) {
            OverallStatus.IDLE -> {
                Button(
                    onClick = { vm.start() },
                    enabled = startAnswer.isPossible,
                    modifier = Modifier.testTag(Ids.PROVISIONING_START_BUTTON),
                ) { Text(stringResource(R.string.onboarding_nest_provisioning_start_button)) }
                // The reason on an un-id'd line beneath the button (rule 5).
                DisabledControlReasonText(localized(startAnswer.blockedReason))
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

        if (snap.overall == OverallStatus.SUCCEEDED && sshDoor != null) {
            Spacer(Modifier.height(8.dp))
            Text(
                stringResourceFmt(
                    R.string.onboarding_nest_provisioning_ssh_door_command_no_path_template,
                    sshDoor.command,
                    sshDoor.admittedFrom
                        ?: stringResource(R.string.onboarding_nest_provisioning_ssh_door_address_unknown),
                    sshDoor.fingerprint,
                ),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.PROVISIONING_SSH_DOOR_COMMAND),
            )
            if (!sshDoorKeySaved) {
                OutlinedButton(
                    onClick = {
                        val key = vm.takeSshDoorPrivateKey() ?: return@OutlinedButton
                        sshDoorKeySaved = true
                        sshDoorSaveError = runCatching {
                            context.shareFileBytes(
                                "fauna-${sshDoor.domain}",
                                key.toByteArray(Charsets.UTF_8),
                                mimeType = "application/x-pem-file",
                            )
                        }.exceptionOrNull()?.message
                    },
                    modifier = Modifier.testTag(Ids.PROVISIONING_SSH_DOOR_SAVE_KEY_BUTTON),
                ) { Text(stringResource(R.string.onboarding_nest_provisioning_ssh_door_save_key_button)) }
            }
        }

        (snap.finalError ?: sshDoorSaveError)?.let {
            Spacer(Modifier.height(8.dp))
            Text(
                it,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
            )
        }

        Spacer(Modifier.height(24.dp))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = {
                    vm.back()
                    navController.popBackStack()
                },
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
                        enabled = continuePossibility.isPossible,
                        modifier = Modifier.testTag(Ids.PROVISIONING_CONTINUE_BUTTON),
                    ) { Text(stringResource(R.string.common_continue)) }
                    DisabledControlReasonText(localized(continuePossibility.blockedReason))
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
private fun ProvisioningPriceBom(
    items: List<BillOfMaterialsItem>,
    underDomainLine: @Composable () -> Unit = {},
) {
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
            underDomainLine()
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

/**
 * The page's DNS plan (`provisioning-dns-plan`, onboarding-provisioning.md
 * § 6 *The DNS plan*): nothing while the shared snapshot is not `visible` (the
 * manual path, a bought domain's fresh zone); otherwise its heading — or the
 * reading / read-failed line — and one row per record keyed `<name>|<type>`.
 * Every sentence is resolved from the snapshot's `LocalizedText`; the text is
 * read, never acted on, so it is selectable (`ui/README.md` § Text the person
 * reads can be copied). Compose recomposes only when the snapshot changed, so
 * a selection survives the ticks that leave it alone.
 */
@Composable
internal fun ProvisioningDnsPlan(plan: DnsPlanSnapshot) {
    if (!plan.visible) return
    SelectionContainer(Modifier.testTag(Ids.PROVISIONING_DNS_PLAN)) {
        Column(
            modifier = Modifier.padding(vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(2.dp),
        ) {
            Text(localized(plan.heading) ?: "", style = MaterialTheme.typography.titleSmall)
            plan.rows.forEach { row ->
                Text(
                    localized(row.text) ?: "",
                    modifier = Modifier.testTag("${Ids.PROVISIONING_DNS_PLAN_ROW}[${row.key}]"),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
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
            // Visibility gates on the shared `shows_*` projection
            // (StepSnapshot::recompute_display in libs/fauna-provisioning), never a
            // re-derived rule. This block was the worst drift: it hid the substep
            // row on Skipped/Failed (only `status == RUNNING` showed it) and
            // surfaced a stale `lastError` on a Running step mid-retry.
            if (step.showsSubstep) {
                // cause = the step's lastError — the shared fn fills {cause} for
                // STATUS_RETRYING (the old local mapping passed an empty string).
                // No attempt count is ever shown (onboarding-provisioning.md § 6).
                val text = step.substep?.let {
                    localized(provisioningSubstepLabel(it, step.lastError))
                } ?: ""
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
