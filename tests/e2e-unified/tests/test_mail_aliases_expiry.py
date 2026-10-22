"""E2E journey for a throwaway address's visible expiry and its extend.

Target state: `docs/goal/behavior/mail-aliases.md` § Don't do these ("Don't
auto-expire disposable aliases without telling the user. The expiry is visible
… the user can extend (edit) before expiry") and § Wire shapes
(`update_account_alias` carries the optional `expires_at_ms`; anything but a
still-alive disposable is refused). UX/IDs: tests/e2e-unified/ui.yaml
`mail-aliases-list` — `mail-aliases-list-item-expires-at`,
`mail-aliases-list-item-extend-button`.

What this witnesses is the app half: the row says when the address expires,
the extend button moves the stored expiry out by the disposable default
lifetime (30 days) with the label kept, and the row repaints the new date. A
non-disposable row shows neither.

tier_3: a real app driver against a real fauna-nest.
"""

import secrets
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from i18n.strings import S

# tui leads (the lead app); the other six apps join by adding their marker once
# their run is green.
pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.web]

SEED_DOMAIN = "aliases-expiry-e2e.test"
ROW_EXPIRES = "mail-aliases-list-item-expires-at"
_DAY_MS = 24 * 3600 * 1000


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


def _wait(predicate, timeout=12.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.3)
    return predicate()


@pytest.mark.feature("mail-aliases")
def test_a_throwaway_address_shows_its_expiry_and_is_extended(
    logged_in_app, test_user, nest_instance
):
    """Outcome 14: a throwaway address's row says when it expires; pressing
    extend moves the expiry 30 days further out, keeps its label, and the row
    shows the new date. An ordinary address shows no expiry and no extend."""
    app = logged_in_app
    aliases = app.mail_aliases
    with _user_client(nest_instance, test_user) as api:
        _delete_all(api)
        try:
            seed = "seed" + secrets.token_hex(3)
            api.call(
                "fauna.bridges.create_account_alias",
                {"kind": "exact", "local_domain": SEED_DOMAIN, "pattern": seed,
                 "controls": {"label": ""}},
            )
            aliases.navigate()
            assert aliases.is_page_visible()
            aliases.refresh_via_generate()
            seed_addr = f"{seed}@{SEED_DOMAIN}"
            assert aliases.wait_for_pattern(seed_addr), (
                f"the seeded alias should render; rows={aliases.patterns()!r}; "
                f"error: {aliases.page_error_text(timeout=2.0)!r}"
            )

            label = "Shop " + secrets.token_hex(2)
            before = aliases.disposable_count()
            aliases.add_disposable(ttl_days=2, uses=0, label=label)
            assert aliases.wait_for_disposable_count(before + 1), (
                f"the mint should add a row; rows={aliases.patterns()!r}; "
                f"error: {aliases.page_error_text(timeout=2.0)!r}"
            )

            def stored():
                return next(
                    r for r in _list(api) if r["kind"] == "disposable" and r["label"] == label
                )

            minted = stored()
            idx = next(i for i, p in enumerate(aliases.patterns()) if minted["pattern"] in p)

            # The row says when it expires — the shared label over a local date.
            first = aliases.row_text(ROW_EXPIRES, idx)
            assert first.startswith(S.mail_aliases.expires_on(date="").strip()), (
                f"a throwaway address must say when it expires; reads {first!r}"
            )
            assert aliases.can_extend(idx), "a live throwaway address must offer extend"

            # An ordinary address shows neither.
            seed_idx = aliases.index_of_pattern(seed_addr)
            assert aliases.extend_absent(seed_idx), "an exact address has nothing to extend"

            aliases.extend(idx)
            want = minted["expires_at"] + 30 * _DAY_MS
            assert _wait(lambda: stored()["expires_at"] == want), (
                f"extend must move the expiry 30 days out (stored {stored()['expires_at']}, "
                f"wanted {want}); error: {aliases.page_error_text(timeout=2.0)!r}"
            )
            assert stored()["label"] == label, "an extend must keep the address's label"

            idx = next(i for i, p in enumerate(aliases.patterns()) if minted["pattern"] in p)
            assert _wait(lambda: aliases.row_text(ROW_EXPIRES, idx) != first), (
                f"the row must show the new expiry; still reads {first!r}"
            )
        finally:
            _delete_all(api)
