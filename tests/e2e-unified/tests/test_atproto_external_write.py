"""tier_3: F2.2's headline — an external ATProto app WRITES into Fauna.

The inverse direction of ``test_atproto_firehose_post.py``. That test proves a
Fauna post reaches the network; this one proves the network reaches Fauna: a raw
XRPC client calls ``com.atproto.repo.createRecord`` against the bridge's
``--xrpc-listen``, and the record becomes a **real Fauna post** — signed by the
account's D10 delegated authoring sub-key, ingested through the same
``ingest_post_core`` a Fauna app's own post takes, authored by the account
itself and visible on the account's own Fauna read face.

Three processes, no mocks on the path under test: raw HTTP XRPC client → the Go
bridge (``--xrpc-listen``) → the nest (``ingest_external_write`` →
reverse-translate → sign with K → ``ingest_post_core``). Only tier_3 catches
drift here: the Rust round-trip arm and the Go write handler are separately
unit-tested, and each is green while disagreeing about the wire between them
(``resolved_targets``' shape, the rkey authority, whether the commit carries the
Fauna post id) — and about the DID, which is exactly what this run found.

Five legs, all green since slice 4d:

1. **A top-level post** (``atproto-pds-full.md`` § F2 detail) — refused while no
   delegation exists, accepted on the SAME session token once one is provisioned
   (so the delegation is provably what unlocks it), landing as a real Fauna post
   authored by the ACCOUNT rather than by the sub-key that signed it, and the
   answered ``uri`` names the account's real ATProto identity.
2. **A reply to that post round-trips as a real Fauna reply** — slice 4b's
   headline, reachable only since 4d (below).
3. **A reply to a NON-Fauna post journals** — accepted, on the repo, but no
   Fauna post manufactured (§ F2 detail's ratified journal ruling). Leg 2 is
   this leg's control: before 4d *everything* journaled, so asserting it alone
   would have passed vacuously.
4. **``getRecord`` serves the record from the account's real repo.**
5. **The commit reaches ``subscribeRepos`` under the real DID** — a relay keys
   every commit by that field, so a frame announced under anything else is
   invisible to the network however well formed it is.

**Why legs 2–5 only became assertable at 4d.** The 4c run found the two binaries
maintaining two different repos for one account. The nest half was already
right: the bridge sends no DID at all
(``wsrpc.IngestExternalWrite(ctx, nest, caller.ActorID, …)``) and the nest
resolves the account's real ``did:plc:``. The bridge half was not — it committed
with ``funnel.ApplyBatch(ctx, caller.DID, …)`` where ``caller.DID`` was F1's
``did:fauna:<hex>`` placeholder, while the projection loop committed under the
real DID. ``ApplyBatch`` keys the repo by that string. So the record landed in a
repo no AppView could resolve, and because ``post_map`` is DID-keyed on both
sides, ``FaunaPostIDForATURI`` never matched and EVERY reply journaled.

Neither side's unit tests could see it — each picked its own DID string
(``repo_write_test.go`` used ``at://did:fauna:x/…``), so they were wrong only
*relative to each other*. That is the class of bug this tier_3 exists for. Slice
4d converged them at the source: ``sub`` is the account's real DID, carried from
the nest's ``login_did``, and the actor id rides its own ``fauna_actor`` claim
instead of being hex-decoded back out of a placeholder DID. The relative form of
that invariant is pinned at unit level too, by
``repo_write_test.go::TestTheCommittedRepoIsTheRepoTheNestAnswered`` — which
asserts the two sides AGREE rather than pinning each to its own literal, the
mistake that hid this for two slices.

Provisioning notes. The app credential rides ``provision_app_credential`` and the
delegation rides ``fetch_authoring_key`` + ``provision_authoring_delegation`` —
fixture setup arranging preconditions (e2e conventions point 8), not the behavior
under test, which is the external-protocol XRPC write and by construction has no
Fauna app UI. The cert is minted by the SAME shared-Rust minter all 7 apps
will use (``fauna_client_bridges::atproto_delegation``), so nothing about the
authorization on this path is a test-only reimplementation. The client-UI mint
surface is slice 5; when it lands, a client-UI-driven mint test follows (rule 8
carve-out).
"""

import json
import os
import ssl
import subprocess
import time

import pytest

from helpers.atproto_fakes import FakeDNS, FakePlcDirectory
from helpers.atproto_firehose import (
    car_records as _car_records,
    decode_frame as _decode_frame,
)
from helpers.xrpc_client import (
    load_app_credential_fixture as _load_fixture,
    put_blob as _put_blob,
    xrpc_post as _xrpc_post,
    xrpc_post_bytes as _xrpc_post_bytes,
)

pytestmark = pytest.mark.tier_3

USER_ROTATION_PUB = "did:key:zDnaembgSGUhZULN2Caob4HLJPaxBh92N7rtH21TErzqf8HQo"  # gitleaks:allow
HANDLE_DOMAIN = "fauna.test"

POST_TEXT = "posted from a third-party atproto app, into fauna"
REPLY_TEXT = "replying to that post from the same third-party app"
JOURNAL_TEXT = "replying to somebody who is not on fauna at all"
# Leg 6 (F2.3): what the external app sets on the profile singleton.
PROFILE_NAME = "Alice, edited from a third-party app"
PROFILE_BIO = "this bio was written in bsky.app, not in a fauna client"
# Leg 6b (F2.4 slice 3): a Fauna-set avatar the external bio edit never
# mentions. The PNG signature is enough for Go's sniffer (a prefix match, not
# full decodability) — the same shape `test_atproto_firehose_post.py`'s
# AVATAR_BYTES already proves sniffs as image/png.
SEEDED_AVATAR_BYTES = b"\x89PNG\r\n\x1a\nfauna-external-write-test-avatar"
# Leg 6c (the inbound-picture slice): the picture the EXTERNAL app uploads and
# sets. Deliberately a different length from the seeded one, so "the picture
# changed" is observable by size alone rather than only by CID.
EXTERNAL_AVATAR_BYTES = (
    b"\x89PNG\r\n\x1a\nfauna-external-write-test-avatar-set-by-the-external-app"
)
# Leg 7 (F2.3 piece 3): the compare-and-swap trio. CAS_REFUSED_TEXT is the one
# that must exist NOWHERE — the whole point of refusing before the nest call.
CAS_ANCHOR_TEXT = "a write whose commit cid becomes the next write's swapCommit"
CAS_REFUSED_TEXT = "this write pinned a stale head and must never exist"
CAS_SERVED_TEXT = "this write pinned the current head and must land"
# Leg 8 (F2.3 piece 2): the two records of one batch — same commit, same frame.
BATCH_TEXT_A = "first of a batch, written by an external app"
BATCH_TEXT_B = "second of a batch, in the very same commit"
# Leg 9 (F2.4 slice 2): the media round-trip.
MEDIA_TEXT = "an image post from a third-party app, media and all"
# A complete, genuinely decodable 1x1 PNG — the upload leg SNIFFS the bytes
# (never the declared Content-Type), so the fixture must really be a PNG.
TINY_PNG = bytes.fromhex(
    "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c489"
    "0000000d49444154789c6260f8cfc0000000030001a2b0bb2f0000000049454e44ae426082"
)

