package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ApiClient
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiKidsAppEligibility
import com.fauna.ffi.FfiSupervisionSnapshot
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The Fauna Kids build's eligibility answer (`account-age-band.md` § The
 * account age band → *Kids-app eligibility while it is unknown*): three-way —
 * ELIGIBLE, INELIGIBLE, UNKNOWN — decided by the shared
 * `kidsAppEligibility(latestRead, snapshot)`, never here. Only the kids shell
 * mounts it.
 *
 * The inputs are this account's latest SUCCESSFUL `fauna.family.status` read
 * of this run (the first after sign-in and one on each reconnect, so a
 * graduated account flips on its next read) and its persisted supervision
 * snapshot ([AccountStores.supervisionSnapshot]), which seeds the answer at
 * construction: a supervised child who opens the app offline gets the shell,
 * an account never seen supervised here waits on UNKNOWN. A failed read never
 * changes an answer already given; while the answer is UNKNOWN it raises
 * [readFailedWhileUnknown], which the read that succeeds clears. An account
 * switch starts over for the incoming account — its own snapshot, then its
 * own first read.
 */
@HiltViewModel
class KidsEligibilityVM internal constructor(
    private val api: ApiClient,
    private val accountStores: AccountStores,
    private val decide: (FfiFamilyStatus?, FfiSupervisionSnapshot?) -> FfiKidsAppEligibility,
    private val readStatus: suspend () -> FfiFamilyStatus,
) : ViewModel() {

    @Inject
    constructor(api: ApiClient, accountStores: AccountStores) : this(
        api,
        accountStores,
        { read, snapshot -> com.fauna.ffi.kidsAppEligibility(read, snapshot) },
        { api.familyStatus() },
    )

    private val _eligibility = MutableStateFlow(FfiKidsAppEligibility.UNKNOWN)
    val eligibility: StateFlow<FfiKidsAppEligibility> = _eligibility.asStateFlow()

    private val _readFailedWhileUnknown = MutableStateFlow(false)
    val readFailedWhileUnknown: StateFlow<Boolean> = _readFailedWhileUnknown.asStateFlow()

    /** The actor the current answer belongs to; a read made for another is dropped. */
    private var seededActorHex: String? = null

    init {
        accountStores.registerCloser("kids-eligibility") {
            // Clause (e): start over for whoever is active now.
            if (accountStores.activeActorHex() != seededActorHex) {
                seed()
                refresh()
            }
        }
        seed()
        refresh()
        viewModelScope.launch {
            api.reconnectTick.collect { refresh() }
        }
    }

    private fun seed() {
        seededActorHex = accountStores.activeActorHex()
        _readFailedWhileUnknown.value = false
        _eligibility.value = decide(null, accountStores.supervisionSnapshot())
    }

    private fun refresh() {
        val forActor = seededActorHex
        viewModelScope.launch {
            val status = try {
                readStatus()
            } catch (e: CancellationException) {
                throw e
            } catch (_: Exception) {
                // Clause (d): no information — keep the answer as it stood.
                if (forActor == seededActorHex && _eligibility.value == FfiKidsAppEligibility.UNKNOWN) {
                    _readFailedWhileUnknown.value = true
                }
                return@launch
            }
            if (forActor != seededActorHex) return@launch
            _eligibility.value = decide(status, accountStores.supervisionSnapshot())
            _readFailedWhileUnknown.value = false
        }
    }
}
