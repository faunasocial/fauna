"""The ``nest_retire`` page — retiring an app-provisioned nest from the app
(docs/goal/behavior/nest-retirement.md, the owner of every behavior below).

A walk against the stubbed cloud, entered from the unreachable-nest surface
(``launch_retry`` → ``launch-retire-button``), which is exactly where the view
earns its keep: the commonest reason to retire a box is that it is already
broken or gone, so the page works with no reachable nest.

* **The walk** (§ Done definition): credentials → list (a non-fauna server
  absent) → typed confirm (a wrong name keeps the button dead) → DNS step
  green → server step green → done. Asserted on the provider, not only on the
  page: the records that pointed at the box are gone from the zone, a record
  pointing elsewhere is untouched, and exactly the confirmed server was deleted.
* **The failed-DNS branch** (§ DNS cleanup — scope and order): a DNS failure
  stops the run before the server is touched and offers retry or *delete the
  server anyway*.
* **The binding negative** (§ Confirm shape — the typed name binds the run to
  that server): after a failed DNS step on one row, no gesture reaches the
  delete for a different row. The walk above drives each button only from the
  state that shows it, so without this negative it could not see that class.
* **The transfer-code leg** (§ Transfer authorization code) against the
  bundled-provider reference server, both the ``200`` code and the ``202``
  lock-lifts date.
* **The held credential** (§ Credential stance (b)), from the admin entry: the
  admin's own ``fauna.state.dns`` credential is the only one holding the box's
  zone, so the DNS step can succeed only through it.

Launched through the native launch harness with a saved identity whose nest
refuses the connection — the same seed ``test_onboarding_launch_routing_smoke``
case C uses — so the app lands on ``launch_retry`` by its real routing; no nest
runs for those. The held-credential case is the exception: it signs in as the
session nest's admin (``admin_app``) and enters from ``admin-nest``. The cloud
is ``fakes/fake_cloud.py`` throughout.
"""

from __future__ import annotations

import secrets
import socket

import pytest

from actions.retire import RetireActions
from common.launch_harness import make_launch_harness
from conftest import _trust_seeder, get_available_apps
from helpers.app_surface import app_name, skip_unbuilt
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_2, pytest.mark.tui]

#: The apps whose nest_retire page is built. tui leads; the other six join by
#: batched trickle-down (nest-retirement.md § Done definition), each by adding
#: its name here.
RETIRE_APPS = ("tui",)

BOX_A_IP = "203.0.113.5"
BOX_B_IP = "203.0.113.77"
ELSEWHERE_IP = "198.51.100.9"
HETZNER_TOKEN = "MOCK-hetzner-token"


def _clients() -> list[str]:
    available = get_available_apps()
    return [c for c in RETIRE_APPS if c in available]


def _closed_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.fixture
def retire_driver(request, tmp_path):
    """A client resting on ``launch_retry``: a saved identity whose nest url
    refuses the connection."""
    client = request.param
    app_path = request.getfixturevalue(f"{client}_app_path")
    harness = make_launch_harness(
        client, tmp_path=tmp_path, app_path=app_path, file_backed=True,
        seed_trust=_trust_seeder(request),
    )
    try:
        driver = harness.launch_and_route(
            secret_hex=secrets.token_hex(32),
            node_url=f"http://127.0.0.1:{_closed_port()}",
            trust=None,
        )
        driver.wait_for("launch-retry-button", timeout=60)
        yield driver
    finally:
        harness.teardown()


def _seed_hetzner_account(fake_cloud) -> None:
    """Box A: Fauna-created, PTR `mail.example.test`, apex `A` on its address,
    and a second domain (`second.test`) whose apex also points at it. Box B:
    Fauna-created, no domain. A server someone else created in the same
    account, which must never be listed. Plus one record in the zone that
    points elsewhere and must survive the cleanup."""
    cloud = fake_cloud.hetzner_cloud
    cloud.seed_managed_server(
        server_id=101, name="example-test", ipv4=BOX_A_IP, ptr="mail.example.test"
    )
    cloud.seed_managed_server(server_id=102, name="spare-box", ipv4=BOX_B_IP, ptr=None)
    cloud.seed_managed_server(
        server_id=999, name="someone-elses-db", ipv4="192.0.2.50", labels={"team": "data"}
    )
    dns = cloud.dns
    dns.zones.append({"id": 43, "name": "second.test"})
    dns.seed_record("@", "A", BOX_A_IP)
    dns.seed_record("mail", "A", BOX_A_IP)
    dns.seed_record("*", "A", BOX_A_IP)
    dns.seed_record("@", "MX", "10 mail.example.test.")
    dns.seed_record("www", "A", ELSEWHERE_IP)
    dns.seed_record("@", "A", BOX_A_IP, zone_id=43)


