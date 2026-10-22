package com.fauna.app.core

import android.content.Context
import com.fauna.ffi.FfiGuardianPlaceholder
import com.fauna.ffi.FfiNestClient
import com.fauna.ffi.FfiNestPlaceholder
import com.fauna.ffi.FfiRegionPlaceholder
import com.fauna.ffi.FfiRegionPlane
import com.fauna.ffi.FfiRegionSource
import com.fauna.ffi.FfiRegionView
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.fauna_core.ContentLabelEntry
import java.util.Collections

/**
 * The region content plane on android (`region-blocking.md` § The content
 * plane → *How an app obtains its region's policy*, *The blocked render and the
 * transparency surface*) — a paint of the shared `FfiRegionPlane`
 * (`libs/fauna-ffi/src/region.rs`), the one face the four UniFFI apps drive.
 * windows' `RegionPlaneHost.cs`, apple's `RegionStore.swift`, linux's
 * `apps/fauna-linux/src/region.rs` and web's `#lib/region.svelte.ts` are the
 * sibling legs.
 *
 * Everything that decides — verify, fold, persist, the refresh cadence, the
 * scorer join, the composed verdict, which verbs get a placeholder and which
 * language of the reason it shows — is shared Rust behind the plane. What is
 * left here is exactly what the design lets diverge: the leaf's answer
 * ([RegionLeaf]); where the device record lives (`filesDir`, install-scoped —
 * never the `SecretStore`, which cannot hold a 4 MiB envelope); and when the
 * relay is asked (every transition to `Connected` — login and each reconnect —
 * and a one-minute `refreshIfDue` tick on the shared cadence).
 *
 * Process-lifetime and kept across sign-out and identity switch: a region is a
 * fact about the device, not the account — so the session teardown calls only
 * [clearSession], which forgets the refresh clock.
 *
 * With no plane open (a Robolectric test, a preview) every reader falls back to
 * the family-only arms, so a default-constructed [ContentPolicyInputs] stays
 * FFI-free.
 */
object RegionPlaneHost {
    /** The app's UI language — the authority's reason is shown in it where the
     *  authority wrote one (the app's own strings are English today; linux's
     *  `UI_LANG`, windows' `UiLang`). */
    const val UI_LANG = "en"

    /** The one-minute tick; the shared cadence (`REFRESH_INTERVAL_SECS`, decided
     *  in Rust) says whether a tick asks. */
    private const val TICK_MS = 60_000L

    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    @Volatile
    private var plane: FfiRegionPlane? = null

    @Volatile
    private var nest: FfiNestClient? = null

    private var tick: Job? = null

    private val _revision = MutableStateFlow(0)

    /**
     * Bumped whenever the plane's answer may have changed — opened, or a relay
     * reply folded. Every surface that composed a verdict or painted the settings
     * section keys its memo on it, so a refresh that changed the record repaints
     * the feed, the post detail and the conversation bubbles.
     */
    val revision: StateFlow<Int> = _revision.asStateFlow()

    /**
     * Open the plane — at launch, ahead of the first fetch — restoring the device
     * record under [configDir]. The leaf's answer: [declaredCode] (`null`: the
     * platform reports no region) and its [source]. Idempotent unless [reopen]:
     * the leaf reads one OS setting per launch. In a test-capable build the shared
     * e2e override replaces the code (keeping its source) inside
     * `FfiRegionPlane.open`, so this needs no seam of its own; [reopen] is for a
     * debug relaunch that re-delivered that override into a surviving process.
     */
    @Synchronized
    fun open(declaredCode: String?, source: FfiRegionSource, configDir: String, reopen: Boolean = false) {
        if (plane != null && !reopen) return
        plane = FfiRegionPlane.open(declaredCode, source, configDir)
        _revision.update { it + 1 }
    }

    /** Repaint every composed verdict — a source beside the plane (the
     *  session nest's rules, [NestRulesHost]) changed what it answers. */
    fun invalidate() {
        _revision.update { it + 1 }
    }

    /** The plane, or `null` before [RegionLeaf] opened it. */
    fun plane(): FfiRegionPlane? = plane

    /** Drop the plane and both witness sets — a host test's reset; the app never
     *  closes the device's plane. */
    @androidx.annotation.VisibleForTesting
    @Synchronized
    internal fun resetForTest() {
        plane = null
        nest = null
        blocked.clear()
        painted.clear()
        _revision.update { it + 1 }
    }

