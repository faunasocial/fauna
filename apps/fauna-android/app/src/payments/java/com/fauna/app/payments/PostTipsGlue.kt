package com.fauna.app.payments

import com.fauna.ffi.FfiFeedManager

// The post-tip resolver — **the built half**, compiled with `src/payments/`
// into `debug`, `release` and `foss`. Its own file rather than a member of
// `PaymentsGlue.kt` because its receiver is the FEED manager: the excised twin
// lives in `src/storeSafe/` (the one build type that has a feed but no
// payments), never in `src/noPayments/`, which the `kids` build type also
// takes and whose bindings carry no `FfiFeedManager` at all.

/**
 * Resolve this post's tip surface (`monetization.md` § Tips) — the ONE gated
 * member of `FfiFeedManager` (`fauna-feed/payments` gates
 * `FeedManager::resolve_post_tips`), so it needs the same variant seam as the
 * `fauna.payments.*` calls in `PaymentsGlue.kt` even though it lives on a different client.
 *
 * `PostSummary.tips` itself is ungated and inert: in the excised build the twin
 * of this function never populates it, so it stays `null` forever and the tip
 * render has nothing to paint — which is exactly why the render *also* carries
 * the compile condition (dead is not absent; § What "completely compiled away"
 * means, criterion 1).
 */
suspend fun FfiFeedManager.resolvePostTipsIfBuilt(postId: String) = resolvePostTips(postId)
