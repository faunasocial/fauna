"""tier_3 e2e: the phrase-only identity restore (`recovery_entry`).

Goal docs: ``docs/goal/behavior/onboarding.md`` § 1 Identity (the screen, its
account field, and the two refusals it must distinguish) and
``docs/goal/behavior/identity-succession.md`` § Seed escrow → *Restore path*
(the ceremony: a fresh client with only the phrase fetches the escrow blob over
a pre-identity, challenge-gated kind and unseals the identity seed locally).

This is the day the recovery kit exists for, and it is the one journey no unit
test can stand in for. Three things only a full-stack run establishes:

- **The kit a real ceremony minted opens a blob a real nest is holding.** The
  crate tests prove challenge → sign → fetch → unseal against a recording
  transport; only a live ``fauna-nest`` proves the bytes the create ceremony
  put are the bytes the restore ceremony gets back, across a process boundary
  and a factory reset.
- **The restored seed is the account's seed.** The handle check that follows
  runs a *silent challenge* signed with whatever the restore handed back, and
  the nest verifies it against the registered actor. "Welcome back" is
  therefore end-to-end proof of the seed itself — a wrong 32 bytes would read
  as an unregistered identity, not as a passing test.
- **The account field is what locates the nest for a written-down code.** The
  ceremony is pre-identity, so what follows the handle's ``@`` is the only thing
  that can find the home nest. This journey takes the **64-hex** path — what a
  user who wrote the code down or pressed copy actually holds — so the field
  carries it. (The kit's *copied* form — the same URI its QR encodes — embeds
  ``handle=`` and restores with nothing typed; that path has its own journey
  below, and the parse is pinned by the machine test
  ``a_kit_carrying_its_handle_restores_with_nothing_typed``.)
- **The bare handle is what reaches ``fauna.actor.by_handle``.** Not incidental,
  and easy to lose: the account this journey types is the *direct-address* form
  (``handle@<host:port>`` — the harness nest has no domain), which is the
  ratified locator of last resort, ``onboarding.md`` § 1 Identity →
  ``recovery_entry`` → *The locator of last resort is a direct nest address*.
  A nest resolves handles by exact match on the stored bare handle, so if the
  machine ever sent the typed compound the lookup would find no actor and this
  journey would fail at ``AccountUnknown`` — that is what makes the assertion
  below a live pin on the naming half, against a real nest, rather than a
  restatement of the machine test. ``_assert_direct_address_account`` keeps the
  claim honest if the fixture ever grows a DNS name.

What this journey deliberately does **not** prove is the *locating* half of that
same contract: ``set_provider_base_urls`` short-circuits the resolution (the
harness nest serves plain HTTP, the classification yields ``https``), so the
address's own classification — bracketing an IPv6 literal, honoring an explicit
port, taking the injected loopback port — is pinned in
``libs/fauna-onboarding-machine/tests/recovery_entry_ceremony.rs`` instead
(``a_direct_*_locates``, mutation-graded). Closing that last gap needs a
TLS-serving nest fixture, which does not exist yet.

Latency-independent throughout (convention 14): every wait is a named generous
ceiling on an element or on the handle-check's own outcome message, never a
settle-sleep, and the ceremony's landing rides back on the click that started
it.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app that renders the
restore CTA — today tui, the lead app; the others join as their legs land.
"""
from __future__ import annotations

import secrets

import pytest

