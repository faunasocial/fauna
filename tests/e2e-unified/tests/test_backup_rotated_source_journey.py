"""tier_3 e2e: a rotated source box keeps backing up — the owner's device carries
its writer seat with no gesture.

``docs/goal/architecture/segment-backup-protocol.md`` § Cross-location backup
protocol → *The writer seat* (the *How the seat moves* and *Where the carry runs*
paragraphs): a box that rotates its deployment identity is refused by every
destination, because the seat there still names the identity it had before. One
of the owner's devices carries the seat — over its own connection to the
destination, once it has verified the box's rotation chain — from the Backups
page's audit pass, ahead of the freshness arm, and on the first pass under the
changed identity regardless of the 24 h debounce. The user does nothing.

The journey joins two halves the suite already proves apart:
``test_backups.py``'s enroll-through-the-page + the nest-side sweep poke
(``test_backup_destination_last_upload_time_reflects_a_nest_side_pass``), and
``test_nest_rotation_admin_journey.py``'s rotation through the admin page with
the live session silently re-pinned. Here both run on one box, in order:

    enroll a destination on the Backups page → a sweep uploads (baseline)
    rotate the deployment seed on the admin page → the session re-pins live
    one new mail                                   → backlog for the next sweep
    open the Backups page                          → the list re-files under the
                                                     successor and the audit pass
                                                     carries the seat (no gesture)
    the destination's seat names the successor     → (split verdict, convention 5)
    a sweep                                        → accepted: backlog drained,
                                                     "Last synced" advances

Owner = the box's admin, the one identity that may rotate it; the destination is
the session's ``second_nest``, where this function-scoped box's fresh admin is a
fresh owner, so the seat this journey leaves there is nobody else's.

**tui leads** (`segment-backup-protocol.md` § Implementation status today names
the apps this journey is proven on). The carry is shared Rust every audit host
already runs (``fauna_client_backup::audit::run_audit_pass``, called by tui,
linux, the FFI and the wasm SPA), so the other apps' arms are parity witnesses
for their trickle-down queues, not new mechanism.

Latency-independent (convention 14): the sweep poke is a causal barrier, and
every wait is a named generous budget + deadline poll on served or rendered
state.
"""

import time

import pytest
import requests

from actions import ActionLayer
from common.auth import register_user
from common.launch_harness import make_launch_harness, reached_authenticated_app
from conftest import _trust_seeder
from i18n.strings import S
from tests.test_backups import _backlog_count, _seed_one_mail_segment

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# Element IDs (tests/e2e-unified/ui.yaml § admin_nest).
ARM_BUTTON = "admin-nest-seed-rotate-button"
ROSTER_ITEM_0 = "admin-nest-seed-rotate-roster-item-0"
CONFIRM_BUTTON = "admin-nest-seed-rotate-confirm-button"
STATUS = "admin-nest-seed-rotate-status"
DONE = "Deployment identity rotated. Apps re-trust this nest automatically."
WORKING = "Rotating the deployment identity…"

_NEVER_UPLOAD_TEXT = S.backups.backup_destination_last_upload_never

# Named budgets (generous ceilings; deadline polls pay only the real delay).
_PAGE_BUDGET_S = 30.0         # a page's controls after a nav patch
_ADOPTION_BUDGET_S = 60.0     # the box serving the successor in-process
_CEREMONY_BUDGET_S = 180.0    # the rotation verdict, incl. the live re-pin
_CARRY_BUDGET_S = 120.0       # the Backups page's refresh: re-file + audit + carry
_RENDER_BUDGET_S = 30.0       # the row's re-read after a re-mount


def _poll(check, budget_s, tag, detail=None):
    """Deadline-poll ``check`` until truthy; the failure says what was seen
    (convention 6)."""
    deadline = time.monotonic() + budget_s
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(0.25)
    seen = ""
    if detail is not None:
        try:
            seen = f" — seen: {detail()!r}"
        except Exception as e:  # noqa: BLE001 — diagnosis must not mask the failure
            seen = f" — (could not read: {e})"
    raise AssertionError(f"{tag}: not reached within {budget_s}s{seen}")


def _owner(nest):
    """The box's admin as an owner handle (the shape ``test_backups`` helpers
    take)."""
    sk = nest["admin"]["signing_key"]
    return {
        "signing_key": sk,
        "actor_id_bytes": bytes(sk.verify_key),
        "actor_id_hex": bytes(sk.verify_key).hex(),
    }


def _call(url, owner, kind, payload):
    """One USER-class call as ``owner`` over its own authenticated socket."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        url, actor_id=owner["actor_id_bytes"], signing_key=bytes(owner["signing_key"]),
    )
    with client:
        return client.call(kind, payload)


def _seat_holder(dest_url, owner):
    """The writer the owner's seat at the destination names (hex), or None."""
    grants = _call(dest_url, owner, "fauna.backup.writer_grant.list", {}).get("grants", [])
    return grants[0]["writer_nest_id"].lower() if grants else None


def _served_id(nest_url):
    """The identity the box serves right now (hex), or None mid-restart."""
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    try:
        with WsRpcAnonClient(nest_url) as anon:
            return anon.call("fauna.nest.info", {})["nest_id"]
    except Exception:  # noqa: BLE001 — the re-enter window's error shape varies
        return None


def _sweep(nest_url):
    """The causal barrier: one synchronous backup sweep on the source. The box
    serves its self-signed floor cert (the rotation's pin needs real TLS), so
    the test hook is called unverified, as the suite's other TLS-nest hooks are
    (`test_bluesky_feed_ingest.py`)."""
    resp = requests.post(
        f"{nest_url}/api/v1/test/backup/run-now", json={}, timeout=120, verify=False,
    )
    assert resp.status_code == 200, f"run-now returned {resp.status_code}: {resp.text}"
    assert resp.json().get("ok") is True, resp.text


