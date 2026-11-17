"""Convention 17, the possibility invariants, layer (c): `actuation-changes-or-reports`.

`docs/goal/architecture/e2e-systematic-ui-walks.md` § Implementation status today
(the possibility-invariants bullet) owns the statement; this module is its engine,
and `tests/walk/test_ui_walk_actuation.py` is the scheduled sweep that drives it.

**The invariant.** On a fresh account of a real nest, with nothing seeded past
sign-in, every ENABLED affordance a page offers is actuated through the real door,
and once the frame settles one of two things must hold: the frame's registry
differs from the one before (a surface opened, a row appeared, a value changed,
the page changed), or `error-message` is painted. Where the actuation opened a
form, every empty field is filled by its kind with one fixed valid value, the
form is submitted, and the same test is applied to the page underneath: a form
that closes and leaves the page exactly as it was, with no error, is the
violation. That is the bug class *an affordance offered that cannot succeed* —
the zero-calendar event save that started it reached the log and nobody else.

**What it is not.** It is not where breadth lives (layer (a) is), and it does not
judge whether the change was the RIGHT change — only that the person was told
something. That is deliberately weak: the invariant has to be true by
construction on every legitimate state (convention 17's first discipline), and
"the frame changed or an error was painted" is the weakest statement every honest
affordance satisfies.

**Settling is causal, never a wait** (convention 14). tui's `/element/click`
replies only once the gesture's network half has landed, and `barrier()` then
drains the UI channel, so the registry read after it is the settled frame. A
volatile text (a clock, a relative time) would make every actuation look like a
change and so HIDE violations; the sweep reads each baseline twice and excludes
records whose text moved between the two reads, so volatility can only ever cost
a finding's sensitivity, never invent one.

**The two declared lists** live as data beside this file
(`ui_walk_actuation_lists.toml`), each entry with its reason: affordances that
legitimately change nothing, and affordances the sweep never actuates
(destructive and sovereign acts). Both start as short as honesty allows.

**Scope today, stated so a green run is not over-read:** the canonical top-level
pages (`ui_walk.canonical_pages()`), each element id actuated ONCE per sweep (its
first occurrence — shell chrome repeated on every page is actuated on the first
page that paints it) at its first index, and one form level deep. Sub-pages a
page's actuation opens (a Settings rail entry, a detail view) are judged for the
change they make, not swept themselves.
"""

from __future__ import annotations

import fnmatch
import re
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

import yaml

_HERE = Path(__file__).resolve().parent
_LISTS = _HERE / "ui_walk_actuation_lists.toml"
_UI_YAML = _HERE.parent / "ui.yaml"

ERROR_ID = "error-message"

# The page a re-entry starts from (and the one used when the page IS it).
_OTHER_PAGE = "settings"
_OTHER_PAGE_ALT = "feed"

# Escape presses tried to bring a page back to its entry state after an
# actuation left an overlay up. Bounded: a page that needs more is reported.
RESTORE_ESCAPES = 3


# --- the declared lists ------------------------------------------------------


@dataclass(frozen=True)
class ListEntry:
    pattern: str
    reason: str

    def matches(self, element_id: str) -> bool:
        return fnmatch.fnmatchcase(element_id, self.pattern)


@dataclass
class DeclaredLists:
    never_actuate: list[ListEntry] = field(default_factory=list)
    nothing_changes: list[ListEntry] = field(default_factory=list)

    @staticmethod
    def _entries(raw, table: str) -> list[ListEntry]:
        out = []
        for i, row in enumerate(raw.get(table) or []):
            pattern = (row.get("id") or "").strip()
            reason = (row.get("reason") or "").strip()
            if not pattern or not reason:
                raise AssertionError(
                    f"{_LISTS} [[{table}]] entry {i} has no `id` or no `reason` — "
                    "every declared exception carries its reason, or the list "
                    "becomes the place violations go to hide."
                )
            out.append(ListEntry(pattern, reason))
        return out

    @classmethod
    def load(cls, path: Path = _LISTS) -> "DeclaredLists":
        with open(path, "rb") as f:
            raw = tomllib.load(f)
        return cls(
            never_actuate=cls._entries(raw, "never_actuate"),
            nothing_changes=cls._entries(raw, "nothing_changes"),
        )

    def never(self, element_id: str) -> ListEntry | None:
        return next((e for e in self.never_actuate if e.matches(element_id)), None)

    def unchanged_ok(self, element_id: str) -> ListEntry | None:
        return next((e for e in self.nothing_changes if e.matches(element_id)), None)


