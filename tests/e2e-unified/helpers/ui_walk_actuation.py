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
form is submitted (its own button where one can be named, else a real Enter
keystroke in its last text field; a disabled or never-listed field is not typed
into, and a select is filled by choosing an option it paints), and the same test is applied to the page underneath: a form
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
page's ENTRY frame, though, is also its first paint, which an app may deliver
asynchronously after the entry (linux's Events calendar list, Settings' quota,
the feed's first page): the entry and restore reads (`_read_loaded`) take the
frame only once the app reports no one-shot read owed (`reads_owed` on
`GET /registry`, `registry_frame`) on two reads in a row, bounded by
`LOAD_READS` reads, and an app that reports no count is read as loaded. A
volatile text (a clock, a relative time) would make every actuation look like a
change and so HIDE violations; the sweep reads each baseline twice and excludes
records whose text moved between the two reads, so volatility can only ever cost
a finding's sensitivity, never invent one.

**The two declared lists** live as data beside this file
(`ui_walk_actuation_lists.toml`), each entry with its reason: affordances that
legitimately change nothing, and affordances the sweep never actuates
(destructive and sovereign acts). Both start as short as honesty allows. A
third table, `filter_inputs`, names inputs that filter the page as typed (no
form, nothing to submit), each with its reason; a fourth, `clock_texts`, names
texts that move with the wall clock at a coarser grain than the two entry reads
are apart (a minute-granular uptime), read as volatile by declaration. A surface that brought only
selects (ui.yaml `type: select`) and no submit is judged the same way by rule,
not by list: those selects are view controls (`ActuationSweep._view_controls`).

**Scope today, stated so a green run is not over-read:** the canonical top-level
pages (`ui_walk.canonical_pages()`), each element id actuated ONCE per sweep (its
first occurrence — shell chrome repeated on every page is actuated on the first
page that paints it) at its first index, and one form level deep. Sub-pages a
page's actuation opens (a Settings rail entry, a detail view) are judged for the
change they make, not swept themselves.
"""

from __future__ import annotations

import datetime
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
# Presses a cycling control the sweep moved (`_cyclers`) may take to come round
# to its entry value again — a sort order of up to this many values plus one.
RESTORE_CYCLE_PRESSES = 4
# Frame reads an entry or restore read may take while the app still owes the
# page one-shot reads (`reads_owed` on `GET /registry`, `registry_frame`): a
# bound on reads, never a clock. A page still owed reads past it is read as
# found and named once (`ActuationReport.unloaded`), and its later reads take
# the frame as found at once: a read that never returns would otherwise cost
# the bound at every step of the page.
LOAD_READS = 20


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
    #: Inputs that filter what the page shows as they are typed into (a search
    #: box over a loaded list): no form, nothing to submit. The actuation that
    #: painted one is judged by its own frame change, as for a page.
    filter_inputs: list[ListEntry] = field(default_factory=list)
    #: Texts that move with the wall clock at a coarser grain than the two
    #: entry reads are apart (a humanized uptime, `1m` → `2m`): volatile by
    #: declaration, since the two-read measure sees them only when a tick falls
    #: between its reads. Compared by identity and state alone, like a
    #: measured volatile record — a cost to sensitivity, never a finding.
    clock_texts: list[ListEntry] = field(default_factory=list)

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
            filter_inputs=cls._entries(raw, "filter_inputs"),
            clock_texts=cls._entries(raw, "clock_texts"),
        )

    def never(self, element_id: str) -> ListEntry | None:
        return next((e for e in self.never_actuate if e.matches(element_id)), None)

    def unchanged_ok(self, element_id: str) -> ListEntry | None:
        return next((e for e in self.nothing_changes if e.matches(element_id)), None)

    def live_filter(self, element_id: str) -> ListEntry | None:
        return next((e for e in self.filter_inputs if e.matches(element_id)), None)

    def clock(self, element_id: str) -> ListEntry | None:
        return next((e for e in self.clock_texts if e.matches(element_id)), None)


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


_SCOPE_SEGMENT = re.compile(r"^(.*)\[(\d+)\]$")


def container_keys(records: list[dict]) -> frozenset:
    """Keys of records that scope OTHER records (another record's `scope` path
    names them, `win[0]/view[0]`): a container whose text is its children's
    labels joined (linux's window record), so its look drifts with any repaint
    anywhere inside it. The restore compares such a record by its identity and
    state, never its look — its children are compared one by one already."""
    named = set()
    for r in records:
        own = (r.get("id"), r.get("index"))
        for seg in (r.get("scope") or "").split("/"):
            m = _SCOPE_SEGMENT.match(seg)
            if m and (m.group(1), int(m.group(2))) != own:
                named.add((m.group(1), int(m.group(2))))
    return frozenset(_key(r) for r in records
                     if (r.get("id"), r.get("index")) in named)


_DIFF_SHOWN = 4


def _show(sig: tuple) -> str:
    (rid, index, scope), enabled, editable, look = sig
    out = f"{rid}[{index}]" + (f"@{scope}" if scope else "")
    if not enabled:
        out += " disabled"
    if look and (look[0] or look[1]):
        out += f" {look[0][:30]!r}" + (f" {dict(look[1])}" if look[1] else "")
    return out


def restore_diff(entry: frozenset, found: frozenset) -> str:
    """What keeps a found frame from the entry frame, for a restore line: the
    records only the entry had (`-`) and only the found frame has (`+`), a few
    of each — so a corpus says what an earlier step left behind (convention 6),
    never just that something did."""
    def side(mark: str, sigs: frozenset) -> str:
        shown = sorted(sigs, key=repr)
        more = len(shown) - _DIFF_SHOWN
        return "; ".join(f"{mark}{_show(s)}" for s in shown[:_DIFF_SHOWN]) + \
            (f"; …{more} more" if more > 0 else "")
    parts = [side("-", entry - found) if entry - found else "",
             side("+", found - entry) if found - entry else ""]
    body = " | ".join(p for p in parts if p)
    return f": {body}" if body else ""


def _named(keys) -> list[str]:
    """`id[index]` for each record key, sorted — how a report line names records."""
    return sorted({f"{k[0]}[{k[1]}]" for k in keys})


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
# lives). First match wins; the fallback is plain prose. A date is TODAY's: a
# page opens on the current period, and an item saved outside it leaves the
# frame as it was, so a successful save would read as the violation. `{today}`
# is read at fill time, never at import: a walk (or a suite) that crosses
# midnight would otherwise fill yesterday's date.
# A secp256k1 x-only public key (the generator point's x coordinate): a pubkey
# field validates the curve point, so prose would only ever disable its submit.
_FILL_PUBKEY = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
_FILL_BY_WORD: list[tuple[str, str]] = [
    (r"pubkey", _FILL_PUBKEY),
    (r"email|address-input|mailbox", "walk@example.com"),
    (r"relay", "wss://relay.example.com"),
    (r"url|server|host|endpoint|domain", "https://example.com"),
    (r"port", "443"),
    (r"dtstart|start-date|start-datetime", "{today}T10:00"),
    (r"dtend|end-date|end-datetime", "{today}T11:00"),
    (r"date", "{today}"),
    (r"time", "10:00"),
    (r"handle|username|user-name", "walk.example.com"),
    # A policy editor's cell holds a bound (a count, a size, a sum): a number.
    (r"amount|price|count|number|threshold|days|limit|cell", "5"),
    (r"phone", "+4712345678"),
]
_FILL_DEFAULT = "Walk"


def fill_value(element_id: str, kind: str = "") -> str:
    for pattern, value in _FILL_BY_WORD:
        if re.search(pattern, element_id):
            return value.replace("{today}", datetime.date.today().isoformat())
    return _FILL_DEFAULT


# ui.yaml's `type:` for a drop-down. A select is filled by CHOOSING one of the
# options it paints (`driver.option_texts`, the cross-app `options` attr), never
# typed into — every driver refuses `type_text` on one, and Enter on one too.
SELECT_KIND = "select"

#: The reason a select is listed beside the count line when the surface it came
#: on is a view control (`ActuationSweep._view_controls`), not a form.
VIEW_CONTROL_REASON = ("a select over what the page shows (ui.yaml `type: select`) "
                       "on a surface with no submit: a view control, judged with "
                       "the actuation that painted it, never as a form")


def select_choice(options: list[str], current: str) -> str | None:
    """The option a select is filled with: the first one that is not its current
    value, so the choice is a real change; None when it offers no other."""
    return next((o for o in options if o != current), None)


# A form's submit, among the actuable elements it brought: never one of these…
# Whole id tokens only: a substring test read every `backup-…` id as `back`, and
# so hid the backup-destination form's own confirm.
_NOT_SUBMIT = re.compile(
    r"(^|-)(discard|cancel|back|close|dismiss|delete|remove|clear)(-|$)|tab$")
# …and preferably one of these.
_SUBMIT = re.compile(
    r"^(create|save|submit|send|add|confirm|continue|post|apply)\b|"
    r"-(create|save|submit|send|add|confirm|continue|post|apply|set|lookup|search)(-btn|-button)?$"
)

# A sub-page's own way back to the page that opened it (`settings-nav-back`,
# `admin-nav-back`, a detail view's back button), or a surface's own way out
# (`search-cancel-button`, `feed-search-clear`, a dialog's close): a real gesture
# the restore takes before Escape, because a page that keeps its sub-page through
# a re-entry (tui's Settings rail, apple's open search) brings its entry frame
# back only this way. Taken ONLY when the actuation brought it (`_settled_baseline`).
_BACK = re.compile(r"(^|-)(back|cancel|close|dismiss|clear)(-btn|-button)?$")


def submit_candidate(opened: list[dict]) -> dict | None:
    pool = [r for r in opened if r.get("enabled") and not _NOT_SUBMIT.search(r.get("id") or "")]
    preferred = [r for r in pool if _SUBMIT.search(r.get("id") or "")]
    if preferred:
        return preferred[0]
    return pool[0] if len(pool) == 1 else None


def disabled_submit(opened: list[dict]) -> dict | None:
    """A button that reads as the form's submit but is DISABLED once the sweep
    has filled the form: the form's own validation refused the fill value. The
    step is the sweep's to fix (a better fill value), never a clean pass and
    never an Enter keystroke past the form's own "not yet"."""
    return next((r for r in opened if not r.get("enabled")
                 and not _NOT_SUBMIT.search(r.get("id") or "")
                 and _SUBMIT.search(r.get("id") or "")), None)


_SUBMIT_VERBS = frozenset(
    {"create", "save", "submit", "send", "add", "confirm", "continue", "post",
     "apply", "set", "lookup", "search"})
_ID_NOISE = frozenset({"btn", "button", "input"})


def submit_groups(fields: list[dict], buttons: list[dict]) -> list[tuple[dict, list[dict]]]:
    """Pair each field with the submit of its own section, for a surface that
    opened SEVERAL small forms at once (a linked account's relay, follow and
    signer lists, each with its own add button).

    A button is a section's submit when one of its id tokens is a submit verb;
    its NOUNS are its other tokens, less the page prefix every field shares. A
    field belongs to the button whose nouns all appear among its own tokens
    (the most nouns wins). Returns the groups in button order only when every
    field is paired and there are at least two sections; else `[]`, and the
    single-form rules decide."""
    if not fields:
        return []
    firsts = {(f.get("id") or "").split("-", 1)[0] for f in fields}
    prefix = next(iter(firsts)) if len(firsts) == 1 else None
    cands = []
    for b in buttons:
        bid = b.get("id") or ""
        toks = bid.split("-")
        if _NOT_SUBMIT.search(bid) or not _SUBMIT_VERBS & set(toks):
            continue
        nouns = set(toks) - _SUBMIT_VERBS - _ID_NOISE - ({prefix} if prefix else set())
        if nouns:
            cands.append((b, nouns))
    groups: dict[str, tuple[dict, list[dict]]] = {}
    for f in fields:
        ftoks = set((f.get("id") or "").split("-"))
        fits = [(len(n), b) for b, n in cands if n <= ftoks]
        if not fits:
            return []
        best = max(fits, key=lambda t: t[0])[1]
        groups.setdefault(best["id"], (best, []))[1].append(f)
    if len(groups) < 2:
        return []
    order = [b["id"] for b, _ in cands]
    return sorted(groups.values(), key=lambda g: order.index(g[0]["id"]))


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
    #: `where id -> reason` for each declared live-filter input an actuation
    #: painted (judged with the actuation, never as a form).
    live_filters: dict[str, str] = field(default_factory=dict)
    volatile: dict[str, int] = field(default_factory=dict)
    #: `page -> ["id[index]", …]`: the records `volatile` counts, by name.
    volatile_ids: dict[str, list[str]] = field(default_factory=dict)
    #: Fresh accounts the sweep swapped in after a submit (`fresh_account`).
    accounts_spent: int = 0
    #: `page -> what was owed`: a page whose entry or restore read still had
    #: one-shot reads in flight after `LOAD_READS` reads, read as found from
    #: then on; the app's own `reads_owed_by` names where they started.
    unloaded: dict[str, str] = field(default_factory=dict)

    def lines(self) -> list[str]:
        out = [
            f"actuation-changes-or-reports: {len(self.pages)} pages, "
            f"{len(self.actuated)} actuated, {len(self.forms_submitted)} forms "
            f"submitted, {len(self.violations)} VIOLATION(S), "
            f"{len(self.unjudged)} unjudged, {len(self.never_actuated)} held back, "
            f"{len(self.declared_unchanged)} declared-unchanged, "
            f"{len(self.current_choice)} current-choice, "
            f"{self.accounts_spent} fresh account(s) swapped in",
        ]
        out += [f"  VIOLATION {v}" for v in self.violations]
        out += [f"  submitted {f}" for f in self.forms_submitted]
        out += [f"  actuated  {a}" for a in self.actuated]
        out += [f"  current   {c}" for c in self.current_choice]
        out += [f"  unjudged  {u}" for u in self.unjudged]
        out += [f"  restore   {r}" for r in self.restore_failures]
        out += [f"  held back {k}: {v}" for k, v in self.never_actuated.items()]
        out += [f"  filter    {k}: {v}" for k, v in self.live_filters.items()]
        out += [f"  volatile  {k}: {n} record(s)"
                + (f" ({', '.join(self.volatile_ids[k])})" if self.volatile_ids.get(k) else "")
                for k, n in self.volatile.items()]
        out += [f"  unloaded  {k}: read as found after {LOAD_READS} reads — {n}"
                for k, n in self.unloaded.items()]
        return out


def sign_in_on_a_fresh_device(driver, register, sign_in) -> None:
    """Swap the app to a new, unseeded account the way a NEW DEVICE meets one:
    reset the app to factory state, register the account, then sign it in.

    **The reset is load-bearing, not tidying.** A session patch ADDS its account
    to the app's account list, and that list is one credential item bounded by
    the shared registry (`long-term-store.md` § Multi-account evolution → *The
    index is bounded*: about 11 signed-in accounts). A sweep submitting more
    forms than that, swapping by patch alone, met the bound mid-run: the app
    refused the 12th add (`AccountError::IndexFull`) and stayed on the previous
    account, so the live-identity barrier read the spent account and Settings —
    the page reached at about that count — was never swept past its first
    submit. A reset first keeps the list at one account however many submits
    the sweep makes, and also drops the spent account's device-local state,
    which "nothing seeded past sign-in" (convention 17) excludes too.

    ``register()`` returns the new account; ``sign_in(user)`` signs the app in as
    it (the caller's barrier included).
    """
    driver.reset()
    sign_in(register())


# --- the sweep ---------------------------------------------------------------


class ActuationSweep:
    """Drives one app's pages through the real actuation door.

    Page entry is a `nav` patch — fixture setup, which convention 8 exempts, and
    the same entry `ui_walk.WalkDriver` uses; every step the invariant JUDGES is a
    real gesture through `/element/click` or a real keystroke.
    """

    def __init__(self, driver, report: ActuationReport, lists: DeclaredLists,
                 kinds: dict[str, str] | None = None, fresh_account=None):
        self.driver = driver
        self.report = report
        self.lists = lists
        self.kinds = kinds or {}
        self._done_ids: set[str] = set()
        # A submitted form may create what a later affordance needs (a
        # calendar before the event save) and so move the account past the
        # first-use state the invariant is about (convention 17: nothing seeded
        # past the frontier under test). `fresh_account()` signs the app in as
        # a new, unseeded account; it is called before the next actuation
        # after any submit. Without it the sweep judges the state as found.
        self._fresh_account = fresh_account
        self._spent = False
        # Ids of this page's controls whose OWN look the sweep's press of them
        # moved (a sort order cycling its label): the one kind of record the
        # restore presses again to bring the entry back (`_settled_baseline`).
        # Only a control the sweep itself pressed — a button whose label moved
        # because of another step is never actuated by the restore.
        self._cyclers: set[str] = set()

    def _renew_if_spent(self) -> bool:
        """Swap in a fresh account after a submit; True when it did."""
        if self._spent and self._fresh_account is not None:
            self._fresh_account()
            self.report.accounts_spent += 1
            self._spent = False
            return True
        return False

    # --- frame reads ---------------------------------------------------------

    def _read(self) -> list[dict] | None:
        """The settled frame, or None — never a frame the barrier did not settle.

        A barrier that RAISED settled nothing: web's throws on a navigation
        still in flight past its bound, and a full-document navigation destroys
        the evaluate carrying it. The registry read after it may be the page
        the gesture is leaving, so judging it reports an affordance as "did not
        change" when the sweep only looked too early (web nav tabs did exactly
        that). The failure is kept on `_unsettled` for the caller's unjudged
        line, by reason."""
        self._unsettled = None
        try:
            self.driver.barrier()
        except Exception as exc:
            self._unsettled = f"{type(exc).__name__}: {exc}"
            return None
        try:
            return self.driver.registry_snapshot()
        except Exception:
            return None

    def _no_frame(self, what: str) -> str:
        """The unjudged reason for a `_read` that returned None."""
        why = getattr(self, "_unsettled", None)
        if why:
            return f"the frame did not settle after {what} (barrier failed — {why})"
        return f"no readable frame after {what}"

    def _read_loaded(self) -> tuple[list[dict] | None, int]:
        """A frame read once the app owes the page no read — its async first
        paint landed (the Events page's calendar list, Settings' quota, the
        feed's first page) — and the reads still owed if the bound ran out.

        Causal, never a wait (convention 14): each read is a barrier plus a
        frame read carrying the app's own count of one-shot reads in flight
        (`registry_frame`). The frame returned is one read with no read owed
        right after another with none owed: the first zero says every read that
        had finished is applied, the second that what it applied started no
        new one still in flight. An app publishing no count reads zero every
        time, so its frame is its second read."""
        prev_owed = None
        recs, owed = None, 0
        for _ in range(LOAD_READS):
            try:
                self.driver.barrier()
            except Exception:
                pass
            try:
                recs, owed = self.driver.registry_frame()
            except Exception:
                return None, 0
            if owed == 0 and prev_owed == 0:
                return recs, 0
            prev_owed = owed
        return recs, owed

    def _read_entry(self, page: str) -> list[dict] | None:
        """[`_read_loaded`], naming a page whose reads never settled once —
        with the source lines the app says the owed reads started from — and
        reading that page as found from then on."""
        if page in self.report.unloaded:
            return self._read()
        recs, owed = self._read_loaded()
        if owed:
            by = getattr(self.driver, "last_reads_owed_by", None) or {}
            sites = ", ".join(f"{k}×{v}" for k, v in sorted(by.items()))
            self.report.unloaded[page] = f"{owed} read(s) owed" + (f" ({sites})" if sites else "")
        return recs

    def _enter(self, page: str) -> None:
        self.driver.set_state({"nav": {"stack": [{"view": page}]}})

    def _settled_baseline(self, page: str, entry: frozenset | None,
                          volatile: frozenset) -> list[dict] | None:
        """Bring `page` back to its entry frame and return it.

        A `nav` patch keeps a page's own sub-state (an open sub-page, an open
        form), so when it does not land on the entry frame the page is
        RE-ENTERED the way a person does — from another page, through the
        page's own `{page}-tab`, the sidebar re-entry tui lands on the page's
        canonical entry state — and then a moved one-of-N choice is chosen
        again, a cycling control the sweep pressed (`_cyclers`) is pressed
        round to its entry value, and, while an overlay still stands, its
        own back affordance or Escape (the back key, a real keystroke,
        addressed to the overlay's own record first — `_escape`) is tried a
        bounded number of times."""
        self._enter(page)
        recs = self._read_entry(page)
        if recs is not None and signature(recs, volatile) != entry:
            try:
                self._enter(_OTHER_PAGE if page != _OTHER_PAGE else _OTHER_PAGE_ALT)
                self.driver.click(f"{page}-tab", 0)
                recs = self._read_entry(page)
            except Exception:
                self._enter(page)
                recs = self._read_entry(page)
        # A back affordance the entry frame itself paints leaves the page, so
        # only one the actuation brought is taken.
        entry_keys = {sig[0] for sig in entry} if entry else set()
        # The entry's current choices, read by the judge's own rule — a radio
        # painted `state: on` (tui, apple) and a rail row marked `selected`
        # (linux's list selection) alike.
        chosen = {sig[0] for sig in entry
                  if sig[3] and is_current_choice({"attrs": dict(sig[3][1])})} \
            if entry else set()
        entry_look = {sig[0]: sig[3] for sig in entry} if entry else {}
        tries = 0
        cycles = 0
        stuck: set = set()
        while recs is not None and signature(recs, volatile) != entry \
                and tries < RESTORE_ESCAPES:
            # A one-of-N choice an earlier step moved (a feed filter, a view
            # mode, a tab) persists through re-entry by design: the choice that
            # was in force at entry is chosen again, the gesture a person makes.
            again = next((r for r in recs if _key(r) in chosen and r.get("actuable")
                          and r.get("enabled") and not is_current_choice(r)), None)
            # A control that cycles its own value (a sort order) and that an
            # earlier step pressed is pressed round until its entry look
            # returns, on its own bound; one whose press changes nothing (a
            # choice already in force) is not pressed again.
            cycler = None if again is not None or cycles >= RESTORE_CYCLE_PRESSES \
                else next((r for r in recs if r.get("id") in self._cyclers
                           and _key(r) not in stuck
                           and r.get("actuable") and r.get("enabled")
                           and entry_look.get(_key(r)) is not None
                           and _look(r) != entry_look[_key(r)]), None)
            if cycler is not None:
                was = signature(recs, volatile)
                try:
                    self.driver.click(cycler["id"], cycler.get("index", 0))
                except Exception:
                    break
                cycles += 1
                recs = self._read_entry(page)
                if recs is not None and signature(recs, volatile) == was:
                    stuck.add(_key(cycler))
                continue
            back = next((r for r in recs if r.get("actuable") and r.get("enabled")
                         and _BACK.search(r.get("id") or "")
                         and _key(r) not in entry_keys), None)
            try:
                if again is not None:
                    self.driver.click(again["id"], again.get("index", 0))
                elif back is not None:
                    self.driver.click(back["id"], back.get("index", 0))
                elif not self._escape(recs, entry_keys):
                    break
            except Exception:
                break
            tries += 1
            recs = self._read_entry(page)
        return recs

    def _escape(self, recs: list[dict], entry_keys: set) -> bool:
        """Press Escape where the overlay takes it; True once one press lands.

        Addressed first to the records the actuation brought — the overlay's
        own (a sheet that paints no field still registers itself, and on apple
        only the element that registered a key door accepts a named key; every
        other record refuses it) — then, as before, to the frame's first record
        when a field is up (tui routes the key through its one keymap whatever
        the id). Bounded by `RESTORE_ESCAPES` addressees per try."""
        brought = [r for r in recs if _key(r) not in entry_keys]
        if any(r.get("editable") for r in recs):
            brought.append(recs[0])
        for r in brought[:RESTORE_ESCAPES]:
            try:
                self.driver.press_key(r["id"], "Escape")
                return True
            except Exception:
                continue
        return False

    def _entry_frame(self, page: str) -> tuple[frozenset, frozenset, frozenset] | None:
        """Enter `page` and return its restore signature, its volatile keys
        and the keys the restore compares without their look (`restore_held`)."""
        self._enter(page)
        first = self._read_entry(page)
        second = self._read_entry(page)
        if first is None or second is None:
            return None
        volatile = self._volatile(first, second)
        restore_held = volatile | container_keys(second)
        return signature(second, restore_held), volatile, restore_held

    def _volatile(self, first: list[dict], second: list[dict]) -> frozenset:
        """The measured volatile records, plus every declared clock text."""
        return volatile_keys(first, second) | frozenset(
            _key(r) for r in second if self.lists.clock(r.get("id") or ""))

    # --- one page ------------------------------------------------------------

    def sweep_page(self, page: str) -> None:
        self.report.pages.append(page)
        self._cyclers = set()
        self._renew_if_spent()
        self._enter(page)
        first = self._read_entry(page)
        second = self._read_entry(page)
        if first is None or second is None:
            self.report.unjudged.append(f"{page}: no readable registry at entry")
            return
        volatile = self._volatile(first, second)
        if volatile:
            self.report.volatile[page] = len(volatile)
            self.report.volatile_ids[page] = _named(volatile)
        # What the restore compares: the volatile records and every container's
        # joined text (`container_keys`) are compared by identity and state
        # alone; the judge of each step keeps the volatile set only.
        restore_held = volatile | container_keys(second)
        entry = signature(second, restore_held)

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

        # `[line index, diff, later steps, first line]` of the page's last
        # restore line, while the next steps keep meeting the same leftover.
        leftover = None
        for target in targets:
            if self._renew_if_spent():
                # A fresh account is a new entry frame — its own handle, its
                # own empty states — and the app's factory reset already
                # dropped whatever earlier steps left. Judged against the old
                # account's entry, every later step read as a restore failure.
                fresh = self._entry_frame(page)
                if fresh is not None:
                    entry, volatile, restore_held = fresh
            before = self._settled_baseline(page, entry, restore_held)
            if before is None:
                self.report.unjudged.append(f"{page}/{target['id']}: no frame before")
                continue
            found = signature(before, restore_held)
            if found != entry:
                diff = restore_diff(entry, found)
                if leftover is not None and leftover[1] == diff:
                    # The same leftover as the page's last restore line: a
                    # change no gesture undoes (a calendar once selected stays
                    # selected) is named once, with the steps judged against it.
                    leftover[2] += 1
                    self.report.restore_failures[leftover[0]] = \
                        f"{leftover[3]} — and {leftover[2]} later step(s) judged against it"
                else:
                    line = (f"{page}: entry state not restored before `{target['id']}` "
                            "(an earlier step left a lasting change — judged against the "
                            f"frame as found){diff}")
                    self.report.restore_failures.append(line)
                    leftover = [len(self.report.restore_failures) - 1, diff, 0, line]
            else:
                leftover = None
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
            self.report.unjudged.append(f"{where}: {self._no_frame('the click')}")
            return
        own = next((r for r in after if _key(r) == _key(target)), None)
        if own is not None and not own.get("editable") \
                and (_key(target) not in volatile) and _look(own) != _look(target):
            self._cyclers.add(target["id"])

        # The fields a person can fill: a disabled one is the form's own
        # "not yet" (convention 11 refuses typing into it), and a never-listed
        # one (a file picker) is not typed into. With none left the step is
        # judged as a plain actuation.
        opened_fields = []
        for r in new_ids(before, after, pred=lambda r: r.get("editable")):
            if not r.get("enabled", True):
                continue
            held = self.lists.never(r["id"])
            if held:
                self.report.never_actuated[f"{where} {r['id']}"] = held.reason
                continue
            live = self.lists.live_filter(r["id"])
            if live:
                self.report.live_filters[f"{where} {r['id']}"] = live.reason
                continue
            opened_fields.append(r)
        if opened_fields and self._view_controls(where, before, after, opened_fields):
            opened_fields = []
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
        if self._settled_late(where, before, volatile):
            return
        self.report.violations.append(
            f"{where}: enabled and actuated, but the frame did not change and no "
            "`error-message` was painted"
        )

    def _settled_late(self, where: str, baseline: list[dict], volatile: frozenset) -> bool:
        """Whether a frame judged "unchanged" changes on ONE more barrier read —
        then the first barrier returned before the gesture's work had landed: a
        settle race in the app's barrier, reported unjudged by name, never a
        finding against the affordance and never a silent pass. A second barrier
        is a causal anchor, not a wait (convention 14); a frame still unchanged
        after it is judged as before."""
        again = self._read()
        if again is None:
            return False
        if signature(again, volatile) == signature(baseline, volatile) and not error_painted(again):
            return False
        self.report.unjudged.append(
            f"{where}: settle race — the frame read unchanged after the barrier and "
            "changed on a second barrier read (the app's barrier returned early)"
        )
        return True

    def _is_select(self, rec: dict) -> bool:
        return self.kinds.get(rec.get("id") or "") == SELECT_KIND

    def _view_controls(self, where: str, before: list[dict], after: list[dict],
                       fields: list[dict]) -> bool:
        """A surface whose only fillable fields are selects (a declared filter
        input beside them is already set aside) and which brought no submit of
        any kind — a sort order, a folder or level filter — is a set of view
        controls, not a form: there is nothing to submit, and Enter on a
        drop-down is refused. Each select is listed beside the count line and
        the actuation that painted them is judged by its own frame change, as
        for `filter_inputs`. True when it lists them."""
        if not all(self._is_select(f) for f in fields):
            return False
        buttons = new_ids(before, after, pred=lambda r: r.get("actuable"))
        if submit_groups(fields, buttons) or submit_candidate(buttons) \
                or disabled_submit(buttons):
            return False
        for f in fields:
            self.report.live_filters[f"{where} {f['id']}"] = VIEW_CONTROL_REASON
        return True

    def _choose(self, where: str, f: dict) -> None:
        """Fill a select by choosing an option it paints (`select_choice`)."""
        fid, index = f["id"], f.get("index", 0)
        read = getattr(self.driver, "option_texts", None)
        try:
            options = read(fid, index) if read is not None else None
        except Exception as exc:
            self.report.unjudged.append(
                f"{where}: could not read the options of `{fid}` — {type(exc).__name__}: {exc}")
            return
        if options is None:
            self.report.unjudged.append(
                f"{where}: could not fill `{fid}` — the driver reads no `options` for it")
            return
        choice = select_choice(list(options), (f.get("text") or "").strip())
        if choice is None:
            self.report.unjudged.append(f"{where}: could not fill `{fid}` — select offers no option")
            return
        try:
            self.driver.select(fid, choice, index=index)
        except Exception as exc:
            self.report.unjudged.append(
                f"{where}: could not choose {choice!r} in `{fid}` — {type(exc).__name__}: {exc}")

    def _submit_form(self, where: str, underneath: list[dict], opened: list[dict],
                     fields: list[dict], volatile: frozenset) -> None:
        # Selects are chosen BEFORE any text is typed: a select that re-fires
        # the form's query on change (the search page's type filter re-runs the
        # live buffer) would, chosen after the text, run the search itself, so
        # the submit re-ran it onto the same frame and read as "stayed as it
        # was". Over a blank field the re-fire is the page's own no-op, and the
        # outcome is the submit's alone.
        for f in sorted(fields, key=lambda f: not self._is_select(f)):
            if (f.get("text") or "").strip():
                continue  # prefilled — the form's own default is a valid value
            fid = f["id"]
            if self._is_select(f):
                self._choose(where, f)
                continue
            try:
                self.driver.type_text(fid, fill_value(fid, self.kinds.get(fid, "")))
            except Exception as exc:
                self.report.unjudged.append(
                    f"{where}: could not fill `{fid}` — {type(exc).__name__}: {exc}"
                )
        filled = self._read() or opened
        # A control declared to change nothing on the frame (a link that opens
        # the system browser) is no form's submit, whatever else the form holds.
        buttons = [b for b in new_ids(underneath, filled, pred=lambda r: r.get("actuable"))
                   if not self.lists.unchanged_ok(b.get("id") or "")]
        # Several sections, each with its own submit, come first: one section's
        # button would otherwise pass for the whole surface's submit.
        groups = submit_groups(fields, buttons)
        if groups:
            self._submit_groups(where, underneath, filled, groups, volatile)
            return
        submit = submit_candidate(buttons)
        if submit is None:
            off = disabled_submit(buttons)
            if off is not None:
                self.report.unjudged.append(self._disabled_line(where, off, fields))
                return
            self._submit_by_enter(where, underneath, filled, fields, buttons, volatile)
            return
        self._click_submit(where, underneath, filled, submit, fields, volatile)

    @staticmethod
    def _disabled_line(where: str, off: dict, fields: list[dict]) -> str:
        why = (off.get("text") or "").strip()
        return (f"{where}: the submit `{off['id']}` stayed disabled after the fill "
                f"({', '.join(f['id'] for f in fields)})"
                + (f" — it reads `{why}`" if why else "")
                + "; the fill value is the sweep's to fix")

    def _submit_groups(self, where: str, underneath: list[dict], filled: list[dict],
                       groups: list[tuple[dict, list[dict]]], volatile: frozenset) -> None:
        """Submit each section of a surface that opened several small forms, in
        turn, each judged against the frame just before its own submit."""
        frame = filled
        for button, gfields in groups:
            live = next((r for r in frame if r.get("id") == button["id"]), button)
            if not live.get("enabled"):
                self.report.unjudged.append(self._disabled_line(where, live, gfields))
                continue
            self._click_submit(where, underneath, frame, live, gfields, volatile)
            frame = self._read() or frame

    def _click_submit(self, where: str, underneath: list[dict], filled: list[dict],
                      submit: dict, fields: list[dict], volatile: frozenset) -> None:
        swhere = f"{where} → {submit['id']}"
        try:
            self.driver.click(submit["id"], submit.get("index", 0))
        except Exception as exc:
            self.report.unjudged.append(f"{swhere}: submit refused — {type(exc).__name__}: {exc}")
            return
        self.report.forms_submitted.append(swhere)
        # Judged here as this form's submit: a later page that paints the same
        # id (the contacts lookup, first reached from the feed's contacts tab)
        # does not actuate it again with its field empty.
        self._done_ids.add(submit["id"])
        self._spent = True
        result = self._read()
        if result is None:
            self.report.unjudged.append(f"{swhere}: {self._no_frame('submit')}")
            return
        if error_painted(result):
            return
        still_open = any(r.get("id") == fields[0]["id"] for r in result)
        if still_open:
            if signature(result, volatile) == signature(filled, volatile) \
                    and not self._settled_late(swhere, filled, volatile):
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

    def _submit_by_enter(self, where: str, underneath: list[dict], filled: list[dict],
                         fields: list[dict], buttons: list[dict],
                         volatile: frozenset) -> None:
        """A form with no button the sweep can name as its submit is submitted
        the way a person submits a search box: a real Enter keystroke in its
        last field. Enter changing nothing is the violation only where the form
        offers no other button; beside buttons the sweep could not tell apart,
        one of them may be the submit, so the step stays unjudged."""
        # Enter goes in the last TEXT field: a drop-down refuses it.
        typed = [f for f in fields if not self._is_select(f)] or fields
        last = typed[-1]["id"]
        swhere = f"{where} → {last} ⏎"
        no_submit = (f"{where}: opened a form ({', '.join(f['id'] for f in fields)}) "
                     "with no identifiable submit")
        try:
            self.driver.press_key(last, "Enter")
        except Exception as exc:
            self.report.unjudged.append(
                f"{no_submit}; Enter refused — {type(exc).__name__}: {exc}")
            return
        result = self._read()
        if result is None:
            self.report.unjudged.append(f"{swhere}: {self._no_frame('Enter')}")
            return
        painted = error_painted(result)
        if painted or signature(result, volatile) != signature(filled, volatile):
            self.report.forms_submitted.append(swhere)
            self._spent = True
            if not painted and signature(result, volatile) == signature(underneath, volatile):
                self.report.violations.append(
                    f"{swhere}: submitted by Enter, the form closed and left the page "
                    "exactly as it was, with no `error-message` painted"
                )
            return
        if any(b.get("enabled") and not _NOT_SUBMIT.search(b.get("id") or "")
               for b in buttons):
            self.report.unjudged.append(f"{no_submit}; Enter changed nothing")
            return
        self.report.forms_submitted.append(swhere)
        self.report.violations.append(
            f"{swhere}: the form offers no submit button and Enter changed nothing, "
            "with no `error-message` painted"
        )
