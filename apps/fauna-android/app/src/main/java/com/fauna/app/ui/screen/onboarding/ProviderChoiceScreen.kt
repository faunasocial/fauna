package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.UrlOpener
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.util.AutomationAttributePack
import com.fauna.app.ui.util.getStringFmt
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.onboarding.CapabilityPlain
import com.fauna.ffi.onboarding.FieldTypePlain
import com.fauna.ffi.onboarding.GoalCover
import com.fauna.ffi.onboarding.GoalRow
import com.fauna.ffi.onboarding.ProviderGoal
import com.fauna.ffi.onboarding.ProviderRow
import com.fauna.ffi.onboarding.formatPrice
import com.fauna.ffi.onboarding.handleTld
import com.fauna.ffi.provisioning.ContactInfo
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.launch
import social.fauna.generated.Ids
import social.fauna.generated.PROVIDERS
import uniffi.fauna_core.LocalizedText
import javax.inject.Inject

/** The manual row's key in `provider-row[...]` and `provider-goal-select[dns]`. */
private const val MANUAL = "manual"

@HiltViewModel
class ProviderChoiceVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun toggleBuy(on: Boolean) = host.machine.toggleBuyDomain(on)
    fun setCred(provider: String, field: String, value: String) =
        host.machine.setProviderCred(provider, field, value)
    fun connect(provider: String) = viewModelScope.launch {
        // A failed check is on the row (`ProviderRow.error`), painted as the
        // dialog's status line — nothing to re-derive here.
        runCatching { host.machine.connectProvider(provider) }
    }
    fun disconnect(provider: String) = host.machine.disconnectProvider(provider)
    fun selectCover(goal: ProviderGoal, cover: GoalCover) {
        runCatching { host.machine.selectGoalCover(goal, cover) }
    }
    fun setContact(provider: String, contact: ContactInfo) =
        host.machine.setProviderContact(provider, contact)
    /** `true` when the machine moved on (to `nest_provisioning`). */
    fun continueAction(): Boolean = runCatching { host.machine.continueFromProviders() }.isSuccess
    fun back() = host.machine.back()
}

private fun goalKey(goal: ProviderGoal): String = when (goal) {
    ProviderGoal.DOMAIN -> "domain"
    ProviderGoal.DNS -> "dns"
    ProviderGoal.VPS -> "vps"
}

private fun coverKey(cover: GoalCover): String = when (cover) {
    is GoalCover.Provider -> cover.providerId
    is GoalCover.Manual -> MANUAL
}

private fun coverFor(key: String): GoalCover =
    if (key == MANUAL) GoalCover.Manual else GoalCover.Provider(key)

/**
 * Stage 4d: choose providers (`onboarding-provisioning.md` § 3d), lifted from
 * tui's `wizard/provider_choice.rs`. The goal lines at the top
 * (`provider-goal-row[domain|dns|vps]`, with `provider-goal-select[…]` only when
 * more than one connected provider can cover the goal), the buy box and the
 * no-registrar message (moved from `dns_config`), the provider rows
 * `provider-row[<id>|manual]`, the status line and Back / Continue — and, while a
 * row is open, its `provider-dialog`.
 *
 * **Every rule is the machine's** (§ 3d *Where logic lives*): coverage, the
 * assignment, the reasons, the fields a dialog shows, whether Connect and
 * Continue are live. Which dialog is open is the one piece of state this page
 * keeps — presentation, deciding nothing about coverage — and it paints
 * **inline** under its row, not in a `Dialog` window (`ReportHost`'s
 * precedent: a separate window does not inherit the shell's
 * `testTagsAsResourceId`, so its ids would be invisible to the bridge).
 */
