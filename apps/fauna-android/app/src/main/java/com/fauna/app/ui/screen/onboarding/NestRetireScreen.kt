package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ActorScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.E2eProviderOverride
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SessionAccount
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.components.rememberCopyToClipboard
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.resolveLocalizedNested
import com.fauna.ffi.onboarding.FieldMetaPlain
import com.fauna.ffi.onboarding.FieldTypePlain
import com.fauna.ffi.onboarding.LeftoverKind
import com.fauna.ffi.onboarding.ManagedServerRow
import com.fauna.ffi.onboarding.NestRetireMachine
import com.fauna.ffi.onboarding.RetirePhase
import com.fauna.ffi.onboarding.RetireSnapshot
import com.fauna.ffi.onboarding.StepState
import com.fauna.ffi.onboarding.TransferCodeState
import com.fauna.ffi.onboarding.retireStepStatus
import com.fauna.ffi.provisioning.StepStatus
import com.fauna.ffi.retireHeldInputs
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import social.fauna.generated.Capability
import social.fauna.generated.Ids
import social.fauna.generated.PROVIDERS
import java.net.Inet4Address
import java.net.InetAddress
import java.net.URI
import javax.inject.Inject

/** Which surface opened the page (`nest-retirement.md` § Layout & flow). */
enum class RetireEntry { Launch, Admin }

/** How long the live input reads may hold verify (the tui's bound). */
private const val INPUT_READ_BOUND_MS = 15_000L

/** How long resolving the saved nest's host for the *current* badge may take. */
private const val RESOLVE_BOUND_MS = 5_000L

/** Repaint cadence while a machine call (verify / the run) is in flight. */
private const val IN_FLIGHT_REPAINT_MS = 200L

/**
 * Hosts the `nest_retire` page over the shared [NestRetireMachine]
 * (`docs/goal/behavior/nest-retirement.md` § Where logic lives): this view-model
 * only builds the machine from what the app knows, forwards gestures and
 * repaints on a tick — no page logic and no provider branch (priority #2). The
 * lift of `apps/fauna-tui/src/wizard/nest_retire.rs`.
 *
 * One machine per page visit: [open] builds it, [finish] drops it. The VPS
 * token typed into it is never persisted.
 */
