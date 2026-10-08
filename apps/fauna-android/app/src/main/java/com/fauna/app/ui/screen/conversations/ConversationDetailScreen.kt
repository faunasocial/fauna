package com.fauna.app.ui.screen.conversations

import android.graphics.BitmapFactory
import android.provider.OpenableColumns
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Reply
import androidx.compose.material.icons.automirrored.filled.ReplyAll
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MoreHoriz
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material.icons.filled.Shield
import androidx.compose.material.icons.filled.Verified
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.emoji2.emojipicker.EmojiPickerView
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.BuildConfig
import com.fauna.app.R
import com.fauna.app.core.ContentPolicyInputs
import com.fauna.app.core.ExifStripper
import com.fauna.app.ui.components.C2paBadge
import com.fauna.app.ui.components.ComposeMarkerPlan
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.ContentLabelBadge
import com.fauna.app.ui.components.DocumentBlocks
import com.fauna.app.ui.components.MarkdownDecoration
import com.fauna.app.ui.components.MarkdownWrap
import com.fauna.app.ui.components.MarkdownWrapResult
import com.fauna.app.ui.components.RevealSpan
import com.fauna.app.ui.components.documentAttachments
import com.fauna.app.ui.components.documentHasBlockedRemoteImage
import com.fauna.app.ui.components.documentQuotedMessage
import com.fauna.app.ui.components.documentResolvedLinkPreviews
import com.fauna.app.ui.components.documentResolvingLinkPreviewUrls
import com.fauna.app.ui.components.naiveMarkdownWrap
import com.fauna.app.ui.components.sourceGlyphEmoji
import com.fauna.app.ui.util.FaunaGateVerdict
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.ConversationsVM
import com.fauna.ffi.composeDecorationPlan
import com.fauna.ffi.composeShowMarkersDimRanges
import com.fauna.ffi.decorationMap
import com.fauna.ffi.legalTakedownTombstone
import com.fauna.ffi.matchesMutedKeywords
import com.fauna.ffi.urlHost
import com.fauna.ffi.wrapMarkdownSelection
import kotlinx.coroutines.launch
import uniffi.fauna_conversations.AddParticipantState
import uniffi.fauna_conversations.MessageSnapshot
// The room model's render mappings + the editor's staged state — the SAME
// `fauna_conversations::room` / `::room_settings` helpers tui and linux call
// directly and web reaches through its wasm twins, so nothing below hand-types
// a token, a label, a choice list or a staging rule (priority #2).
import uniffi.fauna_conversations.ReplyPreview
import uniffi.fauna_conversations.RoomSettingsDraft
import uniffi.fauna_conversations.RoomSettingsEdit
import uniffi.fauna_conversations.roomClassAttrToken
import uniffi.fauna_conversations.roomClassLabel
import uniffi.fauna_conversations.roomHistoryPolicyEditorChoices
import uniffi.fauna_conversations.roomHistoryPolicyLabel
import uniffi.fauna_conversations.roomHistoryPolicyToken
import uniffi.fauna_conversations.roomJoinRuleEditorChoices
import uniffi.fauna_conversations.roomJoinRuleLabel
import uniffi.fauna_conversations.roomJoinRuleToken
import uniffi.fauna_conversations.roomMemberChipText
import uniffi.fauna_conversations.roomRoleAttrToken
import uniffi.fauna_conversations.roomSettingsAdminAt
import uniffi.fauna_conversations.roomSettingsEdits
import uniffi.fauna_conversations.roomSettingsIsEligible
import uniffi.fauna_conversations.roomSettingsSeed
import uniffi.fauna_conversations.roomSettingsSetHistoryPolicy
import uniffi.fauna_conversations.roomSettingsSetJoinRule
import uniffi.fauna_conversations.roomSettingsToggleAdmin
import uniffi.fauna_conversations.roomSettingsToggleTransfer
import uniffi.fauna_conversations.roomSettingsTransferStagedAt
import uniffi.fauna_conversations.SendState
import uniffi.fauna_conversations.ThreadCapabilities
import uniffi.fauna_conversations.ThreadDetail
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_conversations.quicksetEmojis
import uniffi.fauna_conversations.tryParseTypedAddress
import uniffi.fauna_conversations.typedAddressDisplayWithBridges
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.RenderDocument
import social.fauna.generated.Ids

/**
 * The shared `decoration_map` FFI mapped into the FFI-free [MarkdownDecoration] the compose
 * Content consumes for inline markdown styling. Defined in the stateful layer (FFI is allowed
 * here); injecting it keeps [ConversationDetailContent] / [NewThreadComposeContent] (and their
 * Robolectric tests) free of the native `.so`. conversations.md § Compose-field inline markdown
 * styling. Reused by [NewThreadComposeScreen] (same package).
 */
internal fun ffiMarkdownDecorate(src: String): List<MarkdownDecoration> =
    decorationMap(src).map { MarkdownDecoration(it.start.toInt(), it.end.toInt(), it.kind, it.level.toInt()) }

/**
 * The shared `wrap_selection` FFI mapped into the FFI-free [MarkdownWrapResult] the compose
 * toolbar consumes (edge-whitespace-safe marker wrapping — conversations.md § Where logic lives).
 */
internal val ffiMarkdownWrap: MarkdownWrap = { sel, prefix, suffix, placeholder ->
    wrapMarkdownSelection(sel, prefix, suffix, placeholder).let {
        MarkdownWrapResult(it.replacement, it.beforeCore, it.core)
    }
}

/**
 * The shared `compose_decoration_plan` FFI (the hide-by-default rule) mapped into the FFI-free
 * [ComposeMarkerPlan] the compose Content consumes for hide-mode styling. `caretByte` is a
 * UTF-8 byte offset (docs/goal/ui/conversations.md § Compose-field inline markdown styling).
 */
internal fun ffiComposeDecorationPlan(src: String, caretByte: Int): ComposeMarkerPlan {
    val plan = composeDecorationPlan(src, caretByte.toULong())
    return ComposeMarkerPlan(
        hide = plan.hide.map { RevealSpan(it.start.toInt(), it.end.toInt()) },
        dim = plan.dim.map { RevealSpan(it.start.toInt(), it.end.toInt()) },
    )
}

/**
 * The shared `compose_show_markers_dim_ranges` FFI (the show-markers dim-mode rule) mapped into
 * the FFI-free [RevealSpan] list the compose Content consumes. Replaces the former client-local
 * caret-line re-derivation (priority #2/#4 — android/linux/windows each hand-rolled this).
 */
internal fun ffiComposeShowMarkersDimRanges(src: String, caretByte: Int): List<RevealSpan> =
    composeShowMarkersDimRanges(src, caretByte.toULong()).map { RevealSpan(it.start.toInt(), it.end.toInt()) }

/**
 * The curated quick-set reaction emoji, in the fixed shared order every app
 * uses (conversations.md § Reactions & message delete). Rendered as the
 * `dm-reaction-option[i]` row inside the `dm-message-actions-menu`.
 *
 * ONE definition, in shared Rust — `fauna_conversations::QUICKSET_EMOJIS`, read here
 * through its [quicksetEmojis] UniFFI face (linux and tui take the const as a crate
 * dep; web reads the wasm twin). This used to be a re-typed Kotlin copy, and the
 * ORDER is a cross-app contract — `dm-reaction-option` is indexed, so an e2e tapping
 * index 0 asserts 👍 on all 7 apps — which four hand-kept copies had nothing to catch
 * drifting (priority #2/#4).
 *
 * `by lazy` so the FFI hop happens on first render rather than at class load.
 */
private val QUICK_SET_EMOJI: List<String> by lazy { quicksetEmojis() }

