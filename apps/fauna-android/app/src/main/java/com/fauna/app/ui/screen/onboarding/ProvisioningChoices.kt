package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.util.localized
import com.fauna.ffi.onboarding.OnboardingMachine
import com.fauna.ffi.onboarding.serverTypeAllowedForMail
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText
import com.fauna.app.ui.controls.DropdownMenuItem
import com.fauna.app.ui.controls.OutlinedTextField
import com.fauna.app.ui.controls.ExposedDropdownMenuBox

/**
 * The server choices — location, mail mode, SSH door, server type and update
 * channel — shared by `vps_config` and `nest_provisioning`'s choices region
 * (`onboarding-provisioning.md` § 3d *The choices on the setting-things-up
 * page*: "§ 5's rules ... apply wherever those controls are shown"). One
 * composable, so the two pages cannot drift apart — tui's
 * `wizard::vps_config::choices` shape.
 *
 * [serverControls] paints the provider-dependent controls (location, mail
 * mode, SSH door, server type); the update channel is always painted, since
 * it does not depend on the provider. Every value is read from the machine and
 * every press forwarded to it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ProvisioningChoices(machine: OnboardingMachine, serverControls: Boolean, tick: Long) {
    val vm = ChoicesActions(machine)
    val state = remember(tick) { machine.vpsConfig() }
    val mailOn = remember(tick) { machine.provisionMailModeEnabled() }
    val sshDoorOn = remember(tick) { machine.provisionSshDoorEnabled() }
    val updateChannel = remember(tick) { machine.provisionUpdateChannel() }
    val channelRows = remember(tick) { machine.updateChannelRows() }

    Column {
        if (serverControls) {
            if (state.locations.isNotEmpty()) {
                Spacer(Modifier.height(16.dp))
                var locationPickerExpanded by remember { mutableStateOf(false) }
                val selectedLocation = state.locations.firstOrNull { it.id == state.selectedLocationId }
                ExposedDropdownMenuBox(
                    expanded = locationPickerExpanded,
                    onExpandedChange = { locationPickerExpanded = !locationPickerExpanded },
                ) {
                    OutlinedTextField(
                        value = selectedLocation?.name ?: "",
                        onValueChange = {},
                        readOnly = true,
                        label = { Text(stringResource(R.string.onboarding_vps_config_location_heading)) },
                        trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = locationPickerExpanded) },
                        modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.VPS_LOCATION_PICKER),
                    )
                    ExposedDropdownMenu(
                        expanded = locationPickerExpanded,
                        onDismissRequest = { locationPickerExpanded = false },
                    ) {
                        state.locations.forEach { loc ->
                            DropdownMenuItem(
                                text = { Text(loc.name) },
                                onClick = {
                                    locationPickerExpanded = false
                                    vm.selectLocation(loc.id)
                                },
                                modifier = Modifier.testTag("vps-location-picker[${loc.id}]"),
                            )
                        }
                    }
                }
            }

            // ── Mail-vs-social mode toggle ──────────────────────────────
            // Decided here (before the box boots) so cloud-init knows whether
            // to provision the scanner sidecars + mail ports. Drives the
            // server-type RAM gate just below (mail ON => only >=2 GB plans).
            // Default reads provisionMailModeEnabled() (handle real-domain
            // default); user-driven thereafter. See onboarding.md §5.
            Spacer(Modifier.height(16.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = mailOn,
                    onCheckedChange = { vm.setProvisionMailMode(it) },
                    modifier = Modifier.testTag(Ids.VPS_CONFIG_MAIL_MODE_TOGGLE),
                )
                Column(Modifier.padding(start = 8.dp)) {
                    Text(
                        stringResource(R.string.onboarding_vps_config_mail_mode_label),
                        style = MaterialTheme.typography.titleSmall,
                    )
                    Text(
                        stringResource(R.string.onboarding_vps_config_mail_mode_desc),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            // ── The SSH door ─────────────────────────────────────────────
            // Beside the mail mode and default off; its warning shows only
            // while it is on, directly under it (onboarding-provisioning.md § 5).
            Spacer(Modifier.height(8.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Checkbox(
                    checked = sshDoorOn,
                    onCheckedChange = { vm.setProvisionSshDoor(it) },
                    modifier = Modifier.testTag(Ids.VPS_CONFIG_SSH_DOOR_TOGGLE),
                )
                Text(
                    stringResource(R.string.onboarding_vps_config_ssh_door_label),
                    style = MaterialTheme.typography.titleSmall,
                    modifier = Modifier.padding(start = 8.dp),
                )
            }
            if (sshDoorOn) {
                Text(
                    stringResource(R.string.onboarding_vps_config_ssh_door_warning),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.testTag(Ids.VPS_CONFIG_SSH_DOOR_WARNING),
                )
            }

            // serverTypes are already curated to <= 5 by the machine; the
            // mail-mode RAM gate filters them before indexing so the radio
            // test-ids stay 0-based over the shown set (shared gate helper,
            // identical across all seven apps).
            if (state.serverTypes.isNotEmpty()) {
                Spacer(Modifier.height(16.dp))
                Text(stringResource(R.string.onboarding_vps_config_server_type_radio_legend), style = MaterialTheme.typography.titleSmall)
                state.serverTypes.filter { serverTypeAllowedForMail(it, mailOn) }.forEachIndexed { idx, server ->
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .selectable(
                                selected = state.selectedServerTypeId == server.id,
                                onClick = { vm.selectServerType(server.id) },
                            )
                            .testTag("vps-server-type-radio[$idx]")
                            .semantics {
                                stateDescription = if (state.selectedServerTypeId == server.id) "on" else "off"
                            },
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        RadioButton(
                            selected = state.selectedServerTypeId == server.id,
                            onClick = { vm.selectServerType(server.id) },
                        )
                        Text(server.id)
                    }
                }
            }
        }
        // ── Update channel ──────────────────────────────────────────
        // Which builds the box's updater follows. Outside the provider block on
        // purpose: the choice does not depend on the provider, so it shows
        // before one is picked. One keyed radio per channel
        // (`vps-config-update-channel-row[<id>]`), in the shared enum's order,
        // selection read from the shared getter. See onboarding-provisioning.md §5.
        Spacer(Modifier.height(16.dp))
        Column(Modifier.testTag(Ids.VPS_CONFIG_UPDATE_CHANNEL_ROW)) {
            Text(
                stringResource(R.string.onboarding_vps_config_update_channel_heading),
                style = MaterialTheme.typography.titleSmall,
            )
            channelRows.forEach { row ->
                val channel = row.channel
                // The shared row list (`update_channel_rows`) owns each row's id,
                // label key and description key (the no-build wording when the
                // registry has no build on the channel, else the compat-line
                // wording for Test / Dev while the product major is 0); the
                // screen only resolves the keys.
                val label = localized(LocalizedText(row.labelKey, emptyMap())) ?: ""
                val desc = localized(LocalizedText(row.descriptionKey, emptyMap())) ?: ""
                val selected = channel == updateChannel
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .selectable(selected = selected, onClick = { vm.setUpdateChannel(channel) })
                        .testTag("vps-config-update-channel-row[${row.id}]")
                        .semantics { stateDescription = if (selected) "on" else "off" },
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    RadioButton(selected = selected, onClick = { vm.setUpdateChannel(channel) })
                    Column(Modifier.padding(start = 8.dp)) {
                        Text(label, style = MaterialTheme.typography.titleSmall)
                        Text(
                            desc,
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

/** The presses the choices forward — the same machine calls `VpsConfigVM`
 *  made before the lift. */
private class ChoicesActions(private val m: OnboardingMachine) {
    fun selectLocation(id: String) = m.selectVpsLocation(id)
    fun selectServerType(id: String) = m.selectVpsServerType(id)
    fun setProvisionMailMode(enabled: Boolean) = m.setProvisionMailMode(enabled)
    fun setProvisionSshDoor(enabled: Boolean) = m.setProvisionSshDoor(enabled)
    fun setUpdateChannel(channel: com.fauna.ffi.provisioning.UpdateChannel) = m.setProvisionUpdateChannel(channel)
}

