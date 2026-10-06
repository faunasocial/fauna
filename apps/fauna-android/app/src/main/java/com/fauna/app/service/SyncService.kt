package com.fauna.app.service

import android.app.Notification
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.IBinder
import androidx.core.app.NotificationCompat
import com.fauna.app.R
import com.fauna.app.core.PhotoBackupEngine
import com.fauna.app.ui.util.getStringFmt
import dagger.hilt.android.AndroidEntryPoint
import kotlinx.coroutines.*
import javax.inject.Inject

@AndroidEntryPoint
class SyncService : Service() {

    @Inject lateinit var photoBackupEngine: PhotoBackupEngine

    private var syncJob: Job? = null
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIFICATION_ID, buildNotification(getString(R.string.photo_backup_notification_starting)))

        syncJob = scope.launch {
            // Observe progress and update notification
            launch {
                photoBackupEngine.uploadedCount.collect { uploaded ->
                    val total = photoBackupEngine.totalScanned.value
                    if (total > 0) {
                        updateNotification(getStringFmt(R.string.photo_backup_notification_progress, uploaded, total))
                    }
                }
            }

            photoBackupEngine.syncNewPhotos()
            updateNotification(getString(R.string.photo_backup_notification_complete))
            delay(2000)
            stopSelf()
        }

        return START_NOT_STICKY
    }

    override fun onDestroy() {
        syncJob?.cancel()
        scope.cancel()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private fun buildNotification(text: String): Notification =
        NotificationCompat.Builder(this, "fauna_sync")
            .setSmallIcon(android.R.drawable.ic_menu_upload)
            .setContentTitle("Fauna")
            .setContentText(text)
            .setOngoing(true)
            .setSilent(true)
            .build()

    private fun updateNotification(text: String) {
        val notification = buildNotification(text)
        getSystemService(NotificationManager::class.java)?.notify(NOTIFICATION_ID, notification)
    }

    companion object {
        private const val NOTIFICATION_ID = 1001
    }
}
