"""tier_3 e2e: a shared-folder **member** decrypts the owner's content through
their own Media page's download — the Phase-0 member custody-ingest READ LEG,
end to end through a shipping client.

The proof owed by task 5 (D3). The Rust half of
the read leg has been green since the work (custody merge, the join/poll
ingest driver, the Media content-key resolution) and the two-nest capstone
(``bins/fauna-sync-agent/tests/cross_nest_agent_capstone.rs``) proves the
*agent-hosted* recipient chain. Neither runs the **client** chain a human uses:
a member's own client process joining, ingesting custody off the receive loop,
and opening a shared file from its Media page. That is what this module runs, and
it is exactly the  lesson's shape — "BUILT" verified at engine level, never
through the production client path.

Flow (`folders.md` § Sharing, `mls-group-key-material.md` § M2):

1. the member (tui) makes the owner a **contact** — so the folder Welcome
   auto-joins off the chat rail (``contact_arrival_disposition(Confirmed) ==
   Auto``, ``NestFolderGate``) with **no** folders UI on the recipient. That
   matters twice: it is the only join path tui can take today (its folders
   settings sub-page is still a stub, ``ui-actual-tui.yaml``), and it is the
   auto-join arm no e2e covered before — ``test_folder_pending_share_accept_
   decline``'s docstring says so in as many words;
2. the owner (linux GUI) creates a ``sync`` set and shares it — minting the MLS
   group, the genesis content key and the sealed envelope;
3. the member's receive loop joins and **ingests custody**
   (``content_key.get`` → ``open_content_key_envelope`` → ``merge_received_keys``
   → its own ``fauna.state.folder-keys`` plane) — the leg under test, entirely in shared Rust;
4. the owner binds a folder and drops a file — the **engine** seals its chunks
   under the M2 content key (``MediaMachine::upload`` would seal under the
   uploader's own ``BackupKey``, so a Media-page upload could never be
   member-readable: the bound folder is the only producer that fits);
5. the member's Media page lists the shared set's file (``fauna.media.list`` →
   ``enumerate_readable_folders`` admits a group-member's set) and the
   ``media-item-detail-download-button`` downloads + decrypts it — ``MediaMachine::download_file``
   resolving content keys from the member's custody via
   ``NestFolderKeyResolver``, then the chunk-store walk under those keys.

**The decrypted bytes are the assertion.** A member whose custody never
populated fails closed in ``content_open_roots`` (no plaintext, no owner-key
fall-through — FS-BIND-5), so a green run cannot be faked by a partial ingest.

Two apps on one machine, so it runs where both are available. The reader is a
``media_member`` seat on any of the 7 apps, each reading through the
``media-item-detail-download-button`` download (user-approved 2026-09-25; tui,
the last app to adopt it, 2026-10-06 — before that its seat read through the
AV-only external-open handoff, which is why the shared file was an ``.mp4``
until then): **web** (the browser download is the observed plaintext) and
**tui**, **linux**, **macOS**, **iOS** and **windows** (the file their
dialog-less e2e save writes into ``driver.download_dir()``; android's seat
launches, its driver has no read-back yet — see ``_MEMBER_SEATS``) — each app's ``NestFolderKeyResolver`` resolves
the set's content keys from the same custody. The first two tests were written
when tui had no folder wizard or share UI, so their owner is linux — or macOS,
which gives the apple member arms an owner on macOS, or windows, which gives
the windows member arm an owner on Windows — and ``--app`` must select the
owner's app plus the member's for an arm to survive collection; the two at the
bottom (a removed member, a second share) run the owner on tui as well, so
``--app tui`` alone selects their tui × tui arms, and their member is the same
``media_member`` seat — which is how an app that binds no folder of its own
(web, iOS, android) witnesses them at all: as the person shared WITH. Hence the
module carries **both** client markers, and the other arms carry theirs on the
seat params.
"""

from __future__ import annotations

import os
import secrets
import time

import pytest

from actions import ActionLayer
from actions.media import MediaActions
from conftest import (
    _E2E_SHARE_RECIPIENT_DEVICE_ID,
    MAIL_PRIMARY_DOMAIN,
    get_available_apps,
)
from drivers import create_driver
from helpers.budgets import RECEIVE_CYCLE_S
from helpers.folder_content import (
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
)
from helpers.folder_share import (
    SHARE_ROSTER_S as _SHARE_ROSTER_S,
    content_key_get as _content_key_get,
    headless_member as _headless_member,
    roster_index as _roster_index,
    share_through_owner_ui as _share_through_owner_ui,
)
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.linux, pytest.mark.tui]

# The scan cadence is no per-folder choice since phase 5 (2026-08-20): every
# e2e launch ticks at the harness's 30 s `FAUNA_E2E_RESCAN_MS` default
# (`drivers/tui.py` / `drivers/linux.py`; the compile-gated seam is
# `always_resident::rescan_interval`), so a missed watcher event still heals inside
# the poll windows below — the cadence the retired wizard picker used to set.

# NOT audio/video, on purpose: every seat reads through the cross-app download
# (`media-item-detail-download-button`), which paints for any item — and a
# non-AV file is exactly what tui's AV-only external-open handoff could never
# read (`fauna_core::share::is_audio_video_filename`), so this is the witness
# that tui's member can open one.
_FILE_NAME = "shared-report.pdf"

# Distinctive and multi-kilobyte: the bytes must survive chunk → seal-under-M2 →
# upload → member fetch → content-key open → whole-file verify, so a truncated
# or wrong-key read cannot coincidentally match.
_FILE_BYTES = b"%PDF-shared-set-payload " * 512

# The post-rotation file (the rotation test). Distinct bytes from `_FILE_BYTES`
# so a stale-manifest read cannot pass by returning generation 1's plaintext.
_FILE2_NAME = "shared-report-gen2.pdf"
_FILE2_BYTES = b"%PDF-post-rotation-payload " * 512

# Every wait below is a named, generous budget polled to a deadline — green runs
# pay only what they use (`testing.md` § conventions point 14).
_UPLOAD_VISIBLE_S = 120.0
_HANDOFF_S = 60.0
_KEYPACKAGE_S = 30.0
# The owner's evict runs an MLS Remove + rotation + re-seal + envelope re-publish
# before the agent's engine restarts under the new generation; generous because
# every step crosses a process boundary.
_REKEY_S = 180.0


# ── helpers ──────────────────────────────────────────────────────────────


