"""tier_3 e2e: the manual DNS-01 certificate issuance on the admin `admin-dns`
page, opened from the app against a real ACME CA — `admin-dns-and-certificates`
outcomes 53 (the challenge record to paste, with complete and cancel), 54 (a
half-done manual issuance is still waiting with the same record after a
restart or on another device, and a failed attempt keeps it for a retry), 59
(complete waits for the domain's name servers to serve the record)
and 60 (the issuance record is never listed as a standing record).
`tls-certificates.md` § B tier 3 and § Surviving an interrupted
manual issuance own the mechanism.

**The CA.** A manual issuance opens its ACME order FROM THE CLIENT, so a witness
needs a CA the app can reach and trust — and the CA is a constant no human
chooses (`tls-certificates.md` § ACME settings). The `acme_pebble` marker runs
the harness's pebble + fake name server stack (`helpers/acme_pebble.py`) and
hands every app launch the compile-gated e2e ACME directory seed
(`e2e-automation-surface-gating.md` § The ACME directory seed), which the shared
native admin-dns machine builder installs. Nothing about the order is faked: the
record on the paste card is the challenge pebble issued.

**Which apps.** tui, linux and android: all three build the admin-dns machine
through the shared native builder, so the same seed reaches them with no per-app
glue; android's runs wait on the first venue run (convention 7's environment
skip). Web is the declared absence (its order runs in
wasm); the remaining native apps trickle down the same way.
"""

import re
import secrets

import pytest

from helpers import budgets
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    # Starts pebble + the fake name server for the session and seeds
    # FAUNA_E2E_ACME_* into every app launch.
    pytest.mark.acme_pebble,
]

_CHALLENGE_PREFIX = "_acme-challenge."


@pytest.fixture(autouse=True)
def _android_cannot_reach_the_acme_stack(request):
    """Android is a declared environment skip (convention 7) — the reason is
    ``helpers.acme_pebble.skip_android_until_it_reaches_the_acme_stack``'s."""
    from helpers.acme_pebble import skip_android_until_it_reaches_the_acme_stack

    skip_android_until_it_reaches_the_acme_stack(request)


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane (mirrors
    ``test_admin_dns_cert_issue.py::_seed_local_domain``). A fresh domain has no
    covering provider credential and no delegation, so its issuance is manual."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        client.call(
            "fauna.bridges.add_local_domain",
            {"domain": domain, "mta_sts_cert_mode": "expand_primary"},
        )


def _error(admin_app) -> str:
    return admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"


def _domain_index(admin_app, domain: str) -> int:
    names = wait_until(
        lambda: (n := admin_app.admin.dns_domain_names()) and domain in n and n,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{domain!r} never listed among {admin_app.admin.dns_domain_names()!r} "
            f"(error: {_error(admin_app)})"
        ),
    )
    return names.index(domain)


def _challenge_rows(admin_app, idx: int, domain: str) -> list[dict[str, str]]:
    want = f"{_CHALLENGE_PREFIX}{domain}"
    return [
        r for r in admin_app.admin.dns_records(idx)
        if r["name"].rstrip(".").lower() == want and r["value"]
    ]


def _begin_manual_issue(admin_app, domain: str) -> tuple[int, dict[str, str]]:
    """Press the issue button on a manual domain and read the paste card's
    challenge row once the CA's order is open."""
    admin_app.admin.navigate_dns()
    idx = _domain_index(admin_app, domain)
    admin_app.admin.issue_cert(idx)
    rows = wait_until(
        lambda: admin_app.admin.cert_pending_paste_present(idx)
        and _challenge_rows(admin_app, idx, domain),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"no paste card with a {_CHALLENGE_PREFIX}{domain} row after issue; "
            f"records={admin_app.admin.dns_records(idx)!r} (error: {_error(admin_app)})"
        ),
    )
    assert len(rows) == 1, f"one challenge for a single-name order, got {rows!r}"
    return idx, rows[0]


def _cancel(admin_app, idx: int, domain: str) -> None:
    admin_app.admin.cancel_manual_cert(idx)
    wait_until(
        lambda: not admin_app.admin.cert_pending_paste_present(idx)
        and not _challenge_rows(admin_app, idx, domain),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"cancel left the paste card up; records={admin_app.admin.dns_records(idx)!r} "
            f"(error: {_error(admin_app)})"
        ),
    )


