"""The owner's audience attestation, verified the way a seat verifies it
(`encryption-at-rest.md` § Readable classes → *The declassification is
owner-ATTESTED*). Shared by the tests that assert an app's answered
``folder-audience-public-confirm`` landed a genuine attestation on the nest row.
"""

from nacl.exceptions import BadSignatureError
from nacl.signing import VerifyKey

# `fauna_protocol::sig_domain::FOLDER_AUDIENCE_ATTESTATION_V1`, byte for byte
# (the trailing NUL is part of the tag).
ATTESTATION_TAG = b"fauna.folders.audience-attestation.v1\0"


def attestation_message(owner: bytes, folder_id: int, name: str, counter: int) -> bytes:
    """`fauna_protocol::folders::audience_attestation_signed_message`, rebuilt
    here from the VERIFIER's inputs — the owner the test knows, the row's own
    id, the name the folder was created under — never from the attestation's
    carried fields, exactly as a seat rebuilds it. Every element is length-
    prefixed with a big-endian u64, the tag first
    (`sig_domain::domain_separated_length_prefixed`)."""
    elements = [
        owner,
        folder_id.to_bytes(8, "big", signed=True),  # i64
        counter.to_bytes(8, "big"),  # u64
        name.encode(),
    ]
    out = len(ATTESTATION_TAG).to_bytes(8, "big") + ATTESTATION_TAG
    for e in elements:
        out += len(e).to_bytes(8, "big") + e
    return out


def attestation_verifies(row: dict, owner: bytes, name: str) -> bool:
    """Does the nest row's served ``audience_attestation`` verify under
    ``owner`` over this row's id and ``name``? False when none is served."""
    attestation = row.get("audience_attestation")
    if attestation is None:
        return False
    message = attestation_message(owner, row["id"], name, attestation["counter"])
    try:
        VerifyKey(owner).verify(message, bytes(attestation["sig"]))
    except BadSignatureError:
        return False
    return True
