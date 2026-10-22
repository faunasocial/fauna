"""The store review record's grammar and its one writer.

Owner: docs/goal/architecture/installers/store-review.md § The record.
`installer/store-review.toml` holds one `[[submission]]` table per
(store, version). This module is the single home of the record's
enumerations (the tier_1 pin `tests/e2e-unified/tests/test_store_review_record.py`
imports them), its validation, and its canonical rendering — the file is
always exactly `render(submissions)`, and the header carries a digest of the
tables, so a hand edit (a misspelling, or a well-spelled change that skipped
`just store-review`) fails the pin.

Stdlib only: tomllib reads, the renderer writes.
"""
from __future__ import annotations

import datetime as _dt
import hashlib
import re
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
RECORD = REPO / "installer" / "store-review.toml"

STORES = ("play", "app-store", "microsoft-store")

TRACKS = {
    "play": ("internal-testing", "closed-testing", "open-testing", "production"),
    "app-store": ("testflight-internal", "testflight-external", "app-store"),
    "microsoft-store": ("private-audience", "public"),
}

STATES = (
    "built",
    "uploaded",
    "in-review",
    "approved",
    "rejected",
    "withdrawn",
    "serving",
    "superseded",
)

FLAVOURS = ("release", "store-safe", "store-no-payments")

# Field order is the rendering order.
FIELDS = ("store", "version", "build", "track", "flavour", "state", "since", "read", "note")

NOTE_MAX = 400

_VERSION = re.compile(r"^\d+\.\d+\.\d+$")
_ADDRESS = re.compile(r"[^\s@]+@[^\s@]+\.[^\s@]+")
_CONTROL = re.compile(r"[\x00-\x1f\x7f]")

HEADER = """\
# The live state of every store submission — one [[submission]] table per
# (store, version), the newest state replacing the previous one in place.
# Owner and grammar: docs/goal/architecture/installers/store-review.md
# § The record; pinned by tests/e2e-unified/tests/test_store_review_record.py.
#
# A store with no table has nothing built or uploaded. "Is anything in
# review?" is answered by any table whose state is "in-review". Every state
# rests on a dated console reading (`read`); nothing is written from a plan.
# No secret, no address, no tester and no person is ever written here.
#
# Written ONLY by `just store-review` (scripts/store-review.py); never edit
# by hand — the digest below covers every table, and the pin fails on a
# hand edit.
"""

_DIGEST_PREFIX = "# written-by: just store-review sha256:"


class RecordError(ValueError):
    """A submission the record's grammar refuses."""


def problems(sub: dict) -> list[str]:
    """Every way one submission breaks the grammar (empty when it is sound)."""
    where = f"{sub.get('store')} {sub.get('version')}"
    out: list[str] = []
    if set(sub) != set(FIELDS):
        return [f"{where}: fields {sorted(sub)}, want {sorted(FIELDS)}"]
    if sub["store"] not in STORES:
        out.append(f"{where}: store {sub['store']!r} is not one of {', '.join(STORES)}")
    elif sub["track"] not in TRACKS[sub["store"]]:
        out.append(
            f"{where}: track {sub['track']!r} is not one of {sub['store']}'s: "
            + ", ".join(TRACKS[sub["store"]])
        )
    if sub["state"] not in STATES:
        out.append(f"{where}: state {sub['state']!r} is not one of {', '.join(STATES)}")
    if sub["flavour"] not in FLAVOURS:
        out.append(f"{where}: flavour {sub['flavour']!r} is not one of {', '.join(FLAVOURS)}")
    if not isinstance(sub["version"], str) or not _VERSION.match(sub["version"]):
        out.append(f"{where}: version {sub['version']!r} is not MAJOR.MINOR.PATCH")
    if not isinstance(sub["build"], str) or not sub["build"].strip():
        out.append(f"{where}: build must be a non-empty string")
    for key in ("since", "read"):
        if not isinstance(sub[key], _dt.date) or isinstance(sub[key], _dt.datetime):
            out.append(f"{where}: {key} must be a date")
    if not out and sub["read"] < sub["since"]:
        out.append(f"{where}: read {sub['read']} is before since {sub['since']}")
    note = sub["note"]
    if not isinstance(note, str) or not note.strip():
        out.append(f"{where}: note must be a sentence or two")
    else:
        if len(note) > NOTE_MAX:
            out.append(f"{where}: note is {len(note)} characters; a sentence or two, at most {NOTE_MAX}")
        if _ADDRESS.search(note):
            out.append(f"{where}: the note names an address; no person, address or secret belongs here")
        if _CONTROL.search(note):
            out.append(f"{where}: the note carries a control character; one line of prose")
    return out