def _owner_actor_hex(owner_app) -> str:
    """The owner's hex actor id off its own reported session state.

    ``folder_share_owner_app`` yields ``(app, nest)`` and keeps its actor dict
    private; linux reports ``session.actor_id`` in its state JSON
    (``apps/fauna-linux/src/main.rs``), which is the same value the fixture wrote
    with ``set_state``."""
    actor_hex = owner_app.driver.get_state("session.actor_id")
    assert isinstance(actor_hex, str) and len(actor_hex) == 64, (
        "the owner's session state must report its 32-byte hex actor id (the "
        f"member's contact row is keyed on it); got {actor_hex!r}"
    )
    return actor_hex


def _member_media_items(nest, member) -> list[dict]:
    """``fauna.media.list`` as the MEMBER — the nest-side witness that the join
    landed (a non-member's aggregate would not contain the owner's set at all,
    ``folder_authz::enumerate_readable_folders``). Read directly rather than
    through the UI so a failure separates "never joined / never recorded" from
    "the client did not paint it"."""
    from common.auth import _user_call

    return _user_call(
        nest["port"],
        member["signing_key"].encode().hex(),
        "fauna.media.list",
        {"limit": 100, "cursor_version": 2},
        nest["url"],
    ).get("items", [])


def _make_contacts(nest, member, owner_actor_hex: str) -> None:
    """Make the OWNER a confirmed contact **of the member** — the precondition
    that turns the incoming folder Welcome into an auto-join.

    Direction matters: ``NestFolderGate`` resolves ``fauna.contacts.status`` on
    the *recipient's* own connection, so it is the member's row for the sharer
    that decides. Driven over raw WS-RPC as fixture setup (e2e rule 8 carve-out
    (b) — arranging a precondition, not standing in for the gesture under test);
    ``knocks.accept`` upserts an ``accepted`` row with no prior knock and
    ``contacts.confirm`` promotes it, exactly as ``tests/api/test_contacts_api.py``
    pins. Both ``accepted`` and ``confirmed`` map to ``Auto``
    (``fauna_core::data::contact_arrival_disposition``); confirm anyway so the
    precondition does not rest on the weaker of the two mappings.
    """
    from common.auth import _user_call

    secret = member["signing_key"].encode().hex()
    for kind in ("fauna.knocks.accept", "fauna.contacts.confirm"):
        _user_call(nest["port"], secret, kind, {"peer_id": owner_actor_hex}, nest["url"])

    status = _user_call(
        nest["port"], secret, "fauna.contacts.status",
        {"peer_id": owner_actor_hex}, nest["url"],
    ).get("status")
    assert status in ("accepted", "confirmed"), (
        "the member must hold the sharer as an accepted/confirmed contact or the "
        f"Welcome knocks instead of auto-joining; got {status!r}"
    )


def _member_conv_diagnosis(member_app) -> str:
    """The member client's own conversations-rail evidence.

    ``agent_diagnosis`` filters to sync/engine/agent keywords — right for the
    owner's agent, wrong here: the member's interesting lines are the receive
    loop's folder poll and the custody ingest, which that filter drops. This
    keeps the poll/custody/folder/MLS lines plus anything loud.
    """
    try:
        text = member_app.driver.app_stderr_text()
    except Exception as exc:  # pragma: no cover - diagnostic path only
        return f"  [member] client stderr unavailable: {exc!r}"
    keys = (
        "folder", "folder", "custody", "content_key", "content key", "poll",
        "welcome", "mls", "epoch", "media", "ERROR", "error", "WARN", "warn", "panic",
    )
    lines = [ln for ln in text.splitlines() if any(k in ln for k in keys)]
    tail = "\n".join(f"    {ln}" for ln in lines[-60:]) or "    <no matching lines>"
    return f"  [member] client stderr (conversations/custody tail):\n{tail}"


def _member_client_probe(member_app, label: str) -> str:
    """The member client's own state at one instant — the discriminator stderr
    could not give (the tui logs nothing useful at default level).

    Reads four things that between them separate every way the Media browser can
    read empty, because ``MediaMachine::refresh`` **keeps prior data on Err** and
    only replaces it on Ok (``libs/fauna-media-machine/src/machine.rs``):

    - ``data.media`` — a machine-backed snapshot serializes ``items`` **plus**
      ``folders``/``sort``/``filter``/``view_grid``; a **missing** machine
      serializes the bare ``{"items": []}`` (``media/mod.rs::state_json``). So
      the presence of the ``filter`` key alone says whether the page has a
      machine at all, and a non-null ``filter`` would say the view is scoped to
      a set that no longer matches.
    - ``nav`` — which view is actually mounted. ``item_count()`` reads rendered
      elements, so a nav that did not land on Media reads as an empty library.
    - ``session`` — whether the client still holds the same authenticated actor
      (the machine's lifetime is the session's: built at the post-auth hook,
      dropped at sign-out).
    - ``messages.error`` — the same text the ``error-message`` element paints.
    """
    driver = member_app.driver
    out = [f"  [member probe: {label}]"]
    for path in ("data.media", "nav", "session", "messages"):
        try:
            out.append(f"    {path} = {driver.get_state(path)!r}")
        except Exception as exc:  # pragma: no cover - diagnostic path only
            out.append(f"    {path} = <unavailable: {exc!r}>")
    try:
        out.append(f"    connection-status = {driver.get_text('connection-status')!r}")
    except Exception as exc:  # pragma: no cover - diagnostic path only
        out.append(f"    connection-status = <unavailable: {exc!r}>")
    # An open detail surface paints ZERO `media-item` elements while
    # `data.media.items` stays full — the exact "empty library" costume
    # `_reenter_media` now defuses. Report both sides so the two are never
    # confused again.
    try:
        out.append(
            f"    media-item-detail open = {driver.is_visible('media-item-detail')!r}"
            f", rendered media-item count = {driver.count('media-item')!r}"
            f", media-empty-state = {driver.is_visible('media-empty-state')!r}"
        )
    except Exception as exc:  # pragma: no cover - diagnostic path only
        out.append(f"    detail/list render = <unavailable: {exc!r}>")
    # Web has no `data.media` state and no stderr: its evidence is the browser
    # console ring the bridge captures (the wasm tracing subscriber echoes to it
    # — `apps/fauna-web/src/lib/wasm.ts` `installLogging`), which is where the
    # custody ingest, the folder poll and a page error would have spoken.
    if driver.is_web():
        try:
            lines = driver.console_log()
            tail = "\n".join(f"      {ln}" for ln in lines[-40:]) or "      <empty>"
            out.append(f"    browser console (last {min(40, len(lines))} of {len(lines)}):\n{tail}")
        except Exception as exc:  # pragma: no cover - diagnostic path only
            out.append(f"    browser console = <unavailable: {exc!r}>")
    return "\n".join(out)


