package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiConnectionState
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import javax.inject.Inject

/**
 * Exposes the live nest WS-RPC [connectionState] for the global
 * `connection-status` indicator (top of the shell). Pure pass-through of
 * [ApiClient.connectionState] — the shared `NestClient::connection_state()`
 * watch pumped through UniFFI (see [ApiClient.startConnectionStatePump]). The
 * Android twin of linux's top-of-sidebar indicator and the web SPA's
 * `connectionStatus` store.
 */
@HiltViewModel
class ConnectionStatusVM @Inject constructor(
    api: ApiClient
) : ViewModel() {
    val connectionState: StateFlow<FfiConnectionState> = api.connectionState
}