@HiltViewModel
class NestRetireVM @Inject constructor(
    private val api: ApiClient,
    private val host: OnboardingHost,
    private val sessionAccount: SessionAccount,
    private val actorScope: ActorScope,
) : ViewModel() {
    private val _tick = MutableStateFlow(0L)

    /** Bumped after every machine call; the screen re-reads the snapshot on it. */
    val tick: StateFlow<Long> = _tick.asStateFlow()

    var machine: NestRetireMachine? = null
        private set
    var entry: RetireEntry = RetireEntry.Launch
        private set
    private var openedVisit: String? = null

    /** The typed credential fields as painted (the machine holds them secret). */
    val creds = mutableStateMapOf<String, String>()
    var confirmTyped by mutableStateOf("")
        private set

    private fun bump() {
        _tick.value = _tick.value + 1
    }

    /**
     * Open the page over whatever the entry showed. The machine takes the e2e
     * provider base-URL override at construction, the saved nest's host as an
     * attribution candidate, and — once resolved — that host's IPv4 for the
     * *current* badge. The rest of what the app knows lands asynchronously
     * ([readInputs]): every held DNS credential and, from `admin-nest`, the
     * nest's active local domains (§ Credential stance (b), § DNS cleanup →
     * *Several domains* (1)).
     */
    fun open(entry: RetireEntry, visit: String) {
        // A recomposition after a configuration change reopens with the SAME
        // visit key: keep the machine (and the token typed into it).
        if (visit == openedVisit && machine != null) return
        finish()
        openedVisit = visit
        this.entry = entry
        creds.clear()
        confirmTyped = ""
        val nestUrl = sessionAccount.nestUrl?.takeIf { it.isNotBlank() } ?: host.machine.nestUrl()
        val uri = runCatching { URI(nestUrl) }.getOrNull()
        val nestHost = uri?.host
        val candidates = listOfNotNull(nestHost?.takeIf { !isIpLiteral(it) })
        val m = NestRetireMachine(candidates, null, emptyList(), emptyList(), E2eProviderOverride.vps(), E2eProviderOverride.doh())
        machine = m
        // Verify is where the machine reads the inputs, so it waits for them.
        m.expectAppInputs()
        viewModelScope.launch { readInputs(m, entry) }
        if (nestHost != null) {
            val port = uri.port.takeIf { it > 0 } ?: if (uri.scheme == "http") 80 else 443
            viewModelScope.launch { resolveCurrent(m, nestHost, port) }
        }
        bump()
    }

    private suspend fun readInputs(m: NestRetireMachine, entry: RetireEntry) {
        if (entry == RetireEntry.Admin) {
            withTimeoutOrNull(INPUT_READ_BOUND_MS) { api.retireLocalDomains() }
                ?.let { m.addCandidateDomains(it) }
        }
        // The shared read: `fauna.state.dns` through the account runtime, bounded
        // in Rust. The admin entry waits for a runtime still assembling; the
        // launch entry (no session) reads only one that is already up.
        val held = retireHeldInputs(waitForRuntime = entry == RetireEntry.Admin)
        m.setHeldDnsJson(held.heldDnsProviderIds, held.heldDnsCredsJson)
        m.setRememberedSlotsJson(
            held.rememberedDkimJson,
            held.rememberedAtprotoJson,
            held.rememberedFaunaSelfJson,
        )
        // A first-listing refusal is not "the account holds none": say why.
        held.notReadyReason?.let { m.setInputError(it) }
        m.appInputsLanded()
        bump()
    }

    private suspend fun resolveCurrent(m: NestRetireMachine, nestHost: String, port: Int) {
        val ipv4 = withTimeoutOrNull(RESOLVE_BOUND_MS) {
            withContext(Dispatchers.IO) {
                runCatching {
                    InetAddress.getAllByName(nestHost).filterIsInstance<Inet4Address>()
                        .firstOrNull()?.hostAddress
                }.getOrNull()
            }
        }
        if (ipv4 != null) {
            m.setCurrentIpv4(ipv4)
            bump()
        }
    }

    private fun isIpLiteral(host: String): Boolean =
        host.contains(':') || host.all { it.isDigit() || it == '.' }

    /** Run a machine call, repainting while it is in flight (the run's step rows move). */
    private fun runOp(op: suspend (NestRetireMachine) -> Unit) {
        val m = machine ?: return
        viewModelScope.launch {
            val repaint = launch {
                while (true) {
                    delay(IN_FLIGHT_REPAINT_MS)
                    bump()
                }
            }
            try {
                op(m)
            } catch (e: Exception) {
                // Failures land in the snapshot (`error` / a failed step row).
            } finally {
                repaint.cancel()
                bump()
            }
        }
    }

    // -- credentials -------------------------------------------------------

    fun selectProvider(id: String) {
        val m = machine ?: return
        if (m.selectedProvider() != id) creds.clear()
        m.selectProvider(id)
        bump()
    }

    fun setCred(field: String, value: String) {
        machine?.setCredentialField(field, value)
        creds[field] = value
        bump()
    }

    fun hostedAuth(fieldId: String, open: (String) -> Unit) = runOp { m ->
        val prompt = m.hostedAuthBegin(fieldId) ?: return@runOp
        open(prompt.verificationUrl)
        m.hostedAuthWait(fieldId)
    }

    fun verify() = runOp { it.verify() }

    // -- list / confirm ----------------------------------------------------

    fun select(serverId: String) {
        machine?.select(serverId)
        bump()
    }

    fun fetchTransferCode() = runOp { it.fetchTransferCode() }

    fun beginConfirm() {
        confirmTyped = ""
        machine?.beginConfirm()
        bump()
    }

    fun setConfirmName(typed: String) {
        machine?.setConfirmName(typed)
        confirmTyped = typed
        bump()
    }

    fun cancelConfirm() {
        confirmTyped = ""
        machine?.back()
        bump()
    }

    fun confirm() = runOp { it.confirm() }

    fun retry() = runOp { it.retry() }

    fun forceServer() = runOp { it.forceServer() }

    /**
     * Back, as tui settled it: from credentials — and from a run stopped on a
     * failure (disarming it) — leave the page; from list / confirm one state
     * back; never while a step is in flight. Returns whether the page is left.
     */
    fun back(): Boolean {
        val m = machine ?: return true
        return when (m.snapshot().phase) {
            RetirePhase.CREDENTIALS, RetirePhase.RUNNING, RetirePhase.DONE -> true
            RetirePhase.LIST, RetirePhase.CONFIRM -> {
                confirmTyped = ""
                m.back()
                bump()
                false
            }
            RetirePhase.LISTING -> false
        }
    }

    /**
     * Close the page: drop the token and everything fetched with it. After
     * retiring this session's own box from `admin-nest` the session this
     * device holds is for a nest that no longer exists, so it is dropped
     * (credentials kept) and the caller re-enters launch routing.
     * Returns whether it did.
     */
    fun leave(): Boolean {
        val retiredCurrent = machine?.snapshot()?.retired?.current == true
        finish()
        if (retiredCurrent && entry == RetireEntry.Admin) {
            actorScope.dropActorScopedState()
            return true
        }
        return false
    }

    private fun finish() {
        machine?.cancel()
        machine = null
        openedVisit = null
        creds.clear()
        confirmTyped = ""
    }

    override fun onCleared() {
        finish()
    }
}

