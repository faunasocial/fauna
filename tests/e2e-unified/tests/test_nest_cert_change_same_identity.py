"""tier_3 e2e: a nest certificate change under the SAME identity is carried
silently; under a DIFFERENT identity it warns.

``docs/features/connect-and-sign-in.md`` outcome 15;
``docs/goal/architecture/transport-trust.md`` § Cross-connection binding, rule 3
(a moved served SPKI is expected and benign: the client re-binds, and when the
freshly bound identity still matches the pin it re-pins the SPKI) and the
stale-pin heal paragraph (built 2026-11-05: the same graduation runs in the
interactive app on a bearer dial refused for a moved SPKI, and a heal refused
because another identity signed the new leaf surfaces as the identity verdict).

The in-process witnesses (``bins/fauna-nest/tests/tls_stale_pin_heal.rs``,
``tls_stale_pin_consumer_heal.rs``) drive the shared dial seam over a switchable
forwarder. This module is the end-to-end one: a real tui process signed in to a
real nest over real TLS, whose served key really changes underneath it.

**The served KEY must change, not just the certificate.** The floor cert is
re-synthesized from a persisted floor key (``floor-privkey.pem``,
``nest/tls-certificates.md`` § A), so a renewal keeps the SPKI and the app's pin
never mismatches — such a test would pass without the heal ever running. The
swap here installs a leaf on a FRESH key through the production install path
(``<tmp_dir>/acme/fullchain.pem`` + ``privkey.pem``, the pair the ACME finish
writes and ``acme::cert_watcher_task`` loads; the boot floor is write-if-absent),
and asserts the served SPKI differs before asserting anything about the app. The
leaf is issued by a throwaway harness CA that the app is never told to trust, so
the app is still on the self-signed branch: the only thing that can accept the
new key is the identity binding.

**The swap rides a nest restart on the same data dir** (``stop_nest`` → write →
``start_nest_in_place``), the Watchtower redeploy shape: it is what forces the
running app to dial afresh, which is where the pinned SPKI is checked. The app
itself is never relaunched, and the assertion that it was not is the session
teardown counter (convention 14).

**The negative control keeps the positive half honest.** The same restart, this
time with the nest's deployment seed (``nest_deployment.key``, its single
identity — reconciled into the DB at boot) replaced as well as the key: a box at
the same address proving a different identity. The same app, on the same
pipeline, must now paint ``launch_identity_changed`` and count the teardown. A
heal that accepted any new key would fail here; a pipeline wedged by the first
half could not paint it.

**Two tests, one per process the outcome names.** The first witnesses the
interactive app's own connection. The second witnesses the background sync —
tui's out-of-process ``fauna-sync-agent``, a pin CONSUMER holding a bearer and a
read-only view of the pins the app writes — on a function-scoped TLS nest of its
own (``rotatable_tls_nest``), because each test replaces its nest's identity and
the module-scoped ``self_signed_nest`` cannot be shared past that. The agent's
arm is observed from outside the agent: a file written into the bound folder
after the swap reaching the nest (``fauna.media.list``, over the harness's own
TLS) with the agent's pid unchanged, and after the identity swap the agent's OWN
log recording that it holds — its own identity refusal, or the refused bearer
mint that follows the app withdrawing its capability on the app's own verdict —
while a further file never arrives. The in-process twin, which isolates the
agent's own identity park, is ``tls_stale_pin_consumer_heal.rs``.

**android's and ios's background sync is in-process, so the second test has
an arm of its own there.** Photo backup (``PhotoBackupEngine``, on both) drives
the ``FfiSyncEngineHost`` built off the app's own ``NestClient``, so the carrier
is a photo put in the device's gallery (never the host ``tmp_path``: neither has
a user-bindable folder tree, and android's test agent refuses the folder-list
commands by design) and the trigger is the production pass the
``photo_backup_scheduled_pass_now`` poke runs (convention 14's ``run_now``):
android's ``PhotoBackupWorker`` enqueued through WorkManager, ios's
``BackgroundScheduler.runScheduledUploadPass`` — the body of the
``social.fauna.sync.upload`` task. "No restart" is the app's process — its pid
and start time, asked of the kernel (``app_process_identity`` on both drivers)
— together with the session teardown counter. The park is read at positive events: the app's own log
recording the identity refusal, the identity-changed surface, then a further
scheduled pass completing (its pass counters) with no new create in the
photo-library set.

tui leads (``testing.md`` § Default app and nest mode); the other native apps
join by adding their marker. linux runs both (its launch spawns the real
``fauna-sync-agent`` as an isolated per-launch child, as tui's does); android and ios
run both, the second through the in-process arm above (neither has an agent or
a bindable folder). macos runs both: its launch spawns the real
``fauna-sync-agent`` as a per-launch child under the relocated ``HOME`` (the
``real_sync_agent`` marker), whose own log the driver reads from
``sync_agent_state_base``. web is structurally out of
scope: the browser terminates TLS and hands WASM no certificate, so no pin
exists to move (``transport-trust.md`` § The browser's bearer socket).
"""

