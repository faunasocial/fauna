package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.MlsManager
import com.fauna.app.core.ShellLog
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.data.db.CachedAccount
import com.fauna.app.data.db.CachedAccountDao
import com.fauna.app.ui.util.resolveLocalizedNested
import com.fauna.ffi.FfiFeatureRow
import com.fauna.ffi.FfiMlsLeg
import com.fauna.ffi.FfiQuotaGetReply
import com.fauna.ffi.FfiStatusLegs
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class StatusVM @Inject constructor(
    private val api: ApiClient,
    private val accountDao: CachedAccountDao,
    private val mlsManager: MlsManager,
    private val conversationsManagerHost: ConversationsManagerHost,
    @ApplicationContext private val appContext: android.content.Context,
) : ViewModel() {

    private val _account = MutableStateFlow<CachedAccount?>(null)
    val account: StateFlow<CachedAccount?> = _account

    private val _quota = MutableStateFlow<FfiQuotaGetReply?>(null)
    val quota: StateFlow<FfiQuotaGetReply?> = _quota

    // `feature-limits-section` — null (not `emptyList()`) is "the read hasn't
    // resolved", so the section is absent rather than empty before its data
    // lands (a limits section painted before its data would tell the user
    // nothing restricts them; `settings.md` § Layout & flow item 2b).
    private val _features = MutableStateFlow<List<FfiFeatureRow>?>(null)
    val features: StateFlow<List<FfiFeatureRow>?> = _features

    // The self host of the shared feature-policy editor: each limits row gains
    // `feature-limits-own-*` (`dynamic-features.md` § Authoring surfaces). A
    // write re-reads the transparency rows too — the effective limits changed.
    val ownLimits = FeatureAuthoringHost(
        scope = viewModelScope,
        tier = "self",
        readRows = api::fetchAuthoredFeatureRows,
        openEditor = api::openFeatureEditor,
        resolve = { resolveLocalizedNested(appContext, it).orEmpty() },
        onWritten = { refreshFeatures() },
    )

    // The Status snapshot's node and MLS legs as loaded so far
    // (`ui/status.md` § State & data shape). A null leg is not loaded — or not
    // applicable here (android has no local sync agent, so `sync` stays null
    // for good) — and its section is not painted. The finished element texts
    // are `statusText` over these (see `StatusScreen`); this VM derives none.
    private val _legs = MutableStateFlow(FfiStatusLegs(node = null, sync = null, mls = null))
    val legs: StateFlow<FfiStatusLegs> = _legs

    init {
        // Re-fetch quota and feature limits on each WS reconnect (transport.md §
        // Push events), part of the linux WsEvent::Reconnected re-fetch set —
        // the pair moved here from the Account page with their sections.
        viewModelScope.launch {
            api.reconnectTick.collect {
                refreshQuota()
                refreshFeatures()
            }
        }
    }

    fun refresh() {
        viewModelScope.launch {
            _account.value = accountDao.get()
        }
        refreshQuota()
        viewModelScope.launch { refreshLegs() }
        refreshFeatures()
        ownLimits.refresh()
    }

    private fun refreshQuota() {
        viewModelScope.launch {
            try {
                _quota.value = api.fetchQuota()
            } catch (e: Exception) {
                ShellLog.w("StatusVM", "quota fetch failed: ${e.message}")
            }
        }
    }

    private fun refreshFeatures() {
        viewModelScope.launch {
            try {
                _features.value = api.fetchFeatures()
            } catch (e: Exception) {
                // Feature-limits display is non-critical, same as quota above.
                ShellLog.w("StatusVM", "feature limits fetch failed: ${e.message}")
            }
        }
    }

    /**
     * The node and MLS legs, each landing as it resolves; a read that fails
     * leaves its leg null (the section is simply not painted — an unreachable
     * nest is `connection-status`'s to say, not a zero here).
     */
    private suspend fun refreshLegs() {
        try {
            val node = api.fetchStatusNode()
            _legs.value = _legs.value.copy(node = node)
        } catch (e: Exception) {
            ShellLog.w("StatusVM", "node leg failed: ${e.message}")
        }
        try {
            val keyPackages = mlsManager.getKeyPackageCount()
            _legs.value = _legs.value.copy(
                mls = FfiMlsLeg(
                    keyPackages = keyPackages.coerceAtLeast(0).toULong(),
                    channels = conversationsManagerHost.manager.secureChannelCount(),
                ),
            )
        } catch (e: Exception) {
            ShellLog.w("StatusVM", "mls leg failed: ${e.message}")
        }
    }
}