/**
 * The `nest_retire` page. [onExit] is told whether the session was dropped
 * (the current box was retired from `admin-nest`): the host then re-enters
 * launch routing; otherwise it simply shows the entry again.
 */
@Composable
fun NestRetireScreen(
    entry: RetireEntry,
    onExit: (sessionDropped: Boolean) -> Unit,
    vm: NestRetireVM = hiltViewModel(),
) {
    // One key per page visit, surviving rotation (the typed token must not reset
    // with the screen) but not leaving the composition.
    val visit = rememberSaveable { java.util.UUID.randomUUID().toString() }
    LaunchedEffect(entry, visit) { vm.open(entry, visit) }
    val tick by vm.tick.collectAsState()
    // Read `tick` BEFORE the early return: the first composition runs ahead of
    // `open`, and only a state read it made can be recomposed by `open`'s bump.
    val m = remember(tick) { vm.machine } ?: return
    val snap = remember(tick, m) { m.snapshot() }
    val exit = { onExit(vm.leave()) }
    val onBack = { if (vm.back()) exit() }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp)
            .verticalScroll(rememberScrollState()),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(stringResource(R.string.onboarding_retire_title), style = MaterialTheme.typography.headlineSmall)
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_retire_subtitle),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        when (snap.phase) {
            RetirePhase.CREDENTIALS, RetirePhase.LISTING -> CredentialsState(vm, m, snap, tick, onBack)
            RetirePhase.LIST -> ListState(vm, m, snap, onBack)
            RetirePhase.CONFIRM -> ConfirmState(vm, m, snap)
            RetirePhase.RUNNING -> RunningState(vm, m, snap, onBack)
            RetirePhase.DONE -> DoneState(m, snap, exit)
        }

        val error = snap.error
        if (!error.isNullOrEmpty()) {
            Spacer(Modifier.height(8.dp))
            Text(
                error,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ERROR_MESSAGE),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

@Composable
private fun BackButton(onBack: () -> Unit) {
    Spacer(Modifier.height(16.dp))
    OutlinedButton(
        onClick = onBack,
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_BACK_BUTTON),
    ) { Text(stringResource(R.string.common_back)) }
}

/**
 * The `vps_config` provider row, credential form and verify button, by the same
 * ids — no location or server-type controls — then Back.
 */
