package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.ActorOption
import com.fauna.app.ui.viewmodel.AdminDnsVM
import social.fauna.generated.Capability
import social.fauna.generated.FieldType
import social.fauna.generated.PROVIDERS
import social.fauna.generated.ProviderMeta
import uniffi.fauna_client_dns.CertHealthState
import uniffi.fauna_client_dns.CertStatusRow
import uniffi.fauna_client_dns.CredentialSummary
import uniffi.fauna_client_dns.DelegationView
import uniffi.fauna_client_dns.DnsCredentialField
import uniffi.fauna_client_dns.DnsRecordRow
import uniffi.fauna_client_dns.DomainView
import uniffi.fauna_client_dns.PendingCertIssue
import uniffi.fauna_client_dns.RecordVerdict
import uniffi.fauna_client_dns.VerifyStatus
import uniffi.fauna_client_mail_settings.LocalDomainView
import uniffi.fauna_client_mail_settings.PrimaryDomainRenameView
import uniffi.fauna_client_mail_settings.RoleAddressKind
import uniffi.fauna_client_mail_settings.roleAddressOptions
import social.fauna.generated.Ids

/**
 * The admin `admin-dns` page (dns-management.md § App surface,
 * mail-multidomain.md § Per-domain catch-all): the unified DNS-management
 * surface. Per active domain it renders the required DNS record matrix with a
 * live red/green verify, a managed/manual mode control, the per-domain catch-all
 * actor picker, a primary badge, and a remove affordance; soft-deleted domains
 * render a restore row; plus the deployment manage-all master switch, the
 * write-only DNS-provider credential block, and add-domain / add-credential
 * forms. Merges the shared `DnsManagementMachine` (records + credentials + mode)
 * with the `LocalDomainMachine` (domain CRUD + catch-all) by domain name.
 *
 * Stateless [AdminDnsContent] is split out for the Compose test harness; the
 * VM-bound [AdminDnsScreen] is the wrapper the NavHost mounts. Dumb renderer of
 * the shared machines — no DNS / domain logic in the shell (priority #2). Mirrors
 * the linux lead (apps/fauna-linux/src/views/admin.rs build_dns_page).
 */
