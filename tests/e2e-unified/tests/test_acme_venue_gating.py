"""The `acme_pebble` stack is the shared venue on the Linux dev VM, and only its marked tests
may need it.

`docs/goal/architecture/e2e-gated-seeds.md` § The ACME directory seed → *The
shared venue*. The seed is session-wide (a native app driver is launched once,
with its environment), so one marked test in a selection asks for the stack
from EVERY app launch of the run. A harness that cannot reach the venue must
therefore seed nothing and leave every unmarked test alone — the lesson of the
docker-era gate this replaces, when one ios run on the macOS dev VM took 212 setup errors
on tests that have nothing to do with a certificate.

Reach is checked, never assumed (decision 7): on the Linux dev VM, the venue's machine, a
venue that does not answer is ensured, and one still down fails the marked
tests; on the macOS and Windows dev VMs it is an environment skip naming the venue. The
venue's wiring is constants beside the image pin (decision 3) — pinned here
against the fleet facts they mirror. No build, no docker.
"""

import re
from pathlib import Path
from types import SimpleNamespace

import pytest

from conftest import _apply_acme_pebble_env, _skip_acme_pebble_without_venue
from fakes import fake_dns
from fakes.fake_dns import FakeDns
from helpers import acme_pebble, acme_venue, feature_ledger

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]


class _Item:
    def __init__(self, marked: bool):
        self.marked = marked

    def get_closest_marker(self, name):
        return object() if self.marked and name == "acme_pebble" else None


class _Request:
    """A launch's `request`: it counts the asks for the stack's fixture."""

    def __init__(self, *items):
        self.session = SimpleNamespace(items=list(items))
        self.asked = []

    def getfixturevalue(self, name):
        self.asked.append(name)
        return SimpleNamespace(env=lambda: {"FAUNA_E2E_ACME_DIRECTORY_URL": "https://seed"})


@pytest.fixture
def venue(monkeypatch):
    """Set whether the venue answers, and which machine this harness is on."""

    def set_state(ready: bool, machine: str = "macos"):
        monkeypatch.setattr(acme_pebble, "venue_ready",
                            lambda: (ready, "" if ready else "the venue does not answer"))
        monkeypatch.setattr(acme_pebble, "host_os", lambda: machine)

    return set_state


def test_an_unreachable_venue_is_never_asked_for_at_a_launch(venue):
    venue(False)
    env, request = {}, _Request(_Item(marked=True), _Item(marked=False))
    _apply_acme_pebble_env(env, request)
    assert request.asked == [], "the launch asked for a venue that does not answer"
    assert env == {}, "a launch with no venue must keep production's CA constant"


def test_a_reachable_venue_seeds_the_launch(venue):
    venue(True)
    env, request = {}, _Request(_Item(marked=True))
    _apply_acme_pebble_env(env, request)
    assert request.asked == ["acme_pebble"]
    assert env == {"FAUNA_E2E_ACME_DIRECTORY_URL": "https://seed"}


def test_a_selection_with_no_marked_test_asks_for_nothing(venue):
    venue(True)
    env, request = {}, _Request(_Item(marked=False))
    _apply_acme_pebble_env(env, request)
    assert request.asked == [] and env == {}


@pytest.mark.parametrize("machine", ["macos", "windows"])
def test_a_marked_test_off_the_venue_os_with_no_venue_is_an_environment_skip(venue, monkeypatch, machine):
    venue(False, machine)
    noted = []
    monkeypatch.setattr(feature_ledger, "note_skip_class", noted.append)
    with pytest.raises(pytest.skip.Exception, match=r"^environment: .*venue"):
        _skip_acme_pebble_without_venue(_Item(marked=True))
    assert noted == [feature_ledger.ENVIRONMENT]


def test_a_marked_test_on_the_venue_os_with_no_venue_fails(venue):
    venue(False, "linux")
    with pytest.raises(pytest.fail.Exception, match="venue"):
        _skip_acme_pebble_without_venue(_Item(marked=True))


def test_an_unmarked_test_with_no_venue_runs(venue):
    venue(False, "linux")
    _skip_acme_pebble_without_venue(_Item(marked=False))


def test_a_marked_test_with_the_venue_runs(venue):
    venue(True, "windows")
    _skip_acme_pebble_without_venue(_Item(marked=True))


@pytest.mark.parametrize(
    "machine, override, host",
    [("linux", None, "127.0.0.1"), ("macos", None, "10.1.8.35"), ("windows", None, "10.1.8.35"),
     ("linux", "private", "10.1.8.35"), ("macos", "loopback", "127.0.0.1")],
)
def test_the_seeded_address_is_derived_from_the_machine(monkeypatch, machine, override, host):
    monkeypatch.setattr(acme_pebble, "host_os", lambda: machine)
    acme_pebble.set_reach_override(override)
    try:
        assert acme_pebble.venue_host() == host
    finally:
        acme_pebble.set_reach_override(None)


def test_an_unknown_reach_is_refused():
    with pytest.raises(ValueError):
        acme_pebble.set_reach_override("0.0.0.0")


