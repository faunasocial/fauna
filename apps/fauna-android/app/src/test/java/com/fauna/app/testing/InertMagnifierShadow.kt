package com.fauna.app.testing

import android.widget.Magnifier
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements

/**
 * Makes the platform [Magnifier] inert under Robolectric.
 *
 * Compose's text selection shows the platform magnifier (API 28+) while a
 * selection gesture drags, and dismisses it when the gesture ends. Robolectric
 * gives the magnifier's popup no `Surface`, so the real `dismiss()` throws a
 * NullPointerException (`Magnifier$InternalPopupWindow.destroy`), and any test
 * that drives a long-press selection dies there. The magnifier is a purely
 * visual lens: a test that asserts what a selection does, rather than what the
 * lens looks like, installs this shadow with
 * `@Config(shadows = [InertMagnifierShadow::class])`.
 */
@Implements(value = Magnifier::class, minSdk = 28)
class InertMagnifierShadow {

    @Implementation
    fun show(sourceCenterX: Float, sourceCenterY: Float) = Unit

    @Implementation
    fun show(
        sourceCenterX: Float,
        sourceCenterY: Float,
        magnifierCenterX: Float,
        magnifierCenterY: Float,
    ) = Unit

    @Implementation
    fun update() = Unit

    @Implementation
    fun dismiss() = Unit
}