@Composable
fun AdminDnsScreen(
    navController: NavController,
    vm: AdminDnsVM = hiltViewModel(),
) {
    val active by vm.activeDomains.collectAsState()
    val removed by vm.removedDomains.collectAsState()
    val activeRename by vm.activeRename.collectAsState()
    val renameAvailable by vm.renameAvailable.collectAsState()
    val addingFirstDomain by vm.addingFirstDomain.collectAsState()
    val dnsDomains by vm.dnsDomains.collectAsState()
    val credentials by vm.credentials.collectAsState()
    val actors by vm.actors.collectAsState()
    val manageAll by vm.manageAll.collectAsState()
    val certStatuses by vm.certStatuses.collectAsState()
    val delegations by vm.delegations.collectAsState()
    val pendingCert by vm.pendingCert.collectAsState()
    val working by vm.working.collectAsState()
    val error by vm.error.collectAsState()

    AdminDnsContent(
        activeDomains = active,
        removedDomains = removed,
        activeRename = activeRename,
        renameAvailable = renameAvailable,
        addingFirstDomain = addingFirstDomain,
        dnsDomains = dnsDomains,
        credentials = credentials,
        actors = actors,
        manageAll = manageAll,
        certStatuses = certStatuses,
        delegations = delegations,
        pendingCert = pendingCert,
        working = working,
        error = error,
        onBack = { navController.popBackStack() },
        onRefresh = vm::refresh,
        onAddDomain = vm::addDomain,
        onRemoveDomain = vm::removeDomain,
        onRestoreDomain = vm::restoreDomain,
        onStartRename = vm::startPrimaryRename,
        onCompleteRename = vm::completePrimaryRename,
        onExtendRename = vm::extendPrimaryRenameGrace,
        onAbortRename = vm::abortPrimaryRename,
        onSetCatchAll = vm::setCatchAll,
        onSetRoleAddress = vm::setRoleAddress,
        onSetMode = vm::setMode,
        onSetManageAll = vm::setManageAll,
        onPutCredential = vm::putCredential,
        onClearCredential = vm::clearCredential,
        onIssueCert = vm::issueCert,
        onBeginManualIssue = vm::beginManualIssue,
        onCompleteManualIssue = vm::completeManualIssue,
        onCancelManualIssue = vm::cancelManualIssue,
        onDelegateRenewal = vm::delegateRenewal,
        onRemoveDelegation = vm::removeDelegation,
        onSetAutoRenew = vm::setAutoRenew,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminDnsContent(
    activeDomains: List<LocalDomainView>,
    removedDomains: List<LocalDomainView>,
    activeRename: PrimaryDomainRenameView?,
    renameAvailable: Boolean,
    addingFirstDomain: Boolean,
    dnsDomains: List<DomainView>,
    credentials: List<CredentialSummary>,
    actors: List<ActorOption>,
    manageAll: Boolean,
    certStatuses: List<CertStatusRow>,
    delegations: List<DelegationView>,
    pendingCert: PendingCertIssue?,
    working: Boolean,
    error: String?,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onAddDomain: (String) -> Unit,
    onRemoveDomain: (String) -> Unit,
    onRestoreDomain: (String) -> Unit,
    onStartRename: (ByteArray, Long?) -> Unit,
    onCompleteRename: (ByteArray, Boolean) -> Unit,
    onExtendRename: (ByteArray, Long) -> Unit,
    onAbortRename: (ByteArray) -> Unit,
    onSetCatchAll: (String, String?) -> Unit,
    onSetRoleAddress: (String, RoleAddressKind, String?) -> Unit,
    onSetMode: (String, Boolean) -> Unit,
    onSetManageAll: (Boolean) -> Unit,
    onPutCredential: (String, List<DnsCredentialField>, String) -> Unit,
    onClearCredential: (UInt) -> Unit,
    onIssueCert: (String) -> Unit,
    onBeginManualIssue: (String) -> Unit,
    onCompleteManualIssue: () -> Unit,
    onCancelManualIssue: () -> Unit,
    onDelegateRenewal: (String, String) -> Unit,
    onRemoveDelegation: (String) -> Unit,
    onSetAutoRenew: (String, Boolean) -> Unit,
) {
    // Primary-domain rename wizard state (mail-primary-domain-rename.md § UX
    // surface). `renameSheetOpen` gates the start-a-rename sheet; `renameTarget`
    // is the picked new-primary domain name (pre-set when opened via a row's
    // "Promote", empty → the first non-primary when opened via "Rename primary").
    var renameSheetOpen by remember { mutableStateOf(false) }
    var renameTarget by remember { mutableStateOf("") }
    val nonPrimary = activeDomains.filter { !it.isPrimary }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_dns_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack, modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK)) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
                actions = {
                    IconButton(
                        onClick = onRefresh,
                        enabled = !working,
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_REFRESH_BUTTON),
                    ) {
                        Icon(Icons.Default.Refresh, stringResource(R.string.admin_dns_refresh))
                    }
                },
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.admin_dns_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // Page-level error (rule #2): list_records / verify_records /
            // PutCredentials / domain CRUD failures route here.
            // No `else` placeholder: a shim carrying this id must LEAVE the tree
            // when it has nothing to say (`e2e-conventions.md` convention 2's
            // rider, obligation (a)). The bridge resolves visibility as bare
            // existence in the semantics tree (`ElementOps.isVisible` =
            // `findAll(id).isNotEmpty()`), so an empty `Box` here made
            // `is_visible("error-message")` structurally true on a clean page —
            // and the `assert not is_visible("error-message")` every such test
            // opens with could then never fail.
            if (!error.isNullOrEmpty()) {
                Text(
                    error,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
                )
            }

            // ── Deployment-wide primary-domain-rename banner ──
            // Shown while a rename is in flight; a dumb render of the shared
            // snapshot's activeRename. Action buttons are gated on the projected
            // can_* flags (the nest owns state advancement + validation);
            // complete/abort are reveal-then-confirm. (mail-primary-domain-rename.md
            // § UX surface — mirrors the web lead.)
            activeRename?.let { rename ->
                RenameBanner(
                    rename = rename,
                    working = working,
                    onComplete = { force -> onCompleteRename(rename.renameId, force) },
                    onExtend = { days -> onExtendRename(rename.renameId, days) },
                    onAbort = { onAbortRename(rename.renameId) },
                )
            }

            // ── Deployment master switch ──
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Text(
                    stringResource(R.string.admin_dns_manage_all),
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = manageAll,
                    onCheckedChange = onSetManageAll,
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_MANAGE_ALL_TOGGLE),
                )
            }

            AddDomainForm(
                working = working,
                addingFirstDomain = addingFirstDomain,
                onAddDomain = onAddDomain,
            )
            CredentialsSection(
                credentials = credentials,
                working = working,
                onPutCredential = onPutCredential,
                onClearCredential = onClearCredential,
            )

            HorizontalDivider()

            // ── Active domains: merge ld (name/primary/catch-all) + dns (records/mode) ──
            if (activeDomains.isEmpty()) {
                Text(
                    stringResource(R.string.admin_dns_empty),
                    style = MaterialTheme.typography.bodyMedium,
                )
                Text(
                    stringResource(R.string.admin_dns_empty_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                // Zones the held credentials cover — the delegate-zone-select options
                // (DelegateRenewal validates a held credential covers the chosen zone).
                val credZones = credentials.flatMap { it.zones }.distinct().sorted()
                activeDomains.forEach { domain ->
                    val dns = dnsDomains.firstOrNull { it.domain == domain.domain }
                    DomainRow(
                        domain = domain,
                        managed = dns?.mode == "managed",
                        autoRenew = dns?.autoRenew ?: false,
                        records = dns?.records ?: emptyList(),
                        actors = actors,
                        cert = certStatuses.firstOrNull { it.domain == domain.domain },
                        delegation = delegations.firstOrNull { it.domain == domain.domain },
                        pending = pendingCert?.takeIf { it.domain == domain.domain },
                        credZones = credZones,
                        working = working,
                        renameAvailable = renameAvailable,
                        activeRename = activeRename,
                        onOpenRename = {
                            // "Rename primary" from the primary row: seed the picker
                            // with the first non-primary (the admin can re-pick).
                            renameTarget = nonPrimary.firstOrNull()?.domain ?: ""
                            renameSheetOpen = true
                        },
                        onOpenPromote = {
                            // "Promote to primary" from a non-primary row: pre-target it.
                            renameTarget = domain.domain
                            renameSheetOpen = true
                        },
                        onRemove = { onRemoveDomain(domain.domain) },
                        onSetCatchAll = { onSetCatchAll(domain.domain, it) },
                        onSetRoleAddress = { role, hex -> onSetRoleAddress(domain.domain, role, hex) },
                        onSetMode = { onSetMode(domain.domain, it) },
                        onSetAutoRenew = { onSetAutoRenew(domain.domain, it) },
                        onIssueCert = { onIssueCert(domain.domain) },
                        onBeginManualIssue = { onBeginManualIssue(domain.domain) },
                        onCompleteManualIssue = onCompleteManualIssue,
                        onCancelManualIssue = onCancelManualIssue,
                        onDelegateRenewal = { zone -> onDelegateRenewal(domain.domain, zone) },
                        onRemoveDelegation = { onRemoveDelegation(domain.domain) },
                    )
                }
            }

            // ── Soft-deleted domains (30-day restore) ──
            if (removedDomains.isNotEmpty()) {
                HorizontalDivider()
                Text(
                    stringResource(R.string.admin_dns_removed_title),
                    style = MaterialTheme.typography.titleSmall,
                )
                Text(
                    stringResource(R.string.admin_dns_removed_desc),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                removedDomains.forEach { domain ->
                    RemovedDomainRow(domain = domain, working = working) {
                        onRestoreDomain(domain.domain)
                    }
                }
            }
        }

        // ── Start-a-rename wizard sheet (single instance; at most one rename in
        // flight). Opened from a domain row's rename/promote button; picks the new
        // primary from existing active non-primary domains (the two-step rule — the
        // wizard never adds a domain) + an optional grace override, then dispatches
        // StartPrimaryRename. (mail-primary-domain-rename.md § UX surface.)
        if (renameSheetOpen) {
            RenameSheet(
                candidates = nonPrimary,
                target = renameTarget,
                onTargetChange = { renameTarget = it },
                working = working,
                onSubmit = { graceDays ->
                    nonPrimary.firstOrNull { it.domain == renameTarget }
                        ?.let { onStartRename(it.domainId, graceDays) }
                    renameSheetOpen = false
                    renameTarget = ""
                },
                onCancel = {
                    renameSheetOpen = false
                    renameTarget = ""
                },
            )
        }
    }
}

// ── Add-domain form ──────────────────────────────────────────────────────────

