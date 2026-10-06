"""Keep a sudo-gated test run from leaving root-owned build artifacts in the checkout.

The macOS installer suite's full-install legs need real `sudo`, so the whole
pytest process runs as root. Anything it *builds* along the way is therefore
written as root — and a root-owned file inside a developer checkout is not an
inconvenience, it is a one-way door for every unprivileged tool afterwards:
`cargo clean` dies with `EACCES` on the first one and stops, and the machine's
disk-reclaim sweeps skip what they cannot delete.

**What happened (2026-08-16).** The 2026-06-28 real-machine verification run
lazily triggered release builds as root — `fauna-nest-daemon`,
`fauna-bridge-supervisor` and `fauna-sync` under
`target/aarch64-apple-darwin/release/`, plus the host FFI under
`target/release/` — leaving ~3.8G of root-owned artifacts in the checkout.
Nothing noticed. They sat there for seven weeks on a disk-bound machine,
invisible in effect to every reclaim path, until a human found them and removed
them by hand with `sudo`.

Two mechanisms keep it from recurring, and this module holds the testable half
of each:

`refuse_root_build` — the *decision* behind the `pkg_path` fixture's refusal to
build a missing-or-stale `.pkg` while running as root (added 2026-06-28, the
same day as the incident run, which is why it did not prevent it). It was
inline in the fixture and so had no test of its own; it lives here to get one.

`root_owned_paths` — the *proof*. The refusal covers the build path we know
about; this walks the checkout afterwards and reports anything root-owned
regardless of which path created it, which is what makes it a regression guard
rather than a second copy of the same assumption.

Pure and platform-independent on purpose (`os.stat().st_uid` and a tree walk),
so the logic is tested on every machine rather than only under a real `sudo`.
Ownership itself is POSIX: on Windows `st_uid` is always 0, so the walk is
switched off there (`HAS_POSIX_OWNERSHIP`) and the tests fake the ownership.
"""
from __future__ import annotations

import os

#: Checkout-relative trees a root-run build can write into. `target/` is where
#: cargo puts everything; `apps/fauna-apple/.build` is SwiftPM's. (The apple FFI
#: cache under the invoking user's `~/.cache/fauna-apple-ffi` is at risk too,
#: but it is outside the checkout and outside this guard's reach — the failure
#: message names it so a human knows to look.)
GUARDED_TREES = ("target", os.path.join("apps", "fauna-apple", ".build"))

#: Whether `st_uid` means anything on this box. POSIX only: on Windows it is
#: always 0, which reads as "root-owned" for every file.
HAS_POSIX_OWNERSHIP = hasattr(os, "getuid")

#: Stop walking after this many offenders. The point is to fail with evidence,
#: not to enumerate 3.8G of files.
MAX_REPORTED = 20


def refuse_root_build(*, is_root: bool, cache_is_fresh: bool) -> bool:
    """Should the `.pkg` fixture refuse to build rather than build as root?

    True only when we are root **and** would actually have to build: a fresh
    cache means the artifact already exists (built earlier by the normal user,
    which is the intended flow), so a root run may use it happily.

    The refusal is deliberately not a "build it demoted for them" convenience.
    Dropping privilege mid-run would leave the build's own caches half-owned by
    two users, and the honest fix is a two-step flow the human controls: build
    the `.pkg` unprivileged, then `sudo pytest` the install legs.
    """
    return is_root and not cache_is_fresh


def root_owned_paths(repo_root: str, *, limit: int = MAX_REPORTED) -> list[str]:
    """Root-owned files under the checkout's build trees, up to `limit` of them.

    Returns paths relative to `repo_root`, sorted, so a failure message is
    stable and diffable. Symlinks are not followed — a cargo target dir is
    sometimes a link to a dataset elsewhere, and this guard is about what the
    checkout owns.
    """
    if not HAS_POSIX_OWNERSHIP:
        # Windows has no root and no POSIX owner: `os.lstat().st_uid` is always
        # 0 there, so walking would report EVERY file as root-owned. No sudo
        # run exists on such a box, so there is nothing for this guard to find.
        return []
    found: list[str] = []
    for tree in GUARDED_TREES:
        base = os.path.join(repo_root, tree)
        if not os.path.isdir(base):
            continue
        for dirpath, dirnames, filenames in os.walk(base, followlinks=False):
            for name in list(dirnames) + filenames:
                path = os.path.join(dirpath, name)
                try:
                    if os.lstat(path).st_uid == 0:
                        found.append(os.path.relpath(path, repo_root))
                except OSError:
                    continue  # vanished mid-walk (a live build elsewhere); not our business
                if len(found) >= limit:
                    return sorted(found)
    return sorted(found)


def root_droppings_message(repo_root: str, offenders: list[str], *, when: str) -> str:
    """The failure text — it must tell a human exactly how to get unstuck."""
    listing = "\n  ".join(offenders)
    more = " (and possibly more — the walk stops at the first %d)" % MAX_REPORTED
    return (
        f"root-owned build artifacts in the checkout {when}:\n  {listing}\n"
        f"{more if len(offenders) >= MAX_REPORTED else ''}\n"
        "A root-owned file inside the checkout blocks every unprivileged tool "
        "afterwards — `cargo clean` stops at the first EACCES, and the machine's "
        "disk sweeps skip what they cannot delete, so these do not go away on "
        "their own. Remove them, then re-run:\n"
        f"    sudo find {os.path.join(repo_root, 'target')} -user root -delete\n"
        "Also check the apple FFI cache, which is outside this guard's reach:\n"
        "    ls -ld ~/.cache/fauna-apple-ffi\n"
        "If this fired at the END of a run, something built as root during it — "
        "that is the bug to fix, not the files."
    )
