package com.fauna.app.ui.util

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_client_connected_apps.ComposedText
import uniffi.fauna_core.LocalizedText

/**
 * Pins how Android resolves a shared-Rust [ComposedText]
 * (`fauna_client_connected_apps::compose`): each `nested` text resolves first
 * and fills its argument, then the outer template resolves. The composition
 * itself (order, which key, which args) is pinned in Rust; this pins only the
 * Kotlin half, with hand-built values so no native library is needed.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ComposedTextTest {

    private val context get() = ApplicationProvider.getApplicationContext<android.content.Context>()

    private fun verbatim(text: String) = LocalizedText("connected_apps.verbatim", mapOf("text" to text))

    @Test
    fun aPlainTextFillsItsOwnArguments() {
        val text = ComposedText(
            text = LocalizedText("connected_apps.created", mapOf("time" to "2023-11-14 22:13")),
            nested = emptyMap(),
        )
        assertEquals("Added 2023-11-14 22:13", resolveComposed(context, text))
    }

    @Test
    fun aNestedTextResolvesFirstAndFillsItsArgument() {
        val head = ComposedText(
            text = LocalizedText("connected_apps.row_head", emptyMap()),
            nested = mapOf(
                "name" to verbatim("Graysky"),
                "badge" to LocalizedText("connected_apps.class_device", emptyMap()),
            ),
        )
        val resolved = resolveComposed(context, head)
        assertEquals("Graysky · App on a device", resolved)
    }

    @Test
    fun aBulletIndentsItsNestedText() {
        val bullet = ComposedText(
            text = LocalizedText("connected_apps.bullet", emptyMap()),
            nested = mapOf("text" to verbatim("Post as you")),
        )
        assertEquals("  • Post as you", resolveComposed(context, bullet))
    }

    @Test
    fun anUnknownKeySurfacesRatherThanRenderingBlank() {
        val text = ComposedText(LocalizedText("connected_apps.no_such_key", emptyMap()), emptyMap())
        assertEquals("connected_apps.no_such_key", resolveComposed(context, text))
    }
}
