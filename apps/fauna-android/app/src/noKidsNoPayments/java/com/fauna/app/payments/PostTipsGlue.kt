package com.fauna.app.payments

import com.fauna.ffi.FfiFeedManager

// The post-tip resolver — **the excised half**, compiled into the build types
// with a feed and no payments: `storeSafe` and `storeNoPayments`, the two that
// take `src/noKidsNoPayments/`. `kids` has neither, so it takes no twin of this
// at all — which is why this cannot live in `src/noPayments/`, a directory the
// `kids` build type also compiles, and whose bindings have no `FfiFeedManager`.
// Signature-identical to
// `src/payments/java/com/fauna/app/payments/PostTipsGlue.kt`.

/**
 * Excised — `FeedManager::resolve_post_tips` is compiled out of this flavor's
 * `fauna-ffi`, so `PostSummary.tips` stays permanently `null` (the ungated inert
 * record, `snapshot.rs`'s own posture) and the tip surface has nothing to paint.
 */
@Suppress("UNUSED_PARAMETER")
suspend fun FfiFeedManager.resolvePostTipsIfBuilt(postId: String) = Unit
