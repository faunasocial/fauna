"""The nest + real PDS bridge pair every full OAuth ceremony needs, shared.

**Why a shared fixture module.** An approval by an account with no ACTIVE
ATProto identity is refused post-consent, so any test that must complete a
ceremony end to end — reach a code, redeem it, hold a live grant — needs the
real ``fauna-atproto-bridge`` that mints that identity. Lifted out of
``tests/test_atproto_pds_consent.py`` so the principal-roster tests
(``tests/api/test_third_party_principals.py``) run on the same pair rather than
on a second copy of a 250-line fixture. A module adopting these imports them
by name (they are pytest fixtures); each importing module gets its own pair.
"""

import base64
import json
import os
import subprocess
import time

import pytest

from helpers.atproto_fakes import FakePlcDirectory, dag_cbor

#: The permission set the bridge serves through the e2e-flavor
#: `FAUNA_ATPROTO_PERMISSION_SET_FIXTURES` seam — the same document the Go
#: cross-binary PAR test publishes for real. One member inside the set's own
#: namespace (it expands, inheriting the include's audience) and one outside it
#: (the hierarchy constraint ignores it), so the expansion below cannot be
#: produced by accident.
PERMISSION_SET_NSID = "com.example.calendar.appPerms"
PERMISSION_SET_TITLE = "Calendar access"
PERMISSION_SET_AUDIENCE = "did:web:svc.example"
PERMISSION_SET_MEMBER = "com.example.calendar.sync.push"
PERMISSION_SET_DOCUMENT = dag_cbor({
    "lexicon": 1,
    "id": PERMISSION_SET_NSID,
    "defs": {
        "main": {
            "type": "permission-set",
            "title": PERMISSION_SET_TITLE,
            "permissions": [
                {"resource": "rpc", "lxm": PERMISSION_SET_MEMBER, "inheritAud": True},
                {"resource": "rpc", "lxm": "com.atproto.server.deleteSession", "inheritAud": True},
            ],
        }
    },
})
#: A set nothing publishes: `.invalid` (RFC 6761) never resolves, so the
#: bridge's real chain refuses it without leaving the box — the refusal arm
#: with the bridge PRESENT, as opposed to `tests/api/test_oauth_issuer.py`'s
#: bridge-absent one.
UNRESOLVABLE_SET_NSID = "invalid.harness.perms"

HANDLE_DOMAIN = "fauna.test"
# The ATProto cryptography spec's p256 example vector — a syntactically valid
# senior rotation pubkey. A real client generates its own via
# `fauna-client-atproto::rotation_key`; this test never needs its secret half.
USER_ROTATION_PUB = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"  # gitleaks:allow


@pytest.fixture(scope="module")
def consent_nest_env():
    """The extra env the consent nest starts with (a ``test-hooks`` seam) —
    none by default. A module whose nest needs one overrides THIS fixture
    rather than redefining ``consent_nest``: the nest-mode axis keys its
    start-option table by bare fixture name, so a second ``consent_nest``
    asking for different options is a collision."""
    return None


@pytest.fixture(scope="module")
def consent_nest(nest_binary, tmp_path_factory, consent_nest_env):
    """A claimed nest with a **public** handle domain and a HANDLED user already
    at `hosted_full`.

    The level ladder is arranged over the wire rather than driven through the
    UI, because it is this test's *precondition*, not its subject — the ladder
    has its own UI test (`test_atproto_settings.py`), and e2e rule 8 exempts
    fixture setup for exactly this. What the consent ceremony genuinely needs
    from it is the **mint intent**: an OAuth sign-in completes as an identity,
    so an account whose ATProto identity never went ACTIVE is refused
    (honestly, post-consent) — which is the product behaviour that surfaced the
    first time this fixture skipped the mint.
    """
    nest, cleanup = start_consent_nest(
        nest_binary, tmp_path_factory, extra_env=consent_nest_env
    )
    yield nest
    cleanup()


