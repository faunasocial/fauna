package com.fauna.app.core

import com.fauna.ffi.FfiContentPolicy
import com.fauna.ffi.FfiSupervisionSnapshot
import com.fauna.ffi.contentRenderVerdict
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.fauna_core.ContentLabelEntry
import javax.inject.Inject
import javax.inject.Singleton

/**
 * App-scoped cache of the two inputs the shared content-policy render engine
 * needs (family-safety.md § Content policy) — the **android twin** of linux's
 * `crate::content_policy` module and web's `contentPolicy.svelte.ts`
 * (priority #2/#3, one concept everywhere):
 *
 * 1. the guardian's per-category floor (the `content_policy` of a
 *    `fauna.family.status` reply's gated `supervision` fold; `null` unless
 *    supervised),
 * 2. the viewer's OWN spam/phishing per-mille thresholds
 *    (`fauna.spam.get_preferences`) — the every-user un-darking of
 *    moderation.md § Categories & enforcement item 1, and
 * 3. the guardian's Guardian Notify knob (the same fold's `content_notify`;
 *    default off) — [FamilyNotifyStore] reads it off this same cache rather
 *    than a second `fauna.family.status` poll.
 *
 * The guardian half moves from the fold shared Rust attaches to every reply
 * (`FfiFamilyStatus.supervision`), never from the raw `policy`: the fold gates
 * every supervised field on `supervised_by`, so a policy that names no
 * guardian binds nothing (family-client-enforcement.md § Implementation
 * status today).
 *
 * Both social surfaces (feed post-card, conversation bubble) resolve one item's
 * render verdict through [ContentPolicyInputs.verdictFor], which composes the
 * two, **strictest-wins, entirely in shared Rust** (`contentRenderVerdict`) — no
 * rule assembly lives in Kotlin, exactly as the linux/web legs keep it out of
 * Rust-native code / JS.
 *
 * `@Singleton` (not a ViewModel) so the feed and conversation VMs share ONE
 * cache rather than each re-reading and drifting. Refreshed at construction
 * (the first surface VM injects it post-login) and on every WS reconnect
 * (`api.reconnectTick`) — the same "re-resolve as the socket comes up" idiom
 * [SupervisedIndicatorVM] uses.
 *
 * **A failed read keeps the last-known state in force** (family-safety.md
 * § Content policy, the unfetched-policy ruling clause 1): "read failed" and
 * "read says unsupervised" are different facts, and refresh fires exactly when
 * a read is least likely to succeed (construction, WS reconnect) — so a
 * transient outage must never clear a loaded guardian floor. Only a
 * *successful* status read (which may report unsupervised) or the identity
 * change below moves the guardian half. The own-thresholds half gets the same
 * keep for uniformity, though its failure direction is benign either way.
 *
 * Keep-on-failure makes the identity reset load-bearing (the trap web's
 * `contentPolicy.svelte.ts` documents): without it a failed re-read after an
 * account switch would render the NEXT account against the previous ward's
 * floor. The closer registration lives here, next to the state, per
 * account-scoping.md § The scoping taxonomy.
 */
