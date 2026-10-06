"""Read an app's file-backed credential store — the ``AccountRegistry`` at rest.

The per-app switcher modules each grew a private reader of the same file:
``{credential_dir}/{keyring_app}.json`` is the flat logical-key store the
``CredentialStore`` File backend (tui/linux) and the C# ``LogicalSecretStore``
File backend (windows) write verbatim, with the ``fauna/index`` blob beside the
per-actor slots ``fauna/{actor}/{secret,nest_url,device_id,awaiting_dns,…}``
(``long-term-store.md`` § Multi-account evolution). Reading the file asserts a
registry mutation's effect independent of any reconnect or repaint — the
client-side twin of web's ``localStorage['fauna/index']`` read.

The only per-driver difference is the attribute the resolved path lives under
(tui/linux: ``_resolved_credential_dir`` + ``_resolved_keyring_app``, pinned
across a relaunch by ``preserve_state_across_relaunch()``; windows:
``_cred_dir`` + ``_keyring_app``), so that is the one thing resolved here.
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


def read_store_map(driver) -> dict:
    """The whole flat store (every logical key, verbatim), or ``{}`` when it is
    absent or unreadable."""
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