/**
 * Conversation detail pane (mobile-collapsed detail screen) — observer-driven
 * render of one thread off the shared
 * [uniffi.fauna_conversations.ConversationsManager], mirroring the Linux/Windows
 * precedent (apps/fauna-linux/src/views/conversations/{detail,thread_header,
 * message_bubble}.rs).
 *
 * Per docs/goal/ui/conversations.md §"Architectural rules" #1 (observer-driven),
 * #4 (indexed list IDs) and #5 (capability-gating, never rail branches —
 * affordances gate on `capabilities.*`). The VM-bound [ConversationDetailScreen]
 * is the NavHost wrapper (holds the manager + navigation); the stateless
 * [ConversationDetailContent] is split out for the Compose test harness (no VM,
 * no manager, no FFI native calls).
 *
 * Phase 3 adds the inline compose bar (`dm-text-field`, `dm-send-button`,
 * `topic-toggle-button`, `subject-input`, `attachment-button`, reply preview)
 * rendering off `detail.compose`, and wires the page `error-message` to the
 * compose's `send_state == Failed { reason }`.
 *
 * Phase 4 wires the two header overlays: the rename dialog (`thread-rename-field`
 * / `thread-rename-confirm`, MLS groups only) and the snapshot-driven
 * add-participant overlay (`recipient-picker` + `add-participant-confirm`,
 * forking a new MlsGroup on a FaunaMls 1:1 — conversations.md:31-33). Mirrors
 * apps/fauna-linux/src/views/conversations/{rename_overlay,add_participant_overlay}.rs.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationDetailScreen(
    navController: NavController,
    threadId: String,
    vm: ConversationsVM = hiltViewModel(),
) {
    val snapshot by vm.managerSnapshot.collectAsState()
    val manager = vm.conversationsManager
    val scope = rememberCoroutineScope()
    val context = LocalContext.current

    // `attachment-button` (conversations.md § Attachments): pick a file, stage its
    // bytes on the thread's compose draft via the shared `add_attachment` (send
    // re-resolves it onto the wire). Same contentResolver read + `ExifStripper`
    // idiom as the feed composer (FeedComposeScreen.kt) and the windows reference
    // leg (`DmComposeBar.xaml.cs` `ExifStripper.StripAsync`); no preview/remove UI
    // yet (matches the web leg landed the same day — `remove_attachment` stays
    // unused everywhere).
    val attachmentPickerLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent()
    ) { uri ->
        uri ?: return@rememberLauncherForActivityResult
        val cursor = context.contentResolver.query(uri, null, null, null, null)
        var fileName = "attachment"
        cursor?.use {
            if (it.moveToFirst()) {
                val nameIdx = it.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (nameIdx >= 0) fileName = it.getString(nameIdx) ?: "attachment"
            }
        }
        val mimeType = context.contentResolver.getType(uri) ?: "application/octet-stream"
        scope.launch {
            val rawBytes = context.contentResolver.openInputStream(uri)?.readBytes()
                ?: return@launch
            val bytes = ExifStripper.strip(rawBytes, mimeType)
            manager.addAttachment(threadId, fileName, mimeType, bytes)
        }
    }

    // Muted-keyword collapse (moderation.md § Muted keywords): the client-held muted list
    // (matched at render) + a session-local "revealed" set of message ids (one-tap "Show
    // anyway"; the mute itself is untouched — the linux REVEALED_MUTED twin, held as Compose
    // state, not a global/manager round-trip). Load the list on mount so Settings edits
    // reflect on the next open.
    val mutedKeywords by vm.mutedKeywords.collectAsState()
    val revealedMuted = remember { mutableStateListOf<String>() }
    LaunchedEffect(Unit) { vm.loadMutedKeywords() }
    // Content-policy render inputs (guardian floor + own spam thresholds) — the
    // per-bubble block/collapse verdict resolves off these (family-safety.md
    // § Content policy). Refreshed by `loadMutedKeywords()` above on detail open.
    val contentPolicyInputs by vm.contentPolicyInputs.collectAsState()

    // The "more reactions" picker is the ONE sanctioned per-app divergence
    // (conversations.md § Reactions & message delete): the native emoji picker.
    // The bubble's `dm-reaction-more-button` lifts the target message id up here;
    // this stateful screen hosts the emoji2 `EmojiPickerView` (an AndroidView)
    // and routes the picked emoji to the shared manager — keeping the FFI-free
    // Compose Content (and its Robolectric harness) free of the picker View.
    var moreReactionFor by remember { mutableStateOf<String?>(null) }
    // The one toggle path — the quick-set, the pills and the "more" picker all
    // call it. toggle_reaction resolves Add/Remove against my current state.
    val toggleReaction: (String, String) -> Unit = { msgId, emoji ->
        scope.launch { runCatching { manager.toggleReaction(threadId, msgId, emoji) } }
    }

    // Re-resolve the thread detail whenever the snapshot ticks (the manager is
    // the single source of truth; threadDetail() is the FFI read for one thread).
    val detail: ThreadDetail? = remember(snapshot, threadId) {
        runCatching { manager.threadDetail(threadId) }.getOrNull()
    }

    // Guardian Notify (family-safety.md § Guardian Notify): count every
    // currently-rendered message's guardian-floor enforcement. A no-op per
    // message unless the ward's content_notify knob is on and the guardian
    // floor bites; deduped per message per local day inside the store, so a
    // re-render never re-counts. Mirrors web's conversations/+page.svelte effect.
    LaunchedEffect(detail?.messages) {
        detail?.messages?.forEach { msg -> vm.noteContentEnforcement(msg.messageId, msg.labels) }
    }

    // Post-succession member review (succession-aftermath.md § Propagation →
    // *MLS groups*, item 3a): load the roster once on mount (mirrors
    // `loadMutedKeywords` above), then answer per-chip marks from the cache —
    // index-parallel with `detail.participantDisplays`, so `memberMarks[i]`
    // answers for `thread-member-chip[i]` and nothing here has to predict
    // which chip a flagged person landed on.
    LaunchedEffect(Unit) { vm.loadMemberReviewRoster() }
    val memberReviewRoster by vm.memberReviewRoster.collectAsState()
    val memberMarks: List<ByteArray?> = remember(detail, memberReviewRoster) {
        if (detail == null || memberReviewRoster.isEmpty()) emptyList()
        else vm.memberMarksForThread(threadId, memberReviewRoster)
    }

    ConversationDetailContent(
        detail = detail,
        // Snapshot-driven add-participant overlay state (null = hidden). The
        // manager sets it via openAddParticipant and clears it on
        // confirm/cancel, so the overlay's visibility follows the snapshot — no
        // client-side state machine (conversations.md rule #1).
        addParticipant = snapshot?.addParticipant,
        // The page-level manager error (`confirm_add_participant`/
        // `remove_participant`/`rename_thread` all stamp this on refusal) —
        // `error-message`'s higher-precedence half, mirroring linux's
        // `page_error_text`.
        pageError = snapshot?.error,
        // The dead-receive-rail notice (conversations.md § Errors & edge
        // cases, the fourth truth) — a read of shared state set by the
        // receive loop's supervisor, never by this screen. Re-read on every
        // snapshot tick (the loop's death is what stamps the next snapshot),
        // mirroring windows's `ReceiveStopped`/apple's `receiveStopped()`: an
        // FFI fault degrades to `false` so the page's own errors stay
        // reachable if the probe itself breaks.
        receiveStopped = runCatching { manager.receiveStopped() }.getOrDefault(false),
        // The floor of the `error-message` stack (`ui/conversations.md` §
        // Errors & edge cases → *A fifth truth*): received mail this run that
        // could not open under the account's key set — same FFI-fault-degrades
        // posture as [receiveStopped] above.
        unopenableMail = runCatching { manager.unopenableMailCount() }.getOrDefault(0u),
        // What the compose bar says about the reply in progress — the shared
        // `reply_preview(thread_id)` record, re-read with the thread detail on
        // every snapshot tick (arming or cancelling a reply notifies).
        replyPreview = remember(snapshot, threadId) {
            runCatching { manager.replyPreview(threadId) }.getOrNull()
        },
        memberMarks = memberMarks,
        onKeepMember = { person -> vm.keepMemberReview(person) },
        // The chip itself IS Remove on a membership-change-capable thread
        // (conversations.md § the `thread-member-chip[i]` row) — bound at
        // paint by `ThreadHeader`, never re-looked-up by index here. A
        // refusal puts the member back and sets `snapshot.error` (read above
        // as `pageError`), entirely inside `remove_participant` — no
        // try/catch needed for that half; `runCatching` here only guards a
        // genuine FFI-level failure, mirroring `onRename`/
        // `onAddParticipantConfirm`.
        onRemoveParticipant = { addr ->
            scope.launch { runCatching { manager.removeParticipant(threadId, addr) } }
        },
        onBack = { navController.popBackStack() },
        // Reply seeds the reply-recipients (sender-only); reply-all seeds
        // every participant but self — the shared start_reply does both (and
        // sets the reply-to preview), so it replaces the old bare setReplyTo.
        // On a non-mail rail the seeded recipients are inert (the group is the
        // recipient set). conversations.md § Participants vs. reply recipients.
        onReply = { msgId -> manager.startReply(threadId, msgId, false) },
        onReplyAll = { msgId -> manager.startReply(threadId, msgId, true) },
        // The reply "To" line add input forwards raw text here; parse it through
        // the shared FFI tryParseTypedAddress (multi-rail) before adding, and
        // drop a chip by its address. Mirrors linux's compose_bar add/remove.
        onAddReplyRecipient = { raw ->
            tryParseTypedAddress(raw)?.let { manager.addReplyRecipient(threadId, it) }
        },
        onRemoveReplyRecipient = { addr -> manager.removeReplyRecipient(threadId, addr) },
        // Unstage a staged attachment (dm-compose-attachment-remove), positional
        // over ComposeState.attachments — mirrors linux/apple's first callers of
        // this built-but-previously-unused mutator.
        onRemoveAttachment = { i -> manager.removeAttachment(threadId, i.toUInt()) },
        // Add-participant: the button opens the snapshot-driven overlay; rename:
        // the button opens a local dialog whose Save passes the new label up
        // here. Mirror apps/fauna-linux/src/views/conversations/detail.rs:249-316.
        onAddParticipant = { manager.openAddParticipant(threadId) },
        onRename = { newLabel ->
            // renameThread is async (fires the MLS NameChanged wire op on
            // groups); a no-op against the backend-less prod manager today.
            scope.launch { runCatching { manager.renameThread(threadId, newLabel) } }
        },
        // Save is ONE call: the whole loop — one policy commit per staged
        // change in the draft's order, the hand-over last, stopping at the
        // first refusal — is `ConversationsManager::apply_room_settings`,
        // shared by all seven apps. A refusal is already painted on the page's
        // `error-message` by the call that refused, so `runCatching` here only
        // guards a genuine FFI-level failure, exactly as `onRename` does; a
        // thrown call reports `false`, which keeps the editor open.
        onApplyRoomSettings = { edits, done ->
            scope.launch {
                val allLanded =
                    runCatching { manager.applyRoomSettings(threadId, edits) }.getOrDefault(false)
                done(allLanded)
            }
        },
        onAddParticipantInputChange = {
            // Typing owes a probe — same rule as new-thread compose.
            manager.setAddParticipantRecipientInput(it)
            scope.launch { manager.resolveRecipient() }
        },
        onAddParticipantAccept = {
            // Same probe-then-commit flow as new-thread compose; the manager
            // routes resolve/accept to whichever picker is active — here the
            // add-participant picker, since snapshot.addParticipant is set.
            scope.launch {
                manager.resolveRecipient()
                manager.acceptCurrentRecipientChip()
            }
        },
        onAddParticipantConfirm = {
            // confirmAddParticipant is async (in-place add, or a FaunaMls 1:1
            // fork to a new MlsGroup — conversations.md:31-33); it clears
            // snapshot.addParticipant, which hides the overlay.
            scope.launch { runCatching { manager.confirmAddParticipant() } }
        },
        onAddParticipantCancel = { manager.cancelAddParticipant() },
        // Compose-bar wiring → per-thread manager mutators. Send is a parity
        // no-op in production (no rail backend registered from Kotlin yet); the
        // snapshot re-renders via the observer either way.
        onBodyChange = { manager.setComposeBody(threadId, it) },
        onSubjectChange = { manager.setComposeSubject(threadId, it) },
        onTopicToggle = { manager.toggleTopic(threadId) },
        onSend = { scope.launch { runCatching { manager.send(threadId) } } },
        onAttach = { attachmentPickerLauncher.launch("*/*") },
        onReplyCancel = { manager.setReplyTo(threadId, null) },
        // Inline markdown styling + shared toolbar-wrap, over the FFI (the Content
        // stays FFI-free for Robolectric). conversations.md § Compose-field inline
        // markdown styling + § Where logic lives.
        decorate = ::ffiMarkdownDecorate,
        wrap = ffiMarkdownWrap,
        composeDecorationPlan = ::ffiComposeDecorationPlan,
        composeShowMarkersDimRanges = ::ffiComposeShowMarkersDimRanges,
        // Reply-recipient chip display via the shared per-rail switch (FFI; the
        // Content default is a non-FFI fallback). conversations.md § Where logic lives.
        // A bridged address names its bridge by the label it declared (shared
        // `typed_address_display_with_bridges`).
        displayAddress = { typedAddressDisplayWithBridges(it, snapshot?.bridges ?: emptyList()) },
        bridgeLabels = snapshot?.bridges?.map { it.label } ?: emptyList(),
        // The shared attachment loader (`attachment_bytes(blob_hash)`) for the
        // bubble's `dm-attachment-image` render — an FFI read kept in the stateful
        // screen so the Content stays FFI-free. conversations.md § Attachments.
        loadAttachmentBytes = { manager.attachmentBytes(it) },
        attachmentResident = { manager.attachmentResident(it) },
        // D4 link-preview (render-model.md § D4): fire-once resolve of a `Resolving`
        // bubble block via the shared `ConversationsManager.resolveLinkPreview`, and the
        // og:image loader. The og:image is a nest-served content-addressed plaintext blob
        // (`/api/v1/blob/<hash>`), NOT an MLS attachment, so it loads through the plain HTTP
        // `vm.fetchBlobBytes` — NOT the `attachment_bytes` decrypt path above. Both routed
        // to the VM here so the Content stays FFI-free.
        resolveLinkPreview = { url -> vm.resolveLinkPreview(url) },
        loadLinkPreviewImageBytes = { vm.fetchBlobBytes(it) },
        // load-remote-content-button → shared manager (render-model.md § D3):
        // flips the reveal set + re-emits, so the re-resolved thread detail projects
        // RemoteImage.revealed = true onto this message and the bubble recomposes.
        // Mirrors onReply's direct manager wiring (the Content stays FFI-free).
        onRevealRemoteImages = { msgId -> manager.revealRemoteImages(msgId) },
        // Muted-keyword collapse (moderation.md § Muted keywords): the client-held list +
        // the session-local revealed set + the one-tap reveal (adds the message id to the
        // set; the mute is untouched). The Content stays FFI-free — the match runs inside
        // the bubble via the shared `matchesMutedKeywords`.
        mutedKeywords = mutedKeywords,
        revealedMuted = revealedMuted,
        onRevealMuted = { msgId -> if (msgId !in revealedMuted) revealedMuted.add(msgId) },
        // Content-policy render inputs — the bubble resolves each message's
        // block/collapse verdict off these in shared Rust (family-safety.md
        // § Content policy). The Content stays FFI-free? No — unlike muted
        // keywords, the verdict IS a shared-Rust call, computed per bubble.
        contentPolicyInputs = contentPolicyInputs,
        // Reactions + delete (conversations.md § Reactions & message delete) →
        // the shared manager (both async; FaunaMls-only, capability-gated in the
        // Content). toggle_reaction resolves Add/Remove against my current state;
        // delete_message is sender-only (the manager rejects a non-own target).
        onToggleReaction = toggleReaction,
        onDeleteMessage = { msgId ->
            scope.launch { runCatching { manager.deleteMessage(threadId, msgId) } }
        },
        // dm-reaction-more-button lifts the target up; the picker dialog (below)
        // is hosted here so the Content stays FFI-/View-free.
        onMoreReaction = { msgId -> moreReactionFor = msgId },
        // Mark-as-spam (the live `Insert` consumer, mail-spam.md § Wire shapes) →
        // the VM, which trains the sealed tier-1 model over the retained decrypted
        // body via the shared façade `trainSpamModelClientMail` + writes a sealed
        // audit row. Silent no-op when mail isn't enabled (client-only content the
        // nest can't read → no server fallback). Mirrors linux `mark_message_spam`.
        onMarkSpam = { msg -> vm.markMessageSpam(msg) },
        onReportMessage = { target -> vm.openReport(target) },
    )

    // The "more" emoji picker dialog — shown iff a bubble requested it. The
    // picked emoji toggles a reaction on that message through the SAME
    // toggle lambda the quick-set and the pills use, then the dialog dismisses.
    MoreReactionPicker(
        targetMessageId = moreReactionFor,
        onToggleReaction = toggleReaction,
        onClose = { moreReactionFor = null },
    )
}

