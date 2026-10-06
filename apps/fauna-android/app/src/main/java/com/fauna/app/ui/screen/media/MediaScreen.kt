package com.fauna.app.ui.screen.media

import android.graphics.BitmapFactory
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.Image
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.shareFileBytes
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.MediaVM
import com.fauna.ffi.formatUnixLocalDate
import com.fauna.ffi.shareLinkExpiryLabel
import com.fauna.ffi.shareLinkStateLabel
import com.fauna.ffi.syncDisplayStateLabel
import kotlinx.coroutines.launch
import uniffi.fauna_core.SyncDisplayState
import uniffi.fauna_media_machine.FileVersionSummary
import uniffi.fauna_media_machine.FollowedScopeOption
import uniffi.fauna_media_machine.MediaItemSummary
import uniffi.fauna_media_machine.MediaPageSnapshot
import uniffi.fauna_media_machine.ShareCreateSnapshot
import uniffi.fauna_media_machine.ShareLinkSummary
import uniffi.fauna_media_machine.ShareLinksSnapshot
import social.fauna.generated.Ids

/**
 * Media page (`docs/goal/ui/media.md`) — the unified cross-set **Windows-Explorer**
 * view over the media inside the user's folders (the paradigm settled 2026-06-28:
 * folders are the substrate, Media is their media-optimized viewer). It defaults
 * to the aggregated **all-media** view across every readable set, with a list ↔
 * thumbnail-grid toggle, name/size/date sort, and a per-set filter. Rendered
 * ENTIRELY off the shared [MediaPageSnapshot] the [MediaVM]-bound `MediaMachine`
 * (`libs/fauna-media-machine`) publishes — no cross-set aggregation, sort, filter,
 * or upload/thumbnail logic client-side (priority #2). Mirrors the linux LEAD
 * (`apps/fauna-linux/src/views/media/{mod,item}.rs`) + windows/apple.
 *
 * Media is the **content plane only** (rule 4): it reads folders, it never
 * configures them — set create/place-flags/retention/roster/conflicts all live
 * in Settings → Folders (`FoldersScreen`), not here.
 *
 * The stateless [MediaContent] is split out so it renders under the Compose test
 * harness with a seeded snapshot + injected formatters / thumbnail-loader (FFI-free
 * for Robolectric). The page error flows to the navigation shell's `error-message`
 * banner via [LocalAppMessages] (mirrors `FoldersScreen`).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MediaScreen(
    navController: NavController,
    vm: MediaVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val deepLinkedItem by vm.deepLinkedItem.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current

    LaunchedEffect(Unit) { vm.start() }

    // `media-item-detail` gestures — one set for the tiles and the deep-linked detail.
    val detailActions = remember(vm, context) {
        MediaDetailActions(
            loadVersions = vm::loadVersions,
            onRestoreVersion = vm::restoreVersion,
            onDeleteItem = vm::deleteItem,
            onUndeleteVersion = vm::undeleteVersion,
            // media-item-detail-download-button: the shared query fetches the
            // plaintext, then android's save path — the share sheet, the same
            // one the backups single-file download uses — takes it.
            onDownload = { item, latest, followedScope ->
                vm.downloadBytes(item, latest, followedScope).fold(
                    onSuccess = { bytes ->
                        runCatching { context.shareFileBytes(item.name, bytes) }.exceptionOrNull()
                            ?.let { it.message ?: it.toString() }
                    },
                    onFailure = { it.message ?: it.toString() },
                )
            },
        )
    }

    // Share-link gestures (`share-links.md` § Flows) — pure forwards to the
    // shared machine; the surfaces render off the snapshot.
    val shareActions = remember(vm) {
        MediaShareActions(
            onOpenCreate = vm::openShareCreate,
            onSetExpiry = vm::setShareExpiry,
            onCreate = vm::createShareLink,
            onCloseCreate = vm::closeShareCreate,
            onOpenList = vm::openShareLinks,
            onCloseList = vm::closeShareLinks,
            onArmRevoke = vm::armShareRevoke,
            onCancelRevoke = vm::cancelShareRevoke,
            onConfirmRevoke = vm::confirmShareRevoke,
        )
    }

    // Page error → the navigation shell's `error-message` banner.
    val errorText = localized(snapshot?.error)
    LaunchedEffect(errorText) {
        if (errorText != null) appMessages.showError(errorText)
    }

    // android `file-upload` divergence: a system file picker (a mobile device has
    // no typed path). The picked Uri is held here; the upload reads its bytes
    // off-thread in the VM. ui.yaml's canonical type is text_input — the divergence
    // is documented in ui-actual-android.yaml.
    var pickedUri by remember { mutableStateOf<Uri?>(null) }
    val picker = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent()
    ) { uri: Uri? -> pickedUri = uri }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.media_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
            )
        },
    ) { padding ->
        MediaContent(
            modifier = Modifier.padding(padding),
            snapshot = snapshot,
            pickedName = pickedUri?.lastPathSegment,
            actions = MediaActions(
                onToggleView = vm::toggleView,
                onSetSort = vm::setSort,
                onSetFilter = vm::setFilter,
                onSelectFollowed = vm::selectFollowed,
                onPickFile = { picker.launch("*/*") },
                // upload-button with nothing picked: say so on `error-message`
                // (`media.file_required`), never a disabled or silent button —
                // which presents as a dead one (media.md § User actions).
                onUpload = {
                    val uri = pickedUri
                    if (uri == null) {
                        appMessages.showError(context.getString(R.string.media_file_required))
                    } else {
                        vm.upload(uri)
                        pickedUri = null
                    }
                },
            ),
            formatSize = { ValueFormat.byteSize(context, it) },
            // media-item-date: unix seconds → the shared relative/absolute display.
            formatDate = { ValueFormat.relativeTime(context, it * 1000) },
            loadThumbnail = vm::fetchThumbnail,
            // sync-state-badge (file-sync.md § Per-file sync-status display): the
            // unified Media page is a control-plane surface (fauna.sync.files-backed
            // MediaMachine snapshot, same as linux/web/windows-in-app) — every item it
            // lists already exists as a nest folder member, so it renders the
            // constant Synced state via the shared label, never a hand-written string
            // (mirrors linux `build_state_badge` / `sync_display_state_label`).
            syncStateLabel = localized(syncDisplayStateLabel(SyncDisplayState.SYNCED)).orEmpty(),
            detailActions = detailActions,
            // FileVersionSummary.createdAt is already epoch millis (file-sync.md §
            // File Versions) — unlike media-item-date, no *1000 conversion.
            formatVersionTimestamp = { ValueFormat.relativeTime(context, it) },
            shareActions = shareActions,
        )
    }

    // `SearchNav.File`'s destination (search.md § Where logic lives → *Result
    // navigation (deep link)*) — opened independent of [MediaItemTile]'s own
    // per-tile dialogs, since the located item may not even be in the
    // currently-filtered set (`MediaVM.refresh` already pointed the filter at
    // its own set, but this dialog does not depend on that filter matching).
    deepLinkedItem?.let { item ->
        MediaItemDetail(
            item = item,
            formatSize = { ValueFormat.byteSize(context, it) },
            formatVersionTimestamp = { ValueFormat.relativeTime(context, it) },
            detailActions = detailActions,
            followedScope = snapshot?.followedScope?.value,
            onDismiss = { vm.clearDeepLinkedItem() },
            share = ShareDetailUi(snapshot?.shareCreate, snapshot?.shareExpiryOptions.orEmpty(), shareActions),
        )
    }
}

