"""The run-written half of the feature catalog: what this run observed, per app.

`docs/goal/architecture/feature-catalog.md` § The ledger owns the schema and the
stamp grammar; this module is that section's implementation, and the conftest hooks
(`pytest_runtest_logreport` / `pytest_sessionfinish`) are its only callers.

**Why a document is allowed to say what happened here.** `testing.md` § What the tree
may assert about testing bans a doc from stating what testing *has* happened, because
such a claim is uncorrectable — it decays in place. The ledger is the one exception,
and it is the exception *because* it does not decay: it is first-party (written by
the run that observed the outcome, on the commit it names, against the artifact it
names) and revisable (the next run overwrites it).

**One file per app.** Runs on every development platform write to this directory; a
single shared file would conflict on every rebase. Each run touches only the files of
the apps it drove, plus `nest.json` when it ran nest witnesses.

Two things a caller must get right, and both are why they are functions here rather
than inline in the hooks:

* **App attribution is decided by the run, not by a marker** (§ The marker). An
  app-parametrized test is attributed to the apps its parametrization names — a
  convention-16 seat pair names both — and a driver it launched for any other app
  is a *peer* (a helper seat) that writes no record. A test with no app
  parametrization is attributed to *every* app whose driver it launched — a
  multi-seat test that drives three desktop apps witnesses all three — and one that
  launched no driver is a `nest` witness. `attributed_apps()` is that fold;
  `note_app()` is called from the two doors every driver comes through and
  `note_param_apps()` from conftest's per-test setup, so no test has to remember.
* **`skip_environment` writes nothing** (§ Cell semantics). A missing credential says
  nothing about the feature, so the previous ledger line must stand rather than be
  overwritten with a skip. That is why the three declaration helpers call
  `note_skip_class()` instead of the writer parsing skip messages.
* **A test that brought its OWN nest artifact is recorded against THAT artifact,
  never against the run's.** Provenance is per record: an ordinary record carries
  the run's `commit` / `image_digest` / `nest_mode`, and a `tests/platform/docker/`
  test — which boots its tree's keyed test image (`helpers/nest_test_image.py`), or
  the image the run names in `FAUNA_NEST_TEST_IMAGE` —
  carries the booted container's instead: `nest_mode` is `docker` unconditionally
  (a fact about the test), `image_digest` is the booted image's own registry
  digest (`null` for a hand-built one), and `version` / `commit` are what its
  `/api/v1/health` reported. The container fixtures' health door
  (`tests/platform/docker/helpers.py::wait_for_health`) calls `note_container()`
  at the moment the container answers, and the note is attached to the FIXTURE
  whose setup observed it, because the package's container fixtures are mostly
  module-scoped: a per-test slot would reach only the first test of each module.
  conftest sets `note_own_artifact()` from the fixture closure at setup, and the
  per-report hook then asks `own_artifact_resolution()` for the one artifact the
  test observed. The writer still DECLINES — tallied and printed, never silent —
  in exactly three shapes, each of which would otherwise name no artifact or the
  wrong one: no booted container reported its identity; more than one distinct
  BUILD was observed (a record names exactly one — and an artifact is a build,
  the `(version, commit)` the container reported, so two image ids of one build
  are one artifact: § The ledger, *One artifact is one build*); the image
  reports no commit sha (a hand-built image). Owner of the class and the rulings:
  `feature-catalog.md` § The ledger; `testing.md` § Default app and nest mode,
  exclusion class (7).
* **A record from a DIRTY tree says so** (§ The ledger, *Dirty trees*). `head_commit()`
  names HEAD; when the working tree carries uncommitted changes the code the run
  observed is HEAD *plus* edits nobody can name, so the commit is only a lower
  bound — and the default inner loop (edit, run, commit while it builds) produces
  exactly that tree. The run never refuses to record (a run that observed something
  and did not record it is the failure the catalog exists to prevent); it MARKS:
  `dirty: true` on every record and on the cell a complete observation stamps,
  `.dirty` as a second build-metadata identifier in the stamp. `tree_dirt()` is the
  one definition of dirt — `git status --porcelain` outside `docs/features/ledger/`,
  the run's own output — and the release-candidate gate reads the same function, so
  the gate and the mark cannot disagree. An own-artifact record is never dirty: the
  image is a built artifact, and this checkout's edits are not in it.
* **A record ALSO names the main commit beneath the one that ran** (§ The ledger,
  *Rebased commits*). `commit` is the run's own HEAD, and every landing path rebases
  onto `origin/main` before it pushes, so whenever main moves during a run — the
  ordinary case for a run long enough to matter — the record lands naming a commit
  that resolves nowhere in main's history. `commit` is never rewritten (the
  post-rebase sha names a tree that never ran); instead every record carries
  `base` — `main_base()`, the merge-base of the record's commit with `origin/main`
  at the moment the commit was read — which always resolves (main only ever
  fast-forwards) and means what `dirty` makes `commit` mean, one level up: a lower
  bound on the code the run saw. Unknown (no `origin/main` ref, no git) is `None`,
  never a guess. Asked once per run at collection, beside the commit, and once per
  booted container at `note_container` — never on the per-report path.
"""

from __future__ import annotations

import json
import os
import platform
import re
import subprocess
from datetime import date
from pathlib import Path
from typing import NamedTuple

REPO = Path(__file__).resolve().parents[3]
#: The run's own output, relative to the repository root — the one path whose
#: modification is never dirt (`tree_dirt`): a run WRITES here, so counting it would
#: make every second run dirty for a reason that says nothing about what it observed.
LEDGER_REL = "docs/features/ledger"
LEDGER = REPO / LEDGER_REL
#: The integration branch every landing path rebases onto and fast-forward-pushes
#: (the merge script). A record's `base` is its commit's merge-base with THIS ref
#: as the checkout knew it: the newest main commit in the record's own history.
MAIN_REF = "origin/main"

#: Skip classes recorded in the ledger (convention 7's two declared shapes). An
#: environment skip is deliberately NOT one of them — see the module docstring.
UNBUILT = "unbuilt"
ABSENCE = "absence"
ENVIRONMENT = "environment"

NEST = "nest"

# ── per-test state, reset by the hooks ──────────────────────────────────────
_apps_this_test: set = set()
#: The apps the test's own parametrization names (`conftest.py::_item_param_apps`),
#: empty for an item with no app parametrization. Set by conftest at setup.
_param_apps_this_test: set = set()
_skip_class: str | None = None
#: Set by `app_surface._unbuilt` immediately before its `--strict-app` `pytest.fail`:
#: the red that follows is a refused unbuilt surface, not a failed test body, and the
#: per-report hook records it as `skipped`/`unbuilt` (§ The ledger, `outcome`).
_strict_refusal: bool = False
#: The surface-registry key the firing `require_surface` named, or `None`
#: (§ The ledger, `surface`), and every key the test's calls reached, fired or not,
#: in first-reached order (`surfaces`).
_surface: str | None = None
_surfaces_this_test: list = []
#: Why this test's outcome cannot be attributed to the run's artifact, or `None`.
#: Set per test by conftest from the fixture closure (`nest_surface.OWN_IMAGE_FIXTURES`).
_own_artifact: str | None = None
#: Whether the refusal above has already been tallied for THIS test. One test can
#: reach the per-report hook up to three times (a setup error, a call outcome, a
#: teardown failure), and the tally counts tests, not reports.
_own_artifact_tallied: bool = False
#: The test's real fixture closure (`helpers.fixture_closure.real_fixture_closure`),
#: set by conftest at setup — how an own-artifact test finds the provenance its
#: module-scoped container fixture noted during an EARLIER test's setup.
_closure_this_test: frozenset = frozenset()
#: {image id: provenance} — artifacts observed by THIS test's own body (a restart
#: flow's `wait_for_health`, say), outside any fixture's setup.
_artifacts_this_test: dict = {}