# A real CIDv1 — `strongRef.cid` is `format: cid`, so it is PARSED, and an
# invented base32 literal fails with "Failed to parse multihash". Only the
# non-Fauna target needs a hand-written one; the Fauna target reuses the cid
# the PDS itself answered.
FOREIGN_CID = "bafkreiaha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4"

@pytest.mark.feature("atproto")
def test_external_app_write_becomes_a_fauna_post(
    bluesky_nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
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
    from fauna_ffi import (
        build_authoring_delegation_cert,
        build_profile_with_pictures,
        content_cid,
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

    directory = FakePlcDirectory()
    dns = FakeDNS()
    # `bluesky_nest_binary`, not `nest_binary`: the external-write kind is
    # `#[cfg(feature = "bluesky")]`-gated (see the fixture's docstring).
    nest, nest_cleanup = _make_nest(
        bluesky_nest_binary, tmp_path_factory, "atproto-extwrite-nest",
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
    try:
        admin = nest["admin"]

        # ── 1. Alice enters the hosted level; the bridge boots with the XRPC +
        # firehose listener up. ──
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        alice_secret = bytes(alice["signing_key"])
        alice_ws = WsRpcAdminClient(
            nest["url"], actor_id=alice["actor_id_bytes"], signing_key=alice_secret
        )
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
            assert alice_ws.call(
                "fauna.bridges.atproto.provision_app_credential",
                {"credential_id": "ivory", "label": "Ivory", "verifier": verifier,
                 "dm_allowed": False},
            ).get("ok") is True

        tmp = tmp_path_factory.mktemp("atproto-extwrite-bridge")
        keyfile_path = tmp / "atproto.pds.key"
        xrpc_port = find_free_port()
        xrpc_base = f"https://127.0.0.1:{xrpc_port}"
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

        def _xrpc_get(method: str, query: str = "") -> dict:
            import urllib.request
            url = f"{xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
            ctx = ssl._create_unverified_context()
            with urllib.request.urlopen(url, context=ctx, timeout=15) as resp:
                return json.loads(resp.read())

        def _xrpc_get_status(method: str, query: str = "") -> tuple[int, bytes]:
            """GET a route whose body is not JSON (``sync.getBlob`` serves image
            bytes), answering the status and the raw body.

            An error status is a value here, not an exception: the callers ask
            precisely whether a blob is still served.
            """
            import urllib.error
            import urllib.request
            url = f"{xrpc_base}/xrpc/{method}" + (f"?{query}" if query else "")
            ctx = ssl._create_unverified_context()
            try:
                with urllib.request.urlopen(url, context=ctx, timeout=15) as resp:
                    return resp.status, resp.read()
            except urllib.error.HTTPError as e:
                return e.code, e.read()

        # ── 2. Admin pins the domain and approves the bridge. ──
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
                if any(bytes(r["ed25519_pubkey"]).hex() == pubkey_hex for r in rows):
                    break
                assert proc.poll() is None, "bridge exited pre-enroll:\n" + _tail()
                time.sleep(0.5)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
            else:
                pytest.fail("atproto.pds bridge never appeared PENDING:\n" + _tail())

        approve_bridge(
            nest["url"], admin["signing_key"], bytes.fromhex(pubkey_hex), "atproto.pds",
        )

        # ── 3. The bridge mints alice's DID. The `_atproto` TXT is deliberately
        # NOT published yet: until it is, alice's identity does not resolve
        # ecosystem-side and her first-emit gate stays closed, which is the
        # window leg 0 below drives. ──
        deadline = time.monotonic() + 60
        did = None
        while time.monotonic() < deadline:
            subs = directory.snapshot()
            if subs:
                did = subs[0][0]
                break
            assert proc.poll() is None, "bridge exited before minting:\n" + _tail()
            time.sleep(0.5)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
        assert did is not None, "the bridge must mint a DID:\n" + _tail()

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if "xrpc listener up" in log_path.read_text(errors="replace"):
                break
            assert proc.poll() is None, "bridge exited before serving XRPC:\n" + _tail()
            time.sleep(0.5)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
        else:
            pytest.fail("XRPC listener never came up:\n" + _tail())

        # ── 3b. LEG 0 — SLICE 4e: A WRITE BEFORE THE IDENTITY RESOLVES IS
        # REFUSED, so an external app can never put the account's FIRST firehose
        # event on the network unresolvable (`atproto-pds-full.md` § Ecosystem
        # reality: the first-impression trap — a DID that fails to resolve at
        # first AppView index permanently 404s, because later events only UPDATE
        # an actor row that was never created).
        #
        # Before 4e this ordering was merely ARRANGED by this test: the gate was
        # consulted only by the projection loop while an external write committed
        # straight through the funnel. It is now enforced on the write path, and
        # the arrangement is gone — the DNS record below is published because the
        # PDS demands it, not because the test tiptoes around a gap.
        #
        # Note this refusal precedes the no-delegation one: the gate is decided
        # bridge-side, before the nest is asked to ingest anything, which is what
        # keeps a refused write from leaving a real Fauna post behind. ──
        access, session_did = _create_session(xrpc_base, secret, _tail)
        status, err = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord",
            _create_body(session_did, POST_TEXT), access,
        )
        assert status >= 400, (
            "a write before the account's identity resolves must be refused — a "
            f"200 here puts an unresolvable first event on the firehose: {err}"
        )
        assert "resolvable" in json.dumps(err).lower(), (
            "the refusal must say the identity is not resolvable yet, so the "
            f"account owner knows what to finish; got {err}"
        )

        # ── 3c. Publish the `_atproto` TXT and WAIT for the projection loop to
        # verify it and open the gate. The wait is on the loop's own log line
        # (observable state, convention 14), never a settle-sleep: the budget is
        # sized well above the 30 s poll interval so a loaded box does not fail a
        # correct run. ──
        dns.publish(f"_atproto.alice.{HANDLE_DOMAIN}", [f"did={did}"])
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            if "first-emit gate cleared" in log_path.read_text(errors="replace"):
                break
            assert proc.poll() is None, "bridge exited before clearing the gate:\n" + _tail()
            time.sleep(1.0)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep
        else:
            pytest.fail(
                "the projection loop never cleared alice's first-emit gate, so "
                "every write below would refuse:\n" + _tail()
            )

        # ── 4. THE D10 MINT CEREMONY (atproto-pds-full.md § D10 → Mint
        # ceremony). Fetch K (mint-on-read), sign a cert naming that exact
        # k_pub under alice's IDENTITY key, provision it. Without this the
        # write path refuses `fauna_surface` by design — which the negative
        # assertion below pins BEFORE we provision, so the delegation is
        # provably what unlocks the write rather than something incidental.
        #
        # The gate having just opened is what makes THIS refusal attributable to
        # the delegation: the SAME session token is refused twice for two
        # different reasons and then succeeds, so neither refusal can be passing
        # vacuously for the other's cause. ──
        with alice_ws:
            status, err = _xrpc_post(
                xrpc_base, "com.atproto.repo.createRecord",
                _create_body(session_did, POST_TEXT), access,
            )
            assert status >= 400, (
                "an account with NO authoring delegation must be refused — a "
                f"200 here means the write path signs without authorization: {err}"
            )
            assert "fauna" in json.dumps(err).lower(), (
                "the no-delegation refusal must name the fauna surface so the "
                f"app knows where to authorize; got {err}"
            )
            # …and it is provably NOT leg 0's gate refusal wearing the same
            # status code: the gate is open by now, so this refusal has only one
            # remaining cause.
            assert "resolvable" not in json.dumps(err).lower(), (
                "this must be the no-delegation refusal, but it still reads as "
                f"the first-emit gate — the gate never opened: {err}"
            )

            k_pub = bytes(
                alice_ws.call("fauna.bridges.atproto.fetch_authoring_key", {})["k_pub"]
            )
            assert len(k_pub) == 32, k_pub
            # MICROSECONDS — `Timestamp`'s unit. The gate that re-verifies this
            # cert on every delegated post compares `expires_at` against the
            # post's own microsecond `created_at`, so a millisecond cert reads
            # as long expired and every write it authorizes is rejected.
            #
            # DELIBERATELY Post-only (narrower than the real client ceremony's
            # `AUTHORING_CAPABILITIES` = [Post, UpdateProfile]) — provisioning
            # accepts any subset, and leg 6a below proves cross-binary that a
            # Post-only cert cannot rewrite the profile; leg 6b then
            # re-provisions the full set, which also pins renewal-by-overwrite
            # (no revoke first) across the wire.
            now_micros = int(time.time() * 1_000_000)
            cert = build_authoring_delegation_cert(
                alice_secret, k_pub, ["Post"], created_at_micros=now_micros,
                expires_at_micros=now_micros + 3_600_000_000,  # +1 h
            )
            assert alice_ws.call(
                "fauna.bridges.atproto.provision_authoring_delegation",
                {"cert": cert},
            ).get("ok") is True

        # ── 5. LEG 1 — a top-level post over raw XRPC. The SAME session token
        # that was refused a moment ago now succeeds, so the delegation is
        # provably what unlocked the write. ──
        status, created = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord",
            _create_body(session_did, POST_TEXT), access,
        )
        assert status == 200, f"createRecord failed: {status} {created}\n{_tail()}"
        post_uri, post_cid = created["uri"], created["cid"]
        # "valid" since 2026-08-02: lexicon-schema validation is served against
        # the vendored catalog (`bins/fauna-bridges/internal/atprotolex`), and
        # `app.bsky.feed.post` is a catalog lexicon — the old "unknown, no
        # schema was resolved" premise is retired with the D2 deferred arms.
        assert created.get("validationStatus") == "valid", created
        # The AT-URI the nest answers names the account's REAL `did:plc:`
        # identity — the nest resolves it from the actor id (the bridge sends no
        # DID at all, `wsrpc.IngestExternalWrite(ctx, nest, caller.ActorID, …)`).
        # This half was already correct before 4d; it is the half the session's
        # `sub` now agrees with, which the next assertion states directly.
        assert post_uri.startswith(f"at://{did}/app.bsky.feed.post/"), (
            f"the answered AT-URI must name the account's real ATProto identity "
            f"({did}); got {post_uri}"
        )
        # …and the session the app holds asserts that same identity. Both halves
        # naming one DID is the whole of slice 4d; everything from leg 2 down is
        # a consequence of it.
        assert session_did == did, (
            f"the session names {session_did} while the account's identity is "
            f"{did} — the two halves are writing different repos (slice 4d)"
        )

        # ── 6. THE FAUNA READ FACE. The post must be a real Fauna post on
        # alice's own feed, authored by ALICE (not by the sub-key that signed
        # it) — the D10 chain resolving to the identity is the whole point. ──
        with alice_ws:
            item = _await_feed_item(alice_ws, POST_TEXT, _tail)
        assert item["author"] == alice["actor_id_bytes"].hex(), (
            "a delegated post must be authored by the ACCOUNT, not by the "
            f"authoring sub-key that signed it: {item}"
        )
        assert item["is_reply"] is False, item
        fauna_post_id = item["post_id"]

        # ── 7. LEG 2 — A REPLY TO THAT POST ROUND-TRIPS AS A REAL FAUNA REPLY.
        # This is slice 4b's headline, and it is reachable only now: `post_map`
        # is DID-keyed on both sides, so while the bridge wrote its row under
        # the `did:fauna:` placeholder and a reply's `strongRef` named the real
        # `did:plc:`, `FaunaPostIDForATURI` found nothing and EVERY reply
        # journaled. The reply resolving is therefore also the sharpest proof
        # that the two halves now agree on one repo. ──
        status, replied = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord",
            _create_body(session_did, REPLY_TEXT, parent=(post_uri, post_cid)),
            access,
        )
        assert status == 200, f"reply createRecord failed: {status} {replied}\n{_tail()}"
        assert replied["uri"].startswith(f"at://{did}/app.bsky.feed.post/"), replied

        with alice_ws:
            reply_item = _await_feed_item(alice_ws, REPLY_TEXT, _tail)
        assert reply_item["is_reply"] is True, (
            "the reply must land as a real Fauna reference, not a standalone post — "
            "a False here is the DID-keyed post_map miss silently journaling "
            f"(slice 4d): {reply_item}"
        )
        assert reply_item["author"] == alice["actor_id_bytes"].hex(), reply_item

        # `is_reply` alone only says "some reference resolved". The parent's own
        # reply_count is what proves it resolved to THIS post: a
        # `Reference::Reply` inserts an engagement event keyed by (referencing,
        # target) and bumps the target's counter (`db/posts.rs`), so a wrong or
        # missing target leaves the parent at 0.
        with alice_ws:
            parent = _await_feed_item(
                alice_ws, POST_TEXT, _tail, until=lambda p: p["reply_count"] >= 1
            )
        assert parent["post_id"] == fauna_post_id, parent
        assert parent["reply_count"] == 1, (
            "the parent post's reply_count must be 1 — the reply resolved to some "
            f"other target, or to none at all: {parent}"
        )

        # ── 8. LEG 3 — A REPLY TO A NON-FAUNA POST JOURNALS. The ratified
        # answer (§ F2 detail: journal, never refuse, never flatten), and most
        # of the network is not Fauna, so this is the ORDINARY path.
        #
        # Leg 2 is this leg's control. Before 4d everything journaled, so
        # asserting "this journaled" would have passed vacuously — which is
        # exactly why the two legs move together. ──
        # A well-formed did:plc (24 base32-ish chars) that is simply not this
        # nest's — the shape a real third-party target has, so nothing can pass
        # for the wrong reason (a malformed DID rejected as a parse error would
        # look like the journal path from the outside).
        foreign = "at://did:plc:z72i7hdynmk6r22z27h6tvur/app.bsky.feed.post/3kfor31gnrkey"
        status, journaled = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord",
            _create_body(session_did, JOURNAL_TEXT, parent=(foreign, FOREIGN_CID)),
            access,
        )
        assert status == 200, (
            "a reply to a non-Fauna post must be ACCEPTED and journaled — refusing "
            "would make the account's PDS reject the network's commonest operation: "
            f"{status} {journaled}\n{_tail()}"
        )
        assert journaled["uri"].startswith(f"at://{did}/app.bsky.feed.post/"), journaled

        # It reached the repo (so it appears in its thread on the network), but
        # manufactured no Fauna post — flattening it into one would publish the
        # user's words to their Fauna followers stripped of the conversation.
        with alice_ws:
            bodies = [
                p["body"]
                for p in alice_ws.call("fauna.feed.local.posts", {"limit": 100})["posts"]
            ]
        assert JOURNAL_TEXT not in bodies, (
            "a reply whose target is not a Fauna post must NOT become a Fauna post "
            f"(that is the flattening the journal ruling forbids); feed held {bodies}"
        )

        # ── 9. LEG 4 — THE RECORD IS SERVED FROM THE ACCOUNT'S REAL REPO.
        # The direct consequence of convergence: before 4d the record landed in
        # a `did:fauna:` repo no AppView could resolve, and the bridge's own
        # self-check logged "no repo for did did:plc:…". ──
        got = _xrpc_get(
            "com.atproto.repo.getRecord",
            f"repo={did}&collection=app.bsky.feed.post&rkey={post_uri.rsplit('/', 1)[-1]}",
        )
        assert got.get("uri") == post_uri, f"getRecord answered {got}"
        assert got.get("value", {}).get("text") == POST_TEXT, got
        assert "no repo for did" not in _tail(), (
            "the bridge's own repo self-check reports a missing repo for the "
            f"account's real DID — the two halves are writing different repos:\n{_tail()}"
        )

        # ── 10. LEG 5 — THE COMMIT IS ON THE FIREHOSE, UNDER THE REAL DID.
        # A relay keys everything by the `repo` field, so a commit announced
        # under a placeholder is invisible to the network no matter how well
        # formed it is. ──
        commit_repos, commit_texts = _await_commit_for(xrpc_port, POST_TEXT, _tail)
        assert commit_repos, "no #commit frame reached subscribeRepos; log:\n" + _tail()
        assert commit_repos == {did}, (
            f"the firehose announced commits under {commit_repos}, not the account's "
            f"real identity ({did}) — a relay would key them to a DID that resolves "
            "to nothing"
        )
        assert POST_TEXT in commit_texts, (
            f"the post never reached the firehose; saw {commit_texts}\n{_tail()}"
        )

        # ── 11. LEG 6 — A PROFILE `putRecord` ROUND-TRIPS INTO THE FAUNA
        # PROFILE (F2.3). The mutable-singleton half of the write surface:
        # `putRecord` on `app.bsky.actor.profile` at rkey `self` takes the C2
        # sanctioned update path (`atproto-pds-full.md` § F2 detail), reverse-
        # translating into a real Fauna profile update signed by the same D10
        # sub-key under `Capability::UpdateProfile`.
        #
        # Only tier_3 can prove the load-bearing half: what the repo carries is
        # the NEST's rendering of the merged profile, not the caller's record.
        # Both binaries pass their own unit tests either way — the disagreement
        # is only visible when `getRecord` is asked what actually landed. ──
        # ── 10b. LEG 6a — CAPABILITY SCOPING, CROSS-BINARY. The provisioned
        # cert is deliberately Post-only, so this putRecord must be REFUSED
        # with the re-authorize remedy — a refusal, never an internal error.
        # (Discovered as a live 500 on 2026-07-29: the capability miss used to
        # surface as the chain verify's opaque step-4 rejection.) ──
        profile_record_body = {
            "repo": session_did,
            "collection": "app.bsky.actor.profile",
            "rkey": "self",
            "record": {
                "$type": "app.bsky.actor.profile",
                "displayName": PROFILE_NAME,
                "description": PROFILE_BIO,
            },
        }
        status, err = _xrpc_post(
            xrpc_base, "com.atproto.repo.putRecord", profile_record_body, access,
        )
        assert 400 <= status < 500, (
            "a Post-only cert must REFUSE a profile write (not error, not "
            f"serve): {status} {err}\n{_tail()}"
        )
        assert "re-authorize" in json.dumps(err).lower(), (
            f"the refusal must name the re-authorize remedy; got {err}"
        )

        # Re-provision with the full authoring set — the same overwrite the
        # real ceremony's renewal performs; no revoke call precedes it.
        with alice_ws:
            now_micros = int(time.time() * 1_000_000)
            cert = build_authoring_delegation_cert(
                alice_secret, k_pub, ["Post", "UpdateProfile"],
                created_at_micros=now_micros,
                expires_at_micros=now_micros + 3_600_000_000,  # +1 h
            )
            assert alice_ws.call(
                "fauna.bridges.atproto.provision_authoring_delegation",
                {"cert": cert},
            ).get("ok") is True

        # ── 10c. LEG 6b — A PICTURED PROFILE'S EXTERNAL BIO EDIT (F2.4 slice
        # 3): the ONE inch of slice 3's mechanism no unit test can reach. Both
        # `TestRenderProfileRecordIsExactlyWhatAProjectionPassCommits` (Go) and
        # `a_profile_update_asks_the_projection_to_render_the_record` (nest)
        # pin the render function in isolation; only tier_3 proves the bridge
        # actually calls it with the account's REAL, currently-stored blob
        # data over live WS-RPC + the blob store, not a fixture standing in
        # for either. Alice sets an avatar directly (fixture setup arranging
        # a precondition, e2e conventions point 8 — the behavior under test
        # stays the external XRPC write below), THEN the external app edits
        # only her bio: the avatar sits outside `putRecord`'s translatable
        # scope (§ F2 detail), so it must survive untouched. ──
        seeded_avatar_cid = content_cid(SEEDED_AVATAR_BYTES)
        _put_blob(nest["url"], alice["token"], seeded_avatar_cid, SEEDED_AVATAR_BYTES)
        with alice_ws:
            alice_ws.call(
                "fauna.profile.set",
                {
                    "body": build_profile_with_pictures(
                        alice_secret,
                        display_name="Alice (seeded, pre-external-edit)",
                        avatar_cid=seeded_avatar_cid,
                    )
                },
            )

        before_profile = _fauna_profile(alice_ws, alice["actor_id_bytes"])
        assert before_profile.get("avatar"), (
            "the seeded avatar never reached the Fauna profile; the leg below "
            f"would pass vacuously: {before_profile}"
        )

        # The external app does what a real one does: read the current record,
        # then write it back with the bio changed and the picture ECHOED. That
        # echo is the inbound-picture design's whole point — the ref names bytes
        # only the BRIDGE's blob store can identify as Fauna content, so the
        # write only succeeds because the bridge resolved it and vouched. A
        # caller-trusting implementation and a resolving one are indistinguishable
        # until you drop the vouch, which leg 6d below does.
        # Deadline poll, not a settle-sleep (convention 14): the seeded avatar
        # reaches the REPO record only once a projection pass has fetched,
        # sniffed and stored the bytes, which is asynchronous to the
        # `fauna.profile.set` above. A green run exits on the first pass; the
        # budget is sized far above any non-pathological projection cadence.
        # `_xrpc_get` raises on a non-200, and until the first projection pass
        # commits the profile singleton `getRecord` answers RecordNotFound (400)
        # — a legitimate "not yet", not a failure. So poll the status-returning
        # variant and read the body only once it is really there.
        current_record: dict = {}
        echoed_avatar = None
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            rec_status, rec_body = _xrpc_get_status(
                "com.atproto.repo.getRecord",
                f"repo={did}&collection=app.bsky.actor.profile&rkey=self",
            )
            if rec_status == 200:
                current_record = json.loads(rec_body)
                echoed_avatar = current_record.get("value", {}).get("avatar")
                if echoed_avatar:
                    break
            assert proc.poll() is None, "bridge exited awaiting the avatar projection:\n" + _tail()
            time.sleep(1.0)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep
        assert echoed_avatar, (
            "the seeded avatar never rendered into the repo record, so the echo "
            f"below would be vacuous: {current_record}\n{_tail()}"
        )

        status, put = _xrpc_post(
            xrpc_base, "com.atproto.repo.putRecord",
            {
                "repo": session_did,
                "collection": "app.bsky.actor.profile",
                "rkey": "self",
                "record": {
                    "$type": "app.bsky.actor.profile",
                    "displayName": PROFILE_NAME,
                    "description": PROFILE_BIO,
                    "avatar": echoed_avatar,
                },
            },
            access,
        )
        assert status == 200, f"profile putRecord failed: {status} {put}\n{_tail()}"
        assert put["uri"] == f"at://{did}/app.bsky.actor.profile/self", (
            "the profile singleton lands at the spec rkey `self` under the "
            f"account's real DID; got {put}"
        )

        # Read-your-writes, and the override proof in one assertion: `getRecord`
        # serves what the funnel actually committed, so its cid agreeing with
        # the write's own answer is what says the answered cid names bytes that
        # really landed. Committing the caller's record while answering the
        # projected one (or the reverse) breaks exactly this equality.
        got_profile = _xrpc_get(
            "com.atproto.repo.getRecord",
            f"repo={did}&collection=app.bsky.actor.profile&rkey=self",
        )
        assert got_profile.get("cid") == put["cid"], (
            "the cid answered by putRecord does not name the record the repo "
            f"serves — read-your-writes is broken: put={put} got={got_profile}"
        )
        assert got_profile.get("value", {}).get("displayName") == PROFILE_NAME, got_profile

        # LEG 6b's own payoff: the echoed picture survived — and survived by
        # RESOLUTION, not by the nest taking the caller's word. It is also still
        # rendered by the BRIDGE's own blob store, sniffed and sized there — the
        # four-axis drift (CID, MIME, size, publishable-at-all) a nest-side
        # renderer could never keep in step, per the goal doc's slice-3 ruling.
        # The cid-agreement assert above already covers this record
        # byte-for-byte; these name what would have drifted first if it hadn't.
        avatar_ref = got_profile.get("value", {}).get("avatar")
        assert avatar_ref, (
            "the echoed avatar did not survive the external bio edit — either "
            "the bridge failed to resolve it or the merge dropped it: "
            f"{got_profile}"
        )
        assert avatar_ref.get("mimeType") == "image/png", (
            f"the media type must be SNIFFED from the bytes: {avatar_ref}"
        )
        assert avatar_ref.get("size") == len(SEEDED_AVATAR_BYTES), avatar_ref

        # The Fauna side really updated — this is a ROUND-TRIP, not a journal.
        after_profile = _fauna_profile(alice_ws, alice["actor_id_bytes"])
        assert after_profile.get("display_name") == PROFILE_NAME, (
            "the external app's profile edit did not reach the Fauna profile; "
            f"a journal would look exactly like this: {after_profile}"
        )
        assert after_profile.get("bio") == PROFILE_BIO, after_profile

        # And it MERGED rather than replaced. `avatar` is deliberately IN this
        # loop and not in `volatile`: the echoed ref resolved back to the very
        # ContentHash the profile already held, so content-addressing makes the
        # unchanged case a no-op with no special arm — which is exactly the
        # design's claim, asserted rather than assumed. Every other key is a
        # field ATProto's record cannot express at all. The full
        # merge-preservation pin — every field surviving over a genuinely rich
        # stored profile with links/nests/etc — stays the nest unit test
        # `a_profile_update_preserves_every_field_atproto_cannot_express`;
        # seeding those fields from here would mean re-implementing more of
        # the signed Profile wire in Python than `build_profile_with_pictures`
        # already covers, a duplication this harness deliberately avoids.
        volatile = {"display_name", "bio", "updated_at"}
        for key, was in before_profile.items():
            if key in volatile:
                continue
            assert after_profile.get(key) == was, (
                f"the external profile edit changed {key!r}: {was!r} -> "
                f"{after_profile.get(key)!r}"
            )

        # ── 10d. LEG 6c — AN EXTERNAL APP SETS A PICTURE FROM ITS OWN UPLOAD.
        # The other resolver: bytes this account uploaded through THIS PDS, so
        # the nest's own `atproto_blobs` ledger answers and no bridge vouch is
        # involved. Only tier_3 proves the two halves compose — upload leg,
        # ledger row, pre-flight stamp, merge, and the projection re-rendering
        # the new picture into the repo, across two binaries. ──
        status, uploaded_avatar = _xrpc_post_bytes(
            xrpc_base, "com.atproto.repo.uploadBlob",
            EXTERNAL_AVATAR_BYTES, "application/octet-stream", access,
        )
        assert status == 200, (
            f"avatar uploadBlob failed: {status} {uploaded_avatar}\n{_tail()}"
        )
        new_avatar = uploaded_avatar.get("blob") or {}
        assert new_avatar.get("mimeType") == "image/png", uploaded_avatar
        assert new_avatar["ref"]["$link"] != echoed_avatar["ref"]["$link"], (
            "the fixture must upload a DIFFERENT image than the seeded one, or "
            "the change below is indistinguishable from the echo above"
        )

        status, put = _xrpc_post(
            xrpc_base, "com.atproto.repo.putRecord",
            {
                "repo": session_did,
                "collection": "app.bsky.actor.profile",
                "rkey": "self",
                "record": {
                    "$type": "app.bsky.actor.profile",
                    "displayName": PROFILE_NAME,
                    "description": PROFILE_BIO,
                    "avatar": new_avatar,
                },
            },
            access,
        )
        assert status == 200, f"picture-setting putRecord failed: {status} {put}\n{_tail()}"

        changed_profile = _fauna_profile(alice_ws, alice["actor_id_bytes"])
        assert changed_profile.get("avatar"), changed_profile
        assert changed_profile["avatar"] != after_profile["avatar"], (
            "the external app's uploaded avatar never reached the Fauna profile "
            f"— the picture is still the seeded one: {changed_profile}"
        )

        # And it round-trips back out: the projection re-renders the NEW picture
        # into the repo record, so what bsky.app reads is what the app set.
        served_profile = _xrpc_get(
            "com.atproto.repo.getRecord",
            f"repo={did}&collection=app.bsky.actor.profile&rkey=self",
        )
        served_avatar = served_profile.get("value", {}).get("avatar") or {}
        assert served_avatar.get("size") == len(EXTERNAL_AVATAR_BYTES), (
            f"the repo record still serves the old picture: {served_profile}"
        )

        # ── 10e. LEG 6d — OMISSION CLEARS, AND THE BYTES SURVIVE. Absence is
        # authoritative exactly like `displayName` (§ F2 detail): a record with
        # no `avatar` clears the user's picture, because otherwise a picture set
        # anywhere would be unremovable from bsky.app. Nothing is destroyed —
        # the blob is still served, so any Fauna app can set it again. ──
        status, put = _xrpc_post(
            xrpc_base, "com.atproto.repo.putRecord",
            {
                "repo": session_did,
                "collection": "app.bsky.actor.profile",
                "rkey": "self",
                "record": {
                    "$type": "app.bsky.actor.profile",
                    "displayName": PROFILE_NAME,
                    "description": PROFILE_BIO,
                },
            },
            access,
        )
        assert status == 200, f"picture-clearing putRecord failed: {status} {put}\n{_tail()}"

        cleared_profile = _fauna_profile(alice_ws, alice["actor_id_bytes"])
        assert not cleared_profile.get("avatar"), (
            "omitting the avatar must CLEAR it — preserving it would make a "
            f"picture unremovable from every external app: {cleared_profile}"
        )
        served_profile = _xrpc_get(
            "com.atproto.repo.getRecord",
            f"repo={did}&collection=app.bsky.actor.profile&rkey=self",
        )
        assert not served_profile.get("value", {}).get("avatar"), (
            f"the cleared picture is still in the repo record: {served_profile}"
        )
        # The bytes outlive the reference: getBlob still serves them.
        blob_status, _ = _xrpc_get_status(
            "com.atproto.sync.getBlob",
            f"did={did}&cid={new_avatar['ref']['$link']}",
        )
        assert blob_status == 200, (
            "clearing a picture destroyed its bytes — clearing must be a "
            f"reference change only (got {blob_status})"
        )

        # ── 10f. LEG 6e — A PICTURE NEITHER RESOLVER KNOWS REFUSES. The echo is
        # never trusted: a ref this PDS does not serve would commit a dangling
        # reference forever, so the whole write refuses before anything applies.
        # The mutation-check for legs 6b/6c — without it, a nest that simply
        # believed the caller would pass both. ──
        status, refused_picture = _xrpc_post(
            xrpc_base, "com.atproto.repo.putRecord",
            {
                "repo": session_did,
                "collection": "app.bsky.actor.profile",
                "rkey": "self",
                "record": {
                    "$type": "app.bsky.actor.profile",
                    "displayName": PROFILE_NAME,
                    "avatar": {
                        "$type": "blob",
                        "ref": {"$link": FOREIGN_CID},
                        "mimeType": "image/png",
                        "size": 12,
                    },
                },
            },
            access,
        )
        assert status == 400, (
            "a picture ref neither resolver knows must refuse, not dangle: "
            f"{status} {refused_picture}\n{_tail()}"
        )
        assert not _fauna_profile(alice_ws, alice["actor_id_bytes"]).get("avatar"), (
            "the refused write still changed the profile — a refusal must "
            "precede every effect"
        )

        # ── 12. LEG 7 — COMPARE AND SWAP IS HONOURED (F2.3 piece 3). Only
        # tier_3 can prove this at all: the CAS operand is the repo head the
        # BRIDGE owns, while the write it guards is applied by the NEST — the
        # two halves are separately unit-tested against fakes that cannot
        # disagree with them.
        #
        # The head comes from a write's own `commit.cid`, which is how a real
        # app tracks it (this PDS serves no getLatestCommit). ──
        status, anchor = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord",
            _create_body(session_did, CAS_ANCHOR_TEXT), access,
        )
        assert status == 200, f"CAS anchor write failed: {status} {anchor}\n{_tail()}"
        current_commit = (anchor.get("commit") or {}).get("cid")
        assert current_commit, f"createRecord answered no commit ref: {anchor}"

        stale_body = _create_body(session_did, CAS_REFUSED_TEXT)
        stale_body["swapCommit"] = FOREIGN_CID
        status, err = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord", stale_body, access,
        )
        assert status == 400, (
            f"a stale swapCommit must refuse 400 InvalidSwap; got {status} {err}\n{_tail()}"
        )
        assert err.get("error") == "InvalidSwap", (
            "third-party clients branch on the error NAME to decide whether to "
            f"re-read and retry; got {err}"
        )

        # The SAME write naming the current head is served. This is both the
        # control that says the comparison is real rather than the parameter
        # being ignored, AND the causal barrier for the negative assert below:
        # it travels the identical bridge→nest→ingest path, so once IT is on
        # the feed, a refused write that had reached the nest would be too.
        #
        # The projection loop is a DOCUMENTED concurrent committer for this
        # account (§ F2 detail's CAS bullet — the funnel-side check firing is
        # "a genuine race", and the caller's remedy is to re-read and retry):
        # its pass can land inside this write's own bridge→nest round trip —
        # the nest has created the Fauna post, the bridge has not yet
        # committed, so no post_map row exists and the reconciler projects it
        # first, moving the head between the pre-check and the funnel's
        # under-lock re-check. So the control arm retries with a FRESH anchor
        # on each InvalidSwap — bounded, driven by the observable error name,
        # never a sleep (convention 14). The property proven is unchanged: a
        # swapCommit naming the current head is served.
        for attempt in range(3):
            ok_body = _create_body(session_did, CAS_SERVED_TEXT)
            ok_body["swapCommit"] = current_commit
            status, created = _xrpc_post(
                xrpc_base, "com.atproto.repo.createRecord", ok_body, access,
            )
            if status == 200:
                break
            if status == 400 and created.get("error") == "InvalidSwap":
                status_a, anchor = _xrpc_post(
                    xrpc_base, "com.atproto.repo.createRecord",
                    _create_body(
                        session_did, f"{CAS_ANCHOR_TEXT} (re-anchor {attempt})"
                    ),
                    access,
                )
                assert status_a == 200, (
                    f"re-anchor failed: {status_a} {anchor}\n{_tail()}"
                )
                current_commit = (anchor.get("commit") or {}).get("cid")
                assert current_commit, f"re-anchor answered no commit: {anchor}"
                continue
            break
        assert status == 200, (
            "a swapCommit naming the CURRENT head must be served — without this "
            f"control the refusal above proves nothing: {status} {created}\n{_tail()}"
        )
        with alice_ws:
            _await_feed_item(alice_ws, CAS_SERVED_TEXT, _tail)

        # Nothing was ingested for the refused write: the CAS is checked
        # bridge-side, BEFORE the nest call (§ F2 detail — "nothing ingested").
        # A refusal after it would leave a real Fauna post that all 7 apps show
        # for a write the caller was told failed.
        with alice_ws:
            bodies = [
                p["body"]
                for p in alice_ws.call("fauna.feed.local.posts", {"limit": 100})["posts"]
            ]
        assert CAS_REFUSED_TEXT not in bodies, (
            "a CAS-refused write created a Fauna post anyway — the refusal is "
            f"happening after the nest call, not before it; feed held {bodies}"
        )

        # ── 13. LEG 8 — `applyWrites` IS SERVED, AS ONE COMMIT (F2.3 piece 2).
        # The batch verb over the same path. What tier_3 uniquely proves: the
        # whole batch reaches the network as ONE #commit frame under the
        # account's real DID — a per-write loop would answer 200 just the same
        # while emitting N frames for what the caller asked to be one. ──
        status, applied = _xrpc_post(
            xrpc_base, "com.atproto.repo.applyWrites",
            {
                "repo": session_did,
                "writes": [
                    {
                        "$type": "com.atproto.repo.applyWrites#create",
                        "collection": "app.bsky.feed.post",
                        "value": _create_body(session_did, BATCH_TEXT_A)["record"],
                    },
                    {
                        "$type": "com.atproto.repo.applyWrites#create",
                        "collection": "app.bsky.feed.post",
                        "value": _create_body(session_did, BATCH_TEXT_B)["record"],
                    },
                ],
            },
            access,
        )
        assert status == 200, f"applyWrites failed: {status} {applied}\n{_tail()}"
        results = applied.get("results") or []
        assert len(results) == 2, f"results must be positional, one per write: {applied}"
        for i, r in enumerate(results):
            assert r.get("$type") == "com.atproto.repo.applyWrites#createResult", (
                f"result {i} is not a #createResult: {r}"
            )
            assert r.get("uri", "").startswith(f"at://{did}/"), (
                f"result {i} names a repo other than the account's real DID: {r}"
            )
        assert (applied.get("commit") or {}).get("cid"), (
            f"the batch must answer the commit it produced: {applied}"
        )

        # Both records really landed in the repo…
        for r, text in zip(results, (BATCH_TEXT_A, BATCH_TEXT_B)):
            served = _xrpc_get(
                "com.atproto.repo.getRecord",
                f"repo={did}&collection=app.bsky.feed.post"
                f"&rkey={r['uri'].rsplit('/', 1)[-1]}",
            )
            assert served.get("value", {}).get("text") == text, served

        # …and both became real Fauna posts, so the batch took the same
        # round-trip path a single createRecord does rather than journaling.
        with alice_ws:
            for text in (BATCH_TEXT_A, BATCH_TEXT_B):
                _await_feed_item(alice_ws, text, _tail)

        # ONE commit, proven where it matters — on the wire. Both of the
        # batch's records must ride the SAME #commit frame.
        frame_texts = _await_commit_frame_texts(xrpc_port, BATCH_TEXT_B, _tail)
        assert BATCH_TEXT_A in frame_texts, (
            "the batch's two records reached the firehose in DIFFERENT #commit "
            f"frames — `applyWrites` = one funnel commit; that frame held {frame_texts}"
        )

        # ── 14. LEG 9 — THE MEDIA ROUND-TRIP (F2.4 slice 2). uploadBlob, then
        # a createRecord whose embed references the answered blob, becomes a
        # real Fauna MEDIA post — the direction slice 1 deliberately left open
        # (a record used to round-trip with its text only). The declared
        # Content-Type lies (application/octet-stream) to prove the sniffed
        # type is what travels. ──
        status, uploaded = _xrpc_post_bytes(
            xrpc_base, "com.atproto.repo.uploadBlob",
            TINY_PNG, "application/octet-stream", access,
        )
        assert status == 200, f"uploadBlob failed: {status} {uploaded}\n{_tail()}"
        blob = uploaded.get("blob") or {}
        assert blob.get("mimeType") == "image/png", (
            f"the answered type must be the SNIFFED one, not the declared "
            f"octet-stream: {uploaded}"
        )

        media_body = _create_body(session_did, MEDIA_TEXT)
        # The app echoes the upload reply's blob object verbatim — what real
        # third-party clients do.
        media_body["record"]["embed"] = {
            "$type": "app.bsky.embed.images",
            "images": [{
                "alt": "a tiny square",
                "image": blob,
                "aspectRatio": {"width": 1, "height": 1},
            }],
        }
        status, media_created = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord", media_body, access,
        )
        assert status == 200, (
            f"media createRecord failed: {status} {media_created}\n{_tail()}"
        )

        with alice_ws:
            media_item = _await_feed_item(alice_ws, MEDIA_TEXT, _tail)
        assert media_item.get("has_media") is True, (
            "the round-tripped post must be a real Fauna MEDIA post — has_media "
            f"False is the text-only round-trip slice 2 replaced: {media_item}"
        )
        assert media_item["author"] == alice["actor_id_bytes"].hex(), media_item

        # …and the record serves from the repo with its embed intact — the
        # caller's bytes are what committed, media ref included.
        served = _xrpc_get(
            "com.atproto.repo.getRecord",
            f"repo={did}&collection=app.bsky.feed.post"
            f"&rkey={media_created['uri'].rsplit('/', 1)[-1]}",
        )
        served_embed = served.get("value", {}).get("embed", {})
        assert served_embed.get("$type") == "app.bsky.embed.images", served
        served_ref = served_embed["images"][0]["image"]["ref"]
        assert served_ref.get("$link") == blob["ref"]["$link"], (
            f"the served record must reference the very blob the upload "
            f"answered: {served}"
        )

        # ── 15. LEG 10 — CROSS-BINARY. A record
        # referencing a blob this account never uploaded REFUSES (InvalidRequest
        # naming the remedy), and no Fauna post is manufactured — the repo-scoped
        # serving argument in § F2 detail, observed on the wire. ──
        dangling_body = _create_body(session_did, "this post must never exist")
        dangling_body["record"]["embed"] = {
            "$type": "app.bsky.embed.images",
            "images": [{
                "alt": "",
                "image": {
                    "$type": "blob",
                    "ref": {"$link": FOREIGN_CID},
                    "mimeType": "image/png",
                    "size": 10,
                },
            }],
        }
        status, refused = _xrpc_post(
            xrpc_base, "com.atproto.repo.createRecord", dangling_body, access,
        )
        assert status == 400, (
            f"a dangling blob ref must refuse, got {status} {refused}\n{_tail()}"
        )
        assert refused.get("error") == "InvalidRequest", refused
        assert "uploadBlob" in refused.get("message", ""), (
            f"the refusal must name the remedy: {refused}"
        )
        with alice_ws:
            bodies = [
                p["body"]
                for p in alice_ws.call("fauna.feed.local.posts", {"limit": 100})["posts"]
            ]
        assert "this post must never exist" not in bodies, (
            "a refused media write must manufacture no Fauna post"
        )
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
        dns.close()
        directory.close()
        nest_cleanup()


