"""tier_3 API e2e: what a visitor is served while the nest re-renders a site,
and a labelled post leaving the site — read from outside, as a stranger's
browser sees it (plain HTTP with the author's subdomain as ``Host``).

Two outcomes of ``docs/features/personal-website.md``:

* **13 — a whole old site or a whole new one, never a half; past the
  withdrawal deadline, dark rather than partial** (``web-content-hosting.md``
  § Routing, render, serving → *A reader sees a whole site or none*). A render
  replaces both rendered stores in one transaction as its last act
  (``db/web.rs::replace_web_rendered``); a render answering an owed revoke that
  has not committed within the withdrawal deadline clears the site under its
  own claim and carries on, still committing whole.
* **14 — a post that receives a moderation label leaves the published site**
  (§ Implementation status today, *the label's arrival*). The label write marks
  its publisher's render owed in its own transaction
  (``db/moderation.rs::upsert_content_label``) and ``fauna.labels.attach`` pays
  it, so the render folds the new label through the publisher policies in
  force — here the nest's own rules (``nest-rules.md`` § The publisher leg), a
  category rule on ``spam``.

**How "while a render runs" is observed without racing it** (convention 14).
An ordinary site renders in milliseconds, so fetching pages around a re-render
proves nothing. The ``test-hooks`` door ``/api/v1/test/web/render-park``
(``web_render_park_test_hook.rs``) holds the actor's next REAL render at its
first body write — past its claim, its deadline race and its listing — until
released; the test waits on the park's own state, reads the site, then lets the
render finish. Its ``deadline_ms`` states which side of the withdrawal deadline
the render is on (zero: past it before the first page), the seam the Rust pins
take through ``with_withdrawal_deadline``, so nothing waits out the 10 s
constant. The door that triggers the render (``publish.set`` / ``.unset``)
answers only once its render has, so it runs on a thread of its own.

What stays Rust-pinned (``conformance_web.rs``): the replacement's atomicity
inside one transaction, which no outside reader can park between (the goal
doc's *What no pin observes* (1)), and the production deadline's value.

Dedicated nest: a claimed domain for the subdomain host, and nest rules bind
every member, so setting them on the session-shared nest would change what
every sibling test sees.
"""

from __future__ import annotations

import html
import secrets
import threading
import urllib.error
import urllib.request

import pytest

from common.auth import open_registration
from helpers import budgets
from helpers.admin_wire import admit_adult
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

NEST_DOMAIN = "atomic.example"


@pytest.fixture(scope="module")
def render_atomicity_site_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-web-atomic", claim_domain=NEST_DOMAIN
    )
    open_registration(nest)
    try:
        yield nest
    finally:
        cleanup()