from __future__ import annotations

import os
import secrets
import time
import uuid
from pathlib import Path
from urllib.parse import urlparse

import pytest

from common.nest import (
    NEST_DEPLOYMENT_KEY,
    nest_id_from_data_dir,
    start_nest_in_place,
    stop_nest,
)
from helpers.app_log_section import app_text
from helpers.budgets import IDENTITY_REFRESH_S
from helpers.ephemeral_ca import EphemeralCA
from helpers.tls_spki import served_leaf, spki_sha256_hex
from helpers.waiting import session_generation, wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux]

# Element IDs (tests/e2e-unified/ui.yaml § launch_identity_changed).
IDENTITY_WARNING = "nest-identity-changed-warning"
IDENTITY_TRUST_BUTTON = "nest-identity-changed-trust-button"

#: A post composed after the swap must land: the restart, the jittered reconnect
#: backoff, the refused pinned dial, the anonymous re-bind and the retried bearer
#: dial all sit in front of it. A positive wait, so the ceiling is free on a green
#: run — the same budget ``test_nest_flip_resilience.py`` gives its in-gap write.
POST_SWAP_WRITE_S = 90.0
#: The connection indicator returns to ``Connected`` after the healed redial.
CONNECT_WAIT_S = 90.0


def _served_spki(nest) -> str:
    parsed = urlparse(nest["url"])
    return spki_sha256_hex(served_leaf(parsed.hostname, parsed.port, parsed.hostname))


def _install_fresh_key_leaf(nest) -> None:
    """Write a leaf on a FRESH key into the stopped nest's ACME dir — the
    production install path (key first, then the chain, as the watcher reads the
    pair)."""
    host = urlparse(nest["url"]).hostname
    leaf = EphemeralCA().issue(host)
    acme = Path(nest["tmp_dir"]) / "acme"
    acme.mkdir(parents=True, exist_ok=True)
    (acme / "privkey.pem").write_bytes(leaf.key_pem)
    (acme / "fullchain.pem").write_bytes(leaf.cert_pem)


def _replace_deployment_identity(nest) -> None:
    """Give the stopped nest a different single identity: a fresh 32-byte
    Ed25519 seed, which boot reconciles into the DB's ``nest_keypair``."""
    path = os.path.join(str(nest["tmp_dir"]), NEST_DEPLOYMENT_KEY)
    with open(path, "wb") as f:
        f.write(secrets.token_bytes(32))
    os.chmod(path, 0o600)


def _connection_status(app) -> str | None:
    if app.is_visible("connection-status"):
        return app.get_text("connection-status")
    return None


def _wait_connected(app, timeout: float = CONNECT_WAIT_S) -> str | None:
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = _connection_status(app)
        if last is None or last == "Connected":
            return last
        time.sleep(0.3)
    return last


def _diagnostics(app) -> str:
    """Rule 6: the app's own words on the dial, the pin and the identity."""
    text = app_text(app.driver) or ""
    lines = text.splitlines()
    wanted = ("identity", "spki", "SPKI", "pin", "graduat", "ERROR", "WARN", "panic",
              "launch", "verdict", "session", "offline", "photo_backup")
    hits = [ln for ln in lines if any(w in ln for w in wanted)]
    return (
        f"error={app.error_text()!r} status={_connection_status(app)!r} "
        f"app log ({len(hits)} matching lines): {hits[-40:]!r}"
    )


def _read_pin(app, nest_url):
    import json

    raw = app.driver.call_machine_method(
        "nest_identity_pin_for_test", json.dumps({"nest_url": nest_url})
    )
    if raw in (None, "", "null"):
        return None
    if isinstance(raw, str):
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            return raw
    return raw


