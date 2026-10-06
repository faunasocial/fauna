from __future__ import annotations

from typing import TYPE_CHECKING

from helpers.waiting import wait_until

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class WebSettingsActions:
    """Drive the user-facing "Web" Settings sub-page (``web-settings``).

    Two surfaces on one page (``docs/goal/behavior/web-content-hosting.md``
    § Published-post management):

    - the **subdomain opt-in toggle** (``web-settings-subdomain-toggle``), which
      is also what gives the published-post links an origin at all; and
    - the **Published-posts management section** —
      ``web-published-posts-list`` of ``web-published-post-item`` rows (slug +
      gated-tier badge) from ``fauna.web.publish.list``, each offering copy-link
      / copy-paywall-link (gated rows only) / unpublish, plus
      ``web-published-posts-empty``.

    tui is the lead app (``web-content-hosting.md`` § Published-post management
    → *Rollout*); linux/web/android follow, then the windows/apple trickle-downs.

    **Reading a copied link.** No driver on any app reads the OS clipboard, so
    each copy affordance carries the exact string it wrote as its ``copied``
    attribute — written from the same value that reached the clipboard, never
    re-derived. That is what makes ``copied_link()`` an assertion about the
    CONTENTS rather than about a button existing (the devices-page lesson: an
    unasserted copy affordance rots invisibly).
    """

    # A settings sub-page nav is a fire-and-forget `set_state` patch, and this
    # page's hydrate is THREE round trips (subdomain flag, domain rows, publish
    # list) before a single row exists. Sized far above any non-pathological
    # render rather than to a quiet machine — testing.md § convention 14: a
    # generous positive budget costs a green run nothing, because every wait
    # below is a deadline poll that exits the moment the page is up.
    PAGE_READY_BUDGET_S = 60.0
    # A UI-driven mutation's round trip (a takedown re-reads the list; a paywall
    # link mints). Same convention-14 reasoning.
    MUTATION_BUDGET_S = 60.0

    ROW = "web-published-post-item"

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self, *, timeout: float | None = None) -> None:
        """Navigate to the Web settings sub-page and block until it has rendered.

        The barrier is the point: a caller that navigates and immediately reads a
        row would be racing the page's construction, which is a *causal* race and
        not a slow one.

        Waits on the toggle ONLY — not every app builds the Published-posts
        section yet (macos/ios still toggle-only), so a caller that also needs
        that section hydrated must wait for it explicitly via
        `wait_for_published_posts_section()` rather than this barrier growing
        a dependency the toggle-only apps can never satisfy.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "web"}]},
        })
        self.driver.wait_for(
            "web-settings-subdomain-toggle", timeout=timeout or self.PAGE_READY_BUDGET_S
        )

    def wait_for_published_posts_section(self, *, timeout: float | None = None) -> None:
        """Block until the Published-posts section has hydrated (either the
        empty state or the row container is present) — for apps that build it.

        The toggle paints immediately with a default-OFF guess, independently
        of the page's async hydrate; on an app where that hydrate lands
        strictly after first paint (web — the toggle is visible well before
        `publish.list` resolves) a caller reading
        `is_empty_state_visible()`/`published_slugs()` right after
        `navigate()` would race the hydrate rather than wait on it.
        """
        wait_until(
            lambda: self.driver.is_visible("web-published-posts-empty")
            or self.driver.is_visible("web-published-posts-list"),
            timeout or self.PAGE_READY_BUDGET_S,
            diagnose=lambda: (
                "the Published-posts section never rendered (neither "
                "web-published-posts-empty nor web-published-posts-list); "
                f"error={self.driver.get_text('error-message') if self.driver.is_visible('error-message') else None!r}"
            ),
        )

    # ── The subdomain opt-in (the published links' origin) ────────────────

    def subdomain_enabled(self) -> bool:
        return self.driver.get_attr("web-settings-subdomain-toggle", "state") == "on"

    def set_subdomain_enabled(self, enabled: bool) -> None:
        """Flip the opt-in through the UI and wait for the NEST-confirmed echo.

        The ``state`` attr is written only from a nest-confirmed view, never from
        the click, so waiting on it is a round-trip proof rather than an echo of
        the gesture.
        """
        if self.subdomain_enabled() == enabled:
            return
        self.driver.click("web-settings-subdomain-toggle")
        want = "on" if enabled else "off"
        wait_until(
            lambda: self.driver.get_attr("web-settings-subdomain-toggle", "state") == want,
            self.MUTATION_BUDGET_S,
            diagnose=lambda: (
                f"subdomain toggle never reflected {want!r} after the nest round-trip; "
                f"error={self.driver.get_text('error-message') if self.driver.is_visible('error-message') else None!r}"
            ),
        )

    def subdomain_url_text(self) -> str:
        """``web-settings-subdomain-url`` — the live site URL, or the reason
        there isn't one.

        The row is the OLDEST surface on this page and the one every app renders,
        which is why it is worth asserting separately from the copy affordances:
        the two are projected from the same ``subdomain_view``, so an app that
        composes the URL on the wrong host is wrong here first
        (``web-content-hosting.md`` § Published-post management).

        Empty when the app paints no row at all — tui omits the element rather
        than registering it blank, so a caller asserting a URL must not read
        emptiness as "the row said nothing".
        """
        if not self.driver.is_visible("web-settings-subdomain-url"):
            return ""
        return self.driver.get_text("web-settings-subdomain-url")

    # ── The render status (a site the nest took dark) ─────────────────────

    def render_status_text(self) -> str:
        """``web-settings-render-status`` — the line painted only while the nest
        has taken this user's rendered pages down. Empty when absent, which on
        a hydrated page means the site is healthy: read it only after
        `wait_for_published_posts_section()`, since the flag arrives with the
        same ``publish.list`` read that hydrates that section."""
        if not self.driver.is_visible("web-settings-render-status"):
            return ""
        return self.driver.get_text("web-settings-render-status")

    def wait_for_render_status_absent(self) -> None:
        wait_until(
            lambda: not self.driver.is_visible("web-settings-render-status"),
            self.MUTATION_BUDGET_S,
            diagnose=lambda: (
                "the render status never cleared; "
                f"text={self.render_status_text()!r}"
            ),
        )

    # ── The published-posts section ───────────────────────────────────────

    def is_empty_state_visible(self) -> bool:
        return not self.driver.is_absent("web-published-posts-empty")

    def published_slugs(self) -> list[str]:
        """The slug of every painted row, in paint order. Scoped per row, never a
        global count — a bare ``get_text`` would silently read row 0 forever."""
        slugs = []
        for i in range(self.row_count()):
            slugs.append(self.driver.get_text("web-published-post-slug", scope=f"{self.ROW}[{i}]"))
        return slugs

    def row_count(self) -> int:
        i = 0
        while self.driver.is_visible("web-published-post-slug", scope=f"{self.ROW}[{i}]"):
            i += 1
        return i

    def row_index_for(self, slug: str) -> int:
        for i, painted in enumerate(self.published_slugs()):
            if painted == slug:
                return i
        raise AssertionError(
            f"no published-post row for slug {slug!r}; painted rows: {self.published_slugs()}"
        )

    def gated_tier(self, index: int) -> str:
        """The row's gating tier as DATA (``''`` when ungated) — read off the row
        rather than by matching a translated badge string. Reads straight off the
        row's own attribute via ``index=`` (not ``scope=self.ROW[index]``, which
        would nest the same id inside itself and never resolve)."""
        return self.driver.get_attr(self.ROW, "gated-tier", index=index) or ""

    def offers_paywall_link(self, index: int) -> bool:
        return not self.driver.is_absent(
            "web-published-post-copy-paywall-link-button", scope=f"{self.ROW}[{index}]"
        )

    def copy_link_enabled(self, index: int) -> bool:
        """Whether *Copy web link* is live. Dead ⇒ the actor has no serving
        origin, and the reason is on screen beside it — publishing with no origin
        is legal but unreachable, and the UI says so rather than handing out a
        link that cannot load."""
        return (
            self.driver.get_attr(
                "web-published-post-copy-link-button",
                "disabled",
                scope=f"{self.ROW}[{index}]",
            )
            != "true"
        )

    def copy_web_link(self, index: int) -> str:
        """Click *Copy web link* on row ``index`` and return the string that
        actually reached the clipboard."""
        self.driver.click("web-published-post-copy-link-button", scope=f"{self.ROW}[{index}]")
        return self._copied("web-published-post-copy-link-button", index)

    def copy_paywall_link(self, index: int) -> str:
        """Click *Copy paywall link* on row ``index`` (gated rows only) and return
        the minted, tokened URL that reached the clipboard. The mint is a round
        trip, so the value appears only once the nest answers."""
        self.driver.click(
            "web-published-post-copy-paywall-link-button", scope=f"{self.ROW}[{index}]"
        )
        return self._copied("web-published-post-copy-paywall-link-button", index)

    def unpublish(self, index: int) -> None:
        """Take row ``index``'s page down. One tap — the verb is idempotent and
        reversible, so it carries no destructive-confirm step."""
        self.driver.click("web-published-post-unpublish-button", scope=f"{self.ROW}[{index}]")

    def wait_for_slug_absent(self, slug: str) -> None:
        wait_until(
            lambda: slug not in self.published_slugs(),
            self.MUTATION_BUDGET_S,
            diagnose=lambda: (
                f"{slug!r} never left the published list after the takedown; "
                f"rows: {self.published_slugs()}"
            ),
        )

    def wait_for_slug(self, slug: str) -> int:
        # Wait for BOTH the row's own arrival AND the empty state's departure
        # as one settled condition, not two back-to-back checks: a row
        # appearing and its sibling empty-state element being torn down are
        # the same underlying transition. On apple (macOS/iOS `WebSettingsView`)
        # a client's `Form`/`List`-backed row can keep reporting live,
        # on-screen geometry for a branch that is no longer logically
        # selected (`Form`'s `UITableView` row-reuse pool re-attaching a
        # sentinel to a different cell) — checking only the row let a
        # caller's very next assertion, `not is_empty_state_visible()`, catch
        # that as a spurious failure on a genuinely successful publish.
        wait_until(
            lambda: (slug in self.published_slugs()) and not self.is_empty_state_visible(),
            self.PAGE_READY_BUDGET_S,
            diagnose=lambda: (
                f"{slug!r} never appeared in the published list; rows: {self.published_slugs()}; "
                f"empty_state_visible={self.is_empty_state_visible()}; "
                f"error={self.driver.get_text('error-message') if self.driver.is_visible('error-message') else None!r}"
            ),
        )
        return self.row_index_for(slug)

    def _copied(self, button_id: str, index: int) -> str:
        value = wait_until(
            lambda: self.driver.get_attr(button_id, "copied", scope=f"{self.ROW}[{index}]")
            or None,
            self.MUTATION_BUDGET_S,
            diagnose=lambda: (
                f"{button_id}[{index}] never reported what it copied; "
                f"error={self.driver.get_text('error-message') if self.driver.is_visible('error-message') else None!r}"
            ),
        )
        return value
