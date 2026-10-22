package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.StoreConsentStore
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import javax.inject.Inject

/**
 * Exposes the live [StoreConsentStore.lockLines] for the global
 * `store-consent-lock` (account-age-band.md § App surface → *Store-consent
 * surfaces*). Pure pass-through, the [ScreenTimeLockVM] twin — the store, not
 * this VM, owns the decision.
 */
@HiltViewModel
class StoreConsentLockVM @Inject constructor(
    store: StoreConsentStore,
) : ViewModel() {
    val lockLines: StateFlow<List<String>?> = store.lockLines
}
