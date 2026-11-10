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


def test_fake_cloud_url_map_has_four_keys(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    assert set(urls.keys()) == {"vps", "dns", "nest", "doh"}
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
    """`?name=` is find_server_by_name's pre-flight — a SEEDED account box must
    never answer it, or provisioning would think every box already exists."""
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.seed_managed_server()
    resp = requests.get(
        f"{urls['vps']}/servers?name=example-test",
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 200
    assert resp.json()["servers"] == []


def test_hetzner_pre_flight_finds_the_box_an_earlier_order_bought(fake_cloud) -> None:
    """An order is remembered: the pre-flight then finds that box by its name,
    so a Retry, a start over or a relaunch resumes it instead of buying a
    second one (`onboarding-provisioning.md` § 6, the one-box resume) — and the
    order count is what a resume journey asserts."""
    urls = fake_cloud.url_map()
    auth = {"Authorization": "Bearer test-token"}
    assert fake_cloud.hetzner_cloud.server_orders == []
    before = requests.get(f"{urls['vps']}/servers?name=example-test", headers=auth)
    assert before.json()["servers"] == []

    created = requests.post(
        f"{urls['vps']}/servers",
        json={"name": "example-test", "server_type": "cx23", "image": "ubuntu-26.04"},
        headers=auth,
    )
    assert created.status_code == 201
    box = created.json()["server"]

    found = requests.get(f"{urls['vps']}/servers?name=example-test", headers=auth)
    assert [s["id"] for s in found.json()["servers"]] == [box["id"]]
    assert found.json()["servers"][0]["public_net"]["ipv4"]["ip"] == (
        box["public_net"]["ipv4"]["ip"]
    )
    other = requests.get(f"{urls['vps']}/servers?name=another-test", headers=auth)
    assert other.json()["servers"] == []
    assert [o["name"] for o in fake_cloud.hetzner_cloud.server_orders] == ["example-test"]
    # The retire view's account is a different list: an order never lands in it.
    assert fake_cloud.hetzner_cloud.managed_servers == []


def test_hetzner_a_failed_order_buys_nothing(fake_cloud) -> None:
    urls = fake_cloud.url_map()
    fake_cloud.hetzner_cloud.create_server_always_fails()
    resp = requests.post(
        f"{urls['vps']}/servers",
        json={"name": "example-test"},
        headers={"Authorization": "Bearer test-token"},
    )
    assert resp.status_code == 500
    assert fake_cloud.hetzner_cloud.server_orders == []


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


def test_namecheap_refuses_an_unallowlisted_source_with_http_200_and_echoes_it(fake_cloud) -> None:
    nc = fake_cloud.namecheap
    params = {"ApiUser": "u", "ApiKey": "k", "UserName": "u",
              "ClientIp": "192.0.2.1", "Command": "namecheap.domains.getList"}
    resp = requests.get(nc.base_url(), params=params)
    # Namecheap's API-level refusal rides HTTP 200 (namecheap_api.rs::ApiError).
    assert resp.status_code == 200
    assert 'Status="ERROR"' in resp.text
    assert f'<Error Number="1011150">Invalid request IP: {nc.observed_ip}</Error>' in resp.text
    # The echoed address as ClientIp changes nothing: the gate is the source.
    resp = requests.get(nc.base_url(), params={**params, "ClientIp": nc.observed_ip})
    assert 'Status="ERROR"' in resp.text
    assert [r["ClientIp"] for r in nc.requests] == ["192.0.2.1", nc.observed_ip]


def test_namecheap_lists_domains_once_the_source_is_allowlisted(fake_cloud) -> None:
    nc = fake_cloud.namecheap
    nc.allowlisted.add(nc.observed_ip)
    resp = requests.get(nc.base_url(), params={"Command": "namecheap.domains.getList"})
    assert 'Status="OK"' in resp.text
    assert '<Domain ID="0" Name="example.test" />' in resp.text
