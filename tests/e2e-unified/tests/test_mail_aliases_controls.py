"""E2E journeys for the mail-aliases page's per-row facts and sheet controls.

Target state: docs/goal/behavior/mail-aliases.md § Aliases UX → § Layout (the
row's label, kind badge and hit count; the add/edit sheet; the generate
button's top-of-list + copy + toast), § Per-alias controls (the per-address spam
threshold and hourly limit), § Kind 3 — Wildcard prefix, and § Reserved
local-parts; UX/IDs: tests/e2e-unified/ui.yaml `mail-aliases` page +
`mail-aliases-list` component.

The sibling `test_mail_aliases.py` owns the page's core lifecycle (mint, add,
revoke, delete); this file owns what the page SHOWS about each address and what
the sheet lets a person SET on it, each asserted twice — on the rendered row
and, for every mutation, over the same user WS-RPC surface the app writes
(ground truth, never UI introspection alone).

tier_3: a real app driver against a real fauna-nest; the hit-count journey adds
the real Go MTA bridge and a real inbound SMTP delivery, because a hit is only
ever written by the nest's recipient resolver (`log_alias_hit`), never by a
user-callable door.
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.copy_button import read_clipboard_or_copied
from helpers.mail_aliases import default_alias_domain
from helpers.mail_client_ui import plain_message, route_inbound_mail_to_app
from helpers.mail_wire import _connect_smtp_starttls
from i18n.strings import S

# tui leads (the lead app); the other six apps build the same page off the same
# shared `MailAliasesMachine` and join by adding their marker once their run is
# green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.macos, pytest.mark.ios]

SEED_DOMAIN = "aliases-controls-e2e.test"
ROW_LABEL = "mail-aliases-list-item-label"
ROW_KIND = "mail-aliases-list-item-kind"
ROW_HITS = "mail-aliases-list-item-hits"


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _list(api):
    return api.call("fauna.bridges.list_account_aliases", {})["aliases"]


def _delete_all(api):
    """Leave the session-scoped actor as found (the canonical row refuses
    deletion, which is the point of it)."""
    for row in _list(api):
        try:
            api.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
        except Exception:
            pass


def _row(api, kind, pattern):
    return next(
        (r for r in _list(api) if r["kind"] == kind and r["pattern"] == pattern), None
    )


def _open_page_with_domain(app, api):
    """Seed an exact alias, open the page and pull the seed into the snapshot,
    so the page has a `default_domain` and its create affordances are live
    (the sibling file's arrange, for the same reason). Returns the seed's
    local part."""
    seed = "seed" + secrets.token_hex(3)
    api.call(
        "fauna.bridges.create_account_alias",
        {"kind": "exact", "local_domain": SEED_DOMAIN, "pattern": seed, "controls": {"label": ""}},
    )
    app.mail_aliases.navigate()
    assert app.mail_aliases.is_page_visible()
    app.mail_aliases.refresh_via_generate()
    assert app.mail_aliases.wait_for_pattern(f"{seed}@{SEED_DOMAIN}"), (
        f"the seeded alias should render; rows={app.mail_aliases.patterns()!r}; "
        f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
    )
    return seed


def _wait(predicate, timeout=12.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.3)
    return predicate()


@pytest.mark.feature("mail-aliases")
def test_add_wildcard_prefix_from_the_sheet(logged_in_app, test_user, nest_instance):
    """Outcome 7: a wildcard prefix added from the sheet lands as a Wildcard
    row showing the glob a person reasons about (`<prefix>*@<domain>`)."""
    app = logged_in_app
    prefix = "wc" + secrets.token_hex(3) + "-"
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            _open_page_with_domain(app, api)
            domain = default_alias_domain(api)
            app.mail_aliases.add_wildcard(prefix)
            glob = f"{prefix}*@{domain}"
            assert app.mail_aliases.wait_for_pattern(glob), (
                f"the wildcard should render as {glob!r}; "
                f"rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            idx = app.mail_aliases.index_of_pattern(glob)
            assert app.mail_aliases.row_text(ROW_KIND, idx) == "Wildcard"
            assert _row(api, "wildcard_prefix", prefix) is not None, (
                f"the nest should hold the wildcard prefix {prefix!r}: {_list(api)!r}"
            )
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_add_sheet_sets_label_spam_threshold_and_hourly_limit(
    logged_in_app, test_user, nest_instance
):
    """Outcomes 8 and 11: the label typed on the sheet shows on the row, and
    the per-address spam threshold and hourly mail limit are stored with it."""
    app = logged_in_app
    local = "ctl" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            _open_page_with_domain(app, api)
            addr = f"{local}@{default_alias_domain(api)}"
            app.mail_aliases.add_exact(local, label="Newsletters", spam_threshold=9, rate_per_hour=12)
            assert app.mail_aliases.wait_for_pattern(addr), (
                f"{addr!r} should render; rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            idx = app.mail_aliases.index_of_pattern(addr)
            assert app.mail_aliases.row_text(ROW_LABEL, idx) == "Newsletters"
            row = _row(api, "exact", local)
            assert row is not None, f"the nest should hold {local!r}: {_list(api)!r}"
            assert row["label"] == "Newsletters"
            assert row["spam_threshold_override"] == 9, row
            assert row["rate_limit_per_hour"] == 12, row
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_edit_sheet_changes_label_and_controls_but_never_the_kind(
    logged_in_app, test_user, nest_instance
):
    """Outcome 10: the edit sheet opens pre-populated, its kind picker is
    read-only, and a save changes the label, threshold and limit while the
    address keeps the kind it was minted with."""
    app = logged_in_app
    local = "edt" + secrets.token_hex(3)
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            _open_page_with_domain(app, api)
            addr = f"{local}@{default_alias_domain(api)}"
            app.mail_aliases.add_exact(local, label="Before", spam_threshold=5, rate_per_hour=10)
            assert app.mail_aliases.wait_for_pattern(addr)

            app.mail_aliases.open_edit(app.mail_aliases.index_of_pattern(addr))
            assert app.mail_aliases.sheet_value("mail-aliases-add-sheet-label-input") == "Before"
            assert app.mail_aliases.sheet_value("mail-aliases-add-sheet-spam-threshold-input") == "5"
            assert app.mail_aliases.sheet_value("mail-aliases-add-sheet-rate-per-hour-input") == "10"
            assert app.mail_aliases.kind_picker_disabled(), (
                "the kind picker must be read-only while editing "
                "(mail-aliases.md § Layout: a wildcard cannot become a disposable mid-life)"
            )
            app.mail_aliases.submit_edit(label="After", spam_threshold=7, rate_per_hour=20)

            def _edited():
                r = _row(api, "exact", local)
                return r is not None and r["label"] == "After"

            assert _wait(_edited), (
                f"the edit should reach the nest; rows={_list(api)!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            row = _row(api, "exact", local)
            assert row["spam_threshold_override"] == 7, row
            assert row["rate_limit_per_hour"] == 20, row
            assert _wait(
                lambda: app.mail_aliases.row_text(
                    ROW_LABEL, app.mail_aliases.index_of_pattern(addr)
                ) == "After"
            ), "the row should show the edited label"
            assert app.mail_aliases.row_text(
                ROW_KIND, app.mail_aliases.index_of_pattern(addr)
            ) == "Exact"
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_a_throwaway_address_lasts_and_takes_what_you_chose(
    logged_in_app, test_user, nest_instance
):
    """Outcome 12: on the add sheet you pick Disposable and choose how many days
    the address lasts and how many messages it takes; the address the nest mints
    carries exactly those — not the defaults (30 days, 1 use) — and the page
    lists it."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            _open_page_with_domain(app, api)
            before = app.mail_aliases.disposable_count()
            label = "Shop " + secrets.token_hex(2)
            minted_at_ms = int(time.time() * 1000)
            app.mail_aliases.add_disposable(ttl_days=2, uses=2, label=label)
            assert app.mail_aliases.wait_for_disposable_count(before + 1), (
                f"the mint should add a row; rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            row = next(r for r in _list(api) if r["kind"] == "disposable" and r["label"] == label)
            assert row["uses_remaining"] == 2, f"the chosen use count must be stored: {row!r}"
            lasts_ms = row["expires_at"] - minted_at_ms
            two_days_ms = 2 * 24 * 3600 * 1000
            assert abs(lasts_ms - two_days_ms) < 3600 * 1000, (
                f"the chosen two-day lifetime must be stored; lasts {lasts_ms} ms: {row!r}"
            )
            assert any(row["pattern"] in p for p in app.mail_aliases.patterns()), (
                f"the page must list the minted address; rows={app.mail_aliases.patterns()!r}"
            )
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_generated_disposable_lands_on_top_copied_and_confirmed(
    logged_in_app, test_user, nest_instance
):
    """Outcome 13: a minted throwaway address is the first row, its full
    address is on the clipboard, and the page confirms the copy."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            _open_page_with_domain(app, api)
            before = app.mail_aliases.disposable_count()
            app.mail_aliases.generate_disposable()
            assert app.mail_aliases.wait_for_disposable_count(before + 1), (
                f"the mint should add a row; rows={app.mail_aliases.patterns()!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
            newest = max(
                (r for r in _list(api) if r["kind"] == "disposable"),
                key=lambda r: r["created_at"],
            )
            top = app.mail_aliases.patterns()[0]
            assert "-temp-" in top and newest["pattern"] in top, (
                f"the new throwaway address should head the list; top={top!r}, "
                f"newest={newest!r}"
            )
            assert _wait(lambda: app.mail_aliases.minted_confirmation() is not None), (
                "the page should confirm the copy after a mint"
            )
            confirmed = app.mail_aliases.minted_confirmation()
            assert newest["pattern"] in confirmed and "@" in confirmed, confirmed
            clip = read_clipboard_or_copied(app, "mail-aliases-generate-disposable-button")
            assert clip == confirmed, (
                f"the confirmed address should be what was copied: clip={clip!r}, "
                f"confirmed={confirmed!r}"
            )
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_reserved_name_is_refused_on_the_page(logged_in_app, test_user, nest_instance):
    """Outcome 17: claiming `postmaster` is refused and the page says why;
    nothing is created."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            _open_page_with_domain(app, api)
            app.mail_aliases.add_exact("postmaster")
            err = app.mail_aliases.page_error_text()
            assert "postmaster" in err and "reserved" in err.lower(), (
                f"the page should say postmaster is reserved; error={err!r}"
            )
            assert _row(api, "exact", "postmaster") is None
        finally:
            # The refused sheet stays open with the entry intact (the
            # add-credential convention); close it so the next test starts clean.
            if app.driver.is_visible("mail-aliases-add-sheet-cancel-button"):
                app.mail_aliases.cancel_add()
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_delete_confirm_relabels_the_button_before_deleting(
    logged_in_app, test_user, nest_instance
):
    """Outcome 29: the first Delete press only asks — the button relabels to
    the shared bare armed label `common.confirm_q` ("Confirm?", apps/common.md
    § Two-click confirm) and the nest still holds the address — and the second
    press deletes it. The literal also witnesses that the armed label is
    readable to the driver (apple's `/element/text` read `""` for it until its
    `text:` closure landed)."""
    app = logged_in_app
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            seed = _open_page_with_domain(app, api)
            idx = app.mail_aliases.index_of_pattern(f"{seed}@{SEED_DOMAIN}")
            assert app.mail_aliases.delete_button_text(idx) == S.mail_aliases.delete, (
                f"an unarmed delete button reads {S.mail_aliases.delete!r}: "
                f"{app.mail_aliases.delete_button_text(idx)!r}; "
                f"{app.mail_aliases.driver.diagnose('mail-aliases-list-item-overflow-menu')}"
            )

            app.mail_aliases.arm_delete(idx)
            assert _wait(
                lambda: app.mail_aliases.delete_button_text(idx) == S.common.confirm_q
            ), (
                f"the armed delete must relabel to {S.common.confirm_q!r}: "
                f"{app.mail_aliases.delete_button_text(idx)!r}"
            )
            assert _row(api, "exact", seed) is not None, "arming must delete nothing"

            app.mail_aliases.confirm_delete(idx)
            assert _wait(lambda: _row(api, "exact", seed) is None), (
                f"the confirmed delete should reach the nest: {_list(api)!r}; "
                f"error: {app.mail_aliases.page_error_text(timeout=2.0)!r}"
            )
        finally:
            _delete_all(api)


@pytest.mark.feature("mail-aliases")
def test_row_shows_hit_count_and_last_hit_after_real_inbound(
    logged_in_app, mail_bridge_mta, nest_instance, test_user
):
    """Outcome 9: after one real message arrives through an address, its row
    shows one hit and when it last had one (the shared `alias_hits_label`'s
    dated arm) — observed through the page, with the nest's count as ground
    truth."""
    app = logged_in_app
    local = "hits" + secrets.token_hex(3)
    addr = route_inbound_mail_to_app(app, mail_bridge_mta, nest_instance, test_user, local)
    with _user_client(nest_instance, test_user) as api:
        try:
            nonce = f"aliashit{secrets.token_hex(4)}"
            deadline = time.monotonic() + 40.0
            with _connect_smtp_starttls(mail_bridge_mta.mx_port, mail_bridge_mta.domain, deadline) as conn:
                conn.cmd("MAIL FROM:<sender@external.test>", "250", deadline)
                conn.cmd(f"RCPT TO:<{addr}>", "250", deadline)
                conn.cmd("DATA", "354", deadline)
                conn.send_raw(plain_message("sender@external.test", addr, nonce))
                conn.cmd(".", "250", deadline)
                conn.cmd("QUIT", "221", deadline)
            assert _wait(lambda: (_row(api, "exact", local) or {}).get("hit_count") == 1), (
                f"the nest should count one hit on {addr!r}: {_row(api, 'exact', local)!r}"
            )

            app.mail_aliases.navigate()
            assert app.mail_aliases.is_page_visible()
            assert app.mail_aliases.wait_for_pattern(addr), app.mail_aliases.patterns()
            hits = app.mail_aliases.row_text(ROW_HITS, app.mail_aliases.index_of_pattern(addr))
            assert hits.split()[0] == "1" and "·" in hits, (
                f"the row should show one hit and its date; hits={hits!r}"
            )
        finally:
            row = _row(api, "exact", local)
            if row is not None:
                api.call("fauna.bridges.delete_account_alias", {"alias_id": row["alias_id"]})