def _fauna_profile(alice_ws, actor_id: bytes) -> dict:
    """Alice's stored Fauna profile as a plain dict, or ``{}`` if she has none.

    `fauna.profile.get` serves the stored bytes verbatim — the signed
    embed-as-bytes wire — and the client decodes them, so this decodes the same
    two layers `fauna_core::encoding::decode_profile` does: the outer wire's
    ``bytes`` field holds the canonical dag-cbor of the inner ``Profile``.
    Signature verification is not repeated here; the nest's own gate already ran
    it on ingest, and what this leg asserts is the profile's *content*.
    """
    import cbor2

    with alice_ws:
        try:
            reply = alice_ws.call("fauna.profile.get", {"actor_id": actor_id.hex()})
        except Exception:
            return {}  # no profile published yet
    wire = cbor2.loads(bytes(reply["body"]))
    return cbor2.loads(bytes(wire["bytes"]))


def _create_session(xrpc_base: str, secret: str, tail) -> tuple[str, str]:
    """(accessJwt, did) for alice's raw-XRPC session.

    The DID is the one the SESSION asserts, which is the DID every subsequent
    call must name as `repo` — see the module docstring's convergence note.
    """
    status, sess = _xrpc_post(
        xrpc_base, "com.atproto.server.createSession",
        {"identifier": "alice", "password": secret},
    )
    assert status == 200, f"createSession failed: {status} {sess}\n{tail()}"
    return sess["accessJwt"], sess["did"]


