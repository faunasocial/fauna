"""Browser resolution for every web e2e surface (web bridge + tier_4 smokes).

Playwright's **bundled** Chromium is the canonical e2e browser on every machine
(`docs/goal/architecture/testing.md` § Cross-app e2e conventions, point 9).
It has no machine-wide singleton, so concurrent browser runs — sibling
sessions' web lanes, a tier_4 docker smoke beside a ``--client web`` run, even
a second web driver in one process — are all safe. The snap-Chromium
serialization flock this replaces (``drivers/chromium_lock.py``, removed
2026-07-14) existed only because ``playwright install chromium`` used to be
refused on ubuntu26.04-arm64; the ubuntu24.04-arm64 build downloads and runs
fine with ``PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04-arm64`` set at
*install* time (runtime needs nothing) — see the per-machine setup notes.

Resolution order:

1. ``PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH`` — explicit override, always wins.
2. Playwright's bundled Chromium, when installed.
3. A system browser (``chromium-browser``/``chromium``/``google-chrome``) —
   fallback for a machine missing the bundled install. ⚠ On Ubuntu the system
   chromium is a snap enforcing a machine-wide ProcessSingleton; with no flock
   serializing it any more, concurrent runs there can crash into each other
   ("Failed to create a ProcessSingleton"). The fallback keeps a
   half-configured machine limping single-lane; the fix is the bundled
   install, and the no-browser error below carries that instruction.
"""

from __future__ import annotations

import os
import shutil

_INSTALL_HINT = (
    "install Playwright's bundled Chromium into the e2e venv: "
    "`<venv>/bin/playwright install chromium` (on a distro Playwright's "
    "allowlist doesn't know yet, e.g. ubuntu26.04-arm64, prefix with "
    "PLAYWRIGHT_HOST_PLATFORM_OVERRIDE=ubuntu24.04-arm64 — see the "
    "per-machine setup notes)"
)


def launch_kwargs(playwright, *, headless: bool = True) -> dict:
    """Chromium ``launch()`` kwargs for the resolution order above.

    Takes a started Playwright instance (``sync_playwright().start()`` or the
    ``with sync_playwright() as p`` object) — the bundled-browser probe reads
    ``playwright.chromium.executable_path``. An empty ``executable_path`` in
    the result means "use the bundled build" (Playwright's default).
    """
    kwargs: dict = {"headless": headless}
    explicit = os.environ.get("PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH")
    if explicit:
        kwargs["executable_path"] = explicit
        return kwargs
    try:
        bundled = playwright.chromium.executable_path
    except Exception:
        bundled = None
    if bundled and os.path.exists(bundled):
        return kwargs
    for name in ("chromium-browser", "chromium", "google-chrome"):
        found = shutil.which(name)
        if found:
            kwargs["executable_path"] = found
            return kwargs
    raise RuntimeError(f"No Chromium found for web e2e — {_INSTALL_HINT}.")
