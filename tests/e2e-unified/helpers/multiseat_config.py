"""Zero-config defaults + run-id computation for the three-machine live filesync
convergence test (``tests/test_filesync_multiseat_live.py``).

The whole point is that the operator sets **no environment variables**: the
account seed is resolved per box (:func:`resolve_secret` — the box's own
staging-box identity, else ``~/.fauna-id``), the handle / nest URL are fixed to
the shared live account, the per-phase window has a sane default, and the run
id is derived by READING THE SET's own files (see below). Every value is still
overridable by its matching env var for a one-off manual run.

The run id is ``YYYYMMDD-NN``. It is NOT stored anywhere local — it is computed
from the set's existing file basenames, which is what makes the three machines
agree: the run files are themselves named ``YYYYMMDD-NN-...`` (the announce step
reads them through the app's Media UI), so "the largest NN already written
for today, plus one" is a pure function of the shared set. All three machines
read the same nest-side listing at announce time and get the same answer; a
crashed run leaves id-stamped files the next announce simply counts past. This
module owns only the pure arithmetic (:func:`next_run_id_from_names`); the read
itself is the test's job (it must go through the UI).

Stdlib-only apart from ``helpers.live_handle`` and ``helpers.staging_box``
(both stdlib-only — the rules this module shares rather than re-implements: a
handle is derived from the secret, never guessed; a staging box's identity file
lives where its provisioning test wrote it), so the test imports it as
``from helpers import multiseat_config as cfg``. See the test's module docstring
for the ``next`` -> "Ready to start RUN_ID=..." -> "go" operator handshake.
"""

from __future__ import annotations

import datetime as _dt
import os
import re
import sys
import time as _time
from pathlib import Path

from helpers import live_handle

DEFAULT_NEST_URL = "https://example.com"
DEFAULT_WINDOW_SECS = 180.0
SET_NAME = "e2e-multiseat"

# The ambient account seed (32-byte ed25519 seed, 64 hex chars), the LAST
# fallback after a box's own staging-box identity (:func:`resolve_secret`).
# Canonical file is ``~/.fauna-id`` (the live-mail tests' convention);
# ``~/.fauna.id`` is accepted as a same-content alias in case a machine still
# uses the dotted name. Resolved against ``Path.home()`` at call time.
_SECRET_NAMES = (".fauna-id", ".fauna.id")

# A run-file basename carries the full run id as its prefix: ``YYYYMMDD-NN-...``.
_RUN_ID_PREFIX = re.compile(r"^(\d{8})-(\d{2})-")


def nest_url() -> str:
    """Live nest URL. Only gates the same-machine ``live_box`` flock; the app
    itself discovers the nest by DoH from the handle, so this is advisory."""
    return os.environ.get("FAUNA_LIVE_NEST_URL") or DEFAULT_NEST_URL


def address() -> str:
    """What the seat types into ``handle-input``.

    Its ONE consumer is ``ob.fill_handle(...)``, so this is a *routing* input —
    the `@domain` half is what the client's DoH discovery follows, and the
    localpart half is replaced by the nest's own answer at ``AlreadyOnNest``
    (`helpers/live_handle.py` documents the mechanism). It used to default to a
    hardcoded ``test@example.com``: the example handle from the docstrings, shipped
    as a real default — the out-of-band-knowledge defect in its most dangerous
    form, since a *guess* fails silently where a demand at least stops. The
    probe localpart makes it honest; the domain, which is the half that actually
    matters here, still comes from ``nest_url()``.

    ``FAUNA_LIVE_MAIL_ADDRESS`` still overrides, so a run that sets it is
    unchanged. Kept as a plain accessor (this module is deliberately free of
    intra-suite imports beyond the one rule it now shares).
    """
    return os.environ.get("FAUNA_LIVE_MAIL_ADDRESS") or live_handle.sign_in_handle(nest_url())


def window_secs() -> float:
    return float(os.environ.get("FAUNA_MULTISEAT_TIMEOUT_SECS") or DEFAULT_WINDOW_SECS)