/** Page gesture callbacks. No-op defaults so the Compose test can seed any subset. */
data class MediaActions(
    val onToggleView: () -> Unit = {},
    val onSetSort: (String) -> Unit = {},
    val onSetFilter: (String?) -> Unit = {},
    /** A followed option picked in `media-folder-filter` — its opaque value,
     *  handed back verbatim to the shared `select_followed_scope` gesture
     *  (media.md § Followed public folders). */
    val onSelectFollowed: (String) -> Unit = {},
    val onPickFile: () -> Unit = {},
    val onUpload: () -> Unit = {},
)

/**
 * `media-item-detail` + `file-version-history` gestures (media.md § Element IDs,
 * § User actions), injected so [MediaContent] stays FFI-free for Robolectric —
 * mirrors [loadThumbnail]'s suspend-lambda idiom. Both mutators return `true` on
 * success (mirrors linux reading `machine.snapshot().error` after the call): a
 * delete success closes the detail surface (its subject is gone); a restore
 * success reloads the version rows. Failure keeps the surface open — the page
 * `error-message` banner carries it (media.md § Errors & edge cases).
 */
data class MediaDetailActions(
    val loadVersions: suspend (folder: String, path: String, includePruned: Boolean) -> List<FileVersionSummary> =
        { _, _, _ -> emptyList() },
    val onRestoreVersion: suspend (folder: String, path: String, version: FileVersionSummary) -> Boolean =
        { _, _, _ -> false },
    val onDeleteItem: suspend (folder: String, path: String) -> Boolean = { _, _ -> false },
    /** `file-version-undelete-button` — restore a soft-pruned version to the
     *  listable population (`file-versions.md` § Retention (3), apps row 323). */
    val onUndeleteVersion: suspend (path: String, versionNum: Long) -> Boolean = { _, _ -> false },
    /**
     * `media-item-detail-download-button` — fetch the item's plaintext and hand it
     * to the platform save path. `latest` is the newest version row (keys the
     * shared `download_file` walk); `followedScope` is the active followed
     * scope's value, which routes to the keyless `download_followed` instead.
     * Returns null on success, else the failure's message for `media.error_download`.
     */
    val onDownload: suspend (item: MediaItemSummary, latest: FileVersionSummary?, followedScope: String?) -> String? =
        { _, _, _ -> null },
)

/**
 * Share-link gestures (`share-links.md` § Flows; the `share-link-*` family) —
 * each a pure forward to the shared `MediaMachine`, which owns eligibility, the
 * step state, the reveal-after-registration rule, row states and the errors.
 * No-op defaults so the Compose test can seed any subset (mirrors [MediaActions]).
 */