# ── per-session state ───────────────────────────────────────────────────────
#: The fixtures currently being SET UP, innermost last — pushed and popped by
#: conftest's `pytest_fixture_setup` wrapper, so `note_container()` can attach a
#: booted artifact to the fixture whose setup observed it.
_fixture_stack: list = []
#: {fixture name: {image id: provenance}} — what each fixture's most recent setup
#: observed. Cleared when that fixture sets up again (a module-scoped container
#: fixture re-executing for the next module boots a fresh container, possibly of
#: a different image), so a test's closure always reads the artifacts of the
#: fixture instances that are actually alive for it.
_artifacts_by_fixture: dict = {}
#: What a commit an image reports has to look like to be recorded: the pipeline
#: stamps the full 40-hex sha, `just docker-push-dev` the short 8, and a build
#: with nothing to stamp says `dev` (or `unknown`, from a tree without `.git`).
_SHA_RE = re.compile(r"^[0-9a-f]{7,40}$")
#: {(app, slug, node id): record}
_records: dict = {}
#: {slug: {node ids}} — what this run SELECTED as witnesses of each feature.
_collected: dict = {}
#: {(app, slug): {node ids}} — what it actually observed there.
_seen: dict = {}
#: [(test id, why)] — outcomes this run deliberately did NOT record, because it
#: could not name the artifact they were observed against. Session-scoped and
#: printed by conftest's terminal summary: a silent decline is the
#: hand-maintained-table failure the whole nest axis exists to prevent, and it is
#: reported apart from the mode-gate tally because these tests RAN — they were
#: excluded from the ledger, not from the suite.
_refusals: list = []

#: Worst-outcome-wins ordering within one session. Two parametrizations of the same
#: test collapse to one ledger line (the contract's ids carry no parametrization), so
#: a green `[tui-standalone]` must never overwrite a red `[tui-docker]` — the record
#: would then say the feature works when this very run saw it not.
_SEVERITY = {"error": 0, "failed": 1, "skipped": 2, "xfailed": 3, "xpassed": 4, "passed": 5}


def note_app(app: str) -> None:
    """This test just obtained a driver for `app` (called from the driver doors)."""
    if app:
        _apps_this_test.add(app)


def note_param_apps(apps) -> None:
    """The apps this test's own parametrization names (called from conftest's setup)."""
    _param_apps_this_test.clear()
    _param_apps_this_test.update(a for a in apps if a)


def note_skip_class(kind: str) -> None:
    """The declaration helper about to skip says which of convention 7's classes it is."""
    global _skip_class
    _skip_class = kind


def note_strict_refusal() -> None:
    """The unbuilt gate is about to FAIL the test under `--strict-app` (no body ran)."""
    global _strict_refusal
    _strict_refusal = True


def strict_refusal_this_test() -> bool:
    return _strict_refusal


def note_surface(key: str, *, fired: bool = False) -> None:
    """A `require_surface` call (or the `requires_surface` marker) reached `key`;
    `fired` when it is the gate that skipped (convention 7, *The surface registry*)."""
    global _surface
    if key not in _surfaces_this_test:
        _surfaces_this_test.append(key)
    if fired:
        _surface = key


def surface_this_test() -> str | None:
    return _surface


def surfaces_this_test() -> list:
    return list(_surfaces_this_test)


def note_own_artifact(reason: str) -> None:
    """This test stands up its OWN nest artifact, so its record must name THAT.

    A test that boots its own image (every `tests/platform/docker/` module, through
    `docker_build` → the tree's keyed `fauna-nest-test:src-<key>`) is not a witness
    of the run's nest in any mode: the image was built at a commit whose tree had
    the same image inputs — not necessarily this one — has no relation to the
    run's `--nest docker:<ref>` digest, and is a docker artifact however the run's
    mode reads. § The ledger licenses a record *because* it is first-party "on the
    commit it names, against the artifact it names", so such a test's record is
    written with the booted container's provenance (`own_artifact_resolution`),
    and declined when no single artifact can be named.
    """
    global _own_artifact
    if reason:
        _own_artifact = reason


def reset_test_state() -> None:
    global _skip_class, _own_artifact, _own_artifact_tallied, _closure_this_test, _surface
    global _strict_refusal
    _apps_this_test.clear()
    _param_apps_this_test.clear()
    _skip_class = None
    _strict_refusal = False
    _surface = None
    _surfaces_this_test.clear()
    _own_artifact = None
    _own_artifact_tallied = False
    _closure_this_test = frozenset()
    _artifacts_this_test.clear()


def apps_this_test() -> set:
    return set(_apps_this_test)


def attributed_apps() -> set:
    """The apps this test's outcome is recorded against (§ The marker).

    An app-parametrized test is attributed to the apps its parametrization names: a
    driver it launched for any other app is a helper seat playing the peer (a linux
    sender for a web receiver), and the outcome was observed on the app the run's
    app axis selected. Without an app parametrization every launched driver is a
    seat under test; with none launched either, the test is a `nest` witness.
    """
    return set(_param_apps_this_test or _apps_this_test) or {NEST}


def skip_class_this_test() -> str | None:
    return _skip_class


def own_artifact_this_test() -> str | None:
    return _own_artifact


def note_closure(names) -> None:
    """The test's real fixture closure, from conftest at setup (`_closure_this_test`)."""
    global _closure_this_test
    _closure_this_test = frozenset(names)


def enter_fixture_setup(name: str) -> None:
    """A fixture's setup is starting: the artifacts it observes belong to it, afresh."""
    _fixture_stack.append(name)
    _artifacts_by_fixture[name] = {}


def leave_fixture_setup(name: str) -> None:
    if _fixture_stack and _fixture_stack[-1] == name:
        _fixture_stack.pop()


def note_artifact(*, image_id: str, image_digest: str | None, version: str,
                  commit: str, base: str | None = None) -> dict:
    """A container of `image_id` just answered its health endpoint — remember what it is.

    Attached to every fixture currently being set up (normally exactly the
    container fixture whose body waited on health) or, outside any fixture
    setup, to the running test itself. Keyed by image id, so two containers of
    one image (a two-box topology, a restart) are one artifact. `base` is the
    image commit's own place in main's history (*Rebased commits*) — the caller's
    to ask, so that this door stays a dict write.
    """
    provenance = {"image_id": image_id, "image_digest": image_digest,
                  "version": version, "commit": commit, "base": base}
    if _fixture_stack:
        for name in _fixture_stack:
            _artifacts_by_fixture.setdefault(name, {})[image_id] = provenance
    else:
        _artifacts_this_test[image_id] = provenance
    return provenance


def note_container(container_name: str, health: dict) -> dict | None:
    """`note_artifact` for a real container: its image id and registry digest from
    `docker inspect`, its `version` / `commit` from the health body it served.

    Read off the CONTAINER, never the tag it was started from: a tag can be
    re-pointed by a sibling session mid-run, and the record has to name what this
    container actually ran. `None` (nothing noted) when docker cannot say — the
    per-report hook then declines the record with that as its reason, which is
    the visible failure a silent guess would hide.
    """
    image_id = _container_image_id(container_name)
    if not image_id:
        return None
    commit = str(health.get("commit") or "")
    # The image's commit has its own place in main's history, unrelated to this
    # checkout's HEAD — asked here, once per booted container, so the per-report
    # hook never runs git (*Rebased commits*). A `dev`/`unknown` commit is declined
    # downstream anyway; asking git about it would only be a wasted call.
    base = main_base(commit) if _SHA_RE.match(commit) else None
    return note_artifact(image_id=image_id, image_digest=image_digest_of(image_id),
                         version=str(health.get("version") or ""),
                         commit=commit, base=base)


def _container_image_id(container_name: str) -> str | None:
    try:
        out = subprocess.run(
            ["docker", "inspect", "--format", "{{.Image}}", container_name],
            capture_output=True, text=True, timeout=30, check=True).stdout.strip()
    except Exception:
        return None
    return out or None


def artifacts_this_test() -> dict:
    """{image id: provenance} — every artifact this test observed, through its own
    body or through the setup of any fixture in its closure."""
    observed = dict(_artifacts_this_test)
    for name in _closure_this_test:
        observed.update(_artifacts_by_fixture.get(name) or {})
    return observed


