package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.core.FolderChoice
import com.fauna.app.core.WatchedDirectoryManager
import com.fauna.app.data.db.WatchedDirectory
import com.fauna.app.data.db.WatchedDirectoryDao
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

/** The add dialog's target list: loading, the user's own folders, or why it failed. */
sealed interface FolderChoices {
    data object Loading : FolderChoices
    data class Loaded(val folders: List<FolderChoice>) : FolderChoices
    data class Failed(val message: String) : FolderChoices
}

@HiltViewModel
class WatchedDirectoryVM @Inject constructor(
    private val watchedDirectoryDao: WatchedDirectoryDao,
    private val watchedDirectoryManager: WatchedDirectoryManager,
    private val api: ApiClient
) : ViewModel() {
    val directories: Flow<List<WatchedDirectory>> = watchedDirectoryDao.getEnabled()
    val isScanning: StateFlow<Boolean> = watchedDirectoryManager.isScanning
    val errorMessage: StateFlow<String?> = watchedDirectoryManager.errorMessage

    private val _folderChoices = MutableStateFlow<FolderChoices>(FolderChoices.Loading)
    val folderChoices: StateFlow<FolderChoices> = _folderChoices.asStateFlow()

    /** Refresh the picker's rows — called when the add dialog opens. */
    fun loadFolderChoices() {
        _folderChoices.value = FolderChoices.Loading
        viewModelScope.launch {
            _folderChoices.value = runCatching { api.ownFolderChoices() }.fold(
                onSuccess = { FolderChoices.Loaded(it) },
                onFailure = { FolderChoices.Failed(it.message ?: it.toString()) }
            )
        }
    }

    /** Watch [treeUri], ingesting into the picked set — kept by its ref, labelled by its name. */
    fun addDirectory(treeUri: String, displayName: String, target: FolderChoice) {
        viewModelScope.launch {
            watchedDirectoryDao.upsert(
                WatchedDirectory(
                    treeUri = treeUri,
                    displayName = displayName,
                    folder = target.name,
                    enabled = true,
                    folderId = target.folderId
                )
            )
        }
    }

    fun removeDirectory(dir: WatchedDirectory) {
        viewModelScope.launch { watchedDirectoryDao.delete(dir) }
    }

    fun toggleEnabled(dir: WatchedDirectory) {
        viewModelScope.launch {
            watchedDirectoryDao.upsert(dir.copy(enabled = !dir.enabled))
        }
    }

    fun scanAll() {
        viewModelScope.launch {
            watchedDirectoryManager.scanAllEnabled()
        }
    }
}
