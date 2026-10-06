package com.fauna.app.ui.util

import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Conformance for the android value-formatting resolver pipeline
 * (`docs/goal/behavior/value-formatting.md`). The shared-Rust formatters
 * (`fauna_core::format`, `fauna_provisioning::progress`) compute the bucket /
 * unit *decision* and return an i18n key + args via `LocalizedText`;
 * [resolveLocalized] (used by [ValueFormat] and the onboarding/devices screens)
 * must render those keys EXACTLY to the strings the goal doc prescribes, through
 * the REAL generated android string resources (`res/values/i18n_strings.xml`,
 * via Robolectric).
 *
 * Scope: this locks the **android half** — the generated `size_*` / `time_*` /
 * `onboarding_nest_provisioning_elapsed_template` templates plus the positional
 * arg substitution. The FFI bucket/threshold *decision itself* (which 1024-unit,
 * which time bucket, the dropped-trailing-`.0` rounding) is locked separately by
 * fauna-core's Rust unit tests and the web/windows/linux conformance tests; a
 * real-FFI cross-language test on android needs the host emulator and is out of
 * scope in this environment (compile-verification only).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ValueFormatResolverTest {
    private val context get() = RuntimeEnvironment.getApplication()

    private fun resolve(key: String, vararg args: Pair<String, String>): String? =
        resolveLocalized(context, LocalizedText(key, args.toMap()))

    @Test
    fun byteSizeKeys_renderLocaleInvariantUnits() {
        // fauna_core::format::byte_size returns the already-rounded {value} +
        // the unit key; the android template supplies the unit symbol.
        assertEquals("512 B", resolve("size.bytes", "value" to "512"))
        assertEquals("1.5 KB", resolve("size.kb", "value" to "1.5"))
        assertEquals("5 MB", resolve("size.mb", "value" to "5"))
        assertEquals("3 GB", resolve("size.gb", "value" to "3"))
        assertEquals("1 TB", resolve("size.tb", "value" to "1"))
    }

    @Test
    fun relativeTimeKeys_renderAbbreviatedForms() {
        assertEquals("just now", resolve("time.just_now"))
        assertEquals("5m ago", resolve("time.minutes_ago", "count" to "5"))
        assertEquals("2h ago", resolve("time.hours_ago", "count" to "2"))
        assertEquals("3d ago", resolve("time.days_ago", "count" to "3"))
    }

    @Test
    fun multiArgKey_substitutesByName_regardlessOfArgOrder() {
        // mail_aliases.hits_with_last = "{count} hits · last {date}". The args
        // arrive in a Rust HashMap (libs/fauna-core/src/localized.rs) whose
        // iteration order is non-deterministic, so the resolver MUST substitute
        // by NAME — not by positional Map-iteration order. Both orderings render
        // identically (value-formatting.md § android resolves by name).
        val expected = "3 hits · last Jun 1"
        assertEquals(
            expected,
            resolve("mail_aliases.hits_with_last", "count" to "3", "date" to "Jun 1"),
        )
        assertEquals(
            expected,
            resolve("mail_aliases.hits_with_last", "date" to "Jun 1", "count" to "3"),
        )
    }

    @Test
    fun provisioningElapsedKey_rendersTemplate() {
        assertEquals(
            "7s elapsed",
            resolve("onboarding.nest_provisioning.elapsed_template", "seconds" to "7"),
        )
    }

    @Test
    fun emptyOrNull_returnsNull() {
        assertEquals(null, resolveLocalized(context, LocalizedText("", emptyMap())))
        assertEquals(null, resolveLocalized(context, null))
    }

    @Test
    fun unknownKey_returnsRawKeyToSurfaceMissingTranslation() {
        assertEquals("no.such.key", resolve("no.such.key"))
    }
}