def test_on_the_venue_os_an_unanswering_venue_is_ensured_before_it_is_given_up(monkeypatch):
    answers = iter([False, True])
    ensured = []
    monkeypatch.setattr(acme_pebble, "host_os", lambda: "linux")
    monkeypatch.setattr(acme_pebble, "directory_answers", lambda host, timeout=3: next(answers))
    monkeypatch.setattr(acme_venue, "ensure", lambda: ensured.append(1) or "created")
    acme_pebble.venue_ready.cache_clear()
    try:
        assert acme_pebble.venue_ready() == (True, "")
        assert ensured == [1]
    finally:
        acme_pebble.venue_ready.cache_clear()


def test_off_the_venue_os_an_unanswering_venue_is_never_ensured(monkeypatch):
    monkeypatch.setattr(acme_pebble, "host_os", lambda: "macos")
    monkeypatch.setattr(acme_pebble, "directory_answers", lambda host, timeout=3: False)
    monkeypatch.setattr(acme_venue, "ensure", lambda: pytest.fail("ensured off the venue's OS"))
    acme_pebble.venue_ready.cache_clear()
    try:
        ok, why = acme_pebble.venue_ready()
        assert not ok and "Linux dev VM" in why and "10.1.8.35" in why
    finally:
        acme_pebble.venue_ready.cache_clear()


def test_the_private_address_is_the_fleet_brokers_host():
    broker = _REPO / "scripts" / "core-broker.py"
    if not broker.exists():
        pytest.skip("a tree without the fleet broker has no address to agree with")
    text = broker.read_text(encoding="utf-8")
    m = re.search(r'^BROKER_ADDR = f"([0-9.]+):', text, re.M)
    assert m, "core-broker.py no longer spells BROKER_ADDR as an f-string host:port"
    assert acme_pebble.VENUE_PRIVATE_ADDR == m.group(1)


def test_the_ports_are_fixed_outside_the_ephemeral_range_and_off_pebbles_own():
    ports = {acme_pebble.VENUE_DIRECTORY_PORT, acme_pebble.VENUE_NAME_SERVER_PORT}
    assert len(ports) == 2 and all(p < 32768 for p in ports)
    assert 14000 not in ports, "the Rust pebble testkit binds 14000 on the host network"


def test_the_venue_publishes_on_the_two_addresses_alone():
    args = acme_venue._publish(14453, 53, udp=True) + acme_venue._publish(14400, 14000)
    flags = [a for i, a in enumerate(args) if i % 2 == 0]
    specs = [a for i, a in enumerate(args) if i % 2 == 1]
    hosts = {spec.split(":")[0] for flag, spec in zip(flags, specs) if flag == "-p"}
    assert hosts == {"127.0.0.1", acme_pebble.VENUE_PRIVATE_ADDR}


def test_the_spellings_the_venue_mirrors_agree():
    from tests.platform.docker.helpers import SCANNER_SIDECAR_IMAGE

    assert acme_venue.SIDECAR_IMAGE == SCANNER_SIDECAR_IMAGE
    assert acme_pebble.QUERY_COUNT_LABEL == fake_dns.QUERY_COUNT_LABEL


def test_the_venue_hash_moves_with_the_name_server_it_runs(monkeypatch, tmp_path):
    before = acme_venue.venue_hash()
    changed = tmp_path / "fake_dns.py"
    changed.write_bytes(acme_venue.FAKE_DNS.read_bytes() + b"\n# changed\n")
    monkeypatch.setattr(acme_venue, "FAKE_DNS", changed)
    assert acme_venue.venue_hash() != before


def test_publish_txt_and_txt_queries_speak_the_name_servers_protocol(monkeypatch):
    """The harness's two arms against a real `FakeDns` on loopback: the publish
    REPLACES the name's value, and the count reads what the fake answered."""
    dns = FakeDns(host="127.0.0.1", port=0).start()
    try:
        monkeypatch.setattr(acme_pebble, "VENUE_NAME_SERVER_PORT", dns.port)
        stack = acme_pebble.AcmePebble(host="127.0.0.1")
        name = "_acme-challenge.gate-row.test"
        stack.publish_txt(name, "first")
        stack.publish_txt(name, "second")
        assert dns.served_txt(name) == ["second"]
        assert stack.txt_queries(name) == 1, "served_txt's one query is the one counted"
    finally:
        dns.stop()


@pytest.mark.parametrize(
    "nameserver, seeded",
    [("127.0.0.1:14453", True), ("10.1.8.35:14453", False)],
    ids=["loopback", "private"],
)
def test_a_nest_is_seeded_with_the_name_server_only_at_loopback(monkeypatch, nameserver, seeded):
    """The `test-hooks` nest's name-server reading serves web alone, panics on
    any non-loopback value, and web runs on the Linux dev VM alone with the loopback
    reach — so a private-address seed (every macOS/Windows run, and the
    `--acme-venue-reach private` proof on the Linux dev VM) must never reach a nest's
    launch environment, or every nest of the run exits at startup."""
    import conftest

    monkeypatch.setattr(conftest, "_acme_pebble_seed",
                        lambda request: {"FAUNA_E2E_ACME_NAMESERVER": nameserver,
                                         "FAUNA_E2E_ACME_DIRECTORY_URL": "https://x/dir"})
    env = conftest._nest_launch_seed_env(object())
    assert env == ({"FAUNA_E2E_ACME_NAMESERVER": nameserver} if seeded else {})