@Composable
fun ProviderChoiceScreen(
    navController: NavController,
    vm: ProviderChoiceVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val m = vm.host.machine
    val goals = remember(tick) { m.goalRows() }
    val rows = remember(tick) { m.providerRows() }
    val canOfferBuy = remember(tick) { m.canOfferBuyDomain() }
    val buying = remember(tick) { m.dnsConfig().buyDomain }
    val showNoProvider = remember(tick) { m.shouldShowNoProviderMessage() }
    val tld = remember(tick) { handleTld(handle = m.currentHandle()) ?: "" }
    val domain = remember(tick) { m.providerGoalDomain() }
    val blocked = remember(tick) { m.providersContinueBlockedReason() }
    val ctx = LocalContext.current
    var openDialog by rememberSaveable { mutableStateOf<String?>(null) }

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp).verticalScroll(rememberScrollState()),
    ) {
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_provider_choice_title),
            style = MaterialTheme.typography.headlineSmall,
        )
        Spacer(Modifier.height(4.dp))
        Text(
            stringResource(R.string.onboarding_provider_choice_description),
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(16.dp))

        // ── The goals, in page order ─────────────────────────────────────
        Column(modifier = Modifier.testTag(Ids.PROVIDER_GOAL_ROW)) {
            goals.forEach { row -> GoalLine(row, domain, vm) }
        }

        // ── The buy box, offered only for a domain that can be bought ───
        if (canOfferBuy) {
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = buying,
                    onCheckedChange = { vm.toggleBuy(it) },
                    modifier = Modifier.testTag(Ids.DNS_BUY_DOMAIN_CHECKBOX),
                )
                Text(stringResource(R.string.onboarding_dns_config_buy_domain_checkbox))
            }
        }
        if (showNoProvider) {
            Text(
                ctx.getStringFmt(R.string.onboarding_dns_config_no_provider_carries_tld, tld),
                modifier = Modifier.testTag(Ids.DNS_NO_PROVIDER_MESSAGE),
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        // ── The providers, then the manual row ──────────────────────────
        Spacer(Modifier.height(16.dp))
        Column(modifier = Modifier.testTag(Ids.PROVIDER_ROW)) {
            Text(
                stringResource(R.string.onboarding_provider_choice_providers_heading),
                style = MaterialTheme.typography.titleSmall,
            )
            val dnsCover = goals.firstOrNull { it.goal == ProviderGoal.DNS }?.cover
            rows.forEach { row ->
                ProviderRowItem(
                    row = row,
                    manualChosen = dnsCover is GoalCover.Manual,
                    onOpen = { openDialog = row.providerId },
                    onManual = { vm.selectCover(ProviderGoal.DNS, GoalCover.Manual) },
                )
                // The open dialog paints directly under its own row.
                if (openDialog == row.providerId && row.providerId != MANUAL) {
                    ProviderDialog(row, vm, onClose = { openDialog = null })
                }
            }
        }

        Spacer(Modifier.height(16.dp))
        Text(
            localized(blocked) ?: "",
            modifier = Modifier.testTag(Ids.PROVIDER_CHOICE_STATUS_TEXT),
            style = MaterialTheme.typography.bodyMedium,
        )
        Spacer(Modifier.height(8.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = {
                    vm.back()
                    navController.popBackStack()
                },
                modifier = Modifier.testTag(Ids.PROVIDER_CHOICE_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }
            // Enabled is the machine's answer, the reason on the status line
            // above (`ui/README.md` § Offer only what is possible, rules 5–6).
            Button(
                onClick = {
                    openDialog = null
                    if (vm.continueAction()) navController.navigate(routeForStep(m.step()))
                },
                enabled = blocked == null,
                modifier = Modifier.testTag(Ids.PROVIDER_CHOICE_CONTINUE_BUTTON),
            ) { Text(stringResource(R.string.common_continue)) }
        }
    }
}

/** A provider's display name, or the id for one the registry does not know. */
@Composable
private fun providerName(id: String): String {
    if (id == MANUAL) return stringResource(R.string.onboarding_provider_choice_manual_row)
    val meta = PROVIDERS.firstOrNull { it.id == id } ?: return id
    return localized(LocalizedText(key = meta.displayNameKey, args = emptyMap())) ?: id
}

@Composable
private fun GoalLine(row: GoalRow, domain: String, vm: ProviderChoiceVM) {
    val ctx = LocalContext.current
    val g = goalKey(row.goal)
    val name = when (row.goal) {
        ProviderGoal.DOMAIN -> ctx.getStringFmt(R.string.onboarding_provider_choice_goal_domain, domain)
        ProviderGoal.DNS -> ctx.getStringFmt(R.string.onboarding_provider_choice_goal_dns, domain)
        ProviderGoal.VPS -> stringResource(R.string.onboarding_provider_choice_goal_vps)
    }
    val coverId = row.cover?.let(::coverKey)
    val status = if (coverId != null) {
        ctx.getStringFmt(R.string.onboarding_provider_choice_goal_covered_by, providerName(coverId))
    } else {
        stringResource(R.string.onboarding_provider_choice_goal_open)
    }
    val price = row.priceCents?.let { cents ->
        val cur = row.currency ?: "USD"
        val first = formatPrice(cents, cur)
        row.renewalCents?.let {
            ctx.getStringFmt(R.string.onboarding_provider_choice_goal_price, first, formatPrice(it, cur))
        } ?: ctx.getStringFmt(R.string.onboarding_provider_choice_goal_price_first_year, first)
    }
    Column(modifier = Modifier.padding(vertical = 4.dp)) {
        Text(
            listOfNotNull("$name — $status", price).joinToString(" · "),
            // `state` and the covering `provider`, packed into the node's one
            // string carrier (an empty provider reads as not set).
            modifier = Modifier
                .testTag("provider-goal-row[$g]")
                .semantics {
                    stateDescription = AutomationAttributePack.pack(
                        "state" to if (coverId != null) "covered" else "open",
                        "provider" to (coverId ?: ""),
                    )
                },
            style = MaterialTheme.typography.bodyLarge,
        )
        // The goal's un-id'd note — DNS covered at a provider whose zone the
        // domain does not point at yet (§ 3d).
        localized(row.note)?.let {
            Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        // The choice, only where there is one to make.
        if (row.candidates.size > 1) {
            val options = row.candidates.map { it to providerName(it) }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.onboarding_provider_choice_goal_select),
                    style = MaterialTheme.typography.bodySmall,
                )
                Spacer(Modifier.width(8.dp))
                TokenSelect(
                    testTagValue = "provider-goal-select[$g]",
                    selected = coverId ?: "",
                    options = options,
                    onSelect = { vm.selectCover(row.goal, coverFor(it)) },
                )
            }
        }
    }
}

