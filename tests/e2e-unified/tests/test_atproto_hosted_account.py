"""tier_3: what the Bluesky account a nest hosts does, observed from outside the
nest — real nest + real e2e-flavor ``fauna-atproto-bridge`` + fake PLC
directory, fake DNS and fake relay (``helpers/atproto_hosted.py``).

One witness per outcome of the atproto feature page, each on its own nest so no
test leans on another's state:

* the nest announces itself to the network's relays with ``requestCrawl``, and
  only once alice's identity resolves (``atproto-pds-bridge.md`` § Goal; the
  first-emit gate, ``atproto-pds-full.md`` § Ecosystem reality);
* deleting your own post removes it from the hosted account
  (``atproto-pds-bridge.md`` § Firm boundaries — a Fauna tombstone maps to
  ``deleteRecord``);
* only public posts project: a gated post and a sold post never do
  (``atproto-pds-bridge.md`` § Firm boundaries; "sold" is a degenerate
  single-post tier, ``monetization.md`` § Per-post pay-to-unlock);
* hosting again after a permanent retirement mints a brand-new identity with
  a fresh user rotation key, and the retired one stays retired
  (``atproto-pds-bridge.md`` § Disable & revocation);
* coming back after deleting the presence (no retirement) restores the SAME
  identity and rebuilds the history the user had consented to, at the same
  record addresses (``atproto-pds-bridge.md`` § Disable & revocation; the
  repo is derived state, § Projection & backfill);
* a legally taken-down post is never published to the hosted account, and one
  already published is withdrawn and stays withdrawn after an overturn
  (``atproto-pds-bridge.md`` § Projection & backfill, the 2026-07-29 takedown
  bullet; the rule is ``takedown-and-block.md`` § Legal takedown's);
* a post that would carry neither text nor a picture is never published blank
  (``atproto-pds-bridge.md`` § Projection & backfill);
* once the identity's directory head lists only the user's own key — the state
  a recovery-fork contest leaves — the nest keeps publishing posts but every
  directory operation it signs is refused, so a later handle change stays
  unpublished there (``atproto-identity-custody.md`` § The 72 h recovery-fork
  contest, decisions 3 and 4);
* a handle rename withdraws the ``_atproto`` TXT at the old name and publishes
  it at the new one, leaving what another tool wrote there — kept by the
  admin's client, so driven app-less through the shared DNS machine
  (``atproto-pds-bridge.md`` § Identity; ``dns-management.md`` § Fauna-managed).

Posts are written over WS-RPC, the same pragmatic exception
``test_atproto_firehose_post.py`` takes: what is witnessed is the nest's
publication, and the client-UI journey that writes a post is
``test_feed.py::test_create_post``.
"""

import base64
import json
import os
import ssl
import time
import urllib.error
import urllib.request

import blake3
import pytest

import fauna_ffi
from helpers.atproto_fakes import dag_cbor, op_cid, verify_op_sig
from helpers.atproto_hosted import HANDLE_DOMAIN, PDS_HOST, hosted_account
from helpers.blob_upload import SEALED_MIME, upload_blob
from helpers.xrpc_client import put_blob
from tests.api import ws_api
from tests.api.bare import sign_and_encode_tombstone

pytestmark = pytest.mark.tier_3


def _create_post(account, text: str) -> str:
    with account.ws:
        return account.ws.call(
            "fauna.posts.create",
            {"body": fauna_ffi.build_post(bytes(account.alice["signing_key"]), text)},
        )["post_id"]


