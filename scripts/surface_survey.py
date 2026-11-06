#!/usr/bin/env python3
"""surface_survey.py — the `ast` survey of convention 7's unbuilt-surface gates.

`docs/goal/architecture/e2e-conventions.md` convention 7, the paragraph *The
surface registry*: the registry (`tests/e2e-unified/surfaces.toml`) was SEEDED
from the gate sites whose guard is a literal app set or a module constant, and
this script is that seed's reader. It finds every `skip_unbuilt(` call under
`tests/e2e-unified` and, for each, decides from the parse alone on which of the
seven apps the call fires — by evaluating the guards that reach it (the `if`s
around it and the early exits before it in each enclosing block) once per app.

A guard the parse can evaluate is spelled in one of the two static shapes the
measurement counted (`feature-catalog.md` § Implementation status today): a
literal app set — `app_name(d) not in ("tui", "web")`, an `is_<app>()`
disjunction, `== "android"` — or a module constant (`_BUILT_APPS`, also one
imported from a `helpers.*` module, resolved by `features_scan`'s own reader).
Everything else — a driver capability predicate, a runtime probe, a caller's
decision — is *dynamic*: the parse names no app set, and the site is listed as
such, never guessed.

It is the input of two things. `--check` compares each static site's app set
with the registry entry its surface text maps to (`SEED_KEYS` below — the
hand-named keys the seed collapsed the free-text strings into), so a site and
the registry cannot silently disagree while the migration runs; and
`scripts/check_app_gate_ratchet.py` counts the free-text calls with
`unbuilt_calls()`, so the ratchet and the survey cannot disagree about what a
site is.

Parse-only, stdlib — never an import of the test tree.

Usage:
  surface_survey.py            # every site, grouped by surface text
  surface_survey.py --static   # only the statically-readable sites
  surface_survey.py --json     # the same as data
"""

from __future__ import annotations

import argparse
import ast
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import features_scan  # noqa: E402

REPO = Path(__file__).resolve().parent.parent
E2E = REPO / "tests" / "e2e-unified"
#: Where the gates live: the three call-site trees and the root conftest. The
#: drivers are in the walk for completeness (they hold none today).
SCAN_DIRS = ("tests", "actions", "helpers", "drivers")
ROOT_FILES = ("conftest.py",)
#: The gate's own proof files — fake drivers and app ids exercising the gate
#: itself, not surfaces of any app (the measurement excluded them too). Named by
#: file, so a real gate written in one of them is still a review question, and
#: the free-text ratchet skips exactly these.
PROOF_FILES = frozenset({
    "tests/test_app_surface_declarations.py",
    "tests/test_feature_ledger.py",
})
APPS = ("android", "ios", "linux", "macos", "tui", "web", "windows")
#: The free-text gate's name. `require_surface` is the routed form and is never
#: counted here.
FREE_TEXT = "skip_unbuilt"
#: Names whose call yields the app under test as a string.
_APP_NAME_FUNCS = frozenset({"app_name"})
#: A guard that mentions one of these is about the BOX — which apps this machine
#: can launch — not about the app under test: it is assumed to hold (the seed
#: reads what the app has built, on a box that can run it).
_ENVIRONMENT_NAMES = frozenset({"_avail_clients", "get_available_apps", "sys"})

