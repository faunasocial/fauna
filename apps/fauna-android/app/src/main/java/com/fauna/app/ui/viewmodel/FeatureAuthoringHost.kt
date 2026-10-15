package com.fauna.app.ui.viewmodel

import com.fauna.ffi.FfiAuthoredRow
import com.fauna.ffi.FfiPolicyEditorInterface
import com.fauna.ffi.FfiPolicyEditorView
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.fauna_core.LocalizedText

/**
 * Everything one feature-limits authoring host paints (`dynamic-features.md`
 * § Authoring surfaces): the tier's authored rows, the one open editor's view,
 * and the host's last verdict line / error.
 */
data class FeatureAuthoringState(
    /** `null` until the authored read answers — a summary painted before that
     *  would say "No limit set" with no basis. */
    val rows: List<FfiAuthoredRow>? = null,
    /** The worded reason the authored read failed (the admin host paints it). */
    val readError: String? = null,
    /** The member the open editor is over, or `null` when none is open. */
    val openFeature: String? = null,
    /** The shared editor's derived view; set exactly while [openFeature] is. */
    val view: FfiPolicyEditorView? = null,
    /** `feature-policy-editor-status` — present only after a write. */
    val status: String? = null,
    /** A refused cell or a failed write, worded — the page's `error-message`. */
    val error: String? = null,
    /** A write is in flight (the re-tap guard on Save / Remove). */
    val working: Boolean = false,
)

/** The editor's callbacks, bundled so a host composable takes one parameter. */
class FeatureEditorActions(
    val onOpen: (feature: String) -> Unit,
    val onSetOn: (Boolean) -> Unit,
    val onSetCell: (index: Int, text: String) -> Unit,
    val onSave: () -> Unit,
    val onRemove: () -> Unit,
    val onCancel: () -> Unit,
) {
    companion object {
        /** No-ops — a content test (or a host that is not wired yet) composes with these. */
        val None = FeatureEditorActions({}, {}, { _, _ -> }, {}, {}, {})
    }
}

/**
 * ONE host's state machine for the shared `feature-policy-editor` family — the
 * Kotlin shell around `FfiPolicyEditor` that the admin Nest page
 * ([AdminNestVM]) and the Status sub-page's own-limit control ([StatusVM]) both
 * hold, differing only in [tier]. The twin of linux's `feature_editor.rs`
 * `HostState` and web's `FeaturePolicyEditor.svelte` host wiring.
 *
 * **This decides nothing.** The draft, the cell list, the labels, the no-effect
 * notes, whether Remove renders and the parser behind Save are all the shared
 * editor's; this class holds the handle, forwards keystrokes, and re-reads
 * [FfiPolicyEditorInterface.view] after each change. [openEditor] hands back
 * the interface rather than the concrete object so a JVM test can drive it
 * without the native library.
 *
 * [resolve] is the nested-key resolver ([com.fauna.app.ui.util.resolveLocalizedNested]
 * over the app context): the editor's titles and labels carry an i18n key as an
 * argument, so a plain resolve would leave the raw key in the sentence.
 */
class FeatureAuthoringHost(
    private val scope: CoroutineScope,
    /** `admin` | `self` — the tier the editor opens at. */
    private val tier: String,
    private val readRows: suspend (tier: String) -> List<FfiAuthoredRow>,
    private val openEditor: suspend (tier: String, feature: String) -> FfiPolicyEditorInterface,
    private val resolve: (LocalizedText) -> String,
    /** A write landed (the editor has already re-seeded): the owner re-reads
     *  whatever else shows the same plane. */
    private val onWritten: () -> Unit = {},
) {
    private val _state = MutableStateFlow(FeatureAuthoringState())
    val state: StateFlow<FeatureAuthoringState> = _state

    private var editor: FfiPolicyEditorInterface? = null

    /** Read this tier's authored documents. An open editor is kept. */
    fun refresh() {
        scope.launch {
            try {
                val rows = readRows(tier)
                _state.update { it.copy(rows = rows, readError = null) }
            } catch (e: Exception) {
                _state.update { it.copy(rows = null, readError = failure("features.editor_load_failed", e)) }
            }
        }
    }

    /** Open the editor over [feature], seeded from the tier's AUTHORED document. */
    fun open(feature: String) {
        scope.launch {
            try {
                val opened = openEditor(tier, feature)
                editor = opened
                _state.update {
                    it.copy(openFeature = feature, view = opened.view(), status = null, error = null)
                }
            } catch (e: Exception) {
                _state.update { it.copy(error = failure("features.editor_load_failed", e)) }
            }
        }
    }

    /** `feature-policy-editor-on-radio` (`true`) / `-off-radio` (`false`). */
    fun setOn(on: Boolean) {
        val open = editor ?: return
        open.setOn(on)
        _state.update { it.copy(view = open.view()) }
    }

    /** `feature-policy-editor-cell-input[index]` — one keystroke's new text. */
    fun setCell(index: Int, text: String) {
        val open = editor ?: return
        open.setCell(index.toUInt(), text)
        _state.update { it.copy(view = open.view()) }
    }

    /** `feature-policy-editor-save-button`. */
    fun save() = write(remove = false)

    /** `feature-policy-editor-remove-button`. */
    fun remove() = write(remove = true)

    /** `feature-policy-editor-cancel-button` — closes the editor, touching nothing. */
    fun cancel() {
        editor = null
        _state.update { it.copy(openFeature = null, view = null, status = null, error = null) }
    }

    /**
     * Save (or, with [remove], clear) the open editor's document. The shared
     * parser decides: a cell it cannot read dispatches nothing and its reason
     * lands on the page's error banner, exactly as on tui.
     */
    private fun write(remove: Boolean) {
        val open = editor ?: return
        scope.launch {
            _state.update { it.copy(working = true, error = null) }
            try {
                val verdict = if (remove) open.remove() else open.save()
                val invalid = verdict.invalid
                if (invalid != null) {
                    _state.update { it.copy(error = resolve(invalid)) }
                } else {
                    _state.update {
                        it.copy(view = open.view(), status = verdict.status?.let(resolve))
                    }
                    refresh()
                    onWritten()
                }
            } catch (e: Exception) {
                _state.update { it.copy(error = failure("features.editor_save_failed", e)) }
            } finally {
                _state.update { it.copy(working = false) }
            }
        }
    }

    /** The editor's callbacks for a host composable. */
    fun actions() = FeatureEditorActions(
        onOpen = ::open,
        onSetOn = ::setOn,
        onSetCell = ::setCell,
        onSave = ::save,
        onRemove = ::remove,
        onCancel = ::cancel,
    )

    private fun failure(key: String, e: Exception): String =
        resolve(LocalizedText(key, mapOf("error" to (e.message ?: e.toString()))))
}
