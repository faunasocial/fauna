"""tier_3 e2e: the **bound flip-back**, driven through the app UI — a
group-bound folder is published, and then *left* again, and its corpus follows.

The finding's two halves landed:

- **half 1** — ``fauna_folders_machine::audience_options(bound, current)`` offers
  ``shared`` as *selectable* exactly when ``bound && current == "public"``, so a
  bound folder's public window has a door out at all;
- **half 2** — ``SyncEngine::converge_corpus_to_audience``'s sealed branch
  dispatches on bound-ness, re-sealing a bound engine's public-window plaintext
  via ``reseal_pending_under_current`` instead of no-op'ing in the owner-only
  pass while stamping the corpus "sealed" anyway.

Both are pinned where they live — the option set in ``fauna-folders-machine``'s
unit tests, the engine branch in ``download_file_bytes_test.rs::converge_corpus_
reseals_a_bound_folder_after_a_public_window``. **Neither pin runs the chain a
human walks**, which is the whole lesson: half 1's doc comment justified
itself by pointing at half 2 and half 2's pointed back at half 1, and each was
locally correct. Only the journey shows whether picking ``shared`` in a real
picker actually re-seals real bytes on a real nest.

Flow (``ui/folders.md`` § Audience and website serving; behavior authority
``behavior/folders.md`` § Target re-model):

1. the owner creates a ``sync`` folder and **shares** it — the gesture that mints
   the MLS group, the genesis content key and the sealed envelope, i.e. the only
   way a folder becomes *bound*;
2. a location is bound under it and a file dropped in, so the engine seals real
   chunks under content-key generation 1 — the **back-catalogue** the flip-back
   is about. Binding after the share is load-bearing: an engine built before it
   is unbound and would seal under the owner's own ``BackupKey`` instead;
3. the owner picks ``public`` and answers ``folder-audience-public-confirm``, and
   the declassify pass unseals that back-catalogue;
4. the owner picks ``shared`` — **the affordance half 1 exists to offer** — and
   the corpus re-seals under the folder's current content key.

**The nest's own bytes are the assertion.** Every check reads ground truth over
``fauna.sync.changes.list`` + a raw ``GET /api/v1/manifests/<hash>``: a head
manifest carrying ``stored_hashes`` is AEAD ciphertext addressed by ciphertext
hash, one carrying none is plaintext anyone holding the hash can read. Device-local
state would prove nothing here — the defect this pins recorded local success while
the nest kept plaintext.

Step 3 is also the **vacuity guard**. Without it a run where the engine simply
never converged in either direction would sail through step 4's re-seal assertion
on bytes that were sealed the whole time, and prove nothing at all.

MUTATION is UI-driven throughout (convention 8): the audience is *picked* and the
confirm *clicked*, never a raw ``folders.update``. The wizard, the share and the
location binding are fixture setup (the documented carve-out), and VERIFICATION is
black-box nest reads — the idiom ``test_folder_audience_control.py`` and
``test_public_website_folder_serve.py`` share.

**One app, not two.** The owner seat is ``tui``, which since 2026-07-30 carries
both surfaces this journey needs on one page — ``folder-share-button`` over the
reused recipient picker *and* ``folder-audience-select`` — and direct-spawns the
real ``fauna-sync-agent`` under e2e, so its engine is a genuine one. The recipient
is seeded headlessly: bound-ness is minted by the owner's share gesture and does
not wait on anyone accepting, so no second app is owed for *this* leg.

**The member seat is the second test, and it asserts the opposite direction.** A
member seat with a bound location never follows the owner's declassification
(``encryption-at-rest.md`` § Implementation status today, ruled 2026-10-04): its
engine holds no trusted owner for the folder, so what it adds rests sealed through
the public window and after. That takes a second real seat (``tui_member``).
"""

from __future__ import annotations

import secrets
import urllib.error
import urllib.request

import cbor2
import pytest

