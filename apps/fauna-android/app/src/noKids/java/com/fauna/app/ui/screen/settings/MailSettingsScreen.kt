package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.MailSettingsVM
import uniffi.fauna_client_mail_settings.CredentialKind
import uniffi.fauna_client_mail_settings.MailCredentialSummary
import uniffi.fauna_client_mail_settings.MuaInstructions
import uniffi.fauna_client_mail_settings.PendingRotationStatus
import uniffi.fauna_client_mail_settings.SettingsStatus
import uniffi.fauna_client_mail_settings.passwordStrengthLabel
import uniffi.fauna_client_mail_settings.settingsStatusLabel
import social.fauna.generated.Ids

/**
 * The `mail-settings` hub (`mail-settings.md`): the user's single touchpoint for
 * third-party-MUA access — enable/disable mail, add credentials (listed on the
 * Connected apps page), rotate
 * the MSEK, and read the MUA connection details. The entry into the mail-settings
 * family; its nav rows reach the [MailAliasesScreen] / [MailListsScreen] /
 * [MailExportScreen] / [MailImportScreen] / [MailSpamScreen] sub-pages.
 *
 * Stateless [MailSettingsContent] is split out for the Compose test harness; the
 * VM-bound [MailSettingsScreen] is the wrapper the NavHost mounts. Per priority
 * #2 the shell holds no mail logic — the shared MailSettingsMachine drives every
 * flow. Mirrors the Linux lead (apps/fauna-linux/src/settings/mail.rs);
 * `page-heading` is this screen's local TopAppBar title, `error-message` the
 * global MessageBanner.
 */
@Composable
fun MailSettingsScreen(
    navController: NavController,
    vm: MailSettingsVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val nestEncrypted by vm.nestEncrypted.collectAsState()
    val generatedPassword by vm.generatedPassword.collectAsState()
    val lastToken by vm.lastToken.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    MailSettingsContent(
        enabled = snapshot.enabled,
        caldavEnabled = snapshot.caldavEnabled,
        carddavEnabled = snapshot.carddavEnabled,
        servesWebdavSet = snapshot.servesWebdavSet,
        credentialManagementReachable = snapshot.credentialManagementReachable,
        servingEnabled = snapshot.servingEnabled,
        credentials = snapshot.credentials,
        mua = snapshot.mua,
        pendingRotation = snapshot.pendingRotation,
        status = snapshot.status,
        nestEncrypted = nestEncrypted,
        generatedPassword = generatedPassword,
        lastToken = lastToken,
        onBack = { navController.popBackStack() },
        onNavAliases = { navController.navigate("settings/mail-aliases") },
        onNavLists = { navController.navigate("settings/mail-lists") },
        onNavExport = { navController.navigate("settings/mail-export") },
        onNavImport = { navController.navigate("settings/mail-import") },
        onNavSpam = { navController.navigate("settings/mail-spam") },
        onEnable = vm::enableMail,
        onAddCredential = vm::addCredential,
        onDisableMail = vm::disableMail,
        onSetServingEnabled = vm::setServingEnabled,
        // Advisory password-strength readout via shared `password_strength_label`
        // (mail-settings.md § Implementation status today); FFI lives here, off the
        // testable Content. Empty password → "" (the meter clears).
        strengthLabel = { pw -> resolveLocalized(context, passwordStrengthLabel(pw)).orEmpty() },
        // Status indicator via shared `settings_status_label` (mail-settings.md §
        // the status indicator); FFI lives here, off the testable Content. Gates
        // Idle on `enabled` — a disabled mailbox reads "Mail is disabled".
        statusLabel = { status, enabled -> resolveLocalized(context, settingsStatusLabel(status, enabled)).orEmpty() },
        onStartRotation = vm::startRotation,
        onResumeRotation = vm::resumeRotation,
        onRegeneratePassword = vm::regeneratePassword,
        onClearToken = vm::clearToken,
    )
}

