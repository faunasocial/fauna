from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver

#: Re-reads of the result column after a row vanished mid-read. Each torn read
#: needs a commit landing inside one short read, so a few always suffice.
_TORN_READ_RETRIES = 5


class SearchActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the search page."""
        self.driver.navigate_to("search")
        self.driver.wait_for("search-query-field", timeout=10)

    def query(self, text: str) -> None:
        """Type a search query and submit."""
        self.driver.clear_and_type("search-query-field", text)
        self.driver.click("search-submit-button")
        time.sleep(1)

    def clear(self) -> None:
        """Clear the search query."""
        self.driver.click("search-clear-button")
        time.sleep(0.5)

    def cancel(self) -> None:
        """Cancel and dismiss search."""
        self.driver.click("search-cancel-button")

    def toggle(self) -> None:
        """Toggle search visibility."""
        self.driver.click("search-toggle-button")

    def result_count(self) -> int:
        """Return the number of search result items."""
        return self.driver.count("search-result-item")

    def has_results(self) -> bool:
        """Check if search results are displayed."""
        return self.driver.is_visible("search-results-view")

    def has_no_results(self) -> bool:
        """Check if the no-results message is displayed."""
        return self.driver.is_visible("search-no-results")

    def result_text(self, index: int = 0) -> str:
        """Get the text of a search result by index."""
        return self.driver.get_text("search-result-item", index=index)

    def result_texts(self) -> list[str]:
        """Every painted `search-result-item`'s text, in row order, so a caller
        can assert something of EACH row rather than of the first.

        The page is live: a search that commits between the row count and a
        row's read (a narrowing filter's re-fire, say — 2 rows become 1) leaves
        an index that no longer exists. That is a torn read, never an answer,
        so the whole column is read again; a partial list is never returned,
        because a caller asking "only kind X?" would accept the surviving
        prefix of the OLD page."""
        for _ in range(_TORN_READ_RETRIES):
            try:
                return self.driver.get_texts("search-result-item")
            except LookupError:
                continue
        return self.driver.get_texts("search-result-item")

    def set_type_filter(self, token: str) -> None:
        """Narrow the search to one kind through `search-type-filter`.

        `token` is the filter's wire token — one of
        `fauna_client_search::TYPE_FILTER_OPTIONS` (`all`, `post`, `imap`,
        `profile`), which is the option VALUE every app paints (the human
        label rides beside it). Choosing an option re-fires the last query
        with the live query text on every app (`ui/search.md` § User
        actions), so there is no submit to press after it."""
        self.driver.select("search-type-filter", token)

    def open_result(self, index: int = 0) -> None:
        """Activate the `search-result-item` at `index` — navigates to its
        typed target on apps that act on `SearchNav` (tui first;
        ui/search.md § User actions). All five variants (`Post`/`Draft`/
        `Mail`/`Contact`/`File`) are wired on all 7 apps as of."""
        self.driver.click("search-result-item", index=index)
