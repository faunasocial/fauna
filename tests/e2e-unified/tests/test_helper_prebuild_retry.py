"""Every collection-time helper prebuild retries one lone failure, and its
replayed failure quotes the recipe's output (`build-slot-pool.md` § Build/e2e
slot locks, the collection-time prebuild bullet).

Seen on Windows 2026-11-14: a warm `windows-seal-helper-build` died once with
exit 139 and no output (a Git Bash/MSYS process killed silently), the memo
recorded the failure, and sixteen mail tests then errored with
`see the build output above` over an empty log. The app builds already retried
through `_build_via_just`; the helper builders called `just` directly.

The runner's own retry rules are pinned in `test_pipe_drain.py` (through the app
builder). These pins drive a real helper builder (`_ensure_seal_helper_built`,
the one that crashed) through `_memoized_build`, so what is proved is the
builder-plus-memo layer: a lone crash leaves the memo `ok`, a double crash is
replayed with both attempts quoted, and a slot-wait timeout is not retried.
"""

import ast
import inspect
from pathlib import Path

import pytest

from tests.test_pipe_drain import _fake_just

pytestmark = pytest.mark.tier_1


@pytest.fixture
def seal_helper(monkeypatch, tmp_path):
    """A fresh memo with the generated files already `ok`, so the fake `just`
    sees only the seal-helper recipe, and a directory counting its runs."""
    import conftest

    memo = {"generated-files": ("ok", None)}
    monkeypatch.setattr(conftest, "_BINARY_BUILD_MEMO", memo)
    invocations = tmp_path / "invocations"
    invocations.mkdir()

    def install(body: str) -> None:
        # `$$` per run: an existence check, never a sibling's content read
        # (test_pipe_drain.py's retry pin says why).
        _fake_just(tmp_path, f': > "{invocations}/$$"\n' + body)
        monkeypatch.setenv("PATH", str(tmp_path), prepend=False)

    return conftest, memo, invocations, install


def test_a_lone_helper_build_crash_is_retried_and_the_memo_holds_ok(seal_helper, tmp_path):
    conftest, memo, invocations, install = seal_helper
    sentinel = tmp_path / "first_attempt_happened"
    install(
        f'if [ -e "{sentinel}" ]; then exit 0; fi\n'
        f': > "{sentinel}"\n'
        "exit 139\n"
    )

    path = conftest._ensure_seal_helper_built()

    assert Path(path).name.startswith("seal-helper-testonly")
    assert len(list(invocations.iterdir())) == 2, "one crash, one retry"
    assert memo["seal-helper-binary"] == ("ok", path)
    # A second requester replays the memo: no third run.
    assert conftest._ensure_seal_helper_built() == path
    assert len(list(invocations.iterdir())) == 2


def test_a_helper_build_that_crashes_twice_is_replayed_with_both_attempts_quoted(seal_helper):
    conftest, memo, invocations, install = seal_helper
    install("echo 'Segmentation fault' >&2\nexit 139\n")

    with pytest.raises(RuntimeError) as first:
        conftest._ensure_seal_helper_built()
    assert len(list(invocations.iterdir())) == 2, "retried once, not looped"
    assert memo["seal-helper-binary"][0] == "failed"

    with pytest.raises(RuntimeError) as replay:
        conftest._ensure_seal_helper_built()
    assert len(list(invocations.iterdir())) == 2, "the replay never rebuilds"

    for msg in (str(first.value), str(replay.value)):
        assert "--- attempt 1:" in msg and "--- attempt 2:" in msg
        assert msg.count("exited 139") == 2
        assert msg.count("Segmentation fault") == 2
        assert "seal-helper-build" in msg, "the repro recipe is named"
    assert "already failed to build earlier in this run" in str(replay.value)
    assert "see the build output above" not in str(replay.value)


def test_a_helper_build_slot_wait_timeout_is_not_retried(seal_helper):
    conftest, memo, invocations, install = seal_helper
    install(
        "echo \"no 'build' slot freed within 5400s — queue position 1 of 1\" >&2\n"
        "exit 1\n"
    )

    with pytest.raises(RuntimeError) as excinfo:
        conftest._ensure_seal_helper_built()

    assert len(list(invocations.iterdir())) == 1
    assert "NOT retried" in str(excinfo.value)
    assert memo["seal-helper-binary"][0] == "failed"


def _direct_just_calls(fn_node: ast.AST) -> list[int]:
    """Line numbers of `subprocess.run([... "just" ...], ..., check=True)` in a function."""
    hits = []
    for node in ast.walk(fn_node):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)):
            continue
        if node.func.attr != "run" or not (
            isinstance(node.func.value, ast.Name) and node.func.value.id == "subprocess"
        ):
            continue
        checked = any(
            kw.arg == "check" and isinstance(kw.value, ast.Constant) and kw.value.value is True
            for kw in node.keywords
        )
        names_just = bool(node.args) and any(
            isinstance(c, ast.Constant) and c.value == "just" for c in ast.walk(node.args[0])
        ) or any(
            isinstance(n, ast.Name) for n in ([node.args[0]] if node.args else [])
        )
        if checked and names_just:
            hits.append(node.lineno)
    return hits


def test_no_ensure_builder_calls_just_directly():
    """Every `_ensure_*` builder reaches `just` through `_build_via_just` — the
    one runner that retries and quotes — never a bare checked `subprocess.run`."""
    import conftest

    tree = ast.parse(inspect.getsource(conftest))
    builders = [
        n for n in ast.walk(tree)
        if isinstance(n, ast.FunctionDef) and n.name.startswith("_ensure_")
    ]
    assert len(builders) > 15, "the walk must reach the builders"
    offenders = {b.name: _direct_just_calls(b) for b in builders}
    offenders = {k: v for k, v in offenders.items() if v}
    assert not offenders, (
        f"route these through `_build_via_just(recipe, what)`: {offenders}"
    )
