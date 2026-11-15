"""Previous-build cache — materializes the *pinned previous release's* binaries.

The pinned-previous-build convention is owned by
`docs/goal/architecture/version-compatibility.md` § Dimension 6: the pin lives in
`tests/e2e-unified/pinned-previous-build.toml` and names the most recent production
release by `commit` (+ the immutable `nest_image` tag for that same commit). Pre-1.0
there are no client release artifacts, so **previous client binaries are rebuilt from
source at `commit`** and cached on the dev machine. This module is that cache.

It is the first consumer of the pin file.

**One client per machine.** The pinned *nest* builds anywhere; the pinned *client*
can only be built where that client's toolchain lives (§ Dim 6: "the apple and
windows legs follow the same pattern on their own machines"). So a call materializes
the nest plus exactly ONE client — `linux` on the Linux dev box, `macos` on the Mac,
`windows` on Windows — and `_default_client()` picks it from the platform. Pass `client=`
to override. `tui` is the one client every machine can build, so it is always such an
override (`just e2e-prev-build --client tui`), `ios` is the Mac's second client
(`--client ios`), and `web` the Linux box's (`--client web`: the pinned SPA tree, built by the
pinned `just web-test`). Each shares the cache with the machine's own client: one cache key, the
binaries side by side, and a nest already there is not built again.

**Any commit, not only the pin.** The cache is keyed on a commit, so a caller may name
one of its own choosing — the organisation drives against a kept box name the commit the
box's image was built from (`org-nests.md` § fauna.build → *Disposable*, rule (1);
`helpers/release_app.py` resolves it) — and may ask for the client alone
(`nest=False`, `--client-only`): those drives run no local nest. `reports-pass` is one
more cargo client in the table (`bins/fauna-reports-pass`, the headless reports pass), and
a client whose source directory the commit's tree does not have is refused before
anything is extracted (`client_absent_reason`). The tui's sync agent travels with it
(`CLIENT_COMPANIONS`): the tui resolves `fauna-sync-agent` beside itself, so a cached tui
without one would run agentless.

Layout (under a per-platform root — see `_tmp_root()`):

    <root>/fauna-prev-src/<commit12>/        the pinned source tree (git archive)
    <root>/fauna-prev-target/<commit12>/     scratch cargo target dir (deleted after build)
    <root>/fauna-prev-builds/<commit>/<profile>/{fauna-nest,fauna-desktop,fauna-tui,fauna-sync-agent,fauna-reports-pass,FaunaMacOS,windows-app/,ios-app/}

Only the *binaries* are cached; the multi-GB build dirs are torn down once they are
extracted (set FAUNA_PREV_KEEP_TARGET=1 to keep them for iteration). The cache key is
commit + profile, so it invalidates exactly when the pin moves.

The source tree is materialized with `git archive` (a plain extraction) rather than a
second checkout: nothing is registered with git, so the pinned tree cannot be swept by
the housekeeping that retires stale checkouts — and a tree pinned to an old release
commit is, by construction, always stale-looking. It needs no `.git`: the one consumer of
git metadata in this build is the nest's build-commit stamp (`libs/fauna-build-commit`:
the `commit!()` macro the nest's `main` expands, or `bins/fauna-nest/build.rs` in pins
older than 2026-10-26), which prefers the FAUNA_BUILD_COMMIT env var this module sets
explicitly.

**Profile: debug, deliberately.** The convention (§ Dim 6) prefers release and asks a
session choosing debug to record why. The reason here is fidelity, not speed: every HEAD
counterpart in the harness is a *debug* build — `tests/common/nest.py:build_node()` builds
`fauna-nest` debug, the linux driver launches `$CARGO_TARGET_DIR/debug/fauna-desktop`, and
the macOS driver launches `.build/debug/FaunaMacOS` (conftest
`APP_PATHS`). Pairing a release previous build against a debug HEAD build would confound
*profile* skew (`debug_assertions`, overflow checks, timing) with the *version* skew under
test. Debug on both sides isolates the version axis, which is the whole point of the grid.

Features match `build_node()` exactly (`test-hooks,nostr`) for the same reason: the
harness depends on `test-hooks` endpoints, and a feature delta would be a second variable.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


def _tmp_root() -> Path:
    """The large-temp root this machine parks multi-GB pinned artifacts under.

    NOT the per-branch build-cache dir: a full workspace build at the pin would eat the
    budget this branch's own HEAD builds need (on the Linux box that budget is a hard
    ZFS refquota; on the Mac it is simply a shared, and repeatedly ENOSPC-ing, disk).

    * **Linux dev box** — `/work/tmp`, the dedicated large-temp dataset.
    * **Mac** — there is no `/work/tmp` equivalent; no large-temp dataset is defined
      there (see `docs/goal/architecture/build-system.md`). `~/.cache` is where the apple
      toolchain already parks its multi-GB cross-checkout xcframework cache
      (`~/.cache/fauna-apple-ffi`), so it is the established home for exactly this class
      of artifact: it sits outside every checkout, so the housekeeping that retires stale
      checkouts (and destroys their build-cache datasets) can never sweep it, and it is
      shared across checkouts — which is right, because the cache key is the pinned
      commit, not the branch.
    * **win** — same story as the Mac: no dedicated large-temp dataset is defined there
      either (the shared `C:` drive is the whole budget on that machine), so this parks
      under the user's local-appdata cache dir, outside every checkout, for the same
      sweep-immunity reason.
    """
    override = os.environ.get("FAUNA_PREV_TMP_ROOT")
    if override:
        return Path(override)
    if sys.platform == "darwin":
        return Path.home() / ".cache" / "fauna-prev"
    if sys.platform == "win32":
        return Path.home() / "AppData" / "Local" / "fauna-prev"
    return Path("/work/tmp")


TMP_ROOT = _tmp_root()
SRC_ROOT = TMP_ROOT / "fauna-prev-src"
TARGET_ROOT = TMP_ROOT / "fauna-prev-target"
CACHE_ROOT = TMP_ROOT / "fauna-prev-builds"

# ---------------------------------------------------------------------------
# The keep rule — the previous-build cache's own retention (fleet-disk.md § The ladder,
# rung 0, and § Decisions of the design pass, 3)
# ---------------------------------------------------------------------------
#
# Every pin advance used to leave its predecessor's tree and binaries under the three
# roots for ever. The rule: keep the pinned commit and the newest PREV_KEEP_OTHERS other
# commits (by the newest write across their entries), never an entry written in the last
# PREV_ACTIVE_S (a build in flight). This module runs it at its own write (`build_prev`);
# the dev machines' temp-dir sweep calls `prev_cache_doomed` and takes exactly what it lists.

PREV_KEEP_OTHERS = 2
PREV_ACTIVE_S = 15 * 60
PREV_ROOT_NAMES = ("fauna-prev-src", "fauna-prev-target", "fauna-prev-builds")
_KEY_LEN = 12  # src and target are keyed by commit[:12], builds by the full commit


def prev_cache_doomed(tmp_root: Path, pin_commit: str, now: float | None = None,
                      also_keep: tuple[str, ...] = ()) -> list[Path]:
    """The cache entries the keep rule drops under `tmp_root`: every `<root>/<commit>`
    whose commit is neither the pin, nor one of `also_keep`, nor among the newest
    PREV_KEEP_OTHERS others, and that nothing wrote into in the last PREV_ACTIVE_S."""
    import time

    now = time.time() if now is None else now
    entries: dict[str, list[tuple[Path, float]]] = {}
    for root_name in PREV_ROOT_NAMES:
        root = tmp_root / root_name
        try:
            children = list(os.scandir(root))
        except OSError:
            continue
        for c in children:
            try:
                if not c.is_dir(follow_symlinks=False):
                    continue
                m = c.stat(follow_symlinks=False).st_mtime
            except OSError:
                continue
            entries.setdefault(c.name[:_KEY_LEN], []).append((Path(c.path), m))
    keep = {pin_commit[:_KEY_LEN], *(k[:_KEY_LEN] for k in also_keep)}
    others = sorted((k for k in entries if k not in keep),
                    key=lambda k: max(m for _, m in entries[k]), reverse=True)
    keep.update(others[:PREV_KEEP_OTHERS])
    doomed: list[Path] = []
    for key in others[PREV_KEEP_OTHERS:]:
        for path, m in entries[key]:
            if m < now - PREV_ACTIVE_S and not _written_since(path, now - PREV_ACTIVE_S):
                doomed.append(path)
    return doomed


def _written_since(path: Path, cutoff: float) -> bool:
    for dirpath, dirnames, filenames in os.walk(path, followlinks=False):
        for name in dirnames + filenames:
            try:
                if os.lstat(os.path.join(dirpath, name)).st_mtime >= cutoff:
                    return True
            except OSError:
                continue
    return False


def prune_prev_cache(tmp_root: Path | None = None, pin_commit: str | None = None,
                     also_keep: tuple[str, ...] = ()) -> list[Path]:
    """Run the keep rule: remove what `prev_cache_doomed` lists. Best effort."""
    tmp_root = TMP_ROOT if tmp_root is None else tmp_root
    try:
        pin_commit = pin_commit or prev_commit()
    except Exception:  # noqa: BLE001 — no readable pin: keep everything
        return []
    removed = []
    for path in prev_cache_doomed(tmp_root, pin_commit, also_keep=also_keep):
        shutil.rmtree(path, ignore_errors=True)
        if not path.exists():
            removed.append(path)
    if removed:
        print(f"[prev_build] keep rule removed {len(removed)} cache entries "
              f"(pin {pin_commit[:12]} + newest {PREV_KEEP_OTHERS})", flush=True)
    return removed

# The nest is built on every machine; the client is whichever one this machine can build.
# NEST[1] is the cargo-produced BINARY filename — `.exe` on Windows (cargo's own platform
# convention), bare elsewhere. Every other app entry in CLIENT_BINARY already spells
# its own extension explicitly; this one didn't need to until windows joined.
NEST = (
    "fauna-nest",
    "fauna-nest.exe" if sys.platform == "win32" else "fauna-nest",
    ["--features", "test-hooks,nostr"],
)

# client -> the binary name it produces (the cache-file name, and what the driver launches).
# windows is a subpath, not a bare filename: its MSBuild output is a whole directory of
# managed DLLs/resources + the native fauna_ffi.dll, not a standalone executable (see
# `_build_client` / `build_prev`'s windows handling) — the cached exe lives inside a
# `windows-app/` copy of that entire directory. tui is the one cargo binary that builds
# on windows too, so it carries the suffix NEST[1] does. ios is a bundle: `simctl install`
# takes the `.app` directory, which lives inside an `ios-app/` directory beside its stamp.
# web is no binary at all: its "app" is the built SPA tree (`apps/fauna-web/build`, what the
# `static_dir` fixture names at HEAD), served by the harness's SPA proxy over whichever nest
# a cell dials, so it is cached as a `web-app/` directory beside its stamp. Linux-only, as
# every web build (`build-system.md` § The Deno build sandbox).
CLIENT_BINARY = {
    "linux": "fauna-desktop",
    "tui": "fauna-tui.exe" if sys.platform == "win32" else "fauna-tui",
    "macos": "FaunaMacOS",
    "windows": "windows-app/FaunaApp.exe",
    "ios": "ios-app/FaunaiOS.app",
    "web": "web-app/build",
    "reports-pass": "fauna-reports-pass.exe" if sys.platform == "win32" else "fauna-reports-pass",
}

# The cargo package behind each cargo-built client. macOS, ios and windows are absent on
# purpose: the apple apps are SwiftPM products, windows is an MSBuild/WinUI project — none
# is a cargo binary (see `_build_client`).
CLIENT_PACKAGE = {
    "linux": "fauna-linux",
    "tui": "fauna-tui",
    "reports-pass": "fauna-reports-pass",
}

# client -> the source directory its build needs in the commit's tree. A commit older than
# the client has no such directory, and the build would fail deep inside cargo; asked
# first instead (`client_absent_reason`), so the refusal names the reason.
CLIENT_SOURCE_DIR = {
    "linux": "apps/fauna-linux",
    "tui": "apps/fauna-tui",
    "macos": "apps/fauna-apple",
    "ios": "apps/fauna-apple",
    "windows": "apps/fauna-windows",
    "web": "apps/fauna-web",
    "reports-pass": "bins/fauna-reports-pass",
}

# client -> (cargo package, binary) pairs built in the same cargo call and cached BESIDE
# the client. The tui resolves its sync agent beside itself
# (`agent_spawner::agent_binary_absolute`; the tui driver pins `FAUNA_E2E_SYNC_AGENT_BIN`
# to that sibling), exactly as `just tui-debug` builds the two together — a cached tui
# without its agent runs agentless, which reads as "connected but syncing nothing".
CLIENT_COMPANIONS = {
    "tui": (("fauna-sync-agent",
             "fauna-sync-agent.exe" if sys.platform == "win32" else "fauna-sync-agent"),),
}


def _default_client() -> str:
    """The client this machine can build the pinned binary for."""
    if sys.platform == "darwin":
        return "macos"
    if sys.platform == "win32":
        return "windows"
    return "linux"


def repo_root() -> Path:
    """The repo root of *this* checkout (the HEAD side)."""
    return Path(__file__).resolve().parents[3]


def pin_path() -> Path:
    return repo_root() / "tests" / "e2e-unified" / "pinned-previous-build.toml"


def load_pin() -> dict:
    """Parse the pin file. Keys: commit, nest_image, released."""
    with open(pin_path(), "rb") as fh:
        pin = tomllib.load(fh)
    for key in ("commit", "nest_image", "released"):
        if key not in pin:
            raise RuntimeError(f"{pin_path()} is missing required key '{key}'")
    return pin


def prev_commit() -> str:
    return load_pin()["commit"]


# ---------------------------------------------------------------------------
# Ratified in-place breaks — the grid's EXPECTED-INCOMPATIBLE windows
# ---------------------------------------------------------------------------
#
# `version-compatibility.md` § Dimension 2 records the user-ratified **in-place** compat
# breaks — each a one-time write-off against I2/I4 with no dual-serving remedy owed, each
# with the same stated consequence for § Dimension 6: *until the pin advances past the
# release carrying the break, the grid's cross-version cells are expected-incompatible*.
# This table is that consequence made mechanical. A cross-version cell carries a strict
# xfail (`helpers/skew_client.py::expected_incompatible`) whose condition is exactly
# "the pin PREDATES an entry that covers this cell" — `git merge-base --is-ancestor` over
# the pinned commit — so:
#
#   * the window declares itself: the XFAIL reason names the pin, the break and the
#     owning § Dim 2 entry, instead of a raw `fauna.auth.signature_failed` that reads as
#     a fresh compat alarm (which is how one was filed on 2026-08-27);
#   * it clears itself: the moment a promoted release moves the pin past the break the
#     condition is False at import, no mark is applied, and the cell asserts again —
#     no hand edit on advance, unlike the hand-listed capability tuples in
#     `skew_client.py` (`PREV_BUILD_SUPPORTS_HOME_ISOLATION` sat un-flipped across three
#     pin advances);
#   * it still WITNESSES the break on every run (`run=True`): a cell that PASSES while
#     the pin predates a break covering it is an XPASS → red under `strict=True`, which
#     is precisely the signal for the one regression rule #8 forbids — an untagged accept
#     path quietly re-added to a verifier (`key-material-hierarchy.md` § Architectural
#     rules #8, the tagged-only actor key).
#
# **Append-only.** An entry is never removed: the ancestry check is what decides whether
# it is still pending, and history is what makes that decision correct. A new in-place
# break lands its entry in the SAME commit as its § Dim 2 ratification, naming the cells
# it is expected to fail — measured where a measurement exists, by construction where
# the mechanism leaves no other reading (say which in `what`).

#: The cells of the real-binary grid, by the DIRECTION of skew each exercises — a break
#: names the cells it is expected to fail, because one direction can break without the
#: other (the folders rename did). Five are journey cells; the sixth is the previous-nest
#: build-identity guard, whose only skew is the HEAD *harness* claiming admin on the
#: pinned nest (`common.nest.start_nest` → `claim_admin`, a signed actor-key ceremony) in
#: the module fixture it shares with cell 1 — a break that reaches the harness's own
#: signer, as the sig_domain reset does, takes that fixture down and with it the guard.
#: The mixed-nests cell is cell 1's direction (HEAD app ↔ pinned nest) inside one install
#: that also holds an account on a HEAD nest, so it stands in every window cell 1 does and
#: in none that names only the pinned-client cells.
CELL_HEAD_CLIENT_PREV_NEST = "head_client_prev_nest"   # skew cell 1: HEAD app ↔ pinned nest
CELL_PREV_CLIENT_HEAD_NEST = "prev_client_head_nest"   # skew cell 2: pinned app ↔ HEAD nest
CELL_HEAD_CLIENT_MIXED_NESTS = "head_client_mixed_nests"  # one HEAD app, accounts on both nests
CELL_AT_REST_UPGRADE = "at_rest_upgrade"               # pinned app WRITES against a HEAD nest
CELL_AT_REST_DOWNGRADE = "at_rest_downgrade"           # pinned app OPENS (logs in to a HEAD nest)
CELL_HARNESS_CLAIMS_PREV_NEST = "harness_claims_prev_nest"  # the guard: HEAD harness claims the pinned nest
GRID_CELLS = (
    CELL_HEAD_CLIENT_PREV_NEST,
    CELL_PREV_CLIENT_HEAD_NEST,
    CELL_HEAD_CLIENT_MIXED_NESTS,
    CELL_AT_REST_UPGRADE,
    CELL_AT_REST_DOWNGRADE,
    CELL_HARNESS_CLAIMS_PREV_NEST,
)

_ALL_CELLS = frozenset(GRID_CELLS)

#: A cell OUTSIDE the grid that the same windows govern: the release pipeline's upgrade
#: smoke (run by `build-nest-image.yml` before anything
#: is pushed), which boots the candidate image on a `/data` the pinned previous release
#: wrote. A break that changes what a nest can open at rest — the nest schema's genesis —
#: fails it while the pin predates the break, and nothing in the grid, whose HEAD nest
#: always opens a data dir of its own, would see it.
CELL_IMAGE_UPGRADE_SMOKE = "image_upgrade_smoke"
PIPELINE_CELLS = (CELL_IMAGE_UPGRADE_SMOKE,)

#: The nest UPGRADED IN PLACE (`tests/test_nest_data_upgrade.py`): the pinned nest binary
#: is seeded through the HEAD harness, stopped, and the HEAD nest binary opens the SAME
#: data dir. It is the one tier_3 cell that crosses an at-rest boundary, so a nest-DB
#: genesis reaches it as it reaches the image smoke, and every wire break the HEAD
#: harness's seeding meets at the pinned nest reaches it as it reaches the journeys.
#: Outside `GRID_CELLS` because it is no skew-grid cell: no app, no fixture shared with
#: the grid. **A future at-rest break inside the 0.1.x window** (`version-compatibility.md`
#: § Dimension 2, the fifth exception) turns it red until the pin passes the break — the
#: honest signal; the break's entry here names this cell. Its image twin
#: (`tests/platform/docker/test_nest_image_upgrade.py`: the pinned release's IMAGE seeded
#: by the same harness, a later image on the same volume) is the same hop and carries the
#: same cell.
CELL_NEST_DATA_UPGRADE = "nest_data_upgrade"
UPGRADE_CELLS = (CELL_NEST_DATA_UPGRADE,)
_UPGRADE = frozenset(UPGRADE_CELLS)

#: Every cell a break may name.
KNOWN_CELLS = _ALL_CELLS | frozenset(PIPELINE_CELLS) | _UPGRADE


@dataclass(frozen=True)
class InplaceBreak:
    """One user-ratified in-place compat break (a § Dimension 2 "ratified exception")."""

    commit: str          # full 40-hex SHA of the commit that changed the bytes
    landed: str          # ISO date it landed on origin/main
    what: str            # one line: what changed, which ceremony fails, how it was established
    cells: frozenset     # the grid cells it is expected to fail while the pin predates it

    def reason(self, pin_commit: str) -> str:
        """The XFAIL reason — everything a reader needs to find the ruling without a grep."""
        return (
            f"EXPECTED-INCOMPATIBLE: the pin {pin_commit[:10]} predates the ratified in-place "
            f"break {self.commit[:10]} ({self.landed}: {self.what}) — "
            f"version-compatibility.md § Dimension 2; clears by the pin-update rule once a "
            f"production release promoted past it moves the pin"
        )


#: Adding a break here also needs its 40-hex `commit` allowlisted in the
#: publish gate's approved-secret-fixtures allowlist — a bare high-entropy hex
#: run is default-denied by the publish-hygiene gate's `secret_scan` until reviewed.
RATIFIED_INPLACE_BREAKS: tuple[InplaceBreak, ...] = (
    InplaceBreak(
        commit="3c8f6f7685806a28e1d22aa2cae6f1b2e72ef07f",
        landed="2026-08-13",
        what=(
            "folders rename — ~20 kinds and ~90 payload fields renamed in place with no alias; "
            "`SyncBackupStatusReply.file_sets` → `.folders` sits on the post-login state "
            "population every native app runs, so a PRE-rename client fails at login against "
            "a post-rename nest (measured 2026-08-15: skew cell 2 + the "
            "at-rest upgrade cell red, cell 1 + the downgrade cell green)"
        ),
        # Only the cells where the PINNED client is the one decoding the renamed reply. The
        # downgrade cell logs the pinned build in (auth is untouched by the rename) and asserts
        # only destroy-nothing, which the measurement confirmed still passes. The nest upgrade
        # cell's HEAD harness seeds a folder through the renamed kinds, which a pre-rename
        # pinned nest does not know — by construction.
        cells=frozenset({CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE}) | _UPGRADE,
    ),
    InplaceBreak(
        commit="08a638699aa42908a7c13cccd7c15dad038a31c3",
        landed="2026-08-17",
        what=(
            "actor-key sig_domain reset — every actor-key ceremony (login handshake, "
            "challenge-verify, registration, …) signs domain-tagged bytes and the untagged "
            "accept paths are deleted, so a pre-reset peer fails the FIRST handshake with "
            "fauna.auth.signature_failed in BOTH directions (head→prev measured 2026-08-27 "
            "against the 2026-08-16 image; prev→head by construction — the pinned build "
            "signs untagged; witnessed on the linux leg 2026-08-27: every cell, the guard's "
            "claim_admin fixture included, refused at its first ceremony)"
        ),
        cells=_ALL_CELLS | _UPGRADE,
    ),
    InplaceBreak(
        commit="bdbdd8f28627ed2cbe4f4669bf8350c391ae8e49",
        landed="2026-09-24",
        what=(
            "login nest binding — every bearer-minting signature (handshake, verify, device "
            "and custody handshakes) names the receiving nest, read off the connection first; "
            "`nest_id` is a required request field and the unbound `.v1` forms were retired "
            "with no accept path (user-ruled, no real users yet), so a pre-binding peer fails "
            "its FIRST ceremony in BOTH directions: a pinned client sends no `nest_id` "
            "(fauna.protocol.malformed at a HEAD nest) and a HEAD client's bound signature "
            "does not verify at a pinned nest (fauna.auth.signature_failed) — by construction, "
            "the same shape as the sig_domain reset above"
        ),
        cells=_ALL_CELLS | _UPGRADE,
    ),
    InplaceBreak(
        commit="22ed97a03c5bfb0547014132de3c60cabad16c1f",
        landed="2026-09-24",
        what=(
            "compat-remnant sweep, tranche 1 (the auth/trust surface) — ONE ratification for "
            "the whole sweep (user-ruled, no real users yet): the untagged `CertBinding.sig` "
            "and the verifier's fallback to it are gone (`tagged_sig` required), NAT-mode V2 "
            "is the only form (`nest_id` required, no V1 verify arm, no advert), `expires_in` "
            "is required on every bearer mint, and the pre-Track-2 first-contact fallback "
            "hard-fails; later tranches (the storage-mode contract, the dead-registered "
            "kinds, the wire-payload remnants) ride the same ratification. A pinned client "
            "fails its FIRST ceremony in BOTH directions — a pinned nest signs the channel "
            "binding untagged (a HEAD client refuses it: `BindingRequired`) and a pinned "
            "client's unbound NAT-mode / expires_in-less shapes are refused at a HEAD nest "
            "(fauna.protocol.malformed) — by construction, the same shape as the two above"
        ),
        cells=_ALL_CELLS | _UPGRADE,
    ),
    InplaceBreak(
        commit="c346e33bdad8702713ff8b09522c2e264ed2e5d8",
        landed="2026-09-24",
        what=(
            "compat-remnant sweep, program 2 (the ordinary wire-payload remnants) — the "
            "same one ratification, dated by its own earliest landed commit so a pin "
            "that lands between tranche 1 and program 2 still reads the window: lockout "
            "`duration_secs`, the Nostr DM plaintext ballast, `require_registration` + "
            "nest.info's registration booleans, `dsn_recipient`, "
            "`SnapshotCheckReply.errors` and the succession push `chain` left the wire, "
            "then (same day) media.list's v1 order (`cursor_version` required), the "
            "key-only keyset cursors and the clients' unknown_kind degrade arms. A pinned "
            "peer's older shapes are refused or ignored at a HEAD peer — by construction, "
            "every cell"
        ),
        cells=_ALL_CELLS | _UPGRADE,
    ),
    InplaceBreak(
        commit="5d0da014547bf9f341a99862f59c891fa027352e",
        landed="2026-09-25",
        what=(
            "compat-remnant sweep, program 4 tranche C1 (the nest DB genesis) — the same "
            "one ratification: the 78-step migration history collapsed into a genesis, the "
            "schema-version numbering continued past the retired history with the reader "
            "floor raised to the genesis, and a database the genesis did not write refused "
            "at open (no `application_id` mark). A pinned nest's `/data` "
            "therefore does not boot under a HEAD image — by construction; the grid's "
            "cells are untouched, since every HEAD nest there opens a data dir of its own"
        ),
        cells=frozenset({CELL_IMAGE_UPGRADE_SMOKE}) | _UPGRADE,
    ),
    # ── The breaks after the genesis (backfilled 2026-10-02) ─────────────────────────
    # One entry per `ratified-breaks.txt` block or § Dimension 2 rider that reaches a cell,
    # keyed on that break's LAST landed commit, not its first: `origin/main` only ever
    # fast-forwards, so a pin that includes the last commit includes the whole break,
    # while a first-commit key would clear the window for a pin cut midway through a
    # multi-commit break and leave its later commits as plain reds. Every cell set
    # below is reasoned by construction, never measured — no promoted release sits
    # between the genesis and these breaks to measure against — so a pin that lands
    # inside one of these windows is the first measurement. Where a cell was in
    # doubt it is claimed: a wrong claim XPASSes red NAMING its break, while a
    # missing one is the anonymous red this table exists to end. No entry here
    # names the build-identity guard: its fixture's HEAD-harness ceremony
    # (`claim_admin` + `fauna.auth.nest_handshake`) signs and sends hex strings and
    # pre-flip byte strings only, and `start_nest` reads the nest id off the data dir
    # rather than calling the renamed discovery kind.
    #
    # Ratified in the same window but reaching no cell, so they carry no entry (an
    # entry must name a cell): the pending-reseal sentinel, the sub-tranche 3
    # "pre-expand nest" arms, the tamper-evidence chains' tranche C6, the mail
    # `stored_at` tranche C5, the typed feed rules and the per-writer roster cell's
    # `group_readmissions` (2026-09-25 to 2026-09-27: at-rest `__config` fields an
    # older blob carries through `extra`, and mail, admin, snapshot and custom-feed
    # shapes no grid journey touches); the ATProto-naming leg (2026-09-28: read
    # both, write new, so a HEAD build reads a pinned build's blob, and the declared
    # re-seal residual needs the pinned build to re-seal a HEAD-written
    # `UserConfig`, which no cell does); the credential secrets' byte-string re-cut
    # (2026-09-30: atproto/mail credential rows, outside every journey); the
    # 2026-10-01 survey's dead and reserved shapes, the algorithm-service mesh,
    # its admin flag, the dead `dkim_key_blob_id` column and the create-time path
    # pair (2026-10-01/02: never-called kinds, a nest-internal sidecar, admin and
    # mail rows, fields no writer ever sent); the `RuleTier` spelling (2026-10-02:
    # a gate refusal's detail); the mail-spam model's seal (2026-10-02: mail
    # bridge state); and the three ratified retypes (2026-10-02: muted keywords,
    # the mail-domain overrides, and `notif_type`, whose wire string is unchanged).
    InplaceBreak(
        commit="3f91dedbd3270a8364d0ae2a60bdba11cf936487",
        landed="2026-09-29",
        what=(
            "folders mode contraction (the fourth exception's ratification) — `mode` left "
            "the folder and backup-destination wire shapes in place, so a pinned client "
            "decoding a HEAD reply that lacks the field it still requires fails — by "
            "construction, the 2026-08-13 rename's measured shape (the pinned-client cells)"
        ),
        cells=frozenset({CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE}),
    ),
    InplaceBreak(
        commit="e32b3f8f6d2c8ce50fbdbece7c231d37351b0ec0",
        landed="2026-09-29",
        what=(
            "fixed-width ids as CBOR byte strings (same ratification) — every `[u8; N]` on "
            "the wire, in a signed payload, in a plane value and at rest moved from an "
            "integer array to a byte string, and each strict decoder refuses the other "
            "spelling; the journeys' account plane, MLS and DM shapes carry such ids, so a "
            "pinned peer's shapes are refused in BOTH directions, at rest too — by "
            "construction, every journey cell"
        ),
        cells=(_ALL_CELLS - {CELL_HARNESS_CLAIMS_PREV_NEST}) | _UPGRADE,
    ),
    InplaceBreak(
        commit="e1696ddd34e49ae840200c523631d11339fed336",
        landed="2026-09-29",
        what=(
            "folders role contraction (same ratification) — `role` and `source_device_id` "
            "left the folder-member and sync-status shapes and `flags` went required, so "
            "a pinned client decoding a HEAD reply fails as the mode contraction's does — "
            "by construction (the pinned-client cells)"
        ),
        cells=frozenset({CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE}),
    ),
    InplaceBreak(
        commit="dac4cd64eb9da6c59b9689c1156fcc630a453be2",
        landed="2026-09-29",
        what=(
            "`fauna.sync.device.adopt` removed (the § Dimension 2 cutover rider, user-ratified "
            "2026-09-29; `ratified-breaks.txt` keys nothing for it) — a pinned app hosting "
            "its sync agent calls the kind and meets fauna.protocol.unknown_kind at a HEAD "
            "nest; by construction, unmeasured whether each journey depends on the adoption "
            "(the pinned-client cells)"
        ),
        cells=frozenset({
            CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE, CELL_AT_REST_DOWNGRADE}),
    ),
    InplaceBreak(
        commit="fc6121ec3079716af695da6c0817063df02479c7",
        landed="2026-10-01",
        what=(
            "the early audit's wire-field contractions (A1–A20, user-approved 2026-09-30; "
            "keyed on the last of the batch's four commits) — fields left HEAD replies the "
            "pinned client still requires (`SyncStatusReply.destinations`, "
            "`ModerationInfo.client_scanning_enabled`, the folders' `rescan_interval_secs`) "
            "and optional fields went required — by construction (the pinned-client cells)"
        ),
        cells=frozenset({CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE}),
    ),
    InplaceBreak(
        commit="bfa1ab06988949221dc8ee2ba6931ff69e19a58b",
        landed="2026-10-01",
        what=(
            "variable-length byte fields as CBOR byte strings (baseline survey A2, "
            "user-accepted 2026-10-01) — every `Vec<u8>` on the wire, in a signed payload, "
            "in a plane value and at rest moved from an integer array to a byte string with "
            "no read-fallback; the DM leg's MLS payloads and the stores the at-rest cells "
            "reopen are such fields — by construction, every journey cell"
        ),
        cells=(_ALL_CELLS - {CELL_HARNESS_CLAIMS_PREV_NEST}) | _UPGRADE,
    ),
    InplaceBreak(
        commit="1ed041e891d06f7c5cee3214501afc6834d06a0d",
        landed="2026-10-02",
        what=(
            "`Cid` as an IPLD tag-42 link (user-ruled 2026-10-01) — every content address on "
            "the wire and at rest moved from a bare byte string to tag 42, and the strict "
            "decoder refuses the old spelling, so a feed post and every stored record a cell "
            "reopens fail in BOTH directions — by construction, every journey cell"
        ),
        cells=(_ALL_CELLS - {CELL_HARNESS_CLAIMS_PREV_NEST}) | _UPGRADE,
    ),
    InplaceBreak(
        commit="c21c75d65f11f0afe5b38f28b170ab68a1c27f01",
        landed="2026-10-02",
        what=(
            "the `__config` blob rail retired (closure step 6; user-ruled 2026-09-27) — "
            "`fauna.config.get` / `.put` left the wire, so a pinned client that still "
            "loads its settings blob at login meets fauna.protocol.unknown_kind at a HEAD "
            "nest — by construction (the pinned-client cells)"
        ),
        cells=frozenset({
            CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE, CELL_AT_REST_DOWNGRADE}),
    ),
    InplaceBreak(
        commit="ead595ef4fdb3d0e977db137c15d0fe91729bdbe",
        landed="2026-10-02",
        what=(
            "the wire's `node` names (baseline survey, user-ratified 2026-10-01) — "
            "`fauna.node.{info,resolve}` became `fauna.nest.*` with no alias: the HEAD "
            "harness's trust seed calls `fauna.nest.info` on the pinned nest before every "
            "HEAD app launch, and a pinned app's `fauna.node.info` meets "
            "fauna.protocol.unknown_kind at a HEAD nest — by construction, every journey cell"
        ),
        cells=(_ALL_CELLS - {CELL_HARNESS_CLAIMS_PREV_NEST}) | _UPGRADE,
    ),
    InplaceBreak(
        commit="d9360afe8b0f85d65f107862cb2708c9ec783c5f",
        landed="2026-10-04",
        what=(
            "the nest DB's second genesis collapse (the fourth exception's ratification, "
            "under the user's 2026-10-04 blank-slate rulings) — the one-shot steps of "
            "schemas 79–124 folded into the genesis at 125, which stamps a new "
            "`application_id` and refuses a database of that run at open. A pinned nest's "
            "`/data` therefore does not boot under a HEAD image — by construction, as the "
            "first genesis's entry; the grid's cells are untouched, since every HEAD nest "
            "there opens a data dir of its own"
        ),
        cells=frozenset({CELL_IMAGE_UPGRADE_SMOKE}) | _UPGRADE,
    ),
    # ── Inside the 0.1.x compat-free window (the fifth exception, 2026-10-08) ───────
    # No `ratified-breaks.txt` line is written for these (the window owes none), but a
    # grid cell still meets them, so each still needs an entry here to read as a named
    # XFAIL rather than an anonymous red.
    InplaceBreak(
        commit="b4a80e397abecc00b0077e523d25a2c3fa70a44d",
        landed="2026-10-11",
        what=(
            "the starter-key count made owner-only, with a by-id `keypackage.addressable` "
            "probe (0.1.x window) — a HEAD app resolves a typed actor id through the new "
            "kind, which a pinned nest answers fauna.protocol.unknown_kind, so the DM "
            "recipient never becomes a chip; a pinned app resolves through "
            "`keypackage.count` on the recipient, which a HEAD nest refuses with "
            "fauna.conversations.permission_denied — measured 2026-10-13 on linux in both "
            "skew cells (the DM leg, after login and the feed post pass); the at-rest "
            "upgrade cell, whose pinned app sends the DM, by construction"
        ),
        cells=frozenset({
            CELL_HEAD_CLIENT_PREV_NEST, CELL_PREV_CLIENT_HEAD_NEST, CELL_AT_REST_UPGRADE}),
    ),
)


def _is_ancestor(commit: str, of: str) -> bool:
    """`git merge-base --is-ancestor commit of`, against THIS checkout's history.

    Exit 0 = ancestor, 1 = not — anything else (an unresolvable SHA in a shallow clone,
    a typo) raises: an unknown break must never read as "already included", since that
    would silently un-gate a cell the pin still cannot pass.
    """
    proc = subprocess.run(
        ["git", "-C", str(repo_root()), "merge-base", "--is-ancestor", commit, of],
        capture_output=True, text=True,
    )
    if proc.returncode == 0:
        return True
    if proc.returncode == 1:
        return False
    raise RuntimeError(
        f"git merge-base --is-ancestor {commit[:10]} {of[:10]} failed "
        f"(rc={proc.returncode}): {proc.stderr.strip()}"
    )


#: The commit that keyed the e2e trust seed by nest (`e2e-automation-surface-gating.md`
#: § The e2e trust seed). A build older than it reads `FAUNA_E2E_TRUST_NEST_IDENTITY` as
#: ONE bare identity and ignores a keyed entry, so a launch of such a binary is seeded in
#: the bare form (`conftest._apply_r14_trust_env(..., build_commit=...)`). Like the
#: in-place breaks above, ancestry decides, so the bare arm retires when the pin advances.
KEYED_TRUST_SEED_COMMIT = "4f1878c4e7e7"


def reads_keyed_trust_seed(commit: str) -> bool:
    """Whether a build from ``commit`` reads the nest-keyed trust seed."""
    return _is_ancestor(KEYED_TRUST_SEED_COMMIT, commit)


#: Per client: the commit from which a build of that client can be DRIVEN by the harness
#: on this machine's toolchain. Not a compat break and not a capability: the build runs,
#: but its test agent never reaches the app it renders, so a cell that drives the pinned
#: build of that client cannot assert anything until the pin includes the fix. Ancestry
#: decides, as for the breaks above, so the window lifts when the pin advances.
#:
#: ios: under the Xcode 27.1 SDK the in-process agent's closures, built in `App.init()`,
#: read an AppState the app never renders, so the session patch lands on that copy and
#: the rendered app logs "no usable stored session" and stays signed out (measured
#: 2026-11-01 on macOS against the pin). Fixed (the
#: agent stays unbound until the rendered scene binds it).
PREV_BUILD_DRIVABLE_FROM = {"ios": "f6856235648e"}


def prev_build_undrivable_reason(client: str, pin_commit: str | None = None) -> str:
    """Why the pinned build of `client` cannot be driven, or ``""`` when it can."""
    fix = PREV_BUILD_DRIVABLE_FROM.get(client)
    pin_commit = pin_commit or prev_commit()
    if not fix or _is_ancestor(fix, pin_commit):
        return ""
    return (
        f"PREVIOUS BUILD UNDRIVABLE: the pin {pin_commit[:10]} predates {fix}, the fix that "
        f"lets the harness drive a {client} build of this toolchain "
        f"(helpers/prev_build.py::PREV_BUILD_DRIVABLE_FROM); clears when the pin advances"
    )


def pending_inplace_breaks(pin_commit: str | None = None) -> tuple[InplaceBreak, ...]:
    """The ratified breaks the pin does NOT yet include — the windows currently open.

    Empty once the pin has advanced past every entry (and always empty for HEAD).
    """
    pin_commit = pin_commit or prev_commit()
    return tuple(b for b in RATIFIED_INPLACE_BREAKS if not _is_ancestor(b.commit, pin_commit))


def expected_incompatible_reasons(cell: str, pin_commit: str | None = None) -> tuple[str, ...]:
    """Why `cell` is expected to fail against the pin right now — one reason per pending
    break that covers it; empty when the cell is expected to assert its journey."""
    if cell not in KNOWN_CELLS:
        raise ValueError(
            f"unknown cell {cell!r}; one of {GRID_CELLS + PIPELINE_CELLS + UPGRADE_CELLS}")
    pin_commit = pin_commit or prev_commit()
    return tuple(
        b.reason(pin_commit) for b in pending_inplace_breaks(pin_commit) if cell in b.cells
    )


def cache_dir(commit: str, profile: str = "debug") -> Path:
    return CACHE_ROOT / commit / profile


def cached_binaries(
    commit: str, profile: str = "debug", client: str | None = None, nest: bool = True
) -> dict[str, Path] | None:
    """The cached {nest, <client>} binaries for this key, or None if the cache is cold.

    `nest=False` asks for the client alone (a drive against a deployed nest runs none).
    A client's companions (`CLIENT_COMPANIONS`) must be there too, or the cache is cold:
    a tui cached before its agent was is rebuilt once rather than launched agentless.
    """
    client = client or _default_client()
    wanted = {client: CLIENT_BINARY[client]}
    if nest:
        wanted = {"nest": NEST[1], **wanted}
    for package, binary in CLIENT_COMPANIONS.get(client, ()):
        wanted[package] = binary
    out_dir = cache_dir(commit, profile)
    out = {}
    for key, binary in wanted.items():
        path = out_dir / binary
        if not path.exists():
            return None
        out[key] = path
    if client == "macos" and not _macos_cache_is_complete(out_dir, out[client]):
        return None  # binaries there, frameworks missing — cold, not warm
    if client in _WHOLE_DIR_CLIENTS and not _app_dir_cache_is_complete(out_dir, client):
        return None  # app there, but the directory copy that carries it may not have
                     # finished — see `_app_dir_cache_is_complete`
    return out


def client_absent_reason(commit: str, client: str, repo: Path | None = None) -> str | None:
    """Why `client` cannot be built at `commit`, or None when its source is there.

    A tree read (`git cat-file -e <commit>:<dir>`), so the refusal comes before any
    extraction or build: 0.1.5's tree has no `bins/fauna-reports-pass`, and the reports
    pass is then unavailable for that release (`org-nests.md` § fauna.build →
    *Disposable*, rule (1), the consequence it accepts)."""
    rel = CLIENT_SOURCE_DIR.get(client)
    if rel is None:
        return f"unknown client {client!r} (known: {sorted(CLIENT_SOURCE_DIR)})"
    root = repo or repo_root()
    if subprocess.run(["git", "-C", str(root), "cat-file", "-e", f"{commit}^{{commit}}"],
                      capture_output=True).returncode != 0:
        return f"commit {commit[:12]} is not in this repository"
    if subprocess.run(["git", "-C", str(root), "cat-file", "-e", f"{commit}:{rel}"],
                      capture_output=True).returncode != 0:
        return f"commit {commit[:12]} has no {rel} — the {client} client does not exist there"
    return None


def _cargo_cmd(args: list[str]) -> list[str]:
    """The cargo invocation for this platform.

    On win, a bare `cargo build` picks up Git Bash's `/usr/bin/link.exe` ahead of the
    MSVC linker on PATH and fails — every cargo call on Windows routes through
    `scripts/cargo-win.cmd`, which prepends the correct MSVC/LLVM paths first.
    Elsewhere `cargo` is a bare command.
    """
    cmd = ["cmd", "/c", "scripts\\cargo-win.cmd", *args] if sys.platform == "win32" \
        else ["cargo", *args]
    if os.environ.get("FAUNA_SLOT_HELD_BUILD"):
        return cmd  # already inside a 'build' grant (`just e2e-prev-build`'s wrap)
    # Unwrapped — the web leg, whose `just web-test` must not sit inside an outer build
    # slot (`e2e-prev-build`), or a pytest fixture warming a cold cache: a compile is
    # counted whoever runs it (`core-bound.md` § Per-core tokens).
    slot = repo_root() / "scripts" / "build-slot.py"
    if not slot.exists():
        return cmd
    return ["uv", "run", "--no-project", "python", str(slot), "--pool", "build", "--", *cmd]


def _run(cmd: list[str], cwd: Path, env: dict[str, str], timeout: int) -> None:
    print(f"[prev_build] $ {' '.join(cmd)}  (cwd={cwd})", flush=True)
    result = subprocess.run(cmd, cwd=cwd, env=env, timeout=timeout)
    if result.returncode != 0:
        raise RuntimeError(f"[prev_build] command failed ({result.returncode}): {' '.join(cmd)}")


def ensure_source_tree(commit: str) -> Path:
    """The repo's source tree at `commit`, extracted under the temp root. Idempotent.

    A `git archive` extraction, not a second checkout — see the module doc for why
    (nothing registered with git; nothing for housekeeping to sweep).
    """
    path = SRC_ROOT / commit[:12]
    stamp = path / ".fauna-prev-src-complete"
    if stamp.exists():
        return path
    # A partial extraction (an interrupted run) must not be mistaken for a good tree:
    # the stamp is written last, and a stamp-less dir is rebuilt from scratch.
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True, exist_ok=True)
    archive = subprocess.run(
        ["git", "-C", str(repo_root()), "archive", "--format=tar", commit],
        stdout=subprocess.PIPE, check=True,
    )
    subprocess.run(["tar", "-x", "-C", str(path)], input=archive.stdout, check=True)
    stamp.write_text(commit + "\n")
    return path


def _target_dir(src: Path, commit: str, client: str) -> Path:
    """Where cargo writes while building the pinned tree.

    Off-tree by default (so the whole dir can be dropped without disturbing the source
    stamp). **The apple and windows legs must keep it IN-tree**, and that is load-bearing
    rather than cosmetic: `apple-ffi-host` hardcodes the *relative* slice path
    `target/<profile>/apple-ffi-host/<features>/libfauna_ffi.a` (its own
    `--artifact-dir`, implicit-host since 2026-08-22), and `windows-ffi` hardcodes
    `target/<profile>/windows-ffi/<features>/fauna_ffi.dll` (its flavor-private copy,
    implicit-host since 2026-08-23 — row 60) (both `justfile`), as the
    `build-if-stale` source each gates its bindgen + copy step on. Point CARGO_TARGET_DIR
    somewhere else and cargo happily writes the artifact there while the recipe looks for
    it at the in-tree path, finds nothing, and generates bindings against a missing slice
    (or, on windows, ships a stale `.dll`). Keeping cargo's output where the recipe
    already expects it is the fix; the dir is wiped after extraction either way.
    """
    if client in ("macos", "ios", "windows"):
        return src / "target"
    return TARGET_ROOT / commit[:12]


def _resolve_windows_built_exe(src: Path, profile: str) -> Path:
    """The MSBuild output exe for a pinned windows build.

    Mirrors conftest's `_resolve_windows_app`: MSBuild's `-p:Platform=ARM64` writes under
    a platform-named subdir, but that name isn't worth hardcoding twice — search the same
    candidates conftest does and pick the newest (the pinned tree only ever runs one
    build, but "newest" is a harmless, cheap-to-keep invariant to share with it).
    """
    configuration = "Release" if profile == "release" else "Debug"
    base = src / "apps/fauna-windows/FaunaApp/FaunaApp/bin"
    suffix = f"{configuration}/net10.0-windows10.0.26100/FaunaApp.exe"
    candidates = [p for sub in ("ARM64", "x64", "") if (p := base / sub / suffix).exists()]
    if not candidates:
        raise RuntimeError(f"[prev_build] windows build produced no exe under {base}")
    return max(candidates, key=lambda p: p.stat().st_mtime)


def _ios_derived(src: Path) -> Path:
    """The pinned iOS app's derived data, inside the pinned tree so the tear-down takes it."""
    return src / "apps/fauna-apple/.ios-derived"


