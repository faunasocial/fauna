package com.fauna.app.ui.screen.profile

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import com.fauna.app.R
import com.fauna.app.core.ExifStripper
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.viewmodel.ProfileEditVM
import com.fauna.ffi.FfiProfileLink
import social.fauna.generated.Ids

/**
 * The profile **edit form** (SELF view) per `profile.md` § Where logic lives →
 * *Profile publish/edit*. The user edits `display_name` / `bio` / a repeatable
 * list of links, plus the `avatar`/`banner` pictures; Save runs the shared
 * `fauna-client-profile::build_edited_profile_with_images` (sign) →
 * `fauna.profile.set`. Lifts the linux lead
 * (`apps/fauna-linux/src/views/profile/edit.rs`) onto Compose (priority #1
 * uniform; same ui.yaml IDs).
 *
 * `profile-edit-avatar`/`profile-edit-banner` use Android's real content
 * picker (`ActivityResultContracts.GetContent()`, the same platform API
 * `FeedComposeScreen`'s `compose-file` already uses) — bytes are read via
 * [android.content.ContentResolver], EXIF-stripped, and uploaded **eagerly on
 * pick** (matching `compose-file`'s own timing, not linux/tui's defer-to-Save
 * shape), staging the resulting hash on the VM.
 *
 * FFI-free [ProfileEditFormContent] is split out for the Robolectric harness;
 * the VM-bound [ProfileEditFormSection] is what [ProfileScreen] mounts. The form
 * is revealed only after the read-modify-write fetch returns (matching linux's
 * async `open_form`), so the e2e `profile-edit-form` wait lands on the populated
 * form.
 */
@Composable
fun ProfileEditFormSection(
    actorIdHex: String?,
    onSaved: () -> Unit,
    onCancel: () -> Unit,
    vm: ProfileEditVM = hiltViewModel(),
) {
    val state by vm.state.collectAsState()
    val saving by vm.saving.collectAsState()
    val avatarUploading by vm.avatarUploading.collectAsState()
    val bannerUploading by vm.bannerUploading.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(Unit) { vm.open(actorIdHex) }
    LaunchedEffect(errorMessage) { errorMessage?.let { appMessages.showError(it) } }

    // Real content picker for each picture field (mirrors FeedComposeScreen's
    // compose-file launcher): read bytes via ContentResolver, EXIF-strip, then
    // upload immediately so a Save later that day only signs the resulting hash.
    val avatarPicker = rememberLauncherForActivityResult(ActivityResultContracts.GetContent()) { uri ->
        uri ?: return@rememberLauncherForActivityResult
        val mimeType = context.contentResolver.getType(uri) ?: "application/octet-stream"
        val rawBytes = context.contentResolver.openInputStream(uri)?.readBytes() ?: return@rememberLauncherForActivityResult
        vm.stageAvatar(ExifStripper.strip(rawBytes, mimeType))
    }
    val bannerPicker = rememberLauncherForActivityResult(ActivityResultContracts.GetContent()) { uri ->
        uri ?: return@rememberLauncherForActivityResult
        val mimeType = context.contentResolver.getType(uri) ?: "application/octet-stream"
        val rawBytes = context.contentResolver.openInputStream(uri)?.readBytes() ?: return@rememberLauncherForActivityResult
        vm.stageBanner(ExifStripper.strip(rawBytes, mimeType))
    }

    when (val s = state) {
        is ProfileEditVM.State.Loading ->
            Box(
                modifier = Modifier.fillMaxWidth().padding(16.dp),
                contentAlignment = Alignment.Center,
            ) { CircularProgressIndicator() }
        is ProfileEditVM.State.Ready ->
            ProfileEditFormContent(
                initialDisplayName = s.displayName,
                initialBio = s.bio,
                initialLinks = s.links,
                saving = saving,
                avatarUploading = avatarUploading,
                bannerUploading = bannerUploading,
                onSave = { displayName, bio, links -> vm.save(displayName, bio, links, onSaved) },
                onCancel = onCancel,
                onPickAvatar = { avatarPicker.launch("image/*") },
                onClearAvatar = vm::clearAvatar,
                onPickBanner = { bannerPicker.launch("image/*") },
                onClearBanner = vm::clearBanner,
            )
    }
}

