"""tier_1: `helpers/prev_build.py`'s command line never builds by accident.

A cold pinned build is a whole workspace compile plus the platform's app toolchain —
on Windows well over an hour, and the build-slot count only knows about it when it was
started through `just e2e-prev-build`. The script used to read its flags by
membership test and fall through to `build_prev` for anything it did not recognise,
so `prev_build.py --help` (or a misspelt `--bulid`) started a real, unslotted build of
the pinned commit inside the same source tree as any build already warming it. These
pin the refusal: an unknown flag builds nothing, `--help` prints the usage, and the
flags the recipes and the upgrade smoke pass still reach the build.
"""

import subprocess
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from helpers import prev_build  # noqa: E402

pytestmark = pytest.mark.tier_1


@pytest.fixture
def no_build(monkeypatch):
    """Fail the test the moment anything would build or touch the pin's cache."""
    def _refuse(*_a, **_k):
        pytest.fail("the command line started a build")
    monkeypatch.setattr(prev_build, "build_prev", _refuse)
    monkeypatch.setattr(prev_build, "cached_binaries", _refuse)


def _main(monkeypatch, *args):
    monkeypatch.setattr(prev_build.sys, "argv", ["prev_build.py", *args])
    return prev_build.main()


@pytest.mark.parametrize("flag", ["--help", "-h"])
def test_help_prints_the_usage_and_builds_nothing(flag, monkeypatch, no_build, capsys):
    assert _main(monkeypatch, flag) == 0
    out = capsys.readouterr().out
    assert "--build" in out and "--path" in out


@pytest.mark.parametrize("args", [("--bulid",), ("--client", "windows", "--dry-run")])
def test_an_unknown_flag_is_refused_before_anything_builds(args, monkeypatch, no_build, capsys):
    assert _main(monkeypatch, *args) == 2
    assert args[-1] in capsys.readouterr().err


def test_the_build_flags_the_recipe_passes_still_build(monkeypatch, capsys):
    """`just e2e-prev-build --client windows` → `prev_build.py --build --client windows`."""
    calls = []
    monkeypatch.setattr(prev_build, "build_prev",
                        lambda commit, profile, client, nest=True:
                        calls.append((profile, client, nest)) or {})
    assert _main(monkeypatch, "--build", "--client", "windows", "--profile", "debug") == 0
    assert calls == [("debug", "windows", True)]


SHA = "b" * 40


def test_a_named_commit_client_only_build_keys_on_that_commit(monkeypatch):
    """`just e2e-release-app-build <sha> tui` → `--build --commit <sha> --client tui
    --client-only` (via release_app.py): the named commit, no nest, the pin unread."""
    calls = []
    monkeypatch.setattr(prev_build, "load_pin",
                        lambda: pytest.fail("a named commit must not read the pin"))
    monkeypatch.setattr(prev_build, "build_prev",
                        lambda commit, profile, client, nest=True:
                        calls.append((commit, client, nest)) or {})
    assert _main(monkeypatch, "--build", "--commit", SHA, "--client", "tui",
                 "--client-only") == 0
    assert calls == [(SHA, "tui", False)]


@pytest.mark.parametrize("bad", ["bce577a383", "Z" * 40])
def test_a_named_commit_must_be_a_full_sha(bad, monkeypatch, no_build, capsys):
    assert _main(monkeypatch, "--build", "--commit", bad) == 2
    assert "--commit" in capsys.readouterr().err


def _populate(root, files):
    root.mkdir(parents=True, exist_ok=True)
    for name in files:
        (root / name).write_text("x")


