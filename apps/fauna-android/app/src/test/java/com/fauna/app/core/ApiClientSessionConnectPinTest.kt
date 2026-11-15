package com.fauna.app.core

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Call-site pin: the session client `ApiClient` builds is brought up with the
 * supervised `ensureConnected()`, never the waiting `connect()`
 * (`transport-connection.md` § Connection lifecycle).
 *
 * `connect()` is a 30 s wait that throws with its supervisor still running; the
 * session mounted behind it, which on windows missed the reach-hint dial
 * journey's 90 s budget against a dark hint domain. `ensureConnected()` starts
 * the supervisor and returns. The transport itself is exercised in
 * `libs/fauna-client`; this pins only that the seat makes the supervised call.
 */
class ApiClientSessionConnectPinTest {

    private val code: String = run {
        val file = File("src/main/java/com/fauna/app/core/ApiClient.kt")
        check(file.exists()) { "no ${file.absolutePath} — unit tests must run from the app module dir" }
        file.readLines()
            .filterNot { it.trim().startsWith("//") || it.trim().startsWith("*") }
            .joinToString("\n")
    }

    @Test
    fun theSessionClientIsBroughtUpSupervised() {
        assertTrue(
            "ApiClient must bring the session client up with client.ensureConnected()",
            code.contains("client.ensureConnected()"),
        )
        assertFalse(
            "ApiClient must not await the waiting client.connect() on the session path",
            code.contains("client.connect()"),
        )
    }
}
