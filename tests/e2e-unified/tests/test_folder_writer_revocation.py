"""tier_3 — a writer whose write access the owner takes away sees their app stop
syncing that folder and say so, and their own files stay exactly where they are
(`file-sync.md` § Multi-writer shared sets → *Revocation*; the member row's
`folder-access-revoked-warning`, ID user-approved 2026-07-22).

The mechanism has a Rust pin at every layer — the one-way `AccessGate`
(`fauna-sync-engine`), the agent's durable park (`fauna-sync-agent`'s
`park_revoked_set`), the two-nest capstone — and until this module no test
drove it through an app: nobody watched a member's app while the owner's app
demoted them. That is the sentence a user lives, so it is what runs here.

Two real apps of one kind — both tui (the lead app), or both linux — each with
its own real `fauna-sync-agent` in a private runtime dir, on one handled nest:

1. the OWNER creates a folder, shares it with the MEMBER and makes them a writer
   — every step through the owner's own controls;
2. the member accepts, binds a folder of their own, and a first file uploads —
   the baseline that makes "stops syncing" mean something (a member engine
   that never ran would also never upload the second file);
3. the owner takes write access away (the member row's role select);
4. the member adds a second file. A demoted writer meets the refusal on its
   first upload byte (the write-token mint), the engine parks, the agent
   persists the park on the binding — and the member's row must now SAY so;
5. the owner re-enters their Media page: the listing is judged afresh on every
   refresh, against the roster as it now stands — and the member's FIRST file,
   written while a writer, is still listed and attributed to the member.

Assertions, each on its own observable (convention 6): the warning on the
member's row; the agent's own `config.toml` marking the binding parked; the
nest never recording the second file; both files still on the member's disk,
byte for byte; the owner's Media listing still carrying the first file after
the demotion, its version row naming the member (with the same listing before
the demotion as the vacuity guard); and the member's own log carrying no
not-a-writer skip of its own earlier row.

Step 5's two checks are the one witness of the writer-of-record chain
(`writer-signed-change-records.md` ruling (12) — a writer's rows outlive the
grant; sub-ruling (f), the seats): admission is by writer of record, present
or former, never by the roster "now". Judged by the roster now, the owner's
listing dropped every file a demoted writer had added — a role change read as
silent data loss on every seat but the demoted one's disk.

Its first run found the app half missing on both apps: the agent parked the
binding, and the member's row — now reading `reader`, which is what a demotion
does — painted neither the warning nor the parked binding. The row asked the
access alone; it now asks `fauna_folders_machine::binding_section`, which keys
on the park too.

The web arm (`test_a_former_writers_files_stay_listed_and_open_for_a_web_owner`)
is step 5 alone, told from the owner's seat: web binds no folder, so it is never
the demoted member, but outcome 22's sentence is about the files staying for the
owner and the other members — the reader half, which runs on web. A web owner
demotes (one arm) or removes (the other) a tui writer, and its Media listing
must still carry the writer's file, attributed to them, and open it to the
writer's bytes. The iOS owner arm
(`test_a_former_writers_files_stay_listed_and_open_for_an_ios_owner`) is the
same story with the iOS app as the owner: iOS binds no local folder either, and
its owner reads the folder through the same shared Media listing.

The android arm (`test_a_demoted_writer_on_the_documents_provider_…`) is the
capability-host variant (`file-sync.md` § Multi-writer shared sets →
*Revocation on a capability host*): an android member binds nothing, but its
documents provider is a writing seat, so the member's writes go through the
provider the way another app's picker sends them, and the park is the host's
persisted mark rather than an agent's binding.

The iOS arm (`test_a_demoted_writer_on_the_ios_file_provider_domain_…`) is the
same variant on the File Provider domain. Its seat is the Debug `Fauna-iOS`
app (the `ios_file_provider_app` marker), which embeds the extension, so the
domain the test registers on the seat's own simulator through the DEBUG
`FileProviderTestCLI` verbs is the one the seat's app reads; the host's mark
is read straight off the simulator's app-group state DB.
"""

import secrets

import pytest

from helpers.folder_content import (
    agent_diagnosis,
    atomic_write,
    await_agent_upload,
    bind_location_under_set,
)
from helpers.inert_refusal import is_inert_refusal
from helpers.set_names import find_set
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; linux, whose member row carried the same access-only rule,
    # adopted the shared `binding_section` decision in the same change; macOS
    # and windows (each as the member, per-param below) followed. web, iOS and
    # android bind no local folder at all; android's documents provider and the
    # iOS File Provider domain are the capability-host arms at the foot of this
    # module, and web — which reads the folder through its Media listing — is
    # the OWNER of its own arm (the reader half of outcome 22), its claim the
    # param's own `web` mark; iOS is likewise the owner of an arm of its own,
    # its claim the param's `ios` mark. The module carries only `tui` (a seat of every
    # journey); `linux` sits on the first journey alone, so neither
    # capability-host arm ever reads as a linux witness.
    pytest.mark.tui,
]

