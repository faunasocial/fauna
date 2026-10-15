package com.fauna.app.core

import androidx.navigation.NavController

/**
 * The `fauna://consent` route's door — **the excised half** (`android.md`
 * § Navigation → *In-app routes*; `dynamic-features.md` § Compile-time
 * excision). Compiled into the `kids` build type only, in place of the
 * `src/noKids/` twin: the manifest still registers the `fauna` scheme, but a
 * kids artifact has no Connected apps page, so every route is dropped.
 */
class ConsentHandoff {

    /** Excised — every route is dropped. */
    @Suppress("UNUSED_PARAMETER")
    fun offer(uri: String, appState: AppState): Boolean = false

    /** Excised — nothing is ever held. */
    @Suppress("UNUSED_PARAMETER")
    fun applyHeld(navController: NavController) = Unit

    companion object {
        val shared = ConsentHandoff()
    }
}
