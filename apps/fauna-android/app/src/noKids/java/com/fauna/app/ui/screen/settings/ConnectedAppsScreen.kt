package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.semantics.text
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.SuppressScreenCapture
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.ConnectedAppsVM
import kotlinx.coroutines.launch
import social.fauna.generated.Ids
import uniffi.fauna_atproto_settings_machine.ConsentCardRow
import uniffi.fauna_client_connected_apps.BlockedAppRow
import uniffi.fauna_client_connected_apps.ConnectedAppRow
import uniffi.fauna_client_connected_apps.ConnectedAppsSnapshot

/**
 * The "Connected apps" Settings sub-page (`ui.yaml` page `connected-apps`;
 * `docs/goal/ui/connected-apps.md`; rail slot directly after Task delegation,
 * `settings.md` § Navigation model). Four regions, top to bottom:
 *
 * 1. **Requests** — the quiet-push tray: one built consent card per live
 *    request, with Approve / Decline / *Never show requests from this app*.
 *    Painted only while a request is live — never a "no requests" row, which
 *    would train the user to ignore the one place the anti-phishing check
 *    happens.
 * 2. **Connect an app** — the typed-code start: a code field and a submit.
 * 3. **The roster** — one row per connected app, Revoke with an inline confirm;
 *    a mail app-password row also carries its login, kind and secret controls.
 * 4. **Blocked apps** — one row per blocked client with Unblock; painted only
 *    while something is blocked.
 *
 * A paint shell over the shared `ConnectedAppsMachine` ([ConnectedAppsVM]): the
 * roster's composition, the scope words, the class badge key, *lasts-until* and
 * which verb revokes a row are all the machine's, so this file never picks a
 * revoke verb (a row's `key` is opaque) and never words a scope. The consent
 * card is the built card the AT Protocol page painted before the lift
 * (`atproto_settings_consent_*` wording unchanged) — one card, a new start,
 * never a second one.
 *
 * **The lift.** The AT Protocol page's consent card and connected-app rows, the
 * Nostr page's bunker rows and the Mail & Calendar page's app-password rows
 * render HERE and no longer on their old pages — a row moves, it is never
 * shown twice (`connected-apps.md` § Architectural rules). Reference painter:
 * tui `apps/fauna-tui/src/settings/connected_apps.rs`.
 *
 * Stateless [ConnectedAppsContent] is split out for the Robolectric harness
 * (mirrors [TaskDelegationContent]); this wrapper is what the NavHost mounts.
 * `page-heading` is the TopAppBar title, `error-message` the global
 * MessageBanner — fed from the snapshot's own `error`.
 */
