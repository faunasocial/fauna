// Compose-field inline-markdown decoration mapping (web glue).
//
// The shared Rust tokenizer (`fauna_core::markdown::decoration_map`, over the
// `decorationMap` wasm export) returns styled-content + marker ranges as **UTF-8
// byte** offsets into the raw compose source. CodeMirror 6 positions are **UTF-16
// code units** (`docs/goal/ui/conversations.md` § Compose-field inline markdown
// styling — "each app converts to its native offset unit"). This module is the
// pure conversion: byte ranges → UTF-16 `{from, to, className}` ranges, with the
// per-line marker-reveal rule (markers on the caret's line are shown un-styled so
// they can be edited; markers elsewhere are dimmed). No CodeMirror or DOM
// dependency, so it is unit-testable on its own (`markdown-decorations.test.ts`).
//
// The same scanner feeds `markdownToHtml`, so the editor preview and the sent
// message can never disagree on what is styled (the whole point of sourcing the
// ranges from shared Rust rather than a TS tokenizer).

/** Snake_case decoration kind — the cross-app wire string from
 *  `fauna_core::markdown::MdDecorationKind` (the `decorationMap` wasm face). */
export type MdDecorationKind =
  | 'marker'
  | 'bold'
  | 'italic'
  | 'bold_italic'
  | 'code'
  | 'link'
  | 'image'
  | 'heading'
  | 'blockquote'
  | 'list_marker';

/** One decoration over a **byte** range of the raw source — the JS shape of
 *  `fauna_core::markdown::MdDecoration` (the `decorationMap` wasm return). */
export interface MdDecoration {
  /** UTF-8 byte offset into the raw source (always on a char boundary). */
  start: number;
  end: number;
  kind: MdDecorationKind;
  /** Heading level 1–4 when `kind === 'heading'`, else 0. */
  level: number;
}

/** A marker byte range to **reveal** near the caret — the JS shape of
 *  `fauna_core::markdown::RevealSpan` (the `inlineRevealRanges` wasm face). A
 *  markers-never-shown (Notes) editor hides every marker `decorationMap` returns and
 *  un-hides the ranges in this set so the run under the caret can be edited. `start`/`end`
 *  are UTF-8 byte offsets into the raw source (same space as `MdDecoration`). */
export interface MdRevealRange {
  /** UTF-8 byte offset into the raw source (always on a char boundary). */
  start: number;
  end: number;
}

/** The compose hide-mode marker treatment from `fauna_core::markdown::compose_decoration_plan`
 *  (the `composeDecorationPlan` wasm face): inline emphasis markers to CONCEAL (`hide`) vs
 *  markers to show DIMMED (`dim` = structural prefixes off the caret line + inline markers
 *  revealed at the caret edge). UTF-8 byte ranges (same space as `MdDecoration`); markers in
 *  neither set are shown plain. One shared policy for web + native (priority #2). */
export interface ComposeMarkerPlan {
  hide: MdRevealRange[];
  dim: MdRevealRange[];
}

/** A resolved decoration range in **UTF-16** units (CodeMirror positions), with
 *  the CSS class to apply. `className` is one of the `cm-md-*` classes themed in
 *  `MarkdownEditor.svelte`. */
export interface DecoRange {
  from: number;
  to: number;
  className: string;
}

/** Styled-content kind → CSS class. Marker kinds are handled separately (the
 *  reveal-near-caret rule), so they are absent here. `image` shares `link`'s look
 *  (mirrors the linux applier's `Image → md-link`). Exported so the Notes applier
 *  (`$lib/notes-editor`) reuses the same inline-content class map (priority #4). */
export const CONTENT_CLASS: Partial<Record<MdDecorationKind, string>> = {
  bold: 'cm-md-bold',
  italic: 'cm-md-italic',
  bold_italic: 'cm-md-bold-italic',
  code: 'cm-md-code',
  link: 'cm-md-link',
  image: 'cm-md-link',
  heading: 'cm-md-heading',
  blockquote: 'cm-md-blockquote',
};

/** The dim class for a marker (`*`, `**`, `` ` ``, `#`, `>`, `-`/`1.`, link
 *  brackets) on a line other than the caret's. */
export const MARKER_CLASS = 'cm-md-marker';

/** UTF-8 byte length of a single Unicode code point. Exported so the Notes applier
 *  (`$lib/notes-editor`) shares the one UTF-16↔UTF-8 conversion (priority #4). */
export function utf8Len(codePoint: number): number {
  if (codePoint < 0x80) return 1;
  if (codePoint < 0x800) return 2;
  if (codePoint < 0x10000) return 3;
  return 4;
}

/** Map every UTF-8 byte offset that falls on a char boundary of `value` to its
 *  UTF-16 code-unit index. `decoration_map` offsets are always char boundaries,
 *  so a lookup miss can only mean a bug upstream; callers clamp to `value.length`.
 *  One pass over the code points (a `for…of` iterates by code point, and a
 *  code-point string's `.length` is 1 or 2 — the surrogate-pair count). */
export function byteToUtf16Index(value: string): Map<number, number> {
  const map = new Map<number, number>();
  let byte = 0;
  let u16 = 0;
  map.set(0, 0);
  for (const ch of value) {
    byte += utf8Len(ch.codePointAt(0)!);
    u16 += ch.length;
    map.set(byte, u16);
  }
  return map;
}

/**
 * Convert the shared decoration map (byte ranges) into UTF-16 `DecoRange`s for
 * the CodeMirror compose editor, applying the per-line marker-reveal rule:
 *
 * - Content kinds (bold/italic/…/heading/blockquote) always get their style class.
 * - Marker kinds are **dimmed** (`cm-md-marker`) when they appear in `dimRanges`
 *   (the caret-line reveal decision — `fauna_core::markdown::compose_show_markers_dim_ranges`,
 *   the `composeShowMarkersDimRanges` wasm face); markers absent from `dimRanges` sit on the
 *   caret's line and are revealed (no decoration) so the raw `*`/`#`/`>` source can be edited —
 *   the Obsidian "source on the active line" model. This module never derives the caret-line
 *   rule itself (priority #2/#4 — one shared policy for web + native).
 *
 * `dimRanges` are the **byte** ranges (same space as `decos`) `composeShowMarkersDimRanges`
 * returned for the current caret. Empty ranges (e.g. an empty `## ` heading's content) are
 * dropped — CodeMirror mark decorations require `from < to`.
 */
export function decorationRanges(
  value: string,
  decos: MdDecoration[],
  dimRanges: MdRevealRange[],
): DecoRange[] {
  if (decos.length === 0) return [];
  const idx = byteToUtf16Index(value);
  const b2u = (b: number): number => idx.get(b) ?? value.length;
  const dimmed = new Set(dimRanges.map((r) => `${r.start}:${r.end}`));

  const out: DecoRange[] = [];
  for (const d of decos) {
    const from = b2u(d.start);
    const to = b2u(d.end);
    if (from >= to) continue; // empty range — invalid for a CM mark
    if (d.kind === 'marker' || d.kind === 'list_marker') {
      if (dimmed.has(`${d.start}:${d.end}`)) {
        out.push({ from, to, className: MARKER_CLASS });
      }
      // else: marker on the caret's line — revealed (no decoration)
    } else {
      const cls = CONTENT_CLASS[d.kind];
      if (cls) out.push({ from, to, className: cls });
    }
  }
  return out;
}
