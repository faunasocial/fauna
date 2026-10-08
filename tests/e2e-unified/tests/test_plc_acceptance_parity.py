"""Acceptance parity at the fake directory boundary.

``test_plc_cid_parity.py`` pins that three implementations agree on canonical
dag-cbor and CID framing. This file pins the next layer up: an operation our
production signers emit is **accepted** by an independent validator, and an
operation no listed rotation key signed is **refused** — the fake directory
models `plc.directory`'s submission-time checks rather than merely recording
what it is handed (the acceptance-parity sharpening: the remedy
failing at the moment of use is exactly what a record-everything fake cannot
catch).

The three op literals below were minted by the Rust signer
(``plc_chain::tests::a_signed_chain_matches_the_python_fakes_acceptance_vector``
pins the same CIDs and signatures at the source). RFC6979 makes them
deterministic, so they are true cross-implementation vectors, not this file's
own fixture literals. The genesis and rename are
box-signed (K-256); the contest fork is user-signed (P-256), the exact shape
``recovery_fork::sign_fork_op`` submits, so both production curves are covered.

Only F5 live interop proves the real directory; this proves everything short
of it against an independent (Python ``cryptography``) verifier.
"""

import json
import urllib.error
import urllib.request

import pytest

from helpers.atproto_fakes import FakePlcDirectory, dag_cbor, op_cid

pytestmark = pytest.mark.tier_1

DID = "did:plc:nzz6j2ywvty3hhzbbtdqohd4"
GENESIS_CID = "bafyreidoopsowfvm6gzz6iimy4dry7fvmbilkzthsulpm546aes24y46n4"
RENAME_CID = "bafyreihe2dtt67pqwwsvif45vvhs2ebdurytfcqvjn3bed7bvuubmecmrm"
CONTEST_CID = "bafyreiam3bwoktmvoxgqpchwvzuxzwjlcmlg5vpvbcwgmhsye7lsp5dwzm"

USER_KEY = "did:key:zDnaejgmAHMLkBPMBWnkBxyGxpXx8LgE4WJAYDhwZzyoRAddF"  # P-256, gitleaks:allow
BOX_KEY = "did:key:zQ3shTFEGV8WixKXTA1kBgCkWsuHXxAeJrrYf57uA635Ma8ea"  # K-256, gitleaks:allow


def _op(prev, sig):
    return {
        "type": "plc_operation",
        "prev": prev,
        "rotationKeys": [USER_KEY, BOX_KEY],
        "verificationMethods": {"atproto": "did:key:zBoxSigningKey"},
        "alsoKnownAs": ["at://alice.example.com"],
        "services": {
            "atproto_pds": {
                "type": "AtprotoPersonalDataServer",
                "endpoint": "https://pds.example.com",
            }
        },
        "sig": sig,
    }


GENESIS_OP = _op(
    None,
    "vcWWWr4p-DlD5R1LF0wWaeCnqflKzfOSRExZgKSLnyFKuWExlMyGkLOviMIZlxUeWxT9VhvDgqiZ1S0-xn7saQ",
)
RENAME_OP = _op(
    GENESIS_CID,
    "kaEgv46zHf9KarsZLOXUMAjdItM-oUo-_ZpMpbJnwh8VkLRSluj64Y3McBoQH_pqVQtCNjPkWu1vHMCQR09F0w",
)
CONTEST_OP = _op(
    GENESIS_CID,
    "ufxIoIvzfaESgV8vTwAtQ6YIP7vn0QWw1KqTdEb-E1t88XC8AwF20n3MySLqGJKlwK64nLZjN_Gor5wcnfFKVg",
)


