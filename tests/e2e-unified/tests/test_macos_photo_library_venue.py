"""The macOS photo-library venue stays INSIDE its gates.

The venue (`drivers/macos.py` § the photo-library venue, `e2e-conventions.md`
convention 12's macOS arm) is the one macOS launch allowed to write into the box's
REAL System Photo Library. Its own suite (`tests/real_session/
test_photo_backup_macos.py`) is opt-in and runs rarely, so the property that
matters most — **no ordinary launch can reach the library** — cannot live there:
a regression that let a default macOS sweep seed photos would sit on `origin/main`
until someone next opted in. These run in every ordinary collection and need no
build, no bundle and no macOS.
"""

from types import SimpleNamespace

import pytest

import conftest
from drivers import macos
from drivers.macos import MacosInProcessDriver

pytestmark = pytest.mark.tier_1


def _driver(launch_mode: str | None) -> MacosInProcessDriver:
    """A driver instance with no launch, in the given mode."""
    d = MacosInProcessDriver.__new__(MacosInProcessDriver)
    if launch_mode is not None:
        d._launch_mode = launch_mode
    return d


@pytest.mark.parametrize("mode", [None, "binary", "bundle"])
@pytest.mark.parametrize("seam", [
    lambda d: d.add_photo_to_library("/nonexistent.png"),
    lambda d: d.grant_photos_access(),
    lambda d: d.request_photos_access(timeout=1),
], ids=["seed", "grant", "request"])
def test_every_ordinary_launch_refuses_the_photo_library_seams(mode, seam):
    """Convention 10: a seed from a default launch would put a real photo into the
    host's real library. The refusal must come BEFORE any command is sent."""
    d = _driver(mode)
    d.call_command = lambda *a, **k: pytest.fail("a command reached the app")
    with pytest.raises(RuntimeError, match="photo-library venue"):
        seam(d)


def test_the_venue_mode_sends_the_seed_and_proves_it_landed():
    d = _driver("photo-library")
    sent = []

    def call_command(action, payload=None, timeout=None):
        sent.append((action, payload))
        return '{"local_identifier": "ABC/L0/001", "filename": "x.png", "library_count": 1}'

    d.call_command = call_command
    d.add_photo_to_library("/tmp/x.png")
    assert sent == [("photo_backup_seed_library", {"path": "/tmp/x.png"})]


def test_a_seed_answered_without_an_asset_is_a_named_fixture_failure():
    d = _driver("photo-library")
    d.call_command = lambda *a, **k: None
    # Before sign-in the snapshot carries no `error`; the refusal is read off the
    # app's own loud log line instead (measured on the venue, 2026-09-26).
    d._get = lambda path: {"state": {}}
    d.app_stderr_text = lambda: (
        "ERROR fauna_client: [TestAgent] test agent refused command: "
        "photo_backup_seed_library: Photos access is notDetermined, so …\n")
    with pytest.raises(RuntimeError, match="notDetermined"):
        d.add_photo_to_library("/tmp/x.png")


def test_the_venue_bundle_id_is_never_the_shipped_apps():
    """The one Photos grant belongs to the test bundle, never to `social.fauna.fauna`."""
    assert macos.PHOTO_LIBRARY_BUNDLE_ID != macos.BUNDLE_ID
    assert macos.PHOTO_LIBRARY_BUNDLE_ID.startswith(macos._E2E_BUNDLE_ID_PREFIX + ".")


def test_the_signing_identity_is_selected_by_hash_and_never_named(monkeypatch):
    """The certificate's common name carries a person's name; the driver uses the
    hash and must not echo the name anywhere."""
    listing = (
        '  1) 1111111111111111111111111111111111111111 "Apple Distribution: Org (TEAM1)"\n'
        '  2) 2222222222222222222222222222222222222222 "Apple Development: A Person (TEAM2)"\n'
        "     2 valid identities found\n"
    )
    monkeypatch.setattr(macos.subprocess, "run",
                        lambda *a, **k: SimpleNamespace(stdout=listing, returncode=0))
    assert macos.apple_development_identity() == "2222222222222222222222222222222222222222"


def test_no_development_identity_is_a_named_refusal(monkeypatch):
    monkeypatch.setattr(macos.subprocess, "run",
                        lambda *a, **k: SimpleNamespace(stdout="  0 valid identities found\n",
                                                        returncode=0))
    with pytest.raises(RuntimeError, match="Apple Development"):
        macos.apple_development_identity()


def _item(nodeid: str, *markers: str):
    return SimpleNamespace(
        nodeid=nodeid,
        get_closest_marker=lambda name: name if name in markers else None,
    )


def _request(*items):
    return SimpleNamespace(session=SimpleNamespace(items=list(items)))


def test_an_ordinary_run_never_selects_the_venue():
    assert conftest._macos_photo_library_venue(
        _request(_item("tests/test_x.py::t", "macos"))) is False


def test_a_venue_run_selects_it():
    assert conftest._macos_photo_library_venue(_request(
        _item("tests/real_session/test_photo_backup_macos.py::t",
              "macos", "real_photos_library"))) is True


def test_a_mixed_selection_is_refused_rather_than_swept_into_the_venue():
    """The macOS driver is session-scoped, so one venue test would make EVERY macOS
    launch of the run the venue — an ordinary test swept in would run against the
    real library."""
    with pytest.raises(pytest.UsageError, match="must run alone"):
        conftest._macos_photo_library_venue(_request(
            _item("tests/real_session/test_photo_backup_macos.py::t",
                  "macos", "real_photos_library"),
            _item("tests/test_folders.py::t", "macos"),
        ))
