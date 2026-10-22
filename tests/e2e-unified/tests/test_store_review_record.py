"""The store-review record keeps the grammar its owner doc defines.

`installers/store-review.md` § The record makes `installer/store-review.toml`
the ONE place the live state of every store submission lives — one table per
(store, version), a state from a fixed list, two dates, a short note. A gate
reads it to hold the `:test` stage while a review is open, and a
human reads it today to answer "is anything in review?"; both are only as
good as the record's spelling. This file pins the spelling: the store, track
and state enumerations of § The tracks and § The states, one table per
(store, version), a track that belongs to its store, `since` and `read` as
real dates with `read` never before `since`, and a note that names no
address (the record ships, and no person or secret belongs in it).

The enumerations live once, in `scripts/_store_review.py`, which is also the
record's one writer (`just store-review`): the record must be exactly what
that writer renders, so a hand edit fails here. The writer's refusals are
pinned against an in-memory record.

Static-only — reads one repository file, builds nothing, launches nothing
(tier_1), so it runs on every machine.
"""
from __future__ import annotations

import datetime as _dt
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "scripts"))

import _store_review as sr  # noqa: E402

TODAY = _dt.date(2026, 10, 18)


def _submissions() -> list[dict]:
    return sr.load()


def test_every_table_carries_exactly_the_owned_fields() -> None:
    for sub in _submissions():
        assert set(sub) == set(sr.FIELDS), f"{sub.get('store')} {sub.get('version')}: fields {sorted(sub)}"


def test_store_track_state_flavour_dates_and_note_keep_the_grammar() -> None:
    for sub in _submissions():
        assert sr.problems(sub) == []


def test_one_table_per_store_and_version() -> None:
    seen: set[tuple[str, str]] = set()
    for sub in _submissions():
        key = (sub["store"], sub["version"])
        assert key not in seen, f"two tables for {key}; the newest state replaces the old in place"
        seen.add(key)


def test_the_record_is_what_the_writer_renders() -> None:
    """A hand edit — a misspelling or a well-spelled change — fails here."""
    assert sr.check_text(sr.RECORD.read_text()) == []


def test_a_hand_edit_is_detected() -> None:
    text = sr.RECORD.read_text()
    edited = text.replace('state = "serving"', 'state = "in-review"', 1)
    assert edited != text
    assert any("hand edit" in e for e in sr.check_text(edited))
    misspelled = text.replace('state = "serving"', 'state = "in review"', 1)
    assert any("state 'in review'" in e for e in sr.check_text(misspelled))


# ── The writer's refusals and in-place update ─────────────────────────────

_FIRST = dict(
    store="app-store",
    version="0.1.5",
    state="uploaded",
    read=_dt.date(2026, 10, 17),
    track="testflight-external",
    build="1",
    flavour="store-safe",
    note="Uploaded for Beta App Review.",
    today=TODAY,
)


def test_first_write_creates_the_table_and_render_round_trips() -> None:
    subs, verb = sr.apply([], **_FIRST)
    assert verb == "created"
    assert subs[0]["since"] == subs[0]["read"] == _dt.date(2026, 10, 17)
    assert sr.check_text(sr.render(subs)) == []


def test_later_write_replaces_state_dates_and_note_in_place() -> None:
    subs, _ = sr.apply([], **_FIRST)
    subs, verb = sr.apply(
        subs, store="app-store", version="0.1.5", state="in-review", read=_dt.date(2026, 10, 18), today=TODAY
    )
    assert verb == "updated" and len(subs) == 1
    assert subs[0]["state"] == "in-review"
    assert subs[0]["since"] == _dt.date(2026, 10, 18)
    assert subs[0]["note"] == "Uploaded for Beta App Review."
    # The same state read again keeps its since.
    subs, _ = sr.apply(
        subs, store="app-store", version="0.1.5", state="in-review", read=TODAY, since=None, today=TODAY
    )
    assert subs[0]["since"] == _dt.date(2026, 10, 18)


@pytest.mark.parametrize(
    "change, needle",
    [
        ({"track": "internal-testing"}, "track 'internal-testing' is not one of app-store's"),
        ({"state": "reviewing"}, "state 'reviewing'"),
        ({"flavour": "full"}, "flavour 'full'"),
        ({"store": "steam"}, "store 'steam'"),
        ({"version": "0.1"}, "MAJOR.MINOR.PATCH"),
        ({"since": _dt.date(2026, 10, 18)}, "before since"),
        ({"read": _dt.date(2026, 10, 19)}, "in the future"),
        ({"note": "Asked reviewer@example.com to retry."}, "names an address"),
        ({"note": "x" * (sr.NOTE_MAX + 1)}, "a sentence or two"),
        ({"note": None}, "needs --note"),
    ],
)
def test_writer_refuses(change: dict, needle: str) -> None:
    with pytest.raises(sr.RecordError) as err:
        sr.apply([], **{**_FIRST, **change})
    assert needle in str(err.value)


def test_a_version_never_changes_its_build() -> None:
    subs, _ = sr.apply([], **_FIRST)
    with pytest.raises(sr.RecordError, match="never changes"):
        sr.apply(subs, store="app-store", version="0.1.5", state="in-review", read=TODAY, build="2", today=TODAY)


# ── the `:test` hold (store-review.md § The `:test` hold) ───────────────────
# The predicate `fauna-publish stage` refuses on; that caller is pinned by
# the release tooling's own tests.


def _sub(version: str, state: str, store: str = "app-store") -> dict:
    track = {"app-store": "testflight-external", "play": "closed-testing", "microsoft-store": "private-audience"}[store]
    subs, _ = sr.apply([], **{**_FIRST, "store": store, "version": version, "state": state, "track": track})
    return subs[0]


def test_an_open_review_of_the_version_test_serves_holds_the_stage() -> None:
    open_review = _sub("0.1.5", "in-review", "play")
    subs = [_sub("0.1.5", "serving"), open_review]
    assert sr.stage_hold(subs, serving="0.1.5", staging="0.1.6") == [open_review]
    assert "play 0.1.5" in sr.hold_refusal([open_review], serving="0.1.5", staging="0.1.6")


@pytest.mark.parametrize("state", [s for s in sr.STATES if s != "in-review"])
def test_a_closed_or_withdrawn_review_does_not_hold(state: str) -> None:
    assert sr.stage_hold([_sub("0.1.5", state)], serving="0.1.5", staging="0.1.6") == []


def test_an_older_versions_open_review_does_not_hold() -> None:
    assert sr.stage_hold([_sub("0.1.4", "in-review")], serving="0.1.5", staging="0.1.6") == []


def test_nothing_on_test_holds_nothing() -> None:
    assert sr.stage_hold([_sub("0.1.5", "in-review")], serving=None, staging="0.1.6") == []


@pytest.mark.parametrize(
    "serving, staging, binds",
    [
        ("0.1.5", "0.1.6", True),  # 0.1.x owes no compatibility
        ("0.1.9", "0.2.0", True),  # leaving 0.1.x
        ("0.2.3", "0.2.4", False),  # within a major version from 0.2.0 on: a courtesy
        ("0.9.0", "1.0.0", True),  # a major bump
        ("1.2.0", "1.3.0", False),
        ("1.9.0", "2.0.0", True),
    ],
)
def test_the_hold_binds_only_in_its_window(serving: str, staging: str, binds: bool) -> None:
    assert sr.hold_binds(serving, staging) is binds
    held = sr.stage_hold([_sub(serving, "in-review")], serving=serving, staging=staging)
    assert bool(held) is binds
