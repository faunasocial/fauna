package com.fauna.app.core

import android.content.Context
import android.content.ContextWrapper
import android.util.Log
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import com.fauna.app.testing.TestAgent
import java.io.File
import kotlin.coroutines.resume
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Android twin of FaunaKit's `AccountReauth`
 * (`apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/AccountReauth.swift`) — the
 * re-auth gate for the multi-account switch (`long-term-store.md` § Multi-account
 * evolution → *Per-account re-auth*, Stage 2).
 *
 * Activating an account whose `require_confirm_to_activate` flag is set demands a
 * native re-auth confirmation before the switch. The platform surface is the
 * native [BiometricPrompt] (biometric with device-credential fallback) — an OS
 * dialog that carries no test id, so android renders no ui.yaml element for it
 * (unlike the in-app `account-activate-reauth-prompt` linux/web/tui use).
 *
 * **Fail-closed:** every non-success — biometric unavailable, no host activity,
 * user cancel, lockout, a prompt that fails to present — reads as a decline
 * (`false`); the caller then does nothing (a pure no-op switch). Only an explicit
 * success returns `true`.
 */
object AccountReauth {
    private const val TAG = "AccountReauth"
    private const val REAUTH_RESULT_FILE = "reauth-result"
    private const val APPROVE_VERDICT = "approve"

    /**
     * Confirm activation of a require-confirm account.
     *
     * @param activity the host [FragmentActivity] for [BiometricPrompt]; `null`
     *   fails closed (there is no way to present the OS prompt).
     * @param title    prompt title (`reauth_prompt_title`).
     * @param subtitle prompt subtitle (`reauth_reason`).
     * @return `true` iff the user approved; every other outcome is `false`.
     */
    suspend fun confirmActivation(
        activity: FragmentActivity?,
        title: String,
        subtitle: String,
    ): Boolean {
        // E2e seam: when the app was launched on an e2e credential file it reads
        // `reauth-result` beside that file instead of prompting — android's
        // `{FAUNA_E2E_CREDENTIAL_DIR}/reauth-result` (FaunaKit / windows
        // `AccountReauth`), where the "credential dir" is the filesDir the
        // bridge writes both files into (`POST /reauth-result`). Read per
        // prompt, so one app session exercises both the approve and decline
        // arms without a relaunch. Null in release (the `noAgent` twin).
        e2eVerdict(e2eSeamDir(TestAgent.credentialFilePath))?.let { return it }

        val host = activity ?: run {
            Log.w(TAG, "[account-switch] re-auth has no FragmentActivity host — declining, fail-closed")
            return false
        }
        // biometric OR device credential (PIN/pattern/password) — the android
        // analogue of apple's `.deviceOwnerAuthentication`.
        val authenticators = BiometricManager.Authenticators.BIOMETRIC_STRONG or
            BiometricManager.Authenticators.DEVICE_CREDENTIAL
        val availability = BiometricManager.from(host).canAuthenticate(authenticators)
        if (availability != BiometricManager.BIOMETRIC_SUCCESS) {
            Log.w(
                TAG,
                "[account-switch] re-auth unavailable (canAuthenticate=$availability) — declining, fail-closed",
            )
            return false
        }
        return suspendCancellableCoroutine { cont ->
            try {
                val executor = ContextCompat.getMainExecutor(host)
                val prompt = BiometricPrompt(
                    host,
                    executor,
                    object : BiometricPrompt.AuthenticationCallback() {
                        override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                            if (cont.isActive) cont.resume(true)
                        }

                        override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                            // User cancel, lockout, no-hardware — every terminal
                            // non-success is a decline; the switch is a pure no-op.
                            Log.i(TAG, "[account-switch] re-auth declined (error $errorCode: $errString)")
                            if (cont.isActive) cont.resume(false)
                        }

                        override fun onAuthenticationFailed() {
                            // A single non-matching attempt; the prompt stays up for
                            // a retry. Do NOT resolve — wait for success or a
                            // terminal error.
                        }
                    },
                )
                val info = BiometricPrompt.PromptInfo.Builder()
                    .setTitle(title)
                    .setSubtitle(subtitle)
                    .setAllowedAuthenticators(authenticators)
                    .build()
                prompt.authenticate(info)
            } catch (e: Exception) {
                Log.w(TAG, "[account-switch] re-auth prompt failed to present — declining, fail-closed", e)
                if (cont.isActive) cont.resume(false)
            }
        }
    }

    /**
     * The e2e verdict, factored out so it is unit-testable with no emulator, no
     * activity, and no biometric hardware.
     *
     * @return `null` when no seam is configured (production → prompt for real);
     *   `true` only for the literal `"approve"`; `false` for any other content or
     *   an absent/unreadable file (absent reads as decline — fail-closed, the
     *   strictest arm and the one the apple decline journey drives).
     */
    internal fun e2eVerdict(credDir: String?): Boolean? {
        if (credDir.isNullOrEmpty()) return null
        val verdict = runCatching {
            File(credDir, REAUTH_RESULT_FILE).readText().trim()
        }.getOrNull()
        return verdict == APPROVE_VERDICT
    }

    /**
     * The seam's directory: the one holding the e2e credential file
     * ([TestAgent.credentialFilePath], set from the launch intent), or `null`
     * when the app was not launched on one — production, which then prompts
     * for real. Android's analogue of the other apps' `FAUNA_E2E_CREDENTIAL_DIR`,
     * which no android launch sets: an intent-launched app has no environment
     * to inherit it from.
     */
    internal fun e2eSeamDir(credentialFilePath: String?): String? {
        if (credentialFilePath.isNullOrEmpty()) return null
        return File(credentialFilePath).parent
    }

    /**
     * Run one switcher-row activation gesture and count its completion on the
     * e2e `activation_gestures` counter (`fauna_e2e_agent::ACTIVATION_GESTURES_KEY`)
     * — in a `finally`, so a declined, refused or failed gesture counts too. A
     * declined re-auth changes nothing on screen by design (the prompt is the
     * native dialog, and under the e2e seam not even that), so this count is
     * the only observable that the gesture has FINISHED. Inline so [block] may
     * suspend (the re-auth prompt) when called from a coroutine. A no-op count
     * in release (the `noAgent` twin).
     */
    inline fun <T> activationGesture(block: () -> T): T {
        try {
            return block()
        } finally {
            TestAgent.recordActivationGesture()
        }
    }
}

/** Unwrap a Compose [Context] to its host [FragmentActivity], or `null`. */
tailrec fun Context.findFragmentActivity(): FragmentActivity? = when (this) {
    is FragmentActivity -> this
    is ContextWrapper -> baseContext.findFragmentActivity()
    else -> null
}
