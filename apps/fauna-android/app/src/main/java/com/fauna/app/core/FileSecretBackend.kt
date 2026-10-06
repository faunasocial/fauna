package com.fauna.app.core

import org.json.JSONObject
import java.io.File

/**
 * E2E-only [SecretBackend]: a flat `{native_key: value}` JSON map on disk,
 * selected instead of [SharedPrefsSecretBackend] when the app is launched with
 * an e2e credential file ([com.fauna.app.testing.TestAgent.credentialFilePath]).
 * The Android twin of windows' `FaunaApp.Core/Services/SecretBackend.cs::FileSecretBackend`
 * (same flat-map shape, same `tests/common/accounts.py::build_registry_seed`
 * producer) — this is what lets a test pre-seed a whole multi-account registry
 * before launch with no app code.
 *
 * Unlike windows/linux, the file is written **on-device by the bridge
 * instrumentation itself** (same process/UID as the app under test — see
 * `androidTest/bridge/AppLauncher.kt`) rather than pushed across the host↔device
 * boundary, so there is no directory/namespace split: one fixed path per launch.
 *
 * Corrupt/absent file degrades to "no credentials" (→ onboarding), never a
 * crash — same contract as [SharedPrefsSecretBackend] and every other
 * platform's file backend.
 */
class FileSecretBackend(private val file: File) : SecretBackend {
    private val lock = Any()

    private fun read(): MutableMap<String, String> {
        return try {
            if (!file.exists()) return mutableMapOf()
            val obj = JSONObject(file.readText())
            val map = mutableMapOf<String, String>()
            val keys = obj.keys()
            while (keys.hasNext()) {
                val k = keys.next()
                map[k] = obj.getString(k)
            }
            map
        } catch (e: Exception) {
            // A corrupt/absent credential file degrades to "no credentials",
            // never a crash. Same as SharedPrefsSecretBackend's read contract.
            mutableMapOf()
        }
    }

    private fun write(map: Map<String, String>) {
        try {
            file.parentFile?.mkdirs()
            file.writeText(JSONObject(map).toString())
        } catch (e: Exception) {
            // Best-effort: matches every platform's file-backend write contract
            // (windows' FileSecretBackend.Write swallows too).
        }
    }

    override fun get(key: String): String? = synchronized(lock) { read()[key] }

    override fun set(key: String, value: String) = synchronized(lock) {
        val map = read()
        map[key] = value
        write(map)
    }

    override fun delete(key: String) = synchronized(lock) {
        val map = read()
        if (map.remove(key) != null) write(map)
    }
}