@Composable
private fun AddDomainForm(
    working: Boolean,
    addingFirstDomain: Boolean,
    onAddDomain: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    var input by remember { mutableStateOf("") }

    if (!expanded) {
        OutlinedButton(
            onClick = { expanded = true },
            enabled = !working,
            modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_DOMAIN_BUTTON),
        ) { Text(stringResource(R.string.admin_dns_add_domain)) }
        return
    }
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedTextField(
                value = input,
                onValueChange = { input = it },
                singleLine = true,
                enabled = !working,
                label = { Text(stringResource(R.string.admin_dns_add_domain)) },
                placeholder = { Text(stringResource(R.string.admin_dns_add_domain_placeholder)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_ADD_DOMAIN_INPUT),
            )
            // A domainless nest's first add is a one-way door — say so before the
            // submit that makes it irreversible (`deployment-home-with-public-
            // relay.md` § MUA reach). No testTag: untagged chrome, no ui.yaml id.
            if (addingFirstDomain) {
                Text(
                    text = stringResource(R.string.admin_dns_add_domain_primary_warning),
                    color = MaterialTheme.colorScheme.error,
                )
            }
            // Only the submit reaches the nest (`fauna.bridges.add_local_domain`);
            // the opener above, this input and the cancel are the form's buffer
            // and stay live with no nest.
            val addGate = faunaGate("fauna.bridges.add_local_domain", enabled = !working)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        val d = input.trim().lowercase()
                        if (d.isNotEmpty()) {
                            onAddDomain(d)
                            input = ""
                            expanded = false
                        }
                    },
                    enabled = addGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_DOMAIN_SUBMIT_BUTTON),
                ) { Text(stringResource(R.string.admin_dns_add_domain_submit)) }
                OutlinedButton(
                    onClick = { input = ""; expanded = false },
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_DOMAIN_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.common_cancel)) }
            }
            DisabledControlReasonText(addGate.reason)
        }
    }
}

// ── DNS-provider credentials (write-only add-form + held-credential list) ──────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CredentialsSection(
    credentials: List<CredentialSummary>,
    working: Boolean,
    onPutCredential: (String, List<DnsCredentialField>, String) -> Unit,
    onClearCredential: (UInt) -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_CREDENTIALS_LIST),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(
            stringResource(R.string.admin_dns_credentials_title),
            style = MaterialTheme.typography.titleSmall,
        )
        if (credentials.isEmpty()) {
            Text(
                stringResource(R.string.admin_dns_credentials_empty),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        } else {
            credentials.forEachIndexed { index, cred ->
                CredentialItem(cred = cred, working = working) {
                    onClearCredential(index.toUInt())
                }
            }
        }
        AddCredentialForm(working = working, onPutCredential = onPutCredential)
    }
}

@Composable
private fun CredentialItem(cred: CredentialSummary, working: Boolean, onClear: () -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_CREDENTIAL_ITEM)) {
        Row(
            modifier = Modifier.padding(12.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    cred.label.ifEmpty { cred.providerId },
                    style = MaterialTheme.typography.bodyLarge,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_CREDENTIAL_ITEM_PROVIDER),
                )
                Text(
                    stringResource(R.string.admin_dns_credential_zones) + ": " + cred.zones.joinToString(", "),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_CREDENTIAL_ITEM_ZONES),
                )
            }
            OutlinedButton(
                onClick = onClear,
                enabled = !working,
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.ADMIN_DNS_CREDENTIAL_ITEM_CLEAR_BUTTON),
            ) { Text(stringResource(R.string.common_cancel)) }
        }
    }
}

@Composable
private fun AddCredentialForm(
    working: Boolean,
    onPutCredential: (String, List<DnsCredentialField>, String) -> Unit,
) {
    val ctx = LocalContext.current
    val dnsProviders = remember { PROVIDERS.filter { Capability.DNS in it.capabilities } }
    var expanded by remember { mutableStateOf(false) }
    var selected by remember { mutableStateOf<ProviderMeta?>(null) }
    // Per-field text values for the selected provider, keyed by field id.
    val values = remember { mutableStateMapOf<String, String>() }

    if (!expanded) {
        OutlinedButton(
            onClick = { expanded = true },
            enabled = !working,
            modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_CREDENTIAL_BUTTON),
        ) { Text(stringResource(R.string.admin_dns_add_credential)) }
        return
    }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // Per-DNS-provider selector buttons (keyed [<provider_id>]).
            Row(
                modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_CREDENTIAL_PROVIDER_ROW),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                dnsProviders.forEach { p ->
                    val isSel = selected?.id == p.id
                    FilterChip(
                        selected = isSel,
                        onClick = { selected = p; values.clear() },
                        label = { Text(resolveKey(p.displayNameKey)) },
                        modifier = Modifier.testTag("admin-dns-add-credential-provider-row[${p.id}]"),
                    )
                }
            }

            // Dynamic per-provider field entries (the provider's DNS-capable fields).
            Column(
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_ADD_CREDENTIAL_FORM),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                val fields = selected?.fields?.filter { Capability.DNS in it.kinds } ?: emptyList()
                fields.forEach { field ->
                    OutlinedTextField(
                        value = values[field.id].orEmpty(),
                        onValueChange = { values[field.id] = it },
                        singleLine = true,
                        enabled = !working,
                        label = { Text(resolveKey(field.labelKey)) },
                        visualTransformation = if (field.type == FieldType.SECRET) {
                            PasswordVisualTransformation()
                        } else {
                            VisualTransformation.None
                        },
                        keyboardOptions = if (field.type == FieldType.SECRET) {
                            androidx.compose.foundation.text.KeyboardOptions(keyboardType = KeyboardType.Password)
                        } else {
                            androidx.compose.foundation.text.KeyboardOptions.Default
                        },
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
            }

            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                val provider = selected
                val fields = provider?.fields?.filter { Capability.DNS in it.kinds } ?: emptyList()
                // Resolve the label in the composable scope (resolveKey is @Composable).
                val providerLabel = provider?.let { resolveKey(it.displayNameKey) } ?: ""
                val canSubmit = provider != null && !working &&
                    fields.all { !it.required || values[it.id]?.isNotBlank() == true }
                Button(
                    onClick = {
                        val p = provider ?: return@Button
                        val creds = fields.map {
                            DnsCredentialField(id = it.id, value = values[it.id].orEmpty())
                        }
                        onPutCredential(p.id, creds, providerLabel)
                        selected = null
                        values.clear()
                        expanded = false
                    },
                    enabled = canSubmit,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_CREDENTIAL_SUBMIT_BUTTON),
                ) { Text(stringResource(R.string.admin_dns_add_credential_submit)) }
                OutlinedButton(
                    onClick = { selected = null; values.clear(); expanded = false },
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_ADD_CREDENTIAL_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.common_cancel)) }
            }
        }
    }
}

