"""The open ``admin-dns`` page's background re-check, as the journeys gate it.

Every app with the page ticks the shared ``fauna_client_dns`` machine's
``background_recheck`` every ``record_recheck_secs()`` while the page is open
(``dns-management.md`` § Manual + live verification — the slow cadence; the
e2e launch compresses it with ``FAUNA_DNS_RECHECK_SECS``). The apps whose tick
has landed are listed here; each remaining app's lift adds itself.
"""

from __future__ import annotations

from helpers.app_surface import app_name, skip_unbuilt

# The apps whose open `admin-dns` page ticks `background_recheck`.
DNS_RECHECK_BUILT_APPS = {"tui", "linux", "web", "android"}

# Where each remaining app's tick is captured.
_TRACKED = {
    "windows": "",
    "macos": "",
    "ios": "",
}


def require_dns_background_recheck(driver) -> None:
    """Skip (``skip_unbuilt``) on an app whose open page does not tick yet."""
    name = app_name(driver)
    if name in DNS_RECHECK_BUILT_APPS:
        return
    skip_unbuilt(
        driver,
        surface="admin-dns background re-check cadence",
        detail=(
            "the open page's tick of fauna_client_dns::background_recheck "
            "every record_recheck_secs()"
        ),
        tracked=_TRACKED.get(
            name, "docs/goal/behavior/dns-management.md § Implementation status today"
        ),
    )