#: The seed's hand-named keys: each free-text `surface=` string a static site
#: spells, mapped to its registry key and kind (convention 7: the key is the
#: ui.yaml id where one exists, else a named capability in the same style).
#: The migration (convention 7's last rule) reads it to rewrite a call site;
#: once no site spells a string, its line goes.
SEED_KEYS: dict[str, tuple[str, str]] = {
    "a failed import's record and its Dismiss": ("mail-import-failed-record", "product"),
    "a multi-megabyte compose-body fill via the automation bridge": ("compose-body-bulk-fill", "product"),
    "a native-process-relaunch Sent-copy restart proof": ("native-relaunch-sent-copy-proof", "harness"),
    "a read-only dm-compose-bar on a disconnected bridged room": ("bridged-room-read-only-compose", "product"),
    "a same-device-restart mls.db data-dir persistence proof": ("mls-db-restart-persistence-proof", "harness"),
    "a second seat of one identity in the e2e harness": ("second-seat-same-identity", "harness"),
    "a second signed-in device of the same user": ("second-device-same-user", "harness"),
    "a second, author-side GUI client with a settable pump cadence": ("author-seat-pump-cadence", "harness"),
    "admin-audit-section": ("admin-audit-section", "product"),
    "admin-dns-cert-issuance-error": ("admin-dns-cert-issuance-error", "product"),
    "admin-mail-dmarc-ruf-publish-toggle": ("admin-mail-dmarc-ruf-publish-toggle", "product"),
    "admin-mail-health-sender-dashboards-note": ("admin-mail-health-sender-dashboards-note", "product"),
    "admin-nest-report-block-button": ("admin-nest-report-block-button", "product"),
    "admin-nest-rules-section": ("admin-nest-rules-section", "product"),
    "admin-nest-update-channel-section": ("admin-nest-update-channel-section", "product"),
    "admin-settings-tier-delete-button": ("admin-settings-tier-delete-button", "product"),
    "dm-message-legal-takedown": ("dm-message-legal-takedown", "product"),
    "dm-message-warmup-deferred": ("dm-message-warmup-deferred", "product"),
    "family-policy-rules-section": ("family-policy-rules-section", "product"),
    "first-setup mail auto-mint": ("first-setup-mail-auto-mint", "product"),
    "folder-follow-button / media-folder-filter followed options": ("folder-follow-button", "product"),
    "folder-location-free-space-button": ("folder-location-free-space-button", "product"),
    "folder-location-unreadable": ("folder-location-unreadable", "product"),
    "folder-paywall-hint": ("folder-paywall-hint", "product"),
    "host-side credential fault injection": ("host-credential-fault-injection", "harness"),
    "html-mail inline-image render": ("html-mail-inline-image", "product"),
    "locked launch surface": ("locked-launch-surface", "product"),
    "mail-export drive loop": ("mail-export-drive", "harness"),
    "mail-import full-mailbox notice (Skip the rest)": ("mail-import-full-mailbox-notice", "product"),
    "mail-import-scope-import-duplicates-toggle": ("mail-import-scope-import-duplicates-toggle", "product"),
    "mail-list-members-import-result": ("mail-list-members-import-result", "product"),
    "mail-lists / mail-list-members real-device e2e": ("mail-lists", "product"),
    "mail-rotate-keys-exclude-item": ("mail-rotate-keys-exclude-item", "product"),
    "nest_retire page": ("nest-retire", "product"),
    "offline_share_hold_serves / share_serve_tally": ("share-serve-tally", "harness"),
    "onboarding-terms-notice": ("onboarding-terms-notice", "product"),
    "profile-follow-button at a remote actor's home": ("remote-profile-follow", "product"),
    "profile-handle as <handle>@<current primary>": ("profile-handle-primary-domain", "product"),
    "recovery-kit-sweep-status / recovery-kit-sweep-unvouched-status": ("recovery-kit-sweep-status", "product"),
    "report-sheet": ("federated-report-sheet", "product"),
    "sessions page": ("sessions", "product"),
    "settings-terms-link": ("settings-terms-link", "product"),
    "share-serve-status (the share plane) on the folders page": ("share-serve-status", "product"),
    "the HTML-mail markdown round-trip (render + compose toolbar)": ("html-mail-markdown-roundtrip", "product"),
    "the `selected` attribute on dm-message-timestamp": ("dm-message-timestamp-selected", "harness"),
    "the `{…}` state observable": ("connection-state-observable", "harness"),
    "the admin-mail Microsoft registration section (the walk's precondition)": ("admin-mail-microsoft-registration", "product"),
    "the believable admin-claim onboarding UI drive": ("admin-claim-onboarding-drive", "harness"),
    "the bound-location + agent-stderr upload witness choreography": ("bound-location-upload-witness", "harness"),
    "the client-UI enable-mail docker drive": ("mail-enable-ui-docker-drive", "harness"),
    "the client-UI mail reply round-trip": ("mail-reply-roundtrip", "product"),
    "the cross-device MLS-sync GUI proof": ("mls-cross-device-sync-proof", "harness"),
    "the email-attendee-entry encrypted-CalDAV path": ("email-attendee-encrypted-caldav", "product"),
    "the encrypted-CalDAV calendar-after-reclaim observation": ("calendar-after-reclaim", "product"),
    "the full client-UI mail round-trip": ("mail-full-roundtrip", "product"),
    "the general Settings sub-page": ("settings-general", "product"),
    "the live-nest 3-client CalDAV round-trip drive": ("caldav-live-nest-drive", "harness"),
    "the live-nest CalDAV autoschedule iMIP observer drive": ("caldav-autoschedule-imip-drive", "harness"),
    "the native/TOFU trust-differential drive": ("trust-differential-drive", "harness"),
    "the place-accepts hold journey": ("folder-place-accepts-hold", "product"),
    "the post-LoggedIn dial seam": ("post-logged-in-dial-seam", "harness"),
    "the real-wire cross-nest FaunaMls receive seam": ("cross-nest-mls-receive-seam", "harness"),
    "the registry E2E bridge delegation (`refuse_secret_writes_for_test`)": ("registry-refuse-secret-writes", "harness"),
    "the registry E2E bridge delegation (`set_account_reach_for_test`)": ("registry-set-account-reach", "harness"),
    "the remote author's byline on a delivered post": ("remote-author-byline", "product"),
    "the repost card's target badge (`reposted_gated_*`)": ("repost-target-badge", "product"),
    "the self-signed-tolerant onboarding handle-check e2e drive": ("self-signed-handle-check-drive", "harness"),
    "the shared caltime::pan visible-range policy": ("caltime-pan-visible-range", "product"),
    "the vps-location-picker": ("vps-location-picker", "product"),
    "§5 claim Void verb (subscription-claim-void-button)": ("subscription-claim-void-button", "product"),
}

