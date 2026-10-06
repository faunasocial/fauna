package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * `e2e-conventions.md` convention 11 — what `session.authenticated` means: the
 * authenticated app is MOUNTED, never merely "credentials exist on disk".
 * `TestAgent.serializeState` used to OR in `storage.secretHex != null`, so a
 * credentialed relaunch routed through the onboarding wizard (verify-404, an
 * awaiting-manual-dns "Almost ready") published `true` while sitting on
 * onboarding — the arrival check every native app's tests share
 * (`tests/common/launch_harness.py::reached_authenticated_app`) would then run
 * a whole test body against the wizard instead of the app.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentAuthenticatedFlagTest {

    private fun authenticated(appState: AppState, storage: SecureStorage): Boolean =
        TestAgent.serializeState(appState, storage, null)
            .getJSONObject("session")
            .getBoolean("authenticated")

    @Test
    fun storedCredentialsAloneDoNotReadAsAuthenticatedWhileOnboardingIsMounted() {
        val storage = mock(SecureStorage::class.java)
        whenever(storage.secretHex).thenReturn("abcdef")
        val appState = AppState() // isOnboarding defaults true; no override set

        assertFalse(
            "a stored secret is not a session — the app must actually be past onboarding",
            authenticated(appState, storage),
        )
    }

    @Test
    fun theExplicitOverrideWinsEvenWhileOnboardingIsStillMounted() {
        val storage = mock(SecureStorage::class.java)
        val appState = AppState()
        appState.session.isAuthenticated = true // the set_state login-shortcut override

        assertTrue(
            "an explicit set_state({session:{authenticated:true}}) must win over the derived fact",
            authenticated(appState, storage),
        )
    }

    @Test
    fun theDerivedFactReadsTrueOnceTheAuthenticatedAppIsMounted() {
        val storage = mock(SecureStorage::class.java)
        val appState = AppState()
        appState.isOnboarding = false // AppLaunchVM reached NavTarget.Authenticated

        assertTrue(
            "the app is genuinely mounted past onboarding with no override needed",
            authenticated(appState, storage),
        )
    }
}