WARNING = "folder-access-revoked-warning"

# The shared judge's refusal of a row whose signer it cannot place as a writer
# (`fauna_protocol::sync_writer_sig`'s `NotAWriter`) — the line the finding
# behind ruling (12) read on a demoted member's own seat.
NOT_A_WRITER = "signed actor is not a writer of this set"

# A ceiling for the owner's Media listing to paint a file the nest already
# holds — one nav-edge refresh plus its judge's roster read, on a loaded box.
OWNER_LISTING_S = 120.0


def _path_hash(path: str) -> str:
    """``fauna_core::sync::path_hash`` — plain BLAKE3 over the relative path; a
    sync-type set's ``changes.list`` serves no plaintext path (the S9 scrub)."""
    import blake3

    return blake3.blake3(path.encode()).hexdigest()


def _recorded_path_hashes(nest, owner, set_name: str) -> set[str]:
    """Every path the nest has recorded a change for in ``set_name``, read as
    the OWNER — the plane a demoted member's refused record never reached."""
    from common.auth import sync_changes_list

    reply = sync_changes_list(
        nest["port"],
        secret_key=owner["signing_key"].encode().hex(),
        folder=set_name,
        base_url=nest["url"],
    )
    return {c.get("path_hash") for c in reply.get("changes", [])}


def _recorded_path_hash_counts(nest, owner, set_name: str) -> dict[str, int]:
    """How many changes the nest has recorded per path in ``set_name`` — a
    document the provider created empty is recorded once, so a refused edit is
    one that adds none."""
    from collections import Counter

    from common.auth import sync_changes_list

    reply = sync_changes_list(
        nest["port"],
        secret_key=owner["signing_key"].encode().hex(),
        folder=set_name,
        base_url=nest["url"],
    )
    return dict(Counter(c.get("path_hash") for c in reply.get("changes", [])))


def _member_row(b, set_name: str) -> int | None:
    return b.folder_index(set_name)


def _await_member_keypackage(nest, member) -> None:
    """The owner's share must fetch one of the member's KeyPackages; read the
    non-destructive count so the probe does not consume it."""
    from tests.api import conv_api

    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30.0,
        diagnose=lambda: "the member never published a KeyPackage the owner could admit",
    )


def _owner_shares_as_writer(owner_app, member, set_name: str) -> None:
    """Owner: create the folder, share it with the member, make them a writer —
    every step through the owner's own controls."""
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)
    owner_row = ob.find_and_expand_folder(set_name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == 1,
        20.0,
        diagnose=lambda: f"the share never landed; error={owner_app.error_text()!r}",
    )
    ob.set_member_access("writer", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "writer",
        15.0,
        diagnose=lambda: f"the writer grant never persisted; error={owner_app.error_text()!r}",
    )


def _member_accepts(member_app, set_name: str) -> None:
    """Member: accept the knock and wait for the folder to list."""
    mb = member_app.backups
    mb.navigate_folders()
    assert mb.wait_for_pending_shares(1) == 1, (
        f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
    )
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; error={member_app.error_text()!r}"
    )

    def _row_listed():
        if _member_row(mb, set_name) is not None:
            return True
        mb.navigate_devices()
        mb.navigate_folders()
        return False

    wait_until(
        _row_listed,
        90.0,
        diagnose=lambda: (
            f"[member] the accepted folder never appeared in the list; "
            f"error={member_app.error_text()!r}"
        ),
    )


def _owner_demotes(owner_app, set_name: str) -> None:
    """Owner: take write access away through the member row's role select."""
    ob = owner_app.backups
    ob.navigate_folders()
    owner_row = ob.find_and_expand_folder(set_name)
    if ob.shared_member_count() == 0:
        ob.expand_folder(owner_row)  # the toggle closed a row left open
    ob.set_member_access("reader", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "reader",
        15.0,
        diagnose=lambda: f"the demotion never persisted; error={owner_app.error_text()!r}",
    )


def _owner_media_lists(owner_app, name: str, *, when: str) -> None:
    """Re-enter the owner's Media page until its judged listing carries
    ``name``. Every re-entry is one fresh ``fauna.media.list`` read judged
    whole by the shared judge against the roster read it makes then
    (``row_judge::judge_media_listing`` — a refused item is absent), so a
    listing read after the demotion IS the post-demotion judgement."""
    media = owner_app.media
    seen: list[list[str]] = [[]]

    def listed() -> bool:
        media.reenter()
        media.wait_for_loaded()
        seen[0] = media.item_names()
        return name in seen[0]

    wait_until(
        listed,
        OWNER_LISTING_S,
        interval=2.0,
        diagnose=lambda: (
            f"[owner] {when}, the owner's Media listing never carried the "
            f"member's {name!r} (saw {seen[0]!r}). The nest holds it (the "
            f"recorded-path assertion), so a miss here is the owner's judge "
            f"refusing the member's row — ruling (12): a former writer's rows "
            f"are admitted. error={owner_app.error_text()!r}\n"
            + agent_diagnosis(owner_app, "owner")
        ),
    )


