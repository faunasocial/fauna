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
  (``atproto-pds-bridge.md`` § Disable & revocation).

Posts are written over WS-RPC, the same pragmatic exception
``test_atproto_firehose_post.py`` takes: what is witnessed is the nest's
publication, and the client-UI journey that writes a post is
``test_feed.py::test_create_post``.
"""

import time

import blake3
import pytest

import fauna_ffi
from helpers.atproto_hosted import PDS_HOST, hosted_account
from helpers.blob_upload import SEALED_MIME, upload_blob
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