#: The rust targets `apple-ffi-test`'s device and simulator slices compile for.
_IOS_RUST_TARGETS = ("aarch64-apple-ios", "aarch64-apple-ios-sim")


def _require_ios_rust_targets(src: Path) -> None:
    """Refuse before a build that cannot finish, naming the owner's install.

    The pin's tree builds with the PIN's toolchain (its `rust-toolchain.toml`), which
    trails the working tree's after a toolchain bump, and rustup installs a toolchain's
    pinned targets only: the apple ones are a machine's own setup step, done for the
    toolchain of the day. Cargo's own answer to
    the gap is an `E0463` on the first dependency, after the slot wait.
    """
    with open(src / "rust-toolchain.toml", "rb") as fh:
        channel = tomllib.load(fh)["toolchain"]["channel"]
    listed = subprocess.run(
        ["rustup", f"+{channel}", "target", "list", "--installed"],
        capture_output=True, text=True, check=True,
    ).stdout.split()
    missing = [t for t in _IOS_RUST_TARGETS if t not in listed]
    if missing:
        raise RuntimeError(
            f"[prev_build] the pin's toolchain {channel} has no {' '.join(missing)} target "
            f"on this machine, so the pinned iOS app cannot build. A toolchain install is "
            f"the owner's to approve: rustup +{channel} target add {' '.join(missing)}"
        )


