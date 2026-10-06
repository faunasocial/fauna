package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.ApiClient
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.app.ui.util.resolveLocalized
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import com.fauna.ffi.FfiPublishEntry
import com.fauna.ffi.FfiPublishModelException
import com.fauna.ffi.FfiPublishNgram
import com.fauna.ffi.FfiTrainedTopicRow
import com.fauna.ffi.FfiTrainedTopicsException
import com.fauna.ffi.ngramDirectionLabel
import com.fauna.ffi.ngramDocCountLabel
import javax.inject.Inject

/**
 * One exemplar the review sheet renders — [uniffi.fauna_feed.ScoredExemplar]
 * plus the local prune checkbox state (mirrors web's `PublishExemplar`
 * interface: `{...scored, include: true}`).
 */
data class PublishExemplarUi(
    val postId: String,
    val preview: String,
    val score: Long,
    val included: Boolean,
)

/**
 * One surviving n-gram the Model review renders — [uniffi.fauna_feed.ReviewNgram]
 * plus the local prune state and the two shared-face labels resolved once at
 * scrub time (`ngram_direction_label` / `ngram_doc_count_label`: the
 * publisher's review is a promise about what a subscriber reads back at
 * `labeler-inspect-model-entry-*`, so both ends render the SAME faces).
 */
data class PublishNgramUi(
    val ngram: String,
    val more: UInt,
    val less: UInt,
    val directionText: String,
    val countText: String,
    val included: Boolean,
)

/**
 * The publish review-prune sheet's whole renderable state; `null` = closed.
 *
 * [kind] is the raw `artifact_kind` discriminator the sheet reviews
 * (`"list"` | `"text-model"`, never a label — `fauna_core::format::publish_kind_options`),
 * List by default: the weaker disclosure, so it is what an unattended open
 * lands on. The two kinds review different objects — [exemplars] for the
 * List, [ngrams] for the Model — and the four corpus counters are the
 * Model's mandated corpus-size line, carried UNSHRUNK through to publish.
 */
data class PublishSheetUiState(
    val exemplars: List<PublishExemplarUi>,
    val name: String,
    val busy: Boolean,
    val kind: String = PUBLISH_KIND_LIST,
    val ngrams: List<PublishNgramUi> = emptyList(),
    val moreDocs: UInt = 0u,
    val lessDocs: UInt = 0u,
    val includedExamples: UInt = 0u,
    val markedExamples: UInt = 0u,
) {
    val isModelKind: Boolean get() = kind == PUBLISH_KIND_MODEL

    /** Publish is armed once at least one row of the ACTIVE kind is kept —
     *  independent of the name field (the disarm/re-arm e2e leg flips
     *  checkboxes before ever typing a name). Mirrors linux
     *  `refresh_submit_sensitivity`. */
    val hasIncluded: Boolean
        get() = if (isModelKind) ngrams.any { it.included } else exemplars.any { it.included }
}

/** The two `artifact_kind` values the sheet can review — the wire
 *  discriminators, shared with every app's driver constants. */
const val PUBLISH_KIND_LIST = "list"
const val PUBLISH_KIND_MODEL = "text-model"

/**
 * Drives the Personalization home's **Trained-topics** facet
 * (`topic-factors.md` § Authoring surface & picker, S8) AND the **publish
 * review-prune sheet** (§ Publishing a trained factor) — mirrors
 * [MutedWordsVM]'s free-fn round-trip shape (this facet has no shared
 * `#[uniffi::Object]` machine, unlike [LabelerCatalogVM]) and bundles the
 * publish sheet in the SAME view-model, matching linux's `Ctx` (which owns
 * both) and web's `PersonalizationSection.svelte` (both state blocks in one
 * component) — not a second view-model.
 *
 * The registry↔model-plane sequencing (advisory example-count read, create
 * cap, delete's registry-removal-then-`model.delete` pairing) lives once in
 * `fauna_client_personalization::topics::TrainedTopics`; this VM is a thin
 * wrapper over the `trained_topics_*` UniFFI free-fns that bind it, plus the
 * publish lifecycle's `trained_topic_publish_list` / `_model` (priority #2 —
 * no re-derivation of any of it).
 */