@pytest.mark.feature("atproto")
def test_the_nest_announces_itself_to_the_relay_once_the_identity_resolves(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-announce",
        publish_txt=False,
    ) as account:
        # While alice's handle does not resolve, the first-emit gate holds every
        # commit back, and with it the announce: a relay told about a host whose
        # identity it cannot resolve indexes it broken (the first-impression
        # trap). The gate's own log line is the observable that the pass ran.
        _create_post(account, "posted while the handle does not resolve yet")
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            if "first-emit gated" in account.log_path.read_text(errors="replace"):
                break
            account.assert_alive("while gated")
            time.sleep(0.5)  # sleep-ok: poll interval of a deadline loop
        else:
            pytest.fail("the projection pass never reached the first-emit gate:\n" + account.tail())
        assert account.relay.snapshot() == [], (
            "nothing may be announced while the identity is unresolvable; the relay "
            f"saw {account.relay.snapshot()}"
        )

        # The handle resolves; the next pass commits, and the commit announces.
        account.publish_txt()
        _create_post(account, "posted once the handle resolves")
        account.await_post_texts(lambda texts: len(texts) >= 2, "both posts to project")

        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and not account.relay.snapshot():
            account.assert_alive("before the announce")
            time.sleep(0.5)  # sleep-ok: poll interval of a deadline loop
        announced = account.relay.snapshot()
        assert announced, "a committed repo must be announced with requestCrawl:\n" + account.tail()
        assert set(announced) == {PDS_HOST}, (
            f"the announce must name this PDS's own host {PDS_HOST!r}, got {announced}"
        )
        # Throttled: two commits in quick succession are one announce, never one
        # per commit (the relay follows the firehose once it subscribes).
        assert len(announced) == 1, f"a burst of commits must announce once, got {announced}"


@pytest.mark.feature("atproto")
def test_deleting_a_post_removes_it_from_the_hosted_account(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    doomed = "a post its author will delete"
    kept = "a post its author keeps"
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-self-delete",
    ) as account:
        doomed_id = _create_post(account, doomed)
        _create_post(account, kept)
        account.await_post_texts(
            lambda texts: doomed in texts and kept in texts, "both posts to project",
        )

        tombstone = sign_and_encode_tombstone(
            account.alice["signing_key"], doomed_id, int(time.time() * 1_000_000),
        )
        with account.ws:
            reply = account.ws.call("fauna.posts.delete", {"body": tombstone})
        assert reply["deleted"] is True, reply

        texts = account.await_post_texts(
            lambda texts: doomed not in texts, "the deleted post to leave the hosted repo",
        )
        assert kept in texts, (
            f"the delete must remove ONLY the deleted post; {kept!r} must stay, got {texts}"
        )


@pytest.mark.feature("atproto")
def test_a_gated_or_sold_post_never_reaches_the_hosted_account(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    gated_teaser = "Members only: the gated post's teaser"
    sold_teaser = "For sale: the sold post's teaser"
    sentinel = "a public post written after the gated and the sold one"
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-public-only",
    ) as account:
        secret = bytes(account.alice["signing_key"])
        port, token = account.nest["port"], account.alice["token"]

        def _tier(name: str, rank: int, period_key: bytes, unlocks_post: str | None = None) -> None:
            with account.ws:
                account.ws.call(
                    "fauna.subscriptions.tiers.create",
                    {
                        "name": name,
                        "rank": rank,
                        "description": None,
                        "price_hint": "5 EUR",
                        "payment_url": "https://pay.example/unlock",
                        "auto_approve": False,
                        "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, name, period_key),
                        "unlocks_post": unlocks_post,
                    },
                )

        def _gated_post(teaser: str, tier: str, rank: int, period_key: bytes, seal: bytes) -> tuple[bytes, bytes]:
            body, sealed = fauna_ffi.build_gated_post(
                secret, teaser, teaser + "\n\nThe part you pay for.", tier, rank, seal, period_key,
            )
            upload_blob(port, token, sealed, audience_class="PeriodRestrictedPost", mime=SEALED_MIME)
            return body, sealed

        # A post gated to a subscription tier.
        gold_key = bytes(range(32))
        _tier("gold", 2, gold_key)
        gated_body, _ = _gated_post(gated_teaser, "gold", 2, gold_key, b"\x11" * 32)
        with account.ws:
            account.ws.call("fauna.posts.create", {"body": gated_body})

        # A sold post: the composer's forced ordering — the gated body names its
        # own single-post tier, the tier designates `blake3(body)`, and only
        # then is the post created.
        unlock_key = bytes(range(32, 64))
        unlock_tier = "post-unlock-e2e"
        sold_body, _ = _gated_post(sold_teaser, unlock_tier, 1, unlock_key, b"\x22" * 32)
        sold_id = blake3.blake3(sold_body).hexdigest()
        _tier(unlock_tier, 1, unlock_key, unlocks_post=sold_id)
        with account.ws:
            created = account.ws.call("fauna.posts.create", {"body": sold_body})["post_id"]
        assert created == sold_id, f"the sold post's id must be blake3(body): {created} != {sold_id}"

        # A public post AFTER both: the projection stream is ordered, so once
        # it is published every earlier post has been passed over — the
        # witness for "never" is observable without waiting on a clock.
        _create_post(account, sentinel)
        texts = account.await_post_texts(lambda texts: sentinel in texts, "the public post to project")
        leaked = [t for t in texts if t and (gated_teaser in t or sold_teaser in t)]
        assert not leaked, (
            "only public posts may reach the hosted account; a gated or sold post was "
            f"published: {leaked}"
        )


