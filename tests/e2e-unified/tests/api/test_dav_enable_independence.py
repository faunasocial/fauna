"""tier_3 API e2e: what ``get_mail_config``'s DAV-enable fields can and cannot
witness on a freshly-claimed nest.

``caldav_enabled`` is a **tri-state projected onto a bool**: the DB singleton is
``Option<bool>``, and an *unset* singleton falls back to ``mail_enabled``
(``assemble_fetch_config_reply`` — the one function behind BOTH the bridge's
``fauna.bridges.fetch_config`` and the admin twin ``fauna.bridges.get_mail_config``).
So a bare read of ``caldav_enabled`` **cannot distinguish "no client ever set it"
from "a client set it to exactly this value"** — the two collapse to the same
bool whenever the fallback happens to agree with the intent.

That ambiguity is not a bug (``caldav-server.md`` § Independent enablement
ratifies the fallback: "a mail-enabled box serves calendar out of the box until
the admin explicitly toggles CalDAV off"), but it is a **trap for any test that
reads ``caldav_enabled`` to assert what a client did**: on a freshly-claimed nest
``mail_enabled`` itself is unset and projects ``true`` (the pre-Stage-5 derived
"approved ⇒ enabled" fallback, pinned by ``fetch_config_returns_defaults_for_mta``),
so an untouched nest reports ``caldav_enabled=true`` with no client involvement
whatsoever. A test asserting "the client derived CalDAV ON" against that read is
**vacuously green**, and one asserting "the client derived CalDAV OFF" is
**unpassable**, on every app — see the sibling
``tests/test_caldav_onboarding_derived_enablement.py``, which this test's
disambiguation recipe (steps 2–3 below) is what makes honest.

Three assertions, all **independent of the Stage-5 default-off flip**
(``mail-bridge-lifecycle.md`` § Default-off — changing the unset ``mail_enabled``
fallback from ``true`` to ``false``); they hold before and after it, so this test
does not join the list of fixtures that flip has to rewrite:

1. untouched nest → every DAV toggle MIRRORS ``mail_enabled`` (the unset
   fallback), whatever ``mail_enabled`` itself projects to;
2. ``set_mail_enabled(false)`` → an unset ``caldav_enabled`` follows it to
   ``false`` — this is the probe that makes the read able to witness intent;
3. ``set_caldav_enabled(true)`` + ``set_mail_enabled(false)`` → CalDAV stays
   ``true`` while mail is ``false``, i.e. an explicitly-set toggle does NOT
   follow mail. That is ``caldav-server.md`` § Independent enablement's core
   claim ("independent of email", "a default, not a force-disable") asserted
   end-to-end over the real wire.

Claimed with a LOCAL handle (``localhost``) on purpose: that is the case where
§ 3b's client-side derivation fires **nothing**, so every value read here is the
nest's own projection with provably zero client intent behind it.
"""

import time

