"""tier_1: the live admin probe is claim-aware on a declared-disposable run.

The live provider's admin probe (`_LiveProvider._admin`, `preflight_admin`) is
the harness's first signed round trip with the box. When the box answers
``fauna.auth.not_registered`` it holds no identity for the admin seed —
unclaimed, reset, or another box's seed — and the probe skips the run as
environment (`helpers/live_box_door.py`, `feature-catalog.md` § Cell
semantics).

That skip is right on a shared run, and wrong on exactly one run: a run that
declared its box disposable (`--live-box disposable`, `testing.md` § The
shared-box rule → *The disposable-box declaration*) AND handed the harness the
box's claim code (``FAUNA_LIVE_CLAIM_CODE``). That run came to claim the box —
its legs carry their own UI claim branches (`helpers/live_admin.reach_admin_shell`'s
``claim_code``, the mail leg's claim with the code) — and the probe skipping
first made those branches unreachable: a dev nest left unclaimed by a reset
could only be recovered by hand (measured 2026-10-23, after a tui factory-reset
run left dev.fauna.build unclaimed).

Pinned here, headlessly (no box, no driver):

  1. On a declared-disposable run with a claim code, a ``not_registered`` admin
     probe raises `LiveBoxAwaitsClaim` — never a skip.
  2. Without the declaration, or without the code, it still skips as
     environment: an ambient claim code never makes a shared box claimable.
  3. Every other failure is unchanged by the code (an unreachable box still
     skips; a typed answer that is not ``not_registered`` still raises).
  4. `preflight_admin` answers whether the box is ready: True on a good
     handshake, False when the box awaits this run's claim.
  5. `_LiveProvider._admin` hands back an admin that AWAITS its claim (no
     bearer) instead of skipping, so the session nest resolves.
  6. The session fixtures honour it: `_driver_cache` does not provision the
     session `test_user` against a box awaiting its claim, `test_user` skips
     as environment while the box still awaits it, and the mail leg claims
     with the run's code instead of resetting a box it cannot sign into.
"""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

from helpers import live_box_door, nest_mode

pytestmark = pytest.mark.tier_1

_ROOT = Path(__file__).resolve().parent.parent
_BOX_URL = "https://dev.fauna.build"
_SEED = "44" * 32


class _Refused(Exception):
    """A typed nest answer, shaped like `RpcCallError` (a ``code`` attribute)."""

    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


@pytest.fixture
def _run_mode():
    saved = nest_mode.run_mode()
    yield nest_mode.set_run_mode
    nest_mode.set_run_mode(saved)


