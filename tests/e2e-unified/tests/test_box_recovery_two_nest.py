"""tier_3: box-recovery T2/T3/T4 — the two-nest client e2e over the plane.

``box-recovery.md`` § Goal: after total box loss, the admin's client
re-instantiates a same-identity box so every TOFU-pinned client reconnects
without a trust break. That promise depends on the admin's OWN client having
custodied every box's recovery seed. Since the plane-era consumer cut
(``box-recovery.md`` § The plane-era recovery floor) the custody map rests on
the ACCOUNT PLANE — the fleet-only, tip-sealed kind
``fauna.state.deployment-seeds``, one row per custodied box (before the ``__config`` rail retired at closure
step (6), the map rode its blob; see ``config-dissolution.md``). There is no
seed-map fan-out and no device-local replica for this kind:

- **Capture is the custody leg** (``fauna_client_account_runtime::
  deployment_seeds::run_custody_leg``, § (c) The writes): at the store-ready
  edge and at every post-auth edge that finds the store up, it fetches the
  bound box's seed over ``fauna.admin.deployment_seed.get`` and merges its row
  into the device's own account store, which the plane publishes to the bound
  nest. Its outcome is an INFO line — ``custody leg: captured`` (a new row) or
  ``custody leg: held`` (the fold already had one) — which these tests wait on
  positively (``_CUSTODY_LEG_NEEDLES``), never a sleep.
- **The pre-login reads go through one resolver** (§ (b) The reads): the
  ``nest_recovery`` box list and the launch-retry ``launch-recover-button``
  read this device's OWN account store (the local read) joined with a cold
  read from the saved / entered nest. On a surviving device whose saved nest
  is the dead box, the local read alone reveals the box.

**Client-parametrized (tui + linux).** Both apps drive the IDENTICAL flow
over the shared ``fauna_launch_machine::LaunchMachine`` four-case dispatch and
the shared resolver (``fauna_client_account_runtime::deployment_seeds``); tui
is the lead app, so it is listed first. Linux's launch glue is ``main.rs`` +
``client::load_recoverable_boxes``; tui's twin is ``apps/fauna-tui/src/
{launch,recovery}.rs`` (``route()`` maps the settled ``LaunchPhase`` onto the
same element IDs, ``fetch_launch_recover_boxes`` reads the same resolver).
Both write the credential file in the registry shape alone (the per-actor
``fauna/{actor}/*`` slots); the readiness barrier (``_has_identity_secret``)
matches only ``*/secret`` and the ``nest_url`` surgery only ``*/nest_url``.

**T2** — a fresh device holding only the identity seed lists BOTH boxes by
reaching surviving box B alone (§ (a) The multi-box floor):

  1. Nest A + nest B, both fresh and unclaimed. Launch on config-home ``H1``;
     drive the real onboarding wizard to claim A with a fresh identity; the
     custody leg lands A's row on the plane.
  2. Relaunch on the SAME ``H1`` with the stored ``nest_url`` cleared
     (identity kept) — Case-3 routing lands on ``handle_entry``; claim B. The
     custody leg lands B's row.
  3. Launch a FRESH device (new config-home, no account store), import the
     SAME identity, connect to B under recovery intent → ``nest_recovery``,
     whose box list is the resolver: B's cold read, no local store.
     **Assert ``recover-box-item-0`` AND ``recover-box-item-1`` render.**

  ⚠ **T2 depends on the bind leg** (``docs/goal/architecture/
  account-sync-plane.md`` § The bind leg): A's row reaches B only when a
  runtime bound to B completes B's replica with every fleet row the device
  holds. That leg is ruled and NOT built, so T2 is EXPECTED RED until it lands
  — deliberately not xfail/skip: its red is the guard, its green the bind
  leg's landing gate. It asserts only the observable (both rows listed), no
  mechanism.

**T3** — the ``launch-recover-button`` surviving-device CTA against a REAL
reachable nest holding the custodied box. The reveal gate fires only from
``LaunchPhase::Offline { transient: true }`` (Case 1 — identity + saved
``nest_url``), so a raw byte-level TCP proxy
(``_FirstConnectionRefusingProxy``) refuses exactly its FIRST connection — the
silent challenge — and forwards every later one, so the resolver's cold read
reaches the healthy nest. Keyed on connection COUNT, not time. (The device's
own store also holds the row, so either half of the join reveals the button;
T3 pins the reachable-nest shape, T4 the dead-nest one.)

**T4** — the dead-saved-nest assertion (``box-recovery.md`` § Implementation
status today, the 2026-09-30 finding the consumer cut fixed): claim one box,
let the custody leg rest its row in the device's own store, STOP that box,
relaunch the SAME device. The silent challenge reaches nothing →
launch-retry; the cold read fails too, so the local read alone must reveal
``launch-recover-button`` and list the box in ``nest_recovery``.

**T5** — the custody leg over a LINKED nest (§ (c) The writes, the
linked-connection paragraph): the admin's device stays bound to box A for its
whole life and never clears its stored nest URL. It claims A, then links box B
— a second box the same identity already administers — on the Nests page. The
account runtime's secondary leg opens a connection to B at its next full pass
(the relaunch's prologue), and the custody leg runs over it: B's seed is
custodied beside A's, and B is completed with every fleet row the device
holds. A fresh device that reaches only B then lists BOTH boxes and resolves
each one's self-hosted recovery command — box A's seed and box B's own.

  **T5's device runs its own sync agent, as every desktop does.** The
  secondary leg — and the custody leg riding it — needs a SEED-HOLDING runtime
  (``account-sync-plane.md`` § The bind leg, ruling 4, *the stated bounds*: "A
  seedless host runs no secondary leg"), and on a desktop the seedless agent
  ordinarily holds the engine-singleton role. The app beside it holds the
  seed-leg role and runs the leg in its seed pass (``account-runtime.md``
  § Multi-instance concurrency → *The seed-leg role*); an app that wins the
  engine role runs it inside its own pass. T5 passes whichever process holds
  the engine, and asserts no election outcome.

  **T5 also runs on web** (`_LINKED_APPS`), where the admin's device is a
  browser tab — the seed-holding runtime that pumps, with no agent beside it.
  The journey is the same one through the same pages; only the seat differs
  (`_LinkedSeat`): the boxes serve plain HTTP and allow the SPA's origin, the
  wizard's nest leg is pointed at the box under test with the driver's `nest`
  provider override (a typed loopback handle resolves to `https://`, which a
  browser will not speak to a harness nest), the relaunch is a page reload
  over the same origin store, and each fresh device is a twin page in a
  BrowserContext of its own. T2–T4 stay native: they turn on a stored nest
  URL edited between launches, a TCP proxy and a launch-retry surface.

Harness notes:

- Credentials in e2e are a JSON file, not libsecret
  (``FAUNA_E2E_CREDENTIAL_DIR``), and launch routes on the registry's
  per-actor ``fauna/{actor}/*`` rows alone — the ``nest_url`` surgery edits
  ``fauna/{actor}/nest_url`` (``_nest_url_keys`` below).
- The device's account store lives under the launch's XDG dirs
  (``StoreRoot::platform``), so reusing the SAME ``xdg_base`` across launches
  is what carries the custodied row into that device's next launch.
- The custody leg is admin-gated (``NotAdmin`` logs at debug) — the device
  must have CLAIMED the bound box, not merely authenticated to it.
"""


