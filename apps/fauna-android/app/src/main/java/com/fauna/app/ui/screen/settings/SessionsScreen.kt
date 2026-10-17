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
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.SessionsUiState
import com.fauna.app.ui.viewmodel.SessionsVM
import com.fauna.ffi.FfiSessionRow
import social.fauna.generated.Ids

/**
 * Settings → **Sessions** (`docs/goal/ui/sessions.md`; ui.yaml page `sessions`;
 * the rail slot directly after Devices, `settings.md` § Navigation model) —
 * where the account is signed in right now, as one escalation ladder: the list,
 * sign out everywhere else, the 24-hour lock. The rows, their order, their
 * names and their detail lines are the shared fold's; [SessionsContent] only
 * paints them. A lock that lands calls [onLocked], which leaves the shell for
 * the launch surface (leg 2). Twin of linux `settings/sessions.rs` and tui's.
 */
@Composable
fun SessionsScreen(
    navController: NavController,
    onLocked: () -> Unit,
    vm: SessionsVM = hiltViewModel(),
) {
    val state by vm.state.collectAsState()
    LaunchedEffect(Unit) { vm.hydrate(onLocked) }
    SessionsContent(
        state = state,
        lockConfirmWord = vm.lockConfirmWord,
        onBack = { navController.popBackStack() },
        onRevoke = { vm.revoke(it, onLocked) },
        onArmRevokeOthers = vm::armRevokeOthers,
        onCancelRevokeOthers = vm::cancelRevokeOthers,
        onConfirmRevokeOthers = { vm.confirmRevokeOthers(onLocked) },
        onLockWordChange = vm::onLockWordChange,
        onLock = { vm.lock(onLocked) },
    )
}

/**
 * The stateless render — plain parameters, so it renders under Robolectric with
 * no Hilt, no VM and no FFI. Every ID is ui.yaml's `sessions` page; the list's
 * are indexed by position (`session-card[i]` and its descendants).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SessionsContent(
    state: SessionsUiState,
    lockConfirmWord: String,
    onBack: () -> Unit,
    onRevoke: (String) -> Unit,
    onArmRevokeOthers: () -> Unit,
    onCancelRevokeOthers: () -> Unit,
    onConfirmRevokeOthers: () -> Unit,
    onLockWordChange: (String) -> Unit,
    onLock: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.sessions_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack, modifier = Modifier.testTag(Ids.SETTINGS_NAV_BACK)) {
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
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp, vertical = 8.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            localized(state.snapshot?.error)?.let {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
                )
            }

            // 1. The list. `sessions-empty` is the fail-safe for a reply that
            // carries no rows — never painted before the first read lands.
            val rows = state.snapshot?.rows.orEmpty()
            if (state.snapshot != null && rows.isEmpty()) {
                Text(
                    stringResource(R.string.sessions_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.testTag(Ids.SESSIONS_EMPTY),
                )
            }
            rows.forEach { row ->
                SessionCard(row = row, enabled = !state.busy, onRevoke = { onRevoke(row.tokenId) })
            }

            HorizontalDivider(Modifier.padding(vertical = 8.dp))

            // 2. Sign out everywhere else — the two-press inline confirm, with the
            // required note on what revoking can and cannot do beside it.
            if (state.revokeOthersArmed) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = onConfirmRevokeOthers,
                        enabled = !state.busy,
                        colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                        modifier = Modifier.testTag(Ids.SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON),
                    ) { Text(stringResource(R.string.sessions_revoke_others_confirm)) }
                    OutlinedButton(
                        onClick = onCancelRevokeOthers,
                        modifier = Modifier.testTag(Ids.SESSIONS_REVOKE_OTHERS_CANCEL_BUTTON),
                    ) { Text(stringResource(R.string.sessions_revoke_others_cancel)) }
                }
            } else {
                OutlinedButton(
                    onClick = onArmRevokeOthers,
                    enabled = !state.busy,
                    modifier = Modifier.testTag(Ids.SESSIONS_REVOKE_OTHERS_BUTTON),
                ) { Text(stringResource(R.string.sessions_revoke_others)) }
            }
            Text(
                stringResource(R.string.sessions_revoke_note),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.SESSIONS_REVOKE_NOTE),
            )

            HorizontalDivider(Modifier.padding(vertical = 8.dp))

            // 3. The 24-hour lock. The warning is REQUIRED copy and always
            // visible — never behind the confirm; the word is typed, `LOCK`.
            Text(
                stringResource(R.string.sessions_lockout_warning),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.SESSIONS_LOCKOUT_WARNING),
            )
            OutlinedTextField(
                value = state.lockWord,
                onValueChange = onLockWordChange,
                singleLine = true,
                placeholder = { Text(stringResource(R.string.sessions_lockout_confirm_placeholder)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.SESSIONS_LOCKOUT_CONFIRM_FIELD),
            )
            Button(
                onClick = onLock,
                enabled = !state.busy && state.lockWord == lockConfirmWord,
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.testTag(Ids.SESSIONS_LOCKOUT_BUTTON),
            ) { Text(stringResource(R.string.sessions_lockout_button)) }
        }
    }
}

@Composable
private fun SessionCard(row: FfiSessionRow, enabled: Boolean, onRevoke: () -> Unit) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SESSION_CARD)) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    localized(row.kind).orEmpty(),
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.weight(1f).testTag(Ids.SESSION_KIND),
                )
                localized(row.mark)?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.primary,
                        modifier = Modifier.testTag(Ids.SESSION_THIS_MARK_BADGE),
                    )
                }
            }
            Text(
                localized(row.detail).orEmpty(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.SESSION_DETAIL),
            )
            // Absent on this app's own row — leaving this app is Sign out.
            if (row.canRevoke) {
                TextButton(
                    onClick = onRevoke,
                    enabled = enabled,
                    modifier = Modifier.testTag(Ids.SESSION_REVOKE_BUTTON),
                ) { Text(stringResource(R.string.sessions_revoke)) }
            }
        }
    }
}