def _owner_version_author(owner_app, name: str) -> str:
    """Open ``name``'s detail on the owner's Media page and read its version
    row's recorder (``file-version-author`` — the handle, else the full actor
    id hex: ``fauna_core::format::author_display_label``)."""
    media = owner_app.media
    media.open_item_detail(media.index_of(name))
    try:
        assert media.wait_for_version_count(1, timeout=OWNER_LISTING_S) >= 1, (
            f"[owner] {name}'s version history never rendered; "
            f"error={owner_app.error_text()!r}"
        )
        return media.version_author(0)
    finally:
        media.close_detail()


def _not_a_writer_lines(app) -> list[str]:
    """The seat's app+agent log lines carrying the judge's not-a-writer
    refusal — read for that one string, never paged."""
    try:
        text = app.driver.app_stderr_text()
    except Exception:
        return []
    return [ln for ln in text.splitlines() if NOT_A_WRITER in ln]


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [
        ("tui", "tui"),
        ("linux", "linux"),
        # macOS as the demoted MEMBER — the side this journey is about, a tui
        # owner doing the demoting. `real_sync_agent` because
        # macOS spawns its agent only under that marker.
        pytest.param(
            "tui", "macos", marks=[pytest.mark.macos, pytest.mark.real_sync_agent]
        ),
        # macOS as the demoting OWNER: its member row's role select grants and
        # then takes write access away from a tui member. The owner binds no
        # folder here, so it needs no agent.
        pytest.param("macos", "tui", marks=pytest.mark.macos),
        # windows as the demoted MEMBER, a tui owner again: two windows seats in
        # one run would share ONE agent, because the isolated agent's pipe and
        # data dir are session-scoped (`conftest.isolated_sync_agent_pipe_name`),
        # the same reason `test_windows_writer_member_decrypts_owner_upload`
        # pairs tui with windows. Both agent markers: windows spawns its agent
        # detached (`test_windows_agent_uploads_bound_folder_file` says why).
        pytest.param(
            "tui",
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
@pytest.mark.linux
@pytest.mark.real_conversations
# Two GUI apps + two real agents + a real MLS share/accept, then two engine
# cycles (the baseline upload and the refused one). A ceiling, not an
# expectation — the same budget the sibling two-agent capstone carries.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_demoted_writer_sees_the_folder_stop_syncing_and_keeps_their_files(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    from helpers.sync_agent_config import bound_sync_folder, describe_agent_config

    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app
    request.addfinalizer(lambda: print(agent_diagnosis(member_app, "member")))
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))

    _await_member_keypackage(nest, member)

    # ── 1. Owner: create, share, make them a writer ────────────────────────
    set_name = f"revoke-{secrets.token_hex(4)}"
    _owner_shares_as_writer(owner_app, member, set_name)

    # ── 2. Member: accept, bind, and a first file syncs ────────────────────
    mb = member_app.backups
    _member_accepts(member_app, set_name)

    member_folder = tmp_path / "member-bound"
    member_folder.mkdir()
    bind_location_under_set(member_app, set_name, member_folder, seat="member")

    first, second = (f"{stem}-{secrets.token_hex(3)}.txt" for stem in ("before", "after"))
    first_bytes = f"written while a writer — {secrets.token_hex(8)}\n"
    second_bytes = f"written after the demotion — {secrets.token_hex(8)}\n"
    atomic_write(member_folder / first, first_bytes)
    # The baseline: this member's engine really does sync this folder. Without
    # it, the second file's absence below could be an engine that never ran.
    await_agent_upload(member_app, first, seat="member")
    assert member_app.driver.is_absent(WARNING), (
        "[member] a writer whose grant stands must not be told it was taken away"
    )
    # The vacuity guard for step 5: while the member is a writer, the owner's
    # judged listing carries their first file — so its presence after the
    # demotion is the judge admitting a FORMER writer, not a listing that
    # never judged anything.
    _owner_media_lists(owner_app, first, when="before the demotion")

    # ── 3. Owner: take write access away ───────────────────────────────────
    _owner_demotes(owner_app, set_name)

    # ── 4. Member: the next change is refused, and the app says so ─────────
    atomic_write(member_folder / second, second_bytes)

    def _warning_shown():
        if member_app.driver.is_visible(WARNING):
            return True
        # The row paints its body only while expanded, and a list refresh can
        # collapse it; re-enter the page and re-open this folder's row.
        mb.navigate_devices()
        mb.navigate_folders()
        idx = _member_row(mb, set_name)
        if idx is not None and not member_app.driver.is_visible("folder-location-list"):
            try:
                mb.expand_folder(idx)
            except RuntimeError as exc:
                # Until the park reaches the app, the demoted member's row is a
                # plain reader row with no body to open: some agents refuse
                # that click, where the others no-op it. Not yet — poll again.
                if not is_inert_refusal(exc):
                    raise
                return False
        return member_app.driver.is_visible(WARNING)

    # apple has no `config_home`; its driver names the agent's root directly.
    config_home = getattr(member_app.driver, "config_home", None)
    agent_base = getattr(member_app.driver, "sync_agent_state_base", None)
    wait_until(
        _warning_shown,
        240.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the member's app never said the folder stopped syncing. "
            f"The agent's binding reads "
            f"{bound_sync_folder(config_home, str(member_folder), agent_base)!r} "
            f"(access_revoked true ⇒ the park happened and only the row failed to "
            f"say so; false ⇒ the refusal never parked the engine). "
            f"error={member_app.error_text()!r}\n"
            + describe_agent_config(config_home, agent_base)
            + "\n"
            + agent_diagnosis(member_app, "member")
        ),
    )
    assert member_app.driver.get_text(WARNING), "the warning must carry its words"

    # ── The app stopped syncing it ────────────────────────────────────────
    bound = bound_sync_folder(config_home, str(member_folder), agent_base)
    assert bound is not None and bound.get("access_revoked") is True, (
        f"the agent must persist the park on the binding, or a restart would resume "
        f"syncing a folder the nest refused: {bound!r}"
    )
    recorded = _recorded_path_hashes(nest, owner, set_name)
    assert _path_hash(first) in recorded, (
        "the baseline file must be on the nest (the vacuity guard for the next line)"
    )
    assert _path_hash(second) not in recorded, (
        "a change made after the demotion must never reach the folder"
    )

    # ── …and left the member's own files alone ────────────────────────────
    for name, content in ((first, first_bytes), (second, second_bytes)):
        path = member_folder / name
        assert path.exists() and path.read_text() == content, (
            f"[member] {name} must stay on disk exactly as written — a park never "
            f"touches local files (got {path.read_text() if path.exists() else 'MISSING'!r})"
        )

    # ── 5. A writer's rows outlive the grant (ruling (12)(f)) ─────────────
    # The member's row now reads `reader` on the owner's own controls and the
    # member's engine has parked on the refusal (both asserted above), so the
    # roster every reader reads from here on lists the member as a former
    # writer. The owner's listing, judged afresh, still carries the first file…
    _owner_media_lists(owner_app, first, when="after the demotion")
    author = _owner_version_author(owner_app, first)
    assert member["handle"] in author or member["actor_id_hex"] in author, (
        f"[owner] {first} must stay attributed to the member who wrote it — "
        f"a former writer's row is attributed to the former writer (ruling "
        f"(12)(d)); its version row reads {author!r}"
    )
    # …and the member's own seat never refused its own earlier row.
    refused = _not_a_writer_lines(member_app)
    assert not refused, (
        f"[member] the member's own seat refused a row it recorded while a "
        f"writer — a former writer's rows are admitted on its own pull side "
        f"too (ruling (12)(f)):\n" + "\n".join(f"    {ln}" for ln in refused[-10:])
    )


