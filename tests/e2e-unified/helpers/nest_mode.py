"""The nest-mode axis — what fills the real-nest slot for this run.

`testing.md` § Default app and nest mode, *Mode mechanics (ratified 2026-08-01)*.
Sibling of `app_surface.py`: that module owns the *app* axis's honesty machinery,
this one owns the *nest* axis's.

    standalone   the harness spawns the locally-built `fauna-nest` binary.
                 Seconds to start, fresh private state — the red-green
                 debugging inner loop. THE DEFAULT.
    docker       the real image + `docker/` sidecars under s6 supervision.
                 Heavy to build, then ephemeral: the true deployment artifact,
                 provable before a production release.
    live         an already-deployed real box (default example.com). Zero
                 provisioning, but shared and STATEFUL — the only mode that
                 catches works-on-fresh-only bugs.

Modes never differ in wire, at-rest, or configuration semantics (the only config
surface is the apps — `principles.md` § One configuration surface; mode selection
is bucket-1 artifact wiring, never a production knob). What varies is packaging
and supervision (docker) and environment and state (live).

**The mode is a run-level input, single-valued.** Drivers and the shared nest are
session-scoped, and docker/live are scheduled sweeps by ratified economics — never
inner-loop multipliers. Multi-mode coverage composes as separate pytest
invocations via `just` recipes.

Two invariants this module exists to hold:

**Standalone pays nothing.** Resolution is a pure string parse — no docker probe,
no network, no heavy import — and the standalone handle declares every
capability, so `NestHandle`'s guard is a no-op on the default path. Protecting
mode 1's latency is a requirement, not an optimization.

**Capability honesty.** Nest-handle keys are *declared capabilities*, and a mode
that cannot answer one must say so loudly. `nest["db_path"]` against live raises
a self-diagnosing `NestCapabilityError` naming the mode and the marker to add —
never a bare `KeyError` (which diagnoses nothing and which `except KeyError`
swallows), and never a `.get()` answering `None` (which lets a test skip its own
assertion and report green). The hand-maintained capability table is the
documented failure mode: convention 7's `app_capabilities.py` answered "not
implemented" for an app it had never heard of, silently skipping every state
assertion on tui.
"""

from __future__ import annotations

import os
from dataclasses import dataclass, replace
from urllib.parse import urlsplit

# ── The three modes ─────────────────────────────────────────────────────────

STANDALONE = "standalone"
DOCKER = "docker"
LIVE = "live"
MODES = (STANDALONE, DOCKER, LIVE)

#: The pytest marker that admits a test to a mode it is excluded from by
#: classification. Named in every `NestCapabilityError` so the message carries
#: its own fix rather than sending the reader to a doc.
MODE_MARKERS = {
    STANDALONE: "standalone_only",
    DOCKER: "docker_only",
    LIVE: "live_only",
}

#: Nest-handle keys that are *capabilities* — things a mode may legitimately be
#: unable to provide. Everything outside this set (`url`, `port`, `admin`, …) is
#: part of every mode's contract, and a plain `KeyError` on one is a harness bug
#: rather than a mode mismatch, so those keep plain-dict semantics.
CAPABILITY_KEYS = frozenset({
    "db_path",      # direct SQLite access — no local filesystem on live
    "proc",         # a local `subprocess.Popen` — docker has a container handle
    "tmp_dir",      # the nest's local data dir
    "config_path",  # the config file the harness wrote
    "node_binary",  # the locally-built binary, for in-place restarts
    "blob_dir",
    "log_path",
})


class NestModeError(RuntimeError):
    """A `--nest` value that cannot be honoured — unparseable, or a mode whose
    provider is not built yet. Always fatal: a mode that silently degrades to
    standalone would report a green certification run against an artifact it
    never started."""


class NestCapabilityError(RuntimeError):
    """A test asked the nest handle for something this mode cannot provide.

    Deliberately NOT a `KeyError` subclass: capability probes and dict lookups
    must not be caught by the same `except`, and the whole point is that this
    escapes to the report instead of being swallowed.
    """


