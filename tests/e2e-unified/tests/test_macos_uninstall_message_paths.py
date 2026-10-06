"""`fauna-uninstall`'s farewell message must name the places the user's data is REALLY kept.

The uninstaller preserves data by simply not deleting it, then prints a
"preserved at:" block so the user knows where their own files went. That block
is the only place the product ever tells them — nothing else in any app points
at these directories — so a wrong path there sends a user who just uninstalled
looking in an empty folder for data they were promised was kept.

**Why this test exists.** From the 2026-07-19 state-unification move until
2026-08-21 the block labelled `~/Library/Application Support/Fauna/` as
"sync config", which it is not: that directory is the *app*'s data, while the
sync agent's own config/state-db/chunk-cache/logs live in the shared app-group
container (`installers/macos.md` § File Layout, and the doc comment on
`SyncConfig::base_dir` in `bins/fauna-sync-agent/src/config.rs`). The message
named one real directory under the wrong label and omitted the other entirely.
Three separate doc-consistency sweeps flagged it and it survived all three,
because nothing executable asserted the message — the shell script's strings
were witnessed only by a human reading them. This file is that witness.

Static-only — reads repository files, builds nothing, launches nothing
(tier_1), so it runs on every machine rather than only on a macOS build host,
which is the point: the script is edited from any checkout.

The app-group literal itself is owned by `fauna_core::platform_ids` and pinned
across its many copies by `test_apple_identifier_pins.py`; this file consumes
the same spelling and asserts only what the *message* says.
"""
from __future__ import annotations

import os
import re
import subprocess

import pytest

pytestmark = pytest.mark.tier_1


def _repo_root() -> str:
    here = os.path.dirname(os.path.abspath(__file__))
    result = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True, cwd=here,
    )
    return os.path.normpath(result.stdout.strip())


REPO = _repo_root()
UNINSTALL = os.path.join(REPO, "installer", "macos", "fauna-uninstall")

#: The sync agent's real per-user state home (`installers/macos.md` § File
#: Layout) — the user-domain root since 2026-08-25, when the agent's state left
#: the TCC-protected app-group container (§ Identifier domain, item 5).
AGENT_STATE_HOME = "Library/Application Support/Fauna/sync"
#: The app's per-user data home — the parent of the sync home since the move.
APP_DATA_HOME = "Library/Application Support/Fauna"
#: The app-group container — since the move ONLY the sandboxed File Provider
#: extension's state (+ the app-maintained pin replica), never the agent's.
FP_STATE_HOME = "Library/Group Containers/7457N3M72H.group.social.fauna.shared"


def _preserved_block() -> list[str]:
    """The `echo` lines of the completion message's "preserved at:" block.

    Returns the *echoed text*, one entry per line, from the `preserved at:`
    marker up to the blank-line echo that closes the block. Parsing the script
    rather than running it keeps this tier_1: the block is a run of literal
    `echo` statements, which is exactly why it can rot silently.
    """
    lines = open(UNINSTALL, encoding="utf-8").read().splitlines()
    start = next(
        (i for i, ln in enumerate(lines) if 'echo "preserved at:"' in ln),
        None,
    )
    assert start is not None, (
        f"{UNINSTALL} no longer prints a 'preserved at:' block — if the farewell "
        "message was restructured, update this test to match the new shape rather "
        "than deleting it; the message is still the only place the user is told "
        "where their data went."
    )
    block: list[str] = []
    for ln in lines[start + 1:]:
        stripped = ln.strip()
        if stripped == 'echo ""':
            break
        m = re.match(r'^echo "(.*)"$', stripped)
        if m:
            block.append(m.group(1))
    assert block, "the 'preserved at:' block echoed nothing"
    return block


def test_message_names_the_agents_real_state_home():
    """The block names the user-domain dir the sync agent actually writes to, as sync."""
    block = _preserved_block()
    sync_lines = [line for line in block if APP_DATA_HOME in line]
    assert sync_lines and any("sync" in line.lower() for line in sync_lines), (
        f"the 'preserved at:' block never labels {APP_DATA_HOME!r} as holding sync "
        "state, which is where the sync agent's config, state DBs, chunk cache and "
        "logs actually live since 2026-08-25 (installers/macos.md § File Layout — "
        f"{AGENT_STATE_HOME!r}). A user who just uninstalled is told their sync data "
        "is preserved but not where it is.\nBlock was:\n  " + "\n  ".join(block)
    )


def test_no_line_calls_the_container_the_sync_location():
    """The app-group container is the File Provider EXTENSION's state, never the agent's.

    This is the exact defect class the file was written for: the label and the
    path disagreed, and both were individually plausible. Since 2026-08-25 the
    agent never opens the container (a launchd agent is TCC-prompted there on
    every instance — installers/macos.md § Identifier domain, item 5), so a
    line calling it "sync config + state" would send the user to a directory
    that no longer holds their sync state.
    """
    block = _preserved_block()
    assert any(FP_STATE_HOME in line for line in block), (
        f"the 'preserved at:' block never names {FP_STATE_HOME!r}, the File Provider "
        "extension's per-user state (installers/macos.md § File Layout).\nBlock was:\n  "
        + "\n  ".join(block)
    )
    for line in block:
        if FP_STATE_HOME in line:
            assert "sync" not in line.lower(), (
                f"this line labels the app-group container as sync state:\n  {line}\n"
                f"{FP_STATE_HOME!r} holds the File Provider extension's state; the sync "
                f"agent's state is under {AGENT_STATE_HOME!r}. Naming the wrong one "
                "under a 'sync' label is what this test exists to stop."
            )


def test_every_per_user_path_is_under_the_users_home():
    """Per-user lines are rooted at `$USER_HOME`, not at `/` or a bare `~`.

    The block is printed by a script running as root under `sudo`, so a
    per-user path written without `$USER_HOME` would name root's home (or the
    system directory of the same name) and silently point the user at data that
    is not theirs.
    """
    block = _preserved_block()
    for line in block:
        for home in (AGENT_STATE_HOME, APP_DATA_HOME, FP_STATE_HOME):
            if home not in line:
                continue
            if line.lstrip().startswith("/Library/"):
                continue  # the machine-scoped server dir, deliberately absolute
            assert "$USER_HOME/" in line, (
                f"per-user path printed without $USER_HOME (the script runs as root):"
                f"\n  {line}"
            )
