package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.components.CopyButton
import com.fauna.app.ui.components.QrCanvas
import com.fauna.ffi.FfiException
import com.fauna.ffi.qrMatrix
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids
import uniffi.fauna_core.QrMatrix

@HiltViewModel
class RecoveryKitVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun confirm() = host.machine.confirmRecoveryKit()
    fun skip() = host.machine.skipRecoveryKit()
}

/**
 * The recovery-kit offer (`onboarding.md` § 1 Identity), right after the
 * identity secret is confirmed — android's leg of the page tui led (linux
 * prior art: `apps/fauna-linux/src/views/onboarding/recovery_kit.rs`).
 *
 * The page **mints and displays only**: no nest exists at this position, so
 * registration + escrow run at the signed-in handoff (`OnboardingHost` takes
 * the confirmed root at `LoggedIn`; `PostAuthGlueVM.registerDeferredRecoveryKit`
 * registers THAT root over the shared `register_deferred_kit`). That is why
 * `recovery-kit-escrow-status` renders exactly one state here — the deferred
 * line. The machine routes here only because `OnboardingHost` declares
 * `setRendersRecoveryKit(true)`; the two land together.
 *
 * The display is the bare 64-hex (what a user copies onto paper); the QR and
 * the copy button both carry the machine's one `fauna://recovery` URI
 * (`identity-succession.md` § The RecoveryKey — *Which encoding each
 * affordance carries*). Every value is re-read off the machine on each tick.
 */
@Composable
fun RecoveryKitScreen(
    navController: NavController,
    vm: RecoveryKitVM = hiltViewModel(),
) {
    val tick by vm.host.tick.collectAsState()
    val machine = vm.host.machine
    val secret = remember(tick) { machine.recoveryKitSecretHex() }
    val uri = remember(tick) { machine.recoveryKitUri() }
    val matrix = remember(uri) { uri?.let(::encodeRecoveryQr) }
    val minted = secret != null
    val advance = { navController.navigate(routeForStep(machine.step())) }

    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_recovery_kit_title),
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_recovery_kit_desc),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_DESCRIPTION),
        )
        Spacer(Modifier.height(24.dp))

        Card(
            modifier = Modifier.fillMaxWidth(),
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
        ) {
            Text(
                text = secret ?: stringResource(R.string.onboarding_recovery_kit_not_minted),
                style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                modifier = Modifier.padding(16.dp).testTag(Ids.RECOVERY_KIT_SECRET_DISPLAY),
            )
        }

        Spacer(Modifier.height(8.dp))
        // The URI, read at composition from the machine — never the bare hex.
        CopyButton(
            testTag = Ids.RECOVERY_KIT_SECRET_COPY_BTN,
            text = uri.orEmpty(),
            label = stringResource(R.string.common_copy),
            enabled = uri != null,
            modifier = Modifier.align(Alignment.End),
        )

        if (matrix != null) {
            Spacer(Modifier.height(16.dp))
            QrCanvas(
                matrix = matrix,
                modifier = Modifier.size(220.dp).testTag(Ids.RECOVERY_KIT_QR),
            )
        }

        Spacer(Modifier.height(16.dp))
        Text(
            stringResource(R.string.onboarding_recovery_kit_escrow_deferred),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.testTag(Ids.RECOVERY_KIT_ESCROW_STATUS),
        )

        remember(tick) { machine.errorMessage() }?.ifEmpty { null }?.let {
            Spacer(Modifier.height(8.dp))
            Text(
                it,
                color = MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.ERROR_MESSAGE),
            )
        }

        Spacer(Modifier.height(32.dp))
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            // Skip is one click and never blocks onboarding; the minted root is
            // dropped so nothing registers at handoff, and Settings'
            // never-created warning tells the truth.
            OutlinedButton(
                onClick = {
                    vm.skip()
                    advance()
                },
                modifier = Modifier.testTag(Ids.RECOVERY_KIT_SKIP_BUTTON),
            ) { Text(stringResource(R.string.onboarding_recovery_kit_skip)) }

            Button(
                onClick = {
                    vm.confirm()
                    advance()
                },
                enabled = minted,
                modifier = Modifier.testTag(Ids.RECOVERY_KIT_CONFIRM_BUTTON),
            ) { Text(stringResource(R.string.onboarding_recovery_kit_confirm)) }
        }
    }
}

/** The kit URI as a QR matrix, or `null` when unencodable (the QR is then simply absent). */
private fun encodeRecoveryQr(uri: String): QrMatrix? =
    try {
        qrMatrix(uri)
    } catch (e: FfiException) {
        null
    }
