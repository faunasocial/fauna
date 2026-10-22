"""The locally-built nest test image: its tag is keyed on the image's source
inputs, one build per key under a machine-wide lock, and old keys are collected.

An image only for tests is built locally, never dispatched (owner ruling
2026-10-19, ``docs/goal/architecture/image-pipeline.md`` § Image tags &
channels), and a later test may reuse it. Reuse is only safe when the tag says
WHAT was built: the helper this replaces kept one tag per machine,
``fauna-nest-test:local``, reused by presence alone whatever tree had built it,
with no lock and no build slot. Two sessions that both found it absent both
built, the last finisher owning the tag, and a test could silently run another
checkout's image — the 2026-10-10 DKIM tier_4 failures were exactly a stale
``:local``.

So the tag is ``fauna-nest-test:src-<key>``, the key a hash of what the build
context sends the Dockerfile: every git-tracked path not excluded by
``.dockerignore`` (the Dockerfile and ``.dockerignore`` among them) at its
working-tree content — the index blob for a clean path, the working file's own
hash for a modified one, a deletion marker for a deleted one — plus untracked,
not-gitignored files under the same filter. Same inputs, same tag, across every
checkout on the machine; different inputs never share one. Gitignored files are
outside the key: a build that read one (a stray generated file under ``libs/``)
would be keyed on the tree without it — the context is meant to be the tracked
tree, and ``.dockerignore`` is where a path the build must not see is declared.

**``FAUNA_BUILD_COMMIT`` names A commit with these inputs, not necessarily
yours.** The tag is shared, so the image carries the HEAD of whichever checkout
built it first; a second checkout with the same inputs at another commit reuses
it. Nothing reads it as more: the release-candidate gate
(``feature_ledger.release_candidate_verdict_for_image``) refuses every locally
built image outright — it has no registry digest — and an own-artifact ledger
record stamps the commit the image reports, which is one whose tree built
exactly this image.

Pure where the collection path needs it: ``local_image_tag`` runs git and never
docker, so ``_DockerProvider.image_ref`` can resolve the tag at collection time.
"""

from __future__ import annotations

import contextlib
import datetime
import fnmatch
import functools
import hashlib
import os
import subprocess
import sys
from pathlib import Path

#: The local repository every keyed tag lives under.
REPO = "fauna-nest-test"

#: Keyed tags are ``<REPO>:src-<key>``; GC touches only tags carrying the prefix,
#: so a hand-made tag under the same repository (an old ``:local`` retag) is the
#: human's, never collected.
KEYED_PREFIX = "src-"

#: Hex digits of the sha256 kept in the tag: 64 bits, collision-free in practice
#: for the handful of trees one machine ever builds.
KEY_HEX = 16

#: GC keeps the newest ``GC_KEEP_NEWEST`` keyed tags, and any younger than
#: ``GC_KEEP_DAYS`` days, whatever their rank. Constants, not knobs: a few trees
#: in flight at once (the checkouts a machine runs tier_4 from) each keep their
#: image, and a tag older than a couple of days is a tree nobody is testing.
GC_KEEP_NEWEST = 4
GC_KEEP_DAYS = 2

#: An image the run names INSTEAD of this tree's own build — a pulled
#: ``ghcr.io/faunasocial/nest:dev``, say, when an already-published build is what
#: is under test. Never built, and announced on use: running an image built from
#: another tree is allowed only when the run says so.
OVERRIDE_ENV = "FAUNA_NEST_TEST_IMAGE"

#: The tag ``image_ref`` answers when the key cannot be computed (no git): one no
#: build ever produces, so the presence check refuses it loudly at start.
UNKEYED_TAG = f"{REPO}:unkeyed"

#: Machine-wide: every checkout of the same user shares it, which is the point.
LOCK_DIR = Path.home() / ".cache" / "fauna" / "nest-test-image"


def dockerignore_rules(context: Path) -> list[tuple[bool, str]]:
    """``(negated, pattern)`` per ``.dockerignore`` line, in file order."""
    path = context / ".dockerignore"
    if not path.exists():
        return []
    rules = []
    for raw in path.read_text().splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        negated = line.startswith("!")
        if negated:
            line = line[1:].strip()
        line = line.strip("/")
        if line.startswith("./"):
            line = line[2:]
        if line:
            rules.append((negated, line))
    return rules


def is_ignored(path: str, rules: list[tuple[bool, str]]) -> bool:
    """Whether docker leaves ``path`` (repo-relative, ``/``-separated) out of the
    context: the last rule matching the path or any of its parent directories
    wins, as docker's own matcher does."""
    parts = path.split("/")
    prefixes = ["/".join(parts[:i]) for i in range(1, len(parts) + 1)]
    ignored = False
    for negated, pattern in rules:
        if any(fnmatch.fnmatchcase(p, pattern) for p in prefixes):
            ignored = not negated
    return ignored


def _git(context: Path, *args: str, stdin: str | None = None) -> str:
    return subprocess.run(
        ["git", "-C", str(context), *args],
        input=stdin, capture_output=True, text=True, check=True, timeout=120,
    ).stdout


def _split_z(out: str) -> list[str]:
    return [p for p in out.split("\0") if p]