@Composable
fun ConnectedAppsScreen(
    navController: NavController,
    vm: ConnectedAppsVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val code by vm.code.collectAsState()
    val revokeArmed by vm.revokeArmed.collectAsState()
    val revealed by vm.revealed.collectAsState()
    val appMessages = LocalAppMessages.current

    // Rows are nest state read on every open (a quiet push raises no event), so
    // every visit re-reads — and starts unread.
    LaunchedEffect(Unit) { vm.visit() }
    val error = localized(snapshot?.error)
    LaunchedEffect(error) { error?.let { appMessages.showError(it) } }

    ConnectedAppsContent(
        snapshot = snapshot,
        code = code,
        revokeArmed = revokeArmed,
        revealed = revealed,
        onBack = { navController.popBackStack() },
        onCodeChange = vm::setCode,
        onSubmitCode = vm::submitCode,
        onResolveRequest = vm::resolveRequest,
        onBlockRequest = vm::blockRequest,
        onUnblock = vm::unblock,
        onArmRevoke = vm::armRevoke,
        onConfirmRevoke = vm::confirmRevoke,
        onCancelRevoke = vm::cancelRevoke,
        onToggleReveal = vm::toggleReveal,
        readSecret = vm::readSecret,
        resolveUsername = vm::resolveUsername,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectedAppsContent(
    snapshot: ConnectedAppsSnapshot?,
    code: String,
    revokeArmed: String?,
    revealed: Map<String, String>,
    onBack: () -> Unit,
    onCodeChange: (String) -> Unit,
    onSubmitCode: () -> Unit,
    onResolveRequest: (consentIdHex: String, approved: Boolean) -> Unit,
    onBlockRequest: (consentIdHex: String) -> Unit,
    onUnblock: (clientId: String) -> Unit,
    onArmRevoke: (key: String) -> Unit,
    onConfirmRevoke: (key: String) -> Unit,
    onCancelRevoke: () -> Unit,
    onToggleReveal: (key: String) -> Unit,
    // The on-demand secret read, for Copy: the secret reaches the clipboard
    // without being painted on a screen someone else can read.
    readSecret: suspend (key: String) -> String?,
    // `resolve_mua_username` over UniFFI — shared Rust resolved everything but
    // `{handle}`, so the paint is one substitution and never a locally built
    // address. Injected so Content stays FFI-free for the Robolectric harness.
    resolveUsername: (String) -> String,
    // `fauna_core::format::format_unix_local_ms` over UniFFI (the one shared
    // timestamp wording — `YYYY-MM-DD HH:MM`, local), injected for the same reason.
    formatTime: (Long) -> String = { ms -> com.fauna.ffi.formatUnixLocalMs(ms) },
) {
    // security.md § On-screen secret exposure, rule 2: a minted, revocable
    // secret suppresses capture for exactly the reveal window. The guard
    // refcounts, so two revealed rows hold it once.
    if (revealed.isNotEmpty()) {
        SuppressScreenCapture()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.connected_apps_title),
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
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(16.dp)
                .testTag(Ids.CONNECTED_APPS),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                stringResource(R.string.connected_apps_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── 1. Requests — only while a request is live ──
            val requests = snapshot?.requests.orEmpty()
            if (requests.isNotEmpty()) {
                SectionHeading(R.string.connected_apps_requests_heading)
                requests.forEach { request ->
                    RequestCard(request, onResolveRequest, onBlockRequest)
                }
            }

            // ── 2. Connect an app ──
            SectionHeading(R.string.connected_apps_connect_heading)
            Text(
                stringResource(R.string.connected_apps_connect_hint),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedTextField(
                value = code,
                onValueChange = onCodeChange,
                placeholder = { Text(stringResource(R.string.connected_apps_connect_placeholder)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.CONNECTED_APPS_CONNECT_CODE),
            )
            // Typing a code claims a live, minutes-long request on the nest, so the
            // submit is an online-only commit; the field itself is a buffer and
            // stays live.
            val connectGate = faunaGate("fauna.oauth.consent.lookup_code")
            Button(
                onClick = onSubmitCode,
                enabled = code.isNotBlank() && connectGate.enabled,
                modifier = Modifier.testTag(Ids.CONNECTED_APPS_CONNECT_SUBMIT),
            ) {
                Text(stringResource(R.string.connected_apps_connect_submit))
            }
            DisabledControlReasonText(connectGate.reason)

            // ── 3. The roster ──
            SectionHeading(R.string.connected_apps_roster_heading)
            // The three-state list: nothing until THIS visit's roster read has
            // returned, then either rows or the empty state (`ui/README.md`
            // § List pages).
            if (snapshot?.loaded == true) {
                if (snapshot.principals.isEmpty()) {
                    Text(
                        stringResource(R.string.connected_apps_empty),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.CONNECTED_APPS_EMPTY),
                    )
                }
                snapshot.principals.forEach { row ->
                    RosterRow(
                        row = row,
                        armed = revokeArmed == row.key,
                        revealedSecret = revealed[row.key],
                        onArmRevoke = onArmRevoke,
                        onConfirmRevoke = onConfirmRevoke,
                        onCancelRevoke = onCancelRevoke,
                        onToggleReveal = onToggleReveal,
                        readSecret = readSecret,
                        resolveUsername = resolveUsername,
                        formatTime = formatTime,
                    )
                }
            }

            // ── 4. Blocked apps — only while something is blocked ──
            val blocked = snapshot?.blocked.orEmpty()
            if (blocked.isNotEmpty()) {
                SectionHeading(R.string.connected_apps_blocked_heading)
                Text(
                    stringResource(R.string.connected_apps_blocked_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                blocked.forEach { BlockedRow(it, onUnblock, formatTime) }
            }
        }
    }
}

@Composable
private fun SectionHeading(res: Int) {
    Text(stringResource(res), style = MaterialTheme.typography.titleSmall)
}

/**
 * One `connected-apps-request-card`: a third-party app is asking to act for this
 * account and is waiting for the answer — the built consent card, moved here
 * from the AT Protocol page with its wording unchanged. Every request the nest
 * lists is painted, an unhinted browser request included (listed to every
 * account by design — the binding code is the user's check).
 *
 * Three things render, each load-bearing: **who is asking** — the *resolved*
 * `clientName` plus `clientId` **verbatim** (never a logo: `logo_uri` never
 * crosses to the app at all, since loading an attacker-named URL here would
 * disclose the user's address to whoever published the client's metadata
 * document); **what it wants** — one line per `scopeDescriptions` entry, worded
 * by the shared `authz::describe_scope` the browser's own consent page renders
 * from (never re-worded here), with the permission set's provenance after the
 * effective list rather than instead of it; **the binding code** — minted by the
 * nest, so this value and the browser's have one origin. The code's raw value
 * rides the node's `stateDescription` (this app's one string-attribute carrier,
 * `AutomationSemantics.attrValue`), so a test compares the VALUE, never the
 * localized prose beside it.
 *
 * ⚠ Both answer controls render unconditionally — **never gated on how close
 * the request is to expiring**: a resolution is reported even past
 * `expires_at`, and the row carries no timestamp to check against, which is
 * what keeps that ruling structural rather than remembered.
 */
@Composable
private fun RequestCard(
    request: ConsentCardRow,
    onResolve: (consentIdHex: String, approved: Boolean) -> Unit,
    onBlock: (consentIdHex: String) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.CONNECTED_APPS_REQUEST_CARD)) {
        Column(modifier = Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(
                stringResource(R.string.atproto_settings_consent_heading),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                request.clientName?.let {
                    stringResourceFmt(R.string.atproto_settings_consent_client, it, request.clientId)
                } ?: stringResourceFmt(R.string.atproto_settings_consent_client_unnamed, request.clientId),
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                stringResource(R.string.atproto_settings_consent_scopes_heading),
                style = MaterialTheme.typography.bodySmall,
            )
            request.scopeDescriptions.forEach { line ->
                Text("  •  $line", style = MaterialTheme.typography.bodySmall)
            }
            request.sets.forEach { set ->
                Text(
                    set.title?.let {
                        stringResourceFmt(R.string.atproto_settings_consent_set_heading, it, set.nsid)
                    } ?: stringResourceFmt(R.string.atproto_settings_consent_set_heading_unnamed, set.nsid),
                    style = MaterialTheme.typography.bodySmall,
                )
                set.details?.let { Text("  $it", style = MaterialTheme.typography.bodySmall) }
                set.memberDescriptions.forEach { line ->
                    Text("  •  $line", style = MaterialTheme.typography.bodySmall)
                }
            }
            Text(
                stringResourceFmt(R.string.atproto_settings_consent_code, request.code),
                modifier = Modifier
                    .testTag(Ids.CONNECTED_APPS_REQUEST_CODE)
                    .semantics { stateDescription = request.code },
                style = MaterialTheme.typography.bodyLarge,
            )
            Text(
                stringResource(R.string.atproto_settings_consent_code_hint),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            // ONE verdict for the pair: both outcomes are the SAME wire call with a
            // different boolean, so they issue one kind and carry one reason. Deny is
            // not a local dismiss — the waiting browser only gets a clean refusal
            // once the answer reaches the nest, which is why the "no" gates
            // alongside the "yes".
            val resolveGate = faunaGate("fauna.bridges.atproto.resolve_consent")
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    onClick = { onResolve(request.consentIdHex, true) },
                    enabled = resolveGate.enabled,
                    modifier = Modifier.testTag(Ids.CONNECTED_APPS_REQUEST_APPROVE),
                ) {
                    Text(stringResource(R.string.atproto_settings_consent_approve_button))
                }
                OutlinedButton(
                    onClick = { onResolve(request.consentIdHex, false) },
                    enabled = resolveGate.enabled,
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.CONNECTED_APPS_REQUEST_DECLINE),
                ) {
                    Text(stringResource(R.string.atproto_settings_consent_deny_button))
                }
            }
            DisabledControlReasonText(resolveGate.reason)
            val blockGate = faunaGate("fauna.oauth.consent.block_client")
            TextButton(
                onClick = { onBlock(request.consentIdHex) },
                enabled = blockGate.enabled,
                modifier = Modifier.testTag(Ids.CONNECTED_APPS_REQUEST_BLOCK),
            ) {
                Text(stringResource(R.string.connected_apps_block))
            }
            DisabledControlReasonText(blockGate.reason)
        }
    }
}