#: Entries no static site implied at the seed, written by hand. A harness
#: predicate that is real code keeps its code and gets a registry entry whose
#: `built` list the proof half (`test_app_surface_declarations.py`) pins against
#: the drivers' own answers (convention 7's (surface, app) rule); since the seed,
#: such entries are written in `surfaces.toml` itself — this map only feeds
#: `--emit`, which never overwrites the hand-kept registry.
HAND_ENTRIES: dict[str, dict] = {
    "unclean-kill": {
        "kind": "harness",
        "built": {"ios", "linux", "macos", "tui", "web", "windows"},
        "detail": "the driver's unclean-kill primitive (`PlatformDriver.kill_uncleanly`, "
                  "answered by `supports_unclean_kill`): the driver must own the app "
                  "child it kills — a Popen, a pty session leader, a simulator pid, a "
                  "bridge session — and the android bridge driver owns none",
        "tracked": "docs/goal/architecture/e2e-conventions.md convention 7, mode 5",
    },
}


def scan_files(root: Path = E2E) -> list[Path]:
    files: list[Path] = []
    for sub in SCAN_DIRS:
        base = root / sub
        if base.is_dir():
            files.extend(sorted(base.rglob("*.py")))
    files.extend(root / f for f in ROOT_FILES if (root / f).is_file())
    return [f for f in files if f.relative_to(root).as_posix() not in PROOF_FILES]


def _call_name(node: ast.AST) -> str | None:
    if not isinstance(node, ast.Call):
        return None
    if isinstance(node.func, ast.Name):
        return node.func.id
    if isinstance(node.func, ast.Attribute):
        return node.func.attr
    return None


def unbuilt_calls(tree: ast.AST) -> list[ast.Call]:
    """Every free-text `skip_unbuilt(...)` call in `tree` (bare or attribute)."""
    return [n for n in ast.walk(tree) if _call_name(n) == FREE_TEXT]


# ── evaluating a guard once per app ─────────────────────────────────────────
class _Unknown(Exception):
    """The parse cannot evaluate this expression for a given app."""


