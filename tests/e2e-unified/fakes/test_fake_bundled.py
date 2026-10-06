"""Self-tests for tests/e2e-unified/fakes/fake_bundled.py — the reference
server for the Fauna Bundled Provider API v1.

Plain in-process pytest (no driver, no nest, no app): `requests` hits the
pytest-httpserver fake directly, so a regression in the fake surfaces here
rather than as a mysterious tier_2 journey failure. Mocking-depth axis =
tier_1; not auto-tagged because the tier-tagger only scans `tests/test_*.py`.
"""
from __future__ import annotations

import pytest
import requests

pytestmark = pytest.mark.tier_1


def _bearer(fake_cloud) -> dict[str, str]:
    return {"Authorization": f"Bearer {fake_cloud.bundled.token}"}


def test_base_url_is_the_typed_address_and_me_needs_the_token(fake_cloud) -> None:
    b = fake_cloud.bundled
    assert b.base_url().startswith("http://127.0.0.1:") and b.base_url().endswith("/bundled")
    r = requests.get(f"{b.base_url()}/v1/me")
    assert r.status_code == 401
    assert r.json()["error"]["code"] == "unauthorized"
    r = requests.get(f"{b.base_url()}/v1/me", headers=_bearer(fake_cloud))
    assert r.status_code == 200
    body = r.json()
    assert body["api_version"] == 1
    assert body["zones"] == []
    assert body["locations"][0]["id"] == "eu-1"
    assert r.headers["Access-Control-Allow-Origin"] == "*"


def test_device_flow_auto_approves_on_the_first_poll(fake_cloud) -> None:
    b = fake_cloud.bundled
    r = requests.post(f"{b.base_url()}/v1/auth/device", data={"client_id": "fauna", "scope": "provisioning"})
    assert r.status_code == 200
    d = r.json()
    assert d["user_code"].startswith("FAUNA-") and d["interval"] == 1
    assert d["verification_uri_complete"].endswith(d["user_code"])
    r = requests.post(
        f"{b.base_url()}/v1/auth/token",
        data={
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": d["device_code"],
            "client_id": "fauna",
        },
    )
    assert r.status_code == 200
    assert r.json()["access_token"] == b.token


def test_device_flow_waits_for_approval_when_not_auto(fake_cloud) -> None:
    b = fake_cloud.bundled
    b.auto_approve = False
    d = requests.post(f"{b.base_url()}/v1/auth/device", data={"client_id": "fauna", "scope": "provisioning"}).json()
    form = {
        "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
        "device_code": d["device_code"],
        "client_id": "fauna",
    }
    r = requests.post(f"{b.base_url()}/v1/auth/token", data=form)
    assert r.status_code == 400 and r.json()["error"] == "authorization_pending"
    b.approve(d["user_code"])
    r = requests.post(f"{b.base_url()}/v1/auth/token", data=form)
    assert r.status_code == 200


def test_registration_binds_the_agreed_price_and_mints_the_zone(fake_cloud) -> None:
    b = fake_cloud.bundled
    h = _bearer(fake_cloud)
    chk = requests.get(f"{b.base_url()}/v1/domains/check", params={"name": "new.test"}, headers=h).json()
    assert chk["status"] == "available"
    assert chk["registration_cents"] == b.registration_cents
    assert chk["renewal_cents"] == b.renewal_cents
    assert requests.get(f"{b.base_url()}/v1/domains/check", params={"name": "x.zz"}, headers=h).json()["status"] == "tld_not_supported"

    contact = {"first_name": "T", "last_name": "U", "email": "hello@example.test", "phone": "+1.5555550100",
               "address1": "1 Way", "city": "C", "state": "S", "postal_code": "0", "country": "US"}
    stale = requests.post(f"{b.base_url()}/v1/domains", json={
        "name": "new.test", "years": 1, "agreed_price_cents": 1, "contact": contact, "whois_privacy": True,
    }, headers=h)
    assert stale.status_code == 409 and stale.json()["error"]["code"] == "price_changed"
    ok = requests.post(f"{b.base_url()}/v1/domains", json={
        "name": "new.test", "years": 1, "agreed_price_cents": b.registration_cents, "contact": contact, "whois_privacy": True,
    }, headers=h)
    assert ok.status_code == 201 and ok.json()["nameservers"]
    zones = requests.get(f"{b.base_url()}/v1/me", headers=h).json()["zones"]
    assert [z["name"] for z in zones] == ["new.test"]
    assert requests.get(f"{b.base_url()}/v1/domains/check", params={"name": "new.test"}, headers=h).json()["status"] == "unavailable"
    assert requests.get(f"{b.base_url()}/v1/domains/new.test/auth-code", headers=h).json()["auth_code"]
    assert len(b.registered) == 1 and b.registered[0]["whois_privacy"] is True