def _build_ios(src: Path, env: dict[str, str], profile: str) -> Path:
    """The pinned tree's iOS Simulator app: its `.app`, in the shape HEAD's harness builds.

    Two steps, as at HEAD (`conftest._build_ios_app`): the xcframework, then the SPM
    product. The recipe is `apple-ffi-test`, the three-slice test flavor, because a pin can
    predate the simulator-only `apple-ffi-test-sim`. Its simulator slice is the one this
    build links, so the difference is compile time, never the artifact.
    """
    sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
    from drivers.ios import BUNDLE_ID
    from helpers.ios_app_bundle import (
        SIMULATOR_BUILD_ARGS,
        write_app_bundle,
        write_package_workspace,
    )

    _require_ios_rust_targets(src)
    _run(["just", "apple-ffi-test"], cwd=src, env=env, timeout=5400)
    package_dir = src / "apps/fauna-apple"
    derived = _ios_derived(src)
    configuration = "Release" if profile == "release" else "Debug"
    workspace = write_package_workspace(derived.parent / ".ios-package.xcworkspace", package_dir)
    _run(
        ["xcodebuild", "build", "-workspace", str(workspace), "-configuration", configuration,
         *SIMULATOR_BUILD_ARGS, "-derivedDataPath", str(derived)],
        cwd=package_dir, env=env, timeout=5400,
    )
    exe = derived / "Build/Products" / f"{configuration}-iphonesimulator" / "FaunaiOS"
    if not exe.exists():
        raise RuntimeError(f"[prev_build] xcodebuild produced no executable at {exe}")
    app_dir = derived / "FaunaiOS.app"
    write_app_bundle(app_dir, exe, BUNDLE_ID)
    return app_dir


