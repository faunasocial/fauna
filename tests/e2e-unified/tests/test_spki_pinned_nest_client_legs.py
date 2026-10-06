"""tier_3: a client's residual-HTTP legs reach a self-signed nest on the SPKI-PIN
branch — the half `test_self_signed_nest_client_legs.py` structurally cannot reach.

WHY A SECOND FILE. Its sibling proved the same-box install: a self-signed nest bound
to `127.0.0.1`. But the trust policy is

    ShouldTrust = webPkiValid || isLoopback || spki == pin          (security.md § Transport trust)

and a loopback authority satisfies term 2 *before* any pin is consulted. So every leg
that file proves is proved on the **loopback short-circuit**; the **SPKI-pin** term —
the one the *remote* `test@<ip>` nest actually rides, where there is no CA and no
loopback to fall back on — had zero end-to-end coverage. A pin-branch regression was
invisible: unit tests never open a socket, and the loopback e2e passes either way.

HOW THIS REACHES THE PIN BRANCH. `spki_pinned_nest` (conftest) serves the same
always-live self-signed floor cert, but is dialled on the box's own LAN IP instead of
loopback. `is_loopback_authority` (`fauna-anon-client/src/trust.rs`) classifies by
string/IP-literal and NEVER resolves DNS, so a LAN IP is non-loopback — while the
packets still never leave the box (no elevation, no firewall rule; Windows Firewall
does not filter same-host traffic to the host's own IP). That kills term 2. The floor
cert is self-signed and its SANs are `localhost` + `127.0.0.1` only, so this dial is
also name-mismatched — which kills term 1. `test_the_floor_cert_is_not_webpki_valid`
below asserts term 1 is really dead rather than assuming it.

With terms 1 and 2 both eliminated, **the only way any leg below can succeed is
`spki == pin`.** A green here is a live proof of the pin branch, by elimination.

WHAT ARMS THE PIN. Logging in mints the bearer over WS-RPC (shared-Rust `mint_bearer`
→ `graduate_handshake`), which writes `state().spki[authority]` in the process-global
`OnceLock` inside the statically-linked `fauna_ffi` DLL. The C# callback reads that
same store via `PinnedSpkiForHost` under the same `authority_of` key — one process,
one store, so no extra wiring is needed to connect the Rust bearer path to the .NET
`HttpClient` the Rust rustls pin store cannot otherwise govern.

Native-only, for the same structural reason as the sibling — and, like the sibling,
declared in the FIXTURE (`conftest.py::_declare_no_web_cert_trust`, called by
`spki_pinned_logged_in_app` before it logs in). It used to be declared in the test
bodies, which run only after that fixture has already logged in, so on web it never
fired: both legs ERRORed at setup in `ConnectionBarrierTimeout` instead.

The reason is sharper here than "a browser cannot be told to trust a self-signed
cert" (which is false — `web-bridge/server.py` sets `ignore_https_errors` on every
context precisely so a `wss://` to one works). It is that the pin branch is a
decision the web client cannot make: arming the pin means WRITING `state().spki[
authority]` from a *received* certificate, and a browser hands WASM no certificate at
all (`security.md` § Transport trust — Axis 1's channel binding "cannot be completed"
on web, "structural, not a gap to close later"). With no received cert there is no
SPKI to pin and no `spki == pin` term to reach, so the elimination argument this file
is built on has nothing left to prove on web.
"""

from __future__ import annotations

import socket
import ssl
import urllib.request
import uuid
from pathlib import Path

import pytest


pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
FIXTURE_DIR = Path(__file__).parent.parent / "fixtures"
TEST_IMAGE = FIXTURE_DIR / "test-image.png"


def _unique(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:8]}"


def _split_authority(url: str) -> tuple[str, int]:
    host, _, port = url.split("://", 1)[1].rpartition(":")
    return host, int(port)


def test_the_floor_cert_is_not_webpki_valid(spki_pinned_nest):
    """The load-bearing negative control for every other test in this file.

    The elimination argument only holds if `webPkiValid` is genuinely FALSE for this
    dial. If it were somehow true (a cert that chained to a real CA, or a name that
    happened to match), the legs below would pass on term 1 and this file would claim
    to prove the pin branch while proving nothing at all.

    So assert it directly, the way an outside client sees it: a *verifying* TLS
    handshake to this authority must FAIL. This is external black-box verification
    (testing.md § conventions, point 8(c)) — we observe the nest as a scanner would,
    we drive no mutation.
    """
    host, port = _split_authority(spki_pinned_nest["url"])

    ctx = ssl.create_default_context()  # verification ON — the point of the test
    with pytest.raises(ssl.SSLError) as excinfo:
        with socket.create_connection((host, port), timeout=15) as sock:
            with ctx.wrap_socket(sock, server_hostname=host):
                pass  # a successful handshake here is the failure

    # Either reason is fine (untrusted issuer, or SAN mismatch) — both mean a WebPKI
    # client rejects this cert, which is all the elimination argument needs.
    assert excinfo.value, (
        f"a verifying TLS client ACCEPTED {host}:{port} — the floor cert is WebPKI-valid "
        "for this authority, so `webPkiValid` short-circuits ShouldTrust and the "
        "pin-branch tests below would pass without ever consulting a pin"
    )


