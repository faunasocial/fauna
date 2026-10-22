package com.fauna.app.ui.screen.conversations

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.util.ValueFormat
import social.fauna.generated.Ids
import uniffi.fauna_conversations.RoomSearchNav
import uniffi.fauna_conversations.RoomSearchResultKind
import uniffi.fauna_conversations.RoomSearchResultSnapshot
import uniffi.fauna_conversations.RoomSearchSnapshot
import uniffi.fauna_conversations.RoomSearchStatus

/**
 * The community room's search strip (`docs/goal/behavior/room-search.md` § The
 * surface): the field and its submit, painted while the strip is open on the
 * thread (`ThreadDetail.room.search` is non-null). Submit-driven — typing alone
 * asks nothing — and the draft is local view state that goes with the strip when
 * it closes. The query, the leg, the rows and every word on them are the shared
 * manager's (`room_search`); this paints them and hands the typed query back.
 */
@Composable
internal fun RoomSearchStrip(onSubmit: (String) -> Unit) {
    var draft by remember { mutableStateOf("") }
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        OutlinedTextField(
            value = draft,
            onValueChange = { draft = it },
            singleLine = true,
            placeholder = { Text(stringResource(R.string.conversations_unified_room_search_placeholder)) },
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { onSubmit(draft) }),
            modifier = Modifier.weight(1f).testTag(Ids.ROOM_SEARCH_FIELD),
        )
        IconButton(
            onClick = { onSubmit(draft) },
            modifier = Modifier.testTag(Ids.ROOM_SEARCH_SUBMIT_BUTTON),
        ) {
            Icon(Icons.Default.Search, contentDescription = stringResource(R.string.common_search))
        }
    }
}

/** Whether a submitted query's rows take the message stream's place
 *  (`room-search.md` § The surface): from the moment one is out until the strip
 *  closes. An open strip with nothing submitted leaves the stream where it is. */
internal fun roomSearchReplacesStream(search: RoomSearchSnapshot?): Boolean =
    search != null && search.status != RoomSearchStatus.IDLE

/**
 * What the submitted query painted, in place of the message stream: one row per
 * hit in the order served, or the empty state once the home nest answered. In
 * flight and failed paint no result region at all — a driver waits on a row, the
 * empty state or `error-message` (a refusal rides the page's error), never on a
 * "searching" element.
 *
 * A row's words are the shared snapshot's: sender, time and the opened first line
 * (or the shared *not on this device yet* copy), joined as the other apps join
 * them. The `kind` attribute (`message | post`) is for drivers only and rides
 * `stateDescription`, this app's carrier for a string attribute.
 */
@Composable
internal fun RoomSearchResults(
    search: RoomSearchSnapshot,
    onOpenHit: (RoomSearchNav) -> Unit,
) {
    if (search.status != RoomSearchStatus.DONE) return
    if (search.results.isEmpty()) {
        Column(modifier = Modifier.fillMaxSize().padding(16.dp)) {
            Text(
                stringResource(R.string.conversations_unified_room_search_no_results),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.ROOM_SEARCH_NO_RESULTS),
            )
        }
        return
    }
    val context = LocalContext.current
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        itemsIndexed(search.results) { _, row ->
            val text = roomSearchRowText(row) { ValueFormat.conversationTimestamp(context, it) }
            Text(
                text,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable { onOpenHit(row.navigation) }
                    .padding(horizontal = 16.dp, vertical = 10.dp)
                    .testTag(Ids.ROOM_SEARCH_RESULT_ITEM)
                    .semantics { stateDescription = roomSearchKindToken(row.kind) },
            )
        }
    }
}

/** A row's visible words — sender, time, first line — joined with a middle dot,
 *  the parts the snapshot has (`room-search.md` § The surface → *What a hit row
 *  paints*). */
internal fun roomSearchRowText(
    row: RoomSearchResultSnapshot,
    timestamp: (Long) -> String,
): String = listOfNotNull(
    row.senderDisplay,
    row.sentAtMs?.let(timestamp),
    row.text.takeIf { it.isNotEmpty() },
).joinToString(" · ")

/** The row's `kind` attribute token, the one `RoomSearchResultKind::attr_token`
 *  spells (`message | post`) — a driver-facing label, not a decision. */
internal fun roomSearchKindToken(kind: RoomSearchResultKind): String = when (kind) {
    RoomSearchResultKind.MESSAGE -> "message"
    RoomSearchResultKind.POST -> "post"
}