def _swap(nest, *, new_identity: bool) -> None:
    stop_nest(nest, graceful=True)
    _install_fresh_key_leaf(nest)
    if new_identity:
        _replace_deployment_identity(nest)
    start_nest_in_place(nest)


@pytest.mark.feature("connect-and-sign-in")
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_a_cert_change_under_the_same_identity_is_carried_and_another_identity_warns(
    self_signed_logged_in_app, self_signed_nest
):
    app = self_signed_logged_in_app
    nest = self_signed_nest

    # ── Baseline: signed in over real TLS, connected, a write round-trips, and
    #    the app holds the nest's genuine identity as its pin.
    assert nest["url"].startswith("https://"), nest["url"]
    assert _wait_connected(app) in (None, "Connected"), _diagnostics(app)
    before = f"cert-swap-before-{uuid.uuid4().hex[:8]}"
    app.feed.create_post(before, timeout=POST_SWAP_WRITE_S)
    assert before in app.feed.first_post_text(), _diagnostics(app)

    identity = nest_id_from_data_dir(nest["tmp_dir"])
    assert identity, "the self-signed nest has no deployment identity on disk"
    pin_before = _read_pin(app, nest["url"])
    assert pin_before == identity, (
        f"the signed-in app should pin the nest's genuine identity {identity!r}, "
        f"holds {pin_before!r} — the positive half below would not be exercising "
        "a pinned dial"
    )
    assert app.is_absent(IDENTITY_WARNING), _diagnostics(app)
    spki_before = _served_spki(nest)
    generation_before = session_generation(app.driver)
    assert generation_before is not None, (
        "the app publishes no session_generation teardown counter"
    )

    # ── Positive half: same identity, new served key.
    _swap(nest, new_identity=False)
    spki_after = _served_spki(nest)
    assert spki_after != spki_before, (
        "the nest still serves the key the app pinned — the swap did not move the "
        "SPKI, so nothing below would exercise the heal"
    )
    assert nest_id_from_data_dir(nest["tmp_dir"]) == identity

    # A write composed now needs a bearer connection to the restarted nest, which
    # needs the pinned dial to have been refused for the moved SPKI and healed by
    # the identity binding. No gesture of ours touches the connection.
    after = f"cert-swap-after-{uuid.uuid4().hex[:8]}"
    try:
        app.feed.create_post(after, timeout=POST_SWAP_WRITE_S)
    except Exception as e:  # noqa: BLE001 — re-raised with the app's own words
        raise AssertionError(
            f"a post composed after the same-identity key change never landed: {e}. "
            f"{_diagnostics(app)}"
        ) from e
    assert after in app.feed.first_post_text(), (
        "a post composed after the same-identity key change did not round-trip. "
        f"{_diagnostics(app)}"
    )
    assert _wait_connected(app) in (None, "Connected"), _diagnostics(app)
    assert app.is_absent(IDENTITY_WARNING), (
        "a certificate change under the SAME nest identity must not warn. "
        f"{_diagnostics(app)}"
    )
    assert session_generation(app.driver) == generation_before, (
        "the same-identity key change tore the session down (session_generation "
        f"moved {generation_before} → {session_generation(app.driver)}); the heal "
        f"must carry the running session. {_diagnostics(app)}"
    )
    assert _read_pin(app, nest["url"]) == identity, (
        "the identity pin must be untouched by a heal — only the SPKI cache moves"
    )

    # ── Negative control: a different identity behind the same address.
    _swap(nest, new_identity=True)
    new_identity = nest_id_from_data_dir(nest["tmp_dir"])
    assert new_identity and new_identity != identity, (
        "the control's swap did not change the nest's identity"
    )
    assert _served_spki(nest) != spki_after

    # The app's own reconnect meets the new box; nothing of ours prompts it.
    wait_until(
        lambda: app.is_visible(IDENTITY_WARNING),
        IDENTITY_REFRESH_S,
        diagnose=lambda: (
            "a box proving a DIFFERENT identity at the same address must paint "
            f"the identity-changed surface. {_diagnostics(app)}"
        ),
    )
    assert app.is_visible(IDENTITY_TRUST_BUTTON), _diagnostics(app)
    assert session_generation(app.driver) > generation_before, (
        "the identity-changed escalation must count itself as a session teardown. "
        f"{_diagnostics(app)}"
    )
    assert _read_pin(app, nest["url"]) == identity, (
        "a refused heal must leave the identity pin naming the identity the user "
        "trusted"
    )