private sealed interface CredForm {
    object Closed : CredForm
    object Enable : CredForm
    object Add : CredForm
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailSettingsContent(
    enabled: Boolean,
    caldavEnabled: Boolean,
    carddavEnabled: Boolean,
    // True iff this actor serves >=1 folder over WebDAV
    // (`MailSettingsSnapshot::serves_webdav_set`) — folded from
    // `fauna.folders.list`, NOT the deployment-wide `webdav_enabled` toggle.
    // Gates the `mail-settings-mua-webdav-url` row.
    servesWebdavSet: Boolean,
    // The credential-management reachability predicate, computed once in shared
    // Rust (priority #2/#4: `MailSettingsSnapshot::credential_management_reachable`
    // = `enabled || caldavEnabled || carddavEnabled || servesWebdavSet`) instead
    // of re-derived here. Drives the credential-management section, serve-here
    // toggle, and MUA-instructions block.
    credentialManagementReachable: Boolean,
    servingEnabled: Boolean,
    credentials: List<MailCredentialSummary>,
    mua: MuaInstructions,
    pendingRotation: PendingRotationStatus?,
    status: SettingsStatus,
    nestEncrypted: Boolean,
    generatedPassword: String,
    lastToken: String?,
    onBack: () -> Unit,
    onNavAliases: () -> Unit,
    onNavLists: () -> Unit,
    onNavExport: () -> Unit,
    onNavImport: () -> Unit,
    onNavSpam: () -> Unit,
    onEnable: (String, CredentialKind, Boolean, String) -> Unit,
    onAddCredential: (String, CredentialKind, Boolean, String) -> Unit,
    onDisableMail: () -> Unit,
    onSetServingEnabled: (Boolean) -> Unit,
    // Advisory strength readout for a manually-typed password via shared
    // `password_strength_label`; provided by the wrapper (real FFI), the Content
    // stays JVM-testable with a stub. Empty → "" (the meter clears).
    strengthLabel: (String) -> String,
    // Status-indicator text via shared `settings_status_label(status, enabled)`;
    // provided by the wrapper (real FFI), the Content stays JVM-testable with a
    // stub. Gates Idle on `enabled` so a disabled mailbox reads "Mail is disabled"
    // (not "All up to date"). mail-settings.md § the status indicator.
    statusLabel: (SettingsStatus, Boolean) -> String,
    // `(excluded, onDone)`: `onDone` runs once the rotation's dispatch returns.
    onStartRotation: (List<String>, () -> Unit) -> Unit,
    onResumeRotation: () -> Unit,
    onRegeneratePassword: () -> Unit,
    onClearToken: () -> Unit,
) {
    var form by remember { mutableStateOf<CredForm>(CredForm.Closed) }
    var showRotate by remember { mutableStateOf(false) }
    // `Some(credentials to re-wrap)` while a confirmed rotation runs: the form
    // holds open, progress painted, controls disabled, until its dispatch
    // returns (mail-settings.md § Element visibility) — the tui/web/linux shape.
    var rotationInFlight by remember { mutableStateOf<ULong?>(null) }
    var showDisableConfirm by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.mail_settings_title),
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
            // ── Enabled toggle ──
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(stringResource(R.string.settings_mail_enable_title), style = MaterialTheme.typography.bodyLarge)
                    Text(
                        stringResource(R.string.settings_mail_enable_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                Switch(
                    checked = enabled,
                    onCheckedChange = { on ->
                        // Off-path opens the destructive confirm dialog (the real
                        // decision point); the toggle stays rendering `enabled`
                        // until DisableMail lands. mail-settings.md § Disable mail.
                        if (on && !enabled) form = CredForm.Enable
                        else if (!on && enabled) showDisableConfirm = true
                    },
                    modifier = Modifier.testTag(Ids.MAIL_SETTINGS_ENABLED_TOGGLE),
                )
            }

            // ── Disable-mail confirm (destructive) ──
            // Bulk-revoke every credential + clear the MSEK via the shared
            // DisableMail; cancel leaves the toggle on (snapshot `enabled`
            // unchanged). Mirrors the factory-reset AlertDialog idiom + the linux
            // lead (apps/fauna-linux/src/settings/mail.rs::open_disable_confirm).
            if (showDisableConfirm) {
                AlertDialog(
                    onDismissRequest = { showDisableConfirm = false },
                    modifier = Modifier.testTag(Ids.MAIL_SETTINGS_DISABLE_CONFIRM),
                    title = { Text(stringResource(R.string.settings_mail_disable_title)) },
                    text = { Text(stringResource(R.string.settings_mail_disable_warning)) },
                    confirmButton = {
                        // ⚠ THIS DESTRUCTIVE CONFIRM STAYS LIVE WITH NO NEST, AND
                        // CARRIES NO GATE — deliberately; it is this page's
                        // biggest surprise. Disable-mail is a **client-config
                        // write**, not a bridge call: it drops every row from
                        // the `fauna.state.mail` credentials plus the MSEK and
                        // writes the plane, and `mail-settings.md` § Disable mail
                        // deliberately does NOT flip the deployment-wide
                        // `set_mail_enabled` (one user must not turn mail off for
                        // the box). So the kind is `fauna.account.state.put` —
                        // OfflineSafe, the user's own document, theirs to write
                        // offline — exactly as tui rules it
                        // (`settings/mod.rs:3159-3161`).
                        //
                        // linux DOES declare that kind here
                        // (`settings/mail.rs:244`) and android must not: see the
                        // long note on `MailSpamScreen`'s share-reports toggle —
                        // a `faunaGate` that can never desensitize is rejected by
                        // `check-offline-gate-kinds.py`, because unlike linux's
                        // inert registry entry it hands back an `enabled` a
                        // reader would trust.
                        TextButton(
                            onClick = {
                                showDisableConfirm = false
                                onDisableMail()
                            },
                            colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
                            modifier = Modifier.testTag(Ids.MAIL_SETTINGS_DISABLE_CONFIRM_BUTTON),
                        ) {
                            Text(stringResource(R.string.settings_mail_disable_confirm))
                        }
                    },
                    dismissButton = {
                        TextButton(onClick = { showDisableConfirm = false }) {
                            Text(stringResource(R.string.common_cancel))
                        }
                    },
                )
            }

            // ── Status indicator ──
            Text(
                statusLabel(status, enabled),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.MAIL_SETTINGS_STATUS_INDICATOR),
            )

