package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.InheritedFilterMarks
import com.fauna.app.core.SecureStorage
import com.fauna.ffi.FfiEmailFilter
import com.fauna.ffi.FfiFilterActionInputs
import com.fauna.ffi.FfiFilterRuleInputs
import com.fauna.ffi.actorIdFromSecret
import com.fauna.ffi.describeEmailFilterActionInputs
import com.fauna.ffi.describeEmailFilterRuleInputs
import com.fauna.ffi.emailFilterIsEditable
import com.fauna.ffi.encodeEmailFilterActionInputs
import com.fauna.ffi.encodeEmailFilterRuleInputs
import com.fauna.ffi.parseEmailFilterPriority
import com.fauna.ffi.perMilleToProbability
import com.fauna.ffi.probabilityToPerMille
import com.fauna.ffi.spamThresholdBand
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/** Everything the shared filter form holds — the condition and action inputs
 *  [describeEmailFilterRuleInputs] / [describeEmailFilterActionInputs] decode a
 *  stored filter into, its order as typed (`filter-priority-input`, blank is
 *  0) and its `continue` flag (`filter-continue-toggle`) — what an edit
 *  pre-populates and what create/save encode. */
data class FilterForm(
    val name: String,
    val rule: FfiFilterRuleInputs,
    val action: FfiFilterActionInputs,
    val priority: String,
    val continueOnMatch: Boolean,
)