def _create_body(
    did: str, text: str, parent: tuple[str, str] | None = None
) -> dict:
    """A `com.atproto.repo.createRecord` body for an `app.bsky.feed.post`.

    `createdAt` is the caller's own assertion and the Fauna post adopts it
    verbatim (§ F2 detail), which is what makes the synchronously-answered
    AT-URI the one the record really lands at.
    """
    record: dict = {
        "$type": "app.bsky.feed.post",
        "text": text,
        "createdAt": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()),
    }
    if parent is not None:
        uri, cid = parent
        ref = {"uri": uri, "cid": cid}
        # This post starts no thread of its own, so parent and root coincide —
        # the shape a third-party client sends when replying to a top-level post.
        record["reply"] = {"root": ref, "parent": ref}
    return {"repo": did, "collection": "app.bsky.feed.post", "record": record}


def _await_commit_for(
    xrpc_port: int, text: str, tail, budget_s: float = 60
) -> tuple[set[str], set[str]]:
    """(repos announced, post texts seen) from `subscribeRepos`, replayed from 0.

    Returns as soon as `text` is seen, so a green run pays nothing for the
    budget (convention 14). The repo set is the load-bearing half: a relay keys
    every commit by it, so a frame announced under a DID that resolves to
    nothing is invisible to the network however well formed it is.
    """
    import websocket  # type: ignore[import-untyped]  # from `websocket-client`

    sub = websocket.create_connection(
        f"wss://127.0.0.1:{xrpc_port}/xrpc/com.atproto.sync.subscribeRepos?cursor=0",
        sslopt={"cert_reqs": ssl.CERT_NONE, "check_hostname": False},
        timeout=45,
    )
    repos: set[str] = set()
    texts: set[str] = set()
    try:
        deadline = time.monotonic() + budget_s
        while time.monotonic() < deadline and text not in texts:
            try:
                message = sub.recv()
            except Exception as exc:  # noqa: BLE001 — surfaced by the caller's assert
                pytest.fail(f"firehose read failed: {exc}\nlog:\n{tail()}")
            assert isinstance(message, (bytes, bytearray)), (
                f"subscribeRepos frames are BINARY, got {type(message)}"
            )
            header, body = _decode_frame(bytes(message))
            assert header.get("op") != -1, f"error frame from the firehose: {body}"
            if header.get("t") != "#commit":
                continue
            if isinstance(body.get("repo"), str):
                repos.add(body["repo"])
            for record in _car_records(body.get("blocks", b"")):
                if record.get("$type") == "app.bsky.feed.post":
                    texts.add(record.get("text", ""))
    finally:
        sub.close()
    return repos, texts


