package com.fauna.app.ui.components

import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.fauna.app.ui.navigation.LocalAppMessages
import social.fauna.generated.Ids

/**
 * A modal form's own `error-message` — where a refused or failed submit of an
 * open dialog says why (`docs/goal/ui/README.md` § Offer only what is possible,
 * rule 7: the form's while it is open, else the page's). Absent while [error]
 * is null (e2e convention 2's rider). While it shows, the page's banner stands
 * down — its error is cleared, so at most one `error-message` shows and the
 * test agent's funnel never reads a stale page error over the dialog's.
 */
@Composable
fun ModalFormErrorText(error: String?, modifier: Modifier = Modifier) {
    if (error == null) return
    val appMessages = LocalAppMessages.current
    LaunchedEffect(error) { appMessages.clearError() }
    Text(
        text = error,
        color = MaterialTheme.colorScheme.error,
        style = MaterialTheme.typography.bodySmall,
        modifier = modifier.padding(top = 4.dp).testTag(Ids.ERROR_MESSAGE),
    )
}
