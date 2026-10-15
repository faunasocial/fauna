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

from actions import ActionLayer
from actions.retire import RetireActions
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.launch_harness import make_launch_harness
from common.nest import stop_nest
from conftest import _seeded_environment, _trust_seeder, get_available_apps
from drivers import create_driver
from helpers.app_surface import app_name, skip_unbuilt
from helpers.box_recovery_launch import (
    app_path_for,
    claim_fresh_identity,
    rust_log,
    start_boxes,
    wait_for_custody_leg,
)
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_2, pytest.mark.tui, pytest.mark.linux, pytest.mark.web, pytest.mark.android]

#: The apps whose nest_retire page is built. tui leads; the other six join by
#: batched trickle-down (nest-retirement.md § Done definition), each by adding
#: its name here (and its app marker above).
RETIRE_APPS = ("tui", "linux", "web", "android")

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
    if client == "web":
        harness = make_launch_harness("web", spa_url=request.getfixturevalue("spa_url"))
    else:
        app_path = request.getfixturevalue(f"{client}_app_path")
        harness = make_launch_harness(
            client, tmp_path=tmp_path, app_path=app_path, file_backed=True,
            seed_trust=_trust_seeder(request),
        )
    try:
        secret_hex = secrets.token_hex(32)
        node_url = f"http://127.0.0.1:{_closed_port()}"
        driver = harness.launch_and_route(secret_hex=secret_hex, node_url=node_url, trust=None)
        wait_until(
            lambda: driver.is_visible("launch-retry-button"),
            60,
            diagnose=lambda: "the launch never reached the retry surface: "
            + driver.diagnose("error-message"),
        )
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


def _open_hetzner_list(driver, fake_cloud, *, public_dns: bool = False) -> RetireActions:
    # The override is read when the page's machine is built, so it goes in
    # before the entry is pressed. ``public_dns`` also points the attribution's
    # public-resolver fallback at the fake's DoH (``fake_cloud.doh.a_records``),
    # the only way a domain verifies when no credential covers its zone.
    urls = {"vps": fake_cloud.url_map()["vps"]}
    if public_dns:
        urls["doh"] = fake_cloud.url_map()["doh"]
    driver.set_provider_base_urls(urls)
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


#: The fake provider's publish witness (`FAKE_DNS_PUBLISH_WITNESS`) and the line
#: a publish writes once the slot names it published are saved in
#: `fauna.state.dns` (`PUBLISHED_NAMES_SAVED_LOG_LINE`), both in
#: `libs/fauna-client-dns/src/lib.rs`.
_PUBLISH_WITNESS = "e2e fake DNS provider published"
_NAMES_SAVED = "saved the published DNS slot names for"

#: The three Fauna slots the retire run's remembered-name pass withdraws, as
#: (name test, the marker its values carry) — `fauna_provisioning::txt_slots`.
_SLOTS = {
    "mail-signing key": (lambda name: "._domainkey." in name, "p="),
    "Bluesky handle record": (lambda name: name.startswith("_atproto."), "did="),
    "nest record": (lambda name: name.startswith("_fauna."), "self="),
}


def _published_slot_records(driver, domain: str) -> dict[str, set[tuple[str, str]]]:
    """Every slot TXT the fake provider's witness says the app published under
    ``domain``, by slot — the witness lists each record as ``TYPE NAME VALUE``."""
    found: dict[str, set[tuple[str, str]]] = {}
    for line in driver.app_stderr_text().splitlines():
        if _PUBLISH_WITNESS not in line or domain not in line:
            continue
        for entry in line.split(": ", 1)[-1].split(", "):
            parts = entry.strip().split(" ", 2)
            if len(parts) != 3 or parts[0] != "TXT":
                continue
            _, name, value = parts
            name = name.rstrip(".").lower()
            for slot, (is_name, marker) in _SLOTS.items():
                if is_name(name) and marker in value:
                    found.setdefault(slot, set()).add((name, value))
    return found