@dataclass(frozen=True)
class NestMode:
    """The resolved mode for this run: a name plus its optional argument.

    `argument` carries `docker:IMAGE`'s image ref and `live:URL`'s URL. It is
    deliberately absent from `id` — see that property.

    `disposable_box` is the run's `--live-box disposable` declaration
    (`declare_box`, below): the live box this run hits is one the tree names
    as a staging box and nobody is signed into, so the shared-box policy —
    exclusion class (3) — is off for the run. False for every other run, and
    never inferable from the URL alone (see `declare_box` for why).
    """

    name: str
    argument: str | None = None
    disposable_box: bool = False

    @property
    def id(self) -> str:
        """The test-ID stamp: `test_x[tui-docker]`, never the argument.

        The stamp exists so logs, baselines, and flake history distinguish
        modes without per-mode files (`testing.md:44`). An image ref or URL in
        a node id would make that id vary run to run and break its use as a
        baseline key.
        """
        return self.name

    def __str__(self) -> str:
        return self.name

    @property
    def is_standalone(self) -> bool:
        return self.name == STANDALONE

    @property
    def is_docker(self) -> bool:
        return self.name == DOCKER

    @property
    def is_live(self) -> bool:
        return self.name == LIVE


def parse_nest_mode(raw: str) -> NestMode:
    """Parse a `--nest` / `E2E_NEST` value into a `NestMode`.

    `mode[:argument]`, split on the FIRST colon only so a URL keeps its own
    (`live:https://box.example:8443`).
    """
    text = (raw or "").strip()
    if not text:
        # An empty value is a broken recipe, not a request for the default —
        # defaulting here would hide `--nest "$EMPTY_VAR"` behind a green run.
        raise NestModeError(
            f"--nest was given an empty value; expected one of {', '.join(MODES)} "
            f"(omit the flag entirely for the default, {STANDALONE})"
        )
    name, _, argument = text.partition(":")
    name = name.strip()
    if name not in MODES:
        raise NestModeError(
            f"unknown nest mode {name!r}; expected one of {', '.join(MODES)} "
            f"(optionally {DOCKER}:IMAGE or {LIVE}:URL)"
        )
    return NestMode(name, argument.strip() or None)


def resolve_nest_mode(raw: str | None, environ=None) -> NestMode:
    """The run's mode, from the flag, else `E2E_NEST`, else the default.

    Same precedence as `--app`/`E2E_APPS`: an explicit flag beats the
    environment, which beats the default.

    `None` means the flag was absent; `""` means it was PRESENT and empty
    (`--nest "$UNSET_VAR"` in a recipe) and is an error, not a request for the
    default — the distinction is the whole reason this takes `str | None`.
    """
    if raw is not None:
        return parse_nest_mode(raw)
    env = os.environ if environ is None else environ
    override = env.get("E2E_NEST")
    if override:
        return parse_nest_mode(override)
    return NestMode(STANDALONE)


# ── The box declaration: `--live-box shared|disposable` ─────────────────────
#
# Exclusion class (3) — global-admin-mutating tests are deselected from live —
# is a policy about the BOX: a human may be signed into it, and the test would
# change what they see (`testing.md` § The shared-box rule). The CD gate runs
# against a box the pipeline wipes by design, so on that run the policy
# protects nobody and deselects the gate's own factory-resetting residents —
# invisibly, because a deselect is not a skip and `--fail-on-skip` never sees
# it (measured against the staging box 2026-10-04: the gate passed green
# having run neither mail resident).
#
# The declaration belongs to the RUN, not to a marker and not to a per-box
# table (`testing.md` § The shared-box rule → *The disposable-box
# declaration*). A marker would put the box's property on the test, and a
# `cd_suite` run through `just e2e-live-mode-test` against the production box
# would then factory-reset it. A table that made the staging box always
# disposable would turn every ordinary sweep against it destructive — the
# 2026-10-04 tui sweep that rotated the staging box's keys was exactly a run
# nobody meant to be destructive. So: the run says so, beside `--nest live:URL`,
# and only for a box the tree names below. It has deliberately NO env
# equivalent (`E2E_NEST`'s shape is not followed here): a declaration that
# makes a run destructive sits in the invocation someone reads, never in an
# ambient export a later shell inherits.

