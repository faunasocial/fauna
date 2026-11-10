"""tier_3: a folder shared from ANOTHER nest renders in the recipient's
folders list — the live proof of the apple foreign-set (cross-nest) list wiring.

`ui/folders.md` § Implementation status today → the *Foreign-set (cross-nest)
list source* bullet, and § Sharing a folder → the cross-nest members bullet.

**What this uniquely proves over `test_folder_pending_share_accept_decline`**
(the same-nest twin, `test_folders.py`): there, the accepted set appears because
the recipient's OWN nest rosters them on the set's channel, so
`fauna.folders.list_owned_and_shared` returns it as a `role == "member"` row.
Cross-nest, the recipient's nest holds **no row at all** for the set
(`folders.md`: "their nest holds no row for it"), so the ONLY thing that can put
the row on screen is `DevicesMachine`'s union of the member's own sealed
`fauna.state.folder-keys` foreign-set rows — the union gated by `if let Some(source)` on a
source that must be injected by the client (`libs/fauna-devices-machine/src/machine.rs`).
Apple never called `wireDevicesForeignSets`, so that gate was off and a
cross-nest shared set was silently **absent** — not stale, absent. The wiring
landed in the shared FaunaKit `APIClient.devicesMachine()` (
`APIClient.swift:1533`) **build-verified only**; this is the live render.

The production chain each step here drives, end to end:

  owner on nest A `folder.create` + `folder.share` (binds the set to a real
  MLS group) → `welcome.deliver` naming the recipient's nest B → nest A's
  `originate_welcome_deliver` relays it over the federation channel, stamping the
  envelope with nest A's own `origin_nest_url` → the welcome lands in the
  recipient's durable inbox ON NEST B → the apple app's receive loop drains it,
  the contact-gate knocks (the owner is a stranger) → the recipient ACCEPTS in the
  UI → `join_folder_welcome` sees a NON-EMPTY `home_nest_url` and durably writes
  a `ForeignFolder` into the recipient's own `fauna.state.folder-keys` plane
  (`foreign/<channel>` row) → `CustodyForeignSetsSource::foreign_sets()` reads it
  back out (`ws_rpc.rs`) → `DevicesMachine::foreign_rows` maps it to a
  synthetic `role == "member"` summary → the row renders.

Why the sharer nest needs a handle domain: `origin_nest_url` is
`state.handle_domain_if_set()` (`conversations_handlers.rs:1415`), and
`join_folder_welcome` records **nothing** when `home_nest_url` is empty. So nest
A is `cross_nest_foreign` (handle domain == its own loopback authority), not a
bare `second_nest`. The relay target may be plain HTTP: a loopback peer is an
explicit test-only affordance of `federation_channel::validate_peer_url`.

**Scope boundary — this is a RENDER proof, not a content-fetch proof.** The
recorded `home_nest_url` is `https://127.0.0.1:<portA>` (the handler hard-codes
the `https://` prefix onto the handle domain), which is exactly what production
records; a later cross-nest *content* fetch over that URL is the sync-agent's
plane and is pinned in Rust by `bins/fauna-sync-agent/tests/cross_nest_agent_capstone.rs`.
Nothing here asserts a byte transfer.

RED-prove: comment out the `wireDevicesForeignSets` block in
`APIClient.devicesMachine()` (`APIClient.swift`) and the final assertion fails —
the pending-share knock and the accept still work, the row simply never appears.

Convention 8 (`testing.md`): the sharer side is precondition setup (a headless
owner on a nest no GUI is driving), and the behaviour under test — the accept
gesture and the row it produces — is driven and read entirely through the app UI.

tier_3: two real `fauna-nest` binaries + a real GUI app with a live MLS engine.
"""

from __future__ import annotations

import secrets
import time