@pytest.mark.feature("atproto")
def test_hosting_again_after_a_retirement_mints_a_fresh_identity(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-remint",
    ) as account:
        retired_did, retired_key = account.did, account.rotation_key

        # The delete ceremony with its "also permanently retire" opt-in: the
        # presence is destroyed, then the retirement is requested.
        with account.ws:
            account.ws.call("fauna.bridges.atproto.delete_presence", {})
            account.ws.call("fauna.bridges.atproto.request_tombstone", {})

        # The client retires only once the sweep has finished — observed where
        # the client observes it, the PDS's own getRepoStatus, here on the
        # bridge's listener (the published `pds.<domain>` does not resolve).
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            _, error = account.xrpc_status(
                "com.atproto.sync.getRepoStatus", f"did={retired_did}",
            )
            if error == "RepoNotFound":
                break
            account.assert_alive("while the presence is swept")
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop
        else:
            pytest.fail("the bridge never finished the delete sweep:\n" + account.tail())

        # The user's client signs the tombstone with the held key and reports it.
        prev_cid = fauna_ffi.atproto_retire(account.directory.url, retired_did, retired_key)
        assert prev_cid, "the identity was live; the tombstone must chain to its head"
        with account.ws:
            reply = account.ws.call(
                "fauna.bridges.atproto.record_tombstone", {"prev_cid": prev_cid},
            )
        assert reply["newly_tombstoned"] is True, reply

        # Hosting again is an ordinary mint with a fresh user rotation key.
        fresh_key = fauna_ffi.atproto_rotation_key()
        account.enter_hosted(fresh_key)
        fresh_did = account.await_mint(besides={retired_did})

        genesis = account.directory.genesis_op(fresh_did)
        assert genesis["rotationKeys"][0] == fresh_key["did_key"], (
            "the fresh identity must publish the fresh senior key; genesis lists "
            f"{genesis['rotationKeys']}"
        )
        assert retired_key["did_key"] not in genesis["rotationKeys"], (
            "the retired identity's key must never reappear — both public logs would "
            f"link the two identities; genesis lists {genesis['rotationKeys']}"
        )
        assert account.directory.standing_op_type(retired_did) == "plc_tombstone", (
            "the retired identity must stay retired"
        )
        with account.ws:
            status = account.ws.call("fauna.bridges.atproto.get_integration_status", {})
        assert (status.get("identity") or {}).get("did") == fresh_did, (
            f"the nest must report the fresh identity as alice's, got {status}"
        )


# ── shared seams of the witnesses below ──────────────────────────────────────