/**
 * The "more reactions" sheet for [targetMessageId] (`null` = closed): a pick
 * toggles that emoji on that message through [onToggleReaction] — the screen's
 * one toggle path — then closes the sheet, as does a dismiss. Split out of the
 * screen so the e2e pick seam (`TestAgent`'s `type_text`[dm-reaction-more-button]
 * arm) can be pinned against a real manager without composing the whole screen.
 */
@Composable
internal fun MoreReactionPicker(
    targetMessageId: String?,
    onToggleReaction: (String, String) -> Unit,
    onClose: () -> Unit,
) {
    val msgId = targetMessageId ?: return
    ReactionPickerDialog(
        onPicked = { emoji ->
            onToggleReaction(msgId, emoji)
            onClose()
        },
        onDismiss = onClose,
    )
}

/**
 * The "more reactions" picker — the one sanctioned per-app divergence
 * (conversations.md § Reactions & message delete: the native emoji picker where
 * one exists). Android hosts the emoji2 [EmojiPickerView] via [AndroidView] in a
 * [Dialog]; the picked emoji is lifted to the caller, which toggles the reaction
 * through the shared manager. Not a ui.yaml element (the picker widget itself is
 * the divergence; only the `dm-reaction-more-button` trigger is shared), so it
 * carries no test id.
 *
 * Its cells therefore give UiAutomator nothing to click, so while the sheet is
 * shown it registers the very pick handler its listener calls on
 * `TestAgent.moreReactionPick`, where the `type_text`[dm-reaction-more-button]
 * arm performs the pick (`ui/conversations.md` § Reactions & message delete →
 * *Rendering / picker glue*: a native chooser's driver emits the chooser's own
 * pick). `BuildConfig.DEBUG`-gated so the release shrinker folds it away,
 * where the twin field is never read (convention 15).
 */
