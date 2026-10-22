"""Tier_3 journey: the cold device restores from a custodian NEST — every
device and the home nest gone, the identity secret and the custodian's address
bring the account's state back.

Goal docs: ``docs/goal/ui/nests.md`` § Trust facet — restore from a custodian
nest (the surface, its outcome states, its copy);
``docs/goal/architecture/account-replica-posture.md`` § Replica posture → the
custodian-NEST serve door bullet, *Corollary — the cold device* (bounds
(a)–(e)); ``docs/goal/ui/devices.md`` § Custody facet
(``custody-held-restore-address``, the host's half);
``docs/goal/behavior/onboarding.md`` § 1 → *The cold device* (the entry:
``identity_import``, a fresh box claimed, signed in, then Settings → Nests).

The mechanism is pinned in Rust over two real nests
(``bins/fauna-nest/tests/conformance_custody_nest_serve_client.rs``,
``a_cold_device_restores_at_a_typed_address_and_every_outcome_is_distinct``),
which CONSTRUCTS the custody and calls the shared entry directly. Only this
journey proves the chain a user walks: a custody minted through both apps' UIs,
the host reading the address off their own Devices page, and a factory-reset
app — holding nothing but the identity secret, on a freshly claimed box —
typing that address on its Nests page and getting the account-state rows back
— opened, not merely held — which the engine holder's next full pass publishes
to the fresh box, asked for by the gesture itself (``nests.md`` § Trust facet →
*After a restore*: the app beside its sync agent is a non-holder whose poke
runs no pass, and the agent's own backstop tick is 300 s away, so without the
ask no pass ran inside this test's wait — measured on three runs 2026-10-17).

**Convergence at the fresh box is per ITEM, not per row's bytes.** The bind
leg carries a restored row verbatim where the device still holds it, and a row
the device merged and re-authored arrives as the device's own cover
(``delegable-scope-reclamation.md`` § Delegable-scope reclamation, part (2)),
so the fresh box must list a live row of every item A held; the test records
how many of A's rows arrived verbatim and how many as covers
(``rows_carried_verbatim`` / ``rows_covered``). Measured 2026-10-17 on tui:
all 12 rows A held were fleet-plane rows, which take no cover — ``12``
verbatim, ``0`` covered, ``sealed_rows`` ``0``.

Topology: ``two_nest_custody_rig`` — alice home on TLS nest A, bob on TLS nest
B, B's periodic pump held so every pass is this test's own ``run-now`` poke
(convention 14) — plus a third, unclaimed box C that alice's cold device claims.

**The cold device is the app under test; the lost fleet is the rig's
``fleet_app``.** On every app that drives the ceremony they are one app, reset
at the loss. On web — whose ceremony is a declared absence — the rig runs the
fleet and bob on tui and leaves the web app signed out, so the web column is
exactly the web clause of the cold-device corollary: a browser restoring what
a native fleet custodied, dialing the custodian raw at the address bob's card
shows. Box C is then a browser-dialable one (``start_boxes(browser=True)``:
plain HTTP, the SPA origin on its CORS list), and the web cold device is
routed at it for the whole leg (``_web_onboarding_routed_to``: web's
onboarding dials its own origin until a nest is recorded, and the handle then
resolves to an https URL that box does not serve).

The honest outcomes, each asserted on latency-independent state:

* a plaintext address off loopback reads *address refused*, nothing dialed;
* the fresh box's own address — which holds no custody for her — reads *not
  admitted*;
* a second restore of the custodian's address reads *nothing new*.

**The content opens (``account-replica-posture.md`` § *Escrow custody*,
rulings (2) and (6); bound (e) as amended):** the gesture keys every restored
generation plane-first — the ``fauna.state.escrow-wrap`` rows the custody
carried, opened with the identity secret — between its state-plane walk and
its content walk, so the restored notice states no sealed count. The
measurement ruling (6) owes, per ROW (``sealed_rows``, also recorded as the
test property): BEFORE the gesture keyed (2026-10-16), "Restored 5 of your
account's records. 6 came back sealed …"; AFTER, ``0``.

The content row the lost fleet sealed is alice's custody of bob's nest itself:
the Nests page folds it from the custody-ceremony records and the grant log,
both sealed under the account's generation, and both written by this
journey's own ceremony before the loss. It renders on the fresh device only
when the gesture keyed that generation, so ``custody_count() >= 1`` after the
restore is the content assertion. (A post is not the witness on tui: a post
is the nest's, published by RPC and rendered from the bound nest's feed —
the custody carries the ``post`` scope, but no tui surface reads it from the
store.)
"""

from __future__ import annotations

