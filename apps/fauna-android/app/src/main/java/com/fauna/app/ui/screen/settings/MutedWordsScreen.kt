package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.viewmodel.MutedWordsVM
import social.fauna.generated.Ids

/**
 * The **Muted words** Settings sub-page (`ui.yaml` page `muted-words`; settings.md
 * § Navigation model — placed after Privacy). Edits the single user-global muted-word
 * list: the terms are collapsed at render in the conversation view (moderation.md
 * § Muted keywords). Stateless [MutedWordsContent] is split out so it renders under the
 * Compose test harness FFI-free — the VM-bound [MutedWordsScreen] is the thin wrapper
 * the NavHost mounts (mirrors the sibling settings screens). Mirrors linux
 * `settings/muted_words.rs` + web's page.
 */
@Composable
fun MutedWordsScreen(
    navController: NavController,
    vm: MutedWordsVM = hiltViewModel(),
) {
    val page by vm.page.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val busy by vm.busy.collectAsState()

    // The picker's two options come from the shared catalog (`muted_keyword_levels`),
    // each token the level's own spelling; only the label is this app's.
    val hideLabel = stringResource(R.string.muted_words_level_hide)
    val showLessLabel = stringResource(R.string.muted_words_level_show_less)
    val levels = remember { com.fauna.ffi.mutedKeywordLevels() }
    val options = levels.map { level ->
        com.fauna.ffi.mutedKeywordLevelValue(level) to
            if (level == uniffi.fauna_core.MutedKeywordLevel.SHOW_LESS) showLessLabel else hideLabel
    }

    MutedWordsContent(
        rows = page.keywords.map {
            MutedWordRowState(
                term = it.keyword,
                level = com.fauna.ffi.mutedKeywordLevelValue(com.fauna.ffi.mutedKeywordLevel(it.weight)),
            )
        },
        levelOptions = options,
        loaded = page.loaded,
        busy = busy,
        error = errorMessage,
        onBack = { navController.popBackStack() },
        onAdd = vm::add,
        onRemove = vm::remove,
        onSetLevel = { term, token ->
            com.fauna.ffi.mutedKeywordLevelFromValue(token)?.let { vm.setLevel(term, it) }
        },
    )
}

/** One `muted-word-item` row's state: the term and its level's token
 *  (`hide` | `show-less`, `fauna_core::scoring::MutedKeywordLevel::value`). */
data class MutedWordRowState(val term: String, val level: String)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MutedWordsContent(
    rows: List<MutedWordRowState>,
    /** The level picker's `(token, label)` options in paint order. */
    levelOptions: List<Pair<String, String>>,
    /**
     * Whether the read has returned — the second painting condition of
     * `muted-word-empty` (`docs/goal/ui/README.md` § *List pages: loading is not
     * empty*). `rows` is empty both before the first read returns and after one
     * that found nothing, so gating on its size alone announces "you haven't
     * muted any words yet" over a list nobody has read. Defaults to `true` so a
     * caller passing rows reads naturally; the real screen always passes the
     * shared record's bit.
     */
    loaded: Boolean = true,
    /** A mutation's round trip is in flight — the add button, every remove
     *  button and every level picker wait for the returned record. */
    busy: Boolean = false,
    error: String?,
    onBack: () -> Unit,
    onAdd: (String) -> Unit,
    onRemove: (String) -> Unit,
    onSetLevel: (term: String, token: String) -> Unit,
) {
    var input by remember { mutableStateOf("") }

    fun submit() {
        if (input.isNotBlank()) {
            onAdd(input)
            input = ""
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.muted_words_title),
                        modifier = Modifier.testTag(Ids.MUTED_WORDS),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp),
        ) {
            Text(
                stringResource(R.string.muted_words_description),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 12.dp),
            )

            // Add a term: input + add button (the whole-list set normalizes shared-side).
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = input,
                    onValueChange = { input = it },
                    singleLine = true,
                    placeholder = { Text(stringResource(R.string.muted_words_input_placeholder)) },
                    keyboardOptions = androidx.compose.foundation.text.KeyboardOptions(
                        imeAction = ImeAction.Done,
                    ),
                    keyboardActions = androidx.compose.foundation.text.KeyboardActions(
                        onDone = { submit() },
                    ),
                    modifier = Modifier
                        .weight(1f)
                        .testTag(Ids.MUTED_WORD_INPUT),
                )
                Spacer(Modifier.width(8.dp))
                Button(
                    onClick = { submit() },
                    enabled = !busy,
                    modifier = Modifier.testTag(Ids.MUTED_WORD_ADD_BUTTON),
                ) {
                    Text(stringResource(R.string.muted_words_add))
                }
            }

            error?.let {
                Text(
                    it,
                    color = MaterialTheme.colorScheme.error,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier
                        .padding(top = 8.dp)
                        .testTag(Ids.ERROR_MESSAGE),
                )
            }

            Spacer(Modifier.height(12.dp))

            if (loaded && rows.isEmpty()) {
                Text(
                    stringResource(R.string.muted_words_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.MUTED_WORD_EMPTY),
                )
            } else {
                LazyColumn(modifier = Modifier.testTag(Ids.MUTED_WORD_LIST)) {
                    items(rows, key = { it.term }) { row ->
                        MutedWordRow(
                            row = row,
                            levelOptions = levelOptions,
                            enabled = !busy,
                            onSetLevel = { onSetLevel(row.term, it) },
                            onRemove = { onRemove(row.term) },
                        )
                        HorizontalDivider()
                    }
                }
            }
        }
    }
}

/** One `muted-word-item` row: the term ([muted-word-text]), its level picker
 *  ([muted-word-level-select]) + a remove button ([muted-word-remove-button]). */
@Composable
private fun MutedWordRow(
    row: MutedWordRowState,
    levelOptions: List<Pair<String, String>>,
    enabled: Boolean,
    onSetLevel: (String) -> Unit,
    onRemove: () -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp)
            .testTag(Ids.MUTED_WORD_ITEM),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            row.term,
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier
                .weight(1f)
                .testTag(Ids.MUTED_WORD_TEXT),
        )
        TokenSelect(
            testTagValue = Ids.MUTED_WORD_LEVEL_SELECT,
            selected = row.level,
            options = levelOptions,
            onSelect = onSetLevel,
            enabled = enabled,
        )
        IconButton(
            onClick = onRemove,
            enabled = enabled,
            modifier = Modifier.testTag(Ids.MUTED_WORD_REMOVE_BUTTON),
        ) {
            Icon(
                Icons.Default.Delete,
                contentDescription = stringResource(R.string.muted_words_remove),
            )
        }
    }
}
