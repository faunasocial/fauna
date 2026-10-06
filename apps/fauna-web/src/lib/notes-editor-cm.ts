// CodeMirror 6 wiring for the Notes WYSIWYG editor mode (delta #3/#4/#5). This is the
// browser-only half of the Notes editor: it turns the pure plan from `$lib/notes-editor` into
// CodeMirror decorations + chrome widgets + atomic ranges, and routes structural gestures through
// the shared `apply_structural_gesture` engine. The pure offset/marker logic (the real risk) lives
// in `$lib/notes-editor` and is unit-tested without a browser; this file is the thin CM glue.
//
// Installed as a single extension bundle by `MarkdownEditor.svelte` when its `notes` prop is set —
// the conversations compose path (`notes` unset) is untouched (priority #4: one editor, two modes).
//
// Design tracked internally (§ marker-hiding strategy / § structural-gesture API); the web
// mechanism is proven by 20 headless assertions (tracked internally).

import { RangeSet, Prec, type Extension } from '@codemirror/state';
import {
  Decoration,
  EditorView,
  ViewPlugin,
  WidgetType,
  keymap,
  type DecorationSet,
  type ViewUpdate,
} from '@codemirror/view';
import {
  computeNotePlan,
  composeMarkPlan,
  mapNoteLines,
  utf16ToByte,
  byteToUtf16,
  type NoteHide,
  type NoteLineDeco,
} from './notes-editor';
import {
  decorationMap,
  inlineRevealRanges,
  composeDecorationPlan,
  noteLineMap,
  parseNote,
  serializeNote,
  applyStructuralGesture,
  applyEdits,
  caretToBlockCaret,
  blockCaretToByte,
} from './wasm';
import type { NoteDocument, StructuralGesture } from './notes';

// ── Chrome widgets (replace the always-hidden structural prefix) ──────────────────────────────

class BulletWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const s = document.createElement('span');
    s.className = 'cm-note-bullet';
    s.setAttribute('aria-hidden', 'true');
    s.textContent = '•';
    return s;
  }
}

class OrderedWidget extends WidgetType {
  constructor(readonly n: number) {
    super();
  }
  eq(o: OrderedWidget) {
    return o.n === this.n;
  }
  toDOM() {
    const s = document.createElement('span');
    s.className = 'cm-note-ordered';
    s.setAttribute('aria-hidden', 'true');
    s.textContent = `${this.n}.`;
    return s;
  }
}

class CheckboxWidget extends WidgetType {
  constructor(
    readonly checked: boolean,
    readonly index: number,
    readonly blockId: number,
  ) {
    super();
  }
  eq(o: CheckboxWidget) {
    return o.checked === this.checked && o.index === this.index && o.blockId === this.blockId;
  }
  toDOM(view: EditorView) {
    const btn = document.createElement('button');
    btn.className = 'cm-note-checkbox';
    btn.type = 'button';
    btn.setAttribute('role', 'checkbox');
    // The approved indexed e2e id (spec § ui.yaml surface, user-approved 2026-06-27).
    btn.setAttribute('data-testid', `document-checkbox-${this.index}`);
    btn.setAttribute('aria-checked', String(this.checked));
    btn.textContent = this.checked ? '☑' : '☐';
    // mousedown (not click) + preventDefault so the editor selection isn't disturbed before we
    // dispatch the model toggle.
    btn.addEventListener('mousedown', (e) => {
      e.preventDefault();
      e.stopPropagation();
      toggleCheckbox(view, this.blockId);
    });
    return btn;
  }
  ignoreEvent() {
    return false;
  }
}

function hideWidget(h: NoteHide): WidgetType | null {
  if (!h.widget) return null;
  switch (h.widget.kind) {
    case 'bullet':
      return new BulletWidget();
    case 'ordered':
      return new OrderedWidget(h.widget.number);
    case 'checkbox':
      return new CheckboxWidget(h.widget.checked, h.widget.index, h.widget.blockId);
  }
}

// ── Decoration caches (immutable + shareable across recomputes, like the conversations applier) ─