LEGAL_REFERENCE = "EU-DSA-2026/0059"


def _admin(account) -> dict:
    sk = account.nest["admin"]["signing_key"]
    return {"actor_id_bytes": bytes(sk.verify_key), "signing_key": sk}


def _post_records(account) -> list[dict]:
    """Every ``app.bsky.feed.post`` record the hosted repo serves, ``uri`` and
    ``value`` both."""
    return account.xrpc_get(
        "com.atproto.repo.listRecords",
        f"repo={account.did}&collection=app.bsky.feed.post&limit=100",
    )["records"]


def _run_due_pending_actions(account) -> dict:
    """Fast-forward a pending action's cool-off and run it through the same
    executor the background tick runs (the ``test_atproto_firehose_post.py``
    rename leg's hook)."""
    from conftest import _bridge_admin_post

    return _bridge_admin_post(
        account.nest["url"], account.nest["admin"]["token"],
        "/api/v1/test/pending_actions/run_due", {},
    )


def _sign_plc_op(unsigned: dict, key: dict) -> str:
    """The ``sig`` a P-256 rotation key puts on a PLC operation: ECDSA-SHA256
    over the op's unsigned dag-cbor, fixed-size ``r‖s``, base64url without
    padding. The directory refuses the high-S twin, and ECDSA's nonce makes
    either half equally likely, so this signs until the fake directory's own
    ``verify_op_sig`` accepts — its acceptance rule, never a second copy."""
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.asymmetric.utils import decode_dss_signature

    private = ec.derive_private_key(int(key["secret"], 16), ec.SECP256R1())
    reason = ""
    for _ in range(64):
        r, s = decode_dss_signature(private.sign(dag_cbor(unsigned), ec.ECDSA(hashes.SHA256())))
        sig = base64.urlsafe_b64encode(r.to_bytes(32, "big") + s.to_bytes(32, "big")).decode("ascii").rstrip("=")
        ok, reason = verify_op_sig({**unsigned, "sig": sig}, [key["did_key"]])
        if ok:
            return sig
    raise AssertionError(f"no signature the directory accepts in 64 tries: {reason}")


def _submit_plc_op(directory_url: str, did: str, op: dict) -> int:
    req = urllib.request.Request(
        f"{directory_url}/{did}", data=json.dumps(op).encode(), method="POST",
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, context=ssl._create_unverified_context(), timeout=15) as resp:
            return resp.status
    except urllib.error.HTTPError as exc:
        return exc.code


