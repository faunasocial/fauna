package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.navigation.exitAdminToPrimary
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
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
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.faunaFormGate
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.faunaReadGate
import com.fauna.app.ui.util.paintedReason
import com.fauna.app.ui.util.pressAsked
import com.fauna.app.ui.util.formRefusal
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.and
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.localized
import uniffi.fauna_core.Possibility
import com.fauna.app.ui.viewmodel.AdminCalendarVM
import com.fauna.app.ui.viewmodel.AdminEntryRereadEffect
import uniffi.fauna_client_mail_settings.CaldavPolicyStatus
import social.fauna.generated.Ids
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.OutlinedTextField

/**
 * The flat admin `admin-calendar` page (`admin.md` § 8 Calendar; `caldav-server.md`
 * § Independent enablement) — the deployment-wide **CalDAV-enable** toggle, the
 * direct sibling of `admin-mail`'s mail-enable toggle. Email and calendar are two
 * independently enableable features of one MDA bridge (the MDA runs iff
 * `mail_enabled || caldav_enabled`), so each gets its own admin enable toggle.
 *
 * Stateless [AdminCalendarContent] is split out for the Compose test harness; the
 * VM-bound [AdminCalendarScreen] is the wrapper the NavHost mounts as an admin
 * sub-page. Dumb renderer of the shared `CaldavPolicyMachine`
 * (libs/fauna-client-mail-settings, over UniFFI) — no policy logic in the shell
 * (priority #2). The toggle reads `caldav_enabled` from the Admin read twin
 * `get_mail_config` and writes via `set_caldav_enabled` (both live; no nest work);
 * a nest rejection surfaces via the global error-message banner, never faked green.
 * Mirrors the Linux lead (apps/fauna-linux/src/settings/admin_calendar.rs).
 */
@Composable
fun AdminCalendarScreen(
    navController: NavController,
    vm: AdminCalendarVM = hiltViewModel(),
) {
    AdminEntryRereadEffect(vm.entryReread)
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val toggleAnswer by vm.toggleAnswer.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminCalendarContent(
        caldavEnabled = snapshot.caldavEnabled,
        caldavPort = snapshot.caldavPort.toInt(),
        working = snapshot.status == CaldavPolicyStatus.WORKING,
        onBack = { navController.exitAdminToPrimary() },
        onSetCaldavEnabled = vm::setCaldavEnabled,
        onSavePort = vm::setCaldavPort,
        onPortRefused = appMessages::showError,
        toggleAnswer = toggleAnswer,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminCalendarContent(
    caldavEnabled: Boolean,
    caldavPort: Int,
    working: Boolean,
    onBack: () -> Unit,
    onSetCaldavEnabled: (Boolean) -> Unit,
    onSavePort: (Int) -> Unit,
    // A refused port save's reason, for the global `error-message` banner.
    onPortRefused: (String) -> Unit,
    // The shared `parse_port` validator (u16 in [1, 65535], reject 0), injected so
    // the Robolectric content-test stays FFI-free (mirrors AttendeeRow's `view`).
    parsePort: (String) -> Int? = { com.fauna.ffi.parsePort(it)?.toInt() },
    // The port save's draft answer — the shared
    // `CaldavPolicyMachine::port_draft_possibility` (admin.md § Errors & edge
    // cases, the form bullet). Painted and asked again at the press. Never
    // blocks on a domain deployment: the port is inert there yet saveable.
    portDraft: (String) -> Possibility = { askForm { com.fauna.ffi.caldavPortDraftPossibility(it) } },
    // The toggle's and the port save's answer — the shared
    // `CaldavPolicyMachine::toggle_possibility` (admin.md § Errors & edge
    // cases, *Controls wait for their read*): blocked until `get_mail_config`
    // resolves, after a failed read, and while a change is on its way. Painted,
    // and asked again at the press (refused onto [onPortRefused]).
    toggleAnswer: Possibility = Possibility.Possible,
) {
    val context = LocalContext.current
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_calendar_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_CALENDAR_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.ADMIN_NAV_BACK),
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
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
                stringResource(R.string.admin_calendar_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── CalDAV enable (deployment-wide master toggle; sibling of mail enable) ──
            // A dispatch-on-change toggle IS the commit — no buffer, no save.
            val caldavGate = faunaReadGate(
                toggleAnswer,
                faunaGate("fauna.bridges.set_caldav_enabled", enabled = !working && toggleAnswer.isPossible),
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.admin_calendar_page_enabled_label),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        stringResource(R.string.admin_calendar_page_enabled_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    DisabledControlReasonText(caldavGate.reason)
                }
                Switch(
                    checked = caldavEnabled,
                    onCheckedChange = { on -> pressAsked(context, toggleAnswer, onPortRefused) { onSetCaldavEnabled(on) } },
                    enabled = caldavGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_CALENDAR_ENABLED_TOGGLE).paintedReason(caldavGate.reason),
                )
            }

            // ── Admin-set CalDAV port (set_caldav_port) ──
            // Governs only the router-less direct listener (desktop-native / bare-IP
            // / domainless box); inert on a domain deployment, where CalDAV serves at
            // mail.<domain>:443 via the SNI router (caldav-server.md § Network exposure).
            CaldavPortField(
                caldavPort = caldavPort,
                working = working,
                onSavePort = onSavePort,
                onPortRefused = onPortRefused,
                parsePort = parsePort,
                portDraft = portDraft,
                readAnswer = toggleAnswer,
            )
        }
    }
}

