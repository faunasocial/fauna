"""tier_3: the native Address Book view reads MDA-sealed vCards — CardDAV slice 4b.

The read/consumption twin of the CardDAV round-trip (`test_carddav_roundtrip.py`,
which drives a scripted MUA end-to-end): here a scripted CardDAV MUA *seeds* one
vCard into the store, then the **Fauna app's own UI** — the Contacts page's
`Contacts | Address Book` segment (contacts.md § Layout & flow, carddav-server.md
§ Independent enablement) — reads it back, proving the client decrypts what the MDA
sealed. It drives the real binary stack (real nest + real mail-bridge MDA, real
HTTPS, real seal on PUT, real `list_addressbooks` / `query_cards` + client-side
unseal) over the shared-Rust `fauna-client-carddav` crate (native via
`FfiCarddavClient`, web via the `carddav*` wasm exports).

Why the MSEK aligns: enabling mail through the client UI mints the actor's MSEK and
registers the recipient key the MDA seals card bodies to; the Address Book view
unseals with the SAME MSEK-derived key (`unseal_card_body` — the crate's interop
contract: "same msek yields the same recipient keypair, so MDA-written and
client-written bodies are mutually readable"). So the scripted MUA's PUT and the
Fauna app's read see the same card.

Only tier_3 catches this: a stub or in-process twin never exercises the MDA
seal-on-PUT → client unseal-on-read contract across the real binaries.

RED before slice 4b: the Contacts page had no Address Book segment, so
`contacts-segment-addressbook` / `addressbook-item` / `vcard-card` did not exist and
`switch_to_address_book()` timed out. GREEN once the client renders the segment over
the landed `fauna-client-carddav` crate.

carddav-server.md § Independent enablement + § Read/Write/sync surface;
contacts.md § Layout & flow (Address Book segment).
"""

import time
import uuid

import pytest

from helpers import budgets
from helpers.carddav_client import CardDAVClient, build_vcard
from helpers.mail_dedicated_nest import (
    alias_admin_to_address as _alias_admin_to_address,
    dedicated_node_url as _dedicated_node_url,
    login_as_nest_admin as _login_as_nest_admin,
)

# linux + windows + macos + tui carry the seeded tier_3 read: the seed's scripted
# CardDAV MUA authenticates with a KNOWN bridge password, minted via
# `enable_mail_plain`.
# macos joined once apple's dedicated-mail-nest mint was fixed — the enable+mint had
# been silently no-opping because `login_as_nest_admin` omitted `device_id`, so the
# apple test agent built no `FaunaClient` and `MailSettingsVM` was never configured.
# tui joined 2026-07-29 with its Address Book segment (the 7th and last app to build
# it): its `enable_mail_plain` path drives, and `login_as_nest_admin`'s
# `admin-dashboard-heading` wait resolves against the admin dashboard tui built in M8.
# web joined here: its Address Book view
# (routes/contacts/+page.svelte) and enable-mail-plain path were both already proven
# elsewhere (test_mail_enable_then_mua_round_trip.py); only the marker was missing.
# android grows the same shared mail-settings enable path + Address Book view and
# joins this gate once its enable-mail-plain e2e is confirmed (route-3 follow-on,
# mirroring `test_events.py`'s per-app enablement).
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    pytest.mark.web,
]

# The PLAIN bridge credential the client mints and the scripted CardDAV MUA
# authenticates with (twin of the round-trip test's `_PASSWORD`).
_PASSWORD = "AddressBookRead4bPlainPw0007Kk"

# How long to wait for a seeded card to surface to the MDA read path (server-side
# seal — usually immediate; a short retry guards a settle race).
_SEED_TIMEOUT = 30.0


