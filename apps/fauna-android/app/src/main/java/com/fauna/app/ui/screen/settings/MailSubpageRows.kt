package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AlternateEmail
import androidx.compose.material.icons.filled.Block
import androidx.compose.material.icons.filled.FileDownload
import androidx.compose.material.icons.filled.FileUpload
import androidx.compose.material.icons.filled.Group
import androidx.compose.material.icons.filled.List
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.isShown
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.paintedReason
import social.fauna.generated.Ids
import uniffi.fauna_core.Possibility

/**
 * One of the six mail sub-pages a Settings row opens (`settings.md` §
 * Navigation model: the same page set and ids as the desktop rail; only the
 * navigation widget differs on mobile). [key] is the ui.yaml page id the row's
 * `settings-nav-row[<key>]` carries; [route] is android's destination.
 */
internal data class MailSubpageRow(val key: String, val titleRes: Int, val icon: ImageVector, val route: String)

/** The six rows, in the rail's order (`settings.md` § Navigation model). */
internal val MAIL_SUBPAGE_ROWS = listOf(
    MailSubpageRow("mail-aliases", R.string.mail_aliases_title, Icons.Default.AlternateEmail, "settings/mail-aliases"),
    MailSubpageRow("mail-spam", R.string.mail_spam_title, Icons.Default.Block, "settings/mail-spam"),
    MailSubpageRow("mail-export", R.string.mail_export_title, Icons.Default.FileUpload, "settings/mail-export"),
    MailSubpageRow("mail-import", R.string.mail_import_title, Icons.Default.FileDownload, "settings/mail-import"),
    MailSubpageRow("mail-lists", R.string.mail_lists_title, Icons.Default.List, "settings/mail-lists"),
    MailSubpageRow("mail-list-members", R.string.mail_lists_members_title, Icons.Default.Group, "settings/mail-list-members"),
)

/**
 * The Settings landing's six mail sub-page rows, painted from the ONE shared
 * mail-usable answer (`mail-settings.md` § Errors & edge cases, the mail-usable
 * gate — `MailSettingsSnapshot::mail_usable()` over FFI, read by the session's
 * `MailUsableVM`; never re-derived here): **absent** → none composed (a box
 * that can never serve mail); **blocked** → each shown disabled, its reason
 * published on the row (`Modifier.paintedReason`, the driver's
 * `blocked_reason`) and painted once beneath the group, the Mail & Calendar row
 * above staying the one live step; **possible** → live. FFI-free, so a
 * Robolectric test drives it with a seeded answer.
 */
@Composable
internal fun MailSubpageRows(mailUsable: Possibility, onOpen: (route: String) -> Unit) {
    if (!mailUsable.isShown) return
    val live = mailUsable.isPossible
    val reason = localized(mailUsable.blockedReason)
    Column(modifier = Modifier.fillMaxWidth()) {
        for (row in MAIL_SUBPAGE_ROWS) {
            val label = stringResource(row.titleRes)
            ListItem(
                headlineContent = {
                    Text(
                        label,
                        color = if (live) Color.Unspecified
                        else MaterialTheme.colorScheme.onSurface.copy(alpha = 0.38f),
                    )
                },
                leadingContent = { Icon(row.icon, contentDescription = label) },
                modifier = Modifier
                    .testTag("${Ids.SETTINGS_NAV_ROW}[${row.key}]")
                    .paintedReason(if (live) null else reason)
                    .clickable(enabled = live) { onOpen(row.route) },
            )
        }
        if (!live) DisabledControlReasonText(reason, Modifier.padding(horizontal = 16.dp))
    }
}
