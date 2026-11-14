package com.fauna.app.testing

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The state protocol's `data.feed.posts` array ([TestAgent.feedPostsJson]) —
 * the fix for the pre-existing gap where `TestAgent.kt` hard-coded
 * `feed.posts` to an empty array unconditionally,
 * which left every `tests/e2e-unified/actions/feed.py` id-keyed post reader
 * (`post_state_by_id`, `wait_for_interaction_count_by_id`,
 * `repost_row_state_by_target`, …) permanently blind on android.
 *
 * Mirrors [TestAgentMessagesTest]'s pattern of asserting the JSON re-parse
 * directly, off a raw string — [ApiClient.feedPostsJson]'s actual FFI hop
 * (`FfiFeedManager.postsJson` → `fauna_feed::feed_posts_json`) is proven by
 * the Rust unit test `feed_posts_json_emits_the_full_state_contract`
 * (`libs/fauna-feed/src/manager.rs`); this test pins android's half of the
 * contract — the re-parse, and the `null`/malformed → empty-array fallback.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentFeedPostsTest {

    /**
     * No manager built yet (pre-auth) is the legitimate zero — an empty array,
     * never a thrown exception or a null `posts` key (android always
     * publishes this leg, unlike an app without it at all).
     */
    @Test
    fun nullRawMapsToAnEmptyArray() {
        val posts = TestAgent.feedPostsJson(null)
        assertEquals(0, posts.length())
    }

    /**
     * A malformed string (should never happen — `postsJson()` always emits
     * valid JSON — but the re-parse must degrade the same way `null` does,
     * not crash the whole state dump) also maps to an empty array.
     */
    @Test
    fun malformedRawMapsToAnEmptyArrayRatherThanThrowing() {
        val posts = TestAgent.feedPostsJson("not json")
        assertEquals(0, posts.length())
    }

    /**
     * The real shape `fauna_feed::feed_posts_json` emits re-parses field for
     * field — the re-parse must not drop or rename anything the id-keyed
     * readers key off (`post_id`, the interaction counts, `is_muted`, the
     * repost carrier + viewer pair).
     */
    @Test
    fun aRealPostsArrayReparsesFieldForField() {
        val raw = JSONArray().put(
            JSONObject()
                .put("post_id", "p1")
                .put("author", "a1")
                .put("body", "hello")
                .put("timestamp", 1000)
                .put("tags", JSONArray().put("tag1"))
                .put("has_media", true)
                .put("media_hash", "deadbeef")
                .put("is_reply", false)
                .put("is_muted", false)
                .put("like_count", 3)
                .put("reply_count", 1)
                .put("repost_count", 2)
                .put("quote_count", 0)
                .put("viewer_liked", true)
                .put("reposted_post_id", "orig")
                .put("viewer_repost_id", JSONObject.NULL),
        ).toString()

        val posts = TestAgent.feedPostsJson(raw)
        assertEquals(1, posts.length())
        val row = posts.getJSONObject(0)
        assertEquals("p1", row.getString("post_id"))
        assertEquals(3, row.getInt("like_count"))
        assertTrue("is_muted must survive the re-parse", row.has("is_muted") && !row.getBoolean("is_muted"))
        assertEquals("orig", row.getString("reposted_post_id"))
        assertEquals(JSONObject.NULL, row.get("viewer_repost_id"))
    }
}
