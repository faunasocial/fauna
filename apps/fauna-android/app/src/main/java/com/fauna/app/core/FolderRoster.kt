package com.fauna.app.core

import java.lang.ref.WeakReference
import uniffi.fauna_devices_machine.DevicesMachine

/**
 * The session's folder roster, weakly — the Settings pending-actions row's way
 * to the name of the folder an automatic prune targets (`docs/goal/ui/settings.md`
 * § Pending actions). [com.fauna.app.ui.viewmodel.DevicesVM] publishes the
 * machine it builds; [folderName] asks that machine's shared
 * `DevicesMachine::folder_name` (label custody applied), so this holds no
 * resolver of its own and makes no fetch. The twin of linux's
 * `views::devices_folders::current_folder_name` and web's `sessionFolderName`.
 *
 * Weak so a page torn down on sign-out or an account switch takes its machine
 * with it. `null` before a Devices or Folders page has built one, or for a
 * folder the roster cannot name: the row then paints the neutral sentence.
 */
object FolderRoster {
    @Volatile
    private var machine: WeakReference<DevicesMachine>? = null

    /** Record the machine the Devices/Folders page just built. */
    fun publish(built: DevicesMachine) {
        machine = WeakReference(built)
    }

    /** The name of the folder whose id is [id], or `null`. */
    fun folderName(id: String): String? = machine?.get()?.folderName(id)
}
