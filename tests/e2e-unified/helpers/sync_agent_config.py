"""Read the per-user `fauna-sync-agent`'s own persisted `config.toml`.

The agent's folder↔folder bindings are device-local state it owns outright —
`sync-agent.md` § Control plane split — so the only latency-independent proof that
a client's bind actually reached the agent is the agent's own config file. Every
suite that asserts one reads it through here rather than hand-building the path,
because the path is **not** flat:

* base dir — `$XDG_CONFIG_HOME/fauna/sync` on linux/tui, `%LOCALAPPDATA%\\Fauna\\sync`
  on windows, `~/Library/Application Support/Fauna/sync` on macOS — the user-domain
  root under the launch's relocated HOME, **never the app-group container** (moved
  out 2026-08-25; `bins/fauna-sync-agent/src/config.rs::SyncPaths::base_dir`);
* **per-actor scope** — on EVERY layout the agent re-scopes everything under
  `<base>/<actor-id-hex>/` the moment a capability is provisioned
  (`service.rs::apply_actor_scope`; `on-demand-files.md` § Multi-account × File
  Provider, consequence 3), so one account's state can never be read as another's.
  The `--data-dir` override (the windows launches) scopes too since 2026-09-26 —
  it only moves the base — so under e2e the config lands in the scoped subdir on
  every platform, and a test that hard-codes the flat path reads a file that never
  appears.

Both layouts are searched, so a test neither races the re-scope nor has to know the
actor id.

Process safety: this module only reads files.
"""

from __future__ import annotations

import tomllib
from pathlib import Path


def agent_state_base(
    config_home: str | Path | None, base: str | Path | None = None
) -> Path:
    """The agent's state root for this launch.

    Two entry shapes, because two isolations exist:

    * derived (`base=None`) — the unix launches, where the agent inherits the
      isolated XDG world and writes under `<config_home>/fauna/sync`;
    * **explicit** (`base=<dir>`) — a launch whose state root the DRIVER knows
      and the XDG derivation cannot express, so `config_home` is not consulted
      (and may be `None`). Two families:

      - windows, where a launch that passed `--data-dir` suppresses the
        derivation entirely and makes that dir the base (the per-actor subdir
        nests under it, as on every layout). The windows tui/app launches do
        this because the agent's default root derives from `%LOCALAPPDATA%`,
        not from anything a private config home moves
        (`drivers/tui.py::sync_agent_state_base`). Both windows drivers now
        relocate `%LOCALAPPDATA%` as well (e2e-conventions.md § point 10, the
        third windows axis), so an *unpinned* agent no longer lands in the
        developer's real profile either — but the explicit pin stays
        load-bearing, because `--data-dir` suppresses the `fauna/sync`
        derivation and this reader needs the resulting base.
      - **apple**, which has no XDG world at all: the launch pins HOME
        (`CFFIXED_USER_HOME`), and the store root is
        `<HOME>/Library/Application Support/Fauna/sync` on macOS (the agent's) or
        `<container>/Library/Application Support/Fauna` on iOS (the in-app
        custodian host's, which has no agent process) — both answered by the
        driver's `sync_agent_state_base`.

    The per-actor scoping below applies to all of them, so callers pass whichever
    they have and never branch on platform.
    """
    if base is not None:
        return Path(base)
    if config_home is None:
        raise RuntimeError(
            "this driver answers neither `sync_agent_state_base` nor `config_home`, "
            "so there is no agent state root to read (a launch that has not "
            "happened yet, or a platform whose driver has not grown the answer)"
        )
    return Path(config_home) / "fauna" / "sync"


def agent_config_paths(
    config_home: str | Path, base: str | Path | None = None
) -> list[Path]:
    """Every `config.toml` the agent may have written for this launch.

    Flat first, then the per-actor subdirs (see the module docstring). Returns the
    paths that exist, in that order.
    """
    root = agent_state_base(config_home, base)
    candidates = [root / "config.toml", *sorted(root.glob("*/config.toml"))]
    return [p for p in candidates if p.exists()]


def config_locations(config: dict) -> list:
    """The agent's persisted location rows — the `locations` key, the only one
    the agent reads (`bins/fauna-sync-agent/src/config.rs` refuses any other)."""
    return config.get("locations") or []


def bound_sync_folder(
    config_home: str | Path, folder_path: str, base: str | Path | None = None
) -> dict | None:
    """The agent's persisted location entry for `folder_path`, or `None`.

    `None` covers every not-yet state uniformly — no config written, a partial
    write mid-poll (a `TOMLDecodeError` is a torn read, not a failure), or the
    binding simply not pushed yet — so a caller just polls this against a
    generous deadline.
    """
    for path in agent_config_paths(config_home, base):
        try:
            config = tomllib.loads(path.read_text())
        except (tomllib.TOMLDecodeError, OSError):
            continue
        found = next(
            (f for f in config_locations(config) if f.get("path") == folder_path),
            None,
        )
        if found:
            return found
    return None


