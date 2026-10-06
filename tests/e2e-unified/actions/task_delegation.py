from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


def _norm(s: str) -> str:
    """Lowercase, alphanumeric-only — the cross-app option key.

    A ``<select>``'s ``get_text`` returns its ``<option value>`` on web
    (``web-bridge/server.py`` reads ``el.value`` for select/input/textarea),
    while a GTK ``DropDown`` returns the *selected display label*
    (``automation/find.rs`` reads the selected ``StringObject``). Normalizing
    both collapses ``"this-device"`` and ``"This device"`` onto ``thisdevice``,
    so a test asserts one key on every app. This is the same normalization
    linux's ``select()`` already applies to match a stable key onto a human
    label (``automation/agent.rs::string_model_index``) — the platform
    difference stays here in the action layer.
    """
    return "".join(c.lower() for c in s if c.isalnum())


# Stable option keys, normalized. `PinOption::{Automatic, ThisDevice}` in
# `fauna_core::delegation`; a pin to another participant keys on its
# participant-ref hex and is never *offered* (only rendered), so tests never
# select it.
AUTOMATIC = "automatic"
THIS_DEVICE = "thisdevice"


class TaskDelegationActions:
    """Drive the user-facing "Task delegation" Settings sub-page
    (docs/goal/behavior/participants.md § Task delegation; tests/e2e-unified/ui.yaml
    `task-delegation` page).

    A person sees each heavy background task kind here — its current runner and
    its assignment — and may pin a kind to a capable participant instead of
    letting the automatic policy order choose (Q-B: always-on nest → plugged-in
    desktop → never a battery mobile; the work queues rather than run on a
    phone).

    Rows (`task-delegation-kind-item`, indexed, in `LIVE_TASK_KINDS` order —
    today just `backup-upload`) each carry `task-delegation-kind-name`,
    `task-delegation-kind-runner`, and `task-delegation-assignment-picker`.

    Every app renders `TaskDelegationRow.pin_options` verbatim — the shared
    `fauna_core::delegation::delegation_rows` decides which options are legal,
    because a pin to a participant that can never run the kind would strand it
    forever (participants.md § The assignment picker). So the picker offers
    "This device" on the native desktops and never on web / iOS / Android.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self, leave_timeout: float = 10.0) -> None:
        """Arrive at the task-delegation Settings sub-page — by LEAVING it first.

        Every app re-runs `TaskDelegationView::load()` when the page is *shown*
        (the runner column is live lease state), and that is all a user can do
        to refresh it: go away and come back. Setting the same nav stack the app
        is already on is not an arrival on an app that routes by state — linux
        reloads on the page's GTK `map` (`settings/task_delegation.rs`), which a
        no-op navigation never re-fires — so every "re-read" this layer does
        would silently read the frame already painted. Leaving the Settings
        shell altogether (to the feed) and waiting for the list to go away
        makes each call a real arrival on every app alike. The shell, not its
        root: a sub-page is shell state (`docs/goal/ui/README.md` § Navigation
        model), so on linux a bare Settings nav from inside Settings changes
        nothing and the sub-page stays mapped (`app.rs`, the content stack's
        visible-child notify).
        """
        self.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        deadline = time.time() + leave_timeout
        while self.driver.is_visible("task-delegation-list"):
            if time.time() >= deadline:
                raise AssertionError(
                    "the task-delegation page stayed on screen after navigating "
                    f"to the feed for {leave_timeout}s, so re-opening it "
                    "cannot re-run its load and every read would be stale"
                )
            time.sleep(0.2)
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "task-delegation"}]},
        })

    def is_page_visible(self, timeout: float = 15.0) -> bool:
        """True once the page has *loaded* — i.e. at least one task-kind row is
        rendered.

        The container (`task-delegation-list`) is built synchronously and so is a
        useless readiness signal; the rows arrive from an async
        `TaskDelegationView::load()` that every navigate re-runs (the runner
        column is live lease state). `LIVE_TASK_KINDS` is never empty, so "a row
        exists" is the honest loaded-signal.
        """
        try:
            self.driver.wait_for("task-delegation-list", timeout=timeout)
        except TimeoutError:
            return False
        return self.wait_for_rows(timeout=timeout)

    def wait_for_rows(self, timeout: float = 15.0) -> bool:
        """Poll until at least one task-kind row is rendered."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.row_count() > 0:
                return True
            time.sleep(0.3)
        return self.row_count() > 0

    def wait_for_row_count(self, expected: int, timeout: float = 15.0) -> bool:
        """Poll until the list holds exactly `expected` rows."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.row_count() == expected:
                return True
            time.sleep(0.3)
        return self.row_count() == expected

    def wait_for_assignment(self, index: int, key: str, timeout: float = 15.0) -> bool:
        """Poll until the row's picker settles on `key`.

        A pin write is a config CAS round-trip followed by a full reload, and a
        navigate re-runs the load — so the picker's value is only eventually
        consistent with the nest.
        """
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.row_count() > index and self.assignment_key(index) == key:
                return True
            time.sleep(0.3)
        return self.row_count() > index and self.assignment_key(index) == key

    def wait_for_assignment_across_reload(
        self, index: int, key: str, timeout: float = 30.0
    ) -> str:
        """Poll until the row's picker settles on `key` **with a page reload each
        round**; return the key it last read.

        The stronger twin of `wait_for_assignment`, and the difference matters.
        That one polls the painted picker without re-navigating, so it is
        satisfied by the frame already on screen — which, right after a write,
        is the frame the write has not landed in yet. It answers "does the
        widget show this?"; this answers "does the NEST show this?", because
        every round re-runs `TaskDelegationView::load()`.

        Use it wherever a test depends on an assignment having actually reached
        the account rather than merely having been picked — in particular when
        ARRANGING a precondition, where a false positive is silent: the journey
        then runs against the state it thought it had cleared.
        """
        deadline = time.time() + timeout
        seen = ""
        while time.time() < deadline:
            self.navigate()
            if self.wait_for_rows() and self.row_count() > index:
                seen = self.assignment_key(index)
                if seen == key:
                    return seen
            time.sleep(0.3)
        return seen

    def wait_for_runner(self, index: int, expected: str, timeout: float = 90.0) -> str:
        """Poll until the row's runner line reads `expected`; return what it last read.

        Returning the last-seen text rather than a bool is what lets the caller
        put the *actual* runner string in its failure message — a
        self-diagnosing failure (convention 6), since "not this device" and
        "some other device" are different findings.

        The budget is generous on purpose and costs a green run nothing
        (convention 14): the runner line is advisory-lease state, so it turns
        over only after the seat's own heartbeat lands at the nest and the page
        re-reads it — several seconds of round trips under no load, and this
        machine is never under no load. A deadline poll converges as fast as the
        lease does; a fixed sleep sized for a busy box would be slower on every
        green run and still wrong on a slow one.
        """
        deadline = time.time() + timeout
        seen = ""
        while time.time() < deadline:
            if self.row_count() > index:
                seen = self.runner_text(index)
                if seen == expected:
                    return seen
            # Re-navigating is what re-runs `TaskDelegationView::load()`; the
            # rendered row is a snapshot, not a live subscription.
            self.navigate()
            time.sleep(0.5)
        return seen

    # ── row reads ─────────────────────────────────────────────────────────
    def row_count(self) -> int:
        """How many task-kind rows the page renders (one per LIVE_TASK_KINDS)."""
        return self.driver.count("task-delegation-kind-item")

    def _scope(self, index: int) -> str:
        return f"task-delegation-kind-item[{index}]"

    def kind_names(self) -> list[str]:
        """Each row's resolved display name, in order."""
        return [
            self.driver.get_text("task-delegation-kind-name", scope=self._scope(i))
            for i in range(self.row_count())
        ]

    def runner_text(self, index: int = 0) -> str:
        """The row's current-runner line, as the user reads it."""
        return self.driver.get_text("task-delegation-kind-runner", scope=self._scope(index))

    def assignment_key(self, index: int = 0) -> str:
        """The row's selected assignment, as a normalized cross-app key
        (`AUTOMATIC` / `THIS_DEVICE` / a foreign pin's painted participant name).

        Keyed on the PAINTED label on every app, the same vocabulary
        `option_keys` reads, so "the selection is one of the offered options"
        is a comparison that means something. Web's `get_text` answers a
        `<select>`'s `<option value>` instead — identical after `_norm` for
        Automatic and This device, but a full participant hex against a short
        painted one for a foreign pin — so web reads the selected option's text.
        """
        scope = self._scope(index)
        if self.driver.is_web():
            text = self.driver.get_attr(
                "task-delegation-assignment-picker", "selected-text", scope=scope
            )
            return _norm(text or "")
        return _norm(self.driver.get_text("task-delegation-assignment-picker", scope=scope))

    def option_keys(self, index: int = 0) -> list[str] | None:
        """Every option the row's picker currently OFFERS, as normalized keys —
        not just the selected one.

        The option list is a **correctness surface**, not a cosmetic one
        (participants.md § The assignment picker): a pin to a participant that
        can never run the kind makes it wait forever, so the shared
        `delegation_rows` emits the legal set and every app renders it verbatim.
        Asserting the *absence* of an option is therefore a real assertion about
        the product, and the selected-value read cannot make it — a picker that
        wrongly offered "This device" for a nest-run kind would read `Automatic`
        just like a correct one.

        Returns `None` on a driver whose bridge does not serve the `"options"`
        attr (`PlatformDriver.option_texts`), which a caller must treat as "I
        could not look", never as "there were none".
        """
        texts = self.driver.option_texts(
            "task-delegation-assignment-picker", scope=self._scope(index)
        )
        return None if texts is None else [_norm(text) for text in texts]

    def row_of(self, kind_name: str) -> int:
        """The index of the row whose display name is `kind_name`.

        Raises rather than returning a sentinel: every caller here goes on to
        read that row, and a `-1` would silently read the last one.
        """
        names = self.kind_names()
        if kind_name not in names:
            raise AssertionError(
                f"no {kind_name!r} row on the Task-delegation page; it rendered {names!r}"
            )
        return names.index(kind_name)

    def wait_for_runner_where(
        self, index: int, predicate, timeout: float = 90.0
    ) -> str:
        """Poll until the row's runner line satisfies `predicate`; return what it
        last read.

        The open-predicate twin of `wait_for_runner`, for the assertions that are
        about a *class* of runner rather than one string — "some other device",
        "anything but this one". Same convergence properties: a deadline poll on
        rendered state that re-runs `TaskDelegationView::load()` each round
        (convention 14), returning the last-seen text so the caller can put the
        actual runner in its failure message (convention 6).
        """
        deadline = time.time() + timeout
        seen = ""
        while time.time() < deadline:
            if self.row_count() > index:
                seen = self.runner_text(index)
                if predicate(seen):
                    return seen
            self.navigate()
            time.sleep(0.5)
        return seen

    # ── the pin write ─────────────────────────────────────────────────────
    def set_assignment(self, index: int, key: str) -> None:
        """Pick an assignment option for the row.

        `key` is the stable cross-app key (`"automatic"` / `"this-device"`);
        linux/windows resolve it onto the human label by the same normalization
        `_norm` uses, web onto the `<option value>`.

        The write goes through the shared `TaskDelegationView::set_assignment`,
        which resolves the option and persists the pin via the config **CAS**
        read-modify-write — never a blind last-writer-wins save.
        """
        self.driver.select("task-delegation-assignment-picker", key, scope=self._scope(index))
