from __future__ import annotations

import time
from typing import TYPE_CHECKING

from drivers.http_bridge import SelectOptionNotOffered

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class AdminActions:
    """Actions for the admin dashboard and settings pages.

    Uses set_state for navigation with nested stack paths. Works on any
    client that implements admin UI (web, Linux, potentially others).
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate_dashboard(self) -> None:
        """Navigate to the admin dashboard and wait for stats to load.

        The stat-card values load asynchronously (admin-status check →
        fetch_admin_stats / fetch_admin_server_status → card update), so the
        heading appearing does NOT mean the data is in. Poll until the Users and
        Version cards have non-empty values before returning, so card reads don't
        race the fetch. (The card *value* markers start empty and only fill once
        the response lands — see attach_stat_markers in the linux app.)
        """
        self.driver.navigate_to("admin")
        self.driver.wait_for("admin-dashboard-heading", timeout=15.0)
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            if self.dashboard_card_value("Users") and self.dashboard_card_value("Version"):
                return
            time.sleep(0.3)
        raise AssertionError(
            "admin dashboard stat cards (Users/Version) did not populate "
            "within 15.0s: Users=%r, Version=%r; %s"
            % (
                self.dashboard_card_value("Users"),
                self.dashboard_card_value("Version"),
                self.driver.diagnose("admin-stat-card-value"),
            )
        )

    def navigate_settings(self) -> None:
        """Navigate to admin settings."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "settings"}]},
        })
        self.driver.wait_for("admin-settings-heading", timeout=10.0)

    def factory_reset_via_ui(self) -> str:
        """Drive `fauna.admin.factory_reset` through the admin Danger-zone button
        (T3) and return the pre-filled claim code.

        Clicks `admin-factory-reset-button`, confirms the destructive dialog
        (`admin-factory-reset-confirm-button`), then the client tears down the
        authenticated session and re-seeds onboarding at the claim-code step with
        the returned code pre-filled — the human never sees it. We wait for that
        page and read the pre-filled `claim-code-input`. The nest is restarting
        into the wipe at this point; the caller waits for it to come back fresh
        before submitting. Per `docs/goal/behavior/mail-bridge-lifecycle.md` §
        Factory reset.

        The Danger zone moved from admin-settings to admin-nest in the per-page-
        services redesign (2026-06-04, admin.md § N Nest); clients that haven't
        lifted admin-nest still render Factory Reset on admin-settings, so try
        admin-nest first and fall back. (Drop the fallback once all seven apps
        render admin-nest.)
        """
        self.navigate_nest()
        if self.driver.count("admin-factory-reset-button") == 0:
            self.navigate_settings()
        # The whole danger-zone GroupBox is `.disabled(vm.isBusy)` on apple, so
        # the button renders before it is clickable — visible != enabled.
        self.driver.wait_until_enabled("admin-factory-reset-button", timeout=15.0)
        self.driver.click("admin-factory-reset-button")
        self.driver.wait_for("admin-factory-reset-confirm-button", timeout=10.0)
        self.driver.click("admin-factory-reset-confirm-button")
        # The session is torn down and onboarding re-seeded at claim-code.
        self.driver.wait_for("claim-code-input", timeout=60.0)
        code = self.driver.get_text("claim-code-input")
        assert code, (
            "claim-code-input was not pre-filled after the UI factory reset — the "
            "returned code never reached the input (the human can't type it)."
        )
        return code

    def navigate_bridges_pending(self) -> None:
        """Navigate to the admin pending-bridge approval page.

        The admin sub-page id matches the GTK Stack child name the linux app
        registers (`main.rs` reads `nav.stack[1].id`). No `wait_for` on the
        shared `page-heading` (it can resolve to a hidden sibling page); callers
        poll for `admin-bridges-pending-card` instead, which also covers the
        async WS-RPC fetch the nav triggers.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-bridges-pending"}]},
        })
        time.sleep(1)

    def navigate_dns(self) -> None:
        """Navigate to the admin unified-DNS page (`admin-dns`).

        The admin sub-page id matches the GTK Stack child name the linux app
        registers (`main.rs` reads `nav.stack[1].id`). No `wait_for` on the
        shared `page-heading` (it can resolve to a hidden sibling page); callers
        poll for `admin-dns-record` / `admin-dns-domain-name` instead, which also
        covers the async WS-RPC fetch (list_records + verify_records) the nav
        triggers.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-dns"}]},
        })
        time.sleep(1)

    # --- Reaching a sub-page by clicks (the gesture path a human takes) ---

    def open_page_by_click(self, page_id: str, canary: str, timeout: float = 15.0) -> None:
        """Reach the admin sub-page ``page_id`` the way a human does: the gated
        ``admin-tab``, then that page's rail row ``admin-nav-row[<page_id>]``
        (ui.yaml ``navigation.sub_page_nav_rows``; ``admin.md`` § Navigation
        model), then wait for ``canary`` on the page.

        The tab is clicked only when the row is not already on screen — the rail
        is painted on every admin page, so a caller already inside the shell
        goes straight to the row. On mobile ``admin-tab`` is the in-Settings
        entry (``navigation.gated_tabs``), so ``settings-tab`` opens first.

        This is the path a test bound by the gesture-only rule uses (the live
        tier_4 tests, convention 8); ``navigate_dns`` & co. stay the fast,
        cross-app nav-patch path for everything else.
        """
        row = f"admin-nav-row[{page_id}]"
        if not self.driver.is_visible(row):
            if self.driver.is_mobile():
                self.driver.click("settings-tab")
            self.driver.wait_for("admin-tab", timeout=timeout)
            self.driver.click("admin-tab")
            self.driver.wait_for(row, timeout=timeout)
        self.driver.click(row)
        self.driver.wait_for(canary, timeout=timeout)

    def open_dns_by_click(self) -> None:
        """``admin-dns`` by clicks (``open_page_by_click``); the always-present
        add-domain button is the canary — the record rows load asynchronously,
        so callers still poll for those."""
        self.open_page_by_click("admin-dns", "admin-dns-add-domain-button")

    def open_bridges_pending_by_click(self) -> None:
        """``admin-bridges-pending`` by clicks (``open_page_by_click``); the
        unconditional "Pending approval" section heading is the canary."""
        self.open_page_by_click("admin-bridges-pending", "admin-bridges-pending-section")

    # --- Domain CRUD on admin-dns (dns-management.md § App surface) ---

    def add_domain(self, domain: str) -> None:
        """Add a local mail domain from the admin-dns page: reveal the add form,
        type the domain, submit. The dispatch is an async WS-RPC round-trip
        (`fauna.bridges.add_local_domain` then a `Refresh`); callers poll
        `admin-dns-domain-name` for the new domain rather than asserting here."""
        self.driver.click("admin-dns-add-domain-button")
        # The click reveals the form (convention 14: wait_for the field, not a
        # sleep — the finding generalizes here).
        self.driver.wait_for("admin-dns-add-domain-input", timeout=10.0)
        self.driver.type_text("admin-dns-add-domain-input", domain)
        self.driver.click("admin-dns-add-domain-submit-button")
        time.sleep(1)

    def dns_domain_names(self) -> list[str]:
        """All active-domain names rendered on admin-dns (`admin-dns-domain-name`)."""
        return [t or "" for t in self.driver.get_texts("admin-dns-domain-name")]

    def dns_removed_domain_names(self) -> list[str]:
        """All soft-deleted domain names on admin-dns (`admin-dns-removed-domain-name`)."""
        return [t or "" for t in self.driver.get_texts("admin-dns-removed-domain-name")]

    def remove_domain(self, domain: str) -> None:
        """Soft-delete the local mail domain named `domain` from admin-dns.

        Resolves the domain's row by name: the per-row
        `admin-dns-domain-remove-button` index aligns with `admin-dns-domain-name`
        because the client renders a remove button on EVERY row (just insensitive
        on the primary — `apps/fauna-linux/src/views/admin.rs`, "present on every
        row so the e2e index aligns"). A single click dispatches
        `fauna.bridges.remove_local_domain` (no confirm dialog; soft-delete with a
        30-day restore window). Async round-trip — poll `dns_domain_names()` /
        `dns_removed_domain_names()` for the result rather than asserting here."""
        idx = self.dns_domain_names().index(domain)  # ValueError if absent — fail loud
        self.driver.click("admin-dns-domain-remove-button", index=idx)
        time.sleep(1)

    # --- Primary-domain rename on admin-dns (mail-primary-domain-rename.md § UX) ---

    def open_rename_sheet(self) -> None:
        """Open the start-a-rename wizard from the primary row's "Rename primary"
        button (`admin-dns-domain-rename-button`; present only on the primary,
        enabled once a non-primary domain exists — the two-step rule)."""
        self.driver.click("admin-dns-domain-rename-button")
        time.sleep(0.5)

    def rename_sheet_open(self) -> bool:
        """Whether the rename wizard sheet is showing (`admin-dns-rename-sheet`)."""
        return not self.driver.is_absent("admin-dns-rename-sheet")

    def rename_target_options(self) -> int:
        """How many promotion-target options the rename picker offers
        (`admin-dns-rename-new-primary-select` — the active non-primary domains)."""
        return self.driver.count("admin-dns-rename-new-primary-select")

    def cancel_rename_sheet(self) -> None:
        """Close the rename wizard without starting a rename."""
        self.driver.click("admin-dns-rename-cancel-button")
        time.sleep(0.3)

    def rename_banner_present(self) -> bool:
        """Whether the in-flight rename banner is showing (`admin-dns-rename-banner`)."""
        return self.driver.count("admin-dns-rename-banner") > 0

    def submit_rename(self) -> None:
        """Start the rename from the open sheet (`admin-dns-rename-submit-button`).
        The target defaults to the picker's first option; the dispatch is an async
        WS-RPC round-trip. Callers poll `admin-dns-rename-banner`."""
        self.driver.click("admin-dns-rename-submit-button")
        time.sleep(1)

    def abort_rename(self) -> None:
        """Abort the in-flight rename via the banner's reveal-then-confirm pair
        (`admin-dns-rename-abort-button` → `-abort-confirm-button`). Pre-flip abort
        is a clean state change (nothing to unwind), so this never strands the nest."""
        self.driver.click("admin-dns-rename-abort-button")
        time.sleep(0.3)
        self.driver.click("admin-dns-rename-abort-confirm-button")
        time.sleep(1)

    # --- Managed-mode DNS-provider credentials on admin-dns
    # (dns-management.md § App surface — the "Fauna controls DNS" credential store) ---

    def dns_credentials_list_visible(self) -> bool:
        """Is the held-credentials section (`admin-dns-credentials-list`) rendered?
        Present whenever the page is wired to the credential-store machine; it is
        empty until a credential is held."""
        return self.driver.is_visible("admin-dns-credentials-list")

    def dns_credential_count(self) -> int:
        """Number of held DNS-provider credentials shown (`admin-dns-credential-item`)."""
        return self.driver.count("admin-dns-credential-item")

    def dns_credential_providers(self) -> list[str]:
        """Provider id rendered for each held credential
        (`admin-dns-credential-item-provider`)."""
        return [t or "" for t in self.driver.get_texts("admin-dns-credential-item-provider")]

    def dns_credential_zones(self) -> list[str]:
        """Covered-zone text rendered for each held credential
        (`admin-dns-credential-item-zones`) — the zones the credential's
        ``verify()`` reported, which decide which domains it can manage."""
        return [t or "" for t in self.driver.get_texts("admin-dns-credential-item-zones")]

    def clear_dns_credential(self, index: int = 0) -> None:
        """Remove the held credential at ``index`` via its
        (`admin-dns-credential-item-clear-button`) → the shared
        ``DnsManagementMachine``'s ``ClearCredentials``; domains that lose
        coverage re-render manual. Callers poll the list rather than asserting
        here."""
        self.driver.click("admin-dns-credential-item-clear-button", index=index)
        time.sleep(1)

    def dns_domain_modes(self) -> list[str]:
        """The per-domain mode-control labels (`admin-dns-domain-mode`), aligned
        index-for-index with ``dns_domain_names()``. Each is ``"Fauna-managed"``
        (the shared machine's effective-mode projection is managed: opted-in ∧ a
        held credential covers the domain) or ``"Manual"``."""
        return [t or "" for t in self.driver.get_texts("admin-dns-domain-mode")]

    def refresh_dns(self) -> None:
        """Re-fetch the DNS record matrix + held credentials
        (`admin-dns-refresh-button` → an async `fauna.dns.list_records` /
        `verify_records` round-trip + a credential-store reload). Callers poll for
        the re-rendered state rather than asserting here."""
        self.driver.click("admin-dns-refresh-button")
        time.sleep(1)

    # --- Per-domain TLS-cert lifecycle on admin-dns
    #     (tls-certificates.md § C.4 + § B tier 3) ---

    def cert_statuses(self) -> list[str]:
        """The per-domain served-cert health badges (`admin-dns-cert-status`),
        aligned index-for-index with ``dns_domain_names()``. Each reads the nest's
        `fauna.tls.cert_status` projection: valid-trusted / on-floor — renew needed
        / expiring (`tls-certificates.md` § C.4). A pure read — renders on every
        app, web included."""
        return [t or "" for t in self.driver.get_texts("admin-dns-cert-status")]

    def cert_issue_button_count(self) -> int:
        """How many per-domain get/renew-certificate buttons
        (`admin-dns-cert-issue-button`) render — one per `admin-dns-domain`
        section. The button drives `DnsAction::IssueCert` (managed/delegated) or
        `BeginManualIssueCert` (manual); native-only (disabled on web)."""
        return self.driver.count("admin-dns-cert-issue-button")

    def issue_cert(self, domain_index: int) -> None:
        """Click the get/renew-certificate button for the domain at
        ``domain_index`` (`admin-dns-cert-issue-button`), scoped to that domain's
        section. For a managed/delegated domain this fires a single
        `DnsAction::IssueCert`; for a manual domain it opens the manual-paste flow
        (`BeginManualIssueCert` → the `_acme-challenge` paste surface +
        complete/cancel). The CA round-trip itself is covered by the shared
        machine's pebble tests, not here."""
        self.driver.click(
            "admin-dns-cert-issue-button",
            scope=f"admin-dns-domain[{domain_index}]",
        )

    def cert_pending_paste_present(self, domain_index: int) -> bool:
        """True iff a manual-paste issuance is pending for the domain at
        ``domain_index`` — the `admin-dns-cert-complete-button` /
        `admin-dns-cert-cancel-button` affordances render (scoped to the section)."""
        scope = f"admin-dns-domain[{domain_index}]"
        return self.driver.count("admin-dns-cert-complete-button", scope=scope) > 0

    def auto_renew_present(self, domain_index: int) -> bool:
        """True iff the per-domain auto-renew checkbox (`admin-dns-domain-auto-renew`)
        renders for the domain at ``domain_index`` — shown only for managed/delegated
        domains (tls-certificates.md § C.3), scoped to that domain's section."""
        scope = f"admin-dns-domain[{domain_index}]"
        return self.driver.count("admin-dns-domain-auto-renew", scope=scope) > 0

    def auto_renew_state(self, domain_index: int) -> str | None:
        """The auto-renew checkbox's checked state for the domain at ``domain_index``
        — ``"on"`` / ``"off"`` from the `state` test-attr (the label is the constant
        "Auto-renew", so the state rides an attribute). Scoped to that section."""
        return self.driver.get_attr(
            "admin-dns-domain-auto-renew", "state", scope=f"admin-dns-domain[{domain_index}]"
        )

    def toggle_auto_renew(self, domain_index: int) -> None:
        """Flip the per-domain auto-renew checkbox (`admin-dns-domain-auto-renew` →
        `DnsAction::SetAutoRenew`), scoped to that domain's section. Callers poll the
        re-rendered `state` rather than asserting here."""
        self.driver.click(
            "admin-dns-domain-auto-renew", scope=f"admin-dns-domain[{domain_index}]"
        )

    def delegate_cert_renewal(self, domain_index: int) -> None:
        """Reveal the CNAME renewal-delegation form for the domain at
        ``domain_index`` and delegate to the (default) first held-credential zone
        (`admin-dns-cert-delegate-button` → `-delegate-submit-button`,
        tls-certificates.md § B tier 3 S6b → `DnsAction::DelegateRenewal`). Scoped
        to that domain's `admin-dns-domain` section so the reveal-on-demand form's
        element is unambiguous regardless of how many domains render. The
        zone-select defaults to its first entry; callers needing a specific zone
        select it first."""
        scope = f"admin-dns-domain[{domain_index}]"
        self.driver.click("admin-dns-cert-delegate-button", scope=scope)
        self.driver.click("admin-dns-cert-delegate-submit-button", scope=scope)

    def remove_cert_delegation(self, domain_index: int) -> None:
        """Remove the domain's CNAME renewal-delegation
        (`admin-dns-cert-remove-delegation-button` → `DnsAction::RemoveDelegation`),
        scoped to that domain's section."""
        self.driver.click(
            "admin-dns-cert-remove-delegation-button",
            scope=f"admin-dns-domain[{domain_index}]",
        )

    def cert_delegation_present(self, domain_index: int) -> bool:
        """True iff the domain at ``domain_index`` shows a CNAME delegation — the
        `admin-dns-cert-remove-delegation-button` is rendered (and the delegate form
        replaced by the one-time CNAME). Scoped to that domain's section."""
        scope = f"admin-dns-domain[{domain_index}]"
        return self.driver.count("admin-dns-cert-remove-delegation-button", scope=scope) > 0

    def add_dns_credential(self, provider_id: str, fields: dict[str, str]) -> None:
        """Add a DNS-provider credential via the write-only add-credential form on
        admin-dns: reveal the form (`admin-dns-add-credential-button`), pick the
        provider (`admin-dns-add-credential-provider-row[<provider_id>]` — same
        keyed-button idiom as onboarding's `dns-provider-row[<pid>]`), type each
        field into its `<field-id>` entry (the raw providers.yaml field id, e.g.
        Cloudflare `api-token`), then submit (`admin-dns-add-credential-submit-button`).

        The submit dispatches the shared ``DnsManagementMachine``'s
        ``PutCredentials`` (verify against the provider API, then store in
        ``fauna.state.dns``). The credential lands only on a successful
        ``verify()``; callers poll the credential list / `error-message` for the
        outcome rather than asserting here."""
        self.driver.click("admin-dns-add-credential-button")
        provider_row = f"admin-dns-add-credential-provider-row[{provider_id}]"
        self.driver.wait_for(provider_row, timeout=10.0)
        self.driver.click(provider_row)
        # The field entries rebuild on provider select (convention 14: wait_for
        # the rebuilt field, not a sleep — the finding generalizes here).
        first_field = next(iter(fields))
        self.driver.wait_for(first_field, timeout=10.0)
        for field_id, value in fields.items():
            self.driver.type_text(field_id, value)
        self.driver.click("admin-dns-add-credential-submit-button")
        time.sleep(1)

    def toggle_domain_mode(self, index: int = 0) -> None:
        """Flip a domain's Fauna-managed / manual mode control
        (`admin-dns-domain-mode`, the indexed per-domain toggle on admin-dns).

        Opting **in** to Fauna-managed requires a held DNS-provider credential
        whose zones cover the domain (`dns-management.md` § The two modes);
        without one the shared ``DnsManagementMachine``'s ``SetMode`` rejects with
        ``InvalidState`` and the rejection surfaces in `error-message`. Callers
        poll that element rather than asserting here."""
        self.driver.click("admin-dns-domain-mode", index=index)
        time.sleep(1)

    def toggle_manage_all(self) -> None:
        """Flip the deployment "Fauna controls all domains" master switch
        (`admin-dns-manage-all-toggle`) — sets every active domain's mode at once
        (`dns-management.md` § The two modes; stored per-domain). Like the
        per-domain toggle, opting in requires covering credentials, so callers
        poll the rendered modes / `error-message` for the outcome."""
        self.driver.click("admin-dns-manage-all-toggle")
        time.sleep(1)

    def set_catch_all(self, value: str, index: int = 0) -> None:
        """Designate (or clear with the "None" option) a domain's catch-all actor
        via the per-row picker (`admin-dns-domain-catch-all-select`, indexed by
        active domain). `value` is the option's display text — an actor label or
        the localized "None". Drives the shared ``LocalDomainMachine``'s
        ``SetCatchAllActor`` → ``fauna.bridges.set_catch_all_actor``; callers poll
        ``list_local_domains`` (or the picker) for the persisted designation."""
        self.driver.select("admin-dns-domain-catch-all-select", value, index=index)
        time.sleep(1)

    def catch_all_selected(self, index: int = 0) -> str:
        """The display text currently selected in a domain's catch-all picker
        (`admin-dns-domain-catch-all-select`)."""
        return self.driver.get_text("admin-dns-domain-catch-all-select", index=index) or ""

    def set_role_address(self, role: str, value: str, index: int = 0) -> None:
        """Designate (or clear with the "Admin (default)" option) a domain's per-role
        override actor via the per-row picker
        (`admin-dns-domain-role-address-<role>-select`, indexed by active domain).
        `role` is one of postmaster/abuse/noc/security; `value` is the option's
        display text — an actor label or the localized "Admin (default)". Drives the
        shared ``LocalDomainMachine``'s ``SetRoleAddress`` →
        ``fauna.bridges.set_role_address``; callers poll ``list_local_domains`` for
        the persisted ``role_address_overrides``."""
        self.driver.select(
            f"admin-dns-domain-role-address-{role}-select", value, index=index
        )
        time.sleep(1)

    def role_address_selected(self, role: str, index: int = 0) -> str:
        """The display text currently selected in a domain's per-role override picker
        (`admin-dns-domain-role-address-<role>-select`)."""
        return (
            self.driver.get_text(
                f"admin-dns-domain-role-address-{role}-select", index=index
            )
            or ""
        )

    # --- Aliases page (admin.md § 4 — admin external forwarders) ---

    def navigate_aliases(self) -> None:
        """Navigate to the admin Aliases page (`admin-aliases`).

        The admin sub-page id matches the GTK Stack child name the linux app
        registers. Callers poll for `admin-aliases-forwarder-row-address` /
        `admin-aliases-forwarders-section` rather than the shared `page-heading`
        (which can resolve to a hidden sibling page)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-aliases"}]},
        })
        time.sleep(1)

    def add_forwarder(
        self, domain: str, local_part: str, target: str, *,
        offer_timeout: float = 30.0,
    ) -> None:
        """Create an external forwarder from the admin-aliases add form: pick the
        hosted domain, type the local-part + external target, submit. The
        dispatch is an async WS-RPC round-trip (`fauna.bridges.create_forwarder`
        then a `Refresh`); callers poll `admin-aliases-forwarder-row-address` for
        the new forwarder rather than asserting here.

        Retries the domain select until the add-form offers `domain` (deadline
        poll, `offer_timeout`) — mirrors `MediaActions.set_filter`'s pattern
        (`actions/media.py`): a domain seeded over a
        separate admin WS-RPC connection moments before navigating here can
        still be in flight on the page's own `AdminForwardersLoaded` fetch
        (`list_local_domains`), and the render that lands the fetch rebuilds
        the option list in the same pass, so the retry converges with the
        page rather than racing a fixed pre-navigate sleep. The budget matches
        `BOUNCE_LANDED_BUDGET_S` below rather than `MediaActions`' 15s — under
        heavy fleet build contention the WS-RPC round trip can legitimately
        take longer than 15s to land (:
        measured red at 15s under a 7-deep build-slot queue, green in under 20s
        total once isolated)."""
        deadline = time.monotonic() + offer_timeout
        while True:
            try:
                self.driver.select("admin-aliases-forwarder-add-domain-select", domain)
                break
            except SelectOptionNotOffered:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.5)
        self.driver.type_text("admin-aliases-forwarder-add-pattern-input", local_part)
        self.driver.type_text("admin-aliases-forwarder-add-target-input", target)
        self.driver.click("admin-aliases-forwarder-add-submit-button")
        time.sleep(1)

    def forwarder_addresses(self) -> list[str]:
        """All forwarder source addresses on admin-aliases
        (`admin-aliases-forwarder-row-address`)."""
        return [t or "" for t in self.driver.get_texts("admin-aliases-forwarder-row-address")]

    def forwarder_targets(self) -> list[str]:
        """All forwarder external targets on admin-aliases
        (`admin-aliases-forwarder-row-target`)."""
        return [t or "" for t in self.driver.get_texts("admin-aliases-forwarder-row-target")]

    def delete_forwarder(self, index: int = 0) -> None:
        """Delete the forwarder at `index` (`admin-aliases-forwarder-row-delete-button`).
        Async WS-RPC (`fauna.bridges.delete_forwarder` then a `Refresh`)."""
        self.driver.click("admin-aliases-forwarder-row-delete-button", index=index)
        time.sleep(1)

    def forwarders_action_error_text(self) -> str:
        """The admin-aliases action-error label (`admin-aliases-action-error`).
        Returns "" when the element is absent or carries no text — mirrors
        ``users_action_error_text``'s count-first pattern: windows prunes an
        empty-text label from the UIA tree, so a raw ``get_text`` 404s on the
        idle (no-error) state that every call site hits on success."""
        if self.driver.count("admin-aliases-action-error") == 0:
            return ""
        try:
            return self.driver.get_text("admin-aliases-action-error")
        except Exception:
            return ""

    # --- Nest page (admin.md § N Nest) + legacy Services fallback ---
    #
    # The per-page-services redesign (2026-06-04, admin.md § Admin IA redesign)
    # replaced the standalone admin-services page with the admin-nest page: the
    # operator pairing toggle and Factory Reset now live on admin-nest (Factory
    # Reset came off admin-settings). linux leads; web/windows/macos/ios/android
    # lift it via the route-3 hand-offs. Until a client lifts it, that client
    # still renders the pairing toggle on admin-services, so the
    # navigate_to_pairing_control helper below tries admin-nest first and falls
    # back to the old home — keeping the shared tests green on every app
    # through the migration. (Remove the fallback once all seven apps render
    # admin-nest.) The former read-only storage-mode indicator that also lived
    # here is RETIRED (no-modes retirement, ratified 2026-07-12) — every nest is
    # sealed at rest unconditionally now, so there is no deployment-wide storage
    # mode left to navigate to or display.

    def navigate_nest(self) -> None:
        """Navigate to the admin Nest page (`admin-nest`) — nest-wide settings:
        the operator pairing toggle + Factory Reset (admin.md § N Nest). The
        sub-page id matches the GTK Stack child name (`nav.stack[1].id`).
        Controls load asynchronously, so callers poll the rendered elements
        rather than relying on this returning loaded."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-nest"}]},
        })
        time.sleep(1)

    # ── Legal takedown console (admin-nest; moderation.md § Legal takedown —
    #    the invocation surface) ──

    def takedown_fill(
        self,
        content_id: str,
        reference: str = "",
        *,
        conversation: bool = False,
        restore: bool = False,
    ) -> None:
        """Fill the legal-takedown console's form. Callers navigate_nest() and
        poll `admin-nest-takedown-content-id-input` rendered first (controls
        load async — the navigate_nest contract)."""
        self.driver.fill("admin-nest-takedown-content-id-input", content_id)
        if reference:
            self.driver.fill("admin-nest-takedown-reference-input", reference)
        if conversation:
            self.driver.click("admin-nest-takedown-type-conversation-radio")
        if restore:
            self.driver.click("admin-nest-takedown-restore-checkbox")

    def takedown_arm(self) -> None:
        """Arm the inline confirm (the first of the two-step dispatch)."""
        self.driver.click("admin-nest-takedown-button")

    def takedown_confirm(self) -> None:
        """Dispatch the armed takedown/restore (`fauna.moderation.legal_takedown`)."""
        self.driver.click("admin-nest-takedown-confirm-button")

    def takedown_cancel(self) -> None:
        """Disarm the confirm, touching nothing."""
        self.driver.click("admin-nest-takedown-cancel-button")

    def takedown_confirm_summary(self) -> str:
        """The armed confirm's decision-surface text (names verb + content + citation)."""
        return self.driver.get_text("admin-nest-takedown-confirm-summary")

    def takedown_status_text(self) -> str:
        """The console's outcome verdict (present only after an attempt)."""
        return self.driver.get_text("admin-nest-takedown-status")

    def takedown_content_id(self) -> str:
        """The console's content-id draft, as painted."""
        return self.driver.get_text("admin-nest-takedown-content-id-input")

    # ── Reports queue (admin-nest; moderation.md § User-initiated reporting →
    #    Where it lands) — the takedown console's inbox. Callers navigate_nest()
    #    and poll `admin-nest-reports-section` first. ──

    def report_rows(self) -> list[str]:
        """Every open report's text (`admin-nest-report-item`, flat indexed)."""
        return [
            self.driver.get_text("admin-nest-report-item", index=i)
            for i in range(self.driver.count("admin-nest-report-item"))
        ]

    def report_index(self, needle: str) -> int:
        """The index of the first queue row whose text contains ``needle``, or -1."""
        for i, text in enumerate(self.report_rows()):
            if needle in text:
                return i
        return -1

    def report_open_takedown(self, index: int = 0) -> None:
        """Pre-fill the takedown console from the row at ``index``. A row with
        no takedown (an account report) paints no such button, so ``index`` is
        the button's own occurrence."""
        self.driver.click("admin-nest-report-open-takedown-button", index=index)

    def report_mark_acted(self, index: int = 0) -> None:
        self.driver.click("admin-nest-report-acted-button", index=index)

    def report_dismiss(self, index: int = 0) -> None:
        self.driver.click("admin-nest-report-dismiss-button", index=index)

    # ── Outside-app sign-in keys (admin-nest; authorization-server.md § The
    #    issuer → Two rotation arms) — the nest-held OAuth issuer key set and
    #    its refresh-token secret. Callers navigate_nest() and poll
    #    `admin-nest-oauth-key-item-0` rendered first (the navigate_nest
    #    contract: controls load async). ──

    def oauth_key_rows(self) -> list[str]:
        """Every served key's line, signer first (`admin-nest-oauth-key-item-{n}`,
        the literal per-row ids). Empty while the set has not answered — the
        `admin-nest-oauth-key-reason` line carries why."""
        rows = []
        while self.driver.count(f"admin-nest-oauth-key-item-{len(rows)}") > 0:
            rows.append(self.driver.get_text(f"admin-nest-oauth-key-item-{len(rows)}"))
        return rows

    def oauth_rotate(self) -> None:
        """The ordinary rotation — dispatches on the press, no confirm
        (`fauna.oauth.rotate_issuer_key`; nothing breaks)."""
        self.driver.click("admin-nest-oauth-rotate-button")

    def oauth_arm_force_rotate(self) -> None:
        """Arm the forced issuer-key rotation's confirm (dispatches nothing)."""
        self.driver.click("admin-nest-oauth-force-rotate-button")

    def oauth_arm_secret_force_rotate(self) -> None:
        """Arm the forced saved-sign-in (refresh-token secret) rotation's confirm."""
        self.driver.click("admin-nest-oauth-secret-force-rotate-button")

    def oauth_confirm(self) -> None:
        """Dispatch whichever forced arm is armed (disarm-before-dispatch)."""
        self.driver.click("admin-nest-oauth-confirm-button")

    def oauth_cancel(self) -> None:
        """Disarm the forced confirm, touching nothing."""
        self.driver.click("admin-nest-oauth-cancel-button")

    def oauth_confirm_summary(self) -> str:
        """The armed forced arm's cost, stated before dispatch."""
        return self.driver.get_text("admin-nest-oauth-confirm-summary")

    def oauth_status_text(self) -> str:
        """The last issuer control's verdict (present only after one was used)."""
        return self.driver.get_text("admin-nest-oauth-status")

    def navigate_services(self) -> None:
        """Navigate to the LEGACY admin Services page (`admin-services`).

        Removed from linux by the per-page-services redesign (2026-06-04); kept
        only as the transition fallback for clients that still render the pairing
        toggle here (web/windows/macos/ios/android until they lift admin-nest).
        New tests should use navigate_nest / navigate_to_pairing_control."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-services"}]},
        })
        time.sleep(1)

    def navigate_to_pairing_control(self) -> None:
        """Navigate to the operator `admin-service-pairing-toggle`. It lives on
        admin-nest after the 2026-06-04 redesign; clients that haven't lifted it
        still render it on admin-services, so fall back there when it isn't found
        on admin-nest. (Drop the fallback once all seven apps render admin-nest.)"""
        self.navigate_nest()
        if self.driver.count("admin-service-pairing-toggle") > 0:
            return
        self.navigate_services()

    def service_toggle_present(self, service: str) -> bool:
        """Whether the `admin-service-{service}-toggle` is rendered. Post-redesign
        only `pairing` remains (on admin-nest); the bridge/dns/algorithm toggles
        were dropped with the admin-services page. Uses `count` rather than
        `is_visible` — a toggle low in the page can be built-but-not-on-screen."""
        return self.driver.count(f"admin-service-{service}-toggle") > 0

    # ── admin-custody-hosting — the nest-wide custody-hosting registry ──────
    #
    # `fauna.admin.custody_hosting.{list,remove}`. No sleep anywhere in this
    # family: the read is asynchronous, so callers poll `hosting_row_count()`
    # or `hosting_registry_answered()` to a named budget (convention 14).

    def navigate_custody_hosting(self) -> None:
        """Navigate to the admin Held-Custody page (`admin-custody-hosting`).

        The registry read is asynchronous (admin nav -> AdminHostingClient.list
        -> fold -> render), so this returns as soon as the nav patch is
        delivered; poll `hosting_registry_answered()` for the answer.
        """
        self.driver.set_state({
            "nav": {
                "stack": [
                    {"view": "admin"},
                    {"view": "admin", "id": "admin-custody-hosting"},
                ]
            },
        })

    def hosting_registry_answered(self) -> bool:
        """True once the nest has ANSWERED the registry read.

        `admin-custody-hosting-count` and `admin-custody-hosting-empty` are
        mutually exclusive (`ui.yaml`): a landed read with rows paints the
        former, a landed read with none paints the latter — that split is the
        whole point (an unhydrated page paints neither), so checking only
        `-count` could never observe an honestly-empty answer and this poll
        would time out on every fresh nest regardless of budget. Check both.
        """
        return (
            self.driver.count("admin-custody-hosting-count") > 0
            or self.driver.count("admin-custody-hosting-empty") > 0
        )

    def hosting_row_count(self) -> int:
        """How many registry rows are rendered."""
        return self.driver.count("admin-custody-hosting-row")

    def hosting_row_host(self, index: int = 0) -> str:
        """The abbreviated actor id of the host that deposited row `index`."""
        return self.driver.get_text("admin-custody-hosting-host", index=index)

    def hosting_row_url(self, index: int = 0) -> str:
        """The owner-nest URL row `index` makes this nest dial, verbatim."""
        return self.driver.get_text("admin-custody-hosting-url", index=index)

    def hosting_row_held(self, index: int = 0) -> str:
        """Row `index`'s pump-metered held bytes, as rendered."""
        return self.driver.get_text("admin-custody-hosting-held", index=index)

    def hosting_row_receipt(self, index: int = 0) -> str:
        """Row `index`'s three-state receipt-freshness word."""
        return self.driver.get_text("admin-custody-hosting-receipt", index=index)

    def hosting_remove(self, index: int = 0) -> None:
        """Arm the remove confirm for row `index`, then confirm it.

        Two gestures, exactly as a human makes them: the row's own remove
        button arms a confirm naming that one row, and the confirm dispatches
        `fauna.admin.custody_hosting.remove`. Callers poll `hosting_row_count()`
        for the re-read that follows.
        """
        self.driver.click("admin-custody-hosting-remove-button", index=index)
        self.driver.wait_for("admin-custody-hosting-remove-confirm-button", timeout=10.0)
        self.driver.click("admin-custody-hosting-remove-confirm-button")

    def hosting_remove_armed(self) -> bool:
        """Whether a remove confirm is currently armed."""
        return self.driver.count("admin-custody-hosting-remove-confirm-button") > 0

    def hosting_cancel_remove(self) -> None:
        """Disarm the remove confirm without removing anything."""
        self.driver.click("admin-custody-hosting-remove-cancel-button")

    def navigate_logs(self) -> None:
        """Navigate to the admin Logs page (`admin-logs`) — the nest's fauna-log
        ring fetched over `fauna.admin.logs` (observability.md § Surfaces).

        The ring loads asynchronously (admin nav → fetch_admin_logs →
        update_admin_logs), so callers poll the `log-entry` rows rather than
        relying on this returning "loaded". The page renders with the SAME
        component IDs as the client Settings → Logs page, so the shared
        `LogsActions` accessors (`app.logs.entry_count` / `.set_level` / `.copy`)
        drive it once navigated here.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-logs"}]},
        })
        time.sleep(1)

    def logs_page_visible(self, timeout: float = 15.0) -> bool:
        """True once the admin Logs page landmark (`admin-logs-heading`) is
        present (the page nav id is `admin-logs`)."""
        try:
            self.driver.wait_for("admin-logs-heading", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def wait_for_min_log_entries(self, minimum: int = 1, timeout: float = 15.0) -> bool:
        """Poll until the admin Logs page renders at least `minimum` `log-entry`
        rows. The nest's ring is fetched async on entering the admin shell; a
        settled nest has captured at least its startup log lines."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.count("log-entry") >= minimum:
                return True
            time.sleep(0.3)
        return self.driver.count("log-entry") >= minimum

    def service_status_text(self, service: str) -> str:
        """The `admin-service-{service}-status` badge text (e.g. Enabled/Disabled)."""
        return self.driver.get_text(f"admin-service-{service}-status") or ""

    def toggle_service(self, service: str) -> None:
        """Flip the `admin-service-{service}-toggle`. Post-redesign the only
        surviving toggle is `pairing` (on admin-nest), which dispatches
        `fauna.admin.services.update` name="pairing". Async round-trip — callers
        poll the status badge for the outcome."""
        self.driver.click(f"admin-service-{service}-toggle")
        time.sleep(1)

    # --- Mail page (admin.md § 6 Mail — the flat mail-policy form) ---

    def navigate_mail(self) -> None:
        """Navigate to the admin Mail page (`admin-mail`).

        The admin sub-page id matches the GTK Stack child name the linux app
        registers. The form hydrates asynchronously (admin nav → MailPolicyMachine
        hydrate → get_mail_config → render), so callers poll the rendered controls
        rather than relying on this returning "loaded"."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-mail"}]},
        })
        time.sleep(1)

    def mail_policy_present(self) -> bool:
        """Whether the admin-mail page rendered (the mail-enable toggle is up).
        Uses `count` rather than `is_visible` — a control low in the page can be
        built-but-not-on-screen (the off-screen-not-SHOWING gotcha)."""
        return self.driver.count("admin-mail-enabled-toggle") > 0

    # --- Calendar page (admin.md § 8 Calendar; caldav-server.md § Independent
    #     enablement) — the CalDAV-enable sibling of admin-mail ---

    def navigate_calendar(self) -> None:
        """Navigate to the admin Calendar page (`admin-calendar`) — the
        deployment-wide CalDAV-enable toggle. The sub-page id matches the GTK Stack
        child name. The toggle hydrates asynchronously (admin nav →
        CaldavPolicyMachine hydrate → get_mail_config → render), so callers poll the
        rendered control rather than relying on this returning "loaded"."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-calendar"}]},
        })
        time.sleep(1)

    def calendar_policy_present(self) -> bool:
        """Whether the admin-calendar page rendered (the CalDAV-enable toggle is up)."""
        return self.driver.count("admin-calendar-enabled-toggle") > 0

    def toggle_caldav_enabled(self) -> None:
        """Flip the `admin-calendar-enabled-toggle` (dispatches
        `fauna.bridges.set_caldav_enabled`; the toggle re-reads the persisted state
        from `fauna.bridges.get_mail_config`; async — callers poll the outcome)."""
        self.driver.click("admin-calendar-enabled-toggle")
        time.sleep(1)

    def caldav_port_field_present(self) -> bool:
        """Whether the admin-set CalDAV-port field (the `text_input` + save button,
        admin.md § 8 Calendar) rendered on the admin-calendar page."""
        return (
            self.driver.count("admin-calendar-caldav-port-input") > 0
            and self.driver.count("admin-calendar-caldav-port-save-button") > 0
        )

    def set_caldav_port(self, value: str) -> None:
        """Clear + type into the `admin-calendar-caldav-port-input` (no save).

        ⚠ `wait_until_enabled`, not a bare `clear_and_type`. The whole port
        `GroupBox` carries `.disabled(vm.isBusy)` (`AdminCalendarView.swift`),
        which SwiftUI propagates to the TextField, and the field's
        `.automationField` registers no `isEnabled:` of its own — so the
        registry's "no predicate ⇒ inherit" fold reports it DISABLED for as long
        as the page is hydrating (`apps/apple-e2e-automation.md` § The actuation
        gate). `caldav_port_field_present()` only proves the field RENDERED, so
        the ubiquitous render-check + actuate pair fires inside that window and
        the gate refuses the `clear` with its 409 — the `wait_for` ≠
        `wait_until_enabled` race that section's blast-radius sweep measured.
        Waiting is also what a user does: see a greyed field, wait, then type.
        """
        self.driver.wait_until_enabled(
            "admin-calendar-caldav-port-input", timeout=15.0)
        self.driver.clear_and_type("admin-calendar-caldav-port-input", value)

    def save_caldav_port(self) -> None:
        """Click `admin-calendar-caldav-port-save-button` — validates a u16 in
        `[1, 65535]` client-side (invalid → the page `error-message`), else
        dispatches `fauna.bridges.set_caldav_port` and re-reads the persisted state
        from `fauna.bridges.get_mail_config`; async — callers poll the outcome).

        Same actuation-gate wait as `set_caldav_port` above: the button carries
        its own `.disabled(vm.isBusy)` AND the enclosing GroupBox's."""
        self.driver.wait_until_enabled(
            "admin-calendar-caldav-port-save-button", timeout=15.0)
        self.driver.click("admin-calendar-caldav-port-save-button")
        time.sleep(1)

    # --- Contacts page (admin.md § Contacts; carddav-server.md § Independent
    #     enablement) — the CardDAV-enable contacts sibling of admin-calendar ---

    def navigate_contacts(self) -> None:
        """Navigate to the admin Contacts page (`admin-contacts`) — the
        deployment-wide CardDAV-enable toggle. The toggle hydrates asynchronously
        (admin nav → CarddavPolicyMachine hydrate → get_mail_config → render), so
        callers poll the rendered control rather than relying on this returning
        "loaded"."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-contacts"}]},
        })
        time.sleep(1)

    def contacts_policy_present(self) -> bool:
        """Whether the admin-contacts page rendered (the CardDAV-enable toggle is
        up). Uses `count` rather than `is_visible` (the off-screen-not-SHOWING
        gotcha)."""
        return self.driver.count("admin-contacts-carddav-enabled-toggle") > 0

    def toggle_carddav_enabled(self) -> None:
        """Flip the `admin-contacts-carddav-enabled-toggle` (dispatches
        `fauna.bridges.set_carddav_enabled`; the toggle re-reads the persisted
        state from `fauna.bridges.get_mail_config`; async — callers poll the
        outcome)."""
        self.driver.click("admin-contacts-carddav-enabled-toggle")
        time.sleep(1)

    # --- Files page (admin.md § Files; webdav-server.md § Independent
    #     enablement) — the WebDAV-enable files sibling of admin-contacts ---

    def navigate_files(self) -> None:
        """Navigate to the admin Files page (`admin-files`) — the
        deployment-wide WebDAV-enable toggle. The toggle hydrates asynchronously
        (admin nav → WebdavPolicyMachine hydrate → get_mail_config → render), so
        callers poll the rendered control rather than relying on this returning
        "loaded"."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-files"}]},
        })
        time.sleep(1)

    def files_policy_present(self) -> bool:
        """Whether the admin-files page rendered (the WebDAV-enable toggle is
        up). Uses `count` rather than `is_visible` (the off-screen-not-SHOWING
        gotcha)."""
        return self.driver.count("admin-files-webdav-enabled-toggle") > 0

    def toggle_webdav_enabled(self) -> None:
        """Flip the `admin-files-webdav-enabled-toggle` (dispatches
        `fauna.bridges.set_webdav_enabled`; the toggle re-reads the persisted
        state from `fauna.bridges.get_mail_config`; async — callers poll the
        outcome)."""
        self.driver.click("admin-files-webdav-enabled-toggle")
        time.sleep(1)

    # --- Nest page serving-port field (nest/common.md § Serving ports — the
    #     admin-set client-facing API serving port; the symmetric twin of the
    #     admin-calendar CalDAV-port field above) ---

    def serving_port_field_present(self) -> bool:
        """Whether the admin-set serving-port field (the `text_input` + save button,
        nest/common.md § Serving ports) rendered on the admin-nest page."""
        return (
            self.driver.count("admin-nest-serving-port-input") > 0
            and self.driver.count("admin-nest-serving-port-save-button") > 0
        )

    def set_serving_port(self, value: str) -> None:
        """Clear + type into the `admin-nest-serving-port-input` (no save)."""
        self.driver.clear_and_type("admin-nest-serving-port-input", value)

    def save_serving_port(self) -> None:
        """Click `admin-nest-serving-port-save-button` — validates a u16 in
        `[1, 65535]` client-side (invalid → the page `error-message`), else
        dispatches `fauna.admin.set_serving_port` and re-reads the persisted state
        from `fauna.setup.status` (SetupStatusReply.serving_port). Applies on the
        next nest restart, but the chosen value persists immediately. Async —
        callers poll the outcome."""
        self.driver.click("admin-nest-serving-port-save-button")
        time.sleep(1)


    def region_status_text(self) -> str:
        """`admin-nest-region-status` — the declared region, or that none is
        declared (a NORMAL state, not an error)."""
        return self.driver.get_text("admin-nest-region-status")

    def region_authority_text(self) -> str:
        """`admin-nest-region-authority` — enrolled? / which document is in force.
        Empty string when absent (nothing declared, so no channel to describe)."""
        if self.driver.count("admin-nest-region-authority") == 0:
            return ""
        return self.driver.get_text("admin-nest-region-authority")

    def declare_region(self, code: str) -> None:
        """Type a region code and press save — `fauna.admin.region.set`. A
        malformed code is refused client-side onto the page `error-message` with
        no dispatch.

        Deliberately NO settle sleep (convention 14): every caller already polls
        the state it cares about to a deadline, and a fixed delay here would be
        both slower on a quiet box and too short on a loaded one."""
        self.driver.clear_and_type("admin-nest-region-input", code)
        self.driver.click("admin-nest-region-save-button")

    # ── Feature limits (admin.md § N Nest → Feature limits; the editor itself is
    #    `FeaturePolicyEditorActions`, shared with the self host) ──

    def wait_for_feature_limits(self, timeout: float = 20.0) -> None:
        """Wait for `admin-nest-feature-limits-section` — it registers once the
        nest page's reflect (which carries `fauna.features.policy.get`) lands."""
        self.driver.wait_for("admin-nest-feature-limits-section", timeout=timeout)

    def feature_limit_row(self, index: int) -> dict[str, str]:
        """One registry member's row, read scoped to that row — several members
        render at once, so a flat read would answer with another row's text."""
        scope = f"admin-nest-feature-limits-row[{index}]"
        return {
            "name": self.driver.get_text("admin-nest-feature-limits-name", scope=scope),
            "summary": self.driver.get_text(
                "admin-nest-feature-limits-summary", scope=scope
            ),
        }

    def wait_for_feature_limit_summary(
        self, index: int, expected: str, timeout: float = 20.0
    ) -> dict[str, str]:
        """Poll row `index` until its summary reads `expected` (the section is
        already visible from an earlier read, so its presence proves nothing about
        the re-read a save triggers); return the last-seen row."""
        deadline = time.monotonic() + timeout
        row = self.feature_limit_row(index)
        while time.monotonic() < deadline:
            if row["summary"] == expected:
                return row
            time.sleep(0.2)
            row = self.feature_limit_row(index)
        return row

    def open_feature_limit_editor(self, index: int) -> None:
        """Open the shared editor for row `index` at the ADMIN tier."""
        self.driver.click(
            "admin-nest-feature-limits-edit-button",
            scope=f"admin-nest-feature-limits-row[{index}]",
        )

    def withdraw_region_present(self) -> bool:
        """Whether the withdraw button rendered — it does so only while a region
        is declared."""
        return self.driver.count("admin-nest-region-withdraw-button") > 0

    def withdraw_region(self) -> None:
        """Press withdraw — `fauna.admin.region.set` with the region ABSENT, which
        also retires the previous region's policy document. Async; callers poll
        (no settle sleep, same reason as `declare_region`)."""
        self.driver.click("admin-nest-region-withdraw-button")

    # --- Nest page NAT-mode control (admin.md § Nest → NAT-mode control —
    #     the post-onboarding change surface for the axis the wizard's
    #     nat_mode_choice confirmed once at claim; driven by the shared
    #     AdminNatModeMachine) ---

    def nat_mode_control_present(self) -> bool:
        """Whether the NAT-mode control (two radios + save + status) rendered
        on the admin-nest page."""
        return (
            self.driver.count("admin-nest-nat-mode-public-radio") > 0
            and self.driver.count("admin-nest-nat-mode-private-radio") > 0
            and self.driver.count("admin-nest-nat-mode-save-button") > 0
        )

    def select_nat_mode(self, mode: str) -> None:
        """Click the `admin-nest-nat-mode-{mode}-radio` (mode: public/private)."""
        assert mode in ("public", "private"), f"unknown NAT mode {mode!r}"
        self.driver.click(f"admin-nest-nat-mode-{mode}-radio")

    def save_nat_mode(self) -> None:
        """Click `admin-nest-nat-mode-save-button` — signs + commits the
        selected mode via the mutable `fauna.setup.nat_mode` (the payload
        signature is the authorization). Async — callers poll the outcome
        (the `fauna.setup.status` `node_mode` read is the ground truth)."""
        self.driver.click("admin-nest-nat-mode-save-button")
        time.sleep(1)

    def nat_mode_status_text(self) -> str:
        """The `admin-nest-nat-mode-status` line (submit state + the
        live-vs-restart-applied caveat)."""
        return self.driver.get_text("admin-nest-nat-mode-status") or ""

    # --- Nest page web-app origin (admin.md § N Nest → Web-app origin — what
    #     this nest's own /app/ answers; every sentence is the shared
    #     fauna_client_admin::admin_web_app_origin_view) ---

    def web_app_origin_status_text(self) -> str:
        """The `admin-nest-web-app-origin-status` line — what this nest's
        address answers now (the exact target in central mode)."""
        return self.driver.get_text("admin-nest-web-app-origin-status") or ""

    def select_web_app_origin(self, mode: str) -> None:
        """Click `admin-nest-web-app-origin-{mode}-radio` (bundled/central) —
        local, nothing reaches the nest until the save."""
        assert mode in ("bundled", "central"), f"unknown web-app origin {mode!r}"
        self.driver.click(f"admin-nest-web-app-origin-{mode}-radio")

    def save_web_app_origin(self) -> None:
        """Click `admin-nest-web-app-origin-save-button` —
        `fauna.admin.web_app_origin.set`. Async; callers poll the status line
        (no settle sleep — convention 14)."""
        self.driver.click("admin-nest-web-app-origin-save-button")

    # --- Nest page host-OS-maintenance indicator + restart-now
    #     (installers/vps.md § Host OS Maintenance § 4) ---

    def os_maintenance_status_present(self) -> bool:
        """Whether the host-OS-maintenance status line (`nest-os-maintenance-status`)
        rendered on the admin-nest page. Always present (it reads the os_* fields off
        `fauna.setup.status`; defaults to 'OS up to date')."""
        return self.driver.count("nest-os-maintenance-status") > 0

    def os_maintenance_status_text(self) -> str:
        """The host-OS-maintenance status line text — 'OS up to date' /
        'Security updates pending' / 'Restart pending — …'."""
        return self.driver.get_text("nest-os-maintenance-status")

    def os_updates_count_present(self) -> bool:
        """Whether the pending-security-update count badge (`nest-os-updates-count`)
        is shown — present only when `os_security_updates_pending > 0`."""
        return self.driver.count("nest-os-updates-count") > 0

    def os_updates_count_text(self) -> str:
        """The raw pending-security-update count (`nest-os-updates-count`)."""
        return self.driver.get_text("nest-os-updates-count")

    def os_restart_now_present(self) -> bool:
        """Whether the 'restart now' button (`nest-os-restart-now-button`) is shown —
        present only when `os_reboot_pending` (it expedites the idle reboot)."""
        return self.driver.count("nest-os-restart-now-button") > 0

    def os_restart_now(self) -> None:
        """Click `nest-os-restart-now-button` — dispatches
        `fauna.admin.request_host_restart` (the nest writes a flag the host
        reboot-coordinator picks up). Async — callers poll the outcome."""
        self.driver.click("nest-os-restart-now-button")
        time.sleep(1)

    # --- Web page (web-content-hosting.md § Admin apex hosting) ---

    def navigate_web(self) -> None:
        """Navigate to the admin Web page (`admin-web`) — the nest-wide apex-actor
        designation. The sub-page id matches the GTK Stack child name. The picker
        hydrates asynchronously (admin nav → WebClient.get_apex_actor +
        admin.users.list → render), so callers poll the rendered control."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "admin-web"}]},
        })
        time.sleep(1)

    def set_apex_actor(self, value: str) -> None:
        """Designate (or clear with the "None" option) the deployment apex actor via
        the `admin-web-apex-actor-select` picker. `value` is the option's display
        text — an actor label or the localized "None". Drives `fauna.web.set_apex_actor`;
        callers poll `fauna.web.get_apex_actor` for the persisted designation."""
        self.driver.select("admin-web-apex-actor-select", value)
        time.sleep(1)

    def apex_actor_selected(self) -> str:
        """The display text currently selected in the apex picker."""
        return self.driver.get_text("admin-web-apex-actor-select") or ""

    def mail_field_present(self, element_id: str) -> bool:
        """Whether an `admin-mail-*` control is rendered."""
        return self.driver.count(element_id) > 0

    def mail_field_text(self, element_id: str) -> str:
        """The text of an `admin-mail-*` entry/label."""
        return self.driver.get_text(element_id) or ""

    def set_mail_field(self, element_id: str, value: str) -> None:
        """Clear + type into an `admin-mail-*` raw-integer entry (no save)."""
        self.driver.clear_and_type(element_id, value)

    def save_mail_spam(self) -> None:
        """Click the Spam/inbound-perimeter Save button (full PUT via
        `fauna.bridges.put_spam_policy`; async — callers poll the outcome)."""
        self.driver.click("admin-mail-spam-save-button")
        time.sleep(1)

    def publish_spam_baseline(self) -> None:
        """Click the deployment-baseline publish button
        (`admin-mail-publish-spam-baseline-button` → the shared
        `MailPolicyAction::PublishSpamBaseline` → `fauna.bridges.publish_spam_baseline`).
        The reply is stashed client-side (no page refresh), so callers poll the
        result text rather than a read twin. Async — callers poll the outcome."""
        self.driver.click("admin-mail-publish-spam-baseline-button")
        time.sleep(1)

    def publish_spam_baseline_result(self) -> str:
        """The deployment-baseline publish outcome text
        (`admin-mail-publish-spam-baseline-result`) — empty until Publish is
        clicked, then the "Published from N contributors (M samples)" message, or
        the k-anonymity-floor "too few contributors" withheld message
        (`mail-spam.md` § Cold start Path 2)."""
        return self.driver.get_text("admin-mail-publish-spam-baseline-result") or ""

    def toggle_mail_baseline_standing(self) -> None:
        """Flip `admin-mail-spam-baseline-standing-toggle` — its own gesture
        (the shared `MailPolicyAction::SetBaselineStandingPublish` → a full
        `put_spam_policy` of the persisted group with the one field flipped →
        re-read). Off withdraws the served baseline (`mail-spam.md` § Cold start
        Path 2 → *Standing publish*). Async — callers poll the nest's read."""
        self.driver.click("admin-mail-spam-baseline-standing-toggle")

    def spam_baseline_state(self) -> str:
        """The served baseline's state text (`admin-mail-spam-baseline-state`):
        "Published over N contributors on <date>." or "No baseline published.",
        either followed by "Waiting for more contributor activity." when the last
        run was deferred."""
        return self.driver.get_text("admin-mail-spam-baseline-state") or ""

    def save_mail_auth(self) -> None:
        """Click the auth-enforcement Save button (full PUT via
        `fauna.bridges.put_auth_policy`; async — callers poll the outcome)."""
        self.driver.click("admin-mail-auth-save-button")
        time.sleep(1)

    def save_mail_submission(self) -> None:
        """Click the Submission-quota Save button (full PUT via
        `fauna.bridges.put_submission_policy`; async — callers poll the outcome)."""
        self.driver.click("admin-mail-submission-save-button")
        time.sleep(1)

    def save_mail_imap(self) -> None:
        """Click the IMAP-server policy Save button (full PUT via
        `fauna.bridges.put_imap_policy`; async — callers poll the outcome)."""
        self.driver.click("admin-mail-imap-save-button")
        time.sleep(1)

    def save_mail_outbound(self) -> None:
        """Click the Outbound-delivery Save button (full PUT via
        `fauna.bridges.put_outbound_policy`; async — callers poll the outcome)."""
        self.driver.click("admin-mail-outbound-save-button")
        time.sleep(1)

    def save_mail_alias(self) -> None:
        """Click the alias-policy Save button (full PUT via
        `fauna.bridges.put_alias_policy`; the nest-side alias group reads back
        via the `get_alias_policy` twin, not `get_mail_config`; async — callers
        poll the outcome)."""
        self.driver.click("admin-mail-alias-save-button")
        time.sleep(1)

    def toggle_mail_enabled(self) -> None:
        """Flip the `admin-mail-enabled-toggle` (dispatches
        `fauna.bridges.set_mail_enabled`; async — callers poll the outcome)."""
        self.driver.click("admin-mail-enabled-toggle")
        time.sleep(1)

    def toggle_mail_auto_enable_new_users(self) -> None:
        """Flip the `admin-mail-auto-enable-new-users-toggle` (dispatches
        `fauna.bridges.set_auto_enable_mail_for_new_users`; the toggle re-reads
        the persisted state from `fauna.setup.status`; async — callers poll)."""
        self.driver.click("admin-mail-auto-enable-new-users-toggle")
        time.sleep(1)

    # --- Mail health readout (mail-deliverability.md § Admin-pane
    # Deliverability surface → The mail health readout) — the read-only section
    # at the top of admin-mail. ---

    def mail_health_present(self) -> bool:
        """Whether the health section rendered (it is omitted until the
        `fauna.bridges.mail_health` read answers)."""
        return self.driver.count("admin-mail-health-section") > 0

    def mail_health_status(self) -> str:
        """The categorical line plus the two heartbeat facts
        (`admin-mail-health-status`)."""
        return self.driver.get_text("admin-mail-health-status") or ""

    def mail_health_check_count(self) -> int:
        """How many indexed `admin-mail-health-check` rows rendered."""
        return self.driver.count("admin-mail-health-check")

    def mail_health_check(self, index: int) -> dict:
        """One row's `-label` / `-state` / `-detail` texts."""
        return {
            part: self.driver.get_text(f"admin-mail-health-check-{part}", index=index) or ""
            for part in ("label", "state", "detail")
        }

    def mail_health_delist_present(self) -> bool:
        """Whether `admin-mail-health-delist-link` is shown (only while the
        latest self-check lists the outbound IP)."""
        return self.driver.count("admin-mail-health-delist-link") > 0

    def recheck_mail_health(self) -> None:
        """Click `admin-mail-health-recheck-button` (the shared
        `MailPolicyAction::RecheckHealth`: a blocklist self-check + a diagnostics
        run, then a re-read). Async — callers poll the outcome."""
        self.driver.click("admin-mail-health-recheck-button")

    def mail_warmup_reset_text(self) -> str:
        """The warm-up reset button's label — it relabels once armed, which is
        the two-click confirm's whole visible affordance (ui.yaml scopes no
        confirm id to this button)."""
        return self.driver.get_text("admin-mail-health-warmup-reset-button") or ""

    def arm_mail_warmup_reset(self) -> None:
        """The first click only — arms the two-click confirm, resets nothing."""
        self.driver.click("admin-mail-health-warmup-reset-button")

    def confirm_mail_warmup_reset(self) -> None:
        """The second click — dispatches the shared
        `MailPolicyAction::ResetWarmup` (`fauna.bridges.outbound_warmup_reset`)."""
        self.driver.click("admin-mail-health-warmup-reset-button")

    def dashboard_has_card(self, title: str) -> bool:
        """Check if a dashboard stat card with the given title is visible."""
        count = self.driver.count("admin-stat-card-label")
        for i in range(count):
            label = self.driver.get_text("admin-stat-card-label", index=i)
            if title in label:
                return True
        return False

    def dashboard_card_value(self, title: str) -> str:
        """Get the value from a dashboard stat card."""
        count = self.driver.count("admin-stat-card-label")
        for i in range(count):
            label = self.driver.get_text("admin-stat-card-label", index=i)
            if title in label:
                return self.driver.get_text("admin-stat-card-value", index=i)
        return ""

    def is_dashboard_visible(self) -> bool:
        """Check if the admin dashboard heading is visible."""
        return self.driver.is_visible("admin-dashboard-heading")

    # --- Navigation model: the uniform "leave admin" affordance (admin.md
    # § Navigation model) ---

    def nav_back_present(self) -> bool:
        """Whether the shell's `admin-nav-back` ('leave admin') button exists.

        It lives in the admin shell (present on every admin page), so existence
        is what matters — not on-screen visibility (the linux shell-header button
        may be above the fold of a long page; see the TODO gotchas)."""
        return self.driver.count("admin-nav-back") > 0

    def leave_admin(self) -> None:
        """Click `admin-nav-back` to exit the admin shell back to the non-admin
        app (admin.md § Navigation model)."""
        self.driver.click("admin-nav-back")

    # --- Settings: tier definitions (admin.md § 3) ---

    def tiers_section_visible(self) -> bool:
        """Whether the tier-definitions section anchor is shown on admin-settings
        (`admin-settings-tiers-section`)."""
        return self.driver.is_visible("admin-settings-tiers-section")

    def tier_definition_count(self) -> int:
        """Number of tier *definition* rows on admin-settings
        (`admin-settings-tier-item`) — the nest seeds free/personal/community."""
        return self.driver.count("admin-settings-tier-item")

    def tier_cap_value(self, cap: str, *, index: int = 0) -> str:
        """Read the editable raw-i64 value of a tier-definition cap input on
        `admin-settings` (`admin-settings-tier-cap-{cap}`), scoped to the indexed
        `admin-settings-tier-item[index]` row. `cap` ∈ {inbox, storage, devices,
        blob-size, feeds} (admin.md § 3 — in-place tier-cap editing)."""
        return self.driver.get_text(
            f"admin-settings-tier-cap-{cap}", scope=f"admin-settings-tier-item[{index}]"
        )

    def edit_tier_cap(self, cap: str, value: str, *, index: int = 0) -> None:
        """Set a tier-definition cap input to a raw-i64 `value`, scoped to the
        indexed `admin-settings-tier-item[index]` row."""
        self.driver.fill(
            f"admin-settings-tier-cap-{cap}", value,
            scope=f"admin-settings-tier-item[{index}]",
        )

    def save_tier(self, *, index: int = 0) -> None:
        """Persist a tier row's edited caps via `admin-settings-tier-save-button`
        (`fauna.admin.tiers.update`), scoped to `admin-settings-tier-item[index]`.
        On success the page refetches `tiers.list`, re-rendering the row with the
        persisted values."""
        self.driver.click(
            "admin-settings-tier-save-button", scope=f"admin-settings-tier-item[{index}]"
        )

    # --- Settings: defining a new tier (admin.md § 3 — `admin-settings-tier-add-*`) ---

    def tier_add_section_visible(self) -> bool:
        """Whether the add-a-tier form anchor is shown on admin-settings
        (`admin-settings-tier-add-section`)."""
        return self.driver.is_visible("admin-settings-tier-add-section")

    def tier_name(self, *, index: int = 0) -> str:
        """The name shown on the `index`-th `admin-settings-tier-item` row."""
        return self.driver.get_text("admin-settings-tier-item", index=index)

    def tier_names(self) -> list[str]:
        """Every tier definition row's name, in list order."""
        return [self.tier_name(index=i) for i in range(self.tier_definition_count())]

    def edit_new_tier_name(self, name: str) -> None:
        """Type the new tier's name (`admin-settings-tier-add-name-input`)."""
        self.driver.fill(
            "admin-settings-tier-add-name-input", name,
            scope="admin-settings-tier-add-section",
        )

    def edit_new_tier_cap(self, cap: str, value: str) -> None:
        """Type a raw-i64 cap into the add form (`admin-settings-tier-cap-{cap}`
        scoped under `admin-settings-tier-add-section`, exactly as the same
        inputs are scoped under a row). `cap` ∈ {inbox, storage, devices,
        blob-size, feeds}."""
        self.driver.fill(
            f"admin-settings-tier-cap-{cap}", value,
            scope="admin-settings-tier-add-section",
        )

    def add_tier(self) -> None:
        """Define the tier from the add form (`admin-settings-tier-add-button`,
        `fauna.admin.tiers.create`). On success the page refetches `tiers.list`
        and the new tier appears as an ordinary row."""
        self.driver.click(
            "admin-settings-tier-add-button", scope="admin-settings-tier-add-section"
        )

    # --- Settings: membership designations (monetization.md § Pillar 4) ---
    # A link editor: one `admin-settings-membership-item` row per subscription
    # tier the admin owns, joining it to two quota tiers. Never a third tier
    # list — designating mutates neither tier system.

    def membership_section_visible(self) -> bool:
        """Whether the membership-designation section anchor is shown on
        `admin-settings` (`admin-settings-membership-section`)."""
        return self.driver.is_visible("admin-settings-membership-section")

    def membership_row_count(self) -> int:
        """Number of membership rows on `admin-settings`
        (`admin-settings-membership-item`) — one per subscription tier the
        calling admin owns."""
        return self.driver.count("admin-settings-membership-item")

    def membership_tier_name(self, *, index: int = 0) -> str:
        """Read the subscription-tier name this row designates
        (`admin-settings-membership-tier-select`), scoped to
        `admin-settings-membership-item[index]`."""
        return self.driver.get_text(
            "admin-settings-membership-tier-select",
            scope=f"admin-settings-membership-item[{index}]",
        )

    def membership_admin_tier(self, *, index: int = 0) -> str:
        """Read the admitted quota tier this row currently shows
        (`admin-settings-membership-admin-tier-select`), scoped to
        `admin-settings-membership-item[index]`."""
        return self.driver.get_text(
            "admin-settings-membership-admin-tier-select",
            scope=f"admin-settings-membership-item[{index}]",
        )

    def membership_lapse_tier(self, *, index: int = 0) -> str:
        """Read the lapsed quota tier this row currently shows
        (`admin-settings-membership-lapse-tier-select`), scoped to
        `admin-settings-membership-item[index]`."""
        return self.driver.get_text(
            "admin-settings-membership-lapse-tier-select",
            scope=f"admin-settings-membership-item[{index}]",
        )

    def set_membership_admin_tier(self, target: str, *, index: int = 0) -> None:
        """Set a membership row's admitted quota tier to `target`, scoped to
        `admin-settings-membership-item[index]`. Not yet persisted — call
        `save_membership_tier` to designate/re-point via
        `fauna.admin.membership_tiers.set`."""
        self._pick_tier(
            "admin-settings-membership-admin-tier-select", target, index=index,
        )

    def set_membership_lapse_tier(self, target: str, *, index: int = 0) -> None:
        """Set a membership row's lapsed quota tier to `target`, scoped to
        `admin-settings-membership-item[index]`. Not yet persisted — call
        `save_membership_tier`."""
        self._pick_tier(
            "admin-settings-membership-lapse-tier-select", target, index=index,
        )

    def save_membership_tier(self, *, index: int = 0) -> None:
        """Designate/re-point a membership row via
        `admin-settings-membership-save-button`
        (`fauna.admin.membership_tiers.set` — an upsert), scoped to
        `admin-settings-membership-item[index]`. On success the page refetches
        `membership_tiers.list`, re-rendering the row from persisted state."""
        self.driver.click(
            "admin-settings-membership-save-button",
            scope=f"admin-settings-membership-item[{index}]",
        )

    def clear_membership_tier(self, *, index: int = 0) -> None:
        """Drop a membership row's designation via
        `admin-settings-membership-clear-button`
        (`fauna.admin.membership_tiers.clear`), scoped to
        `admin-settings-membership-item[index]`. The subscription tier itself
        is untouched — it just reverts to an ordinary content tier."""
        self.driver.click(
            "admin-settings-membership-clear-button",
            scope=f"admin-settings-membership-item[{index}]",
        )

    # --- Registration posture (admin-users § Section 2) ---
    #
    # The nest's registration posture + the orthogonal free-tier ceiling, saved
    # together by one `fauna.admin.set_registration_mode` call (admin.md § 2 Users
    # → Section 2; posture semantics owned by public-mode.md § Registration
    # Modes). Driving these through the UI is the point: the kind is Admin-gated
    # and live nest-side, but until a client renders this section the posture is
    # unreachable for the admin it belongs to.

    def registration_mode(self) -> str:
        """The mode the posture picker currently shows — a wire string
        (`open` / `invite_required` / `closed`), not the visible label.

        `get_text` on a selector reports its current *value* (the cross-app
        contract), which is symmetric with `set_registration_mode`.
        """
        return self.driver.get_text("admin-users-registration-mode-select")

    def set_registration_mode(self, target: str) -> None:
        """Set the posture picker to `target` (a wire string). Does not save —
        `save_registration` is the separate, deliberate commit (one Save carries
        mode + ceiling together)."""
        self._pick_tier("admin-users-registration-mode-select", target)

    def max_free_users(self) -> str:
        """The free-tier ceiling as shown — `''` when blank (no cap)."""
        return self.driver.get_text("admin-users-max-free-users-input")

    def set_max_free_users(self, value: str) -> None:
        """Type the free-tier ceiling; `''` clears it (blank = no cap). Orthogonal
        to the mode, but saved by the same button."""
        self.driver.clear_and_type("admin-users-max-free-users-input", value)

    def save_registration(self, *, settle: float = 1.0) -> None:
        """Commit mode + ceiling (one `fauna.admin.set_registration_mode`) — and
        the age require-knob when its toggle changed (a second
        `fauna.admin.set_age_verification_required`, same gesture). The page
        re-reads `fauna.setup.status` afterwards, so the controls re-seed from
        the nest — what you read back is the persisted posture, not the typing.
        Failures land on `admin-users-action-error` (`users_action_error_text`),
        never silently."""
        self.driver.click("admin-users-registration-save-button")
        time.sleep(settle)

    def age_verification_state(self) -> str:
        """"on"/"off" — `admin-users-registration-age-verification-toggle`, the
        "accept only signups carrying app age verification" knob
        (family-safety.md § App surface → *Age-band surfaces*), via the uniform
        toggle-read idiom (`get_attr(id, "state")`)."""
        return self.driver.get_attr("admin-users-registration-age-verification-toggle", "state") or ""

    def set_age_verification(self, enabled: bool) -> None:
        """Flip the require-knob's DRAFT to `enabled` (a click when it differs).
        Does not save — `save_registration` is the one deliberate commit, which
        sends the knob only when it changed."""
        if (self.age_verification_state() == "on") != enabled:
            self.driver.click("admin-users-registration-age-verification-toggle")
            time.sleep(0.3)

    # --- User list ---

    def navigate_users(self) -> None:
        """Navigate to the admin user list page and wait for rows to load.

        The user list is fetched asynchronously (fetch_admin_users on the
        admin-status check), so poll for at least one `user-row` rather than a
        fixed sleep — the admin themselves is always a registered user, so a
        loaded list has >=1 row. Falls through after the timeout so the test's
        own assertion produces the diagnostic if the list genuinely stays empty.

        On web this must force a real remount. The SPA navigates with SvelteKit's
        `goto`, which is a no-op when you are already on the target route — so the
        page's onMount fetch never re-runs and anything seeded out-of-band (a raw
        WS `invite_request.submit`, another admin's action) stays invisible no
        matter how many times a poll loop re-navigates. Bounce through another
        route first. The native apps rebuild the page on every nav, so they
        need no bounce. (Platform branch lives here in the action layer, never in
        a test file.)

        The bounce is confirmed against the observable, never timed (convention
        14). `getState().nav` is derived from `location.pathname`, so "did we
        actually leave this route" is directly readable — and it has to be read,
        because a bounce that has not landed yet turns the second navigation back
        into the very same-route no-op this method exists to defeat, silently
        serving the poll loop stale rows for its whole deadline. A fixed delay
        sized on a quiet box is exactly the wrong instrument: it fails only under
        load, and only by making the data look stale rather than by erroring.
        """
        if self.driver.is_web():
            self.driver.set_state({"nav": {"stack": [{"view": "settings"}]}})
            self._await_left_users_route()
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "users"}]},
        })
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:  # deadline-ok: documented above — the test's own assertion diagnoses a genuinely empty list
            if self.user_count() >= 1:
                return
            time.sleep(0.3)

    # A generous ceiling, not an expected duration: the bounce normally lands in
    # well under a second, and a green run pays only its poll interval. It exists
    # so a heavily loaded box cannot turn a slow navigation into stale data.
    BOUNCE_LANDED_BUDGET_S = 30.0

    def _await_left_users_route(self) -> None:
        """Block until the web bounce has actually left the admin-users route.

        Falls through on timeout rather than raising: the caller's own poll (and
        the test's `pending_requests_diagnosis`) produce a far better message
        than a bare timeout here, and a bounce that never lands still leaves the
        page usable — just stale, which is exactly what the diagnosis reports.
        """
        deadline = time.monotonic() + self.BOUNCE_LANDED_BUDGET_S
        while time.monotonic() < deadline:  # deadline-ok: documented above — the caller's own poll+diagnosis is the real check
            try:
                nav = self.driver.get_state("nav") or {}
                stack = nav.get("stack") or []
                if stack and stack[0].get("view") != "admin":
                    return
            except Exception:
                # A read that fails mid-navigation is not a verdict — retry.
                pass
            time.sleep(0.1)

    # --- Direct admission (Admit section — the third account-creation path,
    #     public-mode.md § Registration & Identity; user-approved IDs 2026-08-15) ---

    def admit_user(self, actor_hex: str, handle: str | None = None,
                   tier: str = "free") -> None:
        """Admit a known actor id directly (one ``fauna.admin.users.create``
        call from the Admit section). ``handle=None`` leaves the handle field
        blank — the deliberate handle-less admission (`public-mode.md`
        § A handle-less account). Does not wait for the nest outcome: the
        durable truth (the admitted actor authenticates / the handle indexes)
        is what a caller polls, so the assertion stays latency-independent."""
        self.driver.clear_and_type("admin-users-admit-actor-input", actor_hex)
        if handle is not None:
            self.driver.clear_and_type("admin-users-admit-handle-input", handle)
        self._pick_tier("admin-users-admit-tier-select", tier)
        self.driver.click("admin-users-admit-button")

    def user_count(self) -> int:
        """Count users in the admin user list."""
        return self.driver.count("user-row")

    def user_actor_ids(self) -> list[str]:
        """Get all actor IDs from the user list."""
        count = self.driver.count("user-actor-id")
        return [self.driver.get_text("user-actor-id", index=i) for i in range(count)]

    def user_row_index(self, actor_hex: str) -> int:
        """The row for ``actor_hex``, found by ACTOR ID — never by position.

        Position-based identity is unsound here. The list is newest first, and
        every account another test creates moves every older row down:
        session-scoped users added by sibling admin tests, and every OTHER app
        parametrization, which shares this one nest (`nest_instance` is
        ``scope="session"``, `conftest.py`), so the list a native run sees already
        carries the users web admitted. Nor is the order among accounts created in
        the same second fixed on every nest: ``created_at`` is second-granular
        (`bins/fauna-nest/src/db/mod.rs` ``now_epoch_secs``), and a nest that does
        not break those ties by insertion order leaves them unspecified, so the
        admin is not reliably last there (nor is row 0 reliably not-the-admin).
        Tests that assumed a position were ordering-luck flaky.

        The displayed ``user-actor-id`` is truncated for readability — both web and
        linux show ``<first-12>…`` — so match the full hex by *prefix* rather than
        by exact string (12 hex chars is 48 bits: collision-free in practice).
        """
        ids = self.user_actor_ids()
        for i, shown in enumerate(ids):
            prefix = shown.rstrip("….")  # drop a trailing ellipsis ("…" or "...")
            if prefix and actor_hex.startswith(prefix):
                return i
        raise AssertionError(
            f"actor {actor_hex[:12]}… not in the user list; ids={ids!r} "
            f"error={self.driver.get_text('error-message') if self.driver.count('error-message') else ''!r}"
        )

    def has_user_row(self, actor_hex: str) -> bool:
        """Whether ``actor_hex`` currently has a row — the pollable form of
        `user_row_index`, which raises by design for callers that expect the row
        to be there already."""
        try:
            self.user_row_index(actor_hex)
            return True
        except AssertionError:
            return False

    def admin_row_index(self, admin_hex: str) -> int:
        """The admin's own row — `user_row_index` under the name the cut-off and
        roster tests read it by (an admin is a user with a role, so the lookup is
        the same one; see that method for why position is never the answer)."""
        return self.user_row_index(admin_hex)

    def users_action_error_text(self) -> str:
        """Read the dedicated Users-hub action-error element
        (`admin-users-action-error`, admin.md § Errors — Users-section action
        failures route here, not the global `error-message` banner). Returns ""
        when the element is absent or carries no text. Uses ``count`` + raw
        ``get_text`` rather than ``is_visible``: the label sits at the hub tail
        below a long ScrolledWindow and may read not-SHOWING though populated."""
        if self.driver.count("admin-users-action-error") == 0:
            return ""
        try:
            return self.driver.get_text("admin-users-action-error")
        except Exception:
            return ""

    def user_tier(self, index: int = 0) -> str:
        """Read the tier shown on a user row's `admin-users-tier-select`."""
        return self.driver.get_text("admin-users-tier-select", index=index)

    def user_mail_serving_status(self, index: int = 0) -> str:
        """Read a user row's read-only IMAP/CalDAV-serving audit indicator
        (`admin-users-mail-serving-status`, scoped to `user-row[index]`).

        The admin only *sees* this — it is set by the user from their own
        mail-settings serve-here toggle (admin.md § Users; deployment-home-with-
        public-relay.md § MUA reach). Renders the i18n `serving_here` /
        `serving_disabled` text (default on)."""
        return self.driver.get_text(
            "admin-users-mail-serving-status", scope=f"user-row[{index}]"
        )

    def _pick_tier(self, test_id: str, target: str, *, index: int = 0,
                   settle: float = 0.6, tries: int = 12) -> None:
        """Set a tier picker to ``target``, cross-app.

        DropDown (linux) / ``<select>`` (web) clients expose a ``select`` action,
        so ``driver.select`` sets the picker (and, for the live Users picker,
        applies the tier via ``fauna.admin.users.update``). The native apps
        still render cycle-buttons (no ``select`` action — the agent 404s with
        "not a selector"), so fall back to cycle-clicking. ``index`` addresses
        per-row pickers.

        Each iteration re-actuates if the label isn't yet ``target``: for the
        live Users picker the apply + list refetch can briefly rebuild the row
        from a stale snapshot and revert the label, so re-selecting until the
        server settles mirrors the old cycle-until-stable loop.

        The leading ``get_text`` read tolerates a transient "not found" too —
        a picker that only just became visible (a section flipped from a
        read-only fallback to the live picker in the same paint, e.g. windows'
        admin-users Registration section) can lag its AutomationPeer behind
        the Visibility flip by a frame; the retry loop below already treats
        the identical 404 from ``select`` this way (measured: without this,
        the very FIRST touch of such a picker in a test — no prior read to
        absorb the settle — 404s outright instead of retrying).
        """
        use_select = True
        for _ in range(tries):
            try:
                if self.driver.get_text(test_id, index=index) == target:
                    return
            except LookupError:
                time.sleep(settle)  # sleep-ok: poll cadence inside the bounded tries×settle retry loop, mirrors the identical select() 404-retry below
                continue
            if use_select:
                try:
                    self.driver.select(test_id, target, index=index)
                    time.sleep(settle)
                    continue
                except (RuntimeError, NotImplementedError):
                    use_select = False  # genuine non-selector → cycle-button
                except LookupError as e:
                    # "not a selector" ⇒ cycle-button client; any other 404
                    # (element transiently absent mid-rebuild) ⇒ retry select.
                    if "not a selector" not in str(e):
                        time.sleep(settle)
                        continue
                    use_select = False
            # Cycle-button path: each click advances (and, for Users, applies).
            self.driver.click(test_id, index=index)
            time.sleep(settle)

    def set_user_tier(self, target: str, index: int = 0) -> None:
        """Change a user's tier to `target` via the row's tier picker.

        On linux the picker is a `gtk::DropDown` (web a `<select>`), actuated by
        `driver.select`; selecting applies the tier via `fauna.admin.users.update`,
        then the list refetches and the row rebuilds with the applied tier.
        Native cycle-button clients fall back to cycle-clicking. After the picker
        shows `target`, settle briefly so the apply round-trip + refetch land
        before the caller asserts persistence.

        Unlike the request-row picker (local-only), this picker *applies live* on
        every cycle tap (`fauna.admin.users.update` + a `users.list` refetch), so a
        bigger inter-tap settle is needed: a tap read before the refetch lands sees
        a stale label and cycles again, overshooting the target. A settle that
        covers the apply round-trip keeps the cycle deterministic.
        """
        self._pick_tier("admin-users-tier-select", target, index=index, settle=2.0)
        time.sleep(1.0)

    # --- User eviction (Users section, admin.md § Users Section 3) ---
    # The evict / cancel-eviction controls are per-row and mutually exclusive
    # (a row shows `admin-users-evict-button` when no eviction is in flight,
    # else `admin-users-cancel-eviction-button`), so they're queried *scoped to
    # the row* (`user-row[i]`) rather than by a global index that would drift as
    # rows flip between the two controls.

    def user_eviction_active(self, index: int = 0) -> bool:
        """True when row `index` shows the cancel-eviction control (an eviction
        is in flight) rather than the evict control. `is_visible_scrolled`, not
        plain `is_visible` — see `user_evict_available` for why."""
        return self.driver.is_visible_scrolled(
            "admin-users-cancel-eviction-button", scope=f"user-row[{index}]"
        )

    def evict_user(self, index: int = 0) -> None:
        """Start eviction for the user at row `index` via its
        `admin-users-evict-button` (one click → `fauna.admin.users.evict` with a
        default reason/category). The list refetches and the row rebuilds with
        the cancel-eviction control (the user is not deleted — eviction is a
        warn→suspend→delete timeline)."""
        self.driver.click("admin-users-evict-button", scope=f"user-row[{index}]")
        time.sleep(1.5)

    def user_evict_available(self, index: int = 0) -> bool:
        """True when row `index` offers `admin-users-evict-button` — i.e. no
        cut-off is in flight and the row is not an admin's.

        `is_visible_scrolled`, not plain `is_visible`: the row grew a 4th line
        (windows' admin-role grant/revoke controls, row 101) — as the roster
        list grows across a test file's shared `nest_instance`, a later row can
        need scrolling further than an unscrolled viewport already shows."""
        return self.driver.is_visible_scrolled("admin-users-evict-button", scope=f"user-row[{index}]")

    def user_suspend_available(self, index: int = 0) -> bool:
        """True when row `index` offers `admin-users-suspend-button`. Withheld on
        an admin's row (the nest refuses with `fauna.admin.conflict`) and on an
        already-suspended row (the nest's suspend is a no-op there). See
        `user_evict_available` for why `is_visible_scrolled`."""
        return self.driver.is_visible_scrolled("admin-users-suspend-button", scope=f"user-row[{index}]")

    def suspend_user(self, index: int = 0) -> None:
        """Suspend the user at row `index` **immediately** via its
        `admin-users-suspend-button` (one click → `fauna.admin.users.suspend`).
        Unlike evict this schedules no deletion — the row rebuilds showing the
        cancel-eviction (restore) control, which is the shared exit from either
        cut-off path (admin.md § 2 → *Cutting a user off*)."""
        self.driver.click("admin-users-suspend-button", scope=f"user-row[{index}]")
        time.sleep(1.5)

    def cancel_user_eviction(self, index: int = 0) -> None:
        """Cancel the in-flight eviction at row `index`
        (`admin-users-cancel-eviction-button` → `fauna.admin.users.cancel_eviction`);
        the list refetches and the row rebuilds with the evict control again."""
        self.driver.click("admin-users-cancel-eviction-button", scope=f"user-row[{index}]")
        time.sleep(1.5)

    # --- Admin-role roster (Users section, admin.md § Admin continuity and
    # succession — the two per-row grant/revoke controls, `admins_add`/
    # `admins_remove`). Mutually exclusive per row like the cut-off pair: a
    # plain row offers make-admin, an is_admin row offers remove-admin. Both
    # are SCHEDULED (pending-action delay window) — the row does not flip
    # instantly on click, so callers poll (`admins.list` or a re-render read)
    # rather than asserting immediately after.

    def user_make_admin_available(self, index: int = 0) -> bool:
        """True when row `index` offers `admin-users-make-admin-button` — i.e.
        the row is not already an admin's.

        `is_visible_scrolled`, not plain `is_visible`: on windows the button
        sits on the row's own dedicated 4th line (below suspend/evict/cancel,
        which share the row's first 3 lines beside the identity text) — a taller
        row than before this control existed, so it can start just past the
        already-scrolled viewport's bottom edge even though the row itself is
        "found".
        """
        return self.driver.is_visible_scrolled(
            "admin-users-make-admin-button", scope=f"user-row[{index}]"
        )

    def user_remove_admin_available(self, index: int = 0) -> bool:
        """True when row `index` offers `admin-users-remove-admin-button` — i.e.
        the row is an admin's. See `user_make_admin_available` for why
        `is_visible_scrolled`."""
        return self.driver.is_visible_scrolled(
            "admin-users-remove-admin-button", scope=f"user-row[{index}]"
        )

    def make_admin(self, index: int = 0) -> None:
        """Grant the admin role to the user at row `index` via
        `admin-users-make-admin-button` (schedules `fauna.admin.admins.add`)."""
        self.driver.click("admin-users-make-admin-button", scope=f"user-row[{index}]")
        time.sleep(1.5)  # sleep-ok: settles the async schedule+re-read round trip; the grant itself has no rendered state to poll on (it's a 24h-delayed pending action, mirroring evict/suspend's identical settle above)

    def remove_admin(self, index: int = 0) -> None:
        """Revoke the admin role from the user at row `index` via
        `admin-users-remove-admin-button` (schedules `fauna.admin.admins.remove`).
        Refused by the nest (`fauna.admin.conflict`, surfaced on
        `admin-users-action-error`) when it would leave zero superadmins."""
        self.driver.click("admin-users-remove-admin-button", scope=f"user-row[{index}]")
        time.sleep(1.5)  # sleep-ok: settles the async schedule+re-read round trip, mirroring evict/suspend's identical settle above

    # --- Pending admin actions (the STANDING `admin-users-pending-section`,
    #     admin.md § Pending admin actions — every account's still-pending
    #     delayed admin action, the co-admin's approve/veto surface) ---

    def pending_section_text(self) -> str:
        """The section container's own text — the bare title while un-hydrated,
        the empty-state line, or the counted title (the settings-page shape)."""
        return self.driver.get_text("admin-users-pending-section") or ""

    def pending_action_descriptions(self) -> list[str]:
        """Every `admin-pending-action-description`, in row order."""
        return [
            self.driver.get_text("admin-pending-action-description", index=i) or ""
            for i in range(self.driver.count("admin-pending-action-item"))
        ]

    def pending_action_row_index(self, actor_hex: str) -> int:
        """The pending-action row naming ``actor_hex`` as its target, by identity
        (the description carries the target id; matched by a 12-hex prefix, the
        `user_row_index` idiom), or ``-1`` when no row names it — the pollable
        form for `wait_until`."""
        prefix = actor_hex[:12]
        for i, text in enumerate(self.pending_action_descriptions()):
            if prefix in text:
                return i
        return -1

    def pending_action_execute_after(self, index: int) -> str:
        return self.driver.get_text("admin-pending-action-execute-after", index=index) or ""

    def pending_action_approvals(self, index: int) -> str:
        """The row's approvals readout (\"0 of 1 approvals\")."""
        return self.driver.get_text("admin-pending-action-approvals", index=index) or ""

    def approve_pending_action(self, index: int) -> None:
        """Add this admin's approval to row ``index``
        (`fauna.pending_actions.approve`; refused for one's own action — the
        refusal lands on `admin-users-action-error`)."""
        self.driver.click("admin-pending-action-approve-button", index=index)

    def cancel_pending_action(self, index: int) -> None:
        """Call row ``index``'s action off (`fauna.pending_actions.cancel`) —
        one click, no confirm."""
        self.driver.click("admin-pending-action-cancel-button", index=index)

    # --- Pagination (Users section) ---

    def pagination_present(self) -> bool:
        """Whether the Users-section pagination container is rendered
        (`admin-users-pagination`). Uses ``count`` (existence in the a11y tree),
        not ``is_visible`` (on-screen): the hub stacks three sections in one
        scrolled view, so the pagination row — last in the Users section — sits
        below the fold and isn't SHOWING even though it's built. The prev/next
        clicks act via AT-SPI regardless of scroll position."""
        return self.driver.count("admin-users-pagination") > 0

    def next_page_enabled(self) -> bool:
        """Is `admin-users-next-page` clickable — i.e. is there a page after
        this one? The app reflects the bound with `set_sensitive`, so this is
        the same fact a user reads off the greyed-out button."""
        return self.driver.is_enabled("admin-users-next-page")

    def prev_page_enabled(self) -> bool:
        """Is `admin-users-prev-page` clickable — i.e. are we past offset 0?"""
        return self.driver.is_enabled("admin-users-prev-page")

    def next_page(self) -> None:
        """Advance the Users list to the next page (`admin-users-next-page` →
        refetch at the next offset).

        Callers must only reach here when the control is live: at the last page
        the app disables it, and driving a disabled control is the illegal act
        convention 11 forbids. Guard with `next_page_enabled()`.
        """
        self.driver.wait_until_enabled("admin-users-next-page", timeout=10.0)
        self.driver.click("admin-users-next-page")
        time.sleep(1.0)

    def prev_page(self) -> None:
        """Go to the previous Users page (`admin-users-prev-page`).

        As with `next_page`, the caller owns the bound check —
        `prev_page_enabled()` — because at offset 0 the app disables this.
        """
        self.driver.wait_until_enabled("admin-users-prev-page", timeout=10.0)
        self.driver.click("admin-users-prev-page")
        time.sleep(1.0)

    # --- Invite codes (the Users hub's "Invite" section) ---

    def navigate_invite_codes(self) -> None:
        """Navigate to the consolidated admin-users hub (the Invite section).

        Invite-code minting folded into `admin-users` (admin.md § Users,
        2026-05-29); it is no longer on `admin-settings`. Wait for the Invite
        section, which the async users/codes fetch fills.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "admin"}, {"view": "admin", "id": "users"}]},
        })
        self.driver.wait_for("admin-users-invite-section", timeout=15.0)

    def create_invite_code(self, tier: str = "free", uses: int = 1,
                           guardian: str | None = None,
                           age_band: str | None = None) -> str:
        """Mint an invite code at `tier`/`uses` and return the minted token.

        Empty code ⇒ the nest mints (admin.md § 3). Reveal the form
        (`open_invite_form`), then `mint_invite_code`: cycle the mint tier-select
        to `tier`, set max-uses, confirm; the new code lands in the codes list
        (read its `invite-code-value`).

        `guardian`, when given, is a user's label/handle to select in
        `admin-users-invite-guardian-select` — the admitted account becomes
        supervised by that guardian (family-safety.md § Wire & data shape).
        Left `None`, the form's default "None" selection stands (an
        unsupervised admission). `age_band` is the option VALUE of
        `admin-users-invite-age-band-select` (`u13` / `13-15` / `16-17` / `18+`;
        family-safety.md § App surface → *Age-band surfaces*) — the select is
        enabled only once a guardian is chosen, so pass it with `guardian`.
        """
        self.open_invite_form()
        return self.mint_invite_code(tier=tier, uses=uses, guardian=guardian, age_band=age_band)

    def open_invite_form(self) -> None:
        """Reveal the create form (`create-invite-code-btn`) and wait for its
        tier picker — the split half of `create_invite_code`, so a test can
        inspect the open form (the band select's guardian gate) before minting."""
        self.driver.click("create-invite-code-btn")
        # Fixed-delay waits here are convention 14's banned "brittle
        # timing-dependent" shape — replaced with `wait_for` (found while
        # chasing a 3/3-reproducible "create-invite-confirm-btn not found" on
        # iOS. This did NOT fix that failure:
        # the button genuinely never renders there (a real iOS-only layout
        # bug, `wait_for` now reports a clean TimeoutError instead of racing
        # a sleep), so it's a real bug, not a race — but the fixed-delay
        # shape was worth removing regardless of that finding.
        self.driver.wait_for("admin-settings-tier-select", timeout=10.0)

    def invite_age_band_enabled(self) -> bool:
        """Whether the open form's `admin-users-invite-age-band-select` is
        enabled — it is only while a guardian is selected (the nest refuses a
        band without a guardian designation, and the UI gates the same way)."""
        return self.driver.is_enabled("admin-users-invite-age-band-select")

    def invite_age_band(self) -> str:
        """The open form's band select value (`not-set` or a wire token)."""
        return self.driver.get_text("admin-users-invite-age-band-select")

    def set_invite_guardian(self, guardian: str) -> None:
        """Select `guardian` (a handle) on the open form's guardian picker
        without minting — for a test that inspects the band gate first."""
        self.driver.select("admin-users-invite-guardian-select", guardian)

    def mint_invite_code(self, tier: str = "free", uses: int = 1,
                         guardian: str | None = None,
                         age_band: str | None = None) -> str:
        """Fill and confirm the ALREADY OPEN create form (see
        `create_invite_code` for the parameters) and return the NEW code."""
        before = self._invite_code_values()
        self._pick_tier("admin-settings-tier-select", tier)
        if guardian is not None:
            self.driver.select("admin-users-invite-guardian-select", guardian)
        if age_band is not None:
            self.driver.select("admin-users-invite-age-band-select", age_band)
        if uses != 1:
            self.driver.clear_and_type("admin-settings-max-uses-input", str(uses))
        self.driver.wait_for("create-invite-confirm-btn", timeout=10.0)
        self.driver.click("create-invite-confirm-btn")
        deadline = time.monotonic() + 15.0
        after = self._invite_code_values()
        while time.monotonic() < deadline and len(after) <= len(before):
            time.sleep(0.3)
            after = self._invite_code_values()
        # Return the code that is NEW, not the one at a guessed position. The
        # old "last row" read assumed the codes list appends newest-last; linux
        # renders it newest-FIRST, so a second mint handed back the FIRST code
        # instead. That silently mis-attributed a guardian-scoped code to the
        # previous test's guardian. Set-difference is ordering-independent, so it
        # holds on every app.
        fresh = [c for c in after if c not in before]
        if fresh:
            return fresh[0]
        return after[-1] if after else ""

    def _invite_code_values(self) -> list[str]:
        """Every `invite-code-value` currently rendered in the codes list."""
        count = self.driver.count("invite-code-value")
        return [self.driver.get_text("invite-code-value", index=i) for i in range(count)]

    def invite_code_item_text(self, code: str) -> str:
        """The `invite-code-item` row text for `code` — where a minted band
        echoes ("free · 1 · Under 13"; family-safety.md § App surface →
        *Age-band surfaces*: the same row, richer text, no new id). Rows and
        values are parallel lists on every app."""
        values = self._invite_code_values()
        assert code in values, f"invite code {code!r} is not in the rendered list: {values!r}"
        return self.driver.get_text("invite-code-item", index=values.index(code))

    def invite_code_count(self) -> int:
        """Count invite codes in the list."""
        return self.driver.count("invite-code-item")

    def copy_button_visible(self) -> bool:
        """Whether the minted-code copy button is shown (after a mint)."""
        return self.driver.is_visible("admin-users-invite-code-copy-btn")

    def copy_minted_code(self) -> None:
        """Click the minted-code copy button."""
        self.driver.click("admin-users-invite-code-copy-btn")

    # --- Pending invite requests (the Users hub's "Pending requests" section) ---

    def invite_request_count(self) -> int:
        """Count rows in the pending-requests section."""
        return self.driver.count("invite-request-row-handle")

    def invite_request_handles(self) -> list[str]:
        """Requested handles, one per pending-request row."""
        count = self.driver.count("invite-request-row-handle")
        return [self.driver.get_text("invite-request-row-handle", index=i) for i in range(count)]

    def invite_request_row_index(self, handle: str) -> int:
        """The pending-request row carrying ``handle`` — never assume index 0.

        The pending list is shared state: `nest_instance` is ``scope="session"``
        (`conftest.py`), so sibling tests AND every other app parametrization
        seed rows here. Worse, a *failed* approval leaves its request `pending`
        rather than dropping it (`docs/goal/behavior/admin.md` § 2 Section 1 —
        approve re-validates the handle at mint time and the row survives a
        refusal). So index 0 is whatever went wrong earliest, not the request the
        caller just submitted: approving it re-runs someone else's failure once
        per test, which is how one stuck row reads as a whole broken module.
        """
        handles = self.invite_request_handles()
        try:
            return handles.index(handle)
        except ValueError:
            raise AssertionError(
                f"pending request {handle!r} is not in the list; "
                f"handles={handles!r}. {self.pending_requests_diagnosis()}"
            ) from None

    def pending_requests_diagnosis(self) -> str:
        """Why the pending-requests section shows no rows (convention 6 — a
        failure must diagnose itself instead of asserting a bare empty list).

        An empty section has three quite different causes, and the bare
        ``error_text()`` most callers reported names NONE of them: the hub
        routes its failures to the dedicated ``admin-users-action-error``
        (admin.md § Errors), so the global ``error-message`` banner reads ``""``
        whether the fetch exploded or returned nothing at all.

        The discriminating facts, in the order that splits the causes:

        * ``action_error`` non-empty  → the hub's fetch chain THREW; that text
          is the cause and no section is populated.
        * ``section`` 0              → the hub never left its loading arm (or
          never mounted), so nothing was fetched yet — a readiness problem,
          not a data problem.
        * ``users`` > 0 with ``rows`` 0 → the fetch chain COMPLETED (the user
          list, fetched from the same ``refresh()``, is populated) and the
          pending list is genuinely empty: either the nest returned no row, or
          every row came back non-pending, so the client-side ``is_pending``
          filter dropped it. Look nest-side / at the wire, not at the page.
        * ``users`` 0 and ``rows`` 0 → the whole hub is empty; treat as a
          fetch/readiness problem, not a pending-request one.
        """
        try:
            action_error = self.users_action_error_text()
        except Exception as exc:  # driver-level read failure is itself a fact
            action_error = f"<unreadable: {exc}>"

        def _count(test_id: str) -> object:
            try:
                return self.driver.count(test_id)
            except Exception as exc:
                return f"<unreadable: {exc}>"

        def _banner() -> str:
            # The global banner, read defensively — this page routes its own
            # failures elsewhere, so it is a tie-breaker, not the main signal.
            try:
                if self.driver.count("error-message") == 0:
                    return ""
                return self.driver.get_text("error-message").strip()
            except Exception as exc:
                return f"<unreadable: {exc}>"

        return (
            f"action_error={action_error!r} "
            f"section={_count('admin-users-requests-section')} "
            f"list={_count('admin-invite-requests-list')} "
            f"rows={_count('invite-request-row-handle')} "
            f"users={_count('user-row')} "
            f"banner={_banner()!r}"
        )

    def set_request_tier(self, target: str, index: int = 0) -> None:
        """Set a request row's `invite-request-row-tier-select` to `target`.

        Unlike the user-row picker this only sets the *approve-at* tier (no nest
        call until approve), so there's no refetch — the picker shows `target`
        immediately after `select` (or cycle-click on native apps)."""
        self._pick_tier("invite-request-row-tier-select", target, index=index)

    def set_request_guardian(self, guardian: str, index: int = 0) -> None:
        """Set a pending request row's `invite-request-row-guardian-select` to
        `guardian` (a user's label/handle) before approving — the admitted
        account becomes supervised by that guardian (family-safety.md §
        Wire & data shape). Unlike the tier picker this is local UI state
        until approve, so no refetch/re-select loop is needed."""
        self.driver.select("invite-request-row-guardian-select", guardian, index=index)

    def request_age_claim(self, index: int = 0) -> str:
        """The row's `invite-request-row-age-claim` — the applicant's recorded
        claim, or "No app age verification" (absence-as-signal,
        family-safety.md § The account age band D6)."""
        return self.driver.get_text("invite-request-row-age-claim", index=index)

    def request_age_band(self, index: int = 0) -> str:
        """The row's `invite-request-row-age-band-select` value — seeded from
        the applicant's claimed band when the request carries a nameable one,
        else `not-set`."""
        return self.driver.get_text("invite-request-row-age-band-select", index=index)

    def set_request_age_band(self, band: str, index: int = 0) -> None:
        """Set a pending request row's `invite-request-row-age-band-select` to
        `band` (an option VALUE: `u13` / `13-15` / `16-17` / `18+` / `not-set`)
        before approving. Enabled only once the row's guardian is selected —
        call `set_request_guardian` first. Local UI state until approve."""
        self.driver.select("invite-request-row-age-band-select", band, index=index)

    def approve_request(self, index: int = 0) -> None:
        """Approve a pending request at its selected tier (+ guardian + band); the list refetches."""
        self.driver.click("invite-request-row-approve-button", index=index)
        time.sleep(1.5)
