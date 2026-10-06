// Browser harness entry for the Notes WYSIWYG editor (Slice 5, second half — the
// one layer the deno unit tests + the real-engine integration proof do NOT
// exercise: actual CodeMirror DOM rendering of the production applier in a real
// browser, plus live-editor structural gestures + IME composition).
//
// It mounts a CodeMirror EditorView wired EXACTLY as `MarkdownEditor.svelte`'s
// `onMount` wires it in Notes mode (apps/fauna-web/src/lib/components/
// MarkdownEditor.svelte:173-190) — same extension stack, installing the SHIPPING
// `notesEditorExtensions()` over the SHIPPING wasm faces (`decorationMap`,
// `inlineRevealRanges`, `parseNote`, `applyStructuralGesture`, …). The Svelte
// wrapper around this view (the `decoCompartment` reconfigure + the controlled
// `value`/`oninput` plumbing) is conversations-shared substrate and is out of
// scope here — it is covered by the gated (b) e2e-unified `--client web` run on
// the `document_detail` host surface (see NEXT § OPEN DEPENDENCY). What is proven
// here is the Notes-specific rendering + gesture path on production code.
//
// `window.__harness` exposes the live editor model + a DOM view of the REVEALED
// text (chrome widgets stripped) so the Python Playwright driver
// (`run_notes_harness.py`) can assert the 20 probe rendering rows + gestures + IME
// against the real DOM and the real CM buffer — the same interface shape the
// throwaway probe used, now over the shipping component's logic.
//
// Contract: the notes web WYSIWYG probe findings (tracked internally) — the 20
// rows. Design: the notes WYSIWYG editor design (tracked internally).

import { EditorState, Compartment } from '@codemirror/state';
import { Decoration, EditorView, keymap } from '@codemirror/view';
import { history, defaultKeymap, historyKeymap } from '@codemirror/commands';

import { ensureWasm } from '$lib/wasm';
import { notesEditorExtensions, composeHideExtension } from '$lib/notes-editor-cm';

// The spec's exact mixed-block probe corpus: a heading, two nested bullets, a
// checked + an unchecked todo, and a paragraph with inline **bold** + `code`.
// Identical to the probe so the 20 rendering rows map across 1:1.
const CORPUS = [
  '# Plan',
  '- groceries',
  '  - milk',
  '- [ ] ship it',
  '- [x] write tests',
  'Buy **milk** and run `code` now.',
].join('\n');

// Chrome widgets rendered by `notesEditorExtensions()` for structural prefixes.
// `visibleText()` strips these so it measures only REVEALED source text.
const CHROME_SELECTOR = '.cm-note-bullet, .cm-note-ordered, .cm-note-checkbox';

const CONTENT_TESTID = 'notes-harness-content';

// Mirror of MarkdownEditor.svelte's onMount extension stack in Notes mode. The
// Notes-specific layer is `notesEditorExtensions()`; the rest (history, default
// keymap, line wrapping, the content testid attribute) is the shared substrate
// the component always installs.
function makeView(parent: HTMLElement): EditorView {
  const decoCompartment = new Compartment();
  return new EditorView({
    parent,
    state: EditorState.create({
      doc: CORPUS,
      extensions: [
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({ 'data-testid': CONTENT_TESTID, spellcheck: 'false' }),
        decoCompartment.of(notesEditorExtensions()),
      ],
    }),
  });
}

function visibleText(view: EditorView): string {
  // Clone the content DOM, drop the chrome widgets, and read text per line — so a
  // hidden marker (rendered as an empty Decoration.replace, i.e. no DOM) and a
  // chrome glyph (bullet/checkbox) both fall out, leaving only revealed source.
  const content = view.contentDOM.cloneNode(true) as HTMLElement;
  content.querySelectorAll(CHROME_SELECTOR).forEach((el) => el.remove());
  const lines = Array.from(content.querySelectorAll('.cm-line')).map(
    (l) => (l as HTMLElement).textContent ?? '',
  );
  return lines.join('\n');
}

declare global {
  interface Window {
    __harness?: Record<string, unknown>;
    __compose?: Record<string, unknown>;
    __harnessError?: string;
  }
}