BOX_SHARED = "shared"
BOX_DISPOSABLE = "disposable"
BOX_DECLARATIONS = (BOX_SHARED, BOX_DISPOSABLE)

#: The hosts a run may declare disposable — the staging boxes, and nothing
#: else (`testing.md` § The shared-box rule, user ruling 2026-10-04:
#: `dev.example.com` from every dev machine, `test.example.com` from CI; example.com
#: is never a test target). Pinned in `test_nest_mode_axis.py` against the
#: image workflows' `VERIFY_HOST`, so the box the pipeline verifies on is
#: always one the gate can declare.
DISPOSABLE_BOX_HOSTS = frozenset({"dev.example.com", "test.example.com"})


def live_url(mode: NestMode) -> str:
    """The box a live run hits: `--nest live:URL` > `FAUNA_LIVE_NEST_URL` > the
    project's own live box — the one precedence the provider, the `live_box`
    flock and the box declaration all read, so none can disagree about which
    box is under test."""
    from helpers import multiseat_config as cfg

    return (mode.argument or cfg.nest_url()).rstrip("/")


def declare_box(mode: NestMode, raw: str | None) -> NestMode:
    """Apply the run's `--live-box` value to `mode`.

    `None` (flag absent) and `shared` leave the mode as it is. `disposable` is
    accepted only when the mode is live AND the box it resolves to is one of
    `DISPOSABLE_BOX_HOSTS`; everything else is refused with the reason. A
    present-but-empty value is a broken recipe, not a request for the default
    — the same contract as `parse_nest_mode`.
    """
    if raw is None:
        return mode
    text = raw.strip()
    if not text:
        raise NestModeError(
            "--live-box was given an empty value; expected one of "
            f"{', '.join(BOX_DECLARATIONS)} (omit the flag for the default, {BOX_SHARED})"
        )
    if text not in BOX_DECLARATIONS:
        raise NestModeError(
            f"unknown box declaration {text!r}; expected one of {', '.join(BOX_DECLARATIONS)}"
        )
    if text == BOX_SHARED:
        return mode
    if not mode.is_live:
        raise NestModeError(
            f"--live-box {BOX_DISPOSABLE} declares a live box disposable, but the "
            f"run's nest mode is {mode.name!r}, which starts its own nest; the "
            f"declaration goes beside --nest {LIVE}:URL"
        )
    url = live_url(mode)
    host = (urlsplit(url).hostname or "").lower()
    if host not in DISPOSABLE_BOX_HOSTS:
        raise NestModeError(
            f"refusing to declare {url} disposable: only a staging box may be "
            f"({', '.join(sorted(DISPOSABLE_BOX_HOSTS))} — testing.md § The "
            "shared-box rule), and a destructive suite against any other box "
            "stays deselected as class (3)"
        )
    return replace(mode, disposable_box=True)


# ── Run-level state, set once by conftest's pytest_configure ────────────────
# `_make_nest` is called by ~17 fixtures, none of which take the mode as an
# argument, so the run's mode lives here and conftest pushes it in — the same
# shape `app_surface._STRICT_APP` uses, for the same reason.

_RUN_MODE = NestMode(STANDALONE)


def set_run_mode(mode: NestMode) -> None:
    """Called by conftest for `--nest`. Not for test code."""
    global _RUN_MODE
    _RUN_MODE = mode


def run_mode() -> NestMode:
    """This run's mode. Defaults to standalone outside a pytest run."""
    return _RUN_MODE


