"""tier_3 e2e: a hosted identity the app knows about stays audited when the nest
stops naming it, and the nest's silence never takes a standing alarm down.

Goal doc: ``docs/goal/behavior/atproto-identity-custody.md`` § The audit floor
and the departed-DID alarm — the audit set is the nest's answer UNIONED with a
floor the client froze itself (``nest_named_dids``: every did:plc the nest ever
named, frozen at the moment it named it), and a departed DID's standing alarm
clears only on a client-side reason, "never on the nest's silence"; "an
unreadable directory is not a resolution".

**Why this is the property worth a full-stack witness.** Every input deciding
whether to audit used to come from one nest-computed reply, so the party the
detector accuses could switch it off: mint under a hostile key, then answer
``identity: null`` — and the settings page's departed-DID clear would even take
the standing banner down for it. Both halves are pinned by Rust unit tests; what
only a real run shows is the composition the finding was about — the floor
written by a real client from a real nest's answer, surviving that nest going
quiet, in the banner the user reads.

**The nest's silence is a ``test-hooks`` seam**
(``POST /api/v1/test/atproto/withhold-identity``): the status reply stops naming
the identity while its row, the DID, the bridge and the public log are all
untouched. It is deliberately NOT a retirement — a retirement leaves an
attributable tombstone, which is the rule's silent arm, a different case from
the one under test (``atproto_identity_test_hook``'s module doc).

The journey (one identity, minted through the app's own AT Protocol page by a real
bridge into ``FakePlcDirectory``, exactly as
``test_alert_sweep_directory_feeders_e2e.py`` does):

0. a sweep while the nest still names the identity freezes it into the floor;
1. a seizure in the public log raises the custody alarm;
2. the nest stops naming the identity — the next sweep, AND the settings page's
   convergence (the one path that clears departed alarms), leave it standing;
3. the directory goes unreadable — the next sweep leaves it standing;
4. the log heals while the nest is still silent — the floor's own audit passes
   and the alarm comes down (proof the audit ran with no help from the nest);
5. a fresh seizure while the nest is STILL silent is raised again.

Latency discipline (convention 14): every sweep is causally anchored on the
app's own pass counter (``await_sweep_pass_after``), and the settings page's
convergence on the agent's awaited nav-edge refresh — never a settle-sleep.

tier_3: real ``fauna-nest`` (``test-hooks``) and the e2e-flavored atproto bridge.
"""
from __future__ import annotations

import json
import subprocess
import urllib.request

import pytest

from actions import ActionLayer
from helpers.atproto_fakes import FakePlcDirectory
from helpers.budgets import ALERT_SWEEP_PASS_S, APP_RELAUNCH_S
from helpers.directory_launch import launch_app_with_directory
from helpers.waiting import alert_sweep_passes, await_sweep_pass_after
from i18n.strings import S
from tests.test_alert_sweep_directory_feeders_e2e import (
    EVIL_SENIOR,
    HANDLE_DOMAIN,
    SWEEP_APPS,
    _alert_text,
    _apps,
    _mint_hosted_identity,
    _poll_for_fragment,
    _reestablish_session,
)

# Every app the directory-feeder suite sweeps: the journey rides that suite's
# launcher and mint path whole, so its app set is this one's.
pytestmark = [
    pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui, pytest.mark.web,
    pytest.mark.android, pytest.mark.windows, pytest.mark.macos, pytest.mark.ios,
]

# The custody alarm's own words after the name slot, from the shared string every
# app renders — identifies feeder #1's alarm whatever name it carries (the nest's
# handle while the nest names the identity, the DID itself once it does not).
_CUSTODY = S.critical_alerts.atproto_custody_mismatch(handle="\0").split("\0")[1][:48]




def _withhold_identity(nest: dict, actor_id_hex: str, withhold: bool) -> None:
    """Make the nest stop (or resume) naming the actor's hosted identity."""
    body = json.dumps({"actor_id": actor_id_hex, "withhold": withhold}).encode()
    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/atproto/withhold-identity",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=10.0) as resp:
        assert resp.status == 200, f"withhold-identity returned {resp.status}"