from helpers.app_surface import skip_environment, skip_unbuilt
from helpers.inherited_corpus import CORPUS_READ_S, seed_an_image_under_this_identity
from helpers.recovery_restore import (
    RESTORE_OUTCOME_S,
    assert_landing_left_no_error,
    assert_restored_the_same_account,
    lose_every_device_and_open_restore,
    point_the_restore_at,
    qualified_account,
    sign_in_after_restore,
    skip_unless_restore_is_built,
    wait_for_restore_landing,
)
from helpers.succession import register_recovery_kit, succeed_identity
from helpers.succession_ceremony import (
    SUCCESSION_AND_RELAUNCH_S,
    kit_on_screen,
    require_stolen_gate,
    settled_actor_id,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The recovery root is 32 bytes as lowercase hex.
_SECRET_HEX_LEN = 64

# A well-formed root that no account ever registered — used to drive refusals
# without minting a real kit.
_UNREGISTERED_ROOT = "7c" * 32


def _assert_direct_address_account(account: str) -> None:
    """The typed account must be the direct-address form this journey claims.

    The docstring says the naming half is pinned live *because* the address
    part cannot be a domain the nest would also accept as its own. That is a
    property of the fixture, not of the product, so assert it rather than
    trusting it: a nest fixture that one day serves a real DNS name would make
    the coverage claim silently false while the test stayed green.
    """
    address = account.split("@", 1)[1]
    host = address.rsplit(":", 1)[0] if ":" in address else address
    host = host.strip("[]")
    is_direct = (
        host == "localhost"
        or host.endswith(".local")
        or all(part.isdigit() for part in host.split("."))
        or ":" in host  # an unbracketed IPv6 literal
    )
    assert is_direct, (
        f"this journey pins the direct-address locator, but the fixture handed "
        f"it the domain {host!r}; either restore a direct-address nest fixture "
        f"or move the naming-half claim out of the module docstring"
    )


@pytest.mark.feature("recovery-kit")
def test_the_phrase_alone_restores_the_account_after_losing_every_device(
    ungranted_app, nest_instance
):
    """create a kit → factory reset → restore from the phrase → the same account.

    ``ungranted_app`` (a dedicated fresh actor) rather than the shared session
    user, for the same reason the Settings ceremonies use it: this writes to the
    account's registration chain and escrow row, so a shared actor would make
    test ORDER load-bearing for everyone else reading that account.
    """
    app = ungranted_app

    # The account being recovered, captured while a session still exists —
    # qualified with the nest's host, because a handle's `@domain` is the only
    # thing a pre-identity ceremony can find a nest from. This is exactly what
    # `fauna_core::resolve::qualify_handle` writes into a kit QR's `handle=`;
    # here the test plays the part of the user typing it, which is the path a
    # written-down 64-hex code takes.
    bare_handle = app.driver.get_state("session.handle")
    assert bare_handle, "the fixture must be signed in with a real nest handle"
    account = f"{bare_handle}@{nest_instance['url'].split('//', 1)[1]}"
    _assert_direct_address_account(account)

    # ── Phase 1: mint a kit on the signed-in device ─────────────────────────
    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()

    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    phrase = app.settings.recovery_kit_secret()
    assert len(phrase) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(phrase)} chars; "
        f"error surface: {app.error_text()!r}"
    )

    # ── Phase 2: lose every device ─────────────────────────────────────────
    # `driver.reset()` is the factory reset — clear the credential namespace
    # and return to onboarding, no relaunch. From here the app holds nothing
    # but what the user typed, which is the precondition this whole plane
    # exists for.
    app.driver.reset()
    app.onboarding.navigate_to_status()
    skip_unless_restore_is_built(app)

    # The tier_3 nest override, the same seam `test_onboarding_localhost.py`
    # uses. The kit's handle resolves to `https://<host>:<port>` (uniform https
    # — the host class no longer picks the scheme), while this harness's nest
    # serves plain HTTP under `FAUNA_INSECURE_DISABLE_TLS`. The override points
    # the pre-identity connection at it; `state.nest_url` still records the
    # resolved https URL, so nothing about the resolution under test is faked.
    point_the_restore_at(app, nest_instance)

    # ── Phase 3: the restore ───────────────────────────────────────────────
    app.onboarding.open_recovery_entry()
    # The phrase read off `recovery-kit-secret-display` is the bare 64-hex code
    # — the human-writable half. It names no account, so the field supplies the
    # handle; that is precisely the case `recovery-entry-account-field` exists
    # for, and the refusal a missing one produces has its own test below.
    #
    # Because the kit names no actor either, the nest is what turns this handle
    # into one (`fauna.actor.by_handle`) — over the direct address typed above.
    # `users.handle` holds the bare handle, so a machine that forwarded the
    # typed `handle@host:port` compound would resolve nothing and land on
    # `account_unknown` right here. Getting past this line is the live proof of
    # the naming half; the landing assertion below is where it surfaces.
    app.onboarding.restore_from_recovery_kit(phrase, account=account)

    # The landing is `handle_entry` — the same landing an import produces,
    # because the seed IS recovered and everything downstream is an import.
    wait_until(
        lambda: app.is_visible("handle-input"),
        45.0,
        diagnose=lambda: (
            "the restore did not land on handle_entry; error surface says "
            f"{app.error_text()!r}, still on recovery_entry="
            f"{app.is_visible('recovery-entry-phrase-field')}, "
            f"on identity_import={app.is_visible('paste-secret-field')}"
        ),
    )
    assert_landing_left_no_error(app)

    prefilled = app.driver.get_text("handle-input")
    assert prefilled == account, (
        "the account the restore proved ownership of must be carried into the "
        f"field handle_entry asks for next; got {prefilled!r}, wanted {account!r}"
    )

    # ── Phase 4: prove it is the SAME identity, not merely A identity ──────
    # The handle check runs a silent challenge signed with whatever the restore
    # handed back; the nest verifies it against the registered actor. Only the
    # real seed produces the already-registered outcome.
    app.onboarding.run_handle_check(timeout=45)
    message = app.driver.get_text("handle-message-area")
    assert "already_on_nest" in message or "Welcome back" in message, (
        "the restored seed must silently authenticate as the account that "
        f"minted the kit; handle-message-area reads {message!r} "
        f"(error surface: {app.error_text()!r})"
    )


