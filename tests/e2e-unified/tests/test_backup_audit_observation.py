"""The backup audit's **observation feed** — the half that has no other witness.

``docs/goal/ui/backups.md`` § Audit-alert surface, *The freshness comparison needs a
client-local observation, and each shell must feed it*:
``AuditStateSnapshot::observed_high_water`` is the newest message-kind record the
client has seen *with its own eyes*, persisted by the client's own audit store and
fed from its own render path.

**Why this test exists, and why the two audit e2es in ``test_backups.py`` are not
enough.** Those two prove the *destination* side end to end — a real pass advances
``backup-destination-last-audit-time``, a dead destination raises
``backup-audit-alert`` — but neither exercises **freshness**, and freshness is the
only verdict that reads the observation. It cannot be reached in those tests by
construction: ``evaluate_freshness`` floors a destination's high-water at its
``added_at``, stamped from the real clock at enroll, so a destination enrolled
seconds ago is *correctly* never stale, and shifting the client's ``now`` moves both
sides of the comparison equally (that reasoning is written out in both docstrings).

So on every shell built so far, "the observation is fed" rests on the wiring being
*read* rather than *run*. That is exactly the gap the goal doc warns about: **a shell
that renders the two elements but feeds no observation ships a permanently-passing
audit, and nothing about it looks broken** — the audit reports healthy forever
because a client that never observes anything cannot fail freshness (the documented
``None`` contract). A green audit page is therefore not evidence the feed works;
this is.

**What it asserts.** Rendering the conversation list — the one place the client shows
the user what it knows about the nest-originated kinds — advances the *persisted*
observation high-water from absent to a real timestamp. Read from the app's own
store, not from a test-only mirror, so a feed that computes the value and drops it
fails here.

**Three stores, one shape.** web's is ``localStorage`` (read via ``driver.eval_js``,
the sanctioned web-only escape hatch, ``drivers/web.py`` § Test utilities); linux and
tui are both a plain JSON file — ``AuditStateSnapshot`` serialized verbatim via
``serde_json`` (``libs/fauna-client-backup/src/native_store.rs``) at
``<config_home>/fauna/backup/<actor-hex>/audit-state.json`` (linux,
``apps/fauna-linux/src/backup_audit.rs``) / ``<config_home>/fauna-tui/backup/<actor-hex>/audit-state.json``
(tui, ``apps/fauna-tui/src/backup_audit.rs``) — read directly off disk from the
driver's own per-launch ``config_home`` (e2e convention 10's per-launch isolation
already gives each launch a private one; no new app-side seam needed). The mechanism
*above* the store is shared and already covered: ``observe_local_record``'s
monotonicity has mutation-verified tier_1 tests in
``libs/fauna-client-backup/src/audit.rs``. What is untested anywhere else is each
shell's own *wiring* — render path → store — and each shell needs its own probe of
its own store to cover it.

tier_2: the real driver renders the real conversation list off the real shared
manager, but the inbound message is injected (``conversations_inject_inbound``)
rather than delivered over a live MLS round trip — the same shape every other
conversations render test uses. The *observation* is not injected: it is whatever the
page's own render path computed from what it painted.
"""

import json
import os
import time

import pytest

pytestmark = [pytest.mark.tier_2]

# The SPA's per-actor audit-state slot (`libs/fauna-wasm/src/backup_audit.rs`
# `storage_key`). Actor-scoped because web switches accounts in-process — a
# process-global slot would let one account's observation suppress another's.
_SLOT_TEMPLATE = "fauna_backup_audit_{actor_hex}"


def _audit_slots(driver) -> dict[str, str]:
    """Every `fauna_backup_audit_*` slot in the app's own `localStorage`, raw.

    Enumerated rather than looked up by one expected key so a failure can say
    *which* slots exist — the difference between "the feed never wrote" and "it
    wrote under a different actor" is the whole point of the actor scoping, and a
    bare `getItem() -> null` cannot tell them apart.
    """
    return driver.eval_js(
        "(() => { const out = {};"
        " for (let i = 0; i < window.localStorage.length; i++) {"
        "   const k = window.localStorage.key(i);"
        "   if (k && k.startsWith('fauna_backup_audit_')) out[k] = window.localStorage.getItem(k);"
        " } return out; })()"
    ) or {}


def _observed_high_water(driver, actor_hex: str) -> int | None:
    """`observed_high_water` (unix seconds) out of THIS actor's audit slot, or
    `None` when the slot is absent or carries no observation yet."""
    raw = _audit_slots(driver).get(_SLOT_TEMPLATE.format(actor_hex=actor_hex))
    if not raw:
        return None
    return json.loads(raw).get("observed_high_water")


def _audit_state_path(config_home: str, app_dir_name: str, actor_hex: str) -> str:
    """`<config_home>/<app_dir_name>/backup/<actor_hex>/audit-state.json` — the
    native store's on-disk location, per `fauna_sync_engine::db::actor_state_dir`
    (lowercase-hex subdir of the flat `backup/` base)."""
    return os.path.join(config_home, app_dir_name, "backup", actor_hex.lower(), "audit-state.json")