/** The class badge's words — the one per-app half of the grouping key, which the
 *  machine derives. An unknown class paints no badge rather than a guess. */
private fun classLabelRes(cls: String): Int? = when (cls) {
    "remote" -> R.string.connected_apps_class_remote
    "device" -> R.string.connected_apps_class_device
    "wasm" -> R.string.connected_apps_class_wasm
    "container" -> R.string.connected_apps_class_container
    "app_password" -> R.string.connected_apps_class_app_password
    "signer" -> R.string.connected_apps_class_signer
    "oauth" -> R.string.connected_apps_class_oauth
    else -> null
}

/**
 * One `connected-apps-item` roster row. The joined description is the item's own
 * text (the `nostr-bunker-app-item` shape — ui.yaml mints no per-field leaves for
 * the columns every row has), declared on the tagged column rather than merged
 * from descendants, which would fold the leaves' tags into this node; a mail
 * app password's own leaves and the Revoke controls are its children.
 *
 * A burned row (the identity-succession burn) has no leaf of its own: its
 * `Access revoked` words sit directly under the name and the item carries a
 * `revoked="true"` attr (`stateDescription`) the tests read.
 */
@Composable
private fun RosterRow(
    row: ConnectedAppRow,
    armed: Boolean,
    revealedSecret: String?,
    onArmRevoke: (String) -> Unit,
    onConfirmRevoke: (String) -> Unit,
    onCancelRevoke: () -> Unit,
    onToggleReveal: (String) -> Unit,
    readSecret: suspend (String) -> String?,
    resolveUsername: (String) -> String,
    formatTime: (Long) -> String,
) {
    val name = localized(row.name) ?: row.name.key
    val badge = classLabelRes(row.`class`)?.let { stringResource(it) }
    val head = if (badge != null) "$name · $badge" else name
    val burned = row.mail?.revoked == true
    val burnedLine = stringResource(R.string.settings_mail_credential_revoked)
    val publisherLine = if (row.clientId != null && row.publisher != null) {
        stringResourceFmt(R.string.connected_apps_publisher, row.publisher) + " — " + row.clientId
    } else {
        null
    }
    val scopes = row.scopeDescriptions.map { "  • ${localized(it) ?: it.key}" }
    val facts = buildList {
        if (!row.connected) add(stringResource(R.string.connected_apps_not_connected))
        add(stringResourceFmt(R.string.connected_apps_created, formatTime(row.createdAtMillis)))
        add(
            row.lastUsedAtMillis?.let { stringResourceFmt(R.string.connected_apps_last_used, formatTime(it)) }
                ?: stringResource(R.string.connected_apps_never_used),
        )
        add(
            row.lastsUntilMillis?.let { stringResourceFmt(R.string.connected_apps_lasts_until, formatTime(it)) }
                ?: stringResource(R.string.connected_apps_open_ended),
        )
    }.joinToString(" · ")
    val itemText = buildList {
        add(head)
        if (burned) add(burnedLine)
        publisherLine?.let { add(it) }
        addAll(scopes)
        add(facts)
    }.joinToString("\n")

    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier
                .padding(12.dp)
                .fillMaxWidth()
                .testTag(Ids.CONNECTED_APPS_ITEM)
                .semantics {
                    text = AnnotatedString(itemText)
                    if (burned) stateDescription = "true"
                },
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(head, style = MaterialTheme.typography.titleSmall)
            if (burned) {
                Text(
                    burnedLine,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                )
            }
            publisherLine?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            scopes.forEach { Text(it, style = MaterialTheme.typography.bodySmall) }
            Text(
                facts,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            row.mail?.let { mail -> MailLeaves(row.key, mail, revealedSecret, onToggleReveal, readSecret, resolveUsername) }

            if (armed) {
                Text(
                    stringResourceFmt(R.string.connected_apps_revoke_prompt, name),
                    style = MaterialTheme.typography.bodySmall,
                )
                // The confirm is the commit. A mail app password's revoke is a
                // write to this client's own config (offline-safe — the same
                // ruling the Mail & Calendar page's revoke carried), so only the
                // nest-side verbs the machine picks for every other row gate.
                val revokeGate = faunaGate("fauna.principals.revoke")
                val revokeEnabled = row.mail != null || revokeGate.enabled
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(
                        onClick = { onConfirmRevoke(row.key) },
                        enabled = revokeEnabled,
                        colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.CONNECTED_APPS_ITEM_REVOKE_CONFIRM),
                    ) {
                        Text(stringResource(R.string.connected_apps_revoke_confirm))
                    }
                    TextButton(
                        onClick = onCancelRevoke,
                        modifier = Modifier.testTag(Ids.CONNECTED_APPS_ITEM_REVOKE_CANCEL),
                    ) {
                        Text(stringResource(R.string.connected_apps_revoke_cancel))
                    }
                }
                if (row.mail == null) DisabledControlReasonText(revokeGate.reason)
            } else {
                OutlinedButton(
                    onClick = { onArmRevoke(row.key) },
                    colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                    modifier = Modifier.testTag(Ids.CONNECTED_APPS_ITEM_REVOKE),
                ) {
                    Text(stringResource(R.string.connected_apps_revoke))
                }
            }
        }
    }
}

