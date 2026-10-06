"""Convention 15, the WEB half: no `window.__fauna_*` hook may be installed from
a production code path.

`docs/goal/architecture/e2e-conventions.md` point 15 — "test agents, automation
servers, and web's `window.__fauna_*` hooks exist only in dev/debug + explicit
test builds, verified absent from release artifacts". The native-FFI half is
pinned by `test_ffi_flavor_split.py`, the sync-agent IPC half by
`test_agent_ipc_seam_gating.py`, the apple call-site half by
`test_apple_seam_gating.py`. **The web half had no witness at all** until
2026-09-02, and the gap was not theoretical: a `grep -rl "__fauna_"
/usr/share/fauna-web` inside `ghcr.io/faunasocial/nest:latest` found
`__fauna_message_banner_mount_count` — installed bare, ungated, from
`$lib/components/MessageBanner.svelte`'s `onMount`, in every build. It is a
mount counter and carries no capability, which is exactly why nobody noticed;
the convention is a *surface* rule, and a surface is what it was.

**The two ways a web hook stays out of a shipped bundle**, both used in the tree
and both accepted here:

  (a) *A lexical guard.* `if (__FAUNA_E2E_AUTOMATION__) { … }` — a vite `define`
      constant that a production `vite build` folds to `false` and strips. This
      is the idiom for a hook that must live in a production module because it
      has to run at module load (`$lib/onboarding/machine.svelte.ts`) or inside a
      component lifecycle (`MessageBanner.svelte`, since this pin).
  (b) *An unreachable installer.* The write sits in an exported `install*`
      function whose only importer is `$lib/e2e-automation`, itself imported only
      from `+layout.svelte`'s `if (__FAUNA_E2E_AUTOMATION__)` branch — so Rollup
      tree-shakes the function out. `installRpcTestHooks` (`$lib/rpc`) and
      `installCommandHook` (`$lib/e2e-commands`) are this shape.

**Nothing here is hand-listed.** (b)'s membership is *derived* — the pin finds
each write's enclosing exported function and walks the real `import { … } from`
statements to see who can reach it — so an installer added tomorrow is covered
with no list to update, and one that acquires a production importer reds
immediately. That the derivation is sound is not merely argued: the same image
grep that found the leak found `__fauna_message_banner_mount_count` as the **only**
`__fauna_` name in the shipped SPA, so `__fauna_rpcEcho` and `__fauna_callCommand`
really are stripped by (b).

Pure text analysis of the web sources — no build, no browser, no driver.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_WEB_SRC = _REPO / "apps" / "fauna-web" / "src"

#: The automation module itself: its sole importer is `+layout.svelte`'s
#: `if (__FAUNA_E2E_AUTOMATION__) import('$lib/e2e-automation')` branch, so the
#: whole chunk is absent from a production build. Its own header states the rule
#: this pin enforces ("add new `window.__fauna_*` hooks here (or behind the same
#: flag), never bare in a component").
_AUTOMATION_HOME = "lib/e2e-automation.ts"

#: The vite `define` constant a production build folds to `false`
#: (`apps/fauna-web/vite.config.ts`, declared in `src/app.d.ts`).
_GUARD = "__FAUNA_E2E_AUTOMATION__"

#: An *installed* hook is an assignment. Reads of a `__fauna_*` global from
#: production code would be a separate smell, but only a write puts the name
#: into the bundle's surface, and only writes are what convention 15 is about.
#: `==`/`===`/`=>` are excluded so a comparison is not mistaken for an install.
_WRITE = re.compile(r"__fauna_[A-Za-z0-9_]*\s*=(?![=>])")

#: `export function name(` / `export async function name(` — the enclosing
#: installer, searched upward from a write.
_EXPORTED_FN = re.compile(r"^\s*export\s+(?:async\s+)?function\s+([A-Za-z0-9_]+)\s*\(")

#: A real `import { a, b } from '…'` statement, braces possibly spanning lines.
#: Bare mentions in prose do not count — `installCommandHook` is named in four
#: comments and imported in exactly one place, and conflating the two would make
#: this pin green for the wrong reason.
_IMPORT = re.compile(r"import\s*\{([^}]*)\}\s*from\s*['\"]([^'\"]+)['\"]", re.S)

#: The opening of a guard block: `if (… __FAUNA_E2E_AUTOMATION__ …) {`. The
#: block's extent is then brace-matched, because **file-level presence of the
#: token is not enough** — six components already carry a guard somewhere, so a
#: file-level test would have let the very leak this pin was written for through
#: had `MessageBanner.svelte` happened to gate anything else. The write itself
#: has to sit inside the block.
_GUARD_OPEN = re.compile(r"if\s*\([^)]*" + _GUARD + r"[^)]*\)\s*\{")


def _web_sources() -> list[Path]:
    return sorted(
        p for ext in ("*.ts", "*.svelte")
        for p in _WEB_SRC.rglob(ext)
    )


def _rel(p: Path) -> str:
    return p.relative_to(_WEB_SRC).as_posix()


def _guard_block_spans(text: str) -> list[tuple[int, int]]:
    """Character ranges of every `if (… __FAUNA_E2E_AUTOMATION__ …) { … }` body.

    Brace-matched rather than line-counted, so a nested object literal or an
    arrow body inside the guard does not end it early. Unbalanced input (a guard
    whose closing brace is missing, i.e. code that would not compile) yields a
    span running to end-of-file, which is the forgiving direction: this pin is
    not a parser and must not red on syntax it cannot model.
    """
    spans = []
    for m in _GUARD_OPEN.finditer(text):
        depth, i = 1, m.end()
        while i < len(text) and depth:
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
            i += 1
        spans.append((m.end(), i))
    return spans


def _is_gated_file(rel: str, text: str) -> bool:
    """File-level gating — the automation home, or a file carrying a guard.

    Deliberately the *coarse* rung, and used only for the importer walk in (b):
    "who can reach this installer" is a module-level question, and `+layout.svelte`
    reaches the automation module through a dynamic `import()` inside its guard,
    which no static-import scan can see anyway. The writes themselves are held to
    the block-precise standard in `_write_is_guarded`.
    """
    return rel == _AUTOMATION_HOME or _GUARD in text


def _write_is_guarded(text: str, offset: int) -> bool:
    """Is this write lexically inside a guard block?"""
    return any(start <= offset < end for start, end in _guard_block_spans(text))


def _enclosing_exported_fn(lines: list[str], idx: int) -> str | None:
    """The nearest `export function` at or above `lines[idx]`."""
    for i in range(idx, -1, -1):
        m = _EXPORTED_FN.match(lines[i])
        if m:
            return m.group(1)
    return None


def _importers_of(symbol: str, sources: dict[Path, str]) -> list[Path]:
    """Every web source with a real `import { … <symbol> … } from '…'`."""
    out = []
    for path, text in sources.items():
        for names, _module in _IMPORT.findall(text):
            if symbol in {n.strip().split(" as ")[0].strip() for n in names.split(",")}:
                out.append(path)
                break
    return out


def test_no_ungated_fauna_hook_is_installed_from_web_source():
    """Every `window.__fauna_*` install is behind the guard, in the automation
    module, or inside an installer only the automation module can reach.

    The failure message names the file, the hook and both remedies, because the
    fix is a judgement call: a hook a *production* surface genuinely needs is not
    a `__fauna_*` hook at all and wants a real name, while a test hook wants the
    guard.
    """
    sources = {p: p.read_text(encoding="utf-8") for p in _web_sources()}
    assert sources, f"no web sources found under {_WEB_SRC} — the pin would pass vacuously"

    violations: list[str] = []
    for path, text in sources.items():
        rel = _rel(path)
        if not _WRITE.search(text) or rel == _AUTOMATION_HOME:
            continue

        spans = _guard_block_spans(text)
        lines = text.splitlines()
        for idx, line in enumerate(lines):
            hit = _WRITE.search(line)
            if not hit:
                continue
            # Offset of this write in the whole file, for the span test.
            offset = sum(len(ln) + 1 for ln in lines[:idx]) + hit.start()
            if any(start <= offset < end for start, end in spans):
                continue
            fn = _enclosing_exported_fn(lines, idx)
            if fn is None:
                violations.append(
                    f"{rel}:{idx + 1}: {hit.group(0).strip()} — installed bare: "
                    f"not inside an `if ({_GUARD})` block, and no enclosing "
                    f"exported installer"
                )
                continue
            ungated = [
                _rel(p) for p in _importers_of(fn, sources)
                if not _is_gated_file(_rel(p), sources[p])
            ]
            if ungated:
                violations.append(
                    f"{rel}:{idx + 1}: {hit.group(0).strip()} — inside "
                    f"`{fn}()`, which is imported by ungated {ungated}"
                )

    assert not violations, (
        "a `window.__fauna_*` hook reaches a production web bundle "
        "(e2e-conventions.md point 15):\n  "
        + "\n  ".join(violations)
        + f"\n\nEither move the install into `$lib/{_AUTOMATION_HOME.split('/')[-1]}` "
        f"(or an installer only it imports), or wrap it in `if ({_GUARD}) {{ … }}` "
        "the way `$lib/onboarding/machine.svelte.ts` does for the hooks that must "
        "run at module load. If the value is genuinely wanted in production, it is "
        "not a test hook: give it a real name outside the `__fauna_` namespace."
    )


def test_the_derivation_can_actually_see_a_leak():
    """The pin's own red-verification, kept as a test rather than a claim.

    A gating pin that never fires is indistinguishable from one whose regex has
    rotted (this file's whole reason for existing is that the *absence* of a
    witness read as compliance for months). So: run the same predicate over a
    synthetic component with the exact shape the real leak had — a bare install
    inside a lifecycle callback, no guard, no exported installer — and require it
    to be caught.
    """
    leaked = (
        "<script lang=\"ts\">\n"
        "  onMount(() => {\n"
        "    (window as unknown as { __fauna_leak_probe?: number })"
        ".__fauna_leak_probe = 1;\n"
        "  });\n"
        "</script>\n"
    )
    lines = leaked.splitlines()
    hits = [i for i, line in enumerate(lines) if _WRITE.search(line)]
    assert hits, "the write regex no longer matches a bare `__fauna_x = 1` install"
    assert not _guard_block_spans(leaked), "an unguarded file must report no guard block"
    assert _enclosing_exported_fn(lines, hits[0]) is None, (
        "a lifecycle-callback install has no enclosing exported installer, so it "
        "must fall through to the bare-install violation"
    )

    # And the *decisive* half: the same write, in a file that carries a guard
    # somewhere ELSE, must still be caught. This is the case a file-level token
    # test gets wrong, and six components in the tree already have that shape.
    decoy = (
        "<script lang=\"ts\">\n"
        f"  if ({_GUARD}) {{\n"
        "    registerSomethingElse();\n"
        "  }\n"
        + leaked
    )
    spans = _guard_block_spans(decoy)
    assert spans, "the guard-block matcher stopped recognising `if (GUARD) { … }`"
    write = _WRITE.search(decoy)
    assert write is not None
    assert not any(s <= write.start() < e for s, e in spans), (
        "a write OUTSIDE the guard block must not be excused by a guard "
        "elsewhere in the same file"
    )


def test_a_comparison_is_not_read_as_an_install():
    """`(window.__fauna_x || 0) > 0` and `=== ` must not count.

    The e2e agent itself reads these globals (`web-bridge/agent.js`), and a
    predicate that flagged reads would push authors toward disabling the pin
    rather than fixing a leak.
    """
    for benign in (
        "const n = window.__fauna_message_banner_mount_count || 0;",
        "if (w.__fauna_probe === undefined) return;",
        "if (w.__fauna_probe == null) return;",
        "const f = () => w.__fauna_probe;",
    ):
        assert not _WRITE.search(benign), f"read/compare misread as an install: {benign}"