import pytest

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def _relay_a_strangers_share(nest_a, nest_b, recipient):
    """Precondition setup (convention 8): a stranger owner on nest A creates a
    set, binds it to a real MLS group and relays its folder Welcome to the
    recipient on nest B over the federation channel. Returns
    ``(owner_handle, set_name)``; the recipient's app is not touched."""
    from helpers.admin_wire import admit_adult
    from tests.api import conv_api

    recipient_id = recipient["actor_id_hex"]

    # ── 1. A stranger owner on nest A, with a real set bound to a real MLS group ──
    owner_handle = "xnowner" + secrets.token_hex(3)
    owner = admit_adult(nest_a, owner_handle, domain=nest_a["authority"], token=True)
    set_name = "xnest-" + secrets.token_hex(3)
    conv_api.folder_create(nest_a["port"], owner, set_name, scheme="https")

    # The recipient's login-time KeyPackage publish is best-effort async, and this
    # fetch is CROSS-NEST: nest A relays it to nest B over the federation channel
    # (`originate_keypackage_fetch`). Poll the destructive fetch until one lands —
    # this doubles as the proof the A→B federation channel dials at all.
    deadline = time.monotonic() + 60
    kp = None
    while time.monotonic() < deadline:
        kp = conv_api.keypackage_fetch(
            nest_a["port"],
            owner,
            recipient_id,
            nest_url=nest_b["peer_url"],
            scheme="https",
        )
        if kp:
            break
        time.sleep(1)  # sleep-ok: poll interval of a deadline poll (convention 14 mechanism 1) — the assertion is the `kp` state below, never elapsed time; a green run pays at most one interval
    assert kp, (
        "the owner's nest must reach the recipient's nest over the federation "
        "channel and consume a published KeyPackage; none arrived — either the "
        "recipient never published one, or the A→B relay never dialled"
    )

    channel_id_hex, welcome_bytes, group_id_hex = conv_api.mint_group_welcome(
        bytes(owner["signing_key"]), kp
    )
    # Bind the set to the group ON NEST A, so nest A can resolve the set's name for
    # the relayed envelope (`set_name` — what the foreign row renders as its title;
    # `foreign_rows` maps `set_name.unwrap_or_default()`).
    conv_api.folder_share(
        nest_a["port"], owner, set_name, group_id_hex, scheme="https"
    )

    # ── 2. The CROSS-NEST welcome relay: nest A → nest B, stamped with A's URL ──
    # `kind.group_id` is the RAW MLS group id (what the real owner-side share
    # sends: `FoldersAuthor::share_set` → `WelcomeKind::Folder`), not the channel id.
    conv_api.welcome_deliver(
        nest_a["port"],
        owner,
        recipient_id,
        channel_id_hex,
        welcome_bytes,
        kind={"type": "folder", "group_id": group_id_hex},
        nest_url=nest_b["peer_url"],
        scheme="https",
    )
    return owner_handle, set_name


# apple legs proved the FaunaKit wiring; linux/web/tui widened in
# 2026-08-11. The test body is client-generic (no per-app
# branching). tui was wired the same session
# (`settings/devices.rs::DevicesState::build`, mirroring linux); web
# followed — a two-part fix (read-side wasm export +
# `setForeignSetsSource` wiring in `FoldersSection.svelte`, PLUS the write-side
# `FolderCustodySink` web never registered at all, without which
# `join_folder_welcome` never persisted a `ForeignFolder` to read back).
# linux IS wired (`views/devices_folders/mod.rs`). Its leg was xfailed for one
# day (2026-08-11) over a row whose title read back as the literal mode string
# "sync": not a nest bug at all, but linux's automation agent reading an
# `adw::ActionRow`'s *subtitle* — the read-only member row is the only
# `folder-row` shape that is a plain ActionRow rather than an `ExpanderRow`, so
# it was also the only one whose title no test had ever asserted. linux now
# declares the row's automation text explicitly, as apple and tui always have
# (`testid::set_test_text`; pinned by `automation::find`'s
# `a_content_row_reads_its_declared_text_not_its_subtitle`).
#
# windows and android wire the same source too (windows
# `DevicesMachineHost` → `NestRpcClient.WireDevicesForeignSetsAsync`, android
# `DevicesVM` → `ApiClient`), so the seat takes the fixture's own seven-app
# default rather than a narrower override. Until 2026-10-08 a five-app override
# left windows and android out, and no windows run could collect this witness
# while the catalog still counted it for the windows column.
@pytest.mark.feature("share-a-folder")
def test_cross_nest_shared_folder_renders_as_a_foreign_row(
    folder_share_recipient_app, cross_nest_foreign
):
    """A set shared from nest A appears in the recipient's folders list on nest B
    once accepted — sourced ONLY from their own `fauna.state.folder-keys` foreign-set row."""
    recipient_app, nest_b, recipient = folder_share_recipient_app
    nest_a = cross_nest_foreign  # the sharer's HOME nest (https, loopback authority)
    b = recipient_app.backups
    _owner_handle, set_name = _relay_a_strangers_share(nest_a, nest_b, recipient)

    # ── 3. The recipient's UI: the stranger's cross-nest share knocks ──────────
    b.navigate_devices()
    b.navigate_folders()
    count = b.wait_for_pending_shares(1)
    assert count == 1, (
        "the relayed cross-nest folder welcome should surface as exactly one "
        f"pending-share knock (the owner is a stranger); count={count} "
        f"error={recipient_app.error_text()!r}"
    )

    # The safety filter still holds cross-nest: rostered-but-un-joined never lists.
    assert b.folder_count() == 0, (
        "an un-accepted cross-nest share must not appear in the list; saw "
        f"{b.folder_count()} row(s)"
    )

    # ── 4. Accept → `join_folder_welcome` records the ForeignFolder ─────────
    b.accept_pending_share(0)
    pending = b.wait_for_pending_shares(0)
    assert pending == 0, (
        f"accepting should consume the knock (join + ack); still {pending} pending "
        f"error={recipient_app.error_text()!r}"
    )
    assert not recipient_app.has_error(), (
        f"accepting the cross-nest share surfaced an error: {recipient_app.error_text()!r}"
    )

    # ── 5. THE PROOF: the row can only have come from the foreign-set union ────
    # The list re-fetches on Folders-page-visible; toggle to re-fire the refresh.
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if b.folder_count() >= 1:
            break
        time.sleep(0.5)  # sleep-ok: poll interval of a deadline poll (convention 14 mechanism 1) — paced re-fire of the page-visible refresh; the assertion is the row count, never elapsed time
        b.navigate_devices()
        b.navigate_folders()
    assert b.folder_count() == 1, (
        f"the accepted CROSS-NEST set {set_name!r} should render as exactly one "
        f"row; saw {b.folder_count()}. The recipient's nest holds no row for a "
        "foreign set, so this row exists only if the client injected the "
        "foreign-sets source (`wireDevicesForeignSets` in "
        f"`APIClient.devicesMachine()`). error={recipient_app.error_text()!r}"
    )
    title = b.folder_titles()[0]
    assert set_name in title, (
        f"the foreign row should carry the home-nest-resolved set name {set_name!r} "
        "(relayed on the welcome envelope, recorded on the ForeignFolder, rendered "
        f"by `foreign_rows`' synthetic summary); got {title!r}"
    )
    assert not recipient_app.has_error(), (
        f"rendering the foreign row surfaced an error: {recipient_app.error_text()!r}"
    )


