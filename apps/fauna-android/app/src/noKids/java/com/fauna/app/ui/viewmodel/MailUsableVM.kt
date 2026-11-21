package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_client_mail_settings.MailSettingsMachine
import uniffi.fauna_client_mail_settings.mailSettingsMailUsable
import uniffi.fauna_client_mail_settings.mailUsableUnbuilt
import uniffi.fauna_client_mail_settings.mailUsableUnread
import uniffi.fauna_core.Possibility
import javax.inject.Inject

/**
 * The mail-usable gate's answer (`mail-settings.md` § Errors & edge cases, the
 * mail-usable gate) for the Mail & Calendar hub's sub-page rows and the six
 * mail sub-pages' entry. Holds no rule of its own: it reads the shared
 * `mail_settings_mail_usable` off the session's ONE mail machine
 * ([ApiClient.sharedMailSettingsMachine], the hub's too, so an enable there
 * lights the rows at once) on a short tick — the linux and web shape —
 * `mail_usable_unread()` while no connection has built the machine yet, and
 * `mail_usable_unbuilt()` when its build fails. FFI stays here, off the
 * stateless Content composables.
 */
@HiltViewModel
class MailUsableVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _answer = MutableStateFlow<Possibility>(mailUsableUnread())
    val answer: StateFlow<Possibility> = _answer.asStateFlow()

    init {
        viewModelScope.launch {
            var hydrated = false
            while (true) {
                val machine = readMachine()
                if (machine != null && !hydrated) {
                    hydrated = true
                    // A door other than the hub (which hydrates on its own)
                    // still gets the nest's answer: one heal-free read.
                    if (!machine.snapshot().read) {
                        try { machine.hydrate() } catch (_: Exception) { }
                    }
                }
                delay(TICK_MS)
            }
        }
    }

    private fun readMachine(): MailSettingsMachine? {
        val machine = try {
            api.sharedMailSettingsMachine()
        } catch (_: Exception) {
            _answer.value = mailUsableUnbuilt()
            return null
        }
        _answer.value = machine?.let { mailSettingsMailUsable(it.snapshot()) } ?: mailUsableUnread()
        return machine
    }

    private companion object {
        const val TICK_MS = 400L
    }
}