const markCache = new Map<string, Decoration>();
function mark(className: string): Decoration {
  let m = markCache.get(className);
  if (!m) {
    m = Decoration.mark({ class: className });
    markCache.set(className, m);
  }
  return m;
}

const lineCache = new Map<string, Decoration>();
function lineDeco(l: NoteLineDeco): Decoration {
  const key = `${l.className}|${l.depth}`;
  let d = lineCache.get(key);
  if (!d) {
    const attributes: Record<string, string> = { class: l.className };
    if (l.depth > 0) attributes.style = `padding-left:${l.depth * 1.4}em`;
    d = Decoration.line({ attributes });
    lineCache.set(key, d);
  }
  return d;
}

// ── The decoration ViewPlugin: pure plan → CM DecorationSet + atomic RangeSet ──────────────────

class NotesView {
  decorations: DecorationSet = Decoration.none;
  atomic: RangeSet<Decoration> = RangeSet.empty;
  constructor(view: EditorView) {
    this.build(view);
  }
  update(u: ViewUpdate) {
    // Recompute on edits (content) AND caret moves (the per-run reveal depends on the caret).
    if (u.docChanged || u.selectionSet) this.build(u.view);
  }
  build(view: EditorView) {
    const value = view.state.doc.toString();
    try {
      const decos = decorationMap(value);
      const caretByte = utf16ToByte(value, view.state.selection.main.head);
      const reveal = inlineRevealRanges(value, caretByte);
      const blocks = parseNote(value).blocks;
      const lineMap = mapNoteLines(value, noteLineMap(value, blocks), blocks);
      const plan = computeNotePlan(value, lineMap, decos, reveal);

      const ranges: { from: number; to: number; value: Decoration }[] = [];
      for (const l of plan.lines) ranges.push({ from: l.pos, to: l.pos, value: lineDeco(l) });
      for (const h of plan.hides) {
        const w = hideWidget(h);
        const deco = w ? Decoration.replace({ widget: w }) : Decoration.replace({});
        ranges.push({ from: h.from, to: h.to, value: deco });
      }
      for (const m of plan.marks) ranges.push({ from: m.from, to: m.to, value: mark(m.className) });

      this.decorations = Decoration.set(
        ranges.map((r) => r.value.range(r.from, r.to)),
        true,
      );
      // Atomic over every hidden range so the caret skips a hidden run as one unit (an inline
      // marker un-hides — leaves this set — the moment the caret reaches its run edge, so the
      // caret is never trapped; the structural prefix is never revealed — always atomic chrome).
      this.atomic = RangeSet.of(
        plan.hides.map((h) => Decoration.replace({}).range(h.from, h.to)),
        true,
      );
    } catch {
      // wasm not ready (shouldn't happen — the page awaits ensureWasm before mounting the editor).
      this.decorations = Decoration.none;
      this.atomic = RangeSet.empty;
    }
  }
}

const notesPlugin = ViewPlugin.fromClass(NotesView, {
  decorations: (v) => v.decorations,
  provide: (plugin) =>
    EditorView.atomicRanges.of((view) => view.plugin(plugin)?.atomic ?? RangeSet.empty),
});

// ── Compose hide-by-default mode: the inline-only half of the Notes hide engine ───────────────
//
// The conversations compose field's default:
// inline emphasis markers HIDDEN (atomic + per-run caret-edge reveal), structural markers left
// DIMMED (no chrome, no gestures — Enter still sends). No widgets, no line decorations, no keymap —
// it is `computeComposePlan` (the inline subset of `computeNotePlan`) over the same wasm faces. The
// dim/styled classes (`cm-md-*`) are themed by `MarkdownEditor.svelte` (shared with the Notes theme).

const COMPOSE_HIDE = Decoration.replace({});

