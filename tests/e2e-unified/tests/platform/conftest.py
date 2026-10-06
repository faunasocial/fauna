"""Fixtures for platform-specific E2E tests.

These fixtures mirror the ones from tests/e2e/conftest.py so that test files
moved from tests/e2e/ into tests/e2e-unified/tests/platform/ can use them
without modification.
"""

import pytest

from common import (
    build_node,
    build_app,
    build_macos_app,
    get_repo_root,
)


@pytest.fixture(scope="session")
def node_binary():
    """Build and return the path to fauna-nest."""
    return build_node()


@pytest.fixture(scope="session")
def static_dir():
    """Return the path to the built Svelte SPA."""
    repo = get_repo_root()
    return str(repo / "apps" / "fauna-web" / "build")


@pytest.fixture(scope="session")
def app_executable():
    """Build and return the path to FaunaApp.exe."""
    return build_app()


@pytest.fixture(scope="session")
def macos_app_bundle():
    """Build and return the path to the macOS Fauna.app bundle."""
    return build_macos_app()

# `two_nodes` used to live here, duplicated verbatim from
# `tests/api/conftest.py`, and `three_nodes` beside it with no consumer in the
# tree at all. Both spawned `common.nest.start_nest` directly, around both
# nest-start entry points, so neither AST pin in `test_nest_mode_axis.py` could
# see them. `two_nodes` is now one mode-routed fixture in the root
# `conftest.py`; `three_nodes` is deleted rather than routed, because routing a
# fixture nothing requests would have bought a third container per run for no
# test at all (`testing.md` § Default app and nest mode, ruling (4) bounds the
# ceiling by the fixture graph, so an unused fixture costs nothing only while
# it also serves nothing).
