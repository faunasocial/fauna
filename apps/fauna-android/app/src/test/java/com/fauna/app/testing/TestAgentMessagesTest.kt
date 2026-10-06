package com.fauna.app.testing

import com.fauna.app.core.AppMessages
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The state protocol's `messages` gate ([TestAgent.messagesJson]).
 *
 * `e2e-conventions.md` convention 2's rider, obligation (b): whatever
 * `error_text()`/`has_error()` resolve to must be fed by the *real* error. The
 * shared helper (`actions/__init__.py::_message_from_state`) falls back to the
 * `error-message` element only when the `messages` key is **absent** — a key
 * present with a null value resolves to `""` and kills the fallback forever.
 *
 * Android's funnel ([AppMessages]) is not its only error surface: page-scoped
 * ViewModels (`AdminDnsVM.error`, `AdminNestVM.error`,
 * `MutedWordsVM.errorMessage`, …) render into the page's own `error-message`
 * element without publishing to the funnel. So a silent funnel must serialize
 * as `null`, deferring to the element — the shape web ratified for the same
 * defect (`tests/e2e-unified/web-bridge/agent.js`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentMessagesTest {

    /**
     * The load-bearing case: a silent funnel serializes as `null`, NOT as
     * `{error: null, warning: null, info: null}`. Mutating [TestAgent.messagesJson]
     * to always return the object reds exactly here — and that mutation is
     * precisely the bug this test exists to prevent, because with the object
     * always present every page-local error on android read back as `""`.
     */
    @Test
    fun silentFunnelSerializesAsNullSoTheElementFallbackStaysLive() {
        assertSame(JSONObject.NULL, TestAgent.messagesJson(AppMessages()))
    }

    @Test
    fun anErrorOnTheFunnelSerializesAsAnObject() {
        val messages = AppMessages().apply { showError("services.update failed") }
        val json = TestAgent.messagesJson(messages) as JSONObject
        assertEquals("services.update failed", json.getString("error"))
        assertEquals(JSONObject.NULL, json.get("warning"))
        assertEquals(JSONObject.NULL, json.get("info"))
    }

    /**
     * A warning or info alone is still "something to say" — the object must
     * appear, or `warning_text()`/`info_text()` would fall through to elements
     * that are not the funnel's.
     */
    @Test
    fun aWarningAloneStillProducesAnObject() {
        val messages = AppMessages().apply { showWarning("dkim publish is stale") }
        val json = TestAgent.messagesJson(messages) as JSONObject
        assertEquals(JSONObject.NULL, json.get("error"))
        assertEquals("dkim publish is stale", json.getString("warning"))
    }

    @Test
    fun anInfoAloneStillProducesAnObject() {
        val messages = AppMessages().apply { showInfo("saved") }
        val json = TestAgent.messagesJson(messages) as JSONObject
        assertEquals("saved", json.getString("info"))
    }

    /**
     * Convention 11: a refused agent command outranks the page's own error, so
     * it rides `messages.error` rather than only a banner.
     */
    @Test
    fun aRefusedAgentCommandOutranksThePagesOwnError() {
        val messages = AppMessages().apply {
            showError("page error")
            reportRefusedAgentCommand("frobnicate")
        }
        val json = TestAgent.messagesJson(messages) as JSONObject
        assertEquals(
            "test agent refused command \"frobnicate\": not implemented on android, " +
                "or its payload/preconditions were rejected",
            json.getString("error"),
        )
    }

    /**
     * Dismissal returns the funnel to silent — so the element fallback comes back
     * live rather than staying shadowed by a lingering all-null object.
     */
    @Test
    fun clearingTheErrorReturnsTheFunnelToNull() {
        val messages = AppMessages().apply { showError("transient") }
        assert(TestAgent.messagesJson(messages) is JSONObject)
        messages.clearError()
        assertSame(JSONObject.NULL, TestAgent.messagesJson(messages))
    }
}
