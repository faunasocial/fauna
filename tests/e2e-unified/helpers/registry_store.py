"""Read an app's file-backed credential store — the ``AccountRegistry`` at rest.

The per-app switcher modules each grew a private reader of the same file:
``{credential_dir}/{keyring_app}.json`` is the flat logical-key store the
``CredentialStore`` File backend (tui/linux) and the C# ``LogicalSecretStore``
File backend (windows) write verbatim, with the ``fauna/index`` blob beside the
per-actor slots ``fauna/{actor}/{secret,nest_url,device_id,awaiting_dns,…}``
(``long-term-store.md`` § Multi-account evolution). Reading the file asserts a
registry mutation's effect independent of any reconnect or repaint.

The only per-driver difference is the attribute the resolved path lives under
(tui/linux: ``_resolved_credential_dir`` + ``_resolved_keyring_app``, pinned
across a relaunch by ``preserve_state_across_relaunch()``; windows:
``_cred_dir`` + ``_keyring_app``), so that is the one thing resolved here — and
web, whose store is the SPA origin's ``localStorage``: its
``LocalStorageSecretStore`` keys every logical key verbatim, so the page's whole
``localStorage`` is the same flat map (``common.cred_store.WebCredStore.read_map``).
A linux launch in real-keyring mode resolves no credential dir and has no store
at rest here (its slots are Secret Service items); a test that reads slots
launches file-backed (``make_launch_harness(..., file_backed=True)``).
"""
from __future__ import annotations

import json
import os


def credential_store_path(driver) -> str | None:
    """``{credential_dir}/{keyring_app}.json`` for this driver's app, or None
    when the driver exposes no file-backed store (web, the mobile apps)."""
    cred_dir = getattr(driver, "_resolved_credential_dir", None) or getattr(
        driver, "_cred_dir", None
    )
    keyring_app = getattr(driver, "_resolved_keyring_app", None) or getattr(
        driver, "_keyring_app", None
    )
    if not cred_dir or not keyring_app:
        return None
    return os.path.join(cred_dir, f"{keyring_app}.json")


def _is_web(driver) -> bool:
    probe = getattr(driver, "is_web", None)
    return bool(callable(probe) and probe())


def read_store_map(driver) -> dict:
    """The whole flat store (every logical key, verbatim), or ``{}`` when it is
    absent or unreadable."""
    if _is_web(driver):
        from common.cred_store import WebCredStore

        return WebCredStore(driver).read_map()
    path = credential_store_path(driver)
    if path is None:
        return {}
    try:
        with open(path) as f:
            stored = json.load(f)
    except (OSError, ValueError, TypeError):
        return {}
    return stored if isinstance(stored, dict) else {}


def read_registry_index(driver) -> dict | None:
    """The persisted ``AccountIndex`` (``{active, accounts:[{actor_id,...}]}``),
    or None when the store or its index blob is absent."""
    idx = read_store_map(driver).get("fauna/index")
    if idx is None:
        return None
    return json.loads(idx) if isinstance(idx, str) else idx


def read_store_slot(driver, key: str):
    """One raw logical slot (``fauna/{actor}/nest_url``, …), or None."""
    return read_store_map(driver).get(key)


def _actor_slot_keys(keys, actor_id: str) -> list[str]:
    """The keys among ``keys`` that are ``actor_id``'s per-actor slots, in all three
    shapes ``PER_ACTOR_KEY_BUILDERS`` uses (``fauna-client-accounts``):
    ``fauna/{actor}/*``, ``{actor}/*`` and the bare ``{actor}``."""
    return sorted(
        k for k in keys
        if k == actor_id
        or k.startswith(f"{actor_id}/")
        or k.startswith(f"fauna/{actor_id}/")
    )


def describe_registry(driver) -> str:
    """A failure-message rendering of the registry at rest: the index verbatim (it
    holds no secret), then, per account and per store file in the credential dir,
    the NAMES of the slots present — never a value, since ``…/secret`` is key
    material.

    The slot names are what ``AccountRegistry::is_provisional`` judges on (every
    per-actor slot but the secret absent, in the app's namespace AND the
    account-store one), so a row that should have been retired as a superseded
    provisional names, in the failure itself, the slot that kept it
    (e2e-conventions.md convention 6). Keys in no account's namespace are listed
    last, also by name only."""
    index = read_registry_index(driver)
    lines = [f"index: {json.dumps(index, sort_keys=True)}"]
    actors = [a.get("actor_id", "") for a in (index or {}).get("accounts", [])]
    if _is_web(driver):
        return "\n".join(lines + _describe_map("localStorage", read_store_map(driver), actors))
    path = credential_store_path(driver)
    if path is None:
        return "(no file-backed credential store for this driver)"
    cred_dir = os.path.dirname(path)
    try:
        names = sorted(n for n in os.listdir(cred_dir) if n.endswith(".json"))
    except OSError as e:
        return "\n".join(lines + [f"(credential dir {cred_dir!r} unreadable: {e})"])
    for name in names:
        try:
            with open(os.path.join(cred_dir, name)) as f:
                stored = json.load(f)
        except (OSError, ValueError, TypeError) as e:
            lines.append(f"{name}: unreadable ({type(e).__name__})")
            continue
        if not isinstance(stored, dict):
            lines.append(f"{name}: not a key/value map")
            continue
        lines.extend(_describe_map(name, stored, actors))
    return "\n".join(lines)


def _describe_map(name: str, stored: dict, actors: list[str]) -> list[str]:
    """One store's lines for ``describe_registry``: per account, the names of its
    slots; then every other key, by name."""
    claimed: set[str] = set()
    lines = [f"{name}:"]
    for actor in actors:
        slots = _actor_slot_keys(stored, actor)
        claimed.update(slots)
        lines.append(f"  {actor}: {slots}")
    lines.append(f"  other keys: {sorted(k for k in stored if k not in claimed)}")
    return lines