from helpers.folder_content import (
    agent_diagnosis,
    atomic_write,
    await_agent_log,
    await_agent_upload,
    bind_location_under_set,
    log_folder_name,
)
from conftest import _E2E_SHARE_RECIPIENT_DEVICE_ID
from helpers.waiting import wait_until

# App markers sit per-test, not module-wide: the owner-only journey runs on
# every app that carries the audience controls + a real sync agent (tui; linux
# joined the slice-4d controls 2026-08-27; macos joined 2026-09-02, row 283;
# windows joined 2026-09-07, row 168), while the member-seat test stays
# tui-only — its second seat is the tui-specific `tui_member` fixture, and a
# module-wide widening would let the scan credit another app with a
# member-seat witness that never runs there.
pytestmark = [pytest.mark.tier_3]

# Generous ceilings, not expectations (convention 14): every wait below is a
# deadline poll on STATE and none of them is a settle-sleep. The corpus
# convergences are the slow ones — they ride the engine's own catch-up cadence,
# so budget several ticks rather than one.
_ROSTER_S = 30.0
_FLAG_S = 30.0
_UI_S = 15.0
_CONVERGE_S = 240.0

_PAGE = "page.html"
_BODY = b"<!doctype html><title>bound flip-back</title><h1>bound flip-back</h1>\n"


def _changes(nest, actor, folder: str) -> list[dict]:
    """The folder's recorded head rows, straight off the nest."""
    from common.auth import sync_changes_list

    reply = sync_changes_list(
        nest["port"],
        secret_key=actor["signing_key"].encode().hex(),
        folder=folder,
        base_url=nest["url"],
    )
    return reply.get("changes", [])


def _head(nest, actor, folder: str, path: str) -> dict | None:
    """The newest recorded row for ``path``.

    Keyed by ``path_hash``, never the plaintext ``path`` wire field: that field
    is the S9 scrub sentinel (empty string) for a sealed sync folder, so a
    plaintext match silently never fires (`file-sync.md` § Sealed names & paths;
    the same trap `test_folder_member_media_decrypt.py` documents).

    ⚠ Rows accumulate — the declassify and the re-seal each RECORD a new
    version of the same path — so the head is the highest ``seq``, not the
    first match.
    """
    import blake3

    want = blake3.blake3(path.encode()).hexdigest()
    rows = [c for c in _changes(nest, actor, folder) if c.get("path_hash") == want]
    return max(rows, key=lambda c: c["seq"]) if rows else None


def _manifest(nest, actor, manifest_hash: str) -> dict:
    """``GET /api/v1/manifests/<hex>`` under the owner's bearer, decoded.

    Fetched over the raw bulk-binary plane rather than asked of a client helper:
    what rests on the nest is exactly the question, and a helper that opened it
    with the owner's keys would answer a different one.
    """
    req = urllib.request.Request(
        f"{nest['url'].rstrip('/')}/api/v1/manifests/{manifest_hash}",
        headers={"Authorization": f"Bearer {actor['token']}"},
        method="GET",
    )
    with urllib.request.urlopen(req) as resp:
        return cbor2.loads(resp.read())


def _sealed_state(nest, actor, folder: str, path: str) -> tuple[str, dict | None]:
    """``("sealed" | "plaintext" | "absent", head_row)`` for ``path``.

    ``stored_hashes`` is the discriminator the wire itself defines: present means
    the chunks are AEAD ciphertext addressed by ciphertext hash — unreadable
    without the folder's content key — absent means plaintext addressed by its
    own hash, readable by anyone holding it, which is precisely what the public
    audience rests (``principles.md`` § The user always controls their data owns
    that one exception).
    """
    head = _head(nest, actor, folder, path)
    if head is None or not head.get("manifest_hash"):
        return "absent", head
    manifest = _manifest(nest, actor, head["manifest_hash"])
    return ("sealed" if manifest.get("stored_hashes") is not None else "plaintext"), head


