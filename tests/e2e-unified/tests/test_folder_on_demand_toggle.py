"""tier_3 — the per-set apple on-demand presence toggle (`folder-on-demand-toggle`),
app-UI driven (on-demand-files.md § Apple File Provider binding / folders.md § Binding).

Apple's on-demand affordance is **set-level, not location-level** (a sanctioned
deviation from the windows `folder-location-mode-toggle` shape): an FP domain
cannot take over an arbitrary user directory, so per set per device there is
exactly **one local presence** — none, a bound always-resident folder, or the
Finder/Files File Provider domain, never two. This toggle is the UI that picks
between the last two, and it is the M5 leg whose cross-app e2e was deferred at
the M5 close.

**Why there is no nest ground-truth read here** (unlike its structural sibling
`test_folder_webdav_toggle.py`, which polls `fauna.folders.list`): on-demand
presence is deliberately **device-local** — `FileProviderDomainPrefs`, the
wrapper over the shared-Rust *disabled-set* store (`FfiOnDemandPrefsStore`, a file
in the app's own state dir), sanctioned device-local config that is never nest
state (a set's Finder presence is a property of THIS device, not of
the folder). So the external verification available to a client-UI test is the
one that matters: that the choice **survives leaving and re-entering the page**,
which a pure `@State` toggle would fail and a store-backed one passes. The
domain add/remove the flip drives is `FileProviderCoordinator.reconcile`, which
is e2e-gated off (FP domains are machine-global — e2e-conventions.md convention
10) and headlessly pinned instead by `FaunaKitTests/FileProviderArbitrationTests`.
This file covers the UI contract those pins do not reach.

The MUTATION is UI-driven throughout (the toggle click); VERIFICATION reads the
toggle's own live state over the uniform `/element/attr?attr=value` contract,
re-entering the page so the read comes from the persisted store rather than from
surviving view state.
"""

import secrets

import pytest

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# `folder-on-demand-toggle` is macos+ios `platform_elements` (ui.yaml
# § folders) — the other five apps render nothing here, by design rather
# than by lag, so they are DESELECTED by the absent marker rather than
# skipped in-body (convention 7: a skip is not coverage, and an app that
# was never supposed to have the surface must not inflate the skip count).
# Both apple targets render the ONE shared FaunaKit `FolderOnDemandToggle`,
# so this is a single renderer proven on two shells. The marks sit on each
# test rather than the module because a module mark is unioned into every
# test's set, and the bound-folder journey below can drive macOS alone.


