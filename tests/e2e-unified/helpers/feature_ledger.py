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

* **App attribution is decided by the run, not by a marker** (§ The marker). A test
  is attributed to *every* app whose driver it launched — a multi-seat test that
  drives three desktop apps witnesses all three — and a test that launched no driver
  is a `nest` witness. `note_app()` is called from the two doors every driver comes
  through, so no test has to remember.
* **`skip_environment` writes nothing** (§ Cell semantics). A missing credential says
  nothing about the feature, so the previous ledger line must stand rather than be
  overwritten with a skip. That is why the three declaration helpers call
  `note_skip_class()` instead of the writer parsing skip messages.
* **A test that brought its OWN nest artifact is recorded against THAT artifact,
  never against the run's.** Provenance is per record: an ordinary record carries
  the run's `commit` / `image_digest` / `nest_mode`, and a `tests/platform/docker/`
  test — which boots `fauna-nest-test:local`, whatever that tag holds on the box —
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
  artifact was observed (a record names exactly one); the image reports no commit
  sha (a hand-built image). Owner of the class and the rulings:
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
_skip_class: str | None = None
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


def note_skip_class(kind: str) -> None:
    """The declaration helper about to skip says which of convention 7's classes it is."""
    global _skip_class
    _skip_class = kind


def note_own_artifact(reason: str) -> None:
    """This test stands up its OWN nest artifact, so its record must name THAT.

    A test that boots its own image (every `tests/platform/docker/` module, through
    `docker_build` → `fauna-nest-test:local`) is not a witness of the run's nest in
    any mode: the image was built at some other commit, has no relation to the
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
    global _skip_class, _own_artifact, _own_artifact_tallied, _closure_this_test
    _apps_this_test.clear()
    _skip_class = None
    _own_artifact = None
    _own_artifact_tallied = False
    _closure_this_test = frozenset()
    _artifacts_this_test.clear()


def apps_this_test() -> set:
    return set(_apps_this_test)


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

    The three declines are the three shapes in which a record would name no
    artifact, or the wrong one (`feature-catalog.md` § The ledger): no booted
    container reported its identity (a fixture that never went through
    `wait_for_health`); more than one distinct artifact was observed (a record
    names exactly one, and picking would be a guess); the image reports no commit
    sha (a hand-built image — `dev` / `unknown` — whose record could not say what
    was built).
    """
    observed = artifacts_this_test()
    if not observed:
        return None, "no container it booted reported its identity through wait_for_health"
    if len(observed) > 1:
        return None, ("it observed {} distinct artifacts ({}) and a record names exactly one"
                      .format(len(observed), ", ".join(sorted(observed))))
    provenance = next(iter(observed.values()))
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
           today: str | None = None) -> None:
    """One observation. The provenance is the RECORD's — the run's for an ordinary
    test, the booted container's for an own-artifact one — which is why the stamp
    is passed per record rather than read from the run. `dirty` is the record's
    too: the run's tree state for an ordinary test, `False` for an own-artifact one
    (the image is built; this checkout's edits are not in it). So is `base`: the
    merge-base of THIS record's commit with `origin/main` (*Rebased commits*), the
    run's HEAD's for an ordinary test and the image commit's for an own-artifact
    one; `None` means unknown, and is written as `null` rather than dropped."""
    key = (app, slug, node_id)
    previous = _records.get(key)
    _seen.setdefault((app, slug), set()).add(node_id)
    if previous and _SEVERITY.get(previous["outcome"], 9) <= _SEVERITY.get(outcome, 9):
        return  # a worse (or equal) outcome already stands for this key this session
    _records[key] = {
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

    ⚠ `scan_app_marks` below AST-parses every test module in the tree, so this is
    seconds, not milliseconds — call it only when the per-column split is what you
    need. `known_slugs` used to, and did not (see its own note).
    """
    fc, pages = _catalog_pages(repo)
    if pages is None:
        return None
    marks = fc.scan_app_marks(repo)
    return {p.slug: {
        "app": set(p.surface_tests("app")),
        NEST: set(p.surface_tests(NEST)),
        # The per-column scopes — the ones `cell_complete` actually judges against
        # (§ Cell semantics, "the feature's whole tagged set FOR THAT APP"). The flat
        # `app` set above is the union across all seven and stays for callers asking
        # about the contract rather than about a column.
        "app_by_column": {app: set(p.surface_tests("app", app, marks)) for app in fc.APPS},
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


def cell_complete(app: str, slug: str, surfaces: dict) -> bool:
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
    """
    entry = surfaces.get(slug) or {}
    if app == NEST:
        scope = entry.get(NEST) or set()
    else:
        # A payload with no per-column split is one built without marker knowledge;
        # its flat set is then the honest answer, exactly as an unmarked test is
        # applicable to every column. `contract_surfaces` always provides the split.
        by_column = entry.get("app_by_column")
        scope = (by_column.get(app) if by_column is not None else entry.get("app")) or set()
    if not scope:
        return False
    # The set is the CONTRACT's, never this run's — intersecting the two would make
    # every `-k` subset look complete against itself, which is the exact dishonesty
    # the whole-set rule exists to stop. Two conditions, both explicit: the run
    # SELECTED the whole set, and it OBSERVED an outcome for each of them (an
    # environment skip records nothing, so it fails the second and leaves the stamp).
    return (_collected.get(slug, set()) >= scope
            and _seen.get((app, slug), set()) >= scope)


def fold_platform_cells(platforms: dict) -> dict:
    """One platform's stamp collides with another's exactly like one test's did
    (feature-catalog.md § The ledger, *Platform keying*): the top-level `cell`
    fields are the OLDEST of every platform's own complete-observation stamp —
    the same "shows the oldest artifact" rule the app/nest halves already
    follow — with every platform's own record kept underneath, undisturbed.

    Public because the ledger's second committer — the merge-gate check pass, which
    upserts harvested cell stamps onto another commit's ledger (feature-catalog.md
    § The ledger, *Check-pass observations*) — must re-fold them exactly as this
    writer does."""
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
            if cell_complete(app, slug, surfaces):
                # This run's own records all share one platform (a run has one
                # host), so flattening loses nothing about THIS observation.
                flat = [rec for by_platform in tests.values() for rec in by_platform.values()]
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
                    entry["cell"] = fold_platform_cells(platforms)
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
    artifacts at one commit (the image IS HEAD, under two modes), unrelated
    commits, or a commit this checkout cannot see leave the cell alone: the
    per-test lines carry their own stamps and the previous cell stamp stands.
    """
    by_stamp: dict = {}
    for rec in records:
        if rec.get("stamp"):
            by_stamp[rec["stamp"]] = rec.get("commit") or ""
    if len(by_stamp) == 1:
        return next(iter(by_stamp))
    commits = list(by_stamp.values())
    if len(set(commits)) != len(commits) or not all(_SHA_RE.match(c) for c in commits):
        return None
    for cell_stamp, commit in by_stamp.items():
        if all(_is_ancestor(commit, other, repo) for other in commits if other != commit):
            return cell_stamp
    return None


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


def record_release(ledger_dir: Path = LEDGER, *, version: str, commit: str,
                   image_digest: str | None, image: str | None = None,
                   today: str | None = None) -> Path:
    """Append this candidate run's row to the release table; return the file written.

    § Tag gate and release table. The row is `(version, commit, image_digest, date,
    promoted)` plus the exact `image` string the run drove — recorded rather than
    re-derived, so the pin the table later feeds names the tag that was actually
    tested rather than one reconstructed from a naming convention.

    **Upsert on (commit, image_digest), never append blindly.** A candidate run is
    re-runnable by design — "a fix → C′ → re-run; the same version string is legal
    until it ships" — and re-running the *same* artifact at the *same* commit is the
    ordinary case (a flake, a wider `--app` set, a second machine's leg). Those are
    one release, observed twice, so the row's date advances and `promoted` is
    preserved; only a genuinely different (commit, digest) pair is a new row. Without
    this the tag gate's "exactly one image digest at HEAD's commit" would be
    satisfiable one moment and ambiguous the next, for no reason but a repeat run.
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
    row.update({"version": version, "commit": commit, "image_digest": image_digest,
                "date": stamp_date})
    if image:
        row["image"] = image
    row.setdefault("promoted", False)

    path.write_text(json.dumps({"schema": 1, "releases": releases}, indent=2,
                               sort_keys=True) + "\n", encoding="utf-8")
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
    own default (`_DockerProvider.DEFAULT_IMAGE`, `fauna-nest-test:local`) —
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


def release_candidate_refusal(mode, repo: Path = REPO) -> str | None:
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
    if not getattr(mode, "is_docker", False):
        return "a candidate run goes against the nest IMAGE (`--nest docker:<image>`)"
    image = getattr(mode, "argument", None)
    if not image:
        return "`--nest docker` names no image; a candidate run needs the exact tag"
    return release_candidate_refusal_for_image(image, repo=repo)


def release_candidate_refusal_for_image(image: str, repo: Path = REPO) -> str | None:
    """The same three preconditions, addressed by image rather than by run mode.

    Split out so `just e2e-release-candidate` can refuse before it spends a docker-mode
    e2e run finding out. Everything below this line is mode-independent — it asks about
    the tree and the artifact, never about the harness.
    """
    # The same `tree_dirt` the ordinary run MARKS by — one definition of dirt, two
    # callers, so the gate can never call clean what the mark calls dirty or vice
    # versa (§ The ledger, *Dirty trees*). The gate's answer to dirt is a refusal
    # where the run's is a mark, because a bare stamp claims the released artifact.
    dirt = tree_dirt(repo)
    if dirt is None:
        return "`git status` failed; the tree must be provably clean"
    if dirt:
        return "the working tree is not clean"

    try:
        inspected = subprocess.run(
            ["docker", "inspect", "--format",
             "{{json .Config.Env}}\t{{json .RepoDigests}}", image],
            capture_output=True, text=True, timeout=60, check=True).stdout.strip()
        env_json, _, digests_json = inspected.partition("\t")
        env = json.loads(env_json) or []
        digests = json.loads(digests_json) or []
    except Exception as exc:
        return f"`docker inspect {image}` failed ({exc})"

    build_commit = ""
    for entry in env:
        key, _, value = entry.partition("=")
        if key == "FAUNA_BUILD_COMMIT":
            build_commit = value.strip()
    head = head_commit(repo)
    if not build_commit:
        return f"{image} carries no FAUNA_BUILD_COMMIT"
    if not head or build_commit != head:
        return (f"{image} was built from {build_commit[:12]}, HEAD is "
                f"{(head or '?')[:12]} — a candidate run witnesses its own commit")
    if not digests:
        return (f"{image} has no registry digest — a locally built image has none, and "
                "a candidate is always the immutable `:sha-<commit>` tag the pipeline "
                "pushed")
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
