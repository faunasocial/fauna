package com.fauna.app.ui.util

import android.content.Context
import androidx.annotation.StringRes
import androidx.compose.runtime.Composable
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import uniffi.fauna_core.LocalizedText

/**
 * The single canonical resolver for a shared-machine [LocalizedText] — an i18n
 * key + args computed once in shared Rust (state-machine messages, devices /
 * folder errors, value formatting via [ValueFormat]). The *decision* lives in
 * shared Rust (`fauna_core` / `fauna_provisioning`); this only maps the returned
 * key to the generated Android string resource and substitutes the args, per
 * priority #2 / `docs/goal/behavior/value-formatting.md`.
 *
 * Returns null when there's nothing to show (null arg or empty key); returns the
 * raw key when no matching resource exists, so a missing translation surfaces
 * rather than rendering blank.
 *
 * Args are substituted **by name**: each `{name}` token in the looked-up
 * template is replaced with `args[name]`. The generated Android resources keep
 * their `{name}` placeholders (the i18n generator no longer rewrites them to
 * positional `%N$s` — see `_android_value`), so resolution is independent of the
 * Rust `HashMap` iteration order that backs [LocalizedText.args] (which is
 * non-deterministic). This matches every other app — windows `Strings.Resolve`,
 * web `L()`, Apple `Bundle`, Linux `fauna_i18n`, and shared Rust
 * `LocalizedText::resolve` — and lets multi-arg keys (e.g. `mail_aliases.hits_with_last`
 * = "{count} hits · last {date}") render deterministically.
 */
fun resolveLocalized(context: Context, text: LocalizedText?): String? {
    if (text == null || text.key.isEmpty()) return null
    val resName = text.key.replace('.', '_')
    val resId = context.resources.getIdentifier(resName, "string", context.packageName)
    if (resId == 0) return text.key
    val template = context.getString(resId)
    return if (text.args.isEmpty()) template else substituteNamed(template, text.args)
}

/** Composable convenience over [resolveLocalized] using the ambient context. */
@Composable
fun localized(text: LocalizedText?): String? = resolveLocalized(LocalContext.current, text)

/**
 * [resolveLocalized], but each **argument** is first put through the same
 * resource lookup too — for templates whose substitution is itself a
 * translatable term rather than raw data. The Kotlin twin of shared Rust
 * `LocalizedText::resolve_nested`: the gated-feature plane's exhaustion
 * sentence and quota-cell label both carry a nested key as an argument (e.g.
 * `window = "features.window_day"`), so plain [resolveLocalized] would leave
 * the raw key inside the rendered sentence. An argument that is not itself a
 * resource key simply misses the lookup and substitutes verbatim.
 */
fun resolveLocalizedNested(context: Context, text: LocalizedText?): String? {
    if (text == null || text.key.isEmpty()) return null
    val resName = text.key.replace('.', '_')
    val resId = context.resources.getIdentifier(resName, "string", context.packageName)
    if (resId == 0) return text.key
    val template = context.getString(resId)
    if (text.args.isEmpty()) return template
    val resolvedArgs = text.args.mapValues { (_, v) ->
        val argResName = v.replace('.', '_')
        val argResId = context.resources.getIdentifier(argResName, "string", context.packageName)
        if (argResId == 0) v else context.getString(argResId)
    }
    return substituteNamed(template, resolvedArgs)
}

/** Composable convenience over [resolveLocalizedNested] using the ambient context. */
@Composable
fun localizedNested(text: LocalizedText?): String? =
    resolveLocalizedNested(LocalContext.current, text)

/**
 * A gated-feature quota cell's headroom sentence, fully composed — the Kotlin
 * twin of shared Rust `fauna_client_features::row::cell_value_text`. When
 * [com.fauna.ffi.FfiLimitCell.magnitudes] is set (`volume` cells), its two
 * localized magnitudes are resolved first and substituted into `value`'s
 * `{remaining}`/`{limit}` holes — a [LocalizedText] argument is a flat string,
 * so a magnitude that is itself localized ("1 TB") has to be composed before
 * the outer template resolves. Without magnitudes, `{remaining}`/`{limit}` are
 * already finished numbers, so plain [resolveLocalized] is enough.
 */
fun cellValueText(context: Context, cell: com.fauna.ffi.FfiLimitCell): String {
    val magnitudes = cell.magnitudes ?: return resolveLocalized(context, cell.value) ?: ""
    val composedArgs = cell.value.args.toMutableMap()
    composedArgs["remaining"] = resolveLocalized(context, magnitudes.remaining) ?: ""
    composedArgs["limit"] = resolveLocalized(context, magnitudes.limit) ?: ""
    val resName = cell.value.key.replace('.', '_')
    val resId = context.resources.getIdentifier(resName, "string", context.packageName)
    val template = if (resId == 0) cell.value.key else context.getString(resId)
    return substituteNamed(template, composedArgs)
}

