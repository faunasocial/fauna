package com.fauna.app.testing

import android.content.Context
import com.fauna.app.core.AppState
import uniffi.fauna_conversations.ConversationsManager

/**
 * Shipping-flavor twin of the debug `TestAgent` (`src/debug/java/.../TestAgent.kt`),
 * compiled into **both** `release` and `storeSafe` (wired in `build.gradle.kts`).
 *
 * It lived in `src/release/java` until 2026-08-16, when `storeSafe` — the
 * App-Store escape hatch's flavor (`dynamic-features.md` § The App-Store escape
 * hatch) — became a second shipping build type. That flavor cannot take
 * `src/release/java` wholesale, because the same directory holds the release
 * flavor's staged UniFFI bindings; so this twin moved to a directory of its own
 * that every shipping flavor takes, and a third one gets it for free.
 *
 * testing.md § Cross-app e2e conventions point 15: the automation surface is
 * compiled out of release artifacts. The real agent — its bridge poll loop, its
 * command dispatch, and every `*ForTest` UniFFI seam it drives — lives in the
 * `debug` source set and is therefore never compiled into a release APK at all.
 *
 * This object exists only so the handful of `src/main` production call sites
 * that *mention* the agent still compile in the release variant, with the same
 * signatures and inert values:
 *
 *   * [com.fauna.app.MainActivity] writes [credentialFilePath] /
 *     [isRealConversationsActive] and calls [start], all inside
 *     `if (BuildConfig.DEBUG)` blocks that R8 folds away;
 *   * [com.fauna.app.di.LaunchModule] reads [credentialFilePath] to decide
 *     whether the e2e file-backed secret store replaces the real one — `null`
 *     here, so it never can; [com.fauna.app.core.AccountReauth] reads it for
 *     the re-auth verdict seam's directory, so the real prompt always shows;
 *   * [com.fauna.app.core.ActorScope] and [com.fauna.app.core.AccountReauth]
 *     call [recordSessionTeardown] / [recordActivationGesture] ungated — the
 *     e2e negative-assert counters, plain no-ops here;
 *   * [com.fauna.app.core.conversations.ConversationsManagerHost] reads
 *     [isE2EActive] / [isRealConversationsActive] and calls
 *     [installMockBackendsIfE2E];
 *   * [com.fauna.app.core.conversations.MessageBannerObserver] calls
 *     [bannerPassStarted] / [recordFiredBanner] / [bannerPassCompleted] on
 *     every banner tick — ungated, so these are the plain no-ops that keep the
 *     firing path identical in every flavor;
 *   * `ApiClient.startConnectionStatePump` calls [observeConnectionReport] on
 *     every connection-state value — ungated, a plain no-op here;
 *   * [com.fauna.app.ui.navigation.FaunaNavHost] writes [focusManager] inside
 *     a `BuildConfig.DEBUG`-gated `SideEffect`;
 *   * `ConversationDetailScreen`'s reaction picker writes [moreReactionPick]
 *     inside a `BuildConfig.DEBUG`-gated `DisposableEffect`.
 *
 * This is the same "gated-real plus same-signature no-op twin" shape linux and
 * tui use for `start_test_agent_if_enabled` / `start_if_enabled`. Keeping the
 * constants `val`s with literal initializers is load-bearing beyond tidiness:
 * it lets the Kotlin compiler and R8 fold `if (TestAgent.isE2EActive)` to
 * `false` and drop the guarded branches outright.
 *
 * ⚠ Anything added to the debug agent that a `src/main` file calls must be
 * mirrored here, or the release variant stops compiling. That compile error is
 * the intended feedback: a production file reaching for the automation surface
 * is exactly what this split exists to catch.
 */
object TestAgent {
    /** Always false in release — the agent that would latch it does not exist here. */
    const val isE2EActive: Boolean = false

    /**
     * Never true in release. `var` rather than `const` because
     * [com.fauna.app.MainActivity] assigns it inside its `BuildConfig.DEBUG`
     * branch, which must still type-check when that branch is dead.
     */
    var isRealConversationsActive: Boolean = false

    /**
     * Never read in release — there is no `focus_move` dispatch here.
     * Mirrors the debug twin's `focusManager` only so
     * [com.fauna.app.ui.navigation.FaunaNavHost]'s `BuildConfig.DEBUG`-gated
     * `SideEffect` still type-checks in the release variant.
     */
    var focusManager: androidx.compose.ui.focus.FocusManager? = null

    /**
     * Never read in release — there is no `type_text` dispatch here. Mirrors the
     * debug twin's `moreReactionPick` only so `ConversationDetailScreen`'s
     * `BuildConfig.DEBUG`-gated registration still type-checks in the release
     * variant.
     */
    var moreReactionPick: ((String) -> Unit)? = null

    /**
     * Always null in release, so `LaunchModule.provideSecretBackend` always
     * picks the real encrypted store. `var` for the same reason as
     * [isRealConversationsActive].
     */
    var credentialFilePath: String? = null

    /** No-op twin: there is no bridge poll loop in a release build. */
    @Suppress("UNUSED_PARAMETER")
    fun start(context: Context, bridgeUrl: String, appState: AppState) {
    }

    /** No-op twin: `installMockBackendsForTest()` is not exported by the
     *  production-flavored bindings, and a release build has no E2E mode to
     *  install mocks for. */
    @Suppress("UNUSED_PARAMETER")
    fun installMockBackendsIfE2E(manager: ConversationsManager) {
    }

    /** No-op twin: the fired-banner log's recorders are `test-helpers` UniFFI
     *  exports, absent from the production-flavored bindings. */
    fun bannerPassStarted() {
    }

    /** No-op twin — see [bannerPassStarted]. */
    @Suppress("UNUSED_PARAMETER")
    fun recordFiredBanner(threadId: String, label: String) {
    }

    /** No-op twin — see [bannerPassStarted]. */
    fun bannerPassCompleted() {
    }

    /** No-op twin: the `connection_reports` counter is a `test-helpers` UniFFI
     *  export, absent from the production-flavored bindings. */
    @Suppress("UNUSED_PARAMETER")
    fun observeConnectionReport(state: com.fauna.ffi.FfiConnectionState) {
    }

    /** No-op twin: the e2e session-generation counter has no reader in a
     *  release build — the agent that would publish it does not exist here. */
    fun recordSessionTeardown() {
    }

    /** No-op twin — see [recordSessionTeardown]. */
    fun recordActivationGesture() {
    }
}