/** What a capability lets the provider do, in the row's words. `ZONE` is a
 *  field kind only, never a provider capability, so it names nothing. */
@Composable
private fun capabilityLabel(c: CapabilityPlain): String? = when (c) {
    CapabilityPlain.REGISTRAR -> stringResource(R.string.onboarding_provider_choice_capability_registrar)
    CapabilityPlain.DNS -> stringResource(R.string.onboarding_provider_choice_capability_dns)
    CapabilityPlain.VPS -> stringResource(R.string.onboarding_provider_choice_capability_vps)
    else -> null
}

@Composable
private fun ProviderRowItem(
    row: ProviderRow,
    manualChosen: Boolean,
    onOpen: () -> Unit,
    onManual: () -> Unit,
) {
    val ctx = LocalContext.current
    val manual = row.providerId == MANUAL
    val text: String
    val connected: Boolean
    val reason: String?
    if (manual) {
        text = stringResource(R.string.onboarding_provider_choice_manual_row) + " — " +
            stringResource(R.string.onboarding_provider_choice_manual_row_help)
        connected = manualChosen
        // The manual row's server and registration reasons restate what the
        // row is (DNS only); only its DNS reason explains anything.
        reason = resolveLocalized(ctx, row.dnsReason)
    } else {
        val caps = row.capabilities.mapNotNull { capabilityLabel(it) }.joinToString(", ")
        val state = when {
            row.connecting -> stringResource(R.string.onboarding_provider_choice_state_connecting)
            row.connected -> stringResource(R.string.onboarding_provider_choice_state_connected)
            else -> stringResource(R.string.onboarding_provider_choice_state_available)
        }
        text = "${providerName(row.providerId)} — $caps · $state"
        connected = row.connected
        // A reason repeated across goals is said once.
        reason = listOf(row.domainReason, row.dnsReason, row.vpsReason)
            .mapNotNull { resolveLocalized(ctx, it) }
            .distinct()
            .joinToString(" ")
            .ifEmpty { null }
    }
    Column(modifier = Modifier.padding(vertical = 4.dp)) {
        FilledTonalButton(
            onClick = if (manual) onManual else onOpen,
            // The manual row covers DNS only; while it cannot, it is dead and
            // says why beneath.
            enabled = !manual || row.dnsReason == null,
            modifier = Modifier
                .fillMaxWidth()
                .testTag("provider-row[${row.providerId}]")
                .semantics { stateDescription = if (connected) "connected" else "available" },
        ) { Text(text) }
        if (row.held) {
            Text(
                stringResource(R.string.onboarding_provider_choice_held_credential),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        DisabledControlReasonText(reason)
    }
}

/** One provider's dialog — the sign-in pop-up, painted under its row. */
@Composable
private fun ProviderDialog(row: ProviderRow, vm: ProviderChoiceVM, onClose: () -> Unit) {
    val tick by vm.host.tick.collectAsState()
    val m = vm.host.machine
    val id = row.providerId
    val ctx = LocalContext.current
    val scope = rememberCoroutineScope()
    val meta = PROVIDERS.firstOrNull { it.id == id } ?: return
    val fields = remember(tick, id) { m.visibleProviderFields(id) }
    val canConnect = remember(tick, id) { m.canConnectProvider(id) }
    val notesKey = remember(tick, id) { m.providerRegistrarNotesKey(id) }
    val showContact = remember(tick, id) { m.providerShowsContactForm(id) }
    val contact = remember(tick, id) { m.providerContact(id) }
    // A secret field never shows a held value, so the typed values are this
    // dialog's own buffer; every keystroke is pushed to the machine.
    val typed = remember(id) { mutableStateMapOf<String, String>() }
    val hostedAuth = remember(tick, fields) { fields.filter { it.fieldType == FieldTypePlain.HOSTED_AUTH } }
    val hostedAuthLabels = remember(tick, hostedAuth) {
        hostedAuth.associate { it.id to hostedAuthButtonLabel(ctx, m.providerHostedAuthState(id, it.id)) }
    }
    val hostedAuthEnabled = remember(tick, hostedAuth) {
        hostedAuth.associate { it.id to m.providerHostedAuthCanBegin(id, it.id) }
    }

    Card(modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).testTag(Ids.PROVIDER_DIALOG)) {
        Column(modifier = Modifier.padding(12.dp)) {
            Text(providerName(id), style = MaterialTheme.typography.titleMedium)
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    meta.signupUrl,
                    modifier = Modifier.testTag(Ids.PROVIDER_DIALOG_LINK),
                    style = MaterialTheme.typography.bodySmall,
                )
                TextButton(
                    onClick = { UrlOpener.open(ctx, meta.signupUrl) },
                    modifier = Modifier.testTag(Ids.PROVIDER_DIALOG_OPEN_BROWSER_BUTTON),
                ) { Text(stringResource(R.string.onboarding_dns_config_open_in_browser)) }
            }
            Text(
                localized(LocalizedText(key = meta.helpKey, args = emptyMap())) ?: meta.helpKey,
                modifier = Modifier.testTag(Ids.PROVIDER_DIALOG_HELP_TEXT),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            notesKey?.let { key ->
                Spacer(Modifier.height(4.dp))
                Text(
                    localized(LocalizedText(key = key, args = emptyMap())) ?: key,
                    modifier = Modifier.testTag(Ids.DNS_REGISTRAR_NOTES_TEXT),
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Spacer(Modifier.height(8.dp))
            CredentialsForm(
                fields = fields,
                creds = typed,
                tag = Ids.PROVIDER_CREDENTIALS_FORM,
                enabled = !row.connecting,
                onChange = { fid, v ->
                    typed[fid] = v
                    vm.setCred(id, fid, v)
                },
                hostedAuthLabels = hostedAuthLabels,
                hostedAuthEnabled = hostedAuthEnabled,
                onHostedAuthClick = { fid ->
                    scope.launch {
                        try {
                            val prompt = m.providerHostedAuthBegin(id, fid)
                            UrlOpener.open(ctx, prompt.verificationUrl)
                            m.providerHostedAuthWait(id, fid)
                        } catch (e: Exception) {
                            // Already in HostedAuthState.Failed (painted as the
                            // button label on the next tick).
                        }
                    }
                },
            )
            Spacer(Modifier.height(8.dp))
            if (row.connected) {
                OutlinedButton(
                    onClick = { vm.disconnect(id) },
                    modifier = Modifier.testTag(Ids.PROVIDER_DISCONNECT_BUTTON),
                ) { Text(stringResource(R.string.onboarding_provider_choice_disconnect)) }
            } else {
                Button(
                    onClick = { vm.connect(id) },
                    enabled = canConnect && !row.connecting,
                    modifier = Modifier.testTag(Ids.PROVIDER_CONNECT_BUTTON),
                ) {
                    if (row.connecting) {
                        CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                    } else Text(stringResource(R.string.onboarding_provider_choice_connect))
                }
            }
            // The WHOIS contact form of a registrar that requires one — § 4's
            // form, only its place moved.
            if (showContact) {
                Spacer(Modifier.height(8.dp))
                ContactForm(
                    contact = contact ?: emptyContact(),
                    enabled = !row.connecting,
                    onChange = { vm.setContact(id, it) },
                )
            }
            val status = when {
                row.connecting -> stringResource(R.string.onboarding_provider_choice_dialog_connecting)
                row.error != null -> row.error ?: ""
                row.connected -> stringResource(R.string.onboarding_provider_choice_dialog_connected)
                else -> ""
            }
            Spacer(Modifier.height(4.dp))
            Text(
                status,
                modifier = Modifier.testTag(Ids.PROVIDER_DIALOG_STATUS_TEXT),
                style = MaterialTheme.typography.bodyMedium,
                color = if (row.error != null) MaterialTheme.colorScheme.error else Color.Unspecified,
            )
            TextButton(
                onClick = onClose,
                modifier = Modifier.testTag(Ids.PROVIDER_DIALOG_CLOSE_BUTTON),
            ) { Text(stringResource(R.string.onboarding_provider_choice_close)) }
        }
    }
}