// ── Per-domain row (admin-dns-domain) + record matrix (admin-dns-record) ───────

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DomainRow(
    domain: LocalDomainView,
    managed: Boolean,
    autoRenew: Boolean,
    records: List<DnsRecordRow>,
    actors: List<ActorOption>,
    cert: CertStatusRow?,
    delegation: DelegationView?,
    pending: PendingCertIssue?,
    credZones: List<String>,
    working: Boolean,
    renameAvailable: Boolean,
    activeRename: PrimaryDomainRenameView?,
    onOpenRename: () -> Unit,
    onOpenPromote: () -> Unit,
    onRemove: () -> Unit,
    onSetCatchAll: (String?) -> Unit,
    onSetRoleAddress: (RoleAddressKind, String?) -> Unit,
    onSetMode: (Boolean) -> Unit,
    onSetAutoRenew: (Boolean) -> Unit,
    onIssueCert: () -> Unit,
    onBeginManualIssue: () -> Unit,
    onCompleteManualIssue: () -> Unit,
    onCancelManualIssue: () -> Unit,
    onDelegateRenewal: (String) -> Unit,
    onRemoveDelegation: () -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_DOMAIN)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // The primary domain cannot be removed (mail-multidomain.md § Additional) —
            // that page predicate is handed to the gate rather than re-tested beside
            // it, so a reconnect restores exactly it and never more. Hoisted out of
            // the header Row so the reason renders beneath the row (§ R11 (account-data-plane.md § The ratified decisions)) instead of
            // as another horizontal item.
            val removeGate = faunaGate(
                "fauna.bridges.remove_local_domain",
                enabled = !working && !domain.isPrimary,
            )
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    domain.domain,
                    style = MaterialTheme.typography.titleMedium,
                    fontFamily = FontFamily.Monospace,
                    modifier = Modifier.weight(1f).testTag(Ids.ADMIN_DNS_DOMAIN_NAME),
                )
                if (domain.isPrimary) {
                    AssistChip(
                        onClick = {},
                        label = { Text(stringResource(R.string.admin_dns_primary_badge)) },
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_PRIMARY_BADGE),
                    )
                }
                OutlinedButton(
                    onClick = onRemove,
                    enabled = removeGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_REMOVE_BUTTON),
                ) { Text(stringResource(R.string.admin_dns_remove)) }
            }
            DisabledControlReasonText(removeGate.reason)

            // Primary-domain rename affordances (mail-primary-domain-rename.md
            // § UX surface). Primary row: "Rename primary" (disabled until a
            // non-primary exists — the two-step rule) + the in-flight state badge.
            // Non-primary row: "Promote to primary" (hidden while a rename runs).
            val renameInFlight = activeRename != null
            if (domain.isPrimary) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    OutlinedButton(
                        onClick = onOpenRename,
                        enabled = !working && renameAvailable && !renameInFlight,
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_RENAME_BUTTON),
                    ) { Text(stringResource(R.string.admin_dns_rename_button)) }
                    if (activeRename != null) {
                        AssistChip(
                            onClick = {},
                            label = {
                                Text(
                                    "${stringResource(R.string.admin_dns_rename_renaming_to)} " +
                                        "${activeRename.newPrimaryDomain} (${activeRename.state})",
                                )
                            },
                            modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_RENAME_STATE),
                        )
                    }
                }
            } else if (!renameInFlight) {
                OutlinedButton(
                    onClick = onOpenPromote,
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_PROMOTE_BUTTON),
                ) { Text(stringResource(R.string.admin_dns_rename_promote)) }
            }

            // Managed / manual mode control.
            Row(
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    stringResource(
                        if (managed) R.string.admin_dns_mode_managed else R.string.admin_dns_mode_manual
                    ),
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = managed,
                    onCheckedChange = onSetMode,
                    enabled = !working,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_MODE),
                )
            }

            // Per-domain auto-renew checkbox (admin-dns-domain-auto-renew,
            // tls-certificates.md § C.3). Shown ONLY for managed/delegated rows — the
            // only kind a synced client can auto-issue; a manual-non-delegated domain
            // can never auto-renew (its autoRenew is always false), so the control is
            // absent there. Default-on: a fresh managed/delegated domain renders
            // checked with no admin action. Toggling dispatches SetAutoRenew (config-only).
            if (managed || delegation != null) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(
                        stringResource(R.string.admin_dns_cert_auto_renew),
                        style = MaterialTheme.typography.labelMedium,
                        modifier = Modifier.weight(1f),
                    )
                    Checkbox(
                        checked = autoRenew,
                        onCheckedChange = onSetAutoRenew,
                        enabled = !working,
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_AUTO_RENEW),
                    )
                }
            }

            // Per-domain catch-all actor picker ("none" clears).
            CatchAllPicker(
                current = domain.catchAllActorId?.let { bytesToHex(it) },
                actors = actors,
                working = working,
                onSetCatchAll = onSetCatchAll,
            )

            // Present only when a SUCCESSION (not an admin) last cleared this
            // domain's catch-all — tells the admin why the picker above
            // reads "none" and that unmatched mail is now bouncing; re-designating
            // via that same picker is the fix. Same read-only-explainer idiom as
            // the rename-state chip above.
            if (domain.catchAllClearedBySuccessionAt != null) {
                Text(
                    stringResource(R.string.admin_dns_catch_all_cleared_by_succession),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.ADMIN_DNS_DOMAIN_CATCH_ALL_CLEARED_STATE),
                )
            }

            // Per-domain role-address override pickers (postmaster/abuse/noc/security;
            // mail-multidomain.md § Per-domain role-address routing). One actor dropdown
            // per overridable RFC 2142 role — option 0 = "Admin (default)" clears the
            // override (the role falls back to the deployment admin), any actor designates.
            // The same per-row picker as catch-all, ×4 (the nest atomic-merges, so each
            // role is independent). Mirrors linux apps/fauna-linux/src/views/admin.rs.
            Text(
                stringResource(R.string.admin_dns_role_address_label),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            remember { roleAddressOptions() }.forEach { option ->
                RoleAddressPicker(
                    storageKey = option.key,
                    current = domain.roleAddressOverrides
                        .firstOrNull { it.role == option.kind }
                        ?.let { bytesToHex(it.actorId) },
                    actors = actors,
                    working = working,
                    onSet = { onSetRoleAddress(option.kind, it) },
                )
            }

            // Per-domain TLS-cert lifecycle (status badge + issue/renew + manual-paste
            // complete/cancel + CNAME renewal-delegation; tls-certificates.md § C.4 +
            // § B tier 2/3). Mirrors linux build_cert_issuance + build_cert_delegation.
            CertSection(
                managed = managed,
                cert = cert,
                delegation = delegation,
                pending = pending,
                credZones = credZones,
                working = working,
                onIssueCert = onIssueCert,
                onBeginManualIssue = onBeginManualIssue,
                onCompleteManualIssue = onCompleteManualIssue,
                onCancelManualIssue = onCancelManualIssue,
                onDelegateRenewal = onDelegateRenewal,
                onRemoveDelegation = onRemoveDelegation,
            )

            // Required DNS records (name / type / value / red-green status / copy).
            records.forEach { record -> RecordRow(record = record) }
        }
    }
}

