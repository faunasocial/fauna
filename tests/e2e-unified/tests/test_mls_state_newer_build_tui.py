"""tier_3 E2E: an app too old for the conversations it saved on this device says
so — update the app, they are intact — and never writes a byte of them (tui).

``docs/goal/architecture/version-compatibility.md`` § Dimension 1 (the client
at-rest table, the ``mls.db`` row): the per-account MLS store carries the
two-number stamp, the open-time verdict runs before any mutation, and a
database a newer build wrote with a BREAKING change is refused honestly —
never migrated, never rewritten, never read as damaged. ``libs/fauna-mls``
answers it with the typed ``MlsError::SchemaIncompatible``; this test closes
the loop from that verdict to what the person sees on the conversations page
(``ConversationsState::state_needs_newer_app`` → ``sync_page_error``), against a
REAL ``fauna-tui`` reopening a REAL on-disk store. The arm itself is pinned at
tier_1 in ``apps/fauna-tui/src/conversations/conv_backend.rs``.

Mechanism: sign a dedicated actor in so its ``mls_state.db`` exists, stop the
app, forge the store's ``schema_meta`` row as a newer build would leave it,
relaunch on the same client-local store, sign back in. **Both** stamp numbers
are raised: ``Incompatible`` needs ``schema_version > bin_v`` AND
``min_reader_version > bin_v`` (``libs/fauna-mls/src/version.rs``) — raising the
reader floor alone is the ``UpgradeOrCurrent`` verdict, which reconciles and
restamps the store, so there would be no prompt and the bytes would move.

The dedicated actor is load-bearing: the forged store is that actor's alone,
so no later test that signs in as the shared ``test_user`` opens it.

tier_3: needs a real ``fauna-nest`` (``nest_instance``); tui and linux (the
store's file name per app is ``helpers/skew_client.py::SCOPED_MLS_DB_NAME``).
android's store is on-device (``AndroidScopeStore``), so its witness waits on
the emulator venue; its arm is pinned by a Robolectric unit test meanwhile. The
file keeps its ``_tui`` name so the ledger key stays valid. The other apps lift
the same arm over the shared ``conversations.errors.state_needs_newer_app``
string.
"""

from __future__ import annotations

import hashlib
import sqlite3
from pathlib import Path

import pytest

from common.scope_store import attach_scope_store
from conftest import _login_app_as, _make_user
from helpers import budgets
from helpers.skew_client import SCOPED_MLS_DB_NAME
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux]

#: Far above any reader floor this build will carry, so the verdict stays
#: ``Incompatible`` across future baseline bumps (the same choice as
#: ``test_account_index_unreadable_launch.FUTURE_VERSION``).
FUTURE_VERSION = 9999


def _client(driver) -> str:
    """The app under test: the key of ``SCOPED_MLS_DB_NAME`` / the scope store."""
    return "linux" if driver.is_linux() else "tui"


def _mls_db(driver, actor_id_hex: str) -> Path | None:
    """The actor's per-account MLS store, or None while it is absent."""
    client = _client(driver)
    for d in attach_scope_store(client, driver).scope_dirs(actor_id_hex):
        candidate = d / SCOPED_MLS_DB_NAME[client]
        if candidate.is_file():
            return candidate
    return None


def _forge_newer_build_stamp(db: Path) -> None:
    """Stamp ``db`` as a newer build with a breaking change leaves it. Closing
    the only connection checkpoints any WAL into the main file, so the hash
    taken next covers everything the app could later read."""
    conn = sqlite3.connect(db)
    try:
        with conn:
            updated = conn.execute(
                "UPDATE schema_meta SET schema_version = ?, min_reader_version = ? WHERE id = 1",
                (FUTURE_VERSION, FUTURE_VERSION),
            ).rowcount
    finally:
        conn.close()
    assert updated == 1, f"precondition: {db} carries no schema_meta row to forge"


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@pytest.mark.feature("upgrades-never-lose-data")
def test_conversations_a_newer_build_saved_say_update_the_app_and_stay_untouched(
    app, request, nest_instance
):
    user = _make_user(nest_instance)
    actor = user["actor_id_hex"]
    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    db = wait_until(
        lambda: _mls_db(app.driver, actor),
        budgets.UI_SETTLE_S,
        diagnose=lambda: (
            "the post-auth hook never opened this actor's MLS store under "
            f"{[str(d) for d in attach_scope_store(_client(app.driver), app.driver).scope_dirs(actor)]}"
        ),
    )

    assert app.driver.preserve_state_across_relaunch(), (
        "this journey needs the relaunched app to reopen the store it saved; "
        "without the pin the driver relaunches on a fresh client-local store"
    )
    # The engine holds the store open; stop the process before the forge.
    app.driver.teardown()
    _forge_newer_build_stamp(db)
    forged = _sha256(db)

    assert app.driver.recover(), "the driver could not relaunch the app"
    if app.driver.is_linux():
        # The pinned client-local store signs the relaunched app in by itself; a
        # replayed `set_state` login lands ~15 ms from that auto-login and
        # races it (the double-login race `conv_backend::build_session_engine`
        # documents), leaving the Status page empty about half the time. Wait
        # for the app's own sign-in instead of issuing a second one.
        from helpers.e2e_session import wait_live_actor_id

        seen = wait_live_actor_id(app, actor)
        assert actor.lower() in seen.lower(), (
            f"the relaunched app never signed itself back in as the dedicated "
            f"actor: account-actor-id={seen!r}, expected {actor!r}"
        )
    else:
        _login_app_as(app, request, nest_instance, user, verify_live_actor=True)

    app.conversations.navigate()
    expected = S.conversations.errors.state_needs_newer_app
    shown = wait_until(
        lambda: (t := app.error_text()) and expected in t and t,
        budgets.UI_SETTLE_S,
        diagnose=lambda: f"error-message reads {app.error_text()!r}, expected {expected!r}",
    )
    assert "damaged" not in shown.lower() and "corrupt" not in shown.lower(), shown

    # Everything that is not conversations works as usual.
    text = f"newer-mls-state probe {actor[:8]}"
    app.driver.navigate_to("feed")
    app.feed.create_post(text)
    assert app.feed.wait_for_post_text(text), f"error-message={app.error_text()!r}"

    assert _sha256(db) == forged, (
        "the app is too old for this store and must never write it — the newer "
        "build's conversations changed on disk across the refusing launch"
    )
