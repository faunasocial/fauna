/** Shared helpers for feed post rendering. Used by PostCard and the feed page.
 *  Relative-time / byte-size formatting now lives in `$lib/value-format`
 *  (shared `fauna_core::format`); see `docs/goal/behavior/value-formatting.md`.
 *  Source classification + canonical labels live in shared Rust
 *  (`fauna_feed::classify_sources` over `$lib/wasm` `classifySources`); the
 *  source-icon emoji map lives in `$lib/source-glyph` (`sourceGlyphEmoji`),
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
// `structuredView` wasm export (`$lib/wasm`), the `StructuredView` type. The
// per-schema field keys now live once in Rust, shared with the nostr bridge
// writer, instead of being re-derived here. See `docs/goal/ui/feed.md`
// § Where logic lives.
