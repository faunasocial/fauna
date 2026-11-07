r"""tier_3 e2e (real, ISOLATED dev-build sync agent): on windows an identity
succession carries the device's on-demand folder binding into the successor's
per-actor scope, the successor's engine registers the folder's CFAPI sync root
again under the same key, and that engine runs the take-over.

Goal doc: ``docs/goal/architecture/apps/sync-agent.md`` § The per-actor scope
follows the account across a succession — the per-platform paragraph's windows
sentence: the CFAPI sync root is registered by the agent from the binding,
keyed by the folder's name and path and recomputable
(``cfapi_host::shell_account_id``), so the engine rebuilt from the carried
config registers it again under the same key; nothing names the actor, nothing
re-keys. ``succession-cut.md`` § The succession cut, ruling (11)(d) is what the
carried engine is for: the successor's engine comes up holding local state for
the set and re-records what it holds.

This is the windows witness of that claim, in the shape of ``test_sync_agent_unprovision_windows.py``: the
agent's own log lines and its own replies, read offset-scoped around the
ceremony — never the app's rendering of them.

**Three assertions, each anchored on the carry.** The carry's own line
(``service.rs::carry_predecessor_scope``) is the causal barrier. It is logged
once the scope has moved and before the provision publishes the successor's
capability, so everything read after it is about the successor's scope.

1. **The binding is listed under the successor.** ``ListLocations`` names the
   bound folder again, with the same mode and set ref, and the binding store
   that holds it is the successor's (``<base>/<successor-hex>/config.toml``),
   no longer the predecessor's. Before the carry existed (measured on tui,
   2026-10-22) ``ListLocations`` answered ``[]`` after the ceremony.
2. **The sync root is registered again, under the same key.** The successor's
   engine connects the root after the carry (``fauna_cfapi::connect``'s
   ``sync root connected``, reached from
   ``cfapi_host::register_and_connect_shell``), and Explorer's
   ``SyncRootManager`` still holds the folder under the Id it held before the
   ceremony (the OS's record, read from the registry the way
   ``fauna_cfapi::shell_registration_for`` reads it).
   ⚠ **The registry read alone would pass without the carry.** The switch keeps
   the registration (``TeardownMode::KeepUnlessMarked``: an un-provision stops
   engines without marking their roots — the agent logs ``sync root
   disconnected; registration kept``), so the predecessor's entry stands
   whether or not any engine comes back for it. The connect after the carry is
   the half only the successor's engine can produce: a re-connect over a kept
   registration re-registers no shell entry, so ``sync root registered with
   shell`` does not appear again.
3. **The take-over re-records.** ``succession take-over: re-recorded history
   heads as this identity`` (the closing line of ``fauna_sync_engine::engine``'s
   take-over pass): the engine the carry brought up found the predecessor's head
   under the retired nonce and re-recorded it as the successor. On an on-demand
   root that pass runs in the engine start's corpus passes
   (``always_resident::converge_corpus_at_start``), never in a pull: the root's
   inbound rail is the placeholder fold, so the pull's ``re-recorded on a nonce
   gain`` line never comes on windows. A verdict the engine does not revisit
   before its next start (``TAKE_OVER_TERMINAL``) ends the wait at once, quoted.

**State on windows: green, three runs with ``--strict-app`` on 2026-11-06**
(71 s, 86 s, 49 s warm). The agent log reads, in order: the carry
(``moved=2``), ``sync root connected`` for the bound folder, ``succession
take-over candidates=1``, then the pass's closing ``re-recorded history heads as
this identity … rerecorded=1 complete=true``, 100 ms later. Two reds came before
it. Until 2026-10-23 the windows provisioner sent no ``accounts``, so the
capability carried no ``predecessor_keys_by_actor`` pair, and the take-over
refused the head as ``sealed under a root its signer may not open``. After that
the take-over re-recorded, but this test waited for the pull's ``re-recorded on a
nonce gain``, which an on-demand root never logs, and timed out at assertion 3
until the pass got a closing line of its own.

**Why ``real_conversations``.** The take-over runs on the nonce gain that the
cut's re-mint produces, and on windows the launch reconcile that runs the cut
runs only inside the real conversations receive loop's post-restore hook
(``test_identity_succession_aftermath.py`` records the 2026-10-07 measurement).
Without the marker no cut runs, and assertion 3 would read as a broken
take-over.

**Isolation.** The default windows posture (convention 10, windows axis (a)):
the app spawns its own ``fauna-sync-agent.exe`` on the session's pipe and
``--data-dir`` (``conftest.py::_apply_isolated_sync_agent_env``), so the box's
installed agent is never touched. The CFAPI root is a folder under this test's
``tmp_path``, and its shell registration's Id is keyed by that path, so no
other run's root can be mistaken for it.

Convention 8: the set is made through the wizard, the folder bound through the
row's binding form (a fresh windows binding starts on-demand — user ruling
2026-09-26, ``fauna_ipc::sync::fresh_binding_mode``), the image dropped where
the engine watches, and the ceremony driven through Settings → Recovery kit.
Convention 14: every wait is a deadline poll on a durable record behind a
named budget.
"""
from __future__ import annotations