def own_artifact_resolution() -> tuple:
    """`(provenance, "")` — the ONE artifact an own-artifact test observed — or
    `(None, why)` when the writer must decline.

    The artifact a record names is a BUILD — the `(version, commit)` its
    container reported, which is all the stamp `<version>-dev+<commit> docker`
    can say — so every image id observed is folded by what it reported first
    (`feature-catalog.md` § The ledger, *One artifact is one build*): the updater
    bench republishes one saved image under several version labels, a new config
    digest and image id over the very same binary, and a test it moves the nest
    across sees two image ids of one build. The registry digest, which CAN tell
    such images apart, survives the fold only when every image observed carries
    the same one; otherwise it is `None`, as for a built-not-pulled image — no
    single digest names what was observed, and the field never guesses.

    The three declines are the three shapes in which a record would name no
    artifact, or the wrong one: no booted container reported its identity (a
    fixture that never went through `wait_for_health`); more than one distinct
    BUILD was observed (a record names exactly one, and picking would be a
    guess); the image reports no commit sha (a hand-built image — `dev` /
    `unknown` — whose record could not say what was built).
    """
    observed = artifacts_this_test()
    if not observed:
        return None, "no container it booted reported its identity through wait_for_health"
    builds: dict = {}
    for provenance in observed.values():
        key = (provenance.get("version") or "", provenance.get("commit") or "")
        builds.setdefault(key, []).append(provenance)
    if len(builds) > 1:
        names = sorted(f"{v or '?'}+{c[:8] if c else '?'}" for v, c in builds)
        return None, ("it observed {} distinct builds ({}) and a record names exactly one"
                      .format(len(builds), ", ".join(names)))
    images = next(iter(builds.values()))
    digests = {p.get("image_digest") for p in images}
    provenance = dict(images[0])
    if len(images) > 1:
        provenance["image_id"] = None
        provenance["image_digest"] = next(iter(digests)) if len(digests) == 1 else None
    commit = provenance.get("commit") or ""
    if not _SHA_RE.match(commit):
        return None, (f"its image reports commit={commit!r} — a build that names no "
                      "commit (a hand-built image; pull a published tag instead)")
    if not provenance.get("version"):
        return None, "its image reports no version"
    return provenance, ""


def note_refusal(test_id: str, reason: str) -> None:
    """Record — once per test — an outcome this run declined to write down.

    One test reaches the per-report hook up to three times (a setup error, a call
    outcome, a teardown failure), and this is a count of tests, not of reports.
    """
    global _own_artifact_tallied
    if _own_artifact_tallied:
        return
    _own_artifact_tallied = True
    _refusals.append((test_id, reason))


def refusals() -> list:
    """What this run observed but could not attribute, for the terminal summary."""
    return list(_refusals)


# ── the stamp ───────────────────────────────────────────────────────────────
_VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"', re.M)


def workspace_version(repo: Path = REPO) -> str:
    """`Cargo.toml [workspace.package] version` — the product version, one home.

    Read rather than passed in: a run cannot claim a version its own commit does not
    carry (`product-version.md` § Source of truth), which is also why the
    release-candidate run takes no version argument.
    """
    text = (repo / "Cargo.toml").read_text(encoding="utf-8")
    start = text.find("[workspace.package]")
    match = _VERSION_RE.search(text, start if start != -1 else 0)
    return match.group(1) if match else "0.0.0"


def head_commit(repo: Path = REPO) -> str:
    try:
        return subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "HEAD"],
            capture_output=True, text=True, timeout=10, check=True).stdout.strip()
    except Exception:
        return ""


def main_base(commit: str, repo: Path = REPO) -> str | None:
    """The newest commit on `origin/main` in `commit`'s own history — its merge-base
    with `MAIN_REF` as this checkout knows it — or `None` when git cannot say.

    § The ledger, *Rebased commits*: `commit` is what ran, and the pre-merge rebase
    replaces it whenever main moved during the run, so the record needs one sha
    that will still resolve afterwards. The merge-base is that sha by construction:
    it is an ancestor of the checkout's `origin/main`, which is an ancestor of the
    real one (main only ever fast-forwards), so it can never stop resolving; and it
    is a LOWER BOUND on the code the run saw — exactly what `dirty` makes `commit`
    mean, one level up. A checkout whose `origin/main` is stale records a base older
    than it need be, never a wrong one. No `origin/main` ref at all (a repo with no
    remote), no git, or no commit → `None`: unknown, never HEAD by default, because
    "this is on main" is the claim that needs proof.
    """
    if not commit:
        return None
    try:
        found = subprocess.run(
            ["git", "-C", str(repo), "merge-base", commit, MAIN_REF],
            capture_output=True, text=True, timeout=10)
    except Exception:
        return None
    if found.returncode != 0:
        return None
    return found.stdout.strip() or None


def tree_dirt(repo: Path = REPO) -> list | None:
    """`git status --porcelain` lines outside `docs/features/ledger/` — `[]` for a
    clean tree, `None` when git cannot say (no repository, no git).

    The ONE definition of dirt (§ The ledger, *Dirty trees*): the ordinary run marks
    its records by it and the release-candidate gate refuses by it, so the two can
    never disagree about what counts. Tracked modifications and untracked files
    alike — a new test file nobody committed is exactly the case — and the catalog's
    pages are NOT exempt: a page holds the contract the run judges completeness
    against. Measured once, at collection, beside the commit.
    """
    # `--untracked-files=all`: porcelain otherwise collapses a wholly-untracked
    # directory to one `?? dir/` line, which the per-file exclude cannot see into.
    try:
        status = subprocess.run(
            ["git", "-C", str(repo), "status", "--porcelain", "--untracked-files=all",
             "--", ".", f":(exclude){LEDGER_REL}"],
            capture_output=True, text=True, timeout=30)
    except Exception:
        return None
    if status.returncode != 0:
        return None
    return [line for line in status.stdout.splitlines() if line.strip()]


def is_dirty(dirt: list | None) -> bool:
    """Dirt → dirty; UNKNOWN → dirty too. Clean is the claim that needs proof: a record
    may say its commit names the code only when git positively said nothing moved."""
    return dirt is None or bool(dirt)


def host_platform() -> str:
    """`linux` / `macos` / `windows` — the host OS, never a host name (leak-check)."""
    system = platform.system().lower()
    return {"darwin": "macos"}.get(system, system)


def stamp(version: str, commit: str, nest_mode: str, *, release_candidate: bool,
          dirty: bool = False) -> str:
    """§ The ledger's stamp grammar.

    The `-dev+<sha>` form is semver build metadata, so a bare `<version> <mode>` stamp
    means — on its own, with nothing else to read — "witnessed against the artifact
    that carries that version". That is the whole point of the two forms. A dirty
    tree adds `.dirty` as a SECOND build-metadata identifier (dot-separated, so the
    string stays legal semver — a second `+` would not be): the commit is then a
    lower bound on the code the run saw, not its name (*Dirty trees*).
    """
    if release_candidate:
        if dirty:
            raise ValueError("a bare stamp claims the released artifact at its commit; a "
                             "dirty tree can never carry one — the candidate gate refuses "
                             "it before any stamp is made, and so does the grammar")
        return f"{version} {nest_mode}"
    metadata = [commit[:8]] if commit else []
    if dirty:
        metadata.append("dirty")
    if not metadata:
        return f"{version}-dev {nest_mode}"
    return f"{version}-dev+{'.'.join(metadata)} {nest_mode}"


# ── recording ───────────────────────────────────────────────────────────────
def note_collected(slug: str, node_id: str) -> None:
    """This run selected `node_id` as a witness of `slug`.

    The collected set is what makes the cell stamp honest: only a run that collected
    a feature's *whole* tagged set may re-stamp its cell (§ Cell semantics), so a
    `-k` subset, a deselection, or an environment skip updates per-test lines and
    leaves the cell alone. It is recorded per slug, not per app, because which app a
    test witnesses is not knowable at collection — the run decides it from the
    drivers the test launches.
    """
    _collected.setdefault(slug, set()).add(node_id)


