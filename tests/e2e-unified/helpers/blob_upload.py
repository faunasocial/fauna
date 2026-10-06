"""``POST /api/v1/blob`` — the multipart ``sidecar`` + ``bytes`` upload shape.

The one HTTP ingest twin still live (the bulk-binary carve-out —
``docs/goal/architecture/api-layers.md`` § HTTP residue) accepts **only**
``multipart/form-data`` with exactly two parts, ``sidecar`` (canonical dag-cbor
``UploadSidecar``) and ``bytes``. The legacy ``application/octet-stream`` body
retired with the blob strict flip on 2026-07-01 and now returns 400 —
``docs/goal/architecture/encryption-at-rest.md`` § Don't do these owns that
claim, and ``bins/fauna-nest/src/blob_routes.rs::upload_blob`` implements it.

**Why this is a helper and not a per-file snippet.** Four suites had each grown
their own byte-identical copy of the boundary assembly, and the fourth site was
written *before* the strict flip and never updated — which is how
``test_blob_api.py``'s three uploads sat red on ``origin/main``. One writer per wire shape is the point: when the sidecar grows a
field, there is a single place that learns about it.

The nest dispatches the per-class envelope-shape verifier on
``sidecar["class"]`` (``AudienceClass``), so the caller must say which class its
bytes are:

* ``Library`` / ``Conversation`` / ``GroupRestrictedPost`` /
  ``PeriodRestrictedPost`` — **sealed** classes: AEAD-shaped bytes,
  ``mime="application/octet-stream"`` (the real MIME rides *inside* the seal),
  ``has_c2pa=False``;
* ``PublicPost`` — plaintext bytes and a real, non-empty ``type/subtype`` MIME.

A violation is not a transport error: it comes back as HTTP 400
``{"error": "ingest rejected: <reason>"}`` carrying the snake_case
``IngestRejectReason::Blob*`` variant, which is what a caller should assert on
when testing the refusals rather than the acceptance.
"""

import json
import urllib.request

import cbor2

from common.auth import port_base_url

# Sealed classes seal the real MIME inside the ciphertext, so the sidecar's own
# MIME is the opaque one. Kept as a constant because it is a protocol value, not
# a description of the payload the caller happens to hold.
SEALED_MIME = "application/octet-stream"


def sidecar_bytes(audience_class, mime, *, has_c2pa=False, thumbnail_hash=None):
    """The canonical dag-cbor ``UploadSidecar`` for one upload.

    Canonical encoding is required, not cosmetic: the nest decodes it with
    ``decode_strict``, so a non-canonical map ordering is a 400.
    """
    return cbor2.dumps(
        {
            "class": audience_class,
            "mime": mime,
            "has_c2pa": has_c2pa,
            "thumbnail_hash": thumbnail_hash,
        },
        canonical=True,
    )


def multipart_body(sidecar, data, boundary):
    """Assemble the two-part body. Order is free; the nest matches on name."""
    return b"".join(
        [
            f"--{boundary}\r\n".encode(),
            b'Content-Disposition: form-data; name="sidecar"\r\n\r\n',
            sidecar,
            f"\r\n--{boundary}\r\n".encode(),
            b'Content-Disposition: form-data; name="bytes"\r\n\r\n',
            data,
            f"\r\n--{boundary}--\r\n".encode(),
        ]
    )


def upload_blob(
    port,
    token,
    data,
    *,
    audience_class,
    mime=SEALED_MIME,
    has_c2pa=False,
    thumbnail_hash=None,
    boundary="faunablobtestboundary",
    host=None,
):
    """Upload one blob and return its hex hash.

    ``audience_class`` is deliberately keyword-only and has no default: the
    class picks the verifier, so guessing it for the caller would turn a
    shape mismatch into a confusing 400 instead of a decision at the call site.
    """
    body = multipart_body(
        sidecar_bytes(
            audience_class, mime, has_c2pa=has_c2pa, thumbnail_hash=thumbnail_hash
        ),
        data,
        boundary,
    )
    req = urllib.request.Request(
        f"{port_base_url(port, host)}/api/v1/blob",
        data=body,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": f"multipart/form-data; boundary={boundary}",
        },
        method="POST",
    )
    resp = urllib.request.urlopen(req)
    return json.loads(resp.read())["hash"]


def download_blob(port, blob_hash, *, host=None):
    """``GET /api/v1/blob/<hex>`` — unauthenticated and navigable by design."""
    req = urllib.request.Request(
        f"{port_base_url(port, host)}/api/v1/blob/{blob_hash}",
        method="GET",
    )
    return urllib.request.urlopen(req).read()
