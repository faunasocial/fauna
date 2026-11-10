package com.fauna.app.ui.components

import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import com.fauna.app.ui.util.localized
import com.fauna.ffi.FfiShowAgain
import social.fauna.generated.Ids

/**
 * The reporter's show-again beside a reported placeholder
 * (`content-policy-show-again-button`, `moderation.md` § Corollary — the
 * reporter's hide is theirs to undo), shared by the feed card and the
 * conversation bubble. [showAgain] is the shared fold's answer
 * (`ContentPolicyInputs.showAgainFor`) — `null` paints nothing — and its label
 * names the scope the fold picked; a press hands the matched ids to
 * [onShowAgain], which runs the un-hide (`ReportSheetStore.showAgain`).
 */
@Composable
fun ShowAgainButton(showAgain: FfiShowAgain?, onShowAgain: (List<String>) -> Unit) {
    if (showAgain == null) return
    TextButton(
        onClick = { onShowAgain(showAgain.ids) },
        modifier = Modifier.testTag(Ids.CONTENT_POLICY_SHOW_AGAIN_BUTTON),
    ) {
        Text(localized(showAgain.label).orEmpty())
    }
}
