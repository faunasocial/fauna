package com.fauna.app.ui.screen.feed

import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.ExifStripper
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.FeedVM
import kotlinx.coroutines.launch
import uniffi.fauna_feed.AttachedFile
import uniffi.fauna_feed.GateRoomOption
import uniffi.fauna_feed.GateTierOption
import uniffi.fauna_feed.SellComposeState
import social.fauna.generated.Ids

/**
 * The feed composer (`feed-compose-bar`), wired to the shared `FeedManager`:
 * text + tags are local Compose state synced into `update_compose` on file-pick
 * and submit; the staged file (`compose-file-ready`) and the composer error
 * (`compose-error`) render from the **snapshot's** `compose` fields. The file
 * picker + blob upload are client glue; submit + validation + post build are
 * shared Rust (`FeedManager::submit_post`). Single attachment — the shared
 * `ComposeState.attached_file` is one `AttachedFile` (matching the other apps).
 *
 * **Draft-persistence v2, posts rail** (`file-sync.md` § Drafts Sync,
 * `docs/goal/ui/feed.md` § Persistence): `body`/`tags` mirror the manager's
 * compose state on every tick UNTIL the user's own first edit (see
 * `userEditedText` below), then push back on every change — the android leg
 * of the same fix linux's posts-rail drafts landing made (linux's compose
 * widgets were never synced to `FeedManager::update_compose` at all before
 * that fix). ⚠ `attached_file` is carried through
 * on every push so a staged upload survives a text edit.
 *
 * **The audience lives in the manager, never here** (`docs/goal/ui/feed.md`
 * § Persistence → *Only user-authored input rests*): each answer of
 * `compose-gate-tier-select` (tier, room, sale and its fields) and the teaser
 * forward through `FeedVM.setCompose*` as they are picked, and the select paints
 * from `snapshot.compose` — so a restored gated, room or sale draft shows its
 * audience, and the Post click submits the manager's audience rather than a
 * page-local Public one (linux's `AudienceControls`, apple's `FeedVM`, tui's
 * `Action::SetGateTier`). Only the audience's free-text fields keep a local
 * copy, mirrored from the manager until the user's own first edit to one of
 * them, for the same restore-race reason as `body`/`tags` below.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FeedComposeScreen(navController: NavController, vm: FeedVM = hiltViewModel()) {
    val context = LocalContext.current
    val snapshot by vm.snapshot.collectAsState()
    var body by remember { mutableStateOf("") }
    var tags by remember { mutableStateOf("") }
    var gatePreview by remember { mutableStateOf("") }
    var sellPrice by remember { mutableStateOf("") }
    // The machine-comparable price (monetization.md § The asking price) —
    // independent of sellPrice above (the free-text hint); no parsing ever
    // infers one from the other. Empty means no machine price: the minted
    // tier stays a tip target forever.
    var sellAskingPrice by remember { mutableStateOf("") }
    var isPosting by remember { mutableStateOf(false) }
    // Flips true on the user's first edit to body/tags (see the two
    // OutlinedTextFields below) — gates both directions of the draft mirror:
    // false, the manager's compose state (possibly still arriving from an
    // async launch-restore) keeps overwriting body/tags; true, the user's own
    // typing is never again clobbered by a later manager tick (a file
    // finishing upload, say), and their edits start pushing outward instead.
    var userEditedText by remember { mutableStateOf(false) }
    // The same guard for the audience's free-text fields (teaser, sale price,
    // asking price) — separate from [userEditedText], so typing the body before
    // the restore lands never stops the restored teaser painting.
    var userEditedAudienceText by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    val compose = snapshot?.compose
    val attachedFile = compose?.attachedFile
    val composeErrorText = localized(compose?.error)
    val ownTiers = snapshot?.ownTiers ?: emptyList()
    val ownRooms = snapshot?.ownRooms ?: emptyList()
    // The audience answer, straight off the manager. The room is its hex
    // channel id (never its label — `update_compose_room` takes the id, and
    // the label is a projection of `own_rooms`). The three answers are
    // mutually exclusive shared-Rust-side: each setter clears the other two.
    val gateTier = compose?.gateTier
    val gateRoom = compose?.gateRoom
    val sell = compose?.sell
    val sellMode = sell != null
    val sellSubscribersFree = sell?.subscribersGetItFree ?: true

    // Refresh the gate-to-tier options on entry: the shared feed manager persists
    // across nav (FeedManagerHost is a singleton), so a tier just minted on the
    // Profile page wouldn't appear in the select without an explicit refresh — the
    // web SPA does the same over its wasm twin (linux reloads on nav instead).
    LaunchedEffect(Unit) { vm.refreshOwnTiers() }
    // Room options are ALSO re-read off the conversations plane's own change
    // tick (`FeedManagerHost`'s installed seam), so this is only the composer's
    // own entry refresh — the same belt-and-suspenders shape `refreshOwnTiers`
    // above already has relative to `refresh_feeds`.
    LaunchedEffect(Unit) { vm.refreshOwnRooms() }

    // Mirror the manager's compose text/tags until the user's first edit. Keyed
    // on [compose] (re-runs on every manager tick) rather than a one-shot
    // effect: the launch-restore (FeedManagerHost.restoreDrafts) is async over
    // the network, so the FIRST tick this screen ever sees is the manager's
    // pre-restore EMPTY state — re-running on every tick (guarded by
    // userEditedText) means whichever tick lands last before the user types
    // wins, which is the restored draft in the ordinary case (nothing types
    // during the sub-second restore round-trip) and the user's own text in the
    // race case (they typed before it landed) — never a stale overwrite of
    // live typing, since the guard flips permanently on first edit.
    LaunchedEffect(compose) {
        if (!userEditedText) {
            compose?.let {
                body = it.text
                tags = it.tags
            }
        }
        if (!userEditedAudienceText) {
            compose?.let {
                gatePreview = it.gatePreview
                sellPrice = it.sell?.price ?: ""
                sellAskingPrice = it.sell?.askingPrice ?: ""
            }
        }
    }

    // Push text/tags back to the manager as the user edits — a cheap local FFI
    // mutation (no I/O; mirrors the file-pick call site below), so every
    // keystroke is fine to forward directly. FeedManagerHost's own debounce
    // owns the expensive part (the network save). Gated on userEditedText so
    // the pre-seed/still-restoring empty values can never push out and
    // overwrite a real persisted draft before it has even been read back.
    LaunchedEffect(body, tags) {
        if (userEditedText) vm.stageAttachment(body, tags, attachedFile)
    }

    val filePickerLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent()
    ) { uri ->
        uri ?: return@rememberLauncherForActivityResult
        val cursor = context.contentResolver.query(uri, null, null, null, null)
        var fileName = "attachment"
        var fileSize = 0L
        cursor?.use {
            if (it.moveToFirst()) {
                val nameIdx = it.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                val sizeIdx = it.getColumnIndex(OpenableColumns.SIZE)
                if (nameIdx >= 0) fileName = it.getString(nameIdx) ?: "attachment"
                if (sizeIdx >= 0) fileSize = it.getLong(sizeIdx)
            }
        }
        val mimeType = context.contentResolver.getType(uri) ?: "application/octet-stream"
        // Read + EXIF-strip the bytes and hold them via [FeedVM.attachComposeFile];
        // do NOT upload here. The seal is decided by the composer's audience, and
        // the audience is not known when the picker returns — so uploading now
        // could only ever publish a plaintext copy of a picture the author is
        // about to restrict, under a hash anyone can fetch and no blob DELETE can
        // remove (`ui/media.md` § Encryption at rest: "The seal is resolved
        // BEFORE the attachment is uploaded, never after"). `FeedVM.submitPost`
        // seals and uploads once the audience is final. The chip below renders
        // off this hash-less staging, so it now appears immediately rather than
        // after a round trip.
        scope.launch {
            val rawBytes = context.contentResolver.openInputStream(uri)?.readBytes()
                ?: return@launch
            vm.attachComposeFile(
                body, tags,
                AttachedFile(name = fileName, size = fileSize.toULong(), blobHash = null, mediaType = mimeType),
                ExifStripper.strip(rawBytes, mimeType),
            )
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.composer_new_post)) },
                navigationIcon = {
                    IconButton(onClick = { navController.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                },
                actions = {
                    IconButton(
                        onClick = { filePickerLauncher.launch("*/*") },
                        modifier = Modifier.testTag(Ids.COMPOSE_FILE)
                    ) {
                        Icon(Icons.Default.Add, contentDescription = stringResource(R.string.media_add_file))
                    }
                    IconButton(
                        onClick = {
                            scope.launch {
                                isPosting = true
                                // The audience is the manager's — never re-read
                                // from this screen (see the KDoc above).
                                val ok = vm.submitPost(body, tags)
                                if (ok) navController.popBackStack() else isPosting = false
                            }
                        },
                        enabled = body.isNotBlank() && !isPosting,
                        modifier = Modifier.testTag(Ids.POST_SUBMIT_BUTTON)
                    ) {
                        Icon(Icons.AutoMirrored.Filled.Send, stringResource(R.string.composer_new_post))
                    }
                }
            )
        }
    ) { padding ->
        Column(modifier = Modifier.padding(padding).fillMaxSize().padding(16.dp)) {
            // Staged attachment (compose-file-ready) — from the shared snapshot.
            attachedFile?.let { af ->
                InputChip(
                    selected = false,
                    onClick = {},
                    label = { Text("${af.name} (${ValueFormat.byteSize(context, af.size.toLong())})") },
                    trailingIcon = {
                        IconButton(
                            // Drop the held bytes with the staged metadata — they
                            // are the only copy, and nothing else clears them.
                            onClick = { vm.clearComposeAttachment(body, tags) },
                            modifier = Modifier.size(18.dp).testTag(Ids.COMPOSE_FILE_REMOVE)
                        ) {
                            Icon(Icons.Default.Close, stringResource(R.string.common_remove), modifier = Modifier.size(14.dp))
                        }
                    },
                    modifier = Modifier.testTag(Ids.COMPOSE_FILE_READY)
                )
                Spacer(Modifier.height(8.dp))
            }
            OutlinedTextField(
                value = tags,
                onValueChange = { tags = it; userEditedText = true },
                modifier = Modifier.fillMaxWidth().testTag(Ids.COMPOSE_TAGS_FIELD),
                placeholder = { Text(stringResource(R.string.feed_post_tags_placeholder)) },
                singleLine = true,
                enabled = !isPosting
            )
            Spacer(Modifier.height(8.dp))
            // Gate-to-tier controls (compose-gate-tier-select / -preview-field):
            // "Public" gates nothing; selecting a tier seals the full body and
            // shows non-subscribers only the teaser (feed.md § Encryption at
            // rest). "Sell this post…" is the select's third answer
            // (monetization.md § Per-post pay-to-unlock) — mutually exclusive
            // with a gate tier, so picking one clears the other.
            GateTierSelect(
                ownTiers = ownTiers,
                gateTier = gateTier,
                onSelect = { vm.setComposeGate(it) },
                enabled = !isPosting,
                ownRooms = ownRooms,
                gateRoom = gateRoom,
                onSelectRoom = { vm.setComposeRoom(it) },
                sellSelected = sellMode,
                onSelectSell = {
                    vm.setComposeSell(
                        sell ?: SellComposeState(
                            price = sellPrice,
                            askingPrice = sellAskingPrice,
                            subscribersGetItFree = true,
                        )
                    )
                },
            )
            if (sellMode) {
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = sellPrice,
                    onValueChange = {
                        sellPrice = it
                        userEditedAudienceText = true
                        vm.setComposeSell(SellComposeState(it, sellAskingPrice, sellSubscribersFree))
                    },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.COMPOSE_SELL_PRICE),
                    placeholder = { Text(stringResource(R.string.feed_post_sell_price_placeholder)) },
                    singleLine = true,
                    enabled = !isPosting,
                )
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = sellAskingPrice,
                    onValueChange = {
                        sellAskingPrice = it
                        userEditedAudienceText = true
                        vm.setComposeSell(SellComposeState(sellPrice, it, sellSubscribersFree))
                    },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.COMPOSE_SELL_ASKING_PRICE),
                    placeholder = { Text(stringResource(R.string.feed_post_sell_asking_price_placeholder)) },
                    singleLine = true,
                    enabled = !isPosting,
                )
                Spacer(Modifier.height(8.dp))
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    Text(stringResource(R.string.feed_post_sell_subscribers_free), modifier = Modifier.weight(1f))
                    Switch(
                        checked = sellSubscribersFree,
                        onCheckedChange = {
                            vm.setComposeSell(SellComposeState(sellPrice, sellAskingPrice, it))
                        },
                        enabled = !isPosting,
                        modifier = Modifier.testTag(Ids.COMPOSE_SELL_SUBSCRIBERS_FREE),
                    )
                }
            }
            if (sellMode || gateTier != null || gateRoom != null) {
                Spacer(Modifier.height(8.dp))
                OutlinedTextField(
                    value = gatePreview,
                    onValueChange = {
                        gatePreview = it
                        userEditedAudienceText = true
                        vm.setComposePreview(it)
                    },
                    modifier = Modifier.fillMaxWidth().testTag(Ids.COMPOSE_GATE_PREVIEW_FIELD),
                    placeholder = { Text(stringResource(R.string.feed_post_gate_preview_placeholder)) },
                    minLines = 2,
                    enabled = !isPosting,
                )
            }
            Spacer(Modifier.height(8.dp))
            composeErrorText?.let { msg ->
                Text(
                    msg,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.COMPOSE_ERROR)
                )
                Spacer(Modifier.height(4.dp))
            }
            OutlinedTextField(
                value = body,
                onValueChange = { body = it; userEditedText = true },
                modifier = Modifier.fillMaxSize().testTag(Ids.COMPOSE_TEXT_FIELD),
                placeholder = { Text(stringResource(R.string.feed_post_whats_on_your_mind)) },
                enabled = !isPosting
            )
        }
    }
}