def _restart_and_read_paste_card(request, admin_app, nest_instance, domain):
    """Relaunch the admin's app — the same installed app coming back on the same
    machine, so the client-local store is pinned across the relaunch — and read
    the paste card the relaunched app shows for ``domain``. The relaunched
    machine holds no live order, so whatever the card shows came from the
    persisted breadcrumb. Returns ``(idx, challenge_rows)``."""
    from conftest import _login_admin_as

    assert admin_app.driver.preserve_state_across_relaunch(), (
        "this journey needs the driver to pin its client-local store across the relaunch"
    )
    assert admin_app.driver.recover(), "the driver could not relaunch the app"
    _login_admin_as(
        admin_app, request, nest_instance, spa_url_fixture="spa_url", fixture_name="admin_app"
    )

    admin_app.admin.navigate_dns()
    idx = _domain_index(admin_app, domain)
    rows = wait_until(
        lambda: admin_app.admin.cert_pending_paste_present(idx)
        and _challenge_rows(admin_app, idx, domain),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the relaunched app shows no pending paste card for {domain}; "
            f"records={admin_app.admin.dns_records(idx)!r} (error: {_error(admin_app)})"
        ),
    )
    return idx, rows


@pytest.mark.feature("admin-dns-and-certificates")
def test_manual_issue_shows_the_challenge_record_to_paste_and_cancel_clears_it(
    admin_app, nest_instance
):
    """Outcome 53: with no provider credential, issuing shows the challenge
    record — an `admin-dns-record` row inside the domain's own section — with
    complete and cancel; cancel clears it."""
    domain = f"manualissue-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, domain)

    idx, row = _begin_manual_issue(admin_app, domain)
    assert row["type"].upper() == "TXT", row
    scope = f"admin-dns-domain[{idx}]"
    assert admin_app.driver.count("admin-dns-cert-cancel-button", scope=scope) > 0, (
        "the paste card carries a cancel button beside complete"
    )
    # The issue button is spent while this domain's order is pending.
    assert admin_app.driver.is_disabled("admin-dns-cert-issue-button", scope=scope), (
        "issue stays disabled while the paste card is up"
    )

    _cancel(admin_app, idx, domain)


@pytest.mark.feature("admin-dns-and-certificates")
def test_manual_issue_survives_an_app_restart_with_the_same_record(
    request, admin_app, nest_instance
):
    """Outcome 54, the restart clause: a half-done manual issuance is still
    waiting with the same record after the app restarts — the breadcrumb the begin step persisted is
    projected back onto the paste card by a machine that holds no live order
    (`tls-certificates.md` § Surviving an interrupted manual issuance)."""
    domain = f"manualresume-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, domain)
    _, before = _begin_manual_issue(admin_app, domain)

    idx, rows = _restart_and_read_paste_card(request, admin_app, nest_instance, domain)
    assert [(r["name"], r["value"]) for r in rows] == [(before["name"], before["value"])], (
        f"the restored paste card must show the record the admin was pasting: "
        f"before={before!r} after={rows!r}"
    )

    _cancel(admin_app, idx, domain)


# A second device of the admin: the same signing key, a device id distinct from
# the first seat's `_E2E_LOGIN_DEVICE_ID`.
_SECOND_SEAT_DEVICE_ID = "fedcba9876543210" * 4


