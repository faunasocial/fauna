#!/usr/bin/env python3
"""One-shot script to tag every tests/e2e-unified/tests/test_*.py with
a tier_1 / tier_2 / tier_3 / tier_4 pytest marker per the mocking-depth
taxonomy (see tests/e2e-unified/README.md).

After this lands, sessions adding new tests declare the tier directly
on the file (one line near the top). The --unmarked-strict conftest
hook (T1) enforces it.

Classification heuristic:
  - tier_1 = no driver, no nest, pure in-process Python
  - tier_2 = real driver + at least one stub (fakes/, set_*_snapshot,
              wiremock, fake DNS, mock bridge, etc.)
  - tier_3 = full stack — every binary real (locally-built)
  - tier_4 = full Docker deployment image + sidecars (tests/platform/docker/)
"""
from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TESTS_DIR = ROOT / "tests" / "e2e-unified" / "tests"

# Explicit per-file tier assignments. Files NOT in this map fall back to
# directory-default below. Default ambiguity is tier_3 per the taxonomy
# bias rule (bias toward the stronger full-stack guarantee).
TIER_1: set[str] = {
    "tests/test_scope_parser.py",
    "tests/test_fauna_ffi.py",
    "tests/test_driver_isolation.py",
    "tests/test_driver_state_api.py",
    # pty backend seam unit test — spawns a throwaway python helper on a pty, but
    # no nest, no client driver, no product stack (like the driver-* units above).
    "tests/test_tui_pty_backend.py",
    # Ephemeral-CA cert minting for the Mastodon interop harness — pure
    # `cryptography`, no nest, no driver, no product stack.
    "tests/test_ephemeral_ca.py",
}

TIER_2: set[str] = {
    # Onboarding wizard UI flows that inject state — no real nest
    "tests/test_handle_entry_outcomes.py",
    "tests/test_handle_first_back_buttons.py",
    "tests/test_invite_request_states.py",
    "tests/test_claim_code_unclaimed_nest.py",
    "tests/test_silent_challenge_unclaimed_nest.py",
    "tests/test_dns_config.py",
    "tests/test_dns_post_instructions.py",
    "tests/test_vps_config.py",
    "tests/test_encryption_mode_choice.py",
    "tests/test_provisioning_progress.py",
    # UI smoke/diag tests that don't reach nest
    "tests/test_ios_app_launch.py",
    "tests/test_ios_debug_elements.py",
    "tests/test_windows_state_diag.py",
    "tests/test_provider_registry_parity.py",
    # Real-app-binary + fake bridge HTTP server
    "tests/test_agent_standalone.py",
    "tests/test_macos_agent_standalone.py",
    "tests/test_sp_linux_smoke.py",
    "tests/test_sp_linux_ws_rpc_echo.py",
    "tests/test_sp_linux_ws_rpc_push.py",
    # Managed-DNS success paths: real nest serves the list_records matrix, but
    # the external DNS provider (verify/publish) is faked via FAUNA_DNS_PROVIDER_FAKE.
    "tests/test_admin_dns_managed.py",
    # Windows twin of `test_onboarding_dns_glue_app.py` (the onboarding→launch
    # DNS-credential seal): real nest + claim + config round-trip, external
    # DNS provider faked via FAUNA_DNS_PROVIDER_FAKE (the native twin). Gates the
    # windows OnboardingViewModel LoggedIn-arm seal glue.
    "tests/test_onboarding_dns_glue_windows.py",
    # SMTP API tests use fake_bridge_daemon + fake DNS — mixed stack
    "tests/api/test_smtp_actor_quota.py",
    "tests/api/test_smtp_auth_verdict_passthrough.py",
    "tests/api/test_smtp_fail_tempfail.py",
    "tests/api/test_smtp_inbound_hardening.py",
    "tests/api/test_smtp_policy_enforcement.py",
    "tests/api/test_smtp_received_header.py",
    # Pending-invite persistence injects wizard state via
    # set_invite_request_snapshot; the fixture chain spins a real nest
    # but the test never hits it. Same pattern as the onboarding-wizard
    # set above.
    "tests/web/test_pending_invite_persistence.py",
}

