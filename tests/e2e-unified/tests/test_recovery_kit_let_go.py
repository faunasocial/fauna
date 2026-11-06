"""tier_3 e2e: the let-go of a dead identity generation (recovery-kit outcome 16).

Goal docs: ``docs/goal/ui/settings.md`` § Recovery kit (*A fifth act — the
let-go of a dead generation*: the line, its type-to-confirm field and its
button, rendered only while the account runtime's dead read answers
non-empty) and ``docs/goal/architecture/fleet-scope-reclamation.md``
§ *Fleet-scope reclamation*, clause (3)(j) (what is dead, and why only the
user may let it go).

**How the journey reaches a dead generation.** A generation is dead when no
device keys it and no holder wraps it. A succession no longer leaves one — the
ceremony keeps the holder's copy (clause (3)(i), bound (β)) — so the
aftermath journeys' fixtures cannot stand in here. Of the three roads clause
(j) names, this takes the third: every device gone, and a holder that no
longer has the wrap — a nest restored from a store older than the deposit.
The account's one device first creates a folder (account-level data the
generation seals: its ``fauna.state.folder-keys`` custody row, which the
severance leaves standing — a device takes only its own rows with it), then
signs out through the app (the severance, so no
member is left whose reach lists the generation); the nest then loses the
account's generation wraps — both copies, the holder's table and the fleet
plane's ``fauna.state.escrow-wrap`` row, which the depositing device writes
beside the receipt, after the deposit, so a store older than the deposit
holds neither (``account-replica-posture.md`` § *Escrow custody*, rulings (1)
and (2)). That is the restored nest's state and the one step no app gesture
can take (``generation_escrow_test_hook.rs``): the plane row is sealed at a
blinded item key the nest cannot derive, so the old device's own store names
it first (the e2e reader ``escrow_wrap_coordinates``). The kit then restores
the account onto a fresh device, whose escrow recovery finds no wrap and so
keys nothing of what the old device sealed. Everything the user then sees and
does is the production path.

Convention 14 throughout: every wait is a deadline poll on observable state.
"""
from __future__ import annotations

import json
import re
import secrets
import urllib.request

import pytest

from helpers.recovery_restore import (
    assert_restored_the_same_account,
    point_the_restore_at,
    qualified_account,
    require_restore_button,
    sign_in_after_restore,
    wait_for_restore_landing,
)
from helpers.waiting import await_account_runtime_assembled, wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_SECRET_HEX_LEN = 64

#: How long the restored device may take to walk the fleet feed and read the
#: old generation dead: the runtime's assembly, the walk, one holder ask. Paid
#: only on red (convention 14).
_DEAD_READ_S = 120.0

#: How long the kit's freshly deposited generation may take to reach the
#: device's merged state as an escrow-wrap row.
_WRAP_ROW_S = 60.0

#: How long the let-go may take to retire every row and settle.
_LET_GO_S = 60.0

#: A word that is not the confirm word but shares its letters' shape.
_WRONG_WORD = "LET IT GO"

_K = S.settings.recovery_kit


def _template_pattern(rendered: str, fields: dict[str, str]) -> re.Pattern:
    """A regex for a rendered i18n template whose ``fields`` (sentinel →
    named-group regex) vary; everything else is matched literally. A field the
    template repeats must repeat the same text (a back-reference)."""
    pattern = re.escape(rendered)
    for sentinel, group in fields.items():
        name = re.match(r"\(\?P<(\w+)>", group).group(1)
        first, *rest = pattern.split(re.escape(sentinel))
        pattern = first + "".join(
            (group if i == 0 else f"(?P={name})") + chunk
            for i, chunk in enumerate(rest)
        )
    return re.compile(f"^{pattern}$")


#: The two copies the shared projection selects between
#: (``generation_let_go::unreadable_status``): dated where any mint record is
#: readable here, undated otherwise. Both name the count; both carry the
#: sign-in-first advice.
_DATED = _template_pattern(
    _K.unreadable_status(rows="\x00R", since="\x00S"),
    {"\x00R": r"(?P<rows>\d+)", "\x00S": r"(?P<since>.+?)"},
)
_UNDATED = _template_pattern(
    _K.unreadable_status_undated(rows="\x00R"), {"\x00R": r"(?P<rows>\d+)"}
)


