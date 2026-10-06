package com.fauna.app.ui.components

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.fauna.app.ui.util.localized
import social.fauna.generated.Ids

/**
 * Displays a content classification label (e.g. "spam:0.85") as a colored badge.
 * Format: "category:confidence" where confidence is 0.0-1.0.
 *
 * The category → label/icon/colour map is **not** hard-coded here — it is the
 * shared `fauna_core::content_category::content_label_style` (UniFFI
 * `com.fauna.ffi.contentLabelStyle`), so every app renders the canonical
 * 5-category vocabulary identically (moderation.md § Where logic lives; priority
 * #1/#2). The label is resolved through the android i18n pipeline; `tint`/`accent`
 * are hex colours this view maps to Compose `Color`.
 */
@Composable
fun ContentLabelBadge(label: String, modifier: Modifier = Modifier) {
    val category = label.split(":", limit = 2).firstOrNull() ?: label
    val style = remember(category) { com.fauna.ffi.contentLabelStyle(category) }
    val displayText = localized(style.label).orEmpty()
    val tint = remember(style.tint) { Color(android.graphics.Color.parseColor(style.tint)) }
    val accent = remember(style.accent) { Color(android.graphics.Color.parseColor(style.accent)) }

    Row(
        modifier = modifier
            .testTag(Ids.CONTENT_LABEL_BADGE)
            .clip(RoundedCornerShape(999.dp))
            .background(tint.copy(alpha = 0.15f))
            .padding(horizontal = 6.dp, vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(
            text = style.icon,
            fontSize = 10.sp
        )
        Spacer(modifier = Modifier.width(3.dp))
        Text(
            text = displayText,
            fontSize = 10.sp,
            fontWeight = FontWeight.Medium,
            color = accent
        )
    }
}