class _Evaluator:
    """Evaluate a boolean guard for one app. `app_vars` are the names bound to the
    app under test's name in the enclosing function (`name = app_name(d)`)."""

    def __init__(self, scope: tuple, app_vars: frozenset):
        self.scope = scope
        self.app_vars = app_vars

    def _is_app_expr(self, node: ast.AST, right: ast.AST) -> bool:
        if isinstance(node, ast.Name) and node.id in self.app_vars:
            return True
        if _call_name(node) in _APP_NAME_FUNCS:
            return True
        # A plain name compared with an app set or an app-name literal is the app
        # id itself — a parametrized `custody_app`, a helper's `client` argument.
        if isinstance(node, ast.Name):
            if isinstance(right, ast.Constant):
                return right.value in APPS
            return features_scan._app_set(right, self.scope) is not None
        return False

    def _collection(self, node: ast.AST) -> frozenset:
        found = features_scan._app_set(node, self.scope)
        if found is None:
            raise _Unknown
        return found

    def eval(self, node: ast.AST, app: str) -> bool:
        if isinstance(node, ast.BoolOp):
            vals = [self.eval(v, app) for v in node.values]
            return all(vals) if isinstance(node.op, ast.And) else any(vals)
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
            return not self.eval(node.operand, app)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) \
                and node.func.attr.startswith("is_") and not node.args:
            which = node.func.attr[3:]
            if which in APPS:
                return app == which
            raise _Unknown
        if isinstance(node, ast.Compare) and len(node.ops) == 1:
            left, op, right = node.left, node.ops[0], node.comparators[0]
            if self._is_app_expr(left, right):
                if isinstance(op, (ast.In, ast.NotIn)):
                    hit = app in self._collection(right)
                    return hit if isinstance(op, ast.In) else not hit
                if isinstance(op, (ast.Eq, ast.NotEq)) and isinstance(right, ast.Constant) \
                        and isinstance(right.value, str):
                    hit = app == right.value
                    return hit if isinstance(op, ast.Eq) else not hit
        raise _Unknown


def _app_vars(func: ast.AST | None) -> frozenset:
    """Names the enclosing function binds to `app_name(...)`."""
    if func is None:
        return frozenset()
    names = set()
    for n in ast.walk(func):
        if isinstance(n, ast.Assign) and _call_name(n.value) in _APP_NAME_FUNCS:
            names.update(t.id for t in n.targets if isinstance(t, ast.Name))
    return frozenset(names)


def _exits(body: list) -> bool:
    """The block always leaves its enclosing block (return / raise / continue / a skip)."""
    if not body:
        return False
    last = body[-1]
    if isinstance(last, (ast.Return, ast.Raise, ast.Continue, ast.Break)):
        return True
    if isinstance(last, ast.Expr) and _call_name(last.value) in (
            "skip", "fail", "exit", FREE_TEXT, "declared_absence", "skip_environment"):
        return True
    return False


@dataclass
class Site:
    rel: str                 # tests/e2e-unified-relative path
    line: int
    surface: str             # the literal `surface=` text, or `<dynamic: …>`
    detail: str = ""
    tracked: str = ""
    #: The apps the call fires on, or None when a guard is dynamic.
    fires: frozenset | None = None
    why_dynamic: str = ""
    guards: list = field(default_factory=list)

    @property
    def built(self) -> frozenset | None:
        return None if self.fires is None else frozenset(APPS) - self.fires

    @property
    def static(self) -> bool:
        return self.fires is not None


def _text(node: ast.AST | None) -> str:
    if node is None:
        return ""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.JoinedStr):
        return "".join(v.value if isinstance(v, ast.Constant) else "{…}" for v in node.values)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        return _text(node.left) + _text(node.right)
    return f"<dynamic: {ast.unparse(node)}>"


def _kw(call: ast.Call, name: str, pos: int | None = None) -> ast.AST | None:
    for kw in call.keywords:
        if kw.arg == name:
            return kw.value
    if pos is not None and len(call.args) > pos:
        return call.args[pos]
    return None


def _conditions(path_nodes: list) -> list:
    """`(expr, polarity, early_exit)` triples that must all hold for the call at
    the end of `path_nodes` (root → … → the call's statement) to run —
    `early_exit` marks a test taken from an `if …: return` before the call rather
    than an `if` around it."""
    conds = []
    for parent, child in zip(path_nodes, path_nodes[1:]):
        if isinstance(parent, ast.If):
            if child is parent.test:
                continue
            conds.append((parent.test, child in parent.body, False))
        if isinstance(parent, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Module)) \
                or isinstance(parent, (ast.If, ast.For, ast.While, ast.With, ast.Try)):
            for attr in ("body", "orelse", "finalbody"):
                block = getattr(parent, attr, None)
                if not isinstance(block, list) or child not in block:
                    continue
                for stmt in block[: block.index(child)]:
                    if isinstance(stmt, ast.If) and _exits(stmt.body) and not stmt.orelse:
                        conds.append((stmt.test, False, True))
    return conds


