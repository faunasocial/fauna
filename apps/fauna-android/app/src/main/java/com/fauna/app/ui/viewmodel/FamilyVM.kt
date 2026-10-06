package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.ScreenTimeStore
import com.fauna.app.core.UndenyDecide
import com.fauna.ffi.FfiFamilyApprovalEntry
import com.fauna.ffi.FfiFamilyStatus
import com.fauna.ffi.FfiReachPolicy
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * The **`family`** page state (family-safety.md § App surface): one shared
 * `fauna.family.status` read answers both the guardian and supervised roles;
 * `fauna.family.approvals.list` is a separate, not-ward-scoped read (one queue
 * across every ward). Pure glue over the shared `FamilyClient` seam (priority
 * #2); lifts the linux/web reference.
 *
 * Observer-free like [SubscriptionSettingsVM]: a manual re-read on mount and
 * after every mutation — no client-side caching. Approve/deny, contact-add,
 * graduate, and the transfer actions all re-read `status`/`approvals` on
 * success so the UI never trusts an optimistic edit (family-safety.md's
 * "reload → re-assert server state" contract).
 */
@HiltViewModel
class FamilyVM @Inject constructor(
    private val api: ApiClient,
    private val screenTimeStore: ScreenTimeStore,
    private val contentPolicyStore: ContentPolicyStore,
) : ViewModel() {

    private val _status = MutableStateFlow<FfiFamilyStatus?>(null)
    val status: StateFlow<FfiFamilyStatus?> = _status.asStateFlow()

    private val _approvals = MutableStateFlow<List<FfiFamilyApprovalEntry>>(emptyList())
    val approvals: StateFlow<List<FfiFamilyApprovalEntry>> = _approvals.asStateFlow()

    /** The ward currently loaded into the one shared (non-indexed) policy editor. */
    private val _selectedWardActorId = MutableStateFlow<ByteArray?>(null)
    val selectedWardActorId: StateFlow<ByteArray?> = _selectedWardActorId.asStateFlow()

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading.asStateFlow()

    val errorMessage = MutableStateFlow<String?>(null)

    /** Re-read status + approvals (mount + after every mutation). */
    fun refresh() {
        viewModelScope.launch {
            errorMessage.value = null
            _isLoading.value = true
            try {
                val s = api.familyStatus()
                _status.value = s
                // This page's own `fauna.family.status` read is the freshest
                // view of the ward's OWN policy anywhere in the app, so it
                // also refreshes every client-enforced input — the global
                // `screen-time-lock` (family-safety.md § Screen time), the
                // content floor and Guardian Notify — off the reply's gated
                // `supervision` fold, never the raw `policy`, exactly as web's
                // Family page does (family-client-enforcement.md
                // § Implementation status today).
                screenTimeStore.setWardScreenTime(
                    s.supervision?.screenTime,
                    s.supervision?.supervisedBy?.handle,
                    s.usageTodayMinutes,
                )
                contentPolicyStore.applySupervision(s.supervision)
                val current = _selectedWardActorId.value
                val stillPresent = current != null && s.wards.any { it.actorId.contentEquals(current) }
                if (!stillPresent) {
                    _selectedWardActorId.value = s.wards.firstOrNull()?.actorId
                }
                _approvals.value = api.familyApprovalsList()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            } finally {
                _isLoading.value = false
            }
        }
    }

    fun selectWard(actorId: ByteArray) {
        _selectedWardActorId.value = actorId
    }

    /** `family-policy-save-button` — persist the selected ward's reach policy. */
    fun savePolicy(supervisedActorId: ByteArray, policy: FfiReachPolicy) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyPolicyUpdate(supervisedActorId, policy)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-device-mark-toggle` — set/clear the guardian-enrolled-device
     *  marker on one of the selected ward's devices (family-safety.md § Full
     *  visibility for young children, Slice F). Not batched behind
     *  `savePolicy`: `device.mark` is its own per-device RPC, so the flip
     *  lands immediately and the refetch re-renders every row from
     *  nest-confirmed state — including snapping a failed flip back. */
    fun markDevice(supervisedActorId: ByteArray, deviceId: String, marked: Boolean) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyDeviceMark(supervisedActorId, deviceId, marked)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
                refresh()
            }
        }
    }

    /**
     * `family-blocked-peer-allow-button` — un-deny one bridge-DM peer
     * (family-safety.md § The bridge-DM gate → *The un-deny surface*): the
     * shared `allowBlockedPeer` over THAT row's own [FfiFamilyBlockedPeer]
     * ([UndenyDecide], rule (f)). Re-reads status so the row drops from
     * nest-confirmed state.
     */
    fun allowBlockedPeer(decide: UndenyDecide) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyAllowBlockedPeer(decide.supervisedActorId, decide.peer)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-approval-{approve,deny}-button` — decide one queued item. */
    fun decideApproval(entry: FfiFamilyApprovalEntry, approve: Boolean) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyApprovalsDecide(
                    entry.supervisedActorId,
                    entry.kind,
                    entry.peerActorId,
                    entry.messageId,
                    // A `feed_source` item's key; empty for every other kind.
                    entry.bridgeId,
                    entry.operation,
                    entry.target,
                    // A `dm_hold` item's key, with bridgeId — an external DM
                    // peer is not an actor on this nest.
                    entry.peerAddress,
                    approve,
                )
                _approvals.value = api.familyApprovalsList()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-contact-add-button` — pre-approve a contact on the ward's behalf. */
    fun addContact(supervisedActorId: ByteArray, peerActorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyContactAdd(supervisedActorId, peerActorId)
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-graduate-confirm-button` — supervised → full account, in place. */
    fun graduate(supervisedActorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyGraduate(supervisedActorId)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-transfer-button` — propose a new guardian for the ward. */
    fun proposeTransfer(supervisedActorId: ByteArray, newGuardianActorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyTransfer(supervisedActorId, newGuardianActorId)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-transfer-cancel-button` — withdraw the ward's pending proposal. */
    fun cancelTransfer(supervisedActorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyTransferCancel(supervisedActorId)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-incoming-transfer-accept-button` — consent to a proposal naming the caller. */
    fun acceptIncomingTransfer(supervisedActorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyTransferAccept(supervisedActorId)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }

    /** `family-incoming-transfer-decline-button` — refuse a proposal naming the caller. */
    fun declineIncomingTransfer(supervisedActorId: ByteArray) {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                api.familyTransferDecline(supervisedActorId)
                refresh()
            } catch (e: Exception) {
                errorMessage.value = e.message ?: "error"
            }
        }
    }
}
