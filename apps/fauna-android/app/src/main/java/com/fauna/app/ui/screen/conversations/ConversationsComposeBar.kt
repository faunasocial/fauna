package com.fauna.app.ui.screen.conversations

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Reply
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.AttachFile
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Description
import androidx.compose.material.icons.filled.Image
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.components.ComposeFieldStyling
import com.fauna.app.ui.components.ComposeMarkerPlan
import com.fauna.app.ui.components.MarkdownDecoration
import com.fauna.app.ui.components.MarkdownHideVisualTransformation
import com.fauna.app.ui.components.MarkdownToolbar
import com.fauna.app.ui.components.MarkdownVisualTransformation
import com.fauna.app.ui.components.MarkdownWrap
import com.fauna.app.ui.components.RecordingVisualTransformation
import com.fauna.app.ui.components.RevealSpan
import com.fauna.app.ui.components.naiveMarkdownWrap
import com.fauna.app.ui.components.utf16IndexToUtf8Byte
import com.fauna.app.ui.util.localized
import uniffi.fauna_conversations.ComposeState
import uniffi.fauna_conversations.RecipientPickerState
import uniffi.fauna_conversations.ReplyPreview
import uniffi.fauna_conversations.SendState
import uniffi.fauna_conversations.ThreadCapabilities
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_conversations.recipientResolveStatus
import uniffi.fauna_core.LocalizedText
import social.fauna.generated.Ids

/**
 * Shared compose primitives for the unified conversations page — the
 * [ComposeBar] (detail-pane + new-thread compose) and the [RecipientPicker]
 * (new-thread compose; reused by the Phase-4 add-participant overlay). Both are
 * **stateless** Content composables: they render a slice of the shared
 * [uniffi.fauna_conversations.ConversationsManager] snapshot
 * ([ComposeState] / [RecipientPickerState]) and forward events to callbacks the
 * stateful Screen wires to manager mutators. Keeping them FFI-free is what makes
 * the Robolectric harness able to construct the snapshot records directly (the
 * `.so` can't load under Robolectric). Android twins of
 * `apps/fauna-linux/src/views/conversations/{compose_bar, recipient_picker}.rs`.
 *
 * Per docs/goal/ui/conversations.md §"Architectural rules" #5 (capability gating,
 * never rail branches) and §"Don't do these" (no `transport-toggle-button`; the
 * rail is decided by recipient resolution, not a UI toggle).
 */