@pytest.mark.feature("recovery-kit")
def test_a_phrase_that_names_no_account_asks_for_one_instead_of_failing(app):
    """A bare 64-hex code identifies no account, so the screen asks for a handle.

    Worth a journey rather than only a machine test: the refusal has to *reach
    the user*. A dropped gesture or a silently-disabled submit reads downstream
    as a product bug, and the machine test cannot see either.
    """
    app.onboarding.navigate_to_status()
    skip_unless_restore_is_built(app)
    app.onboarding.open_recovery_entry()

    app.onboarding.restore_from_recovery_kit(_UNREGISTERED_ROOT)

    wait_until(
        lambda: app.has_error(),
        20.0,
        diagnose=lambda: (
            "a phrase naming no account must refuse on error-message rather "
            "than sit silent or navigate away; page still shows "
            f"phrase-field={app.is_visible('recovery-entry-phrase-field')}, "
            f"handle-input={app.is_visible('handle-input')}"
        ),
    )
    assert app.is_visible("recovery-entry-phrase-field"), (
        "the user stays on the screen to supply what is missing"
    )
    text = app.error_text() or ""
    assert "handle" in text.lower(), (
        "the refusal must name what to do next — enter the account's handle; "
        f"got {text!r}"
    )


@pytest.mark.feature("recovery-kit")
def test_an_identity_secret_is_never_taken_for_a_recovery_phrase(app):
    """Pasting `fauna://identity` here is refused locally, before anything is sent.

    The two codecs are deliberate twins on different hosts precisely so this
    confusion cannot silently register an identity seed as a recovery root
    (`identity-succession.md` § The RecoveryKey → *Kit payload*). The journey
    pins that the refusal is what the user sees.
    """
    app.onboarding.navigate_to_status()
    skip_unless_restore_is_built(app)
    app.onboarding.open_recovery_entry()

    app.onboarding.restore_from_recovery_kit(
        f"fauna://identity?secret={_UNREGISTERED_ROOT}",
        account="alice@example.test",
    )

    wait_until(
        lambda: app.has_error(),
        20.0,
        diagnose=lambda: (
            "an identity payload must be refused on error-message; page shows "
            f"phrase-field={app.is_visible('recovery-entry-phrase-field')}, "
            f"handle-input={app.is_visible('handle-input')}"
        ),
    )
    assert app.is_visible("recovery-entry-phrase-field")
    text = (app.error_text() or "").lower()
    assert "recovery" in text, (
        f"the refusal must say what was expected instead; got {text!r}"
    )