/**
 * The leaves only a mail app-password row carries: kind, login (+ copy) and the
 * secret (+ reveal, copy). ⚠ The hidden secret's text MUST stay EMPTY: the
 * cross-app driver polls it until it turns non-empty and returns that AS the
 * secret, so a mask would pass the reveal test without the on-demand read ever
 * running. The leaf is therefore always present, and only a revealed one holds
 * words.
 */
@Composable
private fun MailLeaves(
    key: String,
    mail: uniffi.fauna_client_connected_apps.MailAppPassword,
    revealedSecret: String?,
    onToggleReveal: (String) -> Unit,
    readSecret: suspend (String) -> String?,
    resolveUsername: (String) -> String,
) {
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current
    val username = resolveUsername(mail.muaUsername)
    Text(
        localized(mail.kind) ?: mail.kind.key,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.testTag(Ids.CONNECTED_APPS_ITEM_TYPE),
    )
    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            username,
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.weight(1f).testTag(Ids.CONNECTED_APPS_ITEM_USERNAME),
        )
        CopyButton(
            testTag = Ids.CONNECTED_APPS_ITEM_COPY_USERNAME,
            text = username,
            label = stringResource(R.string.settings_mail_copy_username),
            outlined = false,
        )
    }
    Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            revealedSecret.orEmpty(),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.weight(1f).testTag(Ids.CONNECTED_APPS_ITEM_SECRET),
        )
        TextButton(
            onClick = { onToggleReveal(key) },
            modifier = Modifier.testTag(Ids.CONNECTED_APPS_ITEM_REVEAL_SECRET),
        ) {
            Text(
                if (revealedSecret != null) stringResource(R.string.settings_mail_hide_secret)
                else stringResource(R.string.settings_mail_reveal_secret),
            )
        }
        // Copy is independent of the reveal toggle: the secret is read on demand
        // at click time, so the text-to-copy is not known up front.
        TextButton(
            onClick = {
                scope.launch { readSecret(key)?.let { clipboard.setText(AnnotatedString(it)) } }
            },
            modifier = Modifier.testTag(Ids.CONNECTED_APPS_ITEM_COPY_SECRET),
        ) {
            Text(stringResource(R.string.settings_mail_copy_secret))
        }
    }
}

/** One `connected-apps-blocked-item`: the client id **verbatim**, as the request
 *  card showed it — nothing here parses it into a host or a name — and when it was
 *  blocked, with Unblock. */
@Composable
private fun BlockedRow(
    blocked: BlockedAppRow,
    onUnblock: (String) -> Unit,
    formatTime: (Long) -> String,
) {
    val since = stringResourceFmt(R.string.connected_apps_blocked_since, formatTime(blocked.blockedAtMillis))
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(
            modifier = Modifier
                .padding(12.dp)
                .fillMaxWidth()
                .testTag(Ids.CONNECTED_APPS_BLOCKED_ITEM)
                .semantics { text = AnnotatedString("${blocked.clientId}\n$since") },
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Text(blocked.clientId, style = MaterialTheme.typography.bodyMedium)
            Text(since, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            TextButton(
                onClick = { onUnblock(blocked.clientId) },
                modifier = Modifier.testTag(Ids.CONNECTED_APPS_BLOCKED_ITEM_UNBLOCK),
            ) {
                Text(stringResource(R.string.connected_apps_unblock))
            }
        }
    }
}