def _build_client(
    client: str, src: Path, target: Path, env: dict[str, str], profile: str
) -> Path:
    """Build the pinned tree's client and return the produced binary.

    linux and tui are plain cargo binaries. macOS is a SwiftPM product whose build is a two-step
    the `justfile` already owns (`mac-debug` = `apple-ffi-host` — the 1-slice host-only
    debug xcframework — then `swift build --product FaunaMacOS`). windows is a WinUI/MSBuild
    project, likewise a two-step `justfile` recipe (`windows-debug` = `windows-ffi` — the
    FFI `.dll` + C# binding regen — then the MSBuild ARM64 XAML compile). Driving both
    through `just` rather than open-coding the steps keeps the build-if-stale gates and
    the generated-file deps firing exactly as they do for a HEAD build, which is the whole
    point of a *fidelity* harness — and routes the FFI half through `cargo-win.cmd`
    exactly as a normal dev build does (see `windows-ffi` in `justfile`). ios has no such
    recipe: `_build_ios`.
    """
    if client == "ios":
        return _build_ios(src, env, profile)

    if client == "web":
        # `web-test`, not `web`: the e2e flavour HEAD's `static_dir` builds, whose wasm
        # chunks carry the test-helpers feature the harness's web agent drives. One
        # profile only — the SPA's wasm is built as the recipe builds it at HEAD too, so
        # `profile` has no web meaning and the cache key alone carries it.
        _run(["just", "web-test"], cwd=src, env=env, timeout=5400)
        return src / "apps/fauna-web/build"

    if client == "macos":
        recipe = "mac-release" if profile == "release" else "mac-debug"
        # The apple toolchain is slow from cold: a full FFI cdylib build for the darwin
        # slice, then the Swift compile of FaunaKit + the app.
        _run(["just", recipe], cwd=src, env=env, timeout=5400)
        # SwiftPM's `.build/<profile>` symlink resolves under either build system.
        return src / "apps/fauna-apple/.build" / profile / "FaunaMacOS"

    if client == "windows":
        recipe = "windows-release" if profile == "release" else "windows-debug"
        _run(["just", recipe], cwd=src, env=env, timeout=5400)
        return _resolve_windows_built_exe(src, profile)

    profile_args = ["--release"] if profile == "release" else []
    companions = [a for package, _ in CLIENT_COMPANIONS.get(client, ()) for a in ("-p", package)]
    _run(
        _cargo_cmd(["build", "-p", CLIENT_PACKAGE[client], *companions, *profile_args]),
        cwd=src, env=env, timeout=5400,
    )
    return target / profile / CLIENT_BINARY[client]


