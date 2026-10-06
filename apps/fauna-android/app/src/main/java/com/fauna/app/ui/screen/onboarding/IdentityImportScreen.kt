package com.fauna.app.ui.screen.onboarding

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.ViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.core.ShellLog
import com.fauna.app.core.OnboardingHost
import com.fauna.ffi.parseIdentityImport
import dagger.hilt.android.lifecycle.HiltViewModel
import javax.inject.Inject
import social.fauna.generated.Ids

@HiltViewModel
class IdentityImportVM @Inject constructor(
    val host: OnboardingHost,
    private val registry: com.fauna.ffi.FfiAccountRegistry,
) : ViewModel() {
    var error by mutableStateOf<String?>(null)
        private set

    /**
     * Per docs/goal/behavior/onboarding.md §1: confirmImportedIdentity validates
     * the secret and returns the canonical hex on success. The client
     * persists the returned value (not the user-supplied input) and
     * surfaces validation errors via OnboardingError.
     *
     * The outer try/catch is for VALIDATION failures only (the wizard throws
     * OnboardingError on bad-format secrets). Persistence routes through the
     * SHARED confirm-identity moment (`confirmIdentity`, both modes — per-actor
     * account + secret read-back on a first run, nothing written in append
     * mode; see [IdentityCreatedVM.confirm]'s doc); its failure surfaces
     * log-only, the cross-app convention for a failed store write.
     */
    fun confirm(secret: String): Boolean {
        val validated = try {
            host.machine.confirmImportedIdentity(secret)
        } catch (e: Exception) {
            error = e.message ?: "Invalid secret key"
            return false
        }
        try {
            registry.confirmIdentity(validated, host.appendMode)
        } catch (e: Exception) {
            ShellLog.w("IdentityImportVM", "persist_confirmed_identity (registry commit + read-back) failed: ${e.message}")
        }
        error = null
        return true
    }

    /** QR payload is `(identity, handle)` per target doc §1.identity_import. */
    fun setHandleFromQr(handle: String) {
        host.machine.setCurrentHandle(handle)
    }

    /**
     * Import a pasted/scanned identity field through the shared
     * `fauna_core::identity_qr` parser (UniFFI `parseIdentityImport`): a bare 64-hex secret,
     * the `fauna://identity?secret=&handle=` query form, or the iOS colon form. web, iOS, and
     * android all parse identical input from one crate instead of each hand-rolling the input
     * grammar (priority #2/#4; onboarding.md §1.identity_import). The face returns an empty
     * list on a parse failure — surface [invalidMessage]; otherwise pre-fill the handle the
     * payload carried (when any) via [setHandleFromQr] before validating+persisting the
     * secret. Returns true when the caller should advance to handle entry.
     */
    fun importIdentity(input: String, invalidMessage: String): Boolean {
        val parts = parseIdentityImport(input.trim())
        if (parts.isEmpty()) {
            error = invalidMessage
            return false
        }
        val handle = parts.getOrNull(1).orEmpty()
        if (handle.isNotEmpty()) setHandleFromQr(handle)
        return confirm(parts[0])
    }
}

@Composable
fun IdentityImportScreen(
    navController: NavController,
    vm: IdentityImportVM = hiltViewModel(),
) {
    // Start on the paste tab (index 1) like the iOS sibling: the QR tab is a camera
    // placeholder until CameraX lands, so paste is the working default.
    var selectedTab by remember { mutableIntStateOf(1) }
    val tabTitles = listOf(
        stringResource(R.string.onboarding_identity_import_scan_tab),
        stringResource(R.string.onboarding_identity_import_paste_tab),
    )

    Column(modifier = Modifier.fillMaxSize()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(24.dp, 24.dp, 24.dp, 0.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            OutlinedButton(
                onClick = { navController.popBackStack() },
                modifier = Modifier.testTag(Ids.IDENTITY_IMPORT_BACK_BUTTON),
            ) { Text(stringResource(R.string.common_back)) }
            Spacer(Modifier.width(16.dp))
            Text(
                stringResource(R.string.onboarding_identity_import_title),
                style = MaterialTheme.typography.headlineSmall,
            )
        }

        TabRow(selectedTabIndex = selectedTab) {
            tabTitles.forEachIndexed { index, title ->
                Tab(
                    selected = selectedTab == index,
                    onClick = { selectedTab = index },
                    text = { Text(title) },
                )
            }
        }

        when (selectedTab) {
            0 -> ScanQrTab()
            1 -> PasteKeyTab(vm = vm, navController = navController)
        }

        // A local validation error wins; otherwise the machine's own reason —
        // the one a launch-time route set with the step
        // (`beginImportIdentityWithReason`, e.g. "this identity was
        // succeeded"), re-read on every machine tick like the other
        // observer-driven onboarding pages.
        val tick by vm.host.tick.collectAsState()
        val machineError = remember(tick) { vm.host.machine.errorMessage() }
        (vm.error ?: machineError?.ifEmpty { null })?.let {
            Text(
                it,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(horizontal = 24.dp).testTag(Ids.ERROR_MESSAGE),
            )
        }
    }
}

@Composable
private fun ScanQrTab() {
    // Camera-only QR tab, matching the iOS sibling's qrScannerSection (`qr-camera-view`).
    // Android has no CameraX yet, so this is a placeholder preview; actual import goes
    // through the paste tab (the shared parser there accepts a pasted `fauna://identity`
    // URI too, so no separate URI field is needed here). When CameraX lands, the scan
    // callback feeds the decoded payload into `vm.importIdentity` like iOS does.
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            stringResource(R.string.onboarding_identity_import_scan_subtitle),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        Card(
            modifier = Modifier
                .fillMaxWidth()
                .aspectRatio(1f)
                .testTag(Ids.QR_CAMERA_VIEW),
            colors = CardDefaults.cardColors(
                containerColor = MaterialTheme.colorScheme.surfaceVariant,
            ),
        ) {
            Box(
                modifier = Modifier.fillMaxSize(),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    text = "QR Camera\n(requires CameraX dependency)",
                    textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
        }
    }
}

@Composable
private fun PasteKeyTab(vm: IdentityImportVM, navController: NavController) {
    var secretInput by remember { mutableStateOf("") }
    val invalidMessage = stringResource(R.string.onboarding_identity_import_invalid_secret)

    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
    ) {
        Text(
            stringResource(R.string.onboarding_identity_import_paste_subtitle),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))

        OutlinedTextField(
            value = secretInput,
            onValueChange = { secretInput = it },
            label = { Text(stringResource(R.string.onboarding_identity_import_paste_label)) },
            placeholder = { Text(stringResource(R.string.onboarding_identity_import_paste_placeholder)) },
            modifier = Modifier.fillMaxWidth().testTag(Ids.PASTE_SECRET_FIELD),
            singleLine = true,
        )

        Spacer(Modifier.height(16.dp))

        Button(
            // A bare 64-hex secret is the common case here, but the shared parser also
            // accepts the union — a pasted `fauna://identity` URI or the iOS colon form
            // (priority #4) — so a URL-bearing handle still pre-fills.
            onClick = {
                if (vm.importIdentity(secretInput, invalidMessage)) {
                    navController.navigate("onboarding/handle-entry")
                }
            },
            enabled = secretInput.isNotBlank(),
            modifier = Modifier.fillMaxWidth().testTag(Ids.IMPORT_SUBMIT_BUTTON),
        ) { Text(stringResource(R.string.onboarding_identity_import_import)) }
    }
}