@HiltViewModel
class TrainedTopicsVM @Inject constructor(
    private val api: ApiClient,
    private val feedManagerHost: FeedManagerHost,
    @ApplicationContext private val appContext: Context,
) : ViewModel() {

    init {
        // The store-change notice: a topic trained on another device appears
        // on the open page ([ApiClient.storeChangedTick]). `load` writes only
        // the topic list — the name draft and a rename in progress stay.
        viewModelScope.launch { api.storeChangedTick.collect { load() } }
    }

    private val _topics = MutableStateFlow<List<FfiTrainedTopicRow>>(emptyList())
    val topics: StateFlow<List<FfiTrainedTopicRow>> = _topics.asStateFlow()

    private val _topicName = MutableStateFlow("")
    val topicName: StateFlow<String> = _topicName.asStateFlow()

    private val _busy = MutableStateFlow(false)
    val busy: StateFlow<Boolean> = _busy.asStateFlow()

    private val _errorMessage = MutableStateFlow<String?>(null)
    val errorMessage: StateFlow<String?> = _errorMessage.asStateFlow()

    /** `Some(factor id)` while [topicName] is retargeted at a rename. */
    private var renamingId: ByteArray? = null
    private val _isRenaming = MutableStateFlow(false)
    val isRenaming: StateFlow<Boolean> = _isRenaming.asStateFlow()

    private val _publish = MutableStateFlow<PublishSheetUiState?>(null)
    val publish: StateFlow<PublishSheetUiState?> = _publish.asStateFlow()

    /** The factor the open sheet targets. `null` while closed. */
    private var publishTargetId: ByteArray? = null

    /** The open sheet's `topic:<hex>` key — what both corpus reads take. */
    private var publishTargetFactorKey: String? = null

    /**
     * Bumped on every [openPublishSheet], [setPublishKind] and
     * [cancelPublish]/[closePublish]. A corpus read is async; without this a
     * result scored for factor A (or for the other kind) can land after the
     * sheet re-opened against factor B (mirrors linux `Ctx::generation` /
     * web `publishGeneration`).
     */
    private var publishGeneration = 0

    /** Kept: `trainedTopicsList` composes a config fetch+unseal with a
     *  second RPC for the per-factor counts, not a single NestClient RPC
     *  (transport.md § Request lifecycle step 3's note). */
    fun load() {
        viewModelScope.launch {
            repeat(HYDRATE_ATTEMPTS) {
                try {
                    _topics.value = api.trainedTopicsList()
                    return@launch
                } catch (_: Exception) {
                    delay(HYDRATE_RETRY_MS)
                }
            }
        }
    }

    fun onTopicNameChange(name: String) {
        _topicName.value = name
    }

    /** Commit the name-input: a rename when a row retargeted it, else a create. */
    fun submitTopic() {
        val name = _topicName.value.trim()
        if (name.isEmpty() || _busy.value) return
        val id = renamingId
        _busy.value = true
        viewModelScope.launch {
            try {
                _topics.value = if (id != null) {
                    api.trainedTopicsRename(id, name)
                } else {
                    api.trainedTopicsCreate(name)
                }
                // Any successful commit resets the input to create mode.
                renamingId = null
                _isRenaming.value = false
                _topicName.value = ""
                _errorMessage.value = null
            } catch (e: Exception) {
                _errorMessage.value = topicsErrorText(e)
            } finally {
                _busy.value = false
            }
        }
    }

    fun startRename(row: FfiTrainedTopicRow) {
        renamingId = row.id
        _isRenaming.value = true
        _topicName.value = row.name
    }

    fun deleteTopic(row: FfiTrainedTopicRow) {
        if (_busy.value) return
        _busy.value = true
        viewModelScope.launch {
            try {
                _topics.value = api.trainedTopicsDelete(row.id)
                // A delete of the row currently being renamed strands the input.
                if (renamingId?.contentEquals(row.id) == true) {
                    renamingId = null
                    _isRenaming.value = false
                    _topicName.value = ""
                }
                _errorMessage.value = null
            } catch (e: Exception) {
                _errorMessage.value = topicsErrorText(e)
            } finally {
                _busy.value = false
            }
        }
    }

    /** Flip a row's Layer-A opt-in ("Learn from my activity" —
     *  engagement-cues.md § Layer A). Registry-only; the model row is untouched. */
    fun toggleEngagement(row: FfiTrainedTopicRow) {
        viewModelScope.launch {
            try {
                _topics.value = api.trainedTopicsSetLearnFromEngagement(row.id, !row.learnFromEngagement)
                _errorMessage.value = null
            } catch (e: Exception) {
                _errorMessage.value = topicsErrorText(e)
            }
        }
    }

    /**
     * "Clear activity data" (`personalization-clear-engagement-data-button`) —
     * delete the sealed `cues:v1` rollup from the user's own nest and reset the
     * live cue engine (`FfiFeedManager::delete_cue_rollup`; engagement-cues.md
     * § At rest).
     *
     * This is the **user-revocable affordance the capture invariant requires**,
     * not a nicety: the client captures engagement cues, so the user must be
     * able to delete them from their own client (the product invariant — a user
     * always controls their data; every new data shape is revocable from the
     * client). It is the sibling of [toggleEngagement] — that one stops future
     * learning, this one erases what was already captured.
     */
    fun clearEngagementData() {
        viewModelScope.launch {
            val manager = feedManagerHost.manager()
            if (manager == null) {
                _errorMessage.value = appContext.getString(R.string.common_not_connected)
                return@launch
            }
            try {
                manager.deleteCueRollup()
                _errorMessage.value = null
            } catch (e: Exception) {
                _errorMessage.value = e.message
            }
        }
    }

    /** Reveal the sheet against one factor, on the default List kind, and
     *  score its corpus — the loaded feed window, deliberately not a paging
     *  crawl (§ Publishing's accepted limitation). */
    fun openPublishSheet(row: FfiTrainedTopicRow) {
        publishTargetId = row.id
        publishTargetFactorKey = row.factorKey
        _publish.value = PublishSheetUiState(exemplars = emptyList(), name = "", busy = false)
        _errorMessage.value = null
        // A corrupt (non-16-byte) id has no addressable model — unreachable via
        // the disabled publish button, kept as a defensive no-op.
        val factorKey = row.factorKey ?: return
        readCorpus(PUBLISH_KIND_LIST, factorKey)
    }

    /**
     * `-publish-kind-select`: swap which artifact kind the open sheet
     * reviews. **Re-reads from scratch** — the two kinds review different
     * objects through different shared faces, so a carried-over prune or
     * resolved-corpus state would paint one kind's refusal over the other's
     * un-read corpus (mirrors tui `set_publish_kind_op`, apple
     * `setPublishKind`). Picking the kind already selected is a no-op — a
     * redundant select must not discard a review in progress. The public
     * name survives the swap.
     */
    fun setPublishKind(kind: String) {
        val state = _publish.value ?: return
        val factorKey = publishTargetFactorKey ?: return
        if (kind == state.kind) return
        _publish.value = PublishSheetUiState(exemplars = emptyList(), name = state.name, busy = false, kind = kind)
        readCorpus(kind, factorKey)
    }

    /** The async corpus read for [kind] — the List's top-N scoring or the
     *  Model's scrub — landing only if no open/close/swap superseded it. */
    private fun readCorpus(kind: String, factorKey: String) {
        publishGeneration += 1
        val generation = publishGeneration
        viewModelScope.launch {
            val manager = feedManagerHost.manager()
            if (manager == null) {
                if (generation == publishGeneration) {
                    _errorMessage.value = appContext.getString(R.string.common_not_connected)
                }
                return@launch
            }
            try {
                if (kind == PUBLISH_KIND_MODEL) {
                    val review = manager.scrubCorpusForFactor(factorKey)
                    if (generation != publishGeneration) return@launch // superseded
                    _publish.value = _publish.value?.copy(
                        ngrams = review.ngrams.map {
                            PublishNgramUi(
                                ngram = it.ngram,
                                more = it.more,
                                less = it.less,
                                directionText = resolveLocalized(appContext, ngramDirectionLabel(it.more, it.less)) ?: "",
                                countText = resolveLocalized(appContext, ngramDocCountLabel(it.more, it.less)) ?: "",
                                included = true,
                            )
                        },
                        moreDocs = review.moreDocs,
                        lessDocs = review.lessDocs,
                        includedExamples = review.includedExamples,
                        markedExamples = review.markedExamples,
                    )
                } else {
                    val scored = manager.scoreCorpusForFactor(factorKey)
                    if (generation != publishGeneration) return@launch // superseded by a close/re-open
                    _publish.value = _publish.value?.copy(
                        exemplars = scored.map {
                            PublishExemplarUi(it.postId, it.preview, it.score, included = true)
                        },
                    )
                }
            } catch (e: Exception) {
                if (generation != publishGeneration) return@launch
                _errorMessage.value = e.message
            }
        }
    }

    /** Close without publishing — drop the target + reviewed rows so a
     *  re-open cannot inherit a stale prune. */
    fun cancelPublish() {
        publishTargetId = null
        publishTargetFactorKey = null
        publishGeneration += 1
        _publish.value = null
    }

    fun onPublishNameChange(name: String) {
        _publish.value = _publish.value?.copy(name = name)
    }

    fun togglePublishExemplarIncluded(index: Int) {
        val state = _publish.value ?: return
        val exemplars = state.exemplars.toMutableList()
        if (index !in exemplars.indices) return
        exemplars[index] = exemplars[index].let { it.copy(included = !it.included) }
        _publish.value = state.copy(exemplars = exemplars)
    }

    fun togglePublishNgramIncluded(index: Int) {
        val state = _publish.value ?: return
        val ngrams = state.ngrams.toMutableList()
        if (index !in ngrams.indices) return
        ngrams[index] = ngrams[index].let { it.copy(included = !it.included) }
        _publish.value = state.copy(ngrams = ngrams)
    }

    /**
     * Publish what survived the prune, as the ACTIVE kind. Model kind:
     * `moreDocs`/`lessDocs` are passed through UNSHRUNK — they are the
     * corpus's own counters, not the pruned entry count, and stay true
     * however much of the vocabulary the user withheld.
     */
    fun submitPublish() {
        val state = _publish.value ?: return
        val targetId = publishTargetId ?: return
        // A blank name is refused here rather than at the wire — the same
        // guard the create/rename flow uses.
        val name = state.name.trim()
        if (name.isEmpty() || state.busy) return
        if (state.isModelKind) {
            val ngrams = state.ngrams.filter { it.included }
                .map { FfiPublishNgram(ngram = it.ngram, more = it.more, less = it.less) }
            if (ngrams.isEmpty()) return
            _publish.value = state.copy(busy = true)
            viewModelScope.launch {
                try {
                    api.publishTrainedFactorModel(targetId, name, state.moreDocs, state.lessDocs, ngrams)
                    _errorMessage.value = null
                    cancelPublish()
                } catch (e: Exception) {
                    _errorMessage.value = publishModelErrorText(e)
                    _publish.value = _publish.value?.copy(busy = false)
                }
            }
            return
        }
        val entries = state.exemplars.filter { it.included }
            .map { FfiPublishEntry(postId = it.postId, score = it.score) }
        // Unreachable through the button (disabled with nothing kept); kept so
        // "never publish an empty list" holds at the call, not only the widget.
        if (entries.isEmpty()) return
        _publish.value = state.copy(busy = true)
        viewModelScope.launch {
            try {
                api.publishTrainedFactorList(targetId, name, entries)
                _errorMessage.value = null
                cancelPublish()
            } catch (e: Exception) {
                _errorMessage.value = e.message
                _publish.value = _publish.value?.copy(busy = false)
            }
        }
    }

    /** The one place the shared crate's typed errors become user-facing text —
     *  the cap message names the limit, so it needs the number rather than a
     *  sentence the crate pre-baked (mirrors linux `localize`). */
    private fun topicsErrorText(e: Exception): String? = when (e) {
        is FfiTrainedTopicsException.Cap -> appContext.getString(R.string.personalization_trained_factor_cap)
            .replace("{max}", e.max.toString())
        else -> e.message
    }

    /** The Model kind's typed refusals — `EmptyVocabulary` is the one a user
     *  fixes by marking more public posts rather than by editing the sheet
     *  (§ Publishing), so it gets the ratified copy; a blank or over-long name
     *  reads the List's own two strings (same field, same bound). */
    private fun publishModelErrorText(e: Exception): String? = when (e) {
        is FfiPublishModelException.EmptyVocabulary ->
            appContext.getString(R.string.personalization_publish_vocabulary_empty)
        is FfiPublishModelException.BlankName ->
            appContext.getString(R.string.personalization_publish_name_blank)
        is FfiPublishModelException.NameTooLong ->
            appContext.getString(R.string.personalization_publish_name_too_long)
                .replace("{max}", e.max.toString())
        else -> e.message
    }

    companion object {
        private const val HYDRATE_ATTEMPTS = 10
        private const val HYDRATE_RETRY_MS = 500L
    }
}
