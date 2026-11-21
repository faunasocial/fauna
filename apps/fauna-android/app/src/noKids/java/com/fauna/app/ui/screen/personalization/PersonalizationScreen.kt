package com.fauna.app.ui.screen.personalization

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Card
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.components.LabelerCatalogItemRow
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.isShown
import uniffi.fauna_core.Possibility
import com.fauna.app.ui.components.TokenSelect
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.FaunaFormVerdict
import com.fauna.app.ui.util.askForm
import com.fauna.app.ui.util.faunaFormGate
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.LabelerCatalogVM
import com.fauna.app.ui.viewmodel.PublishSheetUiState
import com.fauna.app.ui.viewmodel.SignalShareVM
import com.fauna.app.ui.viewmodel.TrainedTopicsVM
import com.fauna.ffi.FfiReportShareEntry
import com.fauna.ffi.FfiTrainedTopicRow
import com.fauna.ffi.publishKindOptions
import com.fauna.ffi.trainedFactorNamePossibility
import com.fauna.ffi.trainedFactorPublishOpenPossibility
import com.fauna.ffi.trainedFactorPublishSubmitPossibility
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogEntry
import uniffi.fauna_labeler_catalog_machine.LabelerCatalogSnapshot
import social.fauna.generated.Ids
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.OutlinedButton
import com.fauna.app.ui.controls.OutlinedTextField
import com.fauna.app.ui.controls.TextButton

/**
 * The **Personalization** home (`ui.yaml` page `personalization`;
 * `content-moderation-and-ranking.md` § Composition — the unified all-6-client
 * authoring surface for the user's single tier-1 ruleset, ratified 2026-07-06).
 * Hubs three facets WITHOUT rebuilding them: **Feeds** (a link out to the feed
 * page's create-feed dialog), **Muted words** (a link re-homing the
 * already-shipped `settings/muted-words` sub-page), and **Community labelers**
 * (the caller's SUBSCRIBED tier-3 labelers, each unsubscribable inline, with a
 * link out to the full `settings/labeler-catalog` browse/inspect/subscribe
 * page). Backed by the shared [LabelerCatalogVM] (mirrors
 * `apps/fauna-linux/src/views/personalization/mod.rs`'s `HomeShell`). Stateless
 * [PersonalizationContent] is split out so it renders under the Compose test
 * harness FFI-free.
 */