def _owner_opens_former_writers_file(owner_app, name: str, expected: str) -> str:
    """Open ``name``'s detail on the owner's Media page, read its version row's
    recorder, and download it: the saved bytes must be the member's plaintext.
    Returns the recorder label. The download is the shared
    ``MediaMachine::download_file`` walk (web's captured browser download), so
    produced plaintext is the owner's reader admitting the row AND opening it."""
    media = owner_app.media
    media.open_item_detail(media.index_of(name))
    try:
        assert media.wait_for_version_count(1, timeout=OWNER_LISTING_S) >= 1, (
            f"[owner] {name}'s version history never rendered; "
            f"error={owner_app.error_text()!r}"
        )
        author = media.version_author(0)
        got = media.download_open_item(timeout=OWNER_LISTING_S)
        assert got == expected.encode(), (
            f"[owner] {name} opened, but its bytes are not what the former writer "
            f"wrote ({len(got)} bytes vs {len(expected.encode())}); "
            f"error={owner_app.error_text()!r}"
        )
        return author
    finally:
        media.close_detail()


def _owner_removes(owner_app, set_name: str) -> None:
    """Owner: remove the member outright through the member row's remove control."""
    ob = owner_app.backups
    ob.navigate_folders()
    owner_row = ob.find_and_expand_folder(set_name)
    if ob.shared_member_count() == 0:
        ob.expand_folder(owner_row)  # the toggle closed a row left open
    ob.remove_shared_member(0, row=owner_row)
    wait_until(
        lambda: ob.shared_member_count() == 0,
        30.0,
        diagnose=lambda: f"the removal never landed; error={owner_app.error_text()!r}",
    )