def _seed_mda_sealed_card(app, handle, request):
    """Enable mail through the Fauna app's own UI (mints the actor's MSEK — the
    key the MDA seals card bodies to and the Address Book view unseals with), then
    seed ONE vCard into the lazy Contacts book via a scripted CardDAV MUA PUT, so
    the Fauna app has an MDA-sealed card to read back.

    Mirrors the `test_carddav_roundtrip._enable_mail_and_client` preamble
    (priority #2/#4 — one shape). Returns `(fn, tel, email, client, book,
    uid)` — the fields the detail pane must surface, plus the scripted MUA
    handle and the seeded card's own identity, so a caller can act on the
    card again (e.g. deleting it to exercise the DROPPED deep-link case).
    """
    nest = handle.nest
    domain = handle.domain

    _login_as_nest_admin(app, nest, _dedicated_node_url(app, handle, request))
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(
        1, timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        "enabling mail must mint the default credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(
        timeout=app.mail_settings.ENABLE_SETTLE_S
    ), (
        f"mail must report enabled; status={app.mail_settings.status_text()!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()
    admin_addr = _alias_admin_to_address(nest, domain)  # admin@<domain>

    base = f"https://127.0.0.1:{handle.caldav_port}"
    client = CardDAVClient(base, admin_addr, _PASSWORD, verify=False)
    client.wait_until_serving()

    # The lazy "Contacts" book is auto-provisioned on the first PROPFIND of an empty
    # home set (backend.go lazyProvisionContacts); the Fauna read seam never
    # provisions (slice 4b is read-only), so seeding a card here is what makes the
    # book exist for `list_addressbooks` to return.
    book = client.contacts_addressbook()
    assert book, "the lazy Contacts book must exist on first PROPFIND"

    uid = f"urn:uuid:{uuid.uuid4()}"
    fn = f"Ada Lovelace {int(time.time())}"
    tel = "+15550142"
    email = "ada@example.com"
    vcf = build_vcard(
        uid, fn, email=email, tel=tel, org="Analytical Engine", note="first programmer"
    )
    assert client.put_card(book, uid, vcf), "seed PUT must return an ETag"

    deadline = time.monotonic() + _SEED_TIMEOUT
    while time.monotonic() < deadline:
        if uid in client.uids(book):
            break
        time.sleep(1.0)
    assert uid in client.uids(book), (
        f"seeded card {uid!r} never surfaced to the MDA read path within "
        f"{_SEED_TIMEOUT:.0f}s"
    )
    return fn, tel, email, client, book, uid


@pytest.mark.feature("address-book")
def test_address_book_lists_and_details_mda_sealed_card(app, dedicated_mail_nest, request):
    """The Contacts → Address Book segment lists an MDA-sealed vCard, and opening it
    shows FN / TEL / EMAIL — proving the client unseals what the MDA wrote."""
    app.contacts.require_address_book_supported()

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    fn, tel, email, _client, _book, _uid = _seed_mda_sealed_card(app, handle, request)

    # Drive the Fauna app UI: Contacts → Address Book segment.
    app.contacts.navigate()
    app.contacts.switch_to_address_book()

    assert app.contacts.wait_for_addressbook(), (
        "the Address Book segment must list the MDA-provisioned Contacts book: "
        f"{app.driver.diagnose('addressbook-item')} error={app.error_text()!r}"
    )
    app.contacts.select_addressbook(0)

    assert app.contacts.wait_for_card(fn), (
        f"the seeded card {fn!r} must render in the card list (decrypt-on-read): "
        f"{app.driver.diagnose('vcard-card-fn')} names={app.contacts.card_names()!r} "
        f"error={app.error_text()!r}"
    )

    # Open the detail pane — FN / TEL / EMAIL must all surface (the client unsealed +
    # parsed the vCard body the MDA sealed on the scripted PUT).
    app.contacts.open_card_by_name(fn)
    app.driver.wait_for("vcard-detail-fn", timeout=10.0)
    assert fn in app.contacts.detail_fn(), (
        f"detail FN must show {fn!r}; got {app.contacts.detail_fn()!r}"
    )
    assert any(tel in t for t in app.contacts.detail_tels()), (
        f"detail must show the TEL {tel!r}; got {app.contacts.detail_tels()!r}"
    )
    assert any(email in e for e in app.contacts.detail_emails()), (
        f"detail must show the EMAIL {email!r}; got {app.contacts.detail_emails()!r}"
    )


@pytest.mark.feature("address-book")
def test_a_card_added_from_another_app_appears_while_on_the_address_book(
    app, dedicated_mail_nest, request
):
    """A card another contacts app PUTs appears in the open book's list while the
    user stays on the Address Book — no navigation, no re-selecting the book.

    The carddav twin of `test_caldav_external_appears.py`. The nest emits
    `fauna.addressbook.changed` to the book owner's devices on every durable card
    write (`transport.md` § Push events); the shared classifier folds it to
    `StaleSurfaces::address_book`, and the page re-reads the books and the open
    book's cards. The Address Book has no poll on any app, so a page that grows
    the second card here re-read off the push: without the wiring it never
    converges at any timeout.
    """
    app.contacts.require_address_book_supported()
    app.contacts.require_address_book_live_refresh()

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    fn, _tel, _email, client, book, _uid = _seed_mda_sealed_card(app, handle, request)

    # Open the book on the seeded card. This is the LAST navigation or selection
    # the test performs — everything after reads the live page.
    app.contacts.navigate()
    app.contacts.switch_to_address_book()
    assert app.contacts.wait_for_addressbook(), (
        "the Address Book segment must list the MDA-provisioned Contacts book: "
        f"{app.driver.diagnose('addressbook-item')} error={app.error_text()!r}"
    )
    app.contacts.select_addressbook(0)
    assert app.contacts.wait_for_card(fn), (
        f"the seeded card {fn!r} must render before the second write: "
        f"names={app.contacts.card_names()!r} error={app.error_text()!r}"
    )

    # Another contacts app adds a second card to the same book.
    uid2 = f"urn:uuid:{uuid.uuid4()}"
    fn2 = f"Grace Hopper {uuid.uuid4().hex[:8]}"
    assert client.put_card(book, uid2, build_vcard(uid2, fn2, email="grace@example.com")), (
        "the second PUT must return an ETag"
    )

    assert app.contacts.wait_for_card(fn2, timeout=budgets.PUSH_REFRESH_S), (
        f"a card another app added ({fn2!r}) never appeared in the open book within "
        f"{budgets.PUSH_REFRESH_S:.0f}s while the page stayed put: "
        f"names={app.contacts.card_names()!r} error={app.error_text()!r}. The "
        "Address Book has no poll, so either the fauna.addressbook.changed push "
        "never reached the page or the page does not re-read the open book on it."
    )
    assert any(fn in n for n in app.contacts.card_names()), (
        f"the refresh must keep the book's existing card {fn!r}; "
        f"names={app.contacts.card_names()!r}"
    )