@pytest.mark.feature("atproto")
def test_after_a_contest_the_nest_keeps_publishing_but_never_writes_the_directory_again(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    """``atproto-identity-custody.md`` § The 72 h recovery-fork contest,
    decision 4: a contest leaves the identity's directory head listing only the
    user's own rotation key while keeping the box's repo signing key and the
    PDS service entry (decision 3) — so the nest goes on publishing posts as
    the user, and every directory operation it signs from then on is refused,
    the bridge's rename op included.

    The head is built here the way decision 3 shapes a fork — the standing op
    carried forward verbatim with ``rotationKeys`` cut to the held key — and
    signed with the key the user's client holds. The client-side contest
    journey (the banner, the confirm, the fork chained around a hostile op) is
    ``test_atproto_custody_alarm.py::test_recovery_fork_contest_end_to_end``;
    what this test observes is the nest's side of the state that journey ends
    in, from outside the nest."""
    before_contest = "a post published before the contest"
    after_contest = "a post published after the contest"
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-post-contest",
    ) as account:
        directory, did = account.directory, account.did
        user_key = account.rotation_key["did_key"]
        _create_post(account, before_contest)
        account.await_post_texts(lambda texts: before_contest in texts, "the first post to project")

        ops = [op for d, op in directory.snapshot() if d == did]
        head = ops[-1]
        assert head["rotationKeys"][0] == user_key and len(head["rotationKeys"]) >= 2, (
            f"the minted identity must list the user's senior key and the box's junior one: {head}"
        )
        fork = {k: v for k, v in head.items() if k != "sig"}
        fork["prev"] = op_cid(head)
        fork["rotationKeys"] = [user_key]
        fork["sig"] = _sign_plc_op(fork, account.rotation_key)
        status = _submit_plc_op(directory.url, did, fork)
        assert status == 200, (
            f"the directory refused the user's own op ({status}): {directory.sig_rejected}"
        )
        assert directory.standing_rotation_keys(did) == [user_key], (
            "after the contest only the user's key may authorize the next op"
        )
        accepted_before_rename = len([1 for d, _ in directory.snapshot() if d == did])

        # The box keeps the repo signing key, so publishing goes on.
        _create_post(account, after_contest)
        account.await_post_texts(
            lambda texts: after_contest in texts,
            "a post written after the contest to publish — the box keeps the repo signing key",
        )

        # A handle change: the new handle resolves (so the bridge does not
        # defer), the bridge signs an alsoKnownAs update with its junior key,
        # and the directory refuses it.
        new_handle = f"alicecontested.{HANDLE_DOMAIN}"
        account.dns.publish(f"_atproto.{new_handle}", [f"did={did}"])
        with account.ws:
            account.ws.call("fauna.profile.handle.change", {"handle": "alicecontested"})
        ran = _run_due_pending_actions(account)
        assert ran.get("executed", 0) >= 1, f"the handle change never applied: {ran}"

        deadline = time.monotonic() + 90
        refused = []
        while time.monotonic() < deadline:
            refused = [reason for d, reason in directory.sig_rejected if d == did]
            if refused:
                break
            account.assert_alive("while the rename is attempted")
            time.sleep(1)  # sleep-ok: poll interval of a deadline loop
        assert refused, (
            "the bridge never attempted the rename op, so its refusal is unobserved; "
            f"accepted ops {directory.snapshot()}\nlog:\n{account.tail()}"
        )
        accepted = [op for d, op in directory.snapshot() if d == did]
        assert len(accepted) == accepted_before_rename, (
            f"no op signed by the box may be accepted after the contest: {accepted[accepted_before_rename:]}"
        )
        standing = accepted[-1]
        assert standing["alsoKnownAs"] == head["alsoKnownAs"], (
            "the handle change must stay unpublished at the directory after a contest: "
            f"{standing['alsoKnownAs']}"
        )
        assert standing["rotationKeys"] == [user_key], standing
        assert standing["verificationMethods"] == head["verificationMethods"], (
            "a contest keeps the box's repo signing key"
        )


@pytest.mark.feature("atproto")
def test_a_taken_down_post_is_never_published_to_the_hosted_account_and_a_published_one_is_withdrawn(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    """A legal takedown and the hosted account (``atproto-pds-bridge.md`` §
    Projection & backfill, the 2026-07-29 bullet; the rule
    ``takedown-and-block.md`` § Legal takedown): a post taken down before the
    projection reached it is never committed; one already committed is deleted
    from the repo; and the overturn publishes nothing again.

    The first half holds the first-emit gate closed (the handle does not
    resolve yet), so nothing can be committed until the takedown has landed —
    a "never" observable without a clock. The linked-account cross-post's
    withdrawal is ``tests/api/test_bluesky_takedown_and_media_proxy.py``'s; the
    ActivityPub outbox half is
    ``tests/api/test_activitypub_federation.py``'s."""
    never = "a post taken down before it was ever published"
    withdrawn = "a post taken down after it was published"
    first_sentinel = "a public post written after the first takedown"
    second_sentinel = "a public post written after the overturn"
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-takedown",
        publish_txt=False,
    ) as account:
        port, admin = account.nest["port"], _admin(account)

        never_id = _create_post(account, never)
        taken = ws_api.moderation_legal_takedown(port, admin, never_id, legal_reference=LEGAL_REFERENCE)
        assert taken.get("status") == "taken_down", taken
        account.publish_txt()
        _create_post(account, first_sentinel)
        texts = account.await_post_texts(lambda t: first_sentinel in t, "the first sentinel to project")
        assert never not in texts, f"a taken-down post was published: {texts}"

        withdrawn_id = _create_post(account, withdrawn)
        account.await_post_texts(lambda t: withdrawn in t, "the second post to project")
        taken = ws_api.moderation_legal_takedown(port, admin, withdrawn_id, legal_reference=LEGAL_REFERENCE)
        assert taken.get("status") == "taken_down", taken
        texts = account.await_post_texts(
            lambda t: withdrawn not in t, "the taken-down post to leave the hosted repo",
        )
        assert first_sentinel in texts, f"the withdrawal must remove only the taken-down post: {texts}"

        restored = ws_api.moderation_legal_takedown(port, admin, withdrawn_id, restore=True)
        assert restored.get("status") == "restored", restored
        _create_post(account, second_sentinel)
        texts = account.await_post_texts(lambda t: second_sentinel in t, "the post after the overturn to project")
        assert withdrawn not in texts and never not in texts, (
            f"an overturn must not publish a withdrawn post again: {texts}"
        )


