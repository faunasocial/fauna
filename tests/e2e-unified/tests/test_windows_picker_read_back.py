"""tier_3 e2e: the windows FlaUI bridge reads a picker's value back the same way
for both item shapes a WinUI ComboBox carries — a keyed option and a data-bound
one — on the picker's first render, after a select, and after the page rebuilds
the picker closed.

**The rule pinned** is convention 11's windows paragraph
(``docs/goal/architecture/e2e-conventions.md``, *What counts as "offered"*):

* a **keyed** option — built by ``FaunaApp/Helpers/PickerOption.cs``, its UIA
  Name the painted label and its AutomationId the stable key — reads back as its
  KEY (``ElementFinder.GetText``), and ``select(id, key)`` picks it;
* a **data-bound** option — an ``ItemsSource`` of strings, carrying no
  AutomationId of its own — reads back as the LABEL it paints, and
  ``select(id, label)`` picks it. A closed picker's selected data-bound item
  answers its template part's x:Name, ``ContentPresenter``, as its
  AutomationId; that is not a key the app set, and the bridge reads it as none
  (``ElementFinder.IsTemplatePartFallback``, in ``GetText`` and in ``Select``'s
  match alike) — so ``select(id, "ContentPresenter")`` is refused, never matched.

**Why a pin.** On 2026-11-06 the bridge began answering a selected item's
AutomationId before its Name, verified on the keyed pickers it was written for.
Every data-bound picker then read back ``ContentPresenter`` and six windows tests
on four pages went red for four days, until a later full run found them and
``IsTemplatePartFallback`` landed (2026-11-10). A change to
the read rule was graded only by whichever tests happened to read a picker of the
shape its author had in mind; this test reads both, so the next change to the
rule meets both before it lands.

**The host.** The UIA read needs a real WinUI ComboBox: tier_1 cannot host one,
and the bridge's ``SelfTest.cs`` has no ComboBox fixture (its self-tests grade
pure functions over no window), so this is a windows-marked tier_3 test on the
cheapest page that paints both shapes side by side — a hosted domain's row on
``admin-dns``. Its DMARC policy picker (``admin-dns-domain-dmarc-policy-select``)
is keyed by the RFC 7489 tag (``AdminDnsPage.xaml.cs``, ``PickerOption.Create``);
its catch-all picker (``admin-dns-domain-catch-all-select``) is bound to a list of
strings (``AdminDnsPage.xaml``, ``ItemsSource="{x:Bind CatchAllOptions}"``). Each
arm first proves its subject still has its shape — the keyed picker reads back a
value that is NOT among the labels it paints, the data-bound one reads back one
that is — so a page change that re-keys the catch-all picker fails here by name,
asking for the arm to move to another data-bound picker, rather than leaving the
shape silently unpinned.

**Red-verified** (2026-11-10) against the bridge with ``IsTemplatePartFallback``
answering false: the read half fails on the first render, before any select,
``last read {'keyed': 'reject', 'data_bound': 'ContentPresenter'}``. The select
half held in that run all the same — ``select(catch-all, "ContentPresenter")``
was still refused on a fresh picker and on a designated one, because the items
of an OPEN popup carry no AutomationId; only the closed picker's selected item
answers the x:Name. So step 4 guards ``Select``'s match against that changing
(a WinUI or bridge change that names popup items by the x:Name), not against
today's shape.

tier_3: real ``fauna-nest`` and the real windows app via ``admin_app``; windows
only (it pins the FlaUI bridge's own read).
"""

import secrets

import pytest

from drivers.http_bridge import SelectOptionNotOffered
from helpers.admin_wire import (
    admin_user,
    catch_all_actor_for,
    picker_option,
    seed_local_domain,
)
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.windows]

KEYED = "admin-dns-domain-dmarc-policy-select"
DATA_BOUND = "admin-dns-domain-catch-all-select"

# WinUI's x:Name fallback for an item with no AutomationId of its own: the
# ComboBoxItem template's content part.
TEMPLATE_PART = "ContentPresenter"

# The keyed picker's labels, in the shared catalog's paint order — and the keys
# (RFC 7489 tags) they stand for.
DMARC_LABELS = [
    S.admin.dns.dmarc_policy_reject,
    S.admin.dns.dmarc_policy_quarantine,
    S.admin.dns.dmarc_policy_none,
]


def _page_error(admin_app) -> str:
    """The page's error text, or "none" — ``error-message`` is not on the page
    while nothing has failed, and a diagnose must never raise in its place."""
    return admin_app.driver.get_text("error-message") if admin_app.has_error() else "none"


def _row_index(admin_app, domain: str) -> int | None:
    names = admin_app.admin.dns_domain_names()
    return names.index(domain) if domain in names else None


def _reads(admin_app, domain: str) -> dict | None:
    """What both CLOSED pickers on ``domain``'s row read back; ``None`` while
    the row is not on the page. A pure read: ``get_text`` opens nothing."""
    i = _row_index(admin_app, domain)
    if i is None:
        return None
    return {
        "keyed": admin_app.driver.get_text(KEYED, index=i) or "",
        "data_bound": admin_app.driver.get_text(DATA_BOUND, index=i) or "",
    }


