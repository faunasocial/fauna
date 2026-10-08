package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.disabled
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.rememberCopyToClipboard
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.MailAliasesVM
import uniffi.fauna_client_mail_settings.AliasKind
import uniffi.fauna_client_mail_settings.AliasView
import uniffi.fauna_client_mail_settings.AliasesStatus
import uniffi.fauna_client_mail_settings.ImportAliasStatusView
import uniffi.fauna_client_mail_settings.ImportResultView
import uniffi.fauna_client_mail_settings.aliasHitsLabel
import uniffi.fauna_client_mail_settings.aliasKindBadge
import social.fauna.generated.Ids

/**
 * The per-account `mail-aliases` page (`mail-aliases.md`): list, create, edit,
 * revoke, delete aliases + mint disposables. A sub-page of the mail-settings hub.
 *
 * Stateless [MailAliasesContent] is split out for the Compose test harness; the
 * VM-bound [MailAliasesScreen] is the wrapper the NavHost mounts. The backend is
 * built (`mail-aliases.md` § Impl status) → genuinely green; errors
 * (`reserved_local_part` / `conflicts_with_existing_alias` / validation) surface
 * via the global error-message banner. Mirrors the Linux lead
 * (apps/fauna-linux/src/settings/mail_aliases.rs); the add-sheet has no domain
 * picker — the machine derives `default_domain` from the user's canonical alias
 * and disables the add controls when it has none.
 */
@Composable
fun MailAliasesScreen(
    navController: NavController,
    vm: MailAliasesVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val hydrated by vm.hydrated.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    MailAliasesContent(
        aliases = snapshot.aliases,
        defaultDomain = snapshot.defaultDomain,
        hydrated = hydrated,
        working = snapshot.status == AliasesStatus.WORKING,
        lastImportResult = snapshot.lastImportResult,
        lastMintedAddress = snapshot.lastMintedAddress,
        // Kind badge via shared `alias_kind_badge` (mail-aliases.md § Shared
        // alias_kind_badge formatter); FFI lives here, off the testable Content.
        kindLabel = { kind -> resolveLocalized(context, aliasKindBadge(kind)).orEmpty() },
        // Hit-count text via shared `alias_hits_label` ("N hits" / "N hits · last
        // <date>") — mail-aliases.md § Hit count. FFI lives here, off the testable
        // Content; the last-hit date is formatted natively, and the with-date branch
        // is a 2-arg key resolved by NAME (value-formatting.md § android resolves by name).
        hitsLabel = { alias ->
            val lastHit = alias.lastHitAtMs?.let { formatLastHitDate(it) }
            resolveLocalized(context, aliasHitsLabel(alias.hitCount, lastHit)).orEmpty()
        },
        onBack = { navController.popBackStack() },
        onCreate = vm::create,
        onGenerateDisposable = { vm.generateDisposable(null, null, "") },
        onGenerateWithParams = vm::generateDisposable,
        onUpdate = vm::update,
        onRevoke = vm::revoke,
        onEnable = vm::enable,
        onDelete = vm::delete,
        onImport = vm::import,
    )
}

/** Local add/edit/import-sheet mode. */
private sealed interface SheetMode {
    object Closed : SheetMode
    object Add : SheetMode
    data class Edit(val alias: AliasView) : SheetMode
    object Import : SheetMode
}

/**
 * Native, locale/timezone-aware rendering of an alias's last-hit epoch-ms for the
 * shared `alias_hits_label` "· last <date>" branch (mirrors how windows formats the
 * date natively; the shared fn takes the already-formatted string).
 */