# --- reading a frame ---------------------------------------------------------


def element_kinds(path: Path = _UI_YAML) -> dict[str, str]:
    """`elements.<id>.type` from ui.yaml — the field kind a fill value is chosen by."""
    with open(path, encoding="utf-8") as f:
        spec = yaml.safe_load(f)
    return {
        k: (v or {}).get("type") or ""
        for k, v in (spec.get("elements") or {}).items()
        if isinstance(v, dict)
    }


def _key(rec: dict) -> tuple:
    return (rec.get("id"), rec.get("index"), rec.get("scope") or "")


def _look(rec: dict) -> tuple:
    """What a person sees of one record: its text and its attrs (a toggle's
    `state`, a checkbox's `checked`, a copy's `copied`)."""
    attrs = rec.get("attrs") or {}
    return (rec.get("text") or "", tuple(sorted((str(k), str(v)) for k, v in attrs.items())))


def signature(records: list[dict], volatile: frozenset = frozenset()) -> frozenset:
    """What "the frame changed" compares: every record's identity, enabled state,
    text and attrs — except the look of records known to move on their own."""
    out = set()
    for r in records:
        k = _key(r)
        look = None if k in volatile else _look(r)
        out.add((k, bool(r.get("enabled")), bool(r.get("editable")), look))
    return frozenset(out)


def volatile_keys(first: list[dict], second: list[dict]) -> frozenset:
    """Records whose look moved between two reads of one unmoved frame."""
    a = {_key(r): _look(r) for r in first}
    b = {_key(r): _look(r) for r in second}
    return frozenset(k for k in a.keys() & b.keys() if a[k] != b[k])


def is_current_choice(rec: dict) -> bool:
    """The record reads as the CURRENT choice of a one-of-N control (a radio or
    segment painted `on`, a tab marked selected). Re-choosing it changes nothing
    by construction — a toggle in the same state would flip, and so change."""
    attrs = rec.get("attrs") or {}
    return str(attrs.get("state", "")).lower() in {"on", "active", "selected", "current"} \
        or str(attrs.get("selected", "")).lower() == "true"


def error_painted(records: list[dict]) -> str:
    for r in records:
        if r.get("id") == ERROR_ID and (r.get("text") or "").strip():
            return r["text"].strip()
    return ""


def new_ids(before: list[dict], after: list[dict], *, pred) -> list[dict]:
    """Records of `after` whose id `before` did not have, filtered by `pred`,
    first occurrence of each id only."""
    had = {r.get("id") for r in before}
    seen: set = set()
    out = []
    for r in after:
        rid = r.get("id")
        if rid in had or rid in seen or not pred(r):
            continue
        seen.add(rid)
        out.append(r)
    return out


# --- filling and submitting a form -------------------------------------------

# One fixed valid value per field kind, chosen by the id's own words (ui.yaml's
# `type:` says text_input for nearly every field, so the id is where the kind
# lives). First match wins; the fallback is plain prose.
_FILL_BY_WORD: list[tuple[str, str]] = [
    (r"email|address-input|mailbox", "walk@example.com"),
    (r"url|server|host|endpoint|domain|relay", "https://example.com"),
    (r"port", "443"),
    (r"dtstart|start-date|start-datetime", "2026-12-01T10:00"),
    (r"dtend|end-date|end-datetime", "2026-12-01T11:00"),
    (r"date", "2026-12-01"),
    (r"time", "10:00"),
    (r"handle|username|user-name", "walk.example.com"),
    (r"amount|price|count|number|threshold|days|limit", "5"),
    (r"phone", "+4712345678"),
]
_FILL_DEFAULT = "Walk"


def fill_value(element_id: str, kind: str = "") -> str:
    for pattern, value in _FILL_BY_WORD:
        if re.search(pattern, element_id):
            return value
    return _FILL_DEFAULT


# A form's submit, among the actuable elements it brought: never one of these…
_NOT_SUBMIT = re.compile(r"discard|cancel|back|close|dismiss|delete|remove|clear|tab$")
# …and preferably one of these.
_SUBMIT = re.compile(
    r"^(create|save|submit|send|add|confirm|continue|post|apply)\b|"
    r"-(create|save|submit|send|add|confirm|continue|post|apply|set)(-btn|-button)?$"
)