import re
import secrets

import pytest

from common.auth import account_state_changes
from conftest import _web_onboarding_routed_to
from helpers.box_recovery_launch import claim_fresh_identity, start_boxes
from helpers.custody_hosting import FleetDevice
from helpers.custody_rig import (
    CUSTODY_S,
    custody_log_tail,
    mint_nest_custody,
    poll_devices,
    pump_once,
    receive_once,
)
from helpers.waiting import (
    account_pump_role,
    await_account_runtime_assembled,
    poke_account_pump,
    wait_until,
)
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# One restore: a handshake and two plane walks against a loopback nest.
_RESTORE_S = 120.0
# A published row reaching a nest: a poked (or the agent's own) pass.
_PASS_S = 180.0


#: Both account-state planes a custody copies — the delegable one and the
#: fleet-only one, where the blessing, the custody rows and the device set live.
_PLANES = ("state", "state-fleet")


def _rows(nest: dict, secret: bytes) -> dict[bytes, dict]:
    """alice's own live account-state rows on `nest`, both planes: each sealed
    entry's bytes → the cleartext coordinates the nest files it under (the
    plane, the blinded item key, the authoring writer and its seq)."""
    rows: dict[bytes, dict] = {}
    for scope in _PLANES:
        changes = account_state_changes(
            nest["port"], secret_key=secret.hex(), scope=scope, base_url=nest["url"],
        )["changes"]
        for c in changes:
            if c.get("entry"):
                rows[bytes(c["entry"])] = {
                    "scope": scope,
                    "item": c.get("path_hash"),
                    "writer": c.get("origin_writer"),
                    "seq": c.get("origin_seq"),
                }
    return rows


def _feed(nest: dict, secret: bytes) -> set[bytes]:
    """alice's own account-state rows on `nest`, both planes, as sealed
    entries."""
    return set(_rows(nest, secret))


def _items(rows: dict[bytes, dict]) -> set[tuple[str, str]]:
    """The items `rows` carry, as (plane, blinded item key)."""
    return {(r["scope"], r["item"]) for r in rows.values()}


def _describe_rows(rows: dict[bytes, dict], at: dict[bytes, dict]) -> str:
    """One line per row of `rows`: its coordinates, and what `at` holds of
    its item — the same bytes, another writer's row, or nothing."""
    by_item: dict[tuple[str, str], list[dict]] = {}
    for entry, r in at.items():
        by_item.setdefault((r["scope"], r["item"]), []).append({**r, "entry": entry})
    lines = []
    for entry, r in sorted(rows.items(), key=lambda kv: (kv[1]["scope"], kv[1]["item"] or "")):
        holders = by_item.get((r["scope"], r["item"]), [])
        if any(h["entry"] == entry for h in holders):
            held = "same bytes"
        elif holders:
            held = "covered by " + ", ".join(
                f"{(h['writer'] or '?')[:8]}#{h['seq']}" for h in holders)
        else:
            held = "NO ROW OF THIS ITEM"
        lines.append(
            f"  {r['scope']:<11} item {(r['item'] or '?')[:12]} "
            f"writer {(r['writer'] or '?')[:8]}#{r['seq']}: {held}"
        )
    items = {(r["scope"], r["item"]) for r in rows.values()}
    others = sorted(
        (r for r in at.values() if (r["scope"], r["item"]) not in items),
        key=lambda r: (r["scope"], r["item"] or ""),
    )
    if others:
        lines.append("  -- rows there of items not in the first set:")
        lines.extend(
            f"  {r['scope']:<11} item {(r['item'] or '?')[:12]} "
            f"writer {(r['writer'] or '?')[:8]}#{r['seq']}"
            for r in others
        )
    return "\n".join(lines)


def _until_on_nest(
    app, nest: dict, secret: bytes, wanted, *, what: str, detail=None,
) -> dict[bytes, dict]:
    """Poke alice's pump and re-read her rows on `nest` until `wanted(rows)`
    (`rows` as `_rows` reads them); `detail(rows)` adds to the diagnosis.

    Waits on the EFFECT, never on her app's pass counter: the engine role is
    often held by the co-located sync agent (`account_pump_role` reads
    `(True, False)`), whose passes publish the same shared store — a poke then
    runs nothing in the app, and convergence still arrives."""
    seen: dict = {}

    def landed() -> bool:
        poke_account_pump(app.driver)
        seen["rows"] = _rows(nest, secret)
        return wanted(seen["rows"])

    wait_until(
        landed,
        _PASS_S,
        interval=1.0,
        diagnose=lambda: (
            f"never observed: {what}; feed holds {len(seen.get('rows', ()))} rows; "
            f"pump role {account_pump_role(app.driver)}"
            + (f"\n{detail(seen.get('rows', {}))}" if detail else "")
        ),
    )
    return seen["rows"]