def record(app: str, slug: str, node_id: str, *, outcome: str, skip_class: str | None,
           version: str, commit: str, base: str | None, image_digest: str | None,
           nest_mode: str, record_stamp: str, dirty: bool,
           today: str | None = None, surface: str | None = None,
           surfaces=()) -> None:
    """One observation. The provenance is the RECORD's — the run's for an ordinary
    test, the booted container's for an own-artifact one — which is why the stamp
    is passed per record rather than read from the run. `dirty` is the record's
    too: the run's tree state for an ordinary test, `False` for an own-artifact one
    (the image is built; this checkout's edits are not in it). So is `base`: the
    merge-base of THIS record's commit with `origin/main` (*Rebased commits*), the
    run's HEAD's for an ordinary test and the image commit's for an own-artifact
    one; `None` means unknown, and is written as `null` rather than dropped.

    `surface` / `surfaces` are the surface-registry keys (§ The ledger): the key the
    firing `require_surface` named, on a `skipped: unbuilt` record, and every key the
    test's calls reached. Both are ADDITIVE — written only when there is one, so a
    record of the pre-registry `skip_unbuilt` form, or of a test that reached no
    registry gate, carries neither, exactly as a record written before them."""
    key = (app, slug, node_id)
    previous = _records.get(key)
    _seen.setdefault((app, slug), set()).add(node_id)
    if previous and _SEVERITY.get(previous["outcome"], 9) <= _SEVERITY.get(outcome, 9):
        return  # a worse (or equal) outcome already stands for this key this session
    rec = {
        "outcome": outcome,
        "skip_class": skip_class,
        "stamp": record_stamp,
        "version": version,
        "commit": commit,
        "base": base or None,
        "image_digest": image_digest,
        "nest_mode": nest_mode,
        "dirty": bool(dirty),
        "date": today or date.today().isoformat(),
        "platform": host_platform(),
    }
    if surface and outcome == "skipped" and skip_class == UNBUILT:
        rec["surface"] = surface
    if surfaces:
        rec["surfaces"] = list(surfaces)
    _records[key] = rec


def _catalog_pages(repo: Path = REPO):
    """`(features_catalog module, pages)`, or `(None, None)` if it cannot be READ.

    "Cannot be read" — a missing tree, an unreadable page — is a different fact
    from "the catalog has no pages", and callers must not conflate them: an empty
    vocabulary would make every marker unknown and red a run for a reason that has
    nothing to do with the run.
    """
    import sys
    features = repo / "docs" / "features"
    scripts = repo / "scripts"
    if str(scripts) not in sys.path:
        sys.path.insert(0, str(scripts))
    try:
        import features_catalog as fc
        return fc, fc.load_pages(features)
    except Exception:
        return None, None


def contract_surfaces(repo: Path = REPO) -> dict:
    """`{slug: {"app": {node ids}, "nest": {node ids}}}` from the committed pages.

    The contract is the authority on which surface a witness is on (§ The two
    surfaces), and the whole-set check needs that split: an app's cell can only ever
    be complete against the feature's `[app]` witnesses, never against its `[nest]`
    ones, which no app run records.

    ⚠ `scan_marks` below AST-parses every test module in the tree, so this is
    seconds, not milliseconds — call it only when the per-column split is what you
    need. `known_slugs` used to, and did not (see its own note).
    """
    fc, pages = _catalog_pages(repo)
    if pages is None:
        return None
    marks, _surfaces, platform_marks = fc.scan_marks(repo)
    return {p.slug: {
        "app": set(p.surface_tests("app")),
        NEST: set(p.surface_tests(NEST)),
        # The per-column scopes — the ones `cell_complete` actually judges against
        # (§ Cell semantics, "the feature's whole tagged set FOR THAT APP"). The flat
        # `app` set above is the union across all seven and stays for callers asking
        # about the contract rather than about a column.
        "app_by_column": {app: set(p.surface_tests("app", app, marks)) for app in fc.APPS},
        # The per-(surface, platform) scopes and the legs they form (§ The ledger,
        # *Stale records*, rulings (h) and (i)): a witness under `tests/platform/<os>/`
        # or carrying `@pytest.mark.host_platform` runs on those OSes alone, so a
        # platform's set is the citations whose host platforms include it, and the
        # surface is complete once every leg holds a stamp. The flat sets above are
        # the whole contract and stay for callers asking about it.
        "nest_by_platform": fc.scope_by_platform(p, NEST),
        "nest_legs": fc.legs(p, NEST),
        "app_by_column_platform": {app: fc.scope_by_platform(p, app, marks, platform_marks)
                                   for app in fc.APPS},
        "app_legs": {app: fc.legs(p, app, marks, platform_marks) for app in fc.APPS},
    } for p in pages}


def known_slugs(repo: Path = REPO) -> set | None:
    """Every slug that has a page, or `None` when the catalog cannot be read.

    An EMPTY set is a real answer — "no page exists yet", which makes every marker a
    collection error, exactly as intended: the pages and the markers that name them
    land in the same commit (§ Implementation status today, step 3).

    ⚠ Reads the PAGES only. It went through `contract_surfaces` until 2026-09-05,
    which meant every pytest collection anywhere under `tests/e2e-unified/` paid an
    `ast.parse` + `ast.walk` of all ~757 test modules — to compute a per-column
    witness split this function then discarded with `set(...)`. Profiled on Windows: 1.7 s of the 3.4 s `payments-excision-spine-check` run, and the same
    toll on every other pytest gate in the tree, for a set of ~40 filenames. The
    conftest calls this at collection to validate `@pytest.mark.feature` slugs
    (`_apply_feature_axis`), so it is on the inner loop, not a gate path only.
    """
    _fc, pages = _catalog_pages(repo)
    return None if pages is None else {p.slug for p in pages}


def _load(path: Path) -> dict:
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8")).get("features") or {}
    except (json.JSONDecodeError, OSError):
        return {}


def _scope(app: str, slug: str, surfaces: dict, platform_: str) -> set:
    """The set a cell of `app` (a column, or `nest`) is judged against on `platform_`.

    Every surface is observed in LEGS, one per platform group (§ The ledger, *Stale
    records*, rulings (h) and (i)): a run is judged against its own platform's set,
    and a platform on no leg (its set a sibling's contains outright) has no entry, so
    it stamps nothing. A payload without the per-platform split is a hand-built one:
    the per-column set — or, without that either, the flat one — stands, exactly as
    an unmarked test is applicable to every column. `contract_surfaces` always
    provides both splits."""
    entry = surfaces.get(slug) or {}
    if app == NEST:
        by_platform = entry.get("nest_by_platform")
        if by_platform is not None:
            return by_platform.get(platform_) or set()
        return entry.get(NEST) or set()
    by_platform = (entry.get("app_by_column_platform") or {}).get(app)
    if by_platform is not None:
        return by_platform.get(platform_) or set()
    by_column = entry.get("app_by_column")
    return (by_column.get(app) if by_column is not None else entry.get("app")) or set()


def _legs(app: str, slug: str, surfaces: dict):
    """The legs `fold_platform_cells` folds a cell of `app` over — `None` for a
    payload built without them (every platform present folds)."""
    entry = surfaces.get(slug) or {}
    if app == NEST:
        return entry.get("nest_legs")
    return (entry.get("app_legs") or {}).get(app)