# ── The background sync's arm ────────────────────────────────────────────────

#: The set the agent seat's folder is bound to; created on the nest before the
#: bind, since an engine for a set the app-pushed key material does not name is
#: withheld (``test_sync_live_apply.py``'s ``_ensure_live_apply_set``).
AGENT_SET = "cert-change-agent-set"
#: A file written into the bound folder reaches ``fauna.media.list``: the
#: watcher, chunk, seal and upload — and after a swap, the nest restart, the
#: agent's reconnect backoff, its refused pinned dial and the re-bind in front of
#: them. A positive wait, so the ceiling is free on a green run.
AGENT_UPLOAD_S = 120.0
#: The agent's own log records the identity refusal after the control's swap.
AGENT_REFUSAL_S = 150.0

#: The shared dial seam's one record of an identity refusal
#: (``libs/fauna-anon-client/src/tls_dial.rs``, the graduate-retry arm).
IDENTITY_REFUSAL_LOG = "[identity] nest identity changed"
#: The agent's bearer mint refused for want of the capability the app pushes —
#: what the agent logs once the app, on its own identity verdict, has withdrawn
#: it (``fauna_ws_substrate::supervisor``'s connect-failed line).
CAPABILITY_WITHDRAWN_LOG = "capability not provisioned"


def _media_count(nest) -> int:
    from common.auth import _user_call

    items = _user_call(
        nest["port"],
        nest["user"]["signing_key"].encode().hex(),
        "fauna.media.list",
        {"limit": 100, "cursor_version": 2},
        nest["url"],
    ).get("items", [])
    return len(items)


def _agent_log(app) -> str:
    """The sync agent's OWN log — never the app's, which carries the same
    shared-seam lines from the app's own dial."""
    reader = getattr(app.driver, "inherited_agent_log_text", None)
    text = reader() if reader is not None else ""
    if not text:
        from drivers.base import detached_agent_log_text

        text = detached_agent_log_text(getattr(app.driver, "sync_agent_state_base", None))
    return text or ""


def _agent_holds(app) -> int:
    """How many times the agent's own log records holding off the nest."""
    text = _agent_log(app)
    return text.count(IDENTITY_REFUSAL_LOG) + text.count(CAPABILITY_WITHDRAWN_LOG)


def _agent_pid(driver) -> int | None:
    """The pid serving this launch's agent endpoint, asked of the kernel."""
    socket_path = getattr(driver, "sync_agent_socket", None)
    if socket_path:
        from helpers.sync_agent_ipc_unix import server_pid

        return server_pid(socket_path)
    pipe = getattr(driver, "sync_agent_pipe", None)
    if pipe:
        from helpers.sync_agent_ipc import pipe_server_pid

        return pipe_server_pid(pipe)
    return None


def _agent_diagnostics(app, nest) -> str:
    """Rule 6: the agent's own words, the nest's count, then the app's."""
    wanted = ("identity", "SPKI", "pinned", "re-binding", "upload", "ERROR", "WARN", "panic")
    lines = [ln for ln in _agent_log(app).splitlines() if any(w in ln for w in wanted)]
    try:
        count = _media_count(nest)
    except Exception as e:  # noqa: BLE001 — diagnostics never mask the failure
        count = f"<unreadable: {e}>"
    return (
        f"nest media count={count!r}; agent log ({len(lines)} matching lines): "
        f"{lines[-40:]!r}; {_diagnostics(app)}"
    )


def _wait_media_count(app, nest, want: int, why: str) -> None:
    wait_until(
        lambda: _media_count(nest) >= want,
        AGENT_UPLOAD_S,
        diagnose=lambda: f"{why} {_agent_diagnostics(app, nest)}",
    )


# ── android and ios: the in-process background sync's arm ───────────────────

#: The poke's pass reaches ``passes_completed``: the dispatch (WorkManager on
#: android, a spawned task on ios), the library scan, the seal and upload — and after a swap, the nest restart
#: and the healed redial in front of them. A positive wait, free on a green run.
IN_PROCESS_PASS_S = 150.0
#: The poke (``fauna_e2e_agent::PHOTO_BACKUP_SCHEDULED_PASS_NOW``).
PHOTO_BACKUP_SCHEDULED_PASS_NOW = "photo_backup_scheduled_pass_now"
#: ios answers the poke with this line when it holds no live client — after an
#: identity refusal tore the session down (``Fauna-iOS/App/FaunaApp.swift``).
IOS_NO_SESSION_POKE_LOG = "photo_backup_scheduled_pass_now: no session yet"


