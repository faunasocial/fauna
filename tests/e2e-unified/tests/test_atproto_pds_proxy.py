"""tier_3: F3 service proxying end-to-end — ``getTimeline`` through the nest.

The F3 proxy leg's definition of success (``docs/goal/behavior/
atproto-pds-full.md`` § F3 detail, *Service proxying*): a session-authed raw
XRPC client calls ``app.bsky.feed.getTimeline`` with NO ``atproto-proxy``
header, and the bridge forwards it to the AppView default with a
freshly minted service JWT — signed with the ACCOUNT's own repo signing key,
unsealed from its nest-held blob — and streams the AppView's reply back.

What only tier_3 catches here: the real three-process wire — raw XRPC client
→ Go bridge (fallback route → D8 over the FFI → per-request signer unseal
over WS-RPC → mint → forward) → the nest's blob/registry kinds. The unit and
handler tests stub the signer source and the fetch; this run unseals a real
sealed identity blob and puts a verifiable ES256K JWT on a real socket.

The fake AppView stands in via the test-only ``FAUNA_ATPROTO_PROXY_FIXTURES``
seam (the ``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` precedent): mapped refs skip
resolution + the SSRF guard (a loopback endpoint is what the guard exists to
refuse), every other ref still runs the full guarded path — which the
unmapped-ref case below proves stays genuine.
"""

import base64
import json
import os
import subprocess
import time

import pytest

from helpers.atproto_fakes import FakeAppView, FakePlcDirectory
from helpers.xrpc_client import (
    load_app_credential_fixture as _load_fixture,
    xrpc_get as _xrpc_get,
    xrpc_post as _xrpc_post,
)

pytestmark = pytest.mark.tier_3

USER_ROTATION_PUB = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"  # gitleaks:allow
HANDLE_DOMAIN = "fauna.test"
APPVIEW_REF = "did:web:api.bsky.app#bsky_appview"


def _jwt_claims(token: str) -> dict:
    parts = token.split(".")
    assert len(parts) == 3, f"want a compact JWT, got {token!r}"
    pad = "=" * (-len(parts[1]) % 4)
    return json.loads(base64.urlsafe_b64decode(parts[1] + pad))