/**
 * The admin-set CalDAV listener port (`admin-calendar-caldav-port-input` +
 * `admin-calendar-caldav-port-save-button`). Holds the in-progress edit string
 * locally (re-seeded whenever the persisted [caldavPort] changes). The save is
 * painted from the shared port draft answer ([portDraft]) and asks it again at
 * the press: a malformed port is refused with its reason via [onPortRefused]
 * (the global error banner) and nothing is sent; otherwise it dispatches
 * `set_caldav_port` via [onSavePort].
 */
@Composable
private fun CaldavPortField(
    caldavPort: Int,
    working: Boolean,
    onSavePort: (Int) -> Unit,
    onPortRefused: (String) -> Unit,
    parsePort: (String) -> Int?,
    portDraft: (String) -> Possibility,
    readAnswer: Possibility,
) {
    val context = LocalContext.current
    var edited by remember(caldavPort) { mutableStateOf(caldavPort.toString()) }

    // Unlike the spam page's threshold override — where the field IS the commit,
    // because its IME action dispatches — this port has a separate Save button.
    // So the ordinary split applies: the input is a pure buffer and stays live
    // with no nest, only Save declares. Same shape as the admin-nest serving-port
    // save the admin-nest batch landed.
    // The read first (a page still loading says so before any draft
    // complaint), then the draft: the port shown is the read's, so a save over
    // an unread value would send a guess.
    val draft = portDraft(edited)
    val portGate = faunaReadGate(
        readAnswer,
        faunaGate("fauna.bridges.set_caldav_port", enabled = !working && (readAnswer and draft).isPossible),
        draft,
    )
    Row(
        modifier = Modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            OutlinedTextField(
                value = edited,
                onValueChange = { edited = it.filter(Char::isDigit) },
                label = { Text(stringResource(R.string.admin_calendar_page_caldav_port_label)) },
                supportingText = {
                    Text(
                        stringResource(R.string.admin_calendar_page_caldav_port_desc),
                        modifier = Modifier.testTag(Ids.ADMIN_CALENDAR_CALDAV_PORT_HELP_TEXT),
                    )
                },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                enabled = !working && readAnswer.isPossible,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_CALENDAR_CALDAV_PORT_INPUT)
                    .paintedReason(localized(readAnswer.blockedReason)),
            )
            DisabledControlReasonText(portGate.reason)
        }
        Button(
            onClick = {
                val refusal = formRefusal(context, readAnswer and portDraft(edited))
                val port = parsePort(edited)
                when {
                    refusal != null -> refusal.reason?.let(onPortRefused)
                    port != null -> onSavePort(port)
                }
            },
            enabled = portGate.enabled,
            modifier = Modifier.testTag(Ids.ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON).paintedReason(portGate.reason),
        ) { Text(stringResource(R.string.admin_calendar_page_caldav_port_save)) }
    }
}