def _open_hetzner_list(driver, fake_cloud) -> RetireActions:
    # The override is read when the page's machine is built, so it goes in
    # before the entry is pressed.
    driver.set_provider_base_urls({"vps": fake_cloud.url_map()["vps"]})
    retire = RetireActions(driver)
    retire.open_from_launch()
    retire.enter_token("hetzner", HETZNER_TOKEN)
    return retire


# ---------------------------------------------------------------------------
# The walk
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_walk_lists_confirms_and_cleans_dns_before_the_server(
    retire_driver, fake_cloud
):
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    retire = _open_hetzner_list(driver, fake_cloud)

    # -- list: the account's Fauna servers, and only those ------------------
    names = retire.row_names()
    assert sorted(names) == ["example-test", "spare-box"], (
        f"the list must hold exactly the Fauna-created servers; got {names!r}"
    )
    a = retire.row_scope("example-test")
    assert driver.get_text("retire-server-domain", scope=a) == "example.test"
    assert "second.test" in driver.get_text("retire-server-secondary-domains", scope=a)
    assert driver.get_text("retire-server-address", scope=a) == BOX_A_IP
    b = retire.row_scope("spare-box")
    assert driver.is_absent("retire-server-domain", scope=b), (
        "an unverified row shows no domain — attribution never guesses"
    )
    assert not driver.is_enabled("retire-delete-button"), "nothing selected yet"

    # Hetzner has no registrar auth-code call: the selected row carries the
    # go-to-your-registrar note, never a button that would fail.
    retire.select("example-test")
    assert driver.is_visible("retire-transfer-code-status", scope=a), driver.diagnose(
        "retire-transfer-code-status", scope=a
    )
    assert driver.is_absent("retire-transfer-code-button", scope=a)

    # -- confirm: the typed-name gate ---------------------------------------
    retire.begin_confirm()
    summary = driver.get_text("retire-confirm-summary")
    for fact in ("example-test", BOX_A_IP, "example.test", "second.test"):
        assert fact in summary, f"the confirm summary must name {fact!r}: {summary!r}"
    assert not driver.is_enabled("retire-confirm-button"), "empty name → dead confirm"
    retire.type_name("example-tes")
    assert not driver.is_enabled("retire-confirm-button"), (
        "a near-miss name must keep the confirm dead: "
        + driver.diagnose("retire-confirm-button")
    )
    retire.type_name("example-test")
    assert driver.is_enabled("retire-confirm-button"), driver.diagnose("retire-confirm-button")
    assert fake_cloud.hetzner_cloud.deleted_server_ids == [], "nothing runs before confirm"
    retire.confirm()

    # -- running → done ------------------------------------------------------
    retire.wait_done()
    assert retire.step_error(0) is None, f"the DNS step failed: {retire.step_error(0)!r}"
    assert retire.step_error(1) is None, f"the server step failed: {retire.step_error(1)!r}"
    assert fake_cloud.hetzner_cloud.deleted_server_ids == ["101"], (
        "exactly the confirmed server is deleted"
    )
    left = fake_cloud.hetzner_cloud.dns.records()
    assert ("www", "A", ELSEWHERE_IP) in left, "a record pointing elsewhere is never touched"
    for gone in (("@", "A", BOX_A_IP), ("mail", "A", BOX_A_IP), ("*", "A", BOX_A_IP)):
        assert gone not in left, f"{gone} still points at the destroyed box; zone: {left!r}"
    assert fake_cloud.hetzner_cloud.dns.records(zone_id=43) == [], (
        "the second domain's apex pointed at the box too, and must be cleaned"
    )
    # The shared-name TXT stay, listed for removal by hand.
    assert driver.is_visible("retire-dns-leftover-text"), driver.diagnose("retire-dns-leftover-text")

    # Done returns to the entry that opened the page.
    driver.click("retire-done-button")
    driver.wait_for("launch-retire-button", timeout=15)


