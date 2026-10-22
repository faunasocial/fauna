package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.ffi.faunaFfiTermsUrl
import social.fauna.generated.Ids

/**
 * The terms-and-content-guidelines link: label `settings.terms_link`, opens the
 * shared `fauna_core::TERMS_URL` (read through `faunaFfiTermsUrl()`, never a
 * literal) in the browser. Used by the onboarding notice below and by Settings'
 * About row (`settings-terms-link`).
 */
@Composable
fun TermsLinkButton(testTag: String, modifier: Modifier = Modifier) {
    val uriHandler = LocalUriHandler.current
    TextButton(
        onClick = { runCatching { uriHandler.openUri(faunaFfiTermsUrl()) } },
        modifier = modifier.testTag(testTag),
    ) { Text(stringResource(R.string.settings_terms_link)) }
}

/**
 * The by-continuing notice — one sentence with the link, placed above a wizard
 * page's bottom row on `invite_request`, `claim_code` and `nest_provisioning`
 * (always) and `handle_entry` (while the shared `terms_notice_visible` is true).
 * Nothing is persisted: no checkbox, no record
 * (`terms-and-content-guidelines.md` § The decisions, item 3).
 */
@Composable
fun TermsNotice(modifier: Modifier = Modifier) {
    Column(modifier = modifier.fillMaxWidth().padding(vertical = 8.dp)) {
        Text(
            stringResource(R.string.onboarding_terms_notice),
            modifier = Modifier.testTag(Ids.ONBOARDING_TERMS_NOTICE),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        TermsLinkButton(Ids.ONBOARDING_TERMS_LINK)
    }
}
