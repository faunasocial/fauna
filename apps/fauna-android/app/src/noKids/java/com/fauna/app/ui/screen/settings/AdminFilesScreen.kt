package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.navigation.exitAdminToPrimary
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
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.faunaReadGate
import com.fauna.app.ui.util.paintedReason
import com.fauna.app.ui.util.pressAsked
import androidx.compose.ui.platform.LocalContext
import uniffi.fauna_core.Possibility
import com.fauna.app.ui.viewmodel.AdminFilesVM
import com.fauna.app.ui.viewmodel.AdminEntryRereadEffect
import uniffi.fauna_client_mail_settings.WebdavPolicyStatus
import social.fauna.generated.Ids

/**
 * The flat admin `admin-files` page (`admin.md` § Files;
 * `webdav-server.md` § Independent enablement) — the deployment-wide
 * **WebDAV-enable** toggle, the files sibling of `admin-contacts`'s
 * CardDAV-enable toggle. Email, calendar, contacts, and files are four
 * independently enableable features of one MDA bridge (the MDA runs iff
 * `mail_enabled || caldav_enabled || carddav_enabled || webdav_enabled`), so
 * each gets its own admin enable toggle. No port field — WebDAV rides the
 * shared DAV listener admin-calendar's port input governs.
 *
 * Stateless [AdminFilesContent] is split out for the Compose test harness;
 * the VM-bound [AdminFilesScreen] is the wrapper the NavHost mounts as an
 * admin sub-page. Dumb renderer of the shared `WebdavPolicyMachine`
 * (libs/fauna-client-mail-settings, over UniFFI) — no policy logic in the shell
 * (priority #2). Mirrors the Linux lead
 * (apps/fauna-linux/src/settings/admin_files.rs).
 */
@Composable
fun AdminFilesScreen(
    navController: NavController,
    vm: AdminFilesVM = hiltViewModel(),
) {
    AdminEntryRereadEffect(vm.entryReread)
    val snapshot by vm.snapshot.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val toggleAnswer by vm.toggleAnswer.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminFilesContent(
        webdavEnabled = snapshot.webdavEnabled,
        working = snapshot.status == WebdavPolicyStatus.WORKING,
        onBack = { navController.exitAdminToPrimary() },
        onSetWebdavEnabled = vm::setWebdavEnabled,
        toggleAnswer = toggleAnswer,
        onRefused = appMessages::showError,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminFilesContent(
    webdavEnabled: Boolean,
    working: Boolean,
    onBack: () -> Unit,
    onSetWebdavEnabled: (Boolean) -> Unit,
    // The toggle's answer — the shared `WebdavPolicyMachine::toggle_possibility`
    // (admin.md § Errors & edge cases, *Controls wait for their read*): blocked
    // until `get_mail_config` resolves, after a failed read, and while its own
    // change is on its way. Painted, and asked again at the press.
    toggleAnswer: Possibility = Possibility.Possible,
    // A refused press's reason, for the global `error-message` banner.
    onRefused: (String) -> Unit = {},
) {
    val context = LocalContext.current
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.admin_files_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_FILES_HEADING),
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
                stringResource(R.string.admin_files_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── WebDAV enable (deployment-wide master toggle; files sibling
            //    of the contacts enable) ──
            // A dispatch-on-change toggle IS the commit — no buffer, no save.
            val webdavGate = faunaReadGate(
                toggleAnswer,
                faunaGate("fauna.bridges.set_webdav_enabled", enabled = !working && toggleAnswer.isPossible),
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.admin_files_page_enabled_label),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        stringResource(R.string.admin_files_page_enabled_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    DisabledControlReasonText(webdavGate.reason)
                }
                Switch(
                    checked = webdavEnabled,
                    onCheckedChange = { on -> pressAsked(context, toggleAnswer, onRefused) { onSetWebdavEnabled(on) } },
                    enabled = webdavGate.enabled,
                    modifier = Modifier.testTag(Ids.ADMIN_FILES_WEBDAV_ENABLED_TOGGLE).paintedReason(webdavGate.reason),
                )
            }
        }
    }
}