def _await_commit_frame_texts(
    xrpc_port: int, text: str, tail, budget_s: float = 60
) -> set[str]:
    """The post texts carried by the ONE `#commit` frame that contains `text`.

    [_await_commit_for] aggregates across frames, which cannot distinguish "one
    commit carrying two records" from "two commits carrying one each" — the
    exact property `applyWrites` = one funnel commit asserts. This returns a
    single frame's own records instead.

    Returns as soon as that frame is seen, so a green run pays nothing for the
    budget (convention 14).
    """
    import websocket  # type: ignore[import-untyped]  # from `websocket-client`

    sub = websocket.create_connection(
        f"wss://127.0.0.1:{xrpc_port}/xrpc/com.atproto.sync.subscribeRepos?cursor=0",
        sslopt={"cert_reqs": ssl.CERT_NONE, "check_hostname": False},
        timeout=45,
    )
    seen: list[set[str]] = []
    try:
        deadline = time.monotonic() + budget_s
        while time.monotonic() < deadline:
            try:
                message = sub.recv()
            except Exception as exc:  # noqa: BLE001 — surfaced by the assert below
                pytest.fail(f"firehose read failed: {exc}\nlog:\n{tail()}")
            header, body = _decode_frame(bytes(message))
            assert header.get("op") != -1, f"error frame from the firehose: {body}"
            if header.get("t") != "#commit":
                continue
            texts = {
                record.get("text", "")
                for record in _car_records(body.get("blocks", b""))
                if record.get("$type") == "app.bsky.feed.post"
            }
            seen.append(texts)
            if text in texts:
                return texts
    finally:
        sub.close()
    pytest.fail(
        f"no #commit frame carried {text!r}; frames held {seen}\nlog:\n{tail()}"
    )


