package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ShellLog
import com.fauna.app.core.atproto.AtprotoSettingsHost
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import uniffi.fauna_atproto_settings_machine.AtprotoSettingsMachine
import javax.inject.Inject

/**
 * Renders the shared `AtprotoSettingsMachine` (libs/fauna-atproto-settings-machine,
 * over UniFFI, via [AtprotoSettingsHost]) for the `atproto` page (docs/goal/ui/atproto.md):
 * the four-rung integration-depth selector, the transition card, the hosted panel, and the F1
 * login-plane surface (app credentials, connected-app sessions, the
 * external-apps kill-switch). Per priority #2 this view-model holds **no**
 * ATProto logic (level semantics, transition-card content, gate interpretation
 * — all of it lives in shared Rust); it dispatches gestures to the host's
 * machine and mirrors its snapshot. Hybrid of [MailSettingsVM]'s secret-needing build
 * (credential secrets are custodied client-side under the BackupKey) and
 * [LabelerCatalogVM]'s observer-driven repaint. Linux lead:
 * apps/fauna-linux/src/settings/atproto.rs.
 *
 * The machine itself lives on [AtprotoSettingsHost], NOT on this VM: a
 * `hiltViewModel()`-scoped machine gets rebuilt every time the user leaves
 * and re-enters this page, which silently disables the S4-C custody alarm's
 * one-convergence debounce (`AtprotoSettingsHost`'s own doc comment has the
 * full story — `docs/goal/behavior/critical-alerts.md` feeder #1).
 */
@HiltViewModel
class AtprotoVM @Inject constructor(
    private val host: AtprotoSettingsHost,
) : ViewModel() {

    /** The whole renderable AT Protocol page, shared across every mount via [host]. */
    val snapshot: StateFlow<uniffi.fauna_atproto_settings_machine.AtprotoSettingsSnapshot>
        get() = host.snapshot

    fun refresh() {
        val m = host.machine() ?: return
        viewModelScope.launch {
            try {
                m.refresh()
            } catch (e: Exception) {
                ShellLog.w("AtprotoVM", "refresh failed: ${e.message}")
            }
            host.refreshSnapshot()
        }
    }

    fun selectLevel(targetLevel: String) = dispatch { it.selectLevel(targetLevel) }

    fun confirmTransition() = dispatch { it.confirmTransition() }

    fun cancelTransition() {
        host.machine()?.cancelTransition()
        host.refreshSnapshot()
    }

    fun setDidMethod(method: String) {
        host.machine()?.setDidMethod(method)
        host.refreshSnapshot()
    }

    fun setHistoryBackfill(enabled: Boolean) {
        host.machine()?.setHistoryBackfill(enabled)
        host.refreshSnapshot()
    }

    fun setExternalAppsEnabled(enabled: Boolean) = dispatch { it.setExternalAppsEnabled(enabled) }

    /**
     * Mint a new app credential and return its `(credentialId, secret)` so the
     * caller can show the secret inline on the just-minted row — the ONLY time
     * it is ever shown (`AtprotoSettingsMachine::mint` doc: "returns the secret
     * even if the local persist fails"; callers must also surface
     * `snapshot().error`, which [host]'s republish already carries). Diffs
     * before/after credential ids the same way the linux lead does, since the
     * snapshot itself carries no "the credential I just minted" pointer.
     */
    suspend fun mint(label: String, dmAllowed: Boolean): Pair<String, String>? {
        val m = host.machine() ?: return null
        val before = m.snapshot().credentials.map { it.credentialId }.toSet()
        val secret = try {
            m.mint(label, dmAllowed)
        } catch (e: Exception) {
            ShellLog.w("AtprotoVM", "mint failed: ${e.message}")
            null
        }
        host.refreshSnapshot()
        if (secret == null) return null
        val newId = m.snapshot().credentials.map { it.credentialId }.firstOrNull { it !in before }
            ?: return null
        return newId to secret
    }

    suspend fun revealSecret(credentialId: String): String? {
        val m = host.machine() ?: return null
        return try {
            m.revealSecret(credentialId)
        } catch (e: Exception) {
            ShellLog.w("AtprotoVM", "reveal secret failed: ${e.message}")
            null
        }
    }

    fun revoke(credentialId: String) = dispatch { it.revoke(credentialId) }

    /**
     * Authorize external ATProto apps to POST as this account — the whole D10
     * mint ceremony in one gesture (`atproto-delegation-authorize`).
     *
     * Also the RENEWAL gesture: provisioning overwrites the stored cert with a
     * freshly dated one, so a lapsed grant recovers with no revoke first
     * (`atproto-pds-full.md` § App surface). Which is why the screen keeps this
     * control rendered on a live row instead of hiding it once authorized.
     */
    fun authorizeExternalApps() = dispatch { it.authorizeExternalApps() }

    /**
     * Revoke the authoring delegation (`atproto-delegation-revoke`) —
     * destructive: it destroys the signing sub-key K nest-side. Already
     * published posts stay verifiable forever (their cert rides their own
     * wire), so this stops FUTURE authoring, never history.
     */
    fun deauthorizeExternalApps() = dispatch { it.deauthorizeExternalApps() }

    // ── The 72 h recovery-fork contest (`atproto-contest-*`) ──────────────
    //
    // Wholly client-side (`atproto-identity-custody.md` § The 72 h
    // recovery-fork contest, decision 9): every call is the device's own
    // connection, never a nest round trip — `openContestConfirm`/
    // `cancelContest` are synchronous (the machine notifies the host's
    // observer itself, same as `cancelTransition` above);
    // `requestContest` is the one network round trip (to the public PLC
    // directory), which is why it still goes through `dispatch`.

    fun openContestConfirm() {
        host.machine()?.openContestConfirm()
        host.refreshSnapshot()
    }

    fun cancelContest() {
        host.machine()?.cancelContest()
        host.refreshSnapshot()
    }

    fun requestContest() = dispatch { it.requestContest() }

    // ── "Delete my Bluesky presence" (`atproto-delete-*`) ─────────────────
    //
    // Row 7's six-app trickle-down: open/cancel are pure-local machine
    // mutations (the machine notifies the host's observer itself, same as
    // `cancelTransition` above); confirm is the one network round trip (the
    // nest's `fauna.bridges.atproto.delete_presence`), so it goes through
    // `dispatch`. Linux lead: `apps/fauna-linux/src/settings/atproto.rs`.

    fun openDeleteConfirm() {
        host.machine()?.openDeleteConfirm()
        host.refreshSnapshot()
    }

    fun cancelDelete() {
        host.machine()?.cancelDelete()
        host.refreshSnapshot()
    }

    fun confirmDelete() = dispatch { it.confirmDelete() }

    private fun dispatch(action: suspend (AtprotoSettingsMachine) -> Unit) {
        val m = host.machine() ?: return
        viewModelScope.launch {
            try {
                action(m)
            } catch (e: Exception) {
                ShellLog.w("AtprotoVM", "gesture failed: ${e.message}")
            }
            host.refreshSnapshot()
        }
    }
}
