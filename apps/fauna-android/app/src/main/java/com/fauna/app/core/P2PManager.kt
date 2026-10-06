package com.fauna.app.core

import com.fauna.ffi.FfiPeerContact
import com.fauna.ffi.FfiPeerDb
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Read-only access to this account's P2P contact DB. All methods are
 * synchronous (FFI calls) — call from a coroutine dispatcher.
 *
 * The WG-era contacts/QR-invite UI that used to sit on top of this manager was
 * removed 2026-07-13 (unsanctioned drift — ui.yaml's p2p notes say Android has
 * "no page surface" and p2p.md § Implementation status today ratifies the
 * invite path as parked WireGuard-era drift).
 *
 * The signal-receive scaffolding that used to live here went with the
 * WireGuard stack 2026-08-23: signaling was WG-only (nest-relayed
 * `fauna.peer.signal`, carrying a peer's WG public key), and iroh's node model
 * has no per-peer add for a signal to drive. When P2P dialing lands it is
 * `PeerNode::dial` over the seam, not a resurrected signal handler.
 */
@Singleton
class P2PManager @Inject constructor(
    private val accountStores: AccountStores,
) {
    @Volatile
    private var peerDb: FfiPeerDb? = null

    /**
     * Lazily open the **active account's** P2P contact database
     * (`account-scoping.md` § Serialized switching — class 1: peer state belongs
     * to one identity). Thread-safe via synchronization.
     *
     * The handle is registered with [AccountStores] so a switch or sign-out closes
     * it along with every other account-scoped store: it points at the opening
     * account's file for as long as it lives, so it must not outlive that
     * account's session.
     */
    @Synchronized
    fun db(context: android.content.Context): FfiPeerDb {
        return peerDb ?: run {
            val db = FfiPeerDb.open(accountStores.p2pContactsDbPath())
            peerDb = db
            accountStores.registerCloser("p2p") { close() }
            db
        }
    }

    /**
     * Drop the open handle; the next [db] call reopens under whoever is active.
     *
     * Deliberately **not** `@Synchronized`: it runs from
     * [AccountStores.closeOpenStores], which holds that object's monitor, while
     * [db] takes this object's monitor and then that one — locking both ways round
     * would deadlock. A close racing an in-flight open can be lost, which is
     * harmless: [db] resolves its path from the account active *at open time*, so
     * the surviving handle is the current account's either way.
     */
    fun close() {
        peerDb = null
    }

    /** List all P2P-enabled contacts. */
    fun listContacts(context: android.content.Context): List<FfiPeerContact> =
        db(context).listP2pEnabled()
}
