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
   persists the park on the binding — and the member's row must now SAY so.

Assertions, each on its own observable (convention 6): the warning on the
member's row; the agent's own `config.toml` marking the binding parked; the
nest never recording the second file; and both files still on the member's
disk, byte for byte.

Its first run found the app half missing on both apps: the agent parked the
binding, and the member's row — now reading `reader`, which is what a demotion
does — painted neither the warning nor the parked binding. The row asked the
access alone; it now asks `fauna_folders_machine::binding_section`, which keys
on the park too.
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
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    # tui leads; linux, whose member row carried the same access-only rule,
    # adopted the shared `binding_section` decision in the same change; macOS
    # and windows (each as the member, per-param below) followed. web, iOS and
    # android bind no local folder at all.
    pytest.mark.tui,
    pytest.mark.linux,
]

WARNING = "folder-access-revoked-warning"


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


def _member_row(b, set_name: str) -> int | None:
    for i in range(b.folder_count()):
        if set_name in b.folder_title(i):
            return i
    return None


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    [
        ("tui", "tui"),
        ("linux", "linux"),
        # macOS as the demoted MEMBER — the side this journey is about. A tui
        # owner does the demoting: apple carries no owner-side access-grant UI
        # (a declared absence, `ui/folders.md`), so a macOS owner cannot take
        # write access away. `real_sync_agent` because
        # macOS spawns its agent only under that marker.
        pytest.param(
            "tui", "macos", marks=[pytest.mark.macos, pytest.mark.real_sync_agent]
        ),
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
    from tests.api import conv_api

    owner_app, nest, owner = folder_share_owner_app
    member_app, _nest, member = folder_share_recipient_app
    request.addfinalizer(lambda: print(agent_diagnosis(member_app, "member")))
    request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))

    # The owner's share must fetch one of the member's KeyPackages; read the
    # non-destructive count so the probe does not consume it.
    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30.0,
        diagnose=lambda: "the member never published a KeyPackage the owner could admit",
    )

    # ── 1. Owner: create, share, make them a writer ────────────────────────
    set_name = f"revoke-{secrets.token_hex(4)}"
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

    # ── 2. Member: accept, bind, and a first file syncs ────────────────────
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

    # ── 3. Owner: take write access away ───────────────────────────────────
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
