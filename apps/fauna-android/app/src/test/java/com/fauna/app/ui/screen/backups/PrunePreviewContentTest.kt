package com.fauna.app.ui.screen.backups

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_backups_machine.CheckOutcome
import uniffi.fauna_backups_machine.BackupsSnapshot
import uniffi.fauna_backups_machine.PolicyState
import uniffi.fauna_backups_machine.PruneCandidate
import uniffi.fauna_backups_machine.PrunePreview

/**
 * Compose-level coverage for the two result surfaces of the Backups page
 * (`docs/goal/ui/backups.md` § Snapshot-list shape): the standing prune preview
 * and the completed check's verdict.
 *
 * These carry the four ids user-approved 2026-08-13
 * (`snapshot-prune-preview` / `-execute-button` / `-cancel-button` /
 * `snapshot-check-result`). They exist to make the *dry-run versus executed*
 * distinction observable, and that distinction rides entirely on
 * `snapshot-prune-execute-button`'s presence: it renders only over a preview
 * that names candidates, so its absence after an execute is how a test knows
 * the policy was applied. A row count cannot do that job — the nest's `list`
 * deliberately keeps soft-deleted rows for the 30-day undelete window, so a
 * pruned set has exactly the row count it started with.
 *
 * Android's e2e leg is gated on the host emulator setup, so without this the
 * ids would ship unverified on this app. The render is a pure function of the
 * machine's `PrunePreview` / `CheckOutcome`, so Robolectric proves the whole
 * contract here.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PrunePreviewContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun preview(
        policyState: PolicyState,
        candidates: List<PruneCandidate> = emptyList(),
        wouldPrune: Long = 0,
        remaining: Long = 3,
    ) = PrunePreview(
        wouldPrune = wouldPrune,
        remaining = remaining,
        candidates = candidates,
        policyState = policyState,
    )

    private fun candidate(id: Long) =
        PruneCandidate(id = id, createdAt = 1_700_000_000L, tags = emptyList())

    @Test
    fun aPreviewNamingACandidate_offersExecute() {
        composeTestRule.setContent {
            PrunePreviewContent(
                preview = preview(
                    PolicyState.APPLIED,
                    candidates = listOf(candidate(7)),
                    wouldPrune = 1,
                ),
                busy = false,
                onExecute = {},
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("snapshot-prune-preview").assertExists()
        composeTestRule.onNodeWithTag("snapshot-prune-execute-button").assertExists()
        composeTestRule.onNodeWithTag("snapshot-prune-cancel-button").assertExists()
    }

    @Test
    fun aPreviewWithNoCandidates_offersNoExecute() {
        // An armed execute over zero candidates would promise an effect it
        // cannot have — and would make the e2e's "the prune was applied" read
        // permanently false.
        composeTestRule.setContent {
            PrunePreviewContent(
                preview = preview(PolicyState.APPLIED),
                busy = false,
                onExecute = {},
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("snapshot-prune-preview").assertExists()
        composeTestRule.onNodeWithTag("snapshot-prune-execute-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("snapshot-prune-cancel-button").assertExists()
    }

    @Test
    fun aPreviewWithNoPolicy_offersNoExecute() {
        composeTestRule.setContent {
            PrunePreviewContent(
                preview = preview(PolicyState.NOT_SET),
                busy = false,
                onExecute = {},
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("snapshot-prune-preview").assertExists()
        composeTestRule.onNodeWithTag("snapshot-prune-execute-button").assertDoesNotExist()
    }

    /** "Nothing to prune" and "no retention policy configured" both stand a
     *  preview and both offer no execute, so the tagged node's OWN text is the
     *  only observable that says which one the nest returned (`ui/backups.md`
     *  § Errors & edge cases — *Prune with no candidates*). Read unmerged: the
     *  e2e bridge reads this node's text, not a join of its children's. */
    @Test
    fun theTwoNoOpVerdicts_rideThePreviewsOwnText() {
        var state by mutableStateOf(PolicyState.NOT_SET)
        composeTestRule.setContent {
            PrunePreviewContent(
                preview = preview(state),
                busy = false,
                onExecute = {},
                onCancel = {},
            )
        }
        val node = composeTestRule.onNodeWithTag("snapshot-prune-preview", useUnmergedTree = true)
        node.assertTextContains("No retention policy configured", substring = true)
        node.assert(!hasText("Nothing to prune", substring = true))

        state = PolicyState.APPLIED
        composeTestRule.waitForIdle()
        node.assertTextContains("Nothing to prune", substring = true)
        node.assert(!hasText("No retention policy configured", substring = true))
    }

    @Test
    fun aBusyMachine_withdrawsExecute() {
        composeTestRule.setContent {
            PrunePreviewContent(
                preview = preview(
                    PolicyState.APPLIED,
                    candidates = listOf(candidate(7)),
                    wouldPrune = 1,
                ),
                busy = true,
                onExecute = {},
                onCancel = {},
            )
        }
        composeTestRule.onNodeWithTag("snapshot-prune-execute-button").assertDoesNotExist()
    }

    @Test
    fun noCheckResult_rendersNoVerdict() {
        composeTestRule.setContent { CheckResultLine(null) }
        composeTestRule.onNodeWithTag("snapshot-check-result").assertDoesNotExist()
    }

    @Test
    fun aCompletedCheck_rendersItsVerdict() {
        composeTestRule.setContent {
            CheckResultLine(
                BackupsSnapshot(
                    folders = emptyList(),
                    selectedFolder = null,
                    snapshots = emptyList(),
                    lastBackedUp = null,
                    inProgressOp = null,
                    checkResult = CheckOutcome(
                        isOk = true,
                        snapshotsChecked = 4,
                        filesChecked = 12,
                        manifestsChecked = 4,
                        chunksChecked = 40,
                        missingManifests = 0,
                        missingChunks = 0,
                        corruptManifests = 0,
                        implicated = emptyList(),
                    ),
                    prunePreview = null,
                    detail = null,
                    error = null,
                )
            )
        }
        composeTestRule.onNodeWithTag("snapshot-check-result")
            .assertTextContains("Integrity check passed", substring = true)
    }
}
