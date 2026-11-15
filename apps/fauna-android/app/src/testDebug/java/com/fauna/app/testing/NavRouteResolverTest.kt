package com.fauna.app.testing

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * One case per row of [NavRouteResolver]'s table.
 *
 * ## Why this test is worth more than it looks
 *
 * Until 2026-08-14 android's nav-patch handler read `stack[0].view` and nothing
 * else, so **every** `{"view":"settings"},{"view":"settings","id":"<page>"}`
 * the cross-app action layer sends landed on the Settings hub, and
 * `{"view":"admin"}` threw. Neither had ever been observed, because android's
 * e2e suite has never run against a device — so the only thing standing between
 * this table and a repeat of that is a test that does not need one.
 *
 * The ids below are not invented: they cover the exact set the action layer
 * sends, enumerated from `tests/e2e-unified/actions/` rather than guessed
 * (plus the handful of further built sub-pages the same formula covers). Two of
 * them, `users` and `settings` under `admin`, are precisely the kind that a
 * `settings/$id` formula silently gets wrong, and they are why this is a table
 * with a test rather than a one-line string concatenation.
 */
class NavRouteResolverTest {

    private fun route(view: String, id: String? = null): String {
        val r = NavRouteResolver.resolve(view, id)
        assertTrue(
            "expected a route for view=$view id=$id, got $r",
            r is NavRouteResolver.Resolution.Route,
        )
        return (r as NavRouteResolver.Resolution.Route).route
    }

    private fun refusal(view: String, id: String? = null): String {
        val r = NavRouteResolver.resolve(view, id)
        assertTrue(
            "expected a refusal for view=$view id=$id, got $r",
            r is NavRouteResolver.Resolution.Refused,
        )
        return (r as NavRouteResolver.Resolution.Refused).reason
    }

    // --- The Settings shell -------------------------------------------------

    @Test
    fun `a bare settings nav lands on the shell's canonical entry`() {
        // android's canonical entry is the Settings hub — the mobile idiom, the
        // same place iOS's More-hub lands, not the desktop rail's Status page.
        assertEquals("settings", route("settings"))
        // An empty id is the same thing as no id, not a route to `settings/`.
        assertEquals("settings", route("settings", ""))
    }

    @Test
    fun `every plain settings sub-page id takes the settings slash id form`() {
        val plain = listOf(
            "account", "atproto", "connected-apps", "devices", "encryption", "folders",
            "labeler-catalog", "logs", "mail-settings", "mail-aliases",
            "mail-export", "mail-lists", "mail-spam", "moderation",
            "muted-words", "nests", "personalization", "privacy", "status",
            "task-delegation", "web",
        )
        plain.forEach { id -> assertEquals("settings/$id", route("settings", id)) }
    }

    @Test
    fun `nostr is a top-level tab on android, not a settings sub-page`() {
        // The rail lists it under Settings on desktop; android gives it its own
        // unconditional drawer tab (nostr.md § Page structure).
        assertEquals("nostr", route("settings", "nostr"))
    }

    @Test
    fun `the subscriptions rail id does not match android's route spelling`() {
        assertEquals("settings/subscriptions", route("settings", "subscription-settings"))
    }

    // --- The admin pages, which android hosts INSIDE Settings ---------------

    @Test
    fun `a bare admin nav lands on the Dashboard rather than nowhere`() {
        // The cross-app rule macOS and Windows already followed and linux
        // adopted: an admin nav never resolves to "nowhere". android has no
        // top-level `admin` route at all — this used to THROW.
        assertEquals("settings/admin", route("admin"))
    }

    @Test
    fun `every admin sub-page id resolves under the settings shell`() {
        val ids = listOf(
            "admin-aliases", "admin-bridges-pending", "admin-calendar",
            "admin-contacts", "admin-dns", "admin-files", "admin-logs",
            "admin-mail", "admin-nest", "admin-web",
        )
        ids.forEach { id -> assertEquals("settings/$id", route("admin", id)) }
    }

    @Test
    fun `the two admin ids a formula would get wrong`() {
        // `users`, not `admin-users` — the id drops the prefix the route keeps.
        assertEquals("settings/admin-users", route("admin", "users"))
        // The admin rail's own "Settings" entry, nav-labelled "Tiers" here.
        // A naive `settings/$id` would send this to the Settings HUB, i.e.
        // straight back out of the admin surface the test is asserting on.
        assertEquals("settings/admin-settings", route("admin", "settings"))
    }

    // --- Legacy single-element navs -----------------------------------------

    @Test
    fun `single-element navs naming a settings sub-page still reach it`() {
        // The action layer sends `navigate_to("devices")` / `("family")` with
        // no second entry; both moved under the Settings shell.
        assertEquals("settings/devices", route("devices"))
        assertEquals("settings/family", route("family"))
        assertEquals("settings/folders", route("folders"))
    }

    @Test
    fun `an ordinary top-level view is its own route`() {
        listOf("feed", "conversations", "contacts", "events", "media",
               "backups", "notifications", "moderation", "bridges", "profile")
            .forEach { v -> assertEquals(v, route(v)) }
    }

    // --- Refusals: honour it or fail loudly (convention 11) -----------------

    @Test
    fun `pages android has never built are refused by name, not redirected`() {
        // The failure mode this forbids: quietly landing on the Settings hub,
        // after which the test asserts against the hub and reports a product
        // bug on a page that does not exist.
        assertTrue(refusal("settings", "general").contains("General"))
        assertTrue(refusal("settings", "p2p").contains("P2P"))
        assertTrue(refusal("settings", "tui-settings").contains("tui-only"))
    }

    @Test
    fun `the removed legacy admin Services page is refused, naming its successor`() {
        val why = refusal("admin", "admin-services")
        assertTrue(why, why.contains("admin-nest"))
    }

    @Test
    fun `mail-list-members is refused because the patch carries no list`() {
        // Its route is parameterised; a nav with no list has nothing to open,
        // and the refusal says how to reach it instead.
        val why = refusal("settings", "mail-list-members")
        assertTrue(why, why.contains("settings/mail-lists"))
    }

    @Test
    fun `search is refused because it is a toolbar toggle, not a destination`() {
        val why = refusal("search")
        assertTrue(why, why.contains("SearchBar"))
    }

    @Test
    fun `every refusal names what was asked for, so it diagnoses itself`() {
        // e2e-conventions.md convention 6: a failure must explain itself from
        // the message alone. A refusal that says only "unsupported" sends the
        // reader to the app instead of to this table.
        listOf(
            "settings" to "general",
            "settings" to "p2p",
            "settings" to "tui-settings",
            "admin" to "admin-services",
            "settings" to "mail-list-members",
        ).forEach { (view, id) ->
            val why = refusal(view, id)
            assertTrue("refusal for $view/$id is too short to diagnose: $why", why.length > 40)
        }
    }
}