# ── After a replace, the new phrase is the one that restores ────────────────


@pytest.mark.feature("recovery-kit")
def test_after_a_replace_the_new_phrase_restores_and_the_old_one_does_not(
    ungranted_app, nest_instance
):
    """create → replace with the held kit → lose every device → the OLD phrase
    is refused, the NEW one brings the account back.

    The kit-in-hand replace re-seals the escrow blob in the same call
    (`settings.md` § User actions, `recovery-kit-replace-button`), because the
    nest deletes the blob the moment a registration changes the key. A replace
    that skipped the re-put, or re-put under the old key, would leave the user
    holding a phrase that restores nothing — and nothing on the signed-in
    device would show it. Only a restore from a device that has lost
    everything can.
    """
    app = ungranted_app
    account = qualified_account(app, nest_instance)
    _assert_direct_address_account(account)

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    old_phrase = app.settings.recovery_kit_secret()
    assert len(old_phrase) == _SECRET_HEX_LEN

    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.replace_kit_with_held(old_phrase)
    wait_until(
        lambda: app.settings.recovery_kit_secret() not in ("", old_phrase),
        30.0,
        diagnose=lambda: (
            "the replace never showed its new kit; status reads "
            f"{app.settings.recovery_kit_status()!r}, error surface: "
            f"{app.error_text()!r}"
        ),
    )
    new_phrase = app.settings.recovery_kit_secret()
    assert len(new_phrase) == _SECRET_HEX_LEN

    lose_every_device_and_open_restore(app, nest_instance)

    # The retired phrase first: it must be refused, and the user must stay on
    # the screen to try again rather than being sent anywhere.
    app.onboarding.restore_from_recovery_kit(old_phrase, account=account)
    wait_until(
        lambda: app.has_error() or app.is_visible("handle-input"),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "the retired phrase got no answer at all; page shows phrase-field="
            f"{app.is_visible('recovery-entry-phrase-field')}"
        ),
    )
    assert app.driver.is_absent("handle-input"), (
        "a phrase the replace retired must not restore the account — it "
        "landed on handle_entry, so the old key still opens the escrow"
    )
    assert app.is_visible("recovery-entry-phrase-field"), (
        "the refusal keeps the user on recovery_entry to try again"
    )

    # …and the phrase the replace showed is the one that works.
    app.onboarding.restore_from_recovery_kit(new_phrase, account=account)
    wait_for_restore_landing(app)
    assert_restored_the_same_account(app)


# ── After a succession, a replaced kit still carries the predecessor seed ───


def _seed_under_a_then_succeed(app, tmp_path) -> tuple[str, str]:
    """upload an image under A → A → B through the UI → read B's closing-act kit.

    Returns ``(inherited_file, successor_kit)``. The steps both post-succession
    restore journeys share, so the one that replaces the kit and the one that
    does not stay the same journey up to their fork.

    - **An image is uploaded under A BEFORE the ceremony** and proven listed and
      painted (``seed_an_image_under_this_identity``): "content sealed under A
      opens" is otherwise satisfied by an empty corpus.
    - **The successor's kit is read off the screen before any navigation**
      (``kit_on_screen``): entering Account clears a kit on screen (shown-once
      custody), and A's kit retired with A, so this is the one kit the user
      holds once the ceremony ends.
    """
    _, inherited_file = seed_an_image_under_this_identity(app, tmp_path)

    old_actor = settled_actor_id(app)
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the Status page must render A's actor id first; got {old_actor!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN

    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    app.settings.succeed_identity_with_held_kit(held)

    # The closing act's kit — read FIRST and with no navigation of our own.
    wait_until(
        lambda: kit_on_screen(app) not in ("", held),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"no fresh kit on screen for the successor (reads "
            f"{kit_on_screen(app)!r}), error={app.error_text()!r}"
        ),
    )
    successor_kit = kit_on_screen(app)
    assert len(successor_kit) == _SECRET_HEX_LEN

    wait_until(
        lambda: settled_actor_id(app) not in ("", old_actor),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"still reads actor={settled_actor_id(app)!r} (old={old_actor!r}) "
            f"error={app.error_text()!r}"
        ),
    )
    return inherited_file, successor_kit