#: What docker cannot answer, and why — each entry a fact about the image, not a
#: harness limitation to be lifted later. Kept as a mapping rather than a bare
#: set so `NestCapabilityError` can say *why* rather than only *that*.
#:
#: **`log_path` used to be here and was MISFILED** (removed 2026-09-02). Its
#: stated reason — "the nest logs to the container's stdout under s6, read with
#: `docker logs`, not to a file the harness owns" — described the transport and,
#: read closely, named a door rather than a wall: the lines exist, `docker logs`
#: is a first-class way to them, and `RUST_LOG` (a catalogued `extra_env` key)
#: reaches the image's nest, so `fauna_nest=debug` — the level the dispatch
#: beacon is emitted at — arrives there too. All three measured against
#: `ghcr.io/faunasocial/nest:latest` before the lift. The provider now streams
#: that log into `<tmp_dir>/nest.log`, the same path standalone publishes, so
#: every reader works unmodified in both modes; the file carries the WHOLE
#: container's output rather than the nest's alone, which for a mode whose point
#: is the deployed artifact is the more honest answer, not a lossy one. See
#: `testing.md` § Default app and nest mode.
DOCKER_ABSENT = {
    "config_path": (
        "the image's entrypoint writes the nest's config itself; the harness "
        "never authors one, so there is no file here to point at"
    ),
    "node_binary": (
        "the binary lives inside the image. There is no local build to re-spawn "
        "— and re-spawning one would be testing standalone while claiming docker"
    ),
}


#: What live cannot answer, and why. Live is absent from EVERY capability, and
#: for one reason wearing seven hats: the nest is on another machine. There is no
#: local disk holding its db/blobs/config/logs, no local process to signal, and
#: no local binary to re-spawn — and a harness that "helpfully" spawned one would
#: be testing standalone while the report said live, which is the exact
#: silent-fallback failure `NestModeError` refuses at configure time.
LIVE_ABSENT = {
    "db_path": (
        "the SQLite file is on the remote box's disk. A live test asserts "
        "through the wire, never by poking the nest's own storage"
    ),
    "proc": (
        "the nest is supervised on the remote box (s6/systemd there), not "
        "spawned by this harness — there is no local process to signal, and "
        "restarting a shared box is destructive under the shared-box rule"
    ),
    "tmp_dir": "the nest's data dir is on the remote box, not on this machine",
    "config_path": (
        "the deployment wrote its own config on the box; the harness never "
        "authored one, so there is no local file to point at"
    ),
    "node_binary": (
        "the binary is on the box. There is no local build behind this nest — "
        "and re-spawning one would be testing standalone while claiming live"
    ),
    "blob_dir": "blob storage is on the remote box's disk",
    "log_path": (
        "the nest logs on the box, read there — not to a file this harness owns"
    ),
}

#: Per-mode absence reasons, so `NestCapabilityError` can say *why* and not only
#: *that*. Keyed by mode name; standalone has no entry because it declares every
#: capability.
_ABSENT_REASONS: dict[str, dict[str, str]] = {
    DOCKER: DOCKER_ABSENT,
    LIVE: LIVE_ABSENT,
}


def absent_capabilities(mode: NestMode) -> frozenset[str]:
    """Which `CAPABILITY_KEYS` this mode cannot answer.

    Standalone answers everything, which is what makes the guard a no-op on the
    default path.

    Docker answers the *state* capabilities and not the *local-process* ones.
    `db_path`/`blob_dir`/`tmp_dir` survive because `DockerProvider` bind-mounts a
    host directory at `/data` (the design record's "worth doing" — SQLite-poking
    fixtures keep working), and the image's `fauna` user is uid 1000, the same
    uid the dev VMs run as, so the host side is genuinely readable rather than
    root-owned. `proc` survives as a container adapter with the
    `terminate`/`wait`/`kill`/`poll` surface the harness actually uses, and
    `log_path` as a host file the provider streams the container's log into —
    two absences remain, both of them genuine (see `DOCKER_ABSENT`).

    Live answers NONE of them — see `LIVE_ABSENT`. That is not harness debt to
    be closed later: every one of these keys is a local-machine fact about a nest
    this machine did not start, so a live run reaching for one is asking the
    wrong question, and the guard says so by name.
    """
    if mode.is_standalone:
        return frozenset()
    reasons = _ABSENT_REASONS.get(mode.name)
    if reasons is None:
        raise NestModeError(_unbuilt_message(mode))
    return frozenset(reasons)