@Composable
fun PersonalizationScreen(
    navController: NavController,
    vm: LabelerCatalogVM = hiltViewModel(),
    topicsVm: TrainedTopicsVM = hiltViewModel(),
    signalShareVm: SignalShareVM = hiltViewModel(),
) {
    val snapshot by vm.snapshot.collectAsState()
    val topics by topicsVm.topics.collectAsState()
    val topicName by topicsVm.topicName.collectAsState()
    val isRenaming by topicsVm.isRenaming.collectAsState()
    val topicsBusy by topicsVm.busy.collectAsState()
    val publishState by topicsVm.publish.collectAsState()
    val topicsError by topicsVm.errorMessage.collectAsState()
    val shareSignals by signalShareVm.share.collectAsState()
    val signalPublished by signalShareVm.published.collectAsState()
    val signalShareError by signalShareVm.errorMessage.collectAsState()
    val shareAnswer by signalShareVm.shareAnswer.collectAsState()
    val clearAnswer by topicsVm.clearAnswer.collectAsState()
    val appMessages = LocalAppMessages.current
    val context = LocalContext.current
    // The kind picker's options — the shared `fauna_core::format::publish_kind_options`
    // catalog, List first, so the option a user picks and the words they read
    // back can never drift (mirrors `BackupDestinationsSection.kindOptions`).
    // `remember` because the catalog is a constant of the build, not of the render.
    val publishKindChoices = remember(context) {
        publishKindOptions().map { it.value to (resolveLocalized(context, it.label) ?: it.value) }
    }

    LaunchedEffect(Unit) {
        vm.start()
        topicsVm.load()
        // Layer-B signal-sharing (engagement-cues.md § Layer B): re-hydrate on
        // every entry to this route (the android idiom for "page visible" —
        // mirrors linux's `connect_map` refresh), since the opt-in + published
        // list change out-of-band.
        signalShareVm.refresh()
    }

    val errorText = localized(snapshot?.error)
    LaunchedEffect(errorText) {
        if (errorText != null) appMessages.showError(errorText)
    }
    LaunchedEffect(topicsError) {
        if (topicsError != null) appMessages.showError(topicsError!!)
    }
    LaunchedEffect(signalShareError) {
        if (signalShareError != null) appMessages.showError(signalShareError!!)
    }

    PersonalizationContent(
        snapshot = snapshot,
        topics = topics,
        topicName = topicName,
        isRenaming = isRenaming,
        topicsBusy = topicsBusy,
        publishState = publishState,
        publishManagerReady = topicsVm.managerReady(),
        shareSignals = shareSignals,
        signalPublished = signalPublished,
        onBack = { navController.popBackStack() },
        onFeedsLink = { navController.navigate("feed") },
        onMutedWordsLink = { navController.navigate("settings/muted-words") },
        onBrowseCatalog = { navController.navigate("settings/labeler-catalog") },
        onUnsubscribe = vm::unsubscribe,
        onTopicNameChange = topicsVm::onTopicNameChange,
        onSubmitTopic = topicsVm::submitTopic,
        onStartRename = topicsVm::startRename,
        onDeleteTopic = topicsVm::deleteTopic,
        onToggleEngagement = topicsVm::toggleEngagement,
        onOpenPublish = topicsVm::openPublishSheet,
        onPublishNameChange = topicsVm::onPublishNameChange,
        onTogglePublishInclude = topicsVm::togglePublishExemplarIncluded,
        publishKindOptions = publishKindChoices,
        onPublishKindChange = topicsVm::setPublishKind,
        onTogglePublishNgramInclude = topicsVm::togglePublishNgramIncluded,
        onSubmitPublish = topicsVm::submitPublish,
        onCancelPublish = topicsVm::cancelPublish,
        // The VM writes the flip of the nest-confirmed value, never the
        // switch's painted one (engagement-cues.md § Layer B).
        onToggleShareSignals = { signalShareVm.toggleShare() },
        onClearEngagementData = topicsVm::clearEngagementData,
        shareSignalsAnswer = shareAnswer,
        clearEngagementAnswer = clearAnswer,
        kindBadge = { entry -> kindBadgeText(context, entry) },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PersonalizationContent(
    snapshot: LabelerCatalogSnapshot?,
    topics: List<FfiTrainedTopicRow> = emptyList(),
    topicName: String = "",
    isRenaming: Boolean = false,
    topicsBusy: Boolean = false,
    publishState: PublishSheetUiState? = null,
    /** Whether the live feed manager is up — a row's Publish… answer's input
     *  (the press asks again in the VM). */
    publishManagerReady: Boolean = true,
    shareSignals: Boolean = false,
    signalPublished: List<FfiReportShareEntry> = emptyList(),
    onBack: () -> Unit,
    onFeedsLink: () -> Unit,
    onMutedWordsLink: () -> Unit,
    onBrowseCatalog: () -> Unit,
    onUnsubscribe: (Int) -> Unit,
    onTopicNameChange: (String) -> Unit = {},
    onSubmitTopic: () -> Unit = {},
    onStartRename: (FfiTrainedTopicRow) -> Unit = {},
    onDeleteTopic: (FfiTrainedTopicRow) -> Unit = {},
    onToggleEngagement: (FfiTrainedTopicRow) -> Unit = {},
    onOpenPublish: (FfiTrainedTopicRow) -> Unit = {},
    onPublishNameChange: (String) -> Unit = {},
    onTogglePublishInclude: (Int) -> Unit = {},
    // The kind select's `(token, label)` catalog — resolved by the screen off
    // the shared face, so this content stays FFI-free under the test harness.
    publishKindOptions: List<Pair<String, String>> = emptyList(),
    onPublishKindChange: (String) -> Unit = {},
    onTogglePublishNgramInclude: (Int) -> Unit = {},
    onSubmitPublish: () -> Unit = {},
    onCancelPublish: () -> Unit = {},
    onToggleShareSignals: (Boolean) -> Unit = {},
    onClearEngagementData: () -> Unit = {},
    // The two engagement-cue controls' answers (`SignalShareVM.shareAnswer`,
    // `TrainedTopicsVM.clearAnswer`); the defaults are for the stateless
    // harness only.
    shareSignalsAnswer: Possibility = Possibility.Possible,
    clearEngagementAnswer: Possibility = Possibility.Possible,
    kindBadge: (LabelerCatalogEntry) -> String = { it.artifactKind },
) {
    val subscribed = snapshot?.entries?.withIndex()?.filter { it.value.subscribed } ?: emptyList()

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.personalization_title),
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
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState())
                .testTag("personalization"),
        ) {
            Button(
                onClick = onFeedsLink,
                modifier = Modifier
                    .testTag(Ids.PERSONALIZATION_FEEDS_LINK)
                    .padding(top = 12.dp),
            ) {
                Text(stringResource(R.string.personalization_feeds_link))
            }
            Button(
                onClick = onMutedWordsLink,
                modifier = Modifier
                    .testTag(Ids.PERSONALIZATION_MUTED_WORDS_LINK)
                    .padding(top = 8.dp),
            ) {
                Text(stringResource(R.string.personalization_muted_words_link))
            }

            // ── Trained topics facet (topic-factors.md § Authoring surface &
            // picker, S8) — one name-input, two flows: the input + create-button
            // mint a topic inline; a row's rename-button retargets the SAME input
            // at that row (prefilled, label flips to "Save name"); any successful
            // commit resets to create mode. Mirrors linux `trained_topics.rs` /
            // web `PersonalizationSection.svelte`.
            Text(
                stringResource(R.string.personalization_trained_topics_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(top = 20.dp, bottom = 8.dp),
            )
            // The create / save-name answer — the shared
            // `drafts::factor_name_possibility` (`docs/goal/ui/settings.md`
            // § Errors & edge cases); the VM's commit asks it again.
            val nameGate = faunaFormGate(askForm { trainedFactorNamePossibility(topicName, topicsBusy) })
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(bottom = 8.dp),
            ) {
                OutlinedTextField(
                    value = topicName,
                    onValueChange = onTopicNameChange,
                    placeholder = { Text(stringResource(R.string.personalization_trained_factor_placeholder)) },
                    singleLine = true,
                    modifier = Modifier
                        .weight(1f)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_NAME_INPUT),
                )
                Button(
                    onClick = onSubmitTopic,
                    enabled = nameGate.enabled,
                    modifier = Modifier
                        .padding(start = 8.dp)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_CREATE_BUTTON),
                ) {
                    Text(
                        stringResource(
                            if (isRenaming) R.string.personalization_trained_factor_save
                            else R.string.personalization_trained_factor_create,
                        ),
                    )
                }
            }
            DisabledControlReasonText(nameGate.reason)
            if (topics.isEmpty()) {
                Text(
                    stringResource(R.string.personalization_trained_topics_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Column(modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_LIST)) {
                topics.forEach { topic ->
                    TrainedTopicRowItem(
                        topic = topic,
                        publishGate = faunaFormGate(
                            askForm {
                                trainedFactorPublishOpenPossibility(
                                    topic.factorKey != null,
                                    publishManagerReady,
                                    topicsBusy,
                                )
                            },
                        ),
                        onStartRename = onStartRename,
                        onDelete = onDeleteTopic,
                        onToggleEngagement = onToggleEngagement,
                        onOpenPublish = onOpenPublish,
                    )
                }
            }

            // Publish review-prune sheet (topic-factors.md § Publishing a
            // trained factor) — single-instance, revealed in place directly
            // under the Trained-topics facet, pre-targeted at the row whose
            // Publish… button opened it (the admin-dns-rename-sheet shape, not
            // a dialog). Mirrors linux `publish_sheet.rs`.
            if (publishState != null) {
                PublishSheetContent(
                    state = publishState,
                    kindOptions = publishKindOptions,
                    onKindChange = onPublishKindChange,
                    onNameChange = onPublishNameChange,
                    onToggleInclude = onTogglePublishInclude,
                    onToggleNgramInclude = onTogglePublishNgramInclude,
                    onSubmit = onSubmitPublish,
                    onCancel = onCancelPublish,
                )
            }

            Text(
                stringResource(R.string.labeler_catalog_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(top = 20.dp, bottom = 8.dp),
            )

            // The subscribed facet is DERIVED from the catalog snapshot, so it
            // inherits that snapshot's `loaded` bit rather than inventing one
            // (README.md § List pages: loading is not empty, rule 4). Same
            // three-state contract as the catalog page: while unloaded compose
            // NEITHER the empty text nor the (then-empty) list container.
            val labelersLoaded = snapshot?.loaded == true
            if (labelersLoaded && subscribed.isEmpty()) {
                Text(
                    stringResource(R.string.personalization_labelers_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.PERSONALIZATION_LABELERS_EMPTY),
                )
            } else if (labelersLoaded) {
                Column(modifier = Modifier.testTag(Ids.PERSONALIZATION_LABELERS_LIST)) {
                    subscribed.forEach { (index, entry) ->
                        LabelerCatalogItemRow(
                            entry = entry,
                            index = index,
                            showInspectSubscribe = false,
                            onInspect = {},
                            onSubscribe = {},
                            onUnsubscribe = onUnsubscribe,
                            kindBadge = kindBadge,
                        )
                    }
                }
            }

            OutlinedButton(
                onClick = onBrowseCatalog,
                modifier = Modifier
                    .testTag(Ids.PERSONALIZATION_BROWSE_CATALOG_BUTTON)
                    .padding(top = 12.dp),
            ) {
                Text(stringResource(R.string.personalization_browse_catalog))
            }

            // "Clear activity data" (engagement-cues.md § At rest) — delete the
            // sealed cues:v1 rollup from the user's own nest + reset the live
            // engine. The user-revocable affordance the capture invariant
            // requires: this client captures engagement cues, so the user must
            // be able to erase them from their own client. Sibling of the
            // per-row "Learn from my activity" toggle (that stops future
            // learning; this erases what was captured).
            // Painted from its shared answer: not live while the feed manager
            // is down, with why beside it (engagement-cues.md § Layer B).
            if (clearEngagementAnswer.isShown) {
                TextButton(
                    onClick = onClearEngagementData,
                    enabled = clearEngagementAnswer.isPossible,
                    modifier = Modifier
                        .padding(top = 8.dp)
                        .testTag(Ids.PERSONALIZATION_CLEAR_ENGAGEMENT_DATA_BUTTON),
                ) {
                    Text(stringResource(R.string.personalization_clear_engagement_data))
                }
                DisabledControlReasonText(localized(clearEngagementAnswer.blockedReason))
            }

            // ── Layer-B signal-sharing pane (engagement-cues.md § Layer B):
            // opt-in toggle + "what this nest publishes" transparency list.
            // Renders straight from the SignalShareVM's nest-confirmed state —
            // never optimistically (opting out withdraws this actor's
            // contributed rows, so the published list can shrink on the same
            // reply). Mirrors linux `wire_signal_sharing`/`render_signal_share`
            // (`apps/fauna-linux/src/views/personalization/mod.rs`). A plain
            // Column, not LazyColumn — this whole page already scrolls via the
            // outer Column's .verticalScroll, and nesting a LazyColumn inside a
            // scrolling Column crashes/misbehaves (the same reason
            // personalization-labelers-list and personalization-trained-factor-list
            // are plain Columns).
            Text(
                stringResource(R.string.personalization_share_signals_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(top = 20.dp, bottom = 8.dp),
            )
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Column(modifier = Modifier.weight(1f)) {
                    Text(
                        stringResource(R.string.personalization_share_signals_label),
                        style = MaterialTheme.typography.bodyLarge,
                    )
                    Text(
                        stringResource(R.string.personalization_share_signals_subtitle),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                // Not live before the status read resolves, nor while the
                // feed manager is down — an unread `false` is never flipped
                // into a guessed `true` (engagement-cues.md § Layer B).
                if (shareSignalsAnswer.isShown) {
                    Switch(
                        checked = shareSignals,
                        onCheckedChange = onToggleShareSignals,
                        enabled = shareSignalsAnswer.isPossible,
                        modifier = Modifier.testTag(Ids.PERSONALIZATION_SHARE_SIGNALS_TOGGLE),
                    )
                }
            }
            DisabledControlReasonText(localized(shareSignalsAnswer.blockedReason))

            Text(
                stringResource(R.string.personalization_signal_published_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(top = 16.dp, bottom = 4.dp),
            )
            Text(
                stringResource(R.string.personalization_signal_published_description),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (signalPublished.isEmpty()) {
                Text(
                    stringResource(R.string.personalization_signal_published_empty),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Column(modifier = Modifier.testTag(Ids.SIGNAL_SHARE_PUBLISHED_LIST)) {
                signalPublished.forEach { entry -> SignalSharePublishedRow(entry) }
            }
        }
    }
}

/** One `signal-share-published-list-item` row — a published ≥k signal/report
 *  aggregate (the export view is nest-wide: `signal:*` and `report:*` alike).
 *  Pure transparency, read-only, no action. Mirrors android's own
 *  `ReportSharePublishedRow` (`ui/screen/settings/MailSpamScreen.kt`) and
 *  linux's `build_signal_published_row`. */
@Composable
private fun SignalSharePublishedRow(entry: FfiReportShareEntry) {
    val contributors = stringResource(R.string.personalization_signal_published_contributors)
    Card(modifier = Modifier.fillMaxWidth().testTag(Ids.SIGNAL_SHARE_PUBLISHED_LIST_ITEM)) {
        Row(
            modifier = Modifier.padding(16.dp).fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Column(modifier = Modifier.weight(1f)) {
                // The count element carries the BARE number — the word rides an
                // adjacent Text, so the row still reads "3 contributors" while
                // `signal-share-published-list-item-count` stays the machine-
                // readable value the cross-app contract specifies (ui.yaml:
                // "the distinct local contributor count"; linux puts the count
                // in a value_marker and the phrase in the row title; the e2e
                // asserts the element's text == "3"). Folding the word into the
                // element would make android the one app whose count reads
                // "3 contributors".
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text(
                        entry.count.toString(),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.testTag(Ids.SIGNAL_SHARE_PUBLISHED_LIST_ITEM_COUNT),
                    )
                    Text(contributors, style = MaterialTheme.typography.bodyMedium)
                }
                Text(
                    entry.factor,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.SIGNAL_SHARE_PUBLISHED_LIST_ITEM_FACTOR),
                )
                Text(
                    entry.contentHash,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.SIGNAL_SHARE_PUBLISHED_LIST_ITEM_HASH),
                )
            }
        }
    }
}

/**
 * One `personalization-trained-factor-item` row: name + example count +
 * engagement-toggle (Layer-A "Learn from my activity") + rename/publish/delete.
 * Mirrors linux `build_row`.
 */
@Composable
private fun TrainedTopicRowItem(
    topic: FfiTrainedTopicRow,
    /** The row's Publish… answer: absent with no addressable model, blocked
     *  while the feed manager is not up (`drafts::publish_open_possibility`). */
    publishGate: FaunaFormVerdict,
    onStartRename: (FfiTrainedTopicRow) -> Unit,
    onDelete: (FfiTrainedTopicRow) -> Unit,
    onToggleEngagement: (FfiTrainedTopicRow) -> Unit,
    onOpenPublish: (FfiTrainedTopicRow) -> Unit,
) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 6.dp)
            .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_ITEM)
            // The `factor` test-attr (key hex only, one bare value so no pack):
            // the e2e `topic_factor_key()` read; a row with no key publishes none.
            .then(
                topic.factorKey?.let { key ->
                    Modifier.semantics { stateDescription = key.removePrefix("topic:") }
                } ?: Modifier,
            ),
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(topic.name, modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_NAME))
            Text(
                stringResource(R.string.personalization_trained_factor_examples)
                    .replace("{count}", topic.exampleCount.toString()),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_EXAMPLE_COUNT),
            )
        }
        Switch(
            checked = topic.learnFromEngagement,
            onCheckedChange = { onToggleEngagement(topic) },
            modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_ENGAGEMENT_TOGGLE),
        )
        OutlinedButton(
            onClick = { onStartRename(topic) },
            modifier = Modifier
                .padding(start = 4.dp)
                .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_RENAME_BUTTON),
        ) {
            Text(stringResource(R.string.personalization_trained_factor_rename))
        }
        // A corrupt (non-16-byte) id has no addressable model — nothing to
        // score, nothing to publish — so the row offers no publish (absent);
        // without the live feed manager it is disabled with its reason, so no
        // sheet opens onto nothing.
        if (publishGate.shown) {
            Column {
                OutlinedButton(
                    onClick = { onOpenPublish(topic) },
                    enabled = publishGate.enabled,
                    modifier = Modifier
                        .padding(start = 4.dp)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_BUTTON),
                ) {
                    Text(stringResource(R.string.personalization_trained_factor_publish))
                }
                DisabledControlReasonText(publishGate.reason)
            }
        }
        OutlinedButton(
            onClick = { onDelete(topic) },
            modifier = Modifier
                .padding(start = 4.dp)
                .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_DELETE_BUTTON),
        ) {
            Text(stringResource(R.string.common_delete))
        }
    }
}

