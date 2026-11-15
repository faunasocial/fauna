"""The store legs' shared grammar: the texts a leg sends, how far it goes, what it reports.

Owner: docs/goal/architecture/installers/store-review.md § The store legs.
The three legs (Google Play and the Microsoft Store in `release.yml`'s
`release` job, the App Store in `release-macos.yml`'s `sign` job) each reach
their store by their own means; what they share lives here, once:

- **The per-release texts** — `installer/store-texts/<store>/<name>.txt`, where
  `<name>` is `whats-new` (Play, the Microsoft Store, the App Store's public
  version) or `what-to-test` (TestFlight). The first line names the version the
  text was approved for — `Version: 0.1.6` — or says it is version-neutral —
  `Version: any` — a blank line follows, then the text itself, sent verbatim. A
  missing file, a malformed header, an empty text, or a text approved for
  another version is refused before any store call (§ The store legs, *The texts
  a store shows come from the tree at the tag*). No leg composes, shortens or
  translates a text.
- **How far each leg goes** — `installer/store-legs.toml`, one `[<store>]` table
  per store holding `furthest = "testing" | "public"`; a store with no table is
  `testing`. `public` is written only on the owner's word.
- **What a leg reports** — the run artifact `store-legs.json`:
  `{"schema": 1, "legs": [<leg>, …]}`, each leg `store`, `version`, `build`,
  `track`, `state` (the store-review record's enumerations,
  `scripts/_store_review.py`), `read` (the UTC minute of the store's own
  reading, `YYYY-MM-DDTHH:MMZ`), `ids` (the store's identifiers for the upload,
  a flat string → string object) and `note`. The attempt's `stores` step reads
  it as DATA and writes each state through `just store-review`, the record's
  only writer; a leg never writes the record. No secret, no address, no tester.
  An optional `tries` list beside `legs` holds a leg's every try at its store —
  `store`, `at` (the UTC minute, as `read`), `outcome` (`refused-permission`:
  the store refused the leg's credential permission, and the leg waited and
  tried again; `done`; `red`: any other failure) and `note` — written even when
  the leg ends red, so the attempt's `stores` step records the tries and the
  review reports the time spent waiting on a store's permission (§ The store
  legs, *A refusal of permission is waited out*).

Stdlib only, so a workflow step runs it with nothing but uv's interpreter.
"""
from __future__ import annotations

import json
import re
import tomllib
from pathlib import Path

import _store_review as sr

REPO = Path(__file__).resolve().parent.parent
TEXTS = REPO / "installer" / "store-texts"
SETTINGS = REPO / "installer" / "store-legs.toml"

SCHEMA = 1
TEXT_NAMES = ("whats-new", "what-to-test")
FURTHEST = ("testing", "public")
LEG_FIELDS = ("store", "version", "build", "track", "state", "read", "ids", "note")
TRY_FIELDS = ("store", "at", "outcome", "note")
TRY_OUTCOMES = ("refused-permission", "done", "red")
#: The texts each store's leg refuses to run without, by how far it goes (§ The store legs,
#: *The texts a store shows come from the tree at the tag*): Play and the Microsoft Store
#: send *What's new* on every track; the App Store sends TestFlight's *What to Test*, and its
#: public version's *What's New* too once its `furthest` is `public`. The one list — the legs
#: and the release's check of the public texts read it.
LEG_TEXTS = {
    "play": {"testing": ("whats-new",), "public": ("whats-new",)},
    "microsoft-store": {"testing": ("whats-new",), "public": ("whats-new",)},
    "app-store": {"testing": ("what-to-test",), "public": ("what-to-test", "whats-new")},
}


def required_texts(store: str, how_far: str) -> tuple[str, ...]:
    """The text names `store`'s leg refuses without when it goes `how_far`."""
    if store not in LEG_TEXTS:
        raise LegError(f"store {store!r} is not one of {', '.join(LEG_TEXTS)}")
    if how_far not in FURTHEST:
        raise LegError(f"furthest {how_far!r} is not one of {', '.join(FURTHEST)}")
    return LEG_TEXTS[store][how_far]

