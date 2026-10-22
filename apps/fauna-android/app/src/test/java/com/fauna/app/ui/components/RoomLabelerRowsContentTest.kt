package com.fauna.app.ui.components

import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertTextContains
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.LabelerCatalogEntryFixture
import com.fauna.app.testing.LabelerInspectViewFixture
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_conversations.HistoryPolicy
import uniffi.fauna_conversations.JoinRule
import uniffi.fauna_conversations.RoomSettingsDraft
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot

/**
 * Compose-level coverage for the stateless [RoomLabelerRowsContent] — the
 * community room's labeler set inside the room policy editor
 * (`ui/conversations.md` § Element IDs → *The room's labeler set*). Seeded
 * with a [LabelerCatalogSnapshot] and a hand-built draft; no Hilt, no VM. The
 * shared predicates (`roomMayNameLabelerKind`, the draft's staged / live reads)
 * run for real through the host `.so`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class RoomLabelerRowsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun draft(labelers: List<String>?) = RoomSettingsDraft(
        joinRule = JoinRule.INVITE,
        historyPolicy = HistoryPolicy.NONE,
        seats = emptyList(),
        admins = emptyList(),
        owners = emptyList(),
        eligible = emptyList(),
        transferTo = null,
        seedJoinRule = JoinRule.INVITE,
        seedHistoryPolicy = HistoryPolicy.NONE,
        seedAdmins = emptyList(),
        nestRead = null,
        seedNestRead = null,
        labelers = labelers,
        seedLabelers = labelers,
    )

    private fun snapshot(vararg ids: Pair<String, String>, inspecting: Boolean = false) =
        LabelerCatalogSnapshot(
            entries = ids.map { (id, kind) ->
                LabelerCatalogEntryFixture.make(labelerId = id, subscribed = false, artifactKind = kind)
            },
            inspecting = if (inspecting) LabelerInspectViewFixture.make() else null,
            error = null,
            loaded = true,
        )

    private fun render(
        snapshot: LabelerCatalogSnapshot?,
        draft: RoomSettingsDraft,
        canSetPolicy: Boolean = true,
        onDraftChange: (RoomSettingsDraft) -> Unit = {},
        onInspect: (Int) -> Unit = {},
    ) {
        composeTestRule.setContent {
            RoomLabelerRowsContent(
                snapshot = snapshot,
                draft = draft,
                canSetPolicy = canSetPolicy,
                onDraftChange = onDraftChange,
                onInspect = onInspect,
                onCloseInspect = {},
            )
        }
    }

    @Test
    fun noLabelersOnTheDraftPaintsNothing() {
        render(snapshot("aa" to "wasm"), draft(labelers = null))
        composeTestRule.onAllNodesWithTag(Ids.ROOM_LABELER_TOGGLE).assertCountEquals(0)
        composeTestRule.onAllNodesWithTag(Ids.ROOM_LABELER_INSPECT_BUTTON).assertCountEquals(0)
    }

    @Test
    fun oneRowPerNameableLabelerInCatalogOrder() {
        render(snapshot("aa" to "wasm", "bb" to "list", "cc" to "text-model"), draft(labelers = listOf("aa")))
        composeTestRule.onAllNodesWithTag(Ids.ROOM_LABELER_TOGGLE).assertCountEquals(2)
        composeTestRule.onAllNodesWithTag(Ids.ROOM_LABELER_INSPECT_BUTTON).assertCountEquals(2)
    }

    @Test
    fun toggleTextNamesTheKindAndAnIdPrefix() {
        val id = "0123456789abcdef0123"
        render(snapshot(id to "wasm"), draft(labelers = emptyList()))
        composeTestRule.onNodeWithTag(Ids.ROOM_LABELER_TOGGLE)
            .assertTextContains("wasm · 0123456789abcdef…", substring = true)
    }

    @Test
    fun toggleStagesThroughTheSharedDraft() {
        var staged: RoomSettingsDraft? = null
        render(
            snapshot("aa" to "wasm"),
            draft(labelers = emptyList()),
            onDraftChange = { staged = it },
        )
        composeTestRule.onNodeWithTag(Ids.ROOM_LABELER_TOGGLE).performClick()
        assertEquals(listOf("aa"), staged?.labelers)
    }

    @Test
    fun toggleIsGreyedWithoutThePolicyCapability() {
        render(snapshot("aa" to "wasm"), draft(labelers = emptyList()), canSetPolicy = false)
        composeTestRule.onNodeWithTag(Ids.ROOM_LABELER_TOGGLE).assertIsNotEnabled()
        composeTestRule.onNodeWithTag(Ids.ROOM_LABELER_INSPECT_BUTTON).assertIsEnabled()
    }

    @Test
    fun inspectCarriesTheCatalogSnapshotIndex() {
        var opened: Int? = null
        render(
            snapshot("aa" to "wasm", "bb" to "wasm"),
            draft(labelers = emptyList()),
            onInspect = { opened = it },
        )
        composeTestRule.onAllNodesWithTag(Ids.ROOM_LABELER_INSPECT_BUTTON)[1].performClick()
        assertEquals(1, opened)
    }

    @Test
    fun anOpenInspectViewReplacesTheRowsInPlace() {
        render(snapshot("aa" to "wasm", inspecting = true), draft(labelers = emptyList()))
        composeTestRule.onNodeWithTag(Ids.LABELER_INSPECT_PANEL).assertExists()
        composeTestRule.onAllNodesWithTag(Ids.ROOM_LABELER_TOGGLE).assertCountEquals(0)
    }
}