def source_entries(context: Path) -> list[tuple[str, str, str]]:
    """``(path, mode, content id)`` for every file the build context sends,
    sorted by path — the key's whole input, exposed so a test can say why two
    trees differ."""
    rules = dockerignore_rules(context)
    entries: dict[str, tuple[str, str]] = {}
    for record in _split_z(_git(context, "ls-files", "-s", "-z")):
        meta, _, path = record.partition("\t")
        mode, blob, _stage = meta.split()
        if not is_ignored(path, rules):
            entries[path] = (mode, blob)
    modified = [p for p in _split_z(_git(context, "ls-files", "-m", "-z"))
                if p in entries]
    untracked = [p for p in _split_z(
        _git(context, "ls-files", "-o", "--exclude-standard", "-z"))
        if not is_ignored(p, rules)]
    dirty = modified + untracked
    present = [p for p in dirty if (context / p).is_file()]
    for p in dirty:
        if p not in present:
            entries[p] = ("deleted", "-")
    if present:
        hashes = _git(context, "hash-object", "--stdin-paths",
                      stdin="\n".join(present) + "\n").split()
        for p, h in zip(present, hashes):
            mode = entries.get(p, ("100644", ""))[0]
            entries[p] = (mode, h)
    return sorted((p, m, h) for p, (m, h) in entries.items())


def source_key(context: Path) -> str:
    """The hash of ``source_entries``: equal exactly when the inputs are."""
    digest = hashlib.sha256()
    for path, mode, blob in source_entries(context):
        digest.update(f"{path}\0{mode}\0{blob}\n".encode())
    return digest.hexdigest()[:KEY_HEX]


@functools.cache
def local_image_tag(context: Path) -> str:
    """This tree's keyed tag. Git only, never docker; cached per process — a run
    tests the tree it started on."""
    try:
        return f"{REPO}:{KEYED_PREFIX}{source_key(Path(context))}"
    except (OSError, subprocess.SubprocessError, ValueError):
        return UNKEYED_TAG


def override_image() -> str | None:
    """The image ``OVERRIDE_ENV`` names, or ``None``."""
    return os.environ.get(OVERRIDE_ENV, "").strip() or None


def image_tag(context: Path) -> str:
    """The image a tier_4 docker test boots: the run's named override, else this
    tree's keyed tag."""
    return override_image() or local_image_tag(context)


@contextlib.contextmanager
def build_lock(tag: str, lock_dir: Path | None = None):
    """Hold the machine-wide lock for ``tag``: a second caller blocks here until
    the first has built, then finds the tag present and reuses it."""
    directory = lock_dir or LOCK_DIR
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / (tag.replace("/", "_").replace(":", "_") + ".lock")
    try:
        import fcntl
    except ImportError:  # Windows: no tier_4 docker venue runs there
        yield path
        return
    with open(path, "w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield path
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def _parse_created(text: str) -> datetime.datetime | None:
    # `docker image ls`'s CreatedAt: "2026-10-20 07:37:45 +0000 UTC"
    try:
        return datetime.datetime.strptime(" ".join(text.split()[:3]),
                                          "%Y-%m-%d %H:%M:%S %z")
    except ValueError:
        return None


def collect_old_tags(keep: str, run=subprocess.run,
                     now: datetime.datetime | None = None) -> list[str]:
    """Untag keyed images past the GC window; returns the tags removed.

    Keeps ``keep`` (the tag just built or reused), the newest
    ``GC_KEEP_NEWEST`` keyed tags, any younger than ``GC_KEEP_DAYS`` days, and
    every tag a container — running or stopped — was started from. Removal is
    ``docker image rm`` without ``-f``, so docker itself refuses an image still
    in use. Never raises: GC is housekeeping, never a reason for a test to fail.
    """
    try:
        listed = run(["docker", "image", "ls", REPO, "--format",
                      "{{.Tag}}\t{{.CreatedAt}}"],
                     capture_output=True, text=True, timeout=60)
        used = run(["docker", "ps", "-a", "--format", "{{.Image}}"],
                   capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return []
    if listed.returncode != 0 or used.returncode != 0:
        return []
    in_use = set(used.stdout.split())
    now = now or datetime.datetime.now(datetime.timezone.utc)
    keyed = []
    for line in listed.stdout.splitlines():
        tag, _, created = line.partition("\t")
        if not tag.startswith(KEYED_PREFIX):
            continue
        keyed.append((_parse_created(created), f"{REPO}:{tag}"))
    oldest = datetime.datetime.min.replace(tzinfo=datetime.timezone.utc)
    keyed.sort(key=lambda e: e[0] or oldest, reverse=True)
    removed = []
    for rank, (created, ref) in enumerate(keyed):
        if (ref == keep or rank < GC_KEEP_NEWEST or ref in in_use
                or created is None
                or now - created < datetime.timedelta(days=GC_KEEP_DAYS)):
            continue
        try:
            done = run(["docker", "image", "rm", ref],
                       capture_output=True, text=True, timeout=120)
        except (OSError, subprocess.SubprocessError):
            continue
        if done.returncode == 0:
            removed.append(ref)
    if removed:
        print(f"[nest-test-image] collected {len(removed)} old keyed tag(s): "
              f"{', '.join(removed)}", file=sys.stderr)
    return removed