def _paths(tree: ast.AST):
    """Yield (ancestor path, call) for every free-text call, root first."""
    stack = [(tree, [tree])]
    while stack:
        node, path = stack.pop()
        for child in ast.iter_child_nodes(node):
            cpath = path + [child]
            if _call_name(child) == FREE_TEXT:
                yield cpath, child
            stack.append((child, cpath))


def survey_file(path: Path, root: Path = E2E) -> list[Site]:
    try:
        tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
    except SyntaxError:
        return []
    scope = features_scan._module_scope(tree, path)
    rel = path.relative_to(root).as_posix()
    sites = []
    for nodes, call in _paths(tree):
        func = next((n for n in reversed(nodes)
                     if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))), None)
        # The statement chain only: expression nodes between the statement and
        # the call (an `Expr`, a `Call`'s args) carry no guard.
        stmts = [n for n in nodes if isinstance(n, (ast.stmt, ast.Module))]
        conds, box_guard = [], False
        for e, pol, early in _conditions(stmts):
            if any(isinstance(n, ast.Name) and n.id in _ENVIRONMENT_NAMES for n in ast.walk(e)):
                # An early exit on the box is assumed not taken; an `if` on the
                # box AROUND the call makes the call's apps the box's question.
                box_guard = box_guard or not early
                continue
            conds.append((e, pol))
        site = Site(rel=rel, line=call.lineno,
                    surface=_text(_kw(call, "surface", 1)),
                    detail=_text(_kw(call, "detail")),
                    tracked=_text(_kw(call, "tracked")),
                    guards=[(ast.unparse(e), pol) for e, pol in conds])
        if box_guard:
            site.why_dynamic = "guarded by a property of the box (which apps or OS it runs)"
        elif any(isinstance(n, ast.ExceptHandler) for n in nodes):
            site.why_dynamic = "a runtime probe (reached from an `except`)"
        elif not conds:
            site.why_dynamic = "no guard of its own (the caller decides)"
        else:
            ev = _Evaluator(scope, _app_vars(func))
            try:
                site.fires = frozenset(
                    a for a in APPS if all(ev.eval(e, a) == pol for e, pol in conds))
            except _Unknown:
                site.why_dynamic = "a guard the parse cannot evaluate per app"
        sites.append(site)
    return sorted(sites, key=lambda s: s.line)


def survey(root: Path = E2E) -> list[Site]:
    return [s for f in scan_files(root) for s in survey_file(f, root)]


def seed_entries(sites: list[Site]) -> tuple[dict, list]:
    """`({key: entry}, unmapped)` — the registry the static sites imply.

    An entry's `built` is the INTERSECTION of its sites' built sets: a surface is
    unbuilt on an app as soon as any site gating it fires there (two sites of
    one test may each name a different app, as `test_p2p_participation.py`'s
    android and iOS legs do), so the seed never declares built what a site still
    gates. Its `detail` is the longest site detail, its `tracked` every distinct
    site citation."""
    entries: dict = {k: {**v, "built": set(v["built"]), "tracked": [v["tracked"]],
                         "sites": []} for k, v in HAND_ENTRIES.items()}
    unmapped = []
    for s in sites:
        if not s.static or s.surface.startswith("<dynamic"):
            continue
        if s.surface not in SEED_KEYS:
            unmapped.append(s)
            continue
        key, kind = SEED_KEYS[s.surface]
        e = entries.setdefault(key, {"kind": kind, "built": set(APPS), "detail": "",
                                     "tracked": [], "sites": []})
        e["built"] &= s.built
        if len(s.detail) > len(e["detail"]):
            e["detail"] = s.detail
        if s.tracked and s.tracked not in e["tracked"]:
            e["tracked"].append(s.tracked)
        e["sites"].append(s)
    return entries, unmapped


def _toml_str(value: str) -> str:
    # A JSON string is a valid TOML basic string (the same escapes).
    return json.dumps(value, ensure_ascii=False)