def _parse_status(line: str) -> int | None:
    """The row count the line names, or None when it is neither copy."""
    m = _DATED.match(line) or _UNDATED.match(line)
    return int(m.group("rows")) if m else None


def _escrow_wrap_coordinates(app) -> list[dict]:
    """The device's own ``fauna.state.escrow-wrap`` rows with the blinded item
    key the nest knows each by (the e2e reader; ``[]`` until one is merged)."""
    raw = app.driver.call_command("escrow_wrap_coordinates", {})
    if isinstance(raw, str):
        raw = json.loads(raw)
    if not isinstance(raw, dict) or not raw.get("found"):
        return []
    return list(raw.get("rows") or [])


def _drop_generation_wraps(port: int, actor_hex: str, item_keys: list[str]) -> dict:
    """The holder loses every generation wrap of ``actor_hex``, and the fleet
    plane its escrow-wrap rows at ``item_keys`` (test hook)."""
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/api/v1/test/generation_escrow/{actor_hex}/drop",
        data=json.dumps({"plane_item_keys": item_keys}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def _reopen_account(app) -> str:
    """Leave and re-enter the Account sub-page — its hydrate re-runs the dead
    read — and return the unreadable line ("" while nothing reads dead)."""
    app.settings._navigate_subpage("status")
    app.settings.open_recovery_kit()
    return app.settings.unreadable_status()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("recovery-kit")
def test_a_dead_generation_is_named_and_let_go_only_on_the_confirm_word(
    ungranted_app, nest_instance
):
    """kit → sign out → the holder loses the wraps → restore → the let-go.

    ``ungranted_app`` (a dedicated fresh actor): the journey signs out, has the
    nest drop the account's wraps and lets data go, so a shared actor would make
    test order load-bearing for every other reader of that account.
    """
    app = ungranted_app
    account = qualified_account(app, nest_instance)
    actor_hex = app.driver.get_state("session.actor_id")
    assert actor_hex and len(actor_hex) == _SECRET_HEX_LEN, (
        f"session.actor_id must be the account's 64-hex id, got {actor_hex!r}"
    )

    # ── A live account has nothing dead, so the trio is not on screen ──────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    assert app.settings.unreadable_status() == "", (
        "a fresh account's only generation is keyed by its own device: nothing "
        f"is dead, yet the line reads {app.settings.unreadable_status()!r}"
    )
    assert app.is_absent("recovery-kit-let-go-button"), (
        "the let-go button renders only while a generation is dead"
    )

    # ── The kit, the one way back after the device is gone ─────────────────
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    phrase = app.settings.recovery_kit_secret()
    assert len(phrase) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(phrase)} chars; error: {app.error_text()!r}"
    )

    # ── Where the plane's copy of the wrap sits, read while the device that
    #    deposited it still has its store ──────────────────────────────────
    coords: list[dict] = []

    def _wrap_rows_merged() -> bool:
        coords[:] = _escrow_wrap_coordinates(app)
        return bool(coords)

    wait_until(
        _wrap_rows_merged,
        _WRAP_ROW_S,
        interval=1.0,
        diagnose=lambda: (
            "a kit was made, so the account has minted and deposited a "
            "generation, yet its device's store holds no escrow-wrap row; "
            f"reader answered {app.driver.call_command('escrow_wrap_coordinates', {})!r}"
        ),
    )
    item_keys = [c["item_key_hex"] for c in coords]

    # ── Account data sealed under that generation: a folder. Its create mints
    #    the folder's nonce into custody, an account-level
    #    `fauna.state.folder-keys` row sealed under the generation tip — the
    #    kind the sign-out's severance leaves standing, where the device's own
    #    rows (its endpoints, its reach) leave with it (clause (4)). Without
    #    it nothing account-level rests under the generation and nothing reads
    #    dead after the restore. After the kit, so a tip resolves. ─────────
    app.backups.navigate_folders()
    app.backups.create_folder_via_wizard(f"let-go-{secrets.token_hex(4)}")

    # ── Every device gone: the one device signs out (the severance) ────────
    app.settings.sign_out()

    # ── The holder no longer has the wraps, either copy (a nest restored from
    #    before the deposit — the step no app gesture can take) ──────────────
    dropped = _drop_generation_wraps(nest_instance["port"], actor_hex, item_keys)
    assert dropped.get("ok"), f"the wrap-drop hook refused: {dropped!r}"
    assert dropped.get("wraps", 0) > 0 and dropped.get("plane_rows", 0) > 0, (
        "the staging must take both copies of the wrap — the holder's table and "
        f"the plane row at {item_keys!r}; the hook answered {dropped!r}"
    )

    # ── The kit restores the account onto a fresh device ───────────────────
    app.onboarding.navigate_to_status()
    require_restore_button(app)
    point_the_restore_at(app, nest_instance)
    app.onboarding.open_recovery_entry()
    app.onboarding.restore_from_recovery_kit(phrase, account=account)
    wait_for_restore_landing(app)
    assert_restored_the_same_account(app)
    sign_in_after_restore(app)
    await_account_runtime_assembled(app.driver)

    # ── The line names the count, the since and the sign-in-first advice ───
    app.settings.navigate()
    app.settings.open_recovery_kit()
    seen: list[str] = []

    def _dead_line() -> bool:
        line = _reopen_account(app)
        seen.append(line)
        return bool(line)

    wait_until(
        _dead_line,
        _DEAD_READ_S,
        interval=2.0,
        diagnose=lambda: (
            "the restored device keys nothing the old device sealed and the "
            f"holder wraps none of it (the hook dropped {dropped!r}), yet the "
            "Account page never showed recovery-kit-unreadable-status; last "
            f"line {seen[-1:]!r}; error surface {app.error_text()!r}"
        ),
    )
    line = app.settings.unreadable_status()
    rows = _parse_status(line)
    # A whole-line match against either i18n copy is also the sign-in-first
    # advice's check: both templates carry it, and the match is anchored.
    assert rows is not None and rows > 0, (
        "the line is the shared projection's copy (count, since-date where a "
        "mint date is readable, and the sign-in-first advice), naming how many "
        f"items rest unreadable; got {line!r}"
    )
    assert app.is_visible("recovery-kit-let-go-confirm-field"), (
        "the confirm field renders beside the line"
    )
    assert app.is_visible("recovery-kit-let-go-button"), (
        "the let-go button renders beside the line"
    )

    # ── An empty field and a wrong word leave the act unarmed ──────────────
    assert not app.settings.let_go_armed(), (
        "with nothing typed the let-go must not be armed"
    )
    app.settings.type_let_go_confirm(_WRONG_WORD)
    assert not app.settings.let_go_armed(), (
        f"{_WRONG_WORD!r} is not the confirm word, yet the let-go is armed"
    )
    assert _parse_status(app.settings.unreadable_status()) == rows, (
        "a wrong word lets nothing go: the line must still name the same "
        f"{rows} items; it reads {app.settings.unreadable_status()!r}"
    )

    # ── The confirm word arms it, and the act runs ─────────────────────────
    # The word itself is not localized (only its prompt is); it is the shared
    # `generation_let_go::LET_GO_CONFIRM_WORD`, which the prompt names.
    word = "LET GO"
    assert word in _K.let_go_confirm_placeholder, (
        "the confirm word the test types must be the one the prompt names: "
        f"{_K.let_go_confirm_placeholder!r}"
    )
    app.settings.type_let_go_confirm(word)
    wait_until(
        app.settings.let_go_armed,
        10.0,
        diagnose=lambda: "the confirm word typed, yet the let-go never armed",
    )
    app.settings.press_let_go()

    wait_until(
        lambda: app.is_absent("recovery-kit-unreadable-status"),
        _LET_GO_S,
        interval=1.0,
        diagnose=lambda: (
            "after the let-go the dead read should answer empty and the trio "
            f"leave the screen; line {app.settings.unreadable_status()!r}, "
            f"error surface {app.error_text()!r}"
        ),
    )
    assert app.is_absent("recovery-kit-let-go-confirm-field")
    assert app.is_absent("recovery-kit-let-go-button")
    done = _template_pattern(
        _K.let_go_done(retired="\x00N"), {"\x00N": r"(?P<n>\d+)"}
    )
    outcome = app.error_text()
    m = done.match(outcome or "")
    assert m and int(m.group("n")) > 0, (
        "the act says what it did on error-message's line, naming the items "
        f"it let go; got {outcome!r}"
    )

    # ── And it stays gone: a fresh read finds nothing dead ─────────────────
    assert _reopen_account(app) == "", (
        "a re-read after the let-go finds nothing dead"
    )
