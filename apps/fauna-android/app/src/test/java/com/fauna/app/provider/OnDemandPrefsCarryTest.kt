package com.fauna.app.provider

import com.fauna.ffi.FfiOnDemandPrefsStore
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File

/**
 * The on-demand toggle across an identity succession on android
 * (`sync-agent.md` § The per-actor scope follows the account across a
 * succession, the android clause): the store sits in each account's own
 * scope, so the successor's first open renames the nearest predecessor's file
 * into its scope and re-keys it through the shared
 * `FfiOnDemandPrefsStore.carryFromPredecessors`.
 *
 * FFI-touching -> runs green via `just android-host-test`. The shared-Rust
 * twin is `fauna_ffi::on_demand_presence`'s
 * `the_succession_carry_persists_through_the_ffi_store`.
 */
class OnDemandPrefsCarryTest {

    @get:Rule val tmp = TemporaryFolder()

    private val pred = "aa".repeat(32)
    private val mid = "cc".repeat(32)
    private val succ = "bb".repeat(32)

    private fun path(actorHex: String) = File(tmp.root, "$actorHex/sync/$ON_DEMAND_PREFS_FILE")

    private fun id(actorHex: String) = "local%3A1@$actorHex"

    @Test
    fun aRootToggledOffBeforeTheSuccessionStaysOffAfterIt() {
        path(pred).parentFile!!.mkdirs()
        FfiOnDemandPrefsStore(path(pred).path).setEnabled(id(pred), false)

        val store = carriedOnDemandPrefs(succ, listOf(pred), ::path)

        assertFalse("the off toggle must survive the succession", store.isEnabled(id(succ)))
        assertFalse("carried by a rename, never a copy", path(pred).exists())
        assertFalse(
            "a reopened store still reads it off",
            FfiOnDemandPrefsStore(path(succ).path).isEnabled(id(succ)),
        )
    }

    @Test
    fun theNearestHopThatHoldsAStoreIsCarried() {
        path(pred).parentFile!!.mkdirs()
        FfiOnDemandPrefsStore(path(pred).path).setEnabled(id(pred), false)

        // `mid` is the nearer hop but never held a store: the far one carries.
        val store = carriedOnDemandPrefs(succ, listOf(mid, pred), ::path)

        assertFalse(store.isEnabled(id(succ)))
    }

    @Test
    fun aSuccessorStoreAlreadyPresentStaysAndThePredecessorsIsLeft() {
        path(pred).parentFile!!.mkdirs()
        FfiOnDemandPrefsStore(path(pred).path).setEnabled(id(pred), false)
        path(succ).parentFile!!.mkdirs()
        FfiOnDemandPrefsStore(path(succ).path).setEnabled("local%3A2@$succ", false)

        val store = carriedOnDemandPrefs(succ, listOf(pred), ::path)

        assertFalse("the successor's own choice stays", store.isEnabled("local%3A2@$succ"))
        assertTrue("no merge: the predecessor's toggle is not pulled in", store.isEnabled(id(succ)))
        assertTrue("never destroyed", path(pred).exists())
    }

    @Test
    fun anAccountThatNeverSucceededOpensItsOwnStore() {
        val store = carriedOnDemandPrefs(succ, emptyList(), ::path)

        assertTrue(store.isEnabled(id(succ)))
        assertFalse("nothing written on a read", path(succ).exists())
    }
}
