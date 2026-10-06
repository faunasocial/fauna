package com.fauna.app.ui.screen.backups

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Download
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.ValueFormat
import com.fauna.app.ui.util.shareFileBytes
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.app.ui.viewmodel.BackupsVM
import kotlinx.coroutines.launch
import social.fauna.generated.Ids

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SnapshotDetailScreen(
    navController: NavController,
    snapshotId: Int,
    vm: BackupsVM = hiltViewModel()
) {
    // The file list is the MACHINE's custody-wired sealed-plane read
    // (`ui/backups.md` § User actions), not a second `SnapshotsClient` this
    // screen wires custody for. Its own `BackupsVM` builds a machine over the
    // already-connected `FfiNestClient`, so arriving here by deep link or after
    // process death works exactly as arriving by tap does.
    val snap by vm.snapshot.collectAsState()
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(snapshotId) {
        vm.openSnapshot(snapshotId.toLong())
    }

    val errorText = resolveLocalized(context, snap?.error)
    LaunchedEffect(errorText) {
        appMessages.showError(errorText)
    }

    // The download's own failure surface — app glue, so it is not the machine's
    // error and a machine tick cannot clear it.
    val downloadError by vm.downloadError.collectAsState()
    LaunchedEffect(downloadError) {
        appMessages.showError(downloadError)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResourceFmt(R.string.backups_snapshot, snapshotId)) },
                navigationIcon = {
                    IconButton(onClick = { navController.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.common_back))
                    }
                }
            )
        }
    ) { padding ->
        Box(modifier = Modifier.padding(padding).fillMaxSize()) {
            val detail = snap?.detail
            if (detail == null) {
                CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
            } else {
                val files = detail.files
                LazyColumn(modifier = Modifier.fillMaxSize().testTag(Ids.SNAPSHOT_DETAIL_FILES)) {
                    items(files) { file ->
                        ListItem(
                            headlineContent = {
                                Text(
                                    file.path,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis
                                )
                            },
                            supportingContent = {
                                // path · size — matches the unified Linux file-row
                                // shape (FfiSnapshotFileEntry carries no mtime).
                                Text(ValueFormat.byteSize(context, file.sizeBytes))
                            },
                            trailingContent = {
                                // Single-file restore (backup-restore.md § 3): fetch
                                // this file's bytes via the shared client-side walk,
                                // then hand them to the platform save/share sheet —
                                // the same FileProvider + ACTION_SEND idiom as the
                                // Data Export section (AccountSettingsScreen.kt).
                                //
                                // A REGULAR-FILE gesture only: a directory row gets no
                                // dead affordance (tui's ruling, inherited).
                                if (file.fileType == "regular") {
                                    IconButton(
                                        onClick = {
                                            scope.launch {
                                                val bytes = vm.downloadSnapshotFile(snapshotId, file.path)
                                                if (bytes != null) {
                                                    try {
                                                        context.shareFileBytes(file.path, bytes)
                                                    } catch (e: Exception) {
                                                        vm.reportDownloadError(e.message)
                                                    }
                                                }
                                            }
                                        },
                                        modifier = Modifier.testTag(Ids.SNAPSHOT_FILE_DOWNLOAD_BUTTON)
                                    ) {
                                        Icon(
                                            Icons.Default.Download,
                                            contentDescription = stringResource(R.string.backups_download)
                                        )
                                    }
                                }
                            }
                        )
                        HorizontalDivider()
                    }
                    if (files.isEmpty()) {
                        item {
                            ListItem(
                                headlineContent = { Text(stringResource(R.string.backups_no_files_in_snapshot)) }
                            )
                        }
                    }
                }
            }
        }
    }
}
