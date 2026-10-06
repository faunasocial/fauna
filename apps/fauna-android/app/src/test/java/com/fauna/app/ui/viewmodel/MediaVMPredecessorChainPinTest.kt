package com.fauna.app.ui.viewmodel

import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * Call-site pin: the Media machine `MediaVM.ensureMachine` builds is handed the
 * attested predecessor ids AND the paired predecessor chain, as tui, linux and
 * web hand them (`writer-signed-change-records.md`, rulings (8)(b) and (8)(c)).
 *
 * Without the ids the listing's judge must prove the succession link by a nest
 * lookup per unplaced signed actor; without the **paired** chain a key arrives
 * bare and never opens a row a predecessor signed — the inherited corpus lists
 * but stays sealed. The machine itself is exercised in
 * `libs/fauna-media-machine/tests/media_lifecycle.rs`; this pins only that the
 * seat makes the calls, so removing either line turns it red.
 */
class MediaVMPredecessorChainPinTest {

    private val code: String = run {
        val file = File("src/main/java/com/fauna/app/ui/viewmodel/MediaVM.kt")
        check(file.exists()) { "no ${file.absolutePath} — unit tests must run from the app module dir" }
        // Comments may name the setters to explain them; only code lines count.
        file.readLines()
            .filterNot { it.trim().startsWith("//") || it.trim().startsWith("*") }
            .joinToString("\n")
    }

    @Test
    fun theMediaMachineIsHandedTheAttestedPredecessorIds() {
        assertTrue(
            "MediaVM.ensureMachine must call setPredecessorActorIds(api.attestedPredecessorActorIds())",
            code.contains("api.attestedPredecessorActorIds()") && code.contains("built.setPredecessorActorIds("),
        )
    }

    @Test
    fun theMediaMachineIsHandedThePairedPredecessorChain() {
        assertTrue(
            "MediaVM.ensureMachine must call setPredecessorChain(ids, keys) off api.predecessorChain()",
            code.contains("api.predecessorChain()") && code.contains("built.setPredecessorChain("),
        )
    }
}
