#!/usr/bin/env bash
set -euo pipefail

# Start a fauna-nest instance with a fresh temp database for E2E tests.
# Prints the port number to stdout. Cleans up on exit.

NEST_BIN="${NEST_BIN:-target/debug/fauna-nest}"

# Build if needed
if [ ! -f "$NEST_BIN" ]; then
    echo "Building fauna-nest..." >&2
    # In the dev tree the build queues for a machine-wide `build` slot. The slot
    # tool does not ship, and a public checkout has no fleet to queue behind.
    SLOT_PY="$(dirname "$0")/build-slot.py"
    if [ -f "$SLOT_PY" ]; then
        uv run --no-project python "$SLOT_PY" --pool build -- cargo build -p fauna-nest
    else
        cargo build -p fauna-nest
    fi
fi

TMPDIR=$(mktemp -d)
trap 'kill "$NEST_PID" 2>/dev/null; rm -rf "$TMPDIR"' EXIT

# Start fauna-nest with OS-assigned port and open registration
"$NEST_BIN" \
    --bind 127.0.0.1:0 \
    --db "$TMPDIR/nest.db" \
    --no-require-registration \
    --registration-open \
    --handle-domain test.fauna.local \
    2>"$TMPDIR/stderr.log" &
NEST_PID=$!

# Wait for the server to print its listening address
for i in $(seq 1 30); do
    if grep -q "Fauna Node listening on" "$TMPDIR/stderr.log" 2>/dev/null; then
        PORT=$(grep "Fauna Node listening on" "$TMPDIR/stderr.log" | grep -oP ':\K[0-9]+' | head -1)
        echo "$PORT"
        # Keep running until killed
        wait "$NEST_PID"
        exit 0
    fi
    sleep 0.5
done

echo "ERROR: fauna-nest did not start within 15 seconds" >&2
cat "$TMPDIR/stderr.log" >&2
exit 1
