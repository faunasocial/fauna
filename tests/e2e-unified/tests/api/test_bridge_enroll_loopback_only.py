"""tier_3 witness: only a process on the nest's own machine can ask to enroll as
a bridge (``docs/goal/behavior/mail-bridge-lifecycle.md`` § Lifecycle phases).

``fauna.bridges.request_enrollment`` is an anonymous, loopback-gated kind: the
dispatcher refuses it from any peer whose source IP is not loopback with
``fauna.bridges.remote_enrollment_unsupported``
(``bins/fauna-nest/src/routes.rs`` step (1c); in-process pins
``request_enrollment_refused_from_non_loopback_peer`` and
``..._from_loopback_peer_passes_the_gate``). This file proves it over the real
socket, with the "remote" arranged honestly: ``spki_pinned_nest`` is a
dedicated nest serving TLS on a wide bind, dialled on the box's own LAN IP, so
the kernel gives the connection that LAN address as its source — a genuinely
non-loopback peer — while the very same listener, dialled on 127.0.0.1, is the
loopback caller. Same port, same TLS listener, only the peer differs.
"""

import secrets
from urllib.parse import urlsplit, urlunsplit

import pytest

from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

pytestmark = pytest.mark.tier_3

REFUSED = "fauna.bridges.remote_enrollment_unsupported"


def _enroll(url: str, pubkey: bytes):
    with WsRpcAnonClient(url) as anon:
        return anon.call(
            "fauna.bridges.request_enrollment",
            {
                "ed25519_pubkey": pubkey,
                "role_hint": "mta",
                "bridge_id": f"mta-{secrets.token_hex(4)}",
            },
        )


def _loopback_url(url: str) -> str:
    parts = urlsplit(url)
    return urlunsplit(parts._replace(netloc=f"127.0.0.1:{parts.port}"))


@pytest.mark.feature("admin-bridges")
def test_a_remote_caller_cannot_ask_to_enroll_as_a_bridge(spki_pinned_nest):
    """A caller arriving from the box's non-loopback address (over the nest's
    TLS listener) is refused."""
    url = spki_pinned_nest["url"]
    assert urlsplit(url).hostname != "127.0.0.1", "fixture must be dialled non-loopback"

    with pytest.raises(RpcCallError) as excinfo:
        _enroll(url, secrets.token_bytes(32))
    assert excinfo.value.code == REFUSED, (
        f"a non-loopback peer must be refused enrollment; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-bridges")
def test_a_process_on_the_nests_own_machine_can_ask_to_enroll_as_a_bridge(spki_pinned_nest):
    """Control for the refusal above: the same listener, dialled on loopback,
    admits the same call and answers an enrollment status — so the refusal is
    the peer's address and nothing else."""
    reply = _enroll(_loopback_url(spki_pinned_nest["url"]), secrets.token_bytes(32))
    assert reply["status"] in ("pending", "approved"), reply