def _entries(changes: list[dict]) -> set[bytes]:
    """A feed page's sealed entries, as comparable bytes."""
    return {bytes(c["entry"]) for c in changes if c.get("entry")}


def _notice_state(text: str) -> str:
    """Which of the six notice states `text` is, by the copy's own templates."""
    states = {
        "running": S.nests.custody_restore_running,
        "refused": S.nests.custody_restore_refused(reason="\0").split("\0")[0],
        "not_admitted": S.nests.custody_restore_not_admitted,
        "mismatch": S.nests.custody_restore_mismatch,
        "nothing_new": S.nests.custody_restore_nothing_new,
        "restored": S.nests.custody_restore_restored(count="\0").split("\0")[0],
    }
    for state, needle in states.items():
        if text.startswith(needle):
            return state
    return f"unknown: {text!r}"


def _restore(app, address: str) -> str:
    """Submit `address` and wait for the notice to leave *running*; returns
    the settled notice text (convention 6: the error surface rides any
    failure)."""
    app.nest_trust.restore_from_custodian(address)
    settled: dict = {}

    def done() -> bool:
        text = app.nest_trust.custody_restore_notice()
        settled["text"] = text
        return bool(text) and _notice_state(text) != "running"

    wait_until(
        done,
        _RESTORE_S,
        interval=0.5,
        diagnose=lambda: (
            f"the restore of {address!r} never settled; notice "
            f"{settled.get('text')!r}, error surface {app.error_text()!r}"
            f"\n--- alice's custody log ---\n{custody_log_tail(app)}"
        ),
    )
    return settled["text"]