def _restore_sign_in_and_open_the_predecessor_corpus(
    app, nest_instance, *, phrase: str, account: str, inherited_file: str,
    window: str, which_blob: str,
) -> None:
    """lose every device → restore from ``phrase`` → sign in → A's corpus opens.

    ``Restored`` alone is not enough: ``machine.rs`` maps only an *unreadable*
    predecessor section to ``RestoredPredecessorsLost``, and an *absent* one
    lands plain ``Restored``. So the no-error landing is necessary and the
    positive half is the corpus: after sign-in the item's NAME lists and its
    thumbnail PAINTS, both opened with A's ``BackupKey`` recovered from the
    blob, and the corpus re-seal line does not read *owed elsewhere*, the arm
    rendered exactly when this device holds no predecessor material.

    ``window`` is the corpus re-seal line the caller read before losing the
    device, quoted on failure; ``which_blob`` names the blob under test.
    """
    lose_every_device_and_open_restore(app, nest_instance)
    app.onboarding.restore_from_recovery_kit(phrase, account=account)
    # No error on landing: rules out `RestoredPredecessorsLost` (an unreadable
    # section). An ABSENT one is what the corpus half below catches.
    wait_for_restore_landing(app)
    assert_restored_the_same_account(app)
    sign_in_after_restore(app)

    def _window() -> str:
        return (
            f" (corpus re-seal line before the device was lost read {window!r}; "
            "if the re-seal had already moved the corpus under B, this read "
            "would not need A's seed at all)"
        )

    app.media.navigate()
    wait_until(
        lambda: inherited_file in app.media.item_names(),
        CORPUS_READ_S,
        diagnose=lambda: (
            f"the restored device lists {app.media.item_names()!r}, not the "
            f"{inherited_file!r} sealed under the retired identity — "
            f"{which_blob} did not carry A's seed, so the names under A "
            f"cannot open; error={app.error_text()!r}" + _window()
        ),
    )
    painted = app.media.wait_for_painted_thumbnails(1, timeout=CORPUS_READ_S)
    if painted is not None:
        assert painted == 1, (
            "the restored device must OPEN the bytes sealed under A, but "
            f"{painted} of 1 thumbnails painted; error={app.error_text()!r}"
            + _window()
        )

    app.settings.navigate()
    app.settings.open_recovery_kit()
    corpus_line = app.settings.aftermath_corpus_reseal_status()
    assert corpus_line != S.settings.recovery_kit.corpus_reseal_owed_elsewhere, (
        f"the restored device reports it holds no predecessor material — "
        f"{which_blob} came back without A's seed" + _window()
    )