def test_the_tui_cache_is_cold_without_its_sync_agent(monkeypatch, tmp_path):
    """The tui resolves `fauna-sync-agent` beside itself, so a cached tui without one
    is cold — rebuilt once, never launched agentless."""
    monkeypatch.setattr(prev_build, "CACHE_ROOT", tmp_path)
    out = prev_build.cache_dir(SHA)
    tui = prev_build.CLIENT_BINARY["tui"]
    agent = prev_build.CLIENT_COMPANIONS["tui"][0][1]
    _populate(out, [tui])
    assert prev_build.cached_binaries(SHA, client="tui", nest=False) is None
    _populate(out, [agent])
    found = prev_build.cached_binaries(SHA, client="tui", nest=False)
    assert set(found) == {"tui", "fauna-sync-agent"}
    # With the nest asked for, its absence is cold too.
    assert prev_build.cached_binaries(SHA, client="tui") is None


def test_the_tui_build_names_the_agent_package_in_one_cargo_call(monkeypatch, tmp_path):
    runs = []
    monkeypatch.setattr(prev_build, "_run", lambda cmd, cwd, env, timeout: runs.append(cmd))
    monkeypatch.setattr(prev_build.sys, "platform", "linux")
    monkeypatch.setenv("FAUNA_SLOT_HELD_BUILD", "1")
    prev_build._build_client("tui", tmp_path, tmp_path / "t", {}, "debug")
    assert runs == [["cargo", "build", "-p", "fauna-tui", "-p", "fauna-sync-agent"]]


def test_an_unslotted_cargo_step_takes_its_own_build_slot(monkeypatch):
    """Outside `e2e-prev-build`'s wrap — the web leg, or a pytest fixture warming a cold
    cache — a compile still goes through the count."""
    monkeypatch.setattr(prev_build.sys, "platform", "linux")
    monkeypatch.delenv("FAUNA_SLOT_HELD_BUILD", raising=False)
    cmd = prev_build._cargo_cmd(["build", "-p", "fauna-nest"])
    assert cmd[cmd.index("--pool") + 1] == "build"
    assert cmd[cmd.index("--") + 1:] == ["cargo", "build", "-p", "fauna-nest"]
    assert cmd[cmd.index("--pool") - 1].endswith("build-slot.py")


def test_the_web_build_is_the_pinned_web_test_recipe(monkeypatch, tmp_path):
    """web's previous "binary" is the SPA tree the pinned `just web-test` builds — the e2e
    flavour HEAD's `static_dir` serves."""
    runs = []
    monkeypatch.setattr(prev_build, "_run",
                        lambda cmd, cwd, env, timeout: runs.append((cmd, cwd)))
    out = prev_build._build_client("web", tmp_path, tmp_path / "t", {}, "debug")
    assert runs == [(["just", "web-test"], tmp_path)]
    assert out == tmp_path / "apps/fauna-web/build"


def test_the_web_cache_is_cold_until_its_tree_copy_is_stamped(monkeypatch, tmp_path):
    monkeypatch.setattr(prev_build, "CACHE_ROOT", tmp_path)
    out = prev_build.cache_dir(SHA)
    (out / "web-app" / "build").mkdir(parents=True)
    assert prev_build.cached_binaries(SHA, client="web", nest=False) is None
    (out / "web-app" / ".fauna-prev-complete").write_text(SHA)
    found = prev_build.cached_binaries(SHA, client="web", nest=False)
    assert found == {"web": out / "web-app" / "build"}


def _rev(*args):
    return subprocess.run(["git", "-C", str(prev_build.repo_root()), *args],
                          capture_output=True, text=True).stdout.split()


def test_a_client_absent_at_the_commit_is_refused_by_a_tree_read():
    """0.1.5's tree has no `bins/fauna-reports-pass`: the refusal comes from a tree
    read, before anything is extracted. This checkout's HEAD has the client; the
    repository's first commit predates every client."""
    assert prev_build.client_absent_reason(_rev("rev-parse", "HEAD")[0], "reports-pass") is None
    why = prev_build.client_absent_reason(_rev("rev-list", "--max-parents=0", "HEAD")[-1],
                                          "reports-pass")
    assert why and "bins/fauna-reports-pass" in why
    assert "not in this repository" in prev_build.client_absent_reason("0" * 40, "tui")
