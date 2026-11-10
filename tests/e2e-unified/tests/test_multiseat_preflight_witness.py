"""tier_1 gates for the multiseat preflight's NEST WITNESS — the app-free read
that runs before any UI step.

Why this file exists: the Windows leg of the live filesync preflight (2026-11-08)
spent 150 s in the tui app's sign-in and failed with
``sign-in never reached the feed within 150s; error=''``. The only signal was
in the app log — the nest answering ``fauna.auth.not_registered`` to the seed
the module resolved from ``~/.fauna-id``, because the box knows no account for
that seed. A read-only signed ``fauna.folders.list`` with the same seed got the
same answer with no app involved, so the client was never at fault: the box
holds no identity for this machine's seed, which is a fact about the box and
the seed (`testing.md` § Default app and nest mode → *Live mode*, the live
preflight), not about any feature.

So the witness classifies its own failure with the live preflight's rule
(``live_box_door.box_not_ready``): a seed the box does not know skips as
environment, naming the seed's source, and the preflight asks the nest BEFORE
the UI sign-in so that answer costs one round trip instead of a 150 s wait.
Any other failure of the witness still settles nothing (``None``) and the
client check runs as before.
"""

from __future__ import annotations

import pytest

import tests.test_filesync_multiseat_live as ms
from clients._ws_rpc_core import RpcCallError
from helpers import feature_ledger as fl

pytestmark = [pytest.mark.tier_1]


def _not_registered(*_a, **_k):
    raise RpcCallError(
        "fauna.auth.not_registered", "error.auth.not_registered", {}, "actor not registered"
    )


@pytest.fixture
def _seed(monkeypatch):
    monkeypatch.setattr(ms, "SECRET", "11" * 32)
    monkeypatch.setattr(ms, "SEAT_FILE", _SEAT_FILE)
    monkeypatch.setattr(ms.cfg, "nest_url", lambda: "https://box.example")


_SEAT_FILE = "/home/seat/.config/fauna/staging-box/box.example/accounts/multiseat-seat.json"


def test_a_seed_the_box_does_not_know_skips_as_environment_naming_its_source(_seed, monkeypatch):
    import common.auth

    monkeypatch.setattr(common.auth, "user_folders_list", _not_registered)
    with pytest.raises(pytest.skip.Exception) as skipped:
        ms._nest_has_set()
    text = str(skipped.value)
    assert text.startswith("environment: ")
    assert "fauna.auth.not_registered" in text
    assert _SEAT_FILE in text
    assert "seat account" in text
    assert "https://box.example" in text
    assert fl.skip_class_this_test() == fl.ENVIRONMENT


@pytest.mark.parametrize(
    "exc",
    [
        RpcCallError("fauna.auth.signature_failed", "errors.sig", {}, None),
        ConnectionRefusedError(111, "Connection refused"),
    ],
    ids=repr,
)
def test_any_other_witness_failure_still_settles_nothing(_seed, monkeypatch, exc):
    import common.auth

    def _raise(*_a, **_k):
        raise exc

    monkeypatch.setattr(common.auth, "user_folders_list", _raise)
    assert ms._nest_has_set() is None


def test_the_preflight_asks_the_nest_before_the_ui_sign_in(monkeypatch):
    """An unregistered seed must be told before the UI sign-in's 150 s wait."""
    signed_in: list[str] = []

    def _skip():
        pytest.skip("environment: the box does not know this seed")

    monkeypatch.setattr(ms, "_seat_of", lambda driver: "windows")
    monkeypatch.setattr(ms, "_nest_has_set", _skip)
    monkeypatch.setattr(ms, "_sign_in", lambda app, seat: signed_in.append(seat))
    monkeypatch.setattr(ms, "_wait_connected", lambda app, seat: None)

    class _App:
        driver = object()

    with pytest.raises(pytest.skip.Exception):
        ms.test_preflight_this_seat_sees_the_shared_set(_App())
    assert signed_in == []
