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

from helpers.admin_wire import (
    admin_user,
    catch_all_actor_for,
    dmarc_policy_for,
    dns_matrix_for,
    dns_verdicts_for,
    picker_option,
    role_overrides_for,
    seed_local_domain,
)
from helpers.app_surface import app_name, skip_unbuilt
from helpers.budgets import PROVIDER_VERIFY_S, RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.dns_recheck import require_dns_background_recheck
from helpers.waiting import wait_until
from i18n.strings import S

# The app's own record of each DNS-page read (`fauna_client_dns`'s
# `MATRIX_LOAD_LOG_LINE` / `RECHECK_LOG_LINE` / `BACKGROUND_RECHECK_LOG_LINE`).
MATRIX_LOAD_LINE = "loaded the DNS record matrix:"
RECHECK_LINE = "re-checked DNS records against public DNS:"
BACKGROUND_LINE = "background re-check of the DNS page"

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

    # --- Remove (soft-delete) --- the row's remove control, through the confirm
    # it opens (mail-multidomain.md § Removing a local domain).
    admin_app.admin.remove_domain(domain)
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

    # The e2e clock compresses the page's background re-check to seconds
    # (``FAUNA_DNS_RECHECK_SECS``), and every re-check dispatch clears the
    # snapshot's prior error — so a verify failure that lands just before a tick
    # can be gone before the first poll reads it (measured on tui: rejection
    # logged, re-check 181 ms later, error never observable). Production's
    # minutes-long cadence has no such window. Re-submit within the same budget
    # until one failure's error is read: each attempt draws a fresh phase against
    # the tick, so the assertion no longer depends on one lucky window
    # (convention 14).
    attempts = 4
    err = None
    for attempt in range(attempts):
        try:
            err = wait_until(_err_text, PROVIDER_VERIFY_S / attempts)
            break
        except AssertionError:
            if attempt == attempts - 1:
                raise AssertionError(
                    f"no admin-dns error-message text appeared after {attempts} "
                    "add-credential submits (each failure's error was cleared by a "
                    "re-check before it could be read, or never surfaced)"
                ) from None
            admin_app.admin.add_dns_credential(
                "cloudflare", {"api-token": "bogus-token-deadbeef"}
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
    ``test_primary_domain_rename.py``; the start→abort round-trip runs in
    ``test_admin_dns_domain_lifecycle.py`` on a dedicated nest, because it must not
    share a nest with assertions about the primary."""
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


# ── The record half of the page, read back row by row (tui leads; the
#    cross-app lift is a separate row). ──

# The label the page paints for each wire verdict — the shared
# `fauna_core::format::dns_verdict_label` keys. A mismatch carries what public
# DNS served after the word, so only its start is fixed.
_VERDICT_LABEL = {
    "ok": S.admin.dns.status_ok,
    "missing": S.admin.dns.status_missing,
    "mismatch": S.admin.dns.status_mismatch,
    "checking": S.admin.dns.status_checking,
}


def _label_matches(label: str, wire_status: str) -> bool:
    expected = _VERDICT_LABEL.get(wire_status, S.admin.dns.status_checking)
    if wire_status == "mismatch":
        return label.startswith(expected)
    return label == expected


def _open_domain(admin_app, nest_instance, domain: str) -> int:
    """Seed ``domain``, open admin-dns, and return the domain's section index."""
    seed_local_domain(nest_instance, domain)
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: domain in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"seeded domain {domain!r} not rendered on admin-dns; got "
            f"{admin_app.admin.dns_domain_names()!r}"
        ),
    )
    return admin_app.admin.dns_domain_names().index(domain)


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_lists_every_record_with_type_value_and_live_status(
    admin_app, nest_instance
):
    """Every record a hosted domain needs is listed under that domain, each with
    its type, the exact value to publish and whether public DNS serves it
    (``dns-management.md`` § Manual + live verification):

    1. The rows under the domain's own section are exactly the nest's matrix
       for it (``fauna.dns.list_records``) — the same (name, type, value)
       triples, none missing, none extra, paired by the row and not by position
       across the page.
    2. Each row's status reads the nest's live verdict for that record
       (``fauna.dns.verify_records``) — live, missing, wrong value, or still
       checking — in the words the page uses for it.
    3. No public DNS serves a ``.test`` name, so the records are not live — and
       that never stops the admin adding another domain from the same page.
    """
    domain = f"rows-{secrets.token_hex(4)}.test"
    idx = _open_domain(admin_app, nest_instance, domain)
    matrix = dns_matrix_for(nest_instance, domain)
    want = sorted((r["name"], r["record_type"], r["expected"]) for r in matrix)
    assert want, f"the nest lists no records for {domain!r}"

    def rows_match_the_nest():
        idx_now = admin_app.admin.dns_domain_names().index(domain)
        rows = admin_app.admin.dns_records(idx_now)
        got = sorted((r["name"], r["type"], r["value"]) for r in rows)
        if got != want:
            return None
        verdicts = dns_verdicts_for(nest_instance, domain)
        if all(
            _label_matches(r["status"], verdicts.get((r["name"], r["type"]), "checking"))
            for r in rows
        ):
            return rows
        return None

    rows = wait_until(
        rows_match_the_nest,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"admin-dns rows under {domain!r} never matched the nest:\n"
            f"  page: {admin_app.admin.dns_records(idx)!r}\n"
            f"  nest matrix: {want!r}\n"
            f"  nest verdicts: {dns_verdicts_for(nest_instance, domain)!r}"
        ),
    )
    assert {r["type"] for r in rows} >= {"MX", "TXT"}, (
        f"a hosted domain's matrix carries at least its MX and TXT records: {rows!r}"
    )

    # --- 3. a record that is not live never blocks the admin ---
    assert any(r["status"] != S.admin.dns.status_ok for r in rows), (
        f"no public DNS serves {domain!r}, so some record must read not-live: {rows!r}"
    )
    another = f"next-{secrets.token_hex(4)}.test"
    admin_app.admin.add_domain(another)
    wait_until(
        lambda: another in admin_app.admin.dns_domain_names() or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"adding {another!r} while {domain!r}'s records are not live must "
            f"succeed; rows {admin_app.admin.dns_domain_names()!r} (error: "
            f"{admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
        ),
    )


