"""tier_3: the nest-side contract of factory reset over the real socket
(``docs/goal/architecture/nest/common.md`` § Factory reset).

A reset is the Admin-gated ``fauna.admin.factory_reset``: the handler stages a
marker and exits, the supervisor restarts the nest, and the boot that follows
wipes deployment state before the database opens. A harness nest has no
supervisor, so ``common.nest.factory_reset_and_restart`` plays that part — the
same helper the calendar re-claim journey uses.

This file pins three outcomes of the ``factory-reset`` page:

- only an admin can reset the nest (a member's call is refused and nothing is
  staged);
- the nest serves the same certificate before and after a reset, so the admin
  can claim it again over it;
- straight after a reset nothing is served — the mail, calendar, contacts and
  files switches read off and their flag files are gone — until the nest is
  claimed and mail is switched on again.

Each test starts its own dedicated nest, since a reset wipes every other test's
state with it.
"""

import os
import ssl

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import claim_admin, create_actor_and_register
from common.nest import CLAIM_CODE, factory_reset_and_restart

pytestmark = pytest.mark.tier_3

#: The deployment flag files the supervisors read (`fauna_deployment_flags`):
#: presence of an enable flag keeps its role process serving.
_ENABLE_FLAGS = ("imap-enabled", "caldav-enabled", "carddav-enabled", "webdav-enabled")

_AXES = (
    ("fauna.bridges.set_mail_enabled", "mail_enabled"),
    ("fauna.bridges.set_caldav_enabled", "caldav_enabled"),
    ("fauna.bridges.set_carddav_enabled", "carddav_enabled"),
    ("fauna.bridges.set_webdav_enabled", "webdav_enabled"),
)


@pytest.fixture
def tls_reset_nest(request, nest_mode, tmp_path_factory):
    """A dedicated claimed nest serving its self-signed floor certificate over
    real HTTPS, so a test can read the certificate it presents."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reset-tls-nest", serve_tls=True
    )
    yield nest
    cleanup()


def _admin(nest):
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _status(nest) -> dict:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.setup.status", {})


def _served_cert(nest) -> str:
    return ssl.get_server_certificate(("127.0.0.1", nest["port"]))


def _flags_present(nest) -> list[str]:
    return [f for f in _ENABLE_FLAGS if os.path.exists(os.path.join(nest["tmp_dir"], f))]


@pytest.mark.feature("factory-reset")
def test_only_an_admin_can_factory_reset(reclaimable_nest):
    """factory-reset 4: a registered member's ``fauna.admin.factory_reset`` is
    refused, no reset marker is staged and the nest still reads claimed; an
    unauthenticated call is refused too. The admin's own call on the same nest
    is the control — it is accepted and the nest comes back unclaimed."""
    nest = reclaimable_nest
    member = create_actor_and_register(nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    with WsRpcAdminClient(
        nest["url"], actor_id=member["actor_id_bytes"], signing_key=bytes(member["signing_key"])
    ) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.admin.factory_reset", {"new_claim_code": CLAIM_CODE})
    assert "permission_denied" in excinfo.value.code, (
        f"a member's reset must be refused as a permission error; got {excinfo.value.code!r}"
    )
    with WsRpcAnonClient(nest["url"]) as anon:
        with pytest.raises(RpcCallError):
            anon.call("fauna.admin.factory_reset", {"new_claim_code": CLAIM_CODE})

    marker = os.path.join(nest["tmp_dir"], "factory-reset-requested")
    assert not os.path.exists(marker), "a refused reset must stage nothing"
    assert nest["proc"].poll() is None, "a refused reset must not stop the nest"
    assert _status(nest).get("claimed") is True, "the nest must still read claimed"

    factory_reset_and_restart(nest, reclaim=False)
    assert _status(nest).get("claimed") is False, "control: the admin's reset goes through"


@pytest.mark.feature("factory-reset")
def test_the_nest_keeps_its_certificate_through_a_reset(tls_reset_nest):
    """factory-reset 7: the certificate the nest presents after a reset is the
    one it presented before, and the admin claims the reset nest again over
    that same HTTPS listener."""
    nest = tls_reset_nest
    before = _served_cert(nest)

    factory_reset_and_restart(nest, reclaim=False)

    assert _served_cert(nest) == before, "the reset nest must present the same certificate"
    st = _status(nest)
    assert st.get("claimed") is False, f"the reset nest must read unclaimed: {st!r}"
    nest["admin"] = claim_admin(nest["port"], CLAIM_CODE, base_url=nest["url"])
    assert _status(nest).get("claimed") is True, "the admin claims it again over the same cert"


@pytest.mark.feature("factory-reset")
def test_nothing_is_served_after_a_reset_until_mail_is_switched_on(reclaimable_nest):
    """factory-reset 16: with mail, calendar, contacts and files all switched
    on, a reset leaves every enable flag file gone and the heartbeat reading
    mail off before anyone claims the nest; after the re-claim every switch
    reads off; switching mail back on brings mail and the three that follow it
    back, flag files included."""
    nest = reclaimable_nest
    with _admin(nest) as admin:
        for kind, _key in _AXES:
            assert admin.call(kind, {"enabled": True}).get("ok") is True, kind
    assert sorted(_flags_present(nest)) == sorted(_ENABLE_FLAGS), (
        "precondition: every switch on writes its flag file"
    )

    factory_reset_and_restart(nest, reclaim=False)

    assert _flags_present(nest) == [], "straight after a reset no service flag may remain"
    st = _status(nest)
    assert st.get("claimed") is False and st.get("email_enabled") is False, st

    nest["admin"] = claim_admin(nest["port"], CLAIM_CODE)
    with _admin(nest) as admin:
        cfg = admin.call("fauna.bridges.get_mail_config", {})
    for _kind, key in _AXES:
        assert cfg.get(key) is False, f"after the re-claim {key} must read off: {cfg!r}"
    assert _flags_present(nest) == [], "a claim alone switches nothing on"

    with _admin(nest) as admin:
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        cfg = admin.call("fauna.bridges.get_mail_config", {})
    for _kind, key in _AXES:
        assert cfg.get(key) is True, f"with mail on again, {key} follows it: {cfg!r}"
    assert "imap-enabled" in _flags_present(nest), "switching mail on serves mail again"