@pytest.mark.feature("admin-dns-and-certificates")
def test_manual_issue_is_waiting_with_the_same_record_on_another_device(
    request, admin_app, nest_instance
):
    """Outcome 54, the other-device clause: a manual issuance begun on one seat
    is waiting with the same record on a SECOND seat of the same admin — a fresh
    launch with an empty client state root and a distinct device id, so it holds
    no live order and nothing local; the card it shows can come only from the
    breadcrumb the first seat persisted to the account's synced DNS record
    (`tls-certificates.md` § Surviving an interrupted manual issuance — "the
    paste card re-surfaces"). The second seat is launched the way every extra
    seat is (`conftest.py::_alice_extra_seat`): the standard launch config,
    which carries the ACME and trust seeds, then a state-protocol login."""
    from actions import ActionLayer
    from conftest import _build_app_config
    from drivers import create_driver
    from helpers.app_surface import app_name
    from helpers.waiting import account_pump_role, await_account_runtime_assembled

    domain = f"manualseat-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, domain)
    first_idx, before = _begin_manual_issue(admin_app, domain)

    # The second seat is the app under test, so the witness is of that app.
    app = app_name(admin_app.driver)
    driver_b = create_driver(app)
    driver_b.launch(_build_app_config(app, nest_instance, request))
    try:
        driver_b.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": nest_instance["admin"]["signing_key"].encode().hex(),
                "device_id": _SECOND_SEAT_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "admin"}]},
        })
        driver_b.wait_for("admin-dashboard-heading", timeout=30.0)
        seat_b = ActionLayer(driver_b)
        # The synced record is read through the account runtime, which a task
        # off the login path assembles with no other completion signal.
        if account_pump_role(driver_b) is not None:
            await_account_runtime_assembled(driver_b)

        def _card_on_b():
            # Re-enter the page on each poll: its first build may predate the
            # account runtime's first pull.
            driver_b.set_state({"nav": {"stack": [{"view": "admin"}]}})
            seat_b.admin.navigate_dns()
            names = seat_b.admin.dns_domain_names() or []
            if domain not in names:
                return None
            idx = names.index(domain)
            return seat_b.admin.cert_pending_paste_present(idx) and _challenge_rows(
                seat_b, idx, domain
            )

        rows = wait_until(
            _card_on_b,
            budgets.RPC_ROUNDTRIP_S * 3,
            diagnose=lambda: (
                f"the second seat shows no pending paste card for {domain}; "
                f"domains={seat_b.admin.dns_domain_names()!r} (error: {_error(seat_b)})"
            ),
        )
        assert [(r["name"], r["value"]) for r in rows] == [(before["name"], before["value"])], (
            f"the second seat must show the record the admin was pasting on the first: "
            f"first={before!r} second={rows!r}"
        )
    finally:
        try:
            driver_b.screenshot("teardown-manual-issue-second-seat")
        except Exception:
            pass
        driver_b.teardown()

    _cancel(admin_app, first_idx, domain)


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_failed_manual_completion_keeps_the_card_for_a_retry(
    request, admin_app, nest_instance, acme_pebble
):
    """Outcome 54, the failed-attempt clause: a completion that fails leaves the
    paste card up with a record, and pressing complete again then issues —
    nothing was discarded (`tls-certificates.md` § Surviving an interrupted
    manual issuance — "nothing is discarded silently").

    **The failure path: a resume the CA answers with a new challenge.** After a
    restart the app holds no live order, so complete re-opens a fresh one for
    the restored account from the breadcrumb; pebble never reuses a pending
    authorization across `new-order` calls (§ Surviving an interrupted manual
    issuance, the S4b measurement), so the CA asks for a different value than
    the one the admin pasted and the completion fails at once with the error
    that says so (`resume_manual_issue`). That is a real failure an admin can
    meet, and it fails fast. A WRONG published value is not: the propagation
    gate polls for the exact value until its fixed 45-minute deadline
    (`acme_shared.rs` `DEFAULT_RESOLVABILITY_DEADLINE`) before the CA is ever
    asked, so it would hold the completion, not fail it."""
    domain = f"manualretry-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, domain)
    _, first = _begin_manual_issue(admin_app, domain)
    # The admin pastes the record they were shown.
    acme_pebble.publish_txt(first["name"], first["value"])

    idx, _ = _restart_and_read_paste_card(request, admin_app, nest_instance, domain)
    admin_app.admin.complete_manual_cert(idx)
    wait_until(
        admin_app.has_error,
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"the resumed completion did not fail — if pebble now reuses the pending "
            f"authorization, this path succeeds and the test needs another failure "
            f"path; card={admin_app.admin.cert_pending_paste_present(idx)} "
            f"records={admin_app.admin.dns_records(idx)!r}"
        ),
    )
    error = _error(admin_app)
    assert "new challenge value" in error, (
        f"the completion failed for another reason than the CA's new challenge: {error!r}"
    )

    # The card is still up, with the record to publish for the retry.
    assert admin_app.admin.cert_pending_paste_present(idx), (
        f"a failed completion retired the paste card; error: {error!r}"
    )
    rows = _challenge_rows(admin_app, idx, domain)
    assert len(rows) == 1 and rows[0]["name"] == first["name"], (
        f"a failed completion must leave one challenge record to paste, got {rows!r}"
    )

    # The retry: publish the record the card now shows and complete again. Only
    # a delivered certificate retires the card (`complete_manual_issue`).
    acme_pebble.publish_txt(rows[0]["name"], rows[0]["value"])
    admin_app.admin.complete_manual_cert(idx)
    wait_until(
        lambda: not admin_app.admin.cert_pending_paste_present(idx),
        _GATE_POLL_S + budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"the retry never issued; records={admin_app.admin.dns_records(idx)!r} "
            f"(error: {_error(admin_app)})"
        ),
    )


