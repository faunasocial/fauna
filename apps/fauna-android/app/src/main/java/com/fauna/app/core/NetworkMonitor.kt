package com.fauna.app.core

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import javax.inject.Inject
import javax.inject.Singleton

enum class NetworkState { WIFI, CELLULAR, NONE }

@Singleton
class NetworkMonitor @Inject constructor(
    @ApplicationContext context: Context
) {
    private val connectivityManager =
        context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager

    private val _state = MutableStateFlow(NetworkState.NONE)
    val state: StateFlow<NetworkState> = _state

    private val _isMetered = MutableStateFlow(true)
    val isMetered: StateFlow<Boolean> = _isMetered

    init {
        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()

        connectivityManager.registerNetworkCallback(request, object :
            ConnectivityManager.NetworkCallback() {
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                _isMetered.value = !caps.hasCapability(
                    NetworkCapabilities.NET_CAPABILITY_NOT_METERED
                )
                _state.value = when {
                    caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> NetworkState.WIFI
                    caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> NetworkState.CELLULAR
                    else -> NetworkState.WIFI // other transports (ethernet, etc.)
                }
            }

            override fun onLost(network: Network) {
                _state.value = NetworkState.NONE
            }
        })
    }

    fun isConnected(): Boolean = _state.value != NetworkState.NONE

    fun shouldSync(fileSize: Long): Boolean = when {
        _state.value == NetworkState.NONE -> false
        !_isMetered.value -> true
        fileSize < 10 * 1024 * 1024 -> true // <10 MB on metered
        else -> false
    }

    fun shouldSyncPhotos(): Boolean =
        _state.value != NetworkState.NONE && !_isMetered.value
}