data class MediaShareActions(
    val onOpenCreate: (folder: String, path: String) -> Unit = { _, _ -> },
    val onSetExpiry: (String) -> Unit = {},
    val onCreate: () -> Unit = {},
    val onCloseCreate: () -> Unit = {},
    val onOpenList: () -> Unit = {},
    val onCloseList: () -> Unit = {},
    val onArmRevoke: (tokenId: String) -> Unit = {},
    val onCancelRevoke: () -> Unit = {},
    val onConfirmRevoke: () -> Unit = {},
)

/** What `media-item-detail` needs to host the share-link create surface. */
data class ShareDetailUi(
    val create: ShareCreateSnapshot? = null,
    val expiryOptions: List<String> = emptyList(),
    val actions: MediaShareActions = MediaShareActions(),
)

@OptIn(ExperimentalLayoutApi::class)
@Composable
fun MediaContent(
    snapshot: MediaPageSnapshot?,
    actions: MediaActions,
    pickedName: String?,
    formatSize: (Long) -> String,
    formatDate: (Long) -> String,
    loadThumbnail: suspend (String) -> ByteArray?,
    syncStateLabel: String,
    detailActions: MediaDetailActions = MediaDetailActions(),
    formatVersionTimestamp: (Long) -> String = { it.toString() },
    shareActions: MediaShareActions = MediaShareActions(),
    modifier: Modifier = Modifier,
) {
    val items = snapshot?.items.orEmpty()
    val shareUi = ShareDetailUi(snapshot?.shareCreate, snapshot?.shareExpiryOptions.orEmpty(), shareActions)
    val grid = snapshot?.viewGrid ?: false
    // Second painting condition of the empty state: has the first
    // `fauna.media.list` RETURNED (README.md § List pages: loading is not
    // empty). A null snapshot has not, so it reads unloaded.
    val loaded = snapshot?.loaded ?: false
    // The active followed browse scope's value — asked of the machine, never
    // parsed: its items open read-only and download keylessly.
    val followedScope = snapshot?.followedScope?.value

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp)
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // ── Explorer chrome ──────────────────────────────────────────────────
        MediaToolbar(snapshot, actions, pickedName, shareActions)

        // ── Items / empty state ──────────────────────────────────────────────
        // A non-lazy layout (all rows realized) so the e2e count-all over
        // `media-item` sees every item (mirrors linux FlowBox / windows'
        // non-virtualizing panel); the test aggregates are small.
        if (items.isEmpty()) {
            // Three renderable states off ONE id (media.md § Default view;
            // README.md § List pages: loading is not empty): `loaded && empty`
            // is the genuine empty state, and while still unloaded we compose
            // NOTHING here — the absence of `media-empty-state` beside zero
            // `media-item` rows is what identifies loading. Minting a
            // `media-loading` id was considered and rejected.
            if (loaded) {
                Text(
                    stringResource(R.string.media_no_media_yet),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .testTag(Ids.MEDIA_EMPTY_STATE)
                        .padding(vertical = 24.dp),
                )
            }
        } else if (grid) {
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                items.forEach { item ->
                    MediaItemTile(
                        item, grid = true, formatSize, formatDate, loadThumbnail, syncStateLabel,
                        detailActions, formatVersionTimestamp, followedScope, shareUi,
                    )
                }
            }
        } else {
            items.forEach { item ->
                MediaItemTile(
                    item, grid = false, formatSize, formatDate, loadThumbnail, syncStateLabel,
                    detailActions, formatVersionTimestamp, followedScope, shareUi,
                )
            }
        }
    }

    // The page-level share-link list + its single revoke confirm.
    snapshot?.shareLinks?.let { links ->
        if (links.open) ShareLinkList(links, shareActions)
    }
}

// ── Explorer chrome ──────────────────────────────────────────────────────────