/**
 * Per-domain TLS-cert lifecycle block (tls-certificates.md § C.4 + § B tier 2/3):
 * the served-cert health badge, the get/renew button (single `IssueCert` for a
 * managed/delegated domain, two-phase `BeginManualIssueCert` for a manual one) with
 * the `_acme-challenge` paste surface while a manual order is pending, and the
 * one-time CNAME renewal-delegation. Mirrors linux `build_cert_issuance` +
 * `build_cert_delegation`. The native apps enable issuance directly via
 * `instant-acme`; the `--client android` e2e RUN stays host-emulator-gated.
 */
@Composable
private fun CertSection(
    managed: Boolean,
    cert: CertStatusRow?,
    delegation: DelegationView?,
    pending: PendingCertIssue?,
    credZones: List<String>,
    working: Boolean,
    onIssueCert: () -> Unit,
    onBeginManualIssue: () -> Unit,
    onCompleteManualIssue: () -> Unit,
    onCancelManualIssue: () -> Unit,
    onDelegateRenewal: (String) -> Unit,
    onRemoveDelegation: () -> Unit,
) {
    // The served-cert health badge: the nest-computed state of the cert the listener
    // actually serves. `null` until RefreshCertStatus returns → a "checking" badge.
    Text(
        certStatusText(cert),
        style = MaterialTheme.typography.bodySmall,
        color = certStatusColor(cert),
        modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_STATUS),
    )

    // Get/renew: single IssueCert for managed/delegated, two-phase BeginManualIssueCert
    // for a manual domain. Inert while a manual order is pending (complete/cancel first).
    val singleIssue = managed || delegation != null
    // ⚠ A DISCRIMINANT site, and the one the gate's own doc names. The two paths
    // are genuinely different kinds, so the gate is handed the SAME `singleIssue`
    // the click takes and the shared table decides which one stays live: the
    // managed/delegated path runs the whole DNS-01 order and ends by DELIVERING
    // the cert to this nest (`fauna.tls.publish_cert`, OnlineOnly), while manual
    // phase 1 only opens the CA order and stashes the breadcrumb in the admin's
    // own `fauna.state.dns` (`fauna.account.state.put`, OfflineSafe — so it stays live
    // with no nest, and greying it would be the gate over-claiming). tui rules
    // the same split on the same flag (`admin/mod.rs` wire_kind, `IssueDnsCert
    // { single_issue }`); never a Kotlin class test in place of the table.
    val issueGate = faunaGate(
        if (singleIssue) "fauna.tls.publish_cert" else "fauna.account.state.put",
        enabled = !working && pending == null,
    )
    OutlinedButton(
        onClick = { if (singleIssue) onIssueCert() else onBeginManualIssue() },
        enabled = issueGate.enabled,
        modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_ISSUE_BUTTON),
    ) { Text(stringResource(R.string.admin_dns_cert_issue)) }
    DisabledControlReasonText(issueGate.reason)

    // Manual-paste surface — only while an order for this domain awaits the admin.
    if (pending != null) {
        Text(
            stringResource(R.string.admin_dns_cert_paste_instructions),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        // The transient _acme-challenge TXT(s) to paste — the same admin-dns-record card.
        pending.challenges.forEach { challenge -> RecordRow(record = challenge) }
        // Manual PHASE 2 is where the manual path finally reaches this nest — it
        // finalizes the order and delivers the cert, the same
        // `fauna.tls.publish_cert` the managed path ends in. Its cancel sibling
        // only drops the breadcrumb (`fauna.account.state.put`, OfflineSafe), so it
        // declares nothing and stays live: abandoning a suspended order must
        // work with no nest, or the admin is stuck holding it.
        val completeGate = faunaGate("fauna.tls.publish_cert", enabled = !working)
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = onCompleteManualIssue,
                enabled = completeGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_COMPLETE_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_cert_issue_complete)) }
            OutlinedButton(
                onClick = onCancelManualIssue,
                enabled = !working,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_cert_issue_cancel)) }
        }
        DisabledControlReasonText(completeGate.reason)
    }

    CertDelegation(
        delegation = delegation,
        credZones = credZones,
        working = working,
        onDelegateRenewal = onDelegateRenewal,
        onRemoveDelegation = onRemoveDelegation,
    )
}

/**
 * The `_acme-challenge` CNAME renewal-delegation affordance (tls-certificates.md
 * § B tier 3, S6b). Delegated → a "Renewals automated" label + the one-time CNAME
 * (the same admin-dns-record shape) + a remove button. Not delegated → a delegate
 * button revealing an inline form (zone picker + submit/cancel), disabled when no
 * held credential covers any zone. Config-only — renders on every app.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CertDelegation(
    delegation: DelegationView?,
    credZones: List<String>,
    working: Boolean,
    onDelegateRenewal: (String) -> Unit,
    onRemoveDelegation: () -> Unit,
) {
    if (delegation != null) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.admin_dns_cert_renewals_automated),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.primary,
                modifier = Modifier.weight(1f),
            )
            OutlinedButton(
                onClick = onRemoveDelegation,
                enabled = !working,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_REMOVE_DELEGATION_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_cert_remove_delegation)) }
        }
        // The one-time CNAME the admin sets once at their registrar.
        RecordRow(record = delegation.cname)
        return
    }

    var formOpen by remember { mutableStateOf(false) }
    if (!formOpen) {
        OutlinedButton(
            onClick = { formOpen = true },
            enabled = !working && credZones.isNotEmpty(),
            modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_DELEGATE_BUTTON),
        ) {
            Text(
                if (credZones.isEmpty()) {
                    stringResource(R.string.admin_dns_cert_delegate_no_zones)
                } else {
                    stringResource(R.string.admin_dns_cert_delegate)
                },
            )
        }
        return
    }

    var zoneExpanded by remember { mutableStateOf(false) }
    var selectedZone by remember(credZones) { mutableStateOf(credZones.firstOrNull() ?: "") }
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        ExposedDropdownMenuBox(
            expanded = zoneExpanded,
            onExpandedChange = { if (!working) zoneExpanded = !zoneExpanded },
        ) {
            OutlinedTextField(
                value = selectedZone,
                onValueChange = {},
                readOnly = true,
                enabled = !working,
                label = { Text(stringResource(R.string.admin_dns_cert_delegate_zone_label)) },
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = zoneExpanded) },
                modifier = Modifier
                    .menuAnchor()
                    .fillMaxWidth()
                    .testTag(Ids.ADMIN_DNS_CERT_DELEGATE_ZONE_SELECT),
            )
            ExposedDropdownMenu(expanded = zoneExpanded, onDismissRequest = { zoneExpanded = false }) {
                credZones.forEach { zone ->
                    DropdownMenuItem(
                        text = { Text(zone) },
                        onClick = { selectedZone = zone; zoneExpanded = false },
                    )
                }
            }
        }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { if (selectedZone.isNotEmpty()) onDelegateRenewal(selectedZone); formOpen = false },
                enabled = !working,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_DELEGATE_SUBMIT_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_cert_delegate_submit)) }
            OutlinedButton(
                onClick = { formOpen = false },
                modifier = Modifier.testTag(Ids.ADMIN_DNS_CERT_DELEGATE_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_cert_delegate_cancel)) }
        }
    }
}

/**
 * The cert-status badge text (`admin-dns-cert-status`, tls-certificates.md § C.4):
 * the nest-reported state of the served cert, with the self-signed sub-label on the
 * floor and the expiry date for a trusted/expiring cert. `null` until
 * RefreshCertStatus has returned (or for a domain the reply didn't cover) → "checking".
 */
