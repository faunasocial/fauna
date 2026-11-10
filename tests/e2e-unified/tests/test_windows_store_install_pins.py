"""Every id the Windows store-install nodes read is one a Windows view draws, in
the flavour that keeps or drops it.

The Microsoft Store delivery is driven by UI Automation alone
(``packaging-witnesses.md`` § Implementation status today, how the Windows Store
install is driven), and its ``store-install`` outcomes are mostly absence checks
(``tests/artifact/test_windows_store_install.py``). An absence asserted of an id no
Windows view draws would pass on a full build as well, so this file pins, before
any build runs or any package is installed:

- each id the nodes assert ABSENT (``STORE_ABSENT_*`` in
  ``helpers/windows_store_install.py``) is spelled by a XAML under
  ``Views\\Payments\\`` — the item group ``FaunaExcisePayments`` removes, so a full
  build draws it and the store-no-payments build cannot — and by no XAML anywhere
  else, which would ship it in both flavours;
- each id they read as KEPT (``STORE_KEPT_IN_PERSON_SHARE``) is spelled by a XAML
  under ``Views\\P2pShare\\``, which only ``FaunaExciseP2pShare`` removes, and the
  store-no-payments arm of ``windows-store-package`` does not set that;
- each id they click or anchor on (``STORE_DRIVEN_IDS``) is spelled by a XAML
  outside ``Views\\Payments\\``, and each Settings rail row they open is a rail tag
  ``SettingsShellPage`` composes into ``settings-nav-row[<key>]``.

The iOS twin is ``test_ios_store_driver_pins.py``. Static only: reads repository
files, builds and launches nothing (tier_1).
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from helpers import windows_store_install as store

pytestmark = pytest.mark.tier_1

ROOT = Path(__file__).resolve().parents[3]
APP = ROOT / "apps/fauna-windows/FaunaApp/FaunaApp"
UI_IDS = APP / "Generated/UiIds.cs"
CSPROJ = APP / "FaunaApp.csproj"
PAYMENTS = APP / "Views/Payments"
P2P_SHARE = APP / "Views/P2pShare"
SETTINGS_SHELL = APP / "Views/SettingsShellPage.xaml"
JUSTFILE = ROOT / "justfile"

_CONST = re.compile(r'public const string (\w+) = "([^"]+)";')
#: An `AutomationProperties.AutomationId` spelled through the generated ids or as
#: a literal; any other binding names no fixed id.
_AUTOMATION_ID = re.compile(r'AutomationId="(?:\{x:Bind ids:Ids\.(\w+)\}|([^"{]+))"')


def _members() -> dict[str, str]:
    """The generated `Ids` members and the ui.yaml ids they hold."""
    return dict(_CONST.findall(UI_IDS.read_text(encoding="utf-8")))


def _xaml_files():
    for path in APP.rglob("*.xaml"):
        if {"obj", "bin"} & set(path.relative_to(APP).parts):
            continue
        yield path


def spelled_ids(text: str, members: dict[str, str]) -> set[str]:
    """The fixed AutomationIds one XAML file spells."""
    found: set[str] = set()
    for member, literal in _AUTOMATION_ID.findall(text):
        if member:
            if member in members:
                found.add(members[member])
        else:
            found.add(literal)
    return found


def _ids_by_place() -> tuple[set[str], set[str], set[str]]:
    """(ids spelled under Views/Payments, under Views/P2pShare, anywhere else)."""
    members = _members()
    payments: set[str] = set()
    p2p_share: set[str] = set()
    elsewhere: set[str] = set()
    for path in _xaml_files():
        ids = spelled_ids(path.read_text(encoding="utf-8"), members)
        if PAYMENTS in path.parents:
            payments |= ids
        elif P2P_SHARE in path.parents:
            p2p_share |= ids
        else:
            elsewhere |= ids
    return payments, p2p_share, elsewhere


def test_the_scanner_reads_both_spellings():
    members = {"PostTipTotal": "post-tip-total"}
    text = (
        '<TextBlock AutomationProperties.AutomationId="{x:Bind ids:Ids.PostTipTotal}" />\n'
        '<Grid AutomationProperties.AutomationId="subscription-offer-row" />\n'
        '<Grid AutomationProperties.AutomationId="{x:Bind Key}" />\n'
    )
    assert spelled_ids(text, members) == {"post-tip-total", "subscription-offer-row"}


def test_the_flavour_drops_payments_and_keeps_the_in_person_share():
    """The two removals the pins below rely on: `FaunaExcisePayments` removes
    `Views\\Payments\\`, `FaunaExciseP2pShare` removes `Views\\P2pShare\\`, and the
    store-no-payments arm of `windows-store-package` sets the first alone."""
    csproj = CSPROJ.read_text(encoding="utf-8")
    assert re.search(
        r"<ItemGroup Condition=\"'\$\(FaunaExcisePayments\)'=='true'\">\s*"
        r"<Page Remove=\"Views\\Payments\\\*\*\\\*\.xaml\" />", csproj), (
        "FaunaApp.csproj no longer removes Views\\Payments\\ under FaunaExcisePayments"
    )
    assert re.search(
        r"<ItemGroup Condition=\"'\$\(FaunaExciseP2pShare\)'=='true'\">\s*"
        r"<Page Remove=\"Views\\P2pShare\\\*\*\\\*\.xaml\" />", csproj), (
        "FaunaApp.csproj no longer removes Views\\P2pShare\\ under FaunaExciseP2pShare alone"
    )
    recipe = JUSTFILE.read_text(encoding="utf-8")
    recipe = recipe[recipe.index("\nwindows-store-package "):]
    # The arm that sets the flavour's settings: a `store-no-payments)` line of its
    # own (the validation case before it lists the three flavours on one line).
    arm = recipe[re.search(r"^\s*store-no-payments\)\s*$", recipe, re.M).start():]
    arm = arm[:arm.index(";;")]
    assert 'PUBLISH_FLAVOR="-p:FaunaExcisePayments=true"' in arm, arm
    assert "FaunaExciseP2pShare" not in arm, arm
    assert store.STORE_FLAVOUR == "store-no-payments"


def test_every_absent_id_is_one_only_the_payments_views_draw():
    payments, _, elsewhere = _ids_by_place()
    undrawn = sorted(set(store.STORE_ABSENT) - payments)
    assert not undrawn, (
        f"the store-install nodes assert {undrawn} absent, but no XAML under Views/Payments "
        f"spells them — a full build would not draw them either, so the absence proves "
        f"nothing. Drop them, or name the id the full build really draws."
    )
    leaked = sorted(set(store.STORE_ABSENT) & elsewhere)
    assert not leaked, (
        f"{leaked} are spelled by a XAML outside Views/Payments, so the store-no-payments "
        f"build ships them too (FaunaApp.csproj, the payments excision)"
    )


def test_every_kept_id_is_drawn_by_the_in_person_share_views():
    _, p2p_share, _ = _ids_by_place()
    missing = sorted(set(store.STORE_KEPT_IN_PERSON_SHARE) - p2p_share)
    assert not missing, f"no XAML under Views/P2pShare spells {missing}"


def test_every_driven_id_survives_the_store_flavour():
    payments, p2p_share, elsewhere = _ids_by_place()
    kept = p2p_share | elsewhere
    missing = sorted(set(store.STORE_DRIVEN_IDS) - kept)
    assert not missing, (
        f"the store-install nodes click or anchor on {missing}, but no XAML the "
        f"store-no-payments build keeps spells them"
        + (f" ({sorted(set(missing) & payments)} only under Views/Payments)" if payments else "")
    )


def test_every_settings_rail_row_the_nodes_open_is_a_composed_rail_tag():
    xaml = SETTINGS_SHELL.read_text(encoding="utf-8")
    code = SETTINGS_SHELL.with_suffix(".xaml.cs").read_text(encoding="utf-8")
    assert "$\"{FaunaApp.UiIds.Ids.SettingsNavRow}[{key}]\"" in code, (
        "SettingsShellPage no longer composes settings-nav-row[<key>] from its rail tags"
    )
    untagged = [k for k in store.STORE_SETTINGS_RAIL_KEYS if f'Tag="{k}"' not in xaml]
    assert not untagged, f"SettingsShellPage's rail has no row tagged {untagged}"
