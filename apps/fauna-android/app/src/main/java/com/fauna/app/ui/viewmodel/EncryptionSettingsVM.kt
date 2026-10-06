package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.R
import com.fauna.app.core.MlsManager
import com.fauna.app.core.conversations.ConversationsManagerHost
import com.fauna.app.ui.util.getStringFmt
import dagger.hilt.android.lifecycle.HiltViewModel
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject

@HiltViewModel
class EncryptionSettingsVM @Inject constructor(
    @ApplicationContext private val context: Context,
    private val mlsManager: MlsManager,
    private val conversationsManagerHost: ConversationsManagerHost,
) : ViewModel() {
    val keyPackageCount = MutableStateFlow<Int?>(null)
    val isPublishing = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    fun loadKeyCount() {
        viewModelScope.launch {
            errorMessage.value = null
            try {
                keyPackageCount.value = mlsManager.getKeyPackageCount()
            } catch (e: Exception) {
                errorMessage.value = context.getStringFmt(R.string.settings_encryption_page_error_key_count, e.message)
            }
        }
    }

    fun refreshKeys() {
        viewModelScope.launch {
            isPublishing.value = true
            errorMessage.value = null
            try {
                // Replenish the one-time pool through the durable session manager
                // (mints on the session MLS engine + notifies the replica autosave),
                // the SAME surface login + web/linux drive — NOT the throwaway-engine
                // `mlsGenerateKeyPackages` FFI, whose fresh private init keys exist
                // nowhere durable and a provider swap wipes, stranding peers
                // (docs/goal/behavior/devices.md § Cross-device MLS group-state sync).
                conversationsManagerHost.manager.ensureKeypackages(KEYPACKAGE_TARGET)
                keyPackageCount.value = mlsManager.getKeyPackageCount()
            } catch (e: Exception) {
                errorMessage.value = context.getStringFmt(R.string.settings_encryption_page_error_refresh_keys, e.message)
            } finally {
                isPublishing.value = false
            }
        }
    }

    private companion object {
        // One-time key packages to keep published on the nest (mirrors the shared
        // Rust `fauna_conversations::KEYPACKAGE_TARGET` = 20; web/linux mirror it too).
        const val KEYPACKAGE_TARGET: ULong = 20uL
    }
}