# ---------------------------------------------------------------------------
# The failed-DNS branch, and the typed name binding the run
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_failed_dns_stops_before_the_server_and_force_deletes_only_it(
    retire_driver, fake_cloud
):
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    fake_cloud.hetzner_cloud.dns.fail_removals = -1
    retire = _open_hetzner_list(driver, fake_cloud)

    retire.select("example-test")
    retire.begin_confirm()
    retire.type_name("example-test")
    retire.confirm()
    retire.wait_dns_failed()
    assert retire.step_error(0), "the DNS step shows why it failed"
    assert fake_cloud.hetzner_cloud.deleted_server_ids == [], (
        "a failed DNS step stops the run BEFORE the server is touched"
    )

    # Nothing re-targets a run once it names a server: no row is selectable.
    assert driver.count(retire.ROW) == 0, (
        "the list must not be on screen while a run names a server"
    )

    # Retry re-runs the same server; the zone still refuses, so it fails again.
    driver.click("retire-retry-button")
    retire.wait_dns_failed()
    assert fake_cloud.hetzner_cloud.deleted_server_ids == []

    # Delete anyway: the armed server, and no other.
    driver.click("retire-force-server-button")
    retire.wait_done()
    assert fake_cloud.hetzner_cloud.deleted_server_ids == ["101"]
    assert [s["id"] for s in fake_cloud.hetzner_cloud.managed_servers] == [102, 999]
    leftover = driver.get_text("retire-dns-leftover-text")
    assert BOX_A_IP in leftover, (
        "the records never removed join the by-hand list, with the value that "
        f"identifies them: {leftover!r}"
    )


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_leaving_a_failed_run_disarms_it_for_every_other_row(
    retire_driver, fake_cloud
):
    """The binding negative:
    after a failed DNS step on box A, walking back out and selecting box B
    must reach no delete — neither *retry* nor *delete anyway* is offered for
    B, and nothing was deleted."""
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    fake_cloud.hetzner_cloud.dns.fail_removals = -1
    retire = _open_hetzner_list(driver, fake_cloud)

    retire.select("example-test")
    retire.begin_confirm()
    retire.type_name("example-test")
    retire.confirm()
    retire.wait_dns_failed()

    # Leave the page from the failed run, come back, pick the other box.
    driver.click("retire-back-button")
    driver.wait_for("launch-retire-button", timeout=15)
    retire.open_from_launch()
    retire.enter_token("hetzner", HETZNER_TOKEN)
    retire.select("spare-box")
    for door in ("retire-force-server-button", "retire-retry-button", "retire-confirm-button"):
        assert driver.is_absent(door), f"{door} must not be reachable for an unconfirmed row"
    assert fake_cloud.hetzner_cloud.deleted_server_ids == [], "no gesture deleted anything"


# ---------------------------------------------------------------------------
# The admin entry's held DNS credential (§ Credential stance (b))
# ---------------------------------------------------------------------------

#: The held credential's token: the fake DNS-provider decorator's sentinel, so
#: `admin-dns` verifies and stores it with no real provider (native reads
#: ``FAUNA_DNS_PROVIDER_FAKE``, set in the tui launch config).
HELD_TOKEN = "fake-dns-ok:example.test"


