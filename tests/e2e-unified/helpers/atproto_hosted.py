"""A hosted Bluesky account, booted from outside the nest: a real nest + the
e2e-flavor ``fauna-atproto-bridge`` (with ``--xrpc-listen``) + the fake PLC
directory, fake DNS and fake relay (``helpers/atproto_fakes.py``).

The setup ``test_atproto_firehose_post.py`` runs inline, lifted for the
single-outcome witnesses that need nothing but a hosted account to observe
(``test_atproto_hosted_account.py``): a handled user enters the hosted level
through the production ``set_integration_level`` transition, the admin approves
the bridge, the bridge mints the DID against the fake directory, and — unless
the caller holds it back — the ``_atproto`` TXT is published so the first-emit
gate opens.

Every seam is a test-only one compiled only into the bridge's e2e flavor
(convention 15, ``e2e-automation-surface-gating.md`` → the Go bridges' leg), so
callers pass the ``atproto_bridge_e2e_binary`` fixture's path.
"""

from __future__ import annotations

import contextlib
import json
import os
import ssl
import subprocess
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

import fauna_ffi
from helpers.atproto_fakes import FakeDNS, FakePlcDirectory, FakeRelay

HANDLE_DOMAIN = "fauna.test"
#: The host the bridge serves and announces: shared Rust's
#: `atproto_pds_host(<primary domain>)`.
PDS_HOST = f"pds.{HANDLE_DOMAIN}"


@dataclass
class HostedAccount:
    """What a witness drives and reads. ``ws`` is alice's WS-RPC client (open
    it with ``with``); ``did`` is her minted DID; ``rotation_key`` is the user
    rotation key that mint published, HELD by the test the way the user's
    client holds it (an ``fauna_ffi.atproto_rotation_key()`` dict), so a
    witness can sign for the identity — a retirement's tombstone."""

    nest: dict
    alice: dict
    ws: object
    did: str
    xrpc_base: str
    log_path: Path
    proc: subprocess.Popen
    directory: FakePlcDirectory
    dns: FakeDNS
    relay: FakeRelay
    rotation_key: dict
    _txt_published: bool = field(default=False, repr=False)

    def tail(self) -> str:
        return self.log_path.read_text(errors="replace")[-6000:]

    def assert_alive(self, during: str) -> None:
        assert self.proc.poll() is None, f"bridge exited {during}:\n" + self.tail()

    def xrpc_get(self, method: str, query: str = "") -> dict:
        url = f"{self.xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
        with urllib.request.urlopen(url, context=ssl._create_unverified_context(), timeout=15) as resp:
            return json.loads(resp.read())

    def xrpc_status(self, method: str, query: str = "") -> tuple[int, str | None]:
        """(HTTP status, XRPC ``error`` name) — for reads expected to refuse."""
        try:
            self.xrpc_get(method, query)
            return 200, None
        except urllib.error.HTTPError as exc:
            try:
                return exc.code, json.loads(exc.read()).get("error")
            except ValueError:
                return exc.code, None

    def enter_hosted(self, rotation_key: dict) -> None:
        """Enter the hosted level through the production transition, publishing
        ``rotation_key`` as the user's senior rotation key."""
        _enter_hosted(self.ws, rotation_key)
        self.rotation_key = rotation_key

    def await_mint(self, *, besides: set[str] = frozenset(), deadline_s: float = 60) -> str:
        """Wait for the bridge to mint a DID at the fake directory that is not
        in ``besides``, and return it."""
        deadline = time.monotonic() + deadline_s
        while time.monotonic() < deadline:
            fresh = [d for d in self.directory.minted_dids() if d not in besides]
            if fresh:
                return fresh[0]
            self.assert_alive("before minting")
            time.sleep(0.5)  # sleep-ok: poll interval of a deadline loop
        raise AssertionError(
            f"the bridge minted no DID besides {sorted(besides)}:\n" + self.tail()
        )

    def publish_txt(self) -> None:
        """Make alice's handle resolvable: the ``_atproto`` TXT a managed-mode
        client publishes, carrying the DID."""
        self.dns.publish(f"_atproto.alice.{HANDLE_DOMAIN}", [f"did={self.did}"])
        self._txt_published = True

    def post_texts(self) -> list[str]:
        """The text of every ``app.bsky.feed.post`` the hosted repo serves."""
        records = self.xrpc_get(
            "com.atproto.repo.listRecords",
            f"repo={self.did}&collection=app.bsky.feed.post&limit=100",
        )["records"]
        return [r.get("value", {}).get("text") for r in records]

    def await_post_texts(self, done, what: str, deadline_s: float = 120) -> list[str]:
        """Poll the hosted repo's posts until ``done(texts)`` holds.

        Polls are spaced wide on purpose: listRecords is the rate-limited
        anonymous XRPC surface (ClassPublicRead, 60 req / 60 s per IP), and the
        projection pass is asynchronous. The deadline is the correctness
        anchor, never the interval."""
        deadline = time.monotonic() + deadline_s
        texts: list[str] = []
        while time.monotonic() < deadline:
            status, _ = self.xrpc_status(
                "com.atproto.repo.listRecords",
                f"repo={self.did}&collection=app.bsky.feed.post&limit=100",
            )
            if status == 200:
                texts = self.post_texts()
                if done(texts):
                    return texts
            self.assert_alive(f"while waiting for {what}")
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop
        raise AssertionError(f"timed out waiting for {what}; repo posts {texts}\nlog:\n{self.tail()}")