@pytest.mark.feature("retire-a-nest")
def test_retire_removes_the_records_the_app_itself_published(
    admin_app, nest_instance, fake_cloud
):
    """Outcome 28: the records the app published for the domain — the slot
    TXT whose names ``fauna.state.dns`` remembers — are removed too, and only
    the values carrying Fauna's marker: a foreign TXT at the same name stays.

    The names reach the held record the way they do in use: a held credential
    on ``admin-dns`` and the domain opted in to Fauna-managed, whose publish
    remembers them. That publish goes to the in-process fake provider, so the
    same records (read off its witness) are put in the cloud fake's zone — the
    zone the retire run then cleans through the held credential.

    The domain is a fresh secondary one, so the session's primary is left
    alone: its publish carries the mail-signing keys and the nest record. The
    Bluesky handle record is published on the primary only; its arm is the
    same pass over a third remembered map, unit-tested beside the other two
    (``libs/fauna-onboarding-machine/src/retire/tests.rs``)."""
    driver = admin_app.driver
    if app_name(driver) not in RETIRE_APPS:
        skip_unbuilt(
            driver,
            surface="nest_retire page",
            tracked="nest-retirement.md § Done definition (the batched trickle-down)",
        )
    if driver.log_scope_across_relaunch() == "none":
        skip_unbuilt(
            driver,
            surface="app-log reader in the e2e driver",
            detail="which slot records the app published is read off the fake provider's witness",
        )
    domain = f"rt-{secrets.token_hex(4)}.test"
    held = f"fake-dns-ok:{domain}"
    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        client.call(
            "fauna.bridges.add_local_domain",
            {"domain": domain, "mta_sts_cert_mode": "expand_primary"},
        )

    driver.enable_dns_fake_provider()
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: domain in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"{domain!r} never rendered on admin-dns: "
        + driver.diagnose("error-message"),
    )
    while admin_app.admin.dns_credential_count() > 0:
        admin_app.admin.clear_dns_credential(0)
    try:
        # -- the app publishes, and remembers what it published ---------------
        admin_app.admin.add_dns_credential("hetzner", {"api-token": held})
        wait_until(
            lambda: admin_app.admin.dns_credential_count() >= 1 or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: "the held credential never stored: "
            + driver.diagnose("error-message"),
        )
        names = admin_app.admin.dns_domain_names()
        admin_app.admin.toggle_domain_mode(index=names.index(domain))
        saved = f"{_NAMES_SAVED} {domain}"
        wait_until(
            lambda: any(saved in ln for ln in driver.app_stderr_text().splitlines()) or None,
            PROVIDER_VERIFY_S,
            diagnose=lambda: (
                f"the managed opt-in never saved the slot names it published for {domain!r} "
                f"(error: {driver.diagnose('error-message')}). App log tail:\n"
                f"{driver.app_stderr_text()[-4000:]}"
            ),
        )
        published = _published_slot_records(driver, domain)
        assert published, (
            f"the publish witnessed no slot record for {domain!r}. App log tail:\n"
            f"{driver.app_stderr_text()[-4000:]}"
        )

        # -- the cloud account: the box, its zone, those records and a foreigner
        cloud = fake_cloud.hetzner_cloud
        cloud.seed_managed_server(server_id=301, name="rt-box", ipv4=BOX_A_IP, ptr=f"mail.{domain}")
        dns = cloud.dns
        dns.zones_by_token[HETZNER_TOKEN] = [{"id": 7, "name": "unrelated.test"}]
        dns.zones_by_token[held] = [{"id": 44, "name": domain}]
        dns.seed_record("@", "A", BOX_A_IP, zone_id=44)

        def relative(fqdn: str) -> str:
            return "@" if fqdn == domain else fqdn[: -len(domain) - 1]

        # TXT go in as the provider stores them (quoted, a DKIM key chunked),
        # and `records()` reports that stored form.
        ours: dict[tuple[str, str], tuple[str, str, str]] = {}
        for slot, records in published.items():
            for fqdn, value in records:
                stored = dns.seed_txt(relative(fqdn), value, zone_id=44)
                ours[(slot, fqdn)] = (relative(fqdn), "TXT", stored)
        siblings = sorted(
            {
                (name, "TXT", dns.seed_txt(name, "foreign-verification=keep-me", zone_id=44))
                for name in {name for name, _, _ in ours.values()}
            }
        )

        # -- retire the box --------------------------------------------------
        driver.set_provider_base_urls({"vps": fake_cloud.url_map()["vps"]})
        retire = RetireActions(driver)
        retire.open_from_admin(admin_app.admin)
        retire.enter_token("hetzner", HETZNER_TOKEN)
        a = retire.row_scope("rt-box")
        assert driver.get_text("retire-server-domain", scope=a) == domain
        retire.select("rt-box")
        retire.begin_confirm()
        retire.type_name("rt-box")
        retire.confirm()
        retire.wait_done()
        assert retire.step_error(0) is None, f"the DNS step failed: {retire.step_error(0)!r}"
        assert cloud.deleted_server_ids == ["301"]

        left = dns.records(zone_id=44)
        for (slot, fqdn), record in ours.items():
            assert record not in left, (
                f"the {slot} the app published at {fqdn} is still in the zone: {left!r}"
            )
        for sibling in siblings:
            assert sibling in left, (
                f"a foreign TXT at a Fauna-known name must survive: {sibling!r} gone from {left!r}"
            )
        driver.click("retire-done-button")
        driver.wait_for("admin-nest-retire-button", timeout=15)
    finally:
        # The credential and the managed mode live in the shared session nest's
        # `fauna.state.dns`; later tests on this nest assume neither is held.
        admin_app.admin.navigate_dns()
        wait_until(
            lambda: admin_app.admin.dns_credential_count() is not None or None,
            RPC_ROUNDTRIP_S,
        )
        names = admin_app.admin.dns_domain_names()
        if domain in names:
            i = names.index(domain)
            modes = admin_app.admin.dns_domain_modes()
            if i < len(modes) and modes[i] == "Fauna-managed":
                admin_app.admin.toggle_domain_mode(index=i)
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
    # The copy names the pre-marker case with its date (nest-retirement.md §
    # Layout & flow) — comparing only against the string would stay green if the
    # date left it again, so the date is asserted on the rendered text itself.
    shown = driver.get_text("retire-server-empty-message")
    assert shown == S.onboarding.retire.empty_message
    assert "2026-07-08" in shown, shown


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