def _reenter_media(driver) -> None:
    """Land on the Media **browser** and re-fire its one nest read — now the
    shared :meth:`MediaActions.reenter` (lifted there 2026-07-29, when the
    multiseat own-ack barrier hit the same frozen-listing trap as run
    ``20260724-07``'s false red). The mechanism — the nav-edge-only refresh AND
    the open-detail-owns-the-pane artifact that cost this suite five runs — is documented on the action."""
    MediaActions(driver).reenter()


def _await_member_item(app, driver, name: str) -> int:
    """Re-enter Media until the item named ``name`` paints; return its index."""
    media = MediaActions(driver)
    deadline = time.monotonic() + _UPLOAD_VISIBLE_S
    names: list[str] = []
    probed = False
    while time.monotonic() < deadline:
        _reenter_media(driver)
        # A GUI app loads the re-entered page asynchronously (tui's agent nav
        # awaits the nav-edge refresh; web's route mounts, builds its machine
        # and pulls `fauna.media.list` after the navigate returns), so read the
        # names only once the page SAYS it loaded — rows, or the empty state.
        # Reading straight after the navigate observes the pre-load blank every
        # time, and the next re-entry tears the in-flight load down: on a loaded
        # box the item then never paints inside the budget however healthy the
        # client is (measured on web 2026-09-25, this test's first web arm).
        media.wait_for_loaded()
        names = media.item_names()
        if name in names:
            return names.index(name)
        if not probed:
            # One probe on the FIRST miss, while the failure is fresh: the
            # 120 s of polling below would otherwise only ever show the
            # settled state, and "empty of everything" needs its cause
            # captured at the moment it first appears.
            probed = True
            print(_member_client_probe(app, f"first miss of {name!r}"))
        time.sleep(2.0)
    pytest.fail(
        f"[member] the shared set's {name!r} never painted in the member's Media "
        f"browser within {_UPLOAD_VISIBLE_S:.0f}s (saw {names!r}). The nest-side "
        f"`fauna.media.list` assertion above already passed, so the row IS "
        f"readable by this actor — this is the client projection, not the join. "
        f"error={app.error_text()!r}\n" + _member_client_probe(app, "at failure")
    )


def _arrange_shared_set_with_file(owner_app, nest, member, tmp_path):
    """Everything both tests need before they diverge: the member is a contact,
    the owner has created + shared a ``sync`` set, and one file is uploaded
    through the owner's engine (sealed under content-key generation 1).

    Returns ``(set_name, owner_folder)``.
    """
    from tests.api import conv_api

    owner_actor_hex = _owner_actor_hex(owner_app)

    # The member's login-time KeyPackage publish is best-effort async and the
    # owner's share must fetch one to admit them. Poll the NON-destructive count
    # (a `keypackage_fetch` probe would consume the very package the share needs).
    deadline = time.monotonic() + _KEYPACKAGE_S
    while time.monotonic() < deadline:
        if conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0:
            break
        time.sleep(1)
    else:
        pytest.fail(
            "the member never published a fetchable KeyPackage, so the owner's "
            "share cannot admit them to the set's MLS group"
        )

    _make_contacts(nest, member, owner_actor_hex)

    set_name = f"shared-media-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])

    deadline = time.monotonic() + _SHARE_ROSTER_S
    while time.monotonic() < deadline:
        if ob.shared_member_count() == 1:
            break
        time.sleep(0.5)
    assert ob.shared_member_count() == 1, (
        "the share should land exactly one member (the set's MLS group + genesis "
        "content key + sealed envelope are minted by this gesture); "
        f"error={owner_app.error_text()!r}"
    )

    # The engine — not the Media page — is the producer that seals under the M2
    # content key, so a bound folder is the only way to put member-readable bytes
    # in a shared set (see this module's docstring).
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")
    atomic_write(owner_folder / _FILE_NAME, _FILE_BYTES)
    await_agent_upload(owner_app, _FILE_NAME, seat="owner")
    return set_name, owner_folder


def _path_hash(path: str) -> str:
    """The canonical hash-addressed path key — ``fauna_core::sync::path_hash``,
    plain BLAKE3 over the normalized (forward-slash) relative path. Post-S9-flip
    (2026-08-02) ``fauna.media.list``'s plaintext ``path`` wire field serves only
    the empty-string scrub sentinel for an ordinary set — matching must key on
    this hash instead (mirrors ``tests/api/test_media_seed.py::_path_hash``;
    ``file-sync.md`` § Sealed names & paths)."""
    import blake3

    return blake3.blake3(path.encode()).hexdigest()


def _await_member_row(nest, member, set_name: str, name: str) -> dict:
    """Wait until ``fauna.media.list`` returns ``name`` FOR THE MEMBER.

    Proves the auto-join actually put the member on the set's roster: the
    aggregate is scoped to sets this actor may read
    (``folder_authz::enumerate_readable_folders``), so a knocking (un-joined)
    member sees nothing here. Checked before any UI assertion so a join failure
    never reads as a rendering or decrypt failure.

    Matches on ``path_hash``, not the plaintext ``path`` wire field — the
    latter is the ratified scrub sentinel (empty string) for an ordinary set
    since the S9 flip, so a plaintext-``endswith`` match never fires postdated
    2026-08-02 (found 2026-08-14: this helper had silently gone stale — every
    call timed out at the full budget with a row present but unmatched).
    """
    expected_hash = _path_hash(name)
    deadline = time.monotonic() + _UPLOAD_VISIBLE_S
    items: list[dict] = []
    matching: list[dict] = []
    while time.monotonic() < deadline:
        items = _member_media_items(nest, member)
        matching = [
            it
            for it in items
            if bytes(it.get("path_hash") or b"").hex() == expected_hash
        ]
        if matching:
            break
        time.sleep(2.0)
    assert matching, (
        f"[member] fauna.media.list returned no row for {name!r} (path_hash "
        f"{expected_hash!r}) within {_UPLOAD_VISIBLE_S:.0f}s (saw {items!r}). "
        "The owner's engine DID report uploading it, so either the Welcome "
        "never auto-joined (the contact gate → the member is not on the "
        "roster) or the record never landed."
    )
    # By hash: a sealed set's rows carry no plaintext set name (schema 114) —
    # `folder` is blank beside `folder_hash` + `folder_sealed`.
    from helpers.set_names import set_name_hash

    row_hash = matching[0].get("folder_hash")
    assert matching[0].get("folder") == set_name or (
        row_hash is not None and bytes(row_hash) == set_name_hash(set_name)
    ), f"the row must be attributed to the shared set: {matching[0]!r}"
    return matching[0]


