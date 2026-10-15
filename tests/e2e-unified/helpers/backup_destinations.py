"""One test's backup-destination enrollment on an account other tests share —
counted from its own rows, never from a zero the page cannot prove.

The Backups page paints its destination rows only after an async load (the
account-plane read behind `navigate()`), and no `ui.yaml` element marks that
load done. A row is proof of a loaded list (rows render in one pass); a zero is
proof of nothing. Convention 14's rider is that absence may only be asserted
from a COMPLETE read (`e2e-latency-independent-assertions.md` § Negative
asserts), so a "start from no destination" guard that reads 0 straight after
navigating — or holds a 0 for a few seconds and believes it — is no guard: in a
whole-suite sweep it let one module's leftover become the next six tests'
wrong count.

So a test never starts from a zero. It enrolls under a name no other test uses
and waits for THAT row: the loaded list. Then it removes every other row
through the page, and its count is its own.

**Row-less registrations.** An enroll registers the destination on the source
nest before it writes the list row (`backup_enroll.rs` § Ordering and
crash-safety), so an enroll cut off between the two leaves a registration the
page never shows and the user has no door to — while the nest's own reads
(`fauna.backup.destination.list`, `fauna.backup.status`) still list it, under
any test that reads them by index. Nothing in the product removes one (the
status read's heal only adds). With every row gone through the page, what the
source nest still registers beyond this test's own is exactly that residue, and
it is deregistered over WS-RPC — fixture cleanup of a state no UI reaches,
which convention 8 exempts. A real row whose registration was dropped is
re-registered by the page's own status read (the heal), so nothing visible is
lost.
"""

from __future__ import annotations

import secrets

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.waiting import wait_until

# A destination row appears after the add's own reload; the same ceiling the
# action layer's count wait uses (`actions/backups.py::DESTINATION_COUNT_WAIT_S`).
ROW_WAIT_S = 40


class DestinationEnrollment:
    """The destinations one test enrolls through the Backups page, for the
    account `user` that the app under test is signed in as on `nest`.

    Register `leave_none` as a finalizer BEFORE the first `add_*`, so a test
    that fails anywhere — its own enroll included — still removes what it
    enrolled."""

    def __init__(self, app, nest: dict, user: dict):
        self.app = app
        self.nest = nest
        self.user = user
        self.names: list[str] = []
        self.before = self.registered_ids()

    # ── the source nest's registry ──

    def _call(self, kind: str, payload: dict) -> dict:
        client = WsRpcAdminClient(
            self.nest["url"],
            actor_id=bytes(self.user["signing_key"].verify_key),
            signing_key=bytes(self.user["signing_key"]),
        )
        with client:
            return client.call(kind, payload)

    def registered_ids(self) -> set[str]:
        """Every destination id the source nest registers for this account."""
        reply = self._call("fauna.backup.destination.list", {})
        return {d["destination_id"] for d in reply["destinations"]}

    def mine(self) -> set[str]:
        """The ids registered since this enrollment began — this test's own."""
        return self.registered_ids() - self.before

    def _deregister(self, ids: set[str]) -> None:
        for destination_id in sorted(ids):
            self._call("fauna.backup.destination.remove", {"destination_id": destination_id})

    # ── the page ──

    def _row_indices(self, mine: bool) -> list[int]:
        b = self.app.backups
        return [
            i
            for i in range(b.destination_count())
            if any(n in b.destination_text(i) for n in self.names) == mine
        ]

    def _wait_for_row(self, name: str) -> None:
        b = self.app.backups
        wait_until(
            lambda: any(name in b.destination_text(i) for i in range(b.destination_count())),
            ROW_WAIT_S,
            diagnose=lambda: (
                f"the Backups page never listed the destination {name!r} this test "
                f"enrolled: rows={[b.destination_text(i) for i in range(b.destination_count())]!r} "
                f"registered={sorted(self.registered_ids())!r} error={self.app.error_text()!r}"
            ),
        )

    def _unique(self, name: str) -> str:
        unique = f"{name} {secrets.token_hex(2)}"
        self.names.append(unique)
        return unique

    def add_destination(self, url: str, name: str) -> str:
        """Enroll the nest at `url` through the Backups page (navigating to it
        first) under `name` plus a unique suffix (so `name in row_text` still holds), wait for its row, and — on
        this enrollment's first add — leave it the page's only row. Returns the
        name it enrolled under."""
        unique = self._unique(name)
        self.app.backups.navigate()
        self.app.backups.add_destination(url, name=unique)
        self._settle(unique)
        return unique

    def add_custodian_destination(self, name: str, capacity: str = "") -> str:
        """`add_destination`'s twin for the client-device kind."""
        unique = self._unique(name)
        self.app.backups.navigate()
        self.app.backups.add_custodian_destination(name=unique, capacity=capacity)
        self._settle(unique)
        return unique

    def _settle(self, unique: str) -> None:
        self._wait_for_row(unique)
        if len(self.names) == 1:
            self._isolate()

    def _isolate(self) -> None:
        """Remove every row that is not this test's, through the page — the
        list is loaded, so its count is the truth — then deregister what the
        source nest still registers beyond this test's own: row-less residue."""
        b = self.app.backups
        others = self._row_indices(mine=False)
        while others:
            count = b.destination_count()
            b.remove_destination(others[0])
            b.wait_for_destination_count(count - 1)
            others = self._row_indices(mine=False)
        self._deregister(self.registered_ids() - self.mine())
        b.wait_for_destination_count(len(self.names))

    def leave_none(self) -> None:
        """Finalizer: remove every destination this test enrolled.

        Nothing registered since the enrollment began → the test's own steps
        already removed it, and there is nothing to wait for. Otherwise one of
        its rows is waited for — a positive wait, whose paint is the load
        proof — and every row of this test's is removed through the page. A
        registration of this test's whose row never paints (an enroll cut off
        before its list write) is deregistered and reported: never a silent
        pass, since that residue is what turned one red into six downstream.
        """
        mine = self.mine()
        if not mine:
            return
        b = self.app.backups
        b.navigate()
        try:
            wait_until(lambda: self._row_indices(mine=True), ROW_WAIT_S)
        except AssertionError:
            self._deregister(self.mine())
            raise AssertionError(
                f"teardown: this test's destination(s) {sorted(mine)!r} were registered "
                f"on the source nest but never listed on the Backups page (an enroll "
                f"cut off before its list write); deregistered them. "
                f"error={self.app.error_text()!r}"
            ) from None
        rows = self._row_indices(mine=True)
        while rows:
            count = b.destination_count()
            b.remove_destination(rows[0])
            b.wait_for_destination_count(count - 1)
            rows = self._row_indices(mine=True)
        left = self.mine()
        if left:
            self._deregister(left)
            raise AssertionError(
                f"teardown: every row of this test's was removed, yet the source nest "
                f"still registered {sorted(left)!r}; deregistered them"
            )
