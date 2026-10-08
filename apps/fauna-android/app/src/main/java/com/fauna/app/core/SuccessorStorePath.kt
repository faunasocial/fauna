package com.fauna.app.core

import com.fauna.ffi.FfiSuccessorStorePath

/**
 * android's `db_path_for` — the per-account MLS store resolver the succession
 * ceremony invokes for the **successor** identity
 * (`docs/goal/behavior/identity-succession.md` § Implementation status today).
 * Twin of apple's `SuccessorStorePath` and windows' resolver of the same name.
 *
 * **A callback, never a `String`:** resolving here **writes** (it creates the
 * successor's scope dir), and the pinned ordering is that an unreachable nest
 * fails before anything is written — so the shared ceremony calls this only
 * after the successor's `connect()` succeeds (`libs/fauna-ffi/src/recovery.rs`
 * takes an `FfiSuccessorStorePath` to make the eager form unrepresentable).
 *
 * **A different file from the predecessor's:** both engines are live at once
 * during the post-succession sweep, and MLS holds one engine per store. The
 * layout is per-actor (`<filesDir>/<actor>/mls.db`), so the successor lands on
 * its own file by construction.
 */
class SuccessorStorePath(private val stores: AccountStores) : FfiSuccessorStorePath {
    override fun mlsDbPath(successorActorHex: String): String = stores.mlsDbPathFor(successorActorHex)
}

/**
 * The retired identity's own MLS store, for `succession_retry_group_sweep`'s
 * `old_store_path` (`docs/goal/ui/settings.md` § Recovery kit → *Finishing an
 * unfinished group sweep*) — the retry's OTHER resolver.
 *
 * ⚠ **The PURE scope resolution, never the creating one.** The retry turns on
 * whether the retired identity's store already exists on this device; a
 * resolver that created it would make an untouched retired identity read as a
 * clean empty run while the thief's leaf sits untouched in every real group.
 */
class RetiredIdentityStorePath(private val stores: AccountStores) : FfiSuccessorStorePath {
    override fun mlsDbPath(successorActorHex: String): String = stores.pureMlsDbPathFor(successorActorHex)
}