def cell_complete(app: str, slug: str, surfaces: dict, borrowed=frozenset()) -> bool:
    """Did this run observe the feature's WHOLE tagged set for this column?

    Only such a run may re-stamp the cell (§ Cell semantics), so the cell's stamp is
    always the stamp of a *complete* observation. Everything narrower — a `-k`
    subset, a deselection, an environment skip that wrote nothing — leaves it alone.

    **The set is this COLUMN's**, not the union across all seven apps. Most contracts
    witness one outcome once per app — a linux test, a windows test, a tui test — and
    no machine can collect all of those in one run (`--app sweep` is at most four
    apps, and windows and linux never share a box). Judging tui against the union
    therefore made every such cell permanently unstampable in every mode, which is
    what this reading is here to stop; § The two surfaces is explicit that an app
    outcome "counts for that column only".

    `borrowed` — the members an EARLIER run observed at this run's own clean stamp
    on this platform (`borrowable`; § Cell semantics, *one artifact, several runs*):
    they count as collected and observed, because the observation is of one
    artifact whichever invocation made it.
    """
    scope = _scope(app, slug, surfaces, host_platform())
    if not scope:
        return False
    # The set is the CONTRACT's, never this run's — intersecting the two would make
    # every `-k` subset look complete against itself, which is the exact dishonesty
    # the whole-set rule exists to stop. Two conditions, both explicit: the run
    # SELECTED the whole set, and it OBSERVED an outcome for each of them (an
    # environment skip records nothing, so it fails the second and leaves the stamp).
    borrowed = set(borrowed)
    return ((_collected.get(slug, set()) | borrowed) >= scope
            and (_seen.get((app, slug), set()) | borrowed) >= scope)


def borrowable(app: str, slug: str, tests: dict, run_records, surfaces: dict) -> dict:
    """`{node id: record}` — the members of this column's set this run did NOT
    observe, whose ledger record on this run's platform carries one of this run's
    own CLEAN stamps (§ Cell semantics, *one artifact, several runs*, ruled
    2026-11-01).

    Some sets cannot be collected by one invocation: conftest refuses a selection
    mixing a re-timed drafts witness with production-window ones, so drafts-survive
    was unstampable on every column. Two runs of one clean stamp observed one
    artifact — the stamp names commit, mode and cleanliness — so together they are
    the complete observation the whole-set rule asks for. A dirty stamp never lends
    or borrows (its commit is a lower bound; two dirty runs of one HEAD may have
    seen different edits), nor does another platform's record, nor another stamp.
    `tests` is the ledger entry's `tests` map, `run_records` this run's records for
    the (app, slug)."""
    run_records = list(run_records)
    clean = {r["stamp"] for r in run_records if r.get("stamp") and not r.get("dirty")}
    if not clean:
        return {}
    platform_ = run_records[0]["platform"]
    scope = _scope(app, slug, surfaces, platform_)
    seen = _seen.get((app, slug), set())
    out = {}
    for node in scope - seen:
        rec = (tests.get(node) or {}).get(platform_)
        if rec and not rec.get("dirty") and rec.get("stamp") in clean:
            out[node] = rec
    return out


def fold_platform_cells(platforms: dict, legs=None) -> dict:
    """One platform's stamp collides with another's exactly like one test's did
    (feature-catalog.md § The ledger, *Platform keying*): the top-level `cell`
    fields are the OLDEST of every platform's own complete-observation stamp —
    the same "shows the oldest artifact" rule the app/nest halves already
    follow — with every platform's own record kept underneath, undisturbed.

    `legs` — every file (§ The ledger, *Stale records*, rulings (h) and (i);
    `features_catalog.legs`): the platform groups that must EACH hold a stamp
    before the surface is a complete observation of the whole set. A leg with no
    stamped platform leaves the top level with no stamp, date or base and
    `complete: False` — every reader that keys on `stamp` then reads that half as
    unstamped, exactly as a cell the prune emptied — while every platform's own
    stamp stays underneath for the day the missing leg lands. `None` (a hand-built
    payload) folds every platform present.

    Public because the ledger's second committer — the merge-gate check pass, which
    upserts harvested cell stamps onto another commit's ledger (feature-catalog.md
    § The ledger, *Check-pass observations*) — must re-fold them exactly as this
    writer does."""
    if legs is not None and any(not (set(leg) & set(platforms)) for leg in legs):
        return {"complete": False, "platforms": platforms}
    oldest = min(platforms.values(), key=lambda c: (c.get("date") or "9999-99-99", c.get("stamp") or ""))
    return {**oldest, "platforms": platforms}


def write(ledger_dir: Path = LEDGER, *, today: str | None = None,
          surfaces: dict | None = None, repo: Path = REPO) -> list:
    """Merge this run's records into the per-app files. Returns the paths written.

    A merge, never a replace: this run drove some apps and some tests, and every line
    it did not touch records an observation that still stands. A test keyed only by
    (app, slug, node id) let a windows run silently overwrite a linux run's record
    for the same test — the ledger could then say "tui passed" or "tui failed", never
    "tui passes on linux and fails on windows", which is exactly the shape a
    platform-specific defect has (feature-catalog.md § The ledger, *Platform
    keying*, ruled 2026-09-06) — so `tests[node id]` is now `{platform: record}`,
    one merge-able slot per platform, and the cell gains the same split.
    """
    if not _records:
        return []
    ledger_dir.mkdir(parents=True, exist_ok=True)
    stamp_date = today or date.today().isoformat()
    if surfaces is None:
        surfaces = contract_surfaces() or {}

    by_app: dict = {}
    for (app, slug, node_id), rec in _records.items():
        by_app.setdefault(app, {}).setdefault(slug, {}).setdefault(node_id, {})[rec["platform"]] = rec

    written = []
    for app, features in sorted(by_app.items()):
        path = ledger_dir / f"{app}.json"
        existing = _load(path)
        for slug, tests in features.items():
            entry = existing.setdefault(slug, {"tests": {}})
            existing_tests = entry.setdefault("tests", {})
            for node_id, by_platform in tests.items():
                existing_tests.setdefault(node_id, {}).update(by_platform)
            own = [rec for by_platform in tests.values() for rec in by_platform.values()]
            borrowed = borrowable(app, slug, existing_tests, own, surfaces)
            if cell_complete(app, slug, surfaces, borrowed):
                # This run's own records all share one platform (a run has one
                # host), and so do the borrowed ones (`borrowable` reads only this
                # platform's slot), so flattening loses nothing about THIS observation.
                flat = own + list(borrowed.values())
                cell_stamp = cell_stamp_for(flat, repo=repo)
                if cell_stamp:
                    # The cell's `dirty` and `base` are those of the records whose
                    # stamp it carries — the marks travel with the stamp (§ The
                    # ledger, *Dirty trees*, *Rebased commits*). One stamp is one
                    # commit, so its records agree on a base; were they ever not to,
                    # the cell says unknown rather than picking one.
                    stamped = [r for r in flat if r.get("stamp") == cell_stamp]
                    bases = {r.get("base") or None for r in stamped}
                    this_platform = stamped[0]["platform"]
                    platforms = dict((entry.get("cell") or {}).get("platforms") or {})
                    platforms[this_platform] = {
                        "stamp": cell_stamp, "date": stamp_date, "complete": True,
                        "dirty": any(bool(r.get("dirty")) for r in stamped),
                        "base": next(iter(bases)) if len(bases) == 1 else None,
                    }
                    entry["cell"] = fold_platform_cells(platforms, legs=_legs(app, slug, surfaces))
        payload = {"schema": 2, "app": app, "features": existing}
        path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        written.append(path)
    return written


def cell_stamp_for(records, repo: Path = REPO) -> str | None:
    """The stamp a complete observation puts on its cell, or `None` to leave it alone.

    § Cell semantics: a cell's stamp names ONE artifact. When every record of the
    observation carries the same stamp — every ordinary feature, and a feature
    witnessed wholly under `tests/platform/docker/` — that is the cell's. When they
    do not (a feature with both `tests/api/` witnesses, recorded against the run's
    nest, and own-image witnesses, recorded against the booted image) the cell
    shows the OLDEST artifact — the one whose commit is a strict ancestor of every
    other's — the same "older of its stamps" rule the app and nest halves already
    follow, so the cell's claim is the weakest one the observation supports. Two
    stamps at that one commit (the image IS HEAD, under two modes) resolve the same
    way, by `_weakest_first`. Unrelated commits, or a commit this checkout cannot
    see, leave the cell alone: the per-test lines carry their own stamps and the
    previous cell stamp stands.
    """
    by_stamp: dict = {}
    for rec in records:
        if rec.get("stamp"):
            by_stamp.setdefault(rec["stamp"], []).append(rec)
    if len(by_stamp) == 1:
        return next(iter(by_stamp))
    commits = {recs[0].get("commit") or "" for recs in by_stamp.values()}
    if not all(_SHA_RE.match(c) for c in commits):
        return None
    oldest = next((commit for commit in commits
                   if all(_is_ancestor(commit, other, repo) for other in commits - {commit})),
                  None)
    if oldest is None:
        return None
    at_oldest = [(s, recs) for s, recs in by_stamp.items() if recs[0].get("commit") == oldest]
    return min(at_oldest, key=_weakest_first)[0]


