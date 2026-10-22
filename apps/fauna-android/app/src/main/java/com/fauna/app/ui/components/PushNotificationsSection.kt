package com.fauna.app.ui.components

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.ui.viewmodel.PushNotificationsVM
import social.fauna.generated.Ids

/**
 * Settings → Account → Push notifications (`settings.md` § Push notifications)
 * — the stateful wrapper: the VM, and the Android 13+ `POST_NOTIFICATIONS`
 * prompt, which rides toggle-on (a refusal registers nothing and renders the
 * line). [PushNotificationsContent] is the render surface.
 */
@Composable
fun PushNotificationsSection(vm: PushNotificationsVM = hiltViewModel()) {
    val optedIn by vm.optedIn.collectAsState()
    val busy by vm.busy.collectAsState()
    val line by vm.line.collectAsState()
    val context = LocalContext.current
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) vm.setOptIn(true) else vm.permissionRefused()
    }
    LaunchedEffect(Unit) { vm.load() }

    PushNotificationsContent(
        optedIn = optedIn,
        busy = busy,
        line = line,
        onToggle = { on ->
            val needsPrompt = on && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                PackageManager.PERMISSION_GRANTED
            if (needsPrompt) permission.launch(Manifest.permission.POST_NOTIFICATIONS) else vm.setOptIn(on)
        },
    )
}

/**
 * The section's plain-parameter render: `push-notifications-section` (always
 * present), the `push-notifications-opt-in-toggle` (painted `on`/`off` from
 * the stored bit — the label carries the status, there is no separate status
 * text) and `push-notifications-error` (only on failure).
 */
@Composable
fun PushNotificationsContent(
    optedIn: Boolean,
    busy: Boolean,
    line: PushNotificationsVM.Line?,
    onToggle: (Boolean) -> Unit,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.PUSH_NOTIFICATIONS_SECTION)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(stringResource(R.string.settings_push_notifications_title), style = MaterialTheme.typography.titleMedium)
            Text(
                stringResource(R.string.settings_push_notifications_device_description),
                style = MaterialTheme.typography.bodySmall,
            )
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    stringResource(R.string.settings_push_notifications_opt_in_label),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = optedIn,
                    onCheckedChange = onToggle,
                    enabled = !busy,
                    modifier = Modifier
                        .testTag(Ids.PUSH_NOTIFICATIONS_OPT_IN_TOGGLE)
                        .semantics { stateDescription = if (optedIn) "on" else "off" },
                )
            }
            if (line != null) {
                Spacer(Modifier.height(4.dp))
                Text(
                    stringResource(
                        when (line) {
                            PushNotificationsVM.Line.NO_DISTRIBUTOR -> R.string.settings_push_notifications_no_distributor
                            PushNotificationsVM.Line.PERMISSION_DENIED -> R.string.status_notifications_permission_denied
                            PushNotificationsVM.Line.UPDATE_FAILED -> R.string.settings_push_notifications_update_failed
                        }
                    ),
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.PUSH_NOTIFICATIONS_ERROR),
                )
            }
        }
    }
}
