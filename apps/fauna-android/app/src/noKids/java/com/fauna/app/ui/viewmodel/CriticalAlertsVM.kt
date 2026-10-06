package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.CriticalAlertsHost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import uniffi.fauna_client_alerts.CriticalAlertRow
import javax.inject.Inject

/**
 * Thin pass-through onto [CriticalAlertsHost.active] for the global
 * `critical-alerts` banner (`docs/goal/behavior/critical-alerts.md`;
 * `FaunaNavHost.kt`'s `CriticalAlertsBanner`, mounted like
 * [ConnectionStatusVM]/[SupervisedIndicatorVM] outside any nav route's
 * composable so it persists across navigation and renders on every
 * authenticated page). All registry state lives on the process-wide host —
 * this VM holds no logic of its own.
 */
@HiltViewModel
class CriticalAlertsVM @Inject constructor(
    host: CriticalAlertsHost,
) : ViewModel() {
    val active: StateFlow<List<CriticalAlertRow>> = host.active
}
