"""tier_1: the stale-device sweep every organisation-account leg runs right
after its sign-in (`helpers/org_account_session.py::clear_stale_devices`).

Owner: `docs/goal/architecture/org-nests.md` § Accounts (a leg stays
device-neutral across re-runs) and `fleet-scope-reclamation.md` clause (4) (a
removal resolves its row's fleet member from client-held truth, and refuses
when this replica has not verified it yet). These replay, over a fake app, the
roster a cap-refused admin met on dev.fauna.build on 2026-11-09: a row
carrying a principal the replica could not verify was refused on every click,
while a principal-less row removed and let this machine enroll. The drive itself is
tier_4 live.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import org_account_session as oas  # noqa: E402

pytestmark = pytest.mark.tier_1

CAP = "capped notice"
UNVERIFIED = "unverified notice"


class _Box:
    """The roster and the page error, advanced by the fake app's clicks.

    ``stale`` rows remove; ``pinned`` rows carry a principal this replica
    cannot verify and are refused. Past the cap this machine holds no row;
    once a slot is free it enrolls on the next visit (the pump's next pass)."""

    CAP_N = 3

    def __init__(self, cards):
        self.cards = list(cards)        # "stale" | "pinned" | "this"
        self.error = ""
        self.clicks = []
        self._settle()

    def _settle(self):
        if "this" not in self.cards and len(self.cards) < self.CAP_N:
            self.cards.append("this")
        if self.error != UNVERIFIED:
            self.error = CAP if "this" not in self.cards else ""

    def visit(self):
        if self.error == UNVERIFIED:
            self.error = ""
        self._settle()

    def remove(self, index):
        self.clicks.append(self.cards[index])
        if self.cards[index] == "pinned":
            self.error = UNVERIFIED
            return
        assert self.cards[index] == "stale"
        del self.cards[index]
        self.error = ""


class _Driver:
    def __init__(self, box):
        self.box = box

    def count(self, element_id, *, scope=None):
        assert element_id == "device-card"
        return len(self.box.cards)

    def is_visible_scrolled(self, element_id, index=0, *, scope=None):
        assert element_id == "device-this-mark-badge"
        i = int(scope.split("[")[1].rstrip("]"))
        return self.box.cards[i] == "this"

    def is_visible(self, element_id, *, scope=None):
        assert element_id == "error-message"
        return bool(self.box.error)

    def get_text(self, element_id, index=0, *, scope=None):
        return self.box.error

    def click(self, element_id, index=0):
        assert element_id == "device-remove-button"
        self.box.remove(index)


class _Settings:
    def __init__(self, box):
        self.box = box

    def open_devices_by_click(self):
        # A re-visit re-renders the page: a refusal shown on the last visit
        # does not survive it (the cap notice is re-derived from the roster).
        self.box.visit()

    def leave_sub_page_by_click(self):
        pass


class _App:
    def __init__(self, box):
        self.driver = _Driver(box)
        self.settings = _Settings(box)

    def error_text(self):
        return self.driver.box.error


@pytest.fixture(autouse=True)
def _quiet(monkeypatch):
    monkeypatch.setattr(oas.time, "sleep", lambda _s: None)
    monkeypatch.setattr(oas, "await_device_removal_ready", lambda _d: None)
    monkeypatch.setattr(oas, "device_limit_text", lambda: CAP)
    monkeypatch.setattr(oas, "unverified_removal_text", lambda: UNVERIFIED)


def test_a_refused_row_is_passed_over_for_the_next(monkeypatch, capsys):
    # dev.fauna.build, 2026-11-09: capped, the first row refused.
    monkeypatch.setattr(oas, "DEVICE_REMOVE_TIMEOUT_S", 0.5)
    box = _Box(["pinned", "stale", "stale"])
    oas.clear_stale_devices(_App(box), "its admin")
    assert box.clicks[:2] == ["pinned", "stale"]
    assert "this" in box.cards and "stale" not in box.cards
    assert box.cards.count("pinned") == 1
    assert "1 stale device(s) refused as unverified" in capsys.readouterr().out


def test_a_plain_sweep_clicks_once_per_stale_row():
    box = _Box(["stale", "this", "stale"])
    oas.clear_stale_devices(_App(box), "its admin")
    assert box.cards == ["this"]
    assert box.clicks == ["stale", "stale"]


def test_capped_with_every_row_refused_fails_naming_the_cap(monkeypatch):
    box = _Box(["pinned", "pinned", "pinned"])
    monkeypatch.setattr(oas, "DEVICE_REMOVE_TIMEOUT_S", 0.0)
    with pytest.raises(pytest.fail.Exception, match="refused at the cap"):
        oas.clear_stale_devices(_App(box), "its admin")


def test_a_removal_that_never_lands_fails_with_this_machine_enrolled(monkeypatch, capsys):
    # Not refused, simply ignored: once enrolled the leg goes on and says so.
    box = _Box(["stale", "this"])
    box.remove = lambda index: box.clicks.append("ignored")
    monkeypatch.setattr(oas, "DEVICE_REMOVE_TIMEOUT_S", 0.0)
    oas.clear_stale_devices(_App(box), "its admin")
    assert box.clicks == ["ignored"]
    assert "left on the roster" in capsys.readouterr().out


# ── the store after the sweep: a save refused as not ready is retried ──
# Measured on dev.fauna.build 2026-11-09: right after the sweep enrolled this
# machine, the DNS credential's save was refused "the account store is not
# ready yet" (the read gate's `holder=true`), and the drive, which pressed it
# once, waited 120 s for a row that could never come.


def test_store_not_ready_reads_the_stores_refusal():
    assert oas.store_not_ready(
        "account state load: the account store is not ready yet")
    assert not oas.store_not_ready("the provider refused the token")
    assert not oas.store_not_ready(None)
    assert not oas.store_not_ready("")


def test_store_not_ready_text_is_the_rust_constant():
    seam = Path(__file__).resolve().parents[3] / "libs/fauna-client-config/src/store_seam.rs"
    assert f'LEDGER_NOT_READY: &str = "{oas.STORE_NOT_READY_TEXT}"' in seam.read_text()


def test_store_hold_lines_keeps_the_gate_and_escrow_lines():
    log = "\n".join([
        "DEBUG pipe request ListEngines",
        "INFO read gate: state-fleet refused as not ready — the unkeyed hold stands device=false holder=true sibling=false",
        "INFO escrow recovery: asked the holder for generation 3",
        "DEBUG pipe request ListEngines",
    ])
    hits = oas.store_hold_lines(log)
    assert len(hits) == 2
    assert "holder=true" in hits[0] and "escrow" in hits[1]


def test_store_hold_lines_keeps_first_and_last_when_long():
    log = "\n".join(f"unkeyed hold stands #{i}" for i in range(50))
    hits = oas.store_hold_lines(log, limit=4)
    assert hits[0].endswith("#0") and hits[-1].endswith("#49")
    assert hits[2] == "<46 more>"