#: The owner name a DNS-01 issuance publishes its one-shot TXT under
#: (RFC 8555 § 8.4) — transient by design, never a steady-state record.
ACME_CHALLENGE_LABEL = "_acme-challenge."


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_never_lists_the_issuance_challenge_record_as_a_standing_record(
    admin_app, nest_instance
):
    """The temporary TXT a certificate issuance publishes is not a record the
    domain needs between issuances, so the records page never lists it — and so
    never shows it as missing (``tls-certificates.md`` § B. Trusted-cert
    acquisition: the transient ``_acme-challenge`` TXT is deliberately not a
    steady-state ``list_records`` row; its absence between renewals is correct).

    Read once every row the nest lists for a freshly added domain has rendered,
    across every domain section on the page — the primary one included, which
    is the one an issuance orders for — and checked against the nest's own
    matrix too (convention 5). No issuance runs in this environment (no CA),
    so this is the steady state between issuances; the paste card a manual
    issuance shows (``admin-dns-cert-*``) is where the TXT appears while one is
    in flight, and is not a records-page row.
    """
    domain = f"acme-{secrets.token_hex(4)}.test"
    idx = _open_domain(admin_app, nest_instance, domain)
    want = sorted(
        (r["name"], r["record_type"], r["expected"])
        for r in dns_matrix_for(nest_instance, domain)
    )

    def all_rendered():
        idx_now = admin_app.admin.dns_domain_names().index(domain)
        rows = admin_app.admin.dns_records(idx_now)
        got = sorted((r["name"], r["type"], r["value"]) for r in rows)
        return rows if got == want else None

    wait_until(
        all_rendered,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"admin-dns rows under {domain!r} never matched the nest:\n"
            f"  page: {admin_app.admin.dns_records(idx)!r}\n  nest matrix: {want!r}"
        ),
    )

    names = admin_app.admin.dns_domain_names()
    for i, section in enumerate(names):
        challenge = [
            r for r in admin_app.admin.dns_records(i)
            if r["name"].startswith(ACME_CHALLENGE_LABEL)
        ]
        assert not challenge, (
            f"the records page lists an issuance challenge record under {section!r} "
            f"as a standing record (status shown: "
            f"{[r['status'] for r in challenge]!r}): {challenge!r}"
        )
        nest_rows = [
            r for r in dns_matrix_for(nest_instance, section)
            if r["name"].startswith(ACME_CHALLENGE_LABEL)
        ]
        assert not nest_rows, (
            f"fauna.dns.list_records carries an issuance challenge record for "
            f"{section!r}: {nest_rows!r}"
        )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_record_value_copies_to_the_clipboard(admin_app, nest_instance):
    """Each record's copy button puts that record's exact value on the
    clipboard, ready to paste at a DNS provider — checked on the first and the
    last record of the domain, so the button copies its own row's value. Polls
    for the value itself, so the first copy left on the clipboard can never
    pass for the second."""
    domain = f"copy-{secrets.token_hex(4)}.test"
    _open_domain(admin_app, nest_instance, domain)
    want = {r["expected"] for r in dns_matrix_for(nest_instance, domain)}

    def all_rendered():
        rows = admin_app.admin.dns_records(admin_app.admin.dns_domain_names().index(domain))
        return rows if {r["value"] for r in rows} == want else None

    rows = wait_until(
        all_rendered,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the records of {domain!r} never all rendered",
    )
    first, last = 0, len(rows) - 1
    assert rows[first]["value"] != rows[last]["value"], f"two distinct values to copy: {rows!r}"
    reads_clipboard = admin_app.driver.is_tui() or admin_app.driver.is_windows()
    for i in (first, last):
        scope = f"admin-dns-domain[{admin_app.admin.dns_domain_names().index(domain)}]"
        admin_app.admin.copy_dns_record(
            admin_app.admin.dns_domain_names().index(domain), i
        )

        def copied(i=i, scope=scope):
            if reads_clipboard:
                return admin_app.driver.get_clipboard_text()
            return admin_app.driver.get_attr(
                "admin-dns-record-copy-button", "copied", i, scope=scope
            )

        wait_until(
            lambda i=i, copied=copied: copied() == rows[i]["value"] or None,
            UI_SETTLE_S,
            diagnose=lambda i=i, copied=copied: (
                f"copying {rows[i]['type']} {rows[i]['name']} left {copied()!r}, "
                f"not its value {rows[i]['value']!r}"
            ),
        )


