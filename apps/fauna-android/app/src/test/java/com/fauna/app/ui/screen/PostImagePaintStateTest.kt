package com.fauna.app.ui.screen

import android.graphics.Bitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.screen.feed.PostImagePaint
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * `post-image` answers `get_attr(post-image, state)` the way linux, apple and
 * windows do: `painted` once decoded bytes reach the picture, `placeholder`
 * while it is an empty box (loading, or a sealed item that does not open).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PostImagePaintStateTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun assertState(want: String) {
        composeTestRule.onNodeWithTag("post-image").assert(
            SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, want),
        )
    }

    @Test
    fun aDecodedPictureIsPainted() {
        val bmp = Bitmap.createBitmap(2, 2, Bitmap.Config.ARGB_8888).asImageBitmap()
        composeTestRule.setContent { PostImagePaint(bmp, null) }
        assertState("painted")
    }

    @Test
    fun noBytesIsAPlaceholder() {
        composeTestRule.setContent { PostImagePaint(null, null) }
        assertState("placeholder")
    }
}