@Composable
private fun ReactionPickerDialog(
    onPicked: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val currentOnPicked by rememberUpdatedState(onPicked)
    val pick: (String) -> Unit = remember { { emoji -> currentOnPicked(emoji) } }
    if (BuildConfig.DEBUG) {
        DisposableEffect(pick) {
            com.fauna.app.testing.TestAgent.moreReactionPick = pick
            onDispose {
                if (com.fauna.app.testing.TestAgent.moreReactionPick === pick) {
                    com.fauna.app.testing.TestAgent.moreReactionPick = null
                }
            }
        }
    }
    Dialog(onDismissRequest = onDismiss) {
        Surface(shape = MaterialTheme.shapes.large) {
            AndroidView(
                factory = { ctx ->
                    EmojiPickerView(ctx).apply {
                        setOnEmojiPickedListener { picked -> pick(picked.emoji) }
                    }
                },
                modifier = Modifier.fillMaxWidth().heightIn(max = 360.dp),
            )
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConversationDetailContent(
    detail: ThreadDetail?,
    onBack: () -> Unit,
    onReply: (String) -> Unit,
    onAddParticipant: () -> Unit,
    onRename: (String) -> Unit,
    // The room policy editor's Save: the staged edits go up, the all-landed
    // verdict comes back — because the editor closes ONLY when every edit
    // landed (`ui/conversations.md` § Element IDs), and the draft this screen
    // holds is what has to be cleared. Defaults to refusing, so a caller that
    // has not wired it leaves the editor open rather than silently pretending
    // the policy changed.
    onApplyRoomSettings: (List<RoomSettingsEdit>, (Boolean) -> Unit) -> Unit =
        { _, done -> done(false) },
    onReplyAll: (String) -> Unit = {},
    onAddReplyRecipient: (String) -> Unit = {},
    onRemoveReplyRecipient: (TypedAddress) -> Unit = {},
    addParticipant: AddParticipantState? = null,
    // Post-succession member review (succession-aftermath.md § Propagation →
    // *MLS groups*), index-parallel with `detail.participantDisplays`. `{}`/
    // empty defaults keep the Content/test harness FFI-free, matching every
    // other injected callback here.
    memberMarks: List<ByteArray?> = emptyList(),
    onKeepMember: (ByteArray) -> Unit = {},
    onRemoveParticipant: (TypedAddress) -> Unit = {},
    // The page-level manager error (`ConversationsSnapshot.error` —
    // `confirm_add_participant`/`remove_participant`/`rename_thread`) — takes
    // precedence over a failed compose-send in `error-message` below, mirroring
    // linux's `page_error_text` (every producer clears this slot on entry, so
    // it is always the more recent of the two by construction).
    pageError: LocalizedText? = null,
    // The fourth `error-message` truth (`conversations.md` § Errors & edge
    // cases, 2026-09-13): the shared receive loop's supervisor died by panic
    // (`ConversationsManager::receive_stopped`) — a STANDING condition set by
    // the supervisor, never by a page producer or gesture, until a newer loop
    // over the same manager retires it. Android has no `engineServedElsewhere`
    // arm (no served-elsewhere state reaches this app by design), so this is
    // TOP precedence here — outranking [pageError] and a failed compose-send
    // below — unlike windows/apple, which read served-elsewhere first. Mirrors
    // apple's `ConversationsVM.pageError` / windows's `ActiveSendErrorReason` /
    // tui's `sync_page_error`.
    receiveStopped: Boolean = false,
    // The floor of the `error-message` stack (`ui/conversations.md` § Errors &
    // edge cases → *A fifth truth*, 2026-09-15): received mail this run the
    // client could not open under the account's current key set
    // (`ConversationsManager::unopenable_mail_count`) — shows only when
    // nothing above it does ([receiveStopped], [pageError], a failed compose-
    // send), and is never cleared by a gesture, only by the records opening on
    // a later re-drain. Mirrors linux `page_error_text`'s `unopenable_mail: u32`
    // param / tui's `sync_page_error` floor arm.
    unopenableMail: UInt = 0u,
    onAddParticipantInputChange: (String) -> Unit = {},
    onAddParticipantAccept: () -> Unit = {},
    onAddParticipantConfirm: () -> Unit = {},
    onAddParticipantCancel: () -> Unit = {},
    onBodyChange: (String) -> Unit = {},
    onSubjectChange: (String) -> Unit = {},
    onTopicToggle: () -> Unit = {},
    onSend: () -> Unit = {},
    onAttach: () -> Unit = {},
    onReplyCancel: () -> Unit = {},
    decorate: (String) -> List<MarkdownDecoration> = { emptyList() },
    wrap: MarkdownWrap = naiveMarkdownWrap,
    composeDecorationPlan: (text: String, caretByte: Int) -> ComposeMarkerPlan =
        { _, _ -> ComposeMarkerPlan(hide = emptyList(), dim = emptyList()) },
    composeShowMarkersDimRanges: (text: String, caretByte: Int) -> List<RevealSpan> = { _, _ -> emptyList() },
    displayAddress: (TypedAddress) -> String = { it.toString() },
    // The serving bridges' declared labels, for the recipient picker's
    // bridges line (`ConversationsSnapshot.bridges`).
    bridgeLabels: List<String> = emptyList(),
    // Byte-size label for a staged-attachment chip (shared `byte_size` FFI) —
    // injected like [displayAddress] so the Content/test harness stays FFI-free;
    // the default calls the real FFI directly (mirrors ProfileTiersTab's
    // claimStatusLabel/providerStatusLabel pattern).
    byteSize: (kotlin.ULong) -> LocalizedText = { com.fauna.ffi.byteSize(it) },
    onRemoveAttachment: (Int) -> Unit = {},
    // Resolves an attachment `blob_hash` to its plaintext bytes (the shared
    // `attachment_bytes` loader). `{ null }` keeps the Content/test harness
    // FFI-free — the bubble then renders the filename stub instead of decoding.
    loadAttachmentBytes: (String) -> ByteArray? = { null },
    // Whether a `blob_hash`'s bytes are resident right now (the shared read-only
    // `attachment_resident` peek) — the second key of the bubble's picture rebuild,
    // since an evict or a refetch changes no message (conversations.md
    // § Attachments → *Retention*). `{ false }` keeps the harness FFI-free.
    attachmentResident: (String) -> Boolean = { false },
    // The shared `reply_preview` record for the compose bar's `dm-reply-preview`,
    // read by the stateful screen (conversations.md § Where logic lives →
    // *Reply preview*).
    replyPreview: ReplyPreview? = null,
    // D4 link-preview (render-model.md § D4) — fire-once resolve of a `Resolving` block,
    // routed to the shared manager. `{}` keeps the Content/test harness FFI-free.
    resolveLinkPreview: (String) -> Unit = {},
    // The og:image loader for a Resolved link-preview card — a plain HTTP blob GET
    // (`/api/v1/blob/<hash>`, NOT the MLS `attachment_bytes` path), so it is `suspend`.
    // `{ null }` keeps the Content/test harness FFI-free (the og:image stays unpainted).
    loadLinkPreviewImageBytes: suspend (String) -> ByteArray? = { null },
    // Opts a message's inbound remote images into loading (`load-remote-content-button`,
    // render-model.md § D3) — routed to the shared manager by the stateful screen.
    // `{}` keeps the Content/test harness FFI-free.
    onRevealRemoteImages: (String) -> Unit = {},
    // Muted-keyword collapse (moderation.md § Muted keywords): the client-held muted list
    // (the bubble matches it at render via the shared `matchesMutedKeywords`), the
    // session-local set of revealed message ids, and the one-tap reveal. Defaults keep the
    // Content/test harness FFI-free (empty list → nothing collapses).
    mutedKeywords: List<uniffi.fauna_core.MutedKeyword> = emptyList(),
    revealedMuted: List<String> = emptyList(),
    onRevealMuted: (String) -> Unit = {},
    // Content-policy render inputs (family-safety.md § Content policy) — the
    // guardian floor + the viewer's own spam/phishing thresholds. The bubble
    // resolves its block/collapse verdict off these in shared Rust. The default
    // (empty) short-circuits to "show" WITHOUT the shared-Rust call
    // ([ContentPolicyInputs.verdictFor]), so the VM-free test harness stays
    // FFI-free exactly as the muted-keyword and lambda defaults above do.
    contentPolicyInputs: ContentPolicyInputs = ContentPolicyInputs(),
    // Reactions + delete (conversations.md § Reactions & message delete). The
    // first arg is the target message id; defaults `{}` keep the Content/test
    // harness FFI-free (the stateful screen routes them to the shared manager).
    onToggleReaction: (String, String) -> Unit = { _, _ -> },
    onDeleteMessage: (String) -> Unit = {},
    onMoreReaction: (String) -> Unit = {},
    // Mark-as-spam on a received message (the live `Insert` consumer, mail-spam.md
    // § Wire shapes). The whole `MessageSnapshot` is lifted so the VM has the
    // retained decrypted body + subject + opaque message id. Default `{}` keeps the
    // Content/test harness FFI-free (the stateful screen routes it to the VM).
    onMarkSpam: (MessageSnapshot) -> Unit = {},
    // `dm-message-report-button` → the shared report sheet (moderation.md
    // § User-initiated reporting); lifts the target the shared Rust constructor built.
    onReportMessage: (com.fauna.ffi.FfiReportTarget) -> Unit = {},
) {
    // Rename overlay visibility is local Compose state (the only client-side
    // state in this view) — the button opens it, Save lifts the label out via
    // onRename. Add-participant visibility is snapshot-driven instead (see
    // `addParticipant`), matching the linux split.
    var renameDialogVisible by remember { mutableStateOf(false) }
    // The room policy editor's staged draft — local Compose state like the
    // rename overlay's, and non-null exactly while the editor is open. The
    // draft itself is shared Rust (`fauna_conversations::RoomSettingsDraft`):
    // the seed, each row's eligibility, the at-most-one-staged hand-over rule,
    // the diff and the commit order are decided there for all seven apps, so
    // this screen only hands it back and forth and paints it.
    var roomSettingsDraft by remember { mutableStateOf<RoomSettingsDraft?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        detail?.let { localized(com.fauna.ffi.threadLabelDisplay(it.label)) ?: it.label }
                            ?: stringResource(R.string.conversations_detail_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            stringResource(R.string.common_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding).fillMaxSize()) {
            // Page-level error surface (conversations.md §"Errors & edge cases":
            // `error-message`). Precedence, mirroring linux's `page_error_text`:
            // [receiveStopped] (the dead-receive-rail notice, a STANDING
            // condition — TOP precedence on android, which has no
            // served-elsewhere arm) outranks `pageError` (a failed
            // `confirm_add_participant`/`remove_participant`/`rename_thread` —
            // every one of those clears this slot on entry, so it is always
            // the more recent of the two by construction), which outranks a
            // failed compose-send (`compose.sendState == Failed { reason }`,
            // stamped by the manager on a backend send error). No client-side
            // state machine (rule #1); every truth is a shared LocalizedText,
            // resolved through the app's string table, never painted raw
            // (rule #3). Below all of it, the floor of the stack:
            // [unopenableMail] — shown only when nothing above it is, never
            // cleared by a gesture (`ui/conversations.md` § Errors & edge
            // cases → *A fifth truth*).
            val pageErrorReason: LocalizedText? =
                if (receiveStopped) {
                    LocalizedText(key = "conversations.errors.receive_stopped", args = emptyMap())
                } else {
                    pageError
                        ?: (detail?.compose?.sendState as? SendState.Failed)?.reason
                        ?: unopenableMail.takeIf { it > 0u }?.let {
                            LocalizedText(
                                key = "conversations.errors.mail_unopenable",
                                args = mapOf("count" to it.toString()),
                            )
                        }
                }
            pageErrorReason?.let { reason ->
                Text(
                    localized(reason).orEmpty(),
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp)
                        .testTag(Ids.ERROR_MESSAGE),
                )
            }

            if (detail == null) {
                Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                    Text(
                        stringResource(R.string.conversations_list_select_conversation),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                ThreadHeader(
                    detail = detail,
                    memberMarks = memberMarks,
                    onAddParticipant = onAddParticipant,
                    onRename = { renameDialogVisible = true },
                    // Seeding is a snapshot read (`RoomSettingsDraft::seed`), so
                    // it happens right here rather than through a callback: a
                    // thread with no policy to edit seeds `null` and the editor
                    // stays shut, which is the same refusal the greyed button
                    // already expresses.
                    onRoomSettings = { roomSettingsDraft = roomSettingsSeed(detail) },
                    onKeepMember = onKeepMember,
                    onRemoveParticipant = onRemoveParticipant,
                )
                HorizontalDivider()
                // Messages area takes the remaining height so the compose bar
                // stays pinned at the bottom (conversations.md §"Empty states":
                // header + compose bar visible, bubble area blank).
                // `listState` drives the selected-message scroll below — the
                // search-hit half of `SearchNav::Mail` (conversations.md § The
                // selected message: "bringing it into view is part of the
                // affordance, not a nicety"). Mirrors linux detail.rs's
                // `scroll_widget_into_view`.
                val listState = rememberLazyListState()
                // `itemsIndexed(detail.messages, ...)` below emits exactly one
                // LazyColumn item per message (the subject divider renders
                // INSIDE that same item slot, before the bubble — see the
                // `msg.subjectLine?.let { ... }` call ahead of `MessageBubble`
                // in the items lambda), so a message's index into
                // `detail.messages` IS its LazyColumn item index. Verified by
                // reading the `itemsIndexed` call below before relying on it.
                LaunchedEffect(detail.selectedMessageId, detail.messages) {
                    val target = detail.selectedMessageId ?: return@LaunchedEffect
                    val index = detail.messages.indexOfFirst { it.messageId == target }
                    if (index >= 0) {
                        listState.animateScrollToItem(index)
                    }
                }
                Box(modifier = Modifier.weight(1f).fillMaxWidth()) {
                    if (detail.messages.isEmpty()) {
                        Box(modifier = Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                            Text(
                                stringResource(R.string.conversations_detail_no_messages),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    } else {
                        LazyColumn(
                            state = listState,
                            modifier = Modifier.fillMaxSize().padding(horizontal = 8.dp),
                        ) {
                            itemsIndexed(detail.messages, key = { _, m -> m.messageId }) { _, msg ->
                                // subject-divider renders BEFORE a bubble whose
                                // subject_line is non-null (the snapshot only sets
                                // it on a subject change — conversations.md §"Layout").
                                msg.subjectLine?.let { subject ->
                                    SubjectDivider(subject)
                                }
                                // The report identity (moderation.md § User-initiated
                                // reporting): a message's PLANE record digest and its
                                // Fauna-rail sender — `null` for a mail/bridged message,
                                // which has no plane identity and paints no report verb.
                                val senderActor = messageSenderActorHex(msg)
                                val reportKey = msg.planeRef?.recordDigest
                                val reported = remember(reportKey, senderActor, contentPolicyInputs) {
                                    contentPolicyInputs.isReported(reportKey, senderActor)
                                }
                                val reportTarget = remember(msg.messageId, msg.isOwn, msg.body, senderActor) {
                                    messageReportTarget(msg, senderActor)
                                }
                                MessageBubble(
                                    msg = msg,
                                    reported = reported,
                                    onReport = reportTarget?.let { target -> { onReportMessage(target) } },
                                    // `SearchNav::Mail`'s second half (conversations.md § The
                                    // selected message) — the read-time-resolved id, `None`
                                    // unless it names a message this thread currently holds.
                                    isSelected = msg.messageId == detail.selectedMessageId,
                                    onReply = { onReply(msg.messageId) },
                                    // dm-reply-all-button is mail-only (the
                                    // recipient-selection capability).
                                    showReplyAll = detail.capabilities.supportsRecipientSelection,
                                    onReplyAll = { onReplyAll(msg.messageId) },
                                    loadAttachmentBytes = loadAttachmentBytes,
                                    attachmentResident = attachmentResident,
                                    byteSize = byteSize,
                                    resolveLinkPreview = resolveLinkPreview,
                                    loadLinkPreviewImageBytes = loadLinkPreviewImageBytes,
                                    // load-remote-content-button → shared manager (D3):
                                    // flips the reveal set + re-emits, so the next
                                    // thread detail projects RemoteImage.revealed = true.
                                    onRevealRemoteImages = { onRevealRemoteImages(msg.messageId) },
                                    // Muted-keyword collapse (moderation.md § Muted keywords):
                                    // match the current list at render; a session-revealed id
                                    // shows in full (the mute stays).
                                    mutedKeywords = mutedKeywords,
                                    muteRevealed = msg.messageId in revealedMuted,
                                    onRevealMuted = { onRevealMuted(msg.messageId) },
                                    // Content-policy verdict (family-safety.md § Content
                                    // policy) — strictest-wins compose of the guardian
                                    // floor + the viewer's own thresholds, resolved in
                                    // shared Rust; a `block` collapses ahead of the muted
                                    // arm (never revealable), a `collapse` behind a reveal.
                                    contentVerdict = if (reported) "block" else contentPolicyInputs.verdictFor(msg.labels),
                                    // Reactions / delete affordances — capability-gated,
                                    // never rail-branched (rule #5). delete is further
                                    // gated on msg.isOwn inside the bubble.
                                    supportsReactions = detail.capabilities.supportsReactions,
                                    supportsMessageDelete = detail.capabilities.supportsMessageDelete,
                                    // Mark-as-spam on any received message — the
                                    // live `Insert` consumer, routed to the VM
                                    // (mail-spam.md § Wire shapes). Silent no-op
                                    // when mail isn't enabled.
                                    canFlagSpam = !msg.isOwn,
                                    onToggleReaction = { emoji -> onToggleReaction(msg.messageId, emoji) },
                                    onDeleteMessage = { onDeleteMessage(msg.messageId) },
                                    onMoreReaction = { onMoreReaction(msg.messageId) },
                                    onMarkSpam = { onMarkSpam(msg) },
                                )
                            }
                        }
                    }
                }
                HorizontalDivider()
                ComposeBar(
                    compose = detail.compose,
                    capabilities = detail.capabilities,
                    onBodyChange = onBodyChange,
                    onSubjectChange = onSubjectChange,
                    onTopicToggle = onTopicToggle,
                    onSend = onSend,
                    onAttach = onAttach,
                    onReplyCancel = onReplyCancel,
                    onAddReplyRecipient = onAddReplyRecipient,
                    onRemoveReplyRecipient = onRemoveReplyRecipient,
                    onRemoveAttachment = onRemoveAttachment,
                    byteSize = byteSize,
                    decorate = decorate,
                    wrap = wrap,
                    composeDecorationPlan = composeDecorationPlan,
                    composeShowMarkersDimRanges = composeShowMarkersDimRanges,
                    displayAddress = displayAddress,
                    replyPreview = replyPreview,
                )

                // Rename overlay (MLS groups only — gated by supportsRename on
                // the header button). Local visibility; Save lifts the new
                // label out via onRename → manager.renameThread. Mirrors
                // apps/fauna-linux/src/views/conversations/rename_overlay.rs.
                if (renameDialogVisible) {
                    RenameDialog(
                        currentLabel = detail.label,
                        onConfirm = { newLabel ->
                            onRename(newLabel)
                            renameDialogVisible = false
                        },
                        onDismiss = { renameDialogVisible = false },
                    )
                }

                // The room policy editor (`room_settings` sub-page). Local
                // visibility, like the rename overlay — but with one rule the
                // rename overlay does not have: it must OUTLIVE its own Save.
                // `apply_room_settings` stops at the first refusal and returns
                // whether every edit landed, and the contract is "closes only
                // when all landed" (`ui/conversations.md` § Element IDs), so
                // the draft is cleared ONLY on a `true` verdict — never in the
                // click handler. (Compose's `AlertDialog` does not dismiss
                // itself on a button press, so it is a safe container here;
                // linux had to abandon `adw::MessageDialog`, which does.)
                roomSettingsDraft?.let { draft ->
                    RoomSettingsDialog(
                        draft = draft,
                        detail = detail,
                        onDraftChange = { roomSettingsDraft = it },
                        onSave = { staged ->
                            // The edits are diffed HERE, where `detail` is
                            // non-null, and the verdict comes back so this
                            // scope — the one that owns the draft — decides
                            // whether the editor closes.
                            onApplyRoomSettings(
                                roomSettingsEdits(staged, detail.participants),
                            ) { allLanded -> if (allLanded) roomSettingsDraft = null }
                        },
                        onDismiss = { roomSettingsDraft = null },
                    )
                }

                // Add-participant overlay — snapshot-driven (shown iff
                // snapshot.addParticipant is set; the manager clears it on
                // confirm/cancel, hiding the dialog). Reuses the shared
                // RecipientPicker. Mirrors
                // apps/fauna-linux/src/views/conversations/add_participant_overlay.rs.
                addParticipant?.let { ap ->
                    AddParticipantDialog(
                        state = ap,
                        onInputChange = onAddParticipantInputChange,
                        onAccept = onAddParticipantAccept,
                        onConfirm = onAddParticipantConfirm,
                        onCancel = onAddParticipantCancel,
                        displayAddress = displayAddress,
                        bridgeLabels = bridgeLabels,
                    )
                }
            }
        }
    }
}

/**
 * Rename-thread modal (`thread-rename-field` on the entry, `thread-rename-confirm`
 * on Save). Local text state prefilled with the current label; Save lifts the
 * trimmed, non-empty label out via [onConfirm]. Android twin of
 * apps/fauna-linux/src/views/conversations/rename_overlay.rs (the IDs match the
 * cross-app e2e contract — see ui-actual extras note).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RenameDialog(
    currentLabel: String,
    onConfirm: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var text by remember(currentLabel) { mutableStateOf(currentLabel) }
    val trimmed = text.trim()
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.conversations_unified_thread_rename)) },
        text = {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                singleLine = true,
                placeholder = {
                    Text(stringResource(R.string.conversations_unified_thread_rename_placeholder))
                },
                modifier = Modifier.fillMaxWidth().testTag(Ids.THREAD_RENAME_FIELD),
            )
        },
        confirmButton = {
            TextButton(
                onClick = { if (trimmed.isNotEmpty()) onConfirm(trimmed) },
                enabled = trimmed.isNotEmpty(),
                modifier = Modifier.testTag(Ids.THREAD_RENAME_CONFIRM),
            ) {
                Text(stringResource(R.string.common_save))
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.common_cancel))
            }
        },
    )
}

/**
 * The room policy editor — the `room_settings` sub-page (`ui/conversations.md`
 * § Element IDs; behavior owner `conversation-rooms.md` § Roles and
 * authorization).
 *
 * A PAINTER and nothing more. The seed, which rows either control may act on,
 * the at-most-one-staged hand-over rule, the diff and the order Save issues it
 * in all live in `fauna_conversations::RoomSettingsDraft` +
 * `ConversationsManager::apply_room_settings`, reached here through the same
 * UniFFI twins apple and windows paint from — so no role rule is re-derived in
 * Kotlin (priority #2). Each staging gesture is "hand me the draft, take back
 * the staged one", the by-value shape UniFFI gives a record.
 *
 * ⚠ The editor must OUTLIVE its own Save: `apply_room_settings` stops at the
 * first refusal and answers whether every edit landed, and the contract is
 * "closes only when all landed". [onSave]'s verdict is what clears the draft;
 * nothing here closes on the press itself. `AlertDialog` is a safe container
 * for that (a button press does not dismiss it) — unlike linux's
 * `adw::MessageDialog`, which its leg had to abandon for this very reason.
 */
@Composable
private fun RoomSettingsDialog(
    draft: RoomSettingsDraft,
    detail: ThreadDetail,
    onDraftChange: (RoomSettingsDraft) -> Unit,
    onSave: (RoomSettingsDraft) -> Unit,
    onDismiss: () -> Unit,
) {
    val caps: ThreadCapabilities = detail.capabilities
    // The list every staging call and every read below indexes against, so a
    // gesture resolves by identity rather than by position
    // (`RoomSettingsDraft::slot_of`).
    val participants = detail.participants
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.conversations_unified_thread_room_settings)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                // The two rules. Both offer the ONE choice list shared by all
                // seven editors, in its order, and round-trip the picker token
                // — never a label and never a hand-typed set.
                Text(
                    stringResource(R.string.conversations_unified_room_join_rule_label),
                    style = MaterialTheme.typography.labelMedium,
                )
                TokenSelect(
                    testTagValue = Ids.ROOM_JOIN_RULE_SELECT,
                    selected = roomJoinRuleToken(draft.`joinRule`),
                    options = roomJoinRuleEditorChoices().map {
                        roomJoinRuleToken(it) to roomJoinRuleLabel(it)
                    },
                    onSelect = { token -> onDraftChange(roomSettingsSetJoinRule(draft, token)) },
                )
                Text(
                    stringResource(R.string.conversations_unified_room_history_policy_label),
                    style = MaterialTheme.typography.labelMedium,
                )
                TokenSelect(
                    testTagValue = Ids.ROOM_HISTORY_POLICY_SELECT,
                    selected = roomHistoryPolicyToken(draft.`historyPolicy`),
                    options = roomHistoryPolicyEditorChoices().map {
                        roomHistoryPolicyToken(it) to roomHistoryPolicyLabel(it)
                    },
                    onSelect = { token ->
                        onDraftChange(roomSettingsSetHistoryPolicy(draft, token))
                    },
                )
                // One row per member, indexed exactly like
                // `thread-member-chip[i]` so a driver's index means the same
                // person on both. Every state — staged admin, staged hand-over,
                // and whether either control may act on this row at all — is
                // read straight off the draft: the owner's own row and a
                // non-Fauna row are ineligible, and no app decides that itself.
                detail.participantDisplays.forEachIndexed { i, display ->
                    val eligible = roomSettingsIsEligible(draft, participants, i.toUInt())
                    val stagedAdmin = roomSettingsAdminAt(draft, participants, i.toUInt())
                    val stagedOwner =
                        roomSettingsTransferStagedAt(draft, participants, i.toUInt())
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(display, modifier = Modifier.weight(1f))
                        TextButton(
                            onClick = {
                                onDraftChange(
                                    roomSettingsToggleAdmin(draft, participants, i.toUInt()),
                                )
                            },
                            enabled = caps.canAppointAdmins && eligible,
                            modifier = Modifier
                                .testTag(Ids.ROOM_ADMIN_TOGGLE)
                                .semantics {
                                    stateDescription = if (stagedAdmin) "true" else "false"
                                },
                        ) {
                            Text(
                                stringResource(
                                    if (stagedAdmin) {
                                        R.string.conversations_unified_room_admin_yes
                                    } else {
                                        R.string.conversations_unified_room_admin_no
                                    },
                                ),
                            )
                        }
                        TextButton(
                            onClick = {
                                onDraftChange(
                                    roomSettingsToggleTransfer(draft, participants, i.toUInt()),
                                )
                            },
                            enabled = caps.canTransferOwnership && eligible,
                            modifier = Modifier
                                .testTag(Ids.ROOM_OWNER_TRANSFER_BUTTON)
                                .semantics {
                                    stateDescription = if (stagedOwner) "true" else "false"
                                },
                        ) {
                            Text(
                                stringResource(
                                    if (stagedOwner) {
                                        R.string.conversations_unified_room_transfer_staged
                                    } else {
                                        R.string.conversations_unified_room_transfer_mark
                                    },
                                ),
                            )
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(
                onClick = { onSave(draft) },
                modifier = Modifier.testTag(Ids.ROOM_SETTINGS_SAVE_BUTTON),
            ) {
                Text(stringResource(R.string.common_save))
            }
        },
        dismissButton = {
            // Cancel carries no id — the goal doc gives this editor Esc/dismiss
            // and no cancel element, like the rename overlay.
            TextButton(onClick = onDismiss) {
                Text(stringResource(R.string.common_cancel))
            }
        },
    )
}

/**
 * Add-participant modal — hosts the reused [RecipientPicker] (driven off
 * [AddParticipantState.picker]) plus an `add-participant-confirm` button. The
 * dialog owns no state; [onInputChange]/[onAccept] feed the picker and
 * [onConfirm]/[onCancel] drive the manager (which clears the snapshot's
 * add-participant state to dismiss). Android twin of
 * apps/fauna-linux/src/views/conversations/add_participant_overlay.rs.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AddParticipantDialog(
    state: AddParticipantState,
    onInputChange: (String) -> Unit,
    onAccept: () -> Unit,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
    displayAddress: (TypedAddress) -> String,
    bridgeLabels: List<String> = emptyList(),
) {
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text(stringResource(R.string.conversations_unified_thread_add_participant)) },
        text = {
            RecipientPicker(
                state = state.picker,
                onInputChange = onInputChange,
                onAccept = onAccept,
                displayAddress = displayAddress,
                bridgeLabels = bridgeLabels,
            )
        },
        confirmButton = {
            // ⚠ The gate applies to ONE arm. Confirming reaches the wire exactly
            // when the target is a bound FaunaMls **group**, whose add commit
            // opens by fetching the newcomer's key package; a FaunaMls 1:1 forks
            // a new group (its first *send* bootstraps it) and a non-FaunaMls
            // rail has no wire membership op at all — both succeed with no nest,
            // so gating them would be the over-claim the contract forbids. The
            // discriminant is NOT re-derived here: `inPlaceMlsGroup` is stamped
            // by the manager off the one shared
            // `fauna_conversations::capabilities::is_in_place_mls_group`
            // predicate `confirm_add_participant` also acts on (priority #2),
            // and it exists for exactly this gate. Same split as apple's
            // AddParticipantSheet.
            val confirmGate =
                if (state.inPlaceMlsGroup) faunaGate("fauna.conversations.keypackage.fetch")
                else FaunaGateVerdict(enabled = true, reason = null)
            Column {
                TextButton(
                    onClick = onConfirm,
                    enabled = confirmGate.enabled,
                    modifier = Modifier.testTag(Ids.ADD_PARTICIPANT_CONFIRM),
                ) {
                    Text(stringResource(R.string.common_add))
                }
                DisabledControlReasonText(confirmGate.reason)
            }
        },
        dismissButton = {
            TextButton(onClick = onCancel) {
                Text(stringResource(R.string.common_cancel))
            }
        },
    )
}

/**
 * Thread header strip (`thread-header` container) — label + protocol-icon, one
 * participant chip per member (the full set, never truncated —
 * conversations.md § Detail pane), and the capability-gated
 * add-participant / rename buttons. Mirrors
 * apps/fauna-linux/src/views/conversations/thread_header.rs.
 */
@Composable
private fun ThreadHeader(
    detail: ThreadDetail,
    memberMarks: List<ByteArray?>,
    onAddParticipant: () -> Unit,
    onRename: () -> Unit,
    onRoomSettings: () -> Unit,
    onKeepMember: (ByteArray) -> Unit,
    onRemoveParticipant: (TypedAddress) -> Unit,
) {
    val caps: ThreadCapabilities = detail.capabilities
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 12.dp, vertical = 8.dp)
            .testTag(Ids.THREAD_HEADER),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                localized(com.fauna.ffi.threadLabelDisplay(detail.label)) ?: detail.label,
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.weight(1f),
            )
            Text(
                sourceGlyphEmoji(detail.glyph),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(start = 8.dp).testTag(Ids.PROTOCOL_ICON),
            )
            // Capability-gated, never rail-branched (rule #5).
            if (caps.supportsRename) {
                IconButton(
                    onClick = onRename,
                    modifier = Modifier.testTag(Ids.THREAD_RENAME_BUTTON),
                ) {
                    Icon(
                        Icons.Default.Edit,
                        contentDescription = stringResource(R.string.conversations_unified_thread_rename),
                    )
                }
            }
            // Present off the RAIL's `supportsMembershipChange` (does this rail
            // model membership at all), then GREYED — never hidden — off the
            // viewer's role permitting an invite in a governed room
            // (`ui/conversations.md` § Architectural rules 5). The role half
            // is the roles table applied in shared Rust (`RoomSnapshot::gate`),
            // never re-derived here; every app gated add/remove on the rail
            // capability ALONE until the room model, so a plain member was
            // offered an Add the room would refuse.
            if (caps.supportsMembershipChange) {
                IconButton(
                    onClick = onAddParticipant,
                    enabled = caps.canInvite,
                    modifier = Modifier.testTag(Ids.THREAD_ADD_PARTICIPANT_BUTTON),
                ) {
                    Icon(
                        Icons.Default.PersonAdd,
                        contentDescription = stringResource(R.string.conversations_unified_thread_add_participant),
                    )
                }
            }
            // The policy editor's door: painted whenever the thread is a room,
            // greyed unless the viewer may set the policy — a policy-less room greys
            // it too, having nothing to edit (`ui/conversations.md` § Element
            // IDs). Absent, like the class statement below, where the rail
            // models no room.
            if (detail.room != null) {
                IconButton(
                    onClick = onRoomSettings,
                    enabled = caps.canSetPolicy,
                    modifier = Modifier.testTag(Ids.THREAD_ROOM_SETTINGS_BUTTON),
                ) {
                    Icon(
                        Icons.Default.Shield,
                        contentDescription = stringResource(R.string.conversations_unified_thread_room_settings),
                    )
                }
            }
        }
        // The room's class statement, read off the projected room and never
        // computed here — the class is a pure function of the member set,
        // derived in shared Rust (`conversation-rooms.md` § The three classes).
        // ABSENT where the rail models no room, which is the goal doc's own
        // word for it. The driver-facing token rides `stateDescription`, this
        // app's established carrier for a string state attribute (the same one
        // `recipient-resolve-status` uses for its `state`), which is the
        // richest mapping a future `/element/attr` bridge route could read.
        detail.room?.let { room ->
            Text(
                roomClassLabel(room.`class`),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .testTag(Ids.THREAD_ROOM_CLASS)
                    .semantics { stateDescription = roomClassAttrToken(room.`class`) },
            )
            // The family gate's marker in the detail, the row's twin — the
            // thread below it stays fully readable (`family-safety.md` § The
            // bridge-DM gate).
            detail.guardianState?.let { GuardianStateMarker(it) }
        }
        // Member chips — one per participant, the FULL set. This screen carried
        // a "first 3 + +N more" cap until 2026-08-28; no other app ever built
        // it, the doc line it came from never specified an expand, and it hid a
        // flagged member's review pair (below) behind the overflow — the
        // surface's job is to show the owner the whole roster
        // (succession-aftermath.md § Propagation → *MLS groups*).
        val displays = detail.participantDisplays
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            displays.forEachIndexed { i, display ->
                // The post-succession review pair, scoped INSIDE this chip
                // (identity-succession.md § Propagation → *MLS groups*, item
                // 3a) — index-parallel with `memberMarks`, so `memberMarks[i]`
                // answers for THIS chip and nothing here has to predict which
                // chip a flagged person landed on. Remove is deliberately NOT
                // re-rendered as its own control: the chip itself already IS
                // it (conversations.md § the `thread-member-chip[i]` row),
                // exactly as tui, linux, apple and web all render it.
                val reviewedPerson = memberMarks.getOrNull(i)
                // Bound at PAINT, never re-looked-up by index at tap time — a
                // roster that shifts between paint and tap must still remove
                // the person this chip named (the wrong-person guard
                // `TypedAddress::same_participant` keys on: the item names
                // this shape, mirroring tui's own test and web's 2026-09-01
                // fix). `null` only if the roster and its
                // display list have desynced, which never happens by
                // construction (`ThreadDetail::participants` is index-parallel
                // with `participantDisplays`).
                val participant = detail.participants.getOrNull(i)
                // Non-removable on a rail without real group membership (mail:
                // the historical From/To/Cc set is informational, not mutable
                // — conversations.md § Participants vs. reply recipients) —
                // gated on `supports_membership_change`, exactly as the
                // add-participant affordance above. A chip that cannot remove
                // is not a control at all: no click handler at all, mirroring
                // web's `role`/`tabindex` omission for the same case.
                //
                // ...AND, since the room model, gated on the viewer's role
                // permitting a removal — the roles table applied in shared Rust
                // (`RoomSnapshot::gate`), never re-derived here
                // (`conversation-rooms.md` § Roles and authorization). Both
                // halves, exactly as linux's and web's headers do it: the rail
                // decides whether membership is mutable at all, the role
                // decides whether THIS viewer may move it.
                val removable =
                    caps.supportsMembershipChange && caps.canRemoveMembers && participant != null
                // The member's role on a governed room: the `role` attribute a
                // driver reads off the chip, and the localized owner/admin mark
                // in its text. `null` on a policy-less room and on every non-room
                // thread, where the chip is the bare display name. Both come
                // from shared Rust — `RoomSnapshot::members` is index-parallel
                // with `participants`, and so with these chips.
                val memberRole = detail.room?.members?.getOrNull(i)?.role
                AssistChip(
                    onClick = if (removable) {
                        { onRemoveParticipant(participant!!) }
                    } else {
                        {}
                    },
                    label = {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Text(roomMemberChipText(display, memberRole))
                            if (reviewedPerson != null) {
                                Text(
                                    stringResource(R.string.conversations_detail_member_unattested_mark),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.error,
                                    modifier = Modifier
                                        .padding(start = 4.dp)
                                        .testTag(Ids.THREAD_MEMBER_UNATTESTED_MARK),
                                )
                                Text(
                                    stringResource(R.string.conversations_detail_member_keep),
                                    style = MaterialTheme.typography.labelSmall,
                                    color = MaterialTheme.colorScheme.primary,
                                    modifier = Modifier
                                        .padding(start = 4.dp)
                                        .clickable { onKeepMember(reviewedPerson) }
                                        .testTag(Ids.THREAD_MEMBER_KEEP_BUTTON),
                                )
                            }
                        }
                    },
                    modifier = Modifier
                        .testTag(Ids.THREAD_MEMBER_CHIP)
                        .then(
                            // Only a governed room has roles to state; on a
                            // policy-less room and every non-room thread the chip
                            // carries no `role` attribute at all, which is the
                            // goal doc's own rule for it (absent on a policy-less
                            // room) rather than a "member" guess.
                            if (memberRole != null) {
                                Modifier.semantics {
                                    stateDescription = roomRoleAttrToken(memberRole)
                                }
                            } else {
                                Modifier
                            },
                        ),
                )
            }
        }
    }
}

