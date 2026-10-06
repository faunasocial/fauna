"""Every general-path `_dedicated_mail_nest_impl` consumer (direct or via the
`_start_mail_venue` mode-routing seam) must play supervisor.

The fixture cold-boots its MTA + MDA with every deployment toggle OFF — the state
a freshly claimed nest is in (`mail-bridge-lifecycle.md` § Default-off on first
claim) — and its tests turn mail on through the app UI. When the app is driven as
this nest's ADMIN (which all but one consumer do), that enable also opens both
roles' gates, so each idling bridge exits 0 for its supervisor
(§ *An idling bridge stays subscribed*); the binaries e2e has no s6, so the test
must call `handle.rebind_after_enable()` before touching `mx_port` / `imaps_port`
/ `caldav_port`. A consumer that forgets connects to a listener that is not bound
and fails at its first port use, some 60+ seconds later, with a message about the
port rather than about the missing call.

⚠ **This rule checks the CALL, not its precondition — and the two are separable.**
`enable_mail_plain` opens the deployment gate only when the enabling actor is the
nest admin: `fauna.bridges.set_mail_enabled` is Admin-class, and
`MailSettingsMachine::enable_mail` calls it best-effort and **swallows** a
non-admin's rejection ("the non-admin no-op",
`libs/fauna-client-mail-settings/src/machine.rs`). So a consumer that drives the
app as a NON-admin mints its mailbox but leaves the subsystem off, `mta.Bindable`
never opens, the idling bridge correctly never exits, and the very
`rebind_after_enable()` this rule mandates then times out after 60s against a
perfectly healthy bridge. Such a consumer must call
`handle.admin_opens_mail_gate()` first — `test_identity_succession_mail_auth.py`
is the one that does, and it is deliberately non-admin because a succession
re-points the whole account. Satisfying this rule is therefore necessary but not
sufficient; the gate has to actually be open.

**Why this is a test and not a checklist.** The audit that first swept these call
sites keyed on a HAND-WRITTEN list of fixture names and missed
`dedicated_caldav_mailbox_less_nest` — a third fixture on the same general path,
whose two consumers would have failed exactly this way. So the fixture set is
derived from `conftest.py` itself here: any future fixture that routes to
`_dedicated_mail_nest_impl` — directly, or via the `_start_mail_venue`
mode-routing seam every general-path fixture now uses — without
`caldav_only=True` is picked up automatically, and its consumers are held to
the same rule the day they are written.

The `caldav_only=True` variants are deliberately out of scope: they seed
`mail_enabled=false, caldav_enabled=true` BEFORE the MDA boots, so their bridges
bind at cold boot and never idle — nothing to rebind.

tier_1: pure source analysis, no nest, no driver, no app.
"""

from __future__ import annotations

import ast
import pathlib

import pytest

pytestmark = [pytest.mark.tier_1]

_E2E_ROOT = pathlib.Path(__file__).resolve().parent.parent
_CONFTEST = _E2E_ROOT / "conftest.py"
_TESTS_DIR = _E2E_ROOT / "tests"
_HELPERS_DIR = _E2E_ROOT / "helpers"

_IMPL = "_dedicated_mail_nest_impl"
# Arm 6's mode-routing seam (`testing.md` § Default app and nest mode, ruling
# (3), landed 2026-09-02) moved every general-path fixture from calling `_IMPL`
# directly to calling `_start_mail_venue(...)`, which forwards **the same
# kwargs** — `caldav_only` included — through to `_IMPL` via the mode
# provider's `start_mail_venue` method. Only the two `caldav_only=True`
# variants (`dedicated_caldav_admin_port_nest`, `dedicated_caldav_only_nest`)
# still call `_IMPL` directly, so both call sites must be scanned or the
# general-path set silently goes empty the moment a fixture is mode-routed —
# exactly what happened here.
_VENUE_START = "_start_mail_venue"
_REBIND = "rebind_after_enable"


def _general_path_fixtures() -> set[str]:
    """Fixture names that route to `_dedicated_mail_nest_impl` — directly, or
    via `_start_mail_venue`'s mode-routing seam — WITHOUT `caldav_only=True`:
    i.e. the ones whose bridges cold-boot idling."""
    tree = ast.parse(_CONFTEST.read_text())
    found: set[str] = set()
    for node in tree.body:
        if not isinstance(node, ast.FunctionDef) or node.name in (_IMPL, _VENUE_START):
            continue
        for call in ast.walk(node):
            if not isinstance(call, ast.Call):
                continue
            if getattr(call.func, "id", None) not in (_IMPL, _VENUE_START):
                continue
            caldav_only = None
            for kw in call.keywords:
                if kw.arg == "caldav_only":
                    caldav_only = ast.literal_eval(kw.value) if isinstance(
                        kw.value, ast.Constant
                    ) else "dynamic"
            if caldav_only is not True:
                found.add(node.name)
    return found


def _functions_reaching_rebind() -> set[str]:
    """Every function anywhere under tests/ or helpers/ that itself calls the
    rebind — so a consumer delegating to a shared preamble counts as covered."""
    covering: set[str] = set()
    for path in sorted(_TESTS_DIR.glob("*.py")) + sorted(_HELPERS_DIR.glob("*.py")):
        for node in ast.walk(ast.parse(path.read_text())):
            if isinstance(node, ast.FunctionDef) and _REBIND in ast.dump(node):
                covering.add(node.name)
    return covering


def test_the_general_path_fixture_set_is_not_empty():
    """Non-vacuity anchor. If `_dedicated_mail_nest_impl`/`_start_mail_venue` are
    ever renamed or the fixtures restructured again, the sweep below would
    silently pass over an empty set and this rule would quietly stop being
    enforced."""
    fixtures = _general_path_fixtures()
    assert fixtures, (
        f"no fixture routes to {_IMPL} (directly or via {_VENUE_START}) without "
        "caldav_only=True — either both were renamed (update this file) or the "
        "general path is gone (delete it). An empty set must never read as 'all "
        "consumers compliant'."
    )


def test_every_general_path_consumer_plays_supervisor():
    fixtures = _general_path_fixtures()
    covering = _functions_reaching_rebind()

    offenders: list[str] = []
    checked = 0
    for path in sorted(_TESTS_DIR.glob("*.py")):
        tree = ast.parse(path.read_text())
        for node in tree.body:
            if not isinstance(node, ast.FunctionDef) or not node.name.startswith("test_"):
                continue
            params = {a.arg for a in node.args.args}
            if not (params & fixtures):
                continue
            checked += 1
            if _REBIND in ast.dump(node):
                continue
            called = {
                getattr(c.func, "id", None) or getattr(c.func, "attr", None)
                for c in ast.walk(node)
                if isinstance(c, ast.Call)
            }
            if called & covering:
                continue
            offenders.append(f"{path.name}::{node.name}")

    assert checked, (
        f"no test binds any of {sorted(fixtures)} — the sweep matched nothing, "
        "which must not read as compliance"
    )
    assert not offenders, (
        "these tests bind a general-path dedicated-mail-nest fixture but never reach "
        f"`handle.{_REBIND}()`, so the bridges they connect to have exited and not "
        "been relaunched — each will fail at its first port use:\n  "
        + "\n  ".join(offenders)
        + "\n\nAdd `handle.rebind_after_enable()` right after the enable step "
        "(pass mta=False / mda=False for a role the enable does not gate)."
    )