def _macos_rpath_deps(binary: Path) -> list[str]:
    """`binary`'s non-system dynamic dependencies, as @rpath-relative paths.

    SwiftPM links its own targets statically and the FFI arrives as a static `.a` inside
    the xcframework, so the tempting assumption is "the executable is standalone". It is
    **not** for every commit a previous build may come from: until 2026-10-08
    `FaunaMacOS` linked an `@rpath/<Name>.framework/Versions/B/<Name>` (the auto-update
    framework the app no longer carries), a real framework bundle that SwiftPM drops
    *beside* the executable in the build dir. Wipe the build dir and the cached binary dies at launch with `rc=-6` and a
    dyld "no such file" for a framework nobody remembers linking.

    So the dependency list is read from the binary itself rather than guessed. Returns
    entries like ``<Name>.framework/Versions/B/<Name>`` (the @rpath prefix stripped),
    which is exactly the path to reproduce next to the cached executable — dyld resolves
    @rpath against @executable_path, so a framework sitting beside the binary is found.
    """
    try:
        out = subprocess.run(
            ["otool", "-L", str(binary)], capture_output=True, text=True, check=True,
        ).stdout
    except (subprocess.CalledProcessError, FileNotFoundError):
        return []  # can't tell (not a Mac / no otool) — nothing to stage
    deps = []
    for line in out.splitlines()[1:]:
        ref = line.strip().split(" ", 1)[0]
        if ref.startswith("@rpath/"):
            deps.append(ref[len("@rpath/"):])
    return deps


