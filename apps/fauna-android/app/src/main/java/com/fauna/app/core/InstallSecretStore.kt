package com.fauna.app.core

import com.fauna.ffi.FfiSecretStore

/**
 * The store holding the **install device secret** the sync device id derives
 * from (`sync-agent-credentials.md` § Credential model, the 2026-09-20 ruling:
 * the id is `derive_device_id(install_secret, actor_id)`).
 *
 * A second encrypted file ([INSTALL_PREFS_FILE]), not a key in
 * `fauna_secure_prefs`, because sign-out resets that file wholesale
 * (`SecureStorage.clear()`, after the registry's `clearAll`) — a secret kept
 * there would die with every sign-out, and the next sign-in would register a
 * new named `sync_devices` row, the exact failure the derivation exists to end.
 * The secret names no account, so surviving sign-out leaves nothing
 * account-naming on disk; only uninstalling (or clearing the app's data) ends
 * it, which is the fresh-machine case.
 *
 * A distinct type rather than a second bare [FfiSecretStore], so Hilt can never
 * hand the credential store where this one is meant, or the reverse.
 */
class InstallSecretStore(val store: FfiSecretStore)
