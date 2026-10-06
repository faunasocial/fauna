/** Web's `SourceGlyph → emoji` call site. Shared Rust owns the *concept*
 *  (`fauna_core::source_glyph::SourceGlyph`, via `Rail::glyph()` /
 *  `SourceKind::glyph()`) AND — since every app renders that concept as the
 *  same emoji — the **map** too (`SourceGlyph::emoji`, reached over wasm as
 *  `sourceGlyphEmoji`). This module keeps only the call.
 *
 *  The SAME map serves the conversations rail AND the feed badge AND the
 *  bridge-subscription row, so the three surfaces can't drift — the
 *  within-client unification of `render-model.md` § Deltas → D5; the shared
 *  map is the cross-client half of the same rule (the seven per-app copies had
 *  already begun to disagree on the envelope's presentation selector).
 *
 *  Keyed off the glyph's stable lowercase id (`fox | envelope | butterfly |
 *  bolt | globe | unknown | archive | bridge`), which is both the serde form on the conversations
 *  snapshot (`thread.glyph`) and the feed badge (`badge.glyph` off
 *  `classifySources`). Canonical concepts ratified by the user 2026-06-22. */
import { sourceGlyphEmoji as wasmSourceGlyphEmoji } from '$lib/wasm';

export function sourceGlyphEmoji(glyph: string): string {
  return wasmSourceGlyphEmoji(glyph);
}
