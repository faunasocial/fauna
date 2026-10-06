"""Self-tests for tests/e2e-unified/fakes/fake_cloud.py.

These run as plain pytest unit tests (no driver, no nest, no client).
They guard the fake's API surface so changes to the handler defaults
or mutators don't silently regress.
"""
from __future__ import annotations

import pytest
import requests

# In-process unit tests: the fake is a local pytest-httpserver process and
# `requests` hits it directly — no nest binary, no client driver. Mocking-depth
# axis = tier_1 (in-process unit test, per the test taxonomy). Not
# auto-tagged because the tier-tagger only scans `tests/test_*.py`, not
# `fakes/`.
pytestmark = pytest.mark.tier_1


def test_fake_cloud_url_map_has_three_keys(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    assert set(urls.keys()) == {"vps", "dns", "nest"}
    for key, url in urls.items():
        assert url.startswith("http://127.0.0.1:"), f"{key}: {url}"


def test_fake_cloud_default_hetzner_create_server_returns_canned(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    resp = requests.post(
        f"{urls['vps']}/servers",
        json={"name": "test-srv", "server_type": "cax11", "image": "debian-12"},
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 201
    body = resp.json()
    assert body["server"]["id"]
    assert body["server"]["public_net"]["ipv4"]["ip"]


def test_fake_cloud_default_cloudflare_list_zones_returns_one_zone(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    resp = requests.get(
        f"{urls['dns']}/zones?per_page=50",
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["success"] is True
    assert len(body["result"]) >= 1


def test_fake_cloud_default_nest_health_returns_200(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    resp = requests.get(f"{urls['nest']}/api/v1/health")
    assert resp.status_code == 200


def test_health_fail_first_then_succeed(fake_cloud) -> None:
    fake_cloud.nest.health.fail_first_then_succeed(2)
    health_url = f"{fake_cloud.url_map()['nest']}/api/v1/health"
    assert requests.get(health_url).status_code == 503
    assert requests.get(health_url).status_code == 503
    assert requests.get(health_url).status_code == 200


def test_responses_carry_cors_headers(fake_cloud) -> None:
    """Every live response (and the OPTIONS preflight) must carry CORS so
    the browser permits the WASM wizard's cross-origin fetch at the fake."""
    urls = fake_cloud.url_map()
    resp = requests.get(f"{urls['nest']}/api/v1/health")
    assert resp.headers.get("Access-Control-Allow-Origin") == "*"
    pre = requests.options(
        f"{urls['dns']}/zones",
        headers={"Access-Control-Request-Method": "GET",
                 "Access-Control-Request-Headers": "authorization"},
    )
    assert pre.status_code == 200
    assert pre.headers.get("Access-Control-Allow-Origin") == "*"
    assert "Authorization" in pre.headers.get("Access-Control-Allow-Headers", "")

# ---------------------------------------------------------------------------
# The retire view's provider surface (docs/goal/behavior/nest-retirement.md)
# ---------------------------------------------------------------------------


def test_hetzner_list_servers_is_empty_without_the_label_selector(fake_cloud) -> None:
    """`?name=` is find_server_by_name's pre-flight — it must keep answering
    empty, or provisioning would think every box already exists."""
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.seed_managed_server()
    resp = requests.get(
        f"{urls['vps']}/servers?name=example-test",
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    assert resp.json()["servers"] == []


def test_hetzner_list_servers_honours_the_marker_selector(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.seed_managed_server(server_id=1, name="alpha-test")
    # A non-fauna box in the same account: never shown, let alone deletable.
    fake_cloud.hetzner_cloud.seed_managed_server(
        server_id=99, name="someone-elses-db", labels={"team": "data"}
    )
    resp = requests.get(
        f"{urls['vps']}/servers?label_selector=managed-by%3Dfauna",
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert [s["id"] for s in body["servers"]] == [1]
    assert body["meta"]["pagination"]["next_page"] is None


def test_hetzner_delete_server_is_idempotent(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.seed_managed_server(server_id=7)
    headers = {"Authorization": "Bearer test-token"}

    first = requests.delete(f"{urls['vps']}/servers/7", headers=headers)
    assert first.status_code == 204
    # A second teardown finds the box already gone; the adapter treats 404 as
    # success, so a re-run after a crash converges rather than raising.
    second = requests.delete(f"{urls['vps']}/servers/7", headers=headers)
    assert second.status_code == 404
    assert fake_cloud.hetzner_cloud.deleted_server_ids == ["7", "7"]
    assert fake_cloud.hetzner_cloud.managed_servers == []


def test_hetzner_dns_remove_records_records_the_value_it_was_given(fake_cloud) -> None:
    """The retire run's DNS cleanup is value-scoped, never a name sweep — so
    the fake has to see the value, otherwise the two are indistinguishable."""
    dns = fake_cloud.dns_base_for("hetzner")
    resp = requests.post(
        f"{dns}/zones/42/rrsets/@/A/actions/remove_records",
        json={"records": [{"value": "203.0.113.5"}]},
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    assert fake_cloud.hetzner_dns.removed_records == [("@", "A", "203.0.113.5")]
    assert fake_cloud.hetzner_dns.added_records == []


def test_hetzner_dns_rrset_read_answers_from_the_seeded_store(fake_cloud) -> None:
    """The retire view attributes a domain by reading its apex `A` and plans
    removals off what the zone holds — a store the read ignores would make
    every box unattributed and every DNS step a no-op."""
    dns = fake_cloud.dns_base_for("hetzner")
    fake_cloud.hetzner_dns.seed_record("@", "A", "203.0.113.5")
    fake_cloud.hetzner_dns.seed_record("mail", "A", "203.0.113.5")
    resp = requests.get(
        f"{dns}/zones/42/rrsets",
        params={"name": "@", "type": "A"},
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    rrsets = resp.json()["rrsets"]
    assert [(r["name"], [v["value"] for v in r["records"]]) for r in rrsets] == [
        ("@", ["203.0.113.5"])
    ]


def test_hetzner_dns_remove_records_takes_the_value_out_of_the_zone(fake_cloud) -> None:
    """A removal empties exactly that value, so a re-run finds nothing left —
    the idempotence the retire run's retry relies on — and a sibling value at
    the same name stays (value-scoped, never a name sweep)."""
    dns = fake_cloud.dns_base_for("hetzner")
    fake_cloud.hetzner_dns.seed_record("@", "A", "203.0.113.5")
    fake_cloud.hetzner_dns.seed_record("@", "A", "198.51.100.9")
    resp = requests.post(
        f"{dns}/zones/42/rrsets/@/A/actions/remove_records",
        json={"records": [{"value": "203.0.113.5"}]},
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    assert fake_cloud.hetzner_dns.records() == [("@", "A", "198.51.100.9")]


def test_hetzner_dns_fail_removals_fails_then_recovers(fake_cloud) -> None:
    """The failed-DNS branch: the next N removals answer 500 and leave the
    zone untouched; the one after succeeds."""
    dns = fake_cloud.dns_base_for("hetzner")
    fake_cloud.hetzner_dns.seed_record("@", "A", "203.0.113.5")
    fake_cloud.hetzner_dns.fail_removals = 1
    url = f"{dns}/zones/42/rrsets/@/A/actions/remove_records"
    body = {"records": [{"value": "203.0.113.5"}]}
    headers = {"Authorization": "Bearer test-token"}
    assert requests.post(url, json=body, headers=headers).status_code == 500
    assert fake_cloud.hetzner_dns.records() == [("@", "A", "203.0.113.5")]
    assert requests.post(url, json=body, headers=headers).status_code == 200
    assert fake_cloud.hetzner_dns.records() == []


def test_hetzner_cloud_base_also_serves_its_dns(fake_cloud) -> None:
    """One Hetzner API serves both servers and zones, and the retire machine
    points one base URL at both — so the VPS prefix answers the zone list."""
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.dns.zones.append({"id": 43, "name": "second.test"})
    resp = requests.get(
        f"{urls['vps']}/zones", headers={"Authorization": "Bearer test-token"}
    )
    assert resp.status_code == 200
    assert [z["name"] for z in resp.json()["zones"]] == ["example.test", "second.test"]


def test_hetzner_dns_zones_and_removals_follow_the_bearer_token(fake_cloud) -> None:
    """Two credentials on one fake see different zones — the retire view's
    held-credential arm — and every removal records which one made it."""
    urls = fake_cloud.url_map()
    dns = fake_cloud.hetzner_cloud.dns
    dns.zones_by_token["entered"] = [{"id": 7, "name": "unrelated.test"}]
    names = lambda tok: [  # noqa: E731
        z["name"]
        for z in requests.get(
            f"{urls['vps']}/zones", headers={"Authorization": f"Bearer {tok}"}
        ).json()["zones"]
    ]
    assert names("entered") == ["unrelated.test"]
    assert names("held") == ["example.test"], "an unmapped token sees `zones`"
    requests.post(
        f"{urls['vps']}/zones/42/rrsets/@/A/actions/remove_records",
        json={"records": [{"value": "203.0.113.5"}]},
        headers={"Authorization": "Bearer held"},
    )
    assert dns.removal_tokens == ["held"]


def test_hetzner_get_server_answers_a_seeded_box_with_its_ptr(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.seed_managed_server(
        server_id=7, name="example-test", ptr="mail.example.test"
    )
    resp = requests.get(
        f"{urls['vps']}/servers/7", headers={"Authorization": "Bearer test-token"}
    )
    assert resp.status_code == 200
    assert resp.json()["server"]["public_net"]["ipv4"]["dns_ptr"] == "mail.example.test"