def _photo_creates(nest) -> int:
    """How many fixture-size creates the nest records in the photo-library set
    (``helpers.photo_backup.fixture_creates``'s reading, over the TLS nest's own
    URL). Zero while the set does not exist yet."""
    from common.auth import sync_changes_list, user_folders_list
    from helpers.photo_backup import FIXTURE_CEILING_B, PHOTO_LIBRARY_SET
    from helpers.set_names import find_set

    port, url = nest["port"], nest["url"]
    secret_key = nest["user"]["signing_key"].encode().hex()
    rows = user_folders_list(port, secret_key=secret_key, base_url=url).get("folders", [])
    if not find_set(rows, PHOTO_LIBRARY_SET):
        return 0
    changes = sync_changes_list(
        port, secret_key=secret_key, folder=PHOTO_LIBRARY_SET, base_url=url,
    ).get("changes", [])
    return sum(
        1 for c in changes
        if c.get("change_type") == "create" and (c.get("size_bytes") or 0) < FIXTURE_CEILING_B
    )


def _in_process_diagnostics(app, nest) -> str:
    """Rule 6: the pass funnel, the nest's record, then the app's own words."""
    from helpers.waiting import describe_photo_backup_funnel

    try:
        creates = _photo_creates(nest)
    except Exception as e:  # noqa: BLE001 — diagnostics never mask the failure
        creates = f"<unreadable: {e}>"
    return (
        f"photo-library creates={creates!r}; {describe_photo_backup_funnel(app.driver)}; "
        f"{_diagnostics(app)}"
    )


def _live_engine(app, nest, *, what: str) -> None:
    """Assert the app still holds the engine the arm drives, naming why when not.

    The funnel is published off the live client's own engine (ios:
    ``appState.liveClient?.photoBackup``), so its absence after it was once seen
    means the app dropped its session — on ios a launch verdict landing after the
    optimistic entry (``leaveAuthenticatedSession``) is the one path that does it
    with no test step. ``helpers.photo_backup.funnel``'s bare refusal cannot say
    which; this reads the teardown counter, the app's words and the surface it
    landed on into the failure (rule 6)."""
    from helpers.waiting import photo_backup_funnel

    if photo_backup_funnel(app.driver) is not None:
        return
    raise AssertionError(
        f"{what}: the app publishes no `photo_backup` funnel any more — it holds "
        "no live client, so the session was dropped (session_generation="
        f"{session_generation(app.driver)!r}). {_in_process_diagnostics(app, nest)}"
    )


def _in_process_pass(app, *, what: str, nest=None) -> None:
    """One production scheduled pass (android's ``PhotoBackupWorker``, ios's
    ``runScheduledUploadPass``), poked and awaited on the funnel's
    ``passes_completed`` (the two-counter barrier, convention 14)."""
    from helpers.photo_backup import await_pass_after, passes

    if nest is not None:
        _live_engine(app, nest, what=what)
    baseline = passes(app.driver)[1]
    app.driver.call_command(PHOTO_BACKUP_SCHEDULED_PASS_NOW)
    await_pass_after(app.driver, baseline, what=what, timeout=IN_PROCESS_PASS_S)


def _in_process_photo_backup_ready(app, nest, tmp_path) -> None:
    """Photos granted, backup on through the app's own toggle, and one gallery
    photo already up through a scheduled pass — so the in-process engine's bearer
    dial has been made before any swap (fixture setup, rule 8 carve-out (b))."""
    from helpers.photo_backup import (
        a_real_png, a_tag, await_quiet, enable_photo_backup, granted,
        prepare_photos_access,
    )

    generation_signed_in = session_generation(app.driver)
    prepare_photos_access(app)
    app.driver.add_photo_to_library(str(a_real_png(tmp_path, tag=a_tag())))
    enable_photo_backup(app)
    # ios: the Photos grant is proven before anything is blamed on the product
    # (a pass without it uploads nothing, exactly as a broken ingest would).
    granted(app)
    await_quiet(app.driver)
    # The Photos grant rides a cold relaunch (`prepare_photos_access`) against
    # this module's TLS nest; ios enters optimistically and a launch verdict can
    # still drop the session after the funnel was first seen — name it here, at
    # setup, rather than as a bare refusal inside the pass barrier.
    _live_engine(
        app, nest,
        what=("after the Photos-grant relaunch (session_generation when signed "
              f"in: {generation_signed_in!r})"),
    )
    _in_process_pass(app, what="the pre-swap scheduled-pass poke", nest=nest)
    assert _photo_creates(nest) >= 1, (
        "precondition: the pre-swap scheduled pass uploaded no photo into the "
        f"photo-library set over its own bearer dial. {_in_process_diagnostics(app, nest)}"
    )