def _create_expanded_sync_folder(b) -> str:
    """A fresh owner folder, its row expanded — the toggle lives in the
    expanded row body, and folder names are globally unique so each test gets a
    set whose device-local pref has never been touched (default-ON is only
    meaningful on a set nothing has flipped)."""
    name = f"ondemand-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    return name


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("files-on-demand")
def test_folder_on_demand_toggle_defaults_on_and_a_flip_survives_re_entry(logged_in_app):
    """Auto-appear default-ON, and the per-set flip persists to the device-local store.

    `on-demand-files.md` § Apple File Provider binding, *Auto-appear default-ON*
    (user decision 2026-07-18): "A folder the device participates in
    appears in Finder/Files automatically as placeholders (metadata cost only);
    the toggle hides it per set per device."

    The re-entry half is the load-bearing one. `FolderOnDemandToggle` holds its
    checked state in `@State private var isOn`, seeded `.onAppear` from
    `FileProviderDomainPrefs.isEnabled(domainId:)` — so a flip that only moved the
    `@State` (and never reached the store) still reads back correctly *while the
    row stays on screen*, and would silently forget the user's choice on the next
    visit and the next launch reconcile. Navigating away and re-expanding forces
    the read to come from the store.
    """
    app = logged_in_app
    b = app.backups

    name = _create_expanded_sync_folder(b)

    assert b.on_demand_toggle_visible(), (
        "a owner row must render folder-on-demand-toggle on apple; "
        f"{app.driver.diagnose('folder-on-demand-toggle')} error={app.error_text()!r}"
    )
    assert b.on_demand_toggle_state() == "on", (
        "auto-appear is default-ON for a set this device participates in "
        "(on-demand-files.md § Apple File Provider binding); got "
        f"{b.on_demand_toggle_state()!r}"
    )

    # MUTATION (UI): hide the set from Finder/Files on this device.
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "off", (
        f"flipping the toggle must uncheck it; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"the on-demand flip raised an error: {app.error_text()!r}"

    # VERIFICATION: leave the page and come back — the state must now be re-read
    # from `FileProviderDomainPrefs`, not from a surviving `@State`.
    app.mail_settings.navigate()
    b.navigate_folders()
    b.find_and_expand_folder(name)
    assert b.on_demand_toggle_visible(), (
        "the row must still render the toggle after re-entry; "
        f"{app.driver.diagnose('folder-on-demand-toggle')}"
    )
    assert b.on_demand_toggle_state() == "off", (
        "the OFF choice must survive leaving and re-entering the page — a flip "
        "that does not reach FileProviderDomainPrefs is forgotten at the next "
        f"launch reconcile; got {b.on_demand_toggle_state()!r}"
    )

    # And back ON, so the store is proven to move in both directions (an
    # append-only disabled-list bug passes the OFF half alone).
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "on"
    app.mail_settings.navigate()
    b.navigate_folders()
    b.find_and_expand_folder(name)
    assert b.on_demand_toggle_state() == "on", (
        "re-enabling must also survive re-entry (the store's disabled-list must "
        f"drop the name, not just stop growing); got {b.on_demand_toggle_state()!r}"
    )
    assert not app.has_error(), f"the on-demand round-trip raised an error: {app.error_text()!r}"


# macOS alone: the module runs on both apple targets, but this one journey needs a
# local-folder BINDING to arbitrate against, and iOS has no binding surface
# (`on-demand-files.md` § Apple File Provider binding: "iOS has no binding
# surface, so there the rule reduces to FP-domain-or-RemoteOnly"). That is a
# witness that can drive only macOS — MARKED for it, never a `declared_absence`
# on ios (feature-catalog.md § Cell semantics, the marked-witness rule,
# 2026-09-26): what iOS lacks is the binding mechanism, while the outcome this
# test witnesses (`files-on-demand` outcome 1, the per-folder call) holds on iOS
# through the module's other witnesses.
@pytest.mark.macos
@pytest.mark.feature("files-on-demand")
def test_folder_on_demand_toggle_yields_to_a_bound_folder(logged_in_app):
    """Arbitration: a bound always-resident folder outranks the on-demand toggle.

    `on-demand-files.md` § Apple File Provider binding (ratified 2026-07-19): "a
    bound location outranks the on-demand toggle ... so a bound set never gets an
    FP domain ... and unbinding lets the toggle decide again (the domain
    reappears, default ON)". The *coordinator* half of that rule (the shared-Rust
    presence plan: own folders ∩ toggle-enabled ∖ bound) is pinned headlessly by
    `FaunaKitTests/FileProviderArbitrationTests`; this asserts the **UI** half —
    the bound row offers no toggle at all, because offering a control that
    the presence plan would then ignore is exactly the lie the arbitration rule
    exists to prevent.

    iOS never collects this test (the `macos` mark above): it has no local-folder
    binding surface at all, so the rule "reduces to FP-domain-or-RemoteOnly" there
    and there is nothing to bind.
    """
    app = logged_in_app
    b = app.backups

    name = _create_expanded_sync_folder(b)

    # Unbound: the toggle is the set's local-presence control.
    assert b.on_demand_toggle_visible(), (
        "an UNBOUND owner row must render the toggle; "
        f"{app.driver.diagnose('folder-on-demand-toggle')} error={app.error_text()!r}"
    )

    # MUTATION: bind an always-resident folder to this set. Seeded through the
    # uniform `sync_inject_locations` seam rather than the add flow — the real
    # one opens an `NSOpenPanel` no e2e driver can drive (the same sanctioned
    # fixture-setup carve-out `test_folders.py`'s nested-binding macOS twin
    # takes; convention 8 covers the behavior-under-test, which here is the
    # toggle's response, not the picker).
    app.sync_locations.inject_locations(
        [{"path": f"/tmp/fauna-e2e-{name}", "folder": name, "mode": "always"}]
    )

    b.navigate_folders()
    b.find_and_expand_folder(name)
    assert not b.on_demand_toggle_visible(), (
        "a set with a bound always-resident folder must NOT offer the on-demand "
        "toggle — the binding is its one local presence, and the presence plan "
        "would refuse the domain the toggle appears to promise "
        f"(on-demand-files.md § Apple File Provider binding); state="
        f"{b.on_demand_toggle_state()!r} error={app.error_text()!r}"
    )
    assert not app.has_error(), f"binding raised an error: {app.error_text()!r}"

    # Unbinding lets the toggle decide again — the second half of the same rule,
    # and the half a "hide it forever once bound" bug would fail.
    app.sync_locations.inject_locations([])
    b.navigate_folders()
    b.find_and_expand_folder(name)
    assert b.on_demand_toggle_visible(), (
        "unbinding must return the on-demand toggle to the row (the domain "
        f"reappears, default ON); {app.driver.diagnose('folder-on-demand-toggle')}"
    )
    assert b.on_demand_toggle_state() == "on", (
        "the returned toggle reads its never-flipped default, ON; got "
        f"{b.on_demand_toggle_state()!r}"
    )



@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("files-on-demand")
def test_folder_on_demand_toggle_on_enrols_a_place_less_device_once(
    logged_in_app, nest_instance, test_user
):
    """Turning the toggle ON is the enrol gesture — exactly one default place.

    `on-demand-files.md` § Apple File Provider binding, *Auto-appear default-ON*:
    on an owner row where this device holds no place "the toggle is still
    *offered* … and turning it on is the **enrol gesture**: it writes this
    device's place at the default point … through `fauna.folders.places.set`".
    The toggle routes the shared `ensure_place` (`file-sync.md` § 4, *A local
    presence writes the place it needs*), which writes only into a gap — so a
    second OFF/ON writes no second row and leaves the first untouched.

    A wizard-made folder starts place-less (`fauna.folders.create` writes no
    place) and the toggle's default-ON seed writes nothing: only the gesture
    enrols. The nest roster (`fauna.folders.members.list`) is the ground truth —
    the enrolment is nest state, unlike the device-local toggle pref above. The
    MUTATION is the UI click; the roster read is VERIFICATION only.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.waiting import wait_until

    app = logged_in_app
    b = app.backups
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )

    def roster(name: str) -> list[dict]:
        with client:
            return client.call("fauna.folders.members.list", {"name": name})["members"]

    name = _create_expanded_sync_folder(b)
    assert b.on_demand_toggle_visible(), (
        f"{app.driver.diagnose('folder-on-demand-toggle')} error={app.error_text()!r}"
    )
    before = {m["device_id"] for m in roster(name)}

    def enrolled() -> list[dict]:
        return [m for m in roster(name) if m["device_id"] not in before]

    # OFF writes nothing — hiding a presence never touches the roster …
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "off"
    assert enrolled() == [], "turning the toggle OFF must not write a place"

    # … and ON enrols this device at the default point, exactly once.
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "on"
    wait_until(
        lambda: len(enrolled()) == 1,
        20.0,
        diagnose=lambda: f"the ON gesture never enrolled this device on {name!r}; "
        f"roster={roster(name)!r} error={app.error_text()!r}",
    )
    (seat,) = enrolled()
    flags = seat.get("flags", {})
    assert (flags.get("originates"), flags.get("accepts"), flags.get("applies_deletes")) == (
        True,
        True,
        True,
    ), f"the enrol writes the default place (all three flags); got {seat!r}"

    # A second OFF/ON finds the place and writes nothing.
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "off"
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "on"
    assert enrolled() == [seat], (
        "a re-enable must write no second row and leave the enrolled place "
        f"untouched; roster={roster(name)!r}"
    )
    assert not app.has_error(), f"the enrol raised an error: {app.error_text()!r}"


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.real_conversations
@pytest.mark.parametrize("folder_share_recipient_app", ["macos", "ios"], indirect=True)
@pytest.mark.feature("files-on-demand")
def test_folder_on_demand_toggle_on_a_member_row_is_hide_only(folder_share_recipient_app):
    """A folder shared WITH the account carries the toggle too — hide-only.

    `on-demand-files.md` § Shared sets on a capability host, decision 3 (the
    user's answer 2026-09-30): a shared folder appears in Finder/Files by
    default once accepted, and `folder-on-demand-toggle` "renders on a member's
    row as well as an owner's, so a member can hide it … the toggle on a
    member's row is hide-only: it never writes a place" (ui/folders.md
    § Element IDs). A reader's row has nothing to expand, so the toggle sits on
    the row itself.

    The sharer is seeded headlessly (fixture setup, convention 8): a stranger
    owns a set, shares it with the recipient as a reader, and delivers the
    Welcome; the recipient ACCEPTS it through its own UI. MUTATION is the
    toggle click; VERIFICATION is the toggle's own persisted state across
    re-entry (the device-local store, as on an owner row) and the OWNER's
    device roster read as ground truth that no place was written — the
    membership is the seat, and the roster is the owner's, where a member's
    device has no row to write.
    """
    import time

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from helpers.waiting import wait_until
    from tests.api import conv_api

    app, nest, recipient = folder_share_recipient_app
    port = nest["port"]
    recipient_id = recipient["actor_id_hex"]
    b = app.backups

    stranger = register_handled_actor(
        port, handle="od" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    kp = wait_until(
        lambda: conv_api.keypackage_fetch(port, stranger, recipient_id),
        30.0,
        diagnose=lambda: "recipient never published a fetchable KeyPackage for the sharer",
    )
    channel_id_hex, welcome_bytes, group_id_hex = conv_api.mint_group_welcome(
        bytes(stranger["signing_key"]), kp
    )
    set_name = "ondemand-shared-" + secrets.token_hex(3)
    conv_api.folder_create(port, stranger, set_name)
    conv_api.folder_share(port, stranger, set_name, group_id_hex)
    conv_api.welcome_deliver(
        port, stranger, recipient_id, channel_id_hex, welcome_bytes,
        kind={"type": "folder", "group_id": group_id_hex},
    )

    owner = WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(stranger["signing_key"].verify_key),
        signing_key=bytes(stranger["signing_key"]),
    )

    def roster() -> list[dict]:
        with owner:
            return owner.call("fauna.folders.members.list", {"name": set_name})["members"]

    before = roster()

    b.navigate_folders()
    assert b.wait_for_pending_shares(1) == 1, (
        f"the reader share should stage as one pending knock; error={app.error_text()!r}"
    )
    b.accept_pending_share(0)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and b.folder_count() < 1:
        time.sleep(0.5)
        b.navigate_devices()
        b.navigate_folders()
    assert b.folder_count() == 1, (
        f"the accepted share should render as one member row; saw {b.folder_count()} "
        f"error={app.error_text()!r}"
    )

    assert b.on_demand_toggle_visible(), (
        "a member's row must render folder-on-demand-toggle (decision 3); "
        f"{app.driver.diagnose('folder-on-demand-toggle')} error={app.error_text()!r}"
    )
    assert b.on_demand_toggle_state() == "on", (
        "a shared folder joins the presence plan by default (ON); got "
        f"{b.on_demand_toggle_state()!r}"
    )

    # MUTATION (UI): hide the shared set on this device, then ask for it back.
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "off"
    b.navigate_devices()
    b.navigate_folders()
    assert b.on_demand_toggle_state() == "off", (
        "the member's OFF choice must survive leaving and re-entering the page; got "
        f"{b.on_demand_toggle_state()!r}"
    )
    b.toggle_on_demand()
    assert b.on_demand_toggle_state() == "on"

    # Hide-only: neither flip wrote a place on the owner's roster.
    after = roster()
    assert after == before, (
        "the toggle on a member's row must write no place — the membership is "
        f"the seat; roster before={before!r} after={after!r}"
    )
    assert not app.has_error(), f"the member-row flips raised an error: {app.error_text()!r}"