            // ── Pending-rotation banner ──
            if (pendingRotation != null) {
                Card(modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_SETTINGS_PENDING_ROTATION_BANNER)) {
                    Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(stringResource(R.string.settings_mail_banner_title))
                        // Resuming a rotation re-seals the MSEK under the next
                        // credential and provisions the blob — the same
                        // `provision_wrapped_mls_blob` the add-credential submit
                        // and the rotate confirm issue.
                        val resumeGate = faunaGate("fauna.bridges.provision_wrapped_mls_blob")
                        Button(
                            onClick = onResumeRotation,
                            enabled = resumeGate.enabled,
                            modifier = Modifier.testTag(Ids.MAIL_SETTINGS_PENDING_ROTATION_RESUME_BUTTON),
                        ) { Text(stringResource(R.string.settings_mail_resume)) }
                        DisabledControlReasonText(resumeGate.reason)
                    }
                }
            }

            // A provisioned mailbox = email on OR CalDAV on OR CardDAV on OR
            // serving >=1 folder over WebDAV (they enable independently but
            // share one MSEK + one `default` bridge credential — caldav-server.md
            // § Authentication / § Independent enablement; webdav-server.md
            // § Independent enablement pt 1). The predicate is computed once in
            // shared Rust (priority #2/#4: `MailSettingsSnapshot::
            // credential_management_reachable`) instead of re-derived here. The
            // credential-management section + serve-here toggle render for any
            // provisioned mailbox, so a CalDAV/CardDAV/WebDAV-only deployment can
            // obtain/rotate the bridge password its MUA needs; the email-specific
            // rows (IMAP/SMTP connection details + the email sub-pages) stay gated
            // on `enabled`. Mirrors the linux lead
            // (apps/fauna-linux/src/settings/mail.rs:1173);
            // mail-settings.md § CalDAV-only mailbox.
            val mailbox = credentialManagementReachable
            if (!mailbox) {
                Text(
                    stringResource(R.string.settings_mail_section_description),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                // The app passwords themselves are rows of the Connected apps page
                // (connected-apps.md § Architectural rules; mail-settings.md § Where
                // the credential rows render): copy, reveal and disconnect live
                // there, so a password is listed in exactly one place. This page
                // keeps Add password, Rotate keys, the keys explainer and the
                // pointer.
                Text(
                    stringResource(R.string.mail_settings_credentials_on_connected_apps),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )

                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = { form = CredForm.Add },
                        modifier = Modifier.testTag(Ids.MAIL_SETTINGS_ADD_CREDENTIAL_BUTTON),
                    ) { Text(stringResource(R.string.settings_mail_add_credential)) }
                    OutlinedButton(
                        onClick = { showRotate = true },
                        enabled = credentials.isNotEmpty(),
                        modifier = Modifier.testTag(Ids.MAIL_SETTINGS_ROTATE_KEYS_BUTTON),
                    ) { Text(stringResource(R.string.settings_mail_rotate_keys)) }
                }

                // keys-info explainer (i)
                Text(
                    stringResource(R.string.mail_settings_keys_info),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MAIL_SETTINGS_KEYS_INFO),
                )

                // MUA connection details render whenever a mailbox is provisioned
                // (email OR CalDAV OR CardDAV OR WebDAV). Within the one
                // mail-settings-mua-instructions block the rows are per-protocol:
                // the IMAP/SMTP host+port rows describe the *email* protocol →
                // gated on `enabled`; the CalDAV host+port rows describe the
                // *calendar* protocol → gated on `caldavEnabled` (mail.<domain>:443,
                // caldav-server.md § Network exposure); the WebDAV URL row
                // describes the *files* protocol → gated on `servesWebdavSet` (one
                // full collection-root URL, no host/port split — no SRV
                // autodiscovery exists for WebDAV); username-format +
                // auth-mechanism are shared by all → always shown. Mirrors the
                // linux lead (apps/fauna-linux/src/settings/mail.rs:1198);
                // mail-settings.md § CalDAV-only mailbox.
                MuaInstructionsSection(mua, enabled, caldavEnabled, servesWebdavSet)

                // ── Local IMAP/CalDAV-serving toggle ──
                // Whether this nest answers IMAP/CalDAV for the user's mailbox
                // (the private-home-behind-public-relay deployment). User-set,
                // default on; one flag covers IMAP + CalDAV and never gates the
                // user's own in-client reads. Visible for any provisioned mailbox
                // (`mailbox` — it applies to a CalDAV-only mailbox just as much as
                // an email one). Mirrors the Linux lead
                // (apps/fauna-linux/src/settings/mail.rs:1116);
                // mail-settings.md § Local IMAP/CalDAV-serving toggle.
                // A dispatch-on-change toggle IS the commit — it fires the
                // caller-scoped `set_mail_serving_enabled` non-optimistically,
                // so there is no buffer for it to sit behind.
                val serveHereGate = faunaGate("fauna.bridges.set_mail_serving_enabled")
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(stringResource(R.string.mail_settings_serve_here_label), style = MaterialTheme.typography.bodyLarge)
                        Text(
                            stringResource(R.string.mail_settings_serve_here_subtitle),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        DisabledControlReasonText(serveHereGate.reason)
                    }
                    Switch(
                        checked = servingEnabled,
                        onCheckedChange = onSetServingEnabled,
                        enabled = serveHereGate.enabled,
                        modifier = Modifier.testTag(Ids.MAIL_SETTINGS_SERVE_HERE_TOGGLE),
                    )
                }

                // ── Sub-page nav rows (Aliases / Lists / Export / Spam) ──
                // All email-feature sub-pages, so they stay gated on `enabled`
                // (a CalDAV-only mailbox has no email aliases/lists/export/spam).
                if (enabled) {
                    HorizontalDivider()
                    NavRow(stringResource(R.string.mail_aliases_title), onNavAliases)
                    NavRow(stringResource(R.string.mail_lists_title), onNavLists)
                    NavRow(stringResource(R.string.mail_export_title), onNavExport)
                    NavRow(stringResource(R.string.mail_import_title), onNavImport)
                    NavRow(stringResource(R.string.mail_spam_title), onNavSpam)
                }
            }

            // ── Add / enable credential form (inline reveal) ──
            if (form != CredForm.Closed) {
                CredentialForm(
                    isEnable = form == CredForm.Enable,
                    nestEncrypted = nestEncrypted,
                    generatedPassword = generatedPassword,
                    lastToken = lastToken,
                    strengthLabel = strengthLabel,
                    onRegeneratePassword = onRegeneratePassword,
                    onSubmit = { name, kind, auto, pw ->
                        if (form == CredForm.Enable) onEnable(name, kind, auto, pw)
                        else onAddCredential(name, kind, auto, pw)
                        // OAUTHBEARER keeps the form open to show the token once.
                        if (kind == CredentialKind.PLAIN) form = CredForm.Closed
                    },
                    onCancel = { form = CredForm.Closed; onClearToken() },
                )
            }

            // ── Rotate-keys confirm (inline reveal) ──
            if (showRotate) {
                RotateKeysForm(
                    credentials = credentials,
                    status = status,
                    inFlight = rotationInFlight,
                    progressLabel = { remaining ->
                        statusLabel(SettingsStatus.RotationInProgress(remaining), true)
                    },
                    onConfirm = { excluded ->
                        if (rotationInFlight == null) {
                            // The credentials the rotation re-wraps: the live ones not excluded.
                            rotationInFlight = credentials
                                .count { !it.revoked && it.credentialId !in excluded }
                                .toULong()
                            onStartRotation(excluded) {
                                rotationInFlight = null
                                showRotate = false
                            }
                        }
                    },
                    onCancel = { showRotate = false },
                )
            }
        }
    }
}