@Composable
private fun MediaToolbar(
    snapshot: MediaPageSnapshot?,
    actions: MediaActions,
    pickedName: String?,
    shareActions: MediaShareActions,
) {
    val grid = snapshot?.viewGrid ?: false
    val sort = snapshot?.sort ?: "name"
    val filter = snapshot?.filter
    val folders = snapshot?.folders.orEmpty()
    val followed = snapshot?.followed.orEmpty()
    val followedScope = snapshot?.followedScope

    // Row 1: view toggle + sort select.
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        // media-view-toggle — label is the CURRENT view (List/Grid); the e2e reads
        // its text and asserts it changes after a toggle.
        OutlinedButton(
            onClick = actions.onToggleView,
            modifier = Modifier.testTag(Ids.MEDIA_VIEW_TOGGLE),
        ) {
            Text(stringResource(if (grid) R.string.media_view_grid else R.string.media_view_list))
        }
        SortSelect(sort, actions.onSetSort, Modifier.weight(1f))
    }

    // Row 2: per-set filter (all-media default + one-set scope + followed scopes).
    FilterSelect(filter, folders, followed, actions.onSetFilter, actions.onSelectFollowed)

    // share-link-list-button — the caller's share links, page-level
    // (`share-links.md` § Flows → List).
    OutlinedButton(
        onClick = shareActions.onOpenList,
        modifier = Modifier.testTag(Ids.SHARE_LINK_LIST_BUTTON),
    ) { Text(stringResource(R.string.share_link_list_button)) }

    // Row 3: file picker + upload into the selected set. A followed scope is
    // read-only, structurally — the row leaves the tree rather than disabling
    // (media.md § Followed public folders; linux/apple/windows do the same).
    if (followedScope == null) Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.fillMaxWidth(),
    ) {
        IconButton(
            onClick = actions.onPickFile,
            modifier = Modifier.testTag(Ids.FILE_UPLOAD),
        ) {
            Icon(Icons.Default.AttachFile, contentDescription = stringResource(R.string.media_choose_file))
        }
        Text(
            pickedName ?: stringResource(R.string.media_choose_file),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        // Always enabled: pressed with nothing picked, the caller surfaces
        // `media.file_required` (media.md § User actions — the empty-path guard).
        Button(
            onClick = actions.onUpload,
            modifier = Modifier.testTag(Ids.UPLOAD_BUTTON),
        ) {
            Text(stringResource(R.string.media_upload))
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SortSelect(current: String, onSelect: (String) -> Unit, modifier: Modifier = Modifier) {
    var expanded by remember { mutableStateOf(false) }
    // Stable sort keys the machine understands (`media-sort-select` values); the
    // label is the shared `fauna_core::format::media_sort_label` decision
    // (priority #2), not a per-app key → resource map.
    val keys = listOf("name", "size", "date")
    val currentLabel = localized(com.fauna.ffi.mediaSortLabel(current)) ?: current
    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { expanded = !expanded },
        modifier = modifier,
    ) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.MEDIA_SORT_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            keys.forEach { key ->
                DropdownMenuItem(
                    text = { Text(localized(com.fauna.ffi.mediaSortLabel(key)) ?: key) },
                    onClick = { expanded = false; onSelect(key) },
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun FilterSelect(
    current: String?,
    folders: List<String>,
    followed: List<FollowedScopeOption>,
    onSelect: (String?) -> Unit,
    onSelectFollowed: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val allLabel = stringResource(R.string.media_filter_all)
    // null filter = the all-media default; else the selected set name — or a
    // followed scope's opaque value, which the value→label side map turns back
    // into its minted label (media.md § Followed public folders: a Compose
    // picker's model holds values, and a followed value is never shown).
    val currentLabel = current?.let { v -> followed.firstOrNull { it.value == v }?.label ?: v } ?: allLabel
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = !expanded }) {
        OutlinedTextField(
            value = currentLabel,
            onValueChange = {},
            readOnly = true,
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.MEDIA_FOLDER_FILTER),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(text = { Text(allLabel) }, onClick = { expanded = false; onSelect(null) })
            folders.forEach { set ->
                DropdownMenuItem(text = { Text(set) }, onClick = { expanded = false; onSelect(set) })
            }
            // Followed scopes append after the own-set options; the value goes
            // back to the shared gesture verbatim, never parsed here.
            followed.forEach { opt ->
                DropdownMenuItem(
                    text = { Text(opt.label) },
                    onClick = { expanded = false; onSelectFollowed(opt.value) },
                )
            }
        }
    }
}

// ── Items ──────────────────────────────────────────────────────────────────

@Composable
private fun MediaItemTile(
    item: MediaItemSummary,
    grid: Boolean,
    formatSize: (Long) -> String,
    formatDate: (Long) -> String,
    loadThumbnail: suspend (String) -> ByteArray?,
    syncStateLabel: String,
    detailActions: MediaDetailActions,
    formatVersionTimestamp: (Long) -> String,
    followedScope: String?,
    share: ShareDetailUi,
) {
    // media-item tap/open → media-item-detail (media.md § User actions). Local UI
    // state only (mirrors FoldersScreen's `showDeleteDialog`/`showShareSheet`).
    var showDetail by remember(item.folder, item.path) { mutableStateOf(false) }

    Card(
        onClick = { showDetail = true },
        modifier = (if (grid) Modifier.width(160.dp) else Modifier.fillMaxWidth())
            .testTag(Ids.MEDIA_ITEM),
    ) {
        if (grid) {
            Column(
                Modifier.padding(8.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                MediaThumbnail(item.thumbnailHash, loadThumbnail, size = 96.dp)
                MediaItemMeta(item, formatSize, formatDate, syncStateLabel)
            }
        } else {
            Row(
                Modifier.padding(12.dp).fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                MediaThumbnail(item.thumbnailHash, loadThumbnail, size = 40.dp)
                Column(
                    Modifier.weight(1f),
                    verticalArrangement = Arrangement.spacedBy(2.dp),
                ) {
                    MediaItemMeta(item, formatSize, formatDate, syncStateLabel)
                }
            }
        }
    }

    if (showDetail) {
        MediaItemDetail(
            item = item,
            formatSize = formatSize,
            formatVersionTimestamp = formatVersionTimestamp,
            detailActions = detailActions,
            followedScope = followedScope,
            onDismiss = { showDetail = false },
            share = share,
        )
    }
}

/**
 * `media-item-detail` (media.md § Element IDs) — the per-item detail surface
 * opened by `media-item` tap/open. Hosts `file-version-history` (version rows off
 * [MediaDetailActions.loadVersions], restore behind a lightweight confirm) and the
 * `media-delete-button` family (a single confirm — deleting records a tombstone,
 * which is reversible-in-spirit via history, unlike the backups immediate-delete
 * ceremony), plus `media-item-detail-download-button` once the version rows
 * carry a manifest. In a followed browse scope ([followedScope] set) the item is
 * head-only: download alone, painted at once — no version read, history, restore
 * or delete (media.md § Followed public folders). Mirrors linux
 * `views/media/detail.rs::open_item_detail`.
 */
@Composable
private fun MediaItemDetail(
    item: MediaItemSummary,
    formatSize: (Long) -> String,
    formatVersionTimestamp: (Long) -> String,
    detailActions: MediaDetailActions,
    followedScope: String?,
    onDismiss: () -> Unit,
    share: ShareDetailUi = ShareDetailUi(),
) {
    val scope = rememberCoroutineScope()
    // The create surface lives inside the detail, so it closes with it.
    val dismiss = {
        if (share.create != null) share.actions.onCloseCreate()
        onDismiss()
    }
    var versions by remember(item.folder, item.path) { mutableStateOf<List<FileVersionSummary>?>(null) }
    var showDeleteConfirm by remember { mutableStateOf(false) }
    var restoreTarget by remember { mutableStateOf<FileVersionSummary?>(null) }
    // The recovery browse (`file-versions.md` § Retention (3), apps row 323) — ON
    // re-lists with includePruned, so soft-pruned rows appear with their badge +
    // undelete button. Reset on every open (mirrors linux's DetailCtx).
    var showPruned by remember(item.folder, item.path) { mutableStateOf(false) }
    // A followed item is head-only (media.md § Followed public folders): no
    // version read, history, recovery browse, restore or delete — download alone.
    val followed = followedScope != null
    // The download's own status line: a failure lands here, not on the page
    // banner the modal hides (linux `download_to`'s reasoning).
    var downloadError by remember(item.folder, item.path) { mutableStateOf<String?>(null) }
    var downloading by remember(item.folder, item.path) { mutableStateOf(false) }

    suspend fun reloadVersions() {
        versions = detailActions.loadVersions(item.folder, item.path, showPruned)
    }
    LaunchedEffect(item.folder, item.path, showPruned, followed) { if (!followed) reloadVersions() }
    // Rows arrive oldest→newest: the last one is the current file, and its
    // manifest keys the shared `download_file` walk.
    val latest = versions?.lastOrNull()

    Dialog(onDismissRequest = dismiss) {
        Surface(
            modifier = Modifier.testTag(Ids.MEDIA_ITEM_DETAIL),
            shape = MaterialTheme.shapes.medium,
        ) {
            Column(
                Modifier.padding(16.dp).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(
                    item.name,
                    style = MaterialTheme.typography.titleMedium,
                    modifier = Modifier.testTag(Ids.MEDIA_ITEM_DETAIL_NAME),
                )

                if (!followed) {
                    Text(stringResource(R.string.media_versions_title), style = MaterialTheme.typography.labelLarge)

                    Row(
                        Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(stringResource(R.string.media_versions_show_pruned), style = MaterialTheme.typography.bodySmall)
                        Switch(
                            checked = showPruned,
                            onCheckedChange = { showPruned = it },
                            modifier = Modifier.testTag(Ids.FILE_VERSION_SHOW_PRUNED_TOGGLE),
                        )
                    }

                    val v = versions
                    if (v == null) {
                        Text(
                            stringResource(R.string.media_versions_loading),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    } else {
                        Column(
                            Modifier.testTag(Ids.FILE_VERSION_LIST),
                            verticalArrangement = Arrangement.spacedBy(4.dp),
                        ) {
                            v.forEach { version ->
                                FileVersionRow(
                                    version = version,
                                    formatSize = formatSize,
                                    formatTimestamp = formatVersionTimestamp,
                                    onRestore = { restoreTarget = version },
                                    onUndelete = {
                                        scope.launch {
                                            if (detailActions.onUndeleteVersion(item.path, version.versionNum)) {
                                                reloadVersions()
                                            }
                                        }
                                    },
                                )
                            }
                        }
                    }
                }

                // share-link-create-modal — the create surface, inside the
                // detail it was opened from (`share-links.md` § Flows → Create).
                share.create?.let { create ->
                    ShareCreateSection(create, share.expiryOptions, share.actions)
                }

                downloadError?.let { msg ->
                    Text(
                        stringResourceFmt(R.string.media_error_download, msg),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }

                Row(
                    Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    // share-link-button — present ONLY on an eligible file (a
                    // public-audience folder), absent otherwise, never inert; the
                    // verdict is the machine's (`share-links.md` § Which files
                    // can be linked).
                    if (!followed && item.shareLinkEligible && share.create == null) {
                        TextButton(
                            onClick = { share.actions.onOpenCreate(item.folder, item.path) },
                            modifier = Modifier.testTag(Ids.SHARE_LINK_BUTTON),
                        ) { Text(stringResource(R.string.share_link_button)) }
                    }
                    if (!followed) {
                        TextButton(
                            onClick = { showDeleteConfirm = true },
                            modifier = Modifier.testTag(Ids.MEDIA_DELETE_BUTTON),
                        ) { Text(stringResource(R.string.media_file_detail_delete_file)) }
                    }
                    // media-item-detail-download-button — painted once a manifest is
                    // known (the version rows), or at once in a followed scope;
                    // never inert (media.md § Element IDs).
                    if (followed || latest != null) {
                        TextButton(
                            onClick = {
                                downloading = true
                                downloadError = null
                                scope.launch {
                                    downloadError = detailActions.onDownload(item, latest, followedScope)
                                    downloading = false
                                }
                            },
                            enabled = !downloading,
                            modifier = Modifier.testTag(Ids.MEDIA_ITEM_DETAIL_DOWNLOAD_BUTTON),
                        ) { Text(stringResource(R.string.media_download)) }
                    }
                    TextButton(
                        onClick = dismiss,
                        modifier = Modifier.testTag(Ids.MEDIA_ITEM_DETAIL_CLOSE_BUTTON),
                    ) { Text(stringResource(R.string.media_detail_close)) }
                }
            }
        }
    }

    // media-delete-confirm-modal — a SINGLE confirm (media.md § Element IDs); cancel
    // is a pure no-op. Success closes the detail (its subject is gone).
    if (showDeleteConfirm) {
        AlertDialog(
            modifier = Modifier.testTag(Ids.MEDIA_DELETE_CONFIRM_MODAL),
            onDismissRequest = { showDeleteConfirm = false },
            title = { Text(stringResource(R.string.media_file_detail_delete_confirm_title)) },
            text = { Text(stringResourceFmt(R.string.media_file_detail_delete_confirm, item.name)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        showDeleteConfirm = false
                        scope.launch {
                            if (detailActions.onDeleteItem(item.folder, item.path)) onDismiss()
                        }
                    },
                    modifier = Modifier.testTag(Ids.MEDIA_DELETE_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.media_file_detail_delete_confirm_button)) }
            },
            dismissButton = {
                TextButton(
                    onClick = { showDeleteConfirm = false },
                    modifier = Modifier.testTag(Ids.MEDIA_DELETE_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.common_cancel)) }
            },
        )
    }

    // file-version-restore-confirm-modal — a LIGHTWEIGHT confirm (restore is
    // reversible: it appends a new version, the pre-restore head stays listed).
    restoreTarget?.let { target ->
        AlertDialog(
            modifier = Modifier.testTag(Ids.FILE_VERSION_RESTORE_CONFIRM_MODAL),
            onDismissRequest = { restoreTarget = null },
            title = { Text(stringResource(R.string.media_restore_confirm_title)) },
            text = { Text(stringResource(R.string.media_restore_confirm_body)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        restoreTarget = null
                        scope.launch {
                            if (detailActions.onRestoreVersion(item.folder, item.path, target)) {
                                reloadVersions()
                            }
                        }
                    },
                    modifier = Modifier.testTag(Ids.FILE_VERSION_RESTORE_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.media_restore_confirm)) }
            },
            dismissButton = {
                TextButton(
                    onClick = { restoreTarget = null },
                    modifier = Modifier.testTag(Ids.FILE_VERSION_RESTORE_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.media_restore_cancel)) }
            },
        )
    }
}