/**
 * Multi-rail recipient picker: chips + input + suggestions + resolve status.
 * Mirrors `recipient_picker.rs`. Chip/suggestion IDs are indexed per
 * conversations.md rule #4 (scoped queries). Chips render without an inline
 * remove affordance and suggestions are non-interactive — matching the linux
 * sibling; `remove_recipient_chip` / `accept_suggestion` are documented future
 * actions not yet exported by the manager binding.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun RecipientPicker(
    state: RecipientPickerState,
    onInputChange: (String) -> Unit,
    onAccept: () -> Unit,
    // Raw-address → display string, injected from the stateful Screen so this
    // Content stays FFI-free for Robolectric (the .so can't load there).
    // Production injects the shared `fauna_conversations::typed_address_display`
    // FFI (the canonical per-rail switch — conversations.md § Where logic lives);
    // the default is a non-FFI fallback used only by the Compose test harness.
    displayAddress: (TypedAddress) -> String = { it.toString() },
    // The serving bridges' declared labels (`ConversationsSnapshot.bridges`) —
    // which far networks a typed address may reach. Only a list: the nest
    // matches an address to its bridge (`conversations.md` § Where logic lives
    // → *The `Bridged` adapter*, ruling 2 (d)).
    bridgeLabels: List<String> = emptyList(),
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 8.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        // Chips row (indexed `recipient-picker-chip`).
        if (state.chips.isNotEmpty()) {
            LazyRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                itemsIndexed(state.chips) { _, chip ->
                    AssistChip(
                        onClick = {},
                        label = { Text(displayAddress(chip)) },
                        modifier = Modifier.testTag(Ids.RECIPIENT_PICKER_CHIP),
                    )
                }
            }
        }

        OutlinedTextField(
            value = state.rawInput,
            onValueChange = onInputChange,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.RECIPIENT_PICKER_INPUT),
            placeholder = {
                Text(stringResource(R.string.conversations_unified_recipient_picker_placeholder))
            },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { onAccept() }),
        )

        // Chrome, like tui's: ui.yaml gives the bridges line no id.
        if (bridgeLabels.isNotEmpty()) {
            Text(
                stringResource(R.string.conversations_unified_recipient_picker_bridges)
                    .replace("{bridges}", bridgeLabels.joinToString(", ")),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        // Suggestions (indexed `recipient-picker-suggestion`) — non-interactive
        // display, parity with the linux sibling.
        state.suggestions.forEachIndexed { _, sug ->
            Text(
                displayAddress(sug),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(vertical = 4.dp)
                    .testTag(Ids.RECIPIENT_PICKER_SUGGESTION),
            )
        }

        // Resolve status — single element with a state attribute (conversations.md
        // §"Errors & edge cases": resolving / resolved / error / not-found). The
        // state token rides `stateDescription` so a future android e2e driver's
        // `get_attr("recipient-resolve-status", "state")` can read it (the android
        // emulator e2e is host-gated today; the visible text mirrors the i18n
        // string for the current state).
        val view = recipientResolveStatus(state.resolveState)
        Text(
            localized(view.label).orEmpty(),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier
                .testTag(Ids.RECIPIENT_RESOLVE_STATUS)
                .semantics { stateDescription = view.token },
        )
    }
}

/**
 * Compose bar at the bottom of the detail pane (and reused in new-thread
 * compose). Reply preview, optional subject input, body field, topic-toggle,
 * attachment + send buttons. Mirrors `compose_bar.rs`. All state lives in the
 * shared [ComposeState]; affordance sensitivity gates on `capabilities.*`
 * (rule #5) — `caps == null` (new-thread compose, rail not yet decided) leaves
 * every affordance enabled.
 */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun ComposeBar(
    compose: ComposeState,
    capabilities: ThreadCapabilities?,
    onBodyChange: (String) -> Unit,
    onSubjectChange: (String) -> Unit,
    onTopicToggle: () -> Unit,
    onSend: () -> Unit,
    onAttach: () -> Unit,
    onReplyCancel: () -> Unit,
    onAddReplyRecipient: (String) -> Unit = {},
    onRemoveReplyRecipient: (TypedAddress) -> Unit = {},
    onRemoveAttachment: (Int) -> Unit = {},
    // Byte-size label for a staged attachment chip, injected like
    // [claimStatusLabel]/[providerStatusLabel] in ProfileTiersTab.kt so tests can
    // swap in a non-FFI double (the .so can't load under Robolectric); production
    // takes the default, which calls the shared `fauna_core::format::byte_size`
    // FFI directly (mirrors `ValueFormat.byteSize` minus the Context-bound
    // resolution, done here via [localized]).
    byteSize: (kotlin.ULong) -> LocalizedText = { com.fauna.ffi.byteSize(it) },
    // Inline markdown styling + shared toolbar-wrap rule, injected from the
    // stateful Screen so this Content stays FFI-free for Robolectric (the .so
    // can't load there). [decorate] maps the shared `decoration_map`; [wrap] is
    // the shared `wrap_selection`. Defaults are FFI-free (no styling / naive
    // wrap) — production screens inject the real ones. conversations.md
    // § Compose-field inline markdown styling + § Where logic lives.
    decorate: (String) -> List<MarkdownDecoration> = { emptyList() },
    wrap: MarkdownWrap = naiveMarkdownWrap,
    // The hide-by-default rule (`compose_decoration_plan`) and the show-markers
    // dim-mode rule (`compose_show_markers_dim_ranges`), injected the same way —
    // both take (text, caretByte: UTF-8 byte offset). Defaults are FFI-free
    // no-ops. docs/goal/ui/conversations.md § Compose-field inline markdown styling.
    composeDecorationPlan: (text: String, caretByte: Int) -> ComposeMarkerPlan =
        { _, _ -> ComposeMarkerPlan(hide = emptyList(), dim = emptyList()) },
    composeShowMarkersDimRanges: (text: String, caretByte: Int) -> List<RevealSpan> = { _, _ -> emptyList() },
    // Raw-address → display string for the reply-recipient chips, injected like
    // [decorate]/[wrap] so the Content stays FFI-free for Robolectric. Production
    // injects the shared `typed_address_display` FFI; the default is the
    // non-FFI test-harness fallback. conversations.md § Where logic lives.
    displayAddress: (TypedAddress) -> String = { it.toString() },
    // The shared `reply_preview(thread_id)` record for `dm-reply-preview`, read by
    // the stateful Screen (an FFI call) and handed in, so this Content stays
    // FFI-free. New-thread compose has no reply, hence the default.
    replyPreview: ReplyPreview? = null,
) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 8.dp, vertical = 4.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        // Reply preview banner — visible only when replying to a message. What it
        // says is the shared `ConversationsManager::reply_preview` record (sender +
        // plain-text excerpt), rendered, never derived here; `null` while a reply
        // is armed means the answered message is outside the fetched window, and
        // the banner stays empty rather than guessing (conversations.md § Where
        // logic lives → *Reply preview*).
        if (compose.replyTo != null) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.AutoMirrored.Filled.Reply,
                    contentDescription = null,
                    modifier = Modifier.size(16.dp),
                )
                Spacer(Modifier.width(4.dp))
                Text(
                    replyPreview?.let { "${it.senderDisplay}: ${it.excerpt}" }.orEmpty(),
                    style = MaterialTheme.typography.labelMedium,
                    maxLines = 1,
                    modifier = Modifier
                        .weight(1f)
                        .testTag(Ids.DM_REPLY_PREVIEW),
                )
                IconButton(
                    onClick = onReplyCancel,
                    modifier = Modifier.size(28.dp).testTag(Ids.DM_REPLY_CANCEL),
                ) {
                    Icon(Icons.Default.Close, contentDescription = stringResource(R.string.common_cancel))
                }
            }
        }

        // Editable reply "To" line — mail only (gated on
        // supportsRecipientSelection, rule #5; FaunaMls's recipients are the
        // group, so it's hidden there). "To:" label | removable recipient chips
        // | add input. Removing a chip drops that recipient from THIS reply only
        // — thread history is untouched. Parsing the add input goes through the
        // FFI `tryParseTypedAddress` in the stateful Screen (this Content stays
        // FFI-free for Robolectric), so the bar just forwards the raw text.
        // conversations.md § Participants vs. reply recipients; mirrors linux
        // compose_bar.rs.
        if (capabilities?.supportsRecipientSelection == true) {
            var replyRecipientInput by remember { mutableStateOf("") }
            FlowRow(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Text(
                    stringResource(R.string.conversations_unified_to_line_label),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.align(Alignment.CenterVertically),
                )
                compose.replyRecipients.forEach { addr ->
                    InputChip(
                        selected = false,
                        onClick = {},
                        label = { Text(displayAddress(addr)) },
                        modifier = Modifier.testTag(Ids.DM_REPLY_RECIPIENT_CHIP),
                        trailingIcon = {
                            Icon(
                                Icons.Default.Close,
                                contentDescription = stringResource(R.string.common_remove),
                                modifier = Modifier
                                    .size(18.dp)
                                    .clickable { onRemoveReplyRecipient(addr) }
                                    .testTag(Ids.DM_REPLY_RECIPIENT_REMOVE),
                            )
                        },
                    )
                }
                OutlinedTextField(
                    value = replyRecipientInput,
                    onValueChange = { replyRecipientInput = it },
                    modifier = Modifier
                        .widthIn(min = 120.dp)
                        .testTag(Ids.DM_REPLY_RECIPIENT_ADD),
                    placeholder = {
                        Text(stringResource(R.string.conversations_unified_reply_recipient_add_placeholder))
                    },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                    keyboardActions = KeyboardActions(onDone = {
                        if (replyRecipientInput.isNotBlank()) {
                            onAddReplyRecipient(replyRecipientInput)
                            replyRecipientInput = ""
                        }
                    }),
                )
            }
        }

        // Staged-attachment chips — one per ComposeState.attachments entry
        // (indexed, per conversations.md rule #4). Informational filename +
        // size (byte_size, shared formatter — never a hand-rolled threshold
        // table); the trailing × unstages via onRemoveAttachment(index),
        // positional over the same list this loop enumerates so chip index
        // and mutator index cannot drift. Mirrors the reply-recipient chip
        // row above and apple's landed FaunaKit/Views/DmComposeBar.swift.
        if (compose.attachments.isNotEmpty()) {
            FlowRow(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                compose.attachments.forEachIndexed { i, att ->
                    InputChip(
                        selected = false,
                        onClick = {},
                        label = { Text("${att.filename} ${localized(byteSize(att.sizeBytes)).orEmpty()}") },
                        modifier = Modifier.testTag(Ids.DM_COMPOSE_ATTACHMENT_CHIP),
                        leadingIcon = {
                            Icon(
                                if (att.isImage) Icons.Default.Image else Icons.Default.Description,
                                contentDescription = null,
                                modifier = Modifier.size(18.dp),
                            )
                        },
                        trailingIcon = {
                            Icon(
                                Icons.Default.Close,
                                contentDescription = stringResource(R.string.common_remove),
                                modifier = Modifier
                                    .size(18.dp)
                                    .clickable { onRemoveAttachment(i) }
                                    .testTag(Ids.DM_COMPOSE_ATTACHMENT_REMOVE),
                            )
                        },
                    )
                }
            }
        }

        // Subject input — visible only when a topic is active (subject_draft
        // is Some). ui.yaml lists it as an optional element of dm-compose-bar.
        compose.subjectDraft?.let { subject ->
            OutlinedTextField(
                value = subject,
                onValueChange = onSubjectChange,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.SUBJECT_INPUT),
                placeholder = {
                    Text(stringResource(R.string.conversations_unified_topic_input_placeholder))
                },
                singleLine = true,
            )
        }

        // Body field. Held as a TextFieldValue so the markdown toolbar can wrap the
        // *selection* (the toolbar is selection-aware). The shared snapshot only
        // carries the plain `bodyDraft` text, so we keep a local TextFieldValue and
        // reconcile it whenever the snapshot text diverges from ours (e.g. the
        // manager clears the draft on send) — user edits flow out via onBodyChange.
        var bodyValue by remember { mutableStateOf(TextFieldValue(compose.bodyDraft)) }
        LaunchedEffect(compose.bodyDraft) {
            if (compose.bodyDraft != bodyValue.text) {
                bodyValue = TextFieldValue(compose.bodyDraft, TextRange(compose.bodyDraft.length))
            }
        }
        // Inline markdown styling (conversations.md § Compose-field inline markdown
        // styling): the buffer keeps literal markdown source; a VisualTransformation
        // styles content ranges using the shared `decoration_map` ranges. Gated on
        // supportsMarkdown to match the toolbar + render path (no styling on a
        // non-markdown rail).
        //
        // Per-editor marker-visibility toggle (`markdown-marker-toggle-button`,
        // docs/goal/ui/conversations.md § Compose-field inline markdown styling): default
        // HIDDEN (markersShown = false) uses `compose_decoration_plan` — inline
        // emphasis markers are concealed (caret-edge reveal keeps the run under
        // the caret editable); pressed reveals every marker dimmed via
        // `compose_show_markers_dim_ranges` (the prior all-dimmed live preview).
        // Client-local, no persistence. Both shared calls take a UTF-8 BYTE caret
        // offset — convert from the field's UTF-16 selection once per recomposition.
        val markdownEnabled = capabilities?.supportsMarkdown ?: true
        val linkColor = MaterialTheme.colorScheme.primary
        val markerColor = MaterialTheme.colorScheme.onSurfaceVariant
        val codeBackground = MaterialTheme.colorScheme.surfaceVariant
        var markersShown by remember { mutableStateOf(false) }
        val decorations = remember(bodyValue.text, markdownEnabled) {
            if (markdownEnabled) decorate(bodyValue.text) else emptyList()
        }
        val caretByte = remember(bodyValue.text, bodyValue.selection.start) {
            utf16IndexToUtf8Byte(bodyValue.text, bodyValue.selection.start)
        }
        val bodyTransformation = if (!markdownEnabled) {
            VisualTransformation.None
        } else if (markersShown) {
            val dimRanges = remember(bodyValue.text, caretByte) {
                composeShowMarkersDimRanges(bodyValue.text, caretByte)
            }
            MarkdownVisualTransformation(decorations, dimRanges, linkColor, markerColor, codeBackground)
        } else {
            val plan = remember(bodyValue.text, caretByte) {
                composeDecorationPlan(bodyValue.text, caretByte)
            }
            MarkdownHideVisualTransformation(decorations, plan, linkColor, markerColor, codeBackground)
        }
        // What the field applied is published for the e2e's `text-runs` read
        // (`ComposeFieldStyling`), and withdrawn when the field leaves the screen
        // so a read after the composer closed is a refusal, not a stale answer.
        DisposableEffect(Unit) { onDispose { ComposeFieldStyling.applied = null } }
        OutlinedTextField(
            value = bodyValue,
            onValueChange = { newValue ->
                val textChanged = newValue.text != bodyValue.text
                bodyValue = newValue
                if (textChanged) onBodyChange(newValue.text)
            },
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.DM_TEXT_FIELD),
            visualTransformation = RecordingVisualTransformation(bodyTransformation),
            minLines = 1,
            maxLines = 4,
        )

        // Markdown compose toolbar (html-mail.md § Composition; conversations.md
        // § compose toolbar). Wraps the selected body text with markdown markers.
        // Capability-gated (rule #5) — disabled, not hidden, on a non-markdown rail
        // (mirrors web/windows/macos/ios). The html-mail flip of
        // derive_capabilities(Smtp,_).supports_markdown=true lights it up on mail;
        // it's also enabled on markdown-capable FaunaMls. `caps == null`
        // (new-thread compose, rail not yet decided) leaves it enabled.
        MarkdownToolbar(
            value = bodyValue,
            onValueChange = { newValue ->
                bodyValue = newValue
                onBodyChange(newValue.text)
            },
            enabled = capabilities?.supportsMarkdown ?: true,
            wrap = wrap,
            markersShown = markersShown,
            onToggleMarkers = if (markdownEnabled) ({ markersShown = !markersShown }) else null,
        )

        // Bottom action row: topic toggle | attach | spacer | send.
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextButton(
                onClick = onTopicToggle,
                enabled = capabilities?.supportsSubject ?: true,
                modifier = Modifier.testTag(Ids.TOPIC_TOGGLE_BUTTON),
            ) {
                Text(stringResource(R.string.conversations_unified_topic_toggle_add))
            }
            IconButton(
                onClick = onAttach,
                enabled = capabilities?.supportsAttachments ?: true,
                modifier = Modifier.testTag(Ids.ATTACHMENT_BUTTON),
            ) {
                Icon(
                    Icons.Default.AttachFile,
                    contentDescription = stringResource(R.string.conversations_unified_attachment_button),
                )
            }
            Spacer(Modifier.weight(1f))
            // Drives the shared `manager.send` over whichever rails the session
            // registered — the real ones `ConversationsManagerHost.
            // startConversationsSession` builds at login (the mocks under plain
            // E2E). A refusal lands on the shared `send_state`, which the page's
            // `error-message` mirrors (conversations.md §"Per-rail backends").
            Button(
                onClick = onSend,
                enabled = compose.sendState !is SendState.Sending,
                modifier = Modifier.testTag(Ids.DM_SEND_BUTTON),
            ) {
                Icon(Icons.AutoMirrored.Filled.Send, contentDescription = null, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(4.dp))
                Text(stringResource(R.string.common_send))
            }
        }
    }
}
