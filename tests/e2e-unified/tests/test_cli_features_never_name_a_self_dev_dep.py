"""A `--features <pkg>/<feat>` flag must never name a crate with a self dev-dependency.

A crate that lists ITSELF as a dev-dependency (`fauna-nest = { path = ".",
features = ["test-helpers"] }` — the established shape that keeps a
release-profile `cargo test` compiling against the crate's own test support)
gives cargo two readings of `--features fauna-nest/activitypub` on a
`-p fauna-nest` invocation: the selected package's own feature, or the feature
of its DEPENDENCY named `fauna-nest`. Cargo takes the second. That edge is a
dev-dependency, which `cargo build` and `cargo tree -e no-dev` never activate,
so the feature lands nowhere — silently, with exit 0.

Measured 2026-10-04: from the day `bins/fauna-nest` gained its self dev-dep
(2026-09-02) every shipped nest image was built by the Dockerfile's
`--features fauna-nest/bluesky,fauna-nest/nostr,fauna-nest/activitypub` WITHOUT
any of the three — the staging box answered `/.well-known/nodeinfo`,
`/.well-known/nostr.json` and the Bluesky OAuth client-metadata route with the
landing page. The dev inner loop never saw it: its workspace feature
unification (`.cargo/config.toml`) folds the dev-dep edge back onto the package;
only the per-invocation (`selected`) resolution every artifact build uses drops
it. The bare form — `--features bluesky,nostr,activitypub`, applied to every
selected package that defines the feature — has no second reading.

tier_1: reads manifests and build files, runs no cargo.
"""

import re
import tomllib
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]

#: `--features X`, `--features=X`, `-F X`, optionally quoted (shell / Dockerfile / yaml).
_SHELL_FEATURES = re.compile(r"""(?:--features[= ]|-F )\s*["']?([A-Za-z0-9_./,:-]+)""")
#: Python argv list form: `"--features", "X"`.
_PY_FEATURES = re.compile(r"""["'](?:--features|-F)["']\s*,\s*f?["']([A-Za-z0-9_./,:{}-]+)["']""")


def _self_dev_dep_names() -> set[str]:
    """Dependency names under which a workspace crate lists itself as a dev-dep."""
    names: set[str] = set()
    for manifest in _REPO.glob("*/**/Cargo.toml"):
        parts = manifest.relative_to(_REPO).parts
        if parts[0] in ("target", "vendor", "node_modules") or "pkg" in parts:
            continue
        try:
            data = tomllib.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError, UnicodeDecodeError):
            continue
        tables = [data.get("dev-dependencies", {})]
        for target in data.get("target", {}).values():
            tables.append(target.get("dev-dependencies", {}))
        for table in tables:
            for key, spec in table.items():
                if isinstance(spec, dict) and "path" in spec:
                    if (manifest.parent / spec["path"]).resolve() == manifest.parent.resolve():
                        names.add(key)
    return names


def _scanned_files() -> list[Path]:
    """Every file that can carry a cargo invocation into an artifact or a gate."""
    out = [p for p in (_REPO / "Dockerfile", _REPO / "justfile") if p.is_file()]
    out += sorted(_REPO.glob("Dockerfile.*"))
    out += sorted((_REPO / ".github" / "workflows").glob("*.yml"))
    for root in (_REPO / "scripts", _REPO / "tools", _REPO / "tests" / "e2e-unified"):
        if root.is_dir():
            out += sorted(
                p for p in root.rglob("*") if p.is_file() and p.suffix in (".sh", ".py")
            )
    return out


def _feature_tokens(path: Path) -> list[tuple[int, str]]:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return []
    rx = _PY_FEATURES if path.suffix == ".py" else _SHELL_FEATURES
    hits: list[tuple[int, str]] = []
    for lineno, raw in enumerate(text.splitlines(), 1):
        if raw.lstrip().startswith("#"):
            continue
        for match in rx.finditer(raw):
            for token in match.group(1).split(","):
                if "/" in token:
                    hits.append((lineno, token.strip()))
    return hits


def test_the_manifest_scan_finds_the_known_self_dev_dep():
    """Positive control: the scan must see the crate that caused the incident,
    or every assertion below guards nothing."""
    assert "fauna-nest" in _self_dev_dep_names()


def test_the_file_scan_sees_pkg_slash_feature_tokens():
    """Positive control for the token parse: the justfile's workspace test
    gate names dependency features in `<pkg>/<feat>` form (legitimately — none
    of those crates lists itself)."""
    tokens = {t for _, t in _feature_tokens(_REPO / "justfile")}
    assert any(t.startswith("fauna-iroh/") for t in tokens), sorted(tokens)


def test_no_cli_feature_names_a_self_dev_dependency():
    banned = _self_dev_dep_names()
    offenders = []
    for path in _scanned_files():
        if path.name == Path(__file__).name:
            continue
        for lineno, token in _feature_tokens(path):
            if token.split("/", 1)[0] in banned:
                offenders.append(f"{path.relative_to(_REPO)}:{lineno}: {token}")
    assert not offenders, (
        "These `--features <pkg>/<feat>` flags name a crate that lists itself as a "
        "dev-dependency, so cargo binds them to that (inactive) dev-dep edge and the "
        "feature is silently dropped from every build. Use the bare feature name "
        "(`--features <feat>`) with `-p <pkg>` instead:\n  " + "\n  ".join(offenders)
    )
