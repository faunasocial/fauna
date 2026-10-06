#!/usr/bin/env bash
# Does one registry crate compile to the same bytes twice? A black-box
# reproducibility probe for a single dependency — no dependency source is read.
#
#   scripts/crate-determinism-probe.sh <crate> [runs=3] [out-dir]
#
# Builds `-p <crate>` alone, release, for the wasm32 target AND the host target,
# `runs` times into its own cargo target (`$CARGO_TARGET_DIR/determinism-probe`,
# never the checkout's shared one), wiping between runs, and compares run 1 with
# every later run BY COUNTS ONLY: byte sizes, `cmp -l | wc -l`, `diff | grep -c`.
# Three views per run: the crate's rlib + rmeta as cargo leaves them, the rlib's
# `ar` members (which codegen units moved), and the crate's macro-expanded form
# (`cargo rustc -- -Zunpretty=expanded`, nightly) — the last tells apart "the
# token stream itself varies" (a build script or procedural macro emitting
# different code per compile) from "rustc/LLVM vary on a fixed input".
# Exit 0 when every view is identical across all runs, 1 otherwise, so the same
# command verifies a fix (`release-integrity.md` § Release signing → *Web-app
# verifiability*, piece 1: the 2026-09-27 core-chunk finding).
#
# ⚠ `<out-dir>/run-*/expanded.rs` IS DEPENDENCY SOURCE. Never open, grep or `diff`
# it into a transcript or a review: it is third-party text, read only under the
# rules of release-integrity.md § Reviewing untrusted source. This script only
# ever prints numbers about it, and that is the contract.
#
# Feature resolution mirrors the shipped core wasm chunk: the manifest is
# `libs/fauna-wasm/Cargo.toml` and `CARGO_RESOLVER_FEATURE_UNIFICATION=selected`,
# exactly as `just _wasm-core-impl` runs wasm-pack; override with PROBE_MANIFEST.
# The wasm32 build carries the justfile's `--remap-path-prefix` triple so the
# artifacts are the release recipe's, not a variant of them.
#
# Why its own target and `unset BASH_ENV`: a shell profile that re-derives
# `CARGO_TARGET_DIR` per checkout from BASH_ENV (the primary dev VM's does)
# would otherwise send a nested build elsewhere — it once turned a "two
# different targets" measurement into one target.
set -uo pipefail
CRATE="${1:?usage: $0 <crate> [runs=3] [out-dir]}"
N="${2:-3}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}/determinism-probe"
OUT="${3:-$CARGO_TARGET_DIR/out-$CRATE}"
unset BASH_ENV
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="--remap-path-prefix=$ROOT=/fauna --remap-path-prefix=$CARGO_HOME=/cargo --remap-path-prefix=$CARGO_TARGET_DIR=/fauna/target"
export CARGO_RESOLVER_FEATURE_UNIFICATION=selected
MF="${PROBE_MANIFEST:-$ROOT/libs/fauna-wasm/Cargo.toml}"
TARGET="${PROBE_TARGET:-wasm32-unknown-unknown}"
W="$CARGO_TARGET_DIR/$TARGET/release"
H="$CARGO_TARGET_DIR/release"
LIB="lib$(echo "$CRATE" | tr '-' '_')-"
rm -rf "$OUT"; mkdir -p "$OUT"
echo "crate=$CRATE runs=$N target=$TARGET manifest=$MF"
echo "cargo target: $CARGO_TARGET_DIR  out: $OUT"

quiet() { grep -v '^\s*Compiling\|^\s*Blocking\|^\s*Finished\|^\s*Fresh\|^\s*Removed' || true; }

for i in $(seq 1 "$N"); do
  R="$OUT/run-$i"; mkdir -p "$R/cross" "$R/host" "$R/cross-ar"
  echo "== run $i: $TARGET build ($(date +%T)) =="
  cargo build --manifest-path "$MF" -p "$CRATE" --lib --release --target "$TARGET" --locked --jobs 4 2>&1 | quiet
  echo "cargo exit: ${PIPESTATUS[0]}"
  cp "$W"/deps/"$LIB"* "$R/cross/"
  echo "== run $i: expanded ($(date +%T)) =="
  cargo rustc --manifest-path "$MF" -p "$CRATE" --lib --release --target "$TARGET" --locked -- -Zunpretty=expanded > "$R/expanded.rs" 2> "$R/expanded.err"
  echo "cargo rustc exit: $? ; expanded bytes: $(stat -c%s "$R/expanded.rs") ; stderr lines: $(wc -l < "$R/expanded.err")"
  echo "== run $i: host build ($(date +%T)) =="
  cargo build --manifest-path "$MF" -p "$CRATE" --lib --release --locked --jobs 4 2>&1 | quiet
  echo "cargo exit: ${PIPESTATUS[0]}"
  cp "$H"/deps/"$LIB"* "$R/host/"
  for rl in "$R"/cross/*.rlib; do (cd "$R/cross-ar" && ar x "$rl"); done
  # wipe for the next run: the whole cross-target tree; the crate alone on the
  # host (its proc-macro deps stay compiled — a macro that varies per INVOCATION
  # shows up regardless; one that varies only per compile of itself would not).
  rm -rf "$CARGO_TARGET_DIR/$TARGET"
  cargo clean --manifest-path "$MF" -p "$CRATE" --release 2>&1 | quiet
done

echo "== compare (first run vs each) =="
status=0
cmpdir() { # label dirA dirB
  local a="$2" b="$3" same=0 diff=0
  for f in "$a"/*; do
    local n; n=$(basename "$f"); local g="$b/$n"
    if [ ! -f "$g" ]; then echo "$1 MISSING: $n"; status=1; continue; fi
    if cmp -s "$f" "$g"; then same=$((same+1)); else
      diff=$((diff+1)); status=1
      echo "$1 DIFF: $n  sizes $(stat -c%s "$f")/$(stat -c%s "$g")  differing-bytes $(cmp -l "$f" "$g" 2>/dev/null | wc -l)"
    fi
  done
  echo "$1 summary: identical=$same differing=$diff"
}
for i in $(seq 2 "$N"); do
  cmpdir "$TARGET r1-r$i" "$OUT/run-1/cross" "$OUT/run-$i/cross"
  cmpdir "$TARGET-ar r1-r$i" "$OUT/run-1/cross-ar" "$OUT/run-$i/cross-ar"
  cmpdir "host r1-r$i" "$OUT/run-1/host" "$OUT/run-$i/host"
  a="$OUT/run-1/expanded.rs"; b="$OUT/run-$i/expanded.rs"
  lines=$(diff "$a" "$b" | grep -c '^[<>]'); [ "$lines" -eq 0 ] || status=1
  echo "expanded r1-r$i: sizes $(stat -c%s "$a")/$(stat -c%s "$b") differing-bytes $(cmp -l "$a" "$b" 2>/dev/null | wc -l) differing-lines $lines of $(wc -l < "$a") hunks $(diff -U0 "$a" "$b" | grep -c '^@@')"
done
if [ "$status" -eq 0 ]; then echo "== RESULT: $CRATE reproducible across $N runs ($(date +%T)) =="
else echo "== RESULT: $CRATE NOT reproducible ($(date +%T)) =="; fi
exit "$status"
