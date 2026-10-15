package com.fauna.app.ui.screen.settings

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.EncryptionSettingsVM
import social.fauna.generated.Ids
import uniffi.fauna_conversations.keypackagesLow

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EncryptionSettingsScreen(
    navController: NavController,
    vm: EncryptionSettingsVM = hiltViewModel()
) {
    val keyPackageCount by vm.keyPackageCount.collectAsState()
    val isPublishing by vm.isPublishing.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()

    LaunchedEffect(Unit) { vm.loadKeyCount() }

    EncryptionSettingsContent(
        keyPackageCount = keyPackageCount,
        isPublishing = isPublishing,
        errorMessage = errorMessage,
        onBack = { navController.popBackStack() },
        onRefreshKeys = vm::refreshKeys,
    )
}

/**
 * Stateless twin of [EncryptionSettingsScreen] (mirrors [PrivacySettingsContent] /
 * [MutedWordsContent]) \u2014 no Hilt, no VM (its one FFI call is the pure shared
 * predicate `keypackagesLow`), so a Robolectric test can drive it directly.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EncryptionSettingsContent(
    keyPackageCount: Int?,
    isPublishing: Boolean,
    errorMessage: String?,
    onBack: () -> Unit,
    onRefreshKeys: () -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        stringResource(R.string.settings_encryption_page_title),
                        modifier = Modifier.testTag(Ids.PAGE_HEADING),
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.common_back))
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .padding(16.dp)
                .fillMaxSize()
        ) {
            Card(modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.padding(16.dp)) {
                    // The warning reads the SAME count the row below states and the
                    // ONE shared threshold (`keypackagesLow`, never a per-app
                    // literal). Painted only once the count has been read AND is
                    // low — an unread page has no basis to claim either way.
                    if (keyPackageCount != null && keypackagesLow(keyPackageCount.toULong())) {
                        Column(modifier = Modifier.testTag(Ids.ENCRYPTION_KEYPACKAGE_LOW_WARNING)) {
                            Text(
                                stringResource(R.string.settings_encryption_page_low_key_warning_title),
                                style = MaterialTheme.typography.titleSmall,
                                color = MaterialTheme.colorScheme.error,
                            )
                            Text(
                                stringResource(R.string.settings_encryption_page_low_key_warning_body)
                                    .replace("{count}", keyPackageCount.toString()),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.error,
                            )
                        }
                        Spacer(Modifier.height(12.dp))
                    }
                    Text(stringResource(R.string.settings_encryption_page_mls_key_packages), style = MaterialTheme.typography.titleMedium)
                    Spacer(Modifier.height(12.dp))

                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween
                    ) {
                        Text(stringResource(R.string.common_available), style = MaterialTheme.typography.bodyMedium)
                        Text(
                            keyPackageCount?.toString() ?: "\u2014",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.testTag(Ids.ENCRYPTION_KEYPACKAGE_COUNT),
                        )
                    }

                    Spacer(Modifier.height(12.dp))

                    // Replenishing the one-time pool publishes the fresh key
                    // packages to the nest (`fauna.conversations.keypackage.
                    // upload`), so there is nothing this button can do with no
                    // nest — tui's `RefreshKeyPackages` is the same gesture. The
                    // page's own in-flight predicate is handed over rather than
                    // re-tested, so a publish already running still wins.
                    val refreshGate = faunaGate(
                        "fauna.conversations.keypackage.upload",
                        enabled = !isPublishing,
                    )
                    Button(
                        onClick = onRefreshKeys,
                        enabled = refreshGate.enabled,
                        modifier = Modifier.testTag(Ids.ENCRYPTION_KEYPACKAGE_REFRESH_BUTTON),
                    ) {
                        Text(stringResource(R.string.settings_encryption_page_refresh_keys))
                    }
                    DisabledControlReasonText(refreshGate.reason)

                    errorMessage?.let {
                        Spacer(Modifier.height(8.dp))
                        Text(
                            it,
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodySmall
                        )
                    }
                }
            }
        }
    }
}