def _macos_cache_is_complete(out_dir: Path, binary: Path) -> bool:
    """Every @rpath dep of the cached `binary` resolves beside it.

    Guards against a **partially populated** cache: the binaries alone look like a warm
    cache to `cached_binaries`, so without this a cache missing its frameworks would be
    served forever and every launch would die at dyld. Re-derived from the binary each
    time rather than recorded in a stamp, so it stays true if the app gains a framework.
    """
    return all((out_dir / dep).exists() for dep in _macos_rpath_deps(binary))


#: Clients cached as a whole directory, not a file: `CLIENT_BINARY[client]`'s first
#: segment is that directory, and it holds the stamp `_app_dir_cache_is_complete` reads.
_WHOLE_DIR_CLIENTS = ("windows", "ios", "web")


def _cache_app_dir(client: str) -> str:
    return CLIENT_BINARY[client].split("/", 1)[0]


def _app_dir_cache_is_complete(out_dir: Path, client: str) -> bool:
    """The cached app directory copy finished (stamp written last).

    The analogue of `_macos_cache_is_complete`, but stamp-based rather than
    dep-introspected: there is no cheap "list every file this needs" equivalent to
    `otool -L` for a managed .NET app or a bundle, and the fix is the same shape as
    `ensure_source_tree`'s extraction stamp — the directory copy either finishes and
    writes the stamp, or it didn't finish and there is nothing to trust.
    """
    return (out_dir / _cache_app_dir(client) / ".fauna-prev-complete").exists()