@pytest.mark.feature("atproto")
def test_a_post_with_neither_text_nor_a_picture_is_never_published_blank(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    """``atproto-pds-bridge.md`` § Projection & backfill: an attachment Bluesky
    cannot carry is dropped from the projected record, and a post left with
    neither text nor an embed is skipped rather than committed blank. The
    control post carries the same unmappable attachment beside its text and
    does publish, so the blank one's absence is the skip, not a refusal of the
    attachment kind."""
    document = b"%PDF-1.4\n% a document Bluesky has no embed for\n"
    with_text = "a post with a document attached and something to say"
    sentinel = "a public post written after the blank one"
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-no-blank",
    ) as account:
        cid = fauna_ffi.content_cid(document)
        put_blob(account.nest["url"], account.alice["token"], cid, document)
        media = [{"blob_cid": cid, "mime": "application/pdf", "size_bytes": len(document)}]
        secret = bytes(account.alice["signing_key"])

        with account.ws:
            account.ws.call("fauna.posts.create", {"body": fauna_ffi.build_post_with_media(secret, with_text, media)})
        with account.ws:
            account.ws.call("fauna.posts.create", {"body": fauna_ffi.build_post_with_media(secret, "", media)})
        _create_post(account, sentinel)

        account.await_post_texts(
            lambda t: with_text in t and sentinel in t, "the control post and the sentinel to project",
        )
        records = _post_records(account)
        blank = [r for r in records if not r.get("value", {}).get("text") and not r.get("value", {}).get("embed")]
        assert not blank, f"a blank post was published: {blank}"
        assert len(records) == 2, (
            "exactly the control post and the sentinel may publish; got "
            f"{[r.get('value') for r in records]}"
        )


