package com.fauna.app.ui.screen.onboarding

import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.ShellLog

/**
 * The shared confirm-identity moment's loud-failure arm, for the three screens
 * that confirm an identity (created, import, recovery entry) — long-term-store.md
 * § Save-failure handling; linux prior art `commit_confirmed_identity`.
 *
 * Commits through `confirmIdentity` (both modes; in append mode it writes
 * nothing). On failure the machine's `identityCommitFailed` returns the wizard
 * to the screen the person confirmed on with the error on `error-message`, so
 * nobody reaches handle entry holding a key stored nowhere; confirming again is
 * the retry. [persistFailedTemplate] is the resolved
 * `onboarding.session_error.persist_successor` string, `{message}` filled here.
 *
 * Returns whether the identity is committed — a caller with a line of its own
 * paints it only then, so it never covers this one.
 */
internal fun commitConfirmedIdentity(
    host: OnboardingHost,
    registry: com.fauna.ffi.FfiAccountRegistry,
    secret: String,
    persistFailedTemplate: String,
    tag: String,
): Boolean =
    try {
        registry.confirmIdentity(secret, host.appendMode)
        true
    } catch (e: Exception) {
        ShellLog.w(tag, "persist_confirmed_identity (registry commit + read-back) failed: ${e.message}")
        host.machine.identityCommitFailed(persistFailedTemplate.replace("{message}", e.message ?: ""))
        false
    }