/**
 * The publish review-prune sheet's content — every row arrives CHECKED
 * (the `restore-kind-checkbox` prune shape: the user edits the factor's own
 * proposal rather than assembling one). Mirrors linux `publish_sheet.rs`.
 *
 * The kind select swaps the whole review body (topic-factors.md § Publishing
 * a trained factor, v2): the List reviews the scored top-N exemplars, the
 * Model reviews **every** surviving n-gram — where the List's rows bound
 * endorsement of already-public ids, these rows ARE the disclosure, so there
 * is no top-N. The select is a raw-value picker (`list` | `text-model`)
 * through the shared [TokenSelect]: the human reads the label, the driver
 * the token.
 */
@Composable
private fun PublishSheetContent(
    state: PublishSheetUiState,
    kindOptions: List<Pair<String, String>>,
    onKindChange: (String) -> Unit,
    onNameChange: (String) -> Unit,
    onToggleInclude: (Int) -> Unit,
    onToggleNgramInclude: (Int) -> Unit,
    onSubmit: () -> Unit,
    onCancel: () -> Unit,
) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SHEET),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(
                stringResource(R.string.personalization_publish_sheet_title),
                style = MaterialTheme.typography.titleSmall,
            )
            Text(
                stringResource(R.string.personalization_publish_kind_label),
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.padding(top = 4.dp),
            )
            TokenSelect(
                testTagValue = Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_KIND_SELECT,
                selected = state.kind,
                options = kindOptions,
                onSelect = onKindChange,
            )
            // The mandated per-kind copy (§ Publishing owns both sets) — the
            // Model's three disclosures plus the corpus-size line whose two
            // numbers make the rebuild's drops visible.
            Text(
                if (state.isModelKind) {
                    stringResource(R.string.personalization_publish_limitation_note_model) + "\n" +
                        stringResource(R.string.personalization_publish_corpus_size)
                            .replace("{included}", state.includedExamples.toString())
                            .replace("{marked}", state.markedExamples.toString())
                } else {
                    stringResource(R.string.personalization_publish_limitation_note)
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .padding(vertical = 4.dp)
                    .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_LIMITATION_NOTE),
            )
            Text(
                stringResource(
                    if (state.isModelKind) R.string.personalization_publish_name_label_model
                    else R.string.personalization_publish_name_label,
                ),
                style = MaterialTheme.typography.bodySmall,
            )
            // Starts blank on every open: the sealed registry name is PRIVATE
            // (§ Publishing), so prefilling it would leak the user's own label
            // into a public artifact by default. The keyboard's Done is the
            // sheet's submit: the same press as Publish, which the VM refuses
            // with its reason while the draft cannot succeed
            // (ui/settings.md § Errors & edge cases).
            OutlinedTextField(
                value = state.name,
                onValueChange = onNameChange,
                placeholder = { Text(stringResource(R.string.personalization_publish_name_placeholder)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Done),
                keyboardActions = KeyboardActions(onDone = { onSubmit() }),
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NAME_INPUT),
            )
            if (state.isModelKind) {
                PublishNgramReview(state = state, onToggleInclude = onToggleNgramInclude)
            } else {
                PublishExemplarReview(state = state, onToggleInclude = onToggleInclude)
            }
            // The shared `drafts::publish_submit_possibility`: a public name
            // within the bound, at least one kept row of the active kind ("the
            // corpus scored nothing" and "the user unchecked everything" alike)
            // and no publish in flight — disabled with its reason, never a
            // click that does nothing; the VM's submit asks it again.
            val submitGate = faunaFormGate(
                askForm { trainedFactorPublishSubmitPossibility(state.name, state.hasIncluded, state.busy) },
            )
            Row(modifier = Modifier.padding(top = 8.dp)) {
                Button(
                    onClick = onSubmit,
                    enabled = submitGate.enabled,
                    modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_SUBMIT_BUTTON),
                ) {
                    Text(stringResource(R.string.personalization_publish_submit))
                }
                OutlinedButton(
                    onClick = onCancel,
                    modifier = Modifier
                        .padding(start = 8.dp)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_CANCEL_BUTTON),
                ) {
                    Text(stringResource(R.string.common_cancel))
                }
            }
            DisabledControlReasonText(submitGate.reason)
        }
    }
}