@Singleton
class ContentPolicyStore @Inject constructor(
    private val api: ApiClient,
    accountStores: AccountStores,
) {
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    private val _inputs = MutableStateFlow(ContentPolicyInputs())
    val inputs: StateFlow<ContentPolicyInputs> = _inputs.asStateFlow()

    /**
     * The actor this store's current state was last seeded for — `null`
     * before the first seed. Lets the closer below tell a genuine switch (the
     * active pointer now names someone else) from a **same-actor drop** (the
     * pointer is unchanged — the debug reset/logout arms below), so it never
     * resurrects the outgoing actor's own snapshot under what is supposed to
     * be a blank reset.
     */
    private var seededActorHex: String? = null

    init {
        accountStores.registerCloser("content-policy") {
            _inputs.value = ContentPolicyInputs()
            // Re-seed for whoever is active NOW (family-safety.md § Content
            // policy, clause 2): the switch closer runs AFTER
            // `AccountStores`'s active pointer already names the incoming
            // actor (`AccountSettingsVM.switchAccount`), so without this an
            // offline switch renders the incoming ward unsupervised until a
            // read succeeds. Guarded on the actor actually having changed —
            // a same-actor drop (the debug `TestAgent` `reset`/`logout` arms:
            // `SecureStorage.clear()` empties `fauna_secure_prefs`, but under
            // `FileSecretBackend` e2e mode the registry's active pointer
            // lives in a DIFFERENT file and survives untouched) must land
            // blank, not the previous ward's own last-known state.
            val newActor = accountStores.activeActorHex()
            if (newActor != null && newActor != seededActorHex) {
                accountStores.supervisionSnapshot()?.let { applySupervision(it) }
            }
            seededActorHex = newActor
        }
        // Seed the guardian half from the persisted last-known supervision
        // snapshot BEFORE the first read fires (family-safety.md § Content
        // policy, clause 2 — "loaded at launch ahead of the first read"): a
        // supervised ward who launches offline keeps their floor instead of
        // rendering unsupervised until a read succeeds. Only the guardian
        // half — the viewer's own thresholds are deliberately not in the
        // snapshot (their absence only under-enforces the viewer's own
        // collapse; they re-arrive with their own read). Synchronous, so
        // refresh()'s coroutine can never lose a race against it; a later
        // successful read supersedes it and the identity closer above drops
        // it exactly like any loaded floor. The restore is the same
        // `FfiSupervisionSnapshot` shape a live read's fold carries, so it
        // enters through the same door.
        accountStores.supervisionSnapshot()?.let { applySupervision(it) }
        seededActorHex = accountStores.activeActorHex()
        refresh()
        scope.launch { api.reconnectTick.collect { refresh() } }
    }

    /**
     * Move the guardian half from a **successful** `fauna.family.status` read
     * made elsewhere — [com.fauna.app.ui.viewmodel.FamilyVM]'s page read, the
     * freshest view of the ward's own policy (web's Family page feeds its floor
     * the same way). [supervision] is that reply's gated fold
     * (`FfiFamilyStatus.supervision`): `null` means the read named nothing
     * enforceable, which clears the half. Never call it for a failed read
     * (clause 1).
     */
    fun applySupervision(supervision: FfiSupervisionSnapshot?) {
        _inputs.update {
            it.copy(contentPolicy = supervision?.contentPolicy, contentNotify = supervision?.contentNotify ?: false)
        }
    }

    /** Re-pull the guardian floor + Notify knob + own spam thresholds. The status
     *  read and the preferences read are independent; each keeps its last-known
     *  half on failure (class doc), so an unsupervised viewer (no family status)
     *  still gets the own-threshold half and vice versa. */
    fun refresh() {
        scope.launch {
            val status = try {
                api.familyStatus()
            } catch (_: Exception) {
                null
            }
            val prefs = try {
                api.getSpamPreferences()
            } catch (_: Exception) {
                null
            }
            _inputs.update { prev ->
                ContentPolicyInputs(
                    // The reply's gated fold, never the raw `policy` (class doc).
                    contentPolicy =
                        if (status != null) status.supervision?.contentPolicy else prev.contentPolicy,
                    contentNotify =
                        if (status != null) status.supervision?.contentNotify ?: false
                        else prev.contentNotify,
                    ownSpamPermille =
                        if (prefs != null) prefs.spamThreshold else prev.ownSpamPermille,
                    ownPhishingPermille =
                        if (prefs != null) prefs.phishingThreshold else prev.ownPhishingPermille,
                )
            }
        }
    }
}

/**
 * The immutable snapshot of the render-engine inputs [ContentPolicyStore] holds.
 * A plain data class (not the store) so a composable can memoize a verdict on it
 * with `remember(labels, inputs)` and a Robolectric test can drive
 * [verdictFor] with fixed inputs, no VM or coroutine needed.
 */
data class ContentPolicyInputs(
    val contentPolicy: FfiContentPolicy? = null,
    val contentNotify: Boolean = false,
    val ownSpamPermille: UShort? = null,
    val ownPhishingPermille: UShort? = null,
) {
    /**
     * The client render verdict for one item's `labels` — one of
     * `"show" | "badge" | "collapse" | "block"`, resolved by the shared
     * `contentRenderVerdict` (strictest-wins compose of the guardian floor and
     * the viewer's own thresholds, all in Rust).
     *
     * When there is neither a guardian floor nor an own threshold, no rule can
     * fire, so the verdict is at most `"badge"` — and `"badge"`/`"show"` are
     * identical to the render gate (both render normally). This short-circuits to
     * `"show"` without the shared-Rust call, which keeps a default-constructed
     * [ContentPolicyInputs] FFI-free (the VM-free conversation content harness
     * renders with the default and must not require the host `.so`).
     */
    fun verdictFor(labels: List<ContentLabelEntry>): String =
        if (contentPolicy == null && ownSpamPermille == null && ownPhishingPermille == null) {
            "show"
        } else {
            contentRenderVerdict(labels, contentPolicy, ownSpamPermille, ownPhishingPermille)
        }
}