@pytest.mark.feature("atproto")
def test_coming_back_after_deleting_the_presence_restores_the_same_identity_and_history(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    """``atproto-pds-bridge.md`` § Disable & revocation: deleting the presence
    sweeps the repo but destroys no identity, so entering a hosted level again
    restores the SAME DID — no second mint at the directory — and the bridge
    rebuilds the repo from the stored projection floor: the post published
    before the deletion is back, at the same record address (deterministic
    rkeys, § Projection & backfill)."""
    history = "a post published before the presence was deleted"
    with hosted_account(
        nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-reentry",
    ) as account:
        did = account.did
        _create_post(account, history)
        account.await_post_texts(lambda t: history in t, "the post to project")
        uri_before = {r["value"].get("text"): r["uri"] for r in _post_records(account)}[history]

        with account.ws:
            account.ws.call("fauna.bridges.atproto.delete_presence", {})
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            _, error = account.xrpc_status("com.atproto.sync.getRepoStatus", f"did={did}")
            if error == "RepoNotFound":
                break
            account.assert_alive("while the presence is swept")
            time.sleep(2)  # sleep-ok: poll interval of a deadline loop
        else:
            pytest.fail("the bridge never finished the delete sweep:\n" + account.tail())

        account.enter_hosted(account.rotation_key)
        account.await_post_texts(lambda t: history in t, "the consented history to be rebuilt", deadline_s=180)
        uri_after = {r["value"].get("text"): r["uri"] for r in _post_records(account)}[history]
        assert uri_after == uri_before, (
            f"the rebuilt record must keep its address: {uri_before} → {uri_after}"
        )
        assert account.directory.minted_dids() == [did], (
            "coming back must restore the identity, never mint a second one: "
            f"{account.directory.minted_dids()}"
        )
        with account.ws:
            status = account.ws.call("fauna.bridges.atproto.get_integration_status", {})
        assert (status.get("identity") or {}).get("did") == did, (
            f"the nest must report the same identity as alice's, got {status}"
        )


def _matrix_names(account) -> set[str]:
    """The record names the nest's DNS matrix lists for its primary domain —
    the read the admin's client publishes from."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = account.nest["admin"]["signing_key"]
    with WsRpcAdminClient(
        account.nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk),
    ) as admin_ws:
        domains = admin_ws.call("fauna.dns.list_records", {})["domains"]
    return {r["name"].rstrip(".") for d in domains if d["is_primary"] for r in d["records"]}


def _await_matrix(account, present: str, absent: str | None = None) -> set[str]:
    """Wait until the matrix lists ``present`` (and no longer ``absent``)."""
    deadline = time.monotonic() + 30
    names = _matrix_names(account)
    while time.monotonic() < deadline:
        if present in names and (absent is None or absent not in names):
            break
        time.sleep(0.5)  # sleep-ok: poll interval of a deadline loop
        names = _matrix_names(account)
    return names


@pytest.mark.feature("atproto")
def test_a_handle_rename_withdraws_the_old_handle_txt(
    nest_binary, atproto_bridge_e2e_binary, tmp_path_factory,
):
    """``atproto-pds-bridge.md`` § Identity: a rename re-derives the ATProto
    handle and re-publishes the ``_atproto`` TXT, and the TXT at the OLD name is
    withdrawn, so a later user who claims the freed handle never finds a second
    ``did=`` there. The withdraw is kept by the admin's client, the only holder
    of the DNS credential (``dns-management.md`` § Fauna-managed →
    *Withdraw-aware convergence*): the shared ``DnsManagementMachine``'s managed
    publish remembers which ``_atproto`` names it published and converges each
    to removal once the name leaves the nest's record matrix. So the witness is
    app-less, through the e2e harness library (``feature-catalog.md`` § The two
    surfaces, the client-kept ``[nest]`` outcome ruling); the DNS page's app
    journeys are ``test_admin_dns_managed.py``.

    **What is real.** The nest, its matrix (one ``_atproto.<handle>`` row per
    active identity, derived from the user's current handle at read time), the
    rename (``fauna.profile.handle.change`` and the pending-action executor the
    background tick runs), and the machine — built as the apps build it and
    driven as the DNS page drives it (``fauna_ffi.harness_dns_dispatch``). The
    provider is the e2e fake, which mirrors every TXT it writes or withdraws
    into a name server as RFC 2136 UPDATEs (``e2e_acme_seam::mirror_fake_txt``);
    here that name server is an in-process ``FakeDns``, read as a resolver
    reads it. The remembered names travel between the two walks the way they
    travel between two app sessions: in the nest's DNS state plane.

    **What another tool wrote stays.** The credential plants a foreign TXT at
    the old name in the provider's zone (``fake-dns-ok:<zone>|<name>=<value>``)
    and the name server serves the same value — a verification token some other
    tool left there. The withdraw takes Fauna's ``did=`` value only."""
    from fakes.fake_dns import FakeDns

    old_name = f"_atproto.alice.{HANDLE_DOMAIN}"
    new_name = f"_atproto.alicerenamed.{HANDLE_DOMAIN}"
    foreign = "other-tool-verification=e2e-foreign"
    credential = {
        "provider_id": "cloudflare",
        "fields": [{"id": "api-token", "value": f"fake-dns-ok:{HANDLE_DOMAIN}|{old_name}={foreign}"}],
        "label": "cloudflare (e2e)",
    }
    zone = FakeDns(host="127.0.0.1", port=0).start()
    zone.add_txt(old_name, foreign)
    launch_env = {"FAUNA_E2E_ACME_NAMESERVER": f"127.0.0.1:{zone.port}"}
    try:
        with hosted_account(
            nest_binary, atproto_bridge_e2e_binary, tmp_path_factory, "atproto-rename-txt",
        ) as account:
            did_value = f"did={account.did}"
            nest_url = account.nest["url"]
            secret = bytes(account.nest["admin"]["signing_key"])

            names = _await_matrix(account, old_name)
            assert old_name in names, (
                f"the nest's DNS matrix must list alice's handle record {old_name!r} "
                f"once her identity is minted; it lists {sorted(names)}"
            )

            # The admin adds the credential and hands the primary to Fauna: the
            # opt-in publishes the matrix, alice's handle TXT with it.
            actions = [
                {"PutCredentials": credential},
                {"SetMode": {"domain": HANDLE_DOMAIN, "managed": True}},
            ]
            dispatched, error = fauna_ffi.harness_dns_dispatch(
                nest_url, secret, actions, launch_env=launch_env,
            )
            assert error == "" and dispatched == len(actions), (
                f"the managed opt-in stopped after {dispatched} of {len(actions)} "
                f"actions: {error}"
            )
            served = zone.served_txt(old_name)
            assert did_value in served and foreign in served, (
                f"the managed publish must serve alice's {did_value!r} at {old_name} "
                f"beside the foreign value; the zone serves {served!r}"
            )

            # Alice renames; the cool-off is fast-forwarded and the same
            # executor the tick runs applies it.
            with account.ws:
                account.ws.call("fauna.profile.handle.change", {"handle": "alicerenamed"})
            ran = _run_due_pending_actions(account)
            assert ran.get("executed", 0) >= 1, f"the handle change never applied: {ran}"
            names = _await_matrix(account, new_name, absent=old_name)
            assert new_name in names and old_name not in names, (
                f"after the rename the matrix must list {new_name!r} and not "
                f"{old_name!r}; it lists {sorted(names)}"
            )

            # The admin's next managed pass — a fresh machine, as a later app
            # session would build it.
            actions = [{"Publish": {"domain": HANDLE_DOMAIN}}]
            dispatched, error = fauna_ffi.harness_dns_dispatch(
                nest_url, secret, actions, launch_env=launch_env,
            )
            assert error == "" and dispatched == len(actions), (
                f"the managed publish after the rename failed: {error}"
            )
            assert did_value in zone.served_txt(new_name), (
                f"the renamed handle's TXT must be published at {new_name}; the zone "
                f"serves {zone.served_txt(new_name)!r}"
            )
            served = zone.served_txt(old_name)
            assert did_value not in served, (
                f"the old handle's {did_value!r} must be withdrawn from {old_name} so "
                f"the freed handle can be claimed; the zone still serves {served!r}"
            )
            assert served == [foreign], (
                f"the withdraw must take Fauna's value only and leave the foreign "
                f"record at {old_name}; the zone serves {served!r}"
            )
    finally:
        zone.stop()
        # The harness sets the seed in this process's environment; a later
        # walk must not mirror into a name server that is gone.
        os.environ.pop("FAUNA_E2E_ACME_NAMESERVER", None)
