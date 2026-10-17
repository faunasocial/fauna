package com.fauna.app.core

import androidx.navigation.NavController
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.fauna_core.AppRoute

/**
 * Android's one door for the `fauna://consent/<request_uri>` route — the
 * same-device handoff's app half (`docs/goal/architecture/apps/android.md`
 * § Navigation → *In-app routes*; mechanism
 * `docs/goal/behavior/authorization-server.md` § Consent → *How the same-device
 * handoff is built*). The Apple twin is FaunaKit's `ConsentHandoff`.
 *
 * `MainActivity` hands every `ACTION_VIEW` URI — the launch intent and
 * `onNewIntent` alike — to [offer], which parses ONLY through the shared
 * `parseAppRoute` (never a second parser in Kotlin). The route is navigation
 * only: the shell navigates to Settings → Connected apps and the page's
 * view-model opens the staged request through the shared machine's
 * `openHandoff`, which reveals its card in the requests tray.
 *
 * A route that arrives signed out is *held* and applied once the authenticated
 * shell mounts ([applyHeld]). Process-wide ([shared]): one pending open at a
 * time is the deep-link semantic, and the activity, the shell and the page all
 * reach it without threading state through the nav graph.
 *
 * Compiled from `src/noKids/` only: the `kids` build type compiles the twin
 * under `src/kids/`, which registers the scheme (the manifest is shared) but
 * drops every route — a kids artifact has no Connected apps page.
 */
class ConsentHandoff {

    /** A consent route that arrived while signed out — applied by [takeHeld]. */
    var held: String? = null
        private set

    private val _pending = MutableStateFlow<String?>(null)

    /**
     * The staged `request_uri` the Connected apps page opens next. Cleared only
     * AFTER the machine's `openHandoff` has returned ([finishOpen]), so a waiter
     * (the e2e `open_route` command) knows the card is painted.
     */
    val pending: StateFlow<String?> = _pending

    /**
     * Offer a URI to the door. Returns `true` when it was a consent route; when
     * [authenticated] the request is staged for the page, otherwise it is held.
     * Anything else — an unparseable URI, an unknown route, or one of the share
     * routes, which have no producer on android — is dropped, never an error.
     */
    fun receive(uri: String, authenticated: Boolean): Boolean {
        val requestUri = consentRequestUri(uri) ?: return false
        if (authenticated) _pending.value = requestUri else held = requestUri
        return true
    }

    /**
     * Move a held route into the staged slot now the session is authenticated.
     * Returns `true` when there was one (the caller then navigates).
     */
    fun takeHeld(): Boolean {
        val h = held ?: return false
        held = null
        _pending.value = h
        return true
    }

    /**
     * The app shell's leg of the door: [receive] with the shell's own notion of
     * signed in (the authenticated shell is mounted — neither the launch/wizard
     * surface nor the add-account overlay), then navigate to the page when the
     * request was staged.
     */
    fun offer(uri: String, appState: AppState): Boolean {
        val authenticated = !appState.isOnboarding && !appState.isAddingAccount
        if (!receive(uri, authenticated)) return false
        if (authenticated) appState.navController?.navigate(ROUTE)
        return true
    }

    /** Apply a held route on the authenticated shell's mount. */
    fun applyHeld(navController: NavController) {
        if (takeHeld()) navController.navigate(ROUTE)
    }

    /**
     * The page's open finished (the machine's `openHandoff` returned): clear the
     * staged request, but only if it is still the one that was opened — a newer
     * route staged meanwhile stays pending.
     */
    fun finishOpen(requestUri: String) {
        _pending.compareAndSet(requestUri, null)
    }

    /** Wait until nothing is staged, or [timeoutMs] elapses; returns whether it drained. */
    suspend fun waitUntilDrained(timeoutMs: Long): Boolean =
        withTimeoutOrNull(timeoutMs) { _pending.first { it == null }; true } ?: false

    companion object {
        /** The process-wide door every caller shares. */
        val shared = ConsentHandoff()

        /** The Connected apps page's route (`KidsExcisedNav.kt`). */
        const val ROUTE = "settings/connected-apps"

        /** The consent route's `request_uri`, or null for any URI that is not exactly that route. */
        fun consentRequestUri(uri: String): String? =
            (com.fauna.ffi.parseAppRoute(uri) as? AppRoute.Consent)?.requestUri
    }
}