private fun formatLastHitDate(epochMs: Long): String =
    java.text.DateFormat.getDateInstance(java.text.DateFormat.MEDIUM).format(java.util.Date(epochMs))

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailAliasesContent(
    aliases: List<AliasView>,
    defaultDomain: String?,
    // Loading-vs-resolved-empty (`ui/README.md` rule 5). Defaults true so
    // existing/other-purpose call sites keep rendering the resolved state.
    hydrated: Boolean = true,
    working: Boolean,
    lastImportResult: ImportResultView?,
    // The full address the last disposable mint produced (`MailAliasesSnapshot.
    // last_minted_address`): copied to the clipboard, confirmed on the page, and
    // published as the generate button's `copied` attr (the copy-button contract
    // `account-actor-id-copy-btn` set) so a test asserts what was copied.
    lastMintedAddress: String? = null,
    kindLabel: (AliasKind) -> String,
    hitsLabel: (AliasView) -> String,
    // Add-sheet numeric-field validators via shared fauna_core::format — parse_count
    // (u32: spam-threshold override + disposable ttl/uses) and parse_count_i64 (i64:
    // rate-limit override) — value-formatting.md § Mail-knob validation, mail-aliases.md
    // § Per-alias controls. FFI lives here as an injected default so the Content test
    // stays off the native path; empty/invalid → null (no override/limit), and
    // parse_count_i64 additionally rejects a negative rate that toLongOrNull passed.
    parseCount: (String) -> UInt? = { com.fauna.ffi.parseCount(it) },
    parseCountI64: (String) -> Long? = { com.fauna.ffi.parseCountI64(it) },
    onBack: () -> Unit,
    onCreate: (AliasKind, String, String, UInt?, Long?) -> Unit,
    onGenerateDisposable: () -> Unit,
    onGenerateWithParams: (UInt?, UInt?, String) -> Unit,
    onUpdate: (String, String, String, UInt?, Long?) -> Unit,
    onRevoke: (String) -> Unit,
    onEnable: (String) -> Unit,
    onDelete: (String) -> Unit,
    onImport: (List<String>) -> Unit,
) {
    var sheet by remember { mutableStateOf<SheetMode>(SheetMode.Closed) }
    // Locally hides a stale `lastImportResult` on reopen (mirrors linux's
    // `open_import_sheet` widget-clear) so a leftover success banner from a
    // prior import can't be mistaken for the outcome of a fresh paste; any
    // other dispatch also clears the underlying snapshot field server-side.
    var importResultDismissed by remember { mutableStateOf(false) }
    val canAdd = defaultDomain != null && !working
    val copyToClipboard = rememberCopyToClipboard()
    LaunchedEffect(lastMintedAddress) { lastMintedAddress?.let(copyToClipboard) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.mail_aliases_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
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
                stringResource(R.string.mail_aliases_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            if (defaultDomain == null) {
                Text(
                    stringResource(R.string.mail_aliases_no_default_domain),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }

            // Of these three, only Generate dispatches. Add and Import merely
            // REVEAL their sheets (arming is local), so they stay live with no
            // nest — and they are the live siblings that stop a blanket grey
            // from passing this page's test.
            val generateGate = faunaGate(
                "fauna.bridges.generate_disposable_alias",
                enabled = canAdd,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { sheet = SheetMode.Add },
                    enabled = canAdd,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_ADD_BUTTON),
                ) { Text(stringResource(R.string.mail_aliases_add_button)) }
                OutlinedButton(
                    onClick = onGenerateDisposable,
                    enabled = generateGate.enabled,
                    modifier = Modifier
                        .testTag(Ids.MAIL_ALIASES_GENERATE_DISPOSABLE_BUTTON)
                        // `copied` rides the stateDescription, this app's one
                        // string-attribute carrier (`AutomationSemantics.attrValue`).
                        .then(
                            if (lastMintedAddress != null) {
                                Modifier.semantics { stateDescription = lastMintedAddress }
                            } else {
                                Modifier
                            },
                        ),
                ) { Text(stringResource(R.string.mail_aliases_generate_button)) }
                OutlinedButton(
                    onClick = { importResultDismissed = true; sheet = SheetMode.Import },
                    enabled = canAdd,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_IMPORT_BUTTON),
                ) { Text(stringResource(R.string.mail_aliases_import_button)) }
            }
            DisabledControlReasonText(generateGate.reason)
            // The mint's copy confirmation, beside the button that minted it.
            lastMintedAddress?.let { address ->
                Text(
                    "${stringResource(R.string.mail_aliases_copied)} $address",
                    style = MaterialTheme.typography.bodySmall,
                )
            }

            // ── Add / edit sheet (inline reveal) ──
            when (val mode = sheet) {
                is SheetMode.Add -> AliasSheet(
                    editing = null,
                    kindLabel = kindLabel,
                    parseCount = parseCount,
                    parseCountI64 = parseCountI64,
                    onSubmitCreate = { kind, pattern, label, spam, rate ->
                        onCreate(kind, pattern, label, spam, rate); sheet = SheetMode.Closed
                    },
                    onSubmitDisposable = { ttl, uses, label ->
                        onGenerateWithParams(ttl, uses, label); sheet = SheetMode.Closed
                    },
                    onSubmitUpdate = { _, _, _, _, _ -> },
                    onCancel = { sheet = SheetMode.Closed },
                )
                is SheetMode.Edit -> AliasSheet(
                    editing = mode.alias,
                    kindLabel = kindLabel,
                    parseCount = parseCount,
                    parseCountI64 = parseCountI64,
                    onSubmitCreate = { _, _, _, _, _ -> },
                    onSubmitDisposable = { _, _, _ -> },
                    onSubmitUpdate = { id, pattern, label, spam, rate ->
                        onUpdate(id, pattern, label, spam, rate); sheet = SheetMode.Closed
                    },
                    onCancel = { sheet = SheetMode.Closed },
                )
                SheetMode.Import -> ImportSheet(
                    lastImportResult = if (importResultDismissed) null else lastImportResult,
                    onSubmit = { lines -> importResultDismissed = false; onImport(lines) },
                    onCancel = { sheet = SheetMode.Closed },
                )
                SheetMode.Closed -> {}
            }

            // ── Alias list (indexed; one row per AliasView) ──
            // Loading vs. resolved-empty (`ui/README.md` rule 5): an
            // un-hydrated first paint must not claim "No aliases yet" — the
            // page does not know that yet, and every create affordance above
            // sits dead with no reason while it's wrong.
            if (!hydrated) {
                Text(
                    stringResource(R.string.mail_aliases_loading),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (aliases.isEmpty()) {
                Text(
                    stringResource(R.string.mail_aliases_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                aliases.forEach { alias ->
                    AliasRow(
                        alias = alias,
                        working = working,
                        kindLabel = kindLabel,
                        hitsLabel = hitsLabel,
                        onEdit = { sheet = SheetMode.Edit(alias) },
                        onRevoke = { onRevoke(alias.aliasIdHex) },
                        onEnable = { onEnable(alias.aliasIdHex) },
                        onDelete = { onDelete(alias.aliasIdHex) },
                    )
                }
            }
        }
    }
}

/**
 * The add/edit sheet. On Add, the kind picker selects Exact / Wildcard /
 * Disposable (Disposable mints via GenerateDisposable; the others Create). On
 * Edit, the kind is immutable (read-only) and submit dispatches Update.
 */
@Composable
private fun AliasSheet(
    editing: AliasView?,
    kindLabel: (AliasKind) -> String,
    parseCount: (String) -> UInt?,
    parseCountI64: (String) -> Long?,
    onSubmitCreate: (AliasKind, String, String, UInt?, Long?) -> Unit,
    onSubmitDisposable: (UInt?, UInt?, String) -> Unit,
    onSubmitUpdate: (String, String, String, UInt?, Long?) -> Unit,
    onCancel: () -> Unit,
) {
    var kind by remember(editing) { mutableStateOf(editing?.kind ?: AliasKind.EXACT) }
    var pattern by remember(editing) { mutableStateOf(editing?.pattern ?: "") }
    var label by remember(editing) { mutableStateOf(editing?.label ?: "") }
    var spam by remember(editing) { mutableStateOf(editing?.spamThresholdOverride?.toString() ?: "") }
    var rate by remember(editing) { mutableStateOf(editing?.rateLimitPerHour?.toString() ?: "") }
    var ttl by remember(editing) { mutableStateOf("") }
    var uses by remember(editing) { mutableStateOf("") }

    val isDisposable = kind == AliasKind.DISPOSABLE

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.mail_aliases_form_title),
                style = MaterialTheme.typography.titleMedium,
            )

            // Kind picker. Read-only on edit (the kind is immutable).
            // On edit the node reports `disabled` = "true" (read-only, not merely
            // inert), the contract tui's `.enabled(!editing)` meets.
            Row(
                modifier = Modifier
                    .testTag(Ids.MAIL_ALIASES_ADD_SHEET_KIND_PICKER)
                    .then(if (editing != null) Modifier.semantics { disabled() } else Modifier),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                if (editing == null) {
                    listOf(AliasKind.EXACT, AliasKind.WILDCARD, AliasKind.DISPOSABLE).forEach { k ->
                        FilterChip(
                            selected = kind == k,
                            onClick = { kind = k },
                            label = { Text(kindLabel(k)) },
                        )
                    }
                } else {
                    AssistChip(onClick = {}, label = { Text(kindLabel(kind)) })
                }
            }

            if (!isDisposable) {
                OutlinedTextField(
                    value = pattern,
                    onValueChange = { pattern = it },
                    label = { Text(stringResource(R.string.mail_aliases_pattern_placeholder)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_ADD_SHEET_PATTERN_INPUT),
                )
            } else {
                // Keep the pattern testTag in the tree for ui.yaml conformance.
                Box(modifier = Modifier.testTag(Ids.MAIL_ALIASES_ADD_SHEET_PATTERN_INPUT))
            }

            OutlinedTextField(
                value = label,
                onValueChange = { label = it },
                label = { Text(stringResource(R.string.mail_aliases_label_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_ADD_SHEET_LABEL_INPUT),
            )
            OutlinedTextField(
                value = spam,
                onValueChange = { spam = it.filter(Char::isDigit) },
                label = { Text(stringResource(R.string.mail_aliases_spam_threshold_placeholder)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_ADD_SHEET_SPAM_THRESHOLD_INPUT),
            )
            OutlinedTextField(
                value = rate,
                onValueChange = { rate = it.filter(Char::isDigit) },
                label = { Text(stringResource(R.string.mail_aliases_rate_per_hour_placeholder)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_ADD_SHEET_RATE_PER_HOUR_INPUT),
            )

            // Disposable-only TTL / uses (shown only when Disposable is selected;
            // the testTags stay in source for ui.yaml conformance).
            if (isDisposable) {
                OutlinedTextField(
                    value = ttl,
                    onValueChange = { ttl = it.filter(Char::isDigit) },
                    label = { Text(stringResource(R.string.mail_aliases_ttl_placeholder)) },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_ADD_SHEET_TTL_INPUT),
                )
                OutlinedTextField(
                    value = uses,
                    onValueChange = { uses = it.filter(Char::isDigit) },
                    label = { Text(stringResource(R.string.mail_aliases_uses_placeholder)) },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_ADD_SHEET_USES_INPUT),
                )
            } else {
                Box(modifier = Modifier.testTag(Ids.MAIL_ALIASES_ADD_SHEET_TTL_INPUT))
                Box(modifier = Modifier.testTag(Ids.MAIL_ALIASES_ADD_SHEET_USES_INPUT))
            }

            // ⚠ A THREE-WAY DISCRIMINANT, and android's is one arm WIDER than
            // the lead app's. tui splits this button's job across two gestures —
            // `MailAliasesSubmit { editing }` (create/update) and a separate
            // `MailAliasesGenerateDisposable` — so its carry is two-way. android
            // folds the disposable submit into the SAME control, so the kind
            // turns on `editing` *and* `isDisposable`. The gate is therefore
            // handed the identical `when` the click handler below takes,
            // arm for arm, so the two cannot drift; splitting it into a
            // two-way guess would declare `create_account_alias` over a click
            // that actually issues `generate_disposable_alias`.
            //
            // All three arms are OnlineOnly today, so this composes to the same
            // observable behaviour — which is exactly why it needs writing down
            // rather than collapsing: the value is that a later reclassification
            // of any one arm moves this control with it. (Contrast
            // `put_spam_model` on the moderation queue, where the collapse IS
            // correct and recorded as such: there both branches answer one
            // gesture, not three.)
            val submitGate = faunaGate(
                when {
                    editing != null -> "fauna.bridges.update_account_alias"
                    isDisposable -> "fauna.bridges.generate_disposable_alias"
                    else -> "fauna.bridges.create_account_alias"
                },
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = {
                        val spamVal = parseCount(spam)
                        val rateVal = parseCountI64(rate)
                        when {
                            editing != null ->
                                onSubmitUpdate(editing.aliasIdHex, pattern, label, spamVal, rateVal)
                            isDisposable ->
                                onSubmitDisposable(parseCount(ttl), parseCount(uses), label)
                            else ->
                                onSubmitCreate(kind, pattern, label, spamVal, rateVal)
                        }
                    },
                    enabled = submitGate.enabled,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_ADD_SHEET_SUBMIT_BUTTON),
                ) { Text(stringResource(R.string.mail_aliases_submit)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_ADD_SHEET_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.mail_aliases_cancel)) }
            }
            DisabledControlReasonText(submitGate.reason)
        }
    }
}

