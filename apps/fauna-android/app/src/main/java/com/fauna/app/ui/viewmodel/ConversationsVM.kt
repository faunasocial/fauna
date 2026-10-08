package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyInputs
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.FamilyNotifyStore
import com.fauna.app.core.conversations.ConversationsManagerHost
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.ConversationsSnapshot
import uniffi.fauna_conversations.MessageSnapshot
import uniffi.fauna_core.ContentLabelEntry
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.launchIn
import kotlinx.coroutines.flow.onEach
import kotlinx.coroutines.launch
import javax.inject.Inject

/**
 * Thin observer over the shared `ConversationsManager` (UniFFI) — the single
 * observable surface the conversations list / detail / compose screens render
 * off, per docs/goal/ui/conversations.md §"No client-side MLS state". All
 * thread state, sealing, decryption and rail routing live in shared Rust; this
 * VM only exposes the snapshot and routes user actions to manager mutators.
 *
 * Phase 5 deleted the bespoke Kotlin MLS/HTTP
 * send path that used to live here (the Room-backed `conversations` flow, the
 * `MlsManager`-driven send/receive, and the `ApiClient` channel/welcome
 * routes the post-T8 nest had already removed). The legacy `ComposeScreen`
 * that drove it is gone; what remains is this snapshot pass-through.
 */
