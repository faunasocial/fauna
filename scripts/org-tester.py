"""A store-install witness machine's own tester on a staging box — the
secret-free route for an org-nest sign-in identity to the machine that runs
a store witness (org-nests.md § fauna.build → Accounts, the witness-host
tester).

`mint <box>` mints this machine's tester identity for the box (`<platform>-
tester`, e.g. `macos-tester` on the macOS witness host), keeps it where the
provisioning drive keeps its own identities — `~/.config/fauna/staging-box/
<box>/accounts/<localpart>.json`, mode 0600, reused on every later run — and
writes its PUBLIC key and handle into the tracked registry
`tests/e2e-unified/org-testers.toml`. The secret never leaves this machine.
Land the registry; the staging-box provisioning drive reads it on every run
of the box and admits each entry by public key, as it admits the reviewers,
so the identity survives every re-creation with no human step. The store
witnesses (`tests/e2e-unified/tests/artifact/test_*_store_install.py`) sign
in as it through the shipped app's own UI.

`list` prints the registry.

The pure decisions are in tests/e2e-unified/helpers/staging_box.py
(`register_witness_tester`, `load_witness_testers`), tier_1-pinned in
test_staging_box_plan.py. Needs pynacl for the public key, so the recipe runs
it on the e2e venv's interpreter (`just org-tester-mint`). Never prints a
secret.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "tests" / "e2e-unified"))

from helpers import staging_box as sb  # noqa: E402


def _mint(box: str) -> int:
    try:
        entry = sb.register_witness_tester(box)
    except sb.StagingPlanError as e:
        print(f"org-tester: {e}", file=sys.stderr)
        return 1
    handle = f"{entry['localpart']}@{entry['domain']}"
    path = sb.account_path(entry["domain"], entry["localpart"])
    print(f"org-tester: {handle} — identity kept at {path} (mode 0600; the secret "
          "stays on this machine)")
    print(f"org-tester: actor {entry['actor_hex'][:12]}… registered "
          f"{entry['registered_at']}")
    rel = sb.WITNESS_TESTER_REGISTRY.relative_to(REPO)
    if entry["changed"]:
        print(f"org-tester: {rel} written — commit and land it; the staging-box "
              f"provisioning drive admits {handle} on its next run of {entry['domain']}")
    else:
        print(f"org-tester: {rel} already names this identity — nothing to land")
    return 0


def _list() -> int:
    try:
        entries = sb.load_witness_testers()
    except sb.StagingPlanError as e:
        print(f"org-tester: {e}", file=sys.stderr)
        return 1
    if not entries:
        print("org-tester: no witness tester registered")
        return 0
    for e in entries:
        kept = sb.account_path(e["domain"], e["localpart"]).is_file()
        print(f"{e['localpart']}@{e['domain']}  actor {e['actor_hex'][:12]}…  "
              f"registered {e['registered_at']}  "
              f"{'identity kept on this machine' if kept else 'identity kept elsewhere'}")
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    mint = sub.add_parser("mint", help="mint this machine's tester for a staging box "
                                       "and register its public key")
    mint.add_argument("box", nargs="?", default="test.fauna.build",
                      help="the staging box (default: test.fauna.build)")
    sub.add_parser("list", help="print the registry")
    args = ap.parse_args(argv)
    if args.cmd == "mint":
        return _mint(args.box)
    return _list()


if __name__ == "__main__":
    sys.exit(main())
