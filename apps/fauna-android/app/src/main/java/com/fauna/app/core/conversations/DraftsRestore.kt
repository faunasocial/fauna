package com.fauna.app.core.conversations

import com.fauna.app.core.ShellLog
import com.fauna.ffi.FfiDraftsSyncInterface
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import uniffi.fauna_conversations.ConversationsManager

/**
 * The two manager calls a launch restore makes — a seam so the capture order in
 * [launchDraftsRestore] is testable without the native manager.
 */
internal interface DraftsRestoreTarget {
    fun identityEpoch(): ULong
    suspend fun restoreDraftsAt(epoch: ULong, bytes: ByteArray)
}

/** The production [DraftsRestoreTarget]: a pass-through to one shared manager. */
internal class ManagerDraftsRestoreTarget(
    private val manager: ConversationsManager,
) : DraftsRestoreTarget {
    override fun identityEpoch(): ULong = manager.identityEpoch()
    override suspend fun restoreDraftsAt(epoch: ULong, bytes: ByteArray) =
        manager.restoreDraftsAt(epoch, bytes)
}

/**
 * Start the launch restore: the target and its identity epoch are read HERE,
 * before the load is launched — never after it returns. [currentTarget] is a
 * getter over the CURRENT session, so a reply the outgoing account's fetch
 * delivers after a switch would otherwise fill the incoming account's manager;
 * the epoch-checked `restoreDraftsAt` also refuses it on a manager cleared in
 * between (`account-scoping.md` § The scoping taxonomy — the writers of
 * account-scoped state retire with the drop). A failure is logged and left
 * non-fatal.
 */
internal fun launchDraftsRestore(
    scope: CoroutineScope,
    sync: FfiDraftsSyncInterface,
    currentTarget: () -> DraftsRestoreTarget,
): Job {
    val target = currentTarget()
    val epoch = target.identityEpoch()
    return scope.launch {
        try {
            sync.load()?.let { target.restoreDraftsAt(epoch, it) }
        } catch (e: Exception) {
            ShellLog.w("ConversationsDrafts", "restore drafts failed: ${e.message}")
        }
    }
}