def build_prev(commit: str, profile: str = "debug", client: str | None = None,
               nest: bool = True) -> dict[str, Path]:
    """Build the pinned release's nest + client binaries and populate the cache.

    Returns {"nest": path, "<client>": path} (plus the client's companions; no "nest"
    when `nest=False`). Slow (a full workspace build, plus the apple toolchain on macOS)
    on a cold cache; a no-op once cached. Warm it out-of-band with:

        python3 tests/e2e-unified/helpers/prev_build.py --build
    """
    client = client or _default_client()
    if client not in CLIENT_BINARY:
        raise RuntimeError(f"unknown client {client!r} (known: {sorted(CLIENT_BINARY)})")

    cached = cached_binaries(commit, profile, client, nest=nest)
    if cached:
        return cached
    absent = client_absent_reason(commit, client)
    if absent:
        raise RuntimeError(f"[prev_build] {absent}")

    src = ensure_source_tree(commit)
    target = _target_dir(src, commit, client)
    target.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ)
    # Load-bearing: without this the pin build would land in *this* branch's build-cache
    # dir, mixing pinned artifacts with HEAD artifacts and eating its budget. (On the
    # macOS leg this points back in-tree on purpose — see `_target_dir`.)
    env["CARGO_TARGET_DIR"] = str(target)
    # The build-commit stamp prefers this over `git rev-parse HEAD`; pin it so the previous nest
    # honestly reports the pinned commit on /api/v1/health (the tests assert on it).
    env["FAUNA_BUILD_COMMIT"] = commit[:8]

    # Generated sources (i18n strings, provider tables) are gitignored, so a fresh
    # extraction has none — the same deps `just linux-debug` / `just mac-debug` declare.
    _run(["just", "i18n-generate", "providers-generate"], cwd=src, env=env, timeout=900)

    out_dir = cache_dir(commit, profile)
    out_dir.mkdir(parents=True, exist_ok=True)
    built = {}
    produced = {}
    # A second client on this machine finds the nest already cached: one commit, one
    # profile, the same features, so it is this build's nest too.
    cached_nest = out_dir / NEST[1]
    if not nest:
        pass  # the client alone — a drive against a deployed nest runs no local one
    elif cached_nest.exists():
        built["nest"] = cached_nest
    else:
        profile_args = ["--release"] if profile == "release" else []
        _run(
            _cargo_cmd(["build", "-p", NEST[0], *NEST[2], *profile_args]),
            cwd=src, env=env, timeout=5400,
        )
        produced["nest"] = target / profile / NEST[1]
    produced[client] = _build_client(client, src, target, env, profile)
    for package, binary in CLIENT_COMPANIONS.get(client, ()):
        produced[package] = target / profile / binary

    for key, path in produced.items():
        if not path.exists():
            raise RuntimeError(f"[prev_build] expected binary not produced: {path}")
        if key == client and client in _WHOLE_DIR_CLIENTS:
            continue  # a whole-directory copy, not a single-file one — see below
        dest = out_dir / path.name
        shutil.copy2(path, dest)
        dest.chmod(0o755)
        built[key] = dest
        print(f"[prev_build] cached {dest}", flush=True)

    if client == "windows":
        # Unlike a cargo binary, a WinUI MSBuild output is a whole directory: managed
        # DLLs, .resx-derived resources, and the native fauna_ffi.dll under
        # runtimes/win-arm64/native/. Caching just the .exe (as the generic loop above
        # does for every other app) produces a binary that dies on launch missing its
        # managed deps — the windows analogue of the macOS framework trap below,
        # except total (the whole directory) rather than two named files. So the whole
        # containing directory travels as one unit, and `built[client]` points at the exe
        # inside the copy.
        windows_out = out_dir / "windows-app"
        shutil.rmtree(windows_out, ignore_errors=True)
        shutil.copytree(produced[client].parent, windows_out)
        # Written LAST (mirrors `ensure_source_tree`'s stamp): `cached_binaries` treats a
        # stamp-less dir as cold, so an interrupted copytree can never be served warm.
        (windows_out / ".fauna-prev-complete").write_text(commit + "\n")
        built[client] = windows_out / produced[client].name
        print(f"[prev_build] cached windows app dir {windows_out}", flush=True)

    if client == "ios":
        # The whole `.app`: `simctl install` takes the bundle, and its plist is part of
        # what it installs. Stamped last, as windows' directory above.
        ios_out = out_dir / _cache_app_dir("ios")
        shutil.rmtree(ios_out, ignore_errors=True)
        ios_out.mkdir()
        shutil.copytree(produced[client], ios_out / produced[client].name)
        (ios_out / ".fauna-prev-complete").write_text(commit + "\n")
        built[client] = ios_out / produced[client].name
        print(f"[prev_build] cached ios app {built[client]}", flush=True)

    if client == "web":
        # The whole SPA tree the proxy serves (`/app/` index, chunks, wasm). Stamped last,
        # as windows' directory above.
        web_out = out_dir / _cache_app_dir("web")
        shutil.rmtree(web_out, ignore_errors=True)
        web_out.mkdir()
        shutil.copytree(produced[client], web_out / produced[client].name)
        (web_out / ".fauna-prev-complete").write_text(commit + "\n")
        built[client] = web_out / produced[client].name
        print(f"[prev_build] cached web tree {built[client]}", flush=True)

    if client == "macos":
        # Stage the framework bundles the executable links (@rpath -> @executable_path):
        # they live BESIDE the binary in the Swift build dir, so they must live beside it
        # in the cache too, or the wipe below takes them with it and every launch dies at
        # dyld. `symlinks=True` is load-bearing — a .framework is a symlink farm
        # (Versions/Current -> B), and flattening it produces a bundle dyld won't load.
        produced_dir = produced[client].parent
        for dep in _macos_rpath_deps(built[client]):
            bundle = dep.split("/", 1)[0]  # <Name>.framework/Versions/B/<Name> -> <Name>.framework
            src_bundle, dest_bundle = produced_dir / bundle, out_dir / bundle
            if src_bundle.exists() and not dest_bundle.exists():
                shutil.copytree(src_bundle, dest_bundle, symlinks=True)
                print(f"[prev_build] staged {dest_bundle}", flush=True)
        missing = [d for d in _macos_rpath_deps(built[client]) if not (out_dir / d).exists()]
        if missing:
            raise RuntimeError(
                f"[prev_build] the cached {built[client].name} links dependencies that are "
                f"not beside it: {missing}. It would die at dyld once the build dir is "
                f"wiped — refusing to cache a binary that cannot launch."
            )

    if not os.environ.get("FAUNA_PREV_KEEP_TARGET"):
        # The binaries (+ their frameworks) are extracted; the multi-GB build dirs have
        # served their purpose. Disk is the scarce resource here, not rebuild time — and
        # on the Mac it is a shared one: a full apple toolchain output alongside a live
        # checkout's target/ has filled this machine's disk before, and an ENOSPC mid-build
        # silently truncates binaries that other builds on the box are still using.
        if client in ("macos", "ios"):
            shutil.rmtree(src / "apps/fauna-apple/.build", ignore_errors=True)
            shutil.rmtree(src / "apps/fauna-apple/FaunaFFI.xcframework", ignore_errors=True)
        if client == "ios":
            shutil.rmtree(_ios_derived(src), ignore_errors=True)
        if client == "windows":
            # MSBuild's bin/obj (extracted into the cache above) — disk is even scarcer
            # here than on the Mac (a single shared `C:` across ~38 concurrent checkouts).
            shutil.rmtree(src / "apps/fauna-windows/FaunaApp/FaunaApp/bin", ignore_errors=True)
            shutil.rmtree(src / "apps/fauna-windows/FaunaApp/FaunaApp/obj", ignore_errors=True)
        shutil.rmtree(target, ignore_errors=True)
        print(f"[prev_build] removed scratch build dirs under {src}", flush=True)

    # The keep rule at this module's own write: the pin, this commit and the newest two
    # others stay; every older commit's tree and binaries go.
    prune_prev_cache(TMP_ROOT, also_keep=(commit,))
    return built