def _await_sealed_state(
    nest,
    actor,
    folder: str,
    want: str,
    *,
    why: str,
    path: str = _PAGE,
    extra_diagnosis=None,
    require_content_key: bool = False,
) -> dict:
    """Deadline-poll the corpus until ``path``'s bytes on the nest reach ``want``.

    ``extra_diagnosis`` is a zero-arg callable appended to the failure message —
    the seat's own agent stderr, so a convergence that never ran is
    distinguishable from one that ran and refused (convention 6). Called only on
    timeout.

    ``require_content_key`` additionally waits for the head to carry an M2
    ``content_key_version``, i.e. for the entry to be sealed under the FOLDER's
    content key rather than the owner's own ``BackupKey``. That is a state wait,
    not a retry, and it closes a real race (measured 2026-08-21, after two runs
    had passed on luck): binding a location immediately after the share can build
    the engine before the share's content keys reach the agent, and such an
    engine seals the first upload under ``BackupKey`` with no generation
    (``path_sealed`` carries ``gen: null``). Nothing is lost when that happens —
    the keys' arrival restarts the engine and the pre-bind M2 pass
    (``reseal_pending_under_current``) re-seals the entry under generation 1 — so
    the honest wait is for the converged state rather than an assertion on
    whichever ordering won (convention 14).
    """

    def _matches():
        state, head = _sealed_state(nest, actor, folder, path)
        if state != want or head is None:
            return None
        if require_content_key and head.get("content_key_version") is None:
            return None
        return head

    def _diagnose() -> str:
        msg = (
            f"{why}: {path!r}'s head never reached {want!r} within "
            f"{_CONVERGE_S:.0f}s — nest says "
            f"{_sealed_state(nest, actor, folder, path)!r}"
        )
        if extra_diagnosis is not None:
            msg += "\n" + extra_diagnosis()
        return msg

    return wait_until(_matches, _CONVERGE_S, interval=2.0, diagnose=_diagnose)


def _folder_row(nest, actor, name: str) -> dict | None:
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(actor["actor_id_bytes"]),
        signing_key=bytes(actor["signing_key"]),
    )
    with client:
        reply = client.call("fauna.folders.list", {})
    # By hash: a sealed set's row rests no plaintext name (schema 114).
    from helpers.set_names import find_set

    return find_set(reply.get("folders", []), name)