@Composable
private fun CredentialsState(
    vm: NestRetireVM,
    m: NestRetireMachine,
    snap: RetireSnapshot,
    tick: Long,
    onBack: () -> Unit,
) {
    val ctx = LocalContext.current
    // Every VPS-capable provider — a box bought through a provider no longer on
    // the curated offer list must still be retirable.
    val providers = remember { PROVIDERS.filter { it.capabilities.contains(Capability.VPS) } }
    val selectedId = remember(tick) { m.selectedProvider() }
    val selected = providers.find { it.id == selectedId }
    val listing = snap.phase == RetirePhase.LISTING

    Row(
        modifier = Modifier.fillMaxWidth().testTag(Ids.VPS_PROVIDER_ROW),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        providers.forEach { provider ->
            FilledTonalButton(
                onClick = { vm.selectProvider(provider.id) },
                modifier = Modifier.testTag("vps-provider-row[${provider.id}]"),
                colors = if (provider.id == selectedId) {
                    ButtonDefaults.filledTonalButtonColors(containerColor = MaterialTheme.colorScheme.primaryContainer)
                } else ButtonDefaults.filledTonalButtonColors(),
            ) { Text(provider.id.replaceFirstChar { it.uppercase() }) }
        }
    }

    if (selected != null) {
        Spacer(Modifier.height(16.dp))
        ProviderHelpRow(selected)
        Spacer(Modifier.height(8.dp))
        val fields: List<FieldMetaPlain> = remember(tick) { m.visibleFields() }
        val hostedFields = fields.filter { it.fieldType == FieldTypePlain.HOSTED_AUTH }
        CredentialsForm(
            fields = fields,
            creds = vm.creds,
            tag = "vps-credentials-form",
            enabled = !listing,
            onChange = { id, v -> vm.setCred(id, v) },
            hostedAuthLabels = hostedFields.associate { it.id to m.hostedAuthButtonText(it.id) },
            hostedAuthEnabled = hostedFields.associate { it.id to m.hostedAuthCanBegin(it.id) },
            onHostedAuthClick = { fieldId -> vm.hostedAuth(fieldId) { url -> UrlOpener.open(ctx, url) } },
        )
        Spacer(Modifier.height(8.dp))
        Button(
            onClick = { vm.verify() },
            enabled = m.canVerify() && !listing,
            modifier = Modifier.testTag(Ids.VPS_VERIFY_BUTTON),
        ) {
            if (listing) {
                CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
            } else Text(stringResource(R.string.provisioning_verify_credentials))
        }
    }
    BackButton(onBack)
}

/**
 * One row per listed server, its members scoped inside `retire-server-item[n]`
 * (the scoped-descendant idiom, e2e convention 1). The row is selectable
 * through plain semantics rather than `clickable`, which would merge its
 * members into one node and hide them from a scoped lookup.
 */
@Composable
private fun ListState(vm: NestRetireVM, m: NestRetireMachine, snap: RetireSnapshot, onBack: () -> Unit) {
    val copy = rememberCopyToClipboard()
    val ctx = LocalContext.current
    if (snap.servers.isEmpty()) {
        Text(
            stringResource(R.string.onboarding_retire_empty_message),
            modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_SERVER_EMPTY_MESSAGE),
            style = MaterialTheme.typography.bodyMedium,
        )
    } else {
        Text(stringResource(R.string.onboarding_retire_list_label), style = MaterialTheme.typography.titleSmall)
    }
    Column(modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_SERVER_LIST)) {
        snap.servers.forEachIndexed { i, row ->
            val isSelected = snap.selected == row.serverId
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(vertical = 4.dp)
                    .testTag("retire-server-item[$i]")
                    .semantics {
                        selected = isSelected
                        onClick { vm.select(row.serverId); true }
                    }
                    .pointerInput(row.serverId) { detectTapGestures { vm.select(row.serverId) } },
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Checkbox(checked = isSelected, onCheckedChange = null)
                    Text(
                        row.name,
                        modifier = Modifier.padding(start = 8.dp).testTag(Ids.RETIRE_SERVER_NAME),
                        style = MaterialTheme.typography.titleSmall,
                    )
                }
                ServerFacts(row)
                if (isSelected) {
                    TransferCode(vm, m, row, copy, ctx)
                }
            }
        }
    }
    Spacer(Modifier.height(8.dp))
    Button(
        onClick = { vm.beginConfirm() },
        enabled = snap.selected != null,
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_DELETE_BUTTON),
    ) { Text(stringResource(R.string.onboarding_retire_delete_button)) }
    BackButton(onBack)
}

