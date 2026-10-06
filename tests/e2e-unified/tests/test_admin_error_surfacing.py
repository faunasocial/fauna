"""tier_3 e2e: the admin mail read-pages surface a failed fetch's error on
their shared ``error-message`` element.

``admin-bridges-pending`` and ``admin-dns`` each render their shared machine's
``snapshot.error`` onto the page ``error-message`` label — the same per-page
convention as the shipped ``mail-settings`` page
(``apps/fauna-linux/src/settings/mail.rs``), and the contract the snapshot types
document ("Last action's error, surfaced via the ``error-message`` element":
``DnsSnapshot.error`` / ``LocalDomainsSnapshot.error`` /
``BridgeApprovalSnapshot.error``). The ``admin-dns`` page now drives BOTH the
DNS matrix (``fauna.dns.list_records``) and the local-domains list
(``fauna.bridges.list_local_domains``) — domain management moved here from the
former ``admin-settings`` email-domains section (2026-05-25) — so its
``error-message`` covers the local-domains rejection that settings used to.

To force a *deterministic* nest error without a fake seam or a disconnect, we
navigate as a **non-admin**. Every backing fetch is Admin-gated —
``fauna.bridges.{list_local_domains,list_pending_bridges}`` and
``fauna.dns.list_records`` all ``require_admin`` on the nest — so the regular
user's ``Refresh`` is rejected, the shared machine records the rejection into
``snapshot.error``, and the page must show it. This works on every app because
the ``am-i-admin`` gate is entry-level and never content-level
(``docs/goal/behavior/admin.md`` § Entry-level, never content-level): the admin
shell's content is built regardless of ``am_i_admin`` — only the nav *entry* is
gated, and the nav-triggered admin fetches are not ``is_admin``-guarded — so a
non-admin can be force-navigated via the state protocol and the fetches fire.
Web was the lone deviation until 2026-09-09, when its ``admin/+layout.svelte``
stopped redirecting a non-admin out of the shell; that redirect is why both
params of this test were red on web from 2026-08-16.

We query the ``error-message`` element directly (not ``app.has_error()``, which
prefers the ``messages.error`` *state* field — i.e. the global banner — over the
per-page label this slice wires). The driver filters by visibility, so only the
visible sub-page's label is read; nav clears the global banner first, so the only
``error-message`` that can become visible here is the page's own.
"""

import time

import pytest

pytestmark = [pytest.mark.tier_3]


# (nav method on AdminActions, human label for failure messages)
_PAGES = [
    ("navigate_bridges_pending", "admin-bridges-pending"),
    ("navigate_dns", "admin-dns (records + local-domains)"),
]


@pytest.mark.parametrize("nav_method,page_label", _PAGES, ids=[p[1] for p in _PAGES])
@pytest.mark.feature("admin-dashboard")
def test_admin_mail_page_surfaces_fetch_error(logged_in_app, nav_method, page_label):
    """A non-admin force-navigated to an admin mail page sees the Admin-gated
    fetch rejection rendered on the page's ``error-message`` label."""
    app = logged_in_app
    getattr(app.admin, nav_method)()

    # The fetch is an async WS-RPC round-trip; poll for the per-page label to
    # become visible with non-empty text.
    deadline = time.time() + 12.0
    text = ""
    while time.time() < deadline:
        if app.driver.is_visible("error-message"):
            text = app.driver.get_text("error-message") or ""
            if text.strip():
                break
        time.sleep(0.5)

    assert app.driver.is_visible("error-message"), (
        f"{page_label}: the Admin-gated fetch rejection did not surface on "
        f"error-message (label stayed hidden)"
    )
    assert text.strip(), (
        f"{page_label}: error-message is visible but empty (expected the "
        f"rejection text)"
    )