def _await_audience(nest, actor, name: str, want: str) -> dict:
    """Poll the nest row until ``fauna.folders.list`` reports ``want``.

    ⚠ **This field is DERIVED, not the stored column** (``folder_handlers::
    audience_of``): the column stores only the declassification (``'public'`` or
    NULL), and the wire tri-state is computed — ``public`` when the column says
    so, else ``shared`` when ``mls_group_id`` is set, else ``private``.

    So the flip-back's ground truth is ``want="shared"``, and that is exactly
    *"the nest's audience column cleared"*: for a folder that is still
    bound, the wire can only say ``shared`` **because** the column went NULL — a
    folder still carrying the declassification reports ``public`` regardless of
    bound-ness. Asserting a cleared/absent field here instead would assert a wire
    shape the nest never sends (measured: it sends ``'shared'``).
    """

    def _matches():
        row = _folder_row(nest, actor, name)
        if row is None:
            return None
        return row if row.get("audience") == want else None

    return wait_until(
        _matches,
        _FLAG_S,
        interval=0.5,
        diagnose=lambda: (
            f"wanted audience={want!r}, nest row is {_folder_row(nest, actor, name)!r}"
        ),
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.windows
@pytest.mark.parametrize(
    "folder_share_owner_app", ["tui", "linux", "macos", "windows"], indirect=True
)
@pytest.mark.real_conversations
# real_sync_agent added row 283 (macOS widening); windows joined row 168 — see
# test_public_website_folder_serve.py's own note for the mechanism
# (FaunaMacApp.FaunaE2E.realSyncAgent / windows' FAUNA_E2E_REAL_SYNC_AGENT gates
# whether startSyncAgentProvisioner / HydrationSessionService is ever called at
# all).
@pytest.mark.real_sync_agent
# windows widened row 168: without this the app-spawned agent rendezvouses on
# the machine-global per-SID pipe instead of a run-private one. No-op elsewhere.
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("public-folders-and-websites")
def test_a_bound_folder_flips_back_from_public_and_reseals_its_corpus(
    folder_share_owner_app, tmp_path
):
    """A bound folder published and then un-published leaves no plaintext behind.

    The owner-seat half of the definition of success: (a) the nest's
    ``audience`` column is cleared, (b) the previously-plaintext entry is
    re-sealed under the folder's current content key, and (c) both happen off the
    picker gesture alone — no restart, no second door.
    """
    from common.auth import register_handled_actor
    from conftest import MAIL_PRIMARY_DOMAIN
    from tests.api import conv_api

    app, nest, owner = folder_share_owner_app
    b = app.backups

    # ── 0. A headless recipient with a fetchable KeyPackage ──────────────────
    #
    # The share's admit step FETCHES one (no KP → `NoKeyPackage`), and two are
    # minted so the single destructive fetch leaves the pool observably
    # non-empty — `test_folders.py::test_folder_full_share_round_trip`'s idiom.
    # No app: bound-ness is minted by the owner's gesture and does not wait on
    # anybody accepting.
    recipient_handle = "flipbob" + secrets.token_hex(2)
    recipient = register_handled_actor(
        nest["port"], handle=recipient_handle, domain=MAIL_PRIMARY_DOMAIN
    )
    conv_api.keypackage_upload(
        nest["port"],
        recipient,
        conv_api.mint_key_packages(bytes(recipient["signing_key"]), 2),
    )

    name = f"flipback-{secrets.token_hex(4)}"
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    row = b.find_and_expand_folder(name)

    # ── 1. Share it — the ONE gesture that binds a folder ────────────────────
    b.open_share_dialog()
    assert b.share_dialog_open(), (
        "folder-share-button must open the recipient picker: "
        f"{app.driver.diagnose('recipient-picker-input')}"
    )
    b.share_recipient(
        handle=recipient_handle, actor_id_hex=recipient["actor_id_hex"]
    )
    wait_until(
        lambda: b.shared_member_count() == 1,
        _ROSTER_S,
        diagnose=lambda: (
            f"the share never landed a roster member, so the folder never bound "
            f"and this test's subject does not exist. nest roster="
            f"{conv_api.folder_member_actors(nest['port'], owner, name)!r} "
            f"error={app.error_text()!r}"
        ),
    )

    # The picker's bound-but-not-public rendering, asserted before the flip so a
    # later red can never be blamed on the folder having failed to bind: `shared`
    # is the current state and is NOT a destination here.
    assert b.audience_current_value() == "shared", (
        "a group-bound folder paints `shared` — bound-ness lives in the MLS "
        "group, and normalize_audience reads an empty column on a bound folder "
        f"as `shared`. got {b.audience_current_value()!r}"
    )

    # ── 2. A real sealed back-catalogue ──────────────────────────────────────
    #
    # The engine — not the Media page — is what seals under the M2 content key,
    # so a bound location is the only producer that puts genuinely group-sealed
    # bytes in a folder. Bound AFTER the share on purpose: an engine built while
    # the folder was still unbound seals under the owner's own BackupKey and
    # there would be no content-key generation for the flip-back to re-seal to.
    location = tmp_path / "owner-bound"
    location.mkdir()
    bind_location_under_set(app, name, location, seat="owner")
    atomic_write(location / _PAGE, _BODY)
    await_agent_upload(app, _PAGE, seat="owner")

    baseline = _await_sealed_state(
        nest,
        owner,
        name,
        "sealed",
        why="the pre-flip back-catalogue",
        require_content_key=True,
        extra_diagnosis=lambda: agent_diagnosis(app, "owner"),
    )
    generation = baseline["content_key_version"]

    # ── 3. Publish it — and the back-catalogue really does go plaintext ──────
    #
    # The vacuity guard. If the engine converged in NEITHER direction, step 4
    # would assert `sealed` on bytes that were never unsealed and pass while
    # proving nothing.
    if not b.audience_select_visible():
        b.find_and_expand_folder(name)
    b.set_audience("public")
    wait_until(
        b.declassify_confirm_visible,
        _UI_S,
        diagnose=lambda: (
            "picking `public` must ARM folder-audience-public-confirm and write "
            f"nothing; error={app.error_text()!r}"
        ),
    )
    b.confirm_public()
    _await_audience(nest, owner, name, "public")
    _await_sealed_state(
        nest,
        owner,
        name,
        "plaintext",
        why="the declassify pass (the public window's back-catalogue)",
    )

    # ── 4. The flip-back — half 1's affordance, half 2's re-seal ─────────────
    #
    # This is the option that did not exist before the fix: for a bound
    # folder the picker offered exactly one selectable value and it was the
    # irreversible one. `set_audience` raising here IS the half-1 regression.
    if not b.audience_select_visible():
        b.find_and_expand_folder(name)
    assert b.audience_current_value() == "public", (
        "the select must paint the folder's CURRENT audience before the "
        f"flip-back; got {b.audience_current_value()!r}"
    )
    b.set_audience("shared")

    # (a) the nest's audience column is cleared — read as the derived `shared`,
    # which a bound folder can only report once the declassification is gone.
    _await_audience(nest, owner, name, "shared")
    assert not app.has_error(), (
        f"the flip-back surfaced an error: {app.error_text()!r}"
    )

    # (b) …and every previously-plaintext entry is sealed again, under the
    # folder's CURRENT content key — the same generation the pre-flip corpus
    # used, since nothing rotated. A re-seal that landed under no generation at
    # all would be a member-unreadable corpus wearing a sealed shape.
    resealed = _await_sealed_state(
        nest,
        owner,
        name,
        "sealed",
        why="the flip-back re-seal",
    )
    assert resealed["seq"] > baseline["seq"], (
        "the re-seal must RECORD a new head version rather than leave the "
        f"plaintext one standing: baseline seq={baseline['seq']}, head={resealed!r}"
    )
    assert resealed.get("content_key_version") == generation, (
        f"the flip-back must re-seal under the folder's current content-key "
        f"generation {generation!r}, got {resealed.get('content_key_version')!r} "
        f"— head row {resealed!r}\n" + agent_diagnosis(app, "owner")
    )

    # And the picker settles back on the bound rendering, so the journey is
    # genuinely round-trip rather than one-way with a cleared column.
    if not b.audience_select_visible():
        b.find_and_expand_folder(name)
    wait_until(
        lambda: b.audience_current_value() == "shared",
        _UI_S,
        diagnose=lambda: (
            f"after the flip-back the select must paint `shared` again; got "
            f"{b.audience_current_value()!r} (row {row})"
        ),
    )

# ── the member seat ──────────────────────────────────────────────────────────

_KEYPACKAGE_S = 60.0
_JOIN_S = 180.0
_MEMBER_PAGE = "member-note.html"
_MEMBER_WINDOW_PAGE = "member-window-note.html"

# The member engine's two lines for the public window (`SyncEngine::
# refresh_sync_mode`, pinned one-per-edge by `connected_arm_heal_test.rs::
# a_member_seat_logs_an_unanchored_public_claim_on_each_edge_only`).
_UNANCHORED_CLAIM = "claims public but this seat holds no trusted owner"
_UNANCHORED_CLAIM_WITHDRAWN = "unanchored public claim was withdrawn"


def _make_contacts(nest, member, owner_actor_hex: str) -> None:
    """Make the OWNER a confirmed contact **of the member**, so the incoming
    folder Welcome auto-joins instead of resting as a knock.

    Direction matters: ``NestFolderGate`` resolves ``fauna.contacts.status`` on
    the *recipient's* connection, so it is the member's row for the sharer that
    decides. Raw WS-RPC as fixture setup (E2E rule 8 carve-out — arranging a
    precondition, not standing in for the gesture under test). Same two kinds
    and the same reasoning as ``test_folder_member_media_decrypt.py``: both
    ``accepted`` and ``confirmed`` map to ``Auto``, and confirming anyway keeps
    the precondition off the weaker mapping.
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


def _member_sees_folder(member_app, name: str) -> bool:
    """Whether the folder has appeared on the MEMBER's own folders page.

    ⚠ This is deliberately a UI read, not a nest read, and the two nest-side
    candidates are both wrong for it:

    - ``fauna.folders.list {}`` returns only OWNED folders — the shared-with-me
      half is behind an opt-in flag (``list_core``'s ``include_shared_with_me``,
      kept opt-in so the owner-scoped contract the sync daemons rely on stays
      byte-identical). A plain list can never show this row.
    - Even with the flag it would be the wrong witness: a ``role == "member"``
      row is only **rostered**, and the nest cannot observe an MLS join at all
      (``folder_handlers.rs``: *"the client must filter to sets it has actually
      joined"*). Rostering happens at ``welcome.deliver``, i.e. at share time —
      before the member has joined anything.

    The member's own row IS the join: tui's B3 filter (``TuiMlsQuery``) drops
    every ``role == "member"`` row whose group it has not joined, so the row
    rendering here means the Welcome was received, joined, and custody ingested.
    It is also exactly what the binding gesture below needs to find.
    """
    b = member_app.backups
    b.navigate_folders()
    return any(name in b.folder_title(i) for i in range(b.folder_count()))


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.parametrize("folder_share_owner_app", ["tui", "web"], indirect=True)
@pytest.mark.real_conversations
@pytest.mark.feature("public-folders-and-websites")
# web owner added 2026-09-20. `public-folders-and-websites`
# 3 was short on web, and the reason assumed was that a web
# owner would need a same-identity NATIVE second device to hold the files — which no
# fixture can express. That premise is wrong for THIS witness: the bound location and
# the files live on the MEMBER seat (`tui_member`), and the owner seat only creates,
# shares, grants write and drives the audience picker — none of which needs a bound
# directory, and all of which web has (`FoldersSection.svelte`'s `changeAudience`
# commits the flip back to `shared` immediately; only `public` arms a confirm). So the
# web leg is an ordinary second owner param, two identities, no new pair fixture.
#
# The web leg needed one harness fix, found by its first run here. `[web]` timed out
# in step 1's `set_member_access` on
# `folder-row >> folder-member-item >> folder-member-role-select` — not a missing
# element (`FoldersSection.svelte:1584`) and not an empty roster (the
# `shared_member_count() == 1` wait just above passed; it queries UNSCOPED). On web
# the `folder-row` `<tr>` CLOSES and the expanded body is a SIBLING
# `<tr class="expanded-row">` — a `<tr>` cannot contain another `<tr>` — so a
# `folder-row[i]/…` subtree query can never reach it. That was already known and
# carved out for the conflict-policy picker (`actions/backups.py::set_conflict_policy`);
# `_member_scope` had simply never been given the same carve-out, so every
# `row=`-scoped member action was structurally unrunnable on web, invisibly, because
# no web test drove one. Fixed there, in one place; dropping the prefix costs no
# precision because `expandedFs` is a single value, so exactly one folder's member
# items are in the DOM. Both cases green 2026-09-20.
#
# ⚠ Both seats must be SELECTED, not just available: run it `--app web,tui`. The
# collection hook deselects a param case whose OWNER app is unselected
# (`test_app_default_and_sweep.py::test_two_real_app_fixture_needs_both_seats_selected`),
# but `tui_member` is a plain fixture rather than a callspec param, so the hook cannot
# see the member seat — under `--app web` alone this would launch a tui the run never
# asked for. The web cell moves by per-driver attribution: a multi-seat test is
# attributed to every driver it launched (`feature-catalog.md` § App attribution).
def test_a_member_seat_keeps_what_it_adds_sealed_through_the_public_window_and_after(
    folder_share_owner_app, tui_member, tmp_path
):
    """A member seat never follows the owner's declassification: what it adds
    rests sealed while the folder is public, and after the owner flips it back.

    Ruled 2026-10-04 (``encryption-at-rest.md`` § Implementation status today):
    a member seat with a bound location runs the bearer-only sync agent's engine
    on all 7 apps. That engine holds no MLS state, so it has no trusted owner to
    verify the owner's audience attestation against, and its verdict is *sealed*
    on every tick — the nest naming the folder ``public`` moves nothing on it.
    This test used to wait for the member's engine to declassify its own path,
    which that ruling says cannot happen.

    **The observable is a member-ORIGINATED path**, deliberately: the owner seat
    binds no location here, so a path only the member's engine ever recorded is
    one only the member's engine could have re-recorded. Its head never moving
    is therefore a statement about the member seat alone.

    **"Stays sealed" is asserted after the verdict, never after a wait.** A
    negative asserted on a timer is a wall-clock test (convention 14). The
    member's engine logs once when it reads a row that claims ``public`` and
    judges it sealed for want of a trusted owner, and once when that claim is
    withdrawn (``SyncEngine::refresh_sync_mode``); each assertion below follows
    the log line that proves the engine has read the state it is asserted under.
    """
    from tests.api import conv_api

    owner_app, nest, owner = folder_share_owner_app
    member_app, member, _record, _runtime = tui_member
    ob = owner_app.backups

    # ── 0. Admittable + a contact, so the Welcome auto-joins ──
    wait_until(
        lambda: conv_api.keypackage_count(
            nest["port"], member, member["actor_id_hex"]
        ) > 0,
        _KEYPACKAGE_S,
        interval=1.0,
        diagnose=lambda: (
            "the member never published a fetchable KeyPackage, so the owner's "
            "share cannot admit them to the folder's MLS group"
        ),
    )
    _make_contacts(nest, member, owner["actor_id_hex"])

    # ── 1. Owner: create, share (the gesture that binds), grant WRITE ──
    #
    # Write access is load-bearing rather than incidental: a reader member has
    # nothing to originate with, and member-originated paths are the whole
    # observable.
    name = f"flipback-m-{secrets.token_hex(4)}"
    ob.navigate_folders()
    ob.create_folder_via_wizard(name)
    row = ob.find_and_expand_folder(name)
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])
    wait_until(
        lambda: ob.shared_member_count() == 1,
        _ROSTER_S,
        diagnose=lambda: (
            f"the share never landed a roster member; nest roster="
            f"{conv_api.folder_member_actors(nest['port'], owner, name)!r} "
            f"error={owner_app.error_text()!r}"
        ),
    )
    ob.set_member_access("writer", 0, row=row)

    # ── 2. The member joins, and binds a location of its own ──
    #
    # The piece `test_folder_member_media_decrypt.py` never needed: that module
    # reads through Media, which needs no engine of the member's own. The verdict
    # under test is an engine's, so this seat needs a real running one.
    wait_until(
        lambda: _member_sees_folder(member_app, name),
        _JOIN_S,
        interval=3.0,
        diagnose=lambda: (
            "the folder never appeared on the member's own folders page, so the "
            "auto-join off the contact rail did not complete (the row is gated "
            "on an actual MLS join, not on rostering); "
            f"rows={member_app.backups.folder_count()} "
            f"error={member_app.error_text()!r}"
        ),
    )
    member_location = tmp_path / "member-bound"
    member_location.mkdir()
    bind_location_under_set(member_app, name, member_location, seat="member")

    def _member_diagnosis() -> str:
        return agent_diagnosis(member_app, "member")

    # ── 3. The member originates a file BEFORE the flip ──
    #
    # The baseline waits for the folder's content key, not just for `sealed`: an
    # engine built before the share's keys reach the agent seals its first upload
    # with no generation and re-records it once they arrive (see
    # `_await_sealed_state`). Waiting that out here is what lets the later steps
    # read an unmoved `seq` as "this engine re-recorded nothing".
    atomic_write(member_location / _MEMBER_PAGE, b"<h1>from the member seat</h1>\n")
    await_agent_upload(member_app, _MEMBER_PAGE, seat="member")
    baseline = _await_sealed_state(
        nest, owner, name, "sealed",
        why="the member's pre-flip upload, under the folder's content key",
        path=_MEMBER_PAGE,
        extra_diagnosis=_member_diagnosis,
        require_content_key=True,
    )

    # ── 4. Publish — and the member seat stays sealed ──
    #
    # The sync point is the member engine's own line: it has READ the row that
    # claims `public` and judged it sealed. Only then is "still sealed" a
    # statement about the verdict rather than about a tick that has not landed.
    if not ob.audience_select_visible():
        ob.find_and_expand_folder(name)
    ob.set_audience("public")
    wait_until(ob.declassify_confirm_visible, _UI_S)
    ob.confirm_public()
    _await_audience(nest, owner, name, "public")
    await_agent_log(
        member_app,
        _UNANCHORED_CLAIM,
        also=log_folder_name(name),
        seat="member",
        window=_CONVERGE_S,
    )

    # A file written INSIDE the public window is the sharp half: the engine's
    # upload arm is live, the nest names the folder public, and it must still
    # seal under the folder's content key.
    atomic_write(member_location / _MEMBER_WINDOW_PAGE, b"<h1>written while public</h1>\n")
    await_agent_upload(member_app, _MEMBER_WINDOW_PAGE, seat="member")
    in_window = _await_sealed_state(
        nest, owner, name, "sealed",
        why="the member's public-window upload must rest sealed",
        path=_MEMBER_WINDOW_PAGE,
        extra_diagnosis=_member_diagnosis,
        require_content_key=True,
    )
    state, head = _sealed_state(nest, owner, name, _MEMBER_PAGE)
    assert (state, head["seq"]) == ("sealed", baseline["seq"]), (
        "the member's pre-flip path must rest sealed and un-re-recorded through "
        f"the public window: baseline seq={baseline['seq']}, now {state!r} "
        f"head={head!r}\n" + _member_diagnosis()
    )

    # ── 5. Flip back — and nothing the member added moved ──
    if not ob.audience_select_visible():
        ob.find_and_expand_folder(name)
    ob.set_audience("shared")
    _await_audience(nest, owner, name, "shared")
    await_agent_log(
        member_app,
        _UNANCHORED_CLAIM_WITHDRAWN,
        also=log_folder_name(name),
        seat="member",
        window=_CONVERGE_S,
    )

    for path, before in ((_MEMBER_PAGE, baseline), (_MEMBER_WINDOW_PAGE, in_window)):
        state, head = _sealed_state(nest, owner, name, path)
        assert state == "sealed" and head.get("content_key_version") is not None, (
            f"{path!r} must rest sealed under a content-key generation after the "
            f"flip-back; nest says {state!r} head={head!r}\n" + _member_diagnosis()
        )
        assert head["seq"] == before["seq"], (
            f"{path!r} was re-recorded across the flip-back (seq {before['seq']} → "
            f"{head['seq']}): a seat that never declassified has nothing to "
            f"re-seal. head={head!r}\n" + _member_diagnosis()
        )
