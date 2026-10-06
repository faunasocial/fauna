package com.fauna.app.core

import android.util.Log

/**
 * The sync device id for an identity, through the shared get-or-create
 * (`FfiAccountRegistry.deviceIdForActor`) — extracted from [SecureStorage] so
 * it is testable on the host against a real registry.
 *
 * The id is the account's persisted id when it has one, else
 * `derive_device_id(install_secret, actor_id)` (`sync-agent-credentials.md`
 * § Credential model, the 2026-09-20 ruling): the same account signing back in
 * after a sign-out comes back to its own named `sync_devices` row, and two
 * accounts on this phone never share an id. Every rule — which slot wins, what
 * is persisted where, the serialized mint — is shared Rust's; this object only
 * turns the identity's secret into the actor id the export is keyed on.
 */
internal object DeviceIdSlot {

    private const val TAG = "FaunaDeviceIdSlot"

    /**
     * The device id the identity [secretHex] registers under, or null.
     *
     * Null when there is no identity, or when no STABLE id exists (the install
     * secret did not persist) — deliberately never an unstable one, because
     * every consumer treats this as an identity, and one that changes per read
     * is worse than an absent one (absent leaves them on the null branch they
     * already handle).
     *
     * @param deviceIdForActor the shared export, bound to this app's registry
     *   and install store.
     */
    fun resolve(
        secretHex: String?,
        cryptoOps: CryptoOps,
        deviceIdForActor: (actorIdHex: String) -> String,
    ): String? {
        if (secretHex.isNullOrEmpty()) return null
        return try {
            val actorIdHex = HexUtil.bytesToHex(cryptoOps.actorIdFromSecret(HexUtil.hexToBytes(secretHex)))
            deviceIdForActor(actorIdHex)
        } catch (e: Exception) {
            Log.e(TAG, "no stable device id; consumers stay disabled", e)
            null
        }
    }
}