def _former_writers_files_stay_for_the_owner(
    request, how, folder_share_owner_app, folder_share_recipient_app, tmp_path
) -> None:
    """Outcome 22's reader half, told from an owner seat that binds no folder:
    a tui writer adds a file, the owner takes write access away (``how`` —
    ``demote`` or ``remove``), and the owner's Media listing must still carry
    the file, attributed to the former writer, and open it to their bytes."""
    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app
    request.addfinalizer(lambda: print(agent_diagnosis(member_app, "member")))

    _await_member_keypackage(nest, member)

    # ── 1. Owner: create, share, make them a writer ───────────────────────
    set_name = f"revoke-{secrets.token_hex(4)}"
    _owner_shares_as_writer(owner_app, member, set_name)

    # ── 2. Member (tui): accept, bind, and a file syncs while a writer ────
    _member_accepts(member_app, set_name)
    member_folder = tmp_path / "member-bound"
    member_folder.mkdir()
    bind_location_under_set(member_app, set_name, member_folder, seat="member")
    name = f"before-{secrets.token_hex(3)}.txt"
    content = f"written while a writer — {secrets.token_hex(8)}\n"
    atomic_write(member_folder / name, content)
    await_agent_upload(member_app, name, seat="member")
    assert _path_hash(name) in _recorded_path_hashes(nest, owner, set_name), (
        "the member's file must be on the nest before write access goes"
    )
    # The vacuity guard: while the member is a writer, the owner's judged
    # listing carries the file — so its presence afterwards is the reader
    # admitting a FORMER writer, not a listing that never judged anything.
    _owner_media_lists(owner_app, name, when="before write access went")

    # ── 3. Owner: take write access away ──────────────────────────────────
    if how == "demote":
        _owner_demotes(owner_app, set_name)
    else:
        _owner_removes(owner_app, set_name)

    # ── 4. The file stays for the owner: listed, attributed, and opens ────
    _owner_media_lists(owner_app, name, when=f"after the {how}")
    author = _owner_opens_former_writers_file(owner_app, name, content)
    assert member["handle"] in author or member["actor_id_hex"] in author, (
        f"[owner] {name} must stay attributed to the member who wrote it — a "
        f"former writer's row is attributed to the former writer (ruling "
        f"(12)(d)); its version row reads {author!r}"
    )


# The reader half of outcome 22 on web. web binds no folder, so it can never be
# the demoted member's seat the journey above is told from; but the outcome's
# sentence is about the files staying for the OWNER and the other members, and a
# web owner reads the folder through its Media listing (`ui/folders.md` § Sharing
# a folder (cross-user) — no location binding on web). So web is the owner here,
# a tui writer — the seat that binds — writes the file, and the two arms are the
# two ways write access goes: a demotion to reader, and a removal. This is its
# own function so the module's `tui` mark never stands in for web: the web claim
# is the param's own mark.
@pytest.mark.parametrize("how", ["demote", "remove"])
@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [pytest.param("web", "tui", marks=pytest.mark.web)],
    indirect=True,
)
@pytest.mark.real_conversations
# A web owner + a tui member with a real agent, an MLS share/accept and one
# engine upload. A ceiling, not an expectation.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_former_writers_files_stay_listed_and_open_for_a_web_owner(
    request, how, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    _former_writers_files_stay_for_the_owner(
        request, how, folder_share_owner_app, folder_share_recipient_app, tmp_path
    )