from common import CLAIM_CODE
from common.auth import port_base_url

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def dav_unclaimed_nest(request, nest_mode, tmp_path_factory):
    """An UNCLAIMED nest — the test drives the claim itself, with a local
    (``localhost``) handle, because a local claim is the case the module
    docstring is about.

    ``unclaimed`` is honoured in every mode (the docker provider declares it),
    so the claim ceremony under test is the same one in a container.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "dav-enable-nest", unclaimed=True)
    yield nest
    cleanup()


def _claim_local(port):
    """Claim the nest with a LOCAL (``localhost``) handle and return the admin
    signing key. Mirrors ``test_claim_primary_domain._claim``: fresh Ed25519
    keypair, signature over ``verify_key ‖ timestamp_be_u64``."""
    from nacl.signing import SigningKey

    sk = SigningKey.generate()
    timestamp = int(time.time())
    from common.sig_domain import claim_admin_signed_message

    sig = sk.sign(claim_admin_signed_message(bytes(sk.verify_key), timestamp)).signature

    with WsRpcAnonClient(port_base_url(port)) as anon:
        anon.call(
            "fauna.auth.claim_admin",
            {
                "claim_code": CLAIM_CODE,
                "actor_id": bytes(sk.verify_key).hex(),
                "signature": sig.hex(),
                "timestamp": timestamp,
                "handle": "test",
                "mail_domain": "localhost",
            },
        )
    return sk


def _admin(port, sk):
    return WsRpcAdminClient(
        port_base_url(port),
        actor_id=bytes(sk.verify_key),
        signing_key=bytes(sk),
    )


@pytest.mark.feature("admin-calendar-contacts-files")
def test_unset_dav_toggles_follow_mail_and_only_an_explicit_set_witnesses_intent(dav_unclaimed_nest):
    nest = dav_unclaimed_nest
    port = nest["port"]
    sk = _claim_local(port)

    # ── 1. Untouched nest: no client has called set_{mail,caldav}_enabled,
    # so every DAV singleton is unset and MIRRORS mail_enabled. Asserted as
    # an identity against mail_enabled (not against a hard-coded bool) so the
    # Stage-5 default-off flip, which only changes what mail_enabled itself
    # projects to, leaves this assertion true.
    with _admin(port, sk) as admin:
        cfg = admin.call("fauna.bridges.get_mail_config", {})
    mail = bool(cfg["mail_enabled"])
    # Reported, deliberately NOT asserted: which side of the Stage-5
    # default-off flip this tree is on. `true` here is what makes an
    # untouched nest report every DAV axis ON, and is the whole reason a bare
    # `caldav_enabled` read cannot witness client intent today; after the flip
    # it reads `false` and this test still passes unchanged.
    print(
        f"\n[dav-enable] untouched freshly-claimed nest: mail_enabled={mail} "
        f"(pre-flip fallback ⇒ True; post-Stage-5-flip ⇒ False)"
    )
    for axis in ("caldav_enabled", "carddav_enabled", "webdav_enabled"):
        assert bool(cfg[axis]) is mail, (
            f"an unset {axis} must fall back to mail_enabled={mail} "
            f"(assemble_fetch_config_reply); got {cfg[axis]!r}. This "
            f"fallback is why a bare {axis} read cannot witness client intent."
        )

    # ── 2. The disambiguating probe. Setting mail OFF explicitly pins the
    # fallback to a KNOWN false, so a still-unset caldav_enabled now reads
    # false — i.e. "no client ever enabled CalDAV" becomes observable.
    with _admin(port, sk) as admin:
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": False})
        cfg = admin.call("fauna.bridges.get_mail_config", {})
    assert bool(cfg["mail_enabled"]) is False, cfg
    assert bool(cfg["caldav_enabled"]) is False, (
        "with mail explicitly OFF, a never-set caldav_enabled must follow it "
        f"to false — that is what makes the read able to witness intent; got {cfg!r}"
    )

    # ── 3. Independent enablement (caldav-server.md § Independent
    # enablement): an EXPLICITLY set CalDAV toggle does not follow mail. Set
    # CalDAV on, then re-assert mail off, and CalDAV must hold its own value —
    # so a client that really did fire set_caldav_enabled(true) is
    # distinguishable from one that fired nothing, which is exactly the
    # discrimination step 2's probe buys.
    with _admin(port, sk) as admin:
        admin.call("fauna.bridges.set_caldav_enabled", {"enabled": True})
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": False})
        cfg = admin.call("fauna.bridges.get_mail_config", {})
    assert bool(cfg["mail_enabled"]) is False, cfg
    assert bool(cfg["caldav_enabled"]) is True, (
        "an explicitly set caldav_enabled must NOT follow mail_enabled — "
        f"CalDAV gates independently of email; got {cfg!r}"
    )
    # The sibling axes stayed unset, so they still follow mail — the
    # independence is per-axis, not a blanket detach.
    assert bool(cfg["carddav_enabled"]) is False, cfg
    assert bool(cfg["webdav_enabled"]) is False, cfg
