"""Fixtures for pure API tests (no UI automation).

Provides the ``node_binary`` fixture that many API tests depend on. These were
originally in tests/e2e/conftest.py.

``two_nodes`` used to live here too, duplicated verbatim in
``tests/platform/conftest.py``. It is now a single mode-routed fixture in the
root ``conftest.py`` beside ``second_nest``/``third_nest`` — see there for why
(`testing.md` § Default app and nest mode, ruling (1)).
"""

import pytest

from common import build_node


@pytest.fixture(scope="session")
def node_binary():
    """Build and return the path to fauna-nest."""
    return build_node()