@pytest.mark.feature("share-a-folder")
def test_a_cross_nest_share_names_the_sharer_by_full_handle(
    folder_share_recipient_app, cross_nest_foreign
):
    """A folder shared from another nest names who shared it by their full
    ``handle@domain`` — on the pending-share knock and on the accepted folder row.

    `federation-shared-folders.md` § Cross-nest shared folders + channel append →
    *The cross-nest owner label*: the home nest stamps ``owner_handle`` /
    ``owner_domain`` on the folder Welcome relay, the receiving nest verifies the
    domain binding, and the pair lands on ``WelcomeInbox`` (the knock's
    ``shared_by_display``) and on the member's ``ForeignFolder`` custody record.
    The folder row reads that record through ``CustodyForeignSetsSource`` →
    ``ForeignSetRow`` → ``DevicesMachine::foreign_rows``, which joins the pair
    with ``qualified_handle`` into ``owner_display`` — the "Shared by ‹who›"
    badge. The recipient's own nest holds no ``users`` row for the sharer, so a
    bare handle (or the short-id fallback) is what a missing stamp would show;
    the domain half is what proves the stamp crossed.

    RED-prove: drop the ``owner_handle``/``owner_domain`` mapping in
    ``CustodyForeignSetsSource::foreign_sets`` and the badge assertion fails
    while the knock assertion still holds.
    """
    recipient_app, nest_b, recipient = folder_share_recipient_app
    nest_a = cross_nest_foreign
    b = recipient_app.backups
    owner_handle, set_name = _relay_a_strangers_share(nest_a, nest_b, recipient)
    qualified = f"{owner_handle}@{nest_a['authority']}"

    # ── The knock names the sharer by full handle ────────────────────────────
    b.navigate_devices()
    b.navigate_folders()
    count = b.wait_for_pending_shares(1)
    assert count == 1, (
        "the relayed cross-nest folder welcome should surface as exactly one "
        f"pending-share knock; count={count} error={recipient_app.error_text()!r}"
    )
    knock = b.pending_share_text(0)
    assert qualified in knock, (
        f"the pending share should name its sharer as {qualified!r} — the home "
        "nest's stamped owner label joined with its domain; got "
        f"{knock!r} error={recipient_app.error_text()!r}"
    )

    # ── Accept; the folder row names the sharer the same way ─────────────────
    b.accept_pending_share(0)
    pending = b.wait_for_pending_shares(0)
    assert pending == 0, (
        f"accepting should consume the knock; still {pending} pending "
        f"error={recipient_app.error_text()!r}"
    )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if b.folder_count() >= 1:
            break
        time.sleep(0.5)  # sleep-ok: poll interval of a deadline poll (convention 14 mechanism 1) — paced re-fire of the page-visible refresh; the assertion is the row count, never elapsed time
        b.navigate_devices()
        b.navigate_folders()
    assert b.folder_count() == 1, (
        f"the accepted cross-nest set {set_name!r} should render as one row; saw "
        f"{b.folder_count()} error={recipient_app.error_text()!r}"
    )
    badge = b.shared_badge_text()
    assert qualified in badge, (
        f"the foreign folder row's shared badge should name its sharer as "
        f"{qualified!r} (the ForeignFolder's stamped owner label, joined by "
        f"`DevicesMachine::foreign_rows`); got {badge!r} "
        f"error={recipient_app.error_text()!r}"
    )
    assert not recipient_app.has_error(), (
        f"the cross-nest share surfaced an error: {recipient_app.error_text()!r}"
    )