# ---------------------------------------------------------------------------
# The list, the confirm and the run — what each promises beyond the walk
# ---------------------------------------------------------------------------


def _leave_page_from_list(driver) -> None:
    """Back out of the page from the list: list → credentials → the entry."""
    driver.click("retire-back-button")
    driver.wait_for("vps-verify-button", timeout=15)
    driver.click("retire-back-button")
    driver.wait_for("launch-retire-button", timeout=15)


@pytest.mark.feature("retire-a-nest")
def test_retire_from_admin_marks_warns_and_retires_the_box_the_session_is_signed_into(
    admin_app, fake_cloud
):
    """The admin entry's session nest runs on 127.0.0.1, so a listed server at
    that address is the box the session is signed into: its row carries the
    current mark (outcome 9) and no other row does, its confirm warns the nest
    will stop existing (outcome 18) and no other box's does, deleting it from
    the admin Nest page works (outcome 5), and the app returns to its start
    afterwards (outcome 19)."""
    driver = admin_app.driver
    if app_name(driver) not in RETIRE_APPS:
        skip_unbuilt(
            driver,
            surface="nest_retire page",
            tracked="nest-retirement.md § Done definition (the batched trickle-down)",
        )
    cloud = fake_cloud.hetzner_cloud
    cloud.seed_managed_server(server_id=201, name="session-nest", ipv4="127.0.0.1", ptr=None)
    cloud.seed_managed_server(server_id=202, name="other-box", ipv4=BOX_B_IP, ptr=None)
    driver.set_provider_base_urls({"vps": fake_cloud.url_map()["vps"]})

    retire = RetireActions(driver)
    retire.open_from_admin(admin_app.admin)
    retire.enter_token("hetzner", HETZNER_TOKEN)
    mine = retire.row_scope("session-nest")
    other = retire.row_scope("other-box")
    wait_until(
        lambda: driver.is_visible("retire-server-current-badge", scope=mine) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "the signed-into box never got its mark: "
        + driver.diagnose("retire-server-current-badge", scope=mine),
    )
    assert driver.get_text("retire-server-current-badge", scope=mine) == (
        S.onboarding.retire.current_badge
    )
    assert driver.is_absent("retire-server-current-badge", scope=other)

    # -- the other box's confirm does not say the nest stops existing ---------
    retire.select("other-box")
    retire.begin_confirm()
    assert S.onboarding.retire.confirm_current not in driver.get_text("retire-confirm-summary")
    driver.click("retire-cancel-button")
    driver.wait_for("retire-delete-button", timeout=15)

    # -- the signed-into box's confirm does, and the run deletes it ----------
    retire.select("session-nest")
    retire.begin_confirm()
    summary = driver.get_text("retire-confirm-summary")
    assert S.onboarding.retire.confirm_current in summary, summary
    retire.type_name("session-nest")
    retire.confirm()
    retire.wait_done()
    assert cloud.deleted_server_ids == ["201"]

    # -- done: the session's nest is gone, so the app starts over ------------
    driver.click("retire-done-button")
    def at_the_start(state) -> bool:
        stack = (state.get("nav") or {}).get("stack") or [{}]
        return (
            (state.get("session") or {}).get("authenticated") is False
            and stack[0].get("view") == "welcome"
        )

    state = driver.get_state(wait_for=at_the_start, timeout=30)
    assert state and at_the_start(state), (
        "retiring the box the session was signed into must end the session and "
        f"return to the start; session={state and state.get('session')!r} "
        f"nav={state and state.get('nav')!r}"
    )


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_confirm_warns_and_states_the_dns_plan_and_an_unattributed_box_touches_no_dns(
    retire_driver, fake_cloud
):
    """The confirm summary says everything on the server is destroyed and names
    the DNS records it will remove (outcomes 16, 17); a box with no confirmed
    domain says none will be removed, can still be deleted, and no DNS record
    is touched for it (outcome 13)."""
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    dns = fake_cloud.hetzner_cloud.dns
    zone_before = dns.records()
    second_before = dns.records(zone_id=43)
    retire = _open_hetzner_list(driver, fake_cloud)

    # -- an attributed box: the destruction warning and the removal plan ------
    retire.select("example-test")
    retire.begin_confirm()
    summary = driver.get_text("retire-confirm-summary")
    assert S.onboarding.retire.confirm_destroyed in summary, summary
    plan_lead = S.onboarding.retire.confirm_dns_removals(records="").strip()
    assert plan_lead in summary, f"the confirm must name the records it removes: {summary!r}"
    assert BOX_A_IP in summary.split(plan_lead, 1)[1], (
        f"the planned removals must name the value that identifies them: {summary!r}"
    )
    assert S.onboarding.retire.confirm_dns_none not in summary, summary
    driver.click("retire-cancel-button")
    driver.wait_for("retire-delete-button", timeout=15)

    # -- a box with no confirmed domain: no plan, and no DNS touched ----------
    retire.select("spare-box")
    retire.begin_confirm()
    summary = driver.get_text("retire-confirm-summary")
    assert S.onboarding.retire.confirm_destroyed in summary, summary
    assert S.onboarding.retire.confirm_dns_none in summary, (
        f"with no domain confirmed the confirm must say no record will be removed: {summary!r}"
    )
    assert plan_lead not in summary, summary
    retire.type_name("spare-box")
    retire.confirm()
    retire.wait_done()
    assert fake_cloud.hetzner_cloud.deleted_server_ids == ["102"], (
        "an unattributed box can still be deleted, and only it"
    )
    assert dns.records() == zone_before, "no DNS record is touched for an unattributed box"
    assert dns.records(zone_id=43) == second_before
    assert dns.removal_tokens == [], "no removal call was made at all"


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_forgets_the_token_on_leaving_and_asks_again(retire_driver, fake_cloud):
    """The cloud token is typed every visit: leaving the page drops it, so the
    next visit opens with no provider chosen, an empty field and a dead verify."""
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    retire = _open_hetzner_list(driver, fake_cloud)
    assert retire.row_names(), "the token verified, so the list is up"

    _leave_page_from_list(driver)
    retire.open_from_launch()
    assert driver.is_absent("vps-verify-button"), (
        "a new visit opens before any provider is chosen: " + driver.diagnose("vps-verify-button")
    )
    driver.click("vps-provider-row[hetzner]")
    driver.wait_for("vps-credentials-form-api-token", timeout=10)
    assert driver.get_text("vps-credentials-form-api-token").strip() == "", (
        "the previous visit's token must not come back"
    )
    assert not driver.is_enabled("vps-verify-button"), (
        "with no token typed this visit, verify stays dead: "
        + driver.diagnose("vps-verify-button")
    )


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_a_server_already_gone_counts_as_deleted(retire_driver, fake_cloud):
    """The provider answers 404 for the delete (the box was removed in its own
    dashboard after the list was read): the run still ends done, no error."""
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    retire = _open_hetzner_list(driver, fake_cloud)
    retire.select("spare-box")
    retire.begin_confirm()
    retire.type_name("spare-box")
    fake_cloud.hetzner_cloud.managed_servers = [
        s for s in fake_cloud.hetzner_cloud.managed_servers if s["id"] != 102
    ]
    retire.confirm()
    retire.wait_done()
    assert fake_cloud.hetzner_cloud.deleted_server_ids == ["102"], (
        "the delete was attempted, and the provider said it was already gone"
    )
    assert retire.step_error(1) is None, (
        f"an already-gone server is not an error: {retire.step_error(1)!r}"
    )


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_a_run_that_stopped_leaves_the_server_listed_and_retiring_again_finishes(
    retire_driver, fake_cloud
):
    """A run that stopped on a failed DNS step and was walked away from leaves
    the server standing and listed; retiring it again finishes the job and
    lists the shared-name TXT records for removal by hand (outcomes 20, 22).
    The page keeps no state of its own across a visit, so leaving it is the
    same as the app being closed there."""
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    cloud = fake_cloud.hetzner_cloud
    cloud.dns.fail_removals = -1
    retire = _open_hetzner_list(driver, fake_cloud)
    retire.select("example-test")
    retire.begin_confirm()
    retire.type_name("example-test")
    retire.confirm()
    retire.wait_dns_failed()
    assert cloud.deleted_server_ids == []

    # Walk away from the stopped run (a stopped run leaves the page in one step).
    driver.click("retire-back-button")
    driver.wait_for("launch-retire-button", timeout=15)
    cloud.dns.fail_removals = 0

    retire.open_from_launch()
    retire.enter_token("hetzner", HETZNER_TOKEN)
    assert "example-test" in retire.row_names(), (
        "the server was never deleted, so it is still listed"
    )
    retire.select("example-test")
    retire.begin_confirm()
    retire.type_name("example-test")
    retire.confirm()
    retire.wait_done()
    assert cloud.deleted_server_ids == ["101"]
    assert retire.step_error(0) is None and retire.step_error(1) is None

    leftover = driver.get_text("retire-dns-leftover-text")
    shared_lead = S.onboarding.retire.leftover_shared_name(record="").strip()
    assert shared_lead in leftover, (
        f"the shared-name records are listed for removal by hand: {leftover!r}"
    )
    assert "_dmarc" in leftover, leftover


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_list_holds_every_server_however_many(retire_driver, fake_cloud):
    """The provider pages its list (fifty to a page in the adapter's read); the
    list on screen holds all of them."""
    driver = retire_driver
    cloud = fake_cloud.hetzner_cloud
    total = 120
    for n in range(total):
        cloud.seed_managed_server(
            server_id=1000 + n, name=f"box-{n:03d}", ipv4=f"203.0.113.{(n % 200) + 20}", ptr=None
        )
    retire = _open_hetzner_list(driver, fake_cloud)
    names = retire.row_names()
    assert len(names) == total, f"expected {total} servers listed, the page shows {len(names)}"
    assert sorted(names) == sorted(f"box-{n:03d}" for n in range(total))


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_without_dns_access_removes_nothing_and_lists_what_still_points_at_the_server(
    retire_driver, fake_cloud
):
    """The domain verifies through public DNS, but the cloud token holds no zone
    for it: the confirm says nothing will be removed and what to remove by hand,
    the run touches no record, and the by-hand list names the records still
    pointing at the deleted server, each with the value that identifies it."""
    driver = retire_driver
    _seed_hetzner_account(fake_cloud)
    cloud = fake_cloud.hetzner_cloud
    cloud.dns.zones_by_token[HETZNER_TOKEN] = [{"id": 7, "name": "unrelated.test"}]
    fake_cloud.doh.a_records["example.test"] = [BOX_A_IP]
    zone_before = cloud.dns.records()
    retire = _open_hetzner_list(driver, fake_cloud, public_dns=True)

    a = retire.row_scope("example-test")
    assert driver.get_text("retire-server-domain", scope=a) == "example.test", (
        "public DNS confirms the apex points at the box"
    )
    retire.select("example-test")
    retire.begin_confirm()
    summary = driver.get_text("retire-confirm-summary")
    assert S.onboarding.retire.confirm_dns_none in summary, summary
    by_hand = S.onboarding.retire.confirm_by_hand(records="").strip()
    assert by_hand in summary and BOX_A_IP in summary.split(by_hand, 1)[1], (
        f"the confirm lists the records to remove by hand, with their values: {summary!r}"
    )
    retire.type_name("example-test")
    retire.confirm()
    retire.wait_done()
    assert cloud.deleted_server_ids == ["101"]
    assert cloud.dns.records() == zone_before, "nothing is removed from a zone the app cannot reach"
    assert cloud.dns.removal_tokens == []

    leftover = driver.get_text("retire-dns-leftover-text")
    urgent = S.onboarding.retire.leftover_points_at_box(record="").strip()
    assert urgent in leftover and BOX_A_IP in leftover.split(urgent, 1)[1], (
        f"the by-hand list starts with the records still pointing at the server: {leftover!r}"
    )
    stale = leftover.find(S.onboarding.retire.leftover_shared_name(record="").strip())
    assert stale > leftover.index(urgent), (
        f"the records still pointing at the server come ahead of the stale ones: {leftover!r}"
    )


