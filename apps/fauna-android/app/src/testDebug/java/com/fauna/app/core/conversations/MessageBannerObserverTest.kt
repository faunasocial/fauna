package com.fauna.app.core.conversations

import android.app.NotificationManager
import com.fauna.app.core.AppState
import com.fauna.app.core.NotificationHelper
import com.fauna.app.core.SecureStorage
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.TestAgent
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import uniffi.fauna_conversations.BodyFormat
import uniffi.fauna_conversations.ConversationsManager
import uniffi.fauna_conversations.MessageBadges
import uniffi.fauna_conversations.Rail
import uniffi.fauna_conversations.RailInboundMessage
import uniffi.fauna_conversations.TypedAddress
import uniffi.fauna_conversations.messageBannersJsonText

/**
 * The android half of `conversations` outcome 11 — *a new message raises a system
 * notification while the app is running, except in the conversation you already
 * have open* (`docs/goal/ui/conversations.md` § Where logic lives).
 *
 * **Scope, deliberately narrow.** The three rules are the shared
 * `MessageNotificationTracker`'s and are unit-tested once in
 * `libs/fauna-conversations/src/notification.rs`. What these pin is the ANDROID
 * WIRING the shared tests cannot see: that [MessageBannerObserver] projects the
 * snapshot and hands the tracker the snapshot's own selection and floor, that the
 * fired-banner log records exactly what reached the platform, and that
 * [ConversationsManagerHost] ticks it and rebuilds the tracker at sign-out. Each
 * drives a REAL manager (UniFFI over host JNA, [FaunaRobolectricTestRunner]).
 *
 * Every inject carries an explicit, strictly increasing stamp rather than the
 * wall clock: two injects inside one millisecond would leave `last_activity_ms`
 * unchanged, and the tracker would decline to fire for a reason that has nothing
 * to do with the rule under test (convention 14). The stamps start past the
 * manager's launch floor, which is taken at construction.
 *
 * The banner log is process-global, so every log assertion reads a delta.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MessageBannerObserverTest {

    private var nextStampMs = System.currentTimeMillis() + 1_000

    private fun mockBackedManager(): ConversationsManager =
        ConversationsManager().also { it.installMockBackendsForTest() }

    /** One inbound FaunaMls message — the `conversations_inject_inbound` shape. */
    private fun inject(manager: ConversationsManager, sender: String, body: String) {
        nextStampMs += 1_000
        manager.injectInboundForTest(
            RailInboundMessage(
                rail = Rail.FAUNA_MLS,
                sender = TypedAddress.Fauna(sender, ByteArray(32)),
                recipients = listOf(TypedAddress.Fauna("me@self-nest.test", ByteArray(32))),
                subject = null,
                body = body,
                bodyFormat = BodyFormat.MARKDOWN,
                timestampMs = nextStampMs,
                messageId = "msg-${java.util.UUID.randomUUID()}",
                inReplyTo = null,
                attachments = emptyList(),
                badges = MessageBadges(
                    encrypted = false,
                    signed = false,
                    verified = false,
                    contentWarning = null,
                ),
                legalTakedownRef = null,
                planeRef = null,
            ),
        )
    }

    /** [inject] a sender with no thread yet; returns the thread it opened. */
    private fun injectNewThread(manager: ConversationsManager, sender: String, body: String): String {
        val before = manager.snapshot().threads.map { it.threadId }.toSet()
        inject(manager, sender, body)
        return manager.snapshot().threads.map { it.threadId }.single { it !in before }
    }

    private fun bannerLog(): JSONObject = JSONObject(messageBannersJsonText())

    /** The `fired` entries appended since [before] was read, as (thread, label). */
    private fun firedSince(before: JSONObject): List<Pair<String, String>> {
        val all = bannerLog().getJSONArray("fired")
        val from = before.getJSONArray("fired").length()
        return (from until all.length()).map {
            val entry = all.getJSONObject(it)
            entry.getString("thread_id") to entry.getString("label")
        }
    }

    @Test
    fun theThreadsAlreadyThereAtLoginRaiseNoBanner() {
        val manager = mockBackedManager()
        val raised = mutableListOf<String>()
        val banners = MessageBannerObserver { raised += it.threadId; true }

        inject(manager, "alpha@self-nest.test", "already here when you signed in")
        banners.tick(manager)

        assertEquals(
            "the first non-empty snapshot seeds the tracker silently — a banner for a " +
                "thread that was already there is a sign-in toast storm",
            emptyList<String>(),
            raised,
        )
    }

    @Test
    fun aNewThreadRaisesOneBannerAndTheLogRecordsIt() {
        val manager = mockBackedManager()
        val raised = mutableListOf<String>()
        val banners = MessageBannerObserver { raised += it.threadId; true }
        val before = bannerLog()

        inject(manager, "alpha@self-nest.test", "seeds the tracker")
        banners.tick(manager)
        val beta = injectNewThread(manager, "beta@self-nest.test", "a message you are not looking at")
        banners.tick(manager)

        assertEquals("exactly one banner, for the new thread", listOf(beta), raised)
        val label = manager.snapshot().threads.single { it.threadId == beta }.label
        assertEquals(
            "the log records the fire at the firing site, under the thread's label",
            listOf(beta to label),
            firedSince(before),
        )
        val after = bannerLog()
        assertEquals("two ticks started", 2L, after.getLong("started") - before.getLong("started"))
        assertEquals("two ticks completed", 2L, after.getLong("completed") - before.getLong("completed"))
    }

    @Test
    fun aBannerThePlatformRefusedIsNotRecorded() {
        val manager = mockBackedManager()
        val attempted = mutableListOf<String>()
        val banners = MessageBannerObserver { attempted += it.threadId; false }
        val before = bannerLog()

        inject(manager, "alpha@self-nest.test", "seeds the tracker")
        banners.tick(manager)
        val beta = injectNewThread(manager, "beta@self-nest.test", "the platform will refuse this one")
        banners.tick(manager)

        assertEquals("the tracker's decision still reached the firing call", listOf(beta), attempted)
        assertEquals(
            "a banner the platform did not take must not enter the log — its entries " +
                "mean a banner was raised",
            emptyList<Pair<String, String>>(),
            firedSince(before),
        )
        assertEquals(
            "the tick still completes, or a negative read could never be anchored",
            2L,
            bannerLog().getLong("completed") - before.getLong("completed"),
        )
    }

    @Test
    fun theOpenThreadIsSuppressedAndTheOthersStillFire() {
        val manager = mockBackedManager()
        val raised = mutableListOf<String>()
        val banners = MessageBannerObserver { raised += it.threadId; true }

        inject(manager, "alpha@self-nest.test", "seeds the tracker")
        banners.tick(manager)
        val alpha = manager.snapshot().threads.single().threadId
        val beta = injectNewThread(manager, "beta@self-nest.test", "first message")
        banners.tick(manager)
        manager.selectThread(beta)
        banners.tick(manager)

        inject(manager, "beta@self-nest.test", "a message in the thread you are reading")
        banners.tick(manager)
        inject(manager, "alpha@self-nest.test", "still talking over here")
        banners.tick(manager)

        // Mutation check: hand the tracker `null` instead of the snapshot's
        // `selectedThreadId` and `beta` appears twice.
        assertEquals(
            "the thread the user has open raises nothing; a different thread still does",
            listOf(beta, alpha),
            raised,
        )
    }

    @Test
    fun theAgentPublishesTheLogAsTopLevelMessageBanners() {
        val manager = mockBackedManager()
        val banners = MessageBannerObserver { true }
        inject(manager, "alpha@self-nest.test", "seeds the tracker")
        banners.tick(manager)
        injectNewThread(manager, "beta@self-nest.test", "a banner for the log")
        banners.tick(manager)

        // Where the witness reads it (`MESSAGE_BANNERS_KEY`): published from
        // launch, never absent — absence is the "no leg" refusal.
        val appState = AppState()
        val state = TestAgent.serializeState(appState, mock(SecureStorage::class.java), null, appState.messages)
        assertEquals(
            "the published key is the shared log, verbatim",
            bannerLog().toString(),
            state.optJSONObject("message_banners")?.toString(),
        )
    }

    @Test
    fun theHostFiresARealNotificationAndReseedsAtSignOut() {
        val app = RuntimeEnvironment.getApplication()
        val platform = shadowOf(app.getSystemService(NotificationManager::class.java))
        val host = ConversationsManagerHost(NotificationHelper(app))
        host.manager.installMockBackendsForTest()
        val before = bannerLog()
        val posted = platform.allNotifications.size

        // No hand ticks from here on: the host's own observer runs one per change.
        inject(host.manager, "alpha@self-nest.test", "the outgoing identity's thread")
        val beta = injectNewThread(host.manager, "beta@self-nest.test", "a message you are not looking at")

        assertEquals(
            "the new thread's banner reached the platform's notification manager",
            posted + 1,
            platform.allNotifications.size,
        )
        assertEquals(listOf(beta), firedSince(before).map { it.first })

        // Sign-out: the incoming identity's threads arrive for the first time.
        host.stopConversationsSession()
        inject(host.manager, "gamma@other-nest.test", "the incoming identity's restored thread")

        // Mutation check: drop `banners.resetForIdentityChange()` from
        // `stopConversationsSession` and `gamma` fires — a carried tracker finds
        // every restored thread new.
        assertEquals(
            "the threads the next identity restores are not new messages — the fresh " +
                "tracker seeds on them silently",
            listOf(beta),
            firedSince(before).map { it.first },
        )
        assertEquals(posted + 1, platform.allNotifications.size)
    }
}
