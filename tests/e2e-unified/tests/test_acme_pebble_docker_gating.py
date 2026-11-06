"""The `acme_pebble` stack is docker's, and only its marked tests may need docker.

`docs/goal/architecture/e2e-gated-seeds.md` § The ACME directory seed. The seed
is session-wide (a native app driver is launched once, with its environment), so
one marked test in a selection asks for the stack from EVERY app launch of the
run. On a box with no docker that ask used to raise `FileNotFoundError` from a
session fixture, whose failure pytest replays at each later launch: one ios run
on macOS took 212 setup errors on tests that have nothing to do with a
certificate, and read as red across 27 pages.

So a box that cannot run the stack skips the marked tests as an environment skip
and leaves every other test of the selection alone. No build, no docker.
"""

from types import SimpleNamespace

import pytest

from conftest import _apply_acme_pebble_env, _skip_acme_pebble_without_docker
from helpers import acme_pebble, feature_ledger

pytestmark = pytest.mark.tier_1


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
def docker(monkeypatch):
    """Set whether this box can run the stack."""

    def set_usable(usable: bool):
        monkeypatch.setattr(acme_pebble, "docker_usable", lambda: usable)

    return set_usable


def test_a_docker_less_launch_never_asks_for_the_stack(docker):
    docker(False)
    env, request = {}, _Request(_Item(marked=True), _Item(marked=False))
    _apply_acme_pebble_env(env, request)
    assert request.asked == [], "the launch asked a docker-less box for the pebble stack"
    assert env == {}, "a launch with no stack must keep production's CA constant"


def test_a_docker_box_still_seeds_the_launch(docker):
    docker(True)
    env, request = {}, _Request(_Item(marked=True))
    _apply_acme_pebble_env(env, request)
    assert request.asked == ["acme_pebble"]
    assert env == {"FAUNA_E2E_ACME_DIRECTORY_URL": "https://seed"}


def test_a_selection_with_no_marked_test_starts_nothing(docker):
    docker(True)
    env, request = {}, _Request(_Item(marked=False))
    _apply_acme_pebble_env(env, request)
    assert request.asked == [] and env == {}


def test_a_marked_test_on_a_docker_less_box_is_an_environment_skip(docker, monkeypatch):
    docker(False)
    noted = []
    monkeypatch.setattr(feature_ledger, "note_skip_class", noted.append)
    with pytest.raises(pytest.skip.Exception, match=r"^environment: .*docker"):
        _skip_acme_pebble_without_docker(_Item(marked=True))
    assert noted == [feature_ledger.ENVIRONMENT]


def test_an_unmarked_test_on_a_docker_less_box_runs(docker):
    docker(False)
    _skip_acme_pebble_without_docker(_Item(marked=False))


def test_a_marked_test_on_a_docker_box_runs(docker):
    docker(True)
    _skip_acme_pebble_without_docker(_Item(marked=True))


@pytest.mark.parametrize(
    "cli, daemon_exit, usable",
    [(None, None, False), ("/bin/docker", 1, False), ("/bin/docker", 0, True)],
    ids=["no-cli", "no-daemon", "both"],
)
def test_docker_usable_wants_the_cli_and_its_daemon(monkeypatch, cli, daemon_exit, usable):
    acme_pebble.docker_usable.cache_clear()
    monkeypatch.setattr(acme_pebble.shutil, "which", lambda name: cli)
    monkeypatch.setattr(
        acme_pebble.subprocess, "run",
        lambda *a, **k: SimpleNamespace(returncode=daemon_exit),
    )
    assert acme_pebble.docker_usable() is usable
    acme_pebble.docker_usable.cache_clear()