@Composable
private fun certStatusText(cert: CertStatusRow?): String {
    val label = stringResource(R.string.admin_dns_cert_label)
    if (cert == null) {
        return "$label ${stringResource(R.string.admin_dns_status_checking)}"
    }
    // `CertHealthState`'s serde/wire variant name — what `certStatusView` (shared
    // Rust, `fauna_core::format::cert_status_view`) matches against; it takes the
    // raw string rather than the typed enum because `fauna_core` does not depend
    // on `fauna-client-dns`.
    val stateWire = when (cert.state) {
        CertHealthState.VALID_TRUSTED -> "ValidTrusted"
        CertHealthState.ON_FLOOR_RENEW_NEEDED -> "OnFloorRenewNeeded"
        CertHealthState.EXPIRING -> "Expiring"
    }
    val view = com.fauna.ffi.certStatusView(stateWire, cert.isFloor, cert.notAfterUnix)
    val state = localized(view.state) ?: view.state.key
    val expiresAtUnix = view.expiresAtUnix
    return when {
        // On the self-signed floor — a trusted cert is needed; the floor's own
        // expiry is not the admin's concern, so we label it self-signed instead.
        view.showSelfSigned -> "$label $state (${stringResource(R.string.admin_dns_cert_self_signed)})"
        expiresAtUnix != null ->
            "$label $state — ${stringResourceFmt(R.string.admin_dns_cert_expires, formatCertExpiry(expiresAtUnix))}"
        else -> "$label $state"
    }
}

/** The cert-status badge colour: primary (trusted), tertiary (expiring / on-floor —
 *  non-fatal, native apps keep working), or muted while still loading. */
@Composable
private fun certStatusColor(cert: CertStatusRow?) = when (cert?.state) {
    CertHealthState.VALID_TRUSTED -> MaterialTheme.colorScheme.primary
    CertHealthState.EXPIRING, CertHealthState.ON_FLOOR_RENEW_NEEDED -> MaterialTheme.colorScheme.tertiary
    null -> MaterialTheme.colorScheme.onSurfaceVariant
}

/** Format a cert notAfter (unix seconds) as a local YYYY-MM-DD date for the badge.
 *  Falls back to the raw seconds on conversion failure (never throws). */
private fun formatCertExpiry(unixSecs: Long): String =
    runCatching {
        java.time.Instant.ofEpochSecond(unixSecs)
            .atZone(java.time.ZoneId.systemDefault())
            .toLocalDate()
            .toString()
    }.getOrDefault(unixSecs.toString())

@Composable
private fun RecordRow(record: DnsRecordRow) {
    // Captioned Name/Type/Value rows + a status/copy row — mirrors the richest
    // peer pattern (apple FaunaKit AdminDnsView, web/linux record cards all render
    // the admin.dns.field_* captions + a visible admin.dns.copy label, #1/#3/#4).
    Column(
        modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_RECORD),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        DnsRecordField(
            stringResource(R.string.admin_dns_field_name),
            record.name,
            "admin-dns-record-name",
        )
        DnsRecordField(
            stringResource(R.string.admin_dns_field_type),
            record.recordType,
            "admin-dns-record-type",
        )
        DnsRecordField(
            stringResource(R.string.admin_dns_field_value),
            record.expected,
            "admin-dns-record-value",
            maxLines = 2,
        )
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                verifyStatusLabel(record.verdict),
                style = MaterialTheme.typography.labelSmall,
                color = verifyStatusColor(record.verdict?.status),
                modifier = Modifier.testTag(Ids.ADMIN_DNS_RECORD_STATUS),
            )
            Spacer(Modifier.weight(1f))
            CopyButton(
                testTag = Ids.ADMIN_DNS_RECORD_COPY_BUTTON,
                text = record.expected,
                label = stringResource(R.string.admin_dns_copy),
                outlined = false,
            )
        }
        // Reverse DNS (PTR) is set at the admin's server/VPS provider, never
        // zone-published (dns-management.md § Records covered) — tui/web/linux
        // already render this note; rule-A approved 2026-08-15.
        if (record.recordType == "PTR") {
            Text(
                stringResource(R.string.admin_dns_ptr_provider_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag("admin-dns-record-provider-note"),
            )
        }
    }
}

