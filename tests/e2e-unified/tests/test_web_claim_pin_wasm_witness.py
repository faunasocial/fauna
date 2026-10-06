"""tier_3 e2e (web-only): the wasm arm of the claim-time nest-identity pin
guard, `docs/goal/architecture/security.md`
§ Transport trust.

`seed_identity_pin_at_claim` (`libs/fauna-onboarding-machine/src/machine.rs`)
writes the claim-proven root as the domain's durable TOFU pin. Both arms carry
a guard: a SAME-root re-seed must not erase an already chain-accepted
`rotation_seq` (which would disarm fork detection — a re-seed the guard skips
must leave the `@seq` suffix `NestIdentityPinStore::set` never writes intact),
while a DIFFERING root (the "start over onto a new box, same domain" case) must
still overwrite authoritatively. The native arm has had this witnessed since
`claim_seeds_identity_pin.rs` landed; the wasm arm — `LocalStoragePinStore`,
keyed by the nest URL verbatim — never did.

**Design.** `seed_identity_pin_at_claim` is reachable only from a REAL claim
success (`wizard_submit_claim_code`'s `Ok(resp)` arm, or the already-claimed
recovery poll) — never a private hook — so this drives one real claim per case
against a real, never-claimed `fauna-nest` (`provision_target_nest`), through
the actual `claim_code` UI page, exactly the way `claim_seeds_identity_pin.rs`'s
native tests exercise the guard: PRE-SEED the precondition state directly
(there, `MemoryPinStore::set`; here, the E2E bridge's
`set_nest_identity_pin_rotation_accepted_for_test` — the same writer, over the
same bridge, `libs/fauna-onboarding-machine/src/machine.rs::
call_machine_free_method`, added alongside its reader
`nest_identity_pin_rotation_seq_for_test` for exactly this row), then perform
ONE real claim whose `fauna://claim` URI's `nest=` root either matches or
differs from the pre-seeded one, and read the result back through the same
bridge (`nest_identity_pin_for_test` / `nest_identity_pin_rotation_seq_for_test`
— the only reader that can observe the `@seq` suffix at all; the older
`nest_identity_pin_for_test` answers only the pinned id).

⚠ **The claim URI's `nest=` root must be the box's REAL `nest_actor_id`, not an
arbitrary 32 bytes.** `wizard_submit_claim_code` holds the URI's root as the
first-contact identity BEFORE claiming (`hold_first_contact_identity`), and the
claim connection graduates against it via a real channel-binding check — even
over this fixture's plain HTTP the nest signs a possession proof with its own
deployment key, so a fabricated root fails outright with `IdentityMismatch`
("channel binding: nest_actor_id is not the expected nest") before
`seed_identity_pin_at_claim` is ever reached (measured; a fabricated root only
works for the native `FakeNestApi`-backed unit tests, which perform no such
check). The real root is exactly what the nest prints on its own console claim
banner (`bins/fauna-nest/src/claim.rs::claim_banner_lines` — the admin's
out-of-band channel) — `helpers.claim_banner.read_real_nest_actor_id_hex` reads it from
`nest["log_path"]`, the same file the fixture already captures the process's
stdout/stderr to.

Two claims, two nests (a nest may be claimed only once — `ClaimError::
AlreadyClaimed` — so the two guard branches need two boxes, not two claims
against one), asserting the two halves of the guard's own property:

* **same root** (`test_..._same_root_preserves_rotation_seq`): the pre-seeded
  root equals the claim URI's root → the guard's `store.get(nest_url) !=
  Some(id)` is `false` → `.set()` never runs → the chain-accepted `seq`
  survives.
* **differing root** (`test_..._differing_root_overwrites`): the pre-seeded
  root differs → the guard's condition is `true` → `.set()` DOES run,
  overwriting the pin AND clearing the stale `seq` (`LocalStoragePinStore::
  set` writes no `@seq` suffix) — the "cannot be 'never write twice'" half of
  the property.

**Red-verify** (manual, not asserted here): drop the wasm guard's `if` (make
`store.set(nest_url, id)` unconditional, per the row's own correction — NOT
deleting the write) and the same-root case reddens exactly on its
`rotation_seq` assertion; the differing-root case is unaffected either way.
"""

from __future__ import annotations

import json

import pytest

from actions import ActionLayer
from conftest import get_available_apps
from drivers import create_driver
from helpers.claim_banner import read_real_nest_actor_id_hex

if "web" not in get_available_apps():
    pytest.skip("web client not selected", allow_module_level=True)

pytestmark = [pytest.mark.tier_3, pytest.mark.web]

#: A stale identity — a prior box's pin on this same domain. Only ever
#: PRE-SEEDED directly into localStorage through the bridge, never dialed, so
#: (unlike the claim URI's own root) it need not be the box's real identity.
ROOT_STALE = "cd" * 32

#: A handle only its local part matters for (`wizard_submit_claim_code`
#: registers the bare local part; the `@domain` suffix becomes `mail_domain`).
HANDLE = "alice@nest.test"

@pytest.fixture
def web_driver(spa_url):
    """A fresh web driver loaded from the run's SPA proxy — independent of
    whichever nest backs `spa_url` (this test never drives that nest; it
    always redirects the wizard's own `nest` provider override at the
    per-test `provision_target_nest`)."""
    driver = create_driver("web")
    driver.launch({"url": f"{spa_url}/app/"})
    yield driver
    driver.teardown()


def _seed_rotation_accepted(driver, nest_url: str, actor_id_hex: str, seq: int) -> None:
    """Seed a CHAIN-ACCEPTED pin through the bridge — the precondition the
    guard's own property needs, mirroring `claim_seeds_identity_pin.rs`'s
    direct `MemoryPinStore::set`/`set_rotation_accepted` pre-seeds."""
    driver.call_machine_method(
        "set_nest_identity_pin_rotation_accepted_for_test",
        json.dumps({"nest_url": nest_url, "actor_id": actor_id_hex, "seq": seq}),
    )


