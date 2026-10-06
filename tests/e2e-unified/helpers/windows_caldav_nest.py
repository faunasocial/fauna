"""Stand up a *serving* Windows-native CalDAV + IMAP nest, headlessly.

The reusable engine behind the Windows CalDAV/IMAP serving e2e (Track D of
the desktop-native CalDAV/IMAP design, tracked internally).
**No Fauna app** — a Python ``CalDAVClient`` / ``imaplib`` IS the e2e client
(the spec's premise). It drives the full *domainless* serving path against the
Windows binaries:

  Stage A — spawn ``fauna-nest-svc.exe --foreground`` on a loopback port, wait
    for it to write its claim code (``<data_dir>/claim-code``) + accept TCP,
    and headless-claim admin (handle ``test@127.0.0.1:<port>``). The nest is
    content-ready from first boot (no-modes retirement, ratified 2026-07-12 —
    every nest is sealed at rest unconditionally, so there is no longer a
    storage-mode commit gating CalDAV).
  Stage B — enable CalDAV (``fauna.bridges.set_caldav_enabled``, Admin) and mint
    a KNOWN-password PLAIN credential for the claimed actor, all over WS-RPC +
    the test-only ``seal-helper`` (the client-side wrapped-blob seal a Fauna
    app UI normally performs — here driven as a subprocess so no client is
    needed). Domainless ⇒ NO account alias (login resolves the bare
    handle ``test`` via the handle→actor store) and NO per-domain cert.
  Stage C — spawn the ``fauna-mail-bridge.exe`` MDA (one process serves IMAP +
    CalDAV): build the ``mda.key`` service-user keyfile, ``register_user`` + HTTP
    pre-approve its enrollment, hand-poke its x25519 into ``bridge_service_users``
    (the floor-cert seal target), write the operator-hatch with the
    ``caldav_listen_https`` / ``imap_listen_*`` binds, and spawn it. The MDA
    fetches nest's self-signed **floor** cert (no ``mail.<domain>`` cert) and
    serves CalDAV/IMAP off it; MUAs connect with ``verify=False``.

Returns a :class:`WindowsCaldavNest` (nest_url, caldav_base_url, imaps_port,
auth_username, password) + ``cleanup()``. The install-gated variant
(increment 3) reuses Stages B–D and swaps only the Stage-A startup (the MSI's
SCM services instead of a standalone spawn).

Prior art ported here (domainless variant — see each step):
  - ``conftest.py::_spawn_mda_bridge`` — the MDA enroll/spawn seam (keyfile,
    register+pre-approve, x25519 poke, operator-hatch, Popen).
  - ``conftest.py::_provision_msek_recipient`` — the headless seal-helper
    credential mint (``provision_recipient_mls_pubkey`` +
    ``provision_wrapped_mls_blob`` + ``provision_mls_snapshot_blob``).
  - ``helpers/caldav_onboarding.py::_provision_serving_bits`` (domainless branch)
    + ``_auth_username`` (the RFC-7617 port-strip).
  - ``common/auth.py::{claim_admin, register_user}``.

Authority: ``docs/goal/behavior/caldav-server.md`` § Process topology / §
Network exposure (desktop-IP) / § Authentication (any-locator login) / §
Independent enablement.
"""

from __future__ import annotations

import base64
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

from drivers.port_util import (
    find_free_port,
    find_free_ports,
    popen_group_kwargs,
    reap_descendants_of,
    track_process,
    untrack_process,
)

# The handle local part + a known PLAIN password the MUA authenticates with.
# Mirrors the proven-green domainless matrix (`helpers/caldav_onboarding.py`):
# handle CARRIES its locator (`test@<locator>`); the login resolves the bare
# local part `test` via the handle→actor store (any-locator design).
HANDLE_LOCAL = "test"
DEFAULT_MUA_PASSWORD = "CalDavWinServePw0042HhZz"  # gitleaks:allow
# The single shared bridge credential id. A bare username (no `+suffix`) selects
# the actor's `default` credential — we mint `default` with a KNOWN password so
# the MUA authenticates as just `test` (no sub-addressing needed).
CREDENTIAL_ID = "default"


# ── Binary discovery (skip with a build hint when missing, like the existing
# increment-1 test's `_nest_svc_exe`). ───────────────────────────────────────


