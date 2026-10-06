"""tier_3: S3 public post → ``subscribeRepos`` firehose frame, end to end —
real nest + real ``fauna-atproto-bridge`` (with ``--xrpc-listen``) + a fake PLC
directory + a fake DNS (``docs/goal/behavior/atproto-pds-bridge.md``
§ Projection & backfill; ``atproto-pds-full.md`` § Ecosystem reality).

This closes flow-trace rows 1, 4 and 6:

1. a handled user creates a **public post** → nest stores it → the
   ``projection_ready`` push nudges the bridge → the bridge fetches it via
   ``fetch_public_posts`` → shared-Rust FFI translates it to
   ``app.bsky.feed.post`` → the per-user funnel applies/signs/persists it →
   a ``#commit`` frame carrying that record reaches a WS subscriber on
   ``com.atproto.sync.subscribeRepos``;
4. before the user's handle resolves, the **first emit is HARD-GATED** — the
   first-impression trap (``atproto-pds-full.md`` § Ecosystem reality: a
   self-hosted account can be relay-indexed yet permanently 404 on the AppView
   if DID resolution fails at first index, so the DID doc + handle must be
   fully resolvable *before* the first event hits the firehose). The test
   asserts the repo stays **empty** while the ``_atproto`` TXT is unpublished,
   then that publishing it lets the deferred posts through on the next pass;
6. the user **changes their Fauna handle** → the pending action applies (through
   the production executor, reached via the ``test-hooks`` run-due endpoint
   because the real cool-off is 6 h) → nest re-derives the ATProto handle at
   read time → the bridge's next pass republishes ``alsoKnownAs`` at the
   directory, **chained to the log head and carrying every other field forward
   verbatim** → an ``#identity`` frame reaches the same live WS subscriber
   (``atproto-pds-bridge.md`` § Identity — handle changes follow the Fauna
   handle, and the network is told to re-resolve).

Steps 8-9 extend this into the **S4-D layer-2 disable ladder**
(``atproto-pds-bridge.md`` § Disable & revocation): the user steps OFF the
hosted level (``set_integration_level → "off"``), and the bridge's next pass
must unserve the repo — ``getRepoStatus`` reports ``active:false`` + ``status
"deactivated"``, content reads refuse ``RepoDeactivated``, ``listRepos`` keeps
it listed but inactive — and announce one ``#account(active=false)`` frame on
the same live subscriber. Re-entering the hosted level reverses all three,
restoring the SAME identity (no re-mint) with an ``#account(active=true)`` frame.

Both fakes are test-only seams, artifact/test IPC, never operator config
(§ Product invariants): ``FAUNA_ATPROTO_PLC_DIRECTORY_URL`` for the directory
and ``FAUNA_ATPROTO_FAKE_DNS_URL`` for the TXT lookups. The DNS one is a URL
rather than a canned table precisely because row 4 needs the record to appear
*part way through the test* — the record carries the DID, which does not exist
until the mint completes, exactly as in a real deployment.

The seam supplies an *answer* to the resolvability gate; it never bypasses it.

Driving the post over WS-RPC (rather than through a client UI) is the same
pragmatic exception ``test_atproto_identity_mint.py`` takes: the S4 client
enable UI does not exist yet. Per ``testing.md`` conventions point 8, the
client-UI-only equivalent proving a real user creates a public post through the
UI is ``tests/e2e-unified/tests/test_feed.py::test_create_post``, which drives
every app's own composer via ``logged_in_app.feed.create_post``.
"""

import json
import os
import shutil
import ssl
import subprocess
import time

import pytest
import websocket  # type: ignore[import-untyped]  # from `websocket-client`

from helpers.app_surface import skip_environment
from helpers.atproto_firehose import (
    car_records as _car_records,
    decode_frame as _decode_frame,
)
from helpers.atproto_fakes import FakeDNS, FakePlcDirectory
from helpers.xrpc_client import put_blob as _put_blob

pytestmark = pytest.mark.tier_3

USER_ROTATION_PUB = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"  # gitleaks:allow
HANDLE_DOMAIN = "fauna.test"
_TLS_INSECURE = {"cert_reqs": ssl.CERT_NONE, "check_hostname": False}

POST_ONE = "hello bluesky, this is a public fauna post"
POST_TWO = "second post, published after the handle resolves"
# S5 slice 3: a reply to POST_ONE, which IS bridged, so the projected record
# must carry a real reply ref. And a reply to the pre-enable post, which is NOT
# bridged (the projection floor withheld it), so its ref must be DROPPED and the
# post project standalone — never a ref to a record no repo serves.
POST_REPLY_TO_BRIDGED = "a reply to a post that is on the network"
POST_REPLY_TO_UNBRIDGED = "a reply to a post that never crossed the floor"
POST_WITH_IMAGE = "a public post carrying a picture"
# The attachment bytes. Not a real PNG — nothing in this path decodes the
# image: the nest stores opaque bytes, the bridge re-hashes them, and the
# AppView is the only thing that would ever render one. A recognisable string
# makes a mismatch readable in a failure message.
IMAGE_BYTES = b"\x89PNG\r\n\x1a\nfauna-atproto-projection-test-image"
# A DIFFERENT PNG from IMAGE_BYTES on purpose: distinct bytes hash to distinct
# blob CIDs, so an avatar ref that accidentally carried the post image's CID
# (or vice versa) fails instead of passing by coincidence.
AVATAR_BYTES = b"\x89PNG\r\n\x1a\nfauna-atproto-projection-test-avatar"


