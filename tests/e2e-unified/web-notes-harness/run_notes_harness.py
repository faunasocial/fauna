#!/usr/bin/env python3
"""Headless-Chromium browser harness for the Notes WYSIWYG editor (web).

Re-proves the 20 rendering rows from the feasibility probe
(tracked internally) on the SHIPPING
`notesEditorExtensions()` applier over the REAL wasm bundle — the one layer the
deno unit tests (`notes-editor.test.ts`) + the real-engine integration proof do
NOT exercise: actual CodeMirror DOM rendering, atomic caret-skip, and chrome
widget clicks in a live browser. It then adds the live-editor GESTURE rows the
probe explicitly deferred ("gestures not prototyped") — Enter/Tab/Shift-Tab/
Backspace routed through the shared `apply_structural_gesture` engine — and an
IME composition pass (delta #6, the probe's other deferred item).

This drives the bundle built by `build_harness.ts` (the production
`notesEditorExtensions()` + the real `$lib/wasm`, mounted in a CodeMirror view
wired exactly as MarkdownEditor.svelte's onMount wires Notes mode). Run via
`just notes-browser-harness` (which builds the bundle first), or directly:

    /home/user/.venvs/fauna/bin/python run_notes_harness.py [--headed]

Exits 0 iff every row passes; non-zero (with a FAIL list) otherwise. Sibling-safe
by construction: the browser is Playwright's bundled Chromium (drivers/browser.py
resolution — no machine-wide singleton, so no serialization needed).
"""

from __future__ import annotations

import json
import socketserver
import sys
import threading
from functools import partial
from http.server import SimpleHTTPRequestHandler
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_BUILD_DIR = _HERE / "build"
# Shared browser resolution (bundled Chromium first — see drivers/browser.py).
sys.path.insert(0, str(_HERE.parent))
from drivers.browser import launch_kwargs  # noqa: E402


# ── results ────────────────────────────────────────────────────────────────
_results: list[tuple[str, bool, str]] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    _results.append((name, bool(ok), detail))
    mark = "PASS" if ok else "FAIL"
    line = f"[{mark}] {name}"
    if detail and not ok:
        line += f"  — {detail}"
    print(line, flush=True)


