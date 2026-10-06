<script lang="ts">
  // Compose editor with inline-markdown styling (the web applier for
  // `docs/goal/ui/conversations.md` § Compose-field inline markdown styling — the
  // browser twin of linux's `compose_decoration.rs` `gtk::TextTag` applier).
  //
  // A CodeMirror 6 editor whose buffer holds the **literal markdown source**
  // (`hello *world*` is exactly what's stored/sent — no rich-text tree, no
  // serialize-on-send boundary). On every edit + caret move we recompute the
  // shared `decorationMap` (over wasm) and apply CodeMirror `Decoration` marks:
  // content ranges get a `.cm-md-*` style, marker characters are dimmed except on
  // the caret's line (where they are revealed so the raw source can be edited).
  // CodeMirror — unlike a transparent-textarea overlay — lays out the styled DOM
  // and positions the caret against the real rendered glyphs, so true bold/italic/
  // larger-heading styling stays caret-aligned (the reason this isn't an overlay;
  // see the 2026-06-14 dependency analysis).
  //
  // Controlled-component: `value` is the source of truth (the wasm
  // ConversationsManager's `body_draft`); the editor mirrors it and emits `oninput`
  // on user edits. CM uses **UTF-16 code-unit** positions, the same unit as JS
  // strings — so `$lib/markdown-decorations` converts the shared map's UTF-8 byte
  // ranges to UTF-16 and the toolbar splice math needs no offset translation.

  import { onMount, onDestroy } from 'svelte';
  import { EditorState, Compartment } from '@codemirror/state';
  import {
    EditorView,
    keymap,
    placeholder as cmPlaceholder,
    Decoration,
    ViewPlugin,
    type DecorationSet,
    type ViewUpdate,
  } from '@codemirror/view';
  import { history, defaultKeymap, historyKeymap } from '@codemirror/commands';
  import { decorationMap, composeShowMarkersDimRanges, wrapMarkdownSelection } from '$lib/wasm';
  import { decorationRanges, byteToUtf16Index } from '$lib/markdown-decorations';
  import { utf16ToByte } from '$lib/notes-editor';
  import { notesEditorExtensions, composeHideExtension } from '$lib/notes-editor-cm';

  interface Props {
    /** The markdown source (controlled — the manager's `body_draft`). */
    value: string;
    /** Fired with the new source on every user edit. */
    oninput: (value: string) => void;
    /** Empty-state placeholder text. */
    placeholder?: string;
    /** Inline styling is active only when the rail supports markdown
     *  (`capabilities.supports_markdown`); off it, the editor is plain text so we
     *  don't mislead the user into thinking `*x*` will render on the wire. */
    markdownEnabled?: boolean;
    /** Notes WYSIWYG mode (the spaces-documents editor): structural markers are
     *  NEVER shown (bullets/checkboxes/headings drawn as chrome from the shared
     *  block model) and inline markers are HIDDEN, revealed only at the caret edge,
     *  with Enter/Tab/Backspace routed through the shared gesture engine. Off (the
     *  default) keeps the conversations compose behaviour EXACTLY — markers dimmed,
     *  whole-line reveal, no gestures (priority #4: one editor, two modes). See
     *  `$lib/notes-editor-cm` + the Notes editor design spec. */
    notes?: boolean;
    /** Compose marker visibility (the per-editor toggle; conversations + feed compose). When
     *  `false` (the default), inline
     *  emphasis markers are HIDDEN (revealed at the caret edge) and structural markers dimmed; when
     *  `true`, the field falls back to the all-dimmed live-preview. Ignored in `notes` mode (Notes
     *  always hides). Enter still sends either way — no structural gestures in compose. */
    markersShown?: boolean;
    /** The e2e id, set on CodeMirror's contenteditable content element. */
    testid?: string;
    /** Bound OUT: the toolbar splice hook (set once the view mounts, null after
     *  destroy). The parent passes it to `MarkdownToolbar`'s `wrap` so the toolbar
     *  buttons splice into THIS editor's buffer. A bound callback (not `bind:this`
     *  + an exported method) keeps the contract framework-version-robust. */
    wrapSelection?: ((prefix: string, suffix: string) => void) | null;
  }

  let {
    value,
    oninput,
    placeholder = '',
    markdownEnabled = true,
    notes = false,
    markersShown = false,
    testid = 'dm-text-field',
    wrapSelection = $bindable(null),
  }: Props = $props();

  // The decoration-layer extension for the current mode: the Notes WYSIWYG bundle
  // (hide + chrome + caret-edge reveal + gesture keymap) when `notes` is set, else
  // the conversations dim+whole-line-reveal applier. Empty when markdown is off.
  function decoExtension() {
    if (!markdownEnabled) return [];
    if (notes) return notesEditorExtensions();
    // Compose: hide inline markers by default; the toggle reveals the all-dimmed live-preview.
    // Either way a quote's lines are indented, as the sent bubble indents a blockquote.
    return [markersShown ? decorationPlugin : composeHideExtension(), quoteLinePlugin];
  }

  let host: HTMLDivElement;
  let view: EditorView | null = null;
  // True while WE programmatically replace the doc (external value change), so the
  // update listener doesn't echo our own write back out as an `oninput`.
  let syncing = false;
  const decoCompartment = new Compartment();

  // Reusable marks, keyed by class — `Decoration.mark` instances are immutable +
  // shareable across the document and across recomputes.
  const markCache = new Map<string, Decoration>();
  function mark(className: string): Decoration {
    let m = markCache.get(className);
    if (!m) {
      m = Decoration.mark({ class: className });
      markCache.set(className, m);
    }
    return m;
  }

  function computeDecorations(v: EditorView): DecorationSet {
    const text = v.state.doc.toString();
    let decos, dimRanges;
    try {
      decos = decorationMap(text);
      dimRanges = composeShowMarkersDimRanges(text, utf16ToByte(text, v.state.selection.main.head));
    } catch {
      // wasm not ready (shouldn't happen — the page awaits ensureWasm before the
      // compose bar renders) → no styling rather than a thrown keystroke handler.
      return Decoration.none;
    }
    const ranges = decorationRanges(text, decos, dimRanges);
    // `sort: true` — overlapping marks (e.g. heading + inner emphasis) and the
    // post-filter order need CodeMirror to sort by (from, side).
    return Decoration.set(
      ranges.map((r) => mark(r.className).range(r.from, r.to)),
      true,
    );
  }

  // A quote is indented as a whole LINE, wrapped continuation included — which a mark
  // on the quote's text cannot do — so each line holding blockquote content gets a
  // `cm-md-quote-line` line decoration (conversations.md § Compose-field inline
  // markdown styling: "indented quotes as you type"; linux's `md-quote-indent`).
  const quoteLine = Decoration.line({ class: 'cm-md-quote-line' });
  function quoteLines(v: EditorView): DecorationSet {
    const text = v.state.doc.toString();
    let decos;
    try {
      decos = decorationMap(text);
    } catch {
      return Decoration.none;
    }
    const idx = byteToUtf16Index(text);
    const starts = new Set<number>();
    for (const d of decos) {
      if (d.kind !== 'blockquote') continue;
      const from = idx.get(d.start) ?? text.length;
      starts.add(v.state.doc.lineAt(from).from);
    }
    return Decoration.set(
      [...starts].sort((a, b) => a - b).map((at) => quoteLine.range(at)),
    );
  }
  const quoteLinePlugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) {
        this.decorations = quoteLines(v);
      }
      update(u: ViewUpdate) {
        if (u.docChanged) this.decorations = quoteLines(u.view);
      }
    },
    { decorations: (v) => v.decorations },
  );

  const decorationPlugin = ViewPlugin.fromClass(
    class {
      decorations: DecorationSet;
      constructor(v: EditorView) {
        this.decorations = computeDecorations(v);
      }
      update(u: ViewUpdate) {
        // Recompute on edits (content) AND caret moves (the marker reveal depends
        // on the caret's line) — mirrors linux re-styling on change + cursor move.
        if (u.docChanged || u.selectionSet) {
          this.decorations = computeDecorations(u.view);
        }
      }
    },
    { decorations: (v) => v.decorations },
  );

  // .cm-md-* mirror the rendered message bubble's look (markdownToHtml →
  // <strong>/<em>/<code>/<h*>/<blockquote>) and the compose `.input`/`.body` CSS,
  // so the live preview reads the same as the sent message.
  const theme = EditorView.theme({
    '&': {
      border: '1px solid var(--border)',
      borderRadius: '6px',
      backgroundColor: 'var(--bg)',
      color: 'var(--text)',
      fontSize: '0.875rem',
    },
    '&.cm-focused': { outline: '2px solid var(--accent)', outlineOffset: '-1px' },
    '.cm-content': {
      padding: '0.45rem 0.6rem',
      minHeight: '70px',
      fontFamily: 'inherit',
      caretColor: 'var(--text)',
    },
    '.cm-scroller': { maxHeight: '40vh', overflowY: 'auto', fontFamily: 'inherit', lineHeight: '1.4' },
    '.cm-placeholder': { color: 'var(--text-muted)' },
    '&.cm-editor': { width: '100%' },
    '.cm-md-bold': { fontWeight: '700' },
    '.cm-md-italic': { fontStyle: 'italic' },
    '.cm-md-bold-italic': { fontWeight: '700', fontStyle: 'italic' },
    '.cm-md-code': { fontFamily: 'monospace' },
    '.cm-md-link': { textDecoration: 'underline', color: 'var(--accent)' },
    '.cm-md-heading': { fontWeight: '700', fontSize: '1.3em' },
    '.cm-md-blockquote': { fontStyle: 'italic', color: 'var(--text-muted)' },
    '.cm-line.cm-md-quote-line': { paddingLeft: '1.2em', borderLeft: '3px solid var(--border)' },
    '.cm-md-marker': { opacity: '0.45' },
  });

  onMount(() => {
    view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: value,
        extensions: [
          history(),
          keymap.of([...defaultKeymap, ...historyKeymap]),
          EditorView.lineWrapping,
          cmPlaceholder(placeholder),
          EditorView.contentAttributes.of({ 'data-testid': testid, spellcheck: 'true' }),
          decoCompartment.of(decoExtension()),
          theme,
          EditorView.updateListener.of((u) => {
            if (u.docChanged && !syncing) oninput(u.state.doc.toString());
          }),
        ],
      }),
    });
    wrapSelection = doWrap;
    e2eRegisterDoc();
  });

  // E2E-only: expose this editor's FULL CodeMirror document by testid.
  //
  // CodeMirror 6 virtualizes its viewport, so the `data-testid` content element
  // holds only the on-screen slice of the doc — reading its `textContent` (what
  // the harness used to do) silently truncates any draft longer than a screenful
  // (measured: 10,812 chars returned for a 2,000,000-char draft). The editor's
  // own `state.doc` is the honest read, and only this component has the handle.
  // Behind the compile-time gate per `testing.md` point 15, so a production
  // `vite build` folds the branch away and ships no `window.__fauna_*` surface.
  function e2eRegisterDoc() {
    if (!__FAUNA_E2E_AUTOMATION__ || typeof window === 'undefined') return;
    const w = window as unknown as {
      __fauna_editor_docs?: Record<string, () => string>;
    };
    (w.__fauna_editor_docs ??= {})[testid] = () => view?.state.doc.toString() ?? '';
  }

  function e2eUnregisterDoc() {
    if (!__FAUNA_E2E_AUTOMATION__ || typeof window === 'undefined') return;
    const w = window as unknown as {
      __fauna_editor_docs?: Record<string, () => string>;
    };
    if (w.__fauna_editor_docs) delete w.__fauna_editor_docs[testid];
  }

  onDestroy(() => {
    e2eUnregisterDoc();
    view?.destroy();
    view = null;
    wrapSelection = null;
  });

  // Reconcile external `value` changes (thread switch, reply seeding, send-clear,
  // toolbar splice routed through the manager) into the editor without echoing
  // back. No-op when the doc already matches (the common case: the user just typed
  // and the manager round-tripped the same string), which is what stops a caret
  // reset on every keystroke.
  $effect(() => {
    const v = value;
    if (!view) return;
    const cur = view.state.doc.toString();
    if (v === cur) return;
    syncing = true;
    // No explicit selection — CodeMirror maps the existing selection through the
    // change (clamped to bounds), which keeps the caret sensible on a send-clear /
    // thread-switch / reply-seed and avoids forcing it to a computed offset.
    view.dispatch({ changes: { from: 0, to: cur.length, insert: v } });
    syncing = false;
  });

  // Reconfigure the decoration layer when the rail's markdown support flips or the
  // mode (conversations ↔ Notes) changes. Reading both `markdownEnabled` and `notes`
  // makes this effect re-run on either.
  $effect(() => {
    void markdownEnabled;
    void notes;
    void markersShown;
    if (!view) return;
    view.dispatch({
      effects: decoCompartment.reconfigure(decoExtension()),
    });
  });

  /** Toolbar splice: wrap the current selection in `prefix`/`suffix` via the
   *  shared `wrapMarkdownSelection` rule (edge whitespace kept OUTSIDE the markers)
   *  and re-select the wrapped core — one atomic transaction so the caret never
   *  races an async snapshot round-trip (the change still flows out via the update
   *  listener → `oninput` → manager). The web twin of the native toolbars'
   *  per-widget splice; the wrap *rule* is shared Rust (priority #1/#2/#4).
   *  Bound out to the toolbar via `wrapSelection` once the view mounts. */
  function doWrap(prefix: string, suffix: string): void {
    if (!view) return;
    const { from, to } = view.state.selection.main;
    const selected = view.state.sliceDoc(from, to);
    const { replacement, beforeCore, core } = wrapMarkdownSelection(selected, prefix, suffix, 'text');
    const selFrom = from + beforeCore.length;
    view.dispatch({
      changes: { from, to, insert: replacement },
      selection: { anchor: selFrom, head: selFrom + core.length },
    });
    view.focus();
  }
</script>

<div class="md-editor" bind:this={host}></div>

<style>
  .md-editor {
    width: 100%;
  }
</style>
