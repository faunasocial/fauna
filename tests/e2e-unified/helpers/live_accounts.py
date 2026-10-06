"""Account-scoped isolation for `--nest live` — what the run created, and how it
gives it back.

`testing.md` § Default app and nest mode, *Mode mechanics*: on live, isolation is
**account-scoped** — the harness provisions its own accounts and reaps them at
teardown, "so an ordinary journey test touches no foreign state by construction
and § The shared-box rule's non-destructive carve-out is satisfied structurally".
Standalone and docker need none of this: their whole nest is thrown away.

**⚠ The reap is `suspend` + `schedule delete`, NOT an immediate delete, and the
difference is a fact about the nest, not a shortcut here.** The ratified text
named `fauna.admin.users.delete` as the teardown call; that kind does not delete
a user — `bins/fauna-nest/src/admin_ws_handlers.rs::delete_handler` calls
`schedule_pending_action(ActionType::AdminDeleteUser)`, whose
`delay_secs()` is **7 days** (`pending_actions.rs`), and the admin surface has no
execute-now kind (only `fauna.admin.pending_actions.list`). The cooling-off
window is deliberate product design — an admin erasing a user is exactly the
action that deserves one — so the harness works *with* it rather than asking for
it to be weakened:

* **`fauna.admin.users.suspend` runs first and is immediate** (`suspend_user_now`
  + token revocation + socket close). From that instant the account cannot
  authenticate, cannot connect, and generates no activity — which is the
  property the shared-box carve-out actually needs: a human on the box sees
  nothing.
* **`fauna.admin.users.delete` is then scheduled**, and the box's own pending-
  action loop executes it ~7 days later. Residue is therefore bounded and
  self-reaping rather than permanent.

So a live run leaves a suspended, delete-scheduled account for up to a week. That
is a real (small) departure from "teardown removes every artifact the run
created", it is written down here and in testing.md rather than inferred, and the
thing that actually closes it is the ephemeral staging box (testing.md § Gap 3),
not a weaker delete. Before this existed a live run leaked accounts *permanently
and unnamed*, so this is strictly the smaller residue.

Nothing in this module raises. A teardown that dies while cleaning up would mask
the failure the test was reporting, so every problem here is a loud printed
warning naming the account by handle and actor id — greppable, and actionable by
a human with an admin client.
"""

from __future__ import annotations

# ── The ledger ──────────────────────────────────────────────────────────────
# Run-level, like `nest_mode._RUN_MODE`: the accounts are created deep inside
# helpers and test bodies, which have no channel to the provider that will reap
# them.

_CREATED: list[dict] = []

#: Every kind whose SUCCESSFUL reply leaves an account on the nest — or, for the
#: `True` entries, a request an admin may turn into one through the app UI,
#: where no harness dial sees the account appear. `observe` is called for every
#: reply at the one WS-RPC wire core (`clients/_ws_rpc_core.py`), so whichever
#: helper, fixture or test body sent the kind, the reap knows the account.
#: Before this, each creating call site owed its own `note` and most did not:
#: one interrupted `dev.example.com` sweep (2026-10-04) leaked 53 accounts.
ACCOUNT_CREATING_KINDS: dict[str, bool] = {
    "fauna.admin.users.create": False,
    "fauna.account.register": False,
    "fauna.account.invite_request.submit": True,
}


def observe(kind: str, payload) -> None:
    """Note the account a successful ``kind`` call created, on live only.

    Called by the wire core after every ``ok`` reply; a no-op for every other
    kind and in every other mode."""
    pending = ACCOUNT_CREATING_KINDS.get(kind)
    if pending is None or not isinstance(payload, dict):
        return
    from helpers import nest_mode

    if not nest_mode.run_mode().is_live:
        return
    actor_id = payload.get("actor_id")
    if isinstance(actor_id, (bytes, bytearray)):
        actor_id = bytes(actor_id).hex()
    if isinstance(actor_id, str):
        note(actor_id, payload.get("handle"), pending=pending)


def note(actor_id_hex: str, handle: str | None = None, *, pending: bool = False) -> None:
    """Record an account this run created on the live box.

    Called only in live mode — in standalone/docker the nest is discarded whole,
    and a ledger that filled up there would make the teardown warning fire about
    accounts on a nest that no longer exists. ``pending`` marks a submitted
    invite request: an account only if something approved it. A later
    non-pending note of the same actor settles it as a real account.
    """
    if not actor_id_hex:
        return
    for entry in _CREATED:
        if entry["actor_id"] == actor_id_hex:
            entry["pending"] = entry["pending"] and pending
            entry["handle"] = entry["handle"] or handle
            return
    _CREATED.append({"actor_id": actor_id_hex, "handle": handle, "pending": pending})


def created() -> list[dict]:
    """The accounts this run created, in creation order."""
    return list(_CREATED)


def reset() -> None:
    """Test-only: clear the ledger between self-test cases."""
    _CREATED.clear()


def describe(entry: dict) -> str:
    handle = entry.get("handle")
    return f"{handle} ({entry['actor_id'][:16]}…)" if handle else entry["actor_id"]


# ── The reap ────────────────────────────────────────────────────────────────

#: What the default logger said this run, for `pytest_terminal_summary`. The
#: reap runs in a fixture finalizer, and pytest discards a passing test's
#: captured fixture output — so a plain `print` put the `[live] reaped N` line,
#: and the RESIDUE warning a human must act on, in no log at all (measured
#: 2026-10-04: no live run log had ever carried either).
_REPORT: list[str] = []