@pytest.mark.parametrize("floor_app", _apps(*SWEEP_APPS))
@pytest.mark.feature("critical-alerts")
def test_the_audit_floor_outlives_the_nests_silence(
    floor_app, request, nest_binary, atproto_bridge_e2e_binary, tmp_path_factory
):
    from common.auth import open_registration, register_handled_actor
    from conftest import _make_nest
    from drivers.port_util import untrack_process

    directory = FakePlcDirectory()
    nest, nest_cleanup = _make_nest(
        nest_binary, tmp_path_factory, "audit-floor-nest", claim_domain=HANDLE_DOMAIN
    )
    open_registration(nest)
    proc = None
    driver = None
    spa_proxy_server = None
    try:
        alice = register_handled_actor(
            nest["port"], handle="alice", domain=HANDLE_DOMAIN, base_url=nest["url"]
        )
        alice_hex = alice["actor_id_bytes"].hex()
        driver, spa_proxy_server = launch_app_with_directory(floor_app, request, nest, directory)
        session = {
            "authenticated": True,
            "node_url": nest["url"],
            "secret_hex": alice["signing_key"].encode().hex(),
            "handle": f"alice@{HANDLE_DOMAIN}",
            "actor_id": alice_hex,
            "device_id": "audit-floor-e2e",
        }
        driver.set_state({"session": session, "nav": {"stack": [{"view": "feed"}]}})
        app = ActionLayer(driver)
        proc, _tail = _mint_hosted_identity(
            app, request, directory, nest, tmp_path_factory, method="plc"
        )
        did = directory.snapshot()[0][0]

        def sweep(what: str) -> str:
            """Re-run the post-auth hook and return the banner once a pass that
            began after this call has completed."""
            started, _ = alert_sweep_passes(driver) or (0, 0)
            _reestablish_session(app, session)
            await_sweep_pass_after(driver, started, budget_s=ALERT_SWEEP_PASS_S, what=what)
            return _alert_text(app)

        # ── 0. The nest names the identity; a sweep freezes it into the floor.
        text = sweep("the floor-freezing pass")
        assert _CUSTODY not in text, (
            f"precondition: an honest log raises no custody alarm; banner reads {text!r}"
        )

        # ── 1. A seizure in the public log raises the alarm.
        directory.tamper_senior_key(EVIL_SENIOR)
        sweep("the seizure")
        text = _poll_for_fragment(app, _CUSTODY, want_present=True)
        assert _CUSTODY in text, (
            "precondition: a senior key this client does not hold must raise the "
            f"custody alarm; banner reads {text!r}; bridge log:\n{_tail()}"
        )

        # ── 2. The nest stops naming the identity. Its silence must not take the
        #       alarm down — neither on the next sweep…
        _withhold_identity(nest, alice_hex, True)
        text = sweep("the first silent pass")
        assert _CUSTODY in text, (
            "the nest's silence cleared a standing custody alarm on the sweep — the "
            "floor must keep auditing a DID the client froze, and silence is not a "
            f"resolution; banner reads {text!r}"
        )
        #       …nor through the settings page, the one path that clears alarms for
        #       a DEPARTED identity. The agent's nav awaits the page's refresh, so
        #       once the identity summary is gone that convergence has finished.
        bs = app.atproto_settings
        bs.navigate()
        assert bs.is_page_visible(timeout=APP_RELAUNCH_S), (
            f"atproto page unreachable: {app.error_text()!r}"
        )
        assert not bs.is_hosted_handle_visible(), (
            "the settings page still shows the hosted identity after the nest stopped "
            "naming it — its convergence never saw the silence, so what follows would "
            "prove nothing"
        )
        text = _alert_text(app)
        assert _CUSTODY in text, (
            "the settings page's departed-DID clear took the alarm down on the nest's "
            "silence — it may clear only on a client-side reason, and this DID is in "
            f"the client's own floor; banner reads {text!r}"
        )
        driver.navigate_to("feed")

        # ── 3. The directory goes unreadable: not a resolution either.
        reads_before = len(directory.audit_reads)
        directory.set_unreadable(True)
        try:
            text = sweep("the unreadable pass")
        finally:
            directory.set_unreadable(False)
        assert len(directory.audit_reads) > reads_before, (
            "the sweep never asked the directory while it was unreadable, so the "
            "survival below would prove nothing about an unreadable read"
        )
        assert _CUSTODY in text, (
            f"an unreadable directory cleared a standing custody alarm; banner reads {text!r}"
        )

        # ── 4. Still audited with no help from the nest: the log heals, the
        #       floor's own audit passes, and the alarm comes down.
        directory.tamper_senior_key(None)
        sweep("the healed pass")
        text = _poll_for_fragment(app, _CUSTODY, want_present=False)
        assert _CUSTODY not in text, (
            "a healed public log must clear the alarm through the floor's audit even "
            f"while the nest names nothing; banner reads {text!r}"
        )

        # ── 5. …and a fresh seizure while the nest is STILL silent is caught.
        directory.tamper_senior_key(EVIL_SENIOR)
        sweep("the silent seizure")
        text = _poll_for_fragment(app, _CUSTODY, want_present=True)
        assert _CUSTODY in text, (
            "a seizure of an identity the nest no longer names went unnoticed — the "
            f"floor must keep it audited; banner reads {text!r}"
        )
        assert did in text, (
            "with the nest naming nothing, the alarm must call the identity by the one "
            f"truthful name the client holds, its DID {did!r}; banner reads {text!r}"
        )
    finally:
        directory.tamper_senior_key(None)
        if driver is not None:
            try:
                driver.teardown()
            except Exception:
                pass
        if spa_proxy_server is not None:
            spa_proxy_server.shutdown()
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
            untrack_process(proc)
        nest_cleanup()
        directory.close()
