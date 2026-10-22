/** Shared helpers for feed post rendering. Used by PostCard and the feed page.
 *  Relative-time / byte-size formatting now lives in `#lib/value-format`
 *  (shared `fauna_core::format`); see `docs/goal/behavior/value-formatting.md`.
 *  Source classification + canonical labels live in shared Rust
 *  (`fauna_feed::classify_sources` over `#lib/wasm` `classifySources`); the
 *  source-icon emoji map lives in `#lib/source-glyph` (`sourceGlyphEmoji`),
 *  keyed off the shared `SourceGlyph` concept and shared with the conversations
 *  rail. See `docs/goal/ui/feed.md` § Where logic lives and
 *  `docs/goal/architecture/render-model.md` § Deltas → D5. */

export function shortActor(hex: string): string {
  if (!hex) return '???';
  // Canonical short id: 12-char prefix + single `…` (U+2026), matching shared
  // `fauna_core::format::short_id` / wasm `shortId` (value-formatting.md § Short id).
  // Kept as a JS one-liner — `shortActor` is on the hot feed render path, so routing
  // it through async-init wasm would couple render to `ensureWasm()`; the only web
  // drift was the trailing glyph (`...` → `…`).
  return hex.slice(0, 12) + '…';
}

export function liveStatusClass(status: string): string {
  switch (status) {
    case 'live': return 'status-live';
    case 'ended': return 'status-ended';
    default: return 'status-planned';
  }
}

// Structured-post field projection (article / community / classified /
// live-activity) moved to shared Rust — `fauna_core::structured` over the
// `structuredView` wasm export (`#lib/wasm`), the `StructuredView` type. The
// per-schema field keys now live once in Rust, shared with the nostr bridge
// writer, instead of being re-derived here. See `docs/goal/ui/feed.md`
// § Where logic lives.

/** The by-id decode (`decodePostFetchedAs`) web may paint, or `null`.
 *  The card and detail body come from web's own `posts.get` + decode while the
 *  badge is the feed manager's `post.verification`, from a separate fetch — so a
 *  body is painted only when the shared decode found it validly signed AND
 *  bound to the id asked for (`valid`). Anything else falls back to the
 *  manager's snapshot body, never a substituted post under the manager's
 *  `Verified` badge (`docs/goal/architecture/security.md` § App display of
 *  unverified content). `valid` must be literally `true`: a missing or
 *  non-boolean field is a refusal, not a pass. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function paintableDecodedPost(decoded: any): any | null {
  return decoded != null && decoded.valid === true ? decoded : null;
}