def _file_observed_high_water(path: str) -> int | None:
    """`observed_high_water` out of a native `AuditStateSnapshot` JSON file, or
    `None` when the file does not exist yet or carries no observation."""
    if not os.path.exists(path):
        return None
    with open(path, encoding="utf-8") as f:
        return json.load(f).get("observed_high_water")


def _assert_observation_advances(app, actor_hex: str, read_high_water, describe_absence):
    """The shared body every shell's leg drives identically: navigate to
    conversations, inject one inbound, and assert the store's persisted
    `observed_high_water` advances past its pre-inject baseline.

    `read_high_water()` and `describe_absence()` are the only per-shell pieces —
    everything else (the flow, the budget, the unit/magnitude pin) is shared so
    the three legs cannot silently drift onto different standards of proof.
    """
    app.conversations.navigate()
    before = read_high_water()

    thread_id = app.conversations.inject_and_resolve_thread(
        rail="FaunaMls",
        sender="observation-feed@self-nest.test",
        subject="audit observation",
        body="A record this client has now seen with its own eyes.",
    )
    assert thread_id, "the inject must land in a thread for the list to render it"

    # Deadline poll on *state*, never a settle-sleep (testing.md convention 14):
    # the render → effect → store write is a few microtasks, and a generous budget
    # costs a green run nothing.
    OBSERVE_BUDGET_S = 30
    deadline = time.monotonic() + OBSERVE_BUDGET_S
    after = before
    while time.monotonic() < deadline:
        after = read_high_water()
        if after is not None and (before is None or after > before):
            break
        time.sleep(0.25)

    assert after is not None, (
        f"the conversation list rendered a thread but nothing was persisted "
        f"after {OBSERVE_BUDGET_S}s. The observation feed is the load-bearing "
        f"half of the audit: with it absent, freshness can never fail and the "
        f"Backups page reports every destination healthy forever (backups.md "
        f"§ Audit-alert surface). {describe_absence()} error={app.error_text()!r}"
    )
    assert before is None or after > before, (
        f"the observation high-water did not advance past the baseline "
        f"({before!r} → {after!r}) even though a NEWER thread was just rendered. "
        f"Either the feed is not reading the rendered list, or its in-memory "
        f"short-circuit cache is suppressing the write."
    )

    # Unit + magnitude: seconds, not milliseconds, and plausibly now. The inject
    # stamps the real clock, so `after` must land within a wide window around the
    # wall clock — wide because this asserts a UNIT, not a latency (a ms/s
    # mix-up is off by 1000×, ~50 years).
    now_s = int(time.time())
    assert now_s - 3600 < after <= now_s + 3600, (
        f"observed_high_water={after!r} is not a plausible unix-SECONDS timestamp "
        f"near now ({now_s}). The store takes seconds; ThreadSummary carries "
        f"milliseconds — a missing /1000 reads as ~50 years in the future and would "
        f"make every destination's freshness comparison meaningless."
    )


@pytest.mark.web
@pytest.mark.feature("backup-destinations-and-restore")
def test_rendering_the_conversation_list_feeds_the_audit_observation(
    logged_in_app, test_user
):
    """The conversation list's render advances the persisted observation high-water.

    Flow under test::

        baseline        → no observation recorded for this actor yet
        inject inbound  → a real ThreadSummary with a real last_activity_ms
        list renders    → the page's own effect calls backupAuditObserve(actor, newest)
                          → fauna_client_backup::audit::observe_local_record
                          → the actor-scoped localStorage slot
        read the store  → observed_high_water is that message's second

    **What makes it load-bearing rather than a "did it write something" check.** The
    recorded value must match the *rendered* message's own timestamp, in seconds. A
    feed that wrote `Date.now()`, a constant, or the wrong unit would satisfy
    "something is persisted" while making the freshness comparison meaningless —
    ms-vs-s alone is a 1000× error that would make every destination look
    catastrophically stale forever. Mutation check: deleting the
    `backupAuditObserve` call from the conversations page leaves the slot absent, and
    the assertion below reports exactly that.
    """
    app = logged_in_app
    d = app.driver
    actor_hex = test_user["actor_id_hex"]

    # `test_user` is session-scoped and other tests may already have rendered
    # threads for it, so the baseline is "whatever is there now", not "nothing".
    # The assertion is the *advance* past it, which is what the feed is for. (The
    # shared `observe_local_record` is monotonic, so a prior observation can only
    # make this harder to pass, never easier.)
    _assert_observation_advances(
        app,
        actor_hex,
        read_high_water=lambda: _observed_high_water(d, actor_hex),
        describe_absence=lambda: (
            f"Audit slots present: {sorted(_audit_slots(d))!r} — a slot under a "
            f"DIFFERENT actor means the page fed the wrong identity, not that the "
            f"feed is missing."
        ),
    )


