package com.fauna.app.ui.screen.media

import android.content.Intent
import android.net.Uri
import android.provider.DocumentsContract
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.data.db.WatchedDirectory
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.ui.viewmodel.FolderChoices
import com.fauna.app.ui.viewmodel.WatchedDirectoryVM

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun WatchedDirectoryScreen(
    navController: NavController,
    vm: WatchedDirectoryVM = hiltViewModel()
) {
    val context = LocalContext.current
    val directories by vm.directories.collectAsState(initial = emptyList())
    val isScanning by vm.isScanning.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()

    val appMessages = LocalAppMessages.current

    // State for the "add directory" dialog. The target is picked from the
    // user's own folders and kept by its ref (`on-demand-files.md` § Hosting
    // multiple on-demand folders — the ref is the binding's only key).
    var pendingUri by remember { mutableStateOf<Uri?>(null) }
    var pendingDisplayName by remember { mutableStateOf("") }
    var pickedFolderId by remember { mutableStateOf<String?>(null) }
    val folderChoices by vm.folderChoices.collectAsState()

    LaunchedEffect(errorMessage) {
        appMessages.showError(errorMessage)
    }

    val directoryPicker = rememberLauncherForActivityResult(
        ActivityResultContracts.OpenDocumentTree()
    ) { uri: Uri? ->
        uri ?: return@rememberLauncherForActivityResult
        // Take persistable permission
        context.contentResolver.takePersistableUriPermission(
            uri,
            Intent.FLAG_GRANT_READ_URI_PERMISSION
        )
        // Extract display name from tree document ID or last path segment
        val treeDocId = try {
            DocumentsContract.getTreeDocumentId(uri)
        } catch (_: Exception) {
            null
        }
        val name = treeDocId?.substringAfterLast('/') ?: uri.lastPathSegment ?: "directory"
        pendingDisplayName = name
        pickedFolderId = null
        vm.loadFolderChoices()
        pendingUri = uri
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.media_watched_directories)) },
                navigationIcon = {
                    IconButton(onClick = { navController.popBackStack() }) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = stringResource(R.string.common_back))
                    }
                }
            )
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = { directoryPicker.launch(null) }
            ) {
                Icon(Icons.Default.Add, contentDescription = stringResource(R.string.media_watched_add_directory))
            }
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
        ) {
            // Scan Now button
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                Button(
                    onClick = { vm.scanAll() },
                    enabled = !isScanning
                ) {
                    Text(stringResource(R.string.media_watched_scan_now))
                }
                if (isScanning) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(16.dp),
                        strokeWidth = 2.dp
                    )
                    Text(stringResource(R.string.media_watched_scanning), style = MaterialTheme.typography.bodySmall)
                }
            }

            if (directories.isEmpty()) {
                // Empty state
                Box(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(32.dp),
                    contentAlignment = Alignment.Center
                ) {
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Icon(
                            Icons.Default.Info,
                            contentDescription = null,
                            modifier = Modifier.size(64.dp),
                            tint = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                        Spacer(Modifier.height(16.dp))
                        Text(
                            stringResource(R.string.media_watched_no_watched),
                            style = MaterialTheme.typography.bodyLarge,
                            color = MaterialTheme.colorScheme.onSurfaceVariant
                        )
                    }
                }
            } else {
                LazyColumn(
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    items(directories, key = { it.treeUri }) { dir ->
                        WatchedDirectoryRow(
                            dir = dir,
                            onToggle = { vm.toggleEnabled(dir) },
                            onDelete = { vm.removeDirectory(dir) }
                        )
                    }
                }
            }
        }
    }

    // Target folder dialog
    if (pendingUri != null) {
        val choices = (folderChoices as? FolderChoices.Loaded)?.folders.orEmpty()
        val picked = choices.firstOrNull { it.folderId == pickedFolderId }
        AlertDialog(
            onDismissRequest = { pendingUri = null },
            title = { Text(stringResource(R.string.media_watched_add_directory)) },
            text = {
                Column {
                    Text(
                        "${stringResource(R.string.media_watched_directory_label)}: $pendingDisplayName",
                        style = MaterialTheme.typography.bodyMedium
                    )
                    Spacer(Modifier.height(12.dp))
                    Text(
                        stringResource(R.string.media_watched_target_folder),
                        style = MaterialTheme.typography.labelLarge
                    )
                    when (val state = folderChoices) {
                        FolderChoices.Loading -> CircularProgressIndicator(
                            modifier = Modifier.padding(8.dp).size(24.dp),
                            strokeWidth = 2.dp
                        )
                        is FolderChoices.Failed -> Text(
                            stringResourceFmt(R.string.media_watched_error_folders, state.message),
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodyMedium
                        )
                        is FolderChoices.Loaded -> if (state.folders.isEmpty()) {
                            Text(
                                stringResource(R.string.media_watched_no_folders),
                                style = MaterialTheme.typography.bodyMedium
                            )
                        } else {
                            Column(
                                modifier = Modifier
                                    .heightIn(max = 280.dp)
                                    .verticalScroll(rememberScrollState())
                                    .selectableGroup()
                            ) {
                                state.folders.forEach { choice ->
                                    Row(
                                        modifier = Modifier
                                            .fillMaxWidth()
                                            .selectable(
                                                selected = choice.folderId == pickedFolderId,
                                                onClick = { pickedFolderId = choice.folderId },
                                                role = Role.RadioButton
                                            )
                                            .padding(vertical = 4.dp),
                                        verticalAlignment = Alignment.CenterVertically
                                    ) {
                                        RadioButton(
                                            selected = choice.folderId == pickedFolderId,
                                            onClick = null
                                        )
                                        Spacer(Modifier.width(8.dp))
                                        Text(choice.name, style = MaterialTheme.typography.bodyLarge)
                                    }
                                }
                            }
                        }
                    }
                }
            },
            confirmButton = {
                TextButton(
                    enabled = picked != null,
                    onClick = {
                        val uri = pendingUri ?: return@TextButton
                        val target = picked ?: return@TextButton
                        vm.addDirectory(
                            treeUri = uri.toString(),
                            displayName = pendingDisplayName,
                            target = target
                        )
                        pendingUri = null
                    }
                ) {
                    Text(stringResource(R.string.common_add))
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingUri = null }) {
                    Text(stringResource(R.string.common_cancel))
                }
            }
        )
    }
}

@Composable
private fun WatchedDirectoryRow(
    dir: WatchedDirectory,
    onToggle: () -> Unit,
    onDelete: () -> Unit
) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .padding(12.dp)
                .fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp)
        ) {
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    dir.displayName,
                    style = MaterialTheme.typography.bodyLarge
                )
                Spacer(Modifier.height(4.dp))
                AssistChip(
                    onClick = {},
                    label = { Text(dir.folder) }
                )
            }
            Switch(
                checked = dir.enabled,
                onCheckedChange = { onToggle() }
            )
            IconButton(onClick = onDelete) {
                Icon(
                    Icons.Default.Delete,
                    contentDescription = stringResource(R.string.media_watched_remove_directory),
                    tint = MaterialTheme.colorScheme.error
                )
            }
        }
    }
}