async function boot() {
  try {
    // Initialise the REAL wasm bundle exactly as the SPA does at boot
    // (ensureWasm → static/fauna_wasm.js → fauna_wasm_bg.wasm). The applier calls
    // into it synchronously on every recompute, so it must be ready before mount.
    await ensureWasm();

    const mount = document.getElementById('mount');
    if (!mount) throw new Error('no #mount element');
    const view = makeView(mount);

    window.__harness = {
      // ── model (the CM buffer — every marker preserved, lossless markdown) ──
      doc: () => view.state.doc.toString(),
      caret: () => view.state.selection.main.head,
      offsetOf: (needle: string) => view.state.doc.toString().indexOf(needle),

      // ── doc control (reset between gesture cases; load a code-block corpus) ──
      setDoc: (text: string, caret = 0) => {
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: text },
          selection: { anchor: Math.min(caret, text.length) },
        });
        view.focus();
      },
      reset: () => {
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: CORPUS },
          selection: { anchor: 0 },
        });
        view.focus();
      },

      // ── caret / selection control ──
      setCaret: (pos: number) => {
        view.dispatch({ selection: { anchor: pos } });
        view.focus();
      },
      setSelection: (anchor: number, head: number) => {
        view.dispatch({ selection: { anchor, head } });
        view.focus();
      },
      focus: () => view.focus(),

      // ── rendered DOM view (chrome stripped → revealed source only) ──
      visibleText: () => visibleText(view),

      // ── structural chrome ──
      checkboxCount: () => view.dom.querySelectorAll('.cm-note-checkbox').length,
      checkboxChecked: (i: number) => {
        const boxes = view.dom.querySelectorAll('.cm-note-checkbox');
        const el = boxes[i] as HTMLElement | undefined;
        return el ? el.getAttribute('aria-checked') === 'true' : null;
      },
      checkboxTestid: (i: number) => {
        const boxes = view.dom.querySelectorAll('.cm-note-checkbox');
        const el = boxes[i] as HTMLElement | undefined;
        return el ? el.getAttribute('data-testid') : null;
      },
      bulletCount: () => view.dom.querySelectorAll('.cm-note-bullet').length,

      // ── console-error sink (assertion 1) ──
      consoleErrors: () => (window as unknown as { __consoleErrors: string[] }).__consoleErrors,
    };

    // ── Compose hide-mode editor (the conversations compose default + the toggle) ──
    // Mounts the SHIPPING `composeHideExtension()` (inline markers hidden, structural dimmed, no
    // gestures — Enter does NOT split). A compartment models the per-editor toggle: hidden (default)
    // ↔ shown (here, raw markers — the production "shown" state is the dimmed live-preview applier,
    // existing shipped behavior; the harness proves the visibility FLIP + the hide rendering).
    const composeMount = document.getElementById('compose-mount');
    if (composeMount) {
      const COMPOSE_CORPUS = ['Buy **milk** and run `code` now.', '# Heading', '- item'].join('\n');
      const deco = new Compartment();
      const cv = new EditorView({
        parent: composeMount,
        state: EditorState.create({
          doc: COMPOSE_CORPUS,
          extensions: [
            history(),
            keymap.of([...defaultKeymap, ...historyKeymap]),
            EditorView.lineWrapping,
            EditorView.contentAttributes.of({ 'data-testid': 'compose-harness-content' }),
            deco.of(composeHideExtension()),
          ],
        }),
      });
      window.__compose = {
        doc: () => cv.state.doc.toString(),
        caret: () => cv.state.selection.main.head,
        offsetOf: (needle: string) => cv.state.doc.toString().indexOf(needle),
        visibleText: () => visibleText(cv),
        setCaret: (pos: number) => {
          cv.dispatch({ selection: { anchor: pos } });
          cv.focus();
        },
        setMarkersShown: (shown: boolean) => {
          // hidden (default) installs the hide applier; shown removes it → raw markers visible.
          cv.dispatch({ effects: deco.reconfigure(shown ? [] : composeHideExtension()) });
        },
        focus: () => cv.focus(),
      };
    }
  } catch (e) {
    window.__harnessError = String(e instanceof Error ? e.stack ?? e.message : e);
  }
}

boot();