def _the_in_process_background_sync_arm(app, nest, photo_dir) -> None:
    """The second test on android and ios: the app's in-process background sync carries
    a same-identity key change with no restart, and parks on another identity."""
    from helpers.photo_backup import a_real_png, a_tag

    driver = app.driver
    identity = nest_id_from_data_dir(nest["tmp_dir"])
    assert identity, "the TLS nest has no deployment identity on disk"
    assert _read_pin(app, nest["url"]) == identity, (
        "the app should pin the nest's genuine identity — the sync engine dials "
        "over the app's own client, so without the pin nothing below exercises a "
        "pinned dial"
    )
    process = driver.app_process_identity()
    assert process, "the app has no process on the device to ask the kernel about"
    generation_before = session_generation(driver)
    assert generation_before is not None, (
        "the app publishes no session_generation teardown counter"
    )
    spki_before = _served_spki(nest)
    creates_before = _photo_creates(nest)
    refusals_before = (app_text(driver) or "").count(IDENTITY_REFUSAL_LOG)

    # ── Positive half: same identity, new served key. A photo taken now reaches
    #    the nest only through the engine's bearer dial against a moved SPKI that
    #    the identity binding carried. Each attempt is one whole scheduled pass (a
    #    pass whose ingest met the nest still restarting leaves the photo pending,
    #    and the next trigger of any kind re-attempts it — `PhotoBackupEngine`).
    _swap(nest, new_identity=False)
    assert _served_spki(nest) != spki_before, (
        "the nest still serves the key the app pinned — the swap did not move the "
        "SPKI, so nothing below would exercise the heal"
    )
    driver.add_photo_to_library(str(a_real_png(photo_dir, tag=a_tag())))

    def _a_pass_carried_it() -> bool:
        _in_process_pass(app, what="the post-swap scheduled-pass poke", nest=nest)
        return _photo_creates(nest) > creates_before

    wait_until(
        _a_pass_carried_it,
        AGENT_UPLOAD_S + IN_PROCESS_PASS_S,
        diagnose=lambda: (
            "a photo taken after a same-identity key change never reached the "
            "nest — the background sync did not carry on. "
            f"{_in_process_diagnostics(app, nest)}"
        ),
    )
    creates_after = _photo_creates(nest)
    assert driver.app_process_identity() == process, (
        "the app's process — the background sync's — was restarted across the "
        "same-identity key change; the outcome promises no restart. "
        f"{_in_process_diagnostics(app, nest)}"
    )
    assert session_generation(driver) == generation_before, (
        f"the same-identity key change tore the app's session down. {_diagnostics(app)}"
    )
    assert app.is_absent(IDENTITY_WARNING), (
        f"a certificate change under the SAME identity must not warn. {_diagnostics(app)}"
    )
    assert (app_text(driver) or "").count(IDENTITY_REFUSAL_LOG) == refusals_before, (
        "the app logged an identity refusal for a box proving the SAME identity. "
        f"{_in_process_diagnostics(app, nest)}"
    )

    # ── Negative control: another identity behind the same address. Read at
    #    positive events, never after a timed quiet window (convention 14): the
    #    app's own log recording the identity refusal, the identity-changed
    #    surface, and then a further scheduled pass completing with nothing new on
    #    the nest — the engine dials over the app's client, so its park is that
    #    client's (`NestClient::identity_park`); on ios the verdict drops the
    #    client itself, so the park is read off the teardown (below).
    _swap(nest, new_identity=True)
    assert nest_id_from_data_dir(nest["tmp_dir"]) != identity, (
        "the control's swap did not change the nest's identity"
    )
    driver.add_photo_to_library(str(a_real_png(photo_dir, tag=a_tag())))
    wait_until(
        lambda: (app_text(driver) or "").count(IDENTITY_REFUSAL_LOG) > refusals_before,
        AGENT_REFUSAL_S,
        diagnose=lambda: (
            "the app's log recorded no identity refusal against a box proving a "
            f"DIFFERENT identity. {_in_process_diagnostics(app, nest)}"
        ),
    )
    wait_until(
        lambda: app.is_visible(IDENTITY_WARNING),
        IDENTITY_REFRESH_S,
        diagnose=lambda: (
            "the app never painted the identity-changed surface for the control's "
            f"box. {_diagnostics(app)}"
        ),
    )
    if driver.is_ios():
        # ios parks by tearing the session down: the identity verdict drops the
        # live client (`leaveAuthenticatedSession`, the escalation the first test
        # counts as a teardown), and the photo-backup engine and its funnel hang
        # off that client — so no further pass CAN run, which is the park. Read
        # it at positive events: the teardown counter moved, the funnel is gone,
        # and the production poke is answered by the app's own no-session line
        # (FaunaApp.swift's `photo_backup_scheduled_pass_now` arm) — then the
        # nest holds nothing new.
        assert session_generation(driver) > generation_before, (
            "the identity refusal left the ios session standing, so its "
            f"background sync was not parked by a teardown. {_in_process_diagnostics(app, nest)}"
        )
        from helpers.waiting import photo_backup_funnel

        assert photo_backup_funnel(driver) is None, (
            "the ios session was torn down but the app still publishes a "
            f"`photo_backup` funnel. {_in_process_diagnostics(app, nest)}"
        )
        noops_before = (app_text(driver) or "").count(IOS_NO_SESSION_POKE_LOG)
        driver.call_command(PHOTO_BACKUP_SCHEDULED_PASS_NOW)
        wait_until(
            lambda: (app_text(driver) or "").count(IOS_NO_SESSION_POKE_LOG) > noops_before,
            IN_PROCESS_PASS_S,
            diagnose=lambda: (
                "the scheduled-pass poke after the identity swap was not answered "
                f"by the app's no-session line. {_in_process_diagnostics(app, nest)}"
            ),
        )
    else:
        _in_process_pass(app, what="the scheduled-pass poke after the identity swap",
                         nest=nest)
    assert _photo_creates(nest) == creates_after, (
        "a photo reached a nest proving a DIFFERENT identity — the background "
        f"sync's bearer rode a dial the pin refused. {_in_process_diagnostics(app, nest)}"
    )
    assert driver.app_process_identity() == process, (
        "the app's process was restarted by the identity escalation; the park "
        f"must hold in the running process. {_in_process_diagnostics(app, nest)}"
    )


