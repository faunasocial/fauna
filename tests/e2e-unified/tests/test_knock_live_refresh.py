"""A `fauna.knock` push live-refreshes a **mounted** contacts page — no navigation.

The knock counterpart of ``test_push_live_refresh.py`` (which does the same for the
notifications page off the generic ``fauna.notification`` push). Knocks are a
*separate* seam on the UniFFI apps: their generic ``FfiPushEvent`` stream carries
``fauna.knock`` only as ``Other``, so they need a dedicated knock pump over
``FfiNestClient.subscribe_knocks`` = ``subscribe_kind("fauna.knock")`` (windows
``NestRpcClient.StartKnockPump``, android ``ApiClient.startKnockPump``, apple's
``FaunaClient.startKnockObserver``). The Rust-native apps (tui, linux) read it off the
generic stream as ``PushEvent::Knock``, whose ``invalidates()`` stales the knocks
surface. See ``transport.md`` § Push events.

**Why this proves the pump and not a poll.** The contacts page has *no* poll on any
client — it re-fetches on mount, on ``.onReconnect``, and (once this lands) on the
dedicated knock signal. So a mounted page whose ``knock-card`` count grows while the
driver never navigates can only have re-fetched off the knock push. A client with no
knock pump wired to the page never converges at *any* timeout.

The knock is stored + pushed by the nest's ``POST /api/v1/test/push/knock`` (the
``test-hooks`` Cargo feature, ``bins/fauna-nest/src/push_test_hooks.rs``), which
drives the exact production path ``routes.rs::store_knock`` uses — a real ``knocks``
row via ``db.push_knock`` plus one real ``PushEvent::Knock`` over the ``fauna.knock``
broker kind. Nothing about the client path is mocked: real WS-RPC socket, real
dedicated-kind subscription, real ``fauna.knocks.list`` fetch, real render. Only the
*trigger* (a second actor's ``fauna.inbox.send``) is replaced by the test endpoint.

Guards ``transport.md`` § Push events — the dedicated ``fauna.knock`` seam — on apple,
the client this test file's history added it to.
"""
from __future__ import annotations

import time
import uuid

import pytest
import requests

from helpers.connection import connection_observable, wait_until_online

pytestmark = pytest.mark.tier_2


@pytest.mark.feature("contacts")
def test_knock_push_live_refreshes_mounted_contacts_page(
    logged_in_app, nest_instance, test_user,
):
    """Nest ``fauna.knock`` push → the dedicated knock seam → mounted page re-fetches."""
    app = logged_in_app

    # Sit on the contacts page and baseline the knock count *while mounted*. The
    # session-scoped actor may already carry knock rows from earlier tests, so assert
    # on the delta, never on an absolute count.
    app.contacts.navigate()
    baseline = app.contacts.knock_count()

    # Fire only once the app's transport is online: `notify_push` fans out to live
    # connections alone, so a push fired into a handshake still in flight is dropped
    # by design and reads as a dead knock pump exactly when the box is loaded. The
    # connection barrier waits on the app's own published verdict under a named
    # ceiling, never a settle-sleep (convention 14).
    wait_until_online(app.driver)

    # A FRESH `sender_id` per call. `nest_instance` / `test_user` are session-scoped,
    # so this one test firing once per `--client` fires more than one knock at the same
    # actor; a distinct 64-hex sender keeps each knock row genuinely distinct (and its
    # sender text meaningful), so the test is order- and client-count-independent.
    sender_id = uuid.uuid4().hex + uuid.uuid4().hex  # 64 hex chars = 32 bytes

    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/knock",
        json={
            "actor_id": test_user["actor_id_hex"],
            "sender_id": sender_id,
            "summary": "knock live-refresh probe",
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks knock endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.text

    # The page must grow a knock-card on its own. No navigate(), no reload — that is the
    # whole assertion.
    #
    # 30s, not the 5s a same-machine push round-trip needs: dev machines run many
    # suites at once, and a tight deadline turns load into a red test. It costs nothing
    # on the happy path (returns the moment the row lands) and nothing in fidelity — the
    # failure this guards, a client with no knock pump wired to the page, never
    # converges at any timeout.
    deadline = time.monotonic() + 30.0
    seen = baseline
    while time.monotonic() < deadline:
        seen = app.contacts.knock_count()
        if seen > baseline:
            return
        time.sleep(0.25)

    err = app.error_text() if app.has_error() else "(no error-message shown)"
    # Name the shape before blaming the pump. Rows whose `knock-sender` renders
    # with no `knock-card` around it mean the page DID re-fetch and the row's own
    # container id is missing — linux's and windows' red for weeks, both first
    # read as a knock pump that was never wired.
    senders = app.driver.count("knock-sender")
    if senders > seen:
        shape = (
            f"the page DID re-render ({senders} knock-sender) but no knock-card "
            f"wraps those rows, so the row's container id is missing — not the push path"
        )
    else:
        shape = "the page never re-fetched, so the client's knock pump is not wired to it"
    raise AssertionError(
        f"a nest fauna.knock push never reached the mounted contacts page: "
        f"knock-card count stuck at {seen} (baseline {baseline}) 30s after the "
        f"test-hook stored a real knock row and fired a real PushEvent::Knock. "
        f"{shape}. Page error banner: {err}. Transport at failure (online before "
        f"the push): {connection_observable(app.driver)}"
    )
