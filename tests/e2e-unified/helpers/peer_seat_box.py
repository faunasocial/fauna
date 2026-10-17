"""Run one peer seat (``fauna_ffi.harness_peer_seat_hold``) as a process of
its own — the entry point a ``tests/platform/docker/`` witness starts inside a
docker box, where the box's network is the point and no build tool reaches.

The box borrows the host's interpreter through the host's loader (the shape
``test_iroh_relay.py`` runs its probe in) and loads the library the host test
already built (``fauna_ffi``'s ``FAUNA_E2E_FFI_LIBRARY``), so nothing here
builds or installs anything.

Usage: ``peer_seat_box.py <nest-url> <seed-hex> <label> <relay-root.der>
<report.json> <release-path> <budget-secs>``. Prints the seat's last report as
JSON on its last line and exits 0; a seat that could not run at all exits 1
with the library's error.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


def main(argv: list[str]) -> int:
    nest_url, seed_hex, label, root_path, report, release, budget = argv
    from fauna_ffi import harness_peer_seat_hold

    try:
        last = harness_peer_seat_hold(
            nest_url, bytes.fromhex(seed_hex), label,
            Path(root_path).read_bytes(), report, release, int(budget),
        )
    except Exception as exc:  # the library's own error, for the test to quote
        print(f"PEER_SEAT_FAILED {exc}", flush=True)
        return 1
    print(json.dumps(last), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