def start_consent_nest(nest_binary, tmp_path_factory, extra_env=None):
    """``consent_nest``'s body, for a module that needs the same nest with
    ``extra_env`` (a ``test-hooks`` seam) — ``consent_nest`` is this with the
    ``consent_nest_env`` fixture's value, which a module overrides.
    Returns ``(nest, cleanup)``."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import _make_nest

    nest, cleanup = _make_nest(
        nest_binary, tmp_path_factory, "atproto-consent-nest",
        claim_domain=HANDLE_DOMAIN, extra_env=extra_env,
    )
    from common.auth import open_registration
    open_registration(nest)
    alice = register_handled_actor(
        nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"],
    )
    alice_ws = WsRpcAdminClient(
        nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    )
    with alice_ws:
        alice_ws.call(
            "fauna.bridges.atproto.set_integration_level",
            {
                "target_level": "hosted_full",
                "did_method": "plc",
                "user_rotation_pub_did_key": USER_ROTATION_PUB,
                "history_backfill": False,
            },
        )
    nest["user"] = alice
    return nest, cleanup


@pytest.fixture
def consent_spa_url(static_dir, consent_nest):
    """Web-only SPA proxy → ``consent_nest``. The session ``spa_url`` only
    proxies the shared ``nest_instance``, where this fixture's alice is
    unregistered — a web app logged in through it never comes online. Pass it
    as ``_login_app_as(..., spa_url_fixture="consent_spa_url")``; mirrors
    ``atproto_hosted_spa_url``."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, consent_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture(scope="module")
