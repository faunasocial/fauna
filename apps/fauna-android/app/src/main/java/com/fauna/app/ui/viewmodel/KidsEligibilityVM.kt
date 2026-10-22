package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The Fauna Kids build's eligibility verdict (`account-age-band.md` § The
 * account age band, the kids-app bullet, item (3)): the shared
 * `kidsAppEligible(status)` over every `fauna.family.status` read — the
 * first one after sign-in and one on each reconnect, so a graduated account
 * flips the verdict on its next status read. Only the kids shell mounts it.
 *
 * `null` = no read has succeeded yet (or the last ones failed): the shell
 * stays up rather than refusing a supervised child on a connection blip —
 * the nest enforces supervision either way.
 */
@HiltViewModel
class KidsEligibilityVM @Inject constructor(
    private val api: ApiClient,
) : ViewModel() {

    private val _eligible = MutableStateFlow<Boolean?>(null)
    val eligible: StateFlow<Boolean?> = _eligible

    init {
        refresh()
        viewModelScope.launch {
            api.reconnectTick.collect { refresh() }
        }
    }

    private fun refresh() {
        viewModelScope.launch {
            try {
                _eligible.value = com.fauna.ffi.kidsAppEligible(api.familyStatus())
            } catch (_: Exception) {
                // No information: keep the last verdict.
            }
        }
    }
}