def _make_ts_segments(tmp_dir, seconds: int = 3, height: int = 180) -> list[bytes]:
    """Real HLS segments, the way the nest's own transcode path produces them.

    Unlike the image bytes above, these CANNOT be a recognisable placeholder:
    the projection concatenates them and hands the result to ffmpeg, so only
    genuine h264+aac MPEG-TS proves the assembly claim. A keyframe every second
    is what lets the muxer cut at the 1 s boundary — the default GOP outlasts
    this clip and would yield a single segment, testing nothing about
    concatenation.
    """
    if shutil.which("ffmpeg") is None:
        skip_environment(
            "ffmpeg not on PATH — deliberately not provisioned on every dev "
            "machine; the video-projection assembly claim needs a real muxer"
        )
    out_dir = tmp_dir / "ts"
    out_dir.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            "ffmpeg", "-nostdin", "-loglevel", "error",
            "-f", "lavfi", "-i",
            f"testsrc=size={height * 16 // 9}x{height}:rate=15:duration={seconds}",
            "-f", "lavfi", "-i", f"sine=frequency=440:duration={seconds}",
            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
            "-g", "15", "-keyint_min", "15", "-sc_threshold", "0",
            "-c:a", "aac", "-b:a", "64k",
            "-f", "hls", "-hls_time", "1", "-hls_list_size", "0",
            "-hls_segment_filename", str(out_dir / "seg%03d.ts"),
            str(out_dir / "stream.m3u8"),
        ],
        check=True,
        capture_output=True,
    )
    names = sorted(p for p in out_dir.iterdir() if p.suffix == ".ts")
    assert len(names) >= 2, (
        f"expected several segments, got {len(names)} — the assembly claim is "
        "only interesting across a segment boundary"
    )
    return [p.read_bytes() for p in names]
# Alice's back-catalogue: a public post written BEFORE she ever enabled the
# Bluesky integration. She declines history backfill, so this must never reach
# the network (`atproto-pds-bridge.md` § Projection & backfill — forward-only
# default: "nothing historical").
POST_BEFORE_ENABLE = "a public post from before alice ever heard of bluesky"


