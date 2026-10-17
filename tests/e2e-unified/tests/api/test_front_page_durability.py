"""The admin's chosen front page outlives a restart and an account taken back
(`docs/goal/behavior/web-content-hosting.md` § Admin apex hosting).

`docs/features/admin-front-page.md` outcomes 8 and 9, on a real `fauna-nest`
binary:

* **8** — the designation is a nest row (`web_apex_actor`) the boot seed reads,
  so after a real stop and start `GET /` still serves the chosen member's site.
  `test_web_revoke_durability.py` leans on this without asserting it.
* **9** — when that member takes a lost account back, the succession re-points
  the designation at the successor key (`db/successions.rs`), so `GET /` keeps
  serving the same site and the admin reads the successor as the front page.

The nest is dedicated to this module: a restart stops the process and a
succession revokes sessions, and neither is ours to do to the session-shared
nest. Process safety: the restart goes through `common.nest.restart_nest`
(the fixture's own process handle only); no `pkill`/`killall`.
"""

from __future__ import annotations

import secrets

import pytest
from nacl.signing import SigningKey

from common.auth import create_actor_and_register
from helpers.succession import register_recovery_kit, succeed_identity
from tests.api.test_web_paywall_folder import _get
from tests.api.test_web_revoke_durability import (
    _admin_client,
    _clear_apex,
    _publish,
    _restart,
    _serve_this_actors_site,
)

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def front_page_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "front-page-nest")
    try:
        yield nest
    finally:
        cleanup()


def _assert_front_page_serves(nest, marker: str, when: str) -> None:
    status, body, _headers = _get(nest["url"], "/")
    assert status == 200 and marker in body, (
        f"{when}: the front page must serve the chosen member's site ({marker!r}), got {status}: {body}"
    )


def _front_page_actor(nest) -> bytes | None:
    with _admin_client(nest) as admin:
        got = admin.call("fauna.web.get_apex_actor", {}).get("actor_id")
    return bytes(got) if got is not None else None


@pytest.mark.feature("admin-front-page")
def test_the_chosen_front_page_keeps_serving_after_a_nest_restart(front_page_nest):
    nest = front_page_nest
    member = create_actor_and_register(nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    marker = f"Front Page {secrets.token_hex(3)}"
    _publish(nest, member, marker, "front-restart")
    _serve_this_actors_site(nest, member)
    try:
        _assert_front_page_serves(nest, marker, "before the restart")

        _restart(nest)

        _assert_front_page_serves(nest, marker, "after the restart")
        assert _front_page_actor(nest) == bytes(member["actor_id_bytes"]), (
            "the admin must still read the same member as the front page after a restart"
        )
    finally:
        _clear_apex(nest)


@pytest.mark.feature("admin-front-page")
def test_the_front_page_follows_its_member_to_a_successor_key(front_page_nest):
    nest = front_page_nest
    member = create_actor_and_register(nest["port"], admin_signing_key=nest["admin"]["signing_key"])
    marker = f"Front Page {secrets.token_hex(3)}"
    _publish(nest, member, marker, "front-succession")
    _serve_this_actors_site(nest, member)
    try:
        _assert_front_page_serves(nest, marker, "before the account is taken back")

        seed_hex = bytes(member["signing_key"]).hex()
        kit_secret = register_recovery_kit(
            nest["url"], actor_id_hex=member["actor_id_hex"], identity_seed_hex=seed_hex
        )
        successor_seed = secrets.token_bytes(32)
        successor_hex = succeed_identity(
            nest["url"],
            old_actor_id_hex=member["actor_id_hex"],
            recovery_secret_hex=kit_secret,
            successor_seed_hex=successor_seed.hex(),
            old_seed_hex=seed_hex,
        )
        assert successor_hex == bytes(SigningKey(successor_seed).verify_key).hex(), successor_hex

        _assert_front_page_serves(nest, marker, "after the account passed to its successor")
        assert _front_page_actor(nest) == bytes.fromhex(successor_hex), (
            "the admin must read the successor key as the front page"
        )
    finally:
        _clear_apex(nest)
