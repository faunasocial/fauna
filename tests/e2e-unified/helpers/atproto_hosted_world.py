"""The hosted-ATProto world a journey mints into: a dedicated nest claimed on a
public handle domain, a handled user ``alice``, the real Go PDS bridge (its
e2e flavor) approved against a fake PLC directory, and a real app pointed at
that directory and logged in as alice — everything short of the mint itself.

Lifted out of ``test_atproto_custody_alarm.py`` (whose custody and contest
journeys mint into it) so a second family of hosted journeys — the ones that
read what the bridge PUBLISHES — reuses one bring-up rather than a copy.
``projection=True`` adds what those need: the bridge's XRPC/firehose listener
(``--xrpc-listen``) and a fake DNS for the first-emit gate's ``_atproto`` TXT
lookup, the same pair ``test_atproto_firehose_post.py`` runs with. Both fakes
are artifact/test IPC (``FAUNA_ATPROTO_PLC_DIRECTORY_URL``,
``FAUNA_ATPROTO_FAKE_DNS_URL``), never operator config, and both are compiled
only into the bridge's e2e flavor (convention 15).
"""
from __future__ import annotations

import contextlib
import json
import os
import ssl
import subprocess
import time
from types import SimpleNamespace

from actions import ActionLayer
from drivers import create_driver
from helpers.app_launch import app_launch_config
from helpers.atproto_fakes import FakeDNS, FakePlcDirectory
from helpers.bridge_enrollment import approve_bridge
from helpers.waiting import await_account_runtime_assembled

HANDLE_DOMAIN = "fauna.test"


