"""Seed and read the web SPA's account registry straight through
``localStorage`` — the browser's ``LocalStorageSecretStore`` keys every registry
logical key verbatim, so a plain ``setItem`` per key is byte-for-byte what the
registry reads at launch (``libs/fauna-client-accounts/src/web_store.rs``).

This is the ONLY identity store web has (2026-09-24,
``long-term-store.md`` § Downgrade mirror + abandoned-append recovery): the
pre-registry ``fauna_secret`` / ``fauna_node_url`` / ``fauna_pending_invite_*``
/ ``fauna_awaiting_dns_*`` keys are neither written nor read by the SPA any
more, so a test that seeds an identity or a wizard-resume slot writes the
registry shape from ``common.accounts`` and reads the per-actor rows back.
"""
from __future__ import annotations

import json

from common.accounts import actor_id_hex, single_account_seed


def seed_js(fields: dict[str, str]) -> str:
    """A one-expression script writing every ``{logical_key: value}`` pair."""
    return "(() => { " + " ".join(
        f"localStorage.setItem({json.dumps(k)}, {json.dumps(v)});" for k, v in fields.items()
    ) + " return true; })()"


def seed(driver, fields: dict[str, str]) -> None:
    driver.eval_js(seed_js(fields))


def seed_identity(
    driver,
    secret_hex: str,
    *,
    nest_url: str | None = None,
    handle: str | None = None,
    domain: str | None = None,
    device_id: str | None = None,
) -> str:
    """One signed-in (or mid-onboarding, when ``nest_url`` is omitted) identity
    in the registry shape; returns its actor id."""
    seed(
        driver,
        single_account_seed(
            secret_hex, nest_url=nest_url, handle=handle, domain=domain, device_id=device_id
        ),
    )
    return actor_id_hex(secret_hex)


def pending_invite_record(
    *, nest_url: str, handle: str, request_id: str, status_json: str
) -> str:
    """The per-actor ``fauna/{actor}/pending_invite`` value —
    ``fauna_launch_machine::PendingInviteRecord`` as the registry stores it."""
    return json.dumps(
        {"nest_url": nest_url, "handle": handle, "request_id": request_id, "status_json": status_json}
    )


def awaiting_dns_record(
    *, nest_url: str, handle: str, dns_records_json: str, claim_code: str
) -> str:
    """The per-actor ``fauna/{actor}/awaiting_dns`` value —
    ``fauna_launch_machine::AwaitingDnsRecord`` as the registry stores it."""
    return json.dumps(
        {
            "nest_url": nest_url,
            "handle": handle,
            "dns_records_json": dns_records_json,
            "claim_code": claim_code,
        }
    )


def read_key(driver, key: str):
    return driver.eval_js(f"localStorage.getItem({json.dumps(key)})")


def read_index(driver) -> dict | None:
    raw = read_key(driver, "fauna/index")
    return json.loads(raw) if raw else None


def active_actor(driver) -> str | None:
    idx = read_index(driver)
    return idx.get("active") if idx else None


def read_slot(driver, actor: str, slot: str):
    """One per-actor row (``secret`` / ``nest_url`` / ``device_id`` /
    ``pending_invite`` / ``awaiting_dns`` / ``pending_factory_reset``), or None."""
    return read_key(driver, f"fauna/{actor}/{slot}")


def read_record(driver, actor: str, slot: str) -> dict | None:
    raw = read_slot(driver, actor, slot)
    return json.loads(raw) if raw else None


def active_secret(driver):
    """The ACTIVE account's secret — the origin-shared identity every unpinned
    tab resolves to (a pinned tab resolves its own account instead)."""
    actor = active_actor(driver)
    return read_slot(driver, actor, "secret") if actor else None