SEED_HEADER = """\
# The surface registry — ONE description of which apps have built a surface.
#
# docs/goal/architecture/e2e-conventions.md convention 7, the paragraph *The
# surface registry*, owns this file: one table per surface key (kebab-case; the
# ui.yaml id where the surface is one), with
#   kind    = "product" (the APP lacks it) | "harness" (the e2e driver lacks it
#             for that app; the matrix reads it as unrun, never as unbuilt)
#   built   = the apps that HAVE it — a positive list
#   detail  = what is missing and what closes it
#   tracked = where the work is captured
# `helpers/app_surface.py::require_surface(driver, key)` and the
# `@pytest.mark.requires_surface("<key>")` marker read it at run time;
# `scripts/features_catalog.py::load_surface_registry` reads it without a run.
# A gate's answer is a function of (surface, app) alone: widening a surface is
# one edit here, and the commit that does it runs `just features-prune`.
#
# Seeded 2026-10-25 by `scripts/surface_survey.py --emit` from the gate sites
# whose guard is a literal app set or a module constant, keys hand-named
# (`SEED_KEYS`); `surface_survey.py --check` keeps those sites and this file
# from disagreeing while the call sites migrate.
"""


def emit_toml(entries: dict) -> str:
    out = [SEED_HEADER]
    for key in sorted(entries):
        e = entries[key]
        out.append(f"\n[{key}]")
        out.append(f"kind = {_toml_str(e['kind'])}")
        out.append("built = [" + ", ".join(_toml_str(a) for a in sorted(e["built"])) + "]")
        out.append(f"detail = {_toml_str(e['detail'])}")
        out.append(f"tracked = {_toml_str('; '.join(e['tracked']))}")
    return "\n".join(out) + "\n"


def drift(sites: list[Site], registry: dict) -> list[str]:
    """Static sites that disagree with the registry: a mapped surface with no
    entry, or an entry declaring built an app the site still gates there."""
    problems = []
    for s in sites:
        if not s.static or s.surface not in SEED_KEYS:
            continue
        key = SEED_KEYS[s.surface][0]
        entry = registry.get(key)
        if entry is None:
            problems.append(f"{s.rel}:{s.line}: surface {s.surface!r} maps to {key!r}, "
                            "which the registry does not declare")
            continue
        extra = set(entry.built) - s.built
        if extra:
            problems.append(f"{s.rel}:{s.line}: the registry declares {key!r} built on "
                            f"{', '.join(sorted(extra))}, where this site still gates it")
    return problems


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--static", action="store_true", help="only statically-readable sites")
    ap.add_argument("--json", action="store_true", help="emit data")
    ap.add_argument("--emit", metavar="PATH",
                    help="write the seed registry the static sites imply to PATH")
    ap.add_argument("--check", action="store_true",
                    help="fail when a static site disagrees with surfaces.toml")
    args = ap.parse_args(argv[1:])
    sites = survey()
    if args.emit:
        entries, unmapped = seed_entries(sites)
        for s in unmapped:
            print(f"unmapped: {s.rel}:{s.line}: {s.surface!r} — name it in SEED_KEYS",
                  file=sys.stderr)
        if unmapped:
            return 1
        if Path(args.emit).exists():
            # The seed is written once; from then on the registry is hand-kept
            # and `--check` holds the sites to it. Emit elsewhere to compare.
            print(f"refusing to overwrite {args.emit}: the registry is hand-kept "
                  "after its seed — emit to another path and compare", file=sys.stderr)
            return 1
        Path(args.emit).write_text(emit_toml(entries), encoding="utf-8")
        print(f"wrote {len(entries)} surface(s) to {args.emit}")
        return 0
    if args.check:
        import features_catalog
        problems = drift(sites, features_catalog.load_surface_registry())
        for line in problems:
            print(line, file=sys.stderr)
        print(f"surface-survey: {'FAIL' if problems else 'OK'} — {len(problems)} "
              "static site(s) disagree with surfaces.toml")
        return 1 if problems else 0
    shown = [s for s in sites if s.static] if args.static else sites
    if args.json:
        print(json.dumps([{
            "file": s.rel, "line": s.line, "surface": s.surface, "detail": s.detail,
            "tracked": s.tracked,
            "built": sorted(s.built) if s.static else None,
            "dynamic": s.why_dynamic or None,
        } for s in shown], indent=2))
        return 0
    n_static = sum(s.static for s in sites)
    print(f"{len(sites)} free-text skip_unbuilt( site(s); {n_static} static, "
          f"{len(sites) - n_static} dynamic\n")
    by_surface: dict = {}
    for s in shown:
        by_surface.setdefault(s.surface, []).append(s)
    for surface in sorted(by_surface):
        print(surface)
        for s in by_surface[surface]:
            what = ",".join(sorted(s.built)) if s.static else f"dynamic — {s.why_dynamic}"
            print(f"    {s.rel}:{s.line}  built=[{what}]")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
