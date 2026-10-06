package com.fauna.app.ui.screen.media

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_media_machine.FileVersionSummary
import uniffi.fauna_media_machine.FollowedScopeOption
import uniffi.fauna_media_machine.MediaItemSummary
import uniffi.fauna_media_machine.MediaPageSnapshot
import uniffi.fauna_media_machine.ShareCreateSnapshot
import uniffi.fauna_media_machine.ShareLinkSummary
import uniffi.fauna_media_machine.ShareLinksSnapshot

/**
 * Compose-level coverage for the stateless [MediaContent] (Media page,
 * `docs/goal/ui/media.md`) — the unified cross-set Windows-Explorer view after the
 * 2026-07-01 explorer rework. Renders with a seeded [MediaPageSnapshot] + injected
 * formatters / thumbnail-loader (so the Content is FFI-free) — no Hilt, no VM, no
 * FFI native calls — exercising the canonical ui.yaml `media` IDs + gesture
 * callbacks on the JVM. (Android E2E `test_media.py --client android` is the
 * standing gate once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MediaContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun item(
        name: String,
        set: String = "photos",
        online: Boolean = true,
        eligible: Boolean = false,
    ) = MediaItemSummary(
        folder = set,
        path = "$set/$name",
        name = name,
        sizeBytes = 2048L,
        updatedAt = 1_700_000_000L,
        thumbnailHash = null,
        sourceOnline = online,
        shareLinkEligible = eligible,
    )

    private fun snapshot(
        items: List<MediaItemSummary> = listOf(item("a.jpg"), item("b.mp4", set = "videos", online = false)),
        folders: List<String> = listOf("photos", "videos"),
        sort: String = "name",
        filter: String? = null,
        viewGrid: Boolean = false,
        loaded: Boolean = true,
        shareCreate: ShareCreateSnapshot? = null,
        shareLinks: ShareLinksSnapshot = ShareLinksSnapshot(open = false, loaded = false, rows = emptyList(), revokeConfirm = null),
    ) = MediaPageSnapshot(
        items = items,
        folders = folders,
        // A sibling field-add: the same entries with each set's id beside its
        // label; nothing this fixture renders reads them.
        folderOptions = emptyList(),
        sort = sort,
        descending = false,
        filter = filter,
        viewGrid = viewGrid,
        // Sibling field-adds this fixture had not caught up with: no followed
        // scopes, and none selected.
        followed = emptyList(),
        followedScope = null,
        error = null,
        // Fixtures render the LOADED page by default: an empty list is then a
        // real empty state, not a first-read still in flight (media.md
        // § Default view).
        loaded = loaded,
        shareCreate = shareCreate,
        shareExpiryOptions = listOf("1d", "7d", "30d", "1y"),
        shareLinks = shareLinks,
    )

    private fun version(
        num: Long,
        size: Long = 4096L,
        created: Long = 1_700_000_000_000L,
        authorDisplay: String = "bob",
        pruned: Boolean = false,
    ) = FileVersionSummary(
        versionNum = num,
        manifestHash = "hash$num",
        sizeBytes = size,
        createdAt = created,
        contentKeyVersion = null,
        authorDisplay = authorDisplay,
        pruned = pruned,
        purgeAfter = null,
    )

    private fun render(
        snapshot: MediaPageSnapshot? = snapshot(),
        actions: MediaActions = MediaActions(),
        pickedName: String? = null,
        detailActions: MediaDetailActions = MediaDetailActions(),
        shareActions: MediaShareActions = MediaShareActions(),
    ) {
        composeTestRule.setContent {
            MediaContent(
                snapshot = snapshot,
                actions = actions,
                pickedName = pickedName,
                formatSize = { "${it}B" },
                formatDate = { "date" },
                loadThumbnail = { null },
                syncStateLabel = "Synced",
                detailActions = detailActions,
                formatVersionTimestamp = { "vdate" },
                shareActions = shareActions,
            )
        }
    }

    // ── Share links (`share-links.md` § Flows; the `share-link-*` family) ──

    private fun linkRow(id: String, state: String, url: String? = null) = ShareLinkSummary(
        tokenId = id,
        name = "$id.txt",
        expiresAt = 1_800_000_000L,
        state = state,
        url = url,
    )

    @Test
    fun shareLinkButtonIsPresentOnlyOnAnEligibleItem() {
        var opened: Pair<String, String>? = null
        render(
            snapshot(items = listOf(item("pub.txt", eligible = true), item("priv.txt", set = "private"))),
            shareActions = MediaShareActions(onOpenCreate = { f, p -> opened = f to p }),
        )
        // The private item: absent, never inert.
        composeTestRule.onAllNodesWithTag("media-item")[1].performScrollTo().performClick()
        composeTestRule.onNodeWithTag("media-item-detail").assertExists()
        composeTestRule.onNodeWithTag("share-link-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("media-item-detail-close-button").performClick()
        // The public item: present, and it forwards the item's identity.
        composeTestRule.onAllNodesWithTag("media-item")[0].performScrollTo().performClick()
        composeTestRule.onNodeWithTag("share-link-button").performClick()
        assertEquals("photos" to "photos/pub.txt", opened)
    }

    @Test
    fun createSurfaceShowsNoUrlUntilTheMachineReportsOne() {
        var created = false
        render(
            snapshot(
                items = listOf(item("pub.txt", eligible = true)),
                shareCreate = ShareCreateSnapshot(name = "pub.txt", expiry = "7d", busy = false, url = null, keyInFragment = false),
            ),
            shareActions = MediaShareActions(onCreate = { created = true }),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("share-link-create-modal").assertExists()
        composeTestRule.onNodeWithTag("share-link-expiry-select").assertExists()
        composeTestRule.onNodeWithTag("share-link-url").assertDoesNotExist()
        composeTestRule.onNodeWithTag("share-link-copy-button").assertDoesNotExist()
        // While the surface is open the opener is withdrawn.
        composeTestRule.onNodeWithTag("share-link-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("share-link-create-button").performClick()
        assertTrue(created)
    }

    @Test
    fun createSurfaceRevealsTheUrlAndCopyOnceRegistered() {
        var closed = false
        render(
            snapshot(
                items = listOf(item("pub.txt", eligible = true)),
                shareCreate = ShareCreateSnapshot(
                    name = "pub.txt", expiry = "7d", busy = false, url = "https://nest.example/share/abc", keyInFragment = false,
                ),
            ),
            shareActions = MediaShareActions(onCloseCreate = { closed = true }),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("share-link-url").assertTextEquals("https://nest.example/share/abc")
        composeTestRule.onNodeWithTag("share-link-copy-button").assertExists()
        composeTestRule.onNodeWithTag("share-link-create-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("share-link-cancel-button").performClick()
        assertTrue(closed)
    }

    @Test
    fun shareLinkListButtonOpensTheList() {
        var opened = false
        render(shareActions = MediaShareActions(onOpenList = { opened = true }))
        composeTestRule.onNodeWithTag("share-link-list-button").performClick()
        assertTrue(opened)
    }

    @Test
    fun shareLinkListPaintsNeitherRowsNorEmptyStateWhileLoading() {
        render(snapshot(shareLinks = ShareLinksSnapshot(open = true, loaded = false, rows = emptyList(), revokeConfirm = null)))
        composeTestRule.onNodeWithTag("share-link-list").assertExists()
        composeTestRule.onNodeWithTag("share-link-empty-state").assertDoesNotExist()
        composeTestRule.onNodeWithTag("share-link-item").assertDoesNotExist()
    }

    @Test
    fun shareLinkListPaintsTheEmptyStateOnceLoaded() {
        render(snapshot(shareLinks = ShareLinksSnapshot(open = true, loaded = true, rows = emptyList(), revokeConfirm = null)))
        composeTestRule.onNodeWithTag("share-link-empty-state").assertExists()
    }

    @Test
    fun shareLinkRowsOfferCopyOnlyWithAUrlAndRevokeOnlyWhenActive() {
        var armed: String? = null
        render(
            snapshot(
                shareLinks = ShareLinksSnapshot(
                    open = true,
                    loaded = true,
                    rows = listOf(linkRow("a", "active", url = "https://nest.example/share/a"), linkRow("r", "revoked")),
                    revokeConfirm = null,
                ),
            ),
            shareActions = MediaShareActions(onArmRevoke = { armed = it }),
        )
        composeTestRule.onAllNodesWithTag("share-link-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("share-link-item-copy-button", useUnmergedTree = true).assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("share-link-revoke-button", useUnmergedTree = true).assertCountEquals(1)
        // The stable state rides `stateDescription` (the e2e `get_attr(…, "state")`).
        composeTestRule.onAllNodesWithTag("share-link-item-state", useUnmergedTree = true)[1]
            .assert(SemanticsMatcher.expectValue(androidx.compose.ui.semantics.SemanticsProperties.StateDescription, "revoked"))
        composeTestRule.onAllNodesWithTag("share-link-revoke-button", useUnmergedTree = true)[0].performClick()
        assertEquals("a", armed)
    }

    @Test
    fun revokeConfirmForwardsConfirmAndCancel() {
        var confirmed = false
        var cancelled = false
        render(
            snapshot(
                shareLinks = ShareLinksSnapshot(
                    open = true, loaded = true, rows = listOf(linkRow("a", "active")), revokeConfirm = "a",
                ),
            ),
            shareActions = MediaShareActions(onConfirmRevoke = { confirmed = true }, onCancelRevoke = { cancelled = true }),
        )
        composeTestRule.onNodeWithTag("share-link-revoke-confirm-modal").assertExists()
        composeTestRule.onNodeWithTag("share-link-revoke-confirm-button").performClick()
        composeTestRule.onNodeWithTag("share-link-revoke-cancel-button").performClick()
        assertTrue(confirmed)
        assertTrue(cancelled)
    }

    @Test
    fun explorerChromeRendersAllIds() {
        render()
        composeTestRule.onNodeWithTag("media-view-toggle").assertExists()
        composeTestRule.onNodeWithTag("media-sort-select").assertExists()
        composeTestRule.onNodeWithTag("media-folder-filter").assertExists()
        composeTestRule.onNodeWithTag("file-upload").assertExists()
        composeTestRule.onNodeWithTag("upload-button").assertExists()
    }

    @Test
    fun viewToggleShowsCurrentViewAndFires() {
        var toggled = false
        // List view → label "List".
        render(snapshot(viewGrid = false), MediaActions(onToggleView = { toggled = true }))
        composeTestRule.onNodeWithText("List").assertExists()
        composeTestRule.onNodeWithTag("media-view-toggle").performClick()
        assertTrue(toggled)
    }

    @Test
    fun gridViewToggleShowsGridLabel() {
        render(snapshot(viewGrid = true))
        composeTestRule.onNodeWithText("Grid").assertExists()
    }

    @Test
    fun filePickerAndUploadButtonFire() {
        var picked = false
        var uploaded = false
        render(
            actions = MediaActions(onPickFile = { picked = true }, onUpload = { uploaded = true }),
            pickedName = "vacation.mp4",
        )
        composeTestRule.onNodeWithTag("file-upload").performClick()
        assertTrue(picked)
        composeTestRule.onNodeWithTag("upload-button").assertIsEnabled().performClick()
        assertTrue(uploaded)
    }

    /**
     * The empty-path guard (media.md § User actions): with nothing picked the
     * button stays live and still reaches the caller, which answers
     * `media.file_required` on `error-message` — a disabled button is the dead
     * button the goal doc forbids.
     */
    @Test
    fun uploadButtonStaysLiveWithNothingPicked() {
        var uploaded = false
        render(actions = MediaActions(onUpload = { uploaded = true }), pickedName = null)
        composeTestRule.onNodeWithTag("upload-button").assertIsEnabled().performClick()
        assertTrue(uploaded)
    }

    /** `media-thumbnail` publishes its `state` on `stateDescription` — `placeholder`
     *  while no decoded picture stands in (the e2e `thumbnail_kind` read). */
    @Test
    fun thumbnailWithoutPicturePublishesPlaceholderState() {
        render()
        composeTestRule.onAllNodesWithTag("media-thumbnail", useUnmergedTree = true)
            .assertAll(SemanticsMatcher.expectValue(
                androidx.compose.ui.semantics.SemanticsProperties.StateDescription,
                THUMBNAIL_PLACEHOLDER,
            ))
    }

    @Test
    fun mediaItemsRenderIndexedWithChildrenAndSourceStatus() {
        render()
        composeTestRule.onAllNodesWithTag("media-item").assertCountEquals(2)
        // media-item is now clickable (opens media-item-detail), which merges its
        // descendants' semantics for accessibility — same as the clickable
        // `conversation-item` row (ConversationsListContentTest); query its children
        // via the unmerged tree, matching that established convention.
        composeTestRule.onAllNodesWithTag("media-item-name", useUnmergedTree = true).assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("media-item-size", useUnmergedTree = true).assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("media-item-date", useUnmergedTree = true).assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("media-thumbnail", useUnmergedTree = true).assertCountEquals(2)
        // media-source-status renders both online + offline dots.
        composeTestRule.onAllNodesWithTag("media-source-status", useUnmergedTree = true).assertCountEquals(2)
        composeTestRule.onNodeWithText("a.jpg", useUnmergedTree = true).assertExists()
        composeTestRule.onNodeWithText("Files reachable", useUnmergedTree = true).assertExists()
        composeTestRule.onNodeWithText("Files unreachable", useUnmergedTree = true).assertExists()
    }

    @Test
    fun mediaItemsRenderSyncStateBadge() {
        render()
        // Every item on this control-plane surface renders the same injected label.
        composeTestRule.onAllNodesWithTag("sync-state-badge", useUnmergedTree = true).assertCountEquals(2)
        composeTestRule.onAllNodesWithText("Synced", useUnmergedTree = true).assertCountEquals(2)
    }

    @Test
    fun emptyStateShownWhenNoMedia() {
        render(snapshot(items = emptyList()))
        composeTestRule.onNodeWithText("No media yet").assertExists()
        composeTestRule.onNodeWithTag("media-empty-state").assertExists()
        composeTestRule.onAllNodesWithTag("media-item").assertCountEquals(0)
    }

    /**
     * The negative half of the three-state rule (`README.md` § *List pages:
     * loading is not empty*): before the first `fauna.media.list` returns the
     * page holds no items AND has no business saying so. Asserted together
     * with [emptyStateShownWhenNoMedia] on purpose — a gate wired to a field
     * that never becomes true would make the empty state vanish forever, which
     * is worse than the bug it fixes, and only the pair catches that.
     *
     * Note there is no `media-loading` id to assert: the ABSENCE of
     * `media-empty-state` beside zero `media-item` rows *is* the loading state.
     */
    @Test
    fun emptyStateWithheldWhileTheFirstReadIsStillInFlight() {
        render(snapshot(items = emptyList(), loaded = false))
        composeTestRule.onNodeWithTag("media-empty-state").assertDoesNotExist()
        composeTestRule.onNodeWithText("No media yet").assertDoesNotExist()
        composeTestRule.onAllNodesWithTag("media-item").assertCountEquals(0)
    }

    /** A null snapshot is the app's very first frame — also not loaded. */
    @Test
    fun emptyStateWithheldBeforeAnySnapshotArrives() {
        render(snapshot = null)
        composeTestRule.onNodeWithTag("media-empty-state").assertDoesNotExist()
    }

    @Test
    fun sortSelectShowsCurrentKeyLabel() {
        // "size" key resolves to the "Size" label in the anchor.
        render(snapshot(sort = "size"))
        composeTestRule.onNodeWithTag("media-sort-select").assertExists()
        composeTestRule.onAllNodesWithText("Size")[0].assertExists()
    }

    @Test
    fun filterSelectShowsSelectedSetName() {
        render(snapshot(filter = "photos"))
        composeTestRule.onNodeWithTag("media-folder-filter").assertExists()
        composeTestRule.onAllNodesWithText("photos")[0].assertExists()
    }

    @Test
    fun filterSelectDefaultsToAllMedia() {
        render(snapshot(filter = null))
        composeTestRule.onAllNodesWithText("All media")[0].assertExists()
    }

    // ── media-item-detail + file-version-history (media.md § Element IDs) ──────

    @Test
    fun mediaItemDetailOpensAndClosesOnNameAndCloseButton() {
        render()
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("media-item-detail").assertExists()
        composeTestRule.onNodeWithTag("media-item-detail-name").assertTextEquals("a.jpg")
        composeTestRule.onNodeWithTag("media-item-detail-close-button").performClick()
        composeTestRule.onNodeWithTag("media-item-detail").assertDoesNotExist()
    }

    @Test
    fun fileVersionHistoryRendersRowsFromLoader() {
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, _ -> listOf(version(2), version(1)) },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("file-version-list").assertExists()
        composeTestRule.onAllNodesWithTag("file-version-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("file-version-timestamp").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("file-version-size").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("file-version-restore-button").assertCountEquals(2)
    }

    @Test
    fun fileVersionAuthorRendersOnEveryRow() {
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, _ ->
                    listOf(version(2, authorDisplay = "alice"), version(1, authorDisplay = "bob"))
                },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        // Every row names its own recorder.
        composeTestRule.onAllNodesWithTag("file-version-author").assertCountEquals(2)
        composeTestRule.onNodeWithText("Edited by alice").assertExists()
        composeTestRule.onNodeWithText("Edited by bob").assertExists()
    }

    @Test
    fun showPrunedToggleExistsAndDefaultsOff() {
        render(detailActions = MediaDetailActions(loadVersions = { _, _, _ -> listOf(version(1)) }))
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("file-version-show-pruned-toggle").assertExists()
        // A live-only listing never renders the pruned badge.
        composeTestRule.onAllNodesWithTag("file-version-pruned-badge").assertCountEquals(0)
    }

    @Test
    fun togglingShowPrunedReissuesTheLoadWithIncludePruned() {
        var lastIncludePruned: Boolean? = null
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, includePruned ->
                    lastIncludePruned = includePruned
                    if (includePruned) listOf(version(2), version(1, pruned = true)) else listOf(version(2))
                },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.waitForIdle()
        assertEquals(false, lastIncludePruned)
        composeTestRule.onNodeWithTag("file-version-show-pruned-toggle").performClick()
        composeTestRule.waitForIdle()
        assertEquals(true, lastIncludePruned)
        composeTestRule.onAllNodesWithTag("file-version-item").assertCountEquals(2)
        // Exactly the pruned row carries the badge + undelete button.
        composeTestRule.onAllNodesWithTag("file-version-pruned-badge").assertCountEquals(1)
        composeTestRule.onAllNodesWithTag("file-version-undelete-button").assertCountEquals(1)
    }

    @Test
    fun undeleteFiresTheCallbackAndReloadsOnSuccess() {
        var undeletedPath: String? = null
        var undeletedVersionNum: Long? = null
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, includePruned ->
                    if (includePruned) listOf(version(1, pruned = true)) else emptyList()
                },
                onUndeleteVersion = { path, num -> undeletedPath = path; undeletedVersionNum = num; true },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("file-version-show-pruned-toggle").performClick()
        composeTestRule.onAllNodesWithTag("file-version-undelete-button")[0].performClick()
        composeTestRule.waitForIdle()
        assertEquals("photos/a.jpg", undeletedPath)
        assertEquals(1L, undeletedVersionNum)
    }

    @Test
    fun restoreVersionRequiresConfirmAndFiresCallbackWithCorrectArgs() {
        var restoredFolder: String? = null
        var restoredPath: String? = null
        var restoredVersion: FileVersionSummary? = null
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, _ -> listOf(version(1)) },
                onRestoreVersion = { fs, path, v ->
                    restoredFolder = fs; restoredPath = path; restoredVersion = v
                    true
                },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        // Clicking restore does NOT fire the callback directly — it opens the confirm.
        composeTestRule.onAllNodesWithTag("file-version-restore-button")[0].performClick()
        assertEquals(null, restoredFolder)
        composeTestRule.onNodeWithTag("file-version-restore-confirm-modal").assertExists()
        composeTestRule.onNodeWithTag("file-version-restore-confirm-button").performClick()
        assertEquals("photos", restoredFolder)
        assertEquals("photos/a.jpg", restoredPath)
        assertEquals(1L, restoredVersion?.versionNum)
        composeTestRule.onNodeWithTag("file-version-restore-confirm-modal").assertDoesNotExist()
    }

    @Test
    fun restoreVersionCancelIsAPureNoOp() {
        var restored = false
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, _ -> listOf(version(1)) },
                onRestoreVersion = { _, _, _ -> restored = true; true },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onAllNodesWithTag("file-version-restore-button")[0].performClick()
        composeTestRule.onNodeWithTag("file-version-restore-cancel-button").performClick()
        composeTestRule.onNodeWithTag("file-version-restore-confirm-modal").assertDoesNotExist()
        assertTrue(!restored)
    }

    @Test
    fun deleteRequiresConfirmAndClosesDetailOnSuccess() {
        var deletedFolder: String? = null
        var deletedPath: String? = null
        render(
            detailActions = MediaDetailActions(
                onDeleteItem = { fs, path -> deletedFolder = fs; deletedPath = path; true },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("media-delete-button").performClick()
        composeTestRule.onNodeWithTag("media-delete-confirm-modal").assertExists()
        assertEquals(null, deletedFolder)
        composeTestRule.onNodeWithTag("media-delete-confirm-button").performClick()
        assertEquals("photos", deletedFolder)
        assertEquals("photos/a.jpg", deletedPath)
        // The detail surface closes — its subject is gone (mirrors linux).
        composeTestRule.onNodeWithTag("media-item-detail").assertDoesNotExist()
    }

    @Test
    fun deleteCancelIsAPureNoOp() {
        var deleted = false
        render(
            detailActions = MediaDetailActions(onDeleteItem = { _, _ -> deleted = true; true }),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("media-delete-button").performClick()
        composeTestRule.onNodeWithTag("media-delete-cancel-button").performClick()
        composeTestRule.onNodeWithTag("media-delete-confirm-modal").assertDoesNotExist()
        composeTestRule.onNodeWithTag("media-item-detail").assertExists()
        assertTrue(!deleted)
    }

    @Test
    fun deleteStaysOpenOnFailure() {
        render(
            detailActions = MediaDetailActions(onDeleteItem = { _, _ -> false }),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("media-delete-button").performClick()
        composeTestRule.onNodeWithTag("media-delete-confirm-button").performClick()
        // Failure (e.g. still-in-flight quota/error) keeps the surface open — the page
        // error banner carries the failure (media.md § Errors & edge cases).
        composeTestRule.onNodeWithTag("media-item-detail").assertExists()
    }

    // ── media-item-detail-download-button (media.md § Element IDs) ─────────────

    @Test
    fun downloadButtonAbsentUntilAVersionRowCarriesAManifest() {
        render(detailActions = MediaDetailActions(loadVersions = { _, _, _ -> emptyList() }))
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.waitForIdle()
        composeTestRule.onNodeWithTag("media-item-detail-download-button").assertDoesNotExist()
    }

    @Test
    fun downloadButtonKeysTheWalkByTheLatestVersionRow() {
        var got: Triple<MediaItemSummary, FileVersionSummary?, String?>? = null
        render(
            detailActions = MediaDetailActions(
                // Rows arrive oldest→newest: the last one is the current file.
                loadVersions = { _, _, _ -> listOf(version(1), version(2)) },
                onDownload = { item, latest, followed -> got = Triple(item, latest, followed); null },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("media-item-detail-download-button")
            .assertTextEquals("Download").performClick()
        composeTestRule.waitForIdle()
        assertEquals("photos/a.jpg", got?.first?.path)
        assertEquals(2L, got?.second?.versionNum)
        assertEquals(null, got?.third)
    }

    @Test
    fun downloadFailureLandsOnTheDetailsOwnStatusLine() {
        render(
            detailActions = MediaDetailActions(
                loadVersions = { _, _, _ -> listOf(version(1)) },
                onDownload = { _, _, _ -> "boom" },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.onNodeWithTag("media-item-detail-download-button").performClick()
        composeTestRule.waitForIdle()
        composeTestRule.onNodeWithText("Failed to download: boom").assertExists()
        composeTestRule.onNodeWithTag("media-item-detail").assertExists()
    }

    // ── Followed public folders (media.md § Followed public folders) ───────────

    private val followedOpt = FollowedScopeOption(
        value = "followed:opaque-1",
        label = "Holiday pics (alice)",
        available = true,
    )

    private fun followedSnapshot() = snapshot(
        items = listOf(item("pub.jpg", set = "Holiday pics")),
        filter = followedOpt.value,
    ).copy(followed = listOf(followedOpt), followedScope = followedOpt)

    @Test
    fun followedOptionsAppendToTheFilterAndRouteToTheScopeGesture() {
        var selected: String? = null
        var filtered = false
        render(
            snapshot().copy(followed = listOf(followedOpt)),
            actions = MediaActions(onSelectFollowed = { selected = it }, onSetFilter = { filtered = true }),
        )
        composeTestRule.onNodeWithTag("media-folder-filter").performClick()
        // The option displays the minted label, never the opaque value.
        composeTestRule.onNodeWithText("Holiday pics (alice)").performClick()
        assertEquals("followed:opaque-1", selected)
        assertTrue(!filtered)
    }

    @Test
    fun activeFollowedScopeShowsItsLabelAndWithholdsUpload() {
        render(followedSnapshot())
        composeTestRule.onAllNodesWithText("Holiday pics (alice)")[0].assertExists()
        composeTestRule.onNodeWithText("followed:opaque-1").assertDoesNotExist()
        composeTestRule.onNodeWithTag("file-upload").assertDoesNotExist()
        composeTestRule.onNodeWithTag("upload-button").assertDoesNotExist()
    }

    @Test
    fun followedItemDetailOffersDownloadAloneAndRoutesItKeylessly() {
        var versionsLoaded = false
        var got: Triple<MediaItemSummary, FileVersionSummary?, String?>? = null
        render(
            followedSnapshot(),
            detailActions = MediaDetailActions(
                loadVersions = { _, _, _ -> versionsLoaded = true; listOf(version(1)) },
                onDownload = { item, latest, followed -> got = Triple(item, latest, followed); null },
            ),
        )
        composeTestRule.onAllNodesWithTag("media-item")[0].performClick()
        composeTestRule.waitForIdle()
        // Head-only: no version read, no history, no recovery browse, no delete.
        assertTrue(!versionsLoaded)
        composeTestRule.onNodeWithTag("file-version-list").assertDoesNotExist()
        composeTestRule.onNodeWithTag("file-version-show-pruned-toggle").assertDoesNotExist()
        composeTestRule.onNodeWithTag("media-delete-button").assertDoesNotExist()
        // Download is painted at once — no manifest to wait for.
        composeTestRule.onNodeWithTag("media-item-detail-download-button").performClick()
        composeTestRule.waitForIdle()
        assertEquals("Holiday pics/pub.jpg", got?.first?.path)
        assertEquals(null, got?.second)
        assertEquals("followed:opaque-1", got?.third)
    }
}
