package com.fauna.app.ui.screen.folders

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import com.fauna.app.p2pshare.SharePlaneReadings
import com.fauna.app.p2pshare.SharePlaneTransferRow
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The peer-transfer surface on the Folders page (`p2p-shared-set-build.md`
 * § Phone peers — design, decisions 2 and 5): the six `share-*` ids paint
 * exactly the readings shared Rust resolved, and a plane that is not running
 * (`null`) paints nothing at all — no line, no list.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FoldersSharePlaneContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(sharePlane: SharePlaneReadings?) {
        composeTestRule.setContent {
            FoldersContent(
                snapshot = null,
                actions = FoldersActions(),
                wizardActions = WizardActions(),
                sharePlane = sharePlane,
            )
        }
    }

    @Test
    fun aStoppedPlaneRendersNothing() {
        render(sharePlane = null)
        composeTestRule.onNodeWithTag("share-serve-status").assertDoesNotExist()
        composeTestRule.onNodeWithTag("share-transfer-list").assertDoesNotExist()
    }

    @Test
    fun aRunningPlanePaintsItsServeLineAndOneItemPerTransfer() {
        render(
            SharePlaneReadings(
                serveStatus = "Serving 1 shared folder",
                transfers = listOf(
                    SharePlaneTransferRow("docs from Ann's device", "2 file(s), 3 change(s) this pass", "Up to date"),
                    SharePlaneTransferRow("photos from Bo's device", "0 file(s), 0 change(s) this pass", "Limited by free space on this device"),
                ),
            ),
        )
        composeTestRule.onNodeWithTag("share-serve-status").assertTextEquals("Serving 1 shared folder")
        composeTestRule.onNodeWithTag("share-transfer-list").assertExists()
        composeTestRule.onAllNodesWithTag("share-transfer-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("share-transfer-name", useUnmergedTree = true)[0]
            .assertTextEquals("docs from Ann's device")
        composeTestRule.onAllNodesWithTag("share-transfer-progress", useUnmergedTree = true)[0]
            .assertTextEquals("2 file(s), 3 change(s) this pass")
        composeTestRule.onAllNodesWithTag("share-transfer-state", useUnmergedTree = true)[1]
            .assertTextEquals("Limited by free space on this device")
    }
}