def _repo_root() -> Path:
    """Repo root via git, with the leading-slash → drive-letter fix the other
    Windows e2e tests apply (`test_installer.py::_get_repo_root`)."""
    here = os.path.dirname(os.path.abspath(__file__))
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()
    if sys.platform == "win32" and out.startswith("/"):
        out = out[1].upper() + ":" + out[2:]
    return Path(os.path.normpath(out))


def nest_svc_exe() -> Path:
    """The Windows FaunaNest service binary (release preferred, debug fallback).

    fauna-nest-service is a ROOT-workspace member (the nested apps/fauna-windows
    workspace was unified away 2026-07-22), so it builds into the root `target/`.
    Both cargo layouts are searched: `target\\<profile>\\` (an implicit-host build —
    since 2026-08-23 the shape the dev loop uses, because win is arm64 and the
    explicit triple named its own host, buying a second artifact tree for nothing;
    measured 0 of 81 workspace-local units shared, 76 of 81 after the drop) and
    `target\\<triple>\\<profile>\\`, which the INSTALLER and release paths still use
    by design — they build both arches from one machine, so their explicit triple is
    load-bearing.

    The exe must be the ``test-hooks`` FLAVOR. This leg dials the service over
    plain ``http://`` (:func:`start_windows_caldav_nest` below), which only works
    because ``desktop_serve::run_serve_loop`` honours ``FAUNA_INSECURE_DISABLE_TLS``
    — and that escape is compiled out of the release flavor (convention 15: the
    compile-time exclusion is the outer security boundary). A release-flavor exe
    therefore serves HTTPS from the self-signed floor and every request here dies
    on a transport error naming nothing about features. Since this helper *finds* a
    pre-built exe rather than building one, a stale one from before the flavor split
    — or from a plain ``cargo build -p fauna-nest-service`` — is the likely case,
    so it is worth naming precisely. Checked by the escape's own strings-witness,
    which is the same artifact property convention 15 pins on the release side.
    """
    root = _repo_root() / "target"
    triple = "aarch64-pc-windows-msvc"
    # The slot-wrapped recipe, never a hand-spelled `cargo-win.cmd build`: on
    # win compiles only through the recipe.
    build_cmd = "just windows-service-build fauna-nest-service test-hooks"
    for profile in ("release", "debug"):
        for exe in (root / triple / profile / "fauna-nest-svc.exe",
                    root / profile / "fauna-nest-svc.exe"):
            if not exe.exists():
                continue
            if b"FAUNA_INSECURE_DISABLE_TLS" not in exe.read_bytes():
                pytest.skip(
                    f"{exe} is the RELEASE flavor (no test-hooks) — it serves HTTPS "
                    f"and this leg dials http://. Rebuild with `{build_cmd}`"
                )
            return exe
    pytest.skip(f"fauna-nest-svc.exe not built — run `{build_cmd}`")


def _root_target_dir() -> Path:
    """The root `target/` dir — where `windows-mail-bridge-build` /
    `windows-seal-helper-build` stage the Go exes BESIDE `fauna_ffi.dll` +
    `libunwind.dll`. The MDA + seal-helper must run from here so Windows'
    image-directory DLL search resolves the FFI dll."""
    return _repo_root() / "target"


def mda_bridge_exe() -> Path:
    exe = _root_target_dir() / "fauna-mail-bridge.exe"
    if not exe.exists():
        pytest.skip(
            "fauna-mail-bridge.exe not built — run `just windows-mail-bridge-build` "
            "(stages fauna_ffi.dll + libunwind.dll beside it in target/)"
        )
    return exe


def seal_helper_exe() -> Path:
    exe = _root_target_dir() / "seal-helper-testonly.exe"
    if not exe.exists():
        pytest.skip(
            "seal-helper-testonly.exe not built — run `just windows-seal-helper-build` "
            "(stages fauna_ffi.dll + libunwind.dll beside it in target/)"
        )
    return exe


# ── seal-helper subprocess (the client-side wrapped-blob seal) ───────────────


