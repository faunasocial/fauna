package com.fauna.app.core

import com.fauna.ffi.logMessage
import uniffi.fauna_log.LogLevel

/**
 * Bridge from Kotlin shell code into the shared `fauna-log` ring — the android
 * twin of the wasm `logMessage` (observability.md § The emit API). Every shell
 * message in the three capture categories comes through here so it lands on the
 * Settings → Logs page alongside the Rust `tracing` events:
 *
 *  * **displayed** to the user — the [AppMessages] banner funnel logs via this,
 *  * **printed** — call beside each `android.util.Log.*` (logcat stays; additive),
 *  * **meaningfully swallowed** — call in the `catch` of a real wire/RPC/IO error.
 *
 * The `logMessage` FFI re-enters the same process-global subscriber
 * `FaunaApp.installLogging` set up (ring + rolling file + stderr). The call is
 * guarded exactly like FaunaApp's `installLogging`/`installNestIdentityPinStore`
 * so a missing native `.so` (Robolectric, or any emit before `installLogging`)
 * never crashes the producer and logging never throws into the caller's control
 * flow — the native ring is the durable record and dropping a line here is safe.
 *
 * Redaction (observability.md § Persistence & privacy): pass operation names,
 * error metadata (`e.message`), or the localized banner string — NEVER message
 * plaintext or secret material (keys, tokens, claim codes).
 */
object ShellLog {
    fun e(target: String, message: String) = emit(LogLevel.ERROR, target, message)

    fun w(target: String, message: String) = emit(LogLevel.WARN, target, message)

    fun i(target: String, message: String) = emit(LogLevel.INFO, target, message)

    fun d(target: String, message: String) = emit(LogLevel.DEBUG, target, message)

    fun emit(level: LogLevel, target: String, message: String) {
        try {
            logMessage(level, target, message)
        } catch (_: Throwable) {
            // FFI unavailable (Robolectric, or before installLogging) — drop;
            // tests and the producer's control flow stay intact.
        }
    }
}
