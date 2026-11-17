package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.ScrollState
import androidx.compose.runtime.Composable
import androidx.navigation.NavController

// The profile page's Posts tab — **the excised half**. Compiled into the `kids`
// build type only, in place of the `src/noKids/` twin, and empty by design: the
// tab's rows are the feed's own cards over an author window of the feed manager,
// and a kids artifact carries no feed (`family-safety.md` § The account age band,
// the kids-app bullet, item (4); `dynamic-features.md` § Compile-time excision) —
// the same seam as `KidsExcisedNav`.

/** Excised — no feed, so no author window and no post cards. */
@Suppress("UNUSED_PARAMETER")
@Composable
fun ProfilePostsSection(
    navController: NavController,
    actorIdHex: String,
    scrollState: ScrollState,
) = Unit