/** The List kind's review body — the scored top-N exemplars. */
@Composable
private fun PublishExemplarReview(state: PublishSheetUiState, onToggleInclude: (Int) -> Unit) {
    Text(
        stringResource(R.string.personalization_publish_exemplars_title),
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.padding(top = 8.dp),
    )
    if (state.exemplars.isEmpty()) {
        Text(
            stringResource(R.string.personalization_publish_exemplars_empty),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_EMPTY),
        )
    }
    Column(modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_LIST)) {
        state.exemplars.forEachIndexed { index, exemplar ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_ITEM),
            ) {
                Checkbox(
                    checked = exemplar.included,
                    onCheckedChange = { onToggleInclude(index) },
                    modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_CHECKBOX),
                )
                Text(
                    exemplar.preview,
                    maxLines = 1,
                    modifier = Modifier
                        .weight(1f)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_TEXT),
                )
                Text(
                    stringResource(R.string.personalization_publish_score)
                        .replace("{score}", exemplar.score.toString()),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_EXEMPLAR_SCORE),
                )
            }
        }
    }
}

/**
 * The Model kind's review body — every surviving n-gram: text, class
 * direction (the dislike half is part of the disclosure, which is why the
 * column exists), the class-blind distinct-post count, and the include
 * checkbox, checked by default. The direction/count texts are the shared
 * faces the subscriber's `labeler-inspect-model-entry-*` twin renders,
 * resolved once at scrub time in the view-model.
 */
