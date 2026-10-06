package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiAdminInviteCode
import com.fauna.ffi.FfiAdminInviteRequest
import com.fauna.ffi.FfiAdminUser
import com.fauna.ffi.FfiRegistrationMode
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * View-model for the consolidated `admin-users` hub (admin.md § Users): one
 * surface, three sections — Pending requests / Invite / Users — all framed
 * around assigning a *tier* (the tier is the quota). Drives the shared
 * `fauna.admin.*` WS-RPC kinds through [ApiClient]'s `FfiAdminClient` wrappers;
 * never the deleted `/admin/api` HTTP twins.
 *
 * Mirrors the Linux reference (`apps/fauna-linux/src/views/admin.rs`
 * `build_users_hub`) and the Windows `AdminUsersViewModel` shape: a thin client
 * over the kinds + the existing message loop, until the per-page snapshot getter
 * lands (admin.md § State & data shape, marked TBD).
 *
 * The Users-section cut-off ladder (evict / suspend / cancel-eviction) is driven
 * from the shared `admin_user_row_controls` decision (admin.md § 2 Users →
 * *Cutting a user off*); the screen renders whichever controls it returns. Still
 * out of scope for this surface (spec-ahead on all apps, admin.md):
 * tier-*definition* editing.
 */
@HiltViewModel
class AdminUsersVM @Inject constructor(
    private val api: ApiClient
) : ViewModel() {

    private val _users = MutableStateFlow<List<FfiAdminUser>>(emptyList())
    val users: StateFlow<List<FfiAdminUser>> = _users

    /**
     * Every account on the nest — the guardian pickers' source (Pending
     * requests / Invite sections), kept separate from the paginated [_users]
     * page above (`fauna_client_admin::users_list_all`, read via
     * [ApiClient.adminUsersListAll]; admin.md § 2 → *Which accounts a picker
     * offers*). Reloaded alongside every [_users] reload ([refresh] and every
     * [reloadUsers] call after a row action), mirroring apple's
     * `loadAllUsers` running beside every `loadUsers`.
     */
    private val _allUsers = MutableStateFlow<List<FfiAdminUser>>(emptyList())
    val allUsers: StateFlow<List<FfiAdminUser>> = _allUsers

    /** Unpaginated total from `users.list` — backs `user-count-text`. */
    private val _userTotal = MutableStateFlow(0L)
    val userTotal: StateFlow<Long> = _userTotal

    /** The 0-based offset of the currently-loaded users page. */
    private val _userOffset = MutableStateFlow(0L)
    val userOffset: StateFlow<Long> = _userOffset

    /** Tier names backing every tier picker; falls back to [DEFAULT_TIERS]. */
    private val _tiers = MutableStateFlow(DEFAULT_TIERS)
    val tiers: StateFlow<List<String>> = _tiers

    private val _inviteCodes = MutableStateFlow<List<FfiAdminInviteCode>>(emptyList())
    val inviteCodes: StateFlow<List<FfiAdminInviteCode>> = _inviteCodes

    /** Only *pending* requests — the "Pending requests" section. */
    private val _pendingRequests = MutableStateFlow<List<FfiAdminInviteRequest>>(emptyList())
    val pendingRequests: StateFlow<List<FfiAdminInviteRequest>> = _pendingRequests

    /** The freshly minted invite token, surfaced for the copy button. */
    private val _mintedCode = MutableStateFlow<String?>(null)
    val mintedCode: StateFlow<String?> = _mintedCode

    /**
     * Section 2 — Registration (admin.md § 2 Users). The nest's registration
     * posture, parsed from `fauna.setup.status.registration_mode` via the
     * shared `registrationModeFromWire`. **`null` does NOT mean "closed"** —
     * it means this client cannot name the posture (a mode string a newer
     * nest added that this client
     * predates). The screen renders read-only in that case; NEVER coerce this
     * to a default variant and offer a Save, which would overwrite the nest's
     * real posture with a guess (public-mode.md § Registration Modes).
     */
    private val _registrationMode = MutableStateFlow<FfiRegistrationMode?>(null)
    val registrationMode: StateFlow<FfiRegistrationMode?> = _registrationMode

    /** The raw wire string behind an unparseable [_registrationMode], for the
     *  read-only section's "unrecognized setting" message. Non-null exactly
     *  when [_registrationMode] is null AND the nest reported *some* value
     *  (an absent field surfaces as null here too — there's nothing to name). */
    private val _unknownRegistrationMode = MutableStateFlow<String?>(null)
    val unknownRegistrationMode: StateFlow<String?> = _unknownRegistrationMode

    /** The free-tier ceiling as typed/displayed: `""` means blank = no cap.
     *  Orthogonal to [_registrationMode] — applies regardless of posture. */
    private val _maxFreeUsers = MutableStateFlow("")
    val maxFreeUsers: StateFlow<String> = _maxFreeUsers

    /** The age require-knob as PERSISTED — `fauna.setup.status
     *  .age_verification_required` ("accept only signups carrying app age
     *  verification", default off; admin.md § 2 → Registration). The section's
     *  toggle is a draft seeded from this; [saveRegistration] sends the knob
     *  only when the draft differs from it. */
    private val _ageVerificationRequired = MutableStateFlow(false)
    val ageVerificationRequired: StateFlow<Boolean> = _ageVerificationRequired

    private val _isLoading = MutableStateFlow(false)
    val isLoading: StateFlow<Boolean> = _isLoading

    /** All three sections route failures here (`admin-users-action-error`). */
    private val _actionError = MutableStateFlow<String?>(null)
    val actionError: StateFlow<String?> = _actionError

    fun refresh() {
        viewModelScope.launch {
            _isLoading.value = true
            _actionError.value = null
            try {
                val tierNames = runCatching { api.adminTiersList().map { it.name } }
                    .onFailure { ShellLog.w("AdminUsersVM", "admin tier list fetch failed: ${it.message}") }
                    .getOrDefault(emptyList())
                if (tierNames.isNotEmpty()) _tiers.value = tierNames

                reloadUsers()

                _inviteCodes.value = api.adminInviteCodesList()
                _pendingRequests.value =
                    api.adminInviteRequestsList().filter { it.status == STATUS_PENDING }

                reloadRegistration()
            } catch (e: Exception) {
                _actionError.value = e.message
            }
            _isLoading.value = false
        }
    }

    /** Change a user's tier (= quota) via `fauna.admin.users.update`. */
    fun setUserTier(user: FfiAdminUser, tier: String) {
        if (tier == user.tier) return
        action {
            api.adminUsersUpdate(user.actorId, tier, user.label)
            reloadUsers()
        }
    }

    /**
     * Start a user's eviction timeline (warn→suspend→delete) via
     * `fauna.admin.users.evict` — the `admin-users-evict-button`. One-click with a
     * default [reason] + the `other` category (the only inputs the row exposes);
     * the user is not deleted, and the refetch shows the cancel control. Mirrors
     * linux `evict_user`.
     */
    fun evictUser(user: FfiAdminUser, reason: String) {
        action {
            api.adminUsersEvict(user.actorId, reason, EVICTION_CATEGORY)
            reloadUsers()
        }
    }

    /**
     * Suspend a user **immediately** via `fauna.admin.users.suspend` — the
     * `admin-users-suspend-button`. No delete timeline (enters `suspended` at
     * once); reversed by [cancelEviction], which suspend and evict share. Mirrors
     * linux `suspend_user`.
     */
    fun suspendUser(user: FfiAdminUser, reason: String) {
        action {
            api.adminUsersSuspend(user.actorId, reason, EVICTION_CATEGORY)
            reloadUsers()
        }
    }

    /**
     * Cancel a user's in-flight eviction / restore a suspended user via
     * `fauna.admin.users.cancel_eviction` — the `admin-users-cancel-eviction-button`.
     * Mirrors linux `cancel_user_eviction`.
     */
    fun cancelEviction(user: FfiAdminUser) {
        action {
            api.adminUsersCancelEviction(user.actorId)
            reloadUsers()
        }
    }

    /**
     * Grant the admin role via `fauna.admin.admins.add` — the
     * `admin-users-make-admin-button`. Schedules a 24h-delayed pending action;
     * a scheduled reply (no error) IS success, the row does not flip to an
     * admin row right away. Mirrors linux `make_admin`.
     */
    fun makeAdmin(user: FfiAdminUser) {
        action {
            api.adminAdminsAdd(user.actorId)
            reloadUsers()
        }
    }

    /**
     * Revoke the admin role via `fauna.admin.admins.remove` — the
     * `admin-users-remove-admin-button`. Refuses (`fauna.admin.conflict`) when
     * it would leave zero superadmins. Mirrors linux `remove_admin`.
     */
    fun removeAdmin(user: FfiAdminUser) {
        action {
            api.adminAdminsRemove(user.actorId)
            reloadUsers()
        }
    }

    /**
     * Mint a closed-registration invite code (empty code ⇒ the nest mints).
     * [guardianActor] binds the code to a supervising guardian (family-safety.md
     * § App surface — the `admin-users-invite-guardian-select`, default
     * `null` = an ordinary unsupervised code). [ageBand] is the
     * `admin-users-invite-age-band-select` wire token, `null` for *not set*
     * (§ App surface → *Age-band surfaces*; the nest refuses a band without a
     * guardian, and the screen gates the select the same way).
     */
    fun createInviteCode(tier: String, uses: Long, guardianActor: ByteArray? = null, ageBand: String? = null) {
        action {
            _mintedCode.value = api.adminInviteCodesCreate("", tier, uses.coerceAtLeast(1), guardianActor, ageBand)
            _inviteCodes.value = api.adminInviteCodesList()
        }
    }

    fun deleteInviteCode(code: String) {
        action {
            api.adminInviteCodesDelete(code)
            _inviteCodes.value = api.adminInviteCodesList()
        }
    }

    /**
     * Approve a pending request, admitting the requester at the chosen tier.
     * [guardianActor] binds the admitted account to a supervising guardian
     * (family-safety.md § App surface — the
     * `invite-request-row-guardian-select`, default `null`). [ageBand] is the
     * row's `invite-request-row-age-band-select` token, `null` for *not set* —
     * the admitting adult's decision, which the applicant's claim only seeds
     * (family-safety.md § The account age band, D5).
     */
    fun approveRequest(
        request: FfiAdminInviteRequest,
        tier: String,
        guardianActor: ByteArray? = null,
        ageBand: String? = null,
    ) {
        action {
            api.adminInviteRequestsApprove(request.id, tier, null, guardianActor, ageBand)
            _pendingRequests.value =
                api.adminInviteRequestsList().filter { it.status == STATUS_PENDING }
            reloadUsers()
        }
    }

    fun denyRequest(request: FfiAdminInviteRequest, reason: String?) {
        action {
            api.adminInviteRequestsDeny(request.id, reason?.takeIf { it.isNotBlank() })
            _pendingRequests.value =
                api.adminInviteRequestsList().filter { it.status == STATUS_PENDING }
        }
    }

    fun clearMintedCode() {
        _mintedCode.value = null
    }

    /**
     * Save the registration posture + free-tier ceiling together — ONE
     * `fauna.admin.set_registration_mode` call (admin.md § 2, Section 2). Not
     * reachable when the section is read-only (the screen renders no Save
     * button then), but guarded anyway: never dispatch with no parsed mode.
     * [maxFreeUsersInput] is the raw field text; the screen already filters it
     * to digits-only as typed (mirrors the invite-code max-uses field), so
     * blank is the only non-digit case here — blank sends `null` (clears the
     * cap), never `0`. After the write, re-reads `fauna.setup.status` so the
     * section reflects the *persisted* posture, not the local selection
     * (admin.md § 2: "no separate confirmation element" — the re-seed IS the
     * acknowledgement, mirrors web `saveRegistration`). The age require-knob
     * rides the same gesture: [ageVerificationRequired] is the toggle's draft,
     * dispatched as `fauna.admin.set_age_verification_required` beside the
     * mode call and ONLY when it differs from the persisted value
     * (family-safety.md § App surface → *Age-band surfaces*; tui's
     * `registration_mutation`).
     */
    fun saveRegistration(mode: FfiRegistrationMode, maxFreeUsersInput: String, ageVerificationRequired: Boolean) {
        action {
            val cap = maxFreeUsersInput.trim().takeIf { it.isNotEmpty() }?.toULong()
            api.adminSetRegistrationMode(mode, cap)
            if (ageVerificationRequired != _ageVerificationRequired.value) {
                api.adminSetAgeVerificationRequired(ageVerificationRequired)
            }
            reloadRegistration()
        }
    }

    /**
     * Admit a known actor id directly via `fauna.admin.users.create` — the
     * third account-creation path (`admin-users-admit-*`; public-mode.md §
     * Registration & Identity). [actorHex] must be exactly 64 hex chars — a
     * malformed id is a user error, never dispatched (the nest would refuse
     * it anyway; failing local keeps the message actionable), surfaced via
     * [invalidActorHint] on `admin-users-action-error` (mirrors tui's
     * `admit_mutation` / linux's admit-button handler). [handle] blank ⇒
     * `null`, the deliberate handle-less admission (public-mode.md § A
     * handle-less account). The form is never cleared here on success or
     * failure — the new row in the Users-section refetch is the feedback.
     */
    fun admitUser(actorHex: String, handle: String, tier: String, invalidActorHint: String) {
        val trimmed = actorHex.trim()
        val isHex = trimmed.length == 64 &&
            trimmed.all { it.isDigit() || it in 'a'..'f' || it in 'A'..'F' }
        if (!isHex) {
            _actionError.value = invalidActorHint
            return
        }
        action {
            api.adminUsersCreate(HexUtil.hexToBytes(trimmed), tier, handle.trim().takeIf { it.isNotEmpty() })
            reloadUsers()
        }
    }

    /**
     * Step to the page after the currently-loaded one, or no-op at the last
     * page — the shared stepper (`fauna_core::format::next_page_offset`, one
     * source of truth with the other six apps' pagination, admin.md § Users)
     * decides the boundary, never a hand-rolled `offset + PAGE_SIZE < total`
     * guard.
     */
    fun nextPage() {
        val next = com.fauna.ffi.nextPageOffset(_userOffset.value, _userTotal.value, PAGE_SIZE) ?: return
        _userOffset.value = next
        action { reloadUsers() }
    }

    /** The page before the currently-loaded one, or no-op at page 1. See
     *  [nextPage]. */
    fun prevPage() {
        val prev = com.fauna.ffi.prevPageOffset(_userOffset.value, PAGE_SIZE) ?: return
        _userOffset.value = prev
        action { reloadUsers() }
    }

    private suspend fun reloadUsers() {
        val reply = api.adminUsersList(limit = PAGE_SIZE, offset = _userOffset.value)
        _users.value = reply.users
        _userTotal.value = reply.total
        reloadAllUsers()
    }

    /**
     * Refetch [allUsers] alongside every [reloadUsers] call. A failed read
     * keeps the list already held rather than emptying the guardian pickers
     * (mirrors apple's `loadAllUsers`).
     */
    private suspend fun reloadAllUsers() {
        runCatching { api.adminUsersListAll() }.onSuccess { _allUsers.value = it }
    }

    /** Read `fauna.setup.status` and re-seed the registration StateFlows —
     *  shared by [refresh] (page load) and [saveRegistration] (post-write
     *  re-read). Parsing goes through the shared `registrationModeFromWire`
     *  (never a client-side string match) so an unrecognized posture renders
     *  read-only rather than falling back to a guessed variant. */
    private suspend fun reloadRegistration() {
        val status = api.nestSetupStatus()
        val raw = status.registrationMode
        val parsed = raw?.let { com.fauna.ffi.registrationModeFromWire(it) }
        _registrationMode.value = parsed
        _unknownRegistrationMode.value = if (parsed == null) raw else null
        _maxFreeUsers.value = status.maxFreeUsers?.toString() ?: ""
        _ageVerificationRequired.value = status.ageVerificationRequired
    }

    /** Run an admin action, routing any failure to `admin-users-action-error`. */
    private fun action(block: suspend () -> Unit) {
        viewModelScope.launch {
            _actionError.value = null
            try {
                block()
            } catch (e: Exception) {
                _actionError.value = e.message
            }
        }
    }

    companion object {
        /** Mirrors the Linux `DEFAULT_TIERS` fallback used before `tiers.list`. */
        val DEFAULT_TIERS = listOf("free", "personal", "community")
        const val STATUS_PENDING = "pending"

        /** Matches every other app's admin-users page size (admin.md § Users;
         *  value-formatting.md § Pagination). */
        const val PAGE_SIZE = 50L

        /**
         * The audit `category` the Users row's cut-off controls submit — the only
         * category the one-click row exposes (mirrors linux's literal `"other"`).
         */
        const val EVICTION_CATEGORY = "other"
    }
}