def submit_candidate(opened: list[dict]) -> dict | None:
    pool = [r for r in opened if r.get("enabled") and not _NOT_SUBMIT.search(r.get("id") or "")]
    preferred = [r for r in pool if _SUBMIT.search(r.get("id") or "")]
    if preferred:
        return preferred[0]
    return pool[0] if len(pool) == 1 else None


# --- the report --------------------------------------------------------------


@dataclass
class ActuationReport:
    """What the sweep did, with its non-vacuity counts beside its findings."""

    pages: list[str] = field(default_factory=list)
    actuated: list[str] = field(default_factory=list)
    forms_submitted: list[str] = field(default_factory=list)
    violations: list[str] = field(default_factory=list)
    #: `where -> reason` for each element the declared never-list held back.
    never_actuated: dict[str, str] = field(default_factory=dict)
    declared_unchanged: list[str] = field(default_factory=list)
    #: Unchanged actuations of a control already reading as the current choice
    #: (`is_current_choice`) — benign by construction, kept visible.
    current_choice: list[str] = field(default_factory=list)
    #: Steps the sweep could not judge (a refused click, an unreadable frame, a
    #: form with no identifiable submit). Never folded into the clean count.
    unjudged: list[str] = field(default_factory=list)
    restore_failures: list[str] = field(default_factory=list)
    volatile: dict[str, int] = field(default_factory=dict)

    def lines(self) -> list[str]:
        out = [
            f"actuation-changes-or-reports: {len(self.pages)} pages, "
            f"{len(self.actuated)} actuated, {len(self.forms_submitted)} forms "
            f"submitted, {len(self.violations)} VIOLATION(S), "
            f"{len(self.unjudged)} unjudged, {len(self.never_actuated)} held back, "
            f"{len(self.declared_unchanged)} declared-unchanged, "
            f"{len(self.current_choice)} current-choice",
        ]
        out += [f"  VIOLATION {v}" for v in self.violations]
        out += [f"  submitted {f}" for f in self.forms_submitted]
        out += [f"  unjudged  {u}" for u in self.unjudged]
        out += [f"  restore   {r}" for r in self.restore_failures]
        out += [f"  held back {k}: {v}" for k, v in self.never_actuated.items()]
        out += [f"  volatile  {k}: {n} record(s)" for k, n in self.volatile.items()]
        return out


# --- the sweep ---------------------------------------------------------------