    /**
     * Ask the relay now over the session's connected [client] (login /
     * reconnect) and arm the minute tick. Best-effort: a failed ask writes
     * nothing (§ Fail posture) and never throws into the connection pump.
     */
    fun refresh(client: FfiNestClient) {
        synchronized(this) {
            nest = client
            if (tick == null) {
                tick = scope.launch {
                    while (isActive) {
                        delay(TICK_MS)
                        ask(onlyIfDue = true)
                    }
                }
            }
        }
        scope.launch { ask(onlyIfDue = false) }
    }

    private suspend fun ask(onlyIfDue: Boolean) {
        val p = plane ?: return
        val n = nest ?: return
        try {
            val changed = if (onlyIfDue) p.refreshIfDue(n) else p.refresh(n)
            if (changed) _revision.update { it + 1 }
        } catch (e: Exception) {
            android.util.Log.w("RegionPlaneHost", "region refresh skipped: ${e::class.simpleName}: ${e.message}")
        }
    }

    /** Forget the refresh clock and the session's nest on an identity change, so
     *  the next login asks at once. The plane itself is the device's and stays. */
    fun clearSession() {
        nest = null
        plane?.clearSession()
    }

    /**
     * One item's render decision with the region composed in — the region, the
     * session nest's rules ([NestRulesHost], `nest-rules.md` § The nest source
     * in the engine), the guardian floor and document, and the viewer's own
     * thresholds, strictest-wins in shared Rust, in the ONE composed call. With
     * no plane open the family-only verdict stands and no placeholder exists.
     */
    fun render(
        labels: List<ContentLabelEntry>,
        inputs: ContentPolicyInputs,
        subject: RegionSubject,
    ): RegionRenderDecision {
        // The viewer's own reports are a third input beside the family floor and
        // the region (moderation.md § Corollary): fold them under the region's
        // answer below.
        val reported = inputs.isReported(subject.reportKey, subject.reportAuthor)
        val p = plane ?: return RegionRenderDecision(
            if (reported) "block" else inputs.verdictFor(labels), null, reported,
        )
        val nest = NestRulesHost.source()
        val r = if (nest != null) {
            p.renderWithNest(
                nest.first, nest.second,
                labels, inputs.contentPolicy, inputs.ownSpamPermille, inputs.ownPhishingPermille,
                subject.contentIdHex, subject.authorHex, subject.text, subject.hashtags,
                subject.hasMedia, UI_LANG,
            )
        } else {
            p.render(
                labels, inputs.contentPolicy, inputs.ownSpamPermille, inputs.ownPhishingPermille,
                subject.contentIdHex, subject.authorHex, subject.text, subject.hashtags,
                subject.hasMedia, UI_LANG,
            )
        }
        if (!reported) {
            return RegionRenderDecision(
                r.verdict, r.placeholder, false, r.nestPlaceholder, r.guardianPlaceholder,
            )
        }
        // Reported: the item is blocked whatever the region said. A region BLOCK
        // keeps its placeholder (convention 17: a region block never renders
        // silent); a region COLLAPSE is dropped — the viewer's own act is the more
        // specific explanation, and a reveal must not lift it. windows' Render.
        return RegionRenderDecision(
            "block", r.placeholder?.takeIf { it.verb == "block" }, true,
            // A nest BLOCK keeps its placeholder too (convention 17's nest twin).
            r.nestPlaceholder?.takeIf { it.verb == "block" },
        )
    }

    /** What the settings region section paints; `null` before the plane is open. */
    fun view(): FfiRegionView? = plane?.view()

    // ── Convention 17: "a region Block never renders silent" ─────────────────
    //
    // `blocked` is the set of on-screen items whose composed verdict the region
    // blocks, registered by each surface's item composable from the verdict it
    // computed off its snapshot (independent of which arm painted); `painted` is
    // the set of block placeholders actually composed. An arm that drops the
    // placeholder shows up as `placeholders < blocked`. windows' and apple's
    // `BlockRenderState`, tui's and linux's `region::block_render_json`. Keys name
    // the surface + item; a lazy list composes only what is on screen, so the
    // registrations live and die with the item's composition.
    private val blocked: MutableSet<String> = Collections.synchronizedSet(HashSet())
    private val painted: MutableSet<String> = Collections.synchronizedSet(HashSet())

    /** Register (or clear) an on-screen item's verdict-side walk. */
    fun witnessBlocked(key: String, isBlocked: Boolean) {
        if (isBlocked) blocked.add(key) else blocked.remove(key)
    }

    /** Register (or clear) a composed region BLOCK placeholder. */
    fun witnessPainted(key: String, isPainted: Boolean) {
        if (isPainted) painted.add(key) else painted.remove(key)
    }

    /** The `region_block_render` state field
     *  (`tests/e2e-unified/helpers/frame_invariants.py`). */
    fun blockRenderState(): Pair<Int, Int> = blocked.size to painted.size
}

