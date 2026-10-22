package com.fauna.app.ui.navigation

import com.fauna.app.core.HexUtil

/**
 * The OTHER-actor profile route's pattern: the viewed actor plus the open
 * target's optional **home** (`profile.md` § Layout & flow → *Reaching another
 * actor's profile with their home carried*). The home is the nest URL the
 * opener carried — a Find User result's lookup, a post author's
 * `PostSummary.home_nest_url`, a contact row's `ContactItem.home_nest_url`, or
 * the state-protocol entry's `home_nest_url` — and it seeds the page's
 * actor-home slot (`ProfileVM`).
 */
const val PROFILE_OTHER_ROUTE = "profile/{actorId}?home={home}"

/**
 * The concrete route for [actorId]'s profile, carrying [home] when it is set.
 * The URL rides the route hex-encoded (UTF-8): a nest URL's `:` and `/` would
 * otherwise need percent-escaping the navigation layer may or may not undo
 * before the view-model reads it, and a hex arg has exactly one reading.
 */
fun profileRoute(actorId: String, home: String? = null): String {
    val carried = home?.trim()?.takeIf { it.isNotEmpty() }
        ?: return "profile/$actorId"
    return "profile/$actorId?home=${HexUtil.bytesToHex(carried.toByteArray(Charsets.UTF_8))}"
}

/** Inverse of [profileRoute]'s `home` arg; `null` for an absent or malformed one. */
fun decodeProfileHome(arg: String?): String? {
    if (arg.isNullOrEmpty() || arg.length % 2 != 0) return null
    return runCatching { String(HexUtil.hexToBytes(arg), Charsets.UTF_8) }
        .getOrNull()
        ?.takeIf { it.isNotBlank() }
}