def _enter_hosted(ws, rotation_key: dict) -> None:
    with ws:
        ws.call(
            "fauna.bridges.atproto.set_integration_level",
            {
                "target_level": "hosted_visible",
                "did_method": "plc",
                "user_rotation_pub_did_key": rotation_key["did_key"],
                "history_backfill": False,
            },
        )


@contextlib.contextmanager
def hosted_account(nest_binary, atproto_bin, tmp_path_factory, label: str, *, publish_txt: bool = True):
    """Yield a :class:`HostedAccount` whose bridge has minted alice's DID.

    With ``publish_txt=False`` the handle stays unresolvable, so the first-emit
    gate holds every projection until the caller runs ``publish_txt()``."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import open_registration, register_handled_actor
    from conftest import _make_nest, _repo_root
    from drivers.port_util import (
        find_free_port,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )
    from helpers.bridge_enrollment import approve_bridge

    directory = FakePlcDirectory()
    dns = FakeDNS()
    relay = FakeRelay()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, f"{label}-nest", claim_domain=HANDLE_DOMAIN,
    )
    open_registration(nest)
    proc = None
    log_fh = None
    try:
        admin = nest["admin"]
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        alice_ws = WsRpcAdminClient(
            nest["url"], actor_id=alice["actor_id_bytes"], signing_key=bytes(alice["signing_key"])
        )
        rotation_key = fauna_ffi.atproto_rotation_key()
        _enter_hosted(alice_ws, rotation_key)

        tmp = tmp_path_factory.mktemp(f"{label}-bridge")
        keyfile_path = tmp / "atproto.pds.key"
        xrpc_port = find_free_port()
        minted = subprocess.run(
            [atproto_bin, f"--keypair-file={keyfile_path}", "--print-pubkey"],
            cwd=_repo_root, capture_output=True, text=True, check=True,
        )
        pubkey_hex = minted.stdout.strip()

        env = os.environ.copy()
        env["FAUNA_ATPROTO_PLC_DIRECTORY_URL"] = directory.url
        env["FAUNA_ATPROTO_FAKE_DNS_URL"] = dns.url
        env["FAUNA_ATPROTO_RELAY_URL"] = relay.url
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
        reap_descendants_of(proc.pid)
        track_process(proc)

        def _tail() -> str:
            return log_path.read_text(errors="replace")[-6000:]

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
                if any(bytes(r["ed25519_pubkey"]).hex() == pubkey_hex for r in rows):
                    break
                assert proc.poll() is None, "bridge exited pre-enroll:\n" + _tail()
                time.sleep(0.5)  # sleep-ok: poll interval of a deadline loop
            else:
                raise AssertionError("atproto.pds bridge never appeared PENDING:\n" + _tail())
        approve_bridge(nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds")

        account = HostedAccount(
            nest=nest, alice=alice, ws=alice_ws, did="",
            xrpc_base=f"https://127.0.0.1:{xrpc_port}", log_path=log_path, proc=proc,
            directory=directory, dns=dns, relay=relay, rotation_key=rotation_key,
        )
        did = account.await_mint()
        assert did.startswith("did:plc:"), (
            f"the bridge must mint a did:plc, got {did!r}:\n" + _tail()
        )
        account.did = did
        if publish_txt:
            account.publish_txt()
        yield account
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
        relay.close()
        dns.close()
        directory.close()
        nest_cleanup()