/** One captioned DNS-record field — a dim caption label over a monospace value. */
@Composable
private fun DnsRecordField(
    label: String,
    value: String,
    valueTestTag: String,
    maxLines: Int = 1,
) {
    Text(
        label,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(
        value,
        style = MaterialTheme.typography.bodySmall,
        fontFamily = FontFamily.Monospace,
        maxLines = maxLines,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.testTag(valueTestTag),
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CatchAllPicker(
    current: String?,
    actors: List<ActorOption>,
    working: Boolean,
    onSetCatchAll: (String?) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val noneLabel = stringResource(R.string.admin_dns_catch_all_none)
    val selectedLabel = current?.let { hex ->
        actors.firstOrNull { it.idHex == hex }?.label ?: hex
    } ?: noneLabel

    // A dispatch-on-pick commit, not a draft: picking IS the
    // `fauna.bridges.set_catch_all_actor` call (designating and clearing are the
    // same kind — `actor: None` is the clear). So the anchor carries the
    // declaration, and desensitizing it closes the menu with it, since the menu
    // only opens from the anchor — the shape `admin-web-apex-actor-select` and
    // the users-row `TierDropdown` already record.
    val catchAllGate = faunaGate("fauna.bridges.set_catch_all_actor", enabled = !working)
    ExposedDropdownMenuBox(
        expanded = expanded && catchAllGate.enabled,
        onExpandedChange = { if (catchAllGate.enabled) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = selectedLabel,
            onValueChange = {},
            readOnly = true,
            enabled = catchAllGate.enabled,
            label = { Text(stringResource(R.string.admin_dns_catch_all_label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(Ids.ADMIN_DNS_DOMAIN_CATCH_ALL_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(noneLabel) },
                onClick = { onSetCatchAll(null); expanded = false },
            )
            actors.forEach { actor ->
                DropdownMenuItem(
                    text = { Text(actor.label) },
                    onClick = { onSetCatchAll(actor.idHex); expanded = false },
                )
            }
        }
    }
    DisabledControlReasonText(catchAllGate.reason)
}

/**
 * One per-domain role-address override dropdown (`admin-dns-domain-role-address-<role>-select`),
 * the catch-all picker ×4. Option 0 = "Admin (default)" clears the override
 * (`onSet(null)` → the role falls back to the deployment admin); any actor designates.
 * `storageKey` is the shared `roleAddressOptions()` table's key for this role —
 * never re-derived here (`mail-multidomain.md` § Per-domain role-address
 * routing → *One owner for the role vocabulary*).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RoleAddressPicker(
    storageKey: String,
    current: String?,
    actors: List<ActorOption>,
    working: Boolean,
    onSet: (String?) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val adminDefaultLabel = stringResource(R.string.admin_dns_role_address_admin_default)
    val selectedLabel = current?.let { hex ->
        actors.firstOrNull { it.idHex == hex }?.label ?: hex
    } ?: adminDefaultLabel

    // Dispatch-on-pick, exactly like the catch-all picker above: the pick IS the
    // `fauna.bridges.set_role_address` call, clear included.
    val roleGate = faunaGate("fauna.bridges.set_role_address", enabled = !working)
    ExposedDropdownMenuBox(
        expanded = expanded && roleGate.enabled,
        onExpandedChange = { if (roleGate.enabled) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = selectedLabel,
            onValueChange = {},
            readOnly = true,
            enabled = roleGate.enabled,
            label = { Text("$storageKey@") },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag("admin-dns-domain-role-address-$storageKey-select"),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(adminDefaultLabel) },
                onClick = { onSet(null); expanded = false },
            )
            actors.forEach { actor ->
                DropdownMenuItem(
                    text = { Text(actor.label) },
                    onClick = { onSet(actor.idHex); expanded = false },
                )
            }
        }
    }
    DisabledControlReasonText(roleGate.reason)
}

@Composable
private fun RemovedDomainRow(domain: LocalDomainView, working: Boolean, onRestore: () -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_REMOVED_DOMAIN)) {
        Row(
            modifier = Modifier.padding(12.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                domain.domain,
                style = MaterialTheme.typography.bodyLarge,
                fontFamily = FontFamily.Monospace,
                modifier = Modifier.weight(1f).testTag(Ids.ADMIN_DNS_REMOVED_DOMAIN_NAME),
            )
            val restoreGate = faunaGate("fauna.bridges.restore_local_domain", enabled = !working)
            OutlinedButton(
                onClick = onRestore,
                enabled = restoreGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_REMOVED_DOMAIN_RESTORE_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_restore)) }
            DisabledControlReasonText(restoreGate.reason)
        }
    }
}

// ── Primary-domain rename (mail-primary-domain-rename.md § UX surface) ─────────

/**
 * The deployment-wide in-flight primary-domain-rename banner (Card;
 * `admin-dns-rename-banner`). A dumb render of the shared [PrimaryDomainRenameView]:
 * the old→new pair + state + a client-rendered grace countdown, plus the
 * complete / extend / abort controls gated on the projected `can_*` flags (the
 * nest owns state advancement + validation). Complete + abort are
 * reveal-then-confirm; the confirm copy names the risk/cost. Mirrors the web lead
 * + the pending-rotation Card idiom (MailSettingsScreen.kt:262).
 */
@Composable
private fun RenameBanner(
    rename: PrimaryDomainRenameView,
    working: Boolean,
    onComplete: (Boolean) -> Unit,
    onExtend: (Long) -> Unit,
    onAbort: () -> Unit,
) {
    var confirmingComplete by remember { mutableStateOf(false) }
    var confirmingAbort by remember { mutableStateOf(false) }
    var extendDays by remember { mutableStateOf("7") }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_RENAME_BANNER)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // Head: title + old → new (mono).
            Text(
                stringResource(R.string.admin_dns_rename_banner_title),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                "${rename.oldPrimaryDomain} → ${rename.newPrimaryDomain}",
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
            )
            // Meta: state + (post-flip) client-rendered grace countdown.
            Text(
                "${stringResource(R.string.admin_dns_rename_state_label)} ${rename.state}",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            val graceEndsAt = rename.graceEndsAt
            if (rename.isPostFlipActive && graceEndsAt != null) {
                Text(
                    "${stringResource(R.string.admin_dns_rename_grace_ends)} ${graceRemaining(graceEndsAt)}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }

            // Complete now (reveal-then-confirm). `force` iff still in grace
            // (can_force_complete); a plain complete once the window has elapsed.
            if (rename.canComplete || rename.canForceComplete) {
                if (confirmingComplete) {
                    if (rename.canForceComplete) {
                        Text(
                            stringResource(R.string.admin_dns_rename_complete_force_warning),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    // Arming is local — only this CONFIRM dispatches
                    // `fauna.bridges.complete_primary_domain_rename`, so the
                    // opener in the else-branch declares nothing and stays live
                    // (a dead opener could not reach the confirm at all).
                    val completeGate = faunaGate(
                        "fauna.bridges.complete_primary_domain_rename",
                        enabled = !working,
                    )
                    Button(
                        onClick = { confirmingComplete = false; onComplete(rename.canForceComplete) },
                        enabled = completeGate.enabled,
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_COMPLETE_CONFIRM_BUTTON),
                    ) { Text(stringResource(R.string.admin_dns_rename_complete_confirm)) }
                    DisabledControlReasonText(completeGate.reason)
                } else {
                    OutlinedButton(
                        onClick = { confirmingComplete = true },
                        enabled = !working,
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_COMPLETE_BUTTON),
                    ) { Text(stringResource(R.string.admin_dns_rename_complete)) }
                }
            }

            // Extend grace by N days ([1, 30]).
            if (rename.canExtend) {
                // The days input beside it is the buffer and stays live; only the
                // commit issues `fauna.bridges.extend_primary_domain_rename_grace`.
                // Hoisted out of the Row so the reason can render BENEATH the pair
                // rather than as a third horizontal item (§ R11: beside itself).
                val extendGate = faunaGate(
                    "fauna.bridges.extend_primary_domain_rename_grace",
                    enabled = !working,
                )
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    OutlinedTextField(
                        value = extendDays,
                        onValueChange = { extendDays = it.filter(Char::isDigit) },
                        singleLine = true,
                        enabled = !working,
                        label = { Text(stringResource(R.string.admin_dns_rename_extend_days_label)) },
                        keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                            keyboardType = KeyboardType.Number,
                        ),
                        modifier = Modifier.weight(1f).testTag(Ids.ADMIN_DNS_RENAME_EXTEND_DAYS_INPUT),
                    )
                    OutlinedButton(
                        onClick = {
                            extendDays.trim().toLongOrNull()?.takeIf { it >= 1 }?.let(onExtend)
                        },
                        enabled = extendGate.enabled,
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_EXTEND_BUTTON),
                    ) { Text(stringResource(R.string.admin_dns_rename_extend)) }
                }
                DisabledControlReasonText(extendGate.reason)
            }

            // Abort (reveal-then-confirm). The post-flip inverse-re-flip cost is
            // named in the confirm warning (only when !is_pre_flip).
            if (rename.canAbort) {
                if (confirmingAbort) {
                    if (!rename.isPreFlip) {
                        Text(
                            stringResource(R.string.admin_dns_rename_abort_postflip_warning),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                        )
                    }
                    // Same arming split as complete: the opener stays live, the
                    // confirm declares `fauna.bridges.abort_primary_domain_rename`.
                    val abortGate = faunaGate(
                        "fauna.bridges.abort_primary_domain_rename",
                        enabled = !working,
                    )
                    Button(
                        onClick = { confirmingAbort = false; onAbort() },
                        enabled = abortGate.enabled,
                        colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_ABORT_CONFIRM_BUTTON),
                    ) { Text(stringResource(R.string.admin_dns_rename_abort_confirm)) }
                    DisabledControlReasonText(abortGate.reason)
                } else {
                    OutlinedButton(
                        onClick = { confirmingAbort = true },
                        enabled = !working,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_ABORT_BUTTON),
                    ) { Text(stringResource(R.string.admin_dns_rename_abort)) }
                }
            }
        }
    }
}