@pytest.mark.feature("retire-a-nest")
def test_retire_from_admin_cleans_dns_through_the_held_credential(admin_app, fake_cloud):
    """From ``admin-nest`` the page passes the signed-in admin's held
    ``fauna.state.dns`` credentials into the machine. The token entered on the
    page reaches the box but its DNS holds only an unrelated zone; the box's
    zone is reachable ONLY through the credential the admin keeps on
    ``admin-dns`` — so a green DNS step, and every removal carrying the held
    token, prove the held credential reached the run."""
    driver = admin_app.driver
    if app_name(driver) not in RETIRE_APPS:
        skip_unbuilt(
            driver,
            surface="nest_retire page",
            tracked="nest-retirement.md § Done definition (the batched trickle-down)",
        )
    _seed_hetzner_account(fake_cloud)
    dns = fake_cloud.hetzner_cloud.dns
    dns.zones_by_token[HETZNER_TOKEN] = [{"id": 7, "name": "unrelated.test"}]
    dns.zones_by_token[HELD_TOKEN] = [{"id": 42, "name": "example.test"}]

    driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: driver.is_visible("admin-dns-add-credential-button") or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "admin-dns never rendered: " + driver.diagnose("error-message"),
    )
    while admin_app.admin.dns_credential_count() > 0:
        admin_app.admin.clear_dns_credential(0)
    try:
        admin_app.admin.add_dns_credential("hetzner", {"api-token": HELD_TOKEN})
        wait_until(
            lambda: admin_app.admin.dns_credential_count() >= 1 or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: "the held credential never stored: "
            + driver.diagnose("error-message"),
        )

        driver.set_provider_base_urls({"vps": fake_cloud.url_map()["vps"]})
        retire = RetireActions(driver)
        retire.open_from_admin(admin_app.admin)
        retire.enter_token("hetzner", HETZNER_TOKEN)
        assert "example-test" in retire.row_names()
        a = retire.row_scope("example-test")
        assert driver.get_text("retire-server-domain", scope=a) == "example.test", (
            "the domain verifies through the held credential's zone"
        )

        retire.select("example-test")
        retire.begin_confirm()
        retire.type_name("example-test")
        retire.confirm()
        retire.wait_done()
        assert retire.step_error(0) is None, f"the DNS step failed: {retire.step_error(0)!r}"
        assert fake_cloud.hetzner_cloud.deleted_server_ids == ["101"]
        left = dns.records()
        for gone in (("@", "A", BOX_A_IP), ("mail", "A", BOX_A_IP)):
            assert gone not in left, f"{gone} still points at the destroyed box; zone: {left!r}"
        assert dns.removal_tokens and set(dns.removal_tokens) == {HELD_TOKEN}, (
            f"every removal must run through the held credential: {dns.removal_tokens!r}"
        )

        driver.click("retire-done-button")
        driver.wait_for("admin-nest-retire-button", timeout=15)
    finally:
        # The credential lives in the shared session nest's `fauna.state.dns`;
        # later tests on this nest assume none is held.
        admin_app.admin.navigate_dns()
        wait_until(
            lambda: driver.is_visible("admin-dns-add-credential-button") or None,
            RPC_ROUNDTRIP_S,
        )
        while admin_app.admin.dns_credential_count() > 0:
            admin_app.admin.clear_dns_credential(0)


# ---------------------------------------------------------------------------
# Empty account
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_empty_account_says_so_and_names_the_pre_marker_case(retire_driver, fake_cloud):
    driver = retire_driver
    retire = _open_hetzner_list(driver, fake_cloud)
    assert driver.is_visible("retire-server-empty-message")
    assert driver.count(retire.ROW) == 0
    # The copy names the pre-marker case (servers set up some other way carry no
    # marker); the 2026-07-08 date left it in the 2026-09-30 compat-remnant reword.
    assert driver.get_text("retire-server-empty-message") == S.onboarding.retire.empty_message


# ---------------------------------------------------------------------------
# Transfer authorization code (the bundled provider, 200 and 202)
# ---------------------------------------------------------------------------


def _seed_bundled_account(bundled) -> None:
    bundled.zones["z-1"] = {"id": "z-1", "name": "example.test"}
    bundled.records["z-1"] = [
        {"id": "r-apex", "type": "A", "name": "@", "value": BOX_A_IP, "ttl": 300}
    ]
    bundled.servers["s-1"] = {
        "id": "s-1",
        "name": "example-test",
        "ipv4": BOX_A_IP,
        "status": "running",
        "labels": {"managed-by": "fauna"},
    }
    bundled.ptr["s-1"] = "mail.example.test"


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.parametrize("lock_until", [None, "2026-12-01T00:00:00Z"], ids=["200", "202"])
@pytest.mark.feature("retire-a-nest")
def test_retire_transfer_code_leg(retire_driver, fake_cloud, lock_until):
    driver = retire_driver
    bundled = fake_cloud.bundled
    _seed_bundled_account(bundled)
    bundled.transfer_lock_until = lock_until

    retire = RetireActions(driver)
    retire.open_from_launch()
    retire.sign_in_bundled(bundled.base_url())
    scope = retire.select("example-test")
    driver.click("retire-transfer-code-button", scope=scope)
    if lock_until is None:
        driver.wait_for("retire-transfer-code-value", timeout=20, scope=scope)
        assert driver.get_text("retire-transfer-code-value", scope=scope) == "EPP-EXAMPLE.TEST"
        assert driver.is_visible("retire-transfer-code-copy-button", scope=scope)
    else:
        driver.wait_for("retire-transfer-code-status", timeout=20, scope=scope)
        status = driver.get_text("retire-transfer-code-status", scope=scope)
        assert "2026-12-01" in status, f"the lock-lifts date must be shown: {status!r}"
        assert driver.is_absent("retire-transfer-code-value", scope=scope)
    # Fetching the code deletes nothing.
    assert list(bundled.servers) == ["s-1"]
