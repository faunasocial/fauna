package com.fauna.app.ui.screen.media

import android.content.Context
import com.fauna.app.data.db.SyncFileState
import com.fauna.app.ui.util.resolveLocalized
import com.fauna.ffi.syncDisplayStateLabel
import uniffi.fauna_core.SyncDisplayState

/**
 * Display-only 1:1 map from the Room [SyncFileState] to the shared
 * [SyncDisplayState] (file-sync.md § Per-file sync-status display) — mirrors
 * apple's `SyncFileState.displayState`. The badge **color** stays an idiomatic
 * per-app render; only the **text label** is shared (resolved below).
 */
internal val SyncFileState.displayState: SyncDisplayState
    get() = when (this) {
        SyncFileState.SYNCED -> SyncDisplayState.SYNCED
        SyncFileState.LOCAL_ONLY -> SyncDisplayState.LOCAL_ONLY
        SyncFileState.REMOTE_ONLY -> SyncDisplayState.REMOTE_ONLY
        SyncFileState.UPLOADING -> SyncDisplayState.UPLOADING
        SyncFileState.DOWNLOADING -> SyncDisplayState.DOWNLOADING
        SyncFileState.CONFLICT -> SyncDisplayState.CONFLICT
    }

/**
 * The user-facing sync-status label (`media.status_label.*`) for this file state,
 * resolved through the shared `sync_display_state_label` — used as the per-file
 * detail text and the list badge's accessible name.
 */
internal fun SyncFileState.syncStatusLabel(context: Context): String =
    resolveLocalized(context, syncDisplayStateLabel(displayState)).orEmpty()
