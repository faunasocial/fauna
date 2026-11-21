package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.MailUsableVM
import social.fauna.generated.Ids
import uniffi.fauna_core.Possibility

/**
 * The entry gate of the six mail sub-pages (Mail aliases, Mail lists, Mail
 * list members, Export mailbox, Import mailbox, Mail spam) — `mail-settings.md`
 * § Errors & edge cases, the mail-usable gate. Wraps each sub-page's ROUTE, so
 * a visit by any door (the hub's row, a deep link, the e2e route patch) while
 * the shared answer is not possible composes only the page's heading, the
 * reason and Back — the sub-page's own view-model (and any session it would
 * mint) is never built. Possible → [content], the screen as built.
 */
@Composable
fun MailSubpageGate(
    title: String,
    navController: NavController,
    usableVm: MailUsableVM = hiltViewModel(),
    content: @Composable () -> Unit,
) {
    val answer by usableVm.answer.collectAsState()
    MailSubpageGateContent(
        answer = answer,
        title = title,
        onBack = { navController.popBackStack() },
        content = content,
    )
}

/** The stateless half of [MailSubpageGate], JVM-testable with a seeded answer. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MailSubpageGateContent(
    answer: Possibility,
    title: String,
    onBack: () -> Unit,
    content: @Composable () -> Unit,
) {
    if (answer.isPossible) {
        content()
        return
    }
    // Absent: the box can never serve mail — the Mail & Calendar page's own
    // unavailable notice; blocked: the shared reason.
    val reason = if (answer is Possibility.Absent) {
        stringResource(R.string.mail_settings_unavailable_notice)
    } else {
        localized(answer.blockedReason).orEmpty()
    }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(title, modifier = Modifier.testTag(Ids.PAGE_HEADING)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.common_back),
                        )
                    }
                },
            )
        },
    ) { padding ->
        Column(modifier = Modifier.padding(padding).padding(16.dp).fillMaxSize()) {
            Text(
                reason,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.MAIL_SUBPAGE_GATE_REASON),
            )
        }
    }
}
