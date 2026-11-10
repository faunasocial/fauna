"""Reading a collected item's fixture closure HONESTLY.

``item.fixturenames`` is not a list of fixture *requests*. pytest also puts
every DIRECTLY-parametrized argname in it and satisfies that name with a
pseudo-fixture of its own making, so a plain ``name in item.fixturenames``
test cannot tell::

    def test_x(app):                                  # launches an application
    @pytest.mark.parametrize("app", [...])            # a string
    def test_y(app):

apart — and every caller keyed on a bare name reads the second as the first.

That cost the harness a whole suite. ``tests/test_app_surface_declarations.py``
opens with ``@pytest.mark.parametrize("app", [...seven app names...])``; the
name matched conftest's ``_LOCAL_NEST_FIXTURE_USERS``, so the session-scoped
autouse ``_session_primary_mail_domain`` resolved ``nest_instance`` at that
test's setup and the entire tier_1 file sat behind a cold ``just`` build of
``fauna-nest`` — silently, because the build had not printed yet. Convention
7's own skip-taxonomy proofs executed on no path at all for as long as the
collision stood (found 2026-08-24, stack dumped at
``_session_primary_mail_domain`` → ``nest_instance`` → ``nest_binary`` →
``build_node`` → ``subprocess.run``). All 50 passed once it was lifted, so the
hang was hiding only itself.

A name is a direct param only when BOTH signals agree, and neither alone will
do:

* it is in ``item.callspec.params`` — but an INDIRECT param
  (``indirect=True`` — ``launch_harness``, ``nest_mode``,
  ``folder_share_owner_app``) lands there too while its real fixture genuinely
  runs, so ``callspec`` alone would drop a real build back inside a test's own
  900 s clock, the bound inversion the prebuild hook exists to prevent;
* AND its fixturedef's function lives in pytest's own module — the synthesized
  ``_pytest.python.get_direct_param_fixture_func`` (``baseid=''``). An indirect
  param's fixturedef is OURS, so this half is what separates the two.

The module test alone will not do either: pytest's BUILT-IN fixtures
(``tmp_path``, ``monkeypatch``, ``tmp_path_factory``) are real fixtures that
also live in ``_pytest.*``, and a survey of 3653 collected items found the
module-only form dropping all three from every single closure. They are absent
from ``callspec.params``, so requiring both signals keeps them.

Unknown shapes answer "real": a spurious prebuild is wasted work, a missing one
is a bound inversion, so the fallback preserves the pre-existing behaviour.
"""

__all__ = [
    "LAZY_RESOLUTION_ATTR", "MAIL_VENUE_BINARIES", "is_real_fixture",
    "lazily_resolved", "real_fixture_closure", "resolves_lazily",
]


def is_real_fixture(item, name) -> bool:
    """Is ``name`` in this item's closure backed by a real fixture?"""
    callspec = getattr(item, "callspec", None)
    if name not in getattr(callspec, "params", ()):
        # Not parametrized at all — a plain fixture request, pytest's own
        # built-ins included.
        return True
    info = getattr(item, "_fixtureinfo", None)
    defs = getattr(info, "name2fixturedefs", {}).get(name) if info is not None else None
    if not defs:
        return True
    module = getattr(getattr(defs[-1], "func", None), "__module__", "") or ""
    # Ours → indirect parametrization, the fixture really runs. Pytest's own →
    # the synthesized direct-param pseudo-fixture, which is just a value.
    return not module.startswith("_pytest.")


def real_fixture_closure(item) -> set:
    """``item.fixturenames`` minus the directly-parametrized argnames."""
    return {
        name for name in getattr(item, "fixturenames", ()) if is_real_fixture(item, name)
    }


# ---------------------------------------------------------------------------
# Builds a fixture resolves LAZILY — declared on the fixture, read by the prebuild
# ---------------------------------------------------------------------------
#
# A fixture that fetches a binary through ``request.getfixturevalue(...)`` keeps
# that binary out of every item's static closure — on purpose, for the mail
# venues (`_start_mail_venue`'s docstring: a declared parameter is what put
# every venue fixture in the binary closure, and the docker provider needs no
# binary at all). The cost is that the collection-time prebuild and
# `just e2e-preflight` never see the build, so on a cold tree it runs inside the
# first test's 900 s timeout (measured 2026-11-07: `test_mail_dmarc_reports.py`'s
# module-own `dmarc_venue` errored every test at setup on `just
# mail-bridge-build`). A hand-kept list of fixture NAMES closed it for the five
# conftest venues and missed every module-own one, so the declaration lives on
# the fixture itself: ``@resolves_lazily(...)``, placed UNDER ``@pytest.fixture``
# so it marks the function pytest keeps as the fixturedef's ``func``. The
# prebuild adds the declared names only in a mode that builds a local nest, so
# the lazy resolution's reason survives.

#: The attribute ``resolves_lazily`` sets on the fixture function.
LAZY_RESOLUTION_ATTR = "__fauna_resolves_lazily__"

#: What a mail venue resolves lazily in standalone mode
#: (`StandaloneProvider.start_mail_venue`, `_dav_toggle_venue_impl`).
MAIL_VENUE_BINARIES = ("mail_bridge_binary", "seal_helper_binary")


def resolves_lazily(*fixture_names):
    """Declare the build fixtures this fixture fetches through ``getfixturevalue``.

    Goes UNDER ``@pytest.fixture``; a pin in `test_e2e_binary_prebuild.py`
    holds every `_start_mail_venue` caller to carrying it."""

    def mark(func):
        setattr(func, LAZY_RESOLUTION_ATTR,
                tuple(getattr(func, LAZY_RESOLUTION_ATTR, ())) + tuple(fixture_names))
        return func

    return mark


def lazily_resolved(item) -> set:
    """The build fixtures this item's real closure resolves lazily — the union of
    every ``@resolves_lazily`` declaration on the fixtures it will run."""
    info = getattr(item, "_fixtureinfo", None)
    name2defs = getattr(info, "name2fixturedefs", {}) if info is not None else {}
    out: set = set()
    for name in real_fixture_closure(item):
        for fixturedef in name2defs.get(name, ()) or ():
            out.update(getattr(getattr(fixturedef, "func", None), LAZY_RESOLUTION_ATTR, ()))
    return out