/**
 * The `custody-holder-receipt-status` line (`docs/goal/ui/devices.md` § Custody
 * facet) — the A7 three-state honesty rule, where fresh / stale / no-receipt-yet
 * are three different strings that never collapse or go empty.
 *
 * Which state maps to which key is decided in shared Rust and already folded
 * into the row; this only resolves that key and substitutes `{when}`. The shared
 * side deliberately hands back epoch SECONDS rather than a rendered timestamp —
 * `format_unix_local` needs the OS timezone database and is native-only, so
 * formatting there would cost `fauna-client-capabilities` the wasm-cleanliness
 * the web leg depends on. Formatting rides the SHARED formatter over FFI, not
 * `DateTimeFormatter`, so a timestamp reads identically on all seven apps.
 * Kotlin twin of linux `i18n::custody_receipt_status` / tui's
 * `receipt_status_text`.
 */
fun custodyReceiptStatusText(
    context: Context,
    receipt: uniffi.fauna_client_capabilities.CustodyReceiptRowView,
): String {
    val text = resolveLocalized(context, receipt.statusLabel).orEmpty()
    val secs = receipt.attestedAtSecs ?: return text
    return text.replace("{when}", com.fauna.ffi.formatUnixLocal(secs))
}

/**
 * The `custody-holder-held-bytes` line — held bytes against the budget in force.
 *
 * The two inner byte texts are themselves [LocalizedText] and are resolved
 * first: a [LocalizedText] argument is a flat string, the same composition
 * [cellValueText] performs for a volume quota cell. `degraded` is **orthogonal
 * to freshness** — a fresh receipt can honestly report dropped payload — so its
 * marker appends to this line rather than replacing the status one. Kotlin twin
 * of linux `i18n::custody_held_bytes` / tui's `held_bytes_text`.
 */
fun custodyHeldBytesText(
    context: Context,
    receipt: uniffi.fauna_client_capabilities.CustodyReceiptRowView,
): String {
    val line = substituteNamed(
        resolveLocalized(context, receipt.heldBytesLabel).orEmpty(),
        mapOf(
            "held" to resolveLocalized(context, receipt.held).orEmpty(),
            "cap" to resolveLocalized(context, receipt.cap).orEmpty(),
        ),
    )
    if (!receipt.degraded) return line
    val badge = resolveLocalized(
        context,
        LocalizedText(com.fauna.ffi.custodyDegradedBadgeKey(), emptyMap()),
    ).orEmpty()
    return "$line — $badge"
}

private val NAMED_TOKEN = Regex("""\{(\w+)}""")

/**
 * Substitute each `{name}` token in [template] with `args[name]`, leaving any
 * token without a matching arg intact (so a missing arg surfaces diagnostically
 * rather than rendering blank). Order-independent — mirrors shared Rust
 * `LocalizedText::resolve` and windows `Strings.Resolve`.
 */
private fun substituteNamed(template: String, args: Map<String, String>): String =
    NAMED_TOKEN.replace(template) { m -> args[m.groupValues[1]] ?: m.value }

/**
 * Map [args] positionally onto the distinct `{name}` tokens of [template] in
 * first-appearance (textual) order. The direct-caller path for UI code that
 * passes positional args instead of a named [LocalizedText] map — mirrors
 * windows `Strings.Format`. Used by [stringResourceFmt].
 */
fun formatNamed(template: String, args: List<Any?>): String {
    val order = NAMED_TOKEN.findAll(template).map { it.groupValues[1] }.distinct().toList()
    val byName = order.zip(args.map { it?.toString() ?: "" }).toMap()
    return substituteNamed(template, byName)
}

/**
 * Compose convenience that resolves [id] and fills its `{name}` tokens with the
 * positional [args] in textual order. Use in place of `stringResource(id, arg…)`
 * for parameterized strings: the generated Android resources now carry `{name}`
 * placeholders (not `%N$s`), so Android's positional `getString(id, arg…)` would
 * not substitute them.
 */
@Composable
fun stringResourceFmt(@StringRes id: Int, vararg args: Any?): String =
    formatNamed(stringResource(id), args.toList())

/**
 * Non-Composable [stringResourceFmt] — resolves [id] off a [Context] and fills
 * its `{name}` tokens with the positional [args] in textual order. For call
 * sites outside composition (e.g. building a string in a callback).
 */
fun Context.getStringFmt(@StringRes id: Int, vararg args: Any?): String =
    formatNamed(getString(id), args.toList())