@pytest.mark.feature("atproto")
def test_atproto_pds_service_proxy_end_to_end(nest_binary, atproto_bridge_e2e_binary, tmp_path_factory):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import _make_nest, _repo_root
    from helpers.bridge_enrollment import approve_bridge
    from drivers.port_util import (
        find_free_port,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )

    secret, verifier = _load_fixture()

    # The bridge binary is built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time — outside the per-test timeout budget; see
    # `_ensure_atproto_bridge_e2e_built`'s docstring in conftest.py for why an
    # in-body `just atproto-bridge-build` call used to die as a bare
    # `Timeout (>900.0s)` under fleet contention. The fixture also picks the
    # right binary per platform, so the windows/unix branch is gone from here.
    # The e2e FLAVOR, not the production binary: the FAUNA_ATPROTO_* seams this
    # test sets are compiled only into `-tags fauna_e2e_fixtures` (convention 15,
    # e2e-automation-surface-gating.md → the Go bridges' leg); the shipped
    # flavor ignores them, so against it this test could not even reach its
    # fakes. The shipped flavor itself is exercised by test_atproto_bridge_enroll
    # (tier_3) and the nest image tests (tier_4).
    atproto_bin = atproto_bridge_e2e_binary

    appview = FakeAppView(
        timeline={"feed": [{"post": {"uri": "at://did:plc:fake/app.bsky.feed.post/1"}}],
                  "cursor": ""},
    )
    directory = FakePlcDirectory()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "atproto-proxy-nest",
        claim_domain=HANDLE_DOMAIN,    )
    from common.auth import open_registration
    open_registration(nest)
    proc = None
    log_fh = None
    try:
        admin = nest["admin"]
        tmp = tmp_path_factory.mktemp("atproto-proxy-bridge")
        keyfile_path = tmp / "atproto.pds.key"
        xrpc_port = find_free_port()
        xrpc_base = f"https://127.0.0.1:{xrpc_port}"

        minted = subprocess.run(
            [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        pubkey_hex = minted.stdout.strip()
        pub_bytes = bytes.fromhex(pubkey_hex)

        env = os.environ.copy()
        env["FAUNA_ATPROTO_PLC_DIRECTORY_URL"] = directory.url
        env["FAUNA_ATPROTO_PROXY_FIXTURES"] = json.dumps({APPVIEW_REF: appview.url})

        log_path = tmp / "bridge.log"
        log_fh = open(log_path, "wb")
        proc = subprocess.Popen(
            [
                atproto_bin,
                f"--keypair-file={keyfile_path}",
                f"--nest-endpoint={nest['url']}",
                f"--data-dir={tmp.as_posix()}",
                f"--xrpc-listen=127.0.0.1:{xrpc_port}",
                "--log-level=debug",
            ],
            stdout=log_fh, stderr=subprocess.STDOUT, env=env,
            **popen_group_kwargs(),
        )
        # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()`
        # is `{}` there, so without this the bridge's only protection is the
        # atexit sweep a killed run never reaches (testing.md § point 9). No-op
        # off Windows.
        reap_descendants_of(proc.pid)
        track_process(proc)

        def _tail() -> str:
            return log_path.read_text(errors="replace")[-4000:]

        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "pending"}
                )["service_users"]
                if any(bytes(r["ed25519_pubkey"]) == pub_bytes for r in rows):
                    break
                assert proc.poll() is None, "bridge exited before enrolling:\n" + _tail()
                time.sleep(0.5)
            else:
                pytest.fail("atproto.pds bridge never appeared PENDING:\n" + _tail())

        approve_bridge(
            nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
        )

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if "xrpc listener up" in log_path.read_text(errors="replace"):
                break
            assert proc.poll() is None, "bridge exited before serving XRPC:\n" + _tail()
            time.sleep(0.5)
        else:
            pytest.fail("XRPC listener never came up:\n" + _tail())

        # ── alice: handled actor, atproto identity (gives the signer unseal a
        # blob to provision-on-read), app credential, XRPC session. ──
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"],
        )
        alice_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=alice["actor_id_bytes"],
            signing_key=bytes(alice["signing_key"]),
        )
        with alice_ws:
            # The one-shot `enable_identity` kind was retired in S4-B — the
            # depth selector's `set_integration_level` is the only level/enable
            # mutation path (`ui/atproto.md` § Transition semantics).
            alice_ws.call(
                "fauna.bridges.atproto.set_integration_level",
                {
                    "target_level": "hosted_visible",
                    "did_method": "plc",
                    "user_rotation_pub_did_key": USER_ROTATION_PUB,
                    "history_backfill": False,
                },
            )
            reply = alice_ws.call(
                "fauna.bridges.atproto.provision_app_credential",
                {"credential_id": "ivory", "label": "Ivory", "verifier": verifier,
                 "dm_allowed": False},
            )
            assert reply.get("ok") is True, reply

            # Since slice 4d, createSession requires an ACTIVE hosted identity
            # (`login_did` nil joins the uniform-401 set), so wait out the
            # bridge's mint round-trip against the fake directory first. Named
            # generous budget + deadline poll (convention 14): the mint loop's
            # own poll tick is 30 s (`mint.go::mintPollInterval`), so the
            # ceiling covers two ticks under load; a green run pays only the
            # actual wait.
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                st = alice_ws.call(
                    "fauna.bridges.atproto.get_integration_status", {}
                )
                if (st.get("identity") or {}).get("status") == "active":
                    break
                assert proc.poll() is None, "bridge exited mid-mint:\n" + _tail()
                time.sleep(0.5)
            else:
                pytest.fail("hosted identity never became ACTIVE:\n" + _tail())

        status, sess = _xrpc_post(
            xrpc_base, "com.atproto.server.createSession",
            {"identifier": "alice", "password": secret},
        )
        assert status == 200, f"createSession failed: {status} {sess}\n{_tail()}"
        access = sess["accessJwt"]

        # ── THE success criterion: a headerless getTimeline comes back with
        # the AppView's timeline. ──
        status, feed = _xrpc_get(
            xrpc_base, "app.bsky.feed.getTimeline?limit=5", access,
        )
        assert status == 200, f"proxied getTimeline failed: {status} {feed}\n{_tail()}"
        assert feed["feed"][0]["post"]["uri"].startswith("at://"), feed

        # What crossed to the AppView: the forwarded query, and a service JWT
        # minted for THIS method and audience — never the PDS access token.
        reqs = appview.snapshot()
        assert len(reqs) == 1, reqs
        path, query, headers, _ = reqs[0]
        assert path == "/xrpc/app.bsky.feed.getTimeline"
        assert query == "limit=5"
        authz = headers.get("Authorization", "")
        assert authz.startswith("Bearer ") and access not in authz, (
            "the caller's PDS access token must never reach the upstream", authz,
        )
        claims = _jwt_claims(authz.removeprefix("Bearer "))
        assert claims["aud"] == APPVIEW_REF, claims
        assert claims["lxm"] == "app.bsky.feed.getTimeline", claims
        assert claims["iss"] == sess["did"], claims
        assert claims["exp"] <= time.time() + 61, ("exp within the 60 s cap", claims)
        assert claims["jti"], claims

        # ── The guard is still live for every UNMAPPED ref: a did:web naming
        # an IP literal is refused by the shared-Rust policy at resolution
        # (DENY_NOT_PUBLIC_NAME), before anything is dialled. ──
        status, err = _xrpc_get(
            xrpc_base,
            "app.bsky.feed.getTimeline",
            access,
            extra_headers={"atproto-proxy": "did:web:169.254.169.254#imds"},
        )
        assert status == 400, ("an unmapped IP-literal target must be refused", status, err)
        assert len(appview.snapshot()) == 1, "the refused target must not be forwarded"

        # ── The proxy plane is authenticated: no token, no forward. ──
        status, err = _xrpc_get(xrpc_base, "app.bsky.feed.getTimeline")
        assert status == 401 and err.get("error") == "AuthenticationRequired", (status, err)

        # ── The closed world survives the fallback: an unknown non-app.bsky
        # method without a header is still MethodNotImplemented. ──
        status, err = _xrpc_get(xrpc_base, "com.example.custom.method", access)
        assert status == 404, (status, err)
        assert len(appview.snapshot()) == 1, appview.snapshot()
    finally:
        if proc is not None:
            try:
                if proc.poll() is None:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        proc.wait()
            finally:
                untrack_process(proc)
        if log_fh is not None:
            log_fh.close()
        appview.close()
        directory.close()
        nest_cleanup()
