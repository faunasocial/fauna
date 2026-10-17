package com.fauna.app.ui.screen

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.ui.util.stringResourceFmt
import social.fauna.generated.Ids

/**
 * The locked launch surface, ui.yaml page `launch_account_locked`
 * (`docs/goal/ui/sessions.md` § Layout & flow → *The locked surface (leg 2)*;
 * behavior `devices.md` § The locked state). The standing notice — that the
 * account is locked, when the lock ends, and that a lock the owner did not set
 * means somebody holds the secret key — and its ONE action, the stolen-identity
 * ceremony. No Retry (nothing clears the lock before its time; the machine's
 * own refresh at that time signs back in) and no fallthrough ("use a different
 * nest" would misstate the problem). [unlockTime] is already rendered through
 * the shared `formatUnixLocal`. Twin of tui's `locked.rs` and linux's launch
 * view `AccountLocked` phase.
 */
@Composable
fun LaunchAccountLockedScreen(
    unlockTime: String,
    onStolen: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_launch_account_locked_title),
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(16.dp))
        // One element, the three sentences in the ratified order — the witness
        // reads them as one notice.
        Text(
            listOf(
                stringResource(R.string.onboarding_launch_account_locked),
                stringResourceFmt(R.string.onboarding_launch_account_locked_until, unlockTime),
                stringResource(R.string.onboarding_launch_account_locked_not_yours),
            ).joinToString(" "),
            style = MaterialTheme.typography.bodyLarge,
            textAlign = TextAlign.Center,
            modifier = Modifier.testTag(Ids.LAUNCH_ACCOUNT_LOCKED_NOTICE),
        )
        Spacer(Modifier.height(32.dp))
        Button(
            onClick = onStolen,
            modifier = Modifier
                .fillMaxWidth()
                .testTag(Ids.LAUNCH_ACCOUNT_LOCKED_STOLEN_BUTTON),
        ) { Text(stringResource(R.string.settings_recovery_kit_stolen)) }
    }
}

/**
 * The onboarding step `identity_stolen_entry`, reached from the locked notice:
 * the kit, the type-to-confirm `SUCCEED`, the irreversible press — composed of
 * existing elements only (`recovery-entry-phrase-field`,
 * `identity-stolen-confirm-field`, `identity-stolen-button`,
 * `recovery-entry-back-button`, `error-message`). The gate starts unarmed and
 * is re-checked in the action arm ([com.fauna.app.ui.viewmodel.AppLaunchVM.succeedStolenLocked]);
 * nothing typed here is persisted.
 */
@Composable
fun IdentityStolenEntryScreen(
    phrase: String,
    confirm: String,
    confirmWord: String,
    busy: Boolean,
    error: String?,
    onPhraseChange: (String) -> Unit,
    onConfirmChange: (String) -> Unit,
    onSucceed: () -> Unit,
    onBack: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            stringResource(R.string.settings_recovery_kit_stolen),
            style = MaterialTheme.typography.headlineSmall,
        )
        Text(
            stringResource(R.string.settings_recovery_kit_stolen_warning),
            style = MaterialTheme.typography.bodyMedium,
        )
        OutlinedTextField(
            value = phrase,
            onValueChange = onPhraseChange,
            placeholder = { Text(stringResource(R.string.settings_recovery_kit_kit_phrase_placeholder)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_ENTRY_PHRASE_FIELD),
        )
        OutlinedTextField(
            value = confirm,
            onValueChange = onConfirmChange,
            singleLine = true,
            placeholder = { Text(stringResource(R.string.settings_recovery_kit_stolen_confirm_placeholder)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.IDENTITY_STOLEN_CONFIRM_FIELD),
        )
        error?.let {
            Text(
                it,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodySmall,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
            )
        }
        Button(
            onClick = onSucceed,
            enabled = !busy && confirm == confirmWord,
            colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
            modifier = Modifier.fillMaxWidth().testTag(Ids.IDENTITY_STOLEN_BUTTON),
        ) { Text(stringResource(R.string.settings_recovery_kit_stolen)) }
        OutlinedButton(
            onClick = onBack,
            enabled = !busy,
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVERY_ENTRY_BACK_BUTTON),
        ) { Text(stringResource(R.string.common_back)) }
    }
}