@Composable
private fun NavRow(label: String, onClick: () -> Unit) {
    ListItem(
        headlineContent = { Text(label) },
        modifier = Modifier.fillMaxWidth().clickable(onClick = onClick),
    )
}

@Composable
private fun MuaInstructionsSection(mua: MuaInstructions, enabled: Boolean, caldavEnabled: Boolean, servesWebdavSet: Boolean) {
    Column(modifier = Modifier.testTag(Ids.MAIL_SETTINGS_MUA_INSTRUCTIONS), verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(stringResource(R.string.settings_mail_mua_title), style = MaterialTheme.typography.titleMedium)
        // Email protocol (IMAP/SMTP) — gated on `enabled`.
        if (enabled) {
            MuaField(R.string.settings_mail_mua_imap_host, mua.imapHost, "mail-settings-mua-imap-host")
            MuaField(R.string.settings_mail_mua_imap_port, mua.imapPort.toString(), "mail-settings-mua-imap-port")
            MuaField(R.string.settings_mail_mua_smtp_host, mua.smtpHost, "mail-settings-mua-smtp-host")
            MuaField(R.string.settings_mail_mua_smtp_port, mua.smtpPort.toString(), "mail-settings-mua-smtp-port")
        }
        // Calendar protocol (CalDAV) — gated on `caldavEnabled`; mail.<domain>:443.
        if (caldavEnabled) {
            MuaField(R.string.settings_mail_mua_caldav_host, mua.caldavHost, "mail-settings-mua-caldav-host")
            MuaField(R.string.settings_mail_mua_caldav_port, mua.caldavPort.toString(), "mail-settings-mua-caldav-port")
        }
        // Files protocol (WebDAV) — one full collection-root URL, not a host/port
        // pair (no SRV autodiscovery exists for WebDAV). Gated on
        // `servesWebdavSet` (this actor serves >=1 folder over WebDAV),
        // independent of email/CalDAV/CardDAV. Mirrors the linux lead
        // (apps/fauna-linux/src/settings/mail.rs:582);
        // mail-settings.md § CalDAV-only mailbox.
        if (servesWebdavSet) {
            MuaField(R.string.settings_mail_mua_webdav_url, mua.webdavUrl, "mail-settings-mua-webdav-url")
        }
        // Shared by all protocols (one credential AUTHs IMAP+SMTP+CalDAV+CardDAV+WebDAV) — always shown.
        MuaField(R.string.settings_mail_mua_username, mua.usernameFormat, "mail-settings-mua-username-format")
        MuaField(R.string.settings_mail_mua_auth, mua.authMechanism, "mail-settings-mua-auth-mechanism")
    }
}