def _weakest_first(item) -> tuple:
    """Sort key over `(stamp, its records)` at one commit, the weakest claim first.

    § Cell semantics. A dirty stamp comes first, because the cell's `dirty` is read
    from the records carrying the chosen stamp and a clean choice would hide dirty
    witnesses. Then the order of `nest_mode.MODES`, which runs from the box's own
    binary to a deployed nest. Then a `-dev` stamp before a bare one, because a bare
    stamp claims the released artifact and an own-image witness never saw it. The
    stamp itself settles the rest, so a choice never depends on record order.
    """
    from helpers.nest_mode import MODES

    cell_stamp, recs = item
    mode = recs[0].get("nest_mode")
    bare = cell_stamp == f"{recs[0].get('version')} {mode}"
    return (not any(r.get("dirty") for r in recs),
            MODES.index(mode) if mode in MODES else len(MODES),
            bare, cell_stamp)


def _is_ancestor(older: str, newer: str, repo: Path = REPO) -> bool:
    try:
        return subprocess.run(
            ["git", "-C", str(repo), "merge-base", "--is-ancestor", older, newer],
            capture_output=True, text=True, timeout=10).returncode == 0
    except Exception:
        return False


def _load_releases(path: Path) -> list:
    """The release table's rows, or `[]` for a file that does not exist yet.

    Deliberately NOT `_load`'s swallow-everything shape. A per-app ledger that cannot
    be parsed loses observations a re-run reproduces; the release table is the only
    record that a given artifact was ever tested, and rewriting it from `[]` would
    erase release history no run can regenerate. So a malformed file raises and the
    caller reports it, exactly as it would for any other corrupt record.
    """
    if not path.exists():
        return []
    data = json.loads(path.read_text(encoding="utf-8"))
    rows = data.get("releases") if isinstance(data, dict) else None
    if rows is None:
        raise ValueError(f"{path}: no `releases` array — refusing to overwrite it")
    return list(rows)


def _load_artifacts(path: Path) -> list:
    """The table's `artifacts` list (schema 2), or `[]` before the first entry."""
    if not path.exists():
        return []
    data = json.loads(path.read_text(encoding="utf-8"))
    return list(data.get("artifacts") or []) if isinstance(data, dict) else []


