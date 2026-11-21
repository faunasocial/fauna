package com.fauna.app.ui.navigation

import androidx.compose.runtime.Composable
import androidx.navigation.NavGraphBuilder
import androidx.navigation.NavHostController

// The parts of the app shell (`FaunaNavHost`) the **kids** build type
// excises — **the excised half** (`family-safety.md` § The account age band,
// the kids-app bullet, item (4); `dynamic-features.md` § Compile-time
// excision). Compiled into the `kids` build type only, in place of the
// `src/noKids/` twin, and empty by design: the feed, search, every bridge,
// web publishing, monetization and third-party destinations are not
// registered, the bridge post-auth legs do not run, and the critical-alerts
// banner (whose only feeder is the Bluesky settings machine) is not mounted.
// A screen only those destinations reach is therefore unreferenced here, and
// the release shrinker drops it from the kids artifact.

/** Excised — no feed, search, bridge, web, monetization or third-party destination. */
@Suppress("UNUSED_PARAMETER", "UnusedReceiverParameter")
fun NavGraphBuilder.kidsExcisedDestinations(navController: NavHostController) = Unit

/** Excised — every leg of the bridge post-auth hook is a bridge. */
@Suppress("UNUSED_PARAMETER")
@Composable
fun KidsExcisedPostAuthEffects(messages: com.fauna.app.core.AppMessages) = Unit

/** Excised — the critical-alert registry's only feeder is the Bluesky plane. */
@Composable
fun CriticalAlertsBanner() = Unit

/** Excised — discovery is compiled out of Fauna Kids, so its submit is never offered. */
@Suppress("UNUSED_PARAMETER")
fun searchSubmitPossibility(query: String): uniffi.fauna_core.Possibility =
    uniffi.fauna_core.Possibility.Absent

/** Excised — mail is compiled out of Fauna Kids, so its sub-page rows are never shown. */
@Composable
fun mailUsableAnswer(): uniffi.fauna_core.Possibility = uniffi.fauna_core.Possibility.Absent
