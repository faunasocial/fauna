package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.E2eBackupAuditPoke
import com.fauna.app.core.SecureStorage
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config

/**
 * `backup_audit_run_now` and `family_notify_check_now` (convention 14's
 * `run_now` pokes; the `backup-audit-run-now` / `family-notify-check-now`
 * surface-registry entries) each have their OWN arm: refused with their own
 * reason when the collaborator is absent, never the catch-all "no arm".
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentAuditNotifyPokeTest {

    private fun refusal(action: String, payload: JSONObject = JSONObject()): String? {
        val appState = AppState()
        val cmd = JSONObject().put("action", action).put("payload", payload)
        runBlocking {
            TestAgent.processCommand(action, cmd, appState, mock(SecureStorage::class.java), null)
        }
        val state = TestAgent.serializeState(appState, mock(SecureStorage::class.java), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    @Test
    fun auditRunNowRefusesNamingItselfWhenNoBackupsPageIsBuilt() {
        val shown = refusal("backup_audit_run_now", JSONObject().put("now_offset_secs", 3600))
        assertNotNull("a poke with no Backups page must surface, not drop", shown)
        assertTrue("fell through to the catch-all: $shown", !shown!!.contains("no arm"))
        assertTrue("must name the command and the missing page: $shown",
            shown.contains("backup_audit_run_now") && shown.contains("Backups page"))
    }

    @Test
    fun notifyCheckNowRefusesNamingItselfBeforeTheStoreIsWired() {
        val shown = refusal("family_notify_check_now")
        assertNotNull("a poke with no store must surface, not drop", shown)
        assertTrue("fell through to the catch-all: $shown", !shown!!.contains("no arm"))
        assertTrue("must name the command and the store: $shown",
            shown.contains("family_notify_check_now") && shown.contains("FamilyNotifyStore"))
    }

    /** The seam the audit arm drives: a request reaches a collecting page view
     *  model exactly once, and is refused (not queued) when none collects. */
    @Test
    fun theAuditPokeReachesACollectorAndIsRefusedWithoutOne() = runBlocking {
        assertFalse(E2eBackupAuditPoke.isListening())
        assertFalse(E2eBackupAuditPoke.request())

        val got = async(start = CoroutineStart.UNDISPATCHED) {
            E2eBackupAuditPoke.requests.first()
        }
        yield()
        assertTrue(E2eBackupAuditPoke.isListening())
        assertTrue(E2eBackupAuditPoke.request())
        assertEquals(Unit, got.await())
    }
}