def _box_field(host: str, key: str) -> str | None:
    """One string field of the staging box's own provisioning record for
    ``host`` — the file ``tests/live/test_staging_box_provision.py`` mints into
    ``helpers.staging_box.state_path(<domain>)`` (``secret_hex``, ``handle``,
    ``mail_password``) — or None when this machine holds no usable record."""
    import json

    from helpers import staging_box

    try:
        state = json.loads(staging_box.state_path(host).read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(state, dict):
        return None
    value = state.get(key)
    return value.strip() if isinstance(value, str) and value.strip() else None


def _box_seed(host: str) -> str | None:
    """The staging box's own provisioning identity (``secret_hex``) for ``host``."""
    return _box_field(host, "secret_hex")


def resolve_mailbox(url: str | None = None) -> tuple[str | None, str | None]:
    """``(address, password)`` of the mailbox a destructive live mail test
    re-claims and enables on the box at ``url`` (default :func:`nest_url`),
    each field resolved on its own: its env var
    (``FAUNA_LIVE_MAIL_ADDRESS`` / ``FAUNA_LIVE_MAIL_PASSWORD``) >
    ``~/.config/fauna/staging-box/<host>.json``'s ``handle`` / ``mail_password``
    — the identity and password the box was provisioned under
    (``staging_box.load_or_create_state``) > for the password only,
    :func:`derived_mail_password` of the box's resolved admin seed. None for a
    field nothing provides.

    The provisioning test nulls the file's ``mail_password`` when mail was
    already on under a credential it never chose (a password on disk would
    open nothing). The factory-resetting modules then CHOOSE the password at
    re-enable, and the derivation makes that choice one value every module of
    the run — port 25's IMAP read-back included — computes at import, with no
    state written. No ``~/.fauna-id``-style machine-wide fallback for the
    address: a mailbox is a per-box fact, and a guessed one would re-claim the
    box under the wrong handle."""
    from urllib.parse import urlparse

    host = urlparse(url or nest_url()).hostname
    fields = []
    for var, key in (("FAUNA_LIVE_MAIL_ADDRESS", "handle"), ("FAUNA_LIVE_MAIL_PASSWORD", "mail_password")):
        env = os.environ.get(var)
        if env and env.strip():
            fields.append(env.strip())
        else:
            fields.append(_box_field(host, key) if host else None)
    address, password = fields
    if address is not None and "@" not in address:
        address = None
    if password is None and address is not None:
        seed = resolve_secret(url)[0]
        password = derived_mail_password(seed) if seed else None
    return address, password


def derived_mail_password(seed_hex: str) -> str:
    """A mailbox password that is a pure function of the box's admin seed —
    what a factory-resetting live module enables mail with when nobody recorded
    one. Keyed off the seed (HMAC, domain-separated), so it is as secret as the
    seed and reveals nothing about it; the shape matches
    ``staging_box.load_or_create_state``'s ``token_urlsafe(24)``."""
    import base64
    import hashlib
    import hmac

    digest = hmac.new(bytes.fromhex(seed_hex), b"fauna live mailbox password v1", hashlib.sha256).digest()
    return base64.urlsafe_b64encode(digest[:24]).decode().rstrip("=")


def resolve_secret(url: str | None = None) -> tuple[str | None, str | None]:
    """``(seed hex, where it came from)`` for the box at ``url`` (default
    :func:`nest_url`), resolved per box: ``FAUNA_LIVE_SECRET_HEX`` >
    ``~/.config/fauna/staging-box/<host>.json``'s ``secret_hex`` > the first
    existing ``~/.fauna-id`` alias. ``(None, None)`` only when nothing provides
    one. The source label is what a refused admin probe names, so a seed
    offered to the wrong box is diagnosed as that, not as a reset box."""
    from urllib.parse import urlparse

    env = os.environ.get("FAUNA_LIVE_SECRET_HEX")
    if env and env.strip():
        return env.strip(), "FAUNA_LIVE_SECRET_HEX"
    host = urlparse(url or nest_url()).hostname
    if host:
        seed = _box_seed(host)
        if seed:
            return seed, f"~/.config/fauna/staging-box/{host}.json"
    for name in _SECRET_NAMES:
        try:
            text = (Path.home() / name).read_text().strip()
        except OSError:
            continue
        if text:
            return text, f"~/{name}"
    return None, None


def load_secret(url: str | None = None) -> str | None:
    """Account seed hex for the box at ``url`` (:func:`resolve_secret`'s
    precedence). Returns None (the test skips) only when nothing provides it."""
    return resolve_secret(url)[0]


def today_str() -> str:
    """Local date as ``YYYYMMDD`` — the run id's date component."""
    return _dt.date.today().strftime("%Y%m%d")


# Which seat each dev machine plays. `sys.platform` is the whole input: one
# machine per platform is the tri-machine setup's premise (one Linux, one macOS
# and one Windows dev machine).
_PLATFORM_SEATS = {"linux": "linux", "darwin": "macos", "win32": "windows"}


def seat_for_platform(sys_platform: str) -> str | None:
    """This machine's seat name from ``sys.platform``, or None off the three.

    The announce step runs on the READER app (``--client tui``), so it has no
    seat driver to ask — but it still must know which app to build before it
    prints a run id (see :func:`local_seat`). Pure, so the mapping is pinned by
    tier_1 tests on every machine rather than only on the one running it.
    """
    return _PLATFORM_SEATS.get(sys_platform)


def local_seat() -> str | None:
    """The seat THIS machine plays, or None if it is not a seat platform.

    Consumed by the announce, which must not print ``Ready to start RUN_ID=...``
    until this machine's seat app is built: that printed line is the
    operator's cue to ``go`` on every machine at once, and a seat that then
    spends ~10 min in a cold build blows the other seats' per-phase rendezvous
    windows (default 180 s). Announcing means "every seat is ready to run".
    """
    return seat_for_platform(sys.platform)


# The native desktop app each platform's seat is normally driven by.
_NATIVE_SEAT_APPS = {"linux": "linux", "darwin": "macos", "win32": "windows"}

# Apps that can drive a seat, and where. The seat identifies the MACHINE; the
# app is only HOW it is driven, so a machine's seat may be driven by its
# native desktop app or by the terminal app — on ALL THREE seat platforms.
#
# tui drives a seat wherever `apps/fauna-tui/src/sync_agent.rs` compiles its
# provisioner in, which since 2026-07-24 is `cfg(any(unix,
# windows))` — the whole surface reaches the agent through one
# `fauna_ipc::endpoint::AgentEndpoint` (per-user unix socket on linux/macOS, the
# named pipe on windows), and `platform_spawner()` picks `SystemdUserUnitSpawner`
# on unix / `WindowsDetachedSpawner` on windows. `just tui-debug` builds
# `fauna-tui` + `fauna-sync-agent` as siblings in `target/debug/` on every
# platform, which is exactly the layout the agent's sibling probe resolves. tui
# as the announce READER is unrestricted — it needs no engine.
#
# ⚠ Spawner + binary resolution are NOT the whole story: the agent's ENDPOINT is
# derived per-platform too, and each platform needed its own isolation arm in
# `drivers/tui.py` before it could be listed here. On darwin the socket comes
# from `dirs::home_dir()`, not `$XDG_RUNTIME_DIR`, so the darwin seat is safe
# only because the driver relocates `HOME`+`CFFIXED_USER_HOME` —
# without it the seat drove the installed `/Applications/Fauna.app` agent
# against the live nest. On windows the pipe leaf comes from
# `sync::current_user_pipe_name()` unless `FAUNA_E2E_SYNC_PIPE` is set, so the
# windows seat is safe only because the driver sets a per-launch pipe leaf +
# data dir and pins the sibling agent binary, and because
# `port_util.reap_descendants_of`'s `KILL_ON_JOB_CLOSE` job object reaps the
# detached agent (windows has no process group; `terminate_tree` alone left it
# orphaned, answering its launch's pipe and locking its own .exe).
# Before adding a platform to this tuple, check all three: provisioner gating,
# binary resolution, AND endpoint derivation. testing.md point 10 owns the axis.
_TUI_SEAT_PLATFORMS = ("linux", "darwin", "win32")


def seat_app(sys_platform: str) -> str | None:
    """Which app will drive this machine's seat: ``FAUNA_MULTISEAT_SEAT_CLIENT``
    if set, else the platform's native desktop app.

    The announce needs this and cannot infer it: the seat run's ``--client`` is a
    pytest argument in a *different* process, so the machine has to be told which
    app it is about to build for. Naming it keeps the announce from building
    the wrong one — on Windows the native build is a ~25 min WinUI MSBuild that a
    tui-seat operator would pay for nothing.
    """
    override = os.environ.get("FAUNA_MULTISEAT_SEAT_CLIENT", "").strip()
    if override:
        return override
    return _NATIVE_SEAT_APPS.get(sys_platform)


def seat_app_error(app_name: str, sys_platform: str) -> str | None:
    """None if ``app_name`` may drive this machine's seat, else why not.

    Rejections are LOUD by design (the test fails, never skips): an app with no
    local sync engine binds a folder with nothing behind it and looks "bound"
    while syncing in neither direction — the exact failure that hid broken
    windows and macOS seats behind a machine-global agent (2026-07-24).
    """
    seat = seat_for_platform(sys_platform)
    if seat is None:
        return f"{sys_platform!r} is not one of the three seat platforms"
    if app_name == "tui":
        if sys_platform not in _TUI_SEAT_PLATFORMS:
            return (
                f"the tui seat needs a per-OS agent spawner "
                f"(apps/fauna-tui/src/sync_agent.rs::platform_spawner has only "
                f"#[cfg(unix)] and #[cfg(windows)] arms), so it cannot drive the "
                f"{seat!r} seat on {sys_platform!r} — use --client {seat}"
            )
        return None
    native = _NATIVE_SEAT_APPS[sys_platform]
    if app_name != native:
        return (
            f"--client {app_name!r} cannot drive the {seat!r} seat on "
            f"{sys_platform!r}: use the native app ({native!r}) or, on unix, "
            f"'tui'. An app with no local sync engine would bind a folder "
            f"with nothing behind it."
        )
    return None


def next_run_id_from_names(names, today: str) -> str:
    """The next run id (``YYYYMMDD-NN``) given the set's current file basenames.

    ``NN`` = 1 + the highest NN already present for ``today`` (``01`` when none).
    Every run file is named ``YYYYMMDD-NN-...``, so each is self-marking and the
    id is a pure function of the shared set's contents — no local ledger, and
    the three machines reading the same set agree by construction. A manual
    ``FAUNA_MULTISEAT_RUN_ID`` override (e.g. ``r1807``) never matches the
    date-prefix regex, so it is never absorbed into the auto sequence.
    """
    hi = 0
    for name in names:
        m = _RUN_ID_PREFIX.match((name or "").strip())
        if m and m.group(1) == today:
            hi = max(hi, int(m.group(2)))
    return f"{today}-{hi + 1:02d}"


def settle_listing(
    snapshot,
    *,
    timeout_s: float = 20.0,
    poll_s: float = 1.0,
    sleep=None,
    monotonic=None,
    loaded=None,
):
    """Poll ``snapshot()`` until the listing has genuinely settled.

    Sameness alone is NOT settledness. ``fauna.media.list`` loads asynchronously,
    so a listing that has not loaded yet reads ``[]`` — and two consecutive ``[]``
    reads look exactly like a stable empty set. The old loop accepted that and
    returned after ~1 s, which silently yields run id ``-01`` no matter what is
    really on the nest. Nothing downstream catches it: the announce prints the
    wrong id, and phase 0's freshness check calls this same helper, so it too sees
    ``[]`` and its ``mine & names`` clash test is vacuous.

    ``loaded`` is the app's own answer to "has the first read RETURNED?" — see
    :meth:`actions.media.MediaActions.has_loaded`, which reads it off the
    ``media-empty-state`` element (ui.yaml `media` page, user-approved
    2026-08-05). Three cases:

    * ``True``  — the page has loaded. Whatever it shows is the truth, so an
      empty listing settles **immediately** instead of paying the full window.
    * ``False`` — the page has NOT loaded. Keep polling, and if the window
      expires **raise**: returning ``[]`` here is precisely the silent wrong-id
      bug, and an app that can tell us it never loaded must not be ignored.
    * ``None`` (``loaded=None``, i.e. no probe supplied, or the probe raised)
      — no signal available. Fall back to the wall-clock settle below,
      unchanged. ``None`` must NOT be read as ``False``: that would turn every
      genuinely empty listing into a hard failure.

    Without an app-supplied answer the rule is the old one, which needs no
    unproven premise: **only a non-empty listing settles early.** An empty one
    polls to the deadline and is then returned as-is — it may be a genuinely
    empty set, which is a legitimate and common state (a fresh set, or one whose
    files were deleted). The caller warns on empty rather than failing, because
    "empty" is not evidence of breakage.

    (A previous revision raised on empty when a ``set_filter`` select had succeeded,
    on the theory that the filter only offers non-empty sets. That is unsound — the
    select succeeds against an empty set — and it false-positived on a correct read.
    ``media-empty-state`` is the *sound* version of that instinct: it is the app
    stating the fact, not the harness inferring it.)

    ``sleep``/``monotonic`` are injectable so the tier_1 tests pay no wall-clock
    time (testing.md § point 14 — fake clock, never a settle-sleep). Note what
    convention 14 buys here: with ``loaded`` supplied the deadline stops being
    the thing the verdict is *inferred from* and becomes a bound a green run
    never pays.
    """
    sleep = sleep or _time.sleep
    monotonic = monotonic or _time.monotonic

    def _has_loaded():
        if loaded is None:
            return None
        try:
            return loaded()
        except Exception:
            # A probe that cannot run tells us nothing; it must never be the
            # reason a correct read fails. Degrade to the wall-clock settle.
            return None

    prev = None
    names = snapshot()
    deadline = monotonic() + timeout_s
    unloaded_seen = False
    while monotonic() < deadline:
        if names and names == prev:  # non-empty AND unchanged → settled
            return names
        if not names:
            state = _has_loaded()
            if state is True:
                return names  # proven empty — no waiting for a nothing
            if state is False:
                unloaded_seen = True
        prev = names
        sleep(poll_s)
        names = snapshot()

    if not names and unloaded_seen and _has_loaded() is False:
        raise RuntimeError(
            f"the Media listing never finished loading within {timeout_s:.0f}s: "
            "no media-item rows and no media-empty-state, so the page is still "
            "reading. Returning an empty listing here would silently compute a "
            "run id from nothing (the -01 bug). Check the seat's WS-RPC status "
            "and the nest's fauna.media.list."
        )
    return names
