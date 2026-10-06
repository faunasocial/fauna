#!/usr/bin/env bash
# Spec Y § 1.9 L3 mandate: fauna-protocol MUST NOT depend on WS/HTTP libs.

set -euo pipefail

# This gate asks a PER-ROOT question — "what does fauna-protocol pull in" — so it
# must opt out of workspace feature unification (`.cargo/config.toml` § feature
# unification, guard 2). Under the default the resolve is "as if the whole
# workspace were selected", which is a different question AND, combined with
# `--no-dedupe` below, an explosively larger tree: measured 2026-08-22, the day
# `[resolver] feature-unification = "workspace"` landed, this gate died on
# `xrealloc: cannot allocate 18446744071562067968 bytes` (exit 2) before it could
# grep anything. A gate that cannot finish is not a strict gate, it is no gate.
export CARGO_RESOLVER_FEATURE_UNIFICATION=selected

FORBIDDEN=(
  "tungstenite"
  "tokio-tungstenite"
  "axum"
  "reqwest"
  "hyper"
  "hyper-util"
)

# Use cargo tree to get the full transitive dep graph for fauna-protocol.
# --locked: gate-recipe rule 6 (test_gate_recipes_are_locked's reason) — a gate
# run must never mutate the checkout's Cargo.lock.
#
# STREAMED TO A FILE, never captured into a variable. `TREE=$(cargo tree …)`
# grows the SHELL by the full size of the output, and that output is not bounded
# by anything this script controls — `--no-dedupe` over a resolve whose shape a
# `.cargo/config.toml` edit can change without touching this file. Measured
# 2026-08-24 on a development machine: bash 5.3.9 grows a `$( )` capture to
# whatever the machine will give it (4 G against a 4 G cgroup memory cap, with
# no ceiling of its own), and a ~75 G shell is exactly what the kernel OOM
# killer shot twice on 2026-08-22/23, each time taking a whole terminal session
# down with it.
TREE_FILE=$(mktemp)
trap 'rm -f "$TREE_FILE"' EXIT
cargo tree --locked -p fauna-protocol --prefix none --no-dedupe >"$TREE_FILE" 2>/dev/null

violations=()
for lib in "${FORBIDDEN[@]}"; do
  if grep -qE "^$lib( v|=)" "$TREE_FILE"; then
    violations+=("$lib")
  fi
done

if [ ${#violations[@]} -ne 0 ]; then
  echo "ERROR: fauna-protocol must not depend on transport libraries (Spec Y § 1.9)."
  echo "Found in dep tree:"
  for v in "${violations[@]}"; do
    echo "  - $v"
  done
  exit 1
fi

echo "fauna-protocol L3 mandate check passed."