# Directory defaults — applied when a file isn't in the explicit maps
# above. tier_3 is the default-default per the taxonomy bias rule.
# Order matters: classify() returns the FIRST prefix match, so the more
# specific `tests/platform/docker/` must precede `tests/platform/`.
DIRECTORY_DEFAULT: dict[str, str] = {
    "tests/platform/docker/": "tier_4",  # real Docker image + sidecars (slowest)
    "tests/platform/fediverse/": "tier_3",  # real-fediverse interop harness: binary nest + a real third-party peer, Mastodon or GoToSocial (opt-in via `just e2e-fediverse-test` / `just e2e-gotosocial-test`, not the image)
    "tests/live/": "tier_4",  # provisions a real EXTERNAL cloud VPS — beyond tier_4, but tier_4 is the honest bucket
    "tests/api/": "tier_3",
    "tests/platform/": "tier_3",
    "tests/scenarios/": "tier_3",
    "tests/web/": "tier_3",
    "tests/": "tier_3",
}


def classify(rel_path: str) -> str:
    if rel_path in TIER_1:
        return "tier_1"
    if rel_path in TIER_2:
        return "tier_2"
    for prefix, tier in DIRECTORY_DEFAULT.items():
        if rel_path.startswith(prefix):
            return tier
    return "tier_3"


# Regex to find an existing pytestmark assignment.
# `_ANY` catches any RHS so we can wrap it as a list — handles
# `pytest.mark.NAME`, `pytest.mark.NAME(args)`, and `[...]` alike.
# Multi-line list-form pytestmarks (the RHS straddling multiple lines)
# are normalized to single-line before this runs.
_PYTESTMARK_LIST = re.compile(
    r"^pytestmark\s*=\s*\[\s*([^\]]*?)\s*\]\s*$", re.MULTILINE
)
_PYTESTMARK_ANY = re.compile(
    r"^pytestmark\s*=\s*(.+?)\s*$", re.MULTILINE
)


def _has_tier_marker(marker_body: str) -> bool:
    return any(t in marker_body for t in ("tier_1", "tier_2", "tier_3", "tier_4"))


def _split_top_commas(s: str) -> list[str]:
    """Split on commas not inside brackets/parens. Naive but sufficient
    for pytestmark RHS expressions in this codebase."""
    out: list[str] = []
    cur: list[str] = []
    depth = 0
    for ch in s:
        if ch in "([{":
            depth += 1
            cur.append(ch)
        elif ch in ")]}":
            depth -= 1
            cur.append(ch)
        elif ch == "," and depth == 0:
            piece = "".join(cur).strip()
            if piece:
                out.append(piece)
            cur = []
        else:
            cur.append(ch)
    last = "".join(cur).strip()
    if last:
        out.append(last)
    return out


def _insert_after_imports(src: str, line: str) -> str:
    """Insert ``line`` after the module docstring and import block.

    Strategy: skip the shebang (if any), then the module docstring (the
    first triple-quoted string), then a contiguous block of imports
    and blank lines. Insert the line with a leading and trailing blank
    line.
    """
    lines = src.splitlines(keepends=True)
    i = 0
    if i < len(lines) and lines[i].startswith("#!"):
        i += 1
    # Skip blank lines + comments
    while i < len(lines) and (lines[i].strip() == "" or lines[i].lstrip().startswith("#")):
        i += 1
    # Skip module docstring
    if i < len(lines):
        stripped = lines[i].lstrip()
        for quote in ('"""', "'''"):
            if stripped.startswith(quote):
                # find closing quote
                rest = stripped[len(quote):]
                if quote in rest:
                    i += 1
                    break
                i += 1
                while i < len(lines) and quote not in lines[i]:
                    i += 1
                if i < len(lines):
                    i += 1
                break
    # Skip imports + blank lines + comments.
    # Handle multi-line parenthesized imports and backslash continuations by
    # tracking paren depth and previous-line continuation.
    paren_depth = 0
    continuation = False
    while i < len(lines):
        cur = lines[i]
        s = cur.lstrip()
        if continuation or paren_depth > 0:
            # Inside a multi-line import — count parens, follow continuations
            paren_depth += cur.count("(") - cur.count(")")
            continuation = cur.rstrip().endswith("\\")
            i += 1
            continue
        if (
            s == ""
            or s.startswith("#")
            or s.startswith("from ")
            or s.startswith("import ")
            or s.startswith("from __future__")
        ):
            paren_depth += cur.count("(") - cur.count(")")
            continuation = cur.rstrip().endswith("\\")
            i += 1
            continue
        break
    # Step back over trailing blank lines so insertion sits right after imports
    while i > 0 and lines[i - 1].strip() == "":
        i -= 1
    insert = []
    if i > 0:
        insert.append("\n")
    insert.append(line)
    insert.append("\n")
    return "".join(lines[:i] + insert + lines[i:])