def run_seal_helper(subcommand: str, params: dict) -> bytes:
    """Subprocess the Windows seal-helper for one client-side seal; return blob
    bytes. Windows analogue of `conftest._run_seal_helper`'s `LD_LIBRARY_PATH`
    block: the dll sits BESIDE the exe in `target/`, which Windows' loader
    searches first, so we just run from that cwd (and put it on PATH defensively).
    Binary params arrive base64-encoded (JSON has no byte type)."""
    import json as _json

    exe = seal_helper_exe()
    target = _root_target_dir()
    env = os.environ.copy()
    env["PATH"] = f"{target}{os.pathsep}{env.get('PATH', '')}"
    proc = subprocess.run(
        [str(exe), subcommand],
        input=_json.dumps(params).encode(),
        capture_output=True,
        cwd=str(target),
        env=env,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"seal-helper {subcommand} failed (exit {proc.returncode}): "
            f"{proc.stderr.decode(errors='replace')}"
        )
    return base64.b64decode(proc.stdout.strip())


# ── small waiters ────────────────────────────────────────────────────────────


def _wait_tcp_accept(port: int, deadline: float) -> bool:
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1.0):
                return True
        except OSError:
            time.sleep(0.2)
    return False


def _wait_for_file(path: Path, deadline: float, proc: subprocess.Popen | None,
                   log_path: Path) -> str:
    """Wait for `path` to appear (and be non-empty).

    When `proc` is a Popen we own (the standalone-spawn path) its early exit is a
    hard failure surfaced with the log. When `proc` is None (the installed nest —
    the `FaunaNest` SCM service owns the process, not us) we just poll until the
    deadline; the service's own failure actions / SCM state are the liveness
    signal, not a Popen handle."""
    while time.monotonic() < deadline:
        try:
            txt = path.read_text().strip()
            if txt:
                return txt
        except OSError:
            pass
        if proc is not None and proc.poll() is not None:
            raise RuntimeError(
                f"fauna-nest-svc exited early (rc={proc.returncode}) before writing "
                f"{path.name}.\nlog:\n{_log_tail(log_path)}"
            )
        time.sleep(0.2)
    raise TimeoutError(
        f"{path} did not appear within the deadline.\nlog:\n{_log_tail(log_path)}"
    )


def _wait_metrics_healthz(port: int, timeout: float = 30.0) -> None:
    """Poll the MDA `/healthz` (HTTP metrics port) until 200, like
    `conftest._wait_for_mail_bridge_metrics`."""
    import urllib.request

    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1.0)
            if resp.status == 200:
                return
        except Exception as e:  # noqa: BLE001 — still booting
            last = e
        time.sleep(0.2)
    raise TimeoutError(
        f"MDA /healthz on {port} not ready within {timeout}s (last: {last})"
    )


def _log_tail(path: Path, n: int = 4000) -> str:
    try:
        return path.read_text(errors="replace")[-n:]
    except OSError:
        return f"(no {path.name})"


def _terminate(proc: subprocess.Popen | None) -> None:
    if proc is None:
        return
    try:
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
    finally:
        untrack_process(proc)
        fh = getattr(proc, "_fauna_log_fh", None)
        if fh is not None:
            fh.close()