@Composable
fun ProfileEditFormContent(
    initialDisplayName: String,
    initialBio: String,
    initialLinks: List<FfiProfileLink>,
    saving: Boolean,
    avatarUploading: Boolean = false,
    bannerUploading: Boolean = false,
    onSave: (String, String, List<FfiProfileLink>) -> Unit,
    onCancel: () -> Unit,
    onPickAvatar: () -> Unit = {},
    onClearAvatar: () -> Unit = {},
    onPickBanner: () -> Unit = {},
    onClearBanner: () -> Unit = {},
) {
    var displayName by remember { mutableStateOf(initialDisplayName) }
    var bio by remember { mutableStateOf(initialBio) }
    // Single source of truth for the repeatable link rows: each edit replaces the
    // list with an updated copy (no per-field local state to drift) — add appends,
    // remove drops by index. Mirrors linux `edit.rs` add_link_row / collect_links.
    var links by remember { mutableStateOf(initialLinks) }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 12.dp)
            .testTag(Ids.PROFILE_EDIT_FORM),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        OutlinedTextField(
            value = displayName,
            onValueChange = { displayName = it },
            singleLine = true,
            label = { Text(stringResource(R.string.profile_edit_display_name)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.PROFILE_EDIT_DISPLAY_NAME),
        )
        OutlinedTextField(
            value = bio,
            onValueChange = { bio = it },
            label = { Text(stringResource(R.string.profile_edit_bio)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.PROFILE_EDIT_BIO),
        )

        // ── Repeatable links (profile-edit-link-list) ──────────────────────────
        Column(
            modifier = Modifier.fillMaxWidth().testTag(Ids.PROFILE_EDIT_LINK_LIST),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            links.forEachIndexed { index, link ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    OutlinedTextField(
                        value = link.label,
                        onValueChange = { v ->
                            links = links.toMutableList().also { it[index] = FfiProfileLink(v, it[index].uri) }
                        },
                        singleLine = true,
                        label = { Text(stringResource(R.string.profile_edit_link_label)) },
                        modifier = Modifier.weight(1f).testTag(Ids.PROFILE_EDIT_LINK_LABEL),
                    )
                    OutlinedTextField(
                        value = link.uri,
                        onValueChange = { v ->
                            links = links.toMutableList().also { it[index] = FfiProfileLink(it[index].label, v) }
                        },
                        singleLine = true,
                        label = { Text(stringResource(R.string.profile_edit_link_url)) },
                        modifier = Modifier.weight(1f).testTag(Ids.PROFILE_EDIT_LINK_URL),
                    )
                    OutlinedButton(
                        onClick = { links = links.toMutableList().also { it.removeAt(index) } },
                        enabled = !saving,
                        modifier = Modifier.testTag(Ids.PROFILE_EDIT_LINK_REMOVE_BUTTON),
                    ) { Text(stringResource(R.string.profile_edit_remove_link)) }
                }
            }
        }
        OutlinedButton(
            onClick = { links = links + FfiProfileLink("", "") },
            enabled = !saving,
            modifier = Modifier.align(Alignment.Start).testTag(Ids.PROFILE_EDIT_LINK_ADD_BUTTON),
        ) { Text(stringResource(R.string.profile_edit_add_link)) }

        // ── Avatar / banner (profile.md § Field ownership) ──────────────────────
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(
                onClick = onPickAvatar,
                enabled = !saving && !avatarUploading,
                modifier = Modifier.testTag(Ids.PROFILE_EDIT_AVATAR),
            ) { Text(stringResource(R.string.profile_edit_avatar)) }
            OutlinedButton(
                onClick = onClearAvatar,
                enabled = !saving,
                modifier = Modifier.testTag(Ids.PROFILE_EDIT_AVATAR_REMOVE_BUTTON),
            ) { Text(stringResource(R.string.profile_edit_remove_avatar)) }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(
                onClick = onPickBanner,
                enabled = !saving && !bannerUploading,
                modifier = Modifier.testTag(Ids.PROFILE_EDIT_BANNER),
            ) { Text(stringResource(R.string.profile_edit_banner)) }
            OutlinedButton(
                onClick = onClearBanner,
                enabled = !saving,
                modifier = Modifier.testTag(Ids.PROFILE_EDIT_BANNER_REMOVE_BUTTON),
            ) { Text(stringResource(R.string.profile_edit_remove_banner)) }
        }

        // ── Save / cancel ──────────────────────────────────────────────────────
        Row(
            modifier = Modifier.align(Alignment.End),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            OutlinedButton(
                onClick = onCancel,
                modifier = Modifier.testTag(Ids.PROFILE_EDIT_CANCEL_BUTTON),
            ) { Text(stringResource(R.string.profile_edit_cancel)) }
            Button(
                onClick = { onSave(displayName, bio, links) },
                enabled = !saving,
                modifier = Modifier.testTag(Ids.PROFILE_EDIT_SAVE_BUTTON),
            ) { Text(stringResource(R.string.profile_edit_save)) }
        }
    }
}