def _find_pytestmark_assignments(src: str) -> list[tuple[int, int, str]]:
    """Return [(start_offset, end_offset, rhs_expr), …] for every top-level
    ``pytestmark = …`` assignment. The end_offset is the position right
    after the trailing newline of the assignment (so removing
    ``src[start:end]`` cleanly drops the whole statement).

    RHS extraction walks forward from `=` until brackets/parens balance
    and we hit a newline, so multi-line list-form assignments are
    handled.
    """
    out: list[tuple[int, int, str]] = []
    pos = 0
    while True:
        m = re.search(r"^pytestmark\s*=\s*", src[pos:], re.MULTILINE)
        if not m:
            break
        start = pos + m.start()
        rhs_start = pos + m.end()
        # Walk RHS until balanced and newline reached.
        depth_paren = 0
        depth_bracket = 0
        depth_brace = 0
        in_str: str | None = None
        i = rhs_start
        while i < len(src):
            ch = src[i]
            if in_str:
                if ch == "\\":
                    i += 2
                    continue
                if ch == in_str:
                    in_str = None
                i += 1
                continue
            if ch == "#":
                # A `#` comment runs to end-of-line. Skip its body WITHOUT
                # consuming the newline, so the depth check below still sees
                # the line ending. Required because comments inside a
                # list-form `pytestmark` routinely contain an apostrophe
                # ("the Spam group's knob"); treating that as a string
                # delimiter desynchronised the scanner for the rest of the
                # file, the walk never balanced, and the caller concluded the
                # file had no `pytestmark` at all — then inserted a duplicate
                # bare one above the real list.
                while i < len(src) and src[i] != "\n":
                    i += 1
                continue
            if ch in ('"', "'"):
                in_str = ch
                i += 1
                continue
            if ch == "(":
                depth_paren += 1
            elif ch == ")":
                depth_paren -= 1
            elif ch == "[":
                depth_bracket += 1
            elif ch == "]":
                depth_bracket -= 1
            elif ch == "{":
                depth_brace += 1
            elif ch == "}":
                depth_brace -= 1
            elif ch == "\n" and depth_paren == 0 and depth_bracket == 0 and depth_brace == 0:
                # End of statement
                rhs_end = i
                end = i + 1  # include the newline
                rhs = src[rhs_start:rhs_end].strip()
                out.append((start, end, rhs))
                pos = end
                break
            i += 1
        else:
            # Ran off end of file without finding closing newline; skip.
            break
    return out


def _all_tests_have_decorator_tier(src: str) -> bool:
    """True when the file has at least one ``test_*`` function and *every* one
    of them already carries a ``@pytest.mark.tier_N`` decorator.

    Deliberately all-or-nothing: if only some are decorated, the undecorated
    ones still need coverage (conftest fails collection on a missing tier
    marker), and a module-level tier marker is additive — it tags the bare
    ones without pulling anything into an extra ``--client`` run, since only
    *client* markers affect selection that way.
    """
    try:
        tree = ast.parse(src)
    except SyntaxError:
        return False  # Unparseable — fall through to the normal insert path.

    tests = [
        node
        for node in ast.walk(tree)
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
        and node.name.startswith("test_")
    ]
    if not tests:
        return False
    return all(
        any("tier_" in ast.unparse(dec) for dec in node.decorator_list)
        for node in tests
    )