@contextlib.contextmanager
def hosted_world(
    app_name, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
    *, projection: bool = False,
):
    """Bring the world up and yield it as a namespace: ``app`` (an
    ``ActionLayer`` logged in as alice, its account runtime assembled),
    ``nest``, ``alice``, ``directory`` (the ``FakePlcDirectory``), ``dns`` (a
    ``FakeDNS`` with ``projection``, else None), ``xrpc_get(method, query)``
    (with ``projection``, else None), ``tail()`` (the bridge log, plus the
    windows shell log), and the bridge's ``atproto_bin`` / ``keyfile_path`` /
    ``env`` for a caller that runs the binary again (the custody module's
    seizure). Tears everything down on exit."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import open_registration, register_handled_actor
    from conftest import _make_nest, _repo_root, _seeded_environment
    from drivers.port_util import (
        find_free_port,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )

    # The e2e FLAVOR of the bridge: the same binary plus the test seams
    # (the fakes' URLs, and the custody module's one-shot hostile-rotation
    # seam), at its own path so it can never stand in for the production
    # artifact. Built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time — outside the per-test timeout budget.
    atproto_bin = atproto_bridge_e2e_binary

    directory = FakePlcDirectory()
    dns = FakeDNS() if projection else None
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "atproto-hosted-world-nest",
        # The domained claim, which is also this nest's ONLY registration of
        # HANDLE_DOMAIN — two doors onto one domain means the loser's arguments
        # are silently discarded (`add_local_domain` is idempotent by domain
        # NAME). The claim's cert mode is inert here: nothing in the DNS matrix
        # or the `_atproto` TXT row reads `mta_sts_cert_mode`, and a plain-HTTP
        # test nest arms no ACME (`testing.md` § Default app and nest mode,
        # ruling (3)).
        claim_domain=HANDLE_DOMAIN,
    )
    open_registration(nest)
    proc = None
    log_fh = None
    driver = None
    spa_proxy_server = None
    try:
        admin = nest["admin"]
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )

        # ── Bridge cold boot against the fakes + admin approval (the
        # test_atproto_identity_mint boot sequence). ──
        tmp = tmp_path_factory.mktemp("atproto-hosted-world-bridge")
        keyfile_path = tmp / "atproto.pds.key"
        minted = subprocess.run(
            [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        pubkey_hex = minted.stdout.strip()
        env = os.environ.copy()
        env["FAUNA_ATPROTO_PLC_DIRECTORY_URL"] = directory.url
        args = [
            atproto_bin,
            f"--keypair-file={keyfile_path}",
            f"--nest-endpoint={nest['url']}",
            f"--data-dir={tmp.as_posix()}",
            "--log-level=debug",
        ]
        xrpc_get = None
        if projection:
            env["FAUNA_ATPROTO_FAKE_DNS_URL"] = dns.url
            xrpc_port = find_free_port()
            args.insert(-1, f"--xrpc-listen=127.0.0.1:{xrpc_port}")
            xrpc_base = f"https://127.0.0.1:{xrpc_port}"

            def xrpc_get(method: str, query: str = "") -> dict:
                import urllib.request
                url = f"{xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
                ctx = ssl._create_unverified_context()
                with urllib.request.urlopen(url, context=ctx, timeout=15) as resp:
                    return json.loads(resp.read())

        log_path = tmp / "bridge.log"
        log_fh = open(log_path, "wb")
        proc = subprocess.Popen(
            args, stdout=log_fh, stderr=subprocess.STDOUT, env=env,
            **popen_group_kwargs(),
        )
        # Windows half of the die-with-the-run guarantee — no-op off Windows.
        reap_descendants_of(proc.pid)
        track_process(proc)

        # Written by the app itself only on windows (see the `environment`
        # block below); absent elsewhere, which `tail` handles by omission.
        app_trace_path = tmp / "app-shell.log"

        def tail() -> str:
            out = log_path.read_text(errors="replace")[-4000:]
            if app_trace_path.exists():
                out += (
                    "\n── app shell log (windows, FAUNA_E2E_AGENT_LOG) ──\n"
                    + app_trace_path.read_text(errors="replace")[-4000:]
                )
            return out

        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # No `add_local_domain`: the claim carried HANDLE_DOMAIN as its
            # `mail_domain`, so the primary row already exists.
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "pending"}
                )["service_users"]
                if any(
                    bytes(r["ed25519_pubkey"]).hex() == pubkey_hex for r in rows
                ):
                    break
                assert proc.poll() is None, "bridge exited pre-enroll; log:\n" + tail()
                time.sleep(0.5)
            approve_bridge(
                nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
            )

        # ── A real app, pointed at the fake directory (the app's OWN read
        # path — the custody check never asks the nest). ──
        if app_name == "web":
            # web has no process env (no `app_path`, no `environment` — see
            # drivers/web.py::launch): the SPA proxy targets THIS world's own
            # nest (mirrors `atproto_hosted_spa_url`'s per-nest-proxy shape), and
            # the directory override rides the JS hook below instead.
            from conftest import _serve_spa_proxy
            static_dir = request.getfixturevalue("static_dir")
            spa_url, spa_proxy_server = _serve_spa_proxy(static_dir, nest["url"])
            driver = create_driver("web")
            driver.launch({"url": spa_url.rstrip("/") + "/app/"})
        else:
            environment = {
                **_seeded_environment(request, nest),
                "FAUNA_ATPROTO_PLC_DIRECTORY_URL": directory.url,
                "FAUNA_DNS_PROVIDER_FAKE": "1",
            }
            if app_name == "linux":
                environment["GTK_A11Y"] = "none"
            if app_name == "windows":
                # The windows shell's own log, teed into one thread-tagged file
                # (FaunaApp.Core/Logs/E2eTrace.cs); `tail` folds it into every
                # failure message — on windows the app's OWN log is the
                # diagnosis, and the custody chain crosses the C# shell twice
                # with no Rust tracing on either hop.
                environment["FAUNA_E2E_AGENT_LOG"] = str(app_trace_path)
            driver = create_driver(app_name)
            driver.launch({
                "url": nest["url"],
                "environment": environment,
                **app_launch_config(app_name, request),
            })
        try:
            driver.wait_for_state(lambda s: s is not None, timeout=30)
        except (TimeoutError, Exception):
            pass
        # No-op on native (the env var above already covers it); web has no
        # env path, so its wasm-side check needs the explicit JS hook.
        driver.enable_fake_plc_directory(directory.url)
        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest["url"],
                "secret_hex": alice["signing_key"].encode().hex(),
                "handle": f"alice@{HANDLE_DOMAIN}",
                "actor_id": alice["actor_id_bytes"].hex(),
                "device_id": "custody-e2e",
            },
            "nav": {"stack": [{"view": "feed"}]},
        })
        # A mint writes the senior rotation key into the account plane's
        # `fauna.state.atproto-identity` rows, whose door refuses while no
        # account runtime runs — and the runtime assembles OFF the login path
        # (on web, only once the conversations manager is built). So the world
        # is handed over only once it has (`test_contact_overlay.py`).
        await_account_runtime_assembled(driver)

        def await_genesis(budget_s: float = 60.0) -> tuple[str, str]:
            """After a mint through the app: wait for the bridge's mint loop to
            pick up the pending identity and submit its genesis op to the fake
            directory; return ``(did, user_key)`` — the DID and the
            client-minted senior rotation key the op leads with."""
            deadline = time.monotonic() + budget_s
            while time.monotonic() < deadline and not directory.snapshot():
                assert proc.poll() is None, "bridge died before minting; log:\n" + tail()
                time.sleep(0.5)
            submissions = directory.snapshot()
            assert submissions, "the bridge never submitted a genesis op; log:\n" + tail()
            did, op = submissions[0]
            assert op["rotationKeys"], "genesis op carries rotation keys"
            user_key = op["rotationKeys"][0]
            assert user_key.startswith("did:key:zDn"), (
                "the client-minted P-256 senior key leads the genesis op "
                f"(custody split); got {op['rotationKeys']!r}"
            )
            return did, user_key

        yield SimpleNamespace(
            await_genesis=await_genesis,
            app=ActionLayer(driver),
            nest=nest,
            alice=alice,
            directory=directory,
            dns=dns,
            xrpc_get=xrpc_get,
            tail=tail,
            proc=proc,
            atproto_bin=atproto_bin,
            keyfile_path=keyfile_path,
            env=env,
        )
    finally:
        if driver is not None:
            try:
                driver.teardown()
            except Exception:
                pass
        if spa_proxy_server is not None:
            spa_proxy_server.shutdown()
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
            untrack_process(proc)
        if log_fh is not None:
            log_fh.close()
        nest_cleanup()
        directory.close()
        if dns is not None:
            dns.close()