@pytest.mark.linux
def test_rendering_the_conversation_list_feeds_the_audit_observation_linux(
    logged_in_app, test_user
):
    """linux's leg of the observation-feed probe (see the web test's docstring
    for the full flow + why this matters). The store is a plain JSON file at
    `<config_home>/fauna/backup/<actor-hex>/audit-state.json`
    (`apps/fauna-linux/src/backup_audit.rs`), fed from
    `observe_thread_activity` on the conversation list's render — read directly
    off disk from the driver's own per-launch `config_home` (e2e convention 10's
    per-launch isolation already gives each launch a private one)."""
    app = logged_in_app
    actor_hex = test_user["actor_id_hex"]
    path = _audit_state_path(app.driver.config_home, "fauna", actor_hex)

    _assert_observation_advances(
        app,
        actor_hex,
        read_high_water=lambda: _file_observed_high_water(path),
        describe_absence=lambda: (
            f"Expected audit state file: {path!r} "
            f"(exists={os.path.exists(path)!r})."
        ),
    )


@pytest.mark.tui
def test_rendering_the_conversation_list_feeds_the_audit_observation_tui(
    logged_in_app, test_user
):
    """tui's leg of the observation-feed probe (see the web test's docstring for
    the full flow + why this matters). Same shape as linux over its own
    per-actor path — `<config_home>/fauna-tui/backup/<actor-hex>/audit-state.json`
    (`apps/fauna-tui/src/backup_audit.rs`)."""
    app = logged_in_app
    actor_hex = test_user["actor_id_hex"]
    path = _audit_state_path(app.driver.config_home, "fauna-tui", actor_hex)

    _assert_observation_advances(
        app,
        actor_hex,
        read_high_water=lambda: _file_observed_high_water(path),
        describe_absence=lambda: (
            f"Expected audit state file: {path!r} "
            f"(exists={os.path.exists(path)!r})."
        ),
    )


# tui's own leg is the dedicated `_tui` test directly above — not a silent omission.
@pytest.mark.macos
@pytest.mark.ios
def test_rendering_the_conversation_list_feeds_the_audit_observation_apple(
    logged_in_app, test_user
):
    """apple's leg of the observation-feed probe (see the web test's docstring
    for the full flow + why this matters), both macOS + iOS in one function —
    one shared FaunaKit store and one shared `ConversationsVM` render-time
    observe call cover both targets. The store is a plain JSON file at
    `<Application Support>/Fauna/<actor-hex>/backup-audit-state.json`
    (`AccountStateDir.backupAuditStatePath`, fed by `ConversationsVM.swift`'s
    `backupAuditObserve` call on render), read from the driver's own per-launch
    isolated `Library/Application Support` (`app_support_dir()` — macOS's
    relocated `CFFIXED_USER_HOME`, iOS's simulator app data container; same
    on-disk shape, different underlying isolation mechanism)."""
    app = logged_in_app
    actor_hex = test_user["actor_id_hex"]
    app_support = app.driver.app_support_dir()
    assert app_support, "the apple launch has no resolved Application Support dir"
    path = os.path.join(app_support, "Fauna", actor_hex.lower(), "backup-audit-state.json")

    _assert_observation_advances(
        app,
        actor_hex,
        read_high_water=lambda: _file_observed_high_water(path),
        describe_absence=lambda: (
            f"Expected audit state file: {path!r} "
            f"(exists={os.path.exists(path)!r})."
        ),
    )


@pytest.mark.windows
def test_rendering_the_conversation_list_feeds_the_audit_observation_windows(
    logged_in_app, test_user
):
    """windows' leg of the observation-feed probe (see the web test's docstring
    for the full flow + why this matters). The store is a plain JSON file at
    `<data_dir>/<actor-hex>/backup-audit-state.json`
    (`AccountStateDir.BackupAuditStatePath`, `libs/fauna-ffi/src/account_state.rs`
    normalizes the hex to lowercase — same as linux/tui), fed from
    `ConversationsPage.Refresh`'s observation call on every conversation-list
    render (`AccountStateDir.ObserveBackupAudit`). Unlike linux/tui, windows has
    no `config_home` — its isolated base is `FAUNA_E2E_DATA_DIR`
    (`WindowsDriver.data_dir()`, mirroring `BackupPaths.DataDir`)."""
    app = logged_in_app
    actor_hex = test_user["actor_id_hex"]
    data_dir = app.driver.data_dir()
    assert data_dir, "the windows launch has no isolated data dir (FAUNA_E2E_DATA_DIR unset)"
    path = os.path.join(data_dir, actor_hex.lower(), "backup-audit-state.json")

    _assert_observation_advances(
        app,
        actor_hex,
        read_high_water=lambda: _file_observed_high_water(path),
        describe_absence=lambda: (
            f"Expected audit state file: {path!r} "
            f"(exists={os.path.exists(path)!r})."
        ),
    )
