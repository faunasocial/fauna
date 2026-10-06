package com.fauna.app.ui.screen.conversations

import android.provider.OpenableColumns
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.ExifStripper
import com.fauna.app.ui.components.ComposeMarkerPlan
import com.fauna.app.ui.components.MarkdownDecoration
import com.fauna.app.ui.components.MarkdownWrap
import com.fauna.app.ui.components.RevealSpan
import com.fauna.app.ui.components.naiveMarkdownWrap
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.ConversationsVM
import kotlinx.coroutines.launch
import uniffi.fauna_conversations.ComposeState
import uniffi.fauna_conversations.SendState
// The room model's class mapping — the shared `fauna_conversations::room`
// helpers every app paints the picker's class statement from.
import uniffi.fauna_conversations.roomClassAttrToken
import uniffi.fauna_conversations.roomClassLabel
import uniffi.fauna_conversations.roomProspectiveClass
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_conversations.typedAddressDisplayWithBridges
import uniffi.fauna_core.LocalizedText
import social.fauna.generated.Ids

/**
 * New-thread compose screen (mobile-collapsed detail pane in "new compose"
 * mode). Reached by tapping `new-conversation-button` on the list, which calls
 * `manager.startNewConversation()` and navigates here. Renders the
 * `recipient-picker` component + the shared compose bar off
 * `snapshot.newThreadCompose`, mirroring the linux 3-state stack's `new_thread`
 * page (`apps/fauna-linux/src/views/conversations/detail.rs`).
 *
 * Per docs/goal/ui/conversations.md §"New-thread compose lives in the detail
 * pane, not a modal" (`:75-80`) + §"Mobile collapse" (`:82-86`): on android the
 * detail pane is a separate screen, so the new-thread compose is its own screen
 * rather than an inline pane. The VM-bound [NewThreadComposeScreen] holds the
 * manager; the stateless [NewThreadComposeContent] is the Robolectric-testable
 * render (plain snapshot records, no FFI).
 *
 * Send goes through the shared `manager.send_new_thread`, which materializes the
 * thread + selects it before the rail send runs — so the thread is selected
 * whether the send succeeds or is refused, and we navigate to its detail screen
 * either way (a refusal then shows there, on `error-message`).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewThreadComposeScreen(
    navController: NavController,
    vm: ConversationsVM = hiltViewModel(),
) {
    val snapshot by vm.managerSnapshot.collectAsState()
    val manager = vm.conversationsManager
    val scope = rememberCoroutineScope()
    val context = LocalContext.current

    // `attachment-button` before a thread exists (conversations.md § Attachments):
    // same contentResolver + `ExifStripper` idiom as ConversationDetailScreen's
    // picker (this package), staging onto the single-slot new-thread draft via
    // `add_new_thread_attachment` — `send_new_thread` carries it onto the
    // materialized thread.
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
            manager.addNewThreadAttachment(fileName, mimeType, bytes)
        }
    }

    NewThreadComposeContent(
        compose = snapshot?.newThreadCompose,
        onBack = {
            // Plain back / dismiss: deactivate the composer VIEW but PRESERVE the
            // half-written draft (conversations.md § Persistence — re-opening +
            // restores it). Distinct from the explicit discard below; the
            // nav-back equivalent of desktop `select_thread`'s deactivate.
            manager.deactivateNewConversation()
            navController.popBackStack()
        },
        onDiscard = {
            // Explicit discard (`new-conversation-cancel`): the one path —
            // alongside a successful send — that drops the new-thread draft.
            manager.cancelNewConversation()
            navController.popBackStack()
        },
        onRecipientInputChange = {
            // Typing owes a probe: the sync write parks the picker on Resolving
            // and the manager's async resolve settles it — launched per change,
            // the manager only stamps a result whose input is still current
            // (conversations.md § Errors & edge cases → *The picker tells the truth*).
            manager.setNewThreadRecipientInput(it)
            scope.launch { manager.resolveRecipient() }
        },
        onAcceptRecipient = {
            // Mirror the linux on-accept flow: probe the typed recipient
            // (promotes a Fauna handle/actor-id to a Fauna chip; the email rail
            // claims a plain address), then commit what the probe confirmed — a
            // chip is never a format parse of the raw text. The manager decides
            // which picker is active.
            scope.launch {
                manager.resolveRecipient()
                manager.acceptCurrentRecipientChip()
            }
        },
        onBodyChange = { manager.setNewThreadBody(it) },
        onSubjectChange = { manager.setNewThreadSubject(it.ifEmpty { null }) },
        onTopicToggle = {
            // No per-thread id for new-thread compose; toggle subject_draft
            // directly (linux detail.rs new-thread topic-toggle).
            val hasSubject = manager.snapshot().newThreadCompose?.subjectDraft != null
            manager.setNewThreadSubject(if (hasSubject) null else "")
        },
        onSend = {
            scope.launch {
                // A refused send throws AFTER materializing + selecting the
                // thread, so navigate on either outcome. Empty
                // recipient → send_new_thread no-ops (Ok(None)), selected stays
                // null → we remain on the compose screen.
                runCatching { manager.sendNewThread() }
                manager.snapshot().selectedThreadId?.let { tid ->
                    navController.navigate("conversation/$tid") {
                        popUpTo("conversations")
                    }
                }
            }
        },
        onAttach = { attachmentPickerLauncher.launch("*/*") },
        onReplyCancel = { /* new-thread compose has no reply context */ },
        // Unstage a staged attachment on the single-slot new-thread draft —
        // mirrors ConversationDetailScreen's onRemoveAttachment.
        onRemoveAttachment = { i -> manager.removeNewThreadAttachment(i.toUInt()) },
        // Inline markdown styling + shared toolbar-wrap over the FFI (Content stays
        // FFI-free for Robolectric). Shared helpers in ConversationDetailScreen.kt.
        decorate = ::ffiMarkdownDecorate,
        wrap = ffiMarkdownWrap,
        composeDecorationPlan = ::ffiComposeDecorationPlan,
        composeShowMarkersDimRanges = ::ffiComposeShowMarkersDimRanges,
        // Recipient chip/suggestion display via the shared per-rail switch (FFI).
        displayAddress = { typedAddressDisplayWithBridges(it, snapshot?.bridges ?: emptyList()) },
        bridgeLabels = snapshot?.bridges?.map { it.label } ?: emptyList(),
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewThreadComposeContent(
    compose: ComposeState?,
    onBack: () -> Unit,
    onDiscard: () -> Unit = onBack,
    onRecipientInputChange: (String) -> Unit,
    onAcceptRecipient: () -> Unit,
    onBodyChange: (String) -> Unit,
    onSubjectChange: (String) -> Unit,
    onTopicToggle: () -> Unit,
    onSend: () -> Unit,
    onAttach: () -> Unit,
    onReplyCancel: () -> Unit,
    onRemoveAttachment: (Int) -> Unit = {},
    decorate: (String) -> List<MarkdownDecoration> = { emptyList() },
    wrap: MarkdownWrap = naiveMarkdownWrap,
    composeDecorationPlan: (text: String, caretByte: Int) -> ComposeMarkerPlan =
        { _, _ -> ComposeMarkerPlan(hide = emptyList(), dim = emptyList()) },
    composeShowMarkersDimRanges: (text: String, caretByte: Int) -> List<RevealSpan> = { _, _ -> emptyList() },
    displayAddress: (TypedAddress) -> String = { it.toString() },
    // The serving bridges' declared labels, for the recipient picker's
    // bridges line (`ConversationsSnapshot.bridges`).
    bridgeLabels: List<String> = emptyList(),
    // Byte-size label for a staged-attachment chip — same FFI-free-injection
    // shape as [displayAddress]; default calls the real shared FFI.
    byteSize: (kotlin.ULong) -> LocalizedText = { com.fauna.ffi.byteSize(it) },
) {
    // System back gesture is a plain dismiss too: route it through onBack so the
    // composer view deactivates while the draft is preserved (conversations.md
    // § Persistence) — never a silent discard.
    BackHandler(onBack = onBack)

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.conversations_list_new_conversation),
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
                actions = {
                    // Explicit discard — the canonical `new-conversation-cancel`
                    // affordance (windows owns the ui.yaml element; linux ships a
                    // header Cancel). Clears the draft + dismisses the composer.
                    IconButton(
                        onClick = onDiscard,
                        modifier = Modifier.testTag(Ids.NEW_CONVERSATION_CANCEL),
                    ) {
                        Icon(Icons.Default.Close, stringResource(R.string.common_cancel))
                    }
                },
            )
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding).fillMaxSize()) {
            // Page-level error surface (conversations.md §"Errors & edge cases":
            // `error-message`) — the active compose's `send_state == Failed`
            // shows the reason, any other state hides it. Reads the shared
            // ComposeState the manager stamps (no client-side state machine).
            // `reason` is a shared LocalizedText, resolved through the app's
            // string table (rule #3: never hardcode English).
            (compose?.sendState as? SendState.Failed)?.let { failed ->
                Text(
                    localized(failed.reason).orEmpty(),
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 16.dp, vertical = 4.dp)
                        .testTag(Ids.ERROR_MESSAGE),
                )
            }

            if (compose == null) {
                Box(modifier = Modifier.fillMaxSize(), contentAlignment = androidx.compose.ui.Alignment.Center) {
                    Text(
                        stringResource(R.string.conversations_list_select_conversation),
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                return@Column
            }

            compose.recipientPicker?.let { picker ->
                RecipientPicker(
                    state = picker,
                    onInputChange = onRecipientInputChange,
                    onAccept = onAcceptRecipient,
                    displayAddress = displayAddress,
                    bridgeLabels = bridgeLabels,
                )
                // The class of the room about to be created, stated once a chip
                // is committed and before the first message goes out
                // (`conversation-rooms.md` § The three classes). Derived in
                // shared Rust from the committed chips and the home-nest
                // choice — this only paints it, and shows nothing before the
                // first chip. The driver-facing token rides `stateDescription`,
                // this app's carrier for a string state attribute.
                roomProspectiveClass(picker.chips, picker.includeHomeNest)?.let { klass ->
                    Text(
                        roomClassLabel(klass),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier
                            .padding(horizontal = 8.dp)
                            .testTag(Ids.RECIPIENT_PICKER_CLASS)
                            .semantics { stateDescription = roomClassAttrToken(klass) },
                    )
                }
                // Group-conversation hint surfaces at ≥2 chips (same threshold
                // as the Windows/linux reference).
                if (picker.chips.size >= 2) {
                    Text(
                        stringResource(R.string.conversations_unified_group_conversation_hint),
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier
                            .padding(horizontal = 8.dp)
                            .testTag(Ids.GROUP_CONVERSATION_HINT),
                    )
                }
            }

            Spacer(Modifier.weight(1f))

            ComposeBar(
                compose = compose,
                capabilities = null,
                onBodyChange = onBodyChange,
                onSubjectChange = onSubjectChange,
                onTopicToggle = onTopicToggle,
                onSend = onSend,
                onAttach = onAttach,
                onReplyCancel = onReplyCancel,
                onRemoveAttachment = onRemoveAttachment,
                decorate = decorate,
                wrap = wrap,
                composeDecorationPlan = composeDecorationPlan,
                composeShowMarkersDimRanges = composeShowMarkersDimRanges,
                displayAddress = displayAddress,
                byteSize = byteSize,
            )
        }
    }
}