@pytest.mark.feature("recovery-kit")
def test_a_kit_replaced_after_a_succession_still_restores_the_predecessor_corpus(
    ungranted_app, nest_instance, tmp_path
):
    """upload under A → succeed A→B → replace B's kit → lose every device →
    restore from the NEW phrase → A's corpus still opens.

    ``identity-succession.md`` § Seed escrow: after a succession the corpus
    stays sealed under the retired identity A until B's re-seal drains, and the
    only copies of A's seed are on the user's devices — so every escrow blob
    write, **kit replacement included**, carries the predecessor chain. The
    replace re-puts the blob, which REPLACES the one the succession's closing
    act sealed; a replace that sealed ``&[]`` (the pre-2026-08-05 defect) hands
    a lost-every-device restore B alone, and the corpus under A goes dark for
    good. The op layer pins that both ways; this is the end-to-end witness.

    **Why each assertion is shaped the way it is:**

    - **The kit replaced is the SUCCESSOR's closing-act kit**, read off the
      screen before any navigation (``kit_on_screen``): A's kit retired with A,
      so "replace with the held kit" means the one the ceremony just showed.
    - **An image is uploaded under A BEFORE the ceremony** and proven listed and
      painted (``seed_an_image_under_this_identity``): "content sealed under A
      opens" is otherwise satisfied by an empty corpus.
    - **``Restored`` alone is not enough.** ``machine.rs`` maps only an
      *unreadable* predecessor section to ``RestoredPredecessorsLost``; an
      *absent* one — exactly what the defect produces — lands plain
      ``Restored``. So the no-error landing is necessary and the positive half
      is the corpus: after sign-in the item's NAME lists and its thumbnail
      PAINTS, both opened with A's ``BackupKey`` recovered from the blob — and
      the corpus re-seal line does not read *owed elsewhere*, the arm rendered
      exactly when this device holds no predecessor material.

    ⚠ **The window.** No deterministic hold on B's corpus re-seal exists, so
    this journey observes the window rather than holding it: it reads the
    corpus re-seal line at the replace and quotes it on failure. The carrying
    rule holds regardless of window state; what depends on it is only whether
    the corpus assertions *discriminate* (a corpus already moved under B opens
    without A's seed). **Measured 2026-09-26 on tui: the line reads ``''`` at
    the replace** (no agent record yet), and the Media item is still sealed
    under A when the device is lost — the mutation matrix below is the proof.

    **Mutation-verified 2026-09-26, and the matrix is the point** (each reverted
    after). The replaced blob has TWO carriers of A's seed — the app's registry
    resolution (``escrow_predecessor_seeds`` in the replace arm) and the shared
    replace's union with the resting blob's own section
    (``kit.rs::carry_forward_predecessors``) — so severing either alone stays
    green by design:

    =====================================================  ======  =====
    severance                                              landing names
    =====================================================  ======  =====
    none (baseline)                                        pass    pass
    tui replace arm's predecessor slice → empty            pass    pass
    that AND the shared carry-forward → caller's slice     pass    RED
    =====================================================  ======  =====

    Row 3's clean landing is the ``Restored``-on-an-absent-section trap made
    visible: the outcome alone would have passed it.
    """
    app = ungranted_app
    account = qualified_account(app, nest_instance)
    _assert_direct_address_account(account)

    inherited_file, successor_kit = _seed_under_a_then_succeed(app, tmp_path)

    # ── Replace B's kit inside the window ───────────────────────────────────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    window_at_replace = app.settings.aftermath_corpus_reseal_status()
    print(f"[window] corpus re-seal line at the replace: {window_at_replace!r}")
    app.settings.replace_kit_with_held(successor_kit)
    wait_until(
        lambda: app.settings.recovery_kit_secret() not in ("", successor_kit),
        30.0,
        diagnose=lambda: (
            "the replace never showed its new kit; status reads "
            f"{app.settings.recovery_kit_status()!r}, error surface: "
            f"{app.error_text()!r}"
        ),
    )
    new_phrase = app.settings.recovery_kit_secret()
    assert len(new_phrase) == _SECRET_HEX_LEN

    # ── Lose every device; the NEW phrase brings B back, and A's corpus ─────
    _restore_sign_in_and_open_the_predecessor_corpus(
        app,
        nest_instance,
        phrase=new_phrase,
        account=account,
        inherited_file=inherited_file,
        window=window_at_replace,
        which_blob="the replaced kit's blob",
    )


