package com.fauna.app.ui.util

/**
 * Several automation attributes in the one `stateDescription` a Compose node
 * publishes: `key=value` pairs joined by `;`, with `%` written `%25` and `;`
 * written `%3B` inside a value — windows' `AutomationAttributePack.Pack`
 * shape (`apps/fauna-windows/FaunaApp/FaunaApp.Core/Helpers/InlineVideoPlayback.cs`),
 * which met the same one-string-per-element limit first. The debug bridge
 * reads one key back ([com.fauna.app.testing.AutomationSemantics.attrValue]),
 * so a test's `get_attr(id, key)` reads the same on android as on every other
 * app. Keys are lowercase kebab; a pack holds two or more pairs — one value
 * needs no packing, and the reader takes a single pair as a bare value, which
 * also keeps a lone id ending in base64 `=` from being misread. An empty value
 * reads as not set (an empty paint is absence on this bridge), so a node whose
 * attribute is sometimes absent packs it empty rather than dropping to one pair.
 */
object AutomationAttributePack {
    fun pack(vararg pairs: Pair<String, String>): String {
        require(pairs.size >= 2) { "a pack holds two or more pairs; publish one value bare" }
        return pairs.joinToString(";") { (key, value) -> "$key=${escape(value)}" }
    }

    private fun escape(value: String): String = value.replace("%", "%25").replace(";", "%3B")
}
