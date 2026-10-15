"""The box-recovery launch journeys' shared steps: start unclaimed boxes
through the run's nest provider, launch a device, claim a box with a fresh
identity, and wait on the custody leg that rests the box's recovery seed in
the device's own account store (docs/goal/architecture/box-recovery.md
§ The plane-era recovery floor).

Lifted out of ``tests/test_box_recovery_two_nest.py`` so a journey in another
module — the retire view's "retiring keeps the recovery key"
(``tests/test_nest_retire.py``) — drives the identical flow rather than a copy.
"""

from __future__ import annotations

import json
import os
import time
from pathlib import Path

import pytest

from conftest import (
    APP_PATHS,
    _repo_root,
    _resolve_cli_binary,
    _resolve_linux_binary,
    _resolve_windows_app,
    browser_origins_to_allow,
)
from helpers import nest_mode as nm


# The client's own tracing target, for RUST_LOG — linux's crate is
# `fauna-desktop`, tui's is `fauna-tui`. Raised to debug so a red diagnoses
# itself (launch routing, the resolver's per-source failures).
# windows has no app crate of its own (its glue is C#), so its tracing target is
# the FFI crate; so do macos and android (their glue is Swift / Kotlin over the
# same FFI).
RUST_LOG_TARGET = {
    "linux": "fauna_desktop", "tui": "fauna_tui",
    "windows": "fauna_ffi", "macos": "fauna_ffi", "android": "fauna_ffi",
}

# The custody leg runs in the shared runtime crate, not the app crate, so its
# target is named explicitly. Its `NotAdmin` outcome and the mark reconcile's
# deferral log at debug!, which is why it is raised to debug too: at plain
# info a leg that ran and found "not an admin" is indistinguishable from one
# that never ran.
CUSTODY_LEG_TARGET = "fauna_client_account_runtime"

# The custody leg's two confirmed outcomes (`deployment_seeds::run_custody_leg`,
# both INFO): `captured` merged a new row, `held` found the fold already
# holding one. Either means this box's row rests in the device's own account
# store. Its unconfirmed outcome (WARN) is `CUSTODY_LEG_UNCONFIRMED`, read into
# the failure message rather than waited on.
CUSTODY_LEG_NEEDLES = (
    "deployment seed custodied off-box on the account plane (custody leg: captured)",
    "deployment seed already custodied off-box on the account plane (custody leg: held)",
)
CUSTODY_LEG_UNCONFIRMED = "deployment-seed custody leg: custody unconfirmed"


# The custody leg over a LINKED nest runs inside the account runtime's pass
# (`fauna_account_plane::account_driver::pass::linked_custody`): `captured` is
# INFO, `held` — a pass that found the row already there — is DEBUG, so the
# pass module is raised to debug for the launches that wait on either.
LINKED_CUSTODY_TARGET = "fauna_account_plane::account_driver::pass"
LINKED_CUSTODY_NEEDLES = (
    "(linked custody leg: captured)",
    "(linked custody leg: held)",
)


def rust_log(client: str) -> str:
    """RUST_LOG for one launch: info everywhere, debug on the app's own crate,
    on the custody leg's crate and on the pass that runs its linked arm."""
    return (
        f"info,{RUST_LOG_TARGET[client]}=debug,{CUSTODY_LEG_TARGET}=debug,"
        f"{LINKED_CUSTODY_TARGET}=debug"
    )

# The authed app shell has mounted once any main-view landmark is visible
# (mirrors test_mail_enable_at_admin_claim.py's LOGGED_IN_MARKERS). Both
# apps under test register both IDs — linux `feed/mod.rs` + tab bar, tui
# `feed/mod.rs:618` + `pages.rs:108` — so the check is client-uniform.
LOGGED_IN_MARKERS = ("feed-view", "feed-tab")


def app_path_for(client: str) -> str:
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
    elif client == "android":
        path = _repo_root / APP_PATHS["android"]
        if not path.exists():
            path = None
        hint = "just android-debug"
    else:  # tui
        path = _resolve_cli_binary()
        hint = "cargo build -p fauna-tui"
    if not path:
        pytest.skip(f"{client} app not built — run '{hint}' first")
    return str(path)


def start_boxes(request, nest_mode, tmp_path_factory, *labels,
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


def nest_url_keys(data: dict) -> list[str]:
    """Every key holding a nest_url-shaped value: the registry's per-actor
    `fauna/{actor}/nest_url` row launch routes on."""
    return [k for k in data if k.endswith("/nest_url")]


def has_identity_secret(data: dict) -> bool:
    """The claim has written this identity's secret to the credential file —
    the per-actor `fauna/{actor}/secret` slot both apps write at claim time."""
    return any(k.endswith("/secret") for k in data)


def clear_stored_nest_url(creds_dir: str, keyring_app: str) -> None:
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
        if has_identity_secret(data):
            break
        time.sleep(0.2)
    assert has_identity_secret(data), (
        f"expected the claim to have written an identity secret to {path}, "
        f"found: {data}"
    )
    keys = nest_url_keys(data)
    assert keys, f"expected a stored nest_url key in {path}, found: {data}"
    cleared = {k: v for k, v in data.items() if k not in keys}
    path.write_text(json.dumps(cleared, indent=2))
    os.chmod(path, 0o600)


def rewrite_stored_node_url(creds_dir: str, keyring_app: str, new_url: str) -> str:
    """Point every stored nest_url-shaped key at `new_url` (keeping the
    identity + everything else). Returns the ORIGINAL url (pre-rewrite) so
    the caller can sanity-check it."""
    path = Path(creds_dir) / f"{keyring_app}.json"
    data = json.loads(path.read_text())
    keys = nest_url_keys(data)
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


def wait_for_authed_app_or_fail(app, markers=LOGGED_IN_MARKERS) -> None:
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
    # A tab's route names the page that failed to render — an empty tree alone
    # does not say whether the app ever left the wizard.
    where = ""
    if app.driver.is_web():
        try:
            where = f" page path: {app.driver.eval_js('location.pathname')!r}."
        except Exception as e:  # pragma: no cover - diagnostic only
            where = f" (page path read failed: {e})."
    pytest.fail(
        "admin never reached the authed app after claim + nat_mode_choice. "
        f"launch error: {err!r}.{where}\n--- accessibility tree ---\n{tree}"
        f"\n--- app log tail ---\n{log}"
    )


def wait_for_log_containing_any(driver, needles, timeout: float = 20.0) -> str:
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
        if CUSTODY_LEG_UNCONFIRMED in text else ""
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


def wait_for_custody_leg(driver, timeout: float = 60.0) -> str:
    """Wait until the custody leg reports this launch's bound box custodied on
    the account plane (`captured` or `held`).

    The leg runs in the background off the store-ready and post-auth edges —
    the UI reaching feed-view does NOT mean it has fetched the seed, merged the
    row into the device's own account store and handed it to the plane.
    Tearing the driver down on the UI signal races that write against process
    teardown, and every later launch of this device (T4's dead-nest local read)
    or of a sibling (T2's cold read) reads what it wrote."""
    return wait_for_log_containing_any(driver, CUSTODY_LEG_NEEDLES, timeout=timeout)


def drive_handle_check_and_claim(app, typed_handle: str, claim_code: str) -> None:
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

    wait_for_authed_app_or_fail(app)


def claim_fresh_identity(app, secret_hex: str, typed_handle: str,
                          claim_code: str) -> None:
    """From identity_choice: import `secret_hex` as a brand-new identity, then
    claim the nest `typed_handle` resolves to."""
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(secret_hex)  # -> handle_entry
    drive_handle_check_and_claim(app, typed_handle, claim_code)
