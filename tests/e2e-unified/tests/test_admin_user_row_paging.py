"""tier_1: the Users hub's row lookup must find an account on ANY page.

Pure in-process exercise of ``actions/admin.py::AdminActions.user_row_index``
against a scripted paginated driver. No nest, no app, no cargo.

The Users list renders one ``fauna.admin.users.list`` page at a time (``admin.md``
§ 2 Section 5), newest first. A whole-suite run admits more than one default page
of accounts to the session nest, so the session admin — the oldest account, the
row every cut-off and roster test reads — leaves page 1 part-way through the
sweep. A lookup that read only the rendered page then failed four hub tests on
every linux sweep with "actor … not in the user list" over exactly 50 rows.

What is pinned: the lookup rewinds to the first page, walks forward through the
next-page control until the row turns up, returns that row's index ON the page it
leaves the list on, and — when no page holds the row — fails naming every row it
read. No timing is asserted (convention 14).
"""

import pytest

from actions.admin import AdminActions

pytestmark = [pytest.mark.tier_1]


class _PagedDriver:
    """A Users list split into fixed pages, with live prev/next controls.

    The element IDs are CLASS attributes, not literals inside the methods:
    ``kind_reach`` keys its call graph on the bare method name, so a literal in
    a method named ``count`` / ``click`` / ``get_text`` would hand every real
    driver call the admin shell's IDs and exclude every ``logged_in_app`` test
    from live.
    """

    ROW = "user-actor-id"
    PAGINATION = "admin-users-pagination"
    PREV = "admin-users-prev-page"
    NEXT = "admin-users-next-page"

    def __init__(self, pages: list[list[str]], start: int = 0):
        self.pages = pages
        self.page = start
        self.clicks: list[str] = []

    def count(self, element_id: str) -> int:
        if element_id == self.ROW:
            return len(self.pages[self.page])
        if element_id == self.PAGINATION:
            return 1
        return 0

    def get_text(self, element_id: str, index: int = 0) -> str:
        assert element_id == self.ROW, element_id
        return self.pages[self.page][index][:12] + "…"

    def is_enabled(self, element_id: str) -> bool:
        if element_id == self.PREV:
            return self.page > 0
        if element_id == self.NEXT:
            return self.page < len(self.pages) - 1
        raise AssertionError(element_id)

    def wait_until_enabled(self, element_id: str, timeout: float) -> None:
        assert self.is_enabled(element_id), f"drove a disabled {element_id}"

    def click(self, element_id: str) -> None:
        self.clicks.append(element_id)
        self.page += 1 if element_id == self.NEXT else -1


def _hex(n: int) -> str:
    # Distinct in the first 12 hex chars — the truncated prefix the row shows.
    return f"{n:012x}" + "ab" * 26


def _paged(total: int, per_page: int) -> list[list[str]]:
    ids = [_hex(0xA000 + n) for n in range(total)]
    return [ids[i:i + per_page] for i in range(0, total, per_page)]


def test_finds_the_oldest_account_past_the_first_page():
    pages = _paged(120, 50)
    admin_hex = pages[-1][-1]
    driver = _PagedDriver(pages)
    row = AdminActions(driver).user_row_index(admin_hex)
    assert driver.page == 2, "the lookup must leave the list on the row's page"
    assert row == len(pages[2]) - 1
    assert driver.get_text("user-actor-id", row) == admin_hex[:12] + "…"


def test_rewinds_before_walking_so_an_earlier_page_is_not_missed():
    pages = _paged(120, 50)
    wanted = pages[0][3]
    driver = _PagedDriver(pages, start=2)
    row = AdminActions(driver).user_row_index(wanted)
    assert (driver.page, row) == (0, 3)


def test_a_single_page_list_clicks_nothing():
    pages = _paged(7, 50)
    driver = _PagedDriver(pages)
    assert AdminActions(driver).user_row_index(pages[0][6]) == 6
    assert driver.clicks == []


def test_absent_everywhere_fails_naming_every_row_read():
    pages = _paged(60, 50)
    driver = _PagedDriver(pages)
    with pytest.raises(AssertionError, match=r"not on any page of the user list; 60 rows read"):
        AdminActions(driver).user_row_index(_hex(0xFFFF))
