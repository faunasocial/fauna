package com.fauna.app.core

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Call-site pins: the two long-lived hosts the android app builds are handed
 * the account's predecessor chain, as the Apple app hands it
 * (`writer-signed-change-records.md` § Writer-signed change records, ruling
 * (8)(b) source (ii) and (8)(c)).
 *
 * - `ApiClient.syncEngineHost` passes the registry OBJECT; Rust runs the one
 *   walk and feeds the reader's owner chain.
 * - `FfiOnDemandSource` (the owned-tree host, a capability construction with no
 *   registry argument) passes the already-paired `FfiPredecessorChain`, never a
 *   zip of two lists.
 *
 * Both arguments are trailing and optional in the binding, so removing either
 * call-site argument still compiles; these pins turn that red. The behaviour is
 * exercised in `libs/fauna-ffi` (`accounts_registry.rs`, `sync_engine_host.rs`).
 */
class EngineHostsPredecessorChainPinTest {

    /** Code lines of [path] from the line containing [anchor] to the call's closing paren. */
    private fun callBody(path: String, anchor: String): String {
        val file = File(path)
        check(file.exists()) { "no ${file.absolutePath} — unit tests must run from the app module dir" }
        // Comments may name the arguments to explain them; only code lines count.
        val lines = file.readLines()
            .filterNot { it.trim().startsWith("//") || it.trim().startsWith("*") }
        val start = lines.indexOfFirst { it.contains(anchor) }
        check(start >= 0) { "$anchor not found in $path" }
        val rest = lines.drop(start)
        val end = rest.indexOfFirst { it.trim().startsWith(")") || it.trimEnd().endsWith(")") }
        return rest.take(if (end < 0) rest.size else end + 1).joinToString("\n")
    }

    @Test
    fun theSyncEngineHostIsHandedTheAccountRegistry() {
        val call = callBody("src/main/java/com/fauna/app/core/ApiClient.kt", "client.syncEngineHost(")
        assertTrue(
            "ApiClient.syncEngineHost must pass accountStores.accountRegistry to client.syncEngineHost",
            call.contains("accountStores.accountRegistry"),
        )
    }

    @Test
    fun theOnDemandOwnedTreeHostIsHandedThePairedPredecessorChain() {
        val call = callBody(
            "src/main/java/com/fauna/app/provider/FfiOnDemandSource.kt",
            "FfiFileProviderHost.appDeadOwnedTree(",
        )
        assertTrue(
            "FfiOnDemandSource must pass accountStores.predecessorChain(acct.actorHex) to appDeadOwnedTree",
            call.contains("accountStores.predecessorChain(acct.actorHex)"),
        )
    }
}
