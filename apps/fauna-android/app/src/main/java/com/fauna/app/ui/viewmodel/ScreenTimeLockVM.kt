package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.ScreenTimeStore
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import javax.inject.Inject

/**
 * Exposes the live [ScreenTimeStore.lockMessage] for the global
 * `screen-time-lock` overlay (family-safety.md § Screen time, Slice E). Pure
 * pass-through, mirroring [ConnectionStatusVM] — the store, not this VM, owns
 * the decision.
 */
@HiltViewModel
class ScreenTimeLockVM @Inject constructor(
    screenTimeStore: ScreenTimeStore,
) : ViewModel() {
    val lockMessage: StateFlow<String?> = screenTimeStore.lockMessage
}