/** Inline subject-change divider (`subject-divider`, indexed). */
@Composable
private fun SubjectDivider(subject: String) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .testTag(Ids.SUBJECT_DIVIDER),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        HorizontalDivider(modifier = Modifier.weight(1f))
        Text(
            subject,
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = 8.dp),
        )
        HorizontalDivider(modifier = Modifier.weight(1f))
    }
}

/**
 * The mark for `SearchNav::Mail`'s second half (conversations.md § The
 * selected message) — a background/border tint, since Compose (like GTK) has
 * fill to vary. A ring rather than a fill swap so it reads on top of both the
 * self and peer bubble treatments, matching linux's `.message-selected`
 * (`box-shadow: inset 0 0 0 2px #f59e0b`, `apps/fauna-linux/src/style.css`) —
 * amber (`#F59E0B`) for cross-app visual consistency (priority #1/#3).
 */
private val SELECTED_MESSAGE_COLOR = Color(0xFFF59E0B)

private fun Modifier.selectedMessageMark(isSelected: Boolean): Modifier =
    if (isSelected) {
        this.border(2.dp, SELECTED_MESSAGE_COLOR, RoundedCornerShape(8.dp)).padding(2.dp)
    } else {
        this
    }

/**
 * A message's `dm-message-timestamp`, carrying the `selected` automation
 * observable (conversations.md § The selected message; `ui/search.md` §
 * State & data shape) via Compose's own boolean semantics property — the
 * natural Compose equivalent of an ARIA/AT-SPI "selected" boolean, and the
 * richest available mapping to what a future `/element/attr` bridge route
 * would read via `SemanticsNode.config[SemanticsProperties.Selected]` /
 * `AccessibilityNodeInfo.isSelected`. Mirrors linux's `message_timestamp_label` /
 * tui's `message_timestamp_element`: every one of [MessageBubble]'s 6 render
 * arms (5 tombstone/collapse early-returns + the normal path) paints its
 * timestamp through this ONE composable, since the timestamp is the one
 * bubble child painted whether the message is deleted, legal-takedown'd,
 * content-blocked, muted, content-collapsed, or normal — so a mail search hit
 * on any of those still has somewhere to land its mark. `selected` is always
 * set (never omitted when false), so a test can tell "not selected" apart
 * from "this app never painted the attribute" (testing.md point 6).
 */