@Composable
private fun ServerFacts(row: ManagedServerRow) {
    Column(modifier = Modifier.padding(start = 48.dp)) {
        row.domain?.let {
            Fact(Ids.RETIRE_SERVER_DOMAIN, stringResource(R.string.onboarding_retire_domain_label), it)
        }
        if (row.secondaryDomains.isNotEmpty()) {
            Fact(
                Ids.RETIRE_SERVER_SECONDARY_DOMAINS,
                stringResource(R.string.onboarding_retire_secondary_domains_label),
                row.secondaryDomains.joinToString(", "),
            )
        }
        row.ipv4?.let {
            Fact(Ids.RETIRE_SERVER_ADDRESS, stringResource(R.string.onboarding_retire_address_label), it)
        }
        if (row.current) {
            Text(
                stringResource(R.string.onboarding_retire_current_badge),
                modifier = Modifier.testTag(Ids.RETIRE_SERVER_CURRENT_BADGE),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.primary,
            )
        }
        if (!row.marked) {
            Text(
                stringResource(R.string.onboarding_retire_unmarked_note),
                modifier = Modifier.testTag(Ids.RETIRE_SERVER_UNMARKED_NOTE),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
            )
        }
    }
}

@Composable
private fun Fact(tag: String, label: String, value: String) {
    Row {
        Text("$label: ", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value, modifier = Modifier.testTag(tag), style = MaterialTheme.typography.bodySmall)
    }
}

/** The transfer-code affordance on the selected row — independent of deleting. */
@Composable
private fun TransferCode(
    vm: NestRetireVM,
    m: NestRetireMachine,
    row: ManagedServerRow,
    copy: (String) -> Unit,
    ctx: android.content.Context,
) {
    Column(modifier = Modifier.padding(start = 48.dp, top = 4.dp)) {
        when (val state = row.transferCode) {
            is TransferCodeState.Idle -> OutlinedButton(
                onClick = { vm.fetchTransferCode() },
                modifier = Modifier.testTag(Ids.RETIRE_TRANSFER_CODE_BUTTON),
            ) { Text(stringResource(R.string.onboarding_retire_transfer_code_button)) }
            is TransferCodeState.Code -> {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        stringResource(R.string.onboarding_retire_transfer_code_label) + ": ",
                        style = MaterialTheme.typography.bodySmall,
                    )
                    Text(
                        state.code,
                        modifier = Modifier.testTag(Ids.RETIRE_TRANSFER_CODE_VALUE),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
                OutlinedButton(
                    onClick = { copy(state.code) },
                    modifier = Modifier.testTag(Ids.RETIRE_TRANSFER_CODE_COPY_BUTTON),
                ) { Text(stringResource(R.string.onboarding_retire_transfer_code_copy)) }
            }
            else -> {}
        }
        resolveLocalized(ctx, m.transferCodeNote(row.serverId))?.let {
            Text(
                it,
                modifier = Modifier.testTag(Ids.RETIRE_TRANSFER_CODE_STATUS),
                style = MaterialTheme.typography.bodySmall,
            )
        }
    }
}

/** The typed-name gate (`nest-retirement.md` § Confirm shape). */
@Composable
private fun ConfirmState(vm: NestRetireVM, m: NestRetireMachine, snap: RetireSnapshot) {
    val ctx = LocalContext.current
    // The summary's provider rides as its display KEY, so it is resolved nested.
    val summary = m.confirmSummary().mapNotNull { resolveLocalizedNested(ctx, it) }
    Text(
        summary.joinToString("\n"),
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_CONFIRM_SUMMARY),
        style = MaterialTheme.typography.bodyMedium,
    )
    Spacer(Modifier.height(16.dp))
    OutlinedTextField(
        value = vm.confirmTyped,
        onValueChange = { vm.setConfirmName(it) },
        label = { Text(stringResource(R.string.onboarding_retire_confirm_name_label)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_CONFIRM_NAME_INPUT),
    )
    Spacer(Modifier.height(8.dp))
    Button(
        onClick = { vm.confirm() },
        enabled = snap.confirmEnabled,
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_CONFIRM_BUTTON),
    ) { Text(stringResource(R.string.onboarding_retire_confirm_button)) }
    Spacer(Modifier.height(8.dp))
    OutlinedButton(
        onClick = { vm.cancelConfirm() },
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_CANCEL_BUTTON),
    ) { Text(stringResource(R.string.onboarding_retire_cancel_button)) }
}

