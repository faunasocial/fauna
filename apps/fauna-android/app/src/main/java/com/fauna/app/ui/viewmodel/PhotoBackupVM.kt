package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import com.fauna.app.core.PhotoBackupEngine
import com.fauna.app.core.SecureStorage
import com.fauna.app.data.db.PhotoBackupDao
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import javax.inject.Inject

@HiltViewModel
class PhotoBackupVM @Inject constructor(
    private val photoBackupEngine: PhotoBackupEngine,
    private val photoBackupDao: PhotoBackupDao,
    private val secureStorage: SecureStorage
) : ViewModel() {

    val syncedCount: Flow<Int> = photoBackupDao.getSyncedCount()

    val isSyncing: StateFlow<Boolean> = photoBackupEngine.isSyncing

    /** The in-flight pass's countdown and size (0 when no pass runs) — per pass, never a session total. */
    val pendingCount: StateFlow<Int> = photoBackupEngine.progress.pendingCount
    val passTotal: StateFlow<Int> = photoBackupEngine.progress.passTotal
    val lastError: StateFlow<String?> = photoBackupEngine.lastError

    val autoBackup = MutableStateFlow(secureStorage.autoPhotoBackup)

    fun setAutoBackup(enabled: Boolean) {
        autoBackup.value = enabled
        secureStorage.autoPhotoBackup = enabled
    }
}