/**
 * **The platform leaf** of the region content plane on android
 * (`region-blocking.md` § How an app obtains its region's policy) — the one
 * genuinely platform-divergent function: it names the declared region and its
 * source and hands both to the shared plane ([RegionPlaneHost.open]).
 *
 * The answer is the country of the **system** locale the user set
 * (Settings → System → Languages → the first language's region), read from
 * `Resources.getSystem()` so a per-app language choice never stands in for it —
 * user-set, visible, network-free, source `SystemRegion`, never detected from a
 * SIM or the network. The code is handed over verbatim; shared Rust decides
 * whether it is a region code at all.
 *
 * Every build type reads this setting today, a Play-distributed build included:
 * the Play storefront is reachable only through the Play Billing Library, which
 * the app does not carry (`region-blocking.md` § Implementation status today).
 *
 * No in-app override: a knob would make the declaration a choice rather than a
 * fact. In a test-capable build the shared e2e override
 * (`FAUNA_E2E_REGION_DECLARED`) replaces the code inside `FfiRegionPlane.open`,
 * keeping this source.
 */
object RegionLeaf {
    /** Open the device's plane with the leaf's answer. A failure leaves the plane
     *  unopened, which renders every surface exactly as before the plane existed
     *  (the family arms alone). */
    fun openPlane(context: Context, reopen: Boolean = false) {
        try {
            RegionPlaneHost.open(declaredCode(), FfiRegionSource.SYSTEM_REGION, context.filesDir.absolutePath, reopen)
        } catch (e: Exception) {
            android.util.Log.w("RegionLeaf", "region plane not opened: ${e::class.simpleName}: ${e.message}")
        }
    }

    /** The system locale's country, or `null` when none is set. */
    private fun declaredCode(): String? {
        val locales = android.content.res.Resources.getSystem().configuration.locales
        val country = if (locales.isEmpty) null else locales[0].country
        return country?.takeIf { it.isNotBlank() }
    }
}

/**
 * The scorer input a bundled region scorer reads for one item — its id, author,
 * text, hashtags and whether it carries media (`FfiRegionPlane.render`) — plus
 * the report subject's keys. windows' and apple's `RegionSubject`, linux's
 * `region::post_input` / `message_input`.
 */
data class RegionSubject(
    val contentIdHex: String?,
    val authorHex: String?,
    val text: String,
    val hashtags: List<String>,
    val hasMedia: Boolean,
    val reportKey: String? = null,
    val reportAuthor: String? = null,
) {
    companion object {
        // A feed post's subject (`RegionSubject.post`) is an extension under
        // src/noKids: `PostSummary` is a feed type the kids build does not carry.

        /** A conversation bubble, post-decrypt — the zero id stands in for the
         *  author, as on tui, linux, windows and apple. [reportKey] is the
         *  message's plane record DIGEST and [reportAuthor] the sender's actor id;
         *  both `null` for a message with no plane identity (mail, bridged). */
        fun message(id: String, text: String, reportKey: String? = null, reportAuthor: String? = null) =
            RegionSubject(id, null, text, emptyList(), false, reportKey, reportAuthor)
    }
}

/**
 * One item's render decision: the composed [verdict], and the region
 * [placeholder] — or the [nestPlaceholder], when a rule of the nest the item was
 * read through drove the verb — to paint AHEAD of the family arm when that
 * source drove a `block` or `collapse` (`null` → the app's existing arms). One
 * source wins a verdict, so at most one of the two is set. [reported] is the
 * viewer's own report (the "You reported this" notice).
 */
data class RegionRenderDecision(
    val verdict: String,
    val placeholder: FfiRegionPlaceholder?,
    val reported: Boolean = false,
    val nestPlaceholder: FfiNestPlaceholder? = null,
    val guardianPlaceholder: FfiGuardianPlaceholder? = null,
) {
    /** The guardian rule's reason, verbatim, to paint beside the family-policy
     *  frame (`content-policy-blocked-reason`) — `null` for a floor, the viewer's
     *  own threshold, a report or any other source (`nest-rules.md` § The shared
     *  rule editor → *The reason is the member's to read*). */
    val guardianReason: String? get() = guardianPlaceholder?.reason?.takeIf { it.isNotEmpty() }

    /** Whether the region blocks this item — convention 17's verdict side. */
    val isRegionBlocked: Boolean get() = placeholder?.verb == "block"

    /** Whether a nest rule blocks this item — convention 17's nest twin. */
    val isNestBlocked: Boolean get() = nestPlaceholder?.verb == "block"
}