@pytest.fixture
def tls_agent_bound_app(request, app, rotatable_tls_nest, tmp_path):
    """``app`` signed in over real TLS to a nest of its own, with a local folder
    bound to a nest set and the launch's own sync agent serving it — one file
    already uploaded by that agent, so its bearer dial has been made before any
    swap (fixture setup arranging the world, rule 8 carve-out (b)). Returns
    ``(app, nest, watch_dir)``.

    On android and ios — no agent, no bindable folder — it arranges the in-process
    twin instead (:func:`_in_process_photo_backup_ready`), and ``watch_dir`` is the
    host directory the gallery photos are written in before they are pushed."""
    from common.auth import user_create_folder, user_folder_ref
    from conftest import _bind_live_location, _declare_no_web_cert_trust, _login_app_as

    nest = rotatable_tls_nest
    if app.driver.is_android() or app.driver.is_ios():
        _login_app_as(app, request, nest, nest["user"])
        _in_process_photo_backup_ready(app, nest, tmp_path)
        return app, nest, tmp_path
    secret_key = nest["user"]["signing_key"].encode().hex()
    user_create_folder(nest["port"], AGENT_SET, secret_key=secret_key, base_url=nest["url"])
    folder_id = user_folder_ref(
        nest["port"], AGENT_SET, secret_key=secret_key, base_url=nest["url"]
    )

    _declare_no_web_cert_trust(app)
    _login_app_as(app, request, nest, nest["user"])
    watch_dir = tmp_path / "agent-bound-folder"
    watch_dir.mkdir()
    _bind_live_location(app, watch_dir, AGENT_SET, folder_id)

    (watch_dir / "before-swap.txt").write_bytes(b"uploaded before the key moved\n")
    _wait_media_count(
        app, nest, 1,
        "precondition: the agent never uploaded the pre-swap file over its own bearer dial.",
    )
    return app, nest, watch_dir