@Composable
private fun MessageTimestamp(timestampMs: Long, isSelected: Boolean) {
    val context = LocalContext.current
    Text(
        ValueFormat.conversationTimestamp(context, timestampMs),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .testTag(Ids.DM_MESSAGE_TIMESTAMP)
            .semantics { selected = isSelected },
    )
}

/** `dm-attachment-image`'s `state` tokens (its `stateDescription`) — linux's two words:
 *  the picture painted from resident bytes, or its declared filename-and-size placeholder. */
internal const val ATTACHMENT_STATE_PAINTED = "painted"
internal const val ATTACHMENT_STATE_PLACEHOLDER = "placeholder"

/**
 * One message bubble (`dm-message-bubble` component) — sender, badges,
 * markdown-rendered body, attachments, and the per-message reply button.
 * Mirrors apps/fauna-linux/src/views/conversations/message_bubble.rs.
 */
@Composable
private fun MessageBubble(
    msg: MessageSnapshot,
    // `SearchNav::Mail`'s second half (conversations.md § The selected message) —
    // whether this is the one message a mail search result named. Painted on
    // EVERY arm below (the 5 tombstone/collapse early-returns + the normal path),
    // never only the normal one: a search hit can point at a message that is
    // since deleted/muted/blocked, and that is exactly the case most needing the
    // mark. See [MessageTimestamp] + [Modifier.selectedMessageMark].
    isSelected: Boolean = false,
    onReply: () -> Unit,
    showReplyAll: Boolean = false,
    onReplyAll: () -> Unit = {},
    loadAttachmentBytes: (String) -> ByteArray? = { null },
    attachmentResident: (String) -> Boolean = { false },
    byteSize: (kotlin.ULong) -> LocalizedText = { com.fauna.ffi.byteSize(it) },
    resolveLinkPreview: (String) -> Unit = {},
    loadLinkPreviewImageBytes: suspend (String) -> ByteArray? = { null },
    onRevealRemoteImages: () -> Unit = {},
    // Muted-keyword collapse (moderation.md § Muted keywords) — the current muted-word
    // list (matched at render via the shared `matchesMutedKeywords`) + a session-local
    // "revealed" flag + the one-tap reveal. Unlike the remote-image reveal (a manager
    // round-trip), the muted reveal is pure session-local client state.
    mutedKeywords: List<uniffi.fauna_core.MutedKeyword> = emptyList(),
    muteRevealed: Boolean = false,
    onRevealMuted: () -> Unit = {},
    // Content-policy render verdict for this message (family-safety.md § Content
    // policy) — one of "show" | "badge" | "collapse" | "block", resolved by the
    // caller off the shared engine. A `block` collapses the bubble to a notice
    // AHEAD of the muted arm (never revealable); a `collapse` collapses behind a
    // one-tap, session-local reveal. Default "show" keeps the harness FFI-free.
    contentVerdict: String = "show",
    // The viewer's OWN report hid this message (moderation.md § Corollary — block
    // also hides): the `block` notice names that act — "You reported this" — not a
    // policy. `contentVerdict` is already `block` when this is set.
    reported: Boolean = false,
    // `dm-message-report-button` (moderation.md § User-initiated reporting): `null`
    // paints nothing — own messages and mail/bridged messages have no verb.
    onReport: (() -> Unit)? = null,
    // Reactions / delete (conversations.md § Reactions & message delete) —
    // capability-gated, never rail-branched. `onToggleReaction` takes the emoji;
    // delete is sender-only (gated on msg.isOwn here too, defence-in-depth with
    // the manager). `onMoreReaction` opens the native picker (hosted upstream).
    supportsReactions: Boolean = false,
    supportsMessageDelete: Boolean = false,
    // Mark-as-spam gate — true on a received message (!is_own), independent of the
    // rail (mail training silently no-ops when mail isn't enabled). conversations
    // surface of mail-spam.md § Wire shapes.
    canFlagSpam: Boolean = false,
    onToggleReaction: (String) -> Unit = {},
    onDeleteMessage: () -> Unit = {},
    onMoreReaction: () -> Unit = {},
    onMarkSpam: () -> Unit = {},
) {
    // Session-local reveal for a content-policy `collapse` verdict (the floor
    // itself persists). Declared before any early return so its remember slot is
    // always allocated in the same order across recompositions of this bubble.
    var contentRevealed by remember(msg.messageId) { mutableStateOf(false) }

    // Deleted tombstone (conversations.md § Reactions & message delete): a
    // cooperative delete-marker every compliant client honors by rendering a
    // localized placeholder — body / attachments / reactions / actions all
    // stripped, so the bubble carries no `dm-message-text` once deleted. Mirrors
    // linux message_bubble.rs (early return with only the `dm-message-deleted`
    // element).
    if (msg.deleted) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).selectedMessageMark(isSelected),
        ) {
            Text(
                stringResource(R.string.conversations_detail_message_deleted),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.DM_MESSAGE_DELETED),
            )
            MessageTimestamp(msg.timestampMs, isSelected)
        }
        return
    }

    // Legal-takedown tombstone (moderation.md § Categories & enforcement item 1):
    // the nest withheld the sealed envelope under a legal obligation, so the bubble
    // collapses (like `deleted`) to the shared localized tombstone rendered in place
    // of the withheld body — never a blank/failed-decrypt bubble. Mirrors linux
    // message_bubble.rs / web +page.svelte and the post FeedScreen quoted tombstone.
    // No dedicated testTag (presentation; a new e2e id needs ui.yaml approval first,
    // § UI Consistency A).
    val legalTakedownRef = msg.legalTakedownRef
    if (legalTakedownRef != null) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).selectedMessageMark(isSelected),
        ) {
            Text(
                localized(legalTakedownTombstone(legalTakedownRef)).orEmpty(),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            MessageTimestamp(msg.timestampMs, isSelected)
        }
        return
    }

    // Content-policy BLOCK (a guardian floor) — collapse the bubble to a
    // policy-naming notice in place of the body, NO reveal (family-safety.md
    // § Content policy). Checked AHEAD of the muted-keyword arm so a message that
    // is both muted (revealable) and blocked can never be revealed past the
    // guardian's block — the linux `message_bubble.rs` ordering.
    // `content-policy-blocked-notice` is the one ui.yaml id this pillar renders.
    if (contentVerdict == "block") {
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).selectedMessageMark(isSelected),
        ) {
            Text(
                stringResource(
                    if (reported) R.string.moderation_report_hidden_placeholder
                    else R.string.family_content_blocked_notice,
                ),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.CONTENT_POLICY_BLOCKED_NOTICE),
            )
            MessageTimestamp(msg.timestampMs, isSelected)
        }
        return
    }

    // Muted-keyword collapse (moderation.md § Muted keywords): a decrypted message whose
    // body matches the user's muted-word list collapses behind a one-tap, session-local
    // reveal (the mute itself is untouched) — mirrors the deleted / legal-takedown
    // tombstones + the load-remote-content-button reveal. A *hide/collapse*, NOT a
    // spam-queue flag (it never feeds the LocalDetectionStore / moderation queue). The
    // match is over the *current* list (reflects Settings edits immediately) via the
    // shared `matchesMutedKeywords`.
    if (!muteRevealed && matchesMutedKeywords(msg.body, mutedKeywords)) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).selectedMessageMark(isSelected),
        ) {
            Text(
                stringResource(R.string.conversations_detail_muted_word),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.DM_MESSAGE_MUTED),
            )
            TextButton(
                onClick = onRevealMuted,
                modifier = Modifier.testTag(Ids.DM_MESSAGE_MUTED_REVEAL_BUTTON),
            ) {
                Text(stringResource(R.string.conversations_detail_muted_reveal))
            }
            MessageTimestamp(msg.timestampMs, isSelected)
        }
        return
    }

    // Content-policy COLLAPSE (a guardian `collapse` floor OR the viewer's own
    // spam/phishing threshold — the every-user un-darking of moderation.md § item
    // 1) — collapse behind a one-tap, session-local reveal; the floor persists.
    // After the muted arm (both are revealable collapses). Presentation only, no
    // test id: v1 e2e drives the block case (the linux/web precedent).
    if (contentVerdict == "collapse" && !contentRevealed) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp).selectedMessageMark(isSelected),
        ) {
            Text(
                stringResource(R.string.family_content_collapsed_notice),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            TextButton(onClick = { contentRevealed = true }) {
                Text(stringResource(R.string.family_content_reveal_button))
            }
            MessageTimestamp(msg.timestampMs, isSelected)
        }
        return
    }

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .selectedMessageMark(isSelected),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        // Top row: sender + badges.
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text(
                msg.senderDisplay,
                style = MaterialTheme.typography.labelLarge,
                fontWeight = FontWeight.Bold,
                modifier = Modifier.testTag(Ids.DM_SENDER),
            )
            Spacer(Modifier.width(8.dp))
            if (msg.badges.encrypted) {
                Icon(
                    Icons.Default.Lock,
                    contentDescription = stringResource(R.string.conversations_detail_badge_encrypted),
                    modifier = Modifier.size(16.dp).testTag(Ids.ENCRYPTED_BADGE),
                )
            }
            if (msg.badges.signed) {
                Icon(
                    Icons.Default.Check,
                    contentDescription = stringResource(R.string.conversations_message_signed),
                    modifier = Modifier.size(16.dp).testTag(Ids.SIGNED_BADGE),
                )
            }
            if (msg.badges.verified) {
                Icon(
                    Icons.Default.Verified,
                    contentDescription = stringResource(R.string.conversations_detail_badge_verified),
                    modifier = Modifier.size(16.dp).testTag(Ids.VERIFIED_BADGE),
                )
            }
            // Per-message timestamp — the SAME shared contextual bucketer the
            // conversation-list row uses (ValueFormat.conversationTimestamp →
            // fauna_core::format::conversation_timestamp_display; today → local
            // 24h clock / Yesterday / weekday / older → locale date,
            // value-formatting.md § Conversation timestamp). Adds the per-bubble
            // time android showed nowhere, unifying with linux/apple
            // (render-model.md § D5-adjacent; priorities #1/#4). Also carries the
            // `selected` observable — see [MessageTimestamp].
            Spacer(Modifier.weight(1f))
            MessageTimestamp(msg.timestampMs, isSelected)
        }

        // Content-label badge — the highest-confidence classifier verdict on
        // this message, if any, via the shared ContentLabelBadge (same idiom
        // as the moderation queue and feed post-cards). msg.badges.contentWarning
        // is a DIFFERENT, unrelated wire field — never populated by
        // libs/fauna-conversations; msg.labels is the real classifier data path
        // (moderation.md § Per-row badge data path).
        com.fauna.ffi.primaryContentLabel(msg.labels)?.let { entry ->
            ContentLabelBadge(label = entry.category)
        }

        // In-bubble reply-quote (render-model.md § D2 QuotedMessage): the manager folds it into
        // the document (PREPENDED) when this message replies to a parent loaded in the thread;
        // hidden otherwise. Painted as a card ABOVE the body (its own `dm-message-quote`
        // element), author + a ≤ 2-line snippet (maxLines=2, ellipsis). Compose twin of linux
        // `build_reply_quote_card` / web's quote card.
        documentQuotedMessage(msg.document)?.let { quote ->
            Surface(
                color = MaterialTheme.colorScheme.surfaceVariant,
                shape = MaterialTheme.shapes.small,
                modifier = Modifier
                    .testTag(Ids.DM_MESSAGE_QUOTE)
                    .padding(bottom = 4.dp),
            ) {
                Column(modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp)) {
                    Text(
                        quote.authorDisplay,
                        style = MaterialTheme.typography.labelMedium,
                        fontWeight = FontWeight.SemiBold,
                        color = MaterialTheme.colorScheme.primary,
                    )
                    Text(
                        quote.snippet,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 2,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }

        // Body — walk the shared RenderDocument the conversations manager already built
        // (markdown / plaintext / inbound-HTML all collapse to one typed node tree —
        // render-model.md § D1); the body is no longer re-parsed at render time. The Compose
        // twin of linux `document.rs` / web `document.ts`.
        //
        // Remote images in an inbound body are BLOCKED until a per-message opt-in
        // (html-mail.md § Rendering / § Security & privacy): the placeholder never fetches;
        // clicking `load-remote-content-button` dispatches `revealRemoteImages(messageId)` to
        // the shared manager (render-model.md § D3), which re-emits the thread detail with
        // `RemoteImage.revealed = true` projected onto THIS message's images. No client-side
        // reveal flag: the manager owns the privacy decision and the document carries it.
        DocumentBlocks(
            msg.document,
            modifier = Modifier.testTag(Ids.DM_MESSAGE_TEXT),
        )
        if (documentHasBlockedRemoteImage(msg.document)) {
            TextButton(
                onClick = onRevealRemoteImages,
                modifier = Modifier.testTag(Ids.LOAD_REMOTE_CONTENT_BUTTON),
            ) {
                Text(stringResource(R.string.conversations_detail_load_remote_content))
            }
        }

        // D4 link-preview cards — one per Resolved `LinkPreview` block the conversations
        // manager folded into the message document (render-model.md § D4), painted BELOW the
        // body like the feed card. The og:image obeys the SAME D3 reveal as a remote image
        // (its `load-remote-content-button` above also reveals it, since
        // `documentHasBlockedRemoteImage` counts a blocked og:image).
        // Not in Fauna Kids: a card is an invitation to open the link, and kids
        // render links as inert text (family-safety.md § The account age band,
        // the kids-app bullet, item (4)) — so no preview is resolved or painted.
        if (!BuildConfig.KIDS) {
            ConversationLinkPreviewCards(
                document = msg.document,
                loadImageBytes = loadLinkPreviewImageBytes,
                resolveLinkPreview = resolveLinkPreview,
            )
        }

        // Attachments — the `Attachment` embed blocks the manager appended after the body
        // (render-model.md § D2: `documentAttachments(msg.document)`, no longer the sibling
        // `msg.attachments` field). Images paint the real bytes (resolved through the shared
        // `attachment_bytes` loader) as an `Image`, the established android idiom
        // (BitmapFactory → asImageBitmap); the loader stays here (it is async). Falls back to
        // its DECLARED placeholder — filename and size, under the same id — when the bytes
        // aren't resident (a FaunaMls nest blob not yet GET+decrypted, one evicted with
        // nowhere to refill from, or the FFI-free test harness), so the `dm-attachment-image`
        // element always renders (conversations.md § Attachments → *Retention*). The picture
        // is keyed on residency as well as on the hash: an evict or a refetch changes no
        // message, so a hash-only key would keep painting a dropped picture and never repaint
        // one fetched again. `dm-attachment-image` answers `get_attr(.., "state")` with
        // `painted` or `placeholder` through `stateDescription`, linux's two tokens.
        documentAttachments(msg.document).forEach { att ->
            val declared = "${att.filename} (${localized(byteSize(att.sizeBytes)).orEmpty()})"
            if (att.isImage) {
                val resident = attachmentResident(att.blobHash)
                val imageBitmap = remember(att.blobHash, resident) {
                    loadAttachmentBytes(att.blobHash)?.let { bytes ->
                        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
                    }
                }
                if (imageBitmap != null) {
                    Image(
                        bitmap = imageBitmap,
                        contentDescription = declared,
                        modifier = Modifier
                            .heightIn(max = 200.dp)
                            .testTag(Ids.DM_ATTACHMENT_IMAGE)
                            .semantics { stateDescription = ATTACHMENT_STATE_PAINTED },
                    )
                } else {
                    Text(
                        declared,
                        style = MaterialTheme.typography.bodySmall,
                        modifier = Modifier
                            .testTag(Ids.DM_ATTACHMENT_IMAGE)
                            .semantics { stateDescription = ATTACHMENT_STATE_PLACEHOLDER },
                    )
                }
            } else {
                Text(
                    declared,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.testTag(Ids.DM_ATTACHMENT_FILE),
                )
            }
            // The receiver's own per-attachment verdict — shared Rust probed the bytes
            // (`AttachmentSnapshot.c2pa`), so the badge sits beside the attachment it vouches
            // for, never on the whole message (conversation-attachments.md § Attachments
            // "C2PA on-device"). The feed's `C2paBadge` leaf, as linux/windows reuse theirs.
            if (att.c2pa) {
                C2paBadge()
            }
        }

        // Aggregated reactions under the bubble (`dm-reaction-pill[i]` — emoji +
        // count, own highlighted, tap-to-toggle). Rendered only when the message
        // carries any; the count/highlight are the shared MessageSnapshot.reactions
        // aggregate. Mirrors linux build_reaction_pills.
        if (msg.reactions.isNotEmpty()) {
            ReactionPills(msg = msg, onToggleReaction = onToggleReaction)
        }

        // Per-message action row: ⋯ actions (reactions / delete) | reply | reply-all.
        // dm-reply-all-button sits next to dm-reply-button, mail-only (showReplyAll
        // = supportsRecipientSelection): reply seeds sender-only, reply-all every
        // participant but self (conversations.md § Participants vs. reply recipients).
        Row(verticalAlignment = Alignment.CenterVertically) {
            // ⋯ per-bubble actions — shown iff ≥1 action is available (react OR
            // own-deletable), so mail bubbles stay clean. Capability-gated, never
            // rail-branched (rule #5).
            MessageActionsMenu(
                canReact = supportsReactions,
                canDelete = supportsMessageDelete && msg.isOwn,
                // Mark-as-spam is offered on any received message (you flag
                // others' content, not your own), never rail-branched — it
                // silently no-ops when mail isn't enabled. Mirrors linux's
                // unconditional `can_flag_spam = !msg.is_own`.
                canFlagSpam = canFlagSpam,
                onToggleReaction = onToggleReaction,
                onMoreReaction = onMoreReaction,
                onDeleteMessage = onDeleteMessage,
                onMarkSpam = onMarkSpam,
                onReport = onReport,
            )
            IconButton(
                onClick = onReply,
                modifier = Modifier.size(32.dp).testTag(Ids.DM_REPLY_BUTTON),
            ) {
                Icon(
                    Icons.AutoMirrored.Filled.Reply,
                    contentDescription = stringResource(R.string.common_reply),
                    modifier = Modifier.size(18.dp),
                )
            }
            if (showReplyAll) {
                IconButton(
                    onClick = onReplyAll,
                    modifier = Modifier.size(32.dp).testTag(Ids.DM_REPLY_ALL_BUTTON),
                ) {
                    Icon(
                        Icons.AutoMirrored.Filled.ReplyAll,
                        contentDescription = stringResource(R.string.conversations_unified_reply_all),
                        modifier = Modifier.size(18.dp),
                    )
                }
            }
        }
    }
}