from __future__ import annotations

import contextlib
import json
import os
import secrets
import socket
import sys
import threading
import time
from pathlib import Path

import pytest

# Mirror the sys.path bootstrap of the other local-onboarding e2e files so the
# shared `common` package (tests/common/) and the e2e-unified helpers resolve.
_tests_dir = str(Path(__file__).resolve().parent.parent.parent)  # tests/
if _tests_dir not in sys.path:
    sys.path.insert(0, _tests_dir)
_e2e_dir = str(Path(__file__).resolve().parent.parent)  # tests/e2e-unified/
if _e2e_dir not in sys.path:
    sys.path.insert(0, _e2e_dir)

from actions import ActionLayer  # noqa: E402
from conftest import (  # noqa: E402
    _apply_r14_trust_env,
    _nest_id_hex,
    _resolve_cli_binary,
    _resolve_linux_binary,
    APP_PATHS,
    _repo_root,
    _resolve_windows_app,
    _seeded_environment,
    browser_origins_to_allow,
    get_available_apps,
)
from common.auth import claim_admin  # noqa: E402
from common.nest import stop_nest  # noqa: E402
from drivers import create_driver  # noqa: E402
from helpers import nest_mode as nm  # noqa: E402
from helpers.waiting import await_account_runtime_assembled  # noqa: E402

# Client-parametrized (box-recovery.md § Recovery UI (step 4): the launch
# surfaces are a tui + linux owe). Both apps drive this identical flow — each
# runs the shared custody leg at its store-ready and post-auth edges and reads
# the shared pre-login resolver for the box list and the surviving-device
# `launch-recover-button` (box-recovery.md § The plane-era recovery floor) —
# over the SHARED `fauna_launch_machine::LaunchMachine` four-case dispatch, so
# only a thin per-app seam remains (binary + tracing target + the
# credential-shape note above). tui is the lead app, so it is listed first.
# Only apps this run selected become params (`get_available_apps` is read at
# import, so `--app` narrows the lists themselves; a test left with an empty
# list is deselected by conftest's empty-parameter-set rule).
_TWO_NEST_APPS = [c for c in ("tui", "linux") if c in get_available_apps()]
# T5 adds web: its device needs no edited credential file, proxy or
# launch-retry surface, so a browser tab can sit in the seat (`_LinkedSeat`).
_LINKED_APPS = _TWO_NEST_APPS + [c for c in ("web",) if c in get_available_apps()]
# T4 (the dead-saved-nest launch entry) also runs on windows and macos: its one
# nest and its same-device relaunch need only the driver's own state-root +
# credential-dir pinning (`local_appdata` / `home`), no proxy or edited credential
# file. The windows and macos glue used to read ONLY the saved nest, so the
# button never appeared exactly when it was needed. (iOS has no launch-retry
# surface at all, so it is not a witness.)
_DEAD_NEST_APPS = _TWO_NEST_APPS + [
    c for c in ("windows", "macos") if c in get_available_apps()]
if not (_LINKED_APPS or _DEAD_NEST_APPS):
    pytest.skip(
        "drives the real tui/linux/web/windows/macos onboarding wizard across live nests",
        allow_module_level=True,
    )

pytestmark = pytest.mark.tier_3

# The client's own tracing target, for RUST_LOG — linux's crate is
# `fauna-desktop`, tui's is `fauna-tui`. Raised to debug so a red diagnoses
# itself (launch routing, the resolver's per-source failures).
# windows has no app crate of its own (its glue is C#), so its tracing target is
# the FFI crate; so does macos (its glue is Swift over the same FFI).
_RUST_LOG_TARGET = {
    "linux": "fauna_desktop", "tui": "fauna_tui",
    "windows": "fauna_ffi", "macos": "fauna_ffi",
}

# The custody leg runs in the shared runtime crate, not the app crate, so its
# target is named explicitly. Its `NotAdmin` outcome and the mark reconcile's
# deferral log at debug!, which is why it is raised to debug too: at plain
# info a leg that ran and found "not an admin" is indistinguishable from one
# that never ran.
_CUSTODY_LEG_TARGET = "fauna_client_account_runtime"

# The custody leg's two confirmed outcomes (`deployment_seeds::run_custody_leg`,
# both INFO): `captured` merged a new row, `held` found the fold already
# holding one. Either means this box's row rests in the device's own account
# store. Its unconfirmed outcome (WARN) is `_CUSTODY_LEG_UNCONFIRMED`, read into
# the failure message rather than waited on.
_CUSTODY_LEG_NEEDLES = (
    "deployment seed custodied off-box on the account plane (custody leg: captured)",
    "deployment seed already custodied off-box on the account plane (custody leg: held)",
)
_CUSTODY_LEG_UNCONFIRMED = "deployment-seed custody leg: custody unconfirmed"


# The custody leg over a LINKED nest runs inside the account runtime's pass
# (`fauna_account_plane::account_driver::pass::linked_custody`): `captured` is
# INFO, `held` — a pass that found the row already there — is DEBUG, so the
# pass module is raised to debug for the launches that wait on either.
_LINKED_CUSTODY_TARGET = "fauna_account_plane::account_driver::pass"
_LINKED_CUSTODY_NEEDLES = (
    "(linked custody leg: captured)",
    "(linked custody leg: held)",
)


def _rust_log(client: str) -> str:
    """RUST_LOG for one launch: info everywhere, debug on the app's own crate,
    on the custody leg's crate and on the pass that runs its linked arm."""
    return (
        f"info,{_RUST_LOG_TARGET[client]}=debug,{_CUSTODY_LEG_TARGET}=debug,"
        f"{_LINKED_CUSTODY_TARGET}=debug"
    )

# The authed app shell has mounted once any main-view landmark is visible
# (mirrors test_mail_enable_at_admin_claim.py's _LOGGED_IN_MARKERS). Both
# apps under test register both IDs — linux `feed/mod.rs` + tab bar, tui
# `feed/mod.rs:618` + `pages.rs:108` — so the check is client-uniform.
_LOGGED_IN_MARKERS = ("feed-view", "feed-tab")