_VERSION = re.compile(r"^\d+\.\d+\.\d+$")
_HEADER = re.compile(r"^Version: (any|\d+\.\d+\.\d+)$")
_MINUTE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z$")
_ADDRESS = re.compile(r"[^\s@]+@[^\s@]+\.[^\s@]+")
NOTE_MAX = 400


class LegError(ValueError):
    """An input or a report the store legs' grammar refuses."""


def text_path(store: str, name: str, root: Path = TEXTS) -> Path:
    if store not in sr.STORES:
        raise LegError(f"store {store!r} is not one of {', '.join(sr.STORES)}")
    if name not in TEXT_NAMES:
        raise LegError(f"text {name!r} is not one of {', '.join(TEXT_NAMES)}")
    return root / store / f"{name}.txt"


def parse_text(raw: str, *, where: str, version: str) -> str:
    """The text body of one store text file, or LegError naming why it is refused."""
    if not _VERSION.match(version):
        raise LegError(f"version {version!r} is not MAJOR.MINOR.PATCH")
    lines = raw.split("\n")
    head = _HEADER.match(lines[0].rstrip("\r")) if lines else None
    if head is None:
        raise LegError(f"{where}: the first line must be `Version: <x.y.z>` or `Version: any`")
    if len(lines) < 2 or lines[1].strip():
        raise LegError(f"{where}: a blank line must follow the Version line")
    approved = head.group(1)
    if approved not in ("any", version):
        raise LegError(f"{where}: approved for {approved}, not {version}")
    body = "\n".join(lines[2:]).strip("\n")
    if not body.strip():
        raise LegError(f"{where}: the text is empty")
    return body


def read_text(store: str, name: str, version: str, root: Path = TEXTS) -> str:
    path = text_path(store, name, root)
    where = str(path.relative_to(root.parent.parent)) if path.is_relative_to(root.parent.parent) else str(path)
    if not path.is_file():
        raise LegError(f"{where}: missing — the version's public-texts round lands it")
    return parse_text(path.read_text(encoding="utf-8"), where=where, version=version)


def furthest(store: str, path: Path = SETTINGS) -> str:
    """How far the store's leg goes: `testing` unless the settings say `public`."""
    if store not in sr.STORES:
        raise LegError(f"store {store!r} is not one of {', '.join(sr.STORES)}")
    if not path.is_file():
        return "testing"
    table = tomllib.loads(path.read_text(encoding="utf-8")).get(store, {})
    value = table.get("furthest", "testing")
    if value not in FURTHEST:
        raise LegError(f"{path.name}: [{store}] furthest {value!r} is not one of {', '.join(FURTHEST)}")
    return value


def leg_problems(leg: object) -> list[str]:
    """Every way one reported leg breaks the grammar (empty when it is sound)."""
    if not isinstance(leg, dict):
        return ["a leg is not a JSON object"]
    out: list[str] = []
    missing = [f for f in LEG_FIELDS if f not in leg]
    extra = sorted(set(leg) - set(LEG_FIELDS))
    if missing:
        out.append(f"missing {', '.join(missing)}")
    if extra:
        out.append(f"unknown field(s) {', '.join(extra)}")
    if missing:
        return out
    if leg["store"] not in sr.STORES:
        out.append(f"store {leg['store']!r} is not one of {', '.join(sr.STORES)}")
    elif leg["track"] not in sr.TRACKS[leg["store"]]:
        out.append(f"track {leg['track']!r} is not one of {', '.join(sr.TRACKS[leg['store']])}")
    if leg["state"] not in sr.STATES:
        out.append(f"state {leg['state']!r} is not one of {', '.join(sr.STATES)}")
    if not isinstance(leg["version"], str) or not _VERSION.match(leg["version"]):
        out.append(f"version {leg['version']!r} is not MAJOR.MINOR.PATCH")
    if not isinstance(leg["build"], str) or not leg["build"].strip():
        out.append("build is not a non-empty string")
    if not isinstance(leg["read"], str) or not _MINUTE.match(leg["read"]):
        out.append(f"read {leg['read']!r} is not a UTC minute YYYY-MM-DDTHH:MMZ")
    ids = leg["ids"]
    if not isinstance(ids, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in ids.items()):
        out.append("ids is not a flat string-to-string object")
    note = leg["note"]
    if not isinstance(note, str):
        out.append("note is not a string")
    else:
        if len(note) > NOTE_MAX:
            out.append(f"note is longer than {NOTE_MAX} characters")
        if _ADDRESS.search(note) or (isinstance(ids, dict) and any(_ADDRESS.search(str(v)) for v in ids.values())):
            out.append("names an address")
    return out