@Composable
private fun PublishNgramReview(state: PublishSheetUiState, onToggleInclude: (Int) -> Unit) {
    Text(
        stringResource(R.string.personalization_publish_ngrams_title),
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.padding(top = 8.dp),
    )
    if (state.ngrams.isEmpty()) {
        // The refusal state: fewer than 3 public examples, or nothing
        // survived the 3-post floor — nothing can be shared without quoting
        // a single post.
        Text(
            stringResource(R.string.personalization_publish_ngrams_empty),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_EMPTY),
        )
    }
    Column(modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_LIST)) {
        state.ngrams.forEachIndexed { index, ngram ->
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_ITEM),
            ) {
                Checkbox(
                    checked = ngram.included,
                    onCheckedChange = { onToggleInclude(index) },
                    modifier = Modifier.testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_CHECKBOX),
                )
                Text(
                    ngram.ngram,
                    maxLines = 1,
                    modifier = Modifier
                        .weight(1f)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_TEXT),
                )
                Text(
                    ngram.directionText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .padding(start = 8.dp)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_DIRECTION),
                )
                Text(
                    ngram.countText,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier
                        .padding(start = 8.dp)
                        .testTag(Ids.PERSONALIZATION_TRAINED_FACTOR_PUBLISH_NGRAM_COUNT),
                )
            }
        }
    }
}