def test_auth_code_reports_a_registry_lock_as_202_never_a_refusal(fake_cloud) -> None:
    """Spec § Exit guarantee 1: the transfer code is handed over on demand,
    and a registry-imposed 60-day lock is reported as `202 available_after` —
    *never* as a refusal. The retire view renders the date the lock lifts
    (`behavior/nest-retirement.md` § Transfer authorization code), so both
    arms need a fake to drive them."""
    b = fake_cloud.bundled
    h = _bearer(fake_cloud)
    b.zones["z-lock"] = {"id": "z-lock", "name": "locked.test"}

    ready = requests.get(f"{b.base_url()}/v1/domains/locked.test/auth-code", headers=h)
    assert ready.status_code == 200
    assert ready.json()["auth_code"] == "EPP-LOCKED.TEST"

    b.transfer_lock_until = "2026-11-18T00:00:00Z"
    locked = requests.get(f"{b.base_url()}/v1/domains/locked.test/auth-code", headers=h)
    assert locked.status_code == 202, "a lock is a date, not a 4xx"
    assert locked.json() == {"available_after": "2026-11-18T00:00:00Z"}
    assert "auth_code" not in locked.json()


def test_records_and_servers_round_trip_with_idempotent_deletes(fake_cloud) -> None:
    b = fake_cloud.bundled
    h = _bearer(fake_cloud)
    b.zones["z-t"] = {"id": "z-t", "name": "t.test"}
    rec = requests.post(f"{b.base_url()}/v1/zones/z-t/records", json={"type": "MX", "name": "@", "value": "mail.t.test", "ttl": 300, "priority": 10}, headers=h)
    assert rec.status_code == 201 and rec.json()["priority"] == 10
    listed = requests.get(f"{b.base_url()}/v1/zones/z-t/records", params={"name": "@", "type": "MX"}, headers=h).json()["records"]
    assert len(listed) == 1
    assert requests.get(f"{b.base_url()}/v1/zones/z-t/records", params={"name": "@", "type": "A"}, headers=h).json()["records"] == []
    rid = listed[0]["id"]
    assert requests.delete(f"{b.base_url()}/v1/zones/z-t/records/{rid}", headers=h).status_code == 204
    assert requests.delete(f"{b.base_url()}/v1/zones/z-t/records/{rid}", headers=h).status_code == 404

    types = requests.get(f"{b.base_url()}/v1/server_types", headers=h).json()["server_types"]
    assert [t["id"] for t in types] == ["small", "medium", "large"]
    srv = requests.post(f"{b.base_url()}/v1/servers", json={
        "name": "nest-t", "location": "eu-1", "server_type": "small", "user_data": "#cloud-config\n",
        "labels": {"managed-by": "fauna"},
    }, headers=h)
    assert srv.status_code == 201
    sid = srv.json()["id"]
    assert requests.get(f"{b.base_url()}/v1/servers", params={"name": "nest-t"}, headers=h).json()["servers"][0]["id"] == sid
    assert requests.get(f"{b.base_url()}/v1/servers", params={"label": "managed-by=fauna"}, headers=h).json()["servers"]
    assert requests.get(f"{b.base_url()}/v1/servers/{sid}/ptr", headers=h).json()["ptr"] is None
    assert requests.put(f"{b.base_url()}/v1/servers/{sid}/ptr", json={"ptr": "mail.t.test"}, headers=h).status_code == 204
    assert b.ptr[sid] == "mail.t.test"
    assert requests.delete(f"{b.base_url()}/v1/servers/{sid}", headers=h).status_code == 204
    assert requests.delete(f"{b.base_url()}/v1/servers/{sid}", headers=h).status_code == 404


def test_pricing_is_public_and_preflight_is_permissive(fake_cloud) -> None:
    b = fake_cloud.bundled
    r = requests.get(f"{b.base_url()}/v1/pricing/tlds")
    assert r.status_code == 200 and any(t["tld"] == "io" for t in r.json()["tlds"])
    pre = requests.options(f"{b.base_url()}/v1/me", headers={"Origin": "https://app.example"})
    assert pre.status_code in (200, 204)
    assert pre.headers["Access-Control-Allow-Origin"] == "*"
    assert "Authorization" in pre.headers["Access-Control-Allow-Headers"]
