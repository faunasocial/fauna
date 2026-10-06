package com.fauna.app

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build
import androidx.hilt.work.HiltWorkerFactory
import androidx.work.Configuration
import com.fauna.app.core.ShellLog
import com.fauna.app.service.CustodianHostWorker
import com.fauna.app.service.CustodianPushKick
import com.fauna.app.service.PhotoBackupWorker
import com.fauna.app.widget.WidgetDataWorker
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.android.HiltAndroidApp
import dagger.hilt.components.SingletonComponent

@HiltAndroidApp
class FaunaApp : Application(), Configuration.Provider {

    /**
     * Pulls the app-scoped [CustodianPushKick] singleton out of the Hilt graph
     * in [onCreate] without field-injecting the Application — so a Robolectric
     * test booting the real [FaunaApp] without Hilt test infra never triggers a
     * boot-time injection failure (the access is inside a try/catch in user code,
     * not the generated injection phase).
     */
    @EntryPoint
    @InstallIn(SingletonComponent::class)
    interface PushKickEntryPoint {
        /** This device's backup-custodian host — the *pull* direction. */
        fun custodianPushKick(): CustodianPushKick

        /** The blessed-grant auto-renew pass at app foreground. */
        fun nestsAutoRenew(): com.fauna.app.service.NestsAutoRenew
    }

    /**
     * Same escape hatch as [PushKickEntryPoint], for the [HiltWorkerFactory] that
     * [workManagerConfiguration] installs.
     */
    @EntryPoint
    @InstallIn(SingletonComponent::class)
    interface WorkerFactoryEntryPoint {
        fun hiltWorkerFactory(): HiltWorkerFactory
    }

    // The androidx.startup InitializationProvider that would otherwise initialize
    // WorkManager eagerly with a DEFAULT configuration is removed in the manifest,
    // so this on-demand configuration is the one WorkManager actually uses.
    //
    // **The worker factory is load-bearing, not optional.** Every worker this app
    // enqueues (WidgetDataWorker, CustodianHostWorker, PhotoBackupWorker) is a
    // @HiltWorker with an @AssistedInject constructor taking injected dependencies
    // beyond (Context, WorkerParameters). WorkManager's DEFAULT factory can only
    // reflect a bare (Context, WorkerParameters) constructor, so under it every one
    // of them fails to construct and the work is marked failed — silently, since a
    // worker that never runs looks exactly like a worker with nothing to do. Only
    // HiltWorkerFactory can build them. Pinned by WorkerFactoryWiringTest.
    //
    // Wrapped like the FFI installs in onCreate: an environment with no Hilt graph
    // (Robolectric booting the real FaunaApp) still yields a usable configuration
    // rather than throwing during Application init.
    override val workManagerConfiguration: Configuration
        get() = Configuration.Builder()
            .apply {
                try {
                    setWorkerFactory(
                        EntryPointAccessors
                            .fromApplication(this@FaunaApp, WorkerFactoryEntryPoint::class.java)
                            .hiltWorkerFactory()
                    )
                } catch (e: Throwable) {
                    ShellLog.w("FaunaApp", "Hilt worker factory unavailable: ${e.message}")
                }
            }
            .build()

    override fun onCreate() {
        super.onCreate()

        // Install the process-global tracing subscriber (the in-memory fauna-log
        // ring + a daily-rolling file under filesDir/logs/) as the first thing
        // the app does, so every tracing event — including the pin-store install
        // below — lands in the ring the Settings → Logs page renders
        // (observability.md § Surfaces; the android twin of linux
        // `client::install_logging`). filesDir is the app-private data dir, the
        // analog of linux ~/.config/fauna. Idempotent. Wrapped like the pin-store
        // install so Robolectric (which can't load the native .so) still boots.
        try {
            com.fauna.ffi.installLogging(filesDir.absolutePath)
        } catch (e: Throwable) {
            android.util.Log.w("FaunaApp", "log subscriber install failed", e)
            ShellLog.w("FaunaApp", "log subscriber install failed: ${e.message}")
        }

        // Install the disk-backed nest-identity pin store before the first
        // authenticated connect, so TOFU pins (self-signed / LAN nests) survive
        // restarts (security.md § Transport trust). filesDir is the app-private
        // data dir; the canonical pin filename is appended inside Rust.
        try {
            com.fauna.ffi.installNestIdentityPinStore(filesDir.absolutePath)
        } catch (e: Throwable) {
            android.util.Log.w("FaunaApp", "nest-identity pin store install failed", e)
            ShellLog.w("FaunaApp", "nest-identity pin store install failed: ${e.message}")
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                "fauna_sync", "Sync",
                NotificationManager.IMPORTANCE_LOW
            ).apply { description = "File sync and photo backup progress" }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)

            val messagesChannel = NotificationChannel(
                "fauna_messages", "Messages",
                NotificationManager.IMPORTANCE_HIGH
            ).apply { description = "New message notifications" }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(messagesChannel)

            val groupsChannel = NotificationChannel(
                "fauna_groups", "Groups",
                NotificationManager.IMPORTANCE_HIGH
            ).apply { description = "Group message and invite notifications" }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(groupsChannel)

            val contactsChannel = NotificationChannel(
                "fauna_contacts", getString(R.string.notifications_type_knock),
                NotificationManager.IMPORTANCE_HIGH
            ).apply { description = getString(R.string.notifications_knock_title) }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(contactsChannel)
        }
        WidgetDataWorker.enqueuePeriodicWork(this)

        // Backup's *pull* direction — the only backup direction this app drives
        // (the source nest is the segment-backup writer; backup-restore.md
        // § Background Tasks): hosting this device's client-device backup
        // custodian replica (backups.md § Third destination kind). Enqueued
        // unconditionally — the pass no-ops until the source nest's registry names
        // this device, so there is no enrollment signal to wait for.
        CustodianHostWorker.enqueuePeriodicWork(this)

        // The photo library's "one-way, continuous ingress" (folders.md § Photo
        // backup). Enqueued unconditionally at a constant period (the de-knobbed
        // reconcile cadence, floored at WorkManager's 15-minute minimum); the
        // worker itself no-ops while `photo-backup-enable-toggle` is off.
        PhotoBackupWorker.enqueuePeriodicWork(this)

        // The custodian's foreground wake — the low-latency sibling of the periodic
        // [CustodianHostWorker], on the shared push-debounce loop. Wrapped like the
        // FFI installs above so Robolectric (no Hilt test graph / no native .so)
        // still boots.
        try {
            EntryPointAccessors.fromApplication(this, PushKickEntryPoint::class.java)
                .custodianPushKick()
                .register()
        } catch (e: Throwable) {
            ShellLog.w("FaunaApp", "custodian push-kick register failed: ${e.message}")
        }
        try {
            EntryPointAccessors.fromApplication(this, PushKickEntryPoint::class.java)
                .nestsAutoRenew()
                .register()
        } catch (e: Throwable) {
            ShellLog.w("FaunaApp", "nests auto-renew register failed: ${e.message}")
        }
    }
}
