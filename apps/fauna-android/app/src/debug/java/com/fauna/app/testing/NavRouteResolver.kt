package com.fauna.app.testing

/**
 * Resolves a state-protocol nav-stack entry — `{"view": …}` plus the optional
 * second entry's `{"id": …}` — to one of android's `FaunaNavHost` routes.
 *
 * ## Why this exists
 *
 * The cross-app action layer navigates a shell sub-page with a **two-element**
 * nav stack: `[{"view":"settings"},{"view":"settings","id":"muted-words"}]`
 * (`ui/README.md` § Navigation model, qualification 2 — the id-bearing entry
 * routes to `<page>` after the shell's entry reset). Until 2026-08-14 android's
 * handler read `stack[0].view` and nothing else, so **every** settings sub-page
 * navigation landed on the Settings hub, and `{"view":"admin"}` — a route
 * android does not have, since its admin pages are `settings/admin*` — threw
 * out of the handler rather than refusing through it. Nobody had noticed
 * because android's e2e suite has never run against a device.
 *
 * Linux resolves the same protocol in `apps/fauna-linux/src/main.rs`
 * (`explicit_sub` + `settings_subpage_for_view`, plus a bare admin nav landing
 * on the Dashboard); this is android's twin of that arm.
 *
 * ## Why a pure function
 *
 * Route resolution is a table, and a table deserves a test per row rather than
 * an emulator. [resolve] touches no `NavController`, so
 * [NavRouteResolverTest] pins every id the action layer actually sends under
 * plain JUnit. The caller keeps the two jobs it cannot delegate: crossing the
 * nav edge, and reporting a [Refused] through the convention-11 funnel.
 */
internal object NavRouteResolver {

    /** The outcome of resolving one nav-stack entry. */
    internal sealed interface Resolution {
        /** Navigate to this `FaunaNavHost` route. */
        data class Route(val route: String) : Resolution

        /**
         * android cannot honour this nav. The [reason] is the convention-11
         * refusal string: it reaches the driver on `error-message`, so it must
         * say *what* was asked for and *why* android cannot serve it — never a
         * silent fallback to a plausible-looking page, which reads downstream
         * as a product bug on whatever page the test lands on instead.
         */
        data class Refused(val reason: String) : Resolution
    }

    /**
     * Settings sub-page ids whose android route is **not** `settings/<id>`.
     * Everything absent from this map and from [UNBUILT] takes the plain
     * `settings/<id>` form, which is what the shipped route table already is.
     */
    private val SETTINGS_ID_OVERRIDES = mapOf(
        // Nostr is a first-class top-level tab on android, not a Settings
        // sub-page (nostr.md § Page structure; `drawerItems` carries
        // `nostr-tab` unconditionally).
        "nostr" to "nostr",
        // The rail entry is `subscription-settings`; android's route kept the
        // shorter `subscriptions` spelling.
        "subscription-settings" to "settings/subscriptions",
    )

    /** Admin sub-page ids whose android route is not `settings/<id>`. */
    private val ADMIN_ID_OVERRIDES = mapOf(
        // The admin rail's own "Settings" entry — nav-labelled "Tiers" on
        // android after the per-page-services redesign.
        "settings" to "settings/admin-settings",
        // The id is bare `users`; the route carries the `admin-` prefix.
        "users" to "settings/admin-users",
    )

    /**
     * Navigations android genuinely cannot serve, each refused by name rather
     * than quietly redirected. Three different reasons, all deliberate:
     * two pages android has never built, one the redesign removed, and one
     * surface that is not a route at all.
     */
    private val UNBUILT = mapOf(
        "settings:general" to
            "android does not build a Settings → General page (no route in " +
            "FaunaNavHost, only orphaned i18n strings)",
        "settings:p2p" to
            "android does not build a Settings → P2P page — it hosts the peer " +
            "node headlessly via its foreground service (p2p.md)",
        "settings:tui-settings" to
            "the Terminal settings page is tui-only (ui.yaml `platforms: [tui]`)",
        "admin:admin-services" to
            "android removed the legacy admin Services page in the per-page-" +
            "services redesign — the pairing toggle lives on admin-nest",
        "settings:mail-list-members" to
            "android's mail-list-members route is parameterised " +
            "(settings/mail-list-members/{listIdHex}/{listName}) and the nav " +
            "patch carries no list, so there is nothing to open — reach it by " +
            "tapping a row on settings/mail-lists",
        "search:" to
            "android's search is a top-app-bar SearchBar toggle, not a " +
            "destination; its only route is the parameterised search/{query}",
    )

    /**
     * Resolve `view` + the optional sub-page `id`.
     *
     * `view` is the first stack entry's `view`; `subId` is the **second**
     * entry's `id` when the action layer sent one (it is that second entry,
     * not an `id` on the first, that carries the sub-page — reading only the
     * first entry is the bug this resolver closes).
     */
    fun resolve(view: String, subId: String? = null): Resolution {
        val id = subId?.takeIf { it.isNotEmpty() }
        UNBUILT["$view:${id.orEmpty()}"]?.let { return Resolution.Refused(it) }

        return when (view) {
            // android hosts admin INSIDE the Settings shell rather than as a
            // second top-level shell, so every admin nav resolves under
            // `settings/`. A bare `{"view":"admin"}` lands on the Dashboard —
            // the cross-app rule (`ui/README.md` § Navigation model) macOS and
            // Windows already followed, and which linux adopted so that an
            // admin nav never resolves to "nowhere".
            "admin" -> Resolution.Route(
                when {
                    id == null -> "settings/admin"
                    else -> ADMIN_ID_OVERRIDES[id] ?: "settings/$id"
                }
            )

            "settings" -> Resolution.Route(
                when {
                    // The shell entry itself: android's canonical entry is the
                    // Settings hub, the mobile idiom iOS's More-hub also lands
                    // on, rather than the desktop rail's Status page.
                    id == null -> "settings"
                    else -> SETTINGS_ID_OVERRIDES[id] ?: "settings/$id"
                }
            )

            // Legacy single-element navs that name a Settings sub-page
            // directly (linux's `settings_subpage_for_view` twin) — the action
            // layer still sends `{"view":"devices"}` / `{"view":"family"}`.
            in SUBPAGE_VIEWS -> Resolution.Route("settings/$view")

            // Every other view is a top-level route under its own name.
            else -> Resolution.Route(view)
        }
    }

    /**
     * Top-level `view` names that are Settings sub-pages on android. Devices
     * and Folders moved under the Settings shell in the 2026-06-28 sync/
     * folder UI unification; Family has always lived there.
     */
    private val SUBPAGE_VIEWS = setOf("devices", "folders", "family")
}