import os
import tomllib
from pathlib import Path

import pytest

from drivers.base import detached_agent_log_text
from helpers.diagnostics import strip_ansi, take_over_log_lines
from helpers.inherited_corpus import BOUND_CORPUS_DIR, seed_an_image_in_a_bound_set
from helpers.succession_ceremony import (
    settled_actor_id,
    succeed_identity_from,
    wait_for_successor_actor,
)
from helpers.waiting import wait_until
from helpers.windows_sync_agent import list_locations, shell_sync_root_id_for

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.real_sync_agent,
    pytest.mark.isolated_sync_agent,
    pytest.mark.real_conversations,
]

#: `service.rs::carry_predecessor_scope`'s success line.
CARRY_LINE = "carried the attested predecessor's per-actor scope into the successor's"
#: `fauna_cfapi::connect`'s line, logged once a root's provider connection is
#: up — the agent's on-demand roots connect only through it.
ROOT_CONNECTED_LINE = "sync root connected"
#: The take-over pass's own closing line when it re-recorded ≥ 1
#: (`fauna_sync_engine::engine`'s `take_over_history_heads`), whichever door ran
#: it. Not the pull's `re-recorded on a nonce gain`: an on-demand root's inbound
#: rail is the placeholder fold, never `pull_remote_changes`, so on windows the
#: take-over runs in the engine start's corpus passes and that line never comes.
TAKE_OVER_LINE = "succession take-over: re-recorded history heads as this identity"
#: A take-over verdict that ends the carry for this engine's life: the engine
#: notes a signer-bound refusal (its error reads `permanently un-appliable`) and
#: a head no root of this account opens (`held, not carried`) and never retries
#: them, and a pass that re-recorded nothing closes on `re-recorded nothing`
#: with its skip counts — on an on-demand root the next pass is the next engine
#: start, so waiting on after any of them only spends the budget.
TAKE_OVER_PREFIX = "succession take-over:"
TAKE_OVER_TERMINAL = ("permanently un-appliable", "held, not carried", "re-recorded nothing")

#: The successor's first provision reaching the agent: the app's hydration
#: start under the new identity (nest connect, grant mint, first convergence
#: tick) and the provision's stop of the outgoing engines, itself bounded at
#: 10 s (`pipe_server.rs::ENGINE_STOP_BUDGET`). Measured from the successor's
#: live session, which the ceremony wait already returned on.
_CARRY_S = 120.0
#: The carried config reaching `ListLocations`: one config load, in the same
#: provision as the carry. The budget covers a busy pipe, not work.
_LISTED_S = 30.0
#: From the carry to the successor's engine connecting the root: the agent's
#: content-key resolution under the new identity (it withholds an engine until
#: an edge names the set), the engine build, and its boot sweep before the
#: connect.
_ROOT_CONNECT_S = 180.0
#: From the carry to the take-over: the cut's re-mint runs in the app's
#: post-store-ready pass behind the successor's account store coming up, and
#: the engine notes the nonce gain at its next pull. Generous for a loaded box;
#: a green run pays none of it.
_TAKE_OVER_S = 240.0
#: The predecessor's engine connecting the root before the ceremony: the
#: upload the seed helper waited on can land in the boot sweep, which runs
#: before the connect.
_ROOT_BEFORE_S = 60.0


def _agent_log(app) -> str:
    """This launch's agent log so far (`<--data-dir>/logs`, the agent runs
    detached so it never reaches the app's own log), ANSI-stripped."""
    return strip_ansi(detached_agent_log_text(app.driver.sync_agent_state_base))


def _norm(path) -> str:
    return str(path).rstrip("\\/").lower()


