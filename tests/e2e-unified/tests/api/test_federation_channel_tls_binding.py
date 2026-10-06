"""tier_3: the nest↔nest federation channel over **real TLS**, and the channel
binding the handshake carries.

``fauna.federation.hello`` carries ``spki_sha256`` — the SHA-256 of the
``SubjectPublicKeyInfo`` of the certificate the **listener** is serving — and the
listener refuses the handshake outright unless the initiator's claimed value
equals the one it actually serves (``federation_channel.rs::
verify_hello_and_build_reply``, check 1, before any signature is even parsed).
That check is what defeats a MITM relaying the channel through a different TLS
endpoint: authentication of *which* nest is the peer comes from the discovered
``nest_id``, and authentication of *which pipe* is the peer's comes from this.

The product's initiator never computes that value from anything it knows about
the peer. It **observes** the leaf off the very connection it is about to speak
the hello on — ``connect_federation_ws``'s capturing rustls verifier records the
served leaf during that connection's own handshake, and hands the fingerprint
back beside the socket.

**Why this file exists.** Until it did, the harness's ``FederationChannelClient``
hard-coded the empty string — the fingerprint a plain-HTTP loopback nest serves
— so it could dial a ``ws://`` peer and nothing else. Every federation test in
the suite is such a peer, which is precisely why the gap was invisible: the one
posture the constant cannot express is the one every *deployment artifact*
actually serves. A nest started from the released image serves TLS always, so
the first federation test admitted to docker mode would have failed on
``HelloError::SpkiMismatch`` against product code that was working correctly
(``testing.md`` § Default app and nest mode — the mode axis exists to stop
exactly that class of ❌).

**Why tier_3 and not a unit test.** The number under test is agreement between
two independent implementations of one fingerprint: the nest computes it in Rust
from ``tbs_certificate.subject_pki.raw`` (``libs/fauna-protocol/src/
tls_spki.rs``), this side computes it in Python from a re-encode of the parsed
key (``helpers/tls_spki.py``). Only a real handshake between them can pin that
they agree — a unit test on either side would encode the same bytes twice and
drift together, the same argument ``test_federation_hello_envelope_verify.py``
makes for the dag-cbor envelope.

Mode-independent by construction: ``serve_tls`` is a declared start option in
both standalone and docker (``testing.md`` ruling (3)), so these nests are the
mode provider's to start and this file collects in either.
"""

import pytest

from clients.ws_rpc_federation_client import FederationChannelClient, RpcCallError
from helpers.tls_spki import served_leaf_spki_sha256_hex

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def tls_federation_peers(request, nest_mode, tmp_path_factory):
    """Two dedicated nests, both serving real HTTPS on their self-signed floor.

    ``serve_tls=True`` drops the harness-wide ``FAUNA_INSECURE_DISABLE_TLS``
    plain-HTTP escape for these two nests only, so their API listeners — and
    therefore ``/api/v1/federation/ws`` — are ``wss://`` with a live
    ``served_cert_spki`` behind them. That is the deployment posture; the rest
    of the suite's federation tests run the loopback one.
    """
    from conftest import _start_dedicated_nest

    initiator, cleanup_initiator = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "fed-tls-initiator", serve_tls=True
    )
    try:
        listener, cleanup_listener = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "fed-tls-listener", serve_tls=True
        )
        try:
            yield initiator, listener
        finally:
            cleanup_listener()
    finally:
        cleanup_initiator()


def _authority(url: str) -> tuple:
    host, _, port = url.split("://", 1)[1].rpartition(":")
    return host, int(port)


def test_the_channel_binds_to_the_cert_the_listener_actually_serves(
    tls_federation_peers,
):
    """A TLS peer's hello carries the served leaf's SPKI, and the nest agrees.

    Three things fail here if the initiator guesses instead of observing, and
    they fail in a way no other test in the suite can:

    1. the connection is refused at the handshake (``SpkiMismatch``) — so
       reaching the call at all proves the claimed fingerprint matched;
    2. the observed value is checked against an independent read of the same
       listener, so a client that "matched" by echoing something the nest sent
       it would still be caught;
    3. the value is non-empty, which is what separates this posture from every
       other federation test in the suite.
    """
    initiator, listener = tls_federation_peers

    assert listener["url"].startswith("https://"), (
        "this file's whole premise is a TLS listener; a plain-HTTP nest serves "
        f"the empty fingerprint and proves nothing. Got {listener['url']!r}"
    )
    host, port = _authority(listener["url"])
    served = served_leaf_spki_sha256_hex(host, port, host)
    assert len(served) == 64, f"expected a sha256 hex fingerprint, got {served!r}"

    with FederationChannelClient(initiator=initiator, target=listener) as fed:
        assert fed.observed_spki_sha256 == served, (
            "the channel binding the initiator signed must be the fingerprint "
            "of the leaf this listener serves — the whole point is that it came "
            f"off the handshake, not from a constant. Signed {fed.observed_spki_sha256!r}, "
            f"served {served!r}"
        )

        # Past the handshake: a live federation kind, refused by the *handler*
        # rather than the channel. These nests were never paired, so
        # `is_paired(actor, initiator_nest_id)` is false — and that verdict is
        # only reachable once the hello verified and the connection was
        # attributed to a peer nest_id. A channel that merely opened could not
        # produce it. (Same witness as `test_namespace_sync.py::
        # test_sync_pull_rejected_when_not_paired`, over TLS.)
        with pytest.raises(RpcCallError) as exc_info:
            fed.call(
                "fauna.federation.sync.pull",
                {"actor_id": "cc" * 32, "namespace": "dd" * 32, "since": 0},
            )
        assert exc_info.value.code == "fauna.federation.forbidden", (
            "expected the unpaired-peer refusal from the sync.pull handler; a "
            "different code means the channel did not reach the handler at all: "
            f"{exc_info.value!r}"
        )


def test_the_binding_is_whatever_this_runs_listener_posture_serves(two_nodes):
    """The general invariant, over whichever posture the run's mode provides.

    Stated as an invariant rather than as a hand-picked outcome (convention 17):
    *the fingerprint the initiator signs is the one the listener serves* — the
    empty string when it serves no certificate at all, the served leaf's SPKI
    when it does. Both branches are real postures, not a value and a fallback:
    ``serve_listener`` derives the listener's side as
    ``current_spki_sha256()…unwrap_or_default()``.

    Which branch this run exercises is the mode's answer, and that is the point
    of asserting it this way — standalone's plain-HTTP nests pin the empty
    branch (and would catch an initiator that started volunteering a non-empty
    value to a plain peer, breaking every federation test at once), while the
    same test under ``--nest docker`` pins the TLS branch against the released
    image's own certificate.
    """
    nest_a, nest_b = two_nodes["nest_a"], two_nodes["nest_b"]
    url = nest_b["url"]
    if url.startswith("http://"):
        expected = ""
    else:
        host, port = _authority(url)
        expected = served_leaf_spki_sha256_hex(host, port, host)
        assert len(expected) == 64, f"expected a sha256 hex, got {expected!r}"

    with FederationChannelClient(initiator=nest_a, target=nest_b) as fed:
        assert fed.observed_spki_sha256 == expected, (
            f"the initiator must sign the fingerprint {url} actually serves: "
            f"signed {fed.observed_spki_sha256!r}, served {expected!r}"
        )