def _member_opens_and_reads(member_app, member_driver, name: str, expected: bytes) -> None:
    """Open the item named ``name`` from the member's own Media page and assert
    the plaintext the member's app produces is ``expected`` — through the
    ``media-item-detail-download-button`` download every app ships (web's
    captured browser download, a native app's dialog-less e2e save into
    ``driver.download_dir()``; tui's included, 2026-10-06).

    The assertion that cannot be faked: a member whose custody lacks the sealing
    generation fails closed in ``content_open_roots`` (no plaintext, no owner-key
    fall-through — FS-BIND-5), so produced plaintext *is* proof of the key.

    Single attempt, full ``_HANDOFF_S`` window — callers whose custody could
    still be catching up (the post-rotation file) anchor on
    ``await_receive_cycle_after`` BEFORE calling this, per convention 14
    mechanism 3, rather than this function retrying the gesture itself.
    """
    index = _await_member_item(member_app, member_driver, name)

    media = MediaActions(member_driver)
    media.open_item_detail(index)
    versions = media.wait_for_version_count(1)
    assert versions >= 1, (
        f"[member] {name}'s version rows never loaded, so the download trigger "
        "cannot paint (it gates on a downloadable manifest); "
        f"error={member_app.error_text()!r}"
    )
    _download_and_read(member_app, member_driver, media, name, expected)


def _download_and_read(member_app, member_driver, media, name: str, expected: bytes) -> None:
    """The download arm of :func:`_member_opens_and_reads`: press the detail's
    download button (user-approved 2026-09-25, ``ui/media.md`` § Element IDs)
    and compare the saved file with the owner's plaintext.

    The download is the shared ``MediaMachine::download_file`` walk keyed by
    the latest version row — the same query tui's external-open runs — with the
    app's ``NestFolderKeyResolver`` resolving the set's content keys from the
    member's own custody; the save path (web's browser download, a native
    app's file) is the only platform glue, read back by
    ``MediaActions.download_open_item``.
    """
    try:
        got = media.download_open_item(timeout=_HANDOFF_S)
    except Exception as exc:  # the click, the capture or the walk failed
        error = (
            member_driver.get_text("error-message")
            if member_driver.is_visible("error-message")
            else ""
        )
        pytest.fail(
            f"[member] the download of {name!r} produced no file: {exc!r}. A "
            "content-key miss fails closed in `content_open_roots` ('lacks "
            "content-key generation N'), which is what a member whose custody "
            "never ingested (or never RE-ingested, across a rotation) looks "
            f"like; a missing trigger means the page did not paint it. "
            f"error-message: {error or '(none)'}"
        )

    assert got == expected, (
        f"[member] the downloaded bytes are NOT the owner's plaintext for {name!r} "
        f"({len(got)} bytes vs {len(expected)} expected). The member decrypted "
        "*something*, so this is a chunk-walk / reassembly fault, not a key fault "
        "(a wrong key cannot produce plausible plaintext)."
    )

    assert not member_driver.is_visible("error-message") or not member_driver.get_text(
        "error-message"
    ), "[member] the successful download must leave no error banner"


# ── fixture ──────────────────────────────────────────────────────────────


# `tui_member` was LIFTED to `conftest.py` (2026-08-21) when the
# member-seat flip-back test needed the same seat — priority #2, one copy.
# It resolves automatically as a conftest fixture; nothing here changed.

# The member seat of every witness below — `media_member` (conftest), one arm
# per app, each reading through the `media-item-detail-download-button`
# download (tui's external-open handoff has its own witness,
# `test_tui_media_external_open.py`). Each arm
# carries its app mark, so the run's app axis selects it exactly as it selects
# the owner's param. The removal and second-share tests further down take the
# same seat: an app that binds no folder (web, iOS, android) cannot own a
# bound-folder witness, so the member seat IS its column for them.
#
# android: the seat launches (a launch-gate app, like apple), and the removal
# witness reads only the Media listing. The three witnesses that DOWNLOAD need
# a read-back seam android's driver does not have yet (`download_dir()`), and
# say so by failing at that assertion — never a skip.
_MEMBER_SEATS = [
    pytest.param("tui", marks=pytest.mark.tui),
    pytest.param("web", marks=pytest.mark.web),
    pytest.param("linux", marks=pytest.mark.linux),
    pytest.param("macos", marks=pytest.mark.macos),
    pytest.param("ios", marks=pytest.mark.ios),
    pytest.param("windows", marks=pytest.mark.windows),
    pytest.param("android", marks=pytest.mark.android),
]

# The owner seat of the first two witnesses. linux, where the suite began; macOS
# so the apple member arms have an owner on macOS, which has no linux app —
# `real_sync_agent` because the owner's bound folder is what seals the file and
# macOS spawns its agent only under that marker (the removal test's macOS
# owner carries the same pair). windows so the windows member arm has an owner
# on Windows, which has no linux app — with the two markers
# `test_windows_agent_uploads_bound_folder_file` documents: `real_sync_agent`
# turns the app's agent on, `isolated_sync_agent` keeps it on this run's own
# pipe and stitches its detached log into `app_stderr_text()` (the rotation
# test's re-key barrier reads it). Every owner × member pairing the run's app
# axis can drive is collected; the rest deselect.
_OWNER_SEATS = [
    "linux",
    pytest.param("macos", marks=[pytest.mark.macos, pytest.mark.real_sync_agent]),
    pytest.param(
        "windows",
        marks=[
            pytest.mark.windows,
            pytest.mark.real_sync_agent,
            pytest.mark.isolated_sync_agent,
        ],
    ),
]

# ── the test ─────────────────────────────────────────────────────────────