def _count(admin_app, line: str) -> int:
    return sum(1 for ln in admin_app.driver.app_stderr_text().splitlines() if line in ln)


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_rechecks_on_demand_and_by_itself_while_open(admin_app, nest_instance):
    """The page re-checks every record against public DNS when the admin asks
    and, while it stays open, by itself on a slow cadence
    (``dns-management.md`` § Manual + live verification). The verdicts of a
    ``.test`` name never change, so the witness is the app's own record of each
    re-check (``fauna_client_dns``'s log lines):

    1. On demand: the re-check button reloads the matrix (a load that is not a
       background tick's) and verifies it.
    2. By itself: with no gesture at all, background ticks re-read and
       re-verify the page — each tick names itself before its load.

    The e2e launch compresses the cadence (``FAUNA_DNS_RECHECK_SECS``); the
    journey polls for the lines, never a fixed delay.
    """
    if admin_app.driver.log_scope_across_relaunch() == "none":
        skip_unbuilt(
            admin_app.driver,
            surface="app-log reader in the e2e driver",
            detail="a re-check is witnessed by the app's own log line",
        )
    domain = f"recheck-{secrets.token_hex(4)}.test"
    _open_domain(admin_app, nest_instance, domain)

    # --- 1. on demand ---
    def foreground_loads() -> int:
        return _count(admin_app, MATRIX_LOAD_LINE) - _count(admin_app, BACKGROUND_LINE)

    loads, checks = foreground_loads(), _count(admin_app, RECHECK_LINE)
    admin_app.admin.refresh_dns()
    wait_until(
        lambda: (
            foreground_loads() > loads and _count(admin_app, RECHECK_LINE) > checks
        )
        or None,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"the re-check button never reloaded and re-verified the records "
            f"(foreground loads {loads} → {foreground_loads()}, re-checks {checks} → "
            f"{_count(admin_app, RECHECK_LINE)})"
        ),
    )

    # --- 2. by itself ---
    require_dns_background_recheck(admin_app.driver)
    ticks, checks = _count(admin_app, BACKGROUND_LINE), _count(admin_app, RECHECK_LINE)
    wait_until(
        lambda: (
            _count(admin_app, BACKGROUND_LINE) >= ticks + 2
            and _count(admin_app, RECHECK_LINE) >= checks + 2
        )
        or None,
        PROVIDER_VERIFY_S,
        diagnose=lambda: (
            f"the open page did not re-check by itself (background ticks {ticks} → "
            f"{_count(admin_app, BACKGROUND_LINE)}, re-checks {checks} → "
            f"{_count(admin_app, RECHECK_LINE)})"
        ),
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_dns_row_shows_where_catch_all_and_role_addresses_go(
    admin_app, nest_instance
):
    """A domain's row shows who its catch-all and each role address currently
    go to (``mail-multidomain.md`` § Per-domain role-address routing): the
    admin chooses on the row, the page is opened afresh — so every value is
    read back from the nest — and the row names the chosen member, while the
    roles left alone still read the admin default."""
    domain = f"routes-{secrets.token_hex(4)}.test"
    _open_domain(admin_app, nest_instance, domain)
    option = picker_option(admin_user(nest_instance))
    idx = admin_app.admin.dns_domain_names().index(domain)
    admin_app.admin.set_catch_all(option, index=idx)
    admin_app.admin.set_role_address("abuse", option, index=idx)
    wait_until(
        lambda: role_overrides_for(nest_instance, domain).get("abuse")
        and catch_all_actor_for(nest_instance, domain)
        or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the choices on {domain!r}'s row never reached the nest",
    )

    # Open the page afresh: nothing below is the picker remembering a click.
    admin_app.driver.set_state({"nav": {"stack": [{"view": "admin"}]}})
    admin_app.admin.navigate_dns()

    def row_reads_back():
        names = admin_app.admin.dns_domain_names()
        if domain not in names:
            return None
        i = names.index(domain)
        shown = {
            "catch-all": admin_app.admin.catch_all_selected(index=i),
            "abuse": admin_app.driver.get_text(
                "admin-dns-domain-role-address-abuse-select", index=i
            ),
            "postmaster": admin_app.driver.get_text(
                "admin-dns-domain-role-address-postmaster-select", index=i
            ),
        }
        ok = (
            shown["catch-all"] == option
            and shown["abuse"] == option
            and shown["postmaster"] == S.admin.dns.role_address_admin_default
        )
        return shown if ok else None

    wait_until(
        row_reads_back,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{domain!r}'s row does not show where its mail goes: expected the "
            f"catch-all and abuse@ on {option!r}, postmaster@ on the admin default"
        ),
    )


