"""tier_1: every live module resolves its admin seed per box, and the seed is
never the opt-in of a destructive one.

Live mode resolves the admin seed per box (`testing.md` § Default app and nest
mode, *Live mode*; `helpers/multiseat_config.resolve_secret`:
``FAUNA_LIVE_SECRET_HEX`` > ``~/.config/fauna/staging-box/<host>.json`` >
``~/.fauna-id``). Until 2026-10-05 eight `live_box` modules read the env var
alone and skipped without it, so against a staging box this fleet provisioned
they needed the seed exported by hand while every other live test found it.

Resolving it for them is not a mechanical swap, because a resolved seed is
AMBIENT: for the modules that factory-reset the box (or rewrite its global
admin state with no restore), the exported seed was doubling as the "yes, run
this" switch, and resolving it silently would make them runnable where they
were not. So the destructive half gates on the suite's existing destructive-live
mechanism instead — the run's disposable-box declaration (`testing.md` § The
shared-box rule → *The disposable-box declaration*, `--live-box disposable`),
via `helpers.live_box_door.destructive_live_box` — which also makes the box a
destructive test resets the declared one, never an exported URL beside it.

Pinned here, headlessly (no box, no driver):

  1. Every `live_box` module that names the seed variable is classified,
     non-destructive or destructive — a new one cannot slip in unlisted.
  2. None reads ``FAUNA_LIVE_SECRET_HEX`` from the environment or requires it in
     a `_REQUIRED*` tuple; each resolves through `live_box_door.admin_seed`.
  3. Each destructive module gates on `destructive_live_box`.
  4. Imported with only ``FAUNA_LIVE_NEST_URL`` and its other non-seed inputs
     set, under a temp HOME holding a staging-box file: the non-destructive
     modules' gates pass on the file's seed; the destructive ones still skip
     on a shared run, and pass only on a declared-disposable one.
  5. The destructive modules' MAILBOX (``FAUNA_LIVE_MAIL_ADDRESS`` /
     ``FAUNA_LIVE_MAIL_PASSWORD``) is resolved the same way
     (`multiseat_config.resolve_mailbox`: env > the box file's ``handle`` /
     ``mail_password`` > a password derived from the box's seed, for a file
     whose password the provisioning test nulled), so a declared-disposable
     run of the CD gate needs nothing exported; the env vars stay per-field
     overrides. And a non-destructive module's box is the `--nest live` run's
     own (`live_box_door.live_box_url`), so ``FAUNA_LIVE_NEST_URL`` is an
     override there too.
"""

from __future__ import annotations

import ast
import importlib.util
import json
import sys
from pathlib import Path

import pytest

from helpers import nest_mode
from helpers.live_box_door import marks_live_box

pytestmark = pytest.mark.tier_1

_ROOT = Path(__file__).resolve().parent.parent
_SEED_VAR = "FAUNA_LIVE_SECRET_HEX"
_BOX = "dev.example.com"
_BOX_URL = f"https://{_BOX}"
_BOX_SEED = "44" * 32
_BOX_HANDLE = f"admin@{_BOX}"
_BOX_MAIL_PASSWORD = "box-file-mail-password"
_MAIL_VARS = ("FAUNA_LIVE_MAIL_ADDRESS", "FAUNA_LIVE_MAIL_PASSWORD")

#: Every `live_box` module that names the seed variable, and whether it is
#: destructive. The value is the reason — the classification is what a reader
#: checks, so it says why, not just which.
_SEED_MODULES = {
    # Non-destructive (the shared-box rule's carve-out, or nothing the human
    # would notice): the seed is resolved per box and is safe as an ambient
    # default, because the `live_box` opt-in, not the seed, selects the test.
    "tests/test_activitypub_live.py": None,
    "tests/test_live_box_bootstrap.py": None,
    "tests/test_filesync_multiseat_live.py": None,
    # Sender leg adds one credential and revokes it; opt-in is the
    # live-provision gate (HETZNER_API_TOKEN + FAUNA_E2E_LIVE=1).
    "tests/live/test_private_relay_hetzner.py": None,
    # Re-issues a certificate and restores the DNS credential + managed mode it
    # set; opt-in is FAUNA_E2E_LIVE=1 + the token (Let's Encrypt budget).
    "tests/live/test_dns01_cert_renewal_hetzner.py": None,
    # Destructive: the run's disposable-box declaration is the opt-in.
    "tests/test_mail_enable_live_nest.py": "factory-resets the box over WS-RPC",
    "tests/test_mail_zero_cheat_live.py": "factory-resets the box through the UI",
    "tests/test_caldav_live_nest.py": "factory-resets the box through the UI",
    "tests/test_caldav_autoschedule_live_nest.py": "factory-resets the box through the UI",
    "tests/test_mail_port25_inbound_live.py": (
        "overwrites the box's global spam policy and never restores it"
    ),
}
_DESTRUCTIVE = sorted(m for m, why in _SEED_MODULES.items() if why)
_NON_DESTRUCTIVE = sorted(m for m, why in _SEED_MODULES.items() if not why)

