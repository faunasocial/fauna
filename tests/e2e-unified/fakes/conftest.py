"""Pytest conftest for tests/e2e-unified/fakes/.

Re-exports the fake_cloud fixture so tests in this directory can
request it without explicit imports. Pytest only auto-discovers
fixtures defined in conftest.py files; the fixture itself lives in
fake_cloud.py so non-test callers can import it directly. Also
re-exports the `httpserver_listen_address` / `make_httpserver` overrides
(bind policy + threading) so this
directory's own self-tests get the same server `tests/conftest.py` gives
every app-facing e2e test.
"""
from fake_cloud import fake_cloud, httpserver_listen_address, make_httpserver  # noqa: F401