@pytest.mark.feature("retire-a-nest")
def test_retire_lists_another_domains_records_by_hand_when_its_dns_is_out_of_reach(
    admin_app, nest_instance, fake_cloud
):
    """A second domain the nest serves also points at the box, but the token
    holds only the first domain's zone: the first is cleaned in the provider,
    the second's records are listed for removal by hand."""
    driver = admin_app.driver
    if app_name(driver) not in RETIRE_APPS:
        skip_unbuilt(
            driver,
            surface="nest_retire page",
            tracked="nest-retirement.md § Done definition (the batched trickle-down)",
        )
    other = "deliverability-e2e.test"
    admin = nest_instance["admin"]
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as client:
        client.call(
            "fauna.bridges.add_local_domain",
            {"domain": other, "mta_sts_cert_mode": "expand_primary"},
        )
    _seed_hetzner_account(fake_cloud)
    cloud = fake_cloud.hetzner_cloud
    cloud.dns.zones_by_token[HETZNER_TOKEN] = [{"id": 42, "name": "example.test"}]
    fake_cloud.doh.a_records["example.test"] = [BOX_A_IP]
    fake_cloud.doh.a_records[other] = [BOX_A_IP]
    driver.set_provider_base_urls(
        {"vps": fake_cloud.url_map()["vps"], "doh": fake_cloud.url_map()["doh"]}
    )

    retire = RetireActions(driver)
    retire.open_from_admin(admin_app.admin)
    retire.enter_token("hetzner", HETZNER_TOKEN)
    a = retire.row_scope("example-test")
    assert driver.get_text("retire-server-domain", scope=a) == "example.test"
    assert other in driver.get_text("retire-server-secondary-domains", scope=a)
    retire.select("example-test")
    retire.begin_confirm()
    retire.type_name("example-test")
    retire.confirm()
    retire.wait_done()

    assert ("@", "A", BOX_A_IP) not in cloud.dns.records(), "the reachable domain is cleaned"
    leftover = driver.get_text("retire-dns-leftover-text")
    urgent = S.onboarding.retire.leftover_points_at_box(record="").strip()
    assert urgent in leftover and other in leftover, (
        f"the unreachable domain's records are listed by hand: {leftover!r}"
    )


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_lists_unmarked_servers_with_a_note_behind_the_same_typed_confirm(
    retire_driver, fake_cloud
):
    """Outcome 10: OVH's instance-create has no label field, so its list is
    the whole account, unfiltered — every row carries the unmarked note, and
    deleting one takes the same typed-name gate as any other row. The two
    servers sit in two different projects: the list walks every project the
    credentials see, and the delete reaches the one that holds the server."""
    driver = retire_driver
    ovh = fake_cloud.ovh
    ovh.seed_instance("proj-a", "inst-1", "old-box", BOX_A_IP)
    ovh.seed_instance("proj-b", "inst-2", "someone-elses-db", BOX_B_IP)
    driver.set_provider_base_urls({"vps": ovh.base_url()})

    retire = RetireActions(driver)
    retire.open_from_launch()
    retire.enter_credentials(
        "ovh", {"app-key": "MOCK-ak", "app-secret": "MOCK-as", "consumer-key": "MOCK-ck"}
    )
    names = retire.row_names()
    assert sorted(names) == ["old-box", "someone-elses-db"], (
        f"an unmarked provider lists every server in every project: {names!r}"
    )
    for name in names:
        scope = retire.row_scope(name)
        assert driver.is_visible("retire-server-unmarked-note", scope=scope), (
            f"{name}: " + driver.diagnose("retire-server-unmarked-note", scope=scope)
        )
        assert driver.get_text("retire-server-unmarked-note", scope=scope) == (
            S.onboarding.retire.unmarked_note
        )

    retire.select("old-box")
    retire.begin_confirm()
    retire.type_name("old-bo")
    assert not driver.is_enabled("retire-confirm-button"), (
        "an unmarked row takes the same exact-name gate: "
        + driver.diagnose("retire-confirm-button")
    )
    retire.type_name("old-box")
    assert driver.is_enabled("retire-confirm-button"), driver.diagnose("retire-confirm-button")
    assert ovh.deleted == [], "nothing runs before confirm"
    retire.confirm()
    retire.wait_done()
    assert retire.step_error(1) is None, f"the server step failed: {retire.step_error(1)!r}"
    assert ovh.deleted == [("proj-a", "inst-1")], (
        f"exactly the confirmed server is deleted, in its own project: {ovh.deleted!r}"
    )
    assert [i["id"] for i in ovh.projects["proj-b"]] == ["inst-2"]


