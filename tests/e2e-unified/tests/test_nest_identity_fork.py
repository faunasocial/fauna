"""tier_3 e2e: rotation-chain FORK evidence — the hard warning naming both heads,
with no way to trust either.

``docs/goal/architecture/nest/box-recovery.md`` § Deployment-seed rotation →
*Client acceptance — re-pin on a verified chain*: "a second distinct head at an
accepted seq is fork evidence — hard warning naming both heads, no silent path and
no re-trust button on that surface". ``tests/e2e-unified/ui.yaml`` page
``launch_identity_changed``, its fork variant: ``nest-identity-fork-warning`` +
``launch-fallthrough-button`` only.

**Planting two competing successors.** The box rotates once for real (admin RPC,
in-process adoption — the same ceremony ``test_nest_rotation_repin.py`` drives), so
its chain names successor ``S1`` at seq 1. The client's installed pin store is then
told — through the shared E2E-bridge name
``set_nest_identity_pin_rotation_accepted_for_test`` — that it chain-accepted a
DIFFERENT head at that same seq 1. That is exactly the client-side state a cloned
box rotating on its own would leave behind: two distinct successors at one accepted
position. The relaunch meets ``S1`` over the live binding, fetches the chain over
the same connection, finds a seq-1 hop naming a head other than the accepted one
(``evaluate_rotation_bridge``'s fork clause B), and the shared launch machine
reports ``LaunchSnapshot::identity_fork``. Everything after the seed is production
code over a real nest binary, real TLS and a real app relaunch; the fork verdict
itself (both clauses) is pinned at tier_1 in ``fauna_client_core::nest_trust``.

Client arm: **tui leads** (the lead app). The other six apps join by extending
``_SUPPORTED_APPS`` as their fork rendering lands (linux and the apple legs ride
the same launch harness; android renders the fork variant since 2026-10-12 but its
arm joins ``_SUPPORTED_APPS`` only once run green on the host emulator; web cannot reach the native binding arm in e2e — its
origin is never TLS, see ``test_nest_rotation_repin.py``).

Latency-independent (convention 14): the one wait beyond the shared launch polls is
the box's in-process adoption — the sanctioned named-budget deadline poll, copied
from ``test_nest_rotation_repin.py``.
"""

import contextlib
import json
import secrets
import time

import pytest

from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder, get_available_apps

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui]

_SUPPORTED_APPS = ("tui", "linux")

# Element IDs (tests/e2e-unified/ui.yaml § launch_identity_changed).
FORK_WARNING = "nest-identity-fork-warning"
IDENTITY_WARNING = "nest-identity-changed-warning"
IDENTITY_TRUST_BUTTON = "nest-identity-changed-trust-button"
LAUNCH_FALLTHROUGH = "launch-fallthrough-button"
LAUNCH_RETRY = "launch-retry-button"

_ROTATE_KIND = "fauna.admin.deployment_seed.rotate"

# Same named budget as test_nest_rotation_repin.py: the in-process generation
# restart's ceiling; a green run pays only the real teardown.
_ADOPTION_BUDGET_S = 60.0


def _fork_clients():
    available = get_available_apps()
    return [c for c in _SUPPORTED_APPS if c in available]


@pytest.fixture(params=_fork_clients())
def fork_client(request):
    return request.param


def _nest_id_hex(nest_url):
    """The identity the box serves right now (hex), over an anonymous socket."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_url) as anon:
        info = anon.call("fauna.nest.info", {})
    return info["nest_id"]


def _poll_until_serves(nest_url, expected_hex, tag):
    """Deadline-poll ``fauna.nest.info`` until the box serves ``expected_hex``,
    tolerating the re-enter window's refused connections."""
    deadline = time.monotonic() + _ADOPTION_BUDGET_S
    last = None
    while time.monotonic() < deadline:
        try:
            served = _nest_id_hex(nest_url)
        except Exception as e:  # noqa: BLE001 — the window's error shape varies
            last = e
        else:
            if served == expected_hex:
                return
            last = f"still serving {served}"
        time.sleep(0.25)
    raise AssertionError(
        f"{tag}: box did not serve the successor within {_ADOPTION_BUDGET_S}s "
        f"(last: {last!r})"
    )


def _bridge_read(driver, name, nest_url):
    """A machine-free bridge reader's answer for ``nest_url``, decoded."""
    raw = driver.call_machine_method(name, json.dumps({"nest_url": nest_url}))
    if raw in (None, "", "null"):
        return None
    if not isinstance(raw, str):
        return raw
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


