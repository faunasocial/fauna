"""A store reviewer's session on a standalone nest seeded the way the staging
boxes are — the inner-loop twin of ``test_store_reviewer_journey.py``.

The live journey runs only on a seeded staging box (`testing.md` § The
shared-box rule), so nothing exercised a reviewer's session on an ordinary
run. This one does, on every machine: the same walk
(``helpers/store_reviewer_walk.py``, the reviewer script and its blast radius)
over the same seeding (``helpers.org_account_session.seed_accounts``, the code
path the staging-box provisioning drive seeds `dev.fauna.build`,
`test.fauna.build` and the acceptance nest with — `org-nests.md`
§ fauna.build → *Accounts*), so a first run a store reviewer would meet is the
first run this test meets.

The setup, all of it through the app's UI except the nest itself:

* **The nest** is ``self_signed_nest`` — a dedicated nest serving real HTTPS on
  loopback. A reviewer signs in by handle, and a handle resolves to an https
  URL the authenticated connection then dials, so the shared plain-HTTP
  ``nest_instance`` cannot carry a handle sign-in (the reasoning
  ``test_recovery_kit_create_journey._sign_in_as`` records). Every account's
  handle domain is the nest's own ``host:port``.
* **The accounts** — the three reviewers and the tester, under the org local
  parts (``helpers.staging_box.REVIEWER_ACCOUNTS`` / ``SEEDING_ACCOUNT``) — are
  minted in memory, never written under ``~/.config``, and admitted by the
  nest's admin on Admin → Users (``org_account_session.admit_accounts``), as
  the drive admits them.
* **The seeding** is the drive's, step for step: the tester asks each
  reviewer to connect and posts, each reviewer accepts and waits for its
  starter keys, the tester messages each reviewer.
* **The conversations are real** (the ``real_conversations`` marker): a
  reviewer's starter keys are published by the shared receive loop, and the
  tester's message reaches the reviewer over real MLS. The native apps start
  that loop under e2e only behind the marker's launch gate — without it a
  windows reviewer publishes no keys and the seeding waits on a count of 0.

Minted and seeded once per nest (the first test pays for it), since the walk
leaves the seeding as it found it; every app of a sweep signs in as the same
accounts, as every app does on a staging box.

web declares the whole module absent: a browser cannot reach a self-signed
nest's raw socket (``conftest._declare_no_web_cert_trust``).

Test taxonomy: tier_3 — locally built nest and app, the real UI end to end.
"""

from __future__ import annotations

import secrets
from urllib.parse import urlsplit

import pytest

from helpers.org_account_session import (
    admit_accounts,
    seed_accounts,
    sign_in_fresh,
    sign_out,
)
from helpers.staging_box import REVIEWER_ACCOUNTS, actor_hex_for, witness_tester_localpart
from helpers.store_reviewer_walk import ACCOUNTS, Nest, run_reviewer_journey

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

#: nest url → the four accounts, minted once per nest: the nest admits a
#: handle once, so a retry or a second app must sign in as the same keys.
_ACCOUNTS: dict[str, dict[str, dict]] = {}
#: nest url → "seeded", or why the seeding failed (later tests fail fast on it
#: rather than seeding again over a half-seeded nest).
_SEEDING: dict[str, str] = {}


def _nest_domain(nest: dict) -> str:
    """The name the nest gives itself on `fauna.nest.info` — what the Status
    page says."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.nest.info", {})["domain"]


@pytest.fixture
def seeded_reviewers(request, app, self_signed_nest):
    """``(accounts, nest)`` on ``self_signed_nest``: the four org accounts
    minted, admitted and seeded, and ``app`` holding no session."""
    from conftest import _declare_no_web_cert_trust, _login_admin_as, _relaunch_trusting_nest

    _declare_no_web_cert_trust(app)
    where = urlsplit(self_signed_nest["url"]).netloc
    nest = Nest(where=where, status_domain=_nest_domain(self_signed_nest))
    url =self_signed_nest["url"]
    accounts = _ACCOUNTS.setdefault(url, {
        lp: {"localpart": lp, "secret_hex": secrets.token_bytes(32).hex()} for lp in ACCOUNTS})
    if url not in _SEEDING:
        try:
            _login_admin_as(app, request, self_signed_nest, spa_url_fixture="spa_url",
                            fixture_name="seeded_reviewers")
            admit_accounts(app, accounts)
            seed_accounts(app, accounts, where, signed_in=False, bare_localparts=True)
            sign_out(app)
        except BaseException as e:
            _SEEDING[url] = f"seeding the reviewer accounts failed: {e!r}"
            raise
        _SEEDING[url] = "seeded"
    elif _SEEDING[url] != "seeded":
        pytest.fail(_SEEDING[url], pytrace=False)
    else:
        # The accounts are the nest's, not an app's: a later test (or app) signs
        # in as them. A relaunch since the seeding named the shared nest only.
        _relaunch_trusting_nest(app.driver, self_signed_nest)
    return accounts, nest


@pytest.mark.parametrize("reviewer", REVIEWER_ACCOUNTS)
def test_a_store_reviewer_can_do_what_a_reviewer_checks_on_a_seeded_nest(
    app, seeded_reviewers, reviewer
):
    accounts, nest = seeded_reviewers
    run_reviewer_journey(app, accounts, reviewer, nest)


def test_a_witness_tester_admitted_by_public_key_alone_signs_in(request, app, self_signed_nest):
    """The witness hosts' testers (`org-nests.md` § fauna.build → *Accounts*):
    the drive admits a store-install witness machine's own tester from the
    tracked registry — public key and handle, no seed — and that machine then
    signs in as it through the app's own UI with the seed it kept. The same
    admission path (``admit_accounts`` over an ``actor_hex``-only record) on a
    standalone nest, the seed held here in memory as the witness host holds
    its own."""
    from conftest import _declare_no_web_cert_trust, _login_admin_as

    _declare_no_web_cert_trust(app)
    where = urlsplit(self_signed_nest["url"]).netloc
    localpart = witness_tester_localpart()
    seed = secrets.token_bytes(32).hex()
    _login_admin_as(app, request, self_signed_nest, spa_url_fixture="spa_url",
                    fixture_name="witness_tester")
    # What the registry carries: the public half only.
    admit_accounts(app, {localpart: {"localpart": localpart, "actor_hex": actor_hex_for(seed)}})
    # What the witness host does with the seed it kept: a fresh app instance,
    # import the identity, type the handle, continue — to Connected.
    sign_in_fresh(app, {"localpart": localpart, "secret_hex": seed}, where, signed_in=True)
    sign_out(app)
