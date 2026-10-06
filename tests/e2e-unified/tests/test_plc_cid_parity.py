"""The fake PLC directory's dag-cbor/CID derivation must match the client's.

Three implementations have to agree on canonical dag-cbor for a recovery
contest to work at all — the bridge (Go) mints an operation and its CID, the
directory serves it, and the client (Rust, ``fauna-client-atproto::plc_chain``)
recomputes that CID to bind the operation's *content* to the identifier its
fork will name as ``prev``.

So the agreement is pinned rather than assumed. These literals are the same
ones asserted in
``plc_chain::tests::a_realistic_operations_cid_matches_the_other_implementations``;
if either side's map-key ordering or multihash framing drifts, one of the two
tests fails — a class of bug that is invisible in any single-language test, and
that would present as "the contest mysteriously refuses to plan".
"""

import pytest

from helpers.atproto_fakes import dag_cbor, op_cid, plc_did

pytestmark = pytest.mark.tier_1

# A realistic `plc_operation`: every field the bridge mints, including the
# nested `services` entry and a null `prev` (a genesis).
OP = {
    "type": "plc_operation",
    "prev": None,
    "rotationKeys": ["did:key:zA", "did:key:zB"],
    "verificationMethods": {"atproto": "did:key:zC"},
    "alsoKnownAs": ["at://alice.example.com"],
    "services": {
        "atproto_pds": {
            "type": "AtprotoPersonalDataServer",
            "endpoint": "https://pds.example.com",
        }
    },
    "sig": "AAAA",
}

CID = "bafyreifracj42ynhwtl2qp2ed6x3zemr76g23tdmahn4gphc3xtdn3bp2u"
DID = "did:plc:weajhtlbu62npkb7iqp27per"


def test_operation_cid_matches_the_rust_client():
    assert op_cid(OP) == CID


def test_genesis_derives_the_same_did_as_the_rust_client():
    assert plc_did(OP) == DID


def test_map_keys_sort_length_first_then_bytewise():
    # The ordering rule itself, stated independently of the vector above: a
    # plain bytewise sort would put "alsoKnownAs" before "prev", and every CID
    # in the fake would then disagree with every CID the client computes.
    encoded = dag_cbor({"prev": "a", "type": "b", "alsoKnownAs": ["c"]})
    assert encoded.index(b"prev") < encoded.index(b"type") < encoded.index(b"alsoKnownAs")


def test_dag_cbor_refuses_floats():
    # dag-cbor forbids them, and silently encoding one would mint a CID no
    # other implementation agrees with.
    with pytest.raises(TypeError):
        dag_cbor({"x": 1.5})
