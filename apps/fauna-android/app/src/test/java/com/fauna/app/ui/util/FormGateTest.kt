package com.fauna.app.ui.util

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.Possibility

/**
 * The submit half of the form case (`docs/goal/ui/README.md` § Offer only what
 * is possible, rule 4): a submit asks the SAME shared predicate its button was
 * painted from and refuses with that reason — [formRefusal] is the one place
 * every android form turns the answer into the `error-message` text. Over the
 * live UniFFI faces (`just android-host-test`), so the reason is the shared
 * predicate's, not a Kotlin restatement.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FormGateTest {

    private val context: Context = ApplicationProvider.getApplicationContext()

    @Test
    fun aBlankPostRefusesWithTheSharedReason() {
        val refusal = formRefusal(context, com.fauna.ffi.feedPostSubmitPossibility("   "))
        assertEquals("Post cannot be empty", refusal?.reason)
    }

    @Test
    fun aBlankSearchRefusesWithTheSharedReason() {
        val refusal = formRefusal(context, com.fauna.ffi.searchQueryPossibility(""))
        assertEquals("Type something to search for first.", refusal?.reason)
    }

    @Test
    fun aDuplicateRelayRefusesInsteadOfDroppingSilently() {
        val refusal = formRefusal(
            context,
            com.fauna.ffi.nostrAddRelayPossibility(listOf("wss://relay.damus.io"), "wss://relay.damus.io"),
        )
        assertEquals("This relay is already in your list.", refusal?.reason)
    }

    @Test
    fun aValidDraftGoes() {
        assertNull(formRefusal(context, com.fauna.ffi.feedReplySubmitPossibility("hello")))
        assertNull(formRefusal(context, com.fauna.ffi.nostrAddFollowPossibility("npub1abc")))
    }

    @Test
    fun anAbsentAnswerRefusesWithNothingToSay() {
        val refusal = formRefusal(context, com.fauna.ffi.nostrConnectedAppsPossibility("remote"))
        assertEquals(FormRefusal(null), refusal)
    }

    // ── The family, folders and profile forms (apps 6040's lift) ──

    @Test
    fun theFamilyFormsRefuseWithTheirOwnSharedReasons() {
        assertEquals(
            "Type the actor ID to pre-approve first.",
            formRefusal(context, com.fauna.ffi.familyContactAddPossibility(" "))?.reason,
        )
        assertEquals(
            "Not a valid actor ID (expected hex)",
            formRefusal(context, com.fauna.ffi.familyContactAddPossibility("zz"))?.reason,
        )
        // The transfer has its own words, never the contact-add string.
        assertEquals(
            "Not a valid actor ID for the new guardian (expected hex)",
            formRefusal(context, com.fauna.ffi.familyTransferPossibility("nothex"))?.reason,
        )
        assertNull(formRefusal(context, com.fauna.ffi.familyTransferPossibility("cd".repeat(32))))
        val screenTime = formRefusal(context, com.fauna.ffi.familyPolicySavePossibility("22:00", "", ""))
        assertEquals(true, screenTime?.reason?.startsWith("Screen time cannot be saved: "))
        assertNull(formRefusal(context, com.fauna.ffi.familyPolicySavePossibility("", "", "")))
    }

    @Test
    fun theFamilySurfaceIsTheSharedAnswer() {
        assertEquals(false, com.fauna.ffi.familyHasSurface(false, 0u, 0u))
        assertEquals(true, com.fauna.ffi.familyHasSurface(false, 0u, 1u))
    }

    @Test
    fun theFolderFormsRefuseWithTheSharedReasons() {
        assertEquals(
            "Sign in first, then follow the folder.",
            formRefusal(context, com.fauna.ffi.folderFollowPossibility("alice", "Pictures", false))?.reason,
        )
        assertEquals(
            "Type who to share with first.",
            formRefusal(context, com.fauna.ffi.folderSharePossibility(" ", true))?.reason,
        )
        assertEquals(
            "Sharing needs your conversations to be ready — wait a moment, then try again.",
            formRefusal(context, com.fauna.ffi.folderSharePossibility("bob", false))?.reason,
        )
        assertNull(formRefusal(context, com.fauna.ffi.folderSharePossibility("bob", true)))
    }

    @Test
    fun theSubscriptionFormsRefuseWithTheSharedReasons() {
        assertEquals(
            "Name the tier first.",
            formRefusal(context, com.fauna.ffi.subscriptionTierFormPossibility(" "))?.reason,
        )
        assertEquals(
            "Type the claim code first.",
            formRefusal(context, com.fauna.ffi.subscriptionRedeemCodePossibility(""))?.reason,
        )
        assertNull(formRefusal(context, com.fauna.ffi.subscriptionRedeemCodePossibility("CODE42")))
    }

    @Test
    fun askFormFailsOpenWhenTheFaceThrows() {
        assertEquals(Possibility.Possible, askForm { throw UnsatisfiedLinkError("no .so") })
    }
}
