package com.fauna.app.ui.navigation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import com.fauna.app.BuildConfig
import com.fauna.app.ui.viewmodel.AppLaunchVM.NavTarget

/** `fauna_e2e_contract::ROOT_SURFACE_WIZARD` — the onboarding wizard, any step. */
const val ROOT_SURFACE_WIZARD = "wizard"

/** `fauna_e2e_contract::ROOT_SURFACE_LAUNCH_PROGRESS` — ui.yaml `onboarding.launch_progress`. */
const val ROOT_SURFACE_LAUNCH_PROGRESS = "launch_progress"

/** `fauna_e2e_contract::ROOT_SURFACE_APP` — the signed-in app, any page. */
const val ROOT_SURFACE_APP = "app"

/**
 * The name the launch surface [FaunaNavHost] mounts for [target] carries in
 * `fauna_e2e_agent::ROOT_SURFACE_KEY`'s journal: its ui.yaml page, or a
 * `launch_*` name for a surface ui.yaml gives no page of its own — tui's
 * `LaunchSurface::root_surface_name` (`apps/fauna-tui/src/launch.rs`), whose
 * names these are. `null` is the launch in flight; [NavTarget.Authenticated]
 * is the session start that follows it, which shows the same progress view
 * (`onboarding.md` § Wizard exit handling → *The wizard becomes the app in
 * place*). [stolenEntry] is the locked surface's ceremony step, [retiring] the
 * `nest_retire` surface hosted over the retry surface.
 *
 * A total `when`, compiled into every flavor: a new target must be named here.
 */
fun rootSurfaceName(
    target: NavTarget?,
    stolenEntry: Boolean = false,
    retiring: Boolean = false,
): String = when (target) {
    null, NavTarget.Authenticated -> ROOT_SURFACE_LAUNCH_PROGRESS
    is NavTarget.Wizard, is NavTarget.Superseded -> ROOT_SURFACE_WIZARD
    NavTarget.TransientError -> if (retiring) "nest_retire" else "launch_retry"
    NavTarget.NeedsUpdate -> "launch_needs_update"
    NavTarget.SignInRefused -> "launch_sign_in_refused"
    is NavTarget.AccountLocked -> if (stolenEntry) "identity_stolen_entry" else "launch_account_locked"
    NavTarget.IdentityChanged, is NavTarget.IdentityForked -> "launch_identity_changed"
    is NavTarget.AccountIndexUnreadable -> "launch_account_index_unreadable"
}

/**
 * Tell the test agent which surface this composition's root shows — read by
 * its per-frame `root_surface` journal (`E2eLoudSurfaces`), which records
 * `null` instead whenever the frame painted no text and no element. A
 * `SideEffect`, so the name lands with the composition it names, ahead of the
 * draw that paints it. `BuildConfig.DEBUG`-gated: the shrinker folds it out of release,
 * where [com.fauna.app.testing.TestAgent.rootSurface] is a no-op twin.
 */
@Composable
fun RootSurface(name: String) {
    if (BuildConfig.DEBUG) {
        SideEffect { com.fauna.app.testing.TestAgent.rootSurface = name }
    }
}
