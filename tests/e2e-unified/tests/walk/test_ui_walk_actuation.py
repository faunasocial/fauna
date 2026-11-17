"""The possibility invariant `actuation-changes-or-reports`, layer (c) — OBSERVING.

Owner: `docs/goal/architecture/e2e-systematic-ui-walks.md` § Implementation status
today, the possibility-invariants bullet; engine and rationale in
`helpers/ui_walk_actuation.py` (read its docstring first), declared lists in
`helpers/ui_walk_actuation_lists.toml`.

**Gate:** `--walk-sweep` (this directory is pruned without it — convention 17:
scheduled sweeps only, never inner-loop).

    pytest tests/walk/test_ui_walk_actuation.py --walk-sweep --app tui

**Observing, not failing.** It is promoted to failing once its first corpus is
triaged; until then the violations are written to the corpus file
(`.walk-actuation.log` beside the harness, gitignored; `FAUNA_E2E_ACTUATION_CORPUS`
redirects it) and printed, and the test asserts only that the sweep really swept.

**The account is fresh.** Registered for this test alone and given NOTHING past
sign-in — no feed, calendar, contact or folder — because the empty, first-use
states are exactly where an affordance that cannot succeed hides (convention 17's
third discipline: fixtures must not seed past the frontier under test).

⚠ Never add a sleep to this file: every click's reply and every `barrier()` is
the step's causal anchor (convention 14).
"""

import os
from pathlib import Path

import pytest

from helpers import ui_walk, ui_walk_actuation

pytestmark = pytest.mark.tier_3

_CORPUS_DEFAULT = Path(__file__).resolve().parent.parent.parent / ".walk-actuation.log"


@pytest.fixture
def fresh_account_app(request, app, nest_instance):
    """`app`, signed in as a brand-new account with nothing seeded past sign-in.

    `_make_user` is NOT used: it creates a default feed, which is state past
    sign-in. The registration below is the same call it makes, minus that feed.
    """
    from common.auth import create_actor_and_register
    from conftest import _login_app_as
    from helpers.live_box_door import reaching_the_live_box

    with reaching_the_live_box("registering the fresh walk account"):
        user = create_actor_and_register(
            nest_instance["port"],
            base_url=nest_instance["url"],
            admin_signing_key=nest_instance["admin"]["signing_key"],
        )
    _login_app_as(app, request, nest_instance, user)
    return app


def _corpus_path() -> Path | None:
    raw = os.environ.get("FAUNA_E2E_ACTUATION_CORPUS")
    if raw is None:
        return _CORPUS_DEFAULT
    if raw.strip().lower() in {"", "0", "off", "no", "false", "none"}:
        return None
    return Path(raw)


def test_every_enabled_affordance_changes_the_frame_or_reports(fresh_account_app, request):
    driver = fresh_account_app.driver
    # No skip for an app without `GET /registry`: its every read is unjudged,
    # and the non-vacuity asserts below then fail it by name.
    report = ui_walk_actuation.ActuationReport()
    sweep = ui_walk_actuation.ActuationSweep(
        driver, report, ui_walk_actuation.DeclaredLists.load(),
        ui_walk_actuation.element_kinds(),
    )
    pages = ui_walk.canonical_pages()
    # A red-verification run sweeps the one page its mutant lives on.
    only = [p for p in os.environ.get("FAUNA_E2E_ACTUATION_PAGES", "").split(",") if p]
    if only:
        unknown = set(only) - set(pages)
        assert not unknown, f"FAUNA_E2E_ACTUATION_PAGES names no canonical page: {unknown}"
        pages = only
    for page in pages:
        try:
            sweep.sweep_page(page)
        except Exception as exc:
            # Navigation is fixture setup: a page this app does not mount is its
            # own area's parity question, not this invariant's finding.
            report.unjudged.append(f"{page}: could not be entered — {type(exc).__name__}: {exc}")

    lines = report.lines()
    print("\n".join(lines))
    path = _corpus_path()
    if path is not None:
        with open(path, "a", encoding="utf-8") as f:
            f.write(f"# {request.node.nodeid}\n")
            f.write("\n".join(lines) + "\n")

    # Non-vacuity — the only things asserted while the sweep observes.
    assert report.pages, "the sweep entered no page"
    assert report.actuated, (
        "the sweep actuated NOTHING on any page — a broken frame read or an "
        "over-wide never-list reporting a clean sweep:\n" + "\n".join(lines)
    )
    assert len(report.unjudged) < len(report.actuated), (
        "more steps went unjudged than were judged — the sweep measured its own "
        "failures, not the app:\n" + "\n".join(lines)
    )