@pytest.mark.feature("take-your-account-back")
def test_after_a_succession_the_successors_own_phrase_restores_the_predecessor_corpus(
    ungranted_app, nest_instance, tmp_path
):
    """upload under A → succeed A→B → lose every device → restore from the
    phrase the ceremony SHOWED → sign in → A's corpus still opens.

    Outcome 21 of *Take your account back*, and the no-replace sibling of the
    journey above. ``identity-succession.md`` § Seed escrow: the kit mint the
    succession owes seals ``[B's seed, A's seed…]``, because the corpus stays
    sealed under A until B's re-seal drains and the only other copies of A's
    seed are on the devices the user just lost. That mint is the ONE site whose
    predecessor slice must be non-empty (the status row names it), and nothing
    else writes the blob this journey restores from: the succession deletes A's
    blob, and no replace runs. So here the blob has exactly one carrier of A's
    seed, where the replace journey's has two (its matrix), and this is the
    end-to-end witness that can tell the owed mint's slice from ``&[]``.

    The assertions are the replace journey's, for the same reasons
    (``_restore_sign_in_and_open_the_predecessor_corpus``). The window is
    observed, not held: the re-seal line is read on the section the closing
    act left on screen, and quoted on failure.

    **Mutation-verified 2026-09-27 on tui** (reverted after): with the owed
    mint's predecessor slice emptied (``session::succession_kit_discharge_op``
    passing no seeds), the restore still LANDS cleanly and the file-name
    assertion goes red. The clean landing is the ``Restored``-on-an-absent-
    section trap: the outcome alone would have passed it.
    """
    app = ungranted_app
    account = qualified_account(app, nest_instance)
    _assert_direct_address_account(account)

    inherited_file, successor_kit = _seed_under_a_then_succeed(app, tmp_path)

    app.settings.navigate()
    app.settings.open_recovery_kit()
    window_at_loss = app.settings.aftermath_corpus_reseal_status()
    print(f"[window] corpus re-seal line before the loss: {window_at_loss!r}")

    _restore_sign_in_and_open_the_predecessor_corpus(
        app,
        nest_instance,
        phrase=successor_kit,
        account=account,
        inherited_file=inherited_file,
        window=window_at_loss,
        which_blob="the blob the succession's closing act sealed",
    )


# ── A restore that cannot work says which case you are in ───────────────────


@pytest.mark.feature("recovery-kit")
def test_a_restore_with_no_sealed_copy_to_restore_from_says_so(
    succeedable_app, nest_instance
):
    """a kit registered with no escrow → lose every device → restore → the
    screen says nothing rests to restore from, and what to do instead.

    The precondition is a kit registered WITHOUT its sealed copy — exactly what
    the API fixture registers (fixture setup, convention 8's carve-out; the
    restore and its answer are the behavior under test, and they are all UI).
    `no_escrow` is the honest answer, not a fault (`onboarding.md` § 1
    Identity), and it must not read like the superseded one: the two ask the
    user to do different things.
    """
    app, user = succeedable_app
    account = qualified_account(app, nest_instance)
    phrase = register_recovery_kit(
        nest_instance["url"],
        actor_id_hex=user["actor_id_hex"],
        identity_seed_hex=bytes(user["signing_key"]).hex(),
    )

    lose_every_device_and_open_restore(app, nest_instance)
    app.onboarding.restore_from_recovery_kit(phrase, account=account)

    wait_until(
        lambda: app.has_error(),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "a kit with nothing resting must refuse on error-message; page "
            f"shows phrase-field={app.is_visible('recovery-entry-phrase-field')}, "
            f"handle-input={app.is_visible('handle-input')}"
        ),
    )
    assert app.error_text() == S.onboarding.recovery_entry.no_escrow, (
        "the refusal must name THIS case — nothing rests to restore from — and "
        f"send the user to a signed-in device; got {app.error_text()!r}"
    )
    assert app.is_visible("recovery-entry-phrase-field"), (
        "no_escrow is not a routing answer: the user stays where they are"
    )


