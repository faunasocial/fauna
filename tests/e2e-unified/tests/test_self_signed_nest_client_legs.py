"""tier_3: a logged-in client reaches a self-signed HTTPS nest on every leg.

The gap this closes (2026-06-20). The Windows app's same-box app→nest TLS work
landed earlier: the windows app's default nest URL is
`https://127.0.0.1:443`, and its C# `DirectNestClient` — the *second* residual-HTTP
leg, a .NET `HttpClient` the Rust rustls pin store cannot govern — trusts a
self-signed nest through a `ServerCertificateCustomValidationCallback`
(`security.md` § Transport trust, the C# leg sub-bullet: accept iff WebPKI-valid,
OR the host is loopback, OR the served cert's SPKI matches the channel-binding pin;
SPKI / pin / loopback classification all FFI'd from shared Rust so the C# value is
byte-identical to the pin). It was **built + unit-tested** (`NestCertTrustTests`,
`NestCertTrustFfiTests`) but **never live-verified**, because every tier_3 e2e nest
rides the process-wide `FAUNA_INSECURE_DISABLE_TLS` escape and serves plain HTTP —
so no automated test had ever walked the C# HTTPS path at all. A TLS reject there is
invisible to unit tests (they never open a socket) and would silently break media,
health and snapshot against a real installed nest.

The apple twin (2026-09-24). The macOS and iOS apps carry the same shape of leg — a
Swift `URLSession` per `APIClient` for blob upload / download — and it made NO trust
decision at all until this test caught it (`-1202` against the loopback floor cert on
both targets). It now rides `NestCertTrustSessionDelegate`, the C# callback's twin
over the same shared-Rust classifiers (`NestCertTrust.swift`, security.md § Transport
trust's apple sub-bullet). So the assertions below name "the app's residual-HTTP leg":
C# `HttpClient` on windows, Swift `URLSession` on macOS / iOS.

The crux is the `self_signed_nest` fixture (conftest): it drops the plain-HTTP escape
for one nest, so the nest serves its always-live self-signed floor cert
(`nest/domains-and-tls-bootstrap.md` § Boot — domainless SANs `localhost` + `127.0.0.1`,
CN `fauna-nest`) over real HTTPS. `self_signed_logged_in_app` then logs the client into
*that* nest instead of the shared plain-HTTP one, so every client→nest call in these
tests rides TLS against a cert no public CA signed.

Which branch of the trust policy this exercises: the nest binds `127.0.0.1`, so the
authority is loopback and the callback's **loopback branch** decides. That is exactly
the same-box-install case the goal doc claims — `installers/windows.md`
§ Implementation status today → *Same-box app default URL* `https://127.0.0.1:443`
+ C# self-signed-floor trust. The **SPKI-pin branch** (a remote `test@<ip>` nest) needs
a non-loopback authority, which the harness cannot bind today; that half is tracked
separately (tracked internally, exercised via a manual test).

Native-only, and the declaration lives in the FIXTURE (`conftest.py::
_declare_no_web_cert_trust`, called by `self_signed_logged_in_app` before it logs in)
— not here in the bodies, where it used to sit and therefore never fired: the login
ran first and both legs ERRORed at setup in `ConnectionBarrierTimeout`.
The reason is that web's client makes no cert-trust decision at all — the browser
terminates TLS and hands WASM no certificate (`security.md` § Transport trust:
Axis 1's channel binding "cannot be completed", "structural, not a gap to close
later") — NOT that a browser cannot be told to trust the floor cert, which it can
(`web-bridge/server.py` sets `ignore_https_errors` on every context for exactly that).
"""

from __future__ import annotations

import ssl
import urllib.request
import uuid
from pathlib import Path

import pytest

pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    # Native-only, as the module docs say — and MARKED so, never left to the
    # fixture's web `declared_absence` to skip (feature-catalog.md § Cell
    # semantics, the marked-witness rule, 2026-09-26): the SUBJECT of both
    # legs is the client's own cert-trust decision, a mechanism web
    # structurally lacks (the browser terminates TLS), while the outcome the
    # blob leg witnesses (`connect-and-sign-in` outcome 4 — reaching and
    # signing in to a nest with no public certificate) is one web reaches by
    # the browser's own door, so a skip here would red a column the page owes
    # nothing of this test on. The fixture-level declaration stays for the
    # other modules that log in against the self-signed nest.
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.android,
    pytest.mark.tui,
]

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def test_health_leg_reaches_a_self_signed_nest(self_signed_logged_in_app):
    """The unauthenticated health check survives the self-signed floor.

    On windows this is `DirectNestClient.IsAvailableAsync()` → `GET /api/v1/health`
    through the cert-validation callback. `StatusViewModel.LoadAsync` only fills
    `account-actor-id` when `IsAvailableAsync()` returned true, so a rendered actor
    id IS the health leg's success signal — a TLS reject would leave it empty.
    """
    app = self_signed_logged_in_app

    # `_navigate_subpage("status")`, not plain `.navigate()`: iOS lands a plain
    # settings navigate on the root page list, where the actor id never renders.
    app.settings._navigate_subpage("status")
    actor_id = app.settings.actor_id()
    assert actor_id, (
        "settings/status rendered no actor id — the client's health leg "
        "(GET /api/v1/health) did not reach the self-signed-HTTPS nest. On windows "
        "that means DirectNestClient's ServerCertificateCustomValidationCallback "
        "rejected the floor cert (the loopback branch should have accepted it). "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("connect-and-sign-in")
def test_blob_legs_round_trip_against_a_self_signed_nest(
    self_signed_logged_in_app, self_signed_nest
):
    """Media upload AND download survive the self-signed floor — the leg the goal
    doc calls out by name ("...and a remote `test@<ip>` self-signed nest serve it
    **media**, with no public CA").

    Both C# blob legs ride the same trusting `HttpClient`:
      * upload   — `DirectNestClient.UploadBlobAsync` → `POST /api/v1/blob` (multipart)
      * download — `BlobImageLoader` → `GetBlobAsync` → `GET /api/v1/blob/{hash}`
    so a rendered image proves the *round trip* went over TLS, not just the POST.
    On macOS / iOS both legs are `APIClient`'s one `URLSession` (`postMultipartBlob`
    and the blob `GET`), answered by `NestCertTrustSessionDelegate`.
    """
    app = self_signed_logged_in_app
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")

    text = _unique("tls-image")
    app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))

    # Assert the two legs in the order they run, so a red run LOCALIZES the fault
    # (testing.md § conventions, point 6 — failures must diagnose themselves):
    #
    #   UPLOAD   — a 64-hex `media_hash` on the decoded post body means
    #              `UploadBlobAsync`'s multipart POST /api/v1/blob completed and the
    #              nest returned a hash. This reads client STATE, not a rendered
    #              pixel, so it is independent of the download leg.
    blob_hash = app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, (
        f"no blob hash on post {text!r} (got {blob_hash!r}) — the blob UPLOAD leg "
        "(POST /api/v1/blob, multipart) did not complete against the "
        "self-signed-HTTPS nest. The post itself was created (that rides WS-RPC via "
        "shared Rust), so this isolates the failure to the app's residual-HTTP leg "
        "(C# HttpClient on windows, Swift URLSession on macOS/iOS). "
        f"post_count={app.feed.post_count()} error={app.error_text()!r}"
    )

    #   DOWNLOAD — the rendered `post-image` element requires BlobImageLoader →
    #              `GetBlobAsync` → GET /api/v1/blob/{hash} to have come back with
    #              bytes. A TLS reject here surfaces as a post with no image rather
    #              than a hard error, so it needs its own assertion.
    assert app.feed.post_has_image_by_text(text), (
        f"post {text!r} carries blob {blob_hash[:12]}… (so the UPLOAD leg worked) but "
        "rendered no image — the blob DOWNLOAD leg (GET /api/v1/blob/{hash}) failed "
        f"against the self-signed-HTTPS nest. error={app.error_text()!r}"
    )

    # Independent confirmation that the bytes really landed on the nest: fetch the
    # blob back over the SAME self-signed HTTPS listener from Python. This is
    # external black-box verification (testing.md § conventions, point 8(c)) — we
    # observe the nest the way an outside client would, we do not drive a mutation.
    # Verification is off by design: the floor cert is self-signed and
    # name-mismatched; client trust here is channel-binding, not WebPKI.
    ctx = ssl._create_unverified_context()
    req = urllib.request.Request(
        f"{self_signed_nest['url']}/api/v1/blob/{blob_hash}", method="GET"
    )
    with urllib.request.urlopen(req, timeout=15, context=ctx) as resp:
        body = resp.read()
        content_type = resp.headers.get("Content-Type", "")

    assert body[:8] == PNG_MAGIC, (
        "blob did not round-trip as a plaintext PNG over the self-signed listener "
        "(PublicPost audience is stored plaintext — encryption-at-rest.md, Media row)"
    )
    assert content_type.startswith("image/"), f"unexpected Content-Type: {content_type!r}"