@Composable
private fun MuaField(labelRes: Int, value: String, testId: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(labelRes), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value, style = MaterialTheme.typography.bodySmall, modifier = Modifier.testTag(testId))
    }
}

/** The add/enable credential form (mail-add-credential element set, rendered inline). */
@Composable
private fun CredentialForm(
    isEnable: Boolean,
    nestEncrypted: Boolean,
    generatedPassword: String,
    lastToken: String?,
    strengthLabel: (String) -> String,
    onRegeneratePassword: () -> Unit,
    onSubmit: (String, CredentialKind, Boolean, String) -> Unit,
    onCancel: () -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var isPlain by remember { mutableStateOf(false) }
    var autogenerate by remember { mutableStateOf(true) }
    var manualPassword by remember { mutableStateOf("") }
    var showPassword by remember { mutableStateOf(false) }

    val kind = if (isPlain) CredentialKind.PLAIN else CredentialKind.O_AUTH_BEARER
    val showWeakWarning = isPlain && !autogenerate && nestEncrypted   // mirrors warn_manual_bridge_password

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.settings_mail_add_title), style = MaterialTheme.typography.titleMedium)

            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.settings_mail_name_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ADD_CREDENTIAL_NAME_INPUT),
            )

            // Type selector: checked = PLAIN (a password) vs a bearer token, which
            // is the default (ui.yaml `mail-add-credential-type-selector`;
            // mail-credentials.md § KDF choice). Landing on PLAIN with
            // autogenerate on is the settled mint-once edge (mail-credentials.md
            // § Mint-once sequencing), so the password shown is the one submitted.
            // The whole labelled row is the toggle (the box takes no click of its
            // own): a tap on the label flips it too, as on every other app.
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .toggleable(value = isPlain, role = Role.Checkbox, onValueChange = {
                        isPlain = it; if (it && autogenerate) onRegeneratePassword()
                    })
                    .testTag(Ids.MAIL_ADD_CREDENTIAL_TYPE_SELECTOR),
            ) {
                Checkbox(checked = isPlain, onCheckedChange = null)
                Text(stringResource(R.string.settings_mail_type_selector))
            }

            if (isPlain) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .toggleable(value = autogenerate, role = Role.Switch, onValueChange = {
                            autogenerate = it; if (it) onRegeneratePassword()
                        })
                        .testTag(Ids.MAIL_ADD_CREDENTIAL_AUTOGENERATE_TOGGLE),
                ) {
                    Switch(checked = autogenerate, onCheckedChange = null)
                    Text(stringResource(R.string.settings_mail_autogenerate))
                }
                OutlinedTextField(
                    value = if (autogenerate) generatedPassword else manualPassword,
                    onValueChange = { if (!autogenerate) manualPassword = it },
                    readOnly = autogenerate,
                    label = { Text(stringResource(R.string.settings_mail_password_placeholder)) },
                    singleLine = true,
                    visualTransformation = if (showPassword || autogenerate) VisualTransformation.None else PasswordVisualTransformation(),
                    modifier = Modifier.fillMaxWidth().testTag(Ids.MAIL_ADD_CREDENTIAL_PASSWORD_INPUT),
                )
                if (!autogenerate) {
                    TextButton(
                        onClick = { showPassword = !showPassword },
                        modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_PASSWORD_SHOW_TOGGLE),
                    ) {
                        Text(if (showPassword) stringResource(R.string.settings_mail_hide) else stringResource(R.string.settings_mail_show))
                    }
                    Text(
                        strengthLabel(manualPassword),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_PASSWORD_STRENGTH_METER),
                    )
                    if (showWeakWarning) {
                        Text(
                            stringResource(R.string.settings_mail_weak_password_warning),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_WEAK_PASSWORD_WARNING),
                        )
                    } else {
                        Box(modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_WEAK_PASSWORD_WARNING))
                    }
                }
            } else if (lastToken != null) {
                // OAUTHBEARER token, shown once after submit.
                Text(
                    "${stringResource(R.string.settings_mail_token_warning)}\n$lastToken",
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_TOKEN_DISPLAY),
                )
                CopyButton(
                    testTag = Ids.MAIL_ADD_CREDENTIAL_TOKEN_COPY_BUTTON,
                    text = lastToken,
                    label = stringResource(R.string.settings_mail_copy_token),
                )
            }

            // ⚠ ONE kind across both modes, and that is NOT a collapsed
            // discriminant. `isEnable` changes only the button's LABEL and which
            // callback the page binds; both bottom out in the same ceremony —
            // seal the MSEK under the new credential and provision the blob
            // (`fauna_client_mail_settings::machine`, `enable_mail_*` and
            // `add_credential` both reaching `provision_wrapped_mls_blob`). tui
            // says the same by mapping `MailAddCredentialSubmit` and
            // `MailRotateConfirm` to that one kind
            // (`settings/mod.rs:3147-3149`), and linux declares it on this very
            // button (`settings/mail.rs:464`). Followed the branch to the
            // `request(...)` before deciding, per batch 5's rule.
            //
            // Every field above stays live: name, kind, the autogenerate toggle,
            // the manual password, the regenerate button and the token copy are
            // all buffer or local, and cancel is an escape. Only this commits.
            val submitGate = faunaGate("fauna.bridges.provision_wrapped_mls_blob")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onSubmit(name, kind, autogenerate, manualPassword) },
                    enabled = submitGate.enabled,
                    modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_SUBMIT_BUTTON),
                ) {
                    Text(if (isEnable) stringResource(R.string.settings_mail_submit_enable) else stringResource(R.string.settings_mail_submit_add))
                }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier.testTag(Ids.MAIL_ADD_CREDENTIAL_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.settings_mail_cancel)) }
            }
            DisabledControlReasonText(submitGate.reason)
        }
    }
}