def _assert_both_read_back(admin_app, domain: str, *, key: str, label: str, state: str):
    """Wait until the keyed picker reads back ``key`` and the data-bound one
    ``label``, then check each arm still has the shape it pins.

    The wait polls the closed pickers alone. The labels are read only once it is
    over: the bridge's ``options`` read opens the popup (its items realize only
    while it is open), and a picker opened and closed between two polls is no
    longer the picker the page built closed — the state this test exists to read.
    """
    last: dict = {}

    def settled():
        read = _reads(admin_app, domain)
        last.clear()
        last.update(read or {"row": "not on the page"})
        if read and read["keyed"] == key and read["data_bound"] == label:
            return read
        return None

    read = wait_until(
        settled,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"{state}: expected the keyed DMARC picker to read back its key {key!r} "
            f"and the data-bound catch-all picker its painted label {label!r}; "
            f"last read {last!r} (a {TEMPLATE_PART!r} read is WinUI's x:Name "
            f"fallback answered as a key — ElementFinder.IsTemplatePartFallback); "
            f"page error: {_page_error(admin_app)!r}"
        ),
    )
    i = _row_index(admin_app, domain)
    read["keyed_labels"] = admin_app.driver.option_texts(KEYED, index=i)
    read["data_bound_labels"] = admin_app.driver.option_texts(DATA_BOUND, index=i)
    assert read["keyed_labels"] == DMARC_LABELS, (
        f"{state}: the keyed DMARC picker should paint the catalog's labels "
        f"{DMARC_LABELS!r} (option_texts lists painted labels on windows); "
        f"got {read['keyed_labels']!r}"
    )
    assert key not in read["keyed_labels"], (
        f"{state}: the keyed arm's key {key!r} is also one of its painted labels "
        f"{read['keyed_labels']!r}, so its read-back no longer proves the "
        f"AutomationId path — the DMARC picker is no longer keyed apart from its labels"
    )
    assert read["data_bound_labels"] and label in read["data_bound_labels"], (
        f"{state}: the data-bound arm read back {label!r}, which is not among the "
        f"labels its picker paints {read['data_bound_labels']!r} — if the catch-all "
        f"picker is now keyed (PickerOption), move this arm to a picker still bound "
        f"to a list of strings"
    )


def test_the_bridge_reads_keyed_and_data_bound_pickers_back_before_and_after_a_closed_rebuild(
    admin_app, nest_instance
):
    """Both shapes read back by their own rule on first render, after a select,
    and after the page is opened afresh (every picker rebuilt closed from nest
    state — the state that read ``ContentPresenter``); the template part's
    x:Name is never selectable as a key."""
    domain = f"pickerread-{secrets.token_hex(4)}.test"
    seed_local_domain(nest_instance, domain)
    option = picker_option(admin_user(nest_instance))
    admin_app.admin.navigate_dns()

    # 1. First render: a fresh domain runs the default policy and has no catch-all.
    _assert_both_read_back(
        admin_app, domain, key="reject", label=S.admin.dns.catch_all_none,
        state="first render",
    )

    # 2. After a select: the data-bound picker by its label, the keyed one by its key.
    admin_app.admin.set_catch_all(option, index=_row_index(admin_app, domain))
    wait_until(
        lambda: catch_all_actor_for(nest_instance, domain) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"selecting {option!r} on {domain!r}'s catch-all never reached the nest",
    )
    admin_app.admin.set_dmarc_policy("quarantine", index=_row_index(admin_app, domain))
    _assert_both_read_back(
        admin_app, domain, key="quarantine", label=option, state="after a select",
    )

    # 3. Rebuilt closed: open the page afresh, so neither picker remembers a click.
    admin_app.driver.set_state({"nav": {"stack": [{"view": "admin"}]}})
    wait_until(
        lambda: admin_app.driver.is_visible("admin-nav-row[admin-dns]") or None,
        UI_SETTLE_S,
        diagnose=lambda: "the admin shell never came back after leaving the Domains page",
    )
    admin_app.admin.navigate_dns()
    _assert_both_read_back(
        admin_app, domain, key="quarantine", label=option, state="rebuilt closed",
    )

    # 4. The template part's x:Name names no option: refused, and nothing moved.
    # (Holds today even without the predicate — the open popup's items carry no
    # AutomationId — so this guards Select's match against that changing.)
    designated = catch_all_actor_for(nest_instance, domain)
    with pytest.raises(SelectOptionNotOffered):
        admin_app.driver.select(DATA_BOUND, TEMPLATE_PART, index=_row_index(admin_app, domain))
    assert catch_all_actor_for(nest_instance, domain) == designated, (
        f"select({DATA_BOUND!r}, {TEMPLATE_PART!r}) was refused yet {domain!r}'s "
        f"catch-all moved from {designated!r} to "
        f"{catch_all_actor_for(nest_instance, domain)!r}"
    )
    _assert_both_read_back(
        admin_app, domain, key="quarantine", label=option, state="after the refused select",
    )