@HiltViewModel
class PrivacySettingsVM @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage,
    private val inheritedFilterMarks: InheritedFilterMarks,
    @ApplicationContext private val appContext: Context,
) : ViewModel() {

    // Inbox mode. `null` until the fetch resolves — settings.md § Privacy
    // sub-page item 7 ("show the stored mode, never a default") forbids
    // pre-selecting a guess; tui's PrivacyState::inbox_mode is the reference
    // Option<String> shape (apps/fauna-tui/src/settings/mod.rs:917), and the
    // apple leg already made the same String -> String?
    // change (FaunaKit's PrivacySettingsVM.swift). loadInboxMode() sets the
    // real value on success; a failed fetch leaves it null rather than
    // guessing, same as tui's page_error while inbox_mode is None.
    val inboxMode = MutableStateFlow<String?>(null)
    val inboxModeLoading = MutableStateFlow(false)
    val inboxModeError = MutableStateFlow<String?>(null)

    // Email filters
    val emailFilters = MutableStateFlow<List<FfiEmailFilter>>(emptyList())
    val emailFilterError = MutableStateFlow<String?>(null)
    val creatingFilter = MutableStateFlow(false)
    // Non-null while the shared form (below) edits an existing filter rather
    // than creating one — save-filter's counterpart to create-filter, same
    // shared form. Set only after a successful filters_get decode, so the
    // sheet opens already pre-populated (never an empty flash).
    val editingFilterId = MutableStateFlow<Long?>(null)
    val editFilterPrefill = MutableStateFlow<FilterForm?>(null)
    // The ids of every rule the succession aftermath carried across that the
    // owner has not answered — `filter-unattested-mark` +
    // `filter-review-keep-button` render on exactly these rows. The shared
    // cache the Account line counts too ([InheritedFilterMarks]).
    val filterMarks: StateFlow<List<Long>> = inheritedFilterMarks.marks

    // Spam preferences
    val spamThreshold = MutableStateFlow(0.5f)
    val phishingThreshold = MutableStateFlow(0.5f)
    val spamPrefsLoading = MutableStateFlow(false)
    val spamPrefsSaved = MutableStateFlow(false)
    val spamPrefsError = MutableStateFlow<String?>(null)

    // Shared spam-prefs presentation contract (fauna_protocol::spam, via the
    // fauna-ffi UniFFI face) — every app reads the same threshold-band buckets
    // instead of re-deriving them per platform (priority #2/#4; settings.md §
    // Spam threshold slider labels). The FFI call lives here, off the stateless
    // PrivacySettingsContent, so the Compose test renders without native libs.

    /** Threshold-band i18n key (`aggressive`/`moderate`/`permissive`) for a 0.0–1.0 slider value. */
    fun spamBandKey(threshold: Float): String =
        spamThresholdBand(probabilityToPerMille(threshold.toDouble()))

    private fun actorId(): String? =
        secureStorage.secretHex?.let {
            try { HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(it))) }
            catch (_: Exception) { null }
        }

    fun loadAll() {
        loadInboxMode()
        loadFilters()
        loadSpamPreferences()
        // Every Privacy visit re-reads the inherited-rule marks (apple's
        // pattern), so a verdict given on another device clears here too.
        viewModelScope.launch { inheritedFilterMarks.reload() }
    }

    private fun loadInboxMode() {
        viewModelScope.launch {
            val id = actorId() ?: return@launch
            try {
                inboxMode.value = api.getInboxMode(id)
            } catch (e: Exception) {
                inboxModeError.value = e.message
            }
        }
    }

    fun updateInboxMode(mode: String) {
        viewModelScope.launch {
            val id = actorId() ?: return@launch
            inboxModeLoading.value = true
            inboxModeError.value = null
            try {
                api.setInboxMode(id, mode)
                inboxMode.value = mode
            } catch (e: Exception) {
                inboxModeError.value = e.message
            }
            inboxModeLoading.value = false
        }
    }

    fun loadFilters() {
        viewModelScope.launch {
            try {
                emailFilters.value = api.listEmailFilters()
            } catch (e: Exception) {
                emailFilterError.value = e.message
            }
        }
    }

    fun createFilter(form: FilterForm) {
        viewModelScope.launch {
            creatingFilter.value = true
            emailFilterError.value = null
            try {
                // The form onto the typed wire values via the shared encoders
                // (fauna_protocol::email::encode_filter_rule_inputs / _action /
                // parse_filter_priority) — the single source of truth every app
                // shares (priority #2/#4). A bad input throws onto the error line.
                val rule = encodeEmailFilterRuleInputs(form.rule)
                val actionTyped = encodeEmailFilterActionInputs(form.action)
                val priority = parseEmailFilterPriority(form.priority)
                api.createEmailFilter(form.name, listOf(rule), "all", actionTyped, priority, form.continueOnMatch)
                loadFilters()
            } catch (e: Exception) {
                emailFilterError.value = e.message
            }
            creatingFilter.value = false
        }
    }

    /** Whether `filter` can open the shared edit form — gated exactly like the
     *  other apps (`fauna_protocol::email::filter_is_editable` via the UniFFI
     *  free function): one condition, and both it and the action describe. A
     *  filter only a raw API call could have produced never opens a form that
     *  would silently narrow it on save. */
    fun isFilterEditable(filter: FfiEmailFilter): Boolean =
        emailFilterIsEditable(filter.rules, filter.action)

    /** Open the shared form pre-populated for an existing filter — a fresh
     *  `filters_get` (not the cached list row), so the edit reflects current
     *  server state. The row's own edit button is gated [isFilterEditable],
     *  so a decode failure here means the filter changed server-side between
     *  list-load and the click (rare) rather than the common case. */
    fun beginEditFilter(id: Long) {
        viewModelScope.launch {
            emailFilterError.value = null
            try {
                val filter = api.getEmailFilter(id)
                val rule = filter.rules.firstOrNull()?.let { describeEmailFilterRuleInputs(it) }
                val action = describeEmailFilterActionInputs(filter.action)
                if (rule == null || action == null) {
                    emailFilterError.value = appContext.getString(R.string.settings_errors_update_filter)
                    return@launch
                }
                editFilterPrefill.value = FilterForm(
                    filter.name, rule, action, filter.priority.toString(), filter.continueOnMatch,
                )
                editingFilterId.value = filter.id
            } catch (e: Exception) {
                emailFilterError.value = e.message
            }
        }
    }

    fun cancelEditFilter() {
        editingFilterId.value = null
        editFilterPrefill.value = null
    }

    fun saveFilter(form: FilterForm) {
        val id = editingFilterId.value ?: return
        viewModelScope.launch {
            creatingFilter.value = true
            emailFilterError.value = null
            try {
                val rule = encodeEmailFilterRuleInputs(form.rule)
                val actionTyped = encodeEmailFilterActionInputs(form.action)
                val priority = parseEmailFilterPriority(form.priority)
                api.updateEmailFilter(id, form.name, listOf(rule), "all", actionTyped, priority, form.continueOnMatch)
                cancelEditFilter()
                loadFilters()
            } catch (e: Exception) {
                emailFilterError.value = e.message
            }
            creatingFilter.value = false
        }
    }

    fun deleteFilter(id: Long) {
        viewModelScope.launch {
            try {
                api.deleteEmailFilter(id)
            } catch (e: Exception) {
                emailFilterError.value = e.message
                return@launch
            }
            loadFilters()
            // `filter-delete` IS the Remove half of an inherited rule's review
            // (`succession-aftermath.md` § Adjudicating — no second removal
            // mechanism): a marked row's verdict is recorded strictly AFTER the
            // deletion landed, best-effort ([InheritedFilterMarks.recordRemoved]).
            inheritedFilterMarks.recordRemoved(id)
        }
    }

    /** `filter-review-keep-button` — the owner recognises this inherited rule:
     *  it stays, and its mark (and its share of the Account line) clears. A
     *  failure is surfaced, never swallowed: a Keep that silently did not land
     *  leaves the owner believing they answered a question asked again later. */
    fun keepFilterMark(id: Long) {
        viewModelScope.launch {
            emailFilterError.value = null
            try {
                inheritedFilterMarks.keep(id)
            } catch (e: Exception) {
                emailFilterError.value = e.message
            }
        }
    }

    private fun loadSpamPreferences() {
        viewModelScope.launch {
            try {
                val prefs = api.getSpamPreferences()
                spamThreshold.value = fromPerMille(prefs.spamThreshold)
                phishingThreshold.value = fromPerMille(prefs.phishingThreshold)
            } catch (_: Exception) {
                // Defaults remain if server doesn't support spam prefs
            }
        }
    }

    fun saveSpamPreferences() {
        viewModelScope.launch {
            spamPrefsLoading.value = true
            spamPrefsSaved.value = false
            spamPrefsError.value = null
            try {
                api.updateSpamPreferences(
                    spamThreshold = toPerMille(spamThreshold.value),
                    phishingThreshold = toPerMille(phishingThreshold.value),
                )
                spamPrefsSaved.value = true
            } catch (e: Exception) {
                spamPrefsError.value = e.message
            }
            spamPrefsLoading.value = false
        }
    }

    // The fauna.spam.* wire carries the thresholds as per-mille UShort (0–1000;
    // dag-cbor forbids floats) while the UI is a 0.0–1.0 slider. The slider↔wire
    // conversion is shared Rust — fauna_protocol::spam::{probability_to_per_mille,
    // per_mille_to_probability} via the fauna-ffi UniFFI face — so every app
    // clamps + rounds (half away from zero) identically instead of hand-rolling
    // `* 1000` / `/ 1000` (settings.md § Spam threshold slider labels; priority #2/#4).
    private fun fromPerMille(perMille: UShort): Float = perMilleToProbability(perMille).toFloat()

    private fun toPerMille(value: Float): UShort = probabilityToPerMille(value.toDouble())
}