def describe_agent_config(config_home: str | Path, base: str | Path | None = None) -> str:
    """One-line diagnostic for a failed poll: which config files exist and what
    paths they bind. Keeps the assertion self-diagnosing (testing.md § point 6)
    instead of leaving "never appeared" ambiguous between *no agent* and *wrong
    path*."""
    paths = agent_config_paths(config_home, base)
    if not paths:
        root = agent_state_base(config_home, base)
        listing = sorted(str(p.relative_to(root)) for p in root.rglob("*")) if root.exists() else []
        return f"no config.toml under {root} (contents: {listing})"
    parts = []
    for path in paths:
        try:
            config = tomllib.loads(path.read_text())
        except (tomllib.TOMLDecodeError, OSError) as e:
            parts.append(f"{path}: unreadable ({e})")
            continue
        bound = [(f.get("path"), f.get("folder")) for f in config_locations(config)]
        parts.append(f"{path}: {bound}")
    return "; ".join(parts)


# ── The custodian store on disk ───────────────────────────────────────────────
#
# A client-device custodian destination keeps the owner's sealed corpus in a
# store the agent owns, under the same per-actor scoping as everything else it
# writes (`fauna_sync_engine::custodian_store::custodian_store_root` =
# `actor_state_dir(<base>, <actor-hex>)/backup-custodian`). Reading it from a
# test is not archaeology for its own sake: it is the only way to make a
# *failing* self-audit happen, which is the observable
# `behavior/backup-destinations.md` § Implementation status today names as owed
# ("a custodian whose store fails its self-audit produces a visibly-failing row
# on the OWNER's device"). Nothing else can rot a store on demand.

CUSTODIAN_STORE_DIR = "backup-custodian"


def custodian_store_roots(
    config_home: str | Path | None, base: str | Path | None = None
) -> list[Path]:
    """Every custodian store this launch's agent has created, newest layout first.

    Searched rather than derived from the actor id, for the same reason
    `agent_config_paths` searches: the agent re-scopes under
    `<base>/<actor-id-hex>/` when a capability is provisioned, and a test that
    hard-codes one layout reads a directory that never appears. Returns `[]`
    until the host stint has created the root — which is exactly the "not
    hosting yet" state a caller should be polling `custodian_pull_run_now` on
    anyway, never sleeping through.
    """
    root = agent_state_base(config_home, base)
    if not root.exists():
        return []
    candidates = [root / CUSTODIAN_STORE_DIR, *sorted(root.glob(f"*/{CUSTODIAN_STORE_DIR}"))]
    return [p for p in candidates if p.is_dir()]


def custodian_store_blobs(store_root: str | Path) -> list[Path]:
    """Every sealed blob file the custodian store holds.

    `<store>/blobs/<first-2-hex>/<64-hex>` (`CustodianStore::blob_path`'s
    two-level fan-out). Files only: the fan-out directories themselves are not
    content, and a caller corrupting "a blob" must not truncate a directory.
    """
    blobs = Path(store_root) / "blobs"
    return sorted(p for p in blobs.rglob("*") if p.is_file())


def describe_custodian_store(store_root: str | Path) -> str:
    """One-line diagnostic for a custodian-store assertion that failed: what the
    store actually contains, relative to its own root.

    Keeps "no blobs to corrupt" from being ambiguous between *the pass stored
    nothing*, *the store is somewhere else*, and *the layout moved* (testing.md
    § point 6 — a failure diagnoses itself).
    """
    root = Path(store_root)
    if not root.exists():
        return f"{root} does not exist"
    entries = sorted(str(p.relative_to(root)) for p in root.rglob("*"))
    return f"{root} contains {entries}"


def describe_agent_state_base(
    config_home: str | Path | None, base: str | Path | None = None, depth: int = 3
) -> str:
    """One-line diagnostic for `custodian_store_roots` coming back empty: what the
    agent's state base holds, to `depth` levels.

    "No store found" is ambiguous between *the agent hosts under a different
    base than the driver answers*, *the per-actor scope nests deeper than the
    search looks*, and *the store was never created*; the listing tells the
    three apart without a debugger (testing.md § point 6 — a failure diagnoses
    itself). Capped at `depth` so a base holding a whole account store cannot
    flood the failure message.
    """
    root = agent_state_base(config_home, base)
    if not root.exists():
        return f"{root} does not exist"
    entries = sorted(
        str(p.relative_to(root))
        for p in root.rglob("*")
        if len(p.relative_to(root).parts) <= depth
    )
    return f"{root} contains {entries}"