def test_health_leg_reaches_a_pinned_non_loopback_nest(spki_pinned_logged_in_app):
    """The health leg lands on the SPKI-pin branch.

    On windows: `DirectNestClient.IsAvailableAsync()` → `GET /api/v1/health` through
    `ValidateServerCertificate`. `StatusViewModel.LoadAsync` only fills
    `account-actor-id` when `IsAvailableAsync()` returned true, so a rendered actor id
    IS the health leg's success signal.

    Note this leg is the *reason* the loopback term exists at all (`trust.rs`: "the
    unauthenticated health check ... can run before any handshake graduates a pin").
    Here there is no loopback term to lean on, so a green proves the pin is armed by
    the time the health check runs — i.e. the bearer mint really does graduate the pin
    ahead of the residual-HTTP legs that depend on it.
    """
    app = spki_pinned_logged_in_app

    # `_navigate_subpage("status")`, not plain `.navigate()`: iOS lands a plain
    # settings navigate on the root page list, where the actor id never renders.
    app.settings._navigate_subpage("status")
    actor_id = app.settings.actor_id()
    assert actor_id, (
        "settings/status rendered no actor id — the client's health leg "
        "(GET /api/v1/health) did not reach the non-loopback self-signed nest. Since "
        "the authority is NOT loopback and the cert is NOT WebPKI-valid, the only "
        "branch that could have accepted it is `spki == pin` — so this means the pin "
        "was not armed (the bearer mint did not graduate it, or the C# callback read a "
        "different authority key than the Rust side wrote). "
        f"error={app.error_text()!r}"
    )


def test_blob_legs_round_trip_on_the_pin_branch(
    spki_pinned_logged_in_app, spki_pinned_nest
):
    """Media upload AND download land on the SPKI-pin branch — the exact claim the
    goal doc makes for the remote case ("...and a remote `test@<ip>` self-signed nest
    serve it **media**, with no public CA").

    Both C# blob legs ride the same trusting `HttpClient`:
      * upload   — `DirectNestClient.UploadBlobAsync` → `POST /api/v1/blob` (multipart)
      * download — `BlobImageLoader` → `GetBlobAsync` → `GET /api/v1/blob/{hash}`
    so a rendered image proves the round trip, not just the POST.
    """
    app = spki_pinned_logged_in_app
    if not TEST_IMAGE.exists():
        pytest.skip("Test image fixture not found")

    text = _unique("pin-image")
    app.feed.create_post_with_image(text=text, image_path=str(TEST_IMAGE))

    # Assert the legs in the order they run so a red run LOCALIZES the fault
    # (testing.md § conventions, point 6).
    blob_hash = app.feed.post_image_blob_hash_by_text(text)
    assert len(blob_hash) == 64, (
        f"no blob hash on post {text!r} (got {blob_hash!r}) — the blob UPLOAD leg "
        "(POST /api/v1/blob, multipart) did not complete against the non-loopback "
        "self-signed nest, i.e. the SPKI-pin branch rejected it. The post itself was "
        "created (that rides WS-RPC via shared Rust, whose rustls verifier pins "
        "separately), so this isolates the failure to the C# HttpClient's pin check. "
        f"post_count={app.feed.post_count()} error={app.error_text()!r}"
    )

    assert app.feed.post_has_image_by_text(text), (
        f"post {text!r} carries blob {blob_hash[:12]}… (so the UPLOAD leg worked) but "
        "rendered no image — the blob DOWNLOAD leg (GET /api/v1/blob/{hash}) failed on "
        f"the pin branch. error={app.error_text()!r}"
    )

    # Independent confirmation the bytes really landed, fetched back over the SAME
    # non-loopback self-signed listener (point 8(c) again). Verification is off here
    # *by design* — this stands in for a client that has the pin, and the preceding
    # test already proved a verifying client is rejected.
    ctx = ssl._create_unverified_context()
    req = urllib.request.Request(
        f"{spki_pinned_nest['url']}/api/v1/blob/{blob_hash}", method="GET"
    )
    with urllib.request.urlopen(req, timeout=15, context=ctx) as resp:
        body = resp.read()
        content_type = resp.headers.get("Content-Type", "")

    assert body[:8] == PNG_MAGIC, (
        "blob did not round-trip as a plaintext PNG over the non-loopback listener "
        "(PublicPost audience is stored plaintext — encryption-at-rest.md, Media row)"
    )
    assert content_type.startswith("image/"), f"unexpected Content-Type: {content_type!r}"