@pytest.mark.parametrize("folder_share_owner_app", _OWNER_SEATS, indirect=True)
@pytest.mark.parametrize("media_member", _MEMBER_SEATS, indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_member_decrypts_shared_set_content_through_their_own_media_page(
    request, folder_share_owner_app, media_member, tmp_path
):
    """A member reads a shared set's real content through their own client.

    The one assertion that cannot be faked: the bytes the member's download saves
    are the plaintext the owner's engine sealed under the set's M2 content key. Every
    link before it is checked separately so a failure names itself (``testing.md``
    § conventions point 6) — KeyPackage published, contact row set, share landed,
    engine uploaded, nest lists it for the member, client paints it.
    """
    owner_app, nest, _owner = folder_share_owner_app
    member_app, member = media_member.app, media_member.actor
    member_driver = member_app.driver

    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
    request.addfinalizer(lambda: print(_member_conv_diagnosis(member_app)))

    set_name, _owner_folder = _arrange_shared_set_with_file(
        owner_app, nest, member, tmp_path
    )

    # ── The nest must list it FOR THE MEMBER ─────────────────────────────
    _await_member_row(nest, member, set_name, _FILE_NAME)

    # ── The member's client paints it, then decrypts it ───────────────────
    _member_opens_and_reads(
        member_app, member_driver, _FILE_NAME, _FILE_BYTES
    )


def _engine_restart_count(owner_app, set_name: str) -> int:
    """How many times the owner's agent has restarted ``set_name``'s engine.

    ``bins/fauna-sync-agent/src/engine_driver.rs`` logs ``"engine key material,
    folder or mode changed; restarting engine"`` with the set name whenever the
    engine's key-material stamp changes. **Binding the folder already produces
    one** (unbound → keyed), so this must be counted, never merely matched: an
    existence check returns instantly on the bind-time line and provides no
    barrier at all against the rotation (found the first time this test ran).
    """
    try:
        text = owner_app.driver.app_stderr_text()
    except Exception:
        return 0
    return sum(
        1
        for line in text.splitlines()
        if "restarting engine" in line and set_name in line
    )


def _await_owner_engine_rekey(owner_app, set_name: str, baseline: int) -> None:
    """Barrier: wait until the owner's agent restarts the set's engine *again*,
    i.e. under the post-rotation content-key generation.

    A **causal barrier, not a settle-sleep** (``testing.md`` § conventions point
    14): content keys are final at engine build time, so the running engine keeps
    the pre-rotation generation until this restart lands, and anything it uploads
    before then is sealed under the OLD generation. ``baseline`` is the restart
    count taken immediately before the eviction gesture.
    """
    deadline = time.monotonic() + _REKEY_S
    while time.monotonic() < deadline:
        if _engine_restart_count(owner_app, set_name) > baseline:
            return
        time.sleep(2.0)
    pytest.fail(
        f"[owner] the agent never restarted {set_name!r}'s engine within "
        f"{_REKEY_S:.0f}s of the eviction (restart count stuck at {baseline}), so "
        "the engine still holds the pre-rotation content key and anything it "
        "uploads now would be sealed under the OLD generation. The rotation is "
        "owner-driven (`DataMessage::FolderContentKeyRotated` → "
        "`restart_engine_for_set`).\n" + agent_diagnosis(owner_app, "owner")
    )


def _content_key_versions(nest, actor, set_name: str) -> dict[str, int | None]:
    """``fauna.sync.changes.list`` → ``{basename: content_key_version}``.

    The vacuity guard's source. Without it this whole test is hollow: an owner
    engine that failed to re-key would seal the post-rotation file under
    generation 1, the member would open it with the custody they already had, and
    a green run would prove nothing about re-ingest.

    Keyed by ``path_hash`` matched against the two known file names, not the
    plaintext ``path`` wire field — the latter is the S9-flip scrub sentinel
    (empty string) for an ordinary set, same trap as ``_await_member_row``
    (found 2026-08-14 in the same pass: this returned ``{}`` unconditionally).
    """
    from common.auth import sync_changes_list

    reply = sync_changes_list(
        nest["port"],
        secret_key=actor["signing_key"].encode().hex(),
        folder=set_name,
        base_url=nest["url"],
    )
    known = {_path_hash(_FILE_NAME): _FILE_NAME, _path_hash(_FILE2_NAME): _FILE2_NAME}
    out: dict[str, int | None] = {}
    for change in reply.get("changes", []):
        name = known.get(change.get("path_hash"))
        if name:
            out[name] = change.get("content_key_version")
    return out


@pytest.mark.parametrize("folder_share_owner_app", _OWNER_SEATS, indirect=True)
@pytest.mark.parametrize("media_member", _MEMBER_SEATS, indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("share-a-folder")
def test_member_re_ingests_custody_across_owner_rotation_and_decrypts_gen2(
    request, folder_share_owner_app, media_member, tmp_path
):
    """A member re-ingests custody across an owner **rotation** and decrypts
    content sealed under the NEW generation — through their own client, with no
    app restart.

    The one carved-out remainder of the Phase-0 read leg. Proven at the Rust
    level by ``conformance_shared_folders.rs::remaining_member_advances_epoch_
    via_distributed_remove_commit_real_nest``; this runs it through the client
    chain a human uses (``ui/folders.md`` § Sharing → *Member custody-ingest
    read leg*; mechanism owner ``mls-group-key-material.md`` § M2).

    **Why a third actor.** The member's re-ingest fires on the conversations poll
    at ``applied >= 1`` (``libs/fauna-conversations/src/backends/fauna_mls.rs``
    → ``maybe_ingest_folder_custody``), so the rotation's trigger must be an
    **MLS commit the member applies**. A rotate-and-republish with no commit (the
    ``serve_disable`` shape) would silently not exercise the leg at all. Evicting
    a member *is* such a commit — so the set needs somebody expendable, and that
    is the third actor: a headless Python identity that publishes a real
    KeyPackage, is added by the owner's share (the 2026-07-23 add path), and is
    evicted without ever joining. It is a pure body in the group.

    **Why the evict runs through the owner's GUI.** A raw ``fauna.folders.
    members.evict`` would drop the roster row and rotate *nothing* — the MLS
    Remove, the re-key, the re-seal and the commit distribution all live in the
    owner's client engine (``FoldersAuthor::remove_member``). Convention 8's
    carve-out permits an API move for arranging a precondition, but there is no
    API that produces this precondition; the gesture must be the real one.
    """
    from tests.api import conv_api
    from common.auth import register_handled_actor

    owner_app, nest, _owner = folder_share_owner_app
    member_app, member = media_member.app, media_member.actor
    member_driver = member_app.driver

    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
    request.addfinalizer(lambda: print(_member_conv_diagnosis(member_app)))

    set_name, owner_folder = _arrange_shared_set_with_file(
        owner_app, nest, member, tmp_path
    )

    # ── Pre-rotation: the member is genuinely reading this set ────────────
    # Not the full download (that is the sibling test's job) — just enough
    # that a later failure is attributable to the rotation and not to a member
    # who never joined. The nest row proves the roster; the painted item proves
    # the client projection.
    _await_member_row(nest, member, set_name, _FILE_NAME)
    _await_member_item(member_app, member_driver, _FILE_NAME)
    # The known-good baseline the post-rotation probe is diffed against: the
    # same four reads, taken while the browser demonstrably paints.
    print(_member_client_probe(member_app, "pre-rotation (known good)"))

    # ── A third actor joins the set's group, then is evicted ──────────────
    third = register_handled_actor(
        nest["port"], handle="third" + secrets.token_hex(3), domain=MAIL_PRIMARY_DOMAIN
    )
    third_secret = third["signing_key"].encode()
    stored = conv_api.keypackage_upload(
        nest["port"], third, conv_api.mint_key_packages(third_secret, 2)
    )
    assert stored >= 1, (
        f"the third actor must publish a fetchable KeyPackage to be addable; "
        f"stored={stored}"
    )

    ob = owner_app.backups
    ob.navigate_folders()
    # `find_and_expand_folder` TOGGLES the expander, so calling it on the row the
    # bind step left open would CLOSE it and hide the Share… button. Re-open when
    # that happened — the same defence `bind_location_under_set` carries.
    row = ob.find_and_expand_folder(set_name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    assert ob.share_button_visible(), (
        "the owner's Share… button must be reachable to add the third actor; "
        f"error={owner_app.error_text()!r}"
    )
    ob.open_share_dialog()
    ob.share_recipient(handle=third["handle"], actor_id_hex=third["actor_id_hex"])

    deadline = time.monotonic() + _SHARE_ROSTER_S
    while time.monotonic() < deadline:
        if ob.shared_member_count() == 2:
            break
        time.sleep(0.5)
    assert ob.shared_member_count() == 2, (
        "the second share must ADD the third actor to the set's existing MLS "
        "group (the 2026-07-23 add path). Landing 1 means the add silently "
        "failed and the eviction below would evict the wrong actor — or, on a "
        "pre-fix build, that the share re-bound the set and dropped the member. "
        f"error={owner_app.error_text()!r}"
    )

    handles = [ob.member_handle(i) for i in range(2)]
    third_index = next(
        (i for i, h in enumerate(handles) if third["handle"] in h), None
    )
    assert third_index is not None, (
        f"the third actor {third['handle']!r} must be identifiable on the roster "
        f"before eviction (saw {handles!r}) — evicting by a guessed index could "
        "remove the member whose re-ingest is under test"
    )

    # Vacuity guard, half 1: generation 2 must not exist YET. Taken immediately
    # before the eviction so a green run proves the generation advanced *during*
    # this test rather than having started high.
    pre = _content_key_versions(nest, member, set_name)
    pre_max = max([v for v in pre.values() if v is not None], default=0)
    assert pre_max <= 1, (
        "before the eviction the set must be on generation 1 (or unstamped), or "
        f"the post-rotation assertion below proves nothing: {pre!r}"
    )

    restarts_before_evict = _engine_restart_count(owner_app, set_name)
    ob.remove_shared_member(index=third_index)
    deadline = time.monotonic() + _SHARE_ROSTER_S
    while time.monotonic() < deadline:
        if ob.shared_member_count() == 1:
            break
        time.sleep(0.5)
    assert ob.shared_member_count() == 1, (
        "the eviction must leave exactly the member on the roster; "
        f"error={owner_app.error_text()!r}"
    )
    assert member["handle"] in ob.member_handle(0), (
        "the surviving roster row must be the MEMBER — if the eviction removed "
        f"them instead, everything below is vacuous (saw {ob.member_handle(0)!r})"
    )

    # ── The owner's engine must pick up generation 2 before writing ───────
    _await_owner_engine_rekey(owner_app, set_name, restarts_before_evict)

    atomic_write(owner_folder / _FILE2_NAME, _FILE2_BYTES)
    await_agent_upload(owner_app, _FILE2_NAME, seat="owner")

    # ── Vacuity guard, half 2: the post-rotation file is on generation 2 ──
    # Deliberately NOT "and file1 is still on 1": the engine's pre-bind re-seal
    # pass (`mls-group-key-material.md` § M2 *Pre-bind re-seal migration*) runs on
    # every restart and re-seals unstamped files under the CURRENT generation, so
    # file1 legitimately reads 2 after the rotation. Pinning it to 1 asserts a
    # property the system does not have (this test's first run failed exactly
    # there). The sound pair is `pre_max <= 1` above + `file2 == 2` here.
    versions = _content_key_versions(nest, member, set_name)
    assert versions.get(_FILE2_NAME) == 2, (
        f"the post-rotation file must be sealed under generation 2, or the "
        f"member's decrypt below proves nothing about re-ingest: "
        f"{_FILE2_NAME}={versions.get(_FILE2_NAME)!r}. All rows: {versions!r} "
        f"(pre-eviction: {pre!r}). Still stamped 1 means the owner's engine "
        "uploaded before it re-keyed."
    )

    # ── The assertion: the member decrypts the POST-ROTATION file ─────────
    # The member's client has not restarted — same process, same session, since
    # the fixture launched it (a relaunch would have wiped its store and dropped
    # the login). So holding generation 2 can only have come from the re-ingest
    # its conversations poll drove when it applied the owner's Remove commit.
    assert member_driver.get_state("session.actor_id") == member["actor_id_hex"], (
        "the member's client must still be the same live session — this test "
        "asserts re-ingest WITHOUT an app restart"
    )
    _await_member_row(nest, member, set_name, _FILE2_NAME)
    # The sharpest instant in the whole test: the nest has *just* served this
    # actor's rows over a raw connection, so anything the client reports empty
    # here is the client's own projection. Probe both sides back to back.
    print(f"  [member] raw fauna.media.list served: {_member_media_items(nest, member)!r}")
    print(_member_client_probe(member_app, "post-rotation, nest just served rows"))

    # ── The member's client must have LOOKED, since the nest confirmed gen-2 ──
    # Convention 14 mechanism 3: everything the member needs (the Remove commit,
    # the rotated envelope) is nest-durable by this point (the vacuity guard
    # above already proved it), so what remained wall-clock-shaped was only the
    # member's OWN re-ingest (`poll_folder_feed` → `maybe_ingest_folder_custody`,
    # which every `full_sweep!` — ticked or poked — runs identically). Poking
    # and anchoring on a cycle that STARTED after `started` replaces the
    # fixture's shortened `FAUNA_CONV_POLL_SECS` tick (+ the open-and-retry loop
    # it justified) as the thing that makes this converge.
    cycles = conv_receive_cycles(member_driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(member_driver)
    await_receive_cycle_after(
        member_driver,
        started,
        budget_s=RECEIVE_CYCLE_S,
        what="the member's custody re-ingest of the owner's post-rotation envelope",
    )

    _member_opens_and_reads(
        member_app, member_driver, _FILE2_NAME, _FILE2_BYTES
    )


# ── the tui-owner pair: what a removal and a second share do to members ──────
#
# Both tests below run the owner on **tui** (the lead app) and on **linux**
# (which restarts the agent's engine after a rotation, `restart_engine_for_set`),
# plus the other bound owners as their columns land. The member — the removed
# person, the first person — is a `media_member` seat, one arm per app.


@pytest.mark.parametrize(
    "folder_share_owner_app",
    [
        "tui",
        "linux",
        # The macOS owner's remove gesture re-pushes the rotated bindings to
        # its agent (`APIClient.removeFolderMember`); the vacuity guard below
        # (the post-removal file sealed under generation 2) is what fails
        # without it. `real_sync_agent`: macOS spawns an agent only under it.
        pytest.param("macos", marks=[pytest.mark.macos, pytest.mark.real_sync_agent]),
        # The windows owner's remove gesture starts the shared retrying re-push
        # (`FoldersPage.TriggerRotationProvision`). windows spawns its agent
        # detached, so it needs both agent markers (the reason is
        # `test_folder_agent_content_sync.py::test_windows_agent_uploads_bound_folder_file`'s).
        pytest.param(
            "windows",
            marks=[
                pytest.mark.windows,
                pytest.mark.real_sync_agent,
                pytest.mark.isolated_sync_agent,
            ],
        ),
    ],
    indirect=True,
)
@pytest.mark.parametrize("media_member", _MEMBER_SEATS, indirect=True)
@pytest.mark.real_conversations
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_removed_member_cannot_open_what_is_added_afterwards(
    request, folder_share_owner_app, media_member, tmp_path
):
    """Someone you remove from a folder cannot open anything added to it
    afterwards (`ui/folders.md` § Sharing a folder (cross-user) — removal
    rotates the content key; mechanism owner `mls-group-key-material.md` § M2).

    The Rust pin is ``conformance_shared_folders.rs::removed_member_reads_pre_
    removal_but_fails_closed_post_removal_real_nest``; the rotation journey
    above proves the REMAINING member keeps reading. This is the removed one:
    their own app, still running, after the owner's app took them off.

    A second, headless member is shared first so the folder is still shared
    after the removal — the realistic case (someone leaves a group that goes
    on), and the one where "added afterwards" means content sealed under a key
    generation the removed person was never given.

    Three refusals, each on its own plane: the nest stops listing the folder's
    files to them (the read gate), the nest refuses them the key bundle it
    served them a moment before (the key plane), and their own Media browser —
    re-read after a receive cycle that began once the new file was durable —
    no longer shows the folder at all.
    """
    owner_app, nest, owner = folder_share_owner_app
    member_app, member = media_member.app, media_member.actor
    member_driver = member_app.driver
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
    request.addfinalizer(lambda: print(_member_conv_diagnosis(member_app)))

    set_name, owner_folder = _arrange_shared_set_with_file(
        owner_app, nest, member, tmp_path
    )

    # ── Before: the member is genuinely reading this folder ───────────────
    _await_member_row(nest, member, set_name, _FILE_NAME)
    _await_member_item(member_app, member_driver, _FILE_NAME)
    before = _content_key_get(nest, member, set_name)
    assert isinstance(before, dict) and before.get("sealed"), (
        "while on the roster the member must be served the folder's key bundle — "
        f"the baseline the refusal below is measured against; got {before!r}"
    )

    # ── A second member keeps the folder shared once the first is gone ────
    stays = _headless_member(nest, "stays")
    row = _share_through_owner_ui(owner_app, set_name, stays, want=2)

    # ── The owner removes the member, through the owner's own controls ────
    ob = owner_app.backups
    restarts_before = _engine_restart_count(owner_app, set_name)
    ob.remove_shared_member(_roster_index(ob, member["handle"], row=row), row=row)
    wait_until(
        lambda: ob.shared_member_count() == 1,
        _SHARE_ROSTER_S,
        diagnose=lambda: f"the removal never landed; error={owner_app.error_text()!r}",
    )
    assert stays["handle"] in ob.member_handle(0, row=row), (
        "the removal must leave exactly the other member on the roster — if it "
        f"removed them instead, nothing below is about the member (saw "
        f"{ob.member_handle(0, row=row)!r})"
    )

    # ── Something is added afterwards, under the rotated key ──────────────
    _await_owner_engine_rekey(owner_app, set_name, restarts_before)
    atomic_write(owner_folder / _FILE2_NAME, _FILE2_BYTES)
    await_agent_upload(owner_app, _FILE2_NAME, seat="owner")
    versions = _content_key_versions(nest, owner, set_name)
    assert versions.get(_FILE2_NAME) == 2, (
        "the file added after the removal must be sealed under the rotated "
        f"generation, or nothing below is about the removal: {versions!r}"
    )

    # ── The removed member cannot open it ─────────────────────────────────
    listed = {
        bytes(it.get("path_hash") or b"").hex() for it in _member_media_items(nest, member)
    }
    assert _path_hash(_FILE2_NAME) not in listed, (
        "the nest must not list a removed member the folder's new file"
    )
    after = _content_key_get(nest, member, set_name)
    assert not (isinstance(after, dict) and after.get("sealed")), (
        "the nest must refuse a removed member the folder's key bundle — without "
        f"it nothing added under the rotated key can be opened; got {after!r}"
    )

    # Their own app, still running, looks again and finds the folder gone.
    # Anchored on a receive cycle that began after the new file was durable
    # (convention 14 mechanism 3), then on the listing CHANGING — the earlier
    # file leaving the browser is the positive proof this read is fresh, so the
    # new file's absence from it is not a stale page.
    cycles = conv_receive_cycles(member_driver)
    poke_receive_cycle(member_driver)
    await_receive_cycle_after(
        member_driver,
        cycles[0] if cycles else None,
        budget_s=RECEIVE_CYCLE_S,
        what="the removed member's receive loop after the removal",
    )
    media = MediaActions(member_driver)

    def _fresh_listing():
        _reenter_media(member_driver)
        # Only a listing the page SAYS it loaded counts (rows, or the empty
        # state): a GUI app loads the re-entered page asynchronously, so the
        # blank before its read lands would otherwise pass for "the folder is
        # gone" — the very answer this waits for.
        if not media.wait_for_loaded():
            return None
        names = media.item_names()
        # Wrapped: an EMPTY listing is the expected answer, and a bare `[]`
        # would read as "not yet" to `wait_until`.
        return (names,) if _FILE_NAME not in names else None

    (names,) = wait_until(
        _fresh_listing,
        _UPLOAD_VISIBLE_S,
        interval=2.0,
        diagnose=lambda: (
            "[member] the removed member's Media browser still lists the folder's "
            f"files after the removal: {media.item_names()!r}\n"
            + _member_client_probe(member_app, "after removal")
        ),
    )
    assert _FILE2_NAME not in names, (
        f"[member] a removed member's app must not offer what was added afterwards: {names!r}"
    )


@pytest.mark.parametrize("folder_share_recipient_app", ["tui"], indirect=True)
@pytest.mark.parametrize("media_member", _MEMBER_SEATS, indirect=True)
@pytest.mark.parametrize(
    "folder_share_owner_app",
    [
        "tui",
        "linux",
        # windows as the owner (both agent markers: see the removal test above).
        pytest.param(
            "windows",
            marks=[
                pytest.mark.windows,
                pytest.mark.real_sync_agent,
                pytest.mark.isolated_sync_agent,
            ],
        ),
    ],
    indirect=True,
)
@pytest.mark.real_conversations
@pytest.mark.timeout(1800)
@pytest.mark.feature("share-a-folder")
def test_a_second_share_keeps_the_first_members_access_and_everything_readable(
    request, folder_share_owner_app, media_member, folder_share_recipient_app, tmp_path
):
    """Sharing a folder with a second person leaves the first person's access as
    it was, and everything already in the folder stays readable for all of them
    (`ui/folders.md` § Sharing → *Adding the 2nd..Nth member*: the newcomer is
    admitted to the set's EXISTING group, no rotation on admit, history on join).

    The regression this guards was real: until 2026-07-23 every share minted a
    fresh group and re-bound the set to it, silently dropping every earlier
    member. The rotation journey above asserts only the roster COUNT after its
    second share; this asserts the sentence, with a real app for each person:

    - the FIRST person (a reader, on whichever app holds the ``media_member``
      seat) opens the folder's file before the second share and again after it,
      in the same running app — their access as it was;
    - the SECOND person (a tui writer, the newcomer) binds a folder of their own
      and the file that was already there arrives on their disk, decrypted —
      history on join, the "everything already in the folder" half;
    - the owner's roster still shows the first person with the same status and
      role, and the file's key generation did not move (no rotation on admit,
      so nothing anyone already holds stopped opening it).

    Three seats: the owner, ``media_member`` (the first person, reading through
    their own app's Media gesture) and the recipient seat (the second person —
    tui, because their engine hydrates a bound folder as a writer).
    """
    from tests.api import conv_api
    from tests.test_folder_agent_content_sync import _await_file_content

    owner_app, nest, owner = folder_share_owner_app
    first_app, first = media_member.app, media_member.actor
    second_app, _nest, second = folder_share_recipient_app
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
    request.addfinalizer(lambda: print(agent_diagnosis(second_app, "second")))
    request.addfinalizer(lambda: print(_member_conv_diagnosis(first_app)))

    set_name, _owner_folder = _arrange_shared_set_with_file(
        owner_app, nest, first, tmp_path
    )

    # ── Before the second share: the first person opens the file ──────────
    _await_member_row(nest, first, set_name, _FILE_NAME)
    _member_opens_and_reads(
        first_app, first_app.driver, _FILE_NAME, _FILE_BYTES
    )
    ob = owner_app.backups
    ob.navigate_folders()
    row = ob.find_and_expand_folder(set_name)
    if not ob.share_button_visible():
        ob.expand_folder(row)
    i = _roster_index(ob, first["handle"], row=row)
    first_before = (ob.member_status(i, row=row), ob.member_access(i, row=row))
    generation_before = _content_key_versions(nest, owner, set_name).get(_FILE_NAME)

    # ── The second share, from the owner's own controls ───────────────────
    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], second, second["actor_id_hex"]) > 0,
        _KEYPACKAGE_S,
        diagnose=lambda: "the second person never published a KeyPackage to admit",
    )
    row = _share_through_owner_ui(owner_app, set_name, second, want=2)
    ob.set_member_access("writer", _roster_index(ob, second["handle"], row=row), row=row)
    wait_until(
        lambda: ob.member_access(_roster_index(ob, second["handle"], row=row), row=row)
        == "writer",
        15.0,
        diagnose=lambda: f"the writer grant never persisted; error={owner_app.error_text()!r}",
    )

    # ── The first person's access is as it was ────────────────────────────
    i = _roster_index(ob, first["handle"], row=row)
    first_after = (ob.member_status(i, row=row), ob.member_access(i, row=row))
    assert first_after == first_before, (
        f"the second share changed the first person's roster row: {first_before} → "
        f"{first_after}"
    )
    rostered = set(conv_api.folder_member_actors(nest["port"], owner, set_name))
    assert {first["actor_id_hex"], second["actor_id_hex"]} <= rostered, (
        f"both people must be on the folder's roster: {rostered!r}"
    )
    assert _content_key_versions(nest, owner, set_name).get(_FILE_NAME) == generation_before, (
        "admitting a second person must not re-key the folder (no rotation on admit)"
    )

    # ── Everything already there is readable by the newcomer ──────────────
    sb = second_app.backups
    sb.navigate_folders()
    assert sb.wait_for_pending_shares(1) == 1, (
        f"[second] the share should wait as one pending knock; error={second_app.error_text()!r}"
    )
    sb.accept_pending_share(0)

    def _second_row_listed():
        if any(set_name in sb.folder_title(k) for k in range(sb.folder_count())):
            return True
        sb.navigate_devices()
        sb.navigate_folders()
        return False

    wait_until(
        _second_row_listed,
        90.0,
        diagnose=lambda: f"[second] the folder never appeared; error={second_app.error_text()!r}",
    )
    second_folder = tmp_path / "second-bound"
    second_folder.mkdir()
    bind_location_under_set(second_app, set_name, second_folder, seat="second")
    _await_file_content(
        second_folder / _FILE_NAME,
        _FILE_BYTES.decode(),
        seat="second",
        app=second_app,
        window=360.0,
    )

    # ── …and still by the first person, in the same running app ───────────
    assert first_app.driver.get_state("session.actor_id") == first["actor_id_hex"], (
        "the first person's app must be the same live session it was before"
    )
    # The download clears its own stale file (`MediaActions.download_open_item`).
    _member_opens_and_reads(
        first_app, first_app.driver, _FILE_NAME, _FILE_BYTES
    )