#: The apps whose launch-retry surface offers both ``launch-retire-button`` and
#: ``launch-recover-button`` over a device's own custodied seed — the retire
#: page's apps that also run the dead-saved-nest recovery journey
#: (``test_box_recovery_two_nest.py`` T4).
_RECOVERY_APPS = [c for c in ("tui", "linux", "android") if c in _clients()]


@pytest.mark.parametrize("client", _RECOVERY_APPS)
@pytest.mark.feature("retire-a-nest")
def test_retire_keeps_the_nests_recovery_key(
    client, nest_mode, tmp_path, tmp_path_factory, request, fake_cloud
):
    """Outcome 34 (nest-retirement.md § Errors & edge cases, *Retired ≠
    forgotten*): retiring the box a device is saved against leaves the box's
    custodied deployment seed where it was, so recovery still offers it.

    A device claims a box and the custody leg rests the box's seed in the
    device's own account store; the box dies; the relaunched device lands on
    launch-retry, whose ``launch-recover-button`` the seed reveals. From that
    same surface the person retires the server — the box they were saved
    against, so the done state routes back to the launch flow — and the
    recovery entry still lists the box, then again from a cold relaunch, which
    reads only what the device kept."""
    app_path = app_path_for(client)
    (nest,), stop_boxes = start_boxes(request, nest_mode, tmp_path_factory, "retire-keeps-seed")
    try:
        secret_hex = secrets.token_hex(32)
        handle = f"admin{secrets.token_hex(3)}@127.0.0.1:{nest['port']}"
        config = {
            "app_path": app_path,
            "xdg_base": str(tmp_path / "xdg"),
            "credential_dir": str(tmp_path / "creds"),
            "keyring_app": "fauna-e2e-retire-keeps-seed",
            "environment": {**_seeded_environment(request, nest), "RUST_LOG": rust_log(client)},
        }
        if client == "android":
            # The relaunches are the same device: keep the store launch 1
            # custodied into (test_box_recovery_two_nest.py T4, the same pin).
            config["preserve_store"] = True

        def recovery_lists_the_box(app, why: str) -> None:
            app.driver.wait_for("launch-recover-button", timeout=60)
            app.click("launch-recover-button")
            app.driver.wait_for("recover-box-item-0", timeout=30)
            assert app.is_visible("recover-box-item-0"), (
                f"{why}: nest_recovery no longer lists the box — "
                + app.driver.diagnose("recover-box-item-0")
            )

        # -- launch 1: claim the box; its seed rests in this device's store ----
        driver = create_driver(client)
        driver.launch(config)
        try:
            claim_fresh_identity(ActionLayer(driver), secret_hex, handle, nest["claim_code"])
            wait_for_custody_leg(driver)
        finally:
            driver.teardown()

        # -- the box dies; the cloud account still holds its server ------------
        stop_nest(nest, graceful=True)
        cloud = fake_cloud.hetzner_cloud
        cloud.seed_managed_server(server_id=401, name="lost-box", ipv4="127.0.0.1", ptr=None)

        # -- launch 2: retire the server from the launch-retry surface ---------
        driver = create_driver(client)
        driver.launch(config)
        try:
            app = ActionLayer(driver)
            driver.wait_for("launch-recover-button", timeout=60)
            driver.set_provider_base_urls({"vps": fake_cloud.url_map()["vps"]})
            retire = RetireActions(driver)
            retire.open_from_launch()
            retire.enter_token("hetzner", HETZNER_TOKEN)
            retire.select("lost-box")
            retire.begin_confirm()
            retire.type_name("lost-box")
            retire.confirm()
            retire.wait_done()
            assert retire.step_error(1) is None, f"the server step failed: {retire.step_error(1)!r}"
            assert cloud.deleted_server_ids == ["401"]
            driver.click("retire-done-button")
            recovery_lists_the_box(app, "right after retiring the server")
        finally:
            driver.teardown()

        # -- launch 3: a cold start reads only what the device kept ------------
        driver = create_driver(client)
        driver.launch(config)
        try:
            recovery_lists_the_box(ActionLayer(driver), "after a relaunch")
        finally:
            driver.teardown()
    finally:
        stop_boxes()


