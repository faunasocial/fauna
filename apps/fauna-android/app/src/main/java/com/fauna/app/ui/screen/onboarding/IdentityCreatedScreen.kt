package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
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
import com.fauna.app.core.ShellLog
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.components.CopyButton
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids

@HiltViewModel
class IdentityCreatedVM @Inject constructor(
    val host: OnboardingHost,
    private val registry: com.fauna.ffi.FfiAccountRegistry,
) : ViewModel() {
    val generatedSecret: String
        get() = host.machine.generatedSecret() ?: ""

    /**
     * Per docs/goal/behavior/onboarding.md §1: confirmGeneratedIdentity returns
     * the secret hex which the client persists immediately. The wizard
     * doesn't own persistence — this is the durable commit point.
     *
     * Routed through the SHARED confirm-identity moment
     * (`fauna_client_accounts::persist_confirmed_identity`, the FFI
     * `confirmIdentity`) — the one call every app's confirm arm makes, in both
     * modes. On a first-run wizard it creates the per-actor account and READS
     * THE SECRET BACK (an infallible store setter would report success on a
     * keystore that kept nothing — at the one write whose silent failure
     * destroys an account outright), then activates it.
     *
     * **In append mode it writes nothing** (`host.appendMode`;
     * long-term-store.md § Downgrade mirror + abandoned-append recovery,
     * onboarding.md § Multi-account): the appended identity stays in the wizard
     * machine (`effectiveSecret()`) until its own terminal registers and
     * switches ([com.fauna.app.ui.viewmodel.AccountSettingsVM.completeAddAccount]),
     * so backing out and cancelling leaves the live session exactly as it was.
     * The rule lives in shared Rust, not in an app-side `if`.
     *
     * Failure surfaces log-only (the cross-app convention for a failed store
     * write); navigation proceeds either way — if persistence failed the user
     * re-onboards next launch, the same outcome the wizard already handles.
     */
    fun confirm() {
        val secret = host.machine.confirmGeneratedIdentity()
        try {
            registry.confirmIdentity(secret, host.appendMode)
        } catch (e: Exception) {
            ShellLog.w("IdentityCreatedVM", "persist_confirmed_identity (registry commit + read-back) failed: ${e.message}")
        }
    }
}

@Composable
fun IdentityCreatedScreen(
    navController: NavController,
    vm: IdentityCreatedVM = hiltViewModel(),
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_identity_created_title),
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_identity_created_desc),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(24.dp))

        Text(
            stringResource(R.string.onboarding_identity_created_secret_key_label),
            style = MaterialTheme.typography.labelMedium,
        )
        Spacer(Modifier.height(8.dp))

        Card(
            modifier = Modifier.fillMaxWidth(),
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
        ) {
            Text(
                text = vm.generatedSecret,
                style = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                modifier = Modifier
                    .padding(16.dp)
                    .testTag(Ids.SECRET_KEY_DISPLAY),
            )
        }

        Spacer(Modifier.height(8.dp))
        CopyButton(
            testTag = Ids.SECRET_KEY_COPY_BTN,
            text = vm.generatedSecret,
            label = stringResource(R.string.common_copy),
            modifier = Modifier.align(Alignment.End),
        )

        Spacer(Modifier.height(16.dp))
        Text(
            stringResource(R.string.onboarding_identity_created_warning),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
        )

        Spacer(Modifier.height(32.dp))

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            OutlinedButton(
                onClick = { navController.popBackStack() },
                modifier = Modifier.testTag(Ids.IDENTITY_CREATED_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }

            Button(
                onClick = {
                    vm.confirm()
                    // The machine decides: the recovery-kit offer (android
                    // declares setRendersRecoveryKit), else handle entry.
                    navController.navigate(routeForStep(vm.host.machine.step()))
                },
                modifier = Modifier.testTag(Ids.IDENTITY_CONTINUE_BUTTON),
            ) { Text(stringResource(R.string.onboarding_identity_created_continue)) }
        }
    }
}
