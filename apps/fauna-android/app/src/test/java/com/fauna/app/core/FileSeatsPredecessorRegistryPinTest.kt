package com.fauna.app.core

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Call-site pin: the conversations session and the served-set walk are handed the
 * account registry, as windows (`NestRpcClient`) and Apple (`APIClient`) hand it.
 *
 * Shared Rust resolves the account's attested predecessor ids off the registry,
 * for this session's own actor, and feeds them to the content-index reader, the
 * launch folder resume and the served-set walk — so a row a retired identity
 * signed is admitted with no `fauna.recovery.succession.lookup` round trip
 * (`writer-signed-change-records.md` ruling (8)(b) source (ii)). Both faces bind a
 * real `FfiNestClient`, so the hand-in is pinned over the source; removing either
 * turns it red.
 */
class FileSeatsPredecessorRegistryPinTest {

    private fun code(path: String): String {
        val file = File(path)
        check(file.exists()) { "no ${file.absolutePath} — unit tests must run from the app module dir" }
        // Comments may name the registry to explain it; only code lines count.
        return file.readLines()
            .filterNot { it.trim().startsWith("//") || it.trim().startsWith("*") }
            .joinToString("\n")
    }

    @Test
    fun theConversationsSessionIsHandedTheRegistry() {
        val host = code("src/main/java/com/fauna/app/core/conversations/ConversationsManagerHost.kt")
        val start = host.indexOf("nestClient.conversationsSession(")
        assertTrue("ConversationsManagerHost no longer builds the session via conversationsSession(", start >= 0)
        val call = host.substring(start, host.indexOf("\n                )", start))
        assertTrue("conversationsSession(...) must be handed the `accounts` registry", call.contains("accounts"))
        val api = code("src/main/java/com/fauna/app/core/ApiClient.kt")
        val caller = api.indexOf("conversationsManagerHost.startConversationsSession(")
        assertTrue("ApiClient no longer starts the conversations session", caller >= 0)
        assertTrue(
            "startConversationsSession must be handed accountStores.accountRegistry",
            api.substring(caller, api.indexOf("\n        )", caller)).contains("accountStores.accountRegistry"),
        )
    }

    @Test
    fun theServeSetWalkIsHandedTheRegistry() {
        val api = code("src/main/java/com/fauna/app/core/ApiClient.kt")
        val start = api.indexOf("com.fauna.ffi.foldersServeSet(")
        assertTrue("ApiClient.serveSetFolder no longer calls foldersServeSet(", start >= 0)
        val call = api.substring(start, api.indexOf("\n        )", start))
        assertTrue("foldersServeSet(...) must be handed accountStores.accountRegistry", call.contains("accountStores.accountRegistry"))
    }
}