#: The non-seed inputs each module still reads from the env, set in the import
#: test so that only the seed decides its gate.
_OTHER_INPUTS = {
    "FAUNA_LIVE_NEST_URL": _BOX_URL,
    "FAUNA_LIVE_MAIL_ADDRESS": f"admin@{_BOX}",
    "FAUNA_LIVE_MAIL_PASSWORD": "not-a-real-password",
    "HETZNER_API_TOKEN": "not-a-real-token",
    "FAUNA_E2E_LIVE": "1",
}


def _tree(rel: str) -> ast.Module:
    return ast.parse((_ROOT / rel).read_text())


# ── 1. classified ──────────────────────────────────────────────────────────


def test_every_live_module_naming_the_seed_is_classified():
    found = set()
    for path in sorted((_ROOT / "tests").rglob("test_*.py")):
        text = path.read_text()
        if _SEED_VAR in text and marks_live_box(ast.parse(text)):
            found.add(path.relative_to(_ROOT).as_posix())
    undeclared = found - set(_SEED_MODULES)
    assert not undeclared, (
        f"these live_box modules name {_SEED_VAR} but are not classified: "
        f"{sorted(undeclared)}. Declare each in `_SEED_MODULES` — None when it "
        "is non-destructive (resolve the seed with `live_box_door.admin_seed`), "
        "else the reason it is destructive (then gate on "
        "`live_box_door.destructive_live_box` too)."
    )
    stale = set(_SEED_MODULES) - found
    assert not stale, f"classified modules no longer naming the seed: {sorted(stale)}"


# ── 2. resolved, never read raw ────────────────────────────────────────────


def _raw_env_reads(tree: ast.Module, var: str = _SEED_VAR) -> list[int]:
    """Lines reading ``var`` from `os.environ` (`.get(VAR…)`, `[VAR]`) or
    `os.getenv(VAR…)`."""
    lines = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Call) and node.args:
            arg = node.args[0]
            func = node.func
            name = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", "")
            if name in {"get", "getenv"} and isinstance(arg, ast.Constant) and arg.value == var:
                lines.append(node.lineno)
        if isinstance(node, ast.Subscript):
            key = node.slice
            if isinstance(key, ast.Constant) and key.value == var:
                lines.append(node.lineno)
    return lines


@pytest.mark.parametrize("rel", sorted(_SEED_MODULES))
def test_no_module_reads_the_seed_from_the_env(rel):
    tree = _tree(rel)
    raw = _raw_env_reads(tree)
    assert not raw, (
        f"{rel} reads {_SEED_VAR} straight from the environment at line(s) {raw}; "
        "resolve it per box with `live_box_door.admin_seed(url)` instead"
    )
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            getattr(t, "id", "").startswith("_REQUIRED") for t in node.targets
        ):
            names = {e.value for e in ast.walk(node.value) if isinstance(e, ast.Constant)}
            assert _SEED_VAR not in names, (
                f"{rel} still REQUIRES {_SEED_VAR} in a _REQUIRED tuple; the seed "
                "is resolved per box, so it is never a precondition"
            )


def _calls(tree: ast.Module) -> set[str]:
    return {
        node.func.attr if isinstance(node.func, ast.Attribute) else getattr(node.func, "id", "")
        for node in ast.walk(tree)
        if isinstance(node, ast.Call)
    }


@pytest.mark.parametrize("rel", [m for m in _NON_DESTRUCTIVE if "filesync" not in m])
def test_a_non_destructive_module_resolves_through_admin_seed(rel):
    # (The multiseat filesync module predates the helper and resolves through
    # `multiseat_config.load_secret`, the same precedence.)
    assert "admin_seed" in _calls(_tree(rel)), f"{rel} does not call live_box_door.admin_seed"


# ── 3. destructive ⇒ the declaration is the opt-in ─────────────────────────


@pytest.mark.parametrize("rel", _DESTRUCTIVE)
def test_a_destructive_module_gates_on_the_disposable_declaration(rel):
    calls = _calls(_tree(rel))
    assert "destructive_live_box" in calls, (
        f"{rel} is classified destructive ({_SEED_MODULES[rel]}) but does not gate "
        "on `live_box_door.destructive_live_box` — with the seed resolved per box, "
        "nothing else stops it running against a box nobody declared disposable"
    )
    assert "admin_seed" in calls