def _row_for(locations: list[dict], path) -> dict | None:
    return next((row for row in locations if _norm(row.get("path", "")) == _norm(path)), None)


def _listed_row(pipe: str, path) -> dict | None:
    """The agent's `ListLocations` row for ``path``, or ``None`` — also while
    the pipe is busy mid-provision (a reply that cannot be had yet is "not
    listed yet", which the caller's deadline turns into a failure)."""
    try:
        return _row_for(list_locations(pipe), path)
    except (OSError, TimeoutError):
        return None


def _scope_paths(base: Path, actor_hex: str) -> list[str]:
    """The locations the binding store under ``actor_hex``'s scope holds, or
    ``[]`` when that scope holds no ``config.toml``."""
    config = base / actor_hex / "config.toml"
    if not config.is_file():
        return []
    text = config.read_text(encoding="utf-8")
    return [loc.get("path", "") for loc in tomllib.loads(text).get("locations", [])]


def _describe_scope(base: Path, actor_hex: str) -> str:
    scope = base / actor_hex
    try:
        entries = sorted(os.listdir(scope))
    except OSError as e:
        entries = [f"<unreadable: {type(e).__name__}>"]
    return f"{actor_hex[:12]}…: {entries}"


def _wait_line_after(app, offset: int, anchor: str, needle: str, budget_s: float, why: str) -> None:
    """Wait until ``needle`` appears in the agent log AFTER ``anchor``, both
    after ``offset``. Splits the decoded text rather than doing byte
    arithmetic, for the reason `test_sync_agent_unprovision_windows.py`'s
    `_wait_log_line_after` states (CRLF makes byte and character offsets
    disagree)."""

    def _seen() -> bool:
        _, sep, tail = _agent_log(app)[offset:].partition(anchor)
        return bool(sep) and needle in tail

    wait_until(
        _seen,
        budget_s,
        diagnose=lambda: (
            f"{why}\n  the agent never logged {needle!r} after {anchor!r}"
            f"\nagent log since the ceremony (tail):\n{_agent_log(app)[offset:][-4000:]}"
        ),
    )