class ComposeHideView {
  decorations: DecorationSet = Decoration.none;
  atomic: RangeSet<Decoration> = RangeSet.empty;
  constructor(view: EditorView) {
    this.build(view);
  }
  update(u: ViewUpdate) {
    if (u.docChanged || u.selectionSet) this.build(u.view);
  }
  build(view: EditorView) {
    const value = view.state.doc.toString();
    try {
      const head = view.state.selection.main.head;
      const decos = decorationMap(value);
      // The marker hide/dim classification + caret-edge reveal is shared Rust; the web just maps
      // the byte ranges to UTF-16 + styles content (priority #2).
      const markerPlan = composeDecorationPlan(value, utf16ToByte(value, head));
      const plan = composeMarkPlan(value, decos, markerPlan);

      const ranges: { from: number; to: number; value: Decoration }[] = [];
      for (const h of plan.hides) ranges.push({ from: h.from, to: h.to, value: COMPOSE_HIDE });
      for (const m of plan.marks) ranges.push({ from: m.from, to: m.to, value: mark(m.className) });

      this.decorations = Decoration.set(
        ranges.map((r) => r.value.range(r.from, r.to)),
        true,
      );
      this.atomic = RangeSet.of(
        plan.hides.map((h) => COMPOSE_HIDE.range(h.from, h.to)),
        true,
      );
    } catch {
      this.decorations = Decoration.none;
      this.atomic = RangeSet.empty;
    }
  }
}

const composeHidePlugin = ViewPlugin.fromClass(ComposeHideView, {
  decorations: (v) => v.decorations,
  provide: (plugin) =>
    EditorView.atomicRanges.of((view) => view.plugin(plugin)?.atomic ?? RangeSet.empty),
});

/** The compose **hide-markers** decoration extension (inline markers hidden + caret-edge reveal,
 *  structural markers dimmed). Installed by `MarkdownEditor.svelte` in place of the dim
 *  `decorationPlugin` when the per-editor marker toggle is in its default (hidden) state. No
 *  gesture keymap — Enter still sends (the structural gestures stay Notes-only). */
export function composeHideExtension(): Extension {
  return [composeHidePlugin];
}

// ── Gesture routing: CM key → shared `apply_structural_gesture` → buffer replace ──────────────

/** The fresh block id a split/insert gesture mints (`> max`). */
function nextId(doc: NoteDocument): number {
  let max = -1;
  for (const b of doc.blocks) if (b.id > max) max = b.id;
  return max + 1;
}

/**
 * Run a structural gesture through the shared engine and apply its result to the buffer. Returns
 * `false` (let CodeMirror handle the key by default) when the gesture is a no-op — an empty `edits`
 * list, which the engine returns for Enter-in-code (→ CM inserts a literal `\n`),
 * Backspace-not-at-start (→ CM deletes a char), an un-indentable first item, etc.
 */
function runGesture(view: EditorView, gesture: StructuralGesture): boolean {
  // Only act on a collapsed caret — a range selection falls through to CM's default editing.
  const sel = view.state.selection.main;
  if (!sel.empty) return false;
  const value = view.state.doc.toString();
  let doc: NoteDocument;
  try {
    doc = parseNote(value);
  } catch {
    return false;
  }
  // Caret seam (shared Rust core, web does only the UTF-16→byte conversion): CM UTF-16 caret →
  // whole-buffer byte → `BlockCaret`.
  const caret = caretToBlockCaret(value, doc.blocks, utf16ToByte(value, sel.head));
  if (!caret) return false;
  let result;
  try {
    result = applyStructuralGesture(doc, caret, gesture, nextId(doc));
  } catch {
    return false;
  }
  if (result.edits.length === 0) return false;

  let newDoc: NoteDocument;
  let newValue: string;
  try {
    newDoc = applyEdits(doc, result.edits);
    newValue = serializeNote(newDoc);
  } catch {
    return false;
  }
  // Inverse seam: the gesture's `BlockCaret` → whole-buffer byte (shared Rust) → CM UTF-16.
  const newCaret = byteToUtf16(newValue, blockCaretToByte(newValue, newDoc.blocks, result.caret));
  view.dispatch({
    changes: { from: 0, to: value.length, insert: newValue },
    selection: { anchor: newCaret },
    scrollIntoView: true,
    userEvent: 'input',
  });
  return true;
}

