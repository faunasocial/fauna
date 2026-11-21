package com.fauna.app.ui.screen.settings

import com.fauna.app.ui.navigation.exitAdminToPrimary
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.ActorOption
import com.fauna.app.ui.viewmodel.AdminWebVM
import com.fauna.app.ui.viewmodel.AdminEntryRereadEffect
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import com.fauna.app.ui.controls.DropdownMenuItem
import com.fauna.app.ui.controls.OutlinedTextField
import com.fauna.app.ui.controls.ExposedDropdownMenuBox

/**
 * The admin `admin-web` page (`web-content-hosting.md` § Admin apex hosting): the
 * deployment apex-actor picker (`admin-web-apex-actor-select`) — an Admin-class
 * designation of which actor's `web` content serves at `https://<domain>/`, "none"
 * clearing it to the built-in info page. The direct analogue of the per-domain
 * catch-all mail actor (`admin-dns-domain-catch-all-select`), built the same way.
 * Per-user subdomain hosting is the user `web-settings` page, not here.
 *
 * Stateless [AdminWebContent] is split out for the Compose test harness; the
 * VM-bound [AdminWebScreen] is the wrapper the NavHost mounts as an admin sub-page.
 * Dumb renderer of the shared `fauna.web.*` kinds (priority #2) — no apex logic in
 * the shell. Mirrors the Linux lead (apps/fauna-linux/src/settings/admin_web.rs);
 * `admin-web-heading` is the page landmark, `error-message` the global MessageBanner.
 */
@Composable
fun AdminWebScreen(
    navController: NavController,
    vm: AdminWebVM = hiltViewModel(),
) {
    AdminEntryRereadEffect(vm.entryReread)
    val currentActorHex by vm.currentActorHex.collectAsState()
    val actors by vm.actors.collectAsState()
    val apexInfo by vm.apexInfo.collectAsState()
    val readState by vm.readState.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    AdminWebContent(
        currentActorHex = currentActorHex,
        actors = actors,
        apexInfo = apexInfo,
        // Not enabled before the read lands, nor after the apex read failed
        // (rule 5, admin.md § 7 Web): the read alone decides.
        pickerAnswer = askForm { com.fauna.ffi.readPossibility(readState) },
        onBack = { navController.exitAdminToPrimary() },
        onSetApex = vm::setApex,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AdminWebContent(
    currentActorHex: String?,
    actors: List<ActorOption>,
    apexInfo: LocalizedText?,
    pickerAnswer: Possibility,
    onBack: () -> Unit,
    onSetApex: (String?) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                // admin-web has no `page-heading`; its landmark element is
                // `admin-web-heading` (ui.yaml admin-web). Mirrors the linux page.
                title = {
                    Text(
                        stringResource(R.string.admin_web_page_title),
                        modifier = Modifier.testTag(Ids.ADMIN_WEB_HEADING),
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
                stringResource(R.string.admin_web_page_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            // ── Apex-actor picker ("none" clears → info page) ──
            Text(
                stringResource(R.string.admin_web_page_apex_select_subtitle),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            ApexActorPicker(
                current = currentActorHex,
                actors = actors,
                answer = pickerAnswer,
                onSetApex = onSetApex,
            )

            // ── Apex-URL explainer ──
            Text(
                // Where the apex serves, or why there is no address — the
                // shared `web_apex_info_text`, never a composed `https:///`.
                apexInfo?.let { localized(it) } ?: "",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.ADMIN_WEB_APEX_INFO),
            )
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ApexActorPicker(
    current: String?,
    actors: List<ActorOption>,
    answer: Possibility,
    onSetApex: (String?) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val noneLabel = stringResource(R.string.admin_web_page_apex_none)
    // A current designation not among the loaded actors stays visible by its hex.
    val selectedLabel = current?.let { hex ->
        actors.firstOrNull { it.idHex == hex }?.label ?: hex
    } ?: noneLabel

    // Dispatch-on-pick: each menu item calls `onSetApex` directly, so this
    // select IS the commit for `fauna.web.set_apex_actor` — there is no Save to
    // carry the declaration. Desensitizing the anchor field closes the whole
    // control, menu included, because the menu only opens from this anchor.
    val apexGate = faunaGate("fauna.web.set_apex_actor", enabled = answer.isPossible)
    ExposedDropdownMenuBox(
        expanded = expanded && apexGate.enabled,
        onExpandedChange = { if (apexGate.enabled) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = selectedLabel,
            onValueChange = {},
            readOnly = true,
            enabled = apexGate.enabled,
            label = { Text(stringResource(R.string.admin_web_page_apex_select_label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(Ids.ADMIN_WEB_APEX_ACTOR_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(noneLabel) },
                onClick = { onSetApex(null); expanded = false },
            )
            actors.forEach { actor ->
                DropdownMenuItem(
                    text = { Text(actor.label) },
                    onClick = { onSetApex(actor.idHex); expanded = false },
                )
            }
        }
    }
    // The read's own reason is the more specific one, so it wins.
    DisabledControlReasonText(answer.blockedReason?.let { localized(it) } ?: apexGate.reason)
}