def record_release(ledger_dir: Path = LEDGER, *, version: str, commit: str,
                   image_digest: str | None, image: str | None = None,
                   public_commit: str | None = None,
                   today: str | None = None, correction: dict | None = None) -> Path:
    """Append this candidate run's row to the release table; return the file written.

    § Tag gate and release table. The row is `(version, commit, image_digest, date,
    candidate_run, promoted)` plus the exact `image` string the run drove — recorded rather than
    re-derived, so the pin the table later feeds names the tag that was actually
    tested rather than one reconstructed from a naming convention.

    **Upsert on (commit, image_digest), never append blindly.** A candidate run is
    re-runnable by design — "a fix → C′ → re-run; the same version string is legal
    until it ships" — and re-running the *same* artifact at the *same* commit is the
    ordinary case (a flake, a wider `--app` set, a second machine's leg). Those are
    one release, observed twice, so the date and `candidate_run` advance, `verified` and
    `promoted` are preserved; only a genuinely different (commit, digest) pair is a new
    row. `just release-verified` upserts into the same row on the same key. Without
    this the tag gate's "exactly one image digest at HEAD's commit" would be
    satisfiable one moment and ambiguous the next, for no reason but a repeat run.

    **A public build is keyed on the private commit too.** The public repository
    builds its own artifact of the published tree, so its image carries the PUBLIC
    commit; the run that accepted it (`release_candidate_verdict_for_image`, paired by
    the Sync-Point trailer) passes that commit here as `public_commit`, recorded beside
    the digest it names (§ Tag gate and release table, "Promotion copies the tested
    digest").

    **A correction run appends to the version's row and moves nothing else.** Given
    `correction` (`{date, carried, excluded}`, `release_gate.record_candidate`), the
    row keyed on the version's commit — never the correction tree's — gains one entry
    in its `corrections` list; its date and `candidate_run` stay the version's own
    run's (release-table.md § Release-candidate run, the correction-run paragraph).
    """
    ledger_dir.mkdir(parents=True, exist_ok=True)
    path = ledger_dir / "releases.json"
    releases = _load_releases(path)
    stamp_date = today or date.today().isoformat()

    row = None
    for existing in releases:
        if existing.get("commit") == commit and existing.get("image_digest") == image_digest:
            row = existing
            break
    if row is None:
        row = {"promoted": False}
        releases.append(row)
    if correction is None or not row.get("candidate_run"):
        row.update({"version": version, "commit": commit, "image_digest": image_digest,
                    "date": stamp_date, "candidate_run": stamp_date})
    if correction is not None:
        row.setdefault("corrections", []).append(
            {"date": correction.get("date") or stamp_date,
             "carried": list(correction.get("carried") or []),
             "excluded": dict(sorted((correction.get("excluded") or {}).items()))})
    if image:
        row["image"] = image
    if public_commit:
        row["public_commit"] = public_commit
    row.setdefault("promoted", False)

    # Schema 2 carries the artifacts of each version beside the rows; this writer
    # keeps them as they stand (release-table.md § Tag gate and release table, *The
    # artifacts of a version*) and writes them in the order their own writer does.
    artifacts = _load_artifacts(path)
    artifacts.sort(key=lambda e: (tuple(int(p) for p in e["version"].split(".")),
                                  e["artifact"]))
    path.write_text(json.dumps({"schema": 2, "releases": releases, "artifacts": artifacts},
                               indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def reset_session_state() -> None:
    """Test-only: drop everything accumulated so a unit test starts clean."""
    _records.clear()
    _collected.clear()
    _seen.clear()
    _refusals.clear()
    _fixture_stack.clear()
    _artifacts_by_fixture.clear()
    reset_test_state()


def resolved_image_ref(mode) -> str | None:
    """The image a `docker`-mode run actually boots (or would boot), by asking
    the mode's own PROVIDER rather than reading `mode.argument` alone.

    `mode.argument` when `--nest docker:<ref>` named one, else the provider's
    own default (`_DockerProvider.default_image`, the tree's keyed
    `fauna-nest-test:src-<key>`) —
    the same three-way resolution `_DockerProvider._resolve_image` boots
    against, factored out as `_DockerProvider.image_ref` so this collection-time
    caller gets the ref without the boot-time presence check (which shells out
    and raises). `None` outside docker mode, and when no provider is registered
    for it (an unbuilt mode in this pytest process) — duck-typed via
    `getattr(provider, "image_ref", None)` so a test double without the method
    answers `None` rather than raising.

    Shared by [`image_digest_for`] and `_apply_feature_axis`'s own `"image"`
    field — one resolution, not two that could disagree.
    """
    if not getattr(mode, "is_docker", False):
        return None
    from helpers import nest_mode as nest_mode_mod

    try:
        provider = nest_mode_mod.provider_for(mode)
    except nest_mode_mod.NestModeError:
        return None
    image_ref = getattr(provider, "image_ref", None)
    if image_ref is None:
        return None
    return image_ref(mode) or None


def image_digest_for(mode) -> str | None:
    """The `sha256:` registry digest of the nest image a `docker`-mode run used.

    The ref is [`resolved_image_ref`] — the provider's own resolved image,
    never `mode.argument` alone. A plain `--nest docker` still boots and runs
    against a real image; asking only the argument answered `null` on every
    one of those runs even though a pulled `:latest` genuinely carries a
    digest, which reads as "built rather than pulled" when it was
    neither.

    `None` whenever `resolved_image_ref` is, and for a genuinely locally built
    image, which has no `RepoDigests` at all — that absence is exactly what
    stops a local build from ever being mistaken for a release artifact
    (§ Release-candidate run).
    """
    image = resolved_image_ref(mode)
    if not image:
        return None
    return image_digest_of(image)


def image_digest_of(ref: str) -> str | None:
    """The `sha256:` registry digest of one image, by tag or by id — `None` for an
    image with no `RepoDigests`, i.e. one built rather than pulled (the absence
    `image_digest_for` above relies on, addressed by image instead of by run)."""
    try:
        out = subprocess.run(
            ["docker", "image", "inspect", "--format", "{{json .RepoDigests}}", ref],
            capture_output=True, text=True, timeout=30, check=True).stdout.strip()
        digests = json.loads(out) or []
    except Exception:
        return None
    for entry in digests:
        _, _, digest = entry.partition("@")
        if digest:
            return digest
    return None


def release_candidate_refusal(mode, repo: Path = REPO,
                              public_clone: Path | None = None) -> str | None:
    """Why this run may NOT stamp a bare version, or `None` if it may.

    The three preconditions of § Release-candidate run, re-verified here rather than
    trusted from the recipe: the recipe is the convenient door, the plugin is the
    gate. A bare `<version> <mode>` stamp is read, on its own, as "witnessed against
    the byte-identical released artifact", so nothing but a real check may produce
    one.

    `just e2e-release-candidate` refuses the same three up front (through
    `release_candidate_refusal_for_image` below — one implementation, two callers, so
    the recipe cannot drift into a weaker check than the plugin's) and never reaches
    this. Running both is deliberate defence in depth, not duplication: the recipe
    saves a wasted docker-mode run, and this one is what actually guards the stamp.
    """
    return release_candidate_verdict(mode, repo=repo, public_clone=public_clone)[0]


def release_candidate_verdict(mode, repo: Path = REPO,
                              public_clone: Path | None = None) -> tuple:
    """`(refusal, public_commit)` for a run mode: `release_candidate_refusal` plus the
    public build commit the release row records beside the digest — `None` for a
    private build, whose `FAUNA_BUILD_COMMIT` IS the row's commit."""
    check = release_candidate_check(mode, repo=repo, public_clone=public_clone)
    return check.refusal, check.public_commit


def release_candidate_check(mode, repo: Path = REPO,
                            public_clone: Path | None = None) -> CandidateCheck:
    """`release_candidate_check_for_image` for a run mode — what the plugin asks, so
    the release row can be keyed on the version's commit of a correction tree."""
    if not getattr(mode, "is_docker", False):
        return CandidateCheck(
            "a candidate run goes against the nest IMAGE (`--nest docker:<image>`)")
    image = getattr(mode, "argument", None)
    if not image:
        return CandidateCheck(
            "`--nest docker` names no image; a candidate run needs the exact tag")
    return release_candidate_check_for_image(image, repo=repo, public_clone=public_clone)


def release_candidate_refusal_for_image(image: str, repo: Path = REPO,
                                        public_clone: Path | None = None) -> str | None:
    """The same three preconditions, addressed by image rather than by run mode.

    Split out so `just e2e-release-candidate` can refuse before it spends a docker-mode
    e2e run finding out. Everything below this line is mode-independent — it asks about
    the tree and the artifact, never about the harness.
    """
    return release_candidate_verdict_for_image(image, repo=repo,
                                               public_clone=public_clone)[0]


#: The test-side paths a correction tree may change beyond the version's commit
#: (release-table.md § Release-candidate run → *A test-side correction after the cut
#: reaches the verdict through a correction run*): the test tree, the run's own ledger
#: output, and Rust compiled only under the `e2e-harness` feature — each Rust entry
#: pinned behind that `cfg` by the release gate's tests — and the release gate's own
#: script, which writes the run's record and ships in no artifact (a correction tree
#: branched at a version's commit must carry the gate that accepts it; the entry is
#: excised from the public tree, which carries no gate). A directory ends in `/`;
#: anything else names one file.
TEST_SIDE_PATHS = (
    "tests/",
    "docs/features/ledger/",
    "libs/fauna-ffi/src/cabi/",
)

_CHERRY_PICK_RE = re.compile(r"\(cherry picked from commit ([0-9a-f]{7,40})\)")


class CandidateCheck(NamedTuple):
    """The candidate precondition's whole answer.

    `refusal` is `None` when the run may stamp a bare version; `public_commit` is the
    public build commit recorded beside the digest (`None` for a private build);
    `commit` is the VERSION's commit the release row is keyed on — HEAD for the two
    older shapes, the commit beneath the carried commits for a correction tree; and
    `carried` the main commits a correction tree carries (empty otherwise)."""

    refusal: str | None
    public_commit: str | None = None
    commit: str | None = None
    carried: list = []


def release_candidate_verdict_for_image(image: str, repo: Path = REPO,
                                        public_clone: Path | None = None) -> tuple:
    """`(refusal, public_commit)` for an image — `release_candidate_check_for_image`
    without the version commit and the carried commits."""
    check = release_candidate_check_for_image(image, repo=repo, public_clone=public_clone)
    return check.refusal, check.public_commit


def _is_test_side(path: str) -> bool:
    return any(path.startswith(entry) if entry.endswith("/") else path == entry
               for entry in TEST_SIDE_PATHS)


def _git_ok(repo: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(["git", "-C", str(repo), *args],
                          capture_output=True, text=True, timeout=60)


def correction_refusal(version_commit: str, head: str, repo: Path = REPO,
                       main_ref: str = "origin/main") -> tuple:
    """`(refusal, carried)` — why HEAD is NOT a correction tree of `version_commit`.

    A correction tree is the version's commit plus commits that (i) change only
    `TEST_SIDE_PATHS` and (ii) each carry the `(cherry picked from commit <sha>)`
    trailer of a commit that is an ancestor of `main_ref`: a correction lands on main
    first and is carried back, never written on the side. `carried` lists those main
    commits, oldest first, when the tree passes."""
    if _git_ok(repo, "merge-base", "--is-ancestor", version_commit, head).returncode:
        return (f"the version's commit {version_commit[:12]} is not an ancestor of HEAD "
                f"{head[:12]}"), []
    changed = _git_ok(repo, "diff", "--name-only", version_commit, head)
    if changed.returncode:
        return f"`git diff` {version_commit[:12]}..HEAD failed", []
    product = [p for p in changed.stdout.splitlines() if p and not _is_test_side(p)]
    if product:
        shown = ", ".join(product[:6]) + (f", … {len(product) - 6} more"
                                          if len(product) > 6 else "")
        return (f"HEAD changes paths outside the test side beyond the version's commit "
                f"{version_commit[:12]} ({shown}) — a change there is a new commit, a "
                f"new image and a new run"), []
    revs = _git_ok(repo, "rev-list", "--reverse", f"{version_commit}..{head}")
    if revs.returncode:
        return f"`git rev-list` {version_commit[:12]}..HEAD failed", []
    carried = []
    for rev in revs.stdout.split():
        body = _git_ok(repo, "log", "-1", "--format=%B", rev).stdout
        found = _CHERRY_PICK_RE.findall(body)
        if not found:
            return (f"commit {rev[:12]} carries no `(cherry picked from commit <sha>)` "
                    f"trailer — a correction lands on main first and is carried back "
                    f"with `git cherry-pick -x`"), []
        source = found[-1]
        if _git_ok(repo, "merge-base", "--is-ancestor", source, main_ref).returncode:
            return (f"commit {rev[:12]} was cherry-picked from {source[:12]}, which is "
                    f"not on {main_ref} — a correction lands on main first"), []
        resolved = _git_ok(repo, "rev-parse", f"{source}^{{commit}}").stdout.strip()
        carried.append(resolved or source)
    return None, carried


def release_candidate_check_for_image(image: str, repo: Path = REPO,
                                      public_clone: Path | None = None,
                                      main_ref: str = "origin/main") -> CandidateCheck:
    """The candidate precondition for an image — see `release_candidate_verdict`.

    The commit precondition has three passing shapes (release-table.md §
    Release-candidate run): the image was built from HEAD itself; it is the PUBLIC
    repository's build of the tree published from HEAD — its `FAUNA_BUILD_COMMIT` a
    public commit whose `Sync-Point:` trailer names HEAD (`public_build_refusal`); or
    HEAD is a CORRECTION TREE of the commit either older shape would accept
    (`correction_refusal`). The row is keyed on that private version commit every
    time; the public shape also hands back the public commit to record beside the
    digest, the correction shape the main commits it carries.
    """
    # The same `tree_dirt` the ordinary run MARKS by — one definition of dirt, two
    # callers, so the gate can never call clean what the mark calls dirty or vice
    # versa (§ The ledger, *Dirty trees*). The gate's answer to dirt is a refusal
    # where the run's is a mark, because a bare stamp claims the released artifact.
    dirt = tree_dirt(repo)
    if dirt is None:
        return CandidateCheck("`git status` failed; the tree must be provably clean")
    if dirt:
        return CandidateCheck("the working tree is not clean")

    try:
        env, digests = _inspect_image(image)
    except Exception as exc:
        return CandidateCheck(f"`docker inspect {image}` failed ({exc})")

    build_commit = ""
    for entry in env:
        key, _, value = entry.partition("=")
        if key == "FAUNA_BUILD_COMMIT":
            build_commit = value.strip()
    head = head_commit(repo)
    if not build_commit:
        return CandidateCheck(f"{image} carries no FAUNA_BUILD_COMMIT")
    public_commit = None
    version_commit = head
    carried: list = []
    if not head or build_commit != head:
        unpaired = (public_build_refusal(build_commit, head, public_clone=public_clone,
                                         repo=repo)
                    if head else "HEAD is unknown")
        if unpaired:
            # The third shape: the commit an older shape would accept, under HEAD.
            version_commit, public_commit, why_not = _version_commit_under(
                build_commit, head, repo, public_clone) if head else (None, None, "")
            correction = ("no version commit to correct" + (f" ({why_not})" if why_not
                                                            else ""))
            if version_commit:
                correction, carried = correction_refusal(version_commit, head, repo,
                                                         main_ref)
            if correction:
                return CandidateCheck(
                    f"{image} was built from {build_commit[:12]}, HEAD is "
                    f"{(head or '?')[:12]} — a candidate run witnesses its own commit, "
                    f"the public build of the tree published from it, or a correction "
                    f"tree of either, and this is none: {unpaired}; as a correction "
                    f"tree: {correction}")
        else:
            public_commit = build_commit
    if not digests:
        return CandidateCheck(
            f"{image} has no registry digest — a locally built image has none, and "
            "a candidate is always the immutable `:sha-<commit>` tag the pipeline "
            "pushed")
    return CandidateCheck(None, public_commit, version_commit, carried)


def _version_commit_under(build_commit: str, head: str, repo: Path,
                          public_clone: Path | None) -> tuple:
    """`(version commit, public commit, why not)` — the private commit an older shape
    would accept for this image, when it lies under HEAD: the build commit itself for a
    private build, or the commit a public build's Sync-Point names."""
    if not _git_ok(repo, "merge-base", "--is-ancestor", build_commit, head).returncode:
        return build_commit, None, ""
    short, why = sync_point_of(build_commit, public_clone=public_clone, repo=repo)
    if not short:
        return None, None, (f"{build_commit[:12]} is not under HEAD, and as a public "
                            f"build: {why}")
    full = _git_ok(repo, "rev-parse", "--verify", "-q", f"{short}^{{commit}}").stdout.strip()
    if not full:
        return None, None, f"the Sync-Point {short} names no commit of this repository"
    return full, build_commit, ""


def _inspect_image(image: str) -> tuple:
    """`(Config.Env, RepoDigests)` off `docker inspect`; raises when docker cannot say."""
    inspected = subprocess.run(
        ["docker", "inspect", "--format",
         "{{json .Config.Env}}\t{{json .RepoDigests}}", image],
        capture_output=True, text=True, timeout=60, check=True).stdout.strip()
    env_json, _, digests_json = inspected.partition("\t")
    return json.loads(env_json) or [], json.loads(digests_json) or []


# ── the public build's pairing: a public commit → the private commit it published ──
_SYNC_POINT_RE = re.compile(r"^Sync-Point:[ \t]*([0-9a-f]{7,40})[ \t]*$",
                            re.IGNORECASE | re.MULTILINE)


def mirror_dir_default(repo: Path = REPO) -> Path:
    """`<dev-parent>/fauna`, the standing mirror that holds the public history.

    The same derivation as the publish tool's own `mirror_dir_default` — from the
    git COMMON dir, so every linked checkout of the repository lands on the same
    path as the main one: the sibling `fauna` of the directory holding the checkout."""
    try:
        common = subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "--path-format=absolute",
             "--git-common-dir"],
            capture_output=True, text=True, timeout=10, check=True).stdout.strip()
    except Exception:
        common = str(repo / ".git")
    return Path(common).parent.parent / "fauna"


def sync_point_of(public_commit: str, public_clone: Path | None = None,
                  repo: Path = REPO) -> tuple:
    """`(private short sha, reason)` — the `Sync-Point:` trailer of `public_commit`.

    Every public sync commit the publish tool creates ends with
    `Sync-Point: <short sha of the private rev it snapshotted>`, and the squash-merge
    inherits the message verbatim, so the trailer IS the pairing and nothing else has
    to record it. Read in `public_clone` (default the standing mirror,
    `mirror_dir_default`). `(None, why)` when the clone, the commit or the trailer is
    missing — never a guess.
    """
    clone = Path(public_clone) if public_clone else mirror_dir_default(repo)
    if not clone.is_dir():
        return None, (f"no public clone at {clone} to read the Sync-Point trailer from "
                      "(pass --public-clone)")
    try:
        out = subprocess.run(
            ["git", "-C", str(clone), "log", "-1", "--format=%B",
             f"{public_commit}^{{commit}}", "--"],
            capture_output=True, text=True, timeout=30)
    except Exception as exc:
        return None, f"`git log` in {clone} failed ({exc})"
    if out.returncode != 0:
        return None, (f"public commit {public_commit[:12]} is not in {clone} "
                      "(fetch the public repository there, or pass --public-clone)")
    found = _SYNC_POINT_RE.findall(out.stdout)
    if not found:
        return None, (f"public commit {public_commit[:12]} carries no Sync-Point "
                      "trailer — `fauna-publish pr` did not make it")
    return found[-1].lower(), ""


def public_build_refusal(public_commit: str, private_commit: str,
                         public_clone: Path | None = None,
                         repo: Path = REPO) -> str | None:
    """Why `public_commit` is NOT the public build of `private_commit`, or `None`.

    Paired when the public commit's Sync-Point names the private commit (a short-sha
    prefix of it). The one check both the candidate run and `just release-promoted
    --public-commit` make, so the two can never disagree on what pairs."""
    sync_point, why = sync_point_of(public_commit, public_clone=public_clone, repo=repo)
    if sync_point is None:
        return why
    if not private_commit or not private_commit.lower().startswith(sync_point):
        return (f"public commit {public_commit[:12]} was published from {sync_point} "
                f"(its Sync-Point), not {(private_commit or '?')[:12]}")
    return None


def enabled(config) -> bool:
    """On by default — `--no-feature-ledger` opts a run out (§ The ledger).

    A run that observed something and did not record it is the failure the catalog
    exists to prevent, so the default has to be *on*; the opt-out is for the case
    where a session deliberately does not want its scratch run in the record.
    """
    if os.environ.get("FAUNA_NO_FEATURE_LEDGER"):
        return False
    return not bool(config.getoption("no_feature_ledger", default=False))