/**
 * The composer's gate-to-tier select (`compose-gate-tier-select`) — "Public"
 * (index 0, `gate_tier = null`), each of the author's own tiers
 * (`snapshot.own_tiers`), then one option per room the author sits on the
 * floor of (`snapshot.own_rooms`, "Room: ‹label›" — `ui/feed.md` § Encryption
 * at rest → *Room-restricted — the app half*), then **"Sell this post…"** as
 * the always-last answer (`monetization.md` § Per-post pay-to-unlock) —
 * mutually exclusive with a gate tier and a room, never a separate toggle.
 * Selecting a tier, room or sell reveals the shared preview field; the shared
 * manager seals the full body and serves non-subscribers the teaser
 * (feed.md § Encryption at rest). Mirrors the profile Tiers tab's
 * `TierSelect` (priority #3) — a native `ExposedDropdownMenuBox` — and the
 * fixed Public → tiers → rooms → Sell layout linux's `GateOptions` and
 * windows' rebuilt select both converge on.
 *
 * The room answer is carried as its hex channel id ([gateRoom]), never its
 * label — `update_compose_room` takes the id, and a tier could otherwise be
 * named exactly a room option's "Room: ‹label›" text, which is why the
 * anchor's displayed value is resolved by which of [gateRoom]/[gateTier]/
 * [sellSelected] is actually set, never by re-parsing display text: the shape
 * the e2e `select` action still drives by the option's visible text
 * (`driver.select` matches displayed text, so each `DropdownMenuItem`'s text
 * must equal the string passed to `select`/`create_gated_post`), but the
 * click handler below identifies the intended answer directly at the point
 * of click — never by comparing text back out of the selection afterward,
 * the same avoidance windows' rebuild made explicit for its by-index resolve.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun GateTierSelect(
    ownTiers: List<GateTierOption>,
    gateTier: String?,
    onSelect: (String?) -> Unit,
    enabled: Boolean,
    ownRooms: List<GateRoomOption> = emptyList(),
    gateRoom: String? = null,
    onSelectRoom: (String) -> Unit = {},
    sellSelected: Boolean = false,
    onSelectSell: () -> Unit = {},
) {
    var expanded by remember { mutableStateOf(false) }
    val publicLabel = stringResource(R.string.feed_post_gate_public)
    val sellLabel = stringResource(R.string.feed_post_gate_sell)
    val roomLabelTemplate = stringResource(R.string.feed_post_gate_room)
    val selectedRoom = ownRooms.find { it.room == gateRoom }
    val displayValue = when {
        sellSelected -> sellLabel
        selectedRoom != null -> roomLabelTemplate.replace("{room}", selectedRoom.label)
        gateTier != null -> gateTier
        else -> publicLabel
    }

    ExposedDropdownMenuBox(
        expanded = expanded,
        onExpandedChange = { if (enabled) expanded = !expanded },
    ) {
        OutlinedTextField(
            value = displayValue,
            onValueChange = {},
            readOnly = true,
            enabled = enabled,
            label = { Text(stringResource(R.string.feed_post_gate_audience)) },
            trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
            modifier = Modifier
                .menuAnchor()
                .fillMaxWidth()
                .testTag(Ids.COMPOSE_GATE_TIER_SELECT),
        )
        ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            DropdownMenuItem(
                text = { Text(publicLabel) },
                onClick = { onSelect(null); expanded = false },
            )
            ownTiers.forEach { tier ->
                DropdownMenuItem(
                    text = { Text(tier.name) },
                    onClick = { onSelect(tier.name); expanded = false },
                )
            }
            ownRooms.forEach { room ->
                DropdownMenuItem(
                    text = { Text(roomLabelTemplate.replace("{room}", room.label)) },
                    onClick = { onSelectRoom(room.room); expanded = false },
                )
            }
            DropdownMenuItem(
                text = { Text(sellLabel) },
                onClick = { onSelectSell(); expanded = false },
            )
        }
    }
}