def _await_feed_item(alice_ws, body: str, tail, budget_s: float = 60, until=None) -> dict:
    """The account's own Fauna read face until `body` appears.

    A deadline poll on observable state, never a settle-sleep (convention 14):
    a green run returns on the first pass and pays nothing for the budget.
    `fauna.feed.local.posts` is the same wire all 7 apps render their feed
    from, so "visible here" is literally "visible in a Fauna app".

    `until` narrows "appeared" to a predicate on the item, for state that
    settles after the post itself does — the parent's `reply_count`, which is
    bumped by ingesting the *reply*, not the parent.
    """
    deadline = time.monotonic() + budget_s
    seen: list[str] = []
    last: dict | None = None
    while time.monotonic() < deadline:
        posts = alice_ws.call("fauna.feed.local.posts", {"limit": 100})["posts"]
        seen = [p["body"] for p in posts]
        for p in posts:
            if p["body"] == body:
                last = p
                if until is None or until(p):
                    return p
        time.sleep(1)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep — a green run exits on the first pass
    if last is not None:
        pytest.fail(
            f"{body!r} is on the feed but never satisfied the predicate; "
            f"last saw {last}\nlog:\n{tail()}"
        )
    pytest.fail(
        f"{body!r} never became a Fauna post on the account's own feed; "
        f"saw {seen}\nlog:\n{tail()}"
    )