def _app_path(client: str) -> str:
    """The built debug binary for `client`, or skip if it isn't built."""
    if client == "linux":
        path = _resolve_linux_binary()
        hint = "cargo build -p fauna-linux --bin fauna-desktop"
    elif client == "windows":
        path = _resolve_windows_app()
        hint = "just windows-debug"
    elif client == "macos":
        path = _repo_root / APP_PATHS["macos"]
        if not path.exists():
            path = None
        hint = "just mac-debug"
    else:  # tui
        path = _resolve_cli_binary()
        hint = "cargo build -p fauna-tui"
    if not path:
        pytest.skip(f"{client} app not built — run '{hint}' first")
    return str(path)


def _start_boxes(request, nest_mode, tmp_path_factory, *labels,
                 browser: bool = False) -> tuple[list, object]:
    """Start one nest per `labels` through the run's own provider, unclaimed and
    TLS-serving. Returns `(nests, stop_all)`.

    `browser=True` is the shape a web seat dials (`provision_target_nest`'s):
    plain HTTP, since a browser refuses a harness nest's self-signed
    certificate, and the SPA's origin on the nest's CORS allow-list, since the
    page reaches these boxes round its own proxy.

    Routed rather than self-spawned (`testing.md` § Default app and nest mode,
    ruling (1)): these two journeys used to call `common.start_nest` directly,
    which is a *local binary* and therefore excluded them from every mode but
    standalone — while the binary here is entirely incidental. What the tests
    actually need of a box is that it serves TLS, starts unclaimed, and can be
    stopped, and all three are contract keys every provider answers. The nest's
    own `port` and `claim_code` are read off the handle for the same reason:
    both used to be harness-side constants (`find_free_port()` and
    `common.nest.CLAIM_CODE`), and a container's are neither.
    """
    provider = nm.provider_for(nest_mode)
    # Lazily, and only where a local binary is what fills the slot — DECLARING
    # `nest_binary` would put it back in these tests' fixture closure, which is
    # exactly what marks a test standalone-only (`nest_surface.NEST_BINARY_FIXTURES`).
    nest_binary = (
        request.getfixturevalue("nest_binary")
        if nm.builds_local_nest(nest_mode) else None
    )
    options = (
        {"cors_origins": browser_origins_to_allow(request)} if browser
        else {"serve_tls": True}
    )
    nests, cleanups = [], []
    try:
        for label in labels:
            nest, cleanup = provider.start(
                nest_binary, tmp_path_factory, label,
                unclaimed=True, **options,
            )
            nests.append(nest)
            cleanups.append(cleanup)
    except BaseException:
        for cleanup in reversed(cleanups):
            cleanup()
        raise

    def stop_all() -> None:
        for cleanup in reversed(cleanups):
            cleanup()

    return nests, stop_all


def _nest_url_keys(data: dict) -> list[str]:
    """Every key holding a nest_url-shaped value: the registry's per-actor
    `fauna/{actor}/nest_url` row launch routes on."""
    return [k for k in data if k.endswith("/nest_url")]


def _has_identity_secret(data: dict) -> bool:
    """The claim has written this identity's secret to the credential file —
    the per-actor `fauna/{actor}/secret` slot both apps write at claim time."""
    return any(k.endswith("/secret") for k in data)


def _clear_stored_nest_url(creds_dir: str, keyring_app: str) -> None:
    """Strip every nest_url-shaped key, keeping the identity secret intact, so
    the NEXT launch's Case-3 routing (identity present, `nest_url` absent, no
    pending invite) lands directly on `handle_entry` instead of
    silent-challenging the just-claimed box."""
    path = Path(creds_dir) / f"{keyring_app}.json"
    deadline = time.monotonic() + 10.0
    data: dict = {}
    while time.monotonic() < deadline:
        try:
            data = json.loads(path.read_text())
        except Exception:
            data = {}
        if _has_identity_secret(data):
            break
        time.sleep(0.2)
    assert _has_identity_secret(data), (
        f"expected the claim to have written an identity secret to {path}, "
        f"found: {data}"
    )
    keys = _nest_url_keys(data)
    assert keys, f"expected a stored nest_url key in {path}, found: {data}"
    cleared = {k: v for k, v in data.items() if k not in keys}
    path.write_text(json.dumps(cleared, indent=2))
    os.chmod(path, 0o600)


def _rewrite_stored_node_url(creds_dir: str, keyring_app: str, new_url: str) -> str:
    """Point every stored nest_url-shaped key at `new_url` (keeping the
    identity + everything else). Returns the ORIGINAL url (pre-rewrite) so
    the caller can sanity-check it."""
    path = Path(creds_dir) / f"{keyring_app}.json"
    data = json.loads(path.read_text())
    keys = _nest_url_keys(data)
    assert keys, f"expected a stored nest_url key in {path}, found: {data}"
    originals = {data[k] for k in keys}
    assert len(originals) == 1, (
        f"expected every nest_url-shaped key to agree before rewriting, got: {originals}"
    )
    for k in keys:
        data[k] = new_url
    path.write_text(json.dumps(data, indent=2))
    os.chmod(path, 0o600)
    return next(iter(originals))


def _wait_for_authed_app_or_fail(app, markers=_LOGGED_IN_MARKERS) -> None:
    """Click through nat_mode_choice / launch-retry until a main-view landmark
    renders, or fail with the launch error + accessibility tree. Mirrors
    test_mail_enable_at_admin_claim.py's `_drive_admin_claim_to_logged_in`
    tail loop (shared prior art, not re-derived).

    `markers` widens the landmark set for a caller that knows where the app
    comes back: a reloaded browser tab returns to the page it was on."""
    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:
        if any(app.driver.is_visible(m) for m in markers):
            return
        try:
            app.onboarding.finish_nat_mode()
        except Exception:
            pass
        for btn in ("launch-retry-button",):
            try:
                if app.driver.is_visible(btn):
                    app.driver.click(btn)
            except Exception:
                pass
        time.sleep(2.0)

    err = ""
    for eid in ("launch-transient-error", "error-message"):
        try:
            if app.driver.is_visible(eid):
                err = app.driver.get_text(eid)
                break
        except Exception:
            pass
    try:
        tree = app.driver.tree()
    except Exception as e:  # pragma: no cover - diagnostic only
        tree = f"(tree dump failed: {e})"
    try:
        # Distinct lines in first-seen order, not a raw tail: this loop clicks
        # Retry every two seconds, so a stuck launch fills any tail with the
        # same three lines and pushes out the one that says why.
        distinct = dict.fromkeys(
            line[:400] for line in app.driver.app_stderr_text().splitlines())
        log = "\n".join(distinct)[-20000:]
    except Exception as e:  # pragma: no cover - diagnostic only
        log = f"(app log read failed: {e})"
    pytest.fail(
        "admin never reached the authed app after claim + nat_mode_choice. "
        f"launch error: {err!r}\n--- accessibility tree ---\n{tree}"
        f"\n--- app log tail ---\n{log}"
    )