/**
 * The D4 link-preview cards (ui.yaml `link-preview-card`) for a conversation message bubble —
 * one per Resolved `LinkPreview` block in the message [document] (render-model.md § D4), the
 * conversations twin of the feed's `FeedLinkPreviewCards`. A `LaunchedEffect` fires
 * [resolveLinkPreview] **fire-once** for each `Resolving` block (the shared
 * `ConversationsManager.resolveLinkPreview` calls `fauna.linkpreview.resolve`, folds the
 * `Resolved` state, and re-emits). Each card shows title / description / domain (the shared
 * `urlHost` — host without scheme/port, the same source of truth as feed/linux/web/windows) and
 * the og:image, which is **blocked-by-default**: painted only when `revealed` (the D3 twin — the
 * bubble's `load-remote-content-button`, driven by `documentHasBlockedRemoteImage`, reveals it).
 * A `Resolving`/`Failed` block paints no card (the inline body link already shows).
 *
 * The og:image blob is a nest-served content-addressed plaintext blob (`/api/v1/blob/<hash>`,
 * the bulk-binary HTTP carve-out), so [loadImageBytes] is the plain HTTP loader
 * (`vm.fetchBlobBytes`), NOT the MLS `attachment_bytes` decrypt path. Reuses the shared
 * `documentResolvedLinkPreviews` / `documentResolvingLinkPreviewUrls` extractors (DocumentText.kt)
 * the feed card already drives.
 */