# ── a tiny static server for the build dir (correct .wasm mime) ────────────
class _Handler(SimpleHTTPRequestHandler):
    extensions_map = {**SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm"}

    def log_message(self, *args):  # silence
        pass


def _serve(directory: Path) -> tuple[socketserver.TCPServer, int]:
    handler = partial(_Handler, directory=str(directory))
    httpd = socketserver.TCPServer(("127.0.0.1", 0), handler)
    port = httpd.server_address[1]
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, port


# ── harness JS bridge ──────────────────────────────────────────────────────
def make_h(page):
    def h(method: str, *args):
        arg_js = ",".join(json.dumps(a) for a in args)
        return page.evaluate(f"() => window.__harness.{method}({arg_js})")

    return h


def run_assertions(page) -> None:
    h = make_h(page)

    # --- Row 1: no JS/console errors on load -------------------------------
    errs = h("consoleErrors")
    check("01 no console/JS errors on load", errs == [], f"errors={errs}")

    # --- Row 2: the MODEL keeps every marker (lossless markdown buffer) ----
    doc = h("doc")
    keeps = all(
        s in doc
        for s in ("# Plan", "**milk**", "`code`", "- [ ] ship it", "- [x] write tests")
    )
    check("02 model keeps every marker", keeps, f"doc={doc!r}")

    # Caret parked at the very start (line 0), far from the inline runs on line 5.
    h("setCaret", 0)
    vis = h("visibleText")

    # --- Rows 3-4: inline ** and ` HIDDEN when the caret is away -----------
    check("03 inline ** hidden when caret away", "**" not in vis, f"visible={vis!r}")
    check("04 inline ` hidden when caret away", "`" not in vis, f"visible={vis!r}")

    # --- Row 5: heading '# ' prefix hidden ---------------------------------
    check("05 heading # prefix hidden", "# Plan" not in vis and "Plan" in vis, f"visible={vis!r}")

    # --- Row 6: todo '- [ ]' / '- [x]' prefixes hidden ---------------------
    check(
        "06 todo prefixes hidden",
        "[ ]" not in vis and "[x]" not in vis and "ship it" in vis and "write tests" in vis,
        f"visible={vis!r}",
    )

    # --- Row 7: bullet '- ' prefix hidden ----------------------------------
    check("07 bullet - prefix hidden", "- groceries" not in vis and "groceries" in vis, f"visible={vis!r}")

    # --- Row 8: all content words survive ----------------------------------
    words = ["Plan", "groceries", "milk", "ship it", "write tests", "Buy", "code", "now"]
    missing = [w for w in words if w not in vis]
    check("08 all content words survive", not missing, f"missing={missing} visible={vis!r}")

    # --- Row 9: two tappable checkbox <button> widgets ---------------------
    n_cb = h("checkboxCount")
    check("09 two checkbox widgets rendered", n_cb == 2, f"count={n_cb}")

    # --- Rows 10-11: aria-checked reflects the model -----------------------
    check("10 unchecked todo aria-checked=false", h("checkboxChecked", 0) is False, "")
    check("11 checked todo aria-checked=true", h("checkboxChecked", 1) is True, "")
    # the approved indexed e2e id is on the widget
    check(
        "11b checkbox testid is document-checkbox-{index}",
        h("checkboxTestid", 0) == "document-checkbox-0" and h("checkboxTestid", 1) == "document-checkbox-1",
        f"ids={h('checkboxTestid', 0)},{h('checkboxTestid', 1)}",
    )

    # --- Row 12: caret can be placed inside the bold run -------------------
    bold = h("offsetOf", "**milk**")
    h("setCaret", bold + 4)
    check("12 caret placed inside bold run", h("caret") == bold + 4, f"caret={h('caret')}")

    # --- Row 13: caret inside **milk** reveals ITS markers -----------------
    vis_bold = h("visibleText")
    check("13 caret-edge reveal: bold ** reappears", "**milk**" in vis_bold, f"visible={vis_bold!r}")

    # --- Row 14: reveal is LOCAL — `code` stays hidden --------------------
    check("14 reveal is local: code ` stays hidden", "`" not in vis_bold, f"visible={vis_bold!r}")

    # --- Row 15: markers re-hide when the caret leaves the run -------------
    h("setCaret", 0)
    check("15 markers re-hide on caret leave", "**" not in h("visibleText"), "")

    # --- Rows 16-18: checkbox click toggles the MODEL (+ widget re-renders) -
    page.click('css=[data-testid="document-checkbox-0"]')
    doc_after = h("doc")
    check("16 checkbox click toggles model [ ]->[x]", "- [x] ship it" in doc_after, f"doc={doc_after!r}")
    check("17 widget reflects checked after toggle", h("checkboxChecked", 0) is True, "")
    page.click('css=[data-testid="document-checkbox-0"]')
    check("18 second click toggles back [x]->[ ]", "- [ ] ship it" in h("doc"), f"doc={h('doc')!r}")

    # --- Row 19: atomic caret motion skips the '# ' prefix as one unit -----
    h("reset")
    h("setCaret", 0)
    page.keyboard.press("ArrowRight")
    check("19 atomic ArrowRight skips '# ' to offset 2", h("caret") == 2, f"caret={h('caret')}")

    # --- Row 20: typing inside concealed bold content edits the model -----
    h("reset")
    bold = h("offsetOf", "**milk**")
    h("setCaret", bold + 3)  # between 'm' and 'i'
    page.keyboard.type("X")
    check("20 in-place edit inside bold: **mXilk**", "**mXilk**" in h("doc"), f"doc={h('doc')!r}")

    # ── live-editor GESTURES (probe deferred: "gestures not prototyped") ──
    run_gesture_assertions(page, h)

    # ── IME composition over the atomic decorations (delta #6) ──
    run_ime_assertions(page, h)

    # ── compose hide-mode + the per-editor toggle (2026-06-28 cross-surface convergence) ──
    run_compose_assertions(page)


def run_compose_assertions(page) -> None:
    # The conversations compose field's new default: inline markers HIDDEN (caret-edge reveal),
    # structural markers DIMMED (still visible, no chrome), no structural gestures (Enter ≠ split).
    def c(method: str, *args):
        arg_js = ",".join(json.dumps(a) for a in args)
        return page.evaluate(f"() => window.__compose.{method}({arg_js})")

    present = page.evaluate("() => window.__compose !== undefined")
    if not present:
        check("C0 compose editor mounted", False, "window.__compose missing")
        return
    check("C0 compose editor mounted", True)

    c("setMarkersShown", False)  # the default
    c("setCaret", 0)
    vis = c("visibleText")
    check("C1 compose: inline ** hidden by default", "**" not in vis and "milk" in vis, f"vis={vis!r}")
    check("C2 compose: inline ` hidden by default", "`" not in vis and "code" in vis, f"vis={vis!r}")
    check(
        "C3 compose: structural # and - stay DIMMED (visible, not hidden)",
        "# Heading" in vis and "- item" in vis,
        f"vis={vis!r}",
    )

    bold = c("offsetOf", "**milk**")
    c("setCaret", bold + 4)
    check("C4 compose: caret-edge reveal of inline **", "**milk**" in c("visibleText"), "")
    c("setCaret", 0)
    check("C5 compose: ** re-hidden when caret leaves", "**" not in c("visibleText"), "")

    c("setMarkersShown", True)
    check("C6 compose toggle → markers shown", "**milk**" in c("visibleText"), "")
    c("setMarkersShown", False)
    check("C6b compose toggle → markers re-hidden", "**" not in c("visibleText"), "")

    # No structural gesture leaked into compose: Enter inserts a plain newline (parent wires
    # Enter→send; the editor must NOT auto-continue/split the list the way Notes mode does).
    end = c("offsetOf", "- item") + len("- item")
    c("setCaret", end)
    page.keyboard.press("Enter")
    doc = c("doc")
    check(
        "C7 compose: Enter inserts plain newline (no list-continuation gesture)",
        doc.endswith("- item\n") and "- item\n- " not in doc,
        f"doc={doc!r}",
    )


def run_gesture_assertions(page, h) -> None:
    # G1 — Enter splits a paragraph at the caret (newline gesture).
    h("reset")
    at = h("offsetOf", "and run")
    h("setCaret", at)
    page.keyboard.press("Enter")
    doc = h("doc")
    check(
        "G1 Enter splits paragraph at caret",
        "Buy **milk** \nand run `code` now." in doc and doc.count("\n") == 6,
        f"doc={doc!r}",
    )

    # G2 — Backspace at content-start outdents a nested bullet (NOT a chunk-delete
    # of the atomic prefix; routes through backspace_at_start).
    h("reset")
    h("setCaret", h("offsetOf", "milk"))  # first 'milk' = the nested bullet (line 2)
    page.keyboard.press("Backspace")
    lines = h("doc").split("\n")
    check(
        "G2 Backspace-at-start outdents nested bullet '  - milk'->'- milk'",
        lines[2] == "- milk",
        f"line2={lines[2]!r}",
    )

    # G3 — Tab indents a list item under its previous sibling.
    h("reset")
    h("setCaret", h("offsetOf", "ship it"))
    page.keyboard.press("Tab")
    lines = h("doc").split("\n")
    check(
        "G3 Tab indents '- [ ] ship it' (gains leading indent)",
        lines[3].startswith(" ") and lines[3].lstrip().startswith("- [ ] ship it"),
        f"line3={lines[3]!r}",
    )

    # G4 — Shift-Tab outdents.
    h("reset")
    h("setCaret", h("offsetOf", "milk"))
    page.keyboard.press("Shift+Tab")
    lines = h("doc").split("\n")
    check("G4 Shift-Tab outdents '  - milk'->'- milk'", lines[2] == "- milk", f"line2={lines[2]!r}")

    # G5 — Enter inside a code block is a no-op gesture → CM inserts a literal \n.
    h("setDoc", "```\nlet x = 1;\n```", 8)  # caret before 'x'
    before = h("doc")
    page.keyboard.press("Enter")
    after = h("doc")
    check(
        "G5 Enter in code block inserts literal newline (no structural split)",
        after.count("\n") == before.count("\n") + 1 and "let \nx = 1;" in after,
        f"before={before!r} after={after!r}",
    )


def run_ime_assertions(page, h) -> None:
    # Delta #6 — IME / soft-keyboard composition over the atomic decorations. The
    # probe ran desktop Chromium without IME; CM6 supports IME on contenteditable.
    # Compose a CJK character inside the revealed bold run and assert it commits to
    # the model at the caret with the surrounding markers intact.
    h("reset")
    bold = h("offsetOf", "**milk**")
    page.click('css=[data-testid="notes-harness-content"]')
    h("setCaret", bold + 3)  # between 'm' and 'i'
    try:
        cdp = page.context.new_cdp_session(page)
        cdp.send("Input.imeSetComposition", {"text": "猫", "selectionStart": 1, "selectionEnd": 1})
        cdp.send("Input.insertText", {"text": "猫"})
        doc = h("doc")
        check(
            "IME composition commits inside bold run, markers intact",
            "**m猫ilk**" in doc,
            f"doc={doc!r}",
        )
    except Exception as e:  # noqa: BLE001
        check("IME composition commits inside bold run, markers intact", False, f"CDP IME error: {e}")


def main() -> int:
    if not (_BUILD_DIR / "harness.bundle.js").exists():
        print(
            "ERROR: build/harness.bundle.js missing — run `deno run -A build_harness.ts` "
            "(or `just notes-browser-harness`) first.",
            file=sys.stderr,
        )
        return 2

    headed = "--headed" in sys.argv

    httpd, port = _serve(_BUILD_DIR)
    url = f"http://127.0.0.1:{port}/harness.html"

    try:
        from playwright.sync_api import sync_playwright

        with sync_playwright() as pw:
            browser = pw.chromium.launch(**launch_kwargs(pw, headless=not headed))
            page = browser.new_page()
            page.goto(url)
            # Wait for the harness to boot (ensureWasm + mount) or report an error.
            page.wait_for_function(
                "() => window.__harness !== undefined || window.__harnessError !== undefined",
                timeout=30_000,
            )
            err = page.evaluate("() => window.__harnessError")
            if err:
                check("00 harness booted (ensureWasm + mount)", False, err)
            else:
                check("00 harness booted (ensureWasm + mount)", True)
                run_assertions(page)
            browser.close()
    finally:
        httpd.shutdown()

    passed = sum(1 for _, ok, _ in _results if ok)
    total = len(_results)
    fails = [n for n, ok, _ in _results if not ok]
    print(f"\n{passed}/{total} assertions passed.", flush=True)
    if fails:
        print("FAILED: " + ", ".join(fails), flush=True)
        return 1
    print("ALL GREEN — Notes WYSIWYG renders + gestures + IME on the production applier.", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