def _post(directory, did, op):
    """POST an op; return (status, decoded-json body) without raising."""
    req = urllib.request.Request(
        f"{directory.url}/{did}",
        data=json.dumps(op).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


@pytest.fixture
def directory():
    d = FakePlcDirectory()
    yield d
    d.close()


def _audit_cids(directory, did, standing_only=False):
    with urllib.request.urlopen(f"{directory.url}/{did}/log/audit") as resp:
        rows = json.loads(resp.read())
    return [r["cid"] for r in rows if not (standing_only and r["nullified"])]


def test_ops_the_production_signers_emit_are_accepted(directory):
    status, body = _post(directory, DID, GENESIS_OP)
    assert status == 200, f"the box-signed (K-256) genesis must be accepted: {body}"
    status, body = _post(directory, DID, RENAME_OP)
    assert status == 200, f"the box-signed (K-256) update must be accepted: {body}"
    assert _audit_cids(directory, DID) == [GENESIS_CID, RENAME_CID]


def test_the_user_signed_contest_fork_is_accepted_and_displaces(directory):
    _post(directory, DID, GENESIS_OP)
    _post(directory, DID, RENAME_OP)
    status, body = _post(directory, DID, CONTEST_OP)
    assert status == 200, f"the user-signed (P-256) fork must be accepted: {body}"
    assert directory.forks == [(DID, GENESIS_CID, [RENAME_CID])]
    assert _audit_cids(directory, DID, standing_only=True) == [GENESIS_CID, CONTEST_CID]


def test_a_tampered_operation_is_refused_and_changes_nothing(directory):
    _post(directory, DID, GENESIS_OP)
    tampered = dict(RENAME_OP)
    tampered["alsoKnownAs"] = ["at://mallory.example.com"]
    status, body = _post(directory, DID, tampered)
    assert status == 400, "a signature over different bytes must not be accepted"
    assert body["error"] == "InvalidSignature", body
    assert directory.sig_rejected, "the refusal must be recorded for diagnosis"
    assert _audit_cids(directory, DID) == [GENESIS_CID], "a refused op must not enter the log"


def test_a_tampered_genesis_is_refused_against_its_own_keys(directory):
    tampered = dict(GENESIS_OP)
    tampered["alsoKnownAs"] = ["at://mallory.example.com"]
    status, body = _post(directory, DID, tampered)
    assert status == 400, "a genesis must verify against its own rotationKeys"
    assert body["error"] == "InvalidSignature", body


def test_a_signer_the_prev_never_listed_is_refused(directory):
    # A correctly-signed op whose key the fork point never listed — signed
    # here with an independent Python signer, which is the point: the fake
    # must judge by the LISTED keys, not by whether the signature parses.
    from cryptography.hazmat.primitives import hashes
    from cryptography.hazmat.primitives.asymmetric import ec, utils as asn1

    key = ec.derive_private_key(0xDEADBEEF, ec.SECP256R1())
    unsigned = {k: v for k, v in CONTEST_OP.items() if k != "sig"}
    der = key.sign(dag_cbor(unsigned), ec.ECDSA(hashes.SHA256()))
    r, s = asn1.decode_dss_signature(der)
    n = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551  # P-256 order
    if s > n // 2:
        s = n - s
    import base64

    sig = base64.urlsafe_b64encode(r.to_bytes(32, "big") + s.to_bytes(32, "big")).decode().rstrip("=")
    unlisted = dict(unsigned, sig=sig)

    _post(directory, DID, GENESIS_OP)
    _post(directory, DID, RENAME_OP)
    status, body = _post(directory, DID, unlisted)
    assert status == 400, "a key the fork point never listed must not authorize a fork"
    assert body["error"] == "InvalidSignature", body
    assert directory.forks == [], "the refused fork must not displace anything"


def test_a_high_s_signature_is_refused_like_the_real_directory(directory):
    # ECDSA is malleable: (r, n−s) verifies wherever (r, s) does. plc.directory
    # refuses the high-S twin at submission (atproto's low-S requirement), and
    # a fake that accepted it would green a signer the real directory rejects —
    # the exact acceptance-parity gap this file exists to close. Note the Rust
    # client's read-side `verify_op_sig` deliberately ACCEPTS high-S (published
    # history is not ours to refuse); the acceptor and the reader differ by
    # design, and this pin is what keeps the fake on the acceptor's side.
    import base64

    raw = base64.urlsafe_b64decode(CONTEST_OP["sig"] + "==")
    r, s = int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big")
    n = 0xFFFFFFFF00000000FFFFFFFFFFFFFFFFBCE6FAADA7179E84F3B9CAC2FC632551  # P-256 order
    high_s = base64.urlsafe_b64encode(r.to_bytes(32, "big") + (n - s).to_bytes(32, "big")).decode().rstrip("=")
    malleated = dict(CONTEST_OP, sig=high_s)

    _post(directory, DID, GENESIS_OP)
    _post(directory, DID, RENAME_OP)
    status, body = _post(directory, DID, malleated)
    assert status == 400, "the high-S twin must be refused at the acceptance boundary"
    assert body["error"] == "InvalidSignature", body
    assert "high-S" in body["message"], f"the refusal must name the malleation: {body}"


def test_the_pinned_cids_are_the_ops_own_hashes():
    # The literals above bind to each other: each pinned CID is the hash of its
    # pinned op. If someone edits an op literal without re-minting, this fails
    # before any acceptance test confuses the matter.
    assert op_cid(GENESIS_OP) == GENESIS_CID
    assert op_cid(RENAME_OP) == RENAME_CID
    assert op_cid(CONTEST_OP) == CONTEST_CID