/** The rotate-keys confirm form (mail-rotate-keys-confirm element set, inline). */
@Composable
private fun RotateKeysForm(
    credentials: List<MailCredentialSummary>,
    status: SettingsStatus,
    inFlight: ULong?,
    progressLabel: (ULong) -> String,
    onConfirm: (List<String>) -> Unit,
    onCancel: () -> Unit,
) {
    val excluded = remember { mutableStateMapOf<String, Boolean>() }
    // The snapshot's own RotationInProgress wins; else this form's rotation,
    // painted from the count it re-wraps (the snapshot catches up only when the
    // dispatch returns); else nothing.
    val remaining = (status as? SettingsStatus.RotationInProgress)?.credentialsRemaining ?: inFlight
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.settings_mail_rotate_title), style = MaterialTheme.typography.titleMedium)
            Text(
                stringResource(R.string.settings_mail_rotate_warning),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.MAIL_ROTATE_KEYS_WARNING_TEXT),
            )
            Column(modifier = Modifier.testTag(Ids.MAIL_ROTATE_KEYS_EXCLUDE_LIST)) {
                Text(stringResource(R.string.settings_mail_rotate_exclude_caption), style = MaterialTheme.typography.bodySmall)
                credentials.forEach { c ->
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Checkbox(
                            checked = excluded[c.credentialId] == true,
                            onCheckedChange = { excluded[c.credentialId] = it },
                        )
                        Text(c.displayName)
                    }
                }
            }
            if (remaining != null) {
                Text(
                    progressLabel(remaining),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.MAIL_ROTATE_KEYS_PROGRESS_INDICATOR),
                )
            } else {
                Box(modifier = Modifier.testTag(Ids.MAIL_ROTATE_KEYS_PROGRESS_INDICATOR))
            }
            // The per-credential exclusion checkboxes above are a pure local
            // membership draft — read only when this confirm builds the op, and
            // tui says so in as many words. Confirm commits; cancel escapes.
            val rotateGate = faunaGate("fauna.bridges.provision_wrapped_mls_blob")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onConfirm(excluded.filterValues { it }.keys.toList()) },
                    enabled = rotateGate.enabled && inFlight == null,
                    modifier = Modifier.testTag(Ids.MAIL_ROTATE_KEYS_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.settings_mail_rotate_confirm)) }
                OutlinedButton(
                    onClick = onCancel,
                    enabled = inFlight == null,
                    modifier = Modifier.testTag(Ids.MAIL_ROTATE_KEYS_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.settings_mail_cancel)) }
            }
            DisabledControlReasonText(rotateGate.reason)
        }
    }
}
