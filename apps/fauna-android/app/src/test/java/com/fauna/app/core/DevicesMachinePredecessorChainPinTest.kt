package com.fauna.app.core

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Call-site pin: the Devices machine `ApiClient.buildDevicesMachine` builds is
 * handed the paired predecessor chain, as windows (`BuildDevicesMachineAsync`)
 * and Apple (`APIClient.devicesMachine`) hand it.
 *
 * A succession re-points an owned set to the successor and re-seals nothing, so
 * a wizard-made set's name still rests under the PREDECESSOR's owner root; with
 * the session's own key alone the successor's Folders page drops every set it
 * inherited (`succession-aftermath.md` § Re-key scope). The builder is the one
 * seat both consumers (`DevicesVM`, `TaskDelegationVM`) go through, so the call
 * lives there. The machine itself is exercised in `libs/fauna-devices-machine`;
 * this pins only that the seat makes the call, so removing it turns it red.
 */
class DevicesMachinePredecessorChainPinTest {

    private val code: String = run {
        val file = File("src/main/java/com/fauna/app/core/ApiClient.kt")
        check(file.exists()) { "no ${file.absolutePath} — unit tests must run from the app module dir" }
        // Comments may name the setter to explain it; only code lines count.
        val lines = file.readLines()
            .filterNot { it.trim().startsWith("//") || it.trim().startsWith("*") }
        val start = lines.indexOfFirst { it.contains("fun buildDevicesMachine(") }
        check(start >= 0) { "ApiClient.buildDevicesMachine not found" }
        // The builder's own body: up to the next member's `fun ` declaration.
        val rest = lines.drop(start + 1)
        val end = rest.indexOfFirst { it.trimStart().startsWith("fun ") }
        rest.take(if (end < 0) rest.size else end).joinToString("\n")
    }

    @Test
    fun theDevicesMachineIsHandedThePairedPredecessorChain() {
        assertTrue(
            "ApiClient.buildDevicesMachine must call setPredecessorChain(ids, keys) off predecessorChain()",
            code.contains("predecessorChain()") && code.contains(".setPredecessorChain("),
        )
    }
}