@pytest.mark.parametrize("rel", _DESTRUCTIVE)
def test_a_destructive_module_resolves_its_mailbox_per_box(rel):
    """The mailbox, like the seed, is something the box's staging-box file
    already knows — so it is resolved (`live_box_door.mailbox`), never read
    raw or demanded in a `_REQUIRED` tuple."""
    tree = _tree(rel)
    for var in _MAIL_VARS:
        raw = _raw_env_reads(tree, var)
        assert not raw, (
            f"{rel} reads {var} straight from the environment at line(s) {raw}; "
            "resolve the mailbox per box with `live_box_door.mailbox(url)`"
        )
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            getattr(t, "id", "").startswith("_REQUIRED") for t in node.targets
        ):
            names = {e.value for e in ast.walk(node.value) if isinstance(e, ast.Constant)}
            assert not names & set(_MAIL_VARS), (
                f"{rel} still REQUIRES {sorted(names & set(_MAIL_VARS))}; the "
                "mailbox is resolved per box, so the vars are overrides only"
            )
    assert "mailbox" in _calls(tree), f"{rel} does not call live_box_door.mailbox"


# ── 4. the gates themselves, evaluated ─────────────────────────────────────


@pytest.fixture
def _staging_home(tmp_path, monkeypatch):
    """A machine holding only a staging-box file for the box, and every
    non-seed input set — so the seed is the one thing a gate could miss."""
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv(_SEED_VAR, raising=False)
    for key, value in _OTHER_INPUTS.items():
        monkeypatch.setenv(key, value)
    d = tmp_path / ".config" / "fauna" / "staging-box"
    d.mkdir(parents=True)
    (d / f"{_BOX}.json").write_text(
        json.dumps(
            {
                "domain": _BOX,
                "secret_hex": _BOX_SEED,
                "handle": _BOX_HANDLE,
                "mail_password": _BOX_MAIL_PASSWORD,
            }
        )
    )
    yield tmp_path


@pytest.fixture
def _run_mode():
    """Set the run's nest mode for the import, restoring it after."""
    saved = nest_mode.run_mode()
    yield nest_mode.set_run_mode
    nest_mode.set_run_mode(saved)


def _fresh_import(rel: str):
    """Import the module under a private name, so its import-time gate is
    evaluated against this test's environment rather than a cached copy."""
    path = _ROOT / rel
    package = ".".join(Path(rel).with_suffix("").parts[:-1])
    name = f"{package}._seed_gate_probe_{path.stem}"
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    module.__package__ = package
    sys.modules[name] = module
    try:
        spec.loader.exec_module(module)
    finally:
        sys.modules.pop(name, None)
    return module


def _firing_skips(module) -> list[str]:
    """Reasons of the module's `skipif` marks whose condition holds, ignoring
    the one input this machine decides (Docker)."""
    marks = module.pytestmark if isinstance(module.pytestmark, list) else [module.pytestmark]
    return [
        m.kwargs.get("reason", "")
        for m in marks
        if m.name == "skipif" and m.args and m.args[0] and "Docker" not in m.kwargs.get("reason", "")
    ]