def consent_bridge(consent_nest, atproto_bridge_e2e_binary, tmp_path_factory):
    """The real ``fauna-atproto-bridge`` against ``consent_nest``, approved and
    serving its PDS listener.

    Module-scoped: standing the bridge up is an admin approval plus an authed
    handshake, and every test here drives the same one.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from conftest import _repo_root
    from drivers.port_util import (
        find_free_port,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )
    from helpers.bridge_enrollment import approve_bridge

    # The fake PLC directory stands in for `https://plc.directory` through the
    # test-only `FAUNA_ATPROTO_PLC_DIRECTORY_URL` seam, so the bridge really
    # mints alice's `did:plc:` and her identity reaches ACTIVE. Without it the
    # identity stays `pending`, the consent read-back withholds `login_did` (only an
    # ACTIVE identity gets one — the same rule that stops a layer-2 step-down
    # being undone by approving a fresh grant), and an approval redirects
    # `access_denied` instead of releasing a code. That refusal is CORRECT
    # product behaviour; it just is not the path this test is about.
    directory = FakePlcDirectory()

    # The bridge binary is built by the `atproto_bridge_e2e_binary` fixture at
    # COLLECTION time — outside the per-test timeout budget; see
    # `_ensure_atproto_bridge_e2e_built`'s docstring in conftest.py for why an
    # in-body `just atproto-bridge-build` call used to die as a bare
    # `Timeout (>900.0s)` under fleet contention. The fixture also picks the
    # right binary per platform (windows: the gnullvm slice into target/,
    # since Go's cgo cannot link the MSVC-built fauna_ffi.dll), so that branch
    # is gone from here.
    # The e2e FLAVOR, not the production binary: the FAUNA_ATPROTO_* seams this
    # test sets are compiled only into `-tags fauna_e2e_fixtures` (convention 15,
    # e2e-automation-surface-gating.md → the Go bridges' leg); the shipped
    # flavor ignores them, so against it this test could not even reach its
    # fakes. The shipped flavor itself is exercised by test_atproto_bridge_enroll
    # (tier_3) and the nest image tests (tier_4).
    atproto_bin = atproto_bridge_e2e_binary

    tmp = tmp_path_factory.mktemp("atproto-consent-bridge")
    keyfile_path = tmp / "atproto.pds.key"
    xrpc_port = find_free_port()
    xrpc_listen = f"127.0.0.1:{xrpc_port}"

    minted = subprocess.run(
        [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
        cwd=_repo_root, capture_output=True, text=True, check=True,
    )
    pubkey_hex = minted.stdout.strip()
    pub_bytes = bytes.fromhex(pubkey_hex)

    env = os.environ.copy()
    env["FAUNA_ATPROTO_PLC_DIRECTORY_URL"] = directory.url
    # The permission-set document the nest→bridge request call resolves in
    # `test_a_permission_set_resolves_through_the_bridge…` — served as if the
    # chain had verified it (e2e flavor only). Every other NSID still runs the
    # real chain, which is what makes the unresolvable arm's refusal genuine.
    env["FAUNA_ATPROTO_PERMISSION_SET_FIXTURES"] = json.dumps({
        PERMISSION_SET_NSID: base64.b64encode(PERMISSION_SET_DOCUMENT).decode(),
    })

    log_path = tmp / "bridge.log"
    log_fh = open(log_path, "wb")
    proc = subprocess.Popen(
        [
            atproto_bin,
            f"--keypair-file={keyfile_path}",
            f"--nest-endpoint={consent_nest['url']}",
            f"--data-dir={tmp.as_posix()}",
            f"--xrpc-listen={xrpc_listen}",
            "--log-level=debug",
        ],
        stdout=log_fh, stderr=subprocess.STDOUT, env=env,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — `popen_group_kwargs()` is
    # `{}` there, so without this the bridge's only protection is the atexit
    # sweep a killed run never reaches (testing.md § point 9). No-op off Windows.
    reap_descendants_of(proc.pid)
    track_process(proc)

    def tail() -> str:
        return log_path.read_text(errors="replace")[-4000:]

    try:
        admin = consent_nest["admin"]
        admin_ws = WsRpcAdminClient(
            consent_nest["url"],
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
                assert proc.poll() is None, "bridge exited before enrolling:\n" + tail()
                time.sleep(0.5)
            else:
                pytest.fail("atproto.pds bridge never appeared PENDING:\n" + tail())

        approve_bridge(
            consent_nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
        )

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if "xrpc listener up" in log_path.read_text(errors="replace"):
                break
            assert proc.poll() is None, "bridge exited before serving XRPC:\n" + tail()
            time.sleep(0.5)
        else:
            pytest.fail("XRPC listener never came up:\n" + tail())

        # The PDS host the bridge published itself at — what a DPoP proof's
        # `htu` at the resource server is built from, and NOT necessarily the
        # address this test dials. `pds.<primary-domain>` when the nest has one,
        # else the listen address itself. Read it back rather than assuming:
        # guessing wrong fails every PAR with an `htu` mismatch, which reads
        # like a product bug and is not one.
        pds_host = _read_pds_host(log_path, fallback=xrpc_listen)

        # The mint has to LAND before any consent can complete — an approval by
        # an account with no ACTIVE identity is refused, so a test that raced
        # the mint would fail on a real product rule rather than on its subject.
        alice = consent_nest["user"]
        alice_ws = WsRpcAdminClient(
            consent_nest["url"],
            actor_id=alice["actor_id_bytes"],
            signing_key=bytes(alice["signing_key"]),
        )
        with alice_ws:
            deadline = time.monotonic() + 90
            status = None
            while time.monotonic() < deadline:
                reply = alice_ws.call("fauna.bridges.atproto.get_integration_status", {})
                identity = reply.get("identity")
                status = identity and identity.get("status")
                if status == "active":
                    alice_did = identity.get("did")
                    break
                assert proc.poll() is None, "bridge exited before minting:\n" + tail()
                time.sleep(0.5)
            else:
                pytest.fail(
                    f"alice's ATProto identity never reached ACTIVE (status={status!r}); "
                    "without one no OAuth sign-in can complete:\n" + tail()
                )

        yield {
            "base": f"https://{xrpc_listen}",
            "htu_origin": f"https://{pds_host}",
            # The real did:plc the mint landed — what an authenticated call must
            # resolve to, so the token leg asserts the APPROVING account rather
            # than merely some account.
            "did": alice_did,
            "proc": proc,
            "tail": tail,
        }
    finally:
        untrack_process(proc)
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        log_fh.close()
        directory.close()


def _read_pds_host(log_path, fallback: str) -> str:
    """Recover the bridge's own PDS host from its structured log.

    The bridge logs one JSON record per line (`internal/logging`,
    `slog.NewJSONHandler` — never plain `key=value` text), and the "xrpc
    listener up" record (`main.go`) carries the derived host as its
    `pds_host` field — this reads that value back rather than re-deriving it
    here, a second derivation being exactly the kind of drift that makes a
    test assert its own arithmetic. `did:web:<pds-host>` is never emitted as
    literal log text (the service DID is only ever *served*, in the
    `.well-known/did.json` HTTP response), so a substring search for it can
    never match and always falls back silently — the bug this replaces."""
    for line in log_path.read_text(errors="replace").splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("msg") == "xrpc listener up" and "pds_host" in record:
            return record["pds_host"]
    return fallback
