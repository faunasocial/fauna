package com.fauna.app.ui.screen.settings

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
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.AdminCalendarVM
import uniffi.fauna_client_mail_settings.CaldavPolicyStatus
import social.fauna.generated.Ids

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
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    // The port is client-side validated via the shared `parsePort` (u16 in
    // [1, 65535], reject 0) before dispatch; a bad value surfaces via the global banner.
    val invalidPortMessage = stringResource(R.string.admin_calendar_page_caldav_port_invalid)

    AdminCalendarContent(
        caldavEnabled = snapshot.caldavEnabled,
        caldavPort = snapshot.caldavPort.toInt(),
        working = snapshot.status == CaldavPolicyStatus.WORKING,
        onBack = { navController.popBackStack() },
        onSetCaldavEnabled = vm::setCaldavEnabled,
        onSavePort = vm::setCaldavPort,
        onInvalidPort = { appMessages.showError(invalidPortMessage) },
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
    onInvalidPort: () -> Unit,
    // The shared `parse_port` validator (u16 in [1, 65535], reject 0), injected so
    // the Robolectric content-test stays FFI-free (mirrors AttendeeRow's `view`).
    parsePort: (String) -> Int? = { com.fauna.ffi.parsePort(it)?.toInt() },
) {
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
            val caldavGate = faunaGate("fauna.bridges.set_caldav_enabled", enabled = !working)
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
                    onCheckedChange = onSetCaldavEnabled,
                    enabled = caldavGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_CALENDAR_ENABLED_TOGGLE),
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
                onInvalidPort = onInvalidPort,
                parsePort = parsePort,
            )
        }
    }
}

/**
 * The admin-set CalDAV listener port (`admin-calendar-caldav-port-input` +
 * `admin-calendar-caldav-port-save-button`). Holds the in-progress edit string
 * locally (re-seeded whenever the persisted [caldavPort] changes), validates the
 * port on save via the shared `parsePort` (u16 in `[1, 65535]`), then dispatches
 * `set_caldav_port` via [onSavePort] — or surfaces the invalid-port message via
 * [onInvalidPort] (the global error banner).
 */
@Composable
private fun CaldavPortField(
    caldavPort: Int,
    working: Boolean,
    onSavePort: (Int) -> Unit,
    onInvalidPort: () -> Unit,
    parsePort: (String) -> Int?,
) {
    var edited by remember(caldavPort) { mutableStateOf(caldavPort.toString()) }

    // Unlike the spam page's threshold override — where the field IS the commit,
    // because its IME action dispatches — this port has a separate Save button.
    // So the ordinary split applies: the input is a pure buffer and stays live
    // with no nest, only Save declares. Same shape as the admin-nest serving-port
    // save the admin-nest batch landed.
    val portGate = faunaGate("fauna.bridges.set_caldav_port", enabled = !working)
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
                supportingText = { Text(stringResource(R.string.admin_calendar_page_caldav_port_desc)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                enabled = !working,
                modifier = Modifier.fillMaxWidth().testTag(Ids.ADMIN_CALENDAR_CALDAV_PORT_INPUT),
            )
            DisabledControlReasonText(portGate.reason)
        }
        Button(
            onClick = {
                val port = parsePort(edited)
                if (port == null) {
                    onInvalidPort()
                } else {
                    onSavePort(port)
                }
            },
            enabled = portGate.enabled,
            modifier = Modifier.testTag(Ids.ADMIN_CALENDAR_CALDAV_PORT_SAVE_BUTTON),
        ) { Text(stringResource(R.string.admin_calendar_page_caldav_port_save)) }
    }
}