@pytest.mark.feature("recovery-kit")
def test_a_restore_of_an_identity_that_moved_on_routes_to_the_import(
    succeedable_app, nest_instance
):
    """a kit → the account is taken back to a new identity → lose every device
    → restore with the OLD kit → told the account moved, and sent to import.

    The succession is minted through the API fixture (setup, not the behavior
    under test — the ceremony's own UI has ``test_identity_succession_ceremony``).
    The kit is the one the app copied, because it names the identity it was
    made for: a handle typed after a succession names the account's NEW
    identity, and this case is about a phrase for the old one.
    """
    app, user = succeedable_app

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    phrase = app.settings.recovery_kit_secret()
    copied = _copy_the_shown_kit(app, phrase)

    succeed_identity(
        nest_instance["url"],
        old_actor_id_hex=user["actor_id_hex"],
        recovery_secret_hex=phrase,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        old_seed_hex=bytes(user["signing_key"]).hex(),
    )

    lose_every_device_and_open_restore(app, nest_instance)
    app.onboarding.restore_from_recovery_kit(copied)

    wait_until(
        lambda: app.is_visible("paste-secret-field"),
        RESTORE_OUTCOME_S,
        diagnose=lambda: (
            "a kit for an identity that moved on must route to the identity "
            f"import; error surface says {app.error_text()!r}, still on "
            f"recovery_entry={app.is_visible('recovery-entry-phrase-field')}"
        ),
    )
    assert app.error_text() == S.onboarding.recovery_entry.superseded, (
        "the import must say why the user is there and what to do next — it "
        f"is not the no-escrow answer; got {app.error_text()!r}"
    )


# ── A kit you copied restores with nothing typed ────────────────────────────


def _copy_the_shown_kit(app, phrase: str) -> str:
    """Press copy on the kit on screen and return what reached the clipboard.

    Asserts it is the same kit in its account-naming form — the
    ``fauna://recovery`` URI — without ever putting the secret in a message.
    """
    try:
        before = app.driver.get_clipboard_text()
    except NotImplementedError:
        skip_environment(
            f"the {app.driver.__class__.__name__} driver cannot read what the "
            "app copies, so the copied kit cannot be carried to the restore"
        )
    app.settings.copy_recovery_kit()
    wait_until(
        lambda: app.driver.get_clipboard_text() not in (None, before),
        10.0,
        diagnose=lambda: "pressing copy put nothing new on the clipboard",
    )
    copied = app.driver.get_clipboard_text()
    assert copied.startswith("fauna://recovery"), (
        "copy carries the kit's URI form, which names the account; the "
        "clipboard holds something else"
    )
    assert phrase in copied, "the copied kit must be the kit on screen"
    return copied


@pytest.mark.feature("recovery-kit")
def test_a_copied_kit_restores_the_account_with_nothing_typed_but_the_kit(
    ungranted_app, nest_instance
):
    """create → copy → lose every device → paste the copy, leave the account
    field empty → the same account comes back.

    The other restore journeys take the written-down path (bare hex, account
    typed). A kit the user copied — or scanned from its QR, which encodes the
    same URI — carries the account's id and its qualified handle, so the
    restore can find the home nest with nothing else entered
    (`identity-succession.md` § The RecoveryKey → *Kit payload*). The machine
    test ``a_kit_carrying_its_handle_restores_with_nothing_typed`` pins the
    parse; this pins that the app actually copies that form and a real nest
    accepts it.
    """
    app = ungranted_app
    bare_handle = app.driver.get_state("session.handle")
    assert bare_handle, "the fixture must be signed in with a real nest handle"

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    copied = _copy_the_shown_kit(app, app.settings.recovery_kit_secret())

    lose_every_device_and_open_restore(app, nest_instance)
    app.onboarding.restore_from_recovery_kit(copied)

    wait_for_restore_landing(app)
    prefilled = app.driver.get_text("handle-input")
    assert prefilled.split("@", 1)[0] == bare_handle, (
        "the account the copied kit named must be carried into handle_entry; "
        f"got {prefilled!r}, wanted the handle {bare_handle!r}"
    )
    assert_restored_the_same_account(app)