@Composable
private fun ConversationLinkPreviewCards(
    document: RenderDocument,
    loadImageBytes: suspend (String) -> ByteArray?,
    resolveLinkPreview: (String) -> Unit,
) {
    val resolvingUrls = documentResolvingLinkPreviewUrls(document)
    LaunchedEffect(resolvingUrls) {
        resolvingUrls.forEach { resolveLinkPreview(it) }
    }
    val uriHandler = LocalUriHandler.current
    documentResolvedLinkPreviews(document).forEach { lp ->
        Spacer(Modifier.height(8.dp))
        Card(
            onClick = { runCatching { uriHandler.openUri(lp.url) } },
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LINK_PREVIEW_CARD),
        ) {
            Column(modifier = Modifier.padding(12.dp)) {
                // og:image — blocked-by-default (render-model.md § D4): paint only when revealed.
                val hash = lp.imageHash
                if (lp.revealed && hash != null) {
                    val bitmap by produceState<ImageBitmap?>(null, hash) {
                        value = loadImageBytes(hash)?.let { bytes ->
                            BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
                        }
                    }
                    val bmp = bitmap
                    if (bmp != null) {
                        Image(
                            bitmap = bmp,
                            contentDescription = null,
                            modifier = Modifier
                                .fillMaxWidth()
                                .heightIn(max = 180.dp)
                                .testTag(Ids.LINK_PREVIEW_IMAGE),
                        )
                    } else {
                        Box(modifier = Modifier.fillMaxWidth().testTag(Ids.LINK_PREVIEW_IMAGE))
                    }
                }
                if (lp.title.isNotEmpty()) {
                    Text(
                        text = lp.title,
                        style = MaterialTheme.typography.titleSmall,
                        modifier = Modifier.testTag(Ids.LINK_PREVIEW_TITLE),
                    )
                }
                if (lp.description.isNotEmpty()) {
                    Text(
                        text = lp.description,
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 2,
                        modifier = Modifier.testTag(Ids.LINK_PREVIEW_DESCRIPTION),
                    )
                }
                Text(
                    text = urlHost(lp.url),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.LINK_PREVIEW_DOMAIN),
                )
            }
        }
    }
}

/**
 * Under-bubble reaction pill row (`dm-reaction-pill[i]`): one pill per
 * [uniffi.fauna_conversations.ReactionGroup] — emoji + count, own reactions
 * highlighted (`reactedByMe`), tap-to-toggle through [onToggleReaction]. Mirrors
 * linux `build_reaction_pills`.
 */
@Composable
private fun ReactionPills(
    msg: MessageSnapshot,
    onToggleReaction: (String) -> Unit,
) {
    Row(
        modifier = Modifier.padding(top = 2.dp),
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        msg.reactions.forEach { group ->
            Surface(
                color = if (group.reactedByMe) {
                    MaterialTheme.colorScheme.primaryContainer
                } else {
                    MaterialTheme.colorScheme.surfaceVariant
                },
                shape = MaterialTheme.shapes.small,
                modifier = Modifier
                    .testTag(Ids.DM_REACTION_PILL)
                    .clickable { onToggleReaction(group.emoji) },
            ) {
                Text(
                    "${group.emoji} ${group.count}",
                    style = MaterialTheme.typography.labelMedium,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
                )
            }
        }
    }
}

/**
 * The ⋯ per-bubble actions affordance: a `dm-message-actions-button` that opens
 * the `dm-message-actions-menu` dropdown — the reaction quick-set
 * (`dm-reaction-option[i]`, fixed shared order) + `dm-reaction-more-button`
 * (gated [canReact]), the own-only `dm-message-delete-button` →
 * `dm-message-delete-confirm-button` (gated [canDelete]), and the received-only
 * `dm-message-mark-as-spam-button` (gated [canFlagSpam] = !is_own). The ⋯ button
 * is hidden entirely when no action is available. Mirrors linux
 * `build_actions_button` / windows DmMessageBubble.
 */
@Composable
private fun MessageActionsMenu(
    canReact: Boolean,
    canDelete: Boolean,
    canFlagSpam: Boolean,
    onToggleReaction: (String) -> Unit,
    onMoreReaction: () -> Unit,
    onDeleteMessage: () -> Unit,
    onMarkSpam: () -> Unit,
    onReport: (() -> Unit)? = null,
) {
    if (!canReact && !canDelete && !canFlagSpam && onReport == null) return

    var expanded by remember { mutableStateOf(false) }
    // The destructive delete is a two-step inside the same flyout: tapping
    // dm-message-delete-button reveals dm-message-delete-confirm-button. Reset
    // when the menu closes so it always reopens at the first step.
    var confirmingDelete by remember { mutableStateOf(false) }

    Box {
        IconButton(
            onClick = { expanded = true },
            modifier = Modifier.size(32.dp).testTag(Ids.DM_MESSAGE_ACTIONS_BUTTON),
        ) {
            Icon(
                Icons.Default.MoreHoriz,
                contentDescription = stringResource(R.string.conversations_detail_message_actions),
                modifier = Modifier.size(18.dp),
            )
        }
        DropdownMenu(
            expanded = expanded,
            onDismissRequest = {
                expanded = false
                confirmingDelete = false
            },
            modifier = Modifier.testTag(Ids.DM_MESSAGE_ACTIONS_MENU),
        ) {
            if (canReact) {
                // Quick-set emoji row — the 6 fixed shared options. Each is a
                // tappable `dm-reaction-option`; tapping toggles + closes.
                Row(
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    QUICK_SET_EMOJI.forEach { emoji ->
                        Text(
                            emoji,
                            style = MaterialTheme.typography.titleMedium,
                            modifier = Modifier
                                .testTag(Ids.DM_REACTION_OPTION)
                                .clip(MaterialTheme.shapes.small)
                                .clickable {
                                    expanded = false
                                    onToggleReaction(emoji)
                                }
                                .padding(6.dp),
                        )
                    }
                }
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.conversations_detail_more_reactions)) },
                    onClick = {
                        expanded = false
                        onMoreReaction()
                    },
                    modifier = Modifier.testTag(Ids.DM_REACTION_MORE_BUTTON),
                )
            }
            if (canDelete) {
                if (!confirmingDelete) {
                    DropdownMenuItem(
                        text = {
                            Text(
                                stringResource(R.string.conversations_detail_delete_message),
                                color = MaterialTheme.colorScheme.error,
                            )
                        },
                        onClick = { confirmingDelete = true },
                        modifier = Modifier.testTag(Ids.DM_MESSAGE_DELETE_BUTTON),
                    )
                } else {
                    DropdownMenuItem(
                        text = {
                            Text(
                                stringResource(R.string.conversations_detail_delete_message_confirm),
                                color = MaterialTheme.colorScheme.error,
                            )
                        },
                        onClick = {
                            expanded = false
                            confirmingDelete = false
                            onDeleteMessage()
                        },
                        modifier = Modifier.testTag(Ids.DM_MESSAGE_DELETE_CONFIRM_BUTTON),
                    )
                }
            }
            if (canFlagSpam) {
                // Mark-as-spam — the live `Insert` consumer (mail-spam.md § Wire
                // shapes). Offered on any received message (!is_own); trains the
                // sealed tier-1 spam model over the retained decrypted body AND
                // writes a sealed training-history row via the shared façade
                // `trainSpamModelClientMail`. Silent no-op when mail isn't enabled
                // (no server fallback for client-only encrypted content). Mirrors
                // linux build_actions_button (`can_flag_spam`).
                // Training the sealed tier-1 model reseals it and PUTs it to
                // the mail bridge (`fauna.bridges.put_spam_model`); there is
                // deliberately no server-train fallback for conversation
                // content, because the nest cannot read it — so with no nest
                // this gesture has nothing it can do. tui's `MarkMessageSpam`
                // is the same call, and the moderation queue's
                // train-correction button is a DIFFERENT gesture that declares
                // `fauna.moderation.train`.
                val spamGate = faunaGate("fauna.bridges.put_spam_model")
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.conversations_detail_mark_as_spam)) },
                    onClick = {
                        expanded = false
                        onMarkSpam()
                    },
                    enabled = spamGate.enabled,
                    modifier = Modifier.testTag(Ids.DM_MESSAGE_MARK_AS_SPAM_BUTTON),
                )
            }
            if (onReport != null) {
                // Opens the shared report sheet (moderation.md § User-initiated
                // reporting) — received messages with a plane ref only; the
                // shell's ReportHost paints it.
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.conversations_detail_report_message)) },
                    onClick = {
                        expanded = false
                        onReport()
                    },
                    modifier = Modifier.testTag(Ids.DM_MESSAGE_REPORT_BUTTON),
                )
            }
        }
    }
}
