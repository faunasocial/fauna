package com.fauna.app.ui.components

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import com.fauna.app.R
import com.fauna.app.core.ContactAskRender
import social.fauna.generated.Ids

/**
 * The supervised ward's in-app contact ask — the `contact-request-guardian-button`
 * / `contact-request-pending` pair (family-safety.md § Child-initiated contact
 * requests → *App affordance*). One composable, two hosts: the contacts page's
 * Find User result and another actor's profile page, exactly as tui paints the
 * pair on both (`apps/fauna-tui/src/contacts.rs`, `profile/mod.rs`; linux's
 * `views/contacts/guardian_ask.rs`).
 *
 * Stateless: the host computes [render] with [com.fauna.app.core.contactAskRender]
 * (durable pending first, the ask only after the TYPED refusal — rules (a),
 * (c)) and owns `error-message` (rule (b)). No supervision test here — both
 * inputs are supervised-only by construction (rule (h)). `null` paints nothing.
 * No offline-gate declaration: `fauna.family.contact.request` is
 * `OfflineQueued`, which the gate never greys.
 */
@Composable
fun GuardianAskPair(
    render: ContactAskRender?,
    askInFlight: Boolean,
    onAsk: () -> Unit,
    modifier: Modifier = Modifier,
) {
    when (render) {
        ContactAskRender.PENDING -> Text(
            stringResource(R.string.contacts_contact_request_pending),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = modifier.testTag(Ids.CONTACT_REQUEST_PENDING),
        )
        ContactAskRender.ASK -> OutlinedButton(
            onClick = onAsk,
            enabled = !askInFlight,
            modifier = modifier.testTag(Ids.CONTACT_REQUEST_GUARDIAN_BUTTON),
        ) { Text(stringResource(R.string.contacts_ask_guardian)) }
        null -> Unit
    }
}