def _print(message: str) -> None:
    _REPORT.append(message)
    print(message, flush=True)


def drain_report() -> list[str]:
    """The lines the default logger recorded since the last drain, once."""
    lines = list(_REPORT)
    _REPORT.clear()
    return lines


#: `fauna.admin.users.list` clamps `limit` to 1..=500 nest-side.
_USERS_LIST_PAGE = 500


def _successors(entries: list[dict], call) -> list[dict]:
    """The accounts that now hold a handle this run minted under another actor id.

    A succession ceremony the app runs (identity theft, `succession-aftermath.md`)
    moves the account AND its handle to a fresh actor id that no harness dial
    observes; the retired id keeps a handle-less `users` row, which the nest
    suspends without complaint — so a reap of the noted ids alone reports
    success and leaves the account active (measured 2026-10-04 on
    `dev.example.com`: both successors of `test_account_instance_lock_tui.py`).
    Handles are unique per box and this run minted the noted ones, so whoever
    holds one now is this run's. Raises when the listing fails; `reap` turns
    that into a warning.
    """
    handles = {e["handle"]: e for e in entries if e.get("handle")}
    if not handles:
        return []
    known = {e["actor_id"] for e in entries}
    found: list[dict] = []
    seen: set[str] = set()
    offset = 0
    while True:
        reply = call("fauna.admin.users.list", {"limit": _USERS_LIST_PAGE, "offset": offset})
        page = reply.get("users", [])
        fresh = 0
        for user in page:
            actor_id = bytes(user["actor_id"]).hex()
            if actor_id in seen:
                continue
            seen.add(actor_id)
            fresh += 1
            origin = handles.get(user.get("handle"))
            if origin is not None and actor_id not in known:
                found.append({
                    "actor_id": actor_id,
                    "handle": user["handle"],
                    "pending": False,
                    "successor_of": origin["actor_id"],
                })
        offset += len(page)
        if not page or fresh == 0 or offset >= reply.get("total", 0):
            return found


def reap(base_url: str, admin_signing_key, *, call=None, log=_print) -> list[dict]:
    """Suspend + schedule-delete every account this run created. Never raises.

    Returns the entries that could NOT be fully reaped, each carrying a
    ``problem`` string — empty on the clean path. ``call`` is injectable
    (``(kind, payload) -> reply``) so the tier_1 tests exercise the ordering,
    the warning text and the failure paths with no live box.
    """
    entries = created()
    if not entries:
        return []
    if call is None:
        def call(kind, payload):
            from common.auth import _authed_call
            return _authed_call(base_url, admin_signing_key, kind, payload)

    unfollowed: str | None = None
    try:
        entries = entries + _successors(entries, call)
    except Exception as exc:
        unfollowed = repr(exc)

    residue: list[dict] = []
    never_admitted: list[dict] = []
    for entry in entries:
        actor_id = bytes.fromhex(entry["actor_id"])
        problems = []
        # Suspend FIRST and unconditionally: it is the immediate half, so even a
        # failed delete-schedule leaves the account unable to reach the box.
        try:
            call("fauna.admin.users.suspend", {
                "actor_id": actor_id,
                "category": "other",
                "reason": "e2e live run teardown (harness-provisioned account)",
            })
        except Exception as exc:
            if entry.get("pending") and str(getattr(exc, "code", "")).endswith("not_found"):
                # An invite request nobody approved: no account exists to reap.
                never_admitted.append(entry)
                continue
            problems.append(f"suspend failed: {exc!r}")
        try:
            call("fauna.admin.users.delete", {"actor_id": actor_id})
        except Exception as exc:
            problems.append(f"delete-schedule failed: {exc!r}")
        if problems:
            residue.append({**entry, "problem": "; ".join(problems)})

    reset()
    if never_admitted:
        log(
            f"[live] {len(never_admitted)} invite request(s) this run submitted on "
            f"{base_url} were never admitted, so left no account: "
            f"{', '.join(describe(e) for e in never_admitted)}"
        )
    reaped = len(entries) - len(residue) - len(never_admitted)
    if reaped:
        log(
            f"[live] reaped {reaped} harness-provisioned account(s) on "
            f"{base_url}: suspended now, deletion scheduled (the nest executes "
            f"it ~7 days out — `fauna.admin.users.delete` schedules a pending "
            f"action, it does not delete; see helpers/live_accounts.py)"
        )
    successors = [e for e in entries if e.get("successor_of")]
    if successors:
        log(
            f"[live] {len(successors)} of them succeeded a noted account (the app "
            f"ran the ceremony, so the handle moved): "
            f"{', '.join(describe(e) for e in successors)}"
        )
    if unfollowed is not None:
        named = [describe(e) for e in entries if e.get("handle")]
        log(
            f"⚠️  [live] could not list users on {base_url} to follow a handle to "
            f"its successor ({unfollowed}); any of {', '.join(named)} that ran a "
            f"succession ceremony may still be ACTIVE under a successor id. "
            f"Check them from an admin client."
        )
    for entry in residue:
        log(
            f"⚠️  [live] RESIDUE on {base_url}: account {describe(entry)} was "
            f"created by this run and could NOT be cleaned up — "
            f"{entry['problem']}. Delete it from an admin client; it is "
            f"harness residue, not a real user."
        )
    return residue
