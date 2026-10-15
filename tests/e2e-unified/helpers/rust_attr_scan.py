"""Text-analysis primitives for the pure-text Rust seam witnesses.

Shared by `test_shared_crate_seam_gating.py` (does every `*_for_test` seam sit
behind a safe gate?) and `test_apple_seam_gating.py` (which Swift-facing names do
the `test-helpers`-gated UniFFI exports carry?). Both need the same two answers
about Rust source — which bytes are code rather than comment or string, and where
the `#[...]` attributes are — so they live here once.

Pure text analysis, no build and no toolchain.
"""


def strip_strings_and_comments(text: str) -> str:
    """Blank out string/char literal and comment bodies, preserving every
    other byte (and all newlines) in place, so brace-depth tracking cannot be
    fooled by a `"{"` inside a doc comment or a string constant.
    Deliberately simple (no raw-string `r#"..."#` handling — none of this
    crate family uses raw strings for brace-bearing content), matching the
    scanning rigor of this project's other pure-text Rust scanners.
    """
    out = list(text)
    i = 0
    n = len(text)
    while i < n:
        two = text[i : i + 2]
        if two == "//":
            j = text.find("\n", i)
            j = n if j == -1 else j
            for k in range(i, j):
                out[k] = " "
            i = j
        elif two == "/*":
            j = text.find("*/", i + 2)
            j = n if j == -1 else j + 2
            for k in range(i, j):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        elif text[i] == '"':
            j = i + 1
            while j < n and text[j] != '"':
                if text[j] == "\\":
                    j += 1
                j += 1
            j = min(j + 1, n)
            for k in range(i, j):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        elif text[i] == "'" and i + 1 < n and (text[i + 1] == "\\" or (i + 2 < n and text[i + 2] == "'")):
            # A char literal, not a lifetime — the closing `'` follows within
            # 1 (escaped) or 2 (plain) chars. A lifetime (`'a`) never closes,
            # so it falls through untouched.
            j = i + 2 if text[i + 1] != "\\" else i + 3
            while j < n and text[j - 1] != "'":
                j += 1
            for k in range(i, min(j, n)):
                if out[k] != "\n":
                    out[k] = " "
            i = j
        else:
            i += 1
    return "".join(out)


def iter_attr_bounds(text: str) -> list[tuple[int, int]]:
    """`(start, end)` of every `#[...]` attribute in `text` — `start` at the
    `#`, `end` just past the closing `]` — by depth-balanced bracket matching.
    Run it over comment/string-stripped text (`strip_strings_and_comments`) when
    a `]` inside a string could otherwise unbalance the match."""
    bounds: list[tuple[int, int]] = []
    i = 0
    n = len(text)
    while True:
        start = text.find("#[", i)
        if start == -1:
            break
        depth = 1
        pos = start + 2
        while depth > 0 and pos < n:
            c = text[pos]
            if c == "[":
                depth += 1
            elif c == "]":
                depth -= 1
            pos += 1
        bounds.append((start, pos))
        i = pos
    return bounds


def extract_attr_spans(text: str) -> list[str]:
    """Every `#[...]` attribute in `text`, as its inner content (the bytes
    between the brackets), found by depth-balanced bracket matching. This is
    what lets a `#[cfg(all(\n    target_arch = "wasm32",\n    any(...)\n))]`
    attribute — the shape `cargo fmt` produces once a clause list is long
    enough to wrap — be recovered as ONE span rather than several unmatched
    fragments (measured: `fauna-client-atproto/src/genesis_verify.rs` and
    `fauna-client-alert-sweep/src/lib.rs` both wear exactly this wrapped
    shape and were false-positived as ungated before this fix, since no
    individual physical line of a wrapped attribute matches `#[cfg(...)]` on
    its own).
    """
    return [text[s + 2 : e - 1] for s, e in iter_attr_bounds(text)]