/**
 * `share-link-create-modal` — the expiry choice and Create while the link is
 * unregistered; the URL and its Copy ONLY once the machine reports the
 * registration succeeded (the reveal-after-registration rule). Cancel closes it.
 */
@Composable
private fun ShareCreateSection(
    create: ShareCreateSnapshot,
    expiryOptions: List<String>,
    actions: MediaShareActions,
) {
    val clipboard = LocalClipboardManager.current
    Column(
        Modifier.fillMaxWidth().testTag(Ids.SHARE_LINK_CREATE_MODAL),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        HorizontalDivider()
        Text(
            stringResourceFmt(R.string.share_link_create_title, create.name),
            style = MaterialTheme.typography.labelLarge,
        )
        Text(
            stringResource(R.string.share_link_create_body),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        val url = create.url
        // `fauna.share.create` is OnlineOnly: the shared verdict greys Create
        // while the nest is unreachable.
        val gate = faunaGate("fauna.share.create", enabled = !create.busy)
        if (url != null) {
            Text(
                url,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.SHARE_LINK_URL),
            )
            TextButton(
                onClick = { clipboard.setText(AnnotatedString(url)) },
                modifier = Modifier.testTag(Ids.SHARE_LINK_COPY_BUTTON),
            ) { Text(stringResource(R.string.share_link_copy)) }
        } else {
            ShareExpirySelect(create.expiry, expiryOptions, enabled = !create.busy, onSelect = actions.onSetExpiry)
        }
        Row(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.End,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            TextButton(
                onClick = actions.onCloseCreate,
                modifier = Modifier.testTag(Ids.SHARE_LINK_CANCEL_BUTTON),
            ) {
                Text(stringResource(if (url != null) R.string.share_link_close else R.string.share_link_cancel))
            }
            if (url == null) {
                Button(
                    onClick = actions.onCreate,
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.SHARE_LINK_CREATE_BUTTON),
                ) {
                    Text(stringResource(if (create.busy) R.string.share_link_creating else R.string.share_link_create))
                }
            }
        }
        if (url == null) DisabledControlReasonText(gate.reason)
    }
}