def _unbuilt_message(mode: NestMode) -> str:
    built = ", ".join(sorted(_ABSENT_REASONS) + [STANDALONE])
    return (
        f"nest mode {mode.name!r} is not built yet — {built} have providers "
        f"today (see testing.md § Default app and nest mode). Refusing rather "
        f"than falling back to {STANDALONE}: a silent fallback would report a "
        f"green run against a nest it never started."
    )


class NestHandle(dict):
    """The nest handle every mode returns — a dict whose *capability* keys are
    guarded.

    Subclassing `dict` is deliberate: the handle is passed to ~17 fixtures and
    hundreds of call sites that already treat it as a mapping, and priority #1
    says minimise divergence rather than fork the shape. The guard only ever
    fires on a key this mode *declared* absent, so standalone behaves exactly
    like today's plain dict, byte for byte.
    """

    def __init__(self, data, mode: NestMode, absent=()):
        super().__init__(data)
        self._mode = mode
        self._absent = frozenset(absent)

    @property
    def mode(self) -> NestMode:
        return self._mode

    @property
    def absent(self) -> frozenset[str]:
        return self._absent

    def _refuse(self, key: str) -> NestCapabilityError:
        marker = MODE_MARKERS.get(self._mode.name, "standalone_only")
        why = _ABSENT_REASONS.get(self._mode.name, {}).get(key)
        because = f" ({why})" if why else ""
        return NestCapabilityError(
            f"the nest handle has no {key!r} in {self._mode.name!r} mode{because}"
            f": this mode cannot provide it. Either drop the dependency, or mark the "
            f"test `@pytest.mark.{marker}` so it is excluded from the modes "
            f"that cannot run it (testing.md § Default app and nest mode — "
            f"mode eligibility is classified exclusion). To branch instead, "
            f"ask `{key!r} in nest`, which answers False here."
        )

    def __getitem__(self, key):
        if key in self._absent:
            raise self._refuse(key)
        return super().__getitem__(key)

    def __contains__(self, key) -> bool:
        # False, not an exception: this is the sanctioned way for a test or
        # fixture to branch on a capability rather than depend on it.
        if key in self._absent:
            return False
        return super().__contains__(key)

    def get(self, key, *default):
        """`get(key)` on an absent capability RAISES; `get(key, default)` does not.

        The two calls mean different things. A bare `.get()` is a lookup whose
        author expected a value — answering `None` is how a capability mismatch
        turns into `if nest.get("db_path"):` quietly doing nothing and the test
        passing having asserted nothing. Passing an explicit default is a
        deliberate probe, and is left alone.
        """
        if key in self._absent:
            if default:
                return default[0]
            raise self._refuse(key)
        return super().get(key, *default)


# ── Provider registry ───────────────────────────────────────────────────────
#
# A provider answers one question: what fills the real-nest slot? It owns
# start + cleanup and declares its capabilities. `conftest.nest_instance`
# resolves through here; the providers themselves live in conftest because they
# need its fixtures (`nest_binary`, `tmp_path_factory`).

_PROVIDERS: dict[str, object] = {}


def register_provider(name: str, provider) -> None:
    if name not in MODES:
        raise NestModeError(f"cannot register a provider for unknown mode {name!r}")
    _PROVIDERS[name] = provider