@pytest.mark.parametrize("retire_driver", _clients(), indirect=True)
@pytest.mark.feature("retire-a-nest")
def test_retire_transfer_code_is_forgotten_on_leaving_and_offered_only_for_a_confirmed_domain(
    retire_driver, fake_cloud
):
    """Outcome 33: a server whose domain the app could not confirm carries no
    transfer-code button. Outcome 31: a fetched code is gone after the page is
    left and re-entered."""
    driver = retire_driver
    bundled = fake_cloud.bundled
    _seed_bundled_account(bundled)
    bundled.servers["s-2"] = {
        "id": "s-2",
        "name": "unattributed-box",
        "ipv4": "203.0.113.88",
        "status": "running",
        "labels": {"managed-by": "fauna"},
    }
    bundled.ptr["s-2"] = None

    retire = RetireActions(driver)
    retire.open_from_launch()
    retire.sign_in_bundled(bundled.base_url())

    other = retire.select("unattributed-box")
    assert driver.is_absent("retire-server-domain", scope=other)
    assert driver.is_absent("retire-transfer-code-button", scope=other), (
        "no confirmed domain, no transfer-code button"
    )

    scope = retire.select("example-test")
    driver.click("retire-transfer-code-button", scope=scope)
    driver.wait_for("retire-transfer-code-value", timeout=20, scope=scope)
    assert driver.get_text("retire-transfer-code-value", scope=scope) == "EPP-EXAMPLE.TEST"

    _leave_page_from_list(driver)
    retire.open_from_launch()
    retire.sign_in_bundled(bundled.base_url())
    scope = retire.select("example-test")
    assert driver.is_absent("retire-transfer-code-value", scope=scope), (
        "a fetched code must not survive leaving the page"
    )
    assert driver.is_visible("retire-transfer-code-button", scope=scope), (
        "the row offers to fetch it afresh: "
        + driver.diagnose("retire-transfer-code-button", scope=scope)
    )