/** `share-link-expiry-select` — the shared option values, each labelled through
 *  the shared `share_link_expiry_label` (raw when unknown). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ShareExpirySelect(
    current: String,
    options: List<String>,
    enabled: Boolean,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    val labels = (options + current).distinct().associateWith { localized(shareLinkExpiryLabel(it)) ?: it }
    ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { if (enabled) expanded = it }) {
        OutlinedTextField(
            value = labels.getValue(current),
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            label = { Text(stringResource(R.string.share_link_expiry_label)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier.fillMaxWidth().menuAnchor().testTag(Ids.SHARE_LINK_EXPIRY_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            options.forEach { value ->
                DropdownMenuItem(
                    text = { Text(labels.getValue(value)) },
                    onClick = { expanded = false; onSelect(value) },
                )
            }
        }
    }
}

/**
 * `share-link-list` — the caller's links, newest first, with three states off
 * one loaded bit (rows / `share-link-empty-state` / neither = loading), and the
 * single `share-link-revoke-confirm-modal` (`share-links.md` § Flows → List /
 * Revoke). A row's `state` attribute is its stable value on `stateDescription`.
 */
@Composable
private fun ShareLinkList(links: ShareLinksSnapshot, actions: MediaShareActions) {
    val clipboard = LocalClipboardManager.current
    Dialog(onDismissRequest = actions.onCloseList) {
        Surface(
            modifier = Modifier.testTag(Ids.SHARE_LINK_LIST),
            shape = MaterialTheme.shapes.medium,
        ) {
            Column(
                Modifier.padding(16.dp).fillMaxWidth().verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Text(stringResource(R.string.share_link_list_title), style = MaterialTheme.typography.titleMedium)
                when {
                    !links.loaded -> Text(
                        stringResource(R.string.share_link_list_loading),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    links.rows.isEmpty() -> Text(
                        stringResource(R.string.share_link_empty),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.testTag(Ids.SHARE_LINK_EMPTY_STATE),
                    )
                    else -> links.rows.forEach { row ->
                        ShareLinkRow(row, actions, onCopy = { clipboard.setText(AnnotatedString(it)) })
                    }
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                    TextButton(
                        onClick = actions.onCloseList,
                        modifier = Modifier.testTag(Ids.SHARE_LINK_LIST_CLOSE_BUTTON),
                    ) { Text(stringResource(R.string.share_link_close)) }
                }
            }
        }
    }

    val armed = links.revokeConfirm?.let { id -> links.rows.firstOrNull { it.tokenId == id } }
    if (armed != null) {
        val gate = faunaGate("fauna.share.revoke")
        AlertDialog(
            modifier = Modifier.testTag(Ids.SHARE_LINK_REVOKE_CONFIRM_MODAL),
            onDismissRequest = actions.onCancelRevoke,
            title = { Text(stringResource(R.string.share_link_revoke_confirm_title)) },
            text = {
                Column {
                    Text(stringResourceFmt(R.string.share_link_revoke_confirm_body, armed.name))
                    DisabledControlReasonText(gate.reason)
                }
            },
            confirmButton = {
                TextButton(
                    onClick = actions.onConfirmRevoke,
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.SHARE_LINK_REVOKE_CONFIRM_BUTTON),
                ) { Text(stringResource(R.string.share_link_revoke_confirm)) }
            },
            dismissButton = {
                TextButton(
                    onClick = actions.onCancelRevoke,
                    modifier = Modifier.testTag(Ids.SHARE_LINK_REVOKE_CANCEL_BUTTON),
                ) { Text(stringResource(R.string.share_link_cancel)) }
            },
        )
    }
}