def provider_for(mode: NestMode):
    """The provider for this mode, or a fatal error naming what is missing."""
    provider = _PROVIDERS.get(mode.name)
    if provider is None:
        raise NestModeError(_unbuilt_message(mode))
    return provider


def builds_local_nest(mode: NestMode) -> bool:
    """Does this mode need the locally-built `fauna-nest` binary?

    Only standalone does. Docker's binary is in the image and live's is on the
    remote box, so both would pay a cold ~15-25 min cargo build (plus its
    machine-wide `build`-slot wait) for a binary they then never execute — the
    cost `_live_nest_session` was invented to dodge for the tri-machine round,
    generalized here to the axis that supersedes it.

    Providers declare it, defaulting to True: a provider that forgets pays a
    build it does not need, which is slow. The inverse default would let one
    silently serve a nest with no binary behind it, which is wrong.
    """
    return bool(getattr(provider_for(mode), "builds_local_nest", True))


def supported_options(mode: NestMode) -> frozenset[str]:
    """The per-nest start options this mode's provider declares it honours.

    Ruling (3)'s seam, read from the *provider* rather than restated anywhere:
    the thing that knows how to start a nest is the thing that knows which knobs
    it can turn. `nest_surface`'s class-(4) classifier subtracts this set from
    what a fixture asks for, so growing a provider's declaration un-excludes
    every fixture needing only what it now supports, with no table edit.

    Defaults to empty, the opposite of `builds_local_nest` above, and for the
    same reason it defaults the way it does: a provider that forgets to declare
    gets its options refused loudly, where the inverse default would have it
    accept a knob it silently does not turn — a nest that is not the one the
    fixture described, reporting a pass.
    """
    return frozenset(getattr(provider_for(mode), "supported_options", frozenset()))


#: A provider whose inability to honour start options is a property of the MODE
#: rather than of a list of names declares this instead of a set. Live is the
#: case: the harness did not start that nest and cannot restart it with different
#: flags, so *every* option is permanently unhonourable there — and enumerating
#: today's names would quietly re-open the question each time one is added.
ALL_OPTIONS = "*"


def permanently_unsupported(mode: NestMode, candidates) -> frozenset[str]:
    """Which of `candidates` this mode's provider will NEVER honour.

    The distinction the plain `supported_options` subtraction cannot express, and
    the audit needs: "this mode does not turn that knob" is two different facts
    wearing one sentence. For most options it is closable debt — a provider that
    grows the option un-excludes every fixture needing only what it now supports.
    For a few it is permanent by ratification, and a test excluded by one of
    those has a cell that is honestly blank forever rather than a work item.

    Declared by the provider for the same reason `supported_options` is: the
    thing that knows how to start a nest is the thing that knows which knobs it
    can never turn. Defaults to empty — the SAFE default here, the opposite way
    round from `supported_options`: an undeclared permanence reads as closable
    debt, which over-reports work rather than hiding it.
    """
    declared = getattr(
        provider_for(mode), "permanently_unsupported_options", frozenset()
    )
    if declared == ALL_OPTIONS:
        return frozenset(candidates)
    return frozenset(declared) & frozenset(candidates)


def supported_venue_options(mode: NestMode) -> frozenset[str]:
    """The MAIL-VENUE options this mode's provider declares it honours.

    The venue twin of `supported_options`, and a separate vocabulary on purpose.
    A per-nest start option is a knob on one `start` call, identical in every
    mode; a mail VENUE is a provider method (`start_mail_venue`) because its mail
    listeners must be published at container start — a need that exists in docker
    and not in standalone, which is exactly what a name-level, mode-independent
    `FIXTURE_START_OPTIONS` cannot express (`testing.md` § Default app and nest
    mode, ruling (3); arm 6).

    Same default and same reason as above: empty, so a provider that forgets to
    declare refuses loudly rather than silently not honouring what it was asked.
    """
    return frozenset(
        getattr(provider_for(mode), "supported_venue_options", frozenset()))
