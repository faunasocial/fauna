"""Every app's "Export My Data" asks the nest for the payload bytes.

`account-data-plane.md` § Nest-side requirements item 1, *Payload stores*
decision (5) rules that the full archive is the **default and only** shape an
app requests: `GET /api/v1/export` carries the user's content bytes only when
the caller sets `include_blobs=true`, and the nest's `ExportParams` declares
`#[serde(default)]` on that flag (`bins/fauna-nest/src/export_routes.rs:33`) —
so an app that omits it silently downloads an index-only archive. There is no
toggle: the flag is not a user choice, it is what makes the button's own
shipped promise ("Download a copy of all your data") true.

**Why a source assertion as well as a UI test.** The defect this guards is
invisible from the outside — an index-only archive is a well-formed zip that
downloads and opens fine. The observable that separates right from wrong is
*the URL the app requests*, which is exactly what this reads. It was written
while the button had no ui.yaml element ID; `settings-export-data-button` was
minted 2026-08-19 and `test_export_my_data_journey.py` now drives the button end
to end (`ui/settings.md` § Data export). This test stays: it is the per-app
uniformity guard (priority #1), one layer below that journey.

The failure it was written for was live in the tree on 2026-08-17: the nest leg
had landed and **all four** implementing apps still
requested the bare path, so every "Export My Data" on every app downloaded rows
and a manifest with none of the user's actual content.

Static-only — reads repository files, builds nothing, launches nothing
(tier_1), so it runs on every machine, not just the one that can compile the
app whose call site drifted.
"""
from __future__ import annotations

import os
import re
import subprocess

import pytest

pytestmark = pytest.mark.tier_1

# The account export is `/api/v1/export` exactly. The mail-export wizard's
# per-job download path is `/api/v1/export/<id>` — a different feature
# (`mail-export.md`) with its own scope controls — so anything followed by a
# path segment is deliberately not our business.
ACCOUNT_EXPORT_RE = re.compile(r"api/v1/export(?![/\w-])")

# The flag as it must appear in a request URL, and the shared Rust constant
# that carries it for callers that reach the path through `fauna-nest-http`.
FLAG = "include_blobs=true"
RUST_CONST = "EXPORT_FULL"

# Apps that implement the button today (`ui/settings.md` § Data export),
# discovered via the literal `include_blobs=true` query string. Listed so a
# silent *removal* of a working call site fails loudly; new apps are caught
# by the discovery half below.
EXPECTED_IMPLEMENTERS = {
    "apps/fauna-web/src/routes/settings/[[subpage]]/+page.svelte",
    "apps/fauna-android/app/src/main/java/com/fauna/app/core/ApiClient.kt",
    "apps/fauna-apple/FaunaKit/Sources/FaunaKit/Core/APIClient.swift",
    "apps/fauna-windows/FaunaApp/FaunaApp.Core/Services/DirectNestClient.cs",
}

# linux and tui reach the path through the shared constant rather than a
# literal (`fauna_nest_http::paths::account::EXPORT_FULL`), so they are
# invisible to the literal-string discovery above; linux is asserted by name
# instead. tui has no equivalent pin here — an existing gap, not part of this
# guard's scope.
LINUX_CALL_SITE = "apps/fauna-linux/src/client.rs"
SHARED_PATHS = "libs/fauna-nest-http/src/paths.rs"

SOURCE_SUFFIXES = (".rs", ".kt", ".swift", ".svelte", ".ts", ".cs", ".xaml")

# Generated mirrors and build output restate source we already check.
SKIP_DIRS = {
    "node_modules", "target", "build", ".build", "generated", "bin", "obj",
    "DerivedData", ".gradle", "pkg", "dist",
}


def _repo_root() -> str:
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    return os.path.normpath(result.stdout.strip())


def _app_sources(root: str):
    apps = os.path.join(root, "apps")
    for dirpath, dirnames, filenames in os.walk(apps):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for name in filenames:
            if name.endswith(SOURCE_SUFFIXES):
                full = os.path.join(dirpath, name)
                yield full, os.path.relpath(full, root).replace(os.sep, "/")


def _read(path: str) -> str:
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        return handle.read()


def _reaches_account_export(text: str) -> bool:
    return bool(ACCOUNT_EXPORT_RE.search(text))


def test_every_app_export_call_site_requests_the_payload_bytes():
    """No app may request the bare path — the archive would omit user content."""
    root = _repo_root()

    offenders = []
    found = set()
    for full, rel in _app_sources(root):
        text = _read(full)
        if not _reaches_account_export(text):
            continue
        found.add(rel)
        if FLAG not in text and RUST_CONST not in text:
            offenders.append(rel)

    assert not offenders, (
        "these app call sites reach GET /api/v1/export without "
        f"{FLAG!r}, so their 'Export My Data' downloads an index-only archive "
        "with none of the user's content bytes — see account-data-plane.md "
        "§ Nest-side requirements item 1, Payload stores decision (5):\n  "
        + "\n  ".join(sorted(offenders))
    )

    missing = EXPECTED_IMPLEMENTERS - found
    assert not missing, (
        "an app that implemented Export My Data no longer reaches the export "
        "path at all — if the button was deliberately removed, drop it from "
        "EXPECTED_IMPLEMENTERS and from ui/settings.md § Data export in the "
        "same change:\n  " + "\n  ".join(sorted(missing))
    )


def test_linux_reaches_the_export_through_the_flag_carrying_constant():
    """linux is the one app using the shared path constant — pin both halves."""
    root = _repo_root()

    paths_src = _read(os.path.join(root, SHARED_PATHS))
    assert RUST_CONST in paths_src, (
        f"{SHARED_PATHS} must define {RUST_CONST} — the one place the app-side "
        "export URL is written, so a reader sees the ruling next to the path"
    )
    const_line = next(
        line for line in paths_src.splitlines()
        if RUST_CONST in line and "=" in line
    )
    assert FLAG in const_line, (
        f"{RUST_CONST} is the full-archive URL and must carry {FLAG!r}; "
        f"found: {const_line.strip()}"
    )

    linux_src = _read(os.path.join(root, LINUX_CALL_SITE))
    assert RUST_CONST in linux_src, (
        f"{LINUX_CALL_SITE} must fetch {RUST_CONST}, not the bare EXPORT path"
    )


def test_the_nest_still_defaults_the_flag_off():
    """The premise. If this ever flips, the app-side flag stops being load-bearing.

    Not a wish — a guard on *why* the apps must send it. Were the nest to
    default `include_blobs` to true, this whole test file would be dead weight
    and should be reconsidered rather than left asserting a moot invariant.
    """
    root = _repo_root()
    src = _read(os.path.join(root, "bins/fauna-nest/src/export_routes.rs"))
    assert "#[serde(default)]" in src and "pub include_blobs: bool" in src, (
        "export_routes.rs no longer declares include_blobs as a defaulted "
        "bool — re-read account-data-plane.md § Payload stores and decide "
        "whether the app-side flag is still what carries the payload bytes"
    )
