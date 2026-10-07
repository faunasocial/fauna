package com.fauna.app.ui.screen.onboarding

import com.fauna.ffi.onboarding.OnboardingStep
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The two identity-recovery steps route to their own pages now that android
 * renders them (`onboarding.md` § 1 Identity) — until this leg landed both
 * fell back to handle entry, which only stayed harmless because android never
 * declared `setRendersRecoveryKit` nor rendered the restore button.
 */
class RecoveryStepRoutingTest {
    @Test
    fun theRecoveryStepsRouteToTheirOwnPages() {
        assertEquals(RECOVERY_KIT_ROUTE, routeForStep(OnboardingStep.RECOVERY_KIT))
        assertEquals(RECOVERY_ENTRY_ROUTE, routeForStep(OnboardingStep.RECOVERY_ENTRY))
        assertEquals("onboarding/recovery-kit", RECOVERY_KIT_ROUTE)
        assertEquals("onboarding/recovery-entry", RECOVERY_ENTRY_ROUTE)
    }
}