/** One `share-link-item` row: name / expiry / state, Copy only with a verified
 *  URL, Revoke only on an Active row. */
@Composable
private fun ShareLinkRow(row: ShareLinkSummary, actions: MediaShareActions, onCopy: (String) -> Unit) {
    Column(
        Modifier.fillMaxWidth().testTag(Ids.SHARE_LINK_ITEM),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(
            row.name,
            style = MaterialTheme.typography.titleSmall,
            modifier = Modifier.testTag(Ids.SHARE_LINK_ITEM_NAME),
        )
        Text(
            stringResourceFmt(R.string.share_link_expires, formatUnixLocalDate(row.expiresAt)),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.SHARE_LINK_ITEM_EXPIRES),
        )
        Text(
            localized(shareLinkStateLabel(row.state)) ?: row.state,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier
                .testTag(Ids.SHARE_LINK_ITEM_STATE)
                .semantics { stateDescription = row.state },
        )
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            row.url?.let { url ->
                TextButton(
                    onClick = { onCopy(url) },
                    modifier = Modifier.testTag(Ids.SHARE_LINK_ITEM_COPY_BUTTON),
                ) { Text(stringResource(R.string.share_link_copy)) }
            }
            if (row.state == "active") {
                TextButton(
                    onClick = { actions.onArmRevoke(row.tokenId) },
                    modifier = Modifier.testTag(Ids.SHARE_LINK_REVOKE_BUTTON),
                ) { Text(stringResource(R.string.share_link_revoke)) }
            }
        }
    }
}