# The propagation gate's poll interval (`DEFAULT_RESOLVABILITY_POLL_INTERVAL`,
# `libs/fauna-client-dns/src/acme_shared.rs`): a record served after a missed
# probe is seen on the next poll, at most this much later.
_GATE_POLL_S = 15.0

# The gate's line once every challenge TXT is served (`PropagationGate::wait`,
# `acme_shared.rs`); its `attempt` field counts polls from 0.
_GATE_SERVED = "every challenge TXT confirmed served by the authoritative NS"
_ATTEMPT = re.compile(r"\battempt=(\d+)")


def _require_app_log(admin_app) -> None:
    """Declare an environment skip when this launch's app log is unreachable —
    the gate's `attempt` is read from it. Called after the order opened, when the
    app has certainly logged something."""
    from helpers.app_surface import skip_environment

    if not admin_app.driver.app_stderr_text().strip():
        skip_environment(
            "the app's log does not reach driver.app_stderr_text(), which this "
            "test reads the propagation gate's attempt from"
        )


def _gate_attempts(admin_app) -> list[int]:
    """The `attempt` of every gate-passed line in this launch's app log."""
    return [
        int(m.group(1))
        for ln in admin_app.driver.app_stderr_text().splitlines()
        if _GATE_SERVED in ln and (m := _ATTEMPT.search(ln))
    ]


@pytest.mark.feature("admin-dns-and-certificates")
def test_manual_complete_waits_for_the_name_servers_then_issues(
    admin_app, nest_instance, acme_pebble
):
    """Outcomes 59 and 60. Complete is pressed BEFORE the domain's name server
    serves the pasted record: once the gate's probe has asked that name server
    and missed, the issuance is still waiting — paste card up, no error — rather
    than letting the CA validate a record nobody serves. Once the record is
    served the gate passes on a later poll (`attempt` ≥ 1), the CA validates and
    the card retires, which only a delivered certificate does
    (`complete_manual_issue`). After it a fresh read of the records page lists
    no `_acme-challenge` row: the issuance record is transient, never a standing
    record shown as missing (`tls-certificates.md` § Surviving an interrupted
    manual issuance; § B, the transient challenge TXT)."""
    domain = f"manualgate-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, domain)
    idx, row = _begin_manual_issue(admin_app, domain)
    challenge = row["name"]
    _require_app_log(admin_app)

    passed_before = len(_gate_attempts(admin_app))
    probed_before = acme_pebble.txt_queries(challenge)
    admin_app.admin.complete_manual_cert(idx)

    # The gate's first probe reaches the name server, which does not serve the
    # record yet — positively observed, not slept past.
    wait_until(
        lambda: acme_pebble.txt_queries(challenge) > probed_before,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"the propagation gate never asked the name server for {challenge} "
            f"(error: {_error(admin_app)})"
        ),
    )
    assert admin_app.admin.cert_pending_paste_present(idx), (
        f"the paste card retired before the record was served; error: {_error(admin_app)}"
    )
    assert not admin_app.has_error(), (
        f"complete failed while the record was not yet served: {_error(admin_app)}"
    )
    assert len(_gate_attempts(admin_app)) == passed_before, (
        "the propagation gate passed although no name server served the record"
    )

    # The admin's DNS provider catches up: the name server now serves the TXT.
    acme_pebble.publish_txt(challenge, row["value"])
    attempts = wait_until(
        lambda: _gate_attempts(admin_app)[passed_before:] or None,
        _GATE_POLL_S + budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"no gate-passed line after the record was served "
            f"(error: {_error(admin_app)})"
        ),
    )
    assert attempts[0] >= 1, (
        f"the gate must pass on a later poll than the one that missed, "
        f"got attempt={attempts[0]}"
    )

    wait_until(
        lambda: not admin_app.admin.cert_pending_paste_present(idx),
        budgets.PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"the paste card never retired after the gate passed; "
            f"records={admin_app.admin.dns_records(idx)!r} (error: {_error(admin_app)})"
        ),
    )
    assert not admin_app.has_error(), f"the issuance failed: {_error(admin_app)}"

    # Outcome 60: a fresh read of the records page lists no issuance record.
    admin_app.admin.navigate_dns()
    idx = _domain_index(admin_app, domain)
    want = f"{_CHALLENGE_PREFIX}{domain}"
    records = wait_until(
        lambda: admin_app.admin.dns_records(idx) or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the records page lists nothing for {domain} (error: {_error(admin_app)})",
    )
    names = [r["name"].rstrip(".").lower() for r in records]
    assert want not in names, (
        f"the transient {want} record is listed as a standing record: {names!r}"
    )
