package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.HexUtil
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.MemberReviewRow
import com.fauna.app.ui.viewmodel.MemberReviewVM
import social.fauna.generated.Ids

/**
 * The permanent **Members To Review** Settings sub-page (`ui.yaml` page
 * `member_review`; settings.md § Navigation model — rail row directly after
 * Account). Item (iv) of `succession-aftermath.md` § Propagation's two-surface
 * ruling: renders whatever a review sweep left unanswered, with **no sweep
 * gate** of its own, so a deferred backlog stays reachable after the ceremony
 * that raised it scrolls away. Stateless [MemberReviewContent] is split out so
 * it renders under the Compose test harness FFI-free, mirroring
 * [MutedWordsContent] / [EncryptionSettingsContent].
 */
@Composable
fun MemberReviewScreen(
    navController: NavController,
    vm: MemberReviewVM = hiltViewModel(),
) {
    val rows by vm.rows.collectAsState()
    val loaded by vm.loaded.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()

    MemberReviewContent(
        rows = rows,
        loaded = loaded,
        error = errorMessage,
        onBack = { navController.popBackStack() },
        onKeep = vm::keep,
        onRemove = vm::remove,
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MemberReviewContent(
    rows: List<MemberReviewRow>,
    /** Whether the read has returned — the second painting condition of
     *  `member-review-empty` (`docs/goal/ui/README.md` § *List pages: loading
     *  is not empty*): `rows` is empty both before the first read returns and
     *  after one that found nothing open. Defaults to `true` so a caller
     *  passing rows reads naturally; the real screen always passes the VM's
     *  bit. */
    loaded: Boolean = true,
    error: String?,
    onBack: () -> Unit,
    onKeep: (ByteArray) -> Unit,
    onRemove: (ByteArray) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.settings_member_review_page_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
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

            if (loaded && rows.isEmpty()) {
                Text(
                    stringResource(R.string.settings_member_review_page_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .padding(top = 12.dp)
                        .testTag(Ids.MEMBER_REVIEW_EMPTY),
                )
            } else if (rows.isNotEmpty()) {
                // The lead line — the one thing the ephemeral pass (not yet
                // built on android) never has to say: a user opening this
                // page weeks later has no ceremony around them to explain why
                // these names are here.
                Text(
                    stringResource(R.string.settings_member_review_page_intro),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(vertical = 12.dp),
                )

                LazyColumn {
                    items(rows, key = { HexUtil.bytesToHex(it.person) }) { row ->
                        MemberReviewRowItem(
                            row = row,
                            onKeep = { onKeep(row.person) },
                            onRemove = { onRemove(row.person) },
                        )
                        HorizontalDivider()
                    }
                }
            }
        }
    }
}

/** One `member-review-row` for [row], with `member-review-keep-button` /
 *  `member-review-remove-button` scoped **inside** it (the e2e scope
 *  convention — a driver acting on row `[i]` is always acting on the person
 *  row `[i]` names). `who`/each reason resolve through the shared
 *  [localized] seam: `text.who` is a raw handle string when one resolved
 *  ([uniffi.fauna_core.LocalizedText.key] holding it directly, so the lookup
 *  falls through to the verbatim key) or the shared "no longer in any of your
 *  groups" key otherwise — never re-derived here. */
@Composable
private fun MemberReviewRowItem(
    row: MemberReviewRow,
    onKeep: () -> Unit,
    onRemove: () -> Unit,
) {
    val who = localized(row.text.who) ?: stringResource(R.string.settings_recovery_kit_review_unknown_person)
    val reasonText = row.text.reasons.mapNotNull { localized(it) }.joinToString(", ")
    val rowText = stringResourceFmt(R.string.settings_recovery_kit_review_row, who, reasonText)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .testTag(Ids.MEMBER_REVIEW_ROW),
    ) {
        Text(rowText, style = MaterialTheme.typography.bodyMedium)
        Row(
            modifier = Modifier.padding(top = 8.dp),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Button(onClick = onKeep, modifier = Modifier.testTag(Ids.MEMBER_REVIEW_KEEP_BUTTON)) {
                Text(stringResource(R.string.settings_recovery_kit_review_keep))
            }
            OutlinedButton(onClick = onRemove, modifier = Modifier.testTag(Ids.MEMBER_REVIEW_REMOVE_BUTTON)) {
                Text(stringResource(R.string.settings_recovery_kit_review_remove))
            }
        }
    }
}
