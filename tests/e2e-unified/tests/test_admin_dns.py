"""tier_3 e2e: the admin unified-DNS page (`admin-dns`) renders the per-domain
DNS record matrix every domain the deployment hosts needs — each record's name /
type / exact expected value (`admin-dns-domain` / `admin-dns-domain-name` /
`admin-dns-record` / `-name` / `-type` / `-value` / `-status` / `-provider-note`,
the last PTR-row-only).

This is the client-side counterpart to ``tests/api/`` DNS wire coverage and
``test_mail_admin_local_domains.py``: a local mail domain is seeded over the
admin WS-RPC control plane, then the client's admin DNS page must surface that
domain's required records through the shared ``DnsManagementMachine``
(``libs/fauna-client-dns``) over ``fauna.dns.list_records`` + ``verify_records``.

The live red/green verdict (``admin-dns-record-status``) will be ``checking`` /
``missing`` in the test env (no public DNS serves these synthetic domains), so we
assert the record **value** renders for the seeded domain, not a green verdict —
per ``docs/goal/behavior/dns-management.md`` § Manual + live verification.

``nest_instance`` is session-scoped (and the driver is cached across tests), so
we seed a unique domain and assert *that* domain's records are present rather
than asserting an absolute count.
"""

import secrets
import time

import pytest

from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3]

_DOMAIN = f"admindns-{secrets.token_hex(4)}.test"