def _string(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _value(value: object) -> str:
    if isinstance(value, _dt.date):
        return value.isoformat()
    return _string(str(value))


def _tables(submissions: list[dict]) -> str:
    blocks = []
    for sub in submissions:
        lines = ["[[submission]]"] + [f"{key} = {_value(sub[key])}" for key in FIELDS]
        blocks.append("\n".join(lines) + "\n")
    return "\n".join(blocks)


def digest(submissions: list[dict]) -> str:
    return hashlib.sha256(_tables(submissions).encode()).hexdigest()


def render(submissions: list[dict]) -> str:
    """The record's one canonical text for these submissions."""
    head = HEADER + _DIGEST_PREFIX + digest(submissions) + "\n"
    if not submissions:
        return head
    return head + "\n" + _tables(submissions)


def load(path: Path = RECORD) -> list[dict]:
    return loads(path.read_text())


def loads(text: str) -> list[dict]:
    data = tomllib.loads(text)
    if set(data) - {"submission"}:
        raise RecordError(f"unexpected top-level keys: {sorted(set(data) - {'submission'})}")
    return data.get("submission", [])


def apply(
    submissions: list[dict],
    *,
    store: str,
    version: str,
    state: str,
    read: _dt.date,
    since: _dt.date | None = None,
    track: str | None = None,
    build: str | None = None,
    flavour: str | None = None,
    note: str | None = None,
    today: _dt.date | None = None,
) -> tuple[list[dict], str]:
    """Return (new submissions, "created" | "updated"), or raise RecordError.

    The first write of a (store, version) creates its table and needs every
    field. A later one replaces state, since, read and note in place (and the
    track, when the build moves to another of that store's tracks); the build
    identity and flavour of a version never change — a different build is the
    next version (product-version.md § Reships).
    """
    today = today or _dt.date.today()
    if read > today:
        raise RecordError(f"read {read} is in the future; write the date you looked at the console")
    if since is not None and since > today:
        raise RecordError(f"since {since} is in the future")
    out = [dict(s) for s in submissions]
    existing = next((s for s in out if s["store"] == store and s["version"] == version), None)
    if existing is None:
        missing = [n for n, v in (("--track", track), ("--build", build), ("--flavour", flavour), ("--note", note)) if v is None]
        if missing:
            raise RecordError(f"{store} {version} has no table yet; the first write needs {', '.join(missing)}")
        sub = {
            "store": store,
            "version": version,
            "build": build,
            "track": track,
            "flavour": flavour,
            "state": state,
            "since": since or read,
            "read": read,
            "note": note,
        }
        out.append(sub)
        verb = "created"
    else:
        for key, given in (("build", build), ("flavour", flavour)):
            if given is not None and given != existing[key]:
                raise RecordError(
                    f"{store} {version} is {key} {existing[key]!r}; a version's {key} never changes "
                    "(a different build ships as the next version)"
                )
        sub = existing
        if since is None:
            since = sub["since"] if state == sub["state"] else read
        sub.update(state=state, since=since, read=read)
        if track is not None:
            sub["track"] = track
        if note is not None:
            sub["note"] = note
        verb = "updated"
    errors = problems(sub)
    if errors:
        raise RecordError("; ".join(errors))
    return out, verb


def check_text(text: str) -> list[str]:
    """Every reason `text` is not a record `just store-review` wrote."""
    try:
        subs = tomllib.loads(text).get("submission", [])
    except tomllib.TOMLDecodeError as err:
        return [f"not TOML: {err}"]
    errors = [p for s in subs for p in problems(s)]
    seen: set[tuple[str, str]] = set()
    for s in subs:
        key = (s.get("store"), s.get("version"))
        if key in seen:
            errors.append(f"two tables for {key}; the newest state replaces the old in place")
        seen.add(key)
    if not errors and text != render(subs):
        errors.append(
            "the record is not what `just store-review` writes (a hand edit, or a stale digest); "
            "rewrite the change through the recipe"
        )
    return errors


# ── the `:test` hold ────────────────────────────────────────────────────────
# Owner: docs/goal/architecture/installers/store-review.md § The `:test` hold
# (the rule itself is release-table.md § Tag gate and release table, half (a)).
# The one caller is `fauna-publish stage` (the release tooling's stage step), the
# public image dispatch that moves `:test`.

OPEN = "in-review"


def version_key(version: str) -> tuple[int, int, int]:
    major, minor, patch = (int(part) for part in version.split("-", 1)[0].split("."))
    return major, minor, patch


def hold_binds(serving: str, staging: str) -> bool:
    """Whether a review of `serving`'s app build can hold a stage of `staging`.

    Only while a reviewer could meet a nest the app under review was not built
    against: throughout 0.1.x, which owes no compatibility, and across a major
    bump; within one major version from 0.2.0 on the compatibility rule makes
    the mismatch safe and the hold lapses to a courtesy.
    """
    held, new = version_key(serving), version_key(staging)
    return held[:2] == (0, 1) or held[0] != new[0]


def stage_hold(submissions: list[dict], *, serving: str | None, staging: str) -> list[dict]:
    """The open reviews that refuse moving `:test` from `serving` to `staging`.

    Only an `in-review` submission of the version `:test` serves holds — at any
    store, on any track (a build is `in-review` only on a reviewed one). An open
    review of an older version does not: `:test` has already moved past it.
    Empty when nothing serves `:test`, when the stage does not move it past
    `serving`, or outside the binding window.
    """
    if serving is None or version_key(staging) <= version_key(serving):
        return []
    if not hold_binds(serving, staging):
        return []
    return [s for s in submissions if s["version"] == serving and s["state"] == OPEN]


def hold_refusal(open_reviews: list[dict], *, serving: str, staging: str) -> str:
    """The refusal a held stage prints: each open review, and the one way out."""
    lines = [
        f":test serves {serving} and a store review of that version's app build is open, "
        f"so {staging} is not staged (store-review.md § The `:test` hold):"
    ]
    for s in open_reviews:
        lines.append(
            f"  {s['store']} {s['version']} (build {s['build']}, {s['track']}) in review "
            f"since {s['since']}, read {s['read']}"
        )
    lines.append(
        "Wait for the store to answer and record it (`just store-review <store> "
        f"{serving} <state> --read <date>`). The one alternative is the owner's, in the "
        "moment: withdraw the review in that console and record it `withdrawn`. A session "
        "never withdraws a review and never bypasses the hold."
    )
    return "\n".join(lines)