def prev_binaries(
    profile: str = "debug", build: bool = True, client: str | None = None
) -> dict[str, Path]:
    """The pinned previous release's binaries, building them on a cold cache."""
    commit = prev_commit()
    client = client or _default_client()
    cached = cached_binaries(commit, profile, client)
    if cached:
        return cached
    if not build:
        raise RuntimeError(
            f"previous build not cached for {commit[:8]}/{profile}/{client}; warm it with "
            f"`just e2e-prev-build` (or python3 tests/e2e-unified/helpers/prev_build.py --build)"
        )
    return build_prev(commit, profile, client)


def prev_nest_binary(profile: str = "debug", build: bool = True) -> Path:
    """The pinned previous release's NEST alone, for a cell whose app side is HEAD's.

    A cell that drives only HEAD's app (the mixed-nests cell, on the tui leg) needs no
    pinned app, so it reads the nest out of whatever build this machine has cached at
    the pin. Cold, it builds through `prev_binaries` with this machine's own client, so
    the cache keeps the one shape every other cell reads.
    """
    nest = cache_dir(prev_commit(), profile) / NEST[1]
    if nest.exists():
        return nest
    return prev_binaries(profile, build=build)["nest"]


#: The command line, all of it. Flags are read by membership, so anything outside this
#: set must be refused rather than ignored: the fall-through is a real build, and an
#: ignored `--help` once started an unslotted one beside a build already warming the
#: same source tree (`tests/test_prev_build_cli.py`).
_USAGE = """\
usage: prev_build.py [--build] [--client <app>] [--profile debug|release]
                     [--commit <sha>] [--client-only]
       prev_build.py --path [--client <app>] [--profile debug|release]
                     [--commit <sha>] [--client-only]
       prev_build.py --expected-incompatible <cell> [--pin <sha>]

  --build    build the pinned previous release's nest + client into the cache
             (a no-op when warm; slow when cold — run it as `just e2e-prev-build`,
             which takes a build slot)
  --path     print the cached binaries, or exit 1 when the cache is cold
  --commit <sha>
             key the build on <sha> (40 hex) instead of the pin (the release-app
             drives: helpers/release_app.py)
  --client-only
             the client (and its companions) alone, no nest
  --expected-incompatible <cell>
             print why <cell> is expected to fail against the pin (exit 0), or exit 3
"""
_FLAGS_WITH_VALUE = ("--client", "--profile", "--pin", "--expected-incompatible", "--commit")
_BARE_FLAGS = ("--build", "--path", "--client-only")


def _unknown_flag(args: list[str]) -> str | None:
    i = 0
    while i < len(args):
        arg = args[i]
        if arg in _FLAGS_WITH_VALUE:
            i += 2
            continue
        if arg not in _BARE_FLAGS:
            return arg
        i += 1
    return None


def main() -> int:
    profile = "debug"
    args = sys.argv[1:]
    if "--help" in args or "-h" in args:
        print(_USAGE, end="")
        return 0
    unknown = _unknown_flag(args)
    if unknown is not None:
        print(f"prev_build.py: unrecognised argument {unknown!r}\n{_USAGE}", end="",
              file=sys.stderr)
        return 2
    if "--expected-incompatible" in args:
        # The upgrade smoke's question, answered from pin ancestry: exit 0 and print each
        # reason when `cell` is inside an open window against `--pin` (default: the pin
        # file), exit 3 when the cell is expected to pass. Any other failure — an
        # unresolvable SHA in a shallow clone above all — raises, so it can never read as
        # "no window" and silently un-gate the cell.
        cell = args[args.index("--expected-incompatible") + 1]
        pin = args[args.index("--pin") + 1] if "--pin" in args else prev_commit()
        reasons = expected_incompatible_reasons(cell, pin)
        for reason in reasons:
            print(reason)
        return 0 if reasons else 3
    if "--profile" in args:
        profile = args[args.index("--profile") + 1]
    client = _default_client()
    if "--client" in args:
        client = args[args.index("--client") + 1]
    with_nest = "--client-only" not in args
    if "--commit" in args:
        commit = args[args.index("--commit") + 1]
        if len(commit) != 40 or not all(c in "0123456789abcdef" for c in commit):
            print(f"prev_build.py: --commit takes a full 40-hex commit, not {commit!r}",
                  file=sys.stderr)
            return 2
        print(f"[prev_build] commit={commit[:12]} (named, not the pin) "
              f"profile={profile} client={client} nest={with_nest}")
    else:
        pin = load_pin()
        commit = pin["commit"]
        print(
            f"[prev_build] pin: commit={commit[:12]} released={pin['released']} "
            f"profile={profile} client={client} nest={with_nest}"
        )
    if "--path" in args:
        cached = cached_binaries(commit, profile, client, nest=with_nest)
        if not cached:
            print("[prev_build] cache is COLD", file=sys.stderr)
            return 1
        for key, path in cached.items():
            print(f"{key}={path}")
        return 0
    binaries = build_prev(commit, profile, client, nest=with_nest)
    for key, path in binaries.items():
        print(f"{key}={path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