def _spawn_logged(argv: list[str], log_path: Path, *, cwd: str | None = None,
                  env: dict | None = None) -> subprocess.Popen:
    log_fh = open(log_path, "wb")
    # `track_process` alone is NOT the die-with-the-run guarantee: it registers an
    # atexit teardown, which a SIGKILLed pytest never runs, and without a process
    # group `terminate_tree` degrades to single-process — so a grandchild outlives
    # the run either way. Both halves added 2026-08-14 (the Windows half is the one
    # that matters here and is a no-op elsewhere); the tracking stays, since it is
    # what reaps on an ORDERLY exit.
    proc = subprocess.Popen(
        argv, stdout=log_fh, stderr=subprocess.STDOUT, cwd=cwd, env=env,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    proc._fauna_log_fh = log_fh  # keep the handle alive for the proc's lifetime
    track_process(proc)
    return proc


def spawn_nest_svc(data_dir: Path, port: int) -> subprocess.Popen:
    """Spawn the FaunaNest service in its non-disruptive ``--foreground`` mode
    (no SCM registration) on loopback ``port`` against ``data_dir``. The single
    nest-svc spawn path, shared by the floor-cert test and the serving engine.
    The caller owns teardown; the process is also atexit-tracked (port_util) so a
    pytest crash can't leak it. Use :func:`terminate` to stop it."""
    return _spawn_logged(
        [str(nest_svc_exe()), "--foreground", "--port", str(port), "--data-dir", str(data_dir)],
        Path(data_dir) / "nest-svc.log",
    )


# Public aliases (the floor-cert test imports these).
terminate = _terminate
log_tail = _log_tail


# ── Stage A+B: a claimed, CalDAV-enabled, credentialed nest (no MDA yet) ──────


class ProvisionedNest:
    """A claimed Windows nest-svc with CalDAV enabled + a MUA credential minted —
    everything except the MDA. Splitting this out lets a focused test assert the
    headless provisioning (claim + enable + mint) on the Windows binaries BEFORE
    the MDA-serving seam, so a failure localizes."""

    def __init__(self, *, nest_proc, port, nest_url, data_dir, db_path, admin,
                 actor_id, auth_username, password, log_path, locator):
        self.nest_proc = nest_proc
        self.port = port
        self.nest_url = nest_url
        self.data_dir = data_dir
        self.db_path = db_path
        self.admin = admin               # claim_admin dict (signing_key, token, ...)
        self.actor_id = actor_id         # 32-byte admin actor id
        self.auth_username = auth_username
        self.password = password
        self.log_path = log_path
        self.locator = locator           # "127.0.0.1:<port>"

    def cleanup(self) -> None:
        _terminate(self.nest_proc)


def provision_windows_caldav_nest(
    nest_data_dir: Path, *, password: str = DEFAULT_MUA_PASSWORD,
) -> ProvisionedNest:
    """Stage A + B against a freshly-spawned **standalone** nest-svc on an
    ephemeral loopback port: spawn ``fauna-nest-svc.exe --foreground``, then
    claim / commit-storage / enable-CalDAV / mint via
    :func:`provision_caldav_on_running_nest`. The non-disruptive inner-loop path
    (temp data dir, ephemeral port). The install-gated e2e takes the *other*
    entry — it points :func:`provision_caldav_on_running_nest` at the already
    -running ``FaunaNest`` SCM service instead of spawning one."""
    nest_data_dir = Path(nest_data_dir)
    nest_data_dir.mkdir(parents=True, exist_ok=True)
    port = find_free_port()
    nest_proc = spawn_nest_svc(nest_data_dir, port)
    # provision_caldav_on_running_nest owns nest_proc teardown on any failure.
    return provision_caldav_on_running_nest(
        port=port, nest_data_dir=nest_data_dir, nest_proc=nest_proc, password=password,
    )


def provision_caldav_on_running_nest(
    *,
    port: int,
    nest_data_dir: Path,
    nest_proc: subprocess.Popen | None = None,
    password: str = DEFAULT_MUA_PASSWORD,
    claim_timeout: float = 45.0,
    tcp_timeout: float = 30.0,
) -> ProvisionedNest:
    """Claim + commit-storage + enable-CalDAV + mint-credential against an
    **already-running** nest on loopback ``port`` whose data dir is
    ``nest_data_dir``. This is Stage A (claim) + Stage B (the WS-RPC + seal-helper
    provisioning) with the nest *start* factored out, so two callers share it:

    - :func:`provision_windows_caldav_nest` — spawns its own ``fauna-nest-svc.exe``
      and passes the ``nest_proc`` (so ``ProvisionedNest.cleanup`` reaps it).
    - the install-gated e2e — the MSI's ``FaunaNest`` SCM service is the nest, so
      ``nest_proc=None`` (the SCM owns the process; ``cleanup`` is a no-op and the
      test uninstalls the MSI for teardown). The same ``ensure_claim_code_at``
      seed fires on the service ``start()`` path, so ``<nest_data_dir>/claim-code``
      is present either way.

    Domainless: claims the BARE handle ``test`` (``validate_handle`` allows only
    ``[a-z0-9-]``, rejecting an ``@domain`` / ``:port`` suffix), enables CalDAV,
    and mints a KNOWN-password PLAIN ``default`` credential via the seal-helper.
    NO account alias / per-domain cert — any-locator login resolves the
    bare handle and the MDA serves the self-signed floor cert.
    """
    from common.auth import claim_admin
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.caldav_onboarding import _auth_username

    nest_data_dir = Path(nest_data_dir)
    nest_url = f"http://127.0.0.1:{port}"
    locator = f"127.0.0.1:{port}"
    log_path = nest_data_dir / "nest-svc.log"

    try:
        # The nest writes its claim code to <data_dir>/claim-code via
        # ensure_claim_code_at (bin-main AND the Windows service start() path);
        # reconcile may regenerate, so we read rather than pre-seed.
        claim_code = _wait_for_file(
            nest_data_dir / "claim-code", time.monotonic() + claim_timeout,
            nest_proc, log_path,
        )
        if not _wait_tcp_accept(port, time.monotonic() + tcp_timeout):
            raise TimeoutError(
                f"nest never accepted TCP on {port}.\nlog:\n{_log_tail(log_path)}"
            )
        # A domainless nest: the handle is the BARE local part — `validate_handle`
        # (`bins/fauna-nest/src/registration.rs`) allows only `[a-z0-9-]`, so an
        # `@domain` / `:port` suffix is rejected (`fauna.auth.invalid_request`).
        # The client UI splits `test@domain` into handle=`test` + a separate
        # `mail_domain`; headless we just claim the bare handle and pass NO
        # mail_domain (127.0.0.1 is a local target — no registerable mail domain).
        # Any-locator login then resolves the bare local part `test` → this actor
        # (caldav-server.md § Authentication — any-locator local login).
        handle = HANDLE_LOCAL
        admin = claim_admin(port, claim_code, handle=handle)
        admin_sk = admin["signing_key"]
        actor_id = bytes(admin_sk.verify_key)

        # ── Stage B: enable CalDAV + mail + mint the MUA credential (all WS-RPC +
        # seal-helper). `mail_enabled` reads OFF on an unset toggle (mail-bridge-
        # lifecycle.md § Default-off on first claim) and Stage C's MDA gates its
        # whole IMAP listener pair on the snapshot's `MailEnabled` (`mda.go`'s
        # `mailOn := deps.Snapshot.MailEnabled`) — omitting this call left the IMAP
        # listeners never constructed at all, so `test_windows_caldav_imap_round_
        # trip`'s IMAP half failed every run with a flat `ConnectionRefusedError`
        # (nothing wrong with timing — nothing was ever listening). Mirrors the
        # cross-platform MDA fixture's own `set_mail_enabled` call (conftest.py).
        admin_ws = WsRpcAdminClient(nest_url, actor_id=actor_id, signing_key=bytes(admin_sk))
        with admin_ws:
            admin_ws.call("fauna.bridges.set_mail_enabled", {"enabled": True})
            admin_ws.call("fauna.bridges.set_caldav_enabled", {"enabled": True})
            cfg = admin_ws.call("fauna.bridges.get_mail_config", {})
            if not cfg.get("caldav_enabled"):
                raise AssertionError(
                    f"caldav_enabled did not stick after set_caldav_enabled: {cfg!r}"
                )
            if not cfg.get("mail_enabled"):
                raise AssertionError(
                    f"mail_enabled did not stick after set_mail_enabled: {cfg!r}"
                )
            _mint_credential(admin_ws, actor_id, password)

        # The MUA auth username carries the locator HOST as the address domain
        # (`test@127.0.0.1`, port-stripped per RFC-7617) — NOT the bare handle.
        # Two reasons: (1) the MDA builds the CalDAV path from the username as
        # `/caldav/{local}@{domain}/` (`userBasePath`, internal/mda/caldav/backend.go),
        # so the client's home must carry the same `@host` or it mismatches the
        # server's home-set and PROPFIND returns an empty multistatus (no lazy
        # Personal calendar); (2) any-locator login resolves `test@127.0.0.1`
        # (127.0.0.1 isn't a registered mail-domain → bare-handle fallback → the
        # `test` actor). Mirrors the proven domainless matrix (caldav_onboarding).
        auth_username = _auth_username(f"{HANDLE_LOCAL}@{locator}")  # test@127.0.0.1
        return ProvisionedNest(
            nest_proc=nest_proc, port=port, nest_url=nest_url, data_dir=nest_data_dir,
            db_path=str(nest_data_dir / "nest.db"), admin=admin, actor_id=actor_id,
            auth_username=auth_username, password=password, log_path=log_path,
            locator=locator,
        )
    except BaseException:
        # Only reap a process we own — the installed SCM nest (nest_proc=None) must
        # outlive a provisioning failure so the test can diagnose / uninstall it.
        if nest_proc is not None:
            _terminate(nest_proc)
        raise


def _mint_credential(admin_ws, actor_id: bytes, password: str) -> None:
    """Mint a known-password PLAIN credential for `actor_id`, headlessly — the
    domainless, single-actor adaptation of `conftest._provision_msek_recipient`.

    The seal-helper does the client-side seal the Fauna app UI normally does;
    we then upload the recipient pubkey (Admin-class) + the wrapped-MSEK AUTH
    blob + the MLS snapshot (User-class, keyed on the caller's own actor_id). All
    four calls authenticate AS the claimed admin actor (it IS the `test` actor),
    so one WsRpcAdminClient covers them. NO account alias — domainless login
    resolves the bare handle (any-locator design)."""
    import secrets

    from helpers.recipient_seal_key import provision_recipient_seal_key

    msek = secrets.token_bytes(32)
    msek_b64 = base64.b64encode(msek).decode()
    actor_b64 = base64.b64encode(actor_id).decode()

    wrapped_blob = run_seal_helper("seal-wrapped-msek", {
        "msek_b64": msek_b64,
        "actor_id_b64": actor_b64,
        "credential_id": CREDENTIAL_ID,
        "credential_kind": "plain",
        "credential_b64": base64.b64encode(password.encode()).decode(),
    })
    snapshot_blob = run_seal_helper("seal-mls-snapshot", {
        "msek_b64": msek_b64, "actor_id_b64": actor_b64,
    })

    provision_recipient_seal_key(
        admin_ws, actor_id, msek=msek, run_seal_helper=run_seal_helper,
    )
    admin_ws.call(
        "fauna.bridges.provision_wrapped_mls_blob",
        {"actor_id": actor_id, "credential_id": CREDENTIAL_ID, "blob": wrapped_blob},
    )
    admin_ws.call("fauna.bridges.provision_mls_snapshot_blob", {"blob": snapshot_blob})


# ── Stage C: spawn the MDA → a fully serving CalDAV+IMAP nest ─────────────────


class WindowsCaldavNest:
    """A fully serving Windows CalDAV + IMAP nest (nest-svc + MDA). The handle a
    round-trip test drives a Python `CalDAVClient` / `imaplib` against."""

    def __init__(self, *, prov: ProvisionedNest, mda_proc, caldav_port,
                 imaps_port, imap_starttls_port, metrics_port, mda_dir):
        self._prov = prov
        self.mda_proc = mda_proc
        self.caldav_port = caldav_port
        self.imaps_port = imaps_port
        self.imap_starttls_port = imap_starttls_port
        self.metrics_port = metrics_port
        self.mda_dir = mda_dir
        # Convenience pass-throughs.
        self.nest_url = prov.nest_url
        self.port = prov.port
        self.auth_username = prov.auth_username
        self.password = prov.password
        self.caldav_base_url = f"https://127.0.0.1:{caldav_port}"

    def cleanup(self) -> None:
        _terminate(self.mda_proc)
        self._prov.cleanup()


def serve_windows_caldav_nest(prov: ProvisionedNest, mda_dir: Path) -> WindowsCaldavNest:
    """Stage C: enroll + spawn the Windows MDA against a provisioned nest, wait
    until it serves CalDAV (off the floor cert) + IMAP. Domainless port of
    `conftest._spawn_mda_bridge` (no `provision_self_signed_cert`)."""
    import sqlite3
    import cbor2
    from nacl.signing import SigningKey as NaClSigningKey
    from cryptography.hazmat.primitives.asymmetric.x25519 import X25519PrivateKey
    from cryptography.hazmat.primitives.serialization import (
        Encoding, NoEncryption, PrivateFormat, PublicFormat,
    )
    from common.auth import register_user
    from helpers.bridge_enrollment import enroll_and_approve_bridge
    from helpers.caldav_roundtrip import wait_caldav_serving

    mda_dir = Path(mda_dir)
    mda_dir.mkdir(parents=True, exist_ok=True)
    admin_sk = prov.admin["signing_key"]
    bridge_id = "mda-" + os.urandom(4).hex()

    # ── 1. Service-user keypair (Ed25519 signing + X25519 wrap-target).
    ed_sk = NaClSigningKey.generate()
    ed_seed = bytes(ed_sk)
    ed_pubkey = bytes(ed_sk.verify_key)
    x_sk = X25519PrivateKey.generate()
    x_priv = x_sk.private_bytes(Encoding.Raw, PrivateFormat.Raw, NoEncryption())
    x_pubkey = x_sk.public_key().public_bytes(Encoding.Raw, PublicFormat.Raw)

    # ── 2. Canonical-CBOR keyfile named `mda.key` — role_hint derives from the
    # basename; nest rejects any role_hint that isn't mta/mda.
    keyfile = mda_dir / "mda.key"
    keyfile.write_bytes(cbor2.dumps({
        "v": 1,
        "role": "mda",
        "bridge_id": bridge_id,
        "ed25519_seed": ed_seed,
        "x25519_priv": x_priv,
        "created_at": int(time.time()),
    }, canonical=True))

    # ── 3. register_user + pre-approve (the bridge's nest WS actor + the
    # approved bridge_service_users row the x25519 poke targets).
    register_user(prov.port, ed_pubkey.hex(), admin_signing_key=admin_sk)
    enroll_and_approve_bridge(prov.nest_url, admin_sk, ed_pubkey, "mda", bridge_id)

    # ── 4. x25519 hand-poke (the floor-cert seal target). Domainless ⇒ NO
    # provision_self_signed_cert: the MDA fetches under the floor sentinel and
    # nest seals its self-signed FLOOR cert to this x25519.
    conn = sqlite3.connect(prov.db_path, timeout=10.0)
    try:
        conn.execute(
            "UPDATE bridge_service_users SET x25519_pubkey = ?"
            " WHERE ed25519_pubkey = ? AND status != 'revoked'",
            (x_pubkey, ed_pubkey),
        )
        conn.commit()
    finally:
        conn.close()

    # ── 5. Operator-hatch with the four ephemeral binds.
    caldav_port, imaps_port, imap_starttls_port, metrics_port = find_free_ports(4)
    op_hatch = mda_dir / "operator-hatch.toml"
    op_hatch.write_text(
        f'data_dir = "{mda_dir.as_posix()}"\n'
        f'imap_listen_implicit_tls = "127.0.0.1:{imaps_port}"\n'
        f'imap_listen_starttls = "127.0.0.1:{imap_starttls_port}"\n'
        f'caldav_listen_https = "127.0.0.1:{caldav_port}"\n'
        f'metrics_bind_addr = "127.0.0.1:{metrics_port}"\n'
    )

    # ── 6. Spawn the MDA from target/ so the FFI dll resolves (image-dir search).
    target = _root_target_dir()
    env = os.environ.copy()
    env["PATH"] = f"{target}{os.pathsep}{env.get('PATH', '')}"
    mda_proc = _spawn_logged(
        [str(mda_bridge_exe()),
         f"--keypair-file={keyfile}",
         f"--nest-endpoint={prov.nest_url}",
         f"--operator-hatch={op_hatch}",
         "--log-level=debug"],
        mda_dir / "mail-bridge-mda.log",
        cwd=str(target), env=env,
    )

    nest = WindowsCaldavNest(
        prov=prov, mda_proc=mda_proc, caldav_port=caldav_port, imaps_port=imaps_port,
        imap_starttls_port=imap_starttls_port, metrics_port=metrics_port, mda_dir=mda_dir,
    )
    try:
        _wait_metrics_healthz(metrics_port, timeout=45.0)
        wait_caldav_serving(nest.caldav_base_url, verify=False, timeout=60.0)
    except BaseException:
        # Surface the MDA log on any startup failure (errors via logs, not screenshots).
        log = _log_tail(mda_dir / "mail-bridge-mda.log")
        _terminate(mda_proc)
        raise RuntimeError(f"MDA did not come up serving CalDAV.\nMDA log:\n{log}")
    return nest


def start_windows_caldav_nest(
    tmp_path: Path, *, password: str = DEFAULT_MUA_PASSWORD,
) -> WindowsCaldavNest:
    """Convenience: provision (Stage A+B) then serve (Stage C) under `tmp_path`."""
    tmp_path = Path(tmp_path)
    prov = provision_windows_caldav_nest(tmp_path / "nest", password=password)
    try:
        return serve_windows_caldav_nest(prov, tmp_path / "mda")
    except BaseException:
        prov.cleanup()
        raise
