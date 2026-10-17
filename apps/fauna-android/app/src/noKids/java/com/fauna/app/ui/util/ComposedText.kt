package com.fauna.app.ui.util

import android.content.Context
import uniffi.fauna_client_connected_apps.ComposedText
import uniffi.fauna_core.LocalizedText

/**
 * Resolve a shared-Rust [ComposedText] (`fauna_client_connected_apps::compose`):
 * an i18n key plus arguments, some of which are themselves translatable. Each
 * `nested` entry is resolved first and fills the argument of its name (winning
 * over a plain arg of the same name), then the outer template resolves — the
 * Kotlin twin of `ComposedText::resolve`. The wording, order and joining
 * punctuation are the shared module's; this only maps keys to string resources.
 *
 * Lives in the `noKids` source set beside its only consumer: the kids flavor
 * carries no connected-apps FFI types.
 */
fun resolveComposed(context: Context, text: ComposedText): String {
    val args = text.text.args + text.nested.mapValues { (_, v) -> resolveLocalized(context, v).orEmpty() }
    return resolveLocalized(context, LocalizedText(text.text.key, args)).orEmpty()
}