@pytest.mark.feature("admin-dns-and-certificates")
def test_admin_softens_one_domains_dmarc_policy_from_its_row(admin_app, nest_instance):
    """An admin softens one domain's DMARC policy from its row on the Domains
    page, and sets it back (``dmarc-reporting.md`` § Multi-domain deployments):

    1. A freshly added domain's picker reads the default, ``reject``.
    2. Choosing ``quarantine`` reaches the nest — the stored override holds it
       and the domain's published ``_dmarc`` record carries ``p=quarantine;
       sp=quarantine`` — while another domain's record stays ``reject``.
    3. The page opened afresh still shows ``quarantine`` on that row: the value
       is read back from the nest, not the picker remembering a click.
    4. Choosing ``reject`` again clears the override rather than storing it.
    """
    soft =f"dmarc-soft-{secrets.token_hex(4)}.test"
    strict = f"dmarc-strict-{secrets.token_hex(4)}.test"
    seed_local_domain(nest_instance, strict)
    _open_domain(admin_app, nest_instance, soft)

    def picker_for(domain):
        names = admin_app.admin.dns_domain_names()
        if domain not in names:
            return None
        return admin_app.admin.dmarc_policy_selected(index=names.index(domain))

    wait_until(
        lambda: picker_for(soft) == "reject" or None,
        UI_SETTLE_S,
        diagnose=lambda: (
            f"a fresh domain's DMARC picker should read the default 'reject'; "
            f"got {picker_for(soft)!r}"
        ),
    )

    admin_app.admin.set_dmarc_policy(
        "quarantine", index=admin_app.admin.dns_domain_names().index(soft)
    )
    softened = {"stored": "quarantine", "published": ("quarantine", "quarantine")}
    wait_until(
        lambda: dmarc_policy_for(nest_instance, soft) == softened or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"choosing 'quarantine' on {soft!r}'s row never reached the nest: "
            f"{dmarc_policy_for(nest_instance, soft)!r}; page error: "
            f"{admin_app.driver.get_text('error-message')!r}"
        ),
    )
    assert dmarc_policy_for(nest_instance, strict) == {
        "stored": None,
        "published": ("reject", "reject"),
    }, f"softening {soft!r} changed another domain's DMARC policy"

    # Open the page afresh: what the row shows now is read back from the nest.
    admin_app.driver.set_state({"nav": {"stack": [{"view": "admin"}]}})
    admin_app.admin.navigate_dns()
    wait_until(
        lambda: picker_for(soft) == "quarantine" or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{soft!r}'s row does not show its softened policy after reopening the "
            f"page; the picker reads {picker_for(soft)!r}"
        ),
    )

    admin_app.admin.set_dmarc_policy(
        "reject", index=admin_app.admin.dns_domain_names().index(soft)
    )
    restored = {"stored": None, "published": ("reject", "reject")}
    wait_until(
        lambda: dmarc_policy_for(nest_instance, soft) == restored or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"choosing 'reject' on {soft!r}'s row did not clear the override: "
            f"{dmarc_policy_for(nest_instance, soft)!r}"
        ),
    )