/**
 * Bulk paste-import sheet (`mail-aliases.md` § Bulk import, the
 * recipient-whitelist migration path): a multi-line textarea → `Import`.
 * Stays open on submit (unlike [AliasSheet]) so [lastImportResult] renders in
 * place; only Cancel closes it (mirrors the linux lead — windows collapses on
 * submit, which hides its own result and is a latent bug entrusted there).
 */
@Composable
private fun ImportSheet(
    lastImportResult: ImportResultView?,
    onSubmit: (List<String>) -> Unit,
    onCancel: () -> Unit,
) {
    var text by remember { mutableStateOf("") }

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                stringResource(R.string.mail_aliases_import_title),
                style = MaterialTheme.typography.titleMedium,
            )
            Text(
                stringResource(R.string.mail_aliases_import_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                label = { Text(stringResource(R.string.mail_aliases_import_placeholder)) },
                singleLine = false,
                minLines = 4,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_IMPORT_TEXTAREA),
            )
            // The textarea and cancel are buffer and escape; only Import commits.
            val importGate = faunaGate("fauna.bridges.import_account_aliases")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onSubmit(text.split("\n")) },
                    enabled = importGate.enabled,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_IMPORT_SUBMIT_BUTTON),
                ) { Text(stringResource(R.string.mail_aliases_import_submit)) }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_IMPORT_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.mail_aliases_import_cancel)) }
            }
            DisabledControlReasonText(importGate.reason)
            if (lastImportResult != null) {
                Text(
                    importResultText(lastImportResult),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_IMPORT_RESULT),
                )
            }
        }
    }
}