def _seed_local_domain(nest_instance, domain: str) -> None:
    """Add a local domain over the admin WS-RPC control plane — the same surface
    ``fauna.dns.list_records`` enumerates records for. Mirrors
    ``test_admin_email_domains.py::_seed_local_domain``."""
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
            {
                "domain": domain,
                "mta_sts_cert_mode": "expand_primary",
            },
        )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_lists_domain_records(admin_app, nest_instance):
    """A domain added over WS-RPC shows up on the admin DNS page as an
    ``admin-dns-domain`` section whose ``admin-dns-record`` rows carry the exact
    expected values (MX/SPF/DKIM/DMARC/…) for that domain."""
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_dns()

    # Poll for the seeded domain's records to render (the fetch is an async
    # WS-RPC round-trip: list_records then verify_records).
    deadline = time.time() + 12.0
    matched_values: list[str] = []
    domain_names: list[str] = []
    while time.time() < deadline:
        domain_names = [
            admin_app.driver.get_text("admin-dns-domain-name", index=i) or ""
            for i in range(admin_app.driver.count("admin-dns-domain-name"))
        ]
        rec_names = [
            admin_app.driver.get_text("admin-dns-record-name", index=i) or ""
            for i in range(admin_app.driver.count("admin-dns-record-name"))
        ]
        rec_values = [
            admin_app.driver.get_text("admin-dns-record-value", index=i) or ""
            for i in range(admin_app.driver.count("admin-dns-record-value"))
        ]
        # Every record's name embeds the domain (bare-domain MX, `_dmarc.<d>`,
        # `<selector>._domainkey.<d>`, …). Match those and read the paired value.
        matched_values = [
            rec_values[i]
            for i, name in enumerate(rec_names)
            if _DOMAIN in name and i < len(rec_values)
        ]
        if any(domain_names) and _DOMAIN in " ".join(domain_names) and matched_values:
            break
        time.sleep(0.5)

    err = admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"
    assert _DOMAIN in " ".join(domain_names), (
        f"seeded domain {_DOMAIN!r} not found among admin-dns-domain-name "
        f"rows {domain_names!r} (error: {err})"
    )
    assert matched_values, (
        f"no admin-dns-record rows rendered for {_DOMAIN!r} (error: {err})"
    )
    # The TODO/goal-doc contract: the exact expected VALUE renders (a non-empty
    # zone-file RDATA string), not a green verdict — verify is missing/checking
    # in the test env where no public DNS serves these synthetic domains.
    assert any(v.strip() for v in matched_values), (
        f"admin-dns-record-value(s) for {_DOMAIN!r} all empty: {matched_values!r} "
        f"(error: {err})"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_ptr_row_shows_provider_note(admin_app, nest_instance):
    """The PTR record is the one row an admin cannot zone-publish — reverse DNS
    is set at the IP owner (the VPS/server provider), never at the registrar
    (`docs/goal/behavior/dns-management.md` § Records covered) — so it alone
    carries an advisory note the other record types don't.

    `list_records` only emits the host `A`/`AAAA`/`PTR` rows (on the *primary*
    domain) once `fauna.dns.set_host_address` has run (idempotent upsert;
    `dns_handlers.rs::assemble_domain_views` no-ops until then), so this test
    seeds it first.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest_instance["admin"]
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        client.call(
            "fauna.dns.set_host_address",
            {"nest_ipv4": "203.0.113.50", "mail_ipv4": "203.0.113.51"},
        )

    admin_app.admin.navigate_dns()
    admin_app.admin.refresh_dns()

    deadline = time.time() + 12.0
    rec_types: list[str] = []
    note_count = 0
    while time.time() < deadline:
        rec_types = [
            admin_app.driver.get_text("admin-dns-record-type", index=i) or ""
            for i in range(admin_app.driver.count("admin-dns-record-type"))
        ]
        note_count = admin_app.driver.count("admin-dns-record-provider-note")
        if "PTR" in rec_types and note_count >= 1:
            break
        time.sleep(0.5)

    err = admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"
    assert "PTR" in rec_types, (
        f"no PTR record rendered after set_host_address; record types seen: "
        f"{rec_types!r} (error: {err})"
    )
    # PTR-only contract: exactly one note, however many records are rendered.
    assert note_count == 1, (
        f"expected exactly ONE admin-dns-record-provider-note (the PTR row "
        f"only), got {note_count} across record types {rec_types!r} (error: {err})"
    )
    note_text = admin_app.driver.get_text("admin-dns-record-provider-note") or ""
    assert "provider" in note_text.lower(), (
        f"admin-dns-record-provider-note doesn't read as the provider advisory: "
        f"{note_text!r}"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_add_remove_restore_domain(admin_app, nest_instance):
    """The admin-dns page is the unified domain-management surface: add a domain
    from the page → it appears as an ``admin-dns-domain``; remove it → it moves
    to the ``admin-dns-removed-domain`` (soft-deleted) group; restore it → it
    returns to the active list.

    Drives the shared ``LocalDomainMachine`` over
    ``fauna.bridges.{add,remove,restore}_local_domain`` through the admin-dns UI
    (the former ``admin-settings`` email-domains section is removed — domain
    management moved here per ``dns-management.md`` § App surface, 2026-05-25).
    Unique domain + membership assertions (``nest_instance`` is session-scoped).
    """
    domain = f"crud-{secrets.token_hex(4)}.test"

    admin_app.admin.navigate_dns()

    # --- Add ---
    admin_app.admin.add_domain(domain)
    names = wait_until(
        lambda: admin_app.admin.dns_domain_names() if domain in admin_app.admin.dns_domain_names() else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"added domain {domain!r} not in active admin-dns-domain-name rows "
            f"{admin_app.admin.dns_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )

    # --- Remove (soft-delete) --- click the remove button on the added domain's
    # row. The remove-button is present on every active row (disabled on the
    # primary), so its index aligns with admin-dns-domain-name.
    idx = admin_app.admin.dns_domain_names().index(domain)
    admin_app.driver.click("admin-dns-domain-remove-button", index=idx)
    removed = wait_until(
        lambda: admin_app.admin.dns_removed_domain_names() if domain in admin_app.admin.dns_removed_domain_names() else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"removed domain {domain!r} not in admin-dns-removed-domain-name rows "
            f"{admin_app.admin.dns_removed_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )
    assert domain not in admin_app.admin.dns_domain_names(), (
        f"removed domain {domain!r} still in the active list"
    )

    # --- Restore --- click the restore button on the soft-deleted row.
    ridx = admin_app.admin.dns_removed_domain_names().index(domain)
    admin_app.driver.click("admin-dns-removed-domain-restore-button", index=ridx)
    wait_until(
        lambda: admin_app.admin.dns_domain_names() if domain in admin_app.admin.dns_domain_names() else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"restored domain {domain!r} not back in active admin-dns-domain-name rows "
            f"{admin_app.admin.dns_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_credentials_list_and_refresh(admin_app, nest_instance):
    """The managed-mode credential surface renders on admin-dns: the held
    DNS-provider credential list (`admin-dns-credentials-list`) is present (and
    empty — no real provider to verify+store one in the test env), and the
    `admin-dns-refresh-button` re-fetches the record matrix + credential store.

    Wires the shared ``DnsManagementMachine`` credentials variant
    (``build_dns_management_machine_with_credentials``): the page now reads
    ``DnsSnapshot.credentials`` (held creds) alongside the record matrix.
    Per ``dns-management.md`` § Where the credential lives, a credential can only
    enter ``fauna.state.dns`` via a successful ``PutCredentials.verify()`` against a
    real provider API — unavailable in tier_3 — so we assert the credential list
    *renders empty* + the refresh affordance works, not a populated list.
    """
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_dns()

    # The credentials section renders (machine wired to the credential store).
    wait_until(
        admin_app.admin.dns_credentials_list_visible,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "admin-dns-credentials-list not visible — page not wired to the "
            "credential-store machine (build_dns_management_machine_with_credentials)"
        ),
    )
    # No real DNS provider in the test env, so no credential can be stored.
    assert admin_app.admin.dns_credential_count() == 0, (
        f"expected an empty held-credential list, got "
        f"{admin_app.admin.dns_credential_providers()!r}"
    )

    # Refresh re-fetches: the seeded domain's records re-render without error.
    admin_app.admin.refresh_dns()
    wait_until(
        lambda: admin_app.admin.dns_domain_names() if _DOMAIN in admin_app.admin.dns_domain_names() else None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"after admin-dns-refresh-button, seeded domain {_DOMAIN!r} not rendered "
            f"among {admin_app.admin.dns_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_add_credential_shows_provider_error(admin_app, nest_instance):
    """The write-only add-credential form (`admin-dns-add-credential-*`) drives the
    shared ``DnsManagementMachine``'s ``PutCredentials`` — reveal the form, pick a
    provider, type a credential, submit → the machine verifies it against the real
    provider API and, on success, seals it into ``fauna.state.dns``.

    In tier_3 there is **no real Cloudflare account** behind the bogus token, so
    ``PutCredentials.verify()`` fails. The *class* of failure is path-dependent:
    a client that reaches the provider API directly (native — linux/windows/…)
    gets a real 4xx → ``DnsProviderError::Rejected`` ("dns provider rejected: …");
    the **web** client routes the call through the `proxy.fauna.social` CORS proxy
    (`fauna_provisioning::proxy`), and when that hop can't be reached from the e2e
    sandbox the failure is ``DnsProviderError::Transient`` ("dns provider
    unreachable: …"). Both are the same observable contract — and the docstring's
    own "may have no outbound network" caveat already anticipates the unreachable
    case. Per ``dns-management.md`` § The two modes ("a publish failure … surfaces
    as a per-record error … does not silently fall back") we assert the **error
    affordance** — the `error-message` element shows the provider-verify failure
    (either class, both prefixed "dns provider …") — and that the held-credential
    list stays **empty** (nothing is stored on a failed verify). This is gotcha #4
    of the linux mail-admin follow-up: assert the error affordance, not a green store.
    """
    admin_app.admin.navigate_dns()

    wait_until(
        admin_app.admin.dns_credentials_list_visible,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "admin-dns-credentials-list not visible — page not wired to the "
            "credential-store machine"
        ),
    )

    # Cloudflare has a single Secret field (`api-token`), so the form is minimal.
    admin_app.admin.add_dns_credential("cloudflare", {"api-token": "bogus-token-deadbeef"})

    # verify() fails (no real account / unreachable provider) → snapshot.error →
    # the page's error-message carries the provider-verify failure. We poll the
    # element's
    # *text* (what the admin reads) rather than has_error(): a verify-failure
    # error label is toggled visible after the page has gone idle, and headless
    # AT-SPI does not always re-mark a late-toggled label SHOWING — so is_visible
    # (and the has_error()/error_text() helpers built on it) can read false even
    # though the text is set. (The sibling test_admin_error_surfacing covers the
    # is_visible path, where the error arrives at nav-time.) The error label is
    # absent from the AT-SPI tree until shown, so guard get_text with count>0
    # (non-raising) — count>0 ⟺ the label was shown, which only happens on an
    # error. Generous timeout: a real provider HTTP round-trip (or its bounded
    # failure) is slower than an in-process dispatch.
    def _err_text():
        if admin_app.driver.count("error-message") == 0:
            return None
        try:
            return admin_app.driver.get_text("error-message") or None
        except Exception:
            return None

    err = wait_until(
        _err_text,
        PROVIDER_VERIFY_S,
        diagnose=lambda: "no admin-dns error-message text appeared after add-credential submit",
    )
    # Accept either failure class — a provider rejection (reachable provider,
    # bad token) or a transient/unreachable error (proxied web path, provider
    # hop not reachable from the sandbox). Both are surfaced verify failures and
    # both start "dns provider …"; what matters is that the failure reached the
    # error affordance (not a green store).
    assert err and "dns provider" in err.lower(), (
        "expected admin-dns-add-credential submit to surface a provider-verify "
        f"failure (rejected or unreachable) in error-message; got {err!r} (the "
        "form's PutCredentials dispatch must funnel verify() failures to "
        "DnsSnapshot.error)"
    )
    # Nothing is stored on a failed verify — the held-credential list stays empty.
    assert admin_app.admin.dns_credential_count() == 0, (
        f"a failed verify must not store a credential; held list shows "
        f"{admin_app.admin.dns_credential_providers()!r}"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_toggle_managed_without_credential_shows_error(admin_app, nest_instance):
    """The per-domain mode control (`admin-dns-domain-mode`) opts a domain in/out
    of Fauna-managed DNS via the shared ``DnsManagementMachine``'s ``SetMode``.

    Opting **in** requires a held DNS-provider credential whose zones cover the
    domain (``dns-management.md`` § The two modes / § Fauna-managed). In tier_3
    no credential can be stored (no real provider to verify one against — see
    ``test_admin_dns_add_credential_shows_provider_error``), so flipping a
    seeded domain to managed must be **rejected** with ``InvalidState`` and the
    rejection must surface in `error-message`; the domain stays manual (managed
    mode never silently falls back, and never silently succeeds without a
    covering credential). We assert the **error affordance**, mirroring the
    add-credential test's headless-AT-SPI handling (poll the element *text* via
    count>0 + get_text, not is_visible — a late-toggled error label is not
    reliably re-marked SHOWING headless; gotcha #4 of the linux mail-admin follow-up).
    """
    _seed_local_domain(nest_instance, _DOMAIN)

    admin_app.admin.navigate_dns()
    # Wait for the seeded domain's row (carrying admin-dns-domain-mode) to render.
    wait_until(
        lambda: _DOMAIN in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {_DOMAIN!r} not rendered on admin-dns; "
            f"got {admin_app.admin.dns_domain_names()!r}"
        ),
    )

    # Flip the first domain to Fauna-managed. No held credential covers it →
    # SetMode rejects with InvalidState → DnsSnapshot.error → error-message.
    admin_app.admin.toggle_domain_mode(index=0)

    def _err_text():
        if admin_app.driver.count("error-message") == 0:
            return None
        try:
            return admin_app.driver.get_text("error-message") or None
        except Exception:
            return None

    err = wait_until(
        _err_text,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "no admin-dns-domain-mode error-message appeared after toggle",
    )
    assert err and "credential" in err.lower() and "cover" in err.lower(), (
        "expected toggling a domain to Fauna-managed without a covering "
        "credential to surface a SetMode InvalidState rejection in "
        f"error-message; got {err!r} (the admin-dns-domain-mode toggle must "
        "dispatch SetMode and funnel its rejection to DnsSnapshot.error)"
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_rename_sheet_opens_and_cancels(admin_app, nest_instance):
    """The primary-domain-rename wizard wires end-to-end on the client
    (mail-primary-domain-rename.md § UX surface): once a non-primary domain
    exists, the primary row's "Rename primary" button opens the sheet, whose
    picker offers the promotion targets, and Cancel closes it.

    Side-effect-free by design: it never submits, so the shared session nest's
    primary is never flipped. The wire path for ``start`` is proven separately in
    ``test_primary_domain_rename.py``; the pre-flip start→abort round-trip is left
    to CI (it must run on a nest not shared with assertions about the primary)."""
    # Seed a unique non-primary domain so the two-step rule is satisfied and the
    # "Rename primary" affordance enables (snapshot.rename_available == true).
    rename_domain = f"rename-{secrets.token_hex(4)}.test"
    _seed_local_domain(nest_instance, rename_domain)

    admin_app.admin.navigate_dns()
    wait_until(
        lambda: rename_domain in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded non-primary domain {rename_domain!r} never rendered on admin-dns; "
            f"got {admin_app.admin.dns_domain_names()!r}"
        ),
    )

    # The primary row carries exactly one "Rename primary" button; opening it
    # reveals the sheet with a populated target picker.
    admin_app.admin.open_rename_sheet()
    wait_until(
        lambda: admin_app.admin.rename_sheet_open() or None,
        UI_SETTLE_S,
        diagnose=lambda: "clicking admin-dns-domain-rename-button never revealed admin-dns-rename-sheet",
    )
    assert admin_app.admin.rename_target_options() >= 1, (
        "the rename picker (admin-dns-rename-new-primary-select) must offer at "
        "least the seeded non-primary domain as a promotion target"
    )

    # Cancel closes the sheet without starting a rename (no banner appears).
    admin_app.admin.cancel_rename_sheet()
    wait_until(
        lambda: (not admin_app.admin.rename_sheet_open()) or None,
        UI_SETTLE_S,
        diagnose=lambda: "Cancel never closed admin-dns-rename-sheet",
    )
    assert not admin_app.admin.rename_sheet_open(), "Cancel must close the sheet"
    assert not admin_app.admin.rename_banner_present(), (
        "cancelling must not start a rename (no admin-dns-rename-banner)"
    )