# The same reader half with the iOS app as the owner — the seat for whom the
# files stay. iOS binds no local folder either (its member presence is the File
# Provider arm at the foot of this module, which is outcome 12's), but its owner
# reads the folder through the same shared Media listing and judge. Its own
# function for the reason the web arm is: the ios claim is the param's own mark,
# never the module's `tui`.
@pytest.mark.parametrize("how", ["demote", "remove"])
@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [pytest.param("ios", "tui", marks=pytest.mark.ios)],
    indirect=True,
)
@pytest.mark.real_conversations
# An iOS owner + a tui member with a real agent, an MLS share/accept and one
# engine upload. A ceiling, not an expectation.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_former_writers_files_stay_listed_and_open_for_an_ios_owner(
    request, how, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    _former_writers_files_stay_for_the_owner(
        request, how, folder_share_owner_app, folder_share_recipient_app, tmp_path
    )


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [pytest.param("tui", "android", marks=pytest.mark.android)],
    indirect=True,
)
@pytest.mark.real_conversations
# Two apps + a real MLS share/accept, then two provider writes (the baseline
# and the refused one). A ceiling, not an expectation.
@pytest.mark.timeout(1500)
@pytest.mark.feature("share-a-folder")
def test_a_demoted_writer_on_the_documents_provider_is_told_and_keeps_the_unsent_edit(
    request, folder_share_owner_app, folder_share_recipient_app
):
    """The android member's only presence is the documents provider. Their
    second edit is one they had OPEN when the owner took write access away —
    opened while the provider still advertised write, closed after — so its
    close meets the refusal: the host persists its mark and keeps the body in
    its kept root. Asserted, each on its own observable: the warning on the
    member's row; the nest never recording the edit; the provider still
    serving the edit byte for byte; and the host's mark."""
    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
    drv = member_app.driver

    _await_member_keypackage(nest, member)
    set_name = f"revoke-{secrets.token_hex(4)}"
    _owner_shares_as_writer(owner_app, member, set_name)
    _member_accepts(member_app, set_name)

    # ── The set in the provider's root, as a picker lists it ──────────────
    found: dict = {}

    def _set_presented():
        for child in drv.documents_children():
            if set_name in child.get("name", ""):
                found.update(child)
                return True
        return False

    wait_until(
        _set_presented,
        90.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the provider never presented {set_name!r}; root lists "
            f"{drv.documents_children()!r}"
        ),
    )
    set_doc = found["id"]

    # ── Baseline: a writer's provider write reaches the nest ──────────────
    first, second = (f"{stem}-{secrets.token_hex(3)}.txt" for stem in ("before", "after"))
    first_bytes = f"written while a writer — {secrets.token_hex(8)}\n".encode()
    second_bytes = f"written after the demotion — {secrets.token_hex(8)}\n".encode()
    first_doc = drv.documents_create(set_doc, first)
    drv.documents_write(first_doc, first_bytes)
    # The second document exists (created empty) while a writer; its edit is
    # opened below while the plan still advertises write.
    second_doc = drv.documents_create(set_doc, second)
    wait_until(
        lambda: {_path_hash(first), _path_hash(second)}
        <= set(_recorded_path_hash_counts(nest, owner, set_name)),
        120.0,
        interval=2.0,
        diagnose=lambda: (
            f"the baseline never reached the nest through the provider; recorded "
            f"{_recorded_path_hash_counts(nest, owner, set_name)!r}; "
            f"parks {drv.documents_parks()!r}"
        ),
    )
    handle = drv.documents_open_write(second_doc)
    before = _recorded_path_hash_counts(nest, owner, set_name)
    mb = member_app.backups
    mb.navigate_folders()
    assert drv.is_absent(WARNING), (
        "[member] a writer whose grant stands must not be told it was taken away"
    )

    # ── The owner takes write access away; the open edit is closed after ──
    _owner_demotes(owner_app, set_name)
    drv.documents_write_close(handle, second_bytes)

    def _warning_shown():
        if drv.is_visible(WARNING):
            return True
        # The fold runs on the folder-list refresh: re-enter the page.
        mb.navigate_devices()
        mb.navigate_folders()
        return drv.is_visible(WARNING)

    wait_until(
        _warning_shown,
        240.0,
        interval=2.0,
        diagnose=lambda: (
            f"[member] the member's row never said the folder stopped syncing. "
            f"The provider's parks read {drv.documents_parks()!r} (access_revoked "
            f"true ⇒ the host parked and only the row failed to say so). "
            f"error={member_app.error_text()!r}"
        ),
    )
    assert drv.get_text(WARNING), "the warning must carry its words"

    # ── The host's mark ───────────────────────────────────────────────────
    parks = [p for p in drv.documents_parks() if set_name in p.get("name", "")]
    assert parks and parks[0].get("access_revoked") is True, (
        f"the host must persist its park mark on the set's state DB, or an "
        f"unloaded provider would forget it: {parks!r}"
    )

    # ── The nest never recorded the edit ──────────────────────────────────
    after = _recorded_path_hash_counts(nest, owner, set_name)
    assert after.get(_path_hash(first)), (
        "the baseline file must be on the nest (the vacuity guard for the next line)"
    )
    assert after.get(_path_hash(second)) == before.get(_path_hash(second)), (
        f"an edit closed after the demotion must never reach the folder "
        f"(recorded {before.get(_path_hash(second))} → {after.get(_path_hash(second))})"
    )

    # ── …and the provider still serves what the member saved ──────────────
    served = drv.documents_read(second_doc)
    assert served == second_bytes, (
        f"[member] the picker must show what the member saved — the kept body "
        f"before the cache (got {served!r})"
    )


def _ios_group_sync_dir(udid: str):
    """`<app-group>/sync` on the member's simulator — the container base the
    extension's per-set state DBs live under (`SyncStateDir.containerSyncDir`),
    read from the host the way the eviction witness reads the replica."""
    from pathlib import Path

    from tests.platform.ios.test_file_provider_eviction import _BUNDLE_ID, _sim

    listed = _sim("get_app_container", udid, _BUNDLE_ID, "groups").stdout
    for entry in listed.splitlines():
        group = Path(entry.split(None, 1)[-1].strip())
        if (group / "sync").is_dir():
            return group / "sync"
    return None