def _get(nest, path: str, host: str) -> tuple[int, str]:
    req = urllib.request.Request(nest["url"].rstrip("/") + path, headers={"Host": host})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status, html.unescape(resp.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as err:
        return err.code, html.unescape(err.read().decode("utf-8", "replace"))


def _writer(nest) -> tuple[dict, str]:
    """A fresh member with a live subdomain site; returns (actor, host)."""
    handle = f"w{secrets.token_hex(4)}"
    actor = admit_adult(nest, handle, domain=NEST_DOMAIN, token=True)
    with _actor_client(nest["url"], actor) as ws:
        ws.call("fauna.web.set_subdomain_enabled", {"enabled": True})
    return actor, f"{handle}.{NEST_DOMAIN}"


def _post(nest, actor, text: str, offset_us: int) -> str:
    post = sign_and_encode_post(actor["signing_key"], 1_760_000_000_000_000 + offset_us, text)
    return ws_api.create_post(nest["port"], actor, post)


def _park(nest, actor, action: str, **extra) -> dict:
    from conftest import _bridge_admin_post

    reply = _bridge_admin_post(
        nest["url"],
        nest["admin"]["token"],
        "/api/v1/test/web/render-park",
        {"actor_id": actor["actor_id_hex"], "action": action, **extra},
    )
    assert reply.get("ok"), f"the render-park door refused {action!r}: {reply}"
    return reply


class _Door(threading.Thread):
    """Run one web door on a client of its own: it answers only once the render
    it triggers has, and that render is parked."""

    def __init__(self, nest, actor, kind: str, payload: dict):
        super().__init__(daemon=True)
        self.nest, self.actor, self.kind, self.payload = nest, actor, kind, payload
        self.error: BaseException | None = None

    def run(self) -> None:
        try:
            with _actor_client(self.nest["url"], self.actor) as ws:
                ws.call(self.kind, self.payload)
        except BaseException as err:  # noqa: BLE001 — reported by finish()
            self.error = err

    def finish(self) -> None:
        self.join(budgets.RPC_ROUNDTRIP_S)
        assert not self.is_alive(), f"{self.kind} never answered after its render was released"
        assert self.error is None, f"{self.kind} failed: {self.error!r}"


def _wait_parked(nest, actor, door: _Door) -> None:
    def parked():
        assert door.error is None, f"{door.kind} failed before its render parked: {door.error!r}"
        return _park(nest, actor, "status")["parked"]

    wait_until(
        parked,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"no render of the site reached the park; {door.kind} alive={door.is_alive()}",
    )


def _site(nest, host: str, slugs: list[str]) -> dict[str, tuple[int, str]]:
    """Every page a visitor can ask for: the front page, the feed, each post."""
    pages = {"/": _get(nest, "/", host), "/feed.xml": _get(nest, "/feed.xml", host)}
    for slug in slugs:
        pages[f"/post/{slug}"] = _get(nest, f"/post/{slug}", host)
    return pages


def _assert_generation(pages, *, present: dict[str, str], absent: dict[str, str], when: str):
    """The site is exactly one generation: every `present` post's page serves
    its words and the front page and feed list it; every `absent` post's page
    does not serve and neither listing carries it."""
    for slug, words in present.items():
        status, body = pages[f"/post/{slug}"]
        assert status == 200 and words in body, f"{when}: /post/{slug} → {status}: {body[:400]}"
        for listing in ("/", "/feed.xml"):
            status, body = pages[listing]
            assert status == 200 and words in body, (
                f"{when}: {listing} does not list {slug!r} ({status}): {body[:600]}"
            )
    for slug, words in absent.items():
        status, body = pages[f"/post/{slug}"]
        assert status != 200 and words not in body, f"{when}: /post/{slug} → {status}: {body[:400]}"
        for listing in ("/", "/feed.xml"):
            assert words not in pages[listing][1], f"{when}: {listing} lists {slug!r}"


@pytest.mark.feature("personal-website")
def test_a_visitor_sees_the_whole_old_site_or_the_whole_new_one_and_a_withdrawn_site_goes_dark(
    render_atomicity_site_nest,
):
    nest = render_atomicity_site_nest
    actor, host = _writer(nest)
    tag = secrets.token_hex(3)
    words = {s: f"Words of {s} {tag}" for s in ("first", "second", "third")}
    slugs = list(words)

    first = _post(nest, actor, words["first"], 0)
    second = _post(nest, actor, words["second"], 1_000_000)
    third = _post(nest, actor, words["third"], 2_000_000)
    ws_api.web_publish_set(nest["port"], actor, first, "first")
    ws_api.web_publish_set(nest["port"], actor, second, "second")
    _assert_generation(
        _site(nest, host, slugs),
        present={"first": words["first"], "second": words["second"]},
        absent={"third": words["third"]},
        when="baseline",
    )

    # ── A render that adds a post, held in flight: the old site, whole. ──
    _park(nest, actor, "arm")
    door = _Door(nest, actor, "fauna.web.publish.set", {"post_id": bytes.fromhex(third), "slug": "third"})
    door.start()
    try:
        _wait_parked(nest, actor, door)
        _assert_generation(
            _site(nest, host, slugs),
            present={"first": words["first"], "second": words["second"]},
            absent={"third": words["third"]},
            when="mid-render (the new post's render parked)",
        )
    finally:
        _park(nest, actor, "release")
    door.finish()
    _assert_generation(
        _site(nest, host, slugs),
        present=words,
        absent={},
        when="after the render committed",
    )

    # ── A revoking render past its withdrawal deadline: dark, never partial. ──
    _park(nest, actor, "arm", deadline_ms=0)
    door = _Door(nest, actor, "fauna.web.publish.unset", {"post_id": bytes.fromhex(second)})
    door.start()
    try:
        _wait_parked(nest, actor, door)
        dark = _site(nest, host, slugs)
        for path, (status, body) in dark.items():
            for slug, text in words.items():
                assert text not in body, (
                    f"past the deadline the site is dark, yet {path} ({status}) still "
                    f"carries {slug!r} — a partial site: {body[:400]}"
                )
        for slug in slugs:
            status, body = dark[f"/post/{slug}"]
            assert status != 200, f"past the deadline /post/{slug} still serves: {body[:400]}"
    finally:
        _park(nest, actor, "release")
    door.finish()
    _assert_generation(
        _site(nest, host, slugs),
        present={"first": words["first"], "third": words["third"]},
        absent={"second": words["second"]},
        when="after the revoking render committed",
    )


@pytest.mark.feature("personal-website")
def test_a_post_that_receives_a_moderation_label_leaves_the_published_site(render_atomicity_site_nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    nest = render_atomicity_site_nest
    actor, host = _writer(nest)
    tag = secrets.token_hex(3)
    labelled_words, clean_words = f"Labelled words {tag}", f"Clean words {tag}"
    labelled = _post(nest, actor, labelled_words, 0)
    clean = _post(nest, actor, clean_words, 1_000_000)
    ws_api.web_publish_set(nest["port"], actor, labelled, "labelled")
    ws_api.web_publish_set(nest["port"], actor, clean, "clean")

    # The nest's rules block what is labelled spam. A rule alone touches no
    # post: nothing is labelled yet.
    sk = nest["admin"]["signing_key"]
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as ws:
        reply = ws.call("fauna.admin.content_policy.set", {"document": {
            "version": 2,
            "rules": [{
                "factor": "spam",
                "min_permille": 500,
                "verdict": "block",
                "reason_code": "no-spam",
                "reason": {"default": "No spam on this nest."},
            }],
        }})
    assert reply.get("policy") and not reply.get("defect"), reply
    _assert_generation(
        _site(nest, host, ["labelled", "clean"]),
        present={"labelled": labelled_words, "clean": clean_words},
        absent={},
        when="rules in force, no label yet",
    )

    # The label arrives, from the post's own author — one of the producers
    # `fauna.labels.attach` admits — and the post leaves every page of the site.
    stored = ws_api.attach_labels(
        nest["port"], actor, labelled, [{"category": "spam", "confidence_per_mille": 900}]
    )
    assert stored == 1, stored

    pages = _site(nest, host, ["labelled", "clean"])
    for path, (status, body) in pages.items():
        assert labelled_words not in body, (
            f"the labelled post is still on the published site at {path} ({status}): {body[:400]}"
        )
    status, body = pages["/post/clean"]
    assert status == 200 and clean_words in body, f"the unlabelled post went too: {status} {body[:400]}"
    for listing in ("/", "/feed.xml"):
        assert clean_words in pages[listing][1], f"{listing} lost the unlabelled post"