@pytest.mark.parametrize("rel", _NON_DESTRUCTIVE)
def test_a_non_destructive_module_runs_on_the_staging_box_file_alone(rel, _staging_home, _run_mode):
    _run_mode(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"))
    module = _fresh_import(rel)
    assert _firing_skips(module) == [], (
        f"{rel} still skips with no {_SEED_VAR} exported although the box's "
        "staging-box file holds its seed"
    )


@pytest.mark.parametrize("rel", _DESTRUCTIVE)
def test_a_destructive_module_still_skips_on_a_shared_run(rel, _staging_home, _run_mode):
    """The seed now resolves on its own; a shared run must still not select a
    destructive module — standalone (`FAUNA_E2E_LIVE=1` alone) or `--nest live`
    without the declaration."""
    for mode in (nest_mode.NestMode(nest_mode.STANDALONE), nest_mode.parse_nest_mode(f"live:{_BOX_URL}")):
        _run_mode(mode)
        module = _fresh_import(rel)
        reasons = _firing_skips(module)
        assert any("disposable" in r for r in reasons), (
            f"{rel} ({_SEED_MODULES[rel]}) would run on a {mode.name} run that did "
            f"not declare its box disposable; firing skips: {reasons}"
        )


@pytest.mark.parametrize("rel", _DESTRUCTIVE)
def test_a_destructive_module_runs_on_a_declared_disposable_box(rel, _staging_home, _run_mode, monkeypatch):
    """Under the declaration it runs on the staging-box file's seed AND mailbox,
    with no FAUNA_LIVE_MAIL_* exported — and drives the declared box even when
    the env names another one."""
    monkeypatch.setenv("FAUNA_LIVE_NEST_URL", "https://example.com")
    for var in _MAIL_VARS:
        monkeypatch.delenv(var, raising=False)
    declared = nest_mode.declare_box(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"), nest_mode.BOX_DISPOSABLE)
    _run_mode(declared)
    module = _fresh_import(rel)
    assert _firing_skips(module) == [], f"{rel} skips on a declared-disposable run"
    assert module.URL == _BOX_URL, f"{rel} would reset {module.URL}, not the declared {_BOX_URL}"
    assert module.SECRET == _BOX_SEED
    assert module.ADDRESS == _BOX_HANDLE
    assert module.PASSWORD == _BOX_MAIL_PASSWORD


@pytest.mark.parametrize("rel", _DESTRUCTIVE)
def test_the_mailbox_env_vars_stay_overrides(rel, _staging_home, _run_mode, monkeypatch):
    monkeypatch.setenv("FAUNA_LIVE_MAIL_ADDRESS", f"other@{_BOX}")
    monkeypatch.setenv("FAUNA_LIVE_MAIL_PASSWORD", "exported-password")
    _run_mode(nest_mode.declare_box(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"), nest_mode.BOX_DISPOSABLE))
    module = _fresh_import(rel)
    assert _firing_skips(module) == []
    assert (module.ADDRESS, module.PASSWORD) == (f"other@{_BOX}", "exported-password")


@pytest.mark.parametrize("rel", _DESTRUCTIVE)
def test_a_nulled_box_file_password_falls_back_to_the_seed_derived_one(
    rel, _staging_home, _run_mode, monkeypatch
):
    """The provisioning test nulls ``mail_password`` when mail was already on
    under a credential it never chose — the state of the fleet's own staging
    box. Every module of the run then agrees on one seed-derived password."""
    from helpers.multiseat_config import derived_mail_password

    for var in _MAIL_VARS:
        monkeypatch.delenv(var, raising=False)
    box_file = _staging_home / ".config" / "fauna" / "staging-box" / f"{_BOX}.json"
    state = json.loads(box_file.read_text())
    state["mail_password"] = None
    box_file.write_text(json.dumps(state))
    _run_mode(nest_mode.declare_box(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"), nest_mode.BOX_DISPOSABLE))
    module = _fresh_import(rel)
    assert _firing_skips(module) == []
    assert module.ADDRESS == _BOX_HANDLE
    assert module.PASSWORD == derived_mail_password(_BOX_SEED)


_URL_MODULES = ("tests/test_activitypub_live.py", "tests/test_live_box_bootstrap.py")


@pytest.mark.parametrize("rel", _URL_MODULES)
def test_a_non_destructive_module_drives_the_live_runs_box_with_no_url_exported(
    rel, _staging_home, _run_mode, monkeypatch
):
    monkeypatch.delenv("FAUNA_LIVE_NEST_URL", raising=False)
    _run_mode(nest_mode.parse_nest_mode(f"live:{_BOX_URL}"))
    module = _fresh_import(rel)
    assert module.URL == _BOX_URL
    assert _firing_skips(module) == [], f"{rel} skips on a `--nest live:{_BOX_URL}` run"


@pytest.mark.parametrize("rel", _URL_MODULES)
def test_outside_a_live_run_the_url_export_still_selects_the_box(rel, _staging_home, _run_mode, monkeypatch):
    monkeypatch.delenv("FAUNA_LIVE_NEST_URL", raising=False)
    _run_mode(nest_mode.NestMode(nest_mode.STANDALONE))
    assert _fresh_import(rel).URL == ""


# ── 5. the mailbox resolver itself ─────────────────────────────────────────


def test_resolve_mailbox_reads_the_box_file_per_field_under_env(_staging_home, monkeypatch):
    from helpers.multiseat_config import resolve_mailbox

    for var in _MAIL_VARS:
        monkeypatch.delenv(var, raising=False)
    assert resolve_mailbox(_BOX_URL) == (_BOX_HANDLE, _BOX_MAIL_PASSWORD)
    # Another box's file is never consulted: this machine holds none for it.
    assert resolve_mailbox("https://test.example.com") == (None, None)
    # Each env var overrides its own field only.
    monkeypatch.setenv("FAUNA_LIVE_MAIL_PASSWORD", "exported")
    assert resolve_mailbox(_BOX_URL) == (_BOX_HANDLE, "exported")


def test_the_derived_mail_password_is_per_seed_and_does_not_echo_it():
    from helpers.multiseat_config import derived_mail_password

    a, b = derived_mail_password("44" * 32), derived_mail_password("55" * 32)
    assert a == derived_mail_password("44" * 32)
    assert a != b
    assert len(a) == 32 and "44" * 4 not in a