def _ios_host_mark(udid: str, actor_hex: str, folder_id: int) -> int | None:
    """The host's park mark on the set's own state DB — `meta.access_revoked_at`
    in `<app-group>/sync/<actor>/fsid-local-<id>.db` (`SyncDb::access_revoked_at`,
    `FolderRef::state_db_path`), opened read-only so the extension's WAL is
    never written. ``None`` while the DB or the mark is not there."""
    import sqlite3

    base = _ios_group_sync_dir(udid)
    if base is None:
        return None
    db = base / actor_hex.lower() / f"fsid-local-{folder_id}.db"
    if not db.exists():
        return None
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=5)
    try:
        row = conn.execute("SELECT value FROM meta WHERE key = 'access_revoked_at'").fetchone()
    except sqlite3.Error:
        return None
    finally:
        conn.close()
    return int(row[0]) if row else None


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [
        pytest.param(
            "tui", "ios", marks=[pytest.mark.ios, pytest.mark.ios_file_provider_app]
        )
    ],
    indirect=True,
)
@pytest.mark.real_conversations
# Two apps + a real MLS share/accept, the File Provider app's CLI launches on the
# member's simulator, two domain writes and a relaunch. A ceiling, not an
# expectation.
@pytest.mark.timeout(2400)
@pytest.mark.feature("share-a-folder")
def test_a_demoted_writer_on_the_ios_file_provider_domain_is_told_and_keeps_the_unsent_file(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """The iOS member's only presence is the File Provider domain. The seat is
    the Debug `Fauna-iOS` app (`ios_file_provider_app`), so the domain the
    test registers on the seat's own simulator is the one the seat's app reads.
    The member's writes go through the domain the way another app's coordinated
    write does (the DEBUG `FileProviderTestCLI` `write` verb, a launch of the
    same app with no e2e environment): a first while a writer, which the nest
    records, and a second after the owner took write access away, which the
    host refuses at its pre-seal edge and marks. Asserted, each on its own
    observable: the host's mark on the set's state DB; the nest never recording
    the second file; the domain still serving it byte for byte; and the
    warning on the member's row once the seat's app comes back."""
    import hashlib

    import blake3
    import cbor2

    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from tests.platform.ios.test_file_provider_eviction import (
        _BACKUP_KEY_CONTEXT,
        _extension_diagnostics,
        _fauna,
        _keep_logs,
        _register,
    )

    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
    drv = member_app.driver
    udid = drv._launch_config["udid"]
    url = nest["url"]
    member_seed = member["signing_key"].encode()
    member_hex = member["actor_id_hex"]

    _await_member_keypackage(nest, member)
    # The extension keys the account plane's folder custody as the enrolled
    # machine it is a process of — enrolled by the seat before its first
    # sign-in (`ios_file_provider_app`, `conftest._folder_share_recipient_seat`).
    writer_secret, device_authorization = member["fp_principal"]

    set_name = f"revoke-{secrets.token_hex(4)}"
    _owner_shares_as_writer(owner_app, member, set_name)
    _member_accepts(member_app, set_name)
    # Every CLI verb below is a launch of the same bundle, which ends the
    # seat's process: keep its container and keychain for the relaunch.
    assert drv.preserve_state_across_relaunch(), "the iOS seat must pin its state"

    # A same-nest member keys the set as the nest's own row, `local:<id>`
    # (`folderRefForRow`); `fauna.folders.list` is the owner's table, so the id
    # is read there, by the set's name hash (a sealed set rests no plaintext).
    owner_seed = owner["signing_key"].encode()
    with WsRpcAdminClient(
        url, actor_id=bytes.fromhex(owner["actor_id_hex"]), signing_key=owner_seed
    ) as ws:
        rows = ws.call("fauna.folders.list", {})["folders"]
    row = find_set(rows, set_name)
    assert row is not None, f"[owner] the shared set is not listed: {rows!r}"
    folder_id = int(row["id"])

    device_id = secrets.token_bytes(32)
    with WsRpcAdminClient(url, actor_id=bytes.fromhex(member_hex), signing_key=member_seed) as ws:
        ws.call(
            "fauna.sync.register",
            {"device_id": device_id.hex(), "label": "fp-ext", "capabilities": "read,write"},
        )
        # The extension's records go by reference, so its signer must be live
        # on a row — this device's own, as an enrolled machine's is.
        ws.call(
            "fauna.sync.device_grant.register",
            {"device_id": device_id.hex(), "authorization": cbor2.loads(device_authorization)},
        )
    folder_ref = f"local:{folder_id}"
    backup_key = blake3.blake3(member_seed, derive_key_context=_BACKUP_KEY_CONTEXT).digest()

    assert "provision OK" in _fauna(
        udid, "provision", url, member_hex, device_id.hex(), "fp-ext",
        backup_key.hex(), member["token"], writer_secret.hex(), device_authorization.hex(),
    )
    _register(udid, folder_ref, set_name)
    try:
        first, second = (f"{stem}-{secrets.token_hex(3)}.txt" for stem in ("before", "after"))
        first_text = f"written while a writer {secrets.token_hex(8)}"
        second_text = f"written after the demotion {secrets.token_hex(8)}"

        # ── Baseline: a writer's write through the domain reaches the nest ──
        # A host that cannot key the set's custody yet refuses and rebuilds at
        # its next request (`engine_lifecycle`), so that one answer is waited
        # out; any other is the verb's result.
        outs: list[str] = []

        def _baseline_written() -> bool:
            outs.append(_fauna(udid, "write", folder_ref, set_name, first, first_text))
            return "binding indeterminate" not in outs[-1]

        wait_until(
            _baseline_written, 180.0, interval=5.0,
            diagnose=lambda: f"{outs[-1]}\n{_extension_diagnostics(udid)}",
        )
        assert f"write OK: {first}" in outs[-1], (
            f"[member] the baseline write failed: {outs[-1]}\n{_extension_diagnostics(udid)}"
        )
        wait_until(
            lambda: _path_hash(first) in _recorded_path_hashes(nest, owner, set_name),
            120.0,
            interval=2.0,
            diagnose=lambda: (
                f"the baseline never reached the nest through the domain; recorded "
                f"{_recorded_path_hashes(nest, owner, set_name)!r}\n"
                f"{_extension_diagnostics(udid)}"
            ),
        )
        assert _ios_host_mark(udid, member_hex, folder_id) is None, (
            "[member] a writer whose grant stands must carry no park mark"
        )

        # ── The owner takes write access away; the member saves again ──────
        _owner_demotes(owner_app, set_name)
        out = _fauna(udid, "write", folder_ref, set_name, second, second_text)
        assert f"write OK: {second}" in out, (
            f"[member] the domain refused the save itself — the root no longer "
            f"advertised write when it was made: {out}\n{_extension_diagnostics(udid)}"
        )

        # ── The host's mark ────────────────────────────────────────────────
        wait_until(
            lambda: _ios_host_mark(udid, member_hex, folder_id) is not None,
            180.0,
            interval=2.0,
            diagnose=lambda: (
                f"the host never persisted its park mark on "
                f"{_ios_group_sync_dir(udid)}/{member_hex}/fsid-local-{folder_id}.db; "
                f"the nest records {_recorded_path_hashes(nest, owner, set_name)!r} "
                f"(second={_path_hash(second)})\n"
                f"{_extension_diagnostics(udid)}"
            ),
        )

        # ── The nest never recorded the second file ────────────────────────
        recorded = _recorded_path_hashes(nest, owner, set_name)
        assert _path_hash(first) in recorded, (
            "the baseline file must be on the nest (the vacuity guard for the next line)"
        )
        assert _path_hash(second) not in recorded, (
            "a file saved after the demotion must never reach the folder"
        )

        # ── …and the domain still serves what the member saved ─────────────
        out = _fauna(udid, "read", folder_ref, set_name, second)
        digest = hashlib.sha256(second_text.encode()).hexdigest()
        assert f"read OK: {second}" in out and digest in out, (
            f"[member] Files must show what the member saved — the kept body "
            f"(got {out})\n{_extension_diagnostics(udid)}"
        )

        # ── The seat's app comes back and says so ──────────────────────────
        drv.hard_reload()
        mb = member_app.backups

        def _warning_shown():
            if drv.is_visible(WARNING):
                return True
            # The park is read on the folder-list refresh: re-enter the page
            # and re-open this folder's row, whose body the warning heads.
            mb.navigate_devices()
            mb.navigate_folders()
            idx = _member_row(mb, set_name)
            if idx is not None and drv.is_absent("folder-on-demand-toggle"):
                try:
                    mb.expand_folder(idx)
                except RuntimeError as exc:
                    if not is_inert_refusal(exc):
                        raise
                    return False
            return drv.is_visible(WARNING)

        wait_until(
            _warning_shown,
            240.0,
            interval=2.0,
            diagnose=lambda: (
                f"[member] the member's row never said the folder stopped syncing; "
                f"the host's mark reads {_ios_host_mark(udid, member_hex, folder_id)!r} "
                f"(set ⇒ the host parked and only the app failed to read it). "
                f"error={member_app.error_text()!r}"
            ),
        )
        assert drv.get_text(WARNING), "the warning must carry its words"
    finally:
        # What the OS and the extension said, kept past the session's device.
        _keep_logs(udid, tmp_path)
        _fauna(udid, "remove", folder_ref, set_name)
        _fauna(udid, "revoke")
