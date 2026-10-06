"""Admin Logs view — the nest's log ring over WS-RPC
(docs/goal/architecture/apps/observability.md § Surfaces).

An admin-scoped page (`admin-logs`) that fetches the nest's in-memory `fauna-log`
ring via the admin WS-RPC `fauna.admin.logs` and renders it with the SAME widget
(and the same `log-entry` / `log-level-filter` / `log-copy-button` component IDs)
as the client's own Settings → Logs page — so the shared `LogsActions` accessors
(`app.logs.*`) drive it once navigated here. No Clear: there is no admin RPC to
wipe the nest ring. linux leads; the other five apps lift this shape
(priority #1).

tier_3 (full stack): a real `fauna-nest` binary whose `RingLayer` captures the
nest's own startup `tracing` output, fetched by a real admin client over WS-RPC.
This is the only tier that exercises the nest→wire→client log path end to end.
"""
import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("admin-logs")
def test_admin_logs_page_renders_nest_ring(admin_app):
    """The admin Logs page is reachable and shows the nest's captured activity
    (at minimum the nest's startup log lines)."""
    app = admin_app
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible(), (
        f"admin-logs page not reachable. error: {app.error_text()!r}"
    )
    assert app.admin.wait_for_min_log_entries(1, timeout=15.0), (
        "the nest ring should hold at least the nest's startup log line, "
        f"got {app.logs.entry_count()} entries"
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_level_filter_narrows(admin_app):
    """Selecting a severity narrows the admin Logs view to that level and
    everything more severe — `Error` is a subset of `All`. The source is fetched
    once (no refetch), so the filter is a deterministic client-side narrowing."""
    app = admin_app
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible()
    assert app.admin.wait_for_min_log_entries(1, timeout=15.0)
    n_all = app.logs.entry_count()
    assert n_all > 0, "expected captured nest log entries to filter"

    app.logs.set_level("Error")
    n_err = app.logs.entry_count()
    assert n_err <= n_all, (
        f"Error-only view ({n_err}) must be a subset of All ({n_all})"
    )

    # Back to All restores the full set — the filter narrows the held source in
    # memory, it does not refetch, so All is exactly the pre-filter count.
    app.logs.set_level("All")
    assert app.logs.entry_count() == n_all, (
        f"All view should restore {n_all} entries, got {app.logs.entry_count()}"
    )


@pytest.mark.feature("admin-logs")
def test_admin_logs_copy_present(admin_app):
    """The copy affordance is present + actuable on the admin Logs page (the
    clipboard buffer isn't readable headlessly — we assert the affordance, not
    the OS buffer)."""
    app = admin_app
    app.admin.navigate_logs()
    assert app.admin.logs_page_visible()
    assert app.admin.wait_for_min_log_entries(1, timeout=15.0)
    app.logs.copy()
