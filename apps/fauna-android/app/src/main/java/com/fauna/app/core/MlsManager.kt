package com.fauna.app.core

import com.fauna.ffi.actorIdFromSecret
import javax.inject.Inject
import javax.inject.Singleton

/**
 * Read-only key-package-count surface for Encryption settings.
 *
 * Phase 5 deleted the bespoke channel/welcome
 * orchestration that used to live here; the 2026-07-14 device-sync durability
 * fix then removed the local-engine key-package *mint* (`initEngine` +
 * `generateAndPublishKeyPackages`, which minted on a throwaway
 * `MlsEngine::new_in_memory` whose fresh private init keys existed nowhere
 * durable — a provider swap wiped them and stranded peers, `docs/goal/behavior/
 * devices.md` § Cross-device MLS group-state sync). Replenish now flows through
 * the durable `ConversationsManagerHost.manager.ensureKeypackages` surface
 * (mints on the session MLS engine + notifies the replica autosave), exactly like
 * login and the web/linux apps.
 *
 * What remains is the count read: `EncryptionSettingsVM` shows
 * [getKeyPackageCount] via the `fauna.conversations.keypackage.count` WS-RPC kind
 * (`ApiClient.conversationsRpc()` → `FfiConversationsClient`). No MLS state is
 * held client-side here (`docs/goal/ui/conversations.md` rule #2).
 */
@Singleton
class MlsManager @Inject constructor(
    private val api: ApiClient,
    private val secureStorage: SecureStorage
) {
    private val actorIdHex: String
        get() = HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes(secureStorage.secretHex!!)))

    suspend fun getKeyPackageCount(): Int {
        return api.conversationsRpc().keypackageCount(actorIdHex).count.toInt()
    }
}