def _status_row(nest_url, owner):
    """The source's own status projection for the one destination — the
    source the Backups page reads."""
    reply = _call(nest_url, owner, "fauna.backup.status", {})
    rows = reply.get("destinations", [])
    assert len(rows) == 1, f"expected one destination row: {reply!r}"
    return rows[0]


def _rotate_through_the_admin_page(driver, nest_url):
    """The admin journey's drive, compressed: nav → arm → roster → confirm →
    the box serves the successor → the clean verdict. Returns the successor's
    id. Each step is asserted in full by ``test_nest_rotation_admin_journey``;
    here they are preconditions."""
    before = _served_id(nest_url)
    driver.set_state({
        "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-nest"}]},
    })
    _poll(lambda: driver.count(ARM_BUTTON) > 0, _PAGE_BUDGET_S, "admin-nest renders")
    driver.click(ARM_BUTTON)
    _poll(lambda: driver.count(ROSTER_ITEM_0) > 0, _PAGE_BUDGET_S, "the roster resolves")
    from helpers.waiting import account_pump_role, await_account_runtime_assembled

    if account_pump_role(driver) is not None:
        await_account_runtime_assembled(driver)
    driver.click(CONFIRM_BUTTON)
    after = {}

    def _adopted():
        served = _served_id(nest_url)
        if served is not None and served != before:
            after["hex"] = served
            return True
        return False

    _poll(_adopted, _ADOPTION_BUDGET_S, "the box serves the successor",
          detail=lambda: driver.get_text(STATUS) if driver.count(STATUS) > 0 else None)
    _poll(lambda: driver.count(STATUS) > 0 and driver.get_text(STATUS) != WORKING,
          _CEREMONY_BUDGET_S, "the rotation reports its verdict")
    assert driver.get_text(STATUS) == DONE, (
        f"the rotation must end on the clean verdict (the live session re-pinned "
        f"to the successor), got {driver.get_text(STATUS)!r}"
    )
    return before.lower(), after["hex"].lower()


@pytest.mark.feature("backup-destinations-and-restore")
def test_a_rotated_source_box_keeps_backing_up_with_no_gesture(
    request, rotatable_tls_nest, second_nest, tmp_path, tui_app_path,
):

    source = rotatable_tls_nest
    owner = _owner(source)
    # The owner must be registered at the destination for the enroll's
    # handshake (`require_registration` is the nest default) — the same
    # precondition `test_backups` sets up.
    register_user(
        second_nest["port"],
        owner["actor_id_hex"],
        admin_signing_key=second_nest["admin"]["signing_key"],
    )

    harness = make_launch_harness(
        "tui", tmp_path=tmp_path, app_path=tui_app_path, seed_trust=_trust_seeder(request),
    )
    try:
        driver = harness.launch(
            secret_hex=bytes(owner["signing_key"]).hex(),
            node_url=source["url"],
            trust=source,
        )
        reached_authenticated_app(driver, timeout=90)
        app = ActionLayer(driver)

        # ── Before the rotation: enroll through the page, and a sweep uploads
        _seed_one_mail_segment(app, source, owner)
        app.backups.navigate()
        app.backups.wait_for_destination_count(0)
        app.backups.add_destination(second_nest["url"], name="Rotation-proof")
        app.backups.wait_for_destination_count(1)
        _sweep(source["url"])
        baseline = _status_row(source["url"], owner)
        assert baseline["backlog_count"] == 0 and baseline["last_upload_time"], (
            f"the pre-rotation sweep must upload the seeded mail: {baseline!r}"
        )

        # ── The rotation, through the admin page ──────────────────────────
        old_id, new_id = _rotate_through_the_admin_page(driver, source["url"])
        assert _seat_holder(second_nest["url"], owner) == old_id, (
            "before any device carries it, the seat names the predecessor"
        )

        # New content, so the next accepted sweep has something to send.
        _seed_one_mail_segment(app, source, owner)

        # ── The owner opens the Backups page: no other gesture ────────────
        app.backups.navigate()
        app.backups.wait_for_destination_count(1)
        _poll(
            lambda: _seat_holder(second_nest["url"], owner) == new_id,
            _CARRY_BUDGET_S,
            "the device carries the seat to the successor (split verdict: the "
            "destination's own list)",
            detail=lambda: {
                "seat": _seat_holder(second_nest["url"], owner),
                "page_error": app.error_text(),
            },
        )

        # ── The next sweep is accepted ────────────────────────────────────
        _sweep(source["url"])
        carried = _status_row(source["url"], owner)
        assert carried["backlog_count"] == 0, (
            f"after the carry the rotated box's sweep must drain its backlog: {carried!r}"
        )
        assert carried["last_upload_time"] > baseline["last_upload_time"], (
            f"the accepted sweep must advance the upload time past the "
            f"pre-rotation one: before={baseline!r} after={carried!r}"
        )

        # ── And the page says so ──────────────────────────────────────────
        app.driver.navigate_to("feed")
        app.backups.navigate()
        app.backups.wait_for_destination_count(1)

        def _row_drained():
            text = app.driver.get_text(
                "backup-destination-backlog-count",
                scope="backup-destination-status-row[0]",
            )
            return _backlog_count(text) == 0

        _poll(_row_drained, _RENDER_BUDGET_S, "the row's backlog reads 0 queued",
              detail=app.error_text)
        synced = app.driver.get_text(
            "backup-destination-last-upload-time",
            scope="backup-destination-status-row[0]",
        )
        assert synced.strip() != _NEVER_UPLOAD_TEXT, (
            f"the row must read a fresh 'Last synced', got {synced!r}; "
            f"error={app.error_text()!r}"
        )
    finally:
        harness.teardown()
