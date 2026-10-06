package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiProfileLink
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The profile **edit form** (`profile.md` § Where logic lives → *Profile
 * publish/edit*). Pure glue over the shared `fauna-client-profile` FFI seam
 * (priority #2/#3, no logic here); lifts the linux lead
 * (`apps/fauna-linux/src/views/profile/edit.rs`).
 *
 * Read-modify-write: [open] fetches the current `Profile` (`fauna.profile.get`,
 * through the shared read-prove-record) so a re-edit *preserves* the non-display fields the user can't touch here
 * (nests / admin_nests / load_hint / inbox_mode); a first publish (`not_found`)
 * starts from defaults (publish-on-first-edit). [save] runs the shared
 * `build_edited_profile_with_images` (sign) → `fauna.profile.set`, then calls
 * `onSaved` so the caller re-renders the header (observer-free — `feed.md`
 * § Architectural rules). Errors ride [errorMessage] to the page's global banner.
 *
 * Avatar/banner: [stageAvatar]/[stageBanner] upload eagerly on pick (matching
 * `FeedVM`'s own `compose-file` timing, not linux/tui's defer-to-Save shape)
 * and stage the resulting hash as [ImageEdit.Uploaded]; [clearAvatar]/
 * [clearBanner] stage an explicit removal. [save] resolves whichever field
 * was touched into the [com.fauna.ffi.FfiProfileImageEdit] `Keep`/`Clear`/`Set`
 * `build_edited_profile_with_images` needs.
 */
@HiltViewModel
class ProfileEditVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    /** The form's load state: hidden behind a spinner until [open]'s fetch returns. */
    sealed interface State {
        object Loading : State
        data class Ready(
            val displayName: String,
            val bio: String,
            val links: List<FfiProfileLink>,
        ) : State
    }

    /** What a [save] should do to one image field — resolved at Save time into
     *  [com.fauna.ffi.FfiProfileImageEdit]. */
    sealed interface ImageEdit {
        object Keep : ImageEdit
        object Clear : ImageEdit
        data class Uploaded(val blobHashHex: String) : ImageEdit
    }

    private val _state = MutableStateFlow<State>(State.Loading)
    val state: StateFlow<State> = _state.asStateFlow()

    /** True while a [save] publish round-trip is in flight (disables Save). */
    private val _saving = MutableStateFlow(false)
    val saving: StateFlow<Boolean> = _saving.asStateFlow()

    private val _avatarEdit = MutableStateFlow<ImageEdit>(ImageEdit.Keep)
    private val _bannerEdit = MutableStateFlow<ImageEdit>(ImageEdit.Keep)
    private val _avatarUploading = MutableStateFlow(false)
    val avatarUploading: StateFlow<Boolean> = _avatarUploading.asStateFlow()
    private val _bannerUploading = MutableStateFlow(false)
    val bannerUploading: StateFlow<Boolean> = _bannerUploading.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    /** The stored signed profile bytes (read-modify-write base), `null` until [open]. */
    private var baseBody: ByteArray? = null

    /**
     * Fetch the current profile (read-modify-write base) and project it to the
     * editable display fields, then reveal the form. A `not_found`/transport
     * error starts from a blank first-publish (`baseBody == null`). The base
     * loads through the shared read-prove-record ([ApiClient.loadProfileEditBase]),
     * so a linkless successor's first save never waits on a sign-in hop.
     */
    fun open(actorIdHex: String?) {
        viewModelScope.launch {
            val body = if (actorIdHex == null) null else api.loadProfileEditBase()
            baseBody = body
            val display = body?.let { runCatching { api.decodeProfileDisplay(it) }.getOrNull() }
            _avatarEdit.value = ImageEdit.Keep
            _bannerEdit.value = ImageEdit.Keep
            _state.value = State.Ready(
                displayName = display?.displayName.orEmpty(),
                bio = display?.bio.orEmpty(),
                links = display?.links ?: emptyList(),
            )
        }
    }

    /** Upload the picked (already EXIF-stripped) [bytes] via the shared
     *  public-post blob path and stage the resulting hash — a fresh pick
     *  always overrides a pending [clearAvatar]. */
    fun stageAvatar(bytes: ByteArray) = stageImage(bytes, _avatarEdit, _avatarUploading, "avatar")

    fun stageBanner(bytes: ByteArray) = stageImage(bytes, _bannerEdit, _bannerUploading, "banner")

    private fun stageImage(
        bytes: ByteArray,
        edit: MutableStateFlow<ImageEdit>,
        uploading: MutableStateFlow<Boolean>,
        fieldName: String,
    ) {
        viewModelScope.launch {
            uploading.value = true
            try {
                val hash = api.uploadPublicPostBlob(bytes)
                edit.value = ImageEdit.Uploaded(hash)
            } catch (e: Exception) {
                errorMessage.value = "$fieldName: ${e.message ?: "upload failed"}"
            } finally {
                uploading.value = false
            }
        }
    }

    /** Stage an explicit removal — a later [stageAvatar] pick still overrides it. */
    fun clearAvatar() { _avatarEdit.value = ImageEdit.Clear }

    fun clearBanner() { _bannerEdit.value = ImageEdit.Clear }

    private fun toFfi(edit: ImageEdit): com.fauna.ffi.FfiProfileImageEdit = when (edit) {
        is ImageEdit.Keep -> com.fauna.ffi.FfiProfileImageEdit.Keep
        is ImageEdit.Clear -> com.fauna.ffi.FfiProfileImageEdit.Clear
        is ImageEdit.Uploaded -> com.fauna.ffi.FfiProfileImageEdit.Set(edit.blobHashHex)
    }

    /**
     * Build the signed read-modify-write body and publish via `fauna.profile.set`.
     * Empty display_name / bio collapse to `null`; link rows blank in both label
     * and url are dropped (both trimmed) — mirrors linux `edit.rs::collect_links`
     * / `non_empty`. On success [onSaved] fires (close the form + refresh header).
     */
    fun save(displayName: String, bio: String, links: List<FfiProfileLink>, onSaved: () -> Unit) {
        viewModelScope.launch {
            errorMessage.value = null
            _saving.value = true
            try {
                val cleanLinks = links.mapNotNull {
                    val label = it.label.trim()
                    val uri = it.uri.trim()
                    if (label.isEmpty() && uri.isEmpty()) null else FfiProfileLink(label, uri)
                }
                val body = api.buildEditedProfileWithImages(
                    baseBody,
                    displayName.trim().ifBlank { null },
                    bio.trim().ifBlank { null },
                    cleanLinks,
                    toFfi(_avatarEdit.value),
                    toFfi(_bannerEdit.value),
                )
                api.profileSet(body)
                _avatarEdit.value = ImageEdit.Keep
                _bannerEdit.value = ImageEdit.Keep
                onSaved()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _saving.value = false
            }
        }
    }
}