def _wait_for_log_containing_any(driver, needles, timeout: float = 20.0) -> str:
    """Poll this launch's `app.err` until ANY of `needles` appears, returning
    the full log text once found (or failing with the log tail on timeout).

    A positive wait on the state the log reports, never a blind sleep
    (e2e-conventions.md point 14): `timeout` is a ceiling on a hung run, not
    the thing the assertion depends on."""
    needles = tuple(needles)
    deadline = time.monotonic() + timeout
    text = ""
    while time.monotonic() < deadline:
        text = driver.app_stderr_text()
        if any(n in text for n in needles):
            return text
        time.sleep(0.3)
    unconfirmed = (
        "\n(the custody leg DID run and ended unconfirmed — see its WARN line)"
        if _CUSTODY_LEG_UNCONFIRMED in text else ""
    )
    # 20k, not 4k: the 4k window cut off the START of the post-auth window — the
    # very part that says whether the custody task ever ran — and a rerun cannot
    # recover it, because pytest GCs the tmp tree holding this launch's own
    # `app.err` (measured 2026-09-02: this failure's evidence was lost twice
    # exactly that way, on consecutive runs).
    pytest.fail(
        f"timed out waiting for any of {needles!r} in app log{unconfirmed}:\n"
        f"{text[-20000:]}"
    )


def _wait_for_custody_leg(driver, timeout: float = 60.0) -> str:
    """Wait until the custody leg reports this launch's bound box custodied on
    the account plane (`captured` or `held`).

    The leg runs in the background off the store-ready and post-auth edges —
    the UI reaching feed-view does NOT mean it has fetched the seed, merged the
    row into the device's own account store and handed it to the plane.
    Tearing the driver down on the UI signal races that write against process
    teardown, and every later launch of this device (T4's dead-nest local read)
    or of a sibling (T2's cold read) reads what it wrote."""
    return _wait_for_log_containing_any(driver, _CUSTODY_LEG_NEEDLES, timeout=timeout)


def _drive_handle_check_and_claim(app, typed_handle: str, claim_code: str) -> None:
    """From handle_entry (identity already loaded): resolve `typed_handle`
    against an UNCLAIMED nest, submit its own claim code, and ride the wizard to
    the authenticated app.

    `claim_code` is a parameter rather than `common.nest.CLAIM_CODE` because the
    code is a property of the NEST, not of the harness: standalone writes a fixed
    one into its data dir, while the docker provider mints a fresh one per
    container and publishes it on the handle. Reading it off the nest is what
    lets these journeys run in either mode.
    """
    ob = app.onboarding
    ob.fill_handle(typed_handle)
    ob.run_handle_check(timeout=45)  # real local-nest probe, no injection
    ob.submit_handle()

    app.driver.wait_for("claim-code-input", timeout=45)
    app.driver.clear_and_type("claim-code-input", claim_code)
    app.driver.click("claim-code-submit-button")

    _wait_for_authed_app_or_fail(app)