/** Toggle a todo's checkbox via the shared engine (the widget's click handler). */
function toggleCheckbox(view: EditorView, blockId: number): void {
  const value = view.state.doc.toString();
  let doc: NoteDocument;
  try {
    doc = parseNote(value);
  } catch {
    return;
  }
  if (!doc.blocks.some((b) => b.id === blockId)) return;
  let result;
  try {
    result = applyStructuralGesture(doc, { block: blockId, offset: 0 }, 'toggle_checkbox', nextId(doc));
  } catch {
    return;
  }
  if (result.edits.length === 0) return;
  let newValue: string;
  try {
    newValue = serializeNote(applyEdits(doc, result.edits));
  } catch {
    return;
  }
  view.dispatch({ changes: { from: 0, to: value.length, insert: newValue }, userEvent: 'input' });
}

// High-precedence so it shadows the default Enter/Backspace and the browser's Tab. Tab/Shift-Tab
// are structural-only in Notes (no literal tab char), so they always consume the key; Enter and
// Backspace fall through (return false) on a no-op so CM's default runs.
const gestureKeymap = Prec.highest(
  keymap.of([
    { key: 'Enter', run: (v) => runGesture(v, 'newline') },
    {
      key: 'Tab',
      run: (v) => {
        runGesture(v, 'indent');
        return true;
      },
    },
    {
      key: 'Shift-Tab',
      run: (v) => {
        runGesture(v, 'outdent');
        return true;
      },
    },
    { key: 'Backspace', run: (v) => runGesture(v, 'backspace_at_start') },
  ]),
);

// ── The Notes-mode theme (cm-note-* chrome) ───────────────────────────────────────────────────

const notesTheme = EditorView.theme({
  '.cm-note-heading-1': { fontSize: '1.6em', fontWeight: '700', lineHeight: '1.3' },
  '.cm-note-heading-2': { fontSize: '1.4em', fontWeight: '700', lineHeight: '1.3' },
  '.cm-note-heading-3': { fontSize: '1.2em', fontWeight: '700' },
  '.cm-note-heading-4': { fontSize: '1.1em', fontWeight: '700' },
  '.cm-note-quote': {
    borderLeft: '3px solid var(--border)',
    paddingLeft: '0.6rem',
    color: 'var(--text-muted)',
    fontStyle: 'italic',
  },
  '.cm-note-code': {
    fontFamily: 'monospace',
    backgroundColor: 'rgba(127, 127, 127, 0.08)',
  },
  '.cm-note-bullet': { display: 'inline-block', width: '1.2em', color: 'var(--text-muted)' },
  '.cm-note-ordered': { display: 'inline-block', minWidth: '1.4em', color: 'var(--text-muted)' },
  '.cm-note-checkbox': {
    background: 'none',
    border: 'none',
    cursor: 'pointer',
    padding: '0',
    marginRight: '0.3em',
    font: 'inherit',
    fontSize: '1.1em',
    lineHeight: '1',
    color: 'var(--text)',
  },
  // Inline-marker styles match the conversations applier (revealed markers dim; content styled).
  '.cm-md-bold': { fontWeight: '700' },
  '.cm-md-italic': { fontStyle: 'italic' },
  '.cm-md-bold-italic': { fontWeight: '700', fontStyle: 'italic' },
  '.cm-md-code': { fontFamily: 'monospace' },
  '.cm-md-link': { textDecoration: 'underline', color: 'var(--accent)' },
  '.cm-md-marker': { opacity: '0.45' },
});

/** The full Notes-mode extension bundle: the decoration/chrome plugin + atomic ranges (via the
 *  plugin's `provide`) + the high-precedence gesture keymap + the chrome theme. Installed in place
 *  of the conversations decoration plugin when `MarkdownEditor.svelte`'s `notes` prop is set. */
export function notesEditorExtensions(): Extension {
  return [notesPlugin, gestureKeymap, notesTheme];
}