/** One `file-version-item` row: timestamp / size / restore, inside `file-version-list`. */
@Composable
private fun FileVersionRow(
    version: FileVersionSummary,
    formatSize: (Long) -> String,
    formatTimestamp: (Long) -> String,
    onRestore: () -> Unit,
    onUndelete: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().testTag(Ids.FILE_VERSION_ITEM),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            formatTimestamp(version.createdAt),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.weight(1f).testTag(Ids.FILE_VERSION_TIMESTAMP),
        )
        Text(
            formatSize(version.sizeBytes),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.FILE_VERSION_SIZE),
        )
        Text(
            stringResourceFmt(R.string.media_version_author, version.authorDisplay),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.FILE_VERSION_AUTHOR),
        )
        // A soft-pruned row (only an includePruned listing carries one) says so
        // and offers its recovery verb — badge + undelete, present ONLY on
        // pruned rows.
        if (version.pruned) {
            Text(
                stringResource(R.string.media_version_pruned_badge),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.FILE_VERSION_PRUNED_BADGE),
            )
            TextButton(
                onClick = onUndelete,
                modifier = Modifier.testTag(Ids.FILE_VERSION_UNDELETE_BUTTON),
            ) { Text(stringResource(R.string.media_version_undelete)) }
        }
        TextButton(
            onClick = onRestore,
            modifier = Modifier.testTag(Ids.FILE_VERSION_RESTORE_BUTTON),
        ) { Text(stringResource(R.string.media_version_restore)) }
    }
}

@Composable
private fun MediaItemMeta(
    item: MediaItemSummary,
    formatSize: (Long) -> String,
    formatDate: (Long) -> String,
    syncStateLabel: String,
) {
    Text(
        item.name,
        style = MaterialTheme.typography.titleSmall,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.testTag(Ids.MEDIA_ITEM_NAME),
    )
    Text(
        formatSize(item.sizeBytes),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.MEDIA_ITEM_SIZE),
    )
    Text(
        formatDate(item.updatedAt),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.testTag(Ids.MEDIA_ITEM_DATE),
    )
    // media-source-status: the backing set's source-device online/offline dot —
    // distinct from a file's sync-state badge (media.md § Source status vs. sync state).
    val online = item.sourceOnline
    Text(
        stringResource(if (online) R.string.media_source_online else R.string.media_source_offline),
        style = MaterialTheme.typography.labelSmall,
        color = if (online) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
        modifier = Modifier.testTag(Ids.MEDIA_SOURCE_STATUS),
    )
    // sync-state-badge: this file's presence state (file-sync.md § Per-file
    // sync-status display), a child of the media-item component (re-homed here
    // 2026-07-13). Always Synced on this control-plane surface (see the
    // MediaScreen call site); the label is shared, never hand-written.
    Text(
        syncStateLabel,
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.testTag(Ids.SYNC_STATE_BADGE),
    )
}

/**
 * media-thumbnail — the item's thumbnail blob, fetched + decrypted by the shared
 * `MediaMachine::fetch_thumbnail` (injected here as [loadThumbnail]; direct-by-hash
 * GET + owner-`BackupKey` decrypt, all shared Rust) and painted over the placeholder
 * icon. Lazy per-item (on first composition of the hash); a `None` hash or any
 * fetch/decode error keeps the placeholder — one unreadable thumbnail must never
 * blank the item (media.md § Thumbnails / § Implementation status).
 */
@Composable
private fun MediaThumbnail(
    thumbnailHash: String?,
    loadThumbnail: suspend (String) -> ByteArray?,
    size: Dp,
) {
    val bitmap by produceState<ImageBitmap?>(null, thumbnailHash) {
        value = thumbnailHash?.let { h ->
            loadThumbnail(h)?.let { bytes ->
                BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
            }
        }
    }
    val bmp = bitmap
    // The element's `state` — `painted` once the Image holds decoded bytes,
    // `placeholder` while the icon stands in — on `stateDescription`, the one
    // string-attribute carrier the e2e bridge reads (`AutomationSemantics.attrValue`);
    // linux/apple/windows publish the same two tokens off their own image widgets
    // (media.md § Thumbnails).
    if (bmp != null) {
        Image(
            bitmap = bmp,
            contentDescription = null,
            contentScale = ContentScale.Crop,
            modifier = Modifier.size(size).testTag(Ids.MEDIA_THUMBNAIL)
                .semantics { stateDescription = THUMBNAIL_PAINTED },
        )
    } else {
        Icon(
            Icons.Default.Image,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(size).testTag(Ids.MEDIA_THUMBNAIL)
                .semantics { stateDescription = THUMBNAIL_PLACEHOLDER },
        )
    }
}

/** `media-thumbnail`'s `state` tokens — the cross-app vocabulary `actions/media.py::thumbnail_kind` reads. */
internal const val THUMBNAIL_PAINTED = "painted"
internal const val THUMBNAIL_PLACEHOLDER = "placeholder"
