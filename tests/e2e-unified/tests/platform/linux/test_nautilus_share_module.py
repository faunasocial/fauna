"""The GNOME Files Share module, headless (`docs/goal/architecture/apps/linux.md`
§ The file manager's Share entry (GNOME Files), witness part (c)).

`apps/fauna-linux/packaging/nautilus/fauna_share.py` is loaded against a stub
`gi` namespace — no nautilus-python, no app launch — with its agent command
pointed at a stub helper that answers the way `fauna-sync-agent
--share-target` does: two lines (the route, then the leaf's label) and exit 0
for a target, nothing and a non-zero exit without one. The module must offer
its Fauna → Share item exactly when the helper answered, and opening the leaf
must hand the printed route to GIO's default handler. Whether the leaf
*appears* in a real GNOME Files is the human's last inch.
"""

import importlib.util
import json
import os
import subprocess
import sys
import types

import pytest

pytestmark = pytest.mark.tier_1

ROUTE = "fauna://share-link?folder=42&path=photos/a%20b.jpg"
LABEL = "Share"


def _repo_root():
    here = os.path.dirname(os.path.abspath(__file__))
    return subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    ).stdout.strip()


class _MenuItem:
    def __init__(self, name, label):
        self.name, self.label = name, label
        self.submenu = None
        self.handlers = []

    def set_submenu(self, menu):
        self.submenu = menu

    def connect(self, signal, handler):
        assert signal == "activate"
        self.handlers.append(handler)

    def activate(self):
        for handler in self.handlers:
            handler(self)


class _Menu:
    def __init__(self):
        self.items = []

    def append_item(self, item):
        self.items.append(item)


class _Location:
    def __init__(self, path):
        self._path = path

    def get_path(self):
        return self._path


class _FileInfo:
    def __init__(self, path, scheme="file"):
        self._path, self._scheme = path, scheme

    def get_uri_scheme(self):
        return self._scheme

    def get_location(self):
        return _Location(self._path)


@pytest.fixture
def stub_gi(monkeypatch):
    """A `gi` whose `Nautilus 4.0` / `Gio` / `GObject` are just enough for the
    module, recording every URI the module asks GIO to open."""
    launched = []
    gi = types.ModuleType("gi")
    gi.require_version = lambda name, version: None
    repo = types.ModuleType("gi.repository")
    repo.GObject = types.SimpleNamespace(GObject=type("GObject", (), {}))
    repo.Nautilus = types.SimpleNamespace(
        MenuProvider=type("MenuProvider", (), {}), MenuItem=_MenuItem, Menu=_Menu,
    )
    repo.Gio = types.SimpleNamespace(
        AppInfo=types.SimpleNamespace(
            launch_default_for_uri=lambda uri, ctx: launched.append(uri) or True,
        ),
    )
    gi.repository = repo
    monkeypatch.setitem(sys.modules, "gi", gi)
    monkeypatch.setitem(sys.modules, "gi.repository", repo)
    return launched


def _helper(tmp_path, answers):
    """A stub `--share-target` helper: `answers` maps a path to the lines it
    prints (exit 0); any other path prints nothing and exits 1. Returns the
    command line the module's `AGENT_COMMAND` carries."""
    script = tmp_path / "helper.py"
    script.write_text(
        "import json, sys\n"
        f"answers = json.loads({json.dumps(json.dumps(answers))})\n"
        "assert sys.argv[1] == '--share-target', sys.argv\n"
        "lines = answers.get(sys.argv[2])\n"
        "if lines is None:\n"
        "    sys.exit(1)\n"
        "print('\\n'.join(lines))\n"
    )
    return f'"{sys.executable}" -I "{script}"'


def _load(stub_gi, command):
    """The module as the app writes it: its agent-command placeholder replaced
    by the channel's command, as a Python string literal."""
    source_path = os.path.join(
        _repo_root(), "apps", "fauna-linux", "packaging", "nautilus", "fauna_share.py",
    )
    with open(source_path) as f:
        source = f.read()
    placeholder = '"@AGENT_COMMAND@"'
    assert source.count(placeholder) == 1, "the module carries exactly one agent-command slot"
    module = types.ModuleType("fauna_share")
    exec(compile(source.replace(placeholder, json.dumps(command)), source_path, "exec"), module.__dict__)
    return module


def _provider(module):
    providers = [
        v for v in vars(module).values()
        if isinstance(v, type) and issubclass(v, sys.modules["gi.repository"].Nautilus.MenuProvider)
    ]
    assert len(providers) == 1, "one menu provider"
    return providers[0]()


def test_a_file_the_helper_answers_for_offers_fauna_then_share(stub_gi, tmp_path):
    module = _load(stub_gi, _helper(tmp_path, {"/home/u/Site/a b.jpg": [ROUTE, LABEL]}))
    items = _provider(module).get_file_items(None, [_FileInfo("/home/u/Site/a b.jpg")])
    assert len(items) == 1
    (top,) = items
    assert top.label == "Fauna"
    (leaf,) = top.submenu.items
    assert leaf.label == LABEL, "the leaf's label is the helper's second line"
    leaf.activate()
    assert stub_gi == [ROUTE], "activating opens the printed route through GIO"


def test_no_item_when_the_helper_refuses(stub_gi, tmp_path):
    module = _load(stub_gi, _helper(tmp_path, {}))
    assert _provider(module).get_file_items(None, [_FileInfo("/home/u/Site/sub")]) == []


def test_no_item_when_the_agent_command_is_missing(stub_gi, tmp_path):
    module = _load(stub_gi, f'"{tmp_path / "no-such-agent"}"')
    assert _provider(module).get_file_items([_FileInfo("/home/u/a.jpg")]) == []


def test_only_a_single_local_item_asks_at_all(stub_gi, tmp_path):
    module = _load(stub_gi, _helper(tmp_path, {"/a": [ROUTE, LABEL], "/b": [ROUTE, LABEL]}))
    provider = _provider(module)
    assert provider.get_file_items([_FileInfo("/a"), _FileInfo("/b")]) == []
    assert provider.get_file_items([_FileInfo("/a", scheme="sftp")]) == []


def test_a_reply_that_is_not_a_route_offers_nothing(stub_gi, tmp_path):
    module = _load(stub_gi, _helper(tmp_path, {"/a": ["https://example.com/", LABEL]}))
    assert _provider(module).get_file_items([_FileInfo("/a")]) == []
