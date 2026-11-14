package com.fauna.app.core

import com.fauna.app.provider.FfiOnDemandSource
import com.fauna.ffi.FfiEngineRoster
import com.fauna.ffi.FfiException
import kotlinx.coroutines.runBlocking
import javax.inject.Inject
import javax.inject.Singleton

/**
 * This device's engine roster — every set an engine on the phone is bound to —
 * lent to the launch folder pass so the projection walk's take-over claims an
 * adoption marker only for a set no engine here holds (`succession-cut.md`
 * ruling (11)(l), *one consumer per marker*; the Rust side is
 * `fauna-ffi`'s `engine_roster`). Android binds an engine to a set three ways:
 *
 * - the documents provider's on-demand sets ([FfiOnDemandSource.boundScopedIds],
 *   actor-scoped ids);
 * - the watched directories, a disabled one included (bare `FolderRef`s);
 * - the photo backup's set (`photoLibraryBoundRef`, the device-local binding).
 *
 * A host that cannot be read fails the whole reading — the pass then claims
 * nothing new, since an unread binding may be an engine's. Called by Rust off
 * the UI thread, so blocking here is fine.
 */
@Singleton
class EngineRoster @Inject constructor(
    private val onDemandSource: FfiOnDemandSource,
    private val accountStores: AccountStores,
) : FfiEngineRoster {

    override fun boundFolderRefs(): List<String> = runBlocking {
        try {
            val onDemand = onDemandSource.boundScopedIds()
            val watched = accountStores.database().watchedDirectoryDao().getAllOnce().map { it.folderId }
            val photo = listOfNotNull(com.fauna.ffi.photoLibraryBoundRef(accountStores.syncStateDir()))
            onDemand + watched + photo
        } catch (e: FfiException) {
            throw e
        } catch (e: Exception) {
            throw FfiException.General("the engine roster is unreadable: ${e.message}")
        }
    }

    /** Lend this roster to the launch pass; called once at app start. */
    fun install() {
        com.fauna.ffi.installEngineRoster(this)
    }
}