def _shared():
    return nest_mode.declare_box(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"), nest_mode.BOX_SHARED)


def _disposable():
    return nest_mode.declare_box(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"), nest_mode.BOX_DISPOSABLE)


def _probe(exc: BaseException):
    with live_box_door.reaching_the_live_box("probing", admin_probe=True):
        raise exc


# ── 1–3. the door ──────────────────────────────────────────────────────────


def test_a_disposable_run_with_a_claim_code_awaits_the_claim(_run_mode, monkeypatch):
    monkeypatch.setenv("FAUNA_LIVE_CLAIM_CODE", "ABC123")
    _run_mode(_disposable())
    assert live_box_door.claim_pending() is True
    with pytest.raises(live_box_door.LiveBoxAwaitsClaim, match="claim"):
        _probe(_Refused("fauna.auth.not_registered"))


@pytest.mark.parametrize("declared, code", [("shared", "ABC123"), ("disposable", "")])
def test_without_both_halves_a_not_registered_probe_still_skips(_run_mode, monkeypatch, declared, code):
    monkeypatch.setenv("FAUNA_LIVE_CLAIM_CODE", code)
    _run_mode(_disposable() if declared == "disposable" else _shared())
    assert live_box_door.claim_pending() is False
    with pytest.raises(pytest.skip.Exception, match="not ready"):
        _probe(_Refused("fauna.auth.not_registered"))


def test_the_claim_code_changes_nothing_else(_run_mode, monkeypatch):
    monkeypatch.setenv("FAUNA_LIVE_CLAIM_CODE", "ABC123")
    _run_mode(_disposable())
    with pytest.raises(pytest.skip.Exception, match="did not answer"):
        _probe(ConnectionRefusedError("refused"))
    with pytest.raises(_Refused):
        _probe(_Refused("fauna.auth.forbidden"))
    # Off the admin probe the same answer is the product speaking.
    with pytest.raises(_Refused):
        with live_box_door.reaching_the_live_box("provisioning"):
            raise _Refused("fauna.auth.not_registered")


# ── 4. preflight_admin answers readiness ───────────────────────────────────


def test_preflight_admin_answers_whether_the_box_is_ready(_run_mode, monkeypatch):
    import common.auth

    monkeypatch.setenv("FAUNA_LIVE_CLAIM_CODE", "ABC123")
    _run_mode(_disposable())
    monkeypatch.setattr(common.auth, "mint_token_via_handshake", lambda url, sk: "tok")
    assert live_box_door.preflight_admin(_BOX_URL, _SEED, "test") is True

    def refuse(url, sk):
        raise _Refused("fauna.auth.not_registered")

    monkeypatch.setattr(common.auth, "mint_token_via_handshake", refuse)
    assert live_box_door.preflight_admin(_BOX_URL, _SEED, "test") is False


# ── 5. the provider hands back an admin awaiting its claim ─────────────────


def test_the_live_provider_hands_back_an_admin_awaiting_its_claim(_run_mode, monkeypatch):
    import common.auth
    from helpers import multiseat_config

    monkeypatch.setenv("FAUNA_LIVE_CLAIM_CODE", "ABC123")
    _run_mode(_disposable())
    monkeypatch.setattr(multiseat_config, "resolve_secret", lambda url: (_SEED, "test"))

    def refuse(url, sk):
        raise _Refused("fauna.auth.not_registered")

    monkeypatch.setattr(common.auth, "mint_token_via_handshake", refuse)
    provider = nest_mode.provider_for(_disposable())
    admin = provider._admin(_BOX_URL)
    assert admin["awaits_claim"] is True
    assert admin["token"] is None
    assert admin["actor_id_hex"] == bytes(admin["signing_key"].verify_key).hex()

    monkeypatch.setattr(common.auth, "mint_token_via_handshake", lambda url, sk: "tok")
    admin = provider._admin(_BOX_URL)
    assert admin["awaits_claim"] is False
    assert admin["token"] == "tok"


# ── 6. the session fixtures and the mail leg honour it ─────────────────────


def _function_source(rel: str, name: str) -> str:
    tree = ast.parse((_ROOT / rel).read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.FunctionDef) and node.name == name:
            return ast.get_source_segment((_ROOT / rel).read_text(), node)
    raise AssertionError(f"{rel} has no function {name}")


def test_the_session_fixtures_do_not_provision_against_a_box_awaiting_its_claim():
    driver_cache = _function_source("conftest.py", "_driver_cache")
    assert "awaits_claim" in driver_cache, (
        "_driver_cache must not resolve the session test_user against a live box "
        "that awaits this run's claim — registering it needs a claimed box"
    )
    test_user = _function_source("conftest.py", "test_user")
    assert "awaits_claim" in test_user and "skip_environment" in test_user, (
        "test_user must skip as environment while the live box still awaits its claim"
    )


def test_the_mail_leg_claims_with_the_runs_code_on_a_box_awaiting_it():
    leg = _function_source("tests/test_mail_enable_live_nest.py", "test_believable_live_mail_roundtrip")
    assert "preflight_admin(" in leg and "CLAIM_CODE" in leg, (
        "the mail leg must claim with FAUNA_LIVE_CLAIM_CODE when the box awaits "
        "its claim, instead of a WS-RPC reset it cannot authenticate"
    )


# ── 7. a claim-fresh run claims under the box's own handle ─────────────────


class _Onboarding:
    def __init__(self):
        self.typed = None

    def navigate_to_status(self):
        pass

    def import_key(self, secret):
        pass

    def fill_handle(self, handle):
        self.typed = handle

    def run_handle_check(self, timeout):
        pass

    def submit_handle(self):
        pass


class _Driver:
    def is_visible(self, eid):
        return eid == "feed-view"


class _App:
    def __init__(self):
        self.onboarding = _Onboarding()
        self.driver = _Driver()


def test_reach_admin_shell_types_the_claim_handle_when_given(monkeypatch):
    """The box's provisioned handle (`live_box_door.mailbox`) is what a
    claim-fresh run types, so nothing need be exported to claim under it; with
    none given, the derivation probe is typed as before."""
    from helpers import live_admin

    for var in ("FAUNA_LIVE_HANDLE", "FAUNA_LIVE_MAIL_ADDRESS"):
        monkeypatch.delenv(var, raising=False)
    app = _App()
    live_admin.reach_admin_shell(app, nest_url=_BOX_URL, secret_hex=_SEED, claim_handle="box-admin@box.test")
    assert app.onboarding.typed == "box-admin@box.test"
    app = _App()
    live_admin.reach_admin_shell(app, nest_url=_BOX_URL, secret_hex=_SEED)
    assert app.onboarding.typed.startswith("fauna-e2e-handle-probe@")
