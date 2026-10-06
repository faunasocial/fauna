// The Notes editor block-document model — the TS mirror of `fauna_core::notes` (over the
// `parseNote` / `serializeNote` / `applyStructuralGesture` / `applyEdits` wasm faces). Shapes
// match the cross-app JSON pinned by `notes::tests::wire_shape_is_stable` in shared Rust, so
// web and native cannot drift on the editor model (priorities #1/#2). The block model is
// substrate-agnostic (identical under Fork-1 single-buffer and Fork-2 block ops); the web Notes
// editor lowers persistence to the current Fork-1 spaces-documents markdown body today.
//
// Design tracked internally (§ the block-model sketch / § structural-gesture API).

/** A stable, opaque block id (`fauna_core::notes::BlockId`, a transparent newtype → a number). */
export type BlockId = number;

/** The structural kind of a block — the prefix (`- `/`# `/`> `/`1. `/`- [ ] `) is derived from
 *  this, never stored in `text`. `raw` is the verbatim escape hatch for un-modeled constructs. */
export type BlockKind =
  | 'paragraph'
  | 'bullet'
  | 'ordered'
  | 'todo'
  | 'heading'
  | 'quote'
  | 'code'
  | 'raw';

/** One block of a `NoteDocument`. `text` is inline-markdown (markers PRESENT — the editor hides
 *  + styles them); for `code`/`raw` it is the verbatim body. `checked` applies to `todo`,
 *  `level` (1–4) to `heading`. */
export interface Block {
  id: BlockId;
  kind: BlockKind;
  depth: number;
  checked: boolean;
  level: number;
  text: string;
}

/** A Note as a flat, ordered list of blocks (nesting via the integer `depth`). */
export interface NoteDocument {
  blocks: Block[];
}

/** The role a buffer line plays in the rendered editor (`fauna_core::notes::NoteLineRole`,
 *  snake_case). A fenced code block spans `code_open` + zero-or-more `code_body` + `code_close`
 *  lines — all one `code` block; every other block is one `block` line. */
export type NoteLineRole = 'block' | 'code_open' | 'code_body' | 'code_close';

/** Per-buffer-line structural projection from the `noteLineMap` wasm face
 *  (`fauna_core::notes::NoteLine`) — **byte** offsets (the shared substrate-agnostic unit; the
 *  web editor remaps to CodeMirror UTF-16 via `mapNoteLines`). The structural derivation (code
 *  folding, checkbox indexing, ordered-list numbering, structural-prefix range) is computed once
 *  in shared Rust, never re-derived per client (priority #2). */
export interface WasmNoteLine {
  index: number;
  /** Byte offset of the line's first char. */
  start: number;
  /** Byte offset of the line's end (before its `\n`, or buffer end). */
  end: number;
  role: NoteLineRole;
  /** Index of the owning block in the `blocks` passed to `noteLineMap`. */
  block: number;
  /** Structural-prefix byte range to hide + replace with chrome (`prefix_from === prefix_to` ⇒
   *  none). */
  prefix_from: number;
  prefix_to: number;
  /** 0-based index among the document's `todo` blocks, or `null` when not a todo. */
  checkbox_index: number | null;
  /** 1-based number within the ordered-list run at this depth, or `null` when not ordered. */
  ordered_number: number | null;
}

/** A caret inside a block's `text`, as a UTF-8 byte offset (the wasm/native offset unit; the
 *  web editor maps to/from CodeMirror UTF-16 positions). */
export interface BlockCaret {
  block: BlockId;
  offset: number;
}

/** A structural editing gesture routed through the shared engine (one key → one gesture). */
export type StructuralGesture =
  | 'newline'
  | 'indent'
  | 'outdent'
  | 'toggle_checkbox'
  | 'backspace_at_start';

/** One atomic change to a `NoteDocument` — the externally-tagged `fauna_core::notes::BlockEdit`
 *  (snake_case variant key → fields object). Lower to substrate ops (Fork-1: `applyEdits` then
 *  `serializeNote`). */
export type BlockEdit =
  | { split_block: { at: BlockCaret; new_id: BlockId } }
  | { merge_with_prev: { block: BlockId } }
  | { set_depth: { block: BlockId; depth: number } }
  | { set_kind: { block: BlockId; kind: BlockKind } }
  | { set_checked: { block: BlockId; checked: boolean } }
  | { insert_block: { after: BlockId | null; block: Block } };

/** The result of `applyStructuralGesture`: the edits to lower and the caret afterwards. An
 *  empty `edits` means the gesture was a no-op (handle as default text input — e.g. Enter in a
 *  code block). */
export interface GestureResult {
  edits: BlockEdit[];
  caret: BlockCaret;
}
