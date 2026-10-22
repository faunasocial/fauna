package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ShellLog
import com.fauna.app.core.push.PushHost
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * Settings → Account → Push notifications (`settings.md` § Push notifications):
 * one toggle — this install's opt-in bit, never the OS notification
 * permission — and one inline line. The toggle drives the shared registration
 * machine through [PushHost]; windows' `PushNotificationsViewModel` is the
 * twin.
 *
 * [optedIn] is written only from the stored record (load, and after each
 * toggle's attempt), never from the tap, so it is evidence of the persisted
 * bit: a failed enable reads off again, and a disable reads off even when the
 * nest was unreachable (the shared machine clears the bit first).
 */
@HiltViewModel
class PushNotificationsVM @Inject constructor(
    private val pushHost: PushHost,
) : ViewModel() {
    /** What the `push-notifications-error` line says; `null` = not rendered. */
    enum class Line {
        /** This phone has no UnifiedPush distributor app. */
        NO_DISTRIBUTOR,
        /** The OS notification permission was refused at toggle-on. */
        PERMISSION_DENIED,
        /** The registration failed (no session, the nest or the distributor). */
        UPDATE_FAILED,
    }

    private val _optedIn = MutableStateFlow(pushHost.optedIn())
    val optedIn: StateFlow<Boolean> = _optedIn.asStateFlow()

    private val _busy = MutableStateFlow(false)
    val busy: StateFlow<Boolean> = _busy.asStateFlow()

    private val _line = MutableStateFlow<Line?>(null)
    val line: StateFlow<Line?> = _line.asStateFlow()

    /** Re-read the stored bit (the page's appear). */
    fun load() {
        _optedIn.value = pushHost.optedIn()
    }

    /** The toggle, once any permission prompt has answered: on → enable,
     *  off → disable. */
    fun setOptIn(on: Boolean) {
        if (_busy.value) return
        _busy.value = true
        viewModelScope.launch {
            val failure: Line? = try {
                if (on) {
                    when (pushHost.enable()) {
                        null -> null
                        PushHost.EnableFailure.NoDistributor -> Line.NO_DISTRIBUTOR
                        else -> Line.UPDATE_FAILED
                    }
                } else {
                    pushHost.disable()
                    null
                }
            } catch (e: Exception) {
                ShellLog.w("PushNotificationsVM", "toggle failed: ${e.message}")
                Line.UPDATE_FAILED
            }
            _line.value = failure
            _optedIn.value = pushHost.optedIn()
            _busy.value = false
        }
    }

    /** Toggle-on met a refused OS permission: nothing is registered, the bit
     *  stays off, and the line says why. */
    fun permissionRefused() {
        _line.value = Line.PERMISSION_DENIED
        _optedIn.value = pushHost.optedIn()
    }
}
