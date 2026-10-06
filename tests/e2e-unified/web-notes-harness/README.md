# Notes WYSIWYG editor — browser harness (web)

Headless-Chromium proof that the **shipping** Notes editor renders and edits
correctly in a real browser. It mounts the production `notesEditorExtensions()`
applier (`apps/fauna-web/src/lib/notes-editor-cm.ts`) over the **real wasm
bundle** in a CodeMirror view wired exactly as `MarkdownEditor.svelte`'s `onMount`
wires Notes mode, then asserts, against the live DOM + the live CM model:

- the **20 feasibility-probe rendering rows**
  (tracked internally) — markers hidden
  when the caret is away, structural chrome (bullets / checkboxes / heading),
  per-run caret-edge reveal + locality, atomic caret-skip, checkbox-click toggle,
  in-place editing inside concealed content;
- the **live-editor structural gestures** the probe explicitly deferred
  ("gestures not prototyped") — Enter-split, Tab-indent, Shift-Tab-outdent,
  Backspace-at-start-outdent, Enter-in-code literal-newline — each routed through
  the shared `apply_structural_gesture` engine via the real keymap;
- an **IME composition** pass (delta #6) over the atomic decorations.

## Why this exists (and what it does NOT cover)

The deno unit tests (`apps/fauna-web/src/lib/notes-editor.test.ts`) + the
real-engine integration proof cover the pure offset/marker logic and the shared
gesture engine composed with the real wasm. The **one** layer they cannot
exercise is actual CodeMirror DOM rendering in a browser — atomic hide, reveal,
chrome widgets, `atomicRanges` caret-skip, widget clicks, IME. That is exactly
this harness's job.

It mounts the production **applier**; the thin Svelte wrapper around it
(`MarkdownEditor.svelte`'s compartment reconfigure + controlled `value`/`oninput`
plumbing) is conversations-shared substrate and is covered by the eventual,
currently-gated **(b)** e2e-unified `--client web` run on the spaces
`document_detail` host surface (tracked internally, § OPEN DEPENDENCY).
This harness is the interim browser proof until that lands.

## Run

```
just notes-browser-harness          # builds the bundle, then runs the driver
```

or directly (needs deno + the e2e Playwright venv):

```
deno run -A build_harness.ts
/home/user/.venvs/fauna/bin/python run_notes_harness.py          # --headed to watch
```

Exits 0 iff every row passes (28 total), non-zero with a `FAILED:` list otherwise.
The suite is non-vacuous by construction: it asserts the *same* `**` run both
hidden (caret away) and revealed (caret inside), the model-vs-rendered-DOM
divergence, a specific atomic-skip caret offset, and specific gesture model
transforms — a stubbed or broken applier cannot pass all of them, and an absent
wasm binary fails the boot row loudly.

## Files

- `harness-entry.ts` — bundle entry; imports the real `$lib` Notes modules + real
  `ensureWasm`, mounts the view, exposes `window.__harness` (model + revealed-text
  DOM view) for the driver.
- `app-paths-stub.ts` — esbuild alias for SvelteKit's `$app/paths` (`base=''`), the
  only Kit virtual the Notes graph touches at runtime.
- `build_harness.ts` — deno + esbuild bundle build; stages the wasm binary + page
  into `build/` (gitignored).
- `harness.html` — the page (captures console errors for row 01).
- `run_notes_harness.py` — the Playwright driver + the 28 assertions.