@pytest.mark.feature("connect-and-sign-in")
def test_competing_successors_warn_naming_both_heads_with_no_trust(
    request, fork_client, rotatable_tls_nest, tmp_path
):
    """Rotate → plant a competing seq-1 head → relaunch → the fork warning names
    both heads, offers no trust button, and the pin does not move."""
    nest = rotatable_tls_nest
    app_path = request.getfixturevalue(f"{fork_client}_app_path")
    harness = make_launch_harness(
        fork_client, tmp_path=tmp_path, app_path=app_path, seed_trust=_trust_seeder(request)
    )
    try:
        driver = harness.launch(
            secret_hex=bytes(nest["user"]["signing_key"]).hex(),
            node_url=nest["url"],
            trust=nest,
        )
        # First contact TOFU-pins the genuine identity; wait for the app so the
        # async graduation has settled before the store is touched.
        reached_authenticated_app(driver, timeout=90)
        before_hex = _nest_id_hex(nest["url"])

        if not driver.preserve_state_across_relaunch():
            pytest.skip(
                f"{fork_client} driver cannot pin its client store across a "
                "relaunch, so a planted chain-accepted pin cannot survive to be checked"
            )

        # The box's own successor: a real rotation, seq 1.
        from clients.ws_rpc_admin_client import WsRpcAdminClient

        admin = nest["admin"]
        with WsRpcAdminClient(
            nest["url"],
            actor_id=bytes(admin["signing_key"].verify_key),
            signing_key=bytes(admin["signing_key"]),
        ) as ws:
            reply = ws.call(_ROTATE_KIND, {"new_seed": secrets.token_bytes(32).hex()})
        served_hex = bytes(reply["nest_actor_id"]).hex()
        assert served_hex != before_hex, "the box must serve a new identity"
        assert reply.get("seq") == 1, f"the first rotation is seq 1: {reply!r}"
        _poll_until_serves(nest["url"], served_hex, "post-rotation adoption")

        # The competing successor: this client "accepted" a different head at
        # the same seq 1. Seeded last, so it wins over any background re-pin
        # the live session ran against the rotation above.
        competing_hex = secrets.token_bytes(32).hex()
        driver.call_machine_method(
            "set_nest_identity_pin_rotation_accepted_for_test",
            json.dumps({"nest_url": nest["url"], "actor_id": competing_hex, "seq": 1}),
        )
        assert _bridge_read(driver, "nest_identity_pin_for_test", nest["url"]) == competing_hex
        assert _bridge_read(driver, "nest_identity_pin_rotation_seq_for_test", nest["url"]) == 1, (
            "the planted pin must carry the chain-accepted seq — without it neither "
            "fork clause can fire and the relaunch shows the ordinary warning"
        )

        harness.relaunch()
        try:
            driver.wait_for(FORK_WARNING, timeout=60)
        except Exception as e:  # noqa: BLE001 — diagnose the failure shape
            with contextlib.suppress(Exception):
                if driver.is_visible(IDENTITY_WARNING):
                    raise AssertionError(
                        "the relaunch showed the ORDINARY identity-changed warning "
                        "(with its re-trust) on fork evidence — the app does not read "
                        "LaunchSnapshot.identity_fork (box-recovery.md § Client acceptance)"
                    ) from e
            raise

        warning = driver.get_text(FORK_WARNING)
        assert competing_hex in warning and served_hex in warning, (
            f"the fork warning must name both heads ({competing_hex}, {served_hex}): {warning!r}"
        )
        assert driver.is_absent(IDENTITY_TRUST_BUTTON), (
            "fork evidence offers no way to trust either head"
        )
        assert driver.is_absent(IDENTITY_WARNING), (
            "the fork warning is shown INSTEAD of the ordinary one"
        )
        assert driver.is_absent(LAUNCH_RETRY), "a retry cannot change a fork verdict"
        assert driver.is_visible(LAUNCH_FALLTHROUGH), (
            "the fallthrough is the one way out of the fork surface"
        )
        # No silent path: the pin still names the accepted head at its seq.
        assert _bridge_read(driver, "nest_identity_pin_for_test", nest["url"]) == competing_hex
        assert _bridge_read(driver, "nest_identity_pin_rotation_seq_for_test", nest["url"]) == 1
    finally:
        with contextlib.suppress(Exception):
            harness.teardown()
