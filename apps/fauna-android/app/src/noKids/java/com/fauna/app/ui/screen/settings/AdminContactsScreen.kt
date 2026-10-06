package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.AdminContactsVM
import uniffi.fauna_client_mail_settings.CarddavPolicyStatus
import social.fauna.generated.Ids

/**
 * The flat admin `admin-contacts` page (`admin.md` § Contacts;
 * `carddav-server.md` § Independent enablement) — the deployment-wide
 * **CardDAV-enable** toggle, the contacts sibling of `admin-calendar`'s
 * CalDAV-enable toggle. Email, calendar, and contacts are three independently
 * enableable features of one MDA bridge (the MDA runs iff
 * `mail_enabled || caldav_enabled || carddav_enabled`), so each gets its own
 * admin enable toggle. No port field — CardDAV rides the shared DAV listener
 * admin-calendar's port input governs.
 *
 * Stateless [AdminContactsContent] is split out for the Compose test harness;
 * the VM-bound [AdminContactsScreen] is the wrapper the NavHost mounts as an
 * admin sub-page. Dumb renderer of the shared `CarddavPolicyMachine`
 * (libs/fauna-client-mail-settings, over UniFFI) — no policy logic in the shell
 * (priority #2). Mirrors the Linux lead
 * (apps/fauna-linux/src/settings/admin_contacts.rs).
 */
@Composable
fun AdminContactsScreen(
    navController: NavController,
    vm: AdminContactsVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminContactsContent(
        carddavEnabled = snapshot.carddavEnabled,
        working = snapshot.status == CarddavPolicyStatus.WORKING,
        onBack = { navController.popBackStack() },
        onSetCarddavEnabled = vm::setCarddavEnabled,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminContactsContent(
    carddavEnabled: Boolean,
    working: Boolean,
    onBack: () -> Unit,
    onSetCarddavEnabled: (Boolean) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_contacts_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_CONTACTS_HEADING),
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
                stringResource(R.string.admin_contacts_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── CardDAV enable (deployment-wide master toggle; contacts sibling
            //    of the calendar enable) ──
            // A dispatch-on-change toggle IS the commit — no buffer, no save.
            val carddavGate = faunaGate("fauna.bridges.set_carddav_enabled", enabled = !working)
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.admin_contacts_page_enabled_label),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        stringResource(R.string.admin_contacts_page_enabled_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    DisabledControlReasonText(carddavGate.reason)
                }
                Switch(
                    checked = carddavEnabled,
                    onCheckedChange = onSetCarddavEnabled,
                    enabled = carddavGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_CONTACTS_CARDDAV_ENABLED_TOGGLE),
                )
            }
        }
    }
}
