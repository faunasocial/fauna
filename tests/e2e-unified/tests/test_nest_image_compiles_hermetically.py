"""The nest image compiles with no network: a fetch RUN, then `--network=none`.

build-compile-cache.md § The boundary's scope and what OFF means, decision (5):
the image's compile is a runner compile the cargo boundary never reaches —
`docker buildx build` runs the Dockerfile's compile lines inside BuildKit — so
its equivalent is BuildKit's own primitive. Each compiling stage downloads its
locked graph in one RUN (`cargo fetch --locked`, `go mod download`) and compiles
in the next with `RUN --network=none`, so no third-party build script or
proc-macro of the production artifact runs with egress. image-pipeline.md
§ The Dockerfile's stages compile hermetically owns the stage shape.

wasm-pack keeps the network for the wasm-bindgen and binaryen downloads it
makes itself; the chunks it would compile are compiled network-less in the RUN
before it with the cargo arguments it passes, and its own RUN fails on any
`Compiling` line; the WASM-chunk parity gate holds the two chunk lists equal. `cargo install wasm-pack` stays networked (an install fetches
and compiles in one command; decision (1) leaves `install` outside).

tier_1: reads the Dockerfile, runs no docker.
"""

import re
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_1

_REPO = Path(__file__).resolve().parents[3]
_DOCKERFILE = _REPO / "Dockerfile"

_COMPILE = re.compile(r"\bcargo build\b|\bgo -C \S+ build\b")


def _stages(text: str) -> list[tuple[str, list[str]]]:
    """(stage name, its RUN instructions with continuations joined)."""
    stages: list[tuple[str, list[str]]] = []
    joined: list[str] = []
    buf = ""
    for line in text.splitlines():
        if not buf and line.lstrip().startswith("#"):
            continue
        buf += line.rstrip("\\").rstrip() + " " if line.rstrip().endswith("\\") else line
        if not line.rstrip().endswith("\\"):
            joined.append(buf)
            buf = ""
    for inst in joined:
        m = re.match(r"^FROM\s+.*?(?:\s+AS\s+(\S+))?\s*$", inst)
        if m:
            stages.append((m.group(1) or "final", []))
        elif inst.startswith("RUN ") and stages:
            stages[-1][1].append(inst)
    return stages


def _runs() -> list[tuple[str, int, str]]:
    return [
        (name, i, run)
        for name, runs in _stages(_DOCKERFILE.read_text(encoding="utf-8"))
        for i, run in enumerate(runs)
    ]


def test_every_compile_run_has_no_network() -> None:
    compiles = [(n, r) for n, _, r in _runs() if _COMPILE.search(r) and "cargo install" not in r]
    assert {n for n, _ in compiles} >= {"rust-native", "rust-wasm", "bridges-builder"}, compiles
    for name, run in compiles:
        assert run.startswith("RUN --network=none "), f"{name}: a compile RUN keeps the network: {run[:160]}"
        if "cargo build" in run:
            assert "export CARGO_NET_OFFLINE=true" in run, f"{name}: cargo is not told it is offline"
        else:
            assert "GOPROXY=off" in run and "GOFLAGS=-mod=readonly" in run, name


def test_each_compiling_stage_fetches_first() -> None:
    for name, runs in _stages(_DOCKERFILE.read_text(encoding="utf-8")):
        offline = [i for i, r in enumerate(runs) if r.startswith("RUN --network=none ")]
        if not offline:
            continue
        fetch = [i for i, r in enumerate(runs) if "cargo fetch --locked" in r or " mod download" in r]
        assert fetch and min(fetch) < min(offline), f"{name}: no fetch RUN before its network-less compile"
        assert not any(runs[i].startswith("RUN --network=none ") for i in fetch), name


def test_wasm_pack_compiles_nothing_and_says_so() -> None:
    wasm_pack = [r for n, _, r in _runs() if n == "rust-wasm" and "wasm-pack build" in r]
    assert len(wasm_pack) == 1, wasm_pack
    run = wasm_pack[0]
    assert "cargo build" not in run
    assert "grep -q '^ *Compiling '" in run and "exit 1" in run
    assert "export CARGO_NET_OFFLINE=true" in run