def _unwrap(raw):
    """`call_machine_method` hands back a JSON-serialised value (a *quoted*
    string, `null`, or a number), but the agent may already have unwrapped it
    — mirrors `test_nest_identity_pin.py::_read_pin`'s tolerance."""
    if raw in (None, "", "null"):
        return None
    if not isinstance(raw, str):
        return raw
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return raw


def _read_pin(driver, nest_url: str):
    return _unwrap(
        driver.call_machine_method(
            "nest_identity_pin_for_test", json.dumps({"nest_url": nest_url})
        )
    )


def _read_rotation_seq(driver, nest_url: str):
    return _unwrap(
        driver.call_machine_method(
            "nest_identity_pin_rotation_seq_for_test", json.dumps({"nest_url": nest_url})
        )
    )


def _claim_via_ui(driver, app, *, nest_url: str, code_uri: str) -> None:
    """Create a fresh identity, jump straight to `claim_code` for `nest_url`
    (`navigate_to_claim_code_for_known_nest` — pure state mutation, the same
    bridge seam `test_claim_code_uri_paste_reaches_the_machine_intact` uses to
    reach this page without a handle-check detour), paste the full claim URI,
    and submit — a REAL `fauna.auth.claim_admin` round trip."""
    driver.wait_for("create-identity-button", timeout=20)
    driver.click("create-identity-button")
    driver.wait_for("identity-continue-button", timeout=15)
    driver.click("identity-continue-button")

    driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest", json.dumps([nest_url, HANDLE])
    )
    driver.wait_for("claim-code-input", timeout=15)
    driver.clear_and_type("claim-code-input", code_uri)
    assert app.is_enabled("claim-code-submit-button"), (
        "claim-code-submit-button should be enabled once a non-empty claim "
        f"input is typed: {driver.diagnose('claim-code-submit-button')} "
        f"error={app.error_text()!r}"
    )
    driver.click("claim-code-submit-button")
    try:
        driver.wait_for("nat-mode-confirm-button", timeout=30)
    except TimeoutError:
        status = (
            driver.get_text("claim-code-status")
            if driver.is_visible("claim-code-status")
            else "(claim-code-status not visible)"
        )
        raise AssertionError(
            "the real claim_admin round trip did not reach nat_mode_choice: "
            f"claim-code-status={status!r} error={app.error_text()!r} "
            f"claim_code_snapshot={driver.call_machine_method('claim_code_snapshot')!r}"
        ) from None
    assert driver.is_visible("nat-mode-confirm-button"), (
        "the real claim_admin round trip did not complete: "
        f"error={app.error_text()!r}"
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_wasm_claim_pin_same_root_preserves_rotation_seq(
    web_driver, provision_target_nest
):
    nest = provision_target_nest
    app = ActionLayer(web_driver)
    nest_url = nest["url"]
    real_root = read_real_nest_actor_id_hex(nest)
    # `set_provider_base_urls` re-mounts the page — before any identity exists
    # (`test_onboarding_localhost.py`'s ordering).
    web_driver.set_provider_base_urls({"nest": nest_url})

    # Precondition: a chain-accepted pin ALREADY sits under this root — the
    # state a genuine prior rotation would have left.
    _seed_rotation_accepted(web_driver, nest_url, real_root, 7)
    assert _read_rotation_seq(web_driver, nest_url) == 7, (
        "precondition: the chain-accepted seed must land before the claim"
    )

    # The claim's held root is the SAME identity — a re-seed of the root the
    # pin already names.
    uri = f"fauna://claim?code={nest['claim_code']}&nest={real_root}"
    _claim_via_ui(web_driver, app, nest_url=nest_url, code_uri=uri)

    assert _read_pin(web_driver, nest_url) == real_root
    assert _read_rotation_seq(web_driver, nest_url) == 7, (
        "a same-root re-seed through seed_identity_pin_at_claim's wasm branch "
        "must not erase an existing chain-accepted rotation seq — doing so "
        "disarms fork detection"
        ""
    )


@pytest.mark.feature("claim-a-fresh-nest")
def test_wasm_claim_pin_differing_root_overwrites(web_driver, provision_target_nest):
    nest = provision_target_nest
    app = ActionLayer(web_driver)
    nest_url = nest["url"]
    real_root = read_real_nest_actor_id_hex(nest)
    web_driver.set_provider_base_urls({"nest": nest_url})

    # Precondition: a chain-accepted pin sits under a DIFFERENT, stale root —
    # a prior box's identity on this same domain. Pre-seeded directly (never
    # dialed), so it need not be a real identity — only the claim URI's root
    # (below) is ever channel-binding-verified.
    _seed_rotation_accepted(web_driver, nest_url, ROOT_STALE, 7)
    assert _read_rotation_seq(web_driver, nest_url) == 7, (
        "precondition: the chain-accepted seed must land before the claim"
    )

    # The claim's held root DIFFERS from the stale pin — "start over onto a
    # new box, same domain".
    uri = f"fauna://claim?code={nest['claim_code']}&nest={real_root}"
    _claim_via_ui(web_driver, app, nest_url=nest_url, code_uri=uri)

    assert _read_pin(web_driver, nest_url) == real_root, (
        "the claim's possession-proven root must replace a prior box's stale "
        "pin — the fix for the same-root case may not be 'never write twice'"
    )
    assert _read_rotation_seq(web_driver, nest_url) is None, (
        "an authoritative overwrite must clear the stale chain-accepted "
        "rotation seq — LocalStoragePinStore::set never writes an @seq suffix"
    )
