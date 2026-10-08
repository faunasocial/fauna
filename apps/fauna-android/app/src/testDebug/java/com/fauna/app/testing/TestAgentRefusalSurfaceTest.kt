package com.fauna.app.testing

import com.fauna.app.core.AppState
import com.fauna.app.core.NotificationHelper
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.conversations.ConversationsManagerHost
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.ThreadFlavor

/**
 * Convention 11 (`docs/goal/architecture/testing.md` point 11) on android: a test
 * agent MUST NOT silently drop a command — it honours it or fails loudly on the
 * app's own `error-message`.
 *
 * ⚠ **The load-bearing cases here are the two lifetime ones**
 * ([aRefusalOutlivesTheNavigationEveryActionHelperIssues] and
 * [aRefusalDoesNotLeakIntoTheNextTest]). Everything else passes just as happily
 * against the *broken* shapes android and tui both shipped first, so a pin
 * without them is vacuous:
 *
 * - Android shipped the refusal on the general `AppMessages.error` banner from
 *   2026-07-19. `FaunaNavHost` runs
 *   `LaunchedEffect(currentRoute) { appState.messages.clear() }`, so every route
 *   change wiped it — and `patch {"nav": …}`, which every action-layer helper in
 *   `tests/e2e-unified/actions/` issues, IS a route change. The driver read
 *   `error=''`.
 * - A per-page error slot survives a same-page nav and fails a cross-page one.
 *
 * The nav half is asserted through `AppMessages.clear()` deliberately: that is
 * the literal call `FaunaNavHost`'s navigation effect makes, so this pin fails
 * the moment the refusal is moved back onto a slot navigation clears — without
 * needing a composed NavHost.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentRefusalSurfaceTest {

    private fun storage() = mock(SecureStorage::class.java)

    /** What `actions/__init__.py::error_text` actually reads on android: the
     *  state protocol's `messages.error`, not the UI element.
     *
     *  `messages` is `JSONObject.NULL` — not an object of nulls — when the funnel has
     *  nothing to say. That absence is deliberate and load-bearing: it is what makes the
     *  harness fall back to the page's own `error-message` element, so a page-scoped
     *  ViewModel error is visible at all (`TestAgent.messagesJson`, convention 2's rider in
     *  `e2e-conventions.md`). A silent funnel is by definition no error here. */
    private fun errorFromStateProtocol(appState: AppState): String? {
        val state = TestAgent.serializeState(appState, storage(), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    private fun command(action: String, vararg pairs: Pair<String, Any?>): JSONObject =
        JSONObject().apply {
            put("action", action)
            pairs.forEach { (k, v) -> put(k, v) }
        }

    /**
     * Drive one command to completion. `processCommand` is `suspend` because the
     * `conversations_real_*` arms await the shared manager's async wire drivers
     * (`send`, `sendNewThread`, `confirmAddParticipant`, …); the agent's poll loop
     * awaits it the same way, one command at a time.
     */
    private fun dispatch(action: String, cmd: JSONObject, appState: AppState) =
        runBlocking { TestAgent.processCommand(action, cmd, appState, storage(), null) }

    @Test
    fun anUnknownCommandSurfacesOnErrorMessage() {
        val appState = AppState()
        assertNull("clean before the refusal", errorFromStateProtocol(appState))

        dispatch("no_such_command_at_all", command("no_such_command_at_all"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull("a refused command must surface, not only reach logcat", shown)
        assertTrue(
            "the surfaced text must name the refused action so the driver can tell " +
                "WHICH command was dropped: $shown",
            shown!!.contains("no_such_command_at_all"),
        )
    }

    @Test
    fun aRefusalOutlivesTheNavigationEveryActionHelperIssues() {
        val appState = AppState()
        dispatch("no_such_command_at_all", command("no_such_command_at_all"), appState)
        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)

        // The literal call FaunaNavHost's `LaunchedEffect(currentRoute)` makes on
        // every route change — i.e. what `navigate_to` triggers.
        appState.messages.clear()

        assertEquals(
            "a refusal must outlive the navigation every action-layer helper issues; " +
                "this is exactly the slot that reported error='' before",
            shown, errorFromStateProtocol(appState),
        )
    }

    @Test
    fun aRefusalDoesNotLeakIntoTheNextTest() {
        val appState = AppState()
        dispatch("no_such_command_at_all", command("no_such_command_at_all"), appState)
        assertNotNull(errorFromStateProtocol(appState))

        // `reset` is the per-test boundary every `app` fixture drives.
        dispatch("reset", command("reset"), appState)

        assertNull(
            "reset is the ONLY clear point for a refusal — otherwise it leaks into " +
                "the next test of a reused app process",
            errorFromStateProtocol(appState),
        )
    }

    @Test
    fun aRefusalOutranksThePagesOwnError() {
        val appState = AppState()
        appState.messages.showError("a perfectly ordinary product error")
        dispatch("no_such_command_at_all", command("no_such_command_at_all"), appState)

        // The refusal means the app never did what the driver asked, so every later
        // product assertion is reading a state the test did not actually set up.
        assertTrue(
            "the refusal must outrank the page error: ${errorFromStateProtocol(appState)}",
            errorFromStateProtocol(appState)!!.contains("no_such_command_at_all"),
        )
    }

    // ── Recognised arms that DECLINE — the same failure to a driver, so the same
    // funnel. Each of these was a bare `return` or a logcat-only warning. ──────

    @Test
    fun aPatchWithNoStateIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("patch", command("patch"), appState)
        assertTrue(
            "a `patch` carrying no state did nothing at all — that must be loud",
            errorFromStateProtocol(appState)!!.contains("patch"),
        )
    }

    @Test
    fun aNavPatchWithNoNavControllerIsRefusedNotDropped() {
        val appState = AppState()
        // `navController` is null pre-auth / mid-launch — the "recognised arm that
        // declines internally and returns as if it had worked" case.
        val cmd = command("patch").apply {
            put(
                "state",
                JSONObject().put(
                    "nav",
                    JSONObject().put(
                        "stack",
                        org.json.JSONArray().put(JSONObject().put("view", "feed")),
                    ),
                ),
            )
        }

        dispatch("patch", cmd, appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull("a navigate that quietly did nothing must be loud", shown)
        assertTrue("the reason must name the target view: $shown", shown!!.contains("feed"))
    }

    @Test
    fun callMachineMethodWithNoMethodNameIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("call_machine_method", command("call_machine_method"), appState)
        assertTrue(
            "an empty `method` used to return silently: ${errorFromStateProtocol(appState)}",
            errorFromStateProtocol(appState)!!.contains("method"),
        )
    }

    @Test
    fun callMachineMethodBeforeTheHostIsWiredIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("call_machine_method", command("call_machine_method", "method" to "set_step_for_test"), appState)
        val shown = errorFromStateProtocol(appState)
        assertTrue(
            "a null OnboardingHost used to be a logcat warning only: $shown",
            shown!!.contains("set_step_for_test"),
        )
    }

    @Test
    fun conversationsInjectSendFailureWithNoThreadIdIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("conversations_inject_send_failure", command("conversations_inject_send_failure"), appState)
        assertTrue(
            "a missing thread_id used to be a logcat warning only",
            errorFromStateProtocol(appState)!!.contains("thread_id"),
        )
    }

    /**
     * `conversations_inject_page_error` — the membership-op twin of
     * [conversationsInjectSendFailureWithNoThreadIdIsRefusedNotDropped]: a missing
     * `message` must refuse loudly, not silently do nothing.
     */
    @Test
    fun conversationsInjectPageErrorWithNoMessageIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("conversations_inject_page_error", command("conversations_inject_page_error"), appState)
        assertTrue(
            "a missing message used to be reachable with no refusal at all",
            errorFromStateProtocol(appState)!!.contains("message"),
        )
    }

    @Test
    fun conversationsInjectInboundBeforeTheHostIsWiredIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("conversations_inject_inbound", command("conversations_inject_inbound"), appState)
        assertTrue(
            "a null ConversationsManagerHost used to be a logcat warning only",
            errorFromStateProtocol(appState)!!.contains("conversations_inject_inbound"),
        )
    }

    /**
     * `feed_inject_posts` — the twin of [conversationsInjectInboundBeforeTheHostIsWiredIsRefusedNotDropped]
     * for the Feed manager: before [TestAgent.start]
     * wires `feedManagerHost` from the Hilt entry point, the command must refuse
     * loudly rather than silently no-op — convention 11, same as every other
     * manager-host-gated command here.
     */
    @Test
    fun feedInjectPostsBeforeTheHostIsWiredIsRefusedNotDropped() {
        val appState = AppState()
        dispatch("feed_inject_posts", command("feed_inject_posts"), appState)
        assertTrue(
            "a null FeedManagerHost used to be reachable with no refusal at all",
            errorFromStateProtocol(appState)!!.contains("feed_inject_posts"),
        )
    }

    /**
     * The feed witnesses' test-state commands — `feed_inject_error` and the
     * reload-hold pair (`test_feed_error.py`, `test_feed.py`'s refresh witness)
     * — each have their OWN arm: before [TestAgent.start] wires the host they
     * refuse naming the command and the missing collaborator, never the
     * catch-all "no arm" (convention 11's complete-table half).
     */
    @Test
    fun theFeedWitnessCommandsAreRecognisedAndRefuseBeforeTheHostIsWired() {
        for (action in listOf("feed_inject_error", "feed_hold_next_reload", "feed_release_held_reload")) {
            val appState = AppState()
            dispatch(action, command(action), appState)
            val refusal = errorFromStateProtocol(appState)
            assertNotNull("$action must refuse loudly before the host is wired", refusal)
            assertTrue("$action fell through to the catch-all: $refusal", !refusal!!.contains("no arm"))
            assertTrue("$action's refusal must name it and the host: $refusal",
                refusal.contains(action) && refusal.contains("FeedManagerHost"))
        }
    }

    /** `feed_inject_error`'s payload defaults — the `feed.error_load` carrier
     *  tui, web and linux fall back to — and an explicit pair passing through. */
    @Test
    fun feedInjectErrorArgsDefaultToTheLoadFailureCarrier() {
        assertEquals("feed.error_load" to "feed load failed",
            TestAgent.feedInjectErrorArgs(command("feed_inject_error")))
        assertEquals("feed.error_load" to "feed load failed",
            TestAgent.feedInjectErrorArgs(command("feed_inject_error", "key" to "", "message" to JSONObject.NULL)))
        assertEquals("feed.custom" to "boom",
            TestAgent.feedInjectErrorArgs(command("feed_inject_error", "key" to "feed.custom", "message" to "boom")))
    }

    /** An honoured command must NOT write a refusal — otherwise the surface is
     *  noise and the next session learns to ignore it. */
    @Test
    fun anHonouredCommandWritesNoRefusal() {
        val appState = AppState()
        runBlocking {
            dispatch("logout", command("logout"), appState)
        }
        assertNull(errorFromStateProtocol(appState))
        assertNull(appState.messages.refusedAgentCommand.value)
    }

    // ── Convention 11's *cross-app contract* half: "a command any app implements
    // is one every app must either implement or explicitly refuse". The tests
    // above prove refusals are LOUD; these prove the command table is COMPLETE.
    // ──────────────────────────────────────────────────────────────────────────

    /**
     * The commands reachable from an android-collecting e2e test that android's
     * agent must recognise. Verified against `--app android` collection
     * (`pytest --collect-only -q --app android`), not assumed:
     *
     * | command                               | android-collecting callers |
     * |---------------------------------------|----------------------------|
     * | `conversations_real_resolve_send_new` | 8 tests (MLS roundtrip/cross-nest/cross-device/inbox-drain, moderation ×3, reactions) |
     * | `conversations_real_send`             | 2 (MLS roundtrip, cross-device sync) |
     * | `conversations_real_add`              | 1 (MLS roundtrip) |
     * | `conversations_real_remove`           | 1 (MLS roundtrip) |
     * | `conversations_real_rename`           | 1 (MLS roundtrip) |
     * | `conversations_create_mls_group`      | 2 (thread membership, thread rename) |
     * | `conversations_accept_recipient`      | 2 (recipient picker) |
     *
     * `conversations_real_send_attachment` has no android-collecting caller
     * *today* (only `test_fauna_mls_web_receives_from_linux_sender.py`, which is
     * web/linux-marked) and is implemented anyway: it is one manager call away
     * from `conversations_real_send`, and leaving the family half-built is how
     * the next widening of a marker silently reopens this exact gap.
     */
    private val crossAppConversationsCommands = listOf(
        "conversations_real_resolve_send_new",
        "conversations_real_send",
        "conversations_real_send_attachment",
        "conversations_real_add",
        "conversations_real_remove",
        "conversations_real_rename",
        "conversations_create_mls_group",
        "conversations_accept_recipient",
    )

    /**
     * ⚠ **The ratchet.** Every command above must be RECOGNISED — i.e. refused
     * with its own reason, never with the catch-all "no arm for this action".
     *
     * Before this landed, all eight fell through to the `else` arm. That is a
     * *conforming* refusal (pass 23 made the catch-all loud), which is exactly
     * why a test asserting only "something surfaced" is vacuous here: the
     * catch-all names the action too. The distinguishing assertion is that the
     * reason is NOT the catch-all — that is what fails if an arm is deleted.
     */
    @Test
    fun everyCrossAppConversationsCommandHasItsOwnArm() {
        val unrecognised = crossAppConversationsCommands.filter { action ->
            val appState = AppState()
            runBlocking {
                TestAgent.processCommand(action, command(action), appState, storage(), null)
            }
            // Host is null in a unit test, so a recognised arm declines with its
            // own "arrived before the ConversationsManagerHost was wired" reason.
            errorFromStateProtocol(appState)?.contains("no arm") ?: true
        }
        assertTrue(
            "convention 11: these commands reach android through an e2e test that " +
                "COLLECTS on --app android, but android's agent has no arm for them — " +
                "they fall through to the catch-all: $unrecognised",
            unrecognised.isEmpty(),
        )
    }

    /**
     * A recognised arm that cannot run yet must say *which* collaborator is
     * missing. `null` host is the reachable case: a command can arrive before
     * `TestAgent.start` wires the Hilt singleton.
     */
    @Test
    fun aRealConversationsCommandBeforeTheHostIsWiredNamesTheHost() {
        val appState = AppState()
        runBlocking {
            TestAgent.processCommand(
                "conversations_real_send",
                command("conversations_real_send", "thread_id" to "t1", "body" to "hi"),
                appState, storage(), null,
            )
        }
        val shown = errorFromStateProtocol(appState)
        assertNotNull("an unwired real-send must be loud, not a no-op", shown)
        assertTrue(
            "the reason must name the missing collaborator: $shown",
            shown!!.contains("ConversationsManagerHost"),
        )
    }

    /**
     * `sync_inject_locations` is the one command in the enumeration android
     * **deliberately declines**: it is a render fixture over a live bound-folder
     * list that android has no surface for. Convention 11 admits that — but the
     * refusal must say so, not read as an oversight.
     */
    @Test
    fun syncInjectLocationsCarriesADeliberateDocumentedRefusal() {
        val appState = AppState()
        runBlocking {
            dispatch(
                "sync_inject_locations", command("sync_inject_locations"),
            appState,
            )
        }
        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "a deliberate refusal must be distinguishable from an un-built one: $shown",
            !shown!!.contains("no arm"),
        )
    }

    // ── Payload parsing (pure) ────────────────────────────────────────────────

    @Test
    fun parseActorHexAcceptsExactly32Bytes() {
        val hex = "ab".repeat(32)
        val parsed = TestAgent.parseActorHex(hex)
        assertNotNull("64 hex chars is the wire shape for an ActorId", parsed)
        assertEquals(32, parsed!!.size)
        assertEquals(0xAB.toByte(), parsed[0])
    }

    @Test
    fun parseActorHexRejectsWrongLengthAndNonHex() {
        assertNull("31 bytes is not an ActorId", TestAgent.parseActorHex("ab".repeat(31)))
        assertNull("33 bytes is not an ActorId", TestAgent.parseActorHex("ab".repeat(33)))
        assertNull("odd length", TestAgent.parseActorHex("abc"))
        assertNull("non-hex", TestAgent.parseActorHex("zz".repeat(32)))
        assertNull("empty", TestAgent.parseActorHex(""))
    }

    // ── Arms driven against a REAL shared manager ─────────────────────────────
    //
    // Everything above runs with a null `ConversationsManagerHost`, where all
    // eight arms decline identically at the wiring check — enough to prove the
    // command table is complete, useless for proving any arm *works*. These wire
    // a real `ConversationsManagerHost` (UniFFI over host JNA, per
    // `FaunaRobolectricTestRunner`) so the assertion is on the manager's own
    // snapshot. `create_mls_group` is the one arm fully drivable headlessly: it
    // is a snapshot-level fixture that bypasses welcome/key-package distribution,
    // so it needs no rail backend and no nest. The `conversations_real_*` arms
    // genuinely need a nest (they fire wire ops) — their payload-validation
    // refusals are pinned here, their wire halves on the host emulator.

    private fun wireRealManagerHost(): ConversationsManagerHost =
        ConversationsManagerHost(
            NotificationHelper(RuntimeEnvironment.getApplication()),
            com.fauna.app.widget.WidgetUnreadPublisher(RuntimeEnvironment.getApplication()),
        )
            .also { TestAgent.setConversationsManagerHostForTest(it) }

    @After
    fun unwireManagerHost() {
        // `TestAgent` is a process-global `object`: a host left set here would
        // silently turn the null-host pins above into something else entirely.
        TestAgent.setConversationsManagerHostForTest(null)
    }

    /** The arm must MOVE the manager, not merely be recognised. */
    @Test
    fun createMlsGroupActuallyCreatesAnMlsGroupThreadOnTheSharedManager() {
        val host = wireRealManagerHost()
        val appState = AppState()
        val idsBefore = host.manager.snapshot().threads.map { it.threadId }.toSet()

        dispatch(
            "conversations_create_mls_group",
            command("conversations_create_mls_group").apply {
                put("participants", org.json.JSONArray().put("alice@self-nest.test").put("bob@self-nest.test"))
            },
            appState,
        )

        assertNull("an honoured command writes no refusal", errorFromStateProtocol(appState))
        // Identify the new thread by DIFF, never by position: the snapshot is
        // SORTED (SortOrder), so "the one we just made" is not "the last row" —
        // the same trap `actions/conversations.py::create_mls_group` calls out.
        val fresh = host.manager.snapshot().threads.filter { it.threadId !in idsBefore }
        assertEquals("exactly one new thread", 1, fresh.size)
        assertEquals(
            "the new thread must carry the MlsGroup flavor the membership/rename " +
                "tests key on",
            ThreadFlavor.MlsGroup,
            fresh.single().flavor,
        )
    }

    /** A payload the arm cannot use is refused BEFORE it touches the manager. */
    @Test
    fun createMlsGroupWithNoParticipantsIsRefusedAndCreatesNothing() {
        val host = wireRealManagerHost()
        val appState = AppState()
        val before = host.manager.snapshot().threads.size

        dispatch("conversations_create_mls_group", command("conversations_create_mls_group"), appState)

        assertTrue(
            "a missing `participants` array must name the field: " +
                "${errorFromStateProtocol(appState)}",
            errorFromStateProtocol(appState)!!.contains("participants"),
        )
        assertEquals(
            "a refused command must leave the manager untouched",
            before, host.manager.snapshot().threads.size,
        )
    }

    /**
     * `acceptCurrentRecipientChip` returns false when neither picker holds text
     * that parses. Linux ignores that return; android reports it, because a
     * silent false is precisely the "recognised arm that returns as if it had
     * worked" shape convention 11 names — the action layer then waits 5s for a
     * chip that was never going to appear.
     */
    @Test
    fun acceptRecipientWithNoParseablePickerTextIsRefused() {
        wireRealManagerHost()
        val appState = AppState()

        dispatch("conversations_accept_recipient", command("conversations_accept_recipient"), appState)

        val shown = errorFromStateProtocol(appState)
        assertNotNull("an accept that committed no chip must be loud", shown)
        assertTrue(
            "the reason must say no picker had parseable text: $shown",
            shown!!.contains("recipient picker"),
        )
    }

    /** A bad actor hex must refuse by NAME — silently sending `ByteArray(32)`
     *  would address the MLS leaf of the zero actor and "work". */
    @Test
    fun aRealAddWithAnUnparseableActorHexIsRefused() {
        wireRealManagerHost()
        val appState = AppState()

        dispatch(
            "conversations_real_add",
            command(
                "conversations_real_add",
                "thread_id" to "t1",
                "peer_actor_id_hex" to "not-hex",
                "peer_handle" to "bob",
            ),
            appState,
        )

        val shown = errorFromStateProtocol(appState)
        assertNotNull(shown)
        assertTrue(
            "the reason must name the unparseable field: $shown",
            shown!!.contains("peer_actor_id_hex"),
        )
    }

    /** …and a well-formed 64-hex id must get PAST the parse (it then fails on the
     *  absent nest, which is a different, named reason). */
    @Test
    fun aRealAddWithAWellFormedActorHexGetsPastTheParse() {
        wireRealManagerHost()
        val appState = AppState()

        dispatch(
            "conversations_real_add",
            command(
                "conversations_real_add",
                "thread_id" to "t1",
                "peer_actor_id_hex" to "ab".repeat(32),
                "peer_handle" to "bob",
            ),
            appState,
        )

        val shown = errorFromStateProtocol(appState)
        assertTrue(
            "a valid 64-hex id must NOT be rejected by the parse guard — this is " +
                "the mutation that would make the guard reject everything: $shown",
            shown == null || !shown.contains("peer_actor_id_hex"),
        )
    }
}
