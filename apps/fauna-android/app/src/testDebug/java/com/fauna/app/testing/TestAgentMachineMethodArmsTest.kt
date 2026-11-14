package com.fauna.app.testing

import android.content.Context
import com.fauna.app.core.AccountStores
import com.fauna.app.core.ActorScope
import com.fauna.app.core.AppState
import com.fauna.app.core.LogicalSecretStore
import com.fauna.app.core.MemoryBackend
import com.fauna.app.core.OnboardingHost
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.SessionAccount
import com.fauna.ffi.FfiAccountRegistry
import com.fauna.ffi.provisioning.OverallStatus
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import uniffi.fauna_launch_machine.LaunchPersistence

/**
 * The `call_machine_method` arms that need THIS app's own objects — the ones no
 * shared dispatcher can reach — proven to MOVE the real shared Rust, not merely
 * to be recognised (`docs/goal/behavior/onboarding.md` § E2E bridge contract):
 *
 * - `start_provisioning` / `retry_provisioning`: the per-app provisioning drive
 *   the `live-drive-onboarding-bridge` surface needs (`helpers/live_provision.py`
 *   — without it the sync dispatcher no-ops the name and a live drive sits
 *   `Idle` for its whole budget).
 * - `refuse_secret_writes_for_test`: the shared registry arm handed this app's
 *   own `FfiAccountRegistry` (`registry-refuse-secret-writes`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentMachineMethodArmsTest {

    private fun host(registry: FfiAccountRegistry) = OnboardingHost(
        sessionAccount = mock(SessionAccount::class.java),
        registry = registry,
        launchPersistence = mock(LaunchPersistence::class.java),
        actorScope = mock(ActorScope::class.java),
        accountStores = mock(AccountStores::class.java),
        context = mock(Context::class.java),
    )

    private fun callMachineMethod(method: String, jsonArg: String, appState: AppState) {
        val cmd = JSONObject().apply {
            put("action", "call_machine_method")
            put("method", method)
            put("json_arg", jsonArg)
        }
        runBlocking {
            TestAgent.processCommand(
                "call_machine_method", cmd, appState, mock(SecureStorage::class.java), null,
            )
        }
    }

    private fun refusal(appState: AppState): Any? =
        TestAgent.serializeState(appState, mock(SecureStorage::class.java), null, appState.messages)
            .opt("messages")
            .takeUnless { it == JSONObject.NULL }

    @After
    fun unwire() {
        // `TestAgent` is a process-global `object`: targets left set here would
        // turn the null-host refusal pins in other classes into something else.
        TestAgent.setMachineMethodTargetsForTest(null, null)
    }

    @Test
    fun startProvisioningRunsTheSharedOrchestratorAndAcksAtOnce() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        val host = host(registry)
        TestAgent.setMachineMethodTargetsForTest(host, registry)
        assertEquals(OverallStatus.IDLE, host.machine.provisioningSnapshot().overall)
        val appState = AppState()

        callMachineMethod("start_provisioning", "", appState)

        assertNull("the arm acks without a refusal", refusal(appState))
        // The drive was launched, not awaited: its snapshot leaves Idle (it runs,
        // or fails on the empty wizard state) — the sync dispatcher's no-op left
        // it Idle for ever.
        val deadline = System.currentTimeMillis() + 10_000
        while (host.machine.provisioningSnapshot().overall == OverallStatus.IDLE &&
            System.currentTimeMillis() < deadline
        ) {
            Thread.sleep(50)
        }
        assertNotEquals(
            "start_provisioning must reach run_provisioning",
            OverallStatus.IDLE,
            host.machine.provisioningSnapshot().overall,
        )
        host.machine.cancelProvisioning()
    }

    @Test
    fun startProvisioningBeforeTheHostIsWiredIsRefusedNotDropped() {
        val appState = AppState()
        callMachineMethod("retry_provisioning", "", appState)
        val shown = refusal(appState)?.toString()
        assertTrue("a null host must be loud: $shown", shown!!.contains("retry_provisioning"))
    }

    @Test
    fun refuseSecretWritesReachesThisAppsOwnRegistry() {
        val registry = FfiAccountRegistry(LogicalSecretStore(MemoryBackend()))
        TestAgent.setMachineMethodTargetsForTest(null, registry)
        val appState = AppState()

        callMachineMethod("refuse_secret_writes_for_test", """{"refuse":true}""", appState)

        assertNull("the registry arm handles the name", refusal(appState))
        val refused = runCatching { registry.addAccount(SECRET, "https://nest.example", null) }
        assertTrue("an identity write must be refused while the fault is armed", refused.isFailure)

        callMachineMethod("refuse_secret_writes_for_test", """{"refuse":false}""", appState)
        registry.addAccount(SECRET, "https://nest.example", null)
        assertEquals("lifted, the same write lands", 1, registry.list().size)
    }

    private companion object {
        val SECRET = "01".repeat(32)
    }
}
