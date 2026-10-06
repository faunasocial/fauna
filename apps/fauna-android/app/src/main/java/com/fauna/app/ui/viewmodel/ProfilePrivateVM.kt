package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiOverlayEditor
import com.fauna.ffi.FfiOverlaySave
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.OverlayForm
import javax.inject.Inject

/**
 * The Profile's **private section** on another person's profile — the viewer's
 * own nickname, notes and labels on them (`profile.md` § The private section).
 *
 * Glue over the shared `FfiOverlayEditor`: the staging (untouched fields
 * follow the live overlay, the first edit snapshots the baseline), the
 * changed-register diff, the bounds and every refusal text are shared Rust.
 * This class owns the text the fields show and nothing else.
 */
@HiltViewModel
class ProfilePrivateVM @Inject constructor(
    savedStateHandle: SavedStateHandle,
    private val host: ConversationsManagerHost,
    @ApplicationContext private val context: Context,
) : ViewModel() {

    private val actorId: String? = savedStateHandle.get<String>("actorId")

    // One editor per Profile open, over the manager live when the page opened
    // (a Profile is only reachable signed in, after the login swap).
    private val editor: FfiOverlayEditor? = actorId?.let { host.contactOverlays().editor(it) }

    private val _form = MutableStateFlow(editor?.form() ?: OverlayForm("", "", emptyList()))

    /** What the section shows: the staged edits once editing began, else the live overlay. */
    val form: StateFlow<OverlayForm> = _form.asStateFlow()

    /** The section's refusal or failure, for the page's `error-message`; `null` clears it. */
    val error = MutableStateFlow<String?>(null)

    private val _saving = MutableStateFlow(false)
    val saving: StateFlow<Boolean> = _saving.asStateFlow()

    /** Moves with the overlay projection — the screen re-reads the form on it. */
    val epoch: StateFlow<Long> = host.overlayEpoch

    /** Re-read the form: an untouched section follows a sibling device's edit. */
    fun reload() {
        editor?.let { _form.value = it.form() }
    }

    fun setNickname(value: String) {
        editor?.setNickname(value)
        reload()
    }

    fun setNotes(value: String) {
        editor?.setNotes(value)
        reload()
    }

    /** Stage the typed label; `true` when it was staged (the add field then empties). */
    fun addLabel(raw: String): Boolean {
        val refusal = editor?.addLabel(raw)
        show(refusal)
        reload()
        return refusal == null
    }

    fun removeLabel(index: Int) {
        editor?.removeLabel(index.toUInt())
        reload()
    }

    /** One Save for everything staged. A refusal keeps the staged edits on screen. */
    fun save() {
        val editor = editor ?: return
        if (_saving.value) return
        _saving.value = true
        viewModelScope.launch {
            try {
                when (val outcome = editor.save()) {
                    is FfiOverlaySave.Saved -> show(null)
                    is FfiOverlaySave.Refused -> show(outcome.text)
                    is FfiOverlaySave.NotReady -> show(outcome.text)
                    is FfiOverlaySave.Failed -> show(outcome.text)
                }
            } finally {
                _saving.value = false
                reload()
            }
        }
    }

    private fun show(text: LocalizedText?) {
        error.value = text?.let { resolveLocalized(context, it) }
    }
}