def tag_file(path: Path, tier: str) -> tuple[bool, str]:
    """Return (changed, message)."""
    src = path.read_text()
    marker = f"pytest.mark.{tier}"

    assignments = _find_pytestmark_assignments(src)
    if assignments:
        # Gather RHS expressions in source order.
        rhs_exprs: list[str] = []
        for _start, _end, rhs in assignments:
            # If RHS is a list `[a, b, c]`, unpack the contents.
            if rhs.startswith("[") and rhs.endswith("]"):
                inner = rhs[1:-1].strip().rstrip(",").rstrip()
                if inner:
                    rhs_exprs.extend(_split_top_commas(inner))
            else:
                rhs_exprs.append(rhs)
        if any(t in expr for expr in rhs_exprs for t in ("tier_1", "tier_2", "tier_3", "tier_4")):
            return False, f"already-tagged ({tier})"
        rhs_exprs.append(marker)
        replacement = f"pytestmark = [{', '.join(rhs_exprs)}]\n"
        # Replace the FIRST assignment in place; delete the rest.
        # Walk from the back so offsets don't shift while deleting.
        new_src = src
        for start, end, _rhs in reversed(assignments[1:]):
            new_src = new_src[:start] + new_src[end:]
        # Now replace the first assignment.
        first_start, first_end, _ = assignments[0]
        new_src = new_src[:first_start] + replacement + new_src[first_end:]
        path.write_text(new_src)
        return True, f"merged {len(assignments)} existing pytestmark(s), added ({tier})"

    # No module-level `pytestmark` — but the file may tag per-function with
    # `@pytest.mark.tier_N` decorators instead. Some files do that
    # deliberately: a module-level `pytestmark` is a UNION over every item in
    # the file, so a file mixing (say) a windows-only test with cross-client
    # ones must use decorators to keep the windows test out of every other
    # `--client` run. Inserting a module-level marker there contradicts the
    # file's own convention, so if every test function is already tier-tagged
    # by decorator, leave the file alone.
    if _all_tests_have_decorator_tier(src):
        return False, f"already-tagged by decorator ({tier})"

    # Insert one after imports.
    # Check for *module-level* `import pytest` / `from pytest …` only;
    # `import pytest` inside test bodies doesn't count.
    has_module_pytest = bool(
        re.search(r"^import pytest(\s|$)", src, re.MULTILINE)
        or re.search(r"^from pytest\b", src, re.MULTILINE)
    )
    if not has_module_pytest:
        line = f"import pytest\n\npytestmark = {marker}"
    else:
        line = f"pytestmark = {marker}"
    new_src = _insert_after_imports(src, line)
    path.write_text(new_src)
    return True, f"inserted new ({tier})"


def main() -> int:
    changed = 0
    skipped = 0
    by_tier: dict[str, int] = {"tier_1": 0, "tier_2": 0, "tier_3": 0, "tier_4": 0}

    # Find all test files. Skip __init__.py and conftest.py.
    test_files = sorted(
        p for p in TESTS_DIR.rglob("test_*.py") if p.is_file()
    )
    for p in test_files:
        rel = p.relative_to(ROOT / "tests" / "e2e-unified").as_posix()
        tier = classify(rel)
        by_tier[tier] += 1
        did, msg = tag_file(p, tier)
        if did:
            changed += 1
        else:
            skipped += 1
        print(f"  [{tier}] {rel} — {msg}")

    print()
    print(f"Done. {changed} files changed, {skipped} unchanged.")
    print(
        f"Distribution: tier_1={by_tier['tier_1']}  tier_2={by_tier['tier_2']}  "
        f"tier_3={by_tier['tier_3']}  tier_4={by_tier['tier_4']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