def _claim_fresh_identity(app, secret_hex: str, typed_handle: str,
                          claim_code: str) -> None:
    """From identity_choice: import `secret_hex` as a brand-new identity, then
    claim the nest `typed_handle` resolves to."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)  # -> handle_entry
    _drive_handle_check_and_claim(app, typed_handle, claim_code)


class _FirstConnectionRefusingProxy:
    """A raw byte-forwarding TCP proxy that REFUSES its first `refuse_first`
    accepted connections (closes immediately, relays nothing) and
    transparently forwards every connection after that to `target_port`.

    Deterministically reproduces "the saved nest was transiently
    unreachable, then became reachable again moments later"
    (`SilentChallengeOutcome::Transient` on the challenge, a plain config
    read succeeding right after) with NO timing race and no process
    restart: the refusal is keyed on connection COUNT, and the silent
    challenge always opens the first connection to a saved `nest_url`
    (before the async box-list read's own connection). TLS-agnostic — it
    never inspects the stream, so it works transparently for the
    `serve_tls=True` nests the loopback-handle onboarding flow requires.
    """

    def __init__(self, target_port: int, refuse_first: int = 1):
        self._target_port = target_port
        self._refuse_first = refuse_first
        self._count = 0
        self._lock = threading.Lock()
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(8)
        self.port = self._sock.getsockname()[1]
        self._stop_flag = False
        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()

    def _accept_loop(self) -> None:
        while not self._stop_flag:
            self._sock.settimeout(0.5)
            try:
                conn, _ = self._sock.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with self._lock:
                self._count += 1
                refuse = self._count <= self._refuse_first
            if refuse:
                conn.close()
                continue
            threading.Thread(target=self._forward, args=(conn,), daemon=True).start()

    def _forward(self, client_conn: socket.socket) -> None:
        try:
            server_conn = socket.create_connection(("127.0.0.1", self._target_port), timeout=10)
        except OSError:
            client_conn.close()
            return

        def pump(src: socket.socket, dst: socket.socket) -> None:
            try:
                while True:
                    data = src.recv(65536)
                    if not data:
                        break
                    dst.sendall(data)
            except OSError:
                pass
            finally:
                with contextlib.suppress(OSError):
                    dst.shutdown(socket.SHUT_WR)

        t1 = threading.Thread(target=pump, args=(client_conn, server_conn), daemon=True)
        t2 = threading.Thread(target=pump, args=(server_conn, client_conn), daemon=True)
        t1.start()
        t2.start()
        t1.join()
        t2.join()
        with contextlib.suppress(OSError):
            client_conn.close()
        with contextlib.suppress(OSError):
            server_conn.close()

    def stop(self) -> None:
        self._stop_flag = True
        with contextlib.suppress(OSError):
            self._sock.close()
        self._thread.join(timeout=2)


@pytest.mark.parametrize("client", _TWO_NEST_APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_two_nest_fan_out_fires_and_fresh_device_recovers_both_boxes(
    client, nest_mode, tmp_path, tmp_path_factory, request
):
    """T2: a fresh device holding only the identity seed, reaching surviving
    box B alone, lists BOTH of the admin's boxes — including box A, which it
    never saw.

    box-recovery.md § The plane-era recovery floor, (a) The multi-box floor,
    end-to-end through the UI (no API-only shortcut — testing.md point 8).

    ⚠ DEPENDS ON THE BIND LEG (`docs/goal/architecture/account-sync-plane.md`
    § The bind leg), ruled and not built: A's custody row reaches B only when
    the runtime bound to B in launch 2 completes B's replica with every fleet
    row the device holds. Until that leg lands this test is EXPECTED RED at the
    `recover-box-item-1` assertion — deliberately neither xfail nor skip: the
    red is the guard, and the green is the bind leg's landing gate. (The test
    name predates the plane and still says "fan out"; the seed-map fan-out it
    named is retired — the name is kept only because the feature ledgers key
    on it.) Only the observable is asserted — both rows listed — never a
    mechanism (no log line, no file).
    """
    app_path = _app_path(client)
    (nest_a, nest_b), stop_boxes = _start_boxes(
        request, nest_mode, tmp_path_factory, "box-recovery-a", "box-recovery-b")
    try:
        port_a = nest_a["port"]
        port_b = nest_b["port"]
        secret_hex = secrets.token_hex(32)
        localpart = "admin" + secrets.token_hex(3)
        handle_a = f"{localpart}@127.0.0.1:{port_a}"
        handle_b = f"{localpart}@127.0.0.1:{port_b}"

        xdg_h1 = str(tmp_path / "h1-xdg")
        creds_h1 = str(tmp_path / "h1-creds")
        keyring_h1 = "fauna-e2e-box-recovery-h1"
        h1_config = {
            "app_path": app_path,
            "xdg_base": xdg_h1,
            "credential_dir": creds_h1,
            "keyring_app": keyring_h1,
            "environment": {
                # Both boxes this identity claims, named at launch: the escrow
                # trust set is fixed when the account runtime assembles.
                **_seeded_environment(request, nest_a, nest_b),
                "RUST_LOG": _rust_log(client),
            },
        }

        # ---- Launch 1 (H1): fresh identity, claim A. The custody leg lands
        # A's row on the plane. ----
        driver1 = create_driver(client)
        driver1.launch(h1_config)
        try:
            app1 = ActionLayer(driver1)
            _claim_fresh_identity(app1, secret_hex, handle_a,
                                  nest_a["claim_code"])
            # Not an assertion on the mechanism — a teardown barrier: feed-view
            # rendering does not mean the background custody leg has written.
            _wait_for_custody_leg(driver1)
        finally:
            driver1.teardown()

        # ---- Between launches: clear H1's stored nest_url, keep the identity ----
        _clear_stored_nest_url(creds_h1, keyring_h1)

        # ---- Launch 2 (same H1 -> Case 3 -> handle_entry): claim B. The
        # custody leg lands B's row; the bind leg (not built) is what would
        # also complete B's replica with A's row. ----
        driver1b = create_driver(client)
        driver1b.launch(h1_config)
        try:
            app1b = ActionLayer(driver1b)
            _drive_handle_check_and_claim(app1b, handle_b,
                                          nest_b["claim_code"])
            # Same teardown barrier, for B's row.
            _wait_for_custody_leg(driver1b)
        finally:
            driver1b.teardown()

        # ---- Launch 3 (fresh device, no account store, never saw A): list
        # the boxes by reaching B alone — the resolver's cold read of B. ----
        driver2 = create_driver(client)
        driver2.launch({
            "app_path": app_path,
            "environment": {
                **_seeded_environment(request, nest_a, nest_b),
                "RUST_LOG": _rust_log(client),
            },
        })
        try:
            app2 = ActionLayer(driver2)
            app2.onboarding.navigate_to_status()
            app2.click("recover-lost-box-button")
            app2.driver.wait_for("paste-secret-field", timeout=15)
            app2.driver.type_text("paste-secret-field", secret_hex)
            app2.click("import-submit-button")
            app2.driver.wait_for("handle-input", timeout=15)

            app2.onboarding.fill_handle(handle_b)
            app2.onboarding.run_handle_check(timeout=45)
            app2.onboarding.submit_handle()

            app2.driver.wait_for("recover-back-button", timeout=30)
            assert app2.is_visible("recover-back-button"), (
                "recovery-intent handle check against an owned box should land "
                f"on nest_recovery: {app2.driver.diagnose('recover-back-button')}"
            )

            app2.driver.wait_for("recover-box-item-0", timeout=30)
            assert app2.is_visible("recover-box-item-0"), (
                "the fresh device's box list (the pre-login resolver's cold read "
                "of the reachable nest B) never rendered box 0 — not even B's own "
                f"custody row: {app2.driver.diagnose('recover-box-item-0')}"
            )
            app2.driver.wait_for("recover-box-item-1", timeout=30)
            assert app2.is_visible("recover-box-item-1"), (
                "only ONE box rendered: a device that never saw box A must still "
                "list BOTH boxes by reaching box B alone (box-recovery.md § The "
                "plane-era recovery floor, (a)). EXPECTED RED until the bind leg "
                "lands (account-sync-plane.md § The bind leg) — nothing yet "
                "completes B's replica with A's custody row: "
                f"{app2.driver.diagnose('recover-box-item-1')}"
            )
        finally:
            driver2.teardown()
    finally:
        stop_boxes()


@pytest.mark.parametrize("client", _TWO_NEST_APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_launch_recover_button_appears_on_a_reachable_custodied_nest(
    client, nest_mode, tmp_path, tmp_path_factory, request
):
    """T3: the `launch-recover-button` surviving-device entry CTA (linux
    `views/launch.rs` / tui `launch.rs`'s transient-offline `route()` arm),
    against a REAL reachable nest holding the custodied box — riding a
    launch-flow harness the tier_2 suite's `set_recovery_boxes` injection
    cannot reach at all.

    box-recovery.md § Recovery UI (step 4): "Surviving-device entry
    (`launch-recover-button`)... revealed iff >=1 box" — linux
    (`client::load_recoverable_boxes` -> `LaunchView::set_recover_boxes`) and
    tui (`launch::fetch_launch_recover_boxes` -> `DataMessage::LaunchRecoverBoxes`,
    the `!recover_boxes.is_empty()` gate in `launch.rs`), both over the shared
    pre-login resolver (§ The plane-era recovery floor, (b) The reads): this
    device's own account store joined with a cold read of the saved nest. Here
    the saved nest answers the read, so both halves hold the row; T4 is the
    dead-nest twin, where only the local half can.
    """
    app_path = _app_path(client)
    (nest_b,), stop_boxes = _start_boxes(
        request, nest_mode, tmp_path_factory, "box-recovery-t3")
    try:
        port_b = nest_b["port"]
        secret_hex = secrets.token_hex(32)
        localpart = "admin" + secrets.token_hex(3)
        handle_b = f"{localpart}@127.0.0.1:{port_b}"

        xdg_h1 = str(tmp_path / "h1-xdg")
        creds_h1 = str(tmp_path / "h1-creds")
        keyring_h1 = "fauna-e2e-box-recovery-t3"
        h1_config = {
            "app_path": app_path,
            "xdg_base": xdg_h1,
            "credential_dir": creds_h1,
            "keyring_app": keyring_h1,
            "environment": {**_seeded_environment(request, nest_b),
                            "RUST_LOG": _rust_log(client)},
        }

        # ---- Launch 1: fresh identity, claim B directly. The custody leg
        # merges B's row into this device's account store and the plane
        # publishes it to B. ----
        driver1 = create_driver(client)
        driver1.launch(h1_config)
        try:
            app1 = ActionLayer(driver1)
            _claim_fresh_identity(app1, secret_hex, handle_b,
                                  nest_b["claim_code"])
            # The custody leg runs in the background; launch 2 reads the row it
            # writes, so it must have landed before teardown.
            _wait_for_custody_leg(driver1)
        finally:
            driver1.teardown()

        # ---- Rewrite the stored node_url to route through a proxy that
        # refuses its first connection (the silent challenge) then forwards
        # every later one to the real, never-restarted, always-healthy nest B. ----
        proxy = _FirstConnectionRefusingProxy(target_port=port_b)
        try:
            original_url = _rewrite_stored_node_url(
                creds_h1, keyring_h1, f"https://127.0.0.1:{proxy.port}",
            )
            assert f":{port_b}" in original_url, (
                f"expected the claimed node_url to carry port {port_b}: {original_url!r}"
            )

            # ---- Launch 2 (same H1): Case 1 -> silent challenge THROUGH the
            # proxy. First connection (the challenge) refused -> Transient ->
            # TransientRetry. The resolver's cold read opens later connections
            # -> forwarded to the real nest B; joined with the device's own
            # store -> >=1 custodied box -> launch-recover-button visible. ----
            # Launch 2 dials B through the proxy's authority, which its seed
            # must name too (with B's identity: the proxy only forwards).
            _apply_r14_trust_env(
                h1_config["environment"],
                {"url": f"https://127.0.0.1:{proxy.port}", "port": port_b},
                request,
            )
            driver2 = create_driver(client)
            driver2.launch(h1_config)
            try:
                app2 = ActionLayer(driver2)
                app2.driver.wait_for("launch-transient-error", timeout=30)
                assert app2.is_visible("launch-retry-button"), (
                    "expected the TransientRetry surface (retry button) after the "
                    "proxy refused the first connection: "
                    f"{app2.driver.diagnose('launch-retry-button')}"
                )

                app2.driver.wait_for("launch-recover-button", timeout=30)
                assert app2.is_visible("launch-recover-button"), (
                    "launch-recover-button never appeared — the launch-time "
                    "pre-login resolver (this device's store joined with the "
                    "reachable nest's cold read) never found >=1 custodied box: "
                    f"{app2.driver.diagnose('launch-recover-button')}"
                )

                app2.click("launch-recover-button")
                app2.driver.wait_for("recover-back-button", timeout=30)
                assert app2.is_visible("recover-box-item-0"), (
                    "clicking launch-recover-button should seed nest_recovery with "
                    f"the launch-time box list: {app2.driver.diagnose('recover-box-item-0')}"
                )
            finally:
                driver2.teardown()
        finally:
            proxy.stop()
    finally:
        stop_boxes()


@pytest.mark.parametrize("client", _DEAD_NEST_APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_launch_recover_button_appears_when_the_saved_nest_is_dead(
    client, nest_mode, tmp_path, tmp_path_factory, request
):
    """T4: the dead-saved-nest assertion — on a surviving device whose saved
    nest IS the lost box, launch-retry still reveals `launch-recover-button`
    and `nest_recovery` lists the box, from the device's own account store.

    box-recovery.md § The plane-era recovery floor, (b) The reads: "a surviving
    device's stored nest is, in the case recovery exists for, the dead box" —
    so the pre-login resolver joins the local read with the cold read, never
    either-or; and § Implementation status today, the 2026-09-30 finding the
    consumer cut fixed (the launch entry used to read only the saved nest, so
    its button never appeared exactly when it was needed).

    Flow: claim box A on device H1 and wait for the custody leg (A's row now
    rests in H1's own store) → STOP A and leave it down → relaunch H1 unchanged
    (same XDG dirs, credentials and saved `nest_url`) → Case 1's silent
    challenge reaches nothing → Transient → launch-retry; the resolver's cold
    read fails too, so only the local read can answer.
    """
    app_path = _app_path(client)
    (nest_a,), stop_boxes = _start_boxes(
        request, nest_mode, tmp_path_factory, "box-recovery-t4")
    try:
        port_a = nest_a["port"]
        secret_hex = secrets.token_hex(32)
        localpart = "admin" + secrets.token_hex(3)
        handle_a = f"{localpart}@127.0.0.1:{port_a}"

        xdg_h1 = str(tmp_path / "h1-xdg")
        creds_h1 = str(tmp_path / "h1-creds")
        keyring_h1 = "fauna-e2e-box-recovery-t4"
        h1_config = {
            "app_path": app_path,
            "xdg_base": xdg_h1,
            "credential_dir": creds_h1,
            "keyring_app": keyring_h1,
            "environment": {**_seeded_environment(request, nest_a),
                            "RUST_LOG": _rust_log(client)},
        }
        if client == "windows":
            # Both launches are driver instances of the SAME device: pin the
            # `%LOCALAPPDATA%` ancestor (the account-store root hangs off it) AND
            # the data dir under it. A data dir the driver derived itself is the
            # driver's to delete at teardown — taking the account store, and the
            # row launch 1 custodied, with it — while a caller-supplied one
            # outlives the instance.
            h1_config["local_appdata"] = str(tmp_path / "h1-localappdata")
            h1_config["data_dir"] = str(tmp_path / "h1-localappdata" / "Fauna")
        if client == "macos":
            # Both launches are driver instances of the SAME device: pin the HOME
            # (Application Support — where the account-store root, and so the row
            # launch 1 custodied, lives) and the credential dir the driver would
            # otherwise mint fresh per launch. Left to its default each launch is a
            # separate install and launch 2 would boot a signed-out device.
            h1_config["home"] = str(tmp_path / "h1-home")
            h1_config["credential_dir"] = creds_h1

        # ---- Launch 1: fresh identity, claim A. The custody leg merges A's
        # row into this device's own account store. ----
        driver1 = create_driver(client)
        driver1.launch(h1_config)
        try:
            app1 = ActionLayer(driver1)
            _claim_fresh_identity(app1, secret_hex, handle_a,
                                  nest_a["claim_code"])
            # The row launch 2 must find locally: wait for the leg's own
            # confirmed outcome before tearing the device down.
            _wait_for_custody_leg(driver1)
        finally:
            driver1.teardown()

        # ---- The saved nest dies. `stop_nest` signals only the nest THIS run
        # started, through its own handle (standalone's Popen, docker's
        # container adapter) — never a name match; the data dir is left alone
        # and the nest is left DOWN. `stop_boxes()` below is still safe: its
        # cleanup of an already-exited nest is a no-op. ----
        stop_nest(nest_a, graceful=True)

        # ---- Launch 2 (same H1, saved nest_url untouched): Case 1 -> silent
        # challenge to the dead box -> Transient -> TransientRetry. The
        # resolver's cold read of the dead box fails; the local read of H1's
        # own store answers A's row -> launch-recover-button visible. ----
        driver2 = create_driver(client)
        driver2.launch(h1_config)
        try:
            app2 = ActionLayer(driver2)
            # macOS renders the launch error in `error-message`, not
            # `launch-transient-error` (a declared apple divergence,
            # `ui-actual-macos.yaml` § launch_retry), so the retry button is its
            # retry-surface landmark.
            app2.driver.wait_for(
                "launch-retry-button" if client == "macos"
                else "launch-transient-error", timeout=60)
            assert app2.is_visible("launch-retry-button"), (
                "expected the TransientRetry surface (retry button) with the saved "
                "nest stopped — an unreachable saved nest is a Transient silent "
                f"challenge: {app2.driver.diagnose('launch-retry-button')}"
            )

            app2.driver.wait_for("launch-recover-button", timeout=30)
            assert app2.is_visible("launch-recover-button"), (
                "launch-recover-button never appeared with the saved nest dead — "
                "the pre-login resolver's LOCAL read (this device's own account "
                "store) must reveal the custodied box on its own when the cold "
                "read of the saved nest cannot answer (box-recovery.md § The "
                "plane-era recovery floor, (b)): "
                f"{app2.driver.diagnose('launch-recover-button')}"
            )

            app2.click("launch-recover-button")
            app2.driver.wait_for("recover-back-button", timeout=30)
            app2.driver.wait_for("recover-box-item-0", timeout=30)
            assert app2.is_visible("recover-box-item-0"), (
                "nest_recovery should list the dead box from this device's own "
                f"store: {app2.driver.diagnose('recover-box-item-0')}"
            )
            # Exactly the one box this identity custodied — A. A second row
            # would mean the list came from somewhere other than this
            # device's own custody of A.
            assert app2.is_absent("recover-box-item-1"), (
                "nest_recovery listed more than the one box this identity "
                f"custodied: {app2.driver.diagnose('recover-box-item-1')}"
            )
        finally:
            driver2.teardown()
    finally:
        stop_boxes()


def _enter_nest_recovery(app, secret_hex: str, handle: str) -> None:
    """On a fresh device: recovery intent, import the identity, check `handle`
    and land on `nest_recovery`."""
    app.onboarding.navigate_to_status()
    app.click("recover-lost-box-button")
    app.driver.wait_for("paste-secret-field", timeout=15)
    app.driver.type_text("paste-secret-field", secret_hex)
    app.click("import-submit-button")
    _recheck_into_nest_recovery(app, handle)


def _recheck_into_nest_recovery(app, handle: str) -> None:
    """From handle_entry under recovery intent: check `handle` and land on
    `nest_recovery`, whose page entry re-reads the box list."""
    app.driver.wait_for("handle-input", timeout=15)
    app.onboarding.fill_handle(handle)
    app.onboarding.run_handle_check(timeout=45)
    app.onboarding.submit_handle()
    app.driver.wait_for("recover-back-button", timeout=30)


def _recover_box_labels(app) -> list[str]:
    """The label of every `recover-box-item-N` row now rendered, in order."""
    labels = []
    while app.is_visible(f"recover-box-item-{len(labels)}"):
        labels.append(app.driver.get_text(f"recover-box-item-{len(labels)}") or "")
    return labels


class _LinkedSeat:
    """T5's per-app seam: how the admin's device H1 launches bound to nothing,
    how it comes back over the same store, and how a fresh device that can
    reach only box B appears. Everything between — the claim, the link, the
    recovery pages — is the same calls on every app.

    Native (tui, linux): a process per launch over one XDG/credential tree,
    its own sync agent beside it (the module docstring's T5 note). Web: one
    browser tab, reloaded; a fresh device is a twin page in its own
    BrowserContext."""

    def __init__(self, client, request, tmp_path, nest_a, nest_b):
        self.client = client
        self.browser = client == "web"
        self._request = request
        self._tmp_path = tmp_path
        self._nest_a = nest_a
        self._nest_b = nest_b
        self._app_path = None if self.browser else _app_path(client)

    def _h1_config(self) -> dict:
        """H1's launch config, the same store on every launch. Both boxes are
        named in it: the escrow trust set is fixed when the account runtime
        assembles."""
        environment = _seeded_environment(
            self._request, self._nest_a, self._nest_b)
        if self.browser:
            return {
                "url": f"{self._request.getfixturevalue('spa_url')}/app/",
                "environment": environment,
            }
        return {
            "app_path": self._app_path,
            "xdg_base": str(self._tmp_path / "h1-xdg"),
            "credential_dir": str(self._tmp_path / "h1-creds"),
            "keyring_app": "fauna-e2e-box-recovery-t5",
            "environment": {
                **environment,
                "RUST_LOG": _rust_log(self.client),
            },
        }

    def launch(self):
        """H1's first launch, on identity_choice."""
        driver = create_driver(self.client)
        driver.launch(self._h1_config())
        if self.browser:
            # The wizard's nest leg and the session's socket both dial box A
            # (`set_provider_base_urls` re-mounts the page, so before any
            # identity exists).
            driver.set_provider_base_urls({"nest": self._nest_a["url"]})
        return driver

    def await_bound_custody(self, driver) -> None:
        """A barrier before H1 goes down, native only: the custody leg's own
        line for box A. A tab is never torn down between the two launches — a
        reload re-runs the leg at its store-ready edge — and the fresh device's
        box list is the observable either way."""
        if not self.browser:
            _wait_for_custody_leg(driver)

    def relaunch(self, driver):
        """H1 again over the same store, still bound to A: the relaunch's
        first wake runs the secondary leg — in the prologue pass when the app
        holds the engine role, in its seed pass when its agent does."""
        if self.browser:
            driver.hard_reload()
            return driver
        driver.teardown()
        again = create_driver(self.client)
        again.launch(self._h1_config())
        return again

    def await_linked_custody(self, driver) -> None:
        """Native only: the linked custody leg's own line, so a red names the
        leg that did not run. A browser's log is a bounded console ring whose
        absences prove nothing (`log_scope_across_relaunch`); there the fresh
        device's poll is the wait."""
        if not self.browser:
            _wait_for_log_containing_any(
                driver, _LINKED_CUSTODY_NEEDLES, timeout=120.0)

    def fresh_device(self, holder):
        """A device with no store that reaches only box B, on identity_choice."""
        if self.browser:
            twin = holder.open_twin_page()
            twin.set_provider_base_urls({"nest": self._nest_b["url"]})
            return twin
        driver = create_driver(self.client)
        driver.launch({
            "app_path": self._app_path,
            "environment": {
                **_seeded_environment(self._request, self._nest_a, self._nest_b),
                "RUST_LOG": _rust_log(self.client),
            },
        })
        return driver


def _fresh_device_resolves_row(seat, secret_hex, handle, row: int, *,
                               holder) -> str:
    """Bring up a fresh device, reach only `handle`'s nest under recovery
    intent, wait until `nest_recovery` lists two boxes, pick row `row` and
    resolve its self-hosted command. Returns the nest id the command's seed
    derives to.

    `holder` is the still-running device whose pass completes that nest's
    replica; its log tail is read into a failure."""
    from nacl.signing import SigningKey

    driver = seat.fresh_device(holder)
    try:
        app = ActionLayer(driver)
        _enter_nest_recovery(app, secret_hex, handle)
        # The list is read on page entry, and the replica is completed by the
        # holder's pass in the background: re-enter the page until both boxes
        # are listed. A state poll, bounded only against a hang.
        deadline = time.monotonic() + 180.0
        labels = _recover_box_labels(app)
        while len(labels) < 2 and time.monotonic() < deadline:
            app.click("recover-back-button")
            _recheck_into_nest_recovery(app, handle)
            labels = _recover_box_labels(app)
        assert len(labels) == 2, (
            "a fresh device reaching only the linked box must list BOTH boxes "
            "— the linked one (custodied over the linked connection) and the "
            "bound one (its row completed onto the linked box by the secondary "
            f"leg): listed {len(labels)} ({labels!r}).\n--- holder log tail ---\n"
            f"{holder.app_stderr_text()[-8000:]}"
        )

        app.click(f"recover-box-item-{row}")
        app.click("recover-method-selfhosted-button")
        app.driver.wait_for("recover-selfhosted-command", timeout=30)

        def _command_seeds() -> list[str]:
            command = app.driver.get_text("recover-selfhosted-command") or ""
            return [
                token for token in command.replace("=", " ").split()
                if len(token) == 64
                and all(c in "0123456789abcdef" for c in token.lower())
            ]

        # The page paints a placeholder until the resolver answers the selected
        # box's seed: poll the element, bounded against a hang.
        deadline = time.monotonic() + 60.0
        seeds = _command_seeds()
        while not seeds and time.monotonic() < deadline:
            time.sleep(0.5)
            seeds = _command_seeds()
        assert len(seeds) == 1, (
            f"row {row}'s self-hosted command should carry exactly one seed: "
            f"{len(seeds)} found ({app.driver.diagnose('recover-selfhosted-command')})"
        )
        return bytes(SigningKey(bytes.fromhex(seeds[0])).verify_key).hex()
    finally:
        driver.teardown()


@pytest.mark.parametrize("client", _LINKED_APPS)
@pytest.mark.feature("recover-a-lost-nest")
def test_linking_a_box_custodies_it_and_a_fresh_device_recovers_both(
    client, nest_mode, tmp_path, tmp_path_factory, request
):
    """T5: an admin who LINKS their boxes custodies every one of them without
    binding a device to each — and a fresh device reaching only the linked box
    lists both and resolves each one's self-hosted recovery command.

    box-recovery.md § The plane-era recovery floor, (c) The writes: "The leg
    also runs over each linked nest's connection"; (a): the linked nest is a
    replica of the account plane, completed by the user's own devices.

    Device H1 claims box A and stays bound to it — its stored nest URL is never
    cleared (T2's device re-binds to B; this one never does). Box B is already
    one of this identity's boxes: the harness claims it with the same identity,
    standing for a claim made long ago from a device since gone — a
    precondition, not a step of the journey. Every step of the journey itself
    goes through the UI: the claim of A, the link of B on the Nests page, the
    fresh device's recovery.

    The linked custody runs at the account runtime's first wake after the
    link, so H1 is relaunched and left RUNNING while the fresh device reads:
    its pass (or its seed pass, beside an agent that holds the engine role)
    both custodies B and completes B's replica. On web the tab is that
    runtime, and the per-app differences are `_LinkedSeat`'s.
    """
    from nacl.signing import SigningKey

    if client != "web":
        _app_path(client)  # skip before any box starts when the app is unbuilt
    (nest_a, nest_b), stop_boxes = _start_boxes(
        request, nest_mode, tmp_path_factory, "box-recovery-a", "box-recovery-b",
        browser=client == "web")
    try:
        port_a = nest_a["port"]
        port_b = nest_b["port"]
        identity = SigningKey.generate()
        secret_hex = identity.encode().hex()
        localpart = "admin" + secrets.token_hex(3)
        handle_a = f"{localpart}@127.0.0.1:{port_a}"
        handle_b = f"{localpart}@127.0.0.1:{port_b}"

        # The precondition: B is already this identity's box.
        claim_admin(port_b, nest_b["claim_code"], base_url=nest_b.get("url"),
                    handle=localpart, signing_key=identity)

        seat = _LinkedSeat(client, request, tmp_path, nest_a, nest_b)

        # ---- Launch 1 (H1): claim A, then link B on the Nests page. ----
        driver1 = seat.launch()
        try:
            app1 = ActionLayer(driver1)
            _claim_fresh_identity(app1, secret_hex, handle_a,
                                  nest_a["claim_code"])
            seat.await_bound_custody(driver1)

            app1.linked_nests.navigate()
            assert app1.linked_nests.is_page_visible(), (
                "the Nests page never rendered: "
                f"{app1.driver.diagnose('nests-add-button')}"
            )
            app1.linked_nests.link(nest_b["url"])
            assert app1.linked_nests.wait_for_pairing_count(1, timeout=30.0), (
                "linking box B by its address should add one pairing. "
                f"error: {app1.linked_nests.page_error_text()!r}"
            )
        except BaseException:
            driver1.teardown()
            raise

        # ---- Launch 2 (same H1, still bound to A): the prologue pass opens
        # the linked connection to B, runs the custody leg over it and
        # completes B's replica. H1 stays up while the fresh device reads. ----
        driver1b = seat.relaunch(driver1)
        try:
            # A native relaunch lands on the feed; a reloaded tab comes back on
            # the Nests page it linked from, whose shell shows no feed landmark.
            _wait_for_authed_app_or_fail(
                ActionLayer(driver1b),
                markers=_LOGGED_IN_MARKERS + ("nests-add-button",))
            # Assembled in either role: the app runs the linked leg whether it
            # or its sync agent holds the engine (the seed-leg role).
            await_account_runtime_assembled(driver1b)
            seat.await_linked_custody(driver1b)

            # ---- Launches 3 and 4 (fresh devices, each reaching only B): one
            # per listed box, since the instructions page has no way back.
            # Each lists BOTH boxes and resolves one row's self-hosted command;
            # between them the two commands carry the two boxes' own seeds.
            # Rows are picked by index, never by label — a row's text is not
            # readable through every driver. ----
            resolved = [
                _fresh_device_resolves_row(
                    seat, secret_hex, handle_b, row, holder=driver1b)
                for row in (0, 1)
            ]
            id_a = _nest_id_hex(port_a).lower()
            id_b = _nest_id_hex(port_b).lower()
            assert set(resolved) == {id_a, id_b}, (
                "the two listed boxes' self-hosted commands must carry box A's "
                "and box B's own deployment seeds — each derives to its box's "
                f"nest id. Resolved {[r[:8] for r in resolved]}; A is "
                f"{id_a[:8]}…, B is {id_b[:8]}…"
            )
        finally:
            driver1b.teardown()
    finally:
        stop_boxes()
