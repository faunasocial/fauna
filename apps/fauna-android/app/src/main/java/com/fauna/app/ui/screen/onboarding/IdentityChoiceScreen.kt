package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.OnboardingHost
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.FfiSignOutResidue
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids

@HiltViewModel
class IdentityChoiceVM @Inject constructor(
    val host: OnboardingHost,
) : ViewModel() {
    fun beginCreate() = host.machine.beginCreateIdentity()
    fun beginImport() = host.machine.beginImportIdentity()
    fun beginRecoverLostBox() = host.machine.beginRecoverLostBox()
    fun beginRecoveryEntry() = host.machine.beginRecoveryEntry()
}

/**
 * The wizard's root surface — and the one a sign-out lands on, which is why it
 * paints [signOutResidue].
 *
 * `signOutResidue` is what the erase could not remove, while it still owes work
 * (`account-scoping.md` § Erasure follows scope → *the residue surface*). It is
 * painted as ui.yaml's `sign-out-residue` view — its message and Remove Again —
 * an optional element of `identity_choice`, NOT the global `warning-message`
 * (§ global scopes that one to *authenticated* pages, and a sign-out has just
 * left them). `null` — a clean sweep, or any other entry to the wizard — paints
 * no view at all. [onRemoveAgain] runs the shared re-sweep; the caller owns it.
 */
@Composable
fun IdentityChoiceScreen(
    navController: NavController,
    signOutResidue: FfiSignOutResidue? = null,
    onRemoveAgain: (FfiSignOutResidue) -> Unit = {},
    vm: IdentityChoiceVM = hiltViewModel(),
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(
            stringResource(R.string.onboarding_identity_choice_title),
            style = MaterialTheme.typography.headlineMedium,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            stringResource(R.string.onboarding_identity_choice_subtitle),
            style = MaterialTheme.typography.bodyLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(48.dp))

        Button(
            onClick = {
                vm.beginCreate()
                navController.navigate("onboarding/identity-created")
            },
            modifier = Modifier.fillMaxWidth().testTag(Ids.CREATE_IDENTITY_BUTTON),
        ) { Text(stringResource(R.string.onboarding_identity_choice_create_new)) }

        Spacer(Modifier.height(12.dp))

        OutlinedButton(
            onClick = {
                vm.beginImport()
                navController.navigate("onboarding/identity-import")
            },
            modifier = Modifier.fillMaxWidth().testTag(Ids.IMPORT_IDENTITY_BUTTON),
        ) { Text(stringResource(R.string.onboarding_identity_choice_import_existing)) }

        Spacer(Modifier.height(12.dp))

        // The phrase-only IDENTITY restore (onboarding.md § 1 Identity) — a
        // lost identity, distinct from the lost-box NEST recovery below it.
        OutlinedButton(
            onClick = {
                vm.beginRecoveryEntry()
                navController.navigate(RECOVERY_ENTRY_ROUTE)
            },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RESTORE_FROM_RECOVERY_KIT_BUTTON),
        ) { Text(stringResource(R.string.onboarding_identity_choice_restore_from_recovery_kit)) }

        Spacer(Modifier.height(12.dp))

        // Fresh-client recovery entry (box-recovery.md § Recovery UI (step 4)).
        // Routes through identity_import with recovery intent (the identity
        // must be loaded to decrypt the deployment-seed plane), then
        // lands on nest_recovery once a surviving nest resolves as
        // already-owned (Q2-A, via handle_entry).
        TextButton(
            onClick = {
                vm.beginRecoverLostBox()
                navController.navigate("onboarding/identity-import")
            },
            modifier = Modifier.fillMaxWidth().testTag(Ids.RECOVER_LOST_BOX_BUTTON),
        ) { Text(stringResource(R.string.onboarding_identity_choice_recover_lost_box)) }

        SignOutResidueView(signOutResidue, onRemoveAgain)
    }
}

/**
 * The `sign-out-residue` view: the shared line (the `Rendered` copy naming the
 * button beside it, or the retry's own refusal) and Remove Again. Every word is
 * shared Rust's; this only localizes and lays it out. Present exactly while the
 * residue owes work — `null` paints nothing.
 */
@Composable
internal fun SignOutResidueView(
    residue: FfiSignOutResidue?,
    onRemoveAgain: (FfiSignOutResidue) -> Unit,
) {
    if (residue == null) return
    val line = resolveLocalized(LocalContext.current, residue.line()).orEmpty()
    Column(
        modifier = Modifier.fillMaxWidth().padding(top = 16.dp).testTag(Ids.SIGN_OUT_RESIDUE),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            line,
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.testTag(Ids.SIGN_OUT_RESIDUE_MESSAGE),
        )
        Spacer(Modifier.height(8.dp))
        OutlinedButton(
            onClick = { onRemoveAgain(residue) },
            modifier = Modifier.testTag(Ids.SIGN_OUT_RESIDUE_RETRY_BUTTON),
        ) { Text(stringResource(R.string.settings_sign_out_residue_retry)) }
    }
}
