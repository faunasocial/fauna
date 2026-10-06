package com.fauna.app.core

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.navigation.NavHostController
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * Shared message state for the persistent banner — and the **category-1 display
 * funnel** for observability.md: a displayed message is logged into the shared
 * `fauna-log` ring at the *producer* (where the state is set), not in the
 * `MessageBanner` per-tick render ("log on the *event*, not the *paint*"). The
 * fields are read-only `StateFlow` (the banner + TestAgent read them); producers
 * set them through [showError]/[showWarning]/[showInfo], which also log.
 */
class AppMessages {
    private val _error = MutableStateFlow<String?>(null)
    private val _warning = MutableStateFlow<String?>(null)
    private val _info = MutableStateFlow<String?>(null)
    private val _refusedAgentCommand = MutableStateFlow<String?>(null)

    /** Read-only — the `MessageBanner` paint and `TestAgent` observe these. */
    val error: StateFlow<String?> = _error
    val warning: StateFlow<String?> = _warning
    val info: StateFlow<String?> = _info

    /**
     * A test-agent command the app **refused** — convention 11's loud failure
     * (`testing.md` point 11), written only by [reportRefusedAgentCommand] and
     * cleared only by [clearRefusedAgentCommand]. Deliberately NOT one of the
     * three fields above, and deliberately untouched by [clear].
     *
     * Why a dedicated slot, when [error] already paints `error-message`: this is
     * a port of tui's `App::refused_agent_command`, whose doc comment records the
     * two slots that were tried first and are WRONG — both of which look right.
     * Android had shipped the first of them since 2026-07-19:
     *
     * 1. **The general [error] banner** — `FaunaNavHost`'s shell runs
     *    `LaunchedEffect(currentRoute) { appState.messages.clear() }`, so *any*
     *    route change wipes it. `TestAgent`'s `patch {"nav": …}` arm navigates,
     *    and every action-layer helper in `tests/e2e-unified/actions/` issues one
     *    — so a refusal raised by command N was wiped microseconds later by the
     *    navigation of command N+1, and the driver read `error=''`. This is the
     *    same failure tui shipped for one commit; only a mutation caught it there.
     * 2. **A per-page error slot** — survives a *same-page* nav, but a refusal
     *    raised on the feed page is invisible once the helper navigates to
     *    conversations. It passes whichever scenario happens to re-navigate to
     *    the same page, which is how it looks correct.
     *
     * So: page-independent (a refusal is not a property of a page) and
     * nav-independent, with a lifetime of exactly **one test** — `TestAgent`'s
     * `reset` arm is the only clear point, and `reset` is what every `app`
     * fixture drives before a test body, so a refusal can neither be wiped early
     * by navigation nor leak into the next test of a reused app process.
     *
     * Inert in release: nothing outside `TestAgent` ever writes it, and R8 strips
     * `TestAgent` from the release APK (convention 15) — mirroring tui, where the
     * field itself is ungated and only its writer/reader carry `#[cfg]`.
     */
    val refusedAgentCommand: StateFlow<String?> = _refusedAgentCommand

    /** Show an error banner and record it in the ring at `error` level. */
    fun showError(message: String?) {
        _error.value = message
        message?.let { ShellLog.e(TARGET, it) }
    }

    /** Show a warning banner and record it in the ring at `warn` level. */
    fun showWarning(message: String?) {
        _warning.value = message
        message?.let { ShellLog.w(TARGET, it) }
    }

    /** Show an info/success banner and record it in the ring at `info` level. */
    fun showInfo(message: String?) {
        _info.value = message
        message?.let { ShellLog.i(TARGET, it) }
    }

    /** Dismiss without logging (the banner close button / launch reset). */
    fun clearError() {
        _error.value = null
    }

    fun clearWarning() {
        _warning.value = null
    }

    fun clearInfo() {
        _info.value = null
    }

    /**
     * Record a refused test-agent command in [refusedAgentCommand]. [detail]
     * names *why* it was refused (a rejected payload, a manager that is still
     * null, a thrown exception); omit it for an action the agent does not
     * implement at all.
     */
    fun reportRefusedAgentCommand(action: String, detail: String? = null) {
        val why = detail
            ?: "not implemented on android, or its payload/preconditions were rejected"
        val text = "test agent refused command \"$action\": $why"
        _refusedAgentCommand.value = text
        ShellLog.e(TARGET, text)
    }

    /**
     * The per-test clear point for [refusedAgentCommand] — driven only by
     * `TestAgent`'s `reset` arm. Deliberately separate from [clear], which
     * navigation calls.
     */
    fun clearRefusedAgentCommand() {
        _refusedAgentCommand.value = null
    }

    /**
     * The text the `error-message` surface shows: a refusal **outranks** the
     * page's own error, because it means the app never did what the driver
     * asked, so every later product assertion is reading a state the test did
     * not actually set up. Sole owner of that precedence rule — `MessageBanner`
     * (the paint) and `TestAgent.serializeState` (the state protocol) both go
     * through here.
     */
    fun errorForDisplay(): String? = _refusedAgentCommand.value ?: _error.value

    /**
     * Dismiss the three transient messages. Called on every navigation
     * (`FaunaNavHost`) — which is exactly why it must NOT touch
     * [refusedAgentCommand]; see that field's doc.
     */
    fun clear() {
        _error.value = null
        _warning.value = null
        _info.value = null
    }

    companion object {
        private const val TARGET = "AppMessages"
    }
}

class AppState {
    var isOnboarding by mutableStateOf(true)

    /**
     * What a sign-out's erase could not remove, while it still owes work — the
     * whole state of the `sign-out-residue` view on `identity_choice`
     * (`account-scoping.md` § Erasure follows scope → *the residue surface*).
     * `null` is the clean outcome and the only one; a clean sweep says nothing.
     *
     * Held here rather than passed down the callback chain because a sign-out
     * *replaces* the composable tree — the authenticated shell that raised it is
     * gone before the onboarding wizard that must paint it exists. Painted by the
     * wizard's root surface, which is the one a sign-out always lands on: the
     * credentials are gone by then, so the shared LaunchMachine routes to
     * `IDENTITY_CHOICE`.
     *
     * Set by the sign-out, by the signed-out launch's silent re-check (the
     * record outlives the process — `AppLaunchVM.recheckSignOutResidue`), and by
     * Remove Again, whose `null` closes the view. Never cleared on display: until
     * a re-sweep says otherwise the statement is still true.
     */
    var signOutResidue: com.fauna.ffi.FfiSignOutResidue? by mutableStateOf(null)
    // "Add account" append-mode overlay (long-term-store.md § Multi-account
    // evolution) — mutually exclusive with [isOnboarding]; takes priority when
    // both could apply (FaunaNavHost checks it first).
    var isAddingAccount by mutableStateOf(false)
    var navController: NavHostController? = null
    val session = SessionState()
    val messages = AppMessages()
}

class SessionState {
    var isAuthenticated by mutableStateOf(false)
    var nodeUrl: String? by mutableStateOf(null)
    var secretHex: String? by mutableStateOf(null)
    var handle: String? by mutableStateOf(null)
    var deviceId: String? by mutableStateOf(null)
    var actorId: String? by mutableStateOf(null)
}
