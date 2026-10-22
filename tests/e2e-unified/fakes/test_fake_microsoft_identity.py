"""Self-test for `fake_microsoft_identity`: it answers the device-code grant
exactly as `libs/fauna-mail/src/imap_client/oauth_device.rs` asks it, over real
TLS, and each way a sign-in ends is the error code the flow maps.

A subtly wrong answer here would surface later as "the Outlook sign-in is
broken" in a tier_3 run, the most expensive place to learn it.

tier_1: pure in-process sockets, no nest, no driver, no app.
"""

from __future__ import annotations

import json
import ssl
import threading
import urllib.error
import urllib.parse
import urllib.request

import pytest

from fake_imap_source import mint_localhost_chain
from fake_microsoft_identity import DEVICE_CODE_GRANT, VERIFICATION_URI, FakeMicrosoftIdentity

pytestmark = pytest.mark.tier_1

# One chain for the module: the fake serves the leaf, the client trusts the CA
# — the shape the app's trust seed has (`fake_imap_source.mint_localhost_chain`).
_CA, _LEAF, _KEY = mint_localhost_chain()


def FakeMicrosoftIdentity(**kw):  # noqa: N802 — the class's own name, pinned to the chain
    from fake_microsoft_identity import FakeMicrosoftIdentity as Fake

    return Fake(cert=(_LEAF, _KEY), **kw)


def _post(fake, path: str, form: dict[str, str]) -> tuple[int, dict]:
    """One form POST, trusting exactly the fake's certificate — the
    Python-side twin of `NativeOAuthHttp` under the trust seed."""
    ctx = ssl.create_default_context()
    ctx.load_verify_locations(cadata=_CA)
    # Python 3.13+ demands an Authority Key Identifier the minted chain does
    # not carry; rustls, the client that matters, does not.
    ctx.verify_flags &= ~ssl.VERIFY_X509_STRICT
    request = urllib.request.Request(
        f"{fake.authority}{path}",
        data=urllib.parse.urlencode(form).encode(),
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    try:
        with urllib.request.urlopen(request, context=ctx, timeout=10) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as err:
        return err.code, json.loads(err.read())


def _start(fake) -> dict:
    status, body = _post(
        fake,
        "/devicecode",
        {"client_id": "00000000-0000-0000-0000-000000000001", "scope": "imap offline_access"},
    )
    assert status == 200, body
    return body


def _poll(fake, device_code: str) -> tuple[int, dict]:
    return _post(
        fake,
        "/token",
        {"grant_type": DEVICE_CODE_GRANT, "client_id": "x", "device_code": device_code},
    )


def test_a_code_is_pending_until_approved_then_grants_the_distinctive_token():
    with FakeMicrosoftIdentity() as fake:
        code = _start(fake)
        assert code["verification_uri"] == VERIFICATION_URI
        assert code["interval"] == 1 and code["expires_in"] > 0
        assert fake.issued_codes == [code["user_code"]]
        assert fake.device_code_requests[0]["client_id"] == "00000000-0000-0000-0000-000000000001"

        assert _poll(fake, code["device_code"]) == (400, {"error": "authorization_pending"})
        fake.approve(code["user_code"])
        status, grant = _poll(fake, code["device_code"])
        assert status == 200
        assert grant["access_token"] == fake.access_token
        assert grant["access_token"].startswith("fauna-e2e-ms-access-")
        assert fake.issued_tokens == [fake.access_token]
        # Redeemed once.
        assert _poll(fake, code["device_code"])[0] == 400
        # The recorded polls never keep the device code.
        assert all("device_code" not in r for r in fake.token_requests)


@pytest.mark.parametrize(
    ("end", "expected"),
    [
        ("decline", "authorization_declined"),
        ("expire", "expired_token"),
    ],
)
def test_each_ending_answers_the_error_the_flow_maps(end: str, expected: str):
    with FakeMicrosoftIdentity() as fake:
        code = _start(fake)
        getattr(fake, end)(code["user_code"])
        assert _poll(fake, code["device_code"]) == (400, {"error": expected})
        assert fake.issued_tokens == []


def test_a_consent_refusal_carries_the_aadsts65001_family():
    with FakeMicrosoftIdentity() as fake:
        code = _start(fake)
        fake.refuse_consent(code["user_code"])
        status, body = _poll(fake, code["device_code"])
        assert status == 400
        assert 65001 in body["error_codes"] and "AADSTS65001" in body["error_description"]


def test_a_refresh_trades_the_issued_refresh_token_only():
    with FakeMicrosoftIdentity() as fake:
        ok = _post(fake, "/token", {"grant_type": "refresh_token", "refresh_token": fake.refresh_token})
        assert ok[0] == 200 and ok[1]["access_token"] == fake.access_token
        refused = _post(fake, "/token", {"grant_type": "refresh_token", "refresh_token": "nope"})
        assert refused == (400, {"error": "invalid_grant", "error_description": "unknown refresh token"})
        assert all("refresh_token" not in r for r in fake.token_requests)


def test_wait_for_code_follows_the_clients_progress():
    with FakeMicrosoftIdentity() as fake:
        with pytest.raises(TimeoutError, match="asked for 0"):
            fake.wait_for_code(timeout=0.05)
        threading.Thread(target=_start, args=(fake,), daemon=True).start()
        assert fake.wait_for_code(timeout=10) == fake.issued_codes[0]


def test_an_unknown_route_is_answered_never_dropped():
    with FakeMicrosoftIdentity() as fake:
        status, body = _post(fake, "/authorize", {})
        assert status == 404 and body["error"] == "invalid_request"