@pytest.mark.real_conversations
@pytest.mark.feature("nests-and-trust")
def test_a_cold_device_restores_its_account_state_from_a_custodian_nest(
    two_nest_custody_rig, request, nest_mode, tmp_path_factory, record_property,
):
    rig = two_nest_custody_rig
    alice = rig["alice"]
    # The lost fleet (the ceremony, the pre-loss writes) and the cold device.
    fleet_app, alice_app = rig["fleet_app"], rig["alice_app"]
    bob_app = rig["bob_app"]
    home, custodian = rig["home"], rig["custodian"]
    fleet_app.custody.require_supported()
    fleet_app.nest_trust.require_duration_and_blessing_supported()
    alice_app.nest_trust.require_custody_restore_supported()
    alice_secret = bytes(alice["signing_key"])

    # ── The custody, through both UIs, held on bob's NEST.
    mint_nest_custody(rig)

    # ── alice writes one state row the custody covers: she blesses her home
    # nest. Her pass publishes it to A; the snapshot of A's account-state feed
    # is what the cold device must end with.
    before = _feed(home, alice_secret)
    fleet_app.linked_nests.navigate()
    fleet_app.nest_trust.set_blessed(True, nest_index=0)
    _until_on_nest(
        fleet_app, home, alice_secret, lambda rows: bool(set(rows) - before),
        what="the blessing's row published to alice's home nest",
    )

    # ── B's pump pulls until B holds every row A held — read back as one of
    # alice's own devices would, through the custodied-fleet door.
    probe = FleetDevice(custodian["url"], alice, secrets.token_bytes(32))
    held: dict = {}

    # A is re-read on every try: alice's device keeps writing (its endpoint
    # and pass rows), so a snapshot taken once can be superseded at A before
    # B pulls it. B has caught up when it holds what A holds NOW, and that
    # set is what the cold device must end with.
    def b_holds_what_a_holds() -> bool:
        receive_once(fleet_app)
        receive_once(bob_app)
        pump_once(custodian)
        held["home_rows"] = _rows(home, alice_secret)
        held["home"] = set(held["home_rows"])
        device = probe.connect()
        held["rows"] = set().union(*(_entries(device.state_rows(sc)) for sc in _PLANES))
        return held["home"] <= held["rows"]

    wait_until(
        b_holds_what_a_holds,
        CUSTODY_S,
        interval=0.5,
        diagnose=lambda: (
            f"B never held A's account-state rows: {len(held.get('home', ()))} on A, "
            f"{len(held.get('rows', ()))} at B, "
            f"{len(held.get('home', set()) - held.get('rows', set()))} missing"
            f"\n--- bob's custody log ---\n{custody_log_tail(bob_app)}"
        ),
    )
    on_home = held["home"]
    home_rows = held["home_rows"]

    # ── The host's half: bob reads the restore address off his own held card.
    poll_devices(
        bob_app,
        lambda: bool(bob_app.custody.held_restore_address(0)),
        what="the restore address on bob's held-for-others card",
    )
    card = bob_app.custody.held_restore_address(0)
    found = re.search(r"https?://[^\s]+?(?=\s|$)", card)
    assert found, f"the held card names no address: {card!r}"
    address = found.group(0).rstrip(".,;—")

    # ── Every device lost, and the home nest gone with them. A fresh box C;
    # alice's fresh app imports her identity secret and claims it.
    rig["lose_fleet"]()
    (fresh,), stop_fresh = start_boxes(
        request, nest_mode, tmp_path_factory, "custody-cold-device",
        browser=alice_app.driver.is_web())
    request.addfinalizer(stop_fresh)
    alice_app.driver.reset()
    # Web's onboarding dials its own origin until a nest is recorded, and the
    # handle then resolves to https while C serves the browser plain HTTP, so
    # the web cold device is routed at C for the rest of the journey — cleared
    # at teardown (a no-op on every other app).
    routed = _web_onboarding_routed_to(alice_app.driver, fresh["url"])
    routed.__enter__()
    request.addfinalizer(lambda: routed.__exit__(None, None, None))
    claim_fresh_identity(
        alice_app,
        alice_secret.hex(),
        f"{alice['handle']}@127.0.0.1:{fresh['port']}",
        fresh["claim_code"],
    )
    await_account_runtime_assembled(alice_app.driver)

    # ── Settings → Nests: no custody rows on the cold device — the restore
    # form is there anyway, and it is who the form is for.
    alice_app.linked_nests.navigate()
    assert alice_app.nest_trust.custody_count() == 0, (
        "a cold device holds no custody row before it restores"
    )

    # The honest arms first, while nothing is restored yet.
    refused = _restore(alice_app, "http://custodian.example")
    assert _notice_state(refused) == "refused", refused
    not_admitted = _restore(alice_app, fresh["url"])
    assert _notice_state(not_admitted) == "not_admitted", not_admitted
    assert alice_app.nest_trust.custody_count() == 0, "a refusal restored nothing"

    # ── The restore at the address bob read out.
    restored = _restore(alice_app, address)
    assert _notice_state(restored) == "restored", (
        f"{restored!r}\n--- alice's custody log ---\n{custody_log_tail(alice_app)}"
    )
    applied = int(re.search(r"\d+", restored).group(0))
    assert applied >= 1, restored
    sealed = re.search(r"(\d+) came back sealed", restored)
    sealed_rows = int(sealed.group(1)) if sealed else 0
    record_property("sealed_rows", sealed_rows)
    assert sealed_rows == 0, (
        f"the gesture keys every restored generation before it counts: {restored!r}"
        f"\n--- alice's custody log ---\n{custody_log_tail(alice_app)}"
    )

    # The content the lost fleet sealed renders: bob's nest is her custodian
    # row again, folded from generation-sealed records the gesture opened.
    def custody_renders() -> bool:
        alice_app.linked_nests.navigate()
        return alice_app.nest_trust.custody_count() >= 1

    wait_until(
        custody_renders,
        _RESTORE_S,
        interval=1.0,
        diagnose=lambda: (
            "the restored custody rows never rendered on the cold device; "
            f"error surface {alice_app.error_text()!r}"
            f"\n--- alice's custody log ---\n{custody_log_tail(alice_app)}"
        ),
    )

    # ── The next pass publishes what came back to the fresh box (the bind
    # leg). Convergence is per ITEM, not per row's bytes: a restore that keyed
    # its rows merges them, and the device's own merged row then covers a
    # relay row it walked (`delegable-scope-reclamation.md` § Delegable-scope
    # reclamation, part (2)), which the bind leg therefore never carries
    # verbatim. What the fresh box must end with is a live row of every item
    # A held; the diagnosis names each row of A's that it lacks and what, if
    # anything, holds that item there.
    home_items = _items(home_rows)
    on_fresh = _until_on_nest(
        alice_app, fresh, alice_secret,
        lambda rows: home_items <= _items(rows),
        what=f"a live row of every one of the {len(home_items)} items A held "
             f"({len(on_home)} rows) reaching the fresh box",
        detail=lambda rows: "--- A's rows, and what the fresh box holds of each ---\n"
        + _describe_rows(home_rows, rows),
    )
    carried = on_home & set(on_fresh)
    record_property("rows_carried_verbatim", len(carried))
    record_property("rows_covered", len(on_home) - len(carried))

    # Nothing new the second time.
    again = _restore(alice_app, address)
    assert _notice_state(again) == "nothing_new", again