def try_problems(entry: object) -> list[str]:
    """Every way one reported try breaks the grammar (empty when it is sound)."""
    if not isinstance(entry, dict):
        return ["a try is not a JSON object"]
    out: list[str] = []
    missing = [f for f in TRY_FIELDS if f not in entry]
    extra = sorted(set(entry) - set(TRY_FIELDS))
    if missing:
        out.append(f"missing {', '.join(missing)}")
    if extra:
        out.append(f"unknown field(s) {', '.join(extra)}")
    if missing:
        return out
    if entry["store"] not in sr.STORES:
        out.append(f"store {entry['store']!r} is not one of {', '.join(sr.STORES)}")
    if not isinstance(entry["at"], str) or not _MINUTE.match(entry["at"]):
        out.append(f"at {entry['at']!r} is not a UTC minute YYYY-MM-DDTHH:MMZ")
    if entry["outcome"] not in TRY_OUTCOMES:
        out.append(f"outcome {entry['outcome']!r} is not one of {', '.join(TRY_OUTCOMES)}")
    note = entry["note"]
    if not isinstance(note, str):
        out.append("note is not a string")
    else:
        if len(note) > NOTE_MAX:
            out.append(f"note is longer than {NOTE_MAX} characters")
        if _ADDRESS.search(note):
            out.append("names an address")
    return out


def load_artifact(path: Path) -> dict:
    if not path.exists():
        return {"schema": SCHEMA, "legs": []}
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or data.get("schema") != SCHEMA or not isinstance(data.get("legs"), list):
        raise LegError(f"{path}: not a schema-{SCHEMA} store-legs artifact")
    for i, leg in enumerate(data["legs"]):
        bad = leg_problems(leg)
        if bad:
            raise LegError(f"{path}: leg {i}: " + "; ".join(bad))
    extra = sorted(set(data) - {"schema", "legs", "tries"})
    if extra:
        raise LegError(f"{path}: unknown field(s) {', '.join(extra)}")
    if not isinstance(data.get("tries", []), list):
        raise LegError(f"{path}: tries is not a list")
    for i, entry in enumerate(data.get("tries", [])):
        bad = try_problems(entry)
        if bad:
            raise LegError(f"{path}: try {i}: " + "; ".join(bad))
    return data


def add_leg(path: Path, leg: object) -> dict:
    """Validate `leg` and append it to the artifact at `path` (created when absent)."""
    bad = leg_problems(leg)
    if bad:
        raise LegError("the leg is refused: " + "; ".join(bad))
    data = load_artifact(path)
    data["legs"].append({f: leg[f] for f in LEG_FIELDS})  # type: ignore[index]
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return data


def add_tries(path: Path, entries: list) -> dict:
    """Validate each try and append them to the artifact at `path` (created when absent)."""
    for entry in entries:
        bad = try_problems(entry)
        if bad:
            raise LegError("the try is refused: " + "; ".join(bad))
    data = load_artifact(path)
    if entries:
        data.setdefault("tries", []).extend({f: e[f] for f in TRY_FIELDS} for e in entries)
        path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return data