/**
 * The start-a-rename wizard sheet (`admin-dns-rename-sheet`, an AlertDialog):
 * pick the new primary from the deployment's existing active non-primary domains
 * (the two-step rule — the wizard never adds a domain) + an optional grace-days
 * override, then dispatch StartPrimaryRename. The nest validates every
 * precondition; a refusal surfaces via error-message. Mirrors the web lead.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RenameSheet(
    candidates: List<LocalDomainView>,
    target: String,
    onTargetChange: (String) -> Unit,
    working: Boolean,
    onSubmit: (Long?) -> Unit,
    onCancel: () -> Unit,
) {
    var graceDays by remember { mutableStateOf("") }
    var expanded by remember { mutableStateOf(false) }

    // Only the submit dispatches `fauna.bridges.start_primary_domain_rename`; the
    // sheet's picker, grace-days input and cancel are its buffer and stay live.
    // The submit's OWN predicate (a target must be picked) is handed over rather
    // than re-tested, so an untouched sheet stays dead for the sheet's reason and
    // a reconnect restores exactly that. ⚠ Both halves matter to a test: assert
    // the gate only AFTER picking a target, or it passes against an ungated app.
    // Computed here rather than in the `confirmButton` slot so the reason can
    // render at the foot of the sheet body (§ R11) instead of in the button row.
    val submitGate = faunaGate(
        "fauna.bridges.start_primary_domain_rename",
        enabled = !working && target.isNotEmpty(),
    )
    AlertDialog(
        onDismissRequest = onCancel,
        modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_SHEET),
        title = { Text(stringResource(R.string.admin_dns_rename_sheet_title)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                // New-primary picker — existing active non-primary domains only.
                Text(
                    stringResource(R.string.admin_dns_rename_new_primary_label),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                ExposedDropdownMenuBox(
                    expanded = expanded,
                    onExpandedChange = { if (!working) expanded = !expanded },
                ) {
                    OutlinedTextField(
                        value = target,
                        onValueChange = {},
                        readOnly = true,
                        enabled = !working,
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                        modifier = Modifier
                            .menuAnchor()
                            .fillMaxWidth()
                            .testTag(Ids.ADMIN_DNS_RENAME_NEW_PRIMARY_SELECT),
                    )
                    ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                        candidates.forEach { d ->
                            DropdownMenuItem(
                                text = { Text(d.domain) },
                                onClick = { onTargetChange(d.domain); expanded = false },
                            )
                        }
                    }
                }
                // Optional grace-window override (days; blank → nest default 7).
                OutlinedTextField(
                    value = graceDays,
                    onValueChange = { graceDays = it.filter(Char::isDigit) },
                    singleLine = true,
                    enabled = !working,
                    label = { Text(stringResource(R.string.admin_dns_rename_grace_days_label)) },
                    keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                        keyboardType = KeyboardType.Number,
                    ),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_DNS_RENAME_GRACE_DAYS_INPUT),
                )
                DisabledControlReasonText(submitGate.reason)
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onSubmit(graceDays.trim().toLongOrNull()) },
                enabled = submitGate.enabled,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_SUBMIT_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_rename_submit)) }
        },
        dismissButton = {
            TextButton(
                onClick = onCancel,
                modifier = Modifier.testTag(Ids.ADMIN_DNS_RENAME_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.admin_dns_rename_cancel)) }
        },
    )
}

/**
 * Client-rendered countdown to `grace_ends_at` (epoch-millis). The rename state is
 * nest-authoritative; only the *display* is client-side. Routes through the shared
 * `fauna_core::format::grace_countdown` (value-formatting.md § Grace countdown);
 * `null` means past the deadline, render the elapsed label.
 */
@Composable
private fun graceRemaining(endsAtMs: Long): String {
    val text = com.fauna.ffi.graceCountdown(endsAtMs, System.currentTimeMillis())
    return localized(text) ?: stringResource(R.string.admin_dns_rename_grace_elapsed)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Resolve a dotted i18n key (e.g. `provisioning.hetzner.api_token`) against
 *  the generated Android string resources (mirrors [localizedOnboardingText]). */
@Composable
private fun resolveKey(key: String): String {
    val ctx = LocalContext.current
    val resId = ctx.resources.getIdentifier(key.replace('.', '_'), "string", ctx.packageName)
    return if (resId == 0) key else ctx.getString(resId)
}

@Composable
private fun verifyStatusLabel(verdict: RecordVerdict?): String {
    // A null verdict has no wire form to pass — handled client-side; everything
    // else goes through `dnsVerdictLabel` (shared Rust,
    // `fauna_core::format::dns_verdict_label`), which takes the `VerifyStatus`
    // serde/wire variant name as a raw string for the same reason
    // `certStatusView` does, plus what public DNS actually served so a mismatch
    // reads "found 1.2.3.4" instead of dead-ending.
    val status = verdict?.status ?: return "—"
    val statusWire = when (status) {
        VerifyStatus.OK -> "Ok"
        VerifyStatus.MISSING -> "Missing"
        VerifyStatus.MISMATCH -> "Mismatch"
        VerifyStatus.CHECKING -> "Checking"
    }
    return localized(com.fauna.ffi.dnsVerdictLabel(statusWire, verdict.observed)) ?: statusWire
}

@Composable
private fun verifyStatusColor(status: VerifyStatus?) = when (status) {
    VerifyStatus.OK -> MaterialTheme.colorScheme.primary
    VerifyStatus.MISSING, VerifyStatus.MISMATCH -> MaterialTheme.colorScheme.error
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}

private fun bytesToHex(bytes: ByteArray): String =
    bytes.joinToString("") { "%02x".format(it) }