@HiltViewModel
class ConversationsVM @Inject constructor(
    private val conversationsHost: ConversationsManagerHost,
    private val api: ApiClient,
    private val contentPolicyStore: ContentPolicyStore,
    private val familyNotifyStore: FamilyNotifyStore,
) : ViewModel() {

    /** Observable snapshot of all conversations state (shared Rust). */
    val managerSnapshot: StateFlow<ConversationsSnapshot?> get() = conversationsHost.snapshot

    init {
        // Feed the backup audit loop's freshness comparison (docs/goal/ui/backups.md
        // § Audit-alert surface): the newest activity this client has actually
        // DISPLAYED is its own, source-untrusted evidence that data this recent
        // exists, and the audit compares that against what each backup destination
        // is holding. This is the load-bearing half of the audit surface, not an
        // afterthought — a client that never calls this ships a permanently-passing
        // audit. Monotonic + a no-op on a repeat snapshot (mirrors linux's
        // `list.rs::render` / web's conversations `+page.svelte`, the one place each
        // app shows the user what it knows about nest-originated message kinds).
        managerSnapshot.onEach { snapshot ->
            val newest = snapshot?.threads?.maxOfOrNull { it.lastActivityMs } ?: return@onEach
            runCatching { api.backupAuditObserve(newest) }
        }.launchIn(viewModelScope)
    }

    /**
     * The shared content-policy render inputs (guardian floor + the viewer's own
     * spam/phishing thresholds) — each message bubble resolves its block/collapse
     * verdict off this via [ContentPolicyInputs.verdictFor] (family-safety.md
     * § Content policy). The SAME [ContentPolicyStore] the feed uses, so the two
     * social surfaces can never drift on how a floor is enforced.
     */
    val contentPolicyInputs: StateFlow<ContentPolicyInputs> get() = contentPolicyStore.inputs

    /** `dm-message-report-button` — open the shared report sheet on [target]
     *  (moderation.md § User-initiated reporting; the shell's ReportHost paints it). */
    fun openReport(target: com.fauna.ffi.FfiReportTarget) = contentPolicyStore.report.open(target)

    /** Count a rendered message's guardian-floor enforcement for **Guardian
     *  Notify** (family-safety.md § Guardian Notify) — a no-op unless the
     *  ward's `content_notify` knob is on and the guardian floor bites. */
    fun noteContentEnforcement(messageId: String, labels: List<ContentLabelEntry>) =
        familyNotifyStore.record(messageId, labels)

    /** The shared manager, for routing user actions to its mutators. */
    val conversationsManager: ConversationsManager get() = conversationsHost.manager

    /**
     * Open the new-thread compose pre-seeded with [recipient] (a Fauna hex
     * actor-id or a `handle`/`handle@domain`), for the Contacts "message"
     * deep-link. Mirrors the new-thread picker's accept flow: start a new
     * conversation, type the recipient, async-resolve it, then commit the chip.
     *
     * A bare 64-hex actor-id resolves through this path — the FaunaMls rail
     * backend's `resolve_address` decodes the hex and promotes it to a Fauna
     * chip (proven by fauna-conversations'
     * `resolve_recipient_promotes_actor_id_to_fauna_chip`). The synchronous
     * `setNewThreadRecipientInput` alone classifies bare hex as an error; only
     * the async `resolveRecipient` probe promotes it, so the seed must run
     * through `resolveRecipient` + `acceptCurrentRecipientChip`.
     *
     * Like send, resolve/accept are parity no-ops in production today (no rail
     * backend is registered from Kotlin — `register_backend` is not
     * UniFFI-exported), so in prod the recipient lands unresolved, the same
     * stubbed state as the FAB-driven new-thread compose. In e2e (mock
     * backend) it resolves to a chip.
     *
     * `startNewConversation` runs synchronously so the caller can navigate to
     * `conversation_compose` immediately; the picker fills in reactively as the
     * snapshot updates.
     */
    fun startConversationWith(recipient: String) {
        val manager = conversationsManager
        manager.startNewConversation()
        viewModelScope.launch {
            manager.setNewThreadRecipientInput(recipient)
            manager.resolveRecipient()
            manager.acceptCurrentRecipientChip()
        }
    }

    /** Resolve link-preview metadata for [url] (render-model.md § D4) — the
     *  conversations twin of [FeedVM.resolveLinkPreview]. Calls the shared
     *  `ConversationsManager.resolveLinkPreview` once per url (cached), which folds the
     *  `Resolved`/`Failed` state onto the matching message bubble's `LinkPreview` block and
     *  re-emits so the detail screen paints the card. Fire-once: the bubble triggers this
     *  only while the block is still `Resolving`. A no-op when no `LinkPreviewRpc` is wired
     *  (receive-only / SMTP-only rails). */
    fun resolveLinkPreview(url: String) {
        val manager = conversationsManager
        viewModelScope.launch { manager.resolveLinkPreview(url) }
    }

    /** Fetch the bytes for a link-preview og:image blob (the `imageHash` the nest resolved
     *  for a `RenderBlock.LinkPreview`) — the bubble's og:image painter. The og:image is a
     *  nest-served content-addressed plaintext blob (`/api/v1/blob/<hash>`, the bulk-binary
     *  HTTP carve-out), NOT an MLS-encrypted conversation attachment, so it loads through the
     *  plain HTTP `ApiClient` blob loader, NOT the manager's `attachment_bytes` decrypt path.
     *  Mirrors [FeedVM.fetchBlobBytes]; the async byte load stays client glue
     *  (render-model.md § The boundary). */
    suspend fun fetchBlobBytes(hash: String): ByteArray? =
        runCatching { api.fetchBlobBytes(hash) }.getOrNull()

    /** The user's muted-word list (moderation.md § Muted keywords), for the
     *  conversation-detail collapse render. The at-render match is the shared
     *  `matchesMutedKeywords`; this is the client-held list it matches against, refreshed on
     *  detail mount so Settings edits reflect on the next open. A *hide/collapse* control —
     *  never routed through the LocalDetectionStore / moderation queue. */
    val mutedKeywords =
        kotlinx.coroutines.flow.MutableStateFlow<List<uniffi.fauna_core.MutedKeyword>>(emptyList())

    /** Load the muted-word list (best-effort; empty on failure — the collapse just no-ops).
     *  Also re-pulls the content-policy inputs (guardian floor + own spam thresholds) so a
     *  guardian's policy change reaches the conversation render on the next detail open. */
    fun loadMutedKeywords() {
        contentPolicyStore.refresh()
        viewModelScope.launch {
            runCatching { api.mutedKeywordsList().keywords }.getOrNull()?.let { mutedKeywords.value = it }
        }
    }

    /** "Mark as spam" on a received conversation message — the live `Insert` consumer
     *  (mail-spam.md § Wire shapes `put_spam_model` `history_op`). Trains the sealed
     *  tier-1 spam model over the message's retained decrypted body AND writes a sealed
     *  training-history row (the `mail-spam` page renders `{subject} · INBOX` + offers
     *  per-row undo) via the shared façade `trainSpamModelClientMail`. Fire-and-forget,
     *  silent on a non-sealed outcome / mail-not-enabled: a conversation message is
     *  client-only encrypted content the nest can't read, so — unlike a moderation-queue
     *  *server-row* correction — there is NO server train to degrade to (mirrors linux
     *  `mark_message_spam`). The `message_id` is stored opaque (its UTF-8 bytes); the
     *  subject falls back to a body snippet in the shared façade when absent. */
    fun markMessageSpam(msg: MessageSnapshot) {
        if (msg.body.isBlank()) return
        viewModelScope.launch {
            runCatching {
                api.trainSpamModelClientMail(
                    msg.body,
                    true, // is_spam
                    msg.messageId.toByteArray(),
                    "INBOX",
                    msg.subjectLine ?: "",
                )
            }
        }
    }

    // ── Post-succession member review (succession-aftermath.md § Propagation
    // → *MLS groups*, item 3a) ──
    //
    // The open roster, CACHED here rather than re-read per paint (a thread
    // paints far more often than the ledger changes) and refreshed at the
    // two ratified points, adapted to this screen's own architecture the way
    // linux's page-local conversation cache does: this app drives no
    // aftermath pump of its own yet (a separate, unbuilt gap), so "behind
    // the aftermath's raise" is approximated by loading once on detail
    // mount, and "after every adjudication" is the explicit re-read
    // [keepMemberReview] below performs.

    /** The open review roster (`fauna_client_config::load_member_reviews` via
     *  the shared FFI face), for [conversationsManager]'s
     *  `memberReviewMarksForThread` to answer per-thread questions from —
     *  never a hand-rolled per-app scan. Best-effort; empty on failure, the
     *  same "nothing to paint" a thread with no flagged members gives. */
    val memberReviewRoster =
        kotlinx.coroutines.flow.MutableStateFlow<List<com.fauna.ffi.FfiMemberReview>>(emptyList())

    /** Load ONCE on detail mount (`LaunchedEffect(Unit)`, mirroring
     *  [loadMutedKeywords]) — the first of the two ratified refresh points. */
    fun loadMemberReviewRoster() {
        viewModelScope.launch {
            runCatching { api.memberReviewList() }.getOrNull()?.let { memberReviewRoster.value = it }
        }
    }

    /** Record **Keep** for [person], then re-read the roster — the second
     *  ratified refresh point. A concurrent device may have already
     *  answered, which is a success no-op, never an error (mirrors
     *  [MemberReviewVM.keep]). */
    fun keepMemberReview(person: ByteArray) {
        viewModelScope.launch {
            runCatching { api.memberReviewKeep(person) }
            loadMemberReviewRoster()
        }
    }

    /** The review mark each of [threadId]'s member chips carries,
     *  index-parallel with `ThreadDetail.participantDisplays` — see
     *  [ApiClient.memberReviewMarksForThread]. Synchronous (a cache read
     *  over an already-open manager), so the detail screen can call it
     *  directly from a `remember` block. */
    fun memberMarksForThread(
        threadId: String,
        roster: List<com.fauna.ffi.FfiMemberReview>,
    ): List<ByteArray?> =
        runCatching { api.memberReviewMarksForThread(threadId, roster) }.getOrDefault(emptyList())
}
