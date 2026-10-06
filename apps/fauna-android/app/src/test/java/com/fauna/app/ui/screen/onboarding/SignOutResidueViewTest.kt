package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.Box
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onChildren
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiCredentialSweep
import com.fauna.ffi.FfiEraseResidueView
import com.fauna.ffi.FfiEraseSweep
import com.fauna.ffi.FfiSignOutResidue
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertSame
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * The `sign-out-residue` view's paint gate and its retry wiring on
 * `identity_choice` (`account-scoping.md` § Erasure follows scope → *the
 * residue surface*): present exactly while a residue owes work, its message the
 * shared `Rendered` line, and Remove Again handing back the very residue it
 * painted for the shared re-sweep.
 *
 * FFI-touching (the residue is built by shared Rust's `signOutResidueRecord`) —
 * runs green via `just android-host-test`, like [com.fauna.app.core.AccountStoresEraseTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SignOutResidueViewTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @get:Rule
    val tmp = TemporaryFolder()

    /** A residue of one survivor, recorded under a throwaway install base. */
    private fun residue(): FfiSignOutResidue {
        val sweep = FfiEraseSweep(
            erased = 0u,
            survivors = listOf(tmp.root.resolve("aa11".repeat(16)).absolutePath),
            residue = FfiEraseResidueView(survivors = 1u, credentialsSurvived = false, owesWork = true),
        )
        val residue = com.fauna.ffi.signOutResidueRecord(
            tmp.root.absolutePath,
            sweep,
            FfiCredentialSweep(survivors = emptyList(), wipeFailed = false),
        )
        assertNotNull("precondition: a survivor owes work", residue)
        return residue!!
    }

    @Test
    fun noResiduePaintsNoView() {
        composeTestRule.setContent {
            Box(modifier = Modifier.testTag("container")) {
                SignOutResidueView(residue = null, onRemoveAgain = {})
            }
        }
        composeTestRule.onNodeWithTag("container").onChildren().assertCountEquals(0)
    }

    @Test
    fun anOwingResiduePaintsTheMessageAndRemoveAgain() {
        val owing = residue()
        composeTestRule.setContent { SignOutResidueView(residue = owing, onRemoveAgain = {}) }

        composeTestRule.onNodeWithTag(Ids.SIGN_OUT_RESIDUE).assertIsDisplayed()
        composeTestRule.onNodeWithTag(Ids.SIGN_OUT_RESIDUE_MESSAGE)
            .assertTextContains("Remove Again", substring = true)
        composeTestRule.onNodeWithTag(Ids.SIGN_OUT_RESIDUE_RETRY_BUTTON).assertIsDisplayed()
    }

    @Test
    fun removeAgainHandsBackThePaintedResidue() {
        val owing = residue()
        var retried: FfiSignOutResidue? = null
        composeTestRule.setContent {
            SignOutResidueView(residue = owing, onRemoveAgain = { retried = it })
        }

        composeTestRule.onNodeWithTag(Ids.SIGN_OUT_RESIDUE_RETRY_BUTTON).performClick()

        assertSame("Remove Again re-sweeps exactly the residue on screen", owing, retried)
    }
}