@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("connect-and-sign-in")
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_background_sync_carries_a_same_identity_key_change_and_parks_on_another_identity(
    tls_agent_bound_app,
):
    app, nest, watch_dir = tls_agent_bound_app
    if app.driver.is_android() or app.driver.is_ios():
        _the_in_process_background_sync_arm(app, nest, watch_dir)
        return

    identity = nest_id_from_data_dir(nest["tmp_dir"])
    assert identity, "the TLS nest has no deployment identity on disk"
    assert _read_pin(app, nest["url"]) == identity, (
        "the app should pin the nest's genuine identity — the agent reads that pin, "
        "so without it nothing below exercises a pinned dial"
    )
    agent_pid = _agent_pid(app.driver)
    assert agent_pid, "this launch publishes no agent endpoint to ask the kernel about"
    generation_before = session_generation(app.driver)
    spki_before = _served_spki(nest)
    refusals_before = _agent_log(app).count(IDENTITY_REFUSAL_LOG)

    # ── Positive half: same identity, new served key. The agent's socket drops
    #    with the restart; the file written now reaches the nest only through a
    #    bearer dial against a moved SPKI that the identity binding carried.
    #    Which process re-pins the SPKI first — the app on its own redial or the
    #    agent on its own — is not this test's to order: the outcome holds in
    #    either, and the agent's log says which.
    _swap(nest, new_identity=False)
    assert _served_spki(nest) != spki_before, (
        "the nest still serves the key the app pinned — the swap did not move the "
        "SPKI, so nothing below would exercise the heal"
    )
    (watch_dir / "after-swap.txt").write_bytes(b"uploaded after the key moved\n")
    _wait_media_count(
        app, nest, 2,
        "a file written into the bound folder after a same-identity key change "
        "never reached the nest — the background sync did not carry on.",
    )
    assert _agent_pid(app.driver) == agent_pid, (
        "the sync agent was restarted across the same-identity key change; the "
        f"outcome promises no restart. {_agent_diagnostics(app, nest)}"
    )
    assert session_generation(app.driver) == generation_before, (
        f"the same-identity key change tore the app's session down. {_diagnostics(app)}"
    )
    assert app.is_absent(IDENTITY_WARNING), (
        f"a certificate change under the SAME identity must not warn. {_diagnostics(app)}"
    )
    assert _agent_log(app).count(IDENTITY_REFUSAL_LOG) == refusals_before, (
        "the agent logged an identity refusal for a box proving the SAME identity. "
        f"{_agent_diagnostics(app, nest)}"
    )

    # ── Negative control: another identity behind the same address. The agent
    #    must hold — and a file written now must not reach the nest. It holds
    #    one of two ways, and its own log records which: its own dial meets the
    #    new leaf and the identity verdict parks it (`NestClient::identity_park`:
    #    nothing is dialled after it), or — the order measured on tui
    #    2026-11-07, the app being up and redialling too — the app reaches the
    #    verdict first, withdraws the agent's capability, and the agent's next
    #    bearer mint is refused before any dial. Read at two positive events,
    #    the agent's hold and the app's own warning, never after a timed quiet
    #    window (convention 14).
    holds_before = _agent_holds(app)
    _swap(nest, new_identity=True)
    assert nest_id_from_data_dir(nest["tmp_dir"]) != identity, (
        "the control's swap did not change the nest's identity"
    )
    (watch_dir / "after-identity-swap.txt").write_bytes(b"must stay on this device\n")
    wait_until(
        lambda: _agent_holds(app) > holds_before,
        AGENT_REFUSAL_S,
        diagnose=lambda: (
            "the sync agent recorded no hold — neither its own identity refusal nor "
            "a refused bearer mint — against a box proving a DIFFERENT identity. "
            f"{_agent_diagnostics(app, nest)}"
        ),
    )
    assert _media_count(nest) == 2, (
        "a file reached a nest proving a DIFFERENT identity — the agent's bearer "
        f"rode a dial the pin refused. {_agent_diagnostics(app, nest)}"
    )
    wait_until(
        lambda: app.is_visible(IDENTITY_WARNING),
        IDENTITY_REFRESH_S,
        diagnose=lambda: (
            "the app never painted the identity-changed surface for the control's "
            f"box. {_diagnostics(app)}"
        ),
    )
    assert _media_count(nest) == 2, (
        "a file reached a nest proving a DIFFERENT identity after the agent parked. "
        f"{_agent_diagnostics(app, nest)}"
    )