/**
 * The two step rows, reusing the `provisioning-progress` row's ids and the
 * Material glyphs `NestProvisioningScreen` paints for the shared status.
 */
@Composable
private fun StepRows(m: NestRetireMachine, snap: RetireSnapshot) {
    val ctx = LocalContext.current
    snap.steps.forEachIndexed { index, step ->
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(vertical = 6.dp)
                .testTag("provisioning-step-row[$index]"),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Box(
                modifier = Modifier.size(24.dp).testTag(Ids.PROVISIONING_STEP_CHECKBOX),
                contentAlignment = Alignment.Center,
            ) {
                when (retireStepStatus(step.state)) {
                    StepStatus.PENDING -> {}
                    StepStatus.RUNNING -> CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
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
                    resolveLocalized(ctx, m.stepLabel(step.step)) ?: "",
                    modifier = Modifier.testTag(Ids.PROVISIONING_STEP_LABEL),
                    style = MaterialTheme.typography.bodyLarge,
                )
                resolveLocalized(ctx, m.stepNote(step.state))?.let {
                    Text(
                        it,
                        modifier = Modifier.testTag(Ids.PROVISIONING_SUBSTEP),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                (step.state as? StepState.Failed)?.let {
                    Text(
                        it.cause,
                        modifier = Modifier.testTag(Ids.PROVISIONING_STEP_ERROR),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            }
        }
    }
}

@Composable
private fun RunningState(vm: NestRetireVM, m: NestRetireMachine, snap: RetireSnapshot, onBack: () -> Unit) {
    StepRows(m, snap)
    val failed = snap.steps.any { it.state is StepState.Failed }
    if (failed) {
        Spacer(Modifier.height(8.dp))
        Button(
            onClick = { vm.retry() },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_RETRY_BUTTON),
        ) { Text(stringResource(R.string.onboarding_retire_retry_button)) }
    }
    if (snap.forceServerOffered) {
        // The dangling-record warning, repeated where the choice is made.
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_retire_force_server_warning),
            modifier = Modifier.testTag(Ids.RETIRE_FORCE_SERVER_WARNING),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
        )
        OutlinedButton(
            onClick = { vm.forceServer() },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_FORCE_SERVER_BUTTON),
        ) { Text(stringResource(R.string.onboarding_retire_force_server_button)) }
    }
    // Leaving is offered only once the run has stopped on a failure — never
    // while a step is still in flight.
    if (failed) BackButton(onBack)
}

@Composable
private fun DoneState(m: NestRetireMachine, snap: RetireSnapshot, onDone: () -> Unit) {
    val ctx = LocalContext.current
    val copy = rememberCopyToClipboard()
    snap.retired?.let {
        Text(
            stringResource(R.string.onboarding_retire_done_deleted).replace("{name}", it.name),
            style = MaterialTheme.typography.titleSmall,
        )
    }
    StepRows(m, snap)
    val leftover = m.leftoverLines().mapNotNull { resolveLocalized(ctx, it) }.joinToString("\n")
    Spacer(Modifier.height(8.dp))
    Text(
        leftover,
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_DNS_LEFTOVER_TEXT),
        style = MaterialTheme.typography.bodyMedium,
    )
    OutlinedButton(
        onClick = { copy(leftover) },
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_DNS_LEFTOVER_COPY_BUTTON),
    ) { Text(stringResource(R.string.onboarding_retire_leftover_copy)) }
    Spacer(Modifier.height(8.dp))
    Button(
        onClick = onDone,
        modifier = Modifier.fillMaxWidth().testTag(Ids.RETIRE_DONE_BUTTON),
    ) { Text(stringResource(R.string.onboarding_retire_done_button)) }
}
