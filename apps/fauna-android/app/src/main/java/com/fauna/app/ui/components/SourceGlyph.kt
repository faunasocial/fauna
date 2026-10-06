package com.fauna.app.ui.components

import uniffi.fauna_core.SourceGlyph

/**
 * Android's `SourceGlyph → emoji` call site. Shared Rust owns the *concept*
 * (`fauna_core::source_glyph::SourceGlyph`, via `Rail::glyph()` / `SourceKind::glyph()`)
 * AND — since every app renders that concept as the same emoji — the **map** too
 * (`SourceGlyph::emoji`, reached over UniFFI as [com.fauna.ffi.sourceGlyphEmoji]).
 * Android keeps only the call.
 *
 * The SAME map serves the conversations rail (`ConversationListScreen`) AND the feed
 * badge ([ProtocolBadge]), so the two surfaces can't drift — the within-client
 * unification of `docs/goal/architecture/render-model.md` § Deltas → D5; the shared
 * map is the cross-client half of the same rule (the seven per-app copies had already
 * begun to disagree on the envelope's emoji-presentation selector).
 *
 * Canonical concepts ratified by the user 2026-06-22. Presentation only; affordances
 * gate on capabilities, never the glyph.
 */
internal fun sourceGlyphEmoji(glyph: SourceGlyph): String =
    com.fauna.ffi.sourceGlyphEmoji(glyph)