/**
 * `mail-aliases-import-result`: the shared counts template
 * (`mail_aliases.import_result`) plus one `mail_aliases.import_invalid_line`
 * row per invalid outcome — mail-aliases.md:199-201 requires the reason be
 * rendered, not just tallied.
 */
@Composable
private fun importResultText(r: ImportResultView): String {
    val summary = stringResourceFmt(
        R.string.mail_aliases_import_result,
        r.created.toString(), r.skippedDuplicate.toString(), r.invalid.toString(),
    )
    val invalidLines = r.outcomes
        .filter { it.status == ImportAliasStatusView.INVALID }
        .map { stringResourceFmt(R.string.mail_aliases_import_invalid_line, it.address, it.reason.orEmpty()) }
    return (listOf(summary) + invalidLines).joinToString("\n")
}

/**
 * One `mail-aliases-list-item` row, projected from an [AliasView].
 *
 * The canonical `<handle>@<domain>` row ([AliasView.isCanonical]) is the user's
 * primary mailbox + AUTH-login identity; the nest rejects disabling, renaming, or
 * deleting it (`canonical_alias_protected`). It is rendered **read-only** — no
 * overflow-delete / toggle / edit / revoke, just a "Primary address" marker — so
 * the protection is visible up-front rather than surfacing only as an error on
 * attempt (`mail-aliases.md` § Aliases UX). Ordinary rows get the full control
 * set, with a **two-way** "Active" toggle (ON → `Enable`, OFF → `Revoke`) so
 * disabling is no longer a one-way trap (`mail-aliases.md:156`). Mirrors the Linux
 * lead (apps/fauna-linux/src/settings/mail_aliases.rs).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AliasRow(
    alias: AliasView,
    working: Boolean,
    kindLabel: (AliasKind) -> String,
    hitsLabel: (AliasView) -> String,
    onEdit: () -> Unit,
    onRevoke: () -> Unit,
    onEnable: () -> Unit,
    onDelete: () -> Unit,
) {
    var menuOpen by remember { mutableStateOf(false) }
    var auditOpen by remember { mutableStateOf(false) }

    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ALIASES_LIST_ITEM)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Text(
                    alias.address,
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f).testTag(Ids.MAIL_ALIASES_LIST_ITEM_PATTERN),
                )
                AssistChip(
                    onClick = {},
                    label = {
                        Text(kindLabel(alias.kind), modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_KIND))
                    },
                )
                // Overflow Delete — omitted on the canonical row (read-only).
                if (!alias.isCanonical) {
                    // The overflow ICON opens the menu and stays live — arming
                    // is local, and a dead opener could not be reached to prove
                    // the item inside it is gated. The DESTRUCTIVE ITEM declares.
                    val deleteGate = faunaGate("fauna.bridges.delete_account_alias")
                    Box {
                        IconButton(
                            onClick = { menuOpen = true },
                            modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_OVERFLOW_MENU),
                        ) {
                            Icon(Icons.Default.MoreVert, contentDescription = stringResource(R.string.mail_aliases_delete))
                        }
                        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                            DropdownMenuItem(
                                text = {
                                    Column {
                                        Text(stringResource(R.string.mail_aliases_delete))
                                        DisabledControlReasonText(deleteGate.reason)
                                    }
                                },
                                enabled = deleteGate.enabled,
                                onClick = { menuOpen = false; onDelete() },
                            )
                        }
                    }
                }
            }
            if (alias.label.isNotEmpty()) {
                Text(
                    alias.label,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_LABEL),
                )
            } else {
                Box(modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_LABEL))
            }
            Text(
                hitsLabel(alias),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_HITS),
            )
            // ⚠ A DISCRIMINANT on the Active toggle: its two directions are two
            // distinct registered kinds. Which one the next click will issue is
            // read at PAINT time from the row's own state — `alias.disabled`
            // means the next flip enables — which is precisely the `enabling`
            // carry tui gives `MailAliasesToggleActive`
            // (`settings/mod.rs:3194-3199`), and the same expression
            // `onCheckedChange` branches on below.
            val activeGate = faunaGate(
                if (alias.disabled) "fauna.bridges.enable_account_alias"
                else "fauna.bridges.revoke_account_alias",
                enabled = !working,
            )
            // The dedicated revoke button answers the toggle's OFF direction —
            // the same nest-side operation, per tui's own note — so it declares
            // that one kind outright. It keeps its own `!alias.disabled`
            // predicate: an already-revoked alias has nothing to revoke,
            // connected or not.
            val revokeGate = faunaGate(
                "fauna.bridges.revoke_account_alias",
                enabled = !working && !alias.disabled,
            )
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (alias.isCanonical) {
                    // Read-only marker in place of the mutating controls.
                    Text(
                        stringResource(R.string.mail_aliases_primary_address_badge),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.weight(1f),
                    )
                } else {
                    // Two-way "Active" toggle: ON → Enable (re-enable), OFF → Revoke
                    // (soft-off). `checked` is bound to snapshot state, so a refresh
                    // re-renders it without re-firing onCheckedChange.
                    Text(
                        stringResource(R.string.mail_aliases_active_toggle_label),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    Switch(
                        checked = !alias.disabled,
                        onCheckedChange = { checked -> if (checked) onEnable() else onRevoke() },
                        enabled = activeGate.enabled,
                        modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_DISABLED_TOGGLE),
                    )
                    // Edit REVEALS the sheet, so it stays live — this row's live
                    // sibling beside the dead toggle and revoke.
                    OutlinedButton(
                        onClick = onEdit,
                        enabled = !working,
                        modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_EDIT_BUTTON),
                    ) { Text(stringResource(R.string.mail_aliases_edit)) }
                    OutlinedButton(
                        onClick = onRevoke,
                        enabled = revokeGate.enabled,
                        modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_REVOKE_BUTTON),
                    ) { Text(stringResource(R.string.mail_aliases_revoke)) }
                }
                TextButton(
                    onClick = { auditOpen = !auditOpen },
                    modifier = Modifier.testTag(Ids.MAIL_ALIASES_LIST_ITEM_SHOW_AUDIT),
                ) { Text(stringResource(R.string.mail_aliases_show_audit)) }
            }
            if (!alias.isCanonical) {
                // One reason for the pair — they are disabled by the same
                // verdict, and two identical lines under one row would be noise.
                DisabledControlReasonText(activeGate.reason ?: revokeGate.reason)
            }
        }
    }
}
