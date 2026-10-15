package com.fauna.app.core

import uniffi.fauna_feed.PostSummary

/**
 * A feed post (card or detail) as the region plane's scorer input
 * ([RegionSubject]). Its report key is the post's cid + author.
 *
 * Lives under `src/noKids`, beside the feed surfaces that call it: a feed post is
 * a `fauna_feed` type the kids `fauna-ffi` flavor does not export
 * (`dynamic-features.md` § Compile-time excision), while [RegionSubject] and the
 * plane itself stay in `src/main` — a kids build still composes the region into
 * its conversation bubbles ([RegionSubject.message]).
 */
fun RegionSubject.Companion.post(p: PostSummary) =
    RegionSubject(p.postId, p.author, p.body, p.tags, p.hasMedia, p.postId, p.author)