class ActuationSweep:
    """Drives one app's pages through the real actuation door.

    Page entry is a `nav` patch — fixture setup, which convention 8 exempts, and
    the same entry `ui_walk.WalkDriver` uses; every step the invariant JUDGES is a
    real gesture through `/element/click` or a real keystroke.
    """

    def __init__(self, driver, report: ActuationReport, lists: DeclaredLists,
                 kinds: dict[str, str] | None = None):
        self.driver = driver
        self.report = report
        self.lists = lists
        self.kinds = kinds or {}
        self._done_ids: set[str] = set()

    # --- frame reads ---------------------------------------------------------

    def _read(self) -> list[dict] | None:
        try:
            self.driver.barrier()
        except Exception:
            pass
        try:
            return self.driver.registry_snapshot()
        except Exception:
            return None

    def _enter(self, page: str) -> None:
        self.driver.set_state({"nav": {"stack": [{"view": page}]}})

    def _settled_baseline(self, page: str, entry: frozenset | None,
                          volatile: frozenset) -> list[dict] | None:
        """Bring `page` back to its entry frame and return it.

        A `nav` patch keeps a page's own sub-state (an open sub-page, an open
        form), so when it does not land on the entry frame the page is
        RE-ENTERED the way a person does — from another page, through the
        page's own `{page}-tab`, the sidebar re-entry tui lands on the page's
        canonical entry state — and then, while an overlay still stands,
        Escape (the back key, a real keystroke) is tried a bounded number of
        times."""
        self._enter(page)
        recs = self._read()
        if recs is not None and signature(recs, volatile) != entry:
            try:
                self._enter(_OTHER_PAGE if page != _OTHER_PAGE else _OTHER_PAGE_ALT)
                self.driver.click(f"{page}-tab", 0)
                recs = self._read()
            except Exception:
                self._enter(page)
                recs = self._read()
        tries = 0
        while recs is not None and signature(recs, volatile) != entry \
                and tries < RESTORE_ESCAPES and any(r.get("editable") for r in recs):
            try:
                self.driver.press_key(recs[0]["id"], "Escape")
            except Exception:
                break
            tries += 1
            recs = self._read()
        return recs

    # --- one page ------------------------------------------------------------

    def sweep_page(self, page: str) -> None:
        self.report.pages.append(page)
        self._enter(page)
        first = self._read()
        second = self._read()
        if first is None or second is None:
            self.report.unjudged.append(f"{page}: no readable registry at entry")
            return
        volatile = volatile_keys(first, second)
        if volatile:
            self.report.volatile[page] = len(volatile)
        entry = signature(second, volatile)

        targets = []
        for r in second:
            rid = r.get("id") or ""
            if not (r.get("actuable") and r.get("enabled")) or rid in self._done_ids:
                continue
            if rid == f"{page}-tab":
                # A page's own tab re-enters the page it is on; it is judged
                # from the first OTHER page that paints it.
                continue
            self._done_ids.add(rid)
            held = self.lists.never(rid)
            if held:
                self.report.never_actuated[f"{page}/{rid}"] = held.reason
                continue
            targets.append(r)

        for target in targets:
            before = self._settled_baseline(page, entry, volatile)
            if before is None:
                self.report.unjudged.append(f"{page}/{target['id']}: no frame before")
                continue
            if signature(before, volatile) != entry:
                self.report.restore_failures.append(
                    f"{page}: entry state not restored before `{target['id']}` "
                    "(an earlier step left a lasting change — judged against the "
                    "frame as found)"
                )
            # The target may have moved index or gone since the entry read.
            live = next((r for r in before if r.get("id") == target["id"]
                         and r.get("actuable") and r.get("enabled")), None)
            if live is None:
                self.report.unjudged.append(f"{page}/{target['id']}: gone before its turn")
                continue
            self._actuate(page, live, before, volatile)

    def _actuate(self, page: str, target: dict, before: list[dict],
                 volatile: frozenset) -> None:
        where = f"{page}/{target['id']}[{target.get('index', 0)}]"
        try:
            self.driver.click(target["id"], target.get("index", 0))
        except Exception as exc:
            self.report.unjudged.append(f"{where}: click refused — {type(exc).__name__}: {exc}")
            return
        self.report.actuated.append(where)
        after = self._read()
        if after is None:
            self.report.unjudged.append(f"{where}: no readable frame after")
            return

        opened_fields = new_ids(before, after, pred=lambda r: r.get("editable"))
        if opened_fields:
            self._submit_form(where, before, after, opened_fields, volatile)
            return

        if signature(after, volatile) != signature(before, volatile) or error_painted(after):
            return
        if self.lists.unchanged_ok(target["id"]):
            self.report.declared_unchanged.append(where)
            return
        if is_current_choice(target):
            self.report.current_choice.append(where)
            return
        self.report.violations.append(
            f"{where}: enabled and actuated, but the frame did not change and no "
            "`error-message` was painted"
        )

    def _submit_form(self, where: str, underneath: list[dict], opened: list[dict],
                     fields: list[dict], volatile: frozenset) -> None:
        for f in fields:
            if (f.get("text") or "").strip():
                continue  # prefilled — the form's own default is a valid value
            fid = f["id"]
            try:
                self.driver.type_text(fid, fill_value(fid, self.kinds.get(fid, "")))
            except Exception as exc:
                self.report.unjudged.append(
                    f"{where}: could not fill `{fid}` — {type(exc).__name__}: {exc}"
                )
        filled = self._read() or opened
        submit = submit_candidate(
            new_ids(underneath, filled, pred=lambda r: r.get("actuable"))
        )
        if submit is None:
            self.report.unjudged.append(
                f"{where}: opened a form ({', '.join(f['id'] for f in fields)}) "
                "with no identifiable submit"
            )
            return
        swhere = f"{where} → {submit['id']}"
        try:
            self.driver.click(submit["id"], submit.get("index", 0))
        except Exception as exc:
            self.report.unjudged.append(f"{swhere}: submit refused — {type(exc).__name__}: {exc}")
            return
        self.report.forms_submitted.append(swhere)
        result = self._read()
        if result is None:
            self.report.unjudged.append(f"{swhere}: no readable frame after submit")
            return
        if error_painted(result):
            return
        still_open = any(r.get("id") == fields[0]["id"] for r in result)
        if still_open:
            if signature(result, volatile) == signature(filled, volatile):
                self.report.violations.append(
                    f"{swhere}: submitted, the form stayed as it was and no "
                    "`error-message` was painted"
                )
            return
        if signature(result, volatile) == signature(underneath, volatile):
            self.report.violations.append(
                f"{swhere}: submitted, the form closed and left the page exactly "
                "as it was, with no `error-message` painted"
            )