def test_windows_agent_carries_the_on_demand_binding_across_a_succession(ungranted_app, tmp_path):
    app = ungranted_app
    pipe = app.driver.sync_agent_pipe
    state_base = app.driver.sync_agent_state_base
    assert pipe and state_base, (
        "this launch pinned no isolated agent (pipe/`--data-dir`) — the test must "
        f"carry `isolated_sync_agent` (convention 10); pipe={pipe!r} base={state_base!r}"
    )
    base = Path(state_base)

    # ── Precondition: a bound on-demand folder an engine served and uploaded
    # from, under THIS identity. The take-over re-records only what the
    # device's engine holds (11)(d), so the folder carries a real file the
    # engine recorded; and a CFAPI root exists only for an on-demand binding.
    seed_an_image_in_a_bound_set(app, tmp_path)
    watch_dir = tmp_path / BOUND_CORPUS_DIR

    before = wait_until(
        lambda: _listed_row(pipe, watch_dir),
        _LISTED_S,
        diagnose=lambda: f"the agent never listed the bound folder; ListLocations={list_locations(pipe)!r}",
    )
    assert before.get("mode") == "on-demand", (
        "a fresh windows binding starts on-demand (fresh_binding_mode), and only "
        "an on-demand binding has a CFAPI root to re-register — without one this "
        f"journey would witness nothing; the agent lists {before!r}"
    )
    assert before.get("folder_id"), f"the bound row carries no set ref: {before!r}"
    shell_id_before = wait_until(
        lambda: shell_sync_root_id_for(watch_dir),
        _ROOT_BEFORE_S,
        diagnose=lambda: (
            f"no Fauna SyncRootManager entry covers {str(watch_dir)!r} — the "
            "predecessor's engine never registered the on-demand root"
        ),
    )
    old_actor = wait_until(
        lambda: settled_actor_id(app),
        _ROOT_BEFORE_S,
        diagnose=lambda: f"the Status page never rendered the actor id; error={app.error_text()!r}",
    ).lower()
    assert _norm(watch_dir) in [_norm(p) for p in _scope_paths(base, old_actor)], (
        "the binding must sit in the predecessor's own scope before the ceremony, "
        "or a carry has nothing to move; "
        f"{_describe_scope(base, old_actor)} holds {_scope_paths(base, old_actor)!r}"
    )

    # ── The ceremony, from an offset taken just before it ───────────────────
    offset = len(_agent_log(app))
    ceremony_actor, _held = succeed_identity_from(app)
    assert ceremony_actor.lower() == old_actor, (
        f"the ceremony ran from {ceremony_actor!r}, not the identity that bound "
        f"the folder ({old_actor!r})"
    )
    new_actor = wait_for_successor_actor(app, ceremony_actor).lower()

    # ── 1. The binding is listed under the successor ────────────────────────
    wait_until(
        lambda: CARRY_LINE in _agent_log(app)[offset:],
        _CARRY_S,
        diagnose=lambda: (
            "the agent never carried the predecessor's scope into the successor's "
            "(sync-agent.md § The per-actor scope follows the account across a "
            "succession); scopes: "
            f"{_describe_scope(base, old_actor)}; {_describe_scope(base, new_actor)}\n"
            f"agent log since the ceremony (tail):\n{_agent_log(app)[offset:][-4000:]}"
        ),
    )
    after = wait_until(
        lambda: _listed_row(pipe, watch_dir),
        _LISTED_S,
        diagnose=lambda: (
            "the agent logged the carry but does not list the bound folder under "
            f"the successor; ListLocations={list_locations(pipe)!r}"
        ),
    )
    assert (after.get("folder_id"), after.get("mode")) == (
        before.get("folder_id"),
        before.get("mode"),
    ), f"the carried binding changed across the succession: before={before!r} after={after!r}"
    assert _norm(watch_dir) in [_norm(p) for p in _scope_paths(base, new_actor)], (
        "ListLocations names the folder, but the successor's own binding store "
        f"does not hold it: {_describe_scope(base, new_actor)} holds "
        f"{_scope_paths(base, new_actor)!r}"
    )
    assert not (base / old_actor / "config.toml").exists(), (
        "the predecessor's scope still holds a binding store after the carry — a "
        "carry is a move, never a copy (two copies of one engine DB are the "
        f"equivocation shape per-actor scoping exists to prevent); "
        f"{_describe_scope(base, old_actor)}"
    )

    # ── 2. The sync root is registered again, under the same key ───────────
    _wait_line_after(
        app, offset, CARRY_LINE, ROOT_CONNECTED_LINE, _ROOT_CONNECT_S,
        "the successor's engine never connected the carried folder's CFAPI root "
        "— the binding is listed, but no engine serves it",
    )
    shell_id_after = shell_sync_root_id_for(watch_dir)
    assert shell_id_after == shell_id_before, (
        "the successor's root must be registered under the key the folder had "
        "before the ceremony — the key names the folder and its path, never the "
        f"actor (cfapi_host::shell_account_id); before={shell_id_before!r} "
        f"after={shell_id_after!r}"
    )

    # ── 3. The take-over re-records ─────────────────────────────────────────
    # Waits for the take-over's verdict, either way: the re-record line, or a
    # refusal the engine will never retry (TAKE_OVER_TERMINAL). Boxed in a
    # 1-tuple so "re-recorded, no refusal" (an empty list) is still truthy.
    def _take_over_verdict() -> tuple[list[str]] | None:
        _, sep, tail = _agent_log(app)[offset:].partition(CARRY_LINE)
        if not sep:
            return None
        if TAKE_OVER_LINE in tail:
            return ([],)
        refusals = [
            line.strip() for line in tail.splitlines()
            if TAKE_OVER_PREFIX in line and any(t in line for t in TAKE_OVER_TERMINAL)
        ]
        return (refusals,) if refusals else None

    refusals = wait_until(
        _take_over_verdict,
        _TAKE_OVER_S,
        diagnose=lambda: (
            "the carried engine never re-recorded the predecessor's head "
            "(succession-cut.md ruling (11)(d)) and refused none for good; cut and "
            f"take-over lines in the launch's log:\n      {take_over_log_lines(app)}"
            f"\nagent log since the ceremony (tail):\n{_agent_log(app)[offset:][-4000:]}"
        ),
    )[0]
    assert not refusals, (
        "the carried engine ran the take-over and REFUSED the predecessor's head "
        "for good — it holds no root the head's signer may open, i.e. the "
        "successor's capability carried no retired key paired with that "
        "predecessor (`SyncCapability.predecessor_keys_by_actor`, "
        "sync-agent-credentials.md § Credential model):\n      "
        + "\n      ".join(refusals)
        + f"\n  cut and take-over lines in the launch's log:\n      {take_over_log_lines(app)}"
    )