@pytest.mark.feature("atproto")
def test_public_post_reaches_the_firehose(nest_binary, atproto_bridge_e2e_binary, tmp_path_factory):
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import _bridge_admin_post, _make_nest, _repo_root
    from helpers.bridge_enrollment import approve_bridge
    from drivers.port_util import (
        find_free_port,
        popen_group_kwargs,
        reap_descendants_of,
        track_process,
        untrack_process,
    )
    from fauna_ffi import (
        build_post,
        build_post_with_media,
        build_profile_with_pictures,
        build_video_post,
        cid_of_post_id,
        content_cid,
    )
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

    directory = FakePlcDirectory()
    dns = FakeDNS()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "atproto-firehose-nest",
        # The domained claim, which is also this nest's ONLY registration of
        # HANDLE_DOMAIN — the `add_local_domain` that used to sit further down
        # is gone, because two doors onto one domain means the loser's
        # arguments are silently discarded (`add_local_domain` is idempotent by
        # domain NAME). The claim's own cert mode is `expand_primary` where that
        # call asked for `per_host`, and the difference is inert here: nothing
        # in the DNS matrix or the `_atproto` TXT row reads
        # `mta_sts_cert_mode`, and a plain-HTTP test nest arms no ACME at all
        # (`testing.md` § Default app and nest mode, ruling (3)).
        claim_domain=HANDLE_DOMAIN,
    )
    from common.auth import open_registration
    open_registration(nest)
    proc = None
    log_fh = None
    sub = None
    try:
        admin = nest["admin"]

        # ── 1. A handled user enters the hosted level (the depth selector's
        # transition kind — the production enable path) and the bridge boots
        # with the XRPC/firehose listener up. ──
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        alice_secret = bytes(alice["signing_key"])
        alice_ws = WsRpcAdminClient(
            nest["url"], actor_id=alice["actor_id_bytes"], signing_key=alice_secret
        )
        with alice_ws:
            # A public post that predates the integration entirely — the
            # back-catalogue the forward-only default must never publish.
            # Created BEFORE the transition on purpose: the projection floor is
            # derived at the instant consent is given, so this post sits below
            # it and nest must never serve it to the bridge.
            history_post_id = alice_ws.call(
                "fauna.posts.create",
                {"body": build_post(alice_secret, POST_BEFORE_ENABLE)},
            )["post_id"]
            assert history_post_id

            alice_ws.call(
                "fauna.bridges.atproto.set_integration_level",
                {
                    "target_level": "hosted_visible",
                    "did_method": "plc",
                    "user_rotation_pub_did_key": USER_ROTATION_PUB,
                    "history_backfill": False,
                },
            )

        tmp = tmp_path_factory.mktemp("atproto-firehose-bridge")
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
            return log_path.read_text(errors="replace")[-6000:]

        xrpc_base = f"https://127.0.0.1:{xrpc_port}"

        def _xrpc_get(method: str, query: str = "") -> dict:
            import urllib.request
            url = f"{xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
            ctx = ssl._create_unverified_context()
            with urllib.request.urlopen(url, context=ctx, timeout=15) as resp:
                return json.loads(resp.read())

        def _xrpc_get_bytes(method: str, query: str = "") -> bytes:
            """The raw-bytes sibling of :func:`_xrpc_get` — sync.getBlob serves
            octets, not JSON."""
            import urllib.request
            url = f"{xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
            ctx = ssl._create_unverified_context()
            with urllib.request.urlopen(url, context=ctx, timeout=15) as resp:
                return resp.read()

        # ── 2. Admin pins the primary domain and approves the bridge. ──
        admin_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with admin_ws:
            # No `add_local_domain`: the claim carried HANDLE_DOMAIN as its
            # `mail_domain`, so the primary row already exists — one door onto
            # the domain rather than two.
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                rows = admin_ws.call(
                    "fauna.bridges.list_service_users", {"status": "pending"}
                )["service_users"]
                if any(bytes(r["ed25519_pubkey"]).hex() == pubkey_hex for r in rows):
                    break
                assert proc.poll() is None, "bridge exited pre-enroll:\n" + _tail()
                time.sleep(0.5)
            else:
                pytest.fail("atproto.pds bridge never appeared PENDING:\n" + _tail())

        approve_bridge(
            nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
        )

        # ── 3. The bridge mints the DID against the fake directory. ──
        deadline = time.monotonic() + 60
        did = None
        while time.monotonic() < deadline:
            subs = directory.snapshot()
            if subs:
                did = subs[0][0]
                break
            assert proc.poll() is None, "bridge exited before minting:\n" + _tail()
            time.sleep(0.5)
        assert did is not None, "the bridge must mint a DID:\n" + _tail()
        assert did.startswith("did:plc:"), did

        # ── 4. FLOW-TRACE ROW 4 — the user posts publicly while the handle is
        # still unresolvable. The post-create push nudges a projection pass;
        # the first-emit gate must DEFER it, leaving the repo empty. ──
        with alice_ws:
            post_one_id = alice_ws.call(
                "fauna.posts.create", {"body": build_post(alice_secret, POST_ONE)}
            )["post_id"]
        assert post_one_id

        gated_deadline = time.monotonic() + 45
        saw_gate = False
        while time.monotonic() < gated_deadline:
            if "first-emit gated" in log_path.read_text(errors="replace"):
                saw_gate = True
                break
            assert proc.poll() is None, "bridge exited while gated:\n" + _tail()
            time.sleep(0.5)
        assert saw_gate, (
            "the first-emit gate must DEFER projection while the _atproto TXT is "
            "unpublished (first-impression trap); log:\n" + _tail()
        )
        assert dns.lookup_count() > 0, "the gate must actually resolve through DNS"

        repos = _xrpc_get("com.atproto.sync.listRepos")["repos"]
        assert repos == [], (
            "NOTHING may be projected while the first emit is gated — a repo here "
            f"means the gate leaked; got {repos}\nlog:\n" + _tail()
        )

        # ── 5. The handle becomes resolvable (a managed-mode client publishes
        # the `_atproto` TXT the DNS matrix carries). A second post nudges the
        # next pass immediately, rather than waiting out the poll backstop. ──
        dns.publish(f"_atproto.alice.{HANDLE_DOMAIN}", [f"did={did}"])
        with alice_ws:
            post_two_id = alice_ws.call(
                "fauna.posts.create", {"body": build_post(alice_secret, POST_TWO)}
            )["post_id"]
        assert post_two_id

        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            repos = _xrpc_get("com.atproto.sync.listRepos")["repos"]
            if repos:
                break
            assert proc.poll() is None, "bridge exited during projection:\n" + _tail()
            time.sleep(1)
        assert repos, (
            "once the TXT resolves the gate must OPEN and the deferred posts "
            "project (flow-trace retry); log:\n" + _tail()
        )
        assert repos[0]["did"] == did, repos

        # ── 6. FLOW-TRACE ROW 1 — a WS subscriber on subscribeRepos replays
        # from the start of the outbox and receives #commit frames carrying the
        # user's posts as app.bsky.feed.post records. ──
        sub = websocket.create_connection(
            f"wss://127.0.0.1:{xrpc_port}/xrpc/com.atproto.sync.subscribeRepos?cursor=0",
            sslopt=_TLS_INSECURE,
            timeout=45,
        )
        commits: list[tuple[dict, dict]] = []
        texts: set[str] = set()
        paths: list[str] = []
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and POST_ONE not in texts:
            try:
                message = sub.recv()
            except Exception as exc:  # noqa: BLE001 — surfaced in the assert below
                pytest.fail(f"firehose read failed: {exc}\nlog:\n{_tail()}")
            assert isinstance(message, (bytes, bytearray)), (
                f"subscribeRepos frames are BINARY, got {type(message)}"
            )
            header, body = _decode_frame(bytes(message))
            assert header.get("op") != -1, f"error frame from the firehose: {body}"
            if header.get("t") != "#commit":
                continue
            commits.append((header, body))
            paths.extend(op["path"] for op in body.get("ops", []))
            for record in _car_records(body.get("blocks", b"")):
                if record.get("$type") == "app.bsky.feed.post":
                    texts.add(record.get("text", ""))

        assert commits, (
            "a subscriber must receive at least one #commit frame; log:\n" + _tail()
        )
        assert any(p.startswith("app.bsky.feed.post/") for p in paths), (
            f"a #commit must carry an app.bsky.feed.post op; paths={paths}"
        )
        assert POST_ONE in texts, (
            "the #commit frame's blocks must carry the user's public post record "
            f"({POST_ONE!r}); got {texts}, paths={paths}\nlog:\n" + _tail()
        )

        # ── 6b. THE FORWARD-ONLY DEFAULT (`atproto-pds-bridge.md` § Projection &
        # backfill, row 1 of the watermark table). Alice declined history
        # backfill, so the post she wrote BEFORE enabling must not exist on the
        # network — not in a frame, and not in the repo. Asserted against the
        # published repo rather than only the frames, so it holds regardless of
        # the order frames arrived in.
        #
        # This is a causal assertion, not a settle-sleep (convention 14): the
        # repo demonstrably contains POST_ONE, which was written AFTER the
        # historical one and therefore projects after it in the oldest-first
        # stream. If the floor leaked, the older post would already be here. ──
        assert POST_BEFORE_ENABLE not in texts, (
            "a post predating the user's consent reached the FIREHOSE — the "
            "forward-only default is not being honoured; texts=" + repr(texts)
        )
        listed = _xrpc_get(
            "com.atproto.repo.listRecords",
            f"repo={did}&collection=app.bsky.feed.post&limit=100",
        )["records"]
        published = {r.get("value", {}).get("text", "") for r in listed}
        assert POST_ONE in published, (
            "sanity: the repo must hold the post written after enabling, else "
            f"this assertion proves nothing; got {published}"
        )
        assert POST_BEFORE_ENABLE not in published, (
            "a post predating the user's consent was PUBLISHED to their repo. "
            "The forward-only default (`:92` — 'nothing historical') is "
            "inverted; per `:84` the network cannot be made to forget it. "
            f"published={published}"
        )

        # ── 6c. REPLY/QUOTE REF RESOLUTION (`atproto-pds-bridge.md` §
        # Projection & backfill, translation edges — S5 slice 3). Two replies
        # whose parents differ ONLY in whether they are bridged, so a single
        # pass proves both arms of the rule:
        #
        #   * reply to POST_ONE (bridged)          -> a real `reply` ref whose
        #     parent AND root are POST_ONE's record (it starts the thread).
        #   * reply to POST_BEFORE_ENABLE (not bridged, the floor withheld it)
        #     -> the ref is DROPPED and the post projects STANDALONE. This is
        #     the resolved reading of the section's former either/or; the post
        #     must be PRESENT (not skipped) and carry no `reply`.
        #
        # The Go tests cover the resolution against a fake translator; this is
        # what proves the REAL shared-Rust extraction reads a real Fauna
        # `Reference::Reply` out of real stored post bytes. ──
        parent_uri = next(
            r["uri"] for r in listed if r.get("value", {}).get("text") == POST_ONE
        )
        parent_cid = next(
            r["cid"] for r in listed if r.get("value", {}).get("text") == POST_ONE
        )
        with alice_ws:
            reply_bridged_id = alice_ws.call(
                "fauna.posts.create",
                {
                    "body": build_post(
                        alice_secret,
                        POST_REPLY_TO_BRIDGED,
                        reply_to=cid_of_post_id(post_one_id),
                    )
                },
            )["post_id"]
            alice_ws.call(
                "fauna.posts.create",
                {
                    "body": build_post(
                        alice_secret,
                        POST_REPLY_TO_UNBRIDGED,
                        reply_to=cid_of_post_id(history_post_id),
                    )
                },
            )
        assert reply_bridged_id

        # Wait on the projection landing both replies, then read the repo — a
        # deadline poll on observable state, not a settle-sleep (convention 14).
        deadline = time.monotonic() + 120
        by_text: dict[str, dict] = {}
        while time.monotonic() < deadline:
            records = _xrpc_get(
                "com.atproto.repo.listRecords",
                f"repo={did}&collection=app.bsky.feed.post&limit=100",
            )["records"]
            by_text = {r.get("value", {}).get("text", ""): r for r in records}
            if {POST_REPLY_TO_BRIDGED, POST_REPLY_TO_UNBRIDGED} <= by_text.keys():
                break
            assert proc.poll() is None, "bridge exited projecting replies:\n" + _tail()
            time.sleep(2)

        assert POST_REPLY_TO_BRIDGED in by_text, (
            "the reply to a bridged post never projected; "
            f"got {sorted(by_text)}\nlog:\n" + _tail()
        )
        reply_value = by_text[POST_REPLY_TO_BRIDGED]["value"]
        assert "reply" in reply_value, (
            "a reply whose parent IS bridged must carry a reply ref, not project "
            f"standalone; record={reply_value}"
        )
        assert reply_value["reply"]["parent"]["uri"] == parent_uri, (
            f"reply.parent.uri = {reply_value['reply']['parent']['uri']}, "
            f"want the bridged parent {parent_uri}"
        )
        assert reply_value["reply"]["parent"]["cid"] == parent_cid, reply_value
        # POST_ONE starts the thread, so it is its own root.
        assert reply_value["reply"]["root"]["uri"] == parent_uri, (
            f"reply.root.uri = {reply_value['reply']['root']['uri']}, want "
            f"{parent_uri} (the parent starts the thread)"
        )
        assert reply_value["reply"]["root"]["cid"] == parent_cid, reply_value

        assert POST_REPLY_TO_UNBRIDGED in by_text, (
            "a reply to an UNBRIDGED parent must still project — dropping the "
            "ref, never skipping the post (skipping would cascade to every "
            f"reply beneath it). got {sorted(by_text)}\nlog:\n" + _tail()
        )
        standalone_value = by_text[POST_REPLY_TO_UNBRIDGED]["value"]
        assert "reply" not in standalone_value, (
            "a reply whose parent was never bridged must project STANDALONE — "
            "emitting a ref here points at a record no repo serves; "
            f"record={standalone_value}"
        )

        # ── 6d. MEDIA BLOBS (`atproto-pds-bridge.md` § Projection & backfill,
        # the media-blob rule — S5 slice 1a). The full production flow, which no
        # unit test spans: alice uploads image bytes to the nest's CID-addressed
        # blob route -> creates a post whose real `PostBody::TextWithMedia`
        # names that blob -> the shared-Rust extraction reads the attachment out
        # of the stored post bytes -> the bridge fetches those bytes back from
        # the nest, re-hashes them to an ATProto sha256 CID and stores them ->
        # the projected record carries a real `app.bsky.embed.images` ref ->
        # `com.atproto.sync.getBlob` serves the SAME bytes alice uploaded.
        #
        # The last assertion is the one that matters: a blob ref an AppView
        # cannot dereference is exactly the dangling ref § Projection & backfill
        # forbids, and only an end-to-end fetch proves it is not one. ──
        blob_cid = content_cid(IMAGE_BYTES)
        _put_blob(nest["url"], alice["token"], blob_cid, IMAGE_BYTES)
        with alice_ws:
            image_post_id = alice_ws.call(
                "fauna.posts.create",
                {
                    "body": build_post_with_media(
                        alice_secret,
                        POST_WITH_IMAGE,
                        [
                            {
                                "blob_cid": blob_cid,
                                "mime": "image/png",
                                "size_bytes": len(IMAGE_BYTES),
                                "width": 800,
                                "height": 600,
                            }
                        ],
                    )
                },
            )["post_id"]
        assert image_post_id

        deadline = time.monotonic() + 120
        image_value: dict = {}
        while time.monotonic() < deadline:
            records = _xrpc_get(
                "com.atproto.repo.listRecords",
                f"repo={did}&collection=app.bsky.feed.post&limit=100",
            )["records"]
            for r in records:
                if r.get("value", {}).get("text") == POST_WITH_IMAGE:
                    image_value = r["value"]
                    break
            if image_value:
                break
            assert proc.poll() is None, "bridge exited projecting media:\n" + _tail()
            # The assertion below is on observed repo state, and a green run
            # leaves this loop as soon as the record appears — the interval is
            # backoff between observations, never a wait for a fixed duration
            # (testing.md convention 14, mechanism 1).
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop

        assert image_value, (
            "the media post never projected; a post with an unpublishable "
            "attachment must still publish, and this one IS publishable\nlog:\n"
            + _tail()
        )
        embed = image_value.get("embed", {})
        assert embed.get("$type") == "app.bsky.embed.images", (
            "a media post must carry an images embed, not project text-only; "
            f"record={image_value}"
        )
        images = embed.get("images", [])
        assert len(images) == 1, f"embed carried {len(images)} images, want 1: {embed}"
        blob_ref = images[0].get("image", {})
        assert blob_ref.get("mimeType") == "image/png", blob_ref
        assert blob_ref.get("size") == len(IMAGE_BYTES), blob_ref
        assert images[0].get("aspectRatio") == {"width": 800, "height": 600}, images[0]
        # The ref is the ATPROTO blob CID (sha256), never the Fauna one (blake3)
        # the post named — the re-hash § Goal accepts as the projection's cost.
        atproto_blob_cid = blob_ref.get("ref", {}).get("$link") or blob_ref.get("ref")
        assert isinstance(atproto_blob_cid, str) and atproto_blob_cid, (
            f"blob ref carried no CID link: {blob_ref}"
        )
        assert atproto_blob_cid != blob_cid, (
            "the record published the FAUNA cid; ATProto addresses blobs by "
            f"sha256, so the ref must be the re-hashed CID ({atproto_blob_cid})"
        )

        # The ref resolves: getBlob serves back exactly what alice uploaded.
        served = _xrpc_get_bytes(
            "com.atproto.sync.getBlob", f"did={did}&cid={atproto_blob_cid}"
        )
        assert served == IMAGE_BYTES, (
            "sync.getBlob served different bytes than alice uploaded — the "
            f"published blob ref does not resolve. got {len(served)} bytes"
        )

        # And the blob is enumerable, which is what an account migration walks.
        listed_blobs = _xrpc_get("com.atproto.sync.listBlobs", f"did={did}")["cids"]
        assert atproto_blob_cid in listed_blobs, (
            f"listBlobs did not list the published blob: {listed_blobs}"
        )

        # ── 6d-bis. VIDEO (`atproto-pds-bridge.md` § Projection & backfill, the
        # video mapping). The one span no unit test covers: a Fauna video is an
        # HLS manifest plus per-resolution MPEG-TS segments, and the lexicon
        # wants ONE mp4 blob — so alice uploads real segments to the nest ->
        # creates a real `PostBody::Video` naming them -> the shared-Rust
        # extraction describes the renditions -> the bridge fetches those
        # segments back, concatenates them in playback order, remuxes to mp4
        # with stream copy, re-hashes and stores -> the record carries a real
        # `app.bsky.embed.video` -> `com.atproto.sync.getBlob` serves an mp4.
        #
        # This is also the arm that used to publish a BLANK post: a video post
        # carries no text, so before the mapping existed the projection
        # committed an empty record for every video a user posted. ──
        segments = _make_ts_segments(tmp_path_factory.mktemp("atproto-video"))
        seg_descs = []
        for seg in segments:
            seg_cid = content_cid(seg)
            _put_blob(nest["url"], alice["token"], seg_cid, seg)
            seg_descs.append({
                "blob_cid": seg_cid,
                "resolution": 720,
                "byte_size": len(seg),
            })
        # The manifest and thumbnail are named by the post but never published:
        # `app.bsky.embed.video` has no thumbnail field, and the manifest is the
        # dedup key, not bytes to serve.
        manifest_cid = content_cid(b"fauna-atproto-projection-test-manifest")
        thumb_cid = content_cid(b"fauna-atproto-projection-test-thumb")
        with alice_ws:
            video_post_id = alice_ws.call(
                "fauna.posts.create",
                {
                    "body": build_video_post(
                        alice_secret,
                        manifest_cid=manifest_cid,
                        thumbnail_cid=thumb_cid,
                        segments=seg_descs,
                        duration_ms=3000,
                        aspect=(16, 9),
                    )
                },
            )["post_id"]
        assert video_post_id

        deadline = time.monotonic() + 120
        video_value: dict = {}
        while time.monotonic() < deadline:
            records = _xrpc_get(
                "com.atproto.repo.listRecords",
                f"repo={did}&collection=app.bsky.feed.post&limit=100",
            )["records"]
            for r in records:
                if r.get("value", {}).get("embed", {}).get("$type") == "app.bsky.embed.video":
                    video_value = r["value"]
                    break
            if video_value:
                break
            assert proc.poll() is None, "bridge exited projecting video:\n" + _tail()
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop

        assert video_value, (
            "the video post never projected as a video embed — a Fauna video "
            "must assemble into one mp4 blob, not project as a blank record"
            "\nlog:\n" + _tail()
        )
        video_embed = video_value["embed"]
        assert video_value.get("text") == "", (
            f"a video post carries no text of its own: {video_value}"
        )
        assert video_embed.get("aspectRatio") == {"width": 16, "height": 9}, (
            f"aspectRatio must come from the post, not the rendition: {video_embed}"
        )
        video_blob = video_embed.get("video", {})
        assert video_blob.get("mimeType") == "video/mp4", (
            f"the lexicon accepts only mp4: {video_blob}"
        )
        atproto_video_cid = video_blob.get("ref", {}).get("$link") or video_blob.get("ref")
        assert isinstance(atproto_video_cid, str) and atproto_video_cid, (
            f"video blob ref carried no CID link: {video_blob}"
        )
        # The published bytes are ASSEMBLED, so the ref can equal no CID the post
        # named — not a segment's, and not the manifest's.
        for named in [d["blob_cid"] for d in seg_descs] + [manifest_cid, thumb_cid]:
            assert atproto_video_cid != named, (
                "the record referenced a Fauna CID the post named; the published "
                f"video is assembled bytes with a CID of its own ({named})"
            )

        # The ref resolves, and what it resolves to is a real mp4 — the whole
        # point of the mapping. Checked by the container's own `ftyp` box rather
        # than by size, since a truncated or mis-remuxed blob would still have a
        # plausible length.
        served_video = _xrpc_get_bytes(
            "com.atproto.sync.getBlob", f"did={did}&cid={atproto_video_cid}"
        )
        assert len(served_video) > 0, "sync.getBlob served no bytes for the video"
        assert served_video[4:8] == b"ftyp", (
            "sync.getBlob served something that is not an mp4 — the remux did "
            f"not produce a valid container (first bytes: {served_video[:16]!r})"
        )
        assert video_blob.get("size") == len(served_video), (
            f"the record's size {video_blob.get('size')} disagrees with the "
            f"{len(served_video)} bytes getBlob serves"
        )
        # `+faststart`: the moov atom must precede mdat, or a consumer cannot
        # begin playback without fetching the whole blob.
        assert served_video.index(b"moov") < served_video.index(b"mdat"), (
            "the assembled mp4 is not faststart — moov follows mdat, so playback "
            "cannot begin until the entire video has been fetched"
        )

        # ── 6e. PROFILE PICTURES (`atproto-pds-bridge.md` § Projection &
        # backfill, *Scope: posts + profile* + the media-blob rule). The same
        # end-to-end span as 6d, for the profile singleton: alice uploads avatar
        # bytes -> publishes a signed `Profile` naming that blob -> the
        # shared-Rust extraction reads the two ContentHash fields out of the
        # stored profile bytes -> the bridge fetches them back, SNIFFS the media
        # type (a profile stores a bare hash, so nothing declares it) and stores
        # them -> the `app.bsky.actor.profile` record at rkey `self` carries a
        # real blob ref -> `com.atproto.sync.getBlob` serves the same bytes.
        #
        # Then the arm a create-only path silently gets wrong: CLEARING the
        # avatar must remove the ref, not leave the old blob referenced. ──
        avatar_cid = content_cid(AVATAR_BYTES)
        _put_blob(nest["url"], alice["token"], avatar_cid, AVATAR_BYTES)
        with alice_ws:
            alice_ws.call(
                "fauna.profile.set",
                {
                    "body": build_profile_with_pictures(
                        alice_secret,
                        display_name="Alice",
                        avatar_cid=avatar_cid,
                    )
                },
            )

        profile_value = None
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            try:
                profile_value = _xrpc_get(
                    "com.atproto.repo.getRecord",
                    f"repo={did}&collection=app.bsky.actor.profile&rkey=self",
                )["value"]
            except Exception:  # noqa: BLE001 — the record may not exist yet
                profile_value = None
            if profile_value and profile_value.get("avatar"):
                break
            assert proc.poll() is None, "bridge exited projecting the profile:\n" + _tail()
            # Deadline poll on observed repo state, not a fixed wait — a green
            # run leaves as soon as the ref appears (testing.md convention 14).
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop

        assert profile_value and profile_value.get("avatar"), (
            "the profile never projected its avatar; a handle whose profile "
            "record carries no picture is what § Projection scope calls "
            "broken/bot-like on bsky.app\nlog:\n" + _tail()
        )
        avatar_ref = profile_value["avatar"]
        assert avatar_ref.get("mimeType") == "image/png", (
            "the media type must be SNIFFED from the bytes — a profile stores a "
            f"bare ContentHash and declares none: {avatar_ref}"
        )
        assert avatar_ref.get("size") == len(AVATAR_BYTES), avatar_ref
        atproto_avatar_cid = (
            avatar_ref.get("ref", {}).get("$link") or avatar_ref.get("ref")
        )
        assert isinstance(atproto_avatar_cid, str) and atproto_avatar_cid, (
            f"avatar ref carried no CID link: {avatar_ref}"
        )
        assert atproto_avatar_cid != avatar_cid, (
            "the record published the FAUNA cid; ATProto addresses blobs by "
            f"sha256, so the ref must be the re-hashed CID ({atproto_avatar_cid})"
        )
        # The ref resolves — the assertion that proves it is not a dangling ref.
        served_avatar = _xrpc_get_bytes(
            "com.atproto.sync.getBlob", f"did={did}&cid={atproto_avatar_cid}"
        )
        assert served_avatar == AVATAR_BYTES, (
            "sync.getBlob served different bytes than alice uploaded — the "
            f"published avatar ref does not resolve. got {len(served_avatar)} bytes"
        )

        # CLEARING it: the same profile, no picture. The next pass must drop the
        # ref rather than leave the record pointing at the old blob.
        with alice_ws:
            alice_ws.call(
                "fauna.profile.set",
                {
                    "body": build_profile_with_pictures(
                        alice_secret, display_name="Alice cleared"
                    )
                },
            )

        cleared = False
        deadline = time.monotonic() + 90
        while time.monotonic() < deadline:
            value = _xrpc_get(
                "com.atproto.repo.getRecord",
                f"repo={did}&collection=app.bsky.actor.profile&rkey=self",
            )["value"]
            if value.get("displayName") == "Alice cleared":
                cleared = "avatar" not in value
                break
            assert proc.poll() is None, "bridge exited clearing the avatar:\n" + _tail()
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop

        assert cleared, (
            "clearing the avatar must REMOVE the blob ref from the record — a "
            "create-only projection leaves the old picture referenced forever"
            "\nlog:\n" + _tail()
        )

        # ── 7. FLOW-TRACE ROW 6 — the RENAME HOOK. The user changes their Fauna
        # handle; the pending action's 6 h cool-off is fast-forwarded through the
        # test hook, which then runs the SAME executor the background tick runs.
        # The ATProto handle is derived at read time, so applying it IS the
        # rename: the bridge's next pass republishes `alsoKnownAs` at the
        # directory (chained to the log head) and emits an #identity frame so the
        # network re-resolves (`atproto-pds-bridge.md` § Identity). ──
        new_atproto_handle = f"alicerenamed.{HANDLE_DOMAIN}"
        dns.publish(f"_atproto.{new_atproto_handle}", [f"did={did}"])
        with alice_ws:
            alice_ws.call("fauna.profile.handle.change", {"handle": "alicerenamed"})
        ran = _bridge_admin_post(
            nest["url"], admin["token"], "/api/v1/test/pending_actions/run_due", {}
        )
        assert ran.get("executed", 0) >= 1, f"the handle change never applied: {ran}"

        deadline = time.monotonic() + 90
        update_op = None
        while time.monotonic() < deadline:
            subs = directory.snapshot()
            if len(subs) >= 2:
                update_op = subs[-1][1]
                break
            assert not directory.rejected, (
                "the bridge chained a PLC update op to the wrong prev: "
                f"{directory.rejected}\nlog:\n{_tail()}"
            )
            assert proc.poll() is None, "bridge exited during the rename:\n" + _tail()
            time.sleep(1)
        assert update_op is not None, (
            "a handle change must republish alsoKnownAs at the PLC directory "
            "(rename hook); log:\n" + _tail()
        )
        assert update_op["alsoKnownAs"] == [f"at://{new_atproto_handle}"], update_op
        assert update_op["prev"] is not None, (
            f"the update op must chain to the genesis op, not restart: {update_op}"
        )
        # The user's senior rotation key and the repo signing key survive a
        # rename — the op is a strict alsoKnownAs delta over the directory's own
        # state (§ State & data shape: the user can always rotate without us).
        genesis_op = directory.snapshot()[0][1]
        assert update_op["rotationKeys"] == genesis_op["rotationKeys"], (
            "a rename must carry the rotation keys forward verbatim: "
            f"{update_op['rotationKeys']} vs {genesis_op['rotationKeys']}"
        )
        assert update_op["verificationMethods"] == genesis_op["verificationMethods"], (
            "a rename must not touch the repo signing key"
        )

        identity_frames: list[dict] = []
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not identity_frames:
            try:
                message = sub.recv()
            except Exception as exc:  # noqa: BLE001 — surfaced in the assert below
                pytest.fail(f"firehose read failed after the rename: {exc}\nlog:\n{_tail()}")
            header, body = _decode_frame(bytes(message))
            assert header.get("op") != -1, f"error frame from the firehose: {body}"
            if header.get("t") == "#identity":
                identity_frames.append(body)

        assert identity_frames, (
            "a handle change must emit an #identity frame on subscribeRepos so "
            "the network re-resolves the DID; log:\n" + _tail()
        )
        ident = identity_frames[0]
        assert ident["did"] == did, ident
        assert ident.get("handle") == new_atproto_handle, ident
        assert ident["seq"] > commits[-1][1]["seq"], (
            "the #identity frame shares the PDS-global seq line with #commit and "
            f"must follow the commits that preceded it: {ident}"
        )

        first = commits[0][1]
        assert first["repo"] == did, first
        assert first["seq"] >= 1, first
        assert first.get("tooBig") in (None, False), (
            f"Sync v1.1 removed tooBig commit diffs entirely: {first.get('tooBig')}"
        )
        # Sync v1.1 inductive metadata: a repo's FIRST commit has no prior MST
        # root (prevData nil/absent), and every later commit carries one — that
        # chaining is what lets a strict relay validate by MST inversion. Assert
        # the property that actually matters, on a pair, rather than on a key
        # whose nil form may or may not be encoded.
        if len(commits) >= 2:
            second = commits[1][1]
            assert second.get("prevData") is not None, (
                "Sync v1.1 requires inductive #commit metadata: the second "
                f"commit carries no prevData; keys={list(second)}"
            )

        # ── 8. S4-D — LAYER-2 STEP-DOWN. The user steps off the hosted level
        # (set_integration_level → "off"): nest marks the identity deactivated
        # (retaining the DID + sealed keys), and the bridge's next projection pass
        # must UNSERVE the repo and announce #account(active=false)
        # (atproto-pds-bridge.md § Disable & revocation, layer 2). ──
        def _xrpc_status(method: str, query: str = "") -> tuple[int, str | None]:
            """GET returning (http_status, error_name|None) — for a refusal assert."""
            import urllib.error
            import urllib.request
            url = f"{xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
            ctx = ssl._create_unverified_context()
            try:
                with urllib.request.urlopen(url, context=ctx, timeout=15) as resp:
                    return resp.status, None
            except urllib.error.HTTPError as exc:
                try:
                    return exc.code, json.loads(exc.read()).get("error")
                except Exception:  # noqa: BLE001
                    return exc.code, None

        def _await_account_frame(budget_s: float = 120) -> dict:
            """Read the live subscriber until an ``#account`` frame arrives.

            The frame is the CAUSAL signal that the reconcile pass ran, and the
            socket is already open — so this costs no HTTP. Polling the XRPC
            surface for the same news would not: ``ClassPublicRead`` is 60 req /
            60 s per IP and the projection poll backstop is 30 s, so a 1 Hz HTTP
            poll 429s long before the pass fires. A recv timeout is just "nothing
            yet" — keep waiting until the (generous) budget runs out.
            """
            deadline = time.monotonic() + budget_s
            while time.monotonic() < deadline:
                try:
                    message = sub.recv()
                except websocket.WebSocketTimeoutException:
                    assert proc.poll() is None, "bridge exited awaiting #account:\n" + _tail()
                    continue
                except Exception as exc:  # noqa: BLE001 — surfaced as a failure below
                    pytest.fail(f"firehose read failed: {exc}\nlog:\n{_tail()}")
                header, body = _decode_frame(bytes(message))
                assert header.get("op") != -1, f"error frame from the firehose: {body}"
                if header.get("t") == "#account":
                    return body
            pytest.fail("no #account frame reached subscribeRepos; log:\n" + _tail())

        def _await_repo_active(want: bool, tries: int = 8) -> dict:
            """``getRepoStatus`` until ``active == want``.

            Deliberately FEW, SPACED reads: the served-status flag is flipped
            right *after* the frame is emitted (frame-first crash-discipline), so
            a couple of reads absorb that lag — and the anonymous read surface is
            rate-limited, so this must not become a tight poll.
            """
            st_: dict = {}
            for _ in range(tries):
                st_ = _xrpc_get("com.atproto.sync.getRepoStatus", f"did={did}")
                if st_.get("active") is want:
                    return st_
                assert proc.poll() is None, "bridge exited during a level change:\n" + _tail()
                time.sleep(2)
            pytest.fail(
                f"getRepoStatus never reported active={want}; last={st_}\nlog:\n" + _tail()
            )

        with alice_ws:
            alice_ws.call(
                "fauna.bridges.atproto.set_integration_level",
                {
                    "target_level": "off",
                    "did_method": "",
                    "user_rotation_pub_did_key": "",
                    "history_backfill": False,
                },
            )

        # The step-down must announce itself on the firehose: one
        # #account(active=false, status="deactivated").
        down_frame = _await_account_frame()
        assert down_frame["did"] == did, down_frame
        assert down_frame["active"] is False, down_frame
        assert down_frame.get("status") == "deactivated", down_frame

        # …and the repo must stop being served.
        st = _await_repo_active(want=False)
        assert st.get("status") == "deactivated", st
        # Content reads refuse with RepoDeactivated (unserved, no record leak).
        assert _xrpc_status("com.atproto.sync.getRepo", f"did={did}") == (400, "RepoDeactivated"), (
            "a deactivated repo's content reads must refuse with RepoDeactivated"
        )
        # listRepos still lists it, marked inactive with a status reason (Sync v1.1).
        repos = _xrpc_get("com.atproto.sync.listRepos")["repos"]
        assert len(repos) == 1 and repos[0]["did"] == did, repos
        assert repos[0]["active"] is False and repos[0].get("status") == "deactivated", repos[0]

        # ── 9. S4-D — RE-ENTRY reverses all three. Stepping back to the hosted
        # level reactivates the SAME identity (no re-mint): the repo serves again
        # and #account(active=true) is announced. ──
        with alice_ws:
            alice_ws.call(
                "fauna.bridges.atproto.set_integration_level",
                {
                    "target_level": "hosted_visible",
                    "did_method": "plc",
                    "user_rotation_pub_did_key": USER_ROTATION_PUB,
                    "history_backfill": False,
                },
            )

        up_frame = _await_account_frame()
        assert up_frame["did"] == did and up_frame["active"] is True, up_frame
        assert up_frame.get("status") in (None, ""), (
            f"a reactivation #account needs no status reason: {up_frame}"
        )

        _await_repo_active(want=True)
        assert _xrpc_status("com.atproto.sync.getRepo", f"did={did}")[0] == 200, (
            "re-entry must serve the SAME repo again"
        )

        # ── 9b. THE TAKEDOWN RETRACTION — a legally compelled removal must
        # reach Bluesky, not just Fauna (`moderation.md` § Legal takedown → the
        # off-box publish surfaces). The servability predicate only stops
        # FUTURE projection; POST_ONE is already published here, which is
        # exactly the case that matters. Nest-side this is pinned by two unit
        # tests, but only a real admin takedown → real journal witness → real
        # bridge `deleteRecord` proves the cross-binary wire carries it. ──
        def _listed_texts() -> list:
            return [
                r.get("value", {}).get("text")
                for r in _xrpc_get(
                    "com.atproto.repo.listRecords",
                    f"repo={did}&collection=app.bsky.feed.post&limit=100",
                )["records"]
            ]

        before = _listed_texts()
        assert POST_ONE in before, (
            f"precondition: POST_ONE must be published before it can be retracted; got {before}"
        )
        survivors = [t for t in before if t != POST_ONE]
        assert survivors, "precondition: need at least one OTHER record, to prove a targeted retraction"

        takedown_ws = WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        )
        with takedown_ws:
            takedown_ws.call(
                "fauna.moderation.legal_takedown",
                {
                    "content_id": post_one_id,
                    "content_type": "post",
                    "legal_reference": "e2e-court-order-1",
                    "restore": False,
                },
            )

        # Few, SPACED reads for the same two reasons as `_await_repo_active`:
        # the projection loop is asynchronous, and this surface is rate-limited.
        texts: list = before
        for _ in range(10):
            texts = _listed_texts()
            if POST_ONE not in texts:
                break
            assert proc.poll() is None, "bridge exited during the takedown:\n" + _tail()
            time.sleep(3)  # sleep-ok: poll interval of a deadline loop
        assert POST_ONE not in texts, (
            "a legally taken-down post must be RETRACTED from the ATProto repo, not merely "
            f"withheld from future projection; still listed: {texts}\nlog:\n" + _tail()
        )
        # Targeted, not a wipe — the retraction deletes one record, and the
        # rest of the repo keeps serving.
        assert all(s in texts for s in survivors), (
            f"the retraction must delete ONLY the taken-down record; wanted {survivors} to "
            f"survive, got {texts}"
        )

        # ── 10. S5 slice 5 — DELETE MY BLUESKY PRESENCE. The stronger action
        # beside the reversible step-down (atproto-pds-bridge.md § Disable &
        # revocation, layer 2). This is the CROSS-BINARY half the Go tests
        # cannot reach: they drive a fake nest, so only a real
        # `delete_presence` → real roster status → real bridge sweep proves the
        # wire actually carries the tombstone. ──
        with alice_ws:
            alice_ws.call("fauna.bridges.atproto.delete_presence", {})

        # The sweep must announce itself as a DELETION, not a deactivation. A
        # `deleted` identity is inactive too, so a bridge that fell through to
        # the S4-D reconcile would tell the network the reversible thing first —
        # which is why this asserts the status string, not merely active=False.
        gone_frame = _await_account_frame()
        assert gone_frame["did"] == did, gone_frame
        assert gone_frame["active"] is False, gone_frame
        assert gone_frame.get("status") == "deleted", (
            "the sweep's terminal announcement must say deleted, never deactivated: "
            f"{gone_frame}"
        )

        # …and the repo is GONE, not merely unserved. A deactivated repo answers
        # RepoDeactivated and stays listed; a deleted one has no head row at all,
        # so every read is RepoNotFound and listRepos omits it. That difference
        # is the whole distinction between the two actions, on the wire.
        deadline = time.monotonic() + 120
        last: tuple[int, str | None] = (0, None)
        while time.monotonic() < deadline:
            last = _xrpc_status("com.atproto.sync.getRepo", f"did={did}")
            if last == (400, "RepoNotFound"):
                break
            assert proc.poll() is None, "bridge exited during the sweep:\n" + _tail()
            # Spaced deliberately wide: this reads the rate-limited anonymous
            # XRPC surface (ClassPublicRead, 60 req / 60 s per IP), so a tight
            # poll would 429 long before the ≤30 s projection backstop fires.
            # The correctness anchor is the deadline above, never this interval.
            time.sleep(3)  # sleep-ok: poll interval of a deadline loop
        assert last == (400, "RepoNotFound"), (
            f"a deleted repo must answer RepoNotFound, got {last}\nlog:\n{_tail()}"
        )
        assert _xrpc_get("com.atproto.sync.listRepos")["repos"] == [], (
            "a deleted repo must not remain listed — unlike a deactivated one"
        )
    finally:
        if sub is not None:
            try:
                sub.close()
            except Exception:
                pass
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
        dns.close()
        directory.close()
        nest_cleanup()
